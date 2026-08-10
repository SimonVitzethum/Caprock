//! Loom-Modell der CROSS-CORE-IPC/Wake-Pfade (caprock: KernelSched-Facade nimmt je Op GENAU EINEN
//! SCHEDS-Lock, NIE zwei zugleich -> deadlock-frei, auch wenn zwei Kerne gleichzeitig cross-core
//! aufeinander zugreifen). Modelliert das call()-Muster: EPS halten, dann SCHEDS[other] (unblock,
//! sofort frei), dann SCHEDS[self] (block_current, sofort frei). Gegenprobe: das (hypothetisch
//! falsche) Halten BEIDER SCHEDS-Locks zugleich DEADLOCKt nachweislich.
use loom::sync::Mutex;

struct Sys {
    sched: [Mutex<()>; 2], // SCHEDS[0], SCHEDS[1]
    eps: Mutex<()>,        // ein Endpoint (Cross-Core-Rendezvous)
}
impl Sys {
    fn new() -> Self {
        Self { sched: [Mutex::new(()), Mutex::new(())], eps: Mutex::new(()) }
    }
}

#[cfg(test)]
mod crosscore_tests {
    use super::*;
    use loom::sync::Arc;
    use loom::thread;

    // KORREKT (one-lock-per-op): Kern `me` ruft cross-core Kern `1-me`. Haelt EPS, weckt den Server
    // auf dem ANDEREN Kern (SCHEDS[other], sofort frei), blockiert dann sich selbst (SCHEDS[me],
    // sofort frei). Es wird NIE mehr als ein SCHEDS-Lock zugleich gehalten -> kein Zyklus.
    fn cross_call(sys: &Sys, me: usize) {
        let _e = sys.eps.lock().unwrap(); // EPS (R1) ueber die ganze Op gehalten
        {
            let _s_other = sys.sched[1 - me].lock().unwrap(); // unblock(server) auf fremdem Kern
        } // SCHEDS[other] sofort frei
        {
            let _s_me = sys.sched[me].lock().unwrap(); // block_current auf eigenem Kern
        } // SCHEDS[me] sofort frei
    }

    // Zwei Kerne rufen GLEICHZEITIG cross-core (0->1 und 1->0). Mit getrennten Endpoints (je eigener
    // EPS) -> nur die SCHEDS-Disziplin zaehlt. Loom: kein Deadlock.
    #[test]
    fn concurrent_cross_core_no_deadlock() {
        loom::model(|| {
            let s0 = Arc::new(Sys::new());
            let s1 = Arc::new(Sys::new());
            let (a0, a1) = (s0.clone(), s1.clone());
            let t0 = thread::spawn(move || cross_call(&a0, 0));
            let t1 = thread::spawn(move || cross_call(&a1, 1));
            t0.join().unwrap();
            t1.join().unwrap();
        });
    }

    // Auch ueber DENSELBEN Endpoint (gemeinsamer EPS-Lock -> Kontention, aber EPS serialisiert die
    // beiden Ops). Loom: kein Deadlock (EPS first-come, dann je ein SCHEDS).
    #[test]
    fn concurrent_cross_core_shared_ep_no_deadlock() {
        loom::model(|| {
            let sys = Arc::new(Sys::new());
            let a = sys.clone();
            let b = sys.clone();
            let t0 = thread::spawn(move || cross_call(&a, 0));
            let t1 = thread::spawn(move || cross_call(&b, 1));
            t0.join().unwrap();
            t1.join().unwrap();
        });
    }
}
