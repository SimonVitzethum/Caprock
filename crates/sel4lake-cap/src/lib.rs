#![no_std]
#![forbid(unsafe_code)]
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
//! Alles ist sichere Rust-Datenstrukturlogik — **kein `unsafe`**, seit A-3.4 nicht mehr nur
//! behauptet, sondern per `forbid(unsafe_code)` erzwungen.
//!
//! Die Tabellen sind seither **zur Boot-Zeit dimensioniert** ([`CapSpace::attach`]) statt fest im
//! `.bss`. Den Rohspeicher besorgt der Kernel und trägt dessen `unsafe`-Vertrag; hier kommen nur
//! die fertigen [`Slab`](sel4lake_slab::Slab)-Handles an. [`CapSlot`] und [`Object`] sind deshalb
//! öffentlich — als undurchsichtige Platzhalter mit `EMPTY`, ohne zugängliche Felder.

mod object;
mod space;

pub use object::{DmaCoherence, DmaDir, Object, ObjectKind};
pub use space::{CapError, CapInfo, CapPtr, CapSlot, CapSpace, Finalized};
