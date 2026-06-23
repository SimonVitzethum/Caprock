//! PSCI (Power State Coordination Interface) — Sekundärkern-Start.
//!
//! Auf QEMU `virt` ist der Conduit laut DTB `hvc`; PSCI-Aufrufe erfolgen über
//! die `HVC`-Instruktion (Funktions-IDs gemäß SMCCC/PSCI). Inline-Assembler für
//! einen Firmware-Call ist eine erlaubte `unsafe`-Domäne.

use core::arch::asm;

/// `CPU_ON` (SMC64-Funktions-ID gemäß QEMU-DTB).
const PSCI_CPU_ON: u64 = 0xC400_0003;

/// PSCI-Statuscodes (Auswahl).
pub const SUCCESS: i64 = 0;

/// Einen ausgeschalteten Kern starten.
///
/// * `target_mpidr` — Ziel-CPU als MPIDR-Affinitätswert (QEMU `virt`: Aff0 = Index).
/// * `entry_point`  — physische Einstiegsadresse (MMU des Zielkerns ist noch aus).
/// * `context_id`   — beliebiger Wert, den der Kern in `x0` vorfindet (wir
///   übergeben die Stack-Spitze).
///
/// Rückgabe: PSCI-Statuscode (`SUCCESS` = 0, sonst negativ).
pub fn cpu_on(target_mpidr: u64, entry_point: u64, context_id: u64) -> i64 {
    let ret: i64;
    // SAFETY: PSCI-Firmware-Call via HVC. x0..x3 sind Argumente/Rückgabe;
    // x4..x17 dürfen laut SMCCC zerstört werden und werden als Clobber markiert.
    unsafe {
        asm!(
            "hvc #0",
            inlateout("x0") PSCI_CPU_ON => ret,
            in("x1") target_mpidr,
            in("x2") entry_point,
            in("x3") context_id,
            lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
            lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
            lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
            lateout("x16") _, lateout("x17") _,
            options(nostack),
        );
    }
    ret
}
