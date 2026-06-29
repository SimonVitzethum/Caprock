//! Loom-Modell der GLOBALEN Lock-Hierarchie (invariants.md §1): R0 CAPS < R1 {EPS,DMA_CTX} <
//! R2 SCHEDS < R2.5 FP_STATES < R4 MEM. Verifiziert: die im Kernel belegten Schachtelungen sind
//! AUFSTEIGEND -> kein Zyklus -> Loom findet KEINEN Deadlock. Loom (loom::sync::Mutex) erkennt
//! einen Lock-Zyklus, falls einer existiert (Gegenprobe: eine Inversion DEADLOCKt nachweislich).
use loom::sync::Mutex;

/// Die rang-geordneten globalen Locks (je ein Repraesentant). Inhalt egal -> Mutex<()>.
struct Locks {
    caps: Mutex<()>,    // R0
    eps: Mutex<()>,     // R1
    dma_ctx: Mutex<()>, // R1
    scheds: Mutex<()>,  // R2
    fp: Mutex<()>,      // R2.5
    mem: Mutex<()>,     // R4
}
impl Locks {
    fn new() -> Self {
        Self {
            caps: Mutex::new(()),
            eps: Mutex::new(()),
            dma_ctx: Mutex::new(()),
            scheds: Mutex::new(()),
            fp: Mutex::new(()),
            mem: Mutex::new(()),
        }
    }
}

#[cfg(test)]
mod hierarchy_tests {
    use super::*;
    use loom::sync::Arc;
    use loom::thread;

    // Die belegten Kernel-Schachtelungen laufen NEBENLAEUFIG, alle aufsteigend:
    //   delete_leaf: CAPS -> MEM ;  attach: DMA_CTX -> MEM ;  fp_trap: SCHEDS -> FP_STATES.
    // CAPS->MEM und DMA_CTX->MEM konkurrieren um MEM (innerster), bilden aber KEINEN Zyklus.
    #[test]
    fn ascending_nestings_no_deadlock() {
        loom::model(|| {
            let l = Arc::new(Locks::new());
            let a = l.clone();
            let ta = thread::spawn(move || {
                let _c = a.caps.lock().unwrap(); // R0
                let _m = a.mem.lock().unwrap(); // R4  (CAPS -> MEM)
            });
            let b = l.clone();
            let tb = thread::spawn(move || {
                let _d = b.dma_ctx.lock().unwrap(); // R1
                let _m = b.mem.lock().unwrap(); // R4  (DMA_CTX -> MEM)
            });
            let c = l.clone();
            let tc = thread::spawn(move || {
                let _s = c.scheds.lock().unwrap(); // R2
                let _f = c.fp.lock().unwrap(); // R2.5 (SCHEDS -> FP_STATES)
            });
            ta.join().unwrap();
            tb.join().unwrap();
            tc.join().unwrap();
        });
    }

    // Disjunkte (nicht geschachtelte) Pfade: EPS dann frei, dann SCHEDS (R1 vor R2) — kein
    // gleichzeitiges Halten, daher trivial deadlock-frei, auch nebenlaeufig zu CAPS->MEM.
    #[test]
    fn disjoint_r1_then_r2_no_deadlock() {
        loom::model(|| {
            let l = Arc::new(Locks::new());
            let a = l.clone();
            let ta = thread::spawn(move || {
                {
                    let _e = a.eps.lock().unwrap();
                } // EPS freigegeben
                let _s = a.scheds.lock().unwrap(); // dann SCHEDS (R1 vor R2)
            });
            let b = l.clone();
            let tb = thread::spawn(move || {
                let _c = b.caps.lock().unwrap();
                let _m = b.mem.lock().unwrap();
            });
            ta.join().unwrap();
            tb.join().unwrap();
        });
    }
}
