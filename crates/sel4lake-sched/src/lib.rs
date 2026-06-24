#![no_std]
//! Deterministischer **per-Kern-paralleler** Scheduler (ADR 0005).
//!
//! Jede `Scheduler`-Instanz verwaltet **genau einen Kern**: seine eigene
//! TCB-Partition, Run-Queues (eine je Priorität), den laufenden Thread und seine
//! Zombies. Der Kernel hält ein Array solcher Instanzen — eine je Kern, jede hinter
//! einem **eigenen Lock** (`SpinLock`). Damit läuft der heiße Timer-Reschedule-Pfad
//! jedes Kerns **lock-frei gegenüber den anderen Kernen** (echte Parallelität);
//! kein globaler Scheduler-Lock mehr.
//!
//! **TCB-Partitionierung:** Der globale Slot-Raum ist statisch auf die Kerne
//! aufgeteilt — Kern `c` besitzt die Slots `[c*PER_CORE, (c+1)*PER_CORE)`. Eine
//! [`ThreadId`] trägt den **globalen** Slot; daraus ist der besitzende Kern
//! (`tid.core()`) ableitbar, ohne eine fremde Instanz zu sperren. Eine Instanz
//! berührt ausschließlich ihre eigenen Threads; kern-übergreifendes Aufwecken läuft
//! über das Sperren der Zielinstanz + einen Reschedule-IPI (Kernel-Schicht).
//!
//! Threads haben feste Kern-Affinität (keine automatische Migration). Der
//! Kontextwechsel ist ein reiner SP-Tausch im Trap-Pfad (siehe
//! `sel4lake-hal::exception`). Reine, sichere Index-Logik über feste Arrays —
//! **kein `unsafe`** (das Anlegen des initialen TrapFrames steckt in der HAL).

use sel4lake_hal::exception::init_thread_frame;

/// Kerne (eine Scheduler-Instanz je Kern).
pub const NUM_CORES: usize = 8;
/// TCB-Slots je Kern (Partitionsgröße). Großzügig, da die Demo viele (auch
/// dauerhaft geparkte) Threads auf core 0 erzeugt.
const PER_CORE: usize = 64;
/// Globaler Slot-Raum.
const NTHREADS: usize = NUM_CORES * PER_CORE;

/// Thread-Handle (globaler Slot-Index + Generation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThreadId {
    slot: usize,
    gen: u32,
}

impl ThreadId {
    /// In ein einzelnes `u64` packen (für die Ablage in einer Capability).
    pub fn to_raw(self) -> u64 {
        (self.slot as u64) | ((self.gen as u64) << 32)
    }
    /// Aus dem gepackten `u64` rekonstruieren.
    pub fn from_raw(raw: u64) -> Self {
        Self {
            slot: (raw & 0xffff_ffff) as usize,
            gen: (raw >> 32) as u32,
        }
    }
    /// Globaler Slot-Index (0..NTHREADS). Stabiler Index z. B. für per-Thread-
    /// Tabellen im Kernel (etwa den Lazy-FP-Kontextpuffer).
    pub fn slot(self) -> usize {
        self.slot
    }
    /// Der besitzende Kern dieses Threads (aus der statischen Partition).
    pub fn core(self) -> usize {
        self.slot / PER_CORE
    }
    /// Lokaler Index innerhalb der Kern-Partition.
    fn local(self) -> usize {
        self.slot % PER_CORE
    }
}

/// Anzahl der globalen Thread-Slots (Obergrenze für per-Thread-Tabellen im Kernel).
pub const MAX_THREADS: usize = NTHREADS;

/// Anzahl Prioritätsstufen (höher = wichtiger; 0 = niedrigste).
pub const NPRIO: usize = 8;

#[derive(Clone, Copy)]
struct Tcb {
    used: bool,
    gen: u32,
    /// Gesicherter SP (Zeiger auf den TrapFrame), gültig wenn der Thread *nicht* läuft.
    sp: usize,
    /// Priorität (0..NPRIO-1).
    priority: u8,
    /// True, wenn der Thread blockiert ist (nicht in der Ready-Queue).
    blocked: bool,
    /// Stack-Region des Threads (für die Rückgewinnung beim Beenden).
    stack_base: usize,
    stack_len: usize,
}

impl Tcb {
    const EMPTY: Tcb = Tcb {
        used: false,
        gen: 0,
        sp: 0,
        priority: 0,
        blocked: false,
        stack_base: 0,
        stack_len: 0,
    };
}

/// Aufgezeichneter Stack eines beendeten Threads, der noch freigegeben werden muss.
#[derive(Clone, Copy)]
struct Zombie {
    base: usize,
    len: usize,
}

/// FIFO-Ringpuffer lokaler Thread-Indizes (0..PER_CORE).
#[derive(Clone, Copy)]
struct RunQueue {
    buf: [usize; PER_CORE],
    head: usize,
    tail: usize,
    count: usize,
}

impl RunQueue {
    const EMPTY: RunQueue = RunQueue {
        buf: [0; PER_CORE],
        head: 0,
        tail: 0,
        count: 0,
    };

    fn enqueue(&mut self, tid: usize) {
        if self.count < PER_CORE {
            self.buf[self.tail] = tid;
            self.tail = (self.tail + 1) % PER_CORE;
            self.count += 1;
        }
    }

    fn dequeue(&mut self) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let tid = self.buf[self.head];
        self.head = (self.head + 1) % PER_CORE;
        self.count -= 1;
        Some(tid)
    }

    /// Einen bestimmten Eintrag entfernen (für `kill` eines bereiten Threads).
    fn remove(&mut self, target: usize) {
        let n = self.count;
        for _ in 0..n {
            if let Some(t) = self.dequeue() {
                if t != target {
                    self.enqueue(t);
                }
            }
        }
    }
}

/// Maximale Anzahl noch nicht eingesammelter (reaper-)Zombies je Kern.
const NZOMBIES: usize = 16;

/// Scheduler **eines Kerns**: TCB-Partition + Run-Queues + laufender Thread +
/// Zombies. Im Kernel je Kern eine Instanz hinter eigenem Lock.
pub struct Scheduler {
    /// Eigene Kern-ID (gesetzt durch [`bind_core`](Self::bind_core)/`init_core`);
    /// bestimmt den globalen Slot-Offset `core*PER_CORE`.
    core: usize,
    tcbs: [Tcb; PER_CORE],
    /// Laufender Thread (lokaler Index) oder `None` (vor `init_core`).
    current: Option<usize>,
    /// Eine Ready-Queue je Priorität (Round-Robin innerhalb einer Priorität).
    queues: [RunQueue; NPRIO],
    /// Bit `p` gesetzt, wenn `queues[p]` nicht leer ist (O(1)-Auswahl der höchsten).
    bitmap: u32,
    zombies: [Option<Zombie>; NZOMBIES],
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            core: 0,
            tcbs: [Tcb::EMPTY; PER_CORE],
            current: None,
            queues: [RunQueue::EMPTY; NPRIO],
            bitmap: 0,
            zombies: [None; NZOMBIES],
        }
    }

    /// Diese Instanz an Kern `core` binden (setzt den globalen Slot-Offset). Vom
    /// Bootkern für **alle** Instanzen aufzurufen, bevor irgendwo Threads erzeugt
    /// werden — auch für Kerne, deren `init_core` (Idle-Anlage) erst später auf dem
    /// jeweiligen Kern läuft.
    pub fn bind_core(&mut self, core: usize) {
        self.core = core;
    }

    /// Kern initialisieren: der gerade laufende Boot-Kontext wird zum Idle-Thread
    /// (sein SP wird beim ersten Tick gesichert). Auf dem jeweiligen Kern vor dem
    /// Aktivieren von IRQs aufzurufen.
    pub fn init_core(&mut self, core: usize, priority: u8) -> Option<ThreadId> {
        self.core = core;
        let idle = self.alloc_tcb(0, priority)?;
        self.current = Some(idle);
        Some(self.id(idle))
    }

    /// Einen neuen Thread auf diesem Kern mit `priority` erzeugen: initialen Kontext
    /// am Stack-Top anlegen und in die Ready-Queue seiner Priorität einreihen.
    /// `core` muss dem gebundenen Kern dieser Instanz entsprechen.
    pub fn spawn(
        &mut self,
        core: usize,
        entry: usize,
        arg: usize,
        stack_base: usize,
        stack_len: usize,
        priority: u8,
    ) -> Option<ThreadId> {
        debug_assert_eq!(core, self.core);
        let sp = init_thread_frame(stack_base + stack_len, entry, arg, false, 0);
        let t = self.alloc_tcb(sp, priority)?;
        self.tcbs[t].stack_base = stack_base;
        self.tcbs[t].stack_len = stack_len;
        self.enqueue_ready(t);
        Some(self.id(t))
    }

    /// Einen EL0-User-Thread erzeugen: Der initiale Frame liegt auf dem EL1-only
    /// Kernel-Stack `[kstack_base, kstack_base+kstack_len)`, der Thread läuft auf
    /// EL0 mit dem User-Stack `[user_base, user_base+user_len)`.
    ///
    /// Zum Reaping wird der **User-Stack** vermerkt (dynamisch alloziert,
    /// rückgebbar an den Allokator). Der Kernel-Stack stammt aus einem festen
    /// EL1-only Pool im Kernel-Image; er darf **nicht** an den Phys-Allokator
    /// zurückgegeben werden (würde eine Kernel-Image-Region einschleusen) und wird
    /// in Phase 1 geleakt (Pool ist klein und fest).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_user(
        &mut self,
        core: usize,
        entry: usize,
        arg: usize,
        kstack_base: usize,
        kstack_len: usize,
        user_base: usize,
        user_len: usize,
        priority: u8,
    ) -> Option<ThreadId> {
        debug_assert_eq!(core, self.core);
        let sp = init_thread_frame(kstack_base + kstack_len, entry, arg, true, user_base + user_len);
        let t = self.alloc_tcb(sp, priority)?;
        self.tcbs[t].stack_base = user_base;
        self.tcbs[t].stack_len = user_len;
        self.enqueue_ready(t);
        Some(self.id(t))
    }

    /// Der laufende Thread beendet sich selbst: Stack als Zombie vormerken und
    /// zum nächsten Thread wechseln. Der TCB-Slot + Stack werden später per
    /// [`reap`](Self::reap) eingesammelt (der Thread läuft noch auf seinem Stack,
    /// daher nicht sofort freigeben). Gibt den nächsten Frame zurück.
    pub fn exit_current(&mut self, core: usize, _frame: usize) -> usize {
        debug_assert_eq!(core, self.core);
        let cur = self.current.expect("kein laufender Thread");
        self.record_zombie(cur);
        let next = self
            .dequeue_highest()
            .expect("Idle-Thread sollte immer bereit sein");
        self.current = Some(next);
        self.tcbs[next].sp
    }

    /// Einen *nicht laufenden* Thread (blockiert/geparkt) **dieses Kerns** beenden.
    /// Sein Stack wird als Zombie vorgemerkt. Gibt `true`, falls der Thread gültig,
    /// auf diesem Kern und nicht der laufende war.
    pub fn kill(&mut self, tid: ThreadId, core: usize) -> bool {
        debug_assert_eq!(core, self.core);
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        if self.current == Some(s) {
            return false; // laufenden Thread nicht über kill beenden (nutze exit)
        }
        self.remove_from_ready(s);
        self.record_zombie(s);
        true
    }

    /// Einen Zombie dieses Kerns einsammeln: TCB-Slot freigeben und die Stack-Region
    /// zurückgeben, damit der Aufrufer (Kernel) sie dem Allokator zurückgibt.
    pub fn reap(&mut self) -> Option<(usize, usize)> {
        let i = self.zombies.iter().position(|z| z.is_some())?;
        let z = self.zombies[i].take().unwrap();
        Some((z.base, z.len))
    }

    /// Lastmaß dieses Kerns: Anzahl belegter TCB-Slots (laufend + bereit +
    /// blockiert/geparkt). Für die lastbewusste spawn-Platzierung.
    pub fn load(&self) -> usize {
        self.tcbs.iter().filter(|t| t.used).count()
    }

    /// Handle des aktuell laufenden Threads.
    pub fn current_id(&self, core: usize) -> ThreadId {
        debug_assert_eq!(core, self.core);
        let cur = self.current.expect("kein laufender Thread");
        self.id(cur)
    }

    /// Gesicherter Frame eines (blockierten) Threads **dieses Kerns** — für den
    /// IPC-Nachrichtentransfer. `None`, wenn der Thread nicht zu diesem Kern gehört.
    pub fn frame_of(&self, tid: ThreadId) -> Option<usize> {
        self.resolve(tid).map(|s| self.tcbs[s].sp)
    }

    /// Den laufenden Thread blockieren und zum nächsten *bereiten* Thread wechseln.
    pub fn block_current(&mut self, core: usize, frame: usize) -> usize {
        debug_assert_eq!(core, self.core);
        let cur = self.current.expect("kein laufender Thread");
        self.tcbs[cur].sp = frame;
        self.tcbs[cur].blocked = true;
        let next = self
            .dequeue_highest()
            .expect("Idle-Thread sollte immer bereit sein");
        self.current = Some(next);
        self.tcbs[next].sp
    }

    /// Den laufenden Thread blockieren und **direkt** zu `target` (zuvor blockiert,
    /// z. B. ein IPC-Partner auf demselben Kern) wechseln. Rendezvous-Fastpath.
    pub fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize {
        debug_assert_eq!(core, self.core);
        let cur = self.current.expect("kein laufender Thread");
        self.tcbs[cur].sp = frame;
        self.tcbs[cur].blocked = true;
        let t = self.resolve(target).expect("Zielthread ungültig/fremder Kern");
        self.tcbs[t].blocked = false;
        self.current = Some(t);
        self.tcbs[t].sp
    }

    /// Einen blockierten Thread **dieses Kerns** wieder bereit machen. Kern-
    /// übergreifend ruft der Kernel dies auf der Zielinstanz auf (+ Reschedule-IPI).
    ///
    /// Idempotent: wirkt **nur**, wenn der Thread tatsächlich blockiert ist. Ein
    /// Aufruf auf einen laufenden/bereiten Thread ist ein No-Op (verhindert
    /// Doppel-Einreihung) — wichtig, wenn `wake_remote` einen Thread trifft, der
    /// gerade erst dabei ist, sich zu blockieren; ein erneuter Aufruf weckt ihn dann.
    pub fn unblock(&mut self, tid: ThreadId) {
        if let Some(s) = self.resolve(tid) {
            if self.tcbs[s].blocked {
                self.tcbs[s].blocked = false;
                self.enqueue_ready(s);
            }
        }
    }

    fn resolve(&self, tid: ThreadId) -> Option<usize> {
        if tid.core() != self.core {
            return None;
        }
        let local = tid.local();
        if self.tcbs[local].used && self.tcbs[local].gen == tid.gen {
            Some(local)
        } else {
            None
        }
    }

    /// Timer-Tick (oder Reschedule-IPI): aktuellen Frame sichern, rundlaufend den
    /// nächsten Thread wählen und dessen Frame zur Wiederherstellung zurückgeben.
    pub fn on_tick(&mut self, core: usize, frame: usize) -> usize {
        debug_assert_eq!(core, self.core);
        if let Some(cur) = self.current {
            self.tcbs[cur].sp = frame;
            self.enqueue_ready(cur);
        }
        match self.dequeue_highest() {
            Some(next) => {
                self.current = Some(next);
                self.tcbs[next].sp
            }
            None => frame, // nichts lauffähig (sollte nicht vorkommen: Idle ist immer dabei)
        }
    }

    // --- intern ---

    /// Einen Thread (lokaler Index) in die Ready-Queue seiner Priorität einreihen.
    fn enqueue_ready(&mut self, local: usize) {
        let p = self.tcbs[local].priority as usize;
        self.queues[p].enqueue(local);
        self.bitmap |= 1 << p;
    }

    /// Einen bereiten Thread aus seiner Prioritäts-Queue entfernen (für `kill`).
    fn remove_from_ready(&mut self, local: usize) {
        let p = self.tcbs[local].priority as usize;
        self.queues[p].remove(local);
        if self.queues[p].count == 0 {
            self.bitmap &= !(1 << p);
        }
    }

    /// Einen beendeten Thread aufzeichnen: TCB-Slot sofort freigeben (Generation
    /// erhöhen), Stack-Region zum späteren Freigeben (`reap`) vormerken.
    fn record_zombie(&mut self, local: usize) {
        let base = self.tcbs[local].stack_base;
        let len = self.tcbs[local].stack_len;
        let gen = self.tcbs[local].gen.wrapping_add(1);
        self.tcbs[local] = Tcb::EMPTY;
        self.tcbs[local].gen = gen;
        if len > 0 {
            if let Some(z) = self.zombies.iter_mut().find(|z| z.is_none()) {
                *z = Some(Zombie { base, len });
            }
        }
    }

    /// Den nächsten Thread der höchsten nichtleeren Priorität entnehmen (O(1)).
    fn dequeue_highest(&mut self) -> Option<usize> {
        if self.bitmap == 0 {
            return None;
        }
        let p = (31 - self.bitmap.leading_zeros()) as usize; // höchstes gesetztes Bit
        let tid = self.queues[p].dequeue();
        if self.queues[p].count == 0 {
            self.bitmap &= !(1 << p);
        }
        tid
    }

    fn alloc_tcb(&mut self, sp: usize, priority: u8) -> Option<usize> {
        let i = self.tcbs.iter().position(|t| !t.used)?;
        let gen = self.tcbs[i].gen;
        self.tcbs[i] = Tcb {
            used: true,
            gen,
            sp,
            priority,
            blocked: false,
            stack_base: 0,
            stack_len: 0,
        };
        Some(i)
    }

    /// Globale `ThreadId` aus einem lokalen Slot-Index dieses Kerns.
    fn id(&self, local: usize) -> ThreadId {
        ThreadId {
            slot: self.core * PER_CORE + local,
            gen: self.tcbs[local].gen,
        }
    }
}

/// Scheduler-Operationen, wie sie der IPC-/Dispatch-Pfad braucht — abstrahiert von
/// der konkreten Instanz, damit der Kernel **kern-übergreifend** auflösen kann
/// (z. B. einen IPC-Partner auf einem anderen Kern wecken). Bei einer Einkern-Sicht
/// genügt eine `Scheduler`-Instanz; der Kernel stellt eine Facade über alle
/// per-Kern-Instanzen bereit, die je Operation **genau eine** Instanz sperrt
/// (nie zwei gleichzeitig) und beim kern-übergreifenden Wecken einen Reschedule-IPI
/// schickt.
///
/// `current_id`/`block_current`/`switch_to`/`exit_current`/`on_tick`/`kill` beziehen
/// sich auf den **aktuellen** Kern (`core`); `frame_of`/`unblock` dürfen einen
/// Thread auf **irgendeinem** Kern betreffen (kern-übergreifender IPC-Partner).
pub trait SchedOps {
    fn current_id(&mut self, core: usize) -> ThreadId;
    fn frame_of(&mut self, tid: ThreadId) -> Option<usize>;
    fn block_current(&mut self, core: usize, frame: usize) -> usize;
    fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize;
    fn unblock(&mut self, tid: ThreadId);
    fn on_tick(&mut self, core: usize, frame: usize) -> usize;
    fn exit_current(&mut self, core: usize, frame: usize) -> usize;
    fn kill(&mut self, tid: ThreadId, core: usize) -> bool;
    /// Einen physischen Frame `[base, base+len)` in die VSpace des Aufrufers `caller`
    /// mappen (cap-gated; nur für isolierte PDs sinnvoll). `perm_code`: 0=Ro, 1=Rw,
    /// 2=Rx (aus den Cap-Rechten abgeleitet). Granularität nach `len` (2 MiB / 4 KiB).
    fn map_frame(&mut self, caller: ThreadId, base: u64, len: u64, perm_code: u8) -> bool;
    /// Einen zuvor gemappten Frame wieder aus der VSpace des Aufrufers entfernen.
    fn unmap_frame(&mut self, caller: ThreadId, base: u64, len: u64) -> bool;
}
