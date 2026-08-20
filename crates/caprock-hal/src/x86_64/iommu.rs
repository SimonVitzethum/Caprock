//! **Arch-neutrale IOMMU-Fassade** (x86_64 → VT-d). Gegenstück zu `aarch64::iommu`.

use super::vtd;
pub use crate::fault::{FaultKind, FaultRecord};
pub use crate::iommu_health::{IommuHealth, Unhealthy};

/// **The arch-neutral health statement** (see [`crate::iommu_health`]).
///
/// This is the x86 half of the sentence that until now only aarch64 could say. All the parts were
/// already here — `vtd` has `present`, `enabled`, `unit_count`, `units_speaking`,
/// `invalidate_context_cache`, `faults_empty`, `config_errors` — what was missing was that they be
/// asked **in one place, in the same words as the other architecture**.
///
/// ## The liveness proof on x86
///
/// `invalidation_round_trip` is **handed in**, not derived here — same signature as the aarch64
/// half, and for a reason that is not symmetry alone: **a health query must not touch hardware.**
/// The obvious version called [`vtd::invalidate_context_cache`] inline, which would have issued a
/// real global invalidation on every call, including from a report line. A read that mutates the
/// thing it reads is not an observation.
///
/// What the caller passes is a genuine round trip on either path it can take: with Queued
/// Invalidation `invalidate_context_cache` appends a wait descriptor and polls for the unit's
/// status write; without QI it polls `CCMD.ICC` until the unit clears it again. Both mean **the
/// unit acknowledged completion**, not "we wrote a register" — and that distinction is the whole
/// reason the field exists. A silent unit already counts as failure there (`all_ok = false`), so
/// the two statements agree.
pub fn health(invalidation_round_trip: bool) -> IommuHealth {
    if !vtd::present() {
        return IommuHealth::ABSENT;
    }
    IommuHealth {
        present: true,
        translation_enabled: vtd::enabled(),
        units: vtd::unit_count() as u32,
        units_speaking: vtd::units_speaking() as u32,
        invalidation_round_trip,
        faults_empty: vtd::faults_empty(),
        config_errors: vtd::config_errors(),
        // Neu am 2026-08-17: die Fehlerbits der Einheit selbst. `IQE`/`ICE`/`ITE` hat vorher
        // niemand gelesen -- s. `vtd::FSTS_UNIT_ERRORS`.
        hw_error: vtd::fault_status(),
    }
}

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
