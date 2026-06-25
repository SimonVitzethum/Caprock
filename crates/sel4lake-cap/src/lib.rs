#![no_std]
//! Capability-System-Kern von SEL4Lake (ADR 0003).
//!
//! Capabilities sind die einzige Autoritätsquelle: ein Subjekt darf genau das,
//! wofür es eine gültige Capability besitzt. Dieses Crate stellt den Kern bereit:
//!
//! * [`CapSpace`] — eine flache Tabelle typsicherer Capability-Slots (CTE-artig:
//!   Capability + Ableitungs-Metadaten/MDB).
//! * Ein **Capability-Derivation-Tree (CDT)**: jeder abgeleitete Cap ist Kind
//!   seiner Quelle; das ermöglicht rekursive [`CapSpace::revoke`].
//! * Eine **Objekt-Tabelle** mit Referenzzählung: das zugrunde liegende Objekt
//!   (z. B. eine physische Region) wird finalisiert (Speicher zurückgegeben),
//!   sobald die *letzte* darauf verweisende Cap gelöscht wird.
//! * **Generations-Handles** ([`CapPtr`]): externe Verweise prüfen eine
//!   Generationsnummer und erkennen so stale Pointer.
//!
//! Operationen: [`CapSpace::copy`], [`CapSpace::mint`], [`CapSpace::move_cap`],
//! [`CapSpace::delete`], [`CapSpace::revoke`]. Rechte können bei der Ableitung
//! nur *eingeschränkt* werden (kein Privilege-Escalation).
//!
//! Alles ist sichere Rust-Datenstrukturlogik über feste Arrays — **kein `unsafe`**.

mod object;
mod space;

pub use object::ObjectKind;
pub use space::{CapError, CapInfo, CapPtr, CapSpace, ReplyFinal};
