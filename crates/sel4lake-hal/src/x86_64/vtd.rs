//! **Intel VT-d** (DMA Remapping) — das x86-Gegenstück zum SMMUv3-Treiber auf ARM.
//!
//! Zweck ist derselbe: ein bus-masterndes Gerät greift **an der CPU-MMU vorbei** direkt auf
//! physischen Speicher zu. Die IOMMU stellt eine zweite Übersetzungsstufe davor, sodass ein
//! Gerät nur erreicht, was ihm der Kernel ausdrücklich zugeteilt hat.
//!
//! ## Was hier steht — und was (noch) nicht
//!
//! Implementiert ist der **Bring-up mit Default-Block**: Registerbasis aus der ACPI-DMAR,
//! Root-Tabelle anlegen (alle Einträge „not present"), `RTADDR` setzen, `SRTP`, dann `TE` —
//! ab da ist die Übersetzung aktiv und **jede** nicht ausdrücklich erlaubte DMA-Anforderung
//! wird geblockt. Das ist exakt die Aussage, die der `smmu`-Test auf ARM prüft
//! („Default-Abort, CR0-Enable, CMD_SYNC-Round-Trip"), und es ist der sicherheitsrelevante
//! Teil: **ohne Zuteilung geht nichts**.
//!
//! **Nicht** implementiert sind die per-Gerät-Zuteilungen (Kontext-Einträge + Second-Level-
//! Tabellen je Domäne). `attach` meldet deshalb ehrlich `false`: auf x86 lässt sich derzeit
//! kein DMA-Puffer an ein Gerät binden — statt eine Isolation vorzutäuschen, die es nicht gibt.
//! Der Zustand ist trotzdem **sicher**: geblockt statt ungeschützt.

use core::sync::atomic::{AtomicU64, Ordering};

// Registeroffsets (Intel VT-d Spezifikation, Kapitel 10).
const REG_VER: usize = 0x000;
const REG_CAP: usize = 0x008;
const REG_ECAP: usize = 0x010;
const REG_GCMD: usize = 0x018;
const REG_GSTS: usize = 0x01C;
const REG_RTADDR: usize = 0x020;
const REG_CCMD: usize = 0x028;
const REG_FSTS: usize = 0x034;

/// `FSTS.PFO` — **Primary Fault Overflow**: die Fault-Recording-Register sind voll, weitere
/// Faults werden **verworfen**, bis das Bit gelöscht ist.
///
/// Das ist die x86-Entsprechung zu der Lücke, die auf ARM schon einmal zugeschlagen hat: ein
/// Oracle der Form „keine weiteren Faults" bedeutet nach einem Fault-Sturm gar nichts, wenn die
/// Aufzeichnung übergelaufen ist. Der Zähler muss `PFO` mitführen (s. `fault_overflow`).
const FSTS_PFO: u32 = 1 << 0;

/// Bits in `GCMD`, die den **Zustand** tragen und bei jedem Kommando mitgeschrieben werden
/// müssen. `GCMD` ist kein Read-Modify-Write-Register: es wird als Ganzes geschrieben, und ein
/// nicht mitgeschriebenes Zustandsbit wird **gelöscht**.
///
/// Genau daran hängt eine Reihenfolge-Falle: `TE = 0` heißt **nicht** „blockiert", sondern
/// „keine Übersetzung" — also freier DMA. Wer den Root-Table-Pointer wechselt, indem er `SRTP`
/// allein nach `GCMD` schreibt, während `TE` gesetzt ist, öffnet für die Dauer des Wechsels
/// genau das Fenster, gegen das der ganze Mechanismus gebaut ist. Und es sähe im Test wie ein
/// Erfolg aus. Deshalb geht **jedes** Kommando über [`gcmd_issue`], das die Zustandsbits aus
/// `GSTS` übernimmt.
const GCMD_STATE_MASK: u32 = GCMD_TE | (1 << 25) | (1 << 26); // TE, IRE, QIE

/// Global Command/Status: Übersetzung aktivieren bzw. aktiv.
const GCMD_TE: u32 = 1 << 31;
const GSTS_TES: u32 = 1 << 31;
/// Set Root Table Pointer.
const GCMD_SRTP: u32 = 1 << 30;
const GSTS_RTPS: u32 = 1 << 30;
/// Context-Cache: globale Invalidierung anfordern / Fertigmeldung.
const CCMD_ICC: u64 = 1 << 63;
const CCMD_CIRG_GLOBAL: u64 = 1 << 61;

/// Registerbasis der Remapping-Einheit (`0` = keine gefunden).
static UNIT_BASE: AtomicU64 = AtomicU64::new(0);
/// Physische Adresse der Root-Tabelle (`0` = noch keine).
static ROOT_TABLE: AtomicU64 = AtomicU64::new(0);

fn read32(off: usize) -> u32 {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: `b` stammt aus der ACPI-DMAR und liegt im identity-gemappten, uncacheable
    // MMIO-Bereich; volatile Registerzugriffe aliasen keinen Rust-Speicher.
    unsafe { core::ptr::read_volatile((b + off as u64) as *const u32) }
}
fn write32(off: usize, v: u32) {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: wie `read32`.
    unsafe { core::ptr::write_volatile((b + off as u64) as *mut u32, v) };
}
fn read64(off: usize) -> u64 {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: wie `read32`.
    unsafe { core::ptr::read_volatile((b + off as u64) as *const u64) }
}
fn write64(off: usize, v: u64) {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: wie `read32`.
    unsafe { core::ptr::write_volatile((b + off as u64) as *mut u64, v) };
}

/// Ein **Ein-Schritt-Kommando** absetzen, ohne die Zustandsbits zu verlieren.
///
/// `one_shot` ist das Kommandobit (`SRTP`, `SIRTP`, …); alle Zustandsbits werden aus `GSTS`
/// übernommen. `want_mask`/`want_set` beschreiben, worauf gewartet wird.
fn gcmd_issue(one_shot: u32, want_mask: u32, want_set: bool) -> bool {
    let state = read32(REG_GSTS) & GCMD_STATE_MASK;
    write32(REG_GCMD, state | one_shot);
    wait_status(want_mask, want_set)
}

/// Ein **Zustandsbit** setzen oder löschen (ebenfalls unter Erhalt der übrigen).
fn gcmd_set_state(bit: u32, on: bool) -> bool {
    let mut state = read32(REG_GSTS) & GCMD_STATE_MASK;
    if on {
        state |= bit;
    } else {
        state &= !bit;
    }
    write32(REG_GCMD, state);
    wait_status(bit, on)
}

/// **Fähigkeiten der Einheit**, einmal beim Hochlauf gelesen (Schritt 1 des VT-d-Aufbaus).
///
/// Der Grund für diesen Aufsatz ist eine Lektion von der ARM-Seite: `STE.S1STALLD` unbedingt zu
/// setzen war ein Bit, das nur unter einer Capability-Bedingung zulässig ist — die Einheit
/// antwortete mit `C_BAD_STE`, der Stream übersetzte **gar nicht**, und nichts sagte es laut.
/// VT-d hat ein Dutzend solcher Bedingungen. Sie werden deshalb **hier** einmal gelesen,
/// protokolliert und zu benannten Werten abgeleitet; keine spätere Stelle entscheidet selbst.
#[derive(Clone, Copy, Debug)]
pub struct VtdCaps {
    pub raw_cap: u64,
    pub raw_ecap: u64,
    /// Anzahl Domain-IDs (`CAP.ND`) — das x86-Gegenstück zu `NDMA_CTX`. Die Erschöpfung braucht
    /// denselben sauberen Fehlschlag wie das IOVA-Fenster.
    pub num_domains: u32,
    /// **Caching Mode** (`CAP.CM`): sind auch **nicht-präsente** Einträge cachebar?
    ///
    /// Bei `CM = 1` (typisch für QEMU) ist eine Invalidierung **nach dem Anlegen** einer
    /// Übersetzung zwingend, nicht nur nach dem Entfernen. Der Kernel fährt die Invalidierung
    /// unbedingt und protokolliert `CM` nur: sie ist die konservative Variante, die überall
    /// hält. Die umgekehrte Schlussfolgerung („unter QEMU nötig, auf Blech nicht") wäre die
    /// falsche Richtung — und auf Blech fiele das Fehlen nicht auf.
    pub caching_mode: bool,
    /// `CAP.RWBF`: nach Tabellenänderungen muss der Write-Buffer explizit geflusht werden.
    pub rwbf: bool,
    /// `CAP.SAGAW` roh, und die daraus **gewählte** Adressbreite.
    pub sagaw: u32,
    /// Gewählte Eingangsbreite in Bits (39 = 3 Level, 48 = 4 Level, 57 = 5 Level). `0`, wenn die
    /// Einheit keine der unterstützten Stufen anbietet.
    pub agaw_bits: u32,
    /// Seitentabellen-Level zur gewählten Breite.
    pub agaw_levels: u32,
    /// `CAP.MGAW + 1` — harte Obergrenze der Eingangsadresse (x86-Gegenstück zu `CD.T0SZ`).
    pub mgaw_bits: u32,
    /// Anzahl Fault-Recording-Register (`CAP.NFR + 1`) und deren Offset (`CAP.FRO`).
    pub num_fault_regs: u32,
    pub fault_reg_offset: u64,
    /// `ECAP.C` — **Page-Walk-Kohärenz**. Ist sie 0, sieht die Einheit die Schreibvorgänge auf
    /// Root-/Context-/Second-Level-Einträge nicht ohne Cache-Clean. Das betrifft die *Tabellen*,
    /// nicht die DMA-Puffer — ein Pfad, den `dma_granule()` gar nicht abdeckt.
    pub coherent_walk: bool,
    /// `ECAP.QI` — Queued Invalidation (das `CMDQ`-Analogon). Ohne sie bleibt der
    /// registerbasierte Pfad; beides parallel soll es nicht geben.
    pub queued_invalidation: bool,
    /// `ECAP.IR` — Interrupt Remapping vorhanden (eigener Schalter, eigene Baustelle).
    pub interrupt_remapping: bool,
    /// `ECAP.SC` — **Snoop Control**: erlaubt, No-Snoop-Transaktionen per `SNP`-Bit im SLPTE zu
    /// überstimmen. Ohne SC hängt die Kohärenzannahme daran, dass das Gerät kein No-Snoop
    /// verwendet — GPUs tun es. „x86 ist kohärent" ist also eine Aussage unter einer Bedingung,
    /// nicht absolut.
    pub snoop_control: bool,
    /// `ECAP.SMTS` — Scalable Mode. Der Kernel fährt **Legacy**; das ist eine Wahl, keine
    /// Annahme, und sie steht hier, damit sie im Log sichtbar ist.
    pub scalable_mode: bool,
}

impl VtdCaps {
    /// Die Fähigkeiten der Einheit lesen. `None`, wenn keine Einheit gefunden wurde.
    pub fn read() -> Option<VtdCaps> {
        if !present() {
            return None;
        }
        let cap = read64(REG_CAP);
        let ecap = read64(REG_ECAP);
        let nd = (cap & 0b111) as u32;
        let sagaw = ((cap >> 8) & 0x1f) as u32;
        // Bevorzugt 39 Bit (3 Level) — dieselbe Eingangsbreite wie die ARM-Seite, damit die
        // Fensterarithmetik nicht zweimal existiert. Bietet die Einheit sie nicht an, wird
        // aufgestiegen; sie ist NICHT garantiert, manche Implementierungen bieten nur 48.
        let (agaw_bits, agaw_levels) = if sagaw & (1 << 1) != 0 {
            (39, 3)
        } else if sagaw & (1 << 2) != 0 {
            (48, 4)
        } else if sagaw & (1 << 3) != 0 {
            (57, 5)
        } else {
            (0, 0)
        };
        Some(VtdCaps {
            raw_cap: cap,
            raw_ecap: ecap,
            num_domains: 1u32 << (4 + 2 * nd),
            caching_mode: cap & (1 << 7) != 0,
            rwbf: cap & (1 << 4) != 0,
            sagaw,
            agaw_bits,
            agaw_levels,
            mgaw_bits: (((cap >> 16) & 0x3f) as u32) + 1,
            num_fault_regs: (((cap >> 40) & 0xff) as u32) + 1,
            fault_reg_offset: ((cap >> 24) & 0x3ff) * 16,
            coherent_walk: ecap & (1 << 0) != 0,
            queued_invalidation: ecap & (1 << 1) != 0,
            interrupt_remapping: ecap & (1 << 3) != 0,
            snoop_control: ecap & (1 << 7) != 0,
            scalable_mode: ecap & (1 << 43) != 0,
        })
    }

    /// Kann diese Einheit überhaupt eine Zuteilung tragen, wie der Kernel sie baut?
    ///
    /// Heute die einzige harte Bedingung: eine unterstützte AGAW-Stufe, und die Eingangsbreite
    /// muss auch von `MGAW` gedeckt sein. Alles andere ist Verhalten, nicht Machbarkeit.
    pub fn usable(&self) -> bool {
        self.agaw_bits != 0 && self.mgaw_bits >= self.agaw_bits
    }

    /// Obergrenze der Eingangsadresse, gegen die eine IOVA geprüft werden muss.
    pub fn input_limit(&self) -> u64 {
        let bits = self.agaw_bits.min(self.mgaw_bits);
        if bits >= 64 { u64::MAX } else { 1u64 << bits }
    }
}

/// Ist die Fault-Aufzeichnung übergelaufen (`FSTS.PFO`)? Danach sind weitere Faults **verworfen**
/// — jede Aussage der Form „keine weiteren Faults" ist dann bedeutungslos.
pub fn fault_overflow() -> bool {
    present() && read32(REG_FSTS) & FSTS_PFO != 0
}

/// Ist eine Remapping-Einheit vorhanden (ACPI-DMAR gefunden)?
pub fn present() -> bool {
    UNIT_BASE.load(Ordering::Acquire) != 0
}

/// Versionsregister (Diagnose/Test).
pub fn version() -> u32 {
    if present() {
        read32(REG_VER)
    } else {
        0
    }
}

/// Capability-Register (Diagnose/Test).
pub fn cap() -> u64 {
    if present() {
        read64(REG_CAP)
    } else {
        0
    }
}

/// Extended-Capability-Register (Diagnose/Test).
pub fn ecap() -> u64 {
    if present() {
        read64(REG_ECAP)
    } else {
        0
    }
}

/// Ist die Übersetzung aktiv (`GSTS.TES`)? Genau dann blockt die Einheit alles, was nicht
/// ausdrücklich zugeteilt ist.
pub fn enabled() -> bool {
    present() && read32(REG_GSTS) & GSTS_TES != 0
}

/// Registerbasis aus der **ACPI-DMAR** übernehmen (idempotent).
pub fn discover() -> bool {
    if present() {
        return true;
    }
    let Some(base) = super::acpi::dmar_unit_base() else {
        return false;
    };
    UNIT_BASE.store(base, Ordering::Release);
    true
}

/// Auf ein Statusbit warten (begrenzt — ein Hardware-Poll darf im Kernel nie unbegrenzt laufen).
fn wait_status(mask: u32, want_set: bool) -> bool {
    for _ in 0..1_000_000 {
        let set = read32(REG_GSTS) & mask != 0;
        if set == want_set {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// **Bring-up mit Default-Block.**
///
/// `root_table` ist ein vom Kernel geliefertes, **genulltes** 4-KiB-Frame: eine Root-Tabelle,
/// deren 256 Einträge alle „not present" sind. Genau das ist die Default-Block-Politik — jede
/// DMA-Anforderung eines Geräts läuft in einen nicht vorhandenen Kontext und wird abgewiesen.
///
/// Gibt `true` zurück, wenn die Übersetzung danach aktiv ist (`GSTS.TES`).
pub fn init(root_table: u64) -> bool {
    if !discover() {
        return false;
    }
    ROOT_TABLE.store(root_table, Ordering::Release);
    // Root-Table-Pointer setzen (Bits 63:12; Translation Table Mode = Legacy).
    write64(REG_RTADDR, root_table & !0xfff);
    // **Nie** `TE` löschen, um umzukonfigurieren: `TE = 0` heißt freier DMA, nicht Blockade.
    // `gcmd_issue` übernimmt die Zustandsbits aus `GSTS`, der Wechsel läuft also auch dann
    // ohne Lücke, wenn die Übersetzung bereits aktiv ist.
    if !gcmd_issue(GCMD_SRTP, GSTS_RTPS, true) {
        return false;
    }
    invalidate_context_cache();
    // Übersetzung aktivieren.
    gcmd_set_state(GCMD_TE, true)
}

/// Globale Invalidierung des Kontext-Caches (Gegenstück zum `CMD_SYNC`-Round-Trip auf ARM):
/// die Einheit muss die Anforderung quittieren, indem sie `ICC` wieder löscht.
pub fn invalidate_context_cache() -> bool {
    if !present() {
        return false;
    }
    write64(REG_CCMD, CCMD_ICC | CCMD_CIRG_GLOBAL);
    for _ in 0..1_000_000 {
        if read64(REG_CCMD) & CCMD_ICC == 0 {
            return true; // quittiert
        }
        core::hint::spin_loop();
    }
    false
}
