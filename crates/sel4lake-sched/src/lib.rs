#![no_std]
//! Deterministischer Per-Kern-Scheduler (ADR 0005).
//!
//! Phase 4: präemptiver **Round-Robin** je Kern, getrieben vom Timer-Tick.
//! Jeder Kern hat eine eigene Run-Queue; Threads haben feste Kern-Affinität
//! (keine automatische Migration). Ein Thread-Control-Block speichert lediglich
//! den gesicherten Stack-Pointer (der volle Registerkontext liegt als TrapFrame
//! auf dem Thread-Stack); der Kontextwechsel selbst ist ein reiner SP-Tausch im
//! Trap-Pfad (siehe `sel4lake-hal::exception`).
//!
//! Reine, sichere Index-Logik über feste Arrays — **kein `unsafe`** (der einzige
//! unsafe-Anteil, das Anlegen des initialen TrapFrames, steckt gekapselt in
//! `sel4lake-hal`).
//!
//! Prioritäten/Bitmap (ADR 0005) sind als nächste Verfeinerung vorgesehen;
//! Phase 4 schedult reines Round-Robin.

use sel4lake_hal::exception::init_thread_frame;

const NTHREADS: usize = 64;
const NUM_CORES: usize = 8;

/// Thread-Handle (Slot-Index + Generation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadId {
    slot: usize,
    gen: u32,
}

/// Anzahl Prioritätsstufen (höher = wichtiger; 0 = niedrigste).
pub const NPRIO: usize = 8;

#[derive(Clone, Copy)]
struct Tcb {
    used: bool,
    gen: u32,
    /// Gesicherter SP (Zeiger auf den TrapFrame), gültig wenn der Thread *nicht* läuft.
    sp: usize,
    /// Kern, dem dieser Thread fest zugeordnet ist (Affinität).
    core: usize,
    /// Priorität (0..NPRIO-1).
    priority: u8,
    /// True, wenn der Thread blockiert ist (nicht in der Ready-Queue).
    blocked: bool,
}

impl Tcb {
    const EMPTY: Tcb = Tcb {
        used: false,
        gen: 0,
        sp: 0,
        core: 0,
        priority: 0,
        blocked: false,
    };
}

/// FIFO-Ringpuffer von Thread-Indizes.
#[derive(Clone, Copy)]
struct RunQueue {
    buf: [usize; NTHREADS],
    head: usize,
    tail: usize,
    count: usize,
}

impl RunQueue {
    const EMPTY: RunQueue = RunQueue {
        buf: [0; NTHREADS],
        head: 0,
        tail: 0,
        count: 0,
    };

    fn enqueue(&mut self, tid: usize) {
        if self.count < NTHREADS {
            self.buf[self.tail] = tid;
            self.tail = (self.tail + 1) % NTHREADS;
            self.count += 1;
        }
    }

    fn dequeue(&mut self) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let tid = self.buf[self.head];
        self.head = (self.head + 1) % NTHREADS;
        self.count -= 1;
        Some(tid)
    }
}

#[derive(Clone, Copy)]
struct CoreSched {
    current: Option<usize>,
    /// Eine Ready-Queue je Priorität (Round-Robin innerhalb einer Priorität).
    queues: [RunQueue; NPRIO],
    /// Bit `p` gesetzt, wenn `queues[p]` nicht leer ist (O(1)-Auswahl der höchsten).
    bitmap: u32,
}

impl CoreSched {
    const EMPTY: CoreSched = CoreSched {
        current: None,
        queues: [RunQueue::EMPTY; NPRIO],
        bitmap: 0,
    };
}

/// Globaler Scheduler-Zustand: eine TCB-Tabelle + Per-Kern-Run-Queues.
pub struct Scheduler {
    tcbs: [Tcb; NTHREADS],
    cores: [CoreSched; NUM_CORES],
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tcbs: [Tcb::EMPTY; NTHREADS],
            cores: [CoreSched::EMPTY; NUM_CORES],
        }
    }

    /// Kern initialisieren: der gerade laufende Boot-Kontext wird zum Idle-Thread
    /// (sein SP wird beim ersten Tick gesichert). Vor dem Aktivieren von IRQs
    /// aufzurufen.
    pub fn init_core(&mut self, core: usize, priority: u8) -> Option<ThreadId> {
        let idle = self.alloc_tcb(0, core, priority)?;
        self.cores[core].current = Some(idle);
        Some(self.id(idle))
    }

    /// Einen neuen Thread auf `core` mit `priority` erzeugen: initialen Kontext am
    /// Stack-Top anlegen und in die Ready-Queue seiner Priorität einreihen.
    pub fn spawn(
        &mut self,
        core: usize,
        entry: usize,
        arg: usize,
        stack_top: usize,
        priority: u8,
    ) -> Option<ThreadId> {
        let sp = init_thread_frame(stack_top, entry, arg);
        let t = self.alloc_tcb(sp, core, priority)?;
        self.enqueue_ready(t);
        Some(self.id(t))
    }

    /// Handle des aktuell laufenden Threads auf `core`.
    pub fn current_id(&self, core: usize) -> ThreadId {
        let cur = self.cores[core].current.expect("kein laufender Thread");
        self.id(cur)
    }

    /// Gesicherter Frame eines (blockierten) Threads — für IPC-Nachrichtentransfer.
    pub fn frame_of(&self, tid: ThreadId) -> Option<usize> {
        self.resolve(tid).map(|s| self.tcbs[s].sp)
    }

    /// Den laufenden Thread blockieren und zum nächsten *bereiten* Thread wechseln.
    /// Gibt dessen Frame zurück.
    pub fn block_current(&mut self, core: usize, frame: usize) -> usize {
        let cur = self.cores[core].current.expect("kein laufender Thread");
        self.tcbs[cur].sp = frame;
        self.tcbs[cur].blocked = true;
        let next = self
            .dequeue_highest(core)
            .expect("Idle-Thread sollte immer bereit sein");
        self.cores[core].current = Some(next);
        self.tcbs[next].sp
    }

    /// Den laufenden Thread blockieren und **direkt** zu `target` wechseln (das
    /// zuvor außerhalb der Ready-Queue blockiert war, z. B. ein IPC-Partner).
    /// Niedrige Latenz (Rendezvous-Fastpath). Gibt `target`s Frame zurück.
    pub fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize {
        let cur = self.cores[core].current.expect("kein laufender Thread");
        self.tcbs[cur].sp = frame;
        self.tcbs[cur].blocked = true;
        let t = self.resolve(target).expect("Zielthread ungültig");
        self.tcbs[t].blocked = false;
        self.cores[core].current = Some(t);
        self.tcbs[t].sp
    }

    /// Einen blockierten Thread wieder bereit machen (in die Run-Queue seines Kerns).
    pub fn unblock(&mut self, tid: ThreadId) {
        if let Some(s) = self.resolve(tid) {
            self.tcbs[s].blocked = false;
            self.enqueue_ready(s);
        }
    }

    fn resolve(&self, tid: ThreadId) -> Option<usize> {
        if tid.slot < NTHREADS && self.tcbs[tid.slot].used && self.tcbs[tid.slot].gen == tid.gen {
            Some(tid.slot)
        } else {
            None
        }
    }

    /// Timer-Tick auf `core`: aktuellen Frame sichern, rundlaufend den nächsten
    /// Thread wählen und dessen Frame zur Wiederherstellung zurückgeben.
    pub fn on_tick(&mut self, core: usize, frame: usize) -> usize {
        if let Some(cur) = self.cores[core].current {
            self.tcbs[cur].sp = frame;
            self.enqueue_ready(cur);
        }
        match self.dequeue_highest(core) {
            Some(next) => {
                self.cores[core].current = Some(next);
                self.tcbs[next].sp
            }
            None => frame, // nichts lauffähig (sollte nicht vorkommen: Idle ist immer dabei)
        }
    }

    // --- intern ---

    /// Einen Thread in die Ready-Queue seiner Priorität einreihen + Bitmap setzen.
    fn enqueue_ready(&mut self, tid: usize) {
        let core = self.tcbs[tid].core;
        let p = self.tcbs[tid].priority as usize;
        self.cores[core].queues[p].enqueue(tid);
        self.cores[core].bitmap |= 1 << p;
    }

    /// Den nächsten Thread der höchsten nichtleeren Priorität entnehmen (O(1)).
    fn dequeue_highest(&mut self, core: usize) -> Option<usize> {
        let bm = self.cores[core].bitmap;
        if bm == 0 {
            return None;
        }
        let p = (31 - bm.leading_zeros()) as usize; // höchstes gesetztes Bit
        let tid = self.cores[core].queues[p].dequeue();
        if self.cores[core].queues[p].count == 0 {
            self.cores[core].bitmap &= !(1 << p);
        }
        tid
    }

    fn alloc_tcb(&mut self, sp: usize, core: usize, priority: u8) -> Option<usize> {
        let i = self.tcbs.iter().position(|t| !t.used)?;
        let gen = self.tcbs[i].gen;
        self.tcbs[i] = Tcb {
            used: true,
            gen,
            sp,
            core,
            priority,
            blocked: false,
        };
        Some(i)
    }

    fn id(&self, slot: usize) -> ThreadId {
        ThreadId {
            slot,
            gen: self.tcbs[slot].gen,
        }
    }
}
