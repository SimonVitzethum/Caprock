//! **Arch-neutrale IOMMU-Fassade** (aarch64 → SMMUv3).
//!
//! Der Kernel und seine Tests reden über `hal::iommu`, nicht über `hal::smmu` bzw. `hal::vtd`.
//! Die Fassade ist der Ort, an dem die architekturspezifische Fehlerklassifikation in die
//! gemeinsame Form (`fault::FaultRecord`) übersetzt wird — und zwar **bevor** x86 eine Zuteilung
//! hat, damit dem Negativtest keine `cfg`-Verzweigung wächst.

use super::smmu;
use crate::fault::{FaultKind, FaultRecord};
pub use crate::iommu_health::{IommuHealth, Unhealthy};

/// **The arch-neutral health statement** (see [`crate::iommu_health`]) — the aarch64 half.
///
/// The values existed before, but only as the arch-specific `smmu` report line built from
/// `smmu_*` accessors in `system.rs` that were gated `#[cfg(target_arch = "aarch64")]`. x86 had no
/// counterpart, so the two architectures said different things about the same property.
///
/// ## The liveness proof on aarch64
///
/// `invalidation_round_trip` is **not** filled here. Unlike x86, where an invalidation is a
/// self-contained register or queue operation, `CMD_SYNC` on SMMUv3 needs the command queue's
/// physical address and the producer index — state the HAL does not own; it lives in the
/// `DmaEnforcer`. The caller therefore passes the round-trip result it already has from
/// `cmd_sync`, which is the same value the `smmu` line has always printed.
///
/// Handing it in rather than re-deriving it is deliberate: *a checker that recomputes the quantity
/// it checks is checking a second reality.* There is exactly one round trip per boot, and this
/// reports **that** one.
pub fn health(invalidation_round_trip: bool) -> IommuHealth {
    if !smmu::present() {
        return IommuHealth::ABSENT;
    }
    // Die SMMUv3 ist genau eine Einheit; „spricht sie?" ist die Lesbarkeit ihres IDR0. Ein
    // Rueckgabewert von 0 dort heisst „kein Geraet", und `present()` haette dann schon abgewiesen.
    IommuHealth {
        present: true,
        translation_enabled: smmu::enabled(),
        units: 1,
        units_speaking: if smmu::idr0() != 0 { 1 } else { 0 },
        invalidation_round_trip,
        faults_empty: smmu::eventq_empty(),
        config_errors: smmu::config_errors(),
        hw_error: smmu::gerror(),
    }
}

fn classify(kind: u8) -> FaultKind {
    match kind {
        smmu::EVT_F_TRANSLATION => FaultKind::Translation,
        0x13 => FaultKind::Permission, // F_PERMISSION
        0x11 => FaultKind::AddressSize, // F_ADDR_SIZE
        smmu::EVT_C_BAD_STREAMID
        | smmu::EVT_F_STE_FETCH
        | smmu::EVT_C_BAD_STE
        | smmu::EVT_F_CD_FETCH
        | smmu::EVT_C_BAD_CD => FaultKind::Config,
        other => FaultKind::Other(other),
    }
}

/// Keine aufgezeichneten Fehler?
pub fn faults_empty() -> bool {
    smmu::eventq_empty()
}

/// Den ältesten Eintrag lesen, ohne ihn zu verbrauchen.
pub fn peek_fault() -> Option<FaultRecord> {
    smmu::eventq_peek().map(|e| FaultRecord {
        kind: classify(e.kind),
        requester: e.stream_id,
        input_addr: e.input_addr,
        raw: e.kind,
    })
}

/// Die Aufzeichnung leeren; Konfigurationsfehler wandern vorher in [`config_errors`].
pub fn drain_faults() -> u32 {
    smmu::drain_eventq()
}

/// Beobachtungsunabhängiger Zähler für Zustände, nach denen jede Aussage „keine Faults"
/// bedeutungslos ist: Konfigurationsfehler (die Einheit lehnt die Tabellen ab) **und**
/// Überlauf der Aufzeichnung.
pub fn config_errors() -> u32 {
    smmu::config_errors()
}

/// Gegenstueck zu `x86_64::iommu::interrupt_message_window` (B-3.4).
///
/// Auf aarch64 laeuft MSI ueber die **ITS-Doorbell**, und die ist eine ganz gewoehnliche Adresse,
/// die durch die SMMU uebersetzt wird — es gibt kein Fenster, das die Uebersetzung umgeht. Deshalb
/// `None`, und das ist eine **Zusage**, keine Unkenntnis: jede IOVA wird hier uebersetzt.
///
/// (Sollte je eine Plattform dazukommen, auf der die Doorbell ausserhalb der Uebersetzung liegt,
/// gehoert sie hierher — nicht in eine Sonderbehandlung beim Aufrufer.)
pub fn interrupt_message_window() -> Option<(u64, u64)> {
    None
}
