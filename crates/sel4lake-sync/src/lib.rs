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
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.now_serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
        SpinGuard { lock: self }
    }
}

/// RAII-Guard: hält den Lock, gibt bei `Drop` frei.
pub struct SpinGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
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
        // unter dem Lock an den nächsten Halter.
        self.lock.now_serving.fetch_add(1, Ordering::Release);
    }
}
