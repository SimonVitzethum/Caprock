//! Loom-Verifikation des Ticket-SpinLock aus sel4lake-sync (DAIF-IRQ-Maskierung orthogonal -> weg).
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, Ordering};

pub struct TicketLock {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
    data: UnsafeCell<u64>,
}
unsafe impl Sync for TicketLock {}
unsafe impl Send for TicketLock {}

impl TicketLock {
    pub fn new(v: u64) -> Self {
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
            data: UnsafeCell::new(v),
        }
    }
    pub fn lock_and<R>(&self, f: impl FnOnce(&mut u64) -> R) -> R {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.now_serving.load(Ordering::Acquire) != ticket {
            loom::thread::yield_now();
        }
        let r = self.data.with_mut(|p| f(unsafe { &mut *p }));
        self.now_serving.fetch_add(1, Ordering::Release);
        r
    }
}

#[cfg(test)]
mod ticket_tests {
    use super::*;
    use loom::sync::Arc;
    use loom::thread;

    // 2 Threads je +1: FIFO-Ticket-Lock => gegenseitiger Ausschluss => final exakt 2.
    #[test]
    fn ticket_mutual_exclusion() {
        loom::model(|| {
            let lock = Arc::new(TicketLock::new(0));
            let a = lock.clone();
            let b = lock.clone();
            let ta = thread::spawn(move || a.lock_and(|v| *v += 1));
            let tb = thread::spawn(move || b.lock_and(|v| *v += 1));
            ta.join().unwrap();
            tb.join().unwrap();
            assert_eq!(lock.lock_and(|v| *v), 2, "Ticket-Lock: Lost-Update / kein Ausschluss");
        });
    }
}
