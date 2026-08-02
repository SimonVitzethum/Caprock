//! **Arch-neutrale IOMMU-Fassade** (x86_64 → VT-d). Gegenstück zu `aarch64::iommu`.

use super::vtd;
pub use crate::fault::{FaultKind, FaultRecord};

pub fn faults_empty() -> bool {
    vtd::faults_empty()
}
pub fn peek_fault() -> Option<FaultRecord> {
    vtd::peek_fault()
}
pub fn drain_faults() -> u32 {
    vtd::drain_faults()
}
pub fn config_errors() -> u32 {
    vtd::config_errors()
}

/// **Der Adressbereich, in dem eine DMA-Schreibung keine Speicherzugriff ist** (B-3.4).
///
/// Auf x86 liegt dort das Interrupt-Nachrichtenfenster (`0xFEE0_0000..0xFEF0_0000`). Eine
/// DMA-Schreibung dorthin behandelt VT-d **als Interrupt-Nachricht**, nicht als zu uebersetzende
/// Adresse — die Uebersetzung wird gar nicht befragt.
///
/// Das macht den Bereich als **IOVA** unbenutzbar, und zwar auf eine besonders unangenehme Art:
/// eine Region, die dort hineingemappt wuerde, saehe im Seitentabellen-Audit vollkommen richtig
/// aus, und das Geraet erzeugte trotzdem Interrupts statt Speicherzugriffe. Kein Fault, kein
/// Eintrag in der Fehlerwarteschlange — nur Daten, die nirgends ankommen.
///
/// `None` heisst „diese Architektur hat keinen solchen Bereich" und **nicht** „unbekannt": wer hier
/// `None` liefert, sagt zu, dass jede IOVA uebersetzt wird.
pub fn interrupt_message_window() -> Option<(u64, u64)> {
    Some((0xFEE0_0000, 0x0010_0000))
}
