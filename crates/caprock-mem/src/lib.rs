#![no_std]
//! Physisches Speichermodell von Caprock (ADR 0002, ADR 0003).
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

// Die Crate ist `no_std` (sie läuft im Kernel). Die Farb-/Allokator-Arithmetik ist aber
// reine Rechnung ohne Hardware und damit auf dem Host prüfbar — der Testharness braucht
// dafür `std`. Nur unter `cfg(test)`, der Kernelbuild sieht davon nichts.
#[cfg(test)]
extern crate std;

mod alloc;
mod cap;
mod color;
mod region;

pub use alloc::{PhysAllocator, MAX_FRAGMENTS};
pub use cap::MemoryCap;
pub use color::{color_of, pick_free, stripe, ColorMask, MASK_BITS};
pub use region::{PhysRegion, Rights};

/// Seitengröße (Allokationsgranularität).
pub const PAGE: u64 = 4096;

/// Ein `u64` in einer besessenen physischen Region lesen.
///
/// Dies ist der **SAS-Direktzugriff** (ADR 0002): Wer die Memory-Capability für
/// die Region hält, greift direkt auf den (identity-gemappten) realen RAM zu —
/// es gibt keine MMU-Übersetzung und keine per-Prozess-Isolation. Roher
/// Speicherzugriff ist hier unvermeidlich; der Aufrufer garantiert, dass `addr`
/// in seine Region fällt.
pub fn peek_u64(addr: u64) -> u64 {
    // SAFETY: `addr` liegt in einer vom Aufrufer per MemoryCap besessenen,
    // identity-gemappten RW-Region; volatiler Zugriff auf realen RAM.
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

/// Ein `u64` in einer besessenen physischen Region schreiben (siehe [`peek_u64`]).
pub fn poke_u64(addr: u64, val: u64) {
    // SAFETY: wie `peek_u64`; volatiler Schreibzugriff auf besessenen RAM.
    unsafe { core::ptr::write_volatile(addr as *mut u64, val) }
}
