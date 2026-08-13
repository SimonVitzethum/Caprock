//! **Wie tief ein Interrupt-Handler auf dem unterbrochenen Stack geht** — der zweite Summand der
//! Stackrechnung aus C4, und das Paar dazu.
//!
//! ## Warum das hier steht und nicht in `arch/*/exception.rs`
//!
//! Der Inhalt ist **zwei Atomics und drei Zeilen Arithmetik** — kein Register, kein Vektor, kein
//! `cfg`. Er stand trotzdem bis zum 2026-08-13 ausschliesslich in der x86-HAL, waehrend
//! `kernel/src/kstackmark.rs` und `kernel/src/system.rs` ihn **arch-neutral** riefen. Der
//! aarch64-Bau war damit ab dem 2026-08-12 kaputt (`E0425`, dreimal) — dieselbe Klasse, die zehn
//! Stunden vorher schon einmal bezahlt worden war (`hal::mmu::guard_*`).
//!
//! Eine zweite Kopie in der aarch64-HAL waere die naheliegende Behebung gewesen und genau die
//! falsche: **zwei Gedaechtnisse fuer eine Tatsache** driften, und der Zaehler, der driftet, ist
//! der, aus dem `kstack` seine Summe bildet. Es gibt daher **eine** Definition; beide
//! `exception`-Module reichen sie unter ihrem gewohnten Namen weiter, damit die Aufrufstellen
//! arch-neutral bleiben duerfen (`hal::exception::irq_tiefe`).
//!
//! `IRQ_TIEFE_N` ist die Sprechprobe: `MAX == 0` allein waere von „es kam nie ein Interrupt" nicht
//! zu unterscheiden — und eine Summe, deren zweiter Summand nie gemessen wurde, ist eine Summe
//! mit einer erfundenen Null.

use core::sync::atomic::{AtomicU64, Ordering};

static IRQ_TIEFE_MAX: AtomicU64 = AtomicU64::new(0);
static IRQ_TIEFE_N: AtomicU64 = AtomicU64::new(0);

/// **Den IRQ-Verbrauch MELDEN — gerufen aus der Tiefe, nicht am Einsprung.**
///
/// Die erste Fassung mass `frame - &local` im Einsprung von `handle_exception` und kam auf
/// **24 Byte**. Das war der Verbrauch *bis dorthin* und nicht der des Handlers: die Tiefe entsteht
/// erst im Reschedule-Pfad darunter. Eine Summe mit einem systematisch zu kleinen Summanden ist
/// schlimmer als keine — sie sieht aus wie eine Rechnung.
///
/// Deshalb ruft der Kernel diese Funktion **an seiner tiefsten Stelle** im IRQ-Kontext und reicht
/// beide Adressen herein; die HAL kennt den Scheduler nicht (Kerngrenze).
pub fn irq_tiefe_melden(frame: u64, tiefste_sp: u64) {
    IRQ_TIEFE_MAX.fetch_max(frame.saturating_sub(tiefste_sp), Ordering::Relaxed);
    IRQ_TIEFE_N.fetch_add(1, Ordering::Relaxed);
}

/// `(tiefster gemessener IRQ-Verbrauch in Byte, Anzahl der Messungen)`.
pub fn irq_tiefe() -> (u64, u64) {
    (
        IRQ_TIEFE_MAX.load(Ordering::Relaxed),
        IRQ_TIEFE_N.load(Ordering::Relaxed),
    )
}
