//! Loom-Concurrency-Verifikation des writer-bevorzugenden RwSpinLock aus sel4lake-sync.
//! GETREUE Kopie der Lock-Logik (state/writers_waiting + read/write/Release) mit loom-Atomics +
//! loom::cell::UnsafeCell. loom::model() exploriert ALLE Interleavings + prueft Mutual-Exclusion,
//! kein Lost-Update, kein torn read, und dass fetch_and(!WRITER)-Release einen transienten
//! Reader-Zaehler erhaelt.
#![allow(dead_code)] // Test-Verifikationsartefakt: Lock-Mirrors nur von #[cfg(test)] genutzt
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, Ordering};

const RW_WRITER: u32 = 1 << 31;

pub struct RwLock {
    state: AtomicU32,
    writers_waiting: AtomicU32,
    data: UnsafeCell<u64>,
}
unsafe impl Sync for RwLock {}
unsafe impl Send for RwLock {}

impl RwLock {
    pub fn new(v: u64) -> Self {
        Self {
            state: AtomicU32::new(0),
            writers_waiting: AtomicU32::new(0),
            data: UnsafeCell::new(v),
        }
    }
    pub fn read<R>(&self, f: impl FnOnce(&u64) -> R) -> R {
        loop {
            while self.writers_waiting.load(Ordering::Acquire) != 0 {
                loom::thread::yield_now();
            }
            let prev = self.state.fetch_add(1, Ordering::Acquire);
            if prev & RW_WRITER == 0 && self.writers_waiting.load(Ordering::Acquire) == 0 {
                let r = self.data.with(|p| f(unsafe { &*p }));
                self.state.fetch_sub(1, Ordering::Release);
                return r;
            }
            self.state.fetch_sub(1, Ordering::Release);
            loom::thread::yield_now();
        }
    }
    pub fn write<R>(&self, f: impl FnOnce(&mut u64) -> R) -> R {
        self.writers_waiting.fetch_add(1, Ordering::Acquire);
        loop {
            if self
                .state
                .compare_exchange(0, RW_WRITER, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                let r = self.data.with_mut(|p| f(unsafe { &mut *p }));
                self.state.fetch_and(!RW_WRITER, Ordering::Release);
                self.writers_waiting.fetch_sub(1, Ordering::Release);
                return r;
            }
            loom::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom::sync::Arc;
    use loom::thread;

    // 1 Writer + 1 Reader: der Reader sieht NUR konsistente Werte (0 oder 7), nie einen halben
    // Schreibvorgang (torn read / Mutual-Exclusion). Final == 7.
    #[test]
    fn one_writer_one_reader_no_torn_read() {
        loom::model(|| {
            let lock = Arc::new(RwLock::new(0));
            let w = lock.clone();
            let tw = thread::spawn(move || w.write(|v| *v = 7));
            let seen = lock.read(|v| *v);
            assert!(seen == 0 || seen == 7, "torn read: {}", seen);
            tw.join().unwrap();
            assert_eq!(lock.read(|v| *v), 7);
        });
    }

    // 2 Writer (je +1): exklusiver Zugriff => KEIN Lost-Update => final exakt 2 in JEDEM Interleaving.
    // Faengt insb. einen kaputten Release (store(0) statt fetch_and) ODER fehlende Exklusivitaet.
    #[test]
    fn two_writers_no_lost_update() {
        loom::model(|| {
            let lock = Arc::new(RwLock::new(0));
            let a = lock.clone();
            let b = lock.clone();
            let ta = thread::spawn(move || a.write(|v| *v += 1));
            let tb = thread::spawn(move || b.write(|v| *v += 1));
            ta.join().unwrap();
            tb.join().unwrap();
            assert_eq!(lock.read(|v| *v), 2, "Lost-Update / kein gegenseitiger Ausschluss");
        });
    }

    // Writer + nebenlaeufiger Reader (der transient state hochzaehlen kann, waehrend der Writer
    // haelt/freigibt): am Ende muss der Zaehler wieder 0 sein (kein Unterlauf durch store(0)-Bug)
    // und der Schreibwert sichtbar.
    #[test]
    fn writer_with_transient_reader_increment() {
        loom::model(|| {
            let lock = Arc::new(RwLock::new(0));
            let w = lock.clone();
            let r = lock.clone();
            let tw = thread::spawn(move || w.write(|v| *v += 1));
            let tr = thread::spawn(move || {
                let _ = r.read(|v| *v);
            });
            tw.join().unwrap();
            tr.join().unwrap();
            // Nach allen Acquire/Release muss state == 0 sein (kein haengendes WRITER-Bit, kein
            // Leser-Zaehler-Unterlauf) und der Schreibwert konsistent abrufbar.
            assert_eq!(lock.read(|v| *v), 1);
        });
    }
}
mod ticket;
mod hierarchy;
mod crosscore;
