//! **Typisierter atomarer Hook-Slot** — von beiden Architekturen genutzt.
//!
//! Die Trap-Hooks (Reschedule, Syscall, Fault, FP, Geräte-IRQ) müssen aus dem Interrupt-
//! Kontext **lock-frei** lesbar sein, liegen also als `usize` in einem Atomic. Früher stand
//! an jeder Lesestelle ein eigenes `transmute::<usize, KonkreterHookTyp>` — mehrere
//! `unsafe`-Stellen, bei denen ein Vertipper einen Hook als **falschen Funktionstyp**
//! aufgerufen hätte (UB, vom Compiler ungeprüft).
//!
//! Dieser Wrapper bindet den Slot per `PhantomData` an **genau einen** Funktionszeigertyp:
//! `store`/`load` sind typgeprüft, die Verwechslung ist strukturell unmöglich, und die
//! Roh-Konvertierung existiert nur noch **einmal** (hier).

use core::sync::atomic::{AtomicUsize, Ordering};

pub struct AtomicHook<F: Copy> {
    /// Funktionszeiger als `usize`; `0` = nicht gesetzt.
    raw: AtomicUsize,
    _marker: core::marker::PhantomData<fn() -> F>,
}

impl<F: Copy> AtomicHook<F> {
    pub const fn new() -> Self {
        Self {
            raw: AtomicUsize::new(0),
            _marker: core::marker::PhantomData,
        }
    }

    /// Hook setzen (einmalig beim Boot, vor dem Aktivieren von Interrupts).
    pub fn store(&self, hook: F) {
        const {
            // Nur Funktionszeiger (zeigergroß, kein Fat Pointer) passen in den Slot.
            assert!(core::mem::size_of::<F>() == core::mem::size_of::<usize>());
        }
        // SAFETY: `F` ist per const-Assertion zeigergroß; ein Funktionszeiger hat dieselbe
        // Repräsentation wie `usize`. Nur die Roh-Bits werden abgelegt.
        let raw = unsafe { core::mem::transmute_copy::<F, usize>(&hook) };
        self.raw.store(raw, Ordering::Release);
    }

    /// Hook lesen; `None`, solange keiner registriert ist. Lock-frei (Interrupt-Kontext).
    pub fn load(&self) -> Option<F> {
        let raw = self.raw.load(Ordering::Acquire);
        if raw == 0 {
            return None;
        }
        // SAFETY: `raw` wurde ausschließlich von `store` mit einem gültigen Zeiger **genau
        // dieses** Typs `F` geschrieben (der Slot ist über `PhantomData` an `F` gebunden, es
        // gibt keinen anderen Schreibpfad). Größengleichheit ist const-geprüft.
        Some(unsafe { core::mem::transmute_copy::<usize, F>(&raw) })
    }
}

impl<F: Copy> Default for AtomicHook<F> {
    fn default() -> Self {
        Self::new()
    }
}
