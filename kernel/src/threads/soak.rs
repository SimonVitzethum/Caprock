//! Continuous-Soak-Treiber (Burn-in #2) — **nur** mit Feature `soak` einkompiliert.
//!
//! Schließt die Lücke, die der Reboot-Burn-in (#1) offenlässt: **Dauerbetrieb EINER Instanz** +
//! **Langzeit-Speicherkonsistenz**. Nach Abschluss des regulären Selbsttests fährt der Idle-Manager
//! statt `system_off` eine **Endlos-Epochenschleife**. Jede Epoche ruft ausschließlich **bereits
//! vorhandene**, balance-neutrale Kernel-Operationen wiederholt auf (keine neue Kernel-Funktionalität);
//! der **Kernel-Kern bleibt byte-identisch** zum Release (dies ist reiner Harness-/Testcode, als
//! Kindmodul von `threads` über `use super::*` an den Selbsttest gebunden).
//!
//! Jede Epoche baut vollständig ab; am Epochenende werden **alle** Audits + die Ressourcen-Baseline
//! **gegen den Soak-Start** geprüft → jede Drift = echter Befund. Ein `SOAK`-Heartbeat (~1×/Minute)
//! trägt Uptime/freies RAM/Cap-Anzahl/Op-Zähler/Audit-Ergebnis auf die serielle Konsole; der
//! Host-Orchestrator `tools/soak.py` verfolgt daraus die **Kurven über die Zeit**.

use super::*;
use sel4lake_hal::println;

/// Heartbeat-Abstand in Timer-Ticks (TICK_HZ=100 → 6000 = ~60 s).
const SOAK_HEARTBEAT_TICKS: u64 = 6000;
/// Direkte Allokator-Churn-Iterationen je Epoche (variierende Größen).
const SOAK_CHURN_ITERS: u64 = 8;

/// Ressourcen-Baseline zum Soak-Start; jede Epoche muss exakt hierher zurückkehren.
struct SoakBase {
    free: u64,
    cap_obj: usize,
    cap_slots: usize,
}

fn snapshot() -> SoakBase {
    SoakBase {
        free: system::total_free(),
        cap_obj: system::cap_used_objects(),
        cap_slots: system::cap_used_slots(),
    }
}

/// Direkter PhysAllocator-Churn: variierende Größen allozieren, schreiben/lesen, freigeben
/// (lineares Eigentum → balanciert). Gibt die Anzahl verifizierter Round-Trips.
fn region_churn() -> u64 {
    let mut ok = 0;
    for k in 0..SOAK_CHURN_ITERS {
        let sz = 0x1000 * (1 + (k & 3));
        if let Some(cap) = system::alloc(sz, 0x1000) {
            let base = cap.region().base;
            let val = 0xA5A5_0000_0000_0000u64 ^ k;
            poke_u64(base, val);
            if peek_u64(base) == val {
                ok += 1;
            }
            system::free(cap);
        }
    }
    ok
}

/// Eine Soak-Epoche: balance-neutrale Operationen über einen breiten Kernel-Surface — TrustedSAS-
/// Laden (Zertifikat-Gate + ELF-Loader + EL0-PD + Teardown), DMA-Round-Trip (virtio/SMMU-Pfad,
/// Attach/Detach) und direkter Allokator-Churn. Gibt `(loads, dmas, churns)`.
fn epoch() -> (u64, u64, u64) {
    let loads = if check_loadtrusted_el0() { 1 } else { 0 };
    let dmas = if run_dmagen() { 1 } else { 0 };
    let churns = region_churn();
    (loads, dmas, churns)
}

fn audits_clean() -> bool {
    loader::trust_audit() == 0 && ext27_audits_ok()
}

/// Audits + Ressourcen-Baseline gegen den Soak-Start prüfen. `0` = sauber, sonst Anomalie-Code:
/// 1=trust_audit, 2=domain/cap/vspace/loader/ipc_audit, 3=RAM-Drift, 4=Cap-Objekt-Drift,
/// 5=Cap-Slot-Drift.
fn check(base: &SoakBase) -> u32 {
    if loader::trust_audit() != 0 {
        return 1;
    }
    if !ext27_audits_ok() {
        return 2;
    }
    if system::total_free() != base.free {
        return 3;
    }
    if system::cap_used_objects() != base.cap_obj {
        return 4;
    }
    if system::cap_used_slots() != base.cap_slots {
        return 5;
    }
    0
}

/// Endlos-Soak-Schleife (kehrt **nie** zurück). Vom Idle-Manager nach dem Selbsttest aufgerufen.
pub fn run() -> ! {
    // Warm-up-Epoche: vorhandene Quiescenz setzen lassen, DANN die Baseline schnappen (so reflektiert
    // sie den stationären Zustand nach einmaligem Durchlauf aller Operationen).
    let _ = epoch();
    let base = snapshot();
    println!(
        "SOAK start free_bytes={} free_mib={} cap_obj={} cap_slots={} hb_ticks={}",
        base.free,
        base.free / (1024 * 1024),
        base.cap_obj,
        base.cap_slots,
        SOAK_HEARTBEAT_TICKS
    );

    let mut epochs = 0u64;
    let mut loads = 0u64;
    let mut dmas = 0u64;
    let mut churns = 0u64;
    let mut anomalies = 0u64;
    let mut last_hb = hal::timer::ticks(0);

    loop {
        epochs += 1;
        let (l, d, c) = epoch();
        loads += l;
        dmas += d;
        churns += c;

        let code = check(&base);
        if code != 0 {
            anomalies += 1;
            println!(
                "SOAK ANOMALY epoch={} code={} free_bytes={} base_free={} cap_obj={} base_obj={} cap_slots={} base_slots={}",
                epochs, code, system::total_free(), base.free,
                system::cap_used_objects(), base.cap_obj,
                system::cap_used_slots(), base.cap_slots
            );
        }

        let now = hal::timer::ticks(0);
        if now.wrapping_sub(last_hb) >= SOAK_HEARTBEAT_TICKS {
            last_hb = now;
            println!(
                "SOAK hb epoch={} uptime_ticks={} free_bytes={} free_mib={} cap_obj={} cap_slots={} loads={} dmas={} churns={} faults={} anomalies={} audit={}",
                epochs, now, system::total_free(), system::total_free() / (1024 * 1024),
                system::cap_used_objects(), system::cap_used_slots(),
                loads, dmas, churns, system::el0_fault_count(), anomalies,
                if audits_clean() { 0 } else { 1 }
            );
        }
    }
}
