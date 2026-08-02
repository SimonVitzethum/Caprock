//! **Arch-neutrale IOMMU-Fassade** (aarch64 → SMMUv3).
//!
//! Der Kernel und seine Tests reden über `hal::iommu`, nicht über `hal::smmu` bzw. `hal::vtd`.
//! Die Fassade ist der Ort, an dem die architekturspezifische Fehlerklassifikation in die
//! gemeinsame Form (`fault::FaultRecord`) übersetzt wird — und zwar **bevor** x86 eine Zuteilung
//! hat, damit dem Negativtest keine `cfg`-Verzweigung wächst.

use super::smmu;
use crate::fault::{FaultKind, FaultRecord};

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
