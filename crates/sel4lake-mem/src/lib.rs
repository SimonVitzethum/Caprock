#![no_std]
//! Physisches Speichermodell von SEL4Lake (ADR 0002, ADR 0003).
//!
//! Im Single-Address-Space gibt es keine Adressübersetzung: eine
//! [`MemoryCap`] beschreibt direkt einen realen physischen Bereich `[base, len)`
//! mit Rechten. Speicher-**Autorität** wird durch den Besitz solcher Caps
//! geregelt; die [`MemoryCap`] ist ein **lineares** (move-only) Rust-Objekt —
//! Besitz des Wertes ist die Capability. Damit modelliert Rusts Ownership
//! direkt: kein Double-Free, Transfer = Move, Ableitung = `split`.
//!
//! Der [`PhysAllocator`] verwaltet freies RAM (sortierte Freiliste mit
//! Coalescing) und prägt Wurzel-Caps. Alles ist reine Arithmetik über Regionen
//! — **kein Speicherzugriff, kein `unsafe`**.

mod alloc;
mod cap;
mod region;

pub use alloc::PhysAllocator;
pub use cap::MemoryCap;
pub use region::{PhysRegion, Rights};

/// Seitengröße (Allokationsgranularität).
pub const PAGE: u64 = 4096;
