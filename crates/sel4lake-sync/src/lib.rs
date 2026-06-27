#![no_std]
//! Minimale `no_std`-Synchronisationsprimitive.
//!
//! Wichtig: Atomare Lade-/Speicher-Exklusiv-Operationen (LDXR/STXR), auf denen
//! `core::sync::atomic` aufbaut, sind auf aarch64 nur mit **aktivierter MMU**
//! und cacheable Normal-Memory wohldefiniert. Ein [`SpinLock`] darf daher erst
//! benutzt werden, nachdem die MMU eingeschaltet ist (siehe `sel4lake-hal::mmu`
//! und ADR 0002).

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

// --- IRQ-Sicherheit der SpinLocks (Bugfix: reentranter Ticket-Lock-Deadlock) ---
//
// Ein [`SpinLock`] ist ein FIFO-**Ticket**-Lock. Wird derselbe Lock vom IRQ-/Reschedule-Pfad UND
// von Thread-/Idle-Kontext genommen (im Kernel: `SCHEDS[core]` und `NTFNS[]` werden im
// Timer-Tick-Reschedule sowie aus `idle->reap_core`/Syscalls genommen), entsteht ohne IRQ-Maske
// ein Deadlock: feuert der Timer-Tick, während Thread-/Idle-Kontext den Lock hält/erwartet, zieht
// der Reschedule-Hook ein ZWEITES Ticket auf denselben Lock — der erste Halter ist aber im
// IRQ-Handler suspendiert und gibt sein Ticket nie frei. Daher maskiert `lock()` IRQs am eigenen
// Kern VOR dem Ticket-Ziehen und der Guard stellt den vorherigen Zustand beim `Drop` wieder her
// (nesting-sicher: jeder Guard sichert den Stand von VOR seinem Lock).

/// DAIF (Interrupt-Maske) sichern + IRQs am aktuellen Kern maskieren. Gibt den vorherigen
/// DAIF-Zustand zurück (für [`irq_restore`]).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn irq_save_disable() -> u64 {
    let daif: u64;
    // SAFETY: reines Lesen + Setzen des DAIF-Systemregisters (I-Bit); keine Speicherwirkung.
    unsafe {
        core::arch::asm!("mrs {0}, DAIF", out(reg) daif, options(nomem, nostack, preserves_flags));
        core::arch::asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }
    daif
}

/// Den zuvor gesicherten DAIF-Zustand zurückschreiben (I-Bit). Der äußerste Guard gibt so „IRQs an"
/// wieder frei; war IRQ schon maskiert (verschachtelt/im Trap), bleibt es maskiert.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn irq_restore(daif: u64) {
    // SAFETY: schreibt nur das zuvor gelesene DAIF zurück; keine Speicherwirkung.
    unsafe {
        core::arch::asm!("msr DAIF, {0}", in(reg) daif, options(nomem, nostack, preserves_flags));
    }
}

// Host-Builds (z. B. `cargo test` anderer Crates): kein DAIF — No-Op.
#[cfg(not(target_arch = "aarch64"))]
#[inline(always)]
fn irq_save_disable() -> u64 {
    0
}
#[cfg(not(target_arch = "aarch64"))]
#[inline(always)]
fn irq_restore(_daif: u64) {}

/// Fairer Ticket-Spinlock.
///
/// Wer zuerst zieht, kommt zuerst dran (FIFO) — das ist deterministischer als
/// ein CAS-Lock, der einzelne Kerne aushungern lassen kann.
pub struct SpinLock<T: ?Sized> {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: Der Lock serialisiert jeden Zugriff auf `data`; gleichzeitig kann
// höchstens ein Guard existieren. Damit ist geteilter Zugriff zwischen Kernen
// sicher, sofern `T` über Thread-/Kerngrenzen bewegt werden darf (`T: Send`).
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}
unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
            data: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// Sperrt und gibt einen Guard zurück, der beim Verlassen automatisch
    /// freigibt.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        // IRQ-sicher: IRQs am eigenen Kern maskieren, BEVOR ein Ticket gezogen wird (sonst kann der
        // Timer-Tick/Reschedule denselben Lock reentrant ziehen -> Ticket-Deadlock; s. o.).
        let daif = irq_save_disable();
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.now_serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
        SpinGuard { lock: self, daif }
    }
}

/// RAII-Guard: hält den Lock, gibt bei `Drop` frei (und stellt den IRQ-Zustand wieder her).
pub struct SpinGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    /// DAIF-Zustand vor dem Locken (beim `Drop` wiederhergestellt).
    daif: u64,
}

impl<T: ?Sized> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: Solange dieser Guard lebt, hält er exklusiv den Lock; kein
        // anderer Zugriff auf `data` existiert gleichzeitig.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: wie `deref`; exklusiver Besitz durch den lebenden Guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        // Nächstes Ticket bedienen; `Release` veröffentlicht alle Schreibzugriffe
        // unter dem Lock an den nächsten Halter. DANACH den IRQ-Zustand wiederherstellen.
        self.lock.now_serving.fetch_add(1, Ordering::Release);
        irq_restore(self.daif);
    }
}

/// **Writer-bevorzugender Reader-Writer-Spinlock.**
///
/// Mehrere Leser dürfen den kritischen Abschnitt gleichzeitig betreten (geteilt);
/// ein Schreiber bekommt ihn exklusiv. Genau das, was eine Capability-Tabelle braucht:
/// die heißen IPC-Lookups (nur lesend) laufen auf verschiedenen Kernen **parallel**,
/// während die seltenen Mutationen (install/copy/mint/move/delete/revoke/grant)
/// exklusiv serialisiert werden. Schreiber haben Vorrang (angemeldete Schreiber lassen
/// keine neuen Leser mehr eintreten) — so hungern die seltenen Schreiber nicht aus.
///
/// Sperrordnung: Ein `RwSpinLock` nimmt **dieselbe** Position wie ein `SpinLock` an
/// derselben Stelle ein; `read()` und `write()` liegen an derselben Ordnungsposition
/// (shared vs. exklusiv desselben Locks). Wie [`SpinLock`] erst nach MMU-An benutzbar.
pub struct RwSpinLock<T: ?Sized> {
    /// Bit 31 (`WRITER`) gesetzt = ein Schreiber hält exklusiv; Bits 0..30 = Anzahl
    /// aktiver Leser.
    state: AtomicU32,
    /// Anzahl angemeldeter Schreiber (auch wartender). Solange > 0 treten neue Leser
    /// zurück -> Writer-Vorrang, keine Schreiber-Aushungerung.
    writers_waiting: AtomicU32,
    data: UnsafeCell<T>,
}

const RW_WRITER: u32 = 1 << 31;

// SAFETY: Der Lock serialisiert Schreiber exklusiv gegen alle anderen und lässt nur
// gleichzeitige *Leser* zu (die nur `&T` erhalten) — geteilter Zugriff ist damit
// datenrennenfrei, sofern `T: Send + Sync` über Kerngrenzen bewegt/geteilt werden darf.
unsafe impl<T: ?Sized + Send> Sync for RwSpinLock<T> {}
unsafe impl<T: ?Sized + Send> Send for RwSpinLock<T> {}

impl<T> RwSpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(0),
            writers_waiting: AtomicU32::new(0),
            data: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> RwSpinLock<T> {
    /// Geteilt (lesend) sperren. Blockiert nur, solange ein Schreiber hält oder
    /// angemeldet ist.
    pub fn read(&self) -> RwReadGuard<'_, T> {
        loop {
            // Angemeldeten Schreibern den Vortritt lassen (kein Writer-Starving).
            while self.writers_waiting.load(Ordering::Acquire) != 0 {
                core::hint::spin_loop();
            }
            // Optimistisch als Leser eintragen.
            let prev = self.state.fetch_add(1, Ordering::Acquire);
            if prev & RW_WRITER == 0 && self.writers_waiting.load(Ordering::Acquire) == 0 {
                return RwReadGuard { lock: self };
            }
            // Ein Schreiber kam dazwischen -> Eintrag zurücknehmen und erneut versuchen.
            self.state.fetch_sub(1, Ordering::Release);
        }
    }

    /// Exklusiv (schreibend) sperren.
    pub fn write(&self) -> RwWriteGuard<'_, T> {
        self.writers_waiting.fetch_add(1, Ordering::Acquire); // Intent -> Leser warten
        loop {
            // WRITER nur setzen, wenn weder Leser noch ein anderer Schreiber aktiv ist.
            if self
                .state
                .compare_exchange(0, RW_WRITER, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return RwWriteGuard { lock: self };
            }
            core::hint::spin_loop();
        }
    }
}

/// RAII-Guard für geteilten Lesezugriff (`Deref`, kein `DerefMut`).
pub struct RwReadGuard<'a, T: ?Sized> {
    lock: &'a RwSpinLock<T>,
}

impl<T: ?Sized> Deref for RwReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: Solange dieser Read-Guard lebt, ist das WRITER-Bit nicht gesetzt;
        // es existiert kein exklusiver Schreiber -> nur geteilter Lesezugriff (`&T`).
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwReadGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.state.fetch_sub(1, Ordering::Release);
    }
}

/// RAII-Guard für exklusiven Schreibzugriff (`Deref` + `DerefMut`).
pub struct RwWriteGuard<'a, T: ?Sized> {
    lock: &'a RwSpinLock<T>,
}

impl<T: ?Sized> Deref for RwWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: exklusiver Besitz durch das gesetzte WRITER-Bit; kein anderer Zugriff.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for RwWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: wie `deref`; exklusiver Besitz durch den lebenden Write-Guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwWriteGuard<'_, T> {
    fn drop(&mut self) {
        // NUR das WRITER-Bit löschen (nicht `store(0)`): ein Leser könnte transient
        // optimistisch hochgezählt haben und gleich wieder zurücknehmen; `fetch_and`
        // bewahrt eine solche Leserzahl -> kein Unterlauf.
        self.lock.state.fetch_and(!RW_WRITER, Ordering::Release);
        self.lock.writers_waiting.fetch_sub(1, Ordering::Release);
    }
}

// Formale Verifikation (Tier 1, Kani — bounded Model Checking). Nur unter `cargo kani` kompiliert,
// im Normal-Build inert. WICHTIG zur REICHWEITE: Kani ist ein **single-threaded** Modellprüfer und
// **kein** Nebenläufigkeits-Checker — die Kern-Aussage eines Locks (gegenseitiger Ausschluss unter
// *gleichzeitigem* Zugriff über Kerngrenzen, Interleavings) ist **außerhalb** von Kanis Reichweite
// (dafür bräuchte es Loom/TLA+, mögliche spätere Ergänzung). Kani beweist hier die single-threaded
// abgesicherten Eigenschaften: Memory-Safety der Guard-Derefs (kein UB), Lock/Unlock-Round-Trip +
// Daten-Persistenz, und die **Zähler-Arithmetik** (Reader-Count/Writer-Bit panik-/overflow-/
// underflow-frei, Zustands-Rückkehr auf „frei"). Unter dem Kani-Host-Target greifen die No-Op-
// IRQ-Stubs (kein DAIF-Asm).
#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use core::sync::atomic::Ordering::Relaxed;

    /// **BEWEIS (SpinLock, single-thread):** `lock()` liefert exklusiven Zugriff (Guard-Deref
    /// memory-safe), Schreiben+Lesen über den Guard ist konsistent, nach `Drop` ist der Lock wieder
    /// frei (Ticket-Zustand `next==serving`) und **erneut sperrbar**, die Daten persistieren.
    #[kani::proof]
    #[kani::unwind(2)]
    fn spinlock_roundtrip() {
        let lock = SpinLock::new(0u32);
        let v: u32 = kani::any();
        {
            let mut g = lock.lock();
            *g = v;
            assert!(*g == v); // Schreiben+Lesen über den Guard
        } // Drop -> freigeben
        // Ticket-Zustand zurück auf „frei": next_ticket == now_serving.
        assert!(lock.next_ticket.load(Relaxed) == lock.now_serving.load(Relaxed));
        {
            let g = lock.lock(); // erneut sperrbar
            assert!(*g == v); // Daten persistierten
        }
    }

    /// **BEWEIS (RwSpinLock, single-thread):** `write()` liefert exklusiven Schreibzugriff,
    /// anschließend sieht ein `read()` den geschriebenen Wert (Guard-Derefs memory-safe); nach allen
    /// Drops ist der Zustand wieder `0` (frei, kein hängendes WRITER-Bit, keine Leserzahl).
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_write_then_read() {
        let lock = RwSpinLock::new(0u32);
        let v: u32 = kani::any();
        {
            let mut g = lock.write();
            *g = v;
        }
        {
            let g = lock.read();
            assert!(*g == v);
        }
        assert!(lock.state.load(Relaxed) == 0);
        assert!(lock.writers_waiting.load(Relaxed) == 0);
    }

    /// **BEWEIS (RwSpinLock-Arithmetik, single-thread):** verschachtelte Leser zählen den Reader-Count
    /// korrekt hoch/runter (kein Overflow/Underflow), und ein Schreiber-Zyklus löscht **nur** das
    /// WRITER-Bit (`fetch_and`) ohne die Leserzahl zu beschädigen. Nach balancierten Acquire/Release
    /// ist der Zustand exakt `0`. (Genau die dokumentierte „kein-Unterlauf"-Invariante des Writer-Drops.)
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_state_arithmetic() {
        let lock = RwSpinLock::new(0u8);
        {
            let _r1 = lock.read();
            assert!(lock.state.load(Relaxed) == 1);
            let _r2 = lock.read(); // zwei gleichzeitige Leser (im selben Thread)
            assert!(lock.state.load(Relaxed) == 2);
        } // beide Drop -> Reader-Count zurück auf 0 (kein Underflow)
        assert!(lock.state.load(Relaxed) == 0);
        {
            let _w = lock.write();
            assert!(lock.state.load(Relaxed) == RW_WRITER); // nur WRITER-Bit, Leserzahl 0
        } // Drop: fetch_and(!WRITER) -> 0, writers_waiting -> 0
        assert!(lock.state.load(Relaxed) == 0);
        assert!(lock.writers_waiting.load(Relaxed) == 0);
    }
}
