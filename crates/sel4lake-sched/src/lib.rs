#![no_std]
//! Deterministischer **per-Kern-paralleler** Scheduler mit **Thread-Migration** (ADR 0005).
//!
//! Jede `Scheduler`-Instanz verwaltet **genau einen Kern**: seine TCB-Tabelle, Run-Queues,
//! den laufenden Thread und seine Zombies. Der Kernel hält ein Array solcher Instanzen —
//! eine je Kern, jede hinter einem **eigenen Lock**. Damit läuft der heiße
//! Timer-Reschedule-Pfad jedes Kerns **lock-frei gegenüber den anderen Kernen**.
//!
//! ## Identität ohne Kern-Affinität (ext-30)
//!
//! Früher kodierte die [`ThreadId`] den besitzenden Kern (`slot / PER_CORE`) — die
//! **Identität** eines Threads hing damit an seiner **Platzierung**, und Migration war
//! strukturell unmöglich. Jetzt gibt es eine Indirektion:
//!
//! ```text
//!   ThreadId{ gid, gen } ──> Thread-Directory[gid] = (used, gen, core, local)
//!                                                          │      │
//!                                    besitzender Kern ─────┘      └──> Index in dessen TCB-Tabelle
//! ```
//!
//! Das Directory ist eine Tabelle von Atomics: **lock-frei lesbar** von jedem Kern (der
//! Kernel muss wissen, welchen Scheduler er sperren soll, *bevor* er sperrt), und beim
//! Migrieren unter **beiden** Kern-Locks mit einem atomaren Store umgehängt. Die `gid`
//! bleibt lebenslang stabil — Tcb-Caps, `VSPACE_OF`, FP-Kontexte und Kernel-Stack-Slots
//! sind darüber indiziert und überleben eine Migration unverändert.
//!
//! Weil ein Leser zwischen „Directory lesen" und „Kern sperren" überholt werden kann
//! (der Thread migriert genau dann), MUSS jeder kernübergreifende Zugriff nach dem Sperren
//! **erneut prüfen** — `resolve` schlägt dann fehl und der Aufrufer wiederholt.
//!
//! ## Kapazität zur Boot-Zeit (ext-30)
//!
//! TCB-Tabelle, Freilisten, Zombie-Ring und Directory sind [`Slab`]s: die Struktur ist
//! `const` konstruierbar (statische Instanzen), der Speicher kommt beim Boot aus dem RAM —
//! dimensioniert nach Kernzahl und RAM, nicht nach einer Compile-Zeit-Konstante.
//!
//! ## O(1) statt linearer Scans (ext-30)
//!
//! Bei tausenden Threads je Kern sind lineare Scans der Engpass. Daher: Ready-Queues sind
//! **intrusive doppelt verkettete Listen** durch die TCBs (Einreihen/Entfernen O(1), kein
//! Ringpuffer je Priorität), freie TCB-Slots kommen aus einer **Freiliste** (O(1) statt
//! „ersten freien suchen"), `load()` ist ein Zähler, und der MCS-Refill-Scan entfällt
//! komplett, solange kein Budget erschöpft ist.
//!
//! Reine, sichere Index-Logik — **kein `unsafe`** außer den Boot-`attach`-Aufrufen (die
//! Rohspeicher-Domäne liegt in `sel4lake-slab`).

use core::sync::atomic::{AtomicU64, Ordering};
use sel4lake_hal::exception::init_thread_frame;
use sel4lake_slab::{AtomicTable, FreeList, Slab};
use sel4lake_sync::SpinLock;

/// Compile-Zeit-Obergrenze der Kernzahl (dimensioniert nur die Arrays von *Locks*, nicht
/// die Datentabellen). Die **tatsächliche** Kernzahl wird beim Boot ermittelt.
pub const MAX_CORES: usize = 256;

/// Anzahl Prioritätsstufen (höher = wichtiger; 0 = niedrigste).
pub const NPRIO: usize = 8;

/// Sentinel „kein Index" in den intrusiven Listen/Freilisten.
const NIL: u32 = u32::MAX;
/// Sentinel „in keiner Ready-Queue" (`queued`-Feld).
const NOT_QUEUED: u8 = 0xff;

// ---------------------------------------------------------------------------------------
// Thread-Directory: gid -> (used, gen, core, local)
// ---------------------------------------------------------------------------------------

/// Packung eines Directory-Eintrags in ein `u64`:
/// `used(1) | gen(31) | core(16) | local(16)`.
const D_USED: u64 = 1 << 63;
const D_GEN_SHIFT: u32 = 32;
const D_GEN_MASK: u64 = 0x7fff_ffff;
const D_CORE_SHIFT: u32 = 16;
const D_CORE_MASK: u64 = 0xffff;
const D_LOCAL_MASK: u64 = 0xffff;

const fn pack_dir(used: bool, gen: u32, core: usize, local: usize) -> u64 {
    (if used { D_USED } else { 0 })
        | (((gen as u64) & D_GEN_MASK) << D_GEN_SHIFT)
        | (((core as u64) & D_CORE_MASK) << D_CORE_SHIFT)
        | ((local as u64) & D_LOCAL_MASK)
}

/// Das globale Thread-Directory (lock-frei lesbar).
static DIRECTORY: AtomicTable<AtomicU64> = AtomicTable::empty();
/// Freiliste der `gid`s. **Leaf-Lock:** wird nie gehalten, während ein anderer Lock
/// genommen wird. Er wird unter `SCHEDS[core]` genommen (Spawn) bzw. ganz ohne weiteren
/// Lock (Reap) — nie umgekehrt.
static GID_FREE: SpinLock<FreeList> = SpinLock::new(FreeList::empty());

/// Bytes für ein Directory mit `capacity` Thread-Slots (Einträge + Freiliste).
pub const fn directory_bytes(capacity: usize) -> usize {
    capacity * core::mem::size_of::<AtomicU64>() + capacity * core::mem::size_of::<u32>()
}

/// Ausrichtung, die [`attach_directory`] erwartet.
pub const fn directory_align() -> usize {
    core::mem::align_of::<AtomicU64>()
}

/// Das Thread-Directory beim Boot anlegen (**einmalig**, vor dem Erzeugen von Threads und
/// vor dem Start der Sekundärkerne).
///
/// # Safety
/// `mem` zeigt auf mindestens [`directory_bytes(capacity)`](directory_bytes) Bytes, ist auf
/// [`directory_align`] ausgerichtet, exklusiv für den Kernel und lebt bis zum Reboot.
pub unsafe fn attach_directory(mem: *mut u8, capacity: usize) {
    debug_assert!(mem as usize % directory_align() == 0);
    let entries = mem as *mut AtomicU64;
    // Freiliste direkt hinter den Einträgen (u32 braucht 4er-Ausrichtung; der Offset ist
    // ein Vielfaches von 8 -> passt).
    // SAFETY: Vertrag des Aufrufers (Größe/Ausrichtung/Exklusivität); beide Abschnitte
    // liegen disjunkt in derselben Zuteilung.
    unsafe {
        let free_next = mem.add(capacity * core::mem::size_of::<AtomicU64>()) as *mut u32;
        DIRECTORY.attach(entries, capacity, |_| AtomicU64::new(0));
        GID_FREE.lock().attach(free_next, capacity);
    }
}

/// Anzahl der Thread-Slots insgesamt (Directory-Kapazität) — Obergrenze für per-Thread-
/// Tabellen des Kernels (FP-Kontexte, `VSPACE_OF`, Kernel-Stack-Zuordnung).
pub fn thread_capacity() -> usize {
    DIRECTORY.len()
}

/// Anzahl noch freier Thread-Slots (Telemetrie/Tests).
pub fn threads_available() -> usize {
    GID_FREE.lock().available()
}

fn dir_load(gid: usize) -> Option<u64> {
    DIRECTORY.get(gid).map(|e| e.load(Ordering::Acquire))
}

/// **Besitzender Kern** eines Threads — lock-frei.
///
/// Der Wert kann bereits veraltet sein, wenn der Aufrufer den Lock nimmt (der Thread kann
/// genau dann migrieren). Der Aufrufer MUSS nach dem Sperren erneut prüfen (`resolve`
/// schlägt dann fehl) und es mit dem neuen Besitzer wiederholen.
pub fn owner_core(tid: ThreadId) -> Option<usize> {
    let e = dir_load(tid.slot)?;
    if e & D_USED == 0 || ((e >> D_GEN_SHIFT) & D_GEN_MASK) as u32 != tid.gen {
        return None;
    }
    Some(((e >> D_CORE_SHIFT) & D_CORE_MASK) as usize)
}

/// Lebt dieser Thread noch (gültiger Directory-Eintrag)? Lock-frei.
pub fn is_live(tid: ThreadId) -> bool {
    owner_core(tid).is_some()
}

/// Eine `gid` nach dem Einsammeln des Threads wieder freigeben (aus dem Reap-Pfad,
/// **ohne** gehaltenen Scheduler-Lock).
pub fn release_gid(gid: u32) {
    GID_FREE.lock().free(gid as usize);
}

// ---------------------------------------------------------------------------------------
// ThreadId
// ---------------------------------------------------------------------------------------

/// Thread-Handle: **globaler, migrationsstabiler** Slot (`gid`) + Generation.
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
    /// Globaler Thread-Slot (`gid`): stabiler Index für per-Thread-Tabellen des Kernels.
    /// **Überlebt eine Migration** (anders als früher kodiert er den Kern NICHT mehr).
    pub fn slot(self) -> usize {
        self.slot
    }
}

// ---------------------------------------------------------------------------------------
// TCB
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Tcb {
    used: bool,
    /// Globaler Thread-Slot dieses TCBs (Directory-Index).
    gid: u32,
    /// Generation (Kopie des Directory-Eintrags; erkennt stale Handles).
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
    // --- intrusive Verkettung (ext-30) ---
    /// Priorität der Queue, in der der Thread hängt, oder [`NOT_QUEUED`].
    queued: u8,
    /// Nachfolger/Vorgänger in der Ready-Queue bzw. Nachfolger in der Freiliste.
    qnext: u32,
    qprev: u32,
    // --- MCS Scheduling Context (ADR 0005) ---
    /// Budget in Ticks je Periode. `0` = unbeschränkt (klassisches Round-Robin).
    budget: u32,
    /// Periodenlänge in Ticks (Refill-Intervall). Nur relevant bei `budget > 0`.
    period: u32,
    /// Verbleibende Ticks in der aktuellen Periode.
    remaining: u32,
    /// Per-Kern-Tick, ab dem das Budget wieder aufgefüllt wird (bei Erschöpfung gesetzt).
    next_refill: u64,
    /// True, wenn das Budget erschöpft ist (Thread weder laufend noch in Ready-Queue,
    /// wartet auf Refill).
    depleted: bool,
    // --- Budget-Donation (MCS, intra-core IPC): bei einem CALL leiht der Aufrufer dem
    // Server seinen Scheduling-Context; der Server wird gegen das **Konto** des Aufrufers
    // belastet (geteilter SC), bis er antwortet. So begrenzt das Budget des Aufrufers die
    // Server-Arbeit. Cross-core-Calls (kein switch_to-Fastpath) spenden nicht.
    //
    // Die Links sind **lokale** Slots -> eine Donation ist immer intra-core. Ein Thread mit
    // aktiver Donation wird deshalb NICHT migriert (s. `detach_for_migration`). ---
    /// Lokaler Slot des Kontos, gegen das dieser Thread belastet wird (`None` = eigenes).
    sc_donor: Option<usize>,
    /// Lokaler Slot des Threads, der gerade gegen dieses Konto läuft (`None` = keiner).
    sc_donee: Option<usize>,
}

impl Tcb {
    const EMPTY: Tcb = Tcb {
        used: false,
        gid: NIL,
        gen: 0,
        sp: 0,
        priority: 0,
        blocked: false,
        stack_base: 0,
        stack_len: 0,
        queued: NOT_QUEUED,
        qnext: NIL,
        qprev: NIL,
        budget: 0,
        period: 0,
        remaining: 0,
        next_refill: 0,
        depleted: false,
        sc_donor: None,
        sc_donee: None,
    };
}

/// Ein zur Migration aus einem Kern **herausgelöster** Thread. Undurchsichtiges Token:
/// nur [`Scheduler::detach_for_migration`] erzeugt es, nur
/// [`Scheduler::attach_migrated`] nimmt es an. Zwischen beiden Aufrufen existiert der
/// Thread in **keiner** Kern-Tabelle — deshalb müssen beide Kern-Locks über die gesamte
/// Übergabe gehalten werden (der Kernel sperrt sie in aufsteigender Kern-Ordnung).
pub struct Migrant {
    tcb: Tcb,
    /// War der Thread bereit (in einer Ready-Queue)? Dann auf dem Zielkern wieder einreihen.
    was_ready: bool,
}

impl Migrant {
    /// Globaler Slot des migrierenden Threads.
    pub fn gid(&self) -> u32 {
        self.tcb.gid
    }
}

/// Aufgezeichneter Zombie: freizugebende Stack-Region + `gid` (die erst nach dem Reap in
/// die Freiliste zurückgeht, damit ein stale Handle nie auf einen neuen Thread trifft).
#[derive(Clone, Copy)]
struct Zombie {
    base: usize,
    len: usize,
    gid: u32,
}

impl Zombie {
    const EMPTY: Zombie = Zombie {
        base: 0,
        len: 0,
        gid: NIL,
    };
}

/// Kopf einer intrusiven Ready-Queue (die Verkettung liegt in den TCBs).
#[derive(Clone, Copy)]
struct ListHead {
    head: u32,
    tail: u32,
    count: u32,
}

impl ListHead {
    const EMPTY: ListHead = ListHead {
        head: NIL,
        tail: NIL,
        count: 0,
    };
}

// ---------------------------------------------------------------------------------------
// Per-Kern-Lastzähler (lock-frei lesbar)
// ---------------------------------------------------------------------------------------

/// Belegte TCB-Slots je Kern — vom jeweiligen Scheduler gepflegt, **lock-frei** lesbar.
/// Ohne das müsste die lastbewusste Platzierung jeden Kern-Scheduler nacheinander sperren
/// (bei 256 Kernen 256 Lock-Zyklen je Spawn).
static CORE_LOAD: [core::sync::atomic::AtomicUsize; MAX_CORES] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; MAX_CORES];

/// Last (belegte TCB-Slots) des Kerns `core` — lock-frei.
pub fn core_load(core: usize) -> usize {
    CORE_LOAD
        .get(core)
        .map(|l| l.load(Ordering::Relaxed))
        .unwrap_or(usize::MAX)
}

// ---------------------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------------------

/// Scheduler **eines Kerns**: TCB-Tabelle + Ready-Listen + laufender Thread + Zombies.
pub struct Scheduler {
    /// Eigene Kern-ID (gesetzt durch [`attach_storage`](Scheduler::attach_storage)).
    core: usize,
    tcbs: Slab<Tcb>,
    /// Freie lokale TCB-Indizes (O(1)).
    free: FreeList,
    /// Laufender Thread (lokaler Index) oder `None` (vor `init_core`).
    current: Option<usize>,
    /// Eine Ready-Liste je Priorität (Round-Robin innerhalb einer Priorität).
    queues: [ListHead; NPRIO],
    /// Bit `p` gesetzt, wenn `queues[p]` nicht leer ist (O(1)-Auswahl der höchsten).
    bitmap: u32,
    /// Ringpuffer noch nicht eingesammelter Zombies.
    zombies: Slab<Zombie>,
    zhead: usize,
    ztail: usize,
    zcount: usize,
    /// Per-Kern-Tick-Zähler (für MCS-Budget-Refill-Zeitpunkte).
    now: u64,
    /// Telemetrie: wie oft erschöpfte ein Budget bzw. wurde aufgefüllt (für Tests).
    depletions: u64,
    refills: u64,
    /// Belegte TCB-Slots (O(1)-`load()`); wird nach `CORE_LOAD` gespiegelt.
    used: usize,
    /// Anzahl aktuell **erschöpfter** MCS-Konten. Ist sie 0, entfällt der Refill-Scan
    /// vollständig — der Normalfall (kein Budget) kostet damit nichts je Tick.
    depleted_count: usize,
    /// Migrations-Telemetrie (Tests): wie viele Threads dieser Kern abgegeben/aufgenommen hat.
    migrations_out: u64,
    migrations_in: u64,
}

/// Bytes für die Kern-Tabellen bei `capacity` gleichzeitig auf diesem Kern gehosteten
/// Threads (TCBs + Zombie-Ring + Freiliste, ein Block).
pub const fn core_storage_bytes(capacity: usize) -> usize {
    capacity * core::mem::size_of::<Tcb>()
        + capacity * core::mem::size_of::<Zombie>()
        + capacity * core::mem::size_of::<u32>()
}

/// Ausrichtung, die [`Scheduler::attach_storage`] erwartet.
pub const fn core_storage_align() -> usize {
    core::mem::align_of::<Tcb>()
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
            tcbs: Slab::empty(),
            free: FreeList::empty(),
            current: None,
            queues: [ListHead::EMPTY; NPRIO],
            bitmap: 0,
            zombies: Slab::empty(),
            zhead: 0,
            ztail: 0,
            zcount: 0,
            now: 0,
            depletions: 0,
            refills: 0,
            used: 0,
            depleted_count: 0,
            migrations_out: 0,
            migrations_in: 0,
        }
    }

    /// Diese Instanz an Kern `core` binden **und ihre Tabellen anlegen**. Vom Bootkern für
    /// **alle** Instanzen aufzurufen, bevor irgendwo Threads erzeugt werden — auch für
    /// Kerne, deren `init_core` erst später auf dem jeweiligen Kern läuft.
    ///
    /// # Safety
    /// `mem` zeigt auf mindestens [`core_storage_bytes(capacity)`](core_storage_bytes)
    /// Bytes, ist auf [`core_storage_align`] ausgerichtet, exklusiv für **diese** Instanz
    /// und lebt bis zum Reboot.
    pub unsafe fn attach_storage(&mut self, core: usize, mem: *mut u8, capacity: usize) {
        debug_assert!(mem as usize % core_storage_align() == 0);
        self.core = core;
        let tcb_bytes = capacity * core::mem::size_of::<Tcb>();
        let zomb_bytes = capacity * core::mem::size_of::<Zombie>();
        // SAFETY: Vertrag des Aufrufers; die drei Abschnitte liegen disjunkt in derselben
        // Zuteilung und sind (Tcb/Zombie: 8-ausgerichtet, u32: 4) korrekt ausgerichtet.
        unsafe {
            self.tcbs
                .attach(mem as *mut Tcb, capacity, |_| Tcb::EMPTY);
            self.zombies
                .attach(mem.add(tcb_bytes) as *mut Zombie, capacity, |_| Zombie::EMPTY);
            self.free
                .attach(mem.add(tcb_bytes + zomb_bytes) as *mut u32, capacity);
        }
    }

    /// Kern initialisieren: der gerade laufende Boot-Kontext wird zum Idle-Thread.
    /// Auf dem jeweiligen Kern vor dem Aktivieren von IRQs aufzurufen.
    pub fn init_core(&mut self, core: usize, priority: u8) -> Option<ThreadId> {
        debug_assert_eq!(core, self.core);
        let idle = self.alloc_tcb(0, priority)?;
        self.current = Some(idle);
        Some(self.id(idle))
    }

    /// Einen neuen Thread auf diesem Kern mit `priority` erzeugen: initialen Kontext
    /// am Stack-Top anlegen und in die Ready-Queue seiner Priorität einreihen.
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
    /// Zum Reaping wird der **User-Stack** vermerkt (dynamisch alloziert, rückgebbar);
    /// der Kernel-Stack stammt aus einem eigenen EL1-only Pool.
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

    /// Wie [`spawn_user`](Self::spawn_user), aber mit **getrenntem** EL0-SP und Reap-Region — für
    /// den Binary-Loader (ext-26): ein geladenes Programm hat einen **nicht-identity** gemappten
    /// Stack (SP ist eine virtuelle Adresse `el0_sp`, die freizugebende RAM-Region liegt an einer
    /// anderen Physadresse `reap_base`). `entry` ist die virtuelle Entry-Adresse des Programms.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_user_at(
        &mut self,
        core: usize,
        entry: usize,
        arg: usize,
        kstack_base: usize,
        kstack_len: usize,
        el0_sp: usize,
        reap_base: usize,
        reap_len: usize,
        priority: u8,
    ) -> Option<ThreadId> {
        debug_assert_eq!(core, self.core);
        let sp = init_thread_frame(kstack_base + kstack_len, entry, arg, true, el0_sp);
        let t = self.alloc_tcb(sp, priority)?;
        self.tcbs[t].stack_base = reap_base;
        self.tcbs[t].stack_len = reap_len;
        self.enqueue_ready(t);
        Some(self.id(t))
    }

    /// Der laufende Thread beendet sich selbst: Stack als Zombie vormerken und
    /// zum nächsten Thread wechseln. Gibt den nächsten Frame zurück.
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

    /// Einen Zombie dieses Kerns einsammeln: Stack-Region + `gid` zurückgeben, damit der
    /// Aufrufer (Kernel) die Region dem Allokator und die `gid` per [`release_gid`] der
    /// Thread-Freiliste zurückgibt — **beides außerhalb** des Scheduler-Locks.
    pub fn reap(&mut self) -> Option<(usize, usize, u32)> {
        if self.zcount == 0 {
            return None;
        }
        let z = self.zombies[self.zhead];
        self.zhead = (self.zhead + 1) % self.zombies.len();
        self.zcount -= 1;
        Some((z.base, z.len, z.gid))
    }

    /// Lastmaß dieses Kerns: belegte TCB-Slots (O(1)).
    pub fn load(&self) -> usize {
        self.used
    }

    /// Freie TCB-Slots dieses Kerns (kann dieser Kern noch einen Thread aufnehmen?).
    pub fn capacity_left(&self) -> usize {
        self.free.available()
    }

    /// Migrations-Telemetrie: (abgegeben, aufgenommen).
    pub fn migration_stats(&self) -> (u64, u64) {
        (self.migrations_out, self.migrations_in)
    }

    /// Handle des aktuell laufenden Threads.
    pub fn current_id(&self, core: usize) -> ThreadId {
        debug_assert_eq!(core, self.core);
        let Some(cur) = self.current else {
            // Diagnosefreundlich: wer fragt, auf welchem Kern, und hat der überhaupt Tabellen?
            panic!(
                "kein laufender Thread (gefragter Kern {core}, Instanz-Kern {}, TCB-Kapazitaet {})",
                self.core,
                self.tcbs.len()
            )
        };
        self.id(cur)
    }

    /// Gesicherter Frame eines (blockierten) Threads **dieses Kerns** — für den
    /// IPC-Nachrichtentransfer. `None`, wenn der Thread nicht (mehr) zu diesem Kern gehört.
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
        self.remove_from_ready(t); // war er bereits bereit, jetzt läuft er -> ausklinken
        // Budget-Donation: der Aufrufer (`cur`) leiht dem Server (`t`) seinen
        // Scheduling-Context. Belastet wird das **Wurzel-Konto** des Aufrufers (folgt
        // einer evtl. eigenen Spende -> verschachtelte IPC teilt das Wurzel-Budget).
        let account = self.tcbs[cur].sc_donor.unwrap_or(cur);
        self.tcbs[t].sc_donor = Some(account);
        self.tcbs[account].sc_donee = Some(t);
        self.current = Some(t);
        self.tcbs[t].sp
    }

    /// Einen blockierten Thread **dieses Kerns** wieder bereit machen. Kern-übergreifend
    /// ruft der Kernel dies auf der Zielinstanz auf (+ Reschedule-IPI).
    ///
    /// Idempotent: wirkt **nur**, wenn der Thread tatsächlich blockiert ist.
    ///
    /// Rückgabe: ob der Thread auf **diesem** Kern aufgelöst werden konnte. `false` heißt
    /// „tot **oder** inzwischen migriert" — der Aufrufer schlägt den Besitzer dann neu nach
    /// und wiederholt (s. Modul-Doku).
    pub fn unblock(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        if self.tcbs[s].blocked {
            self.tcbs[s].blocked = false;
            self.enqueue_ready(s);
        }
        true
    }

    /// Einen **bestimmten** Thread dieses Kerns extern pausieren (`SYS_PDCTL` PAUSE).
    /// Rückgabe wie [`unblock`](Self::unblock): konnte der Thread hier aufgelöst werden?
    pub fn pause(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        if !self.tcbs[s].blocked {
            self.tcbs[s].blocked = true;
            self.remove_from_ready(s); // No-Op, falls er gerade `current` ist
        }
        true
    }

    /// Auflösung `ThreadId -> lokaler TCB-Index` **dieses** Kerns.
    ///
    /// Schlägt fehl, wenn der Thread tot ist **oder inzwischen auf einen anderen Kern
    /// migriert** ist. Genau das ist die Wiederholungsbedingung für kernübergreifende
    /// Aufrufer (s. Modul-Doku).
    fn resolve(&self, tid: ThreadId) -> Option<usize> {
        let e = dir_load(tid.slot)?;
        if e & D_USED == 0 {
            return None;
        }
        if ((e >> D_GEN_SHIFT) & D_GEN_MASK) as u32 != tid.gen {
            return None;
        }
        if ((e >> D_CORE_SHIFT) & D_CORE_MASK) as usize != self.core {
            return None; // gehört (nicht mehr) diesem Kern
        }
        let local = (e & D_LOCAL_MASK) as usize;
        let t = self.tcbs.get(local)?;
        if t.used && t.gen == tid.gen {
            Some(local)
        } else {
            None
        }
    }

    // --- Migration (ext-30) ---------------------------------------------------------

    /// Einen Thread **dieses** Kerns zur Migration herauslösen.
    ///
    /// Nur der besitzende Kern darf das (der Aufrufer hält dessen Lock). Schlägt fehl bei:
    /// * unbekanntem/fremdem Thread,
    /// * dem **laufenden** Thread (sein Zustand steckt im aktiven Trap-Frame),
    /// * aktiver **Budget-Donation** (die Links sind lokale Slots — eine Spende ist
    ///   definitionsgemäß intra-core; erst nach dem REPLY ist der Thread migrierbar).
    ///
    /// Der Directory-Eintrag bleibt bis [`attach_migrated`](Self::attach_migrated)
    /// unverändert stehen (er zeigt noch auf diesen Kern); da der Aufrufer beide Locks
    /// hält, kann in diesem Fenster niemand den Thread erfolgreich auflösen.
    pub fn detach_for_migration(&mut self, tid: ThreadId) -> Option<Migrant> {
        let s = self.resolve(tid)?;
        if self.current == Some(s) {
            return None;
        }
        if self.tcbs[s].sc_donor.is_some() || self.tcbs[s].sc_donee.is_some() {
            return None;
        }
        let was_ready = self.tcbs[s].queued != NOT_QUEUED;
        self.remove_from_ready(s);
        let mut tcb = self.tcbs[s];
        // Lokale Verkettung/Slot-Bezüge gehören dem Quellkern -> zurücksetzen.
        tcb.queued = NOT_QUEUED;
        tcb.qnext = NIL;
        tcb.qprev = NIL;
        // Slot freigeben (die `gid` bleibt dem Thread!). `gen` NICHT erhöhen — die
        // Identität des Threads bleibt bestehen, er wechselt nur den Kern.
        if self.tcbs[s].depleted {
            self.depleted_count -= 1;
        }
        self.tcbs[s] = Tcb::EMPTY;
        self.free.free(s);
        self.used -= 1;
        CORE_LOAD[self.core].store(self.used, Ordering::Relaxed);
        self.migrations_out += 1;
        Some(Migrant { tcb, was_ready })
    }

    /// Einen herausgelösten Thread auf **diesem** Kern aufnehmen und den Directory-Eintrag
    /// auf ihn umhängen. Gibt den Migranten bei fehlender Kapazität **zurück**, damit der
    /// Aufrufer ihn wieder beim Quellkern einhängen kann (kein Thread-Verlust).
    pub fn attach_migrated(&mut self, m: Migrant) -> Result<(), Migrant> {
        let Some(s) = self.free.alloc() else {
            return Err(m);
        };
        let mut tcb = m.tcb;
        // MCS: `next_refill` ist relativ zur **Tick-Uhr des Quellkerns** -> auf die eigene
        // umrechnen, sonst würde ein erschöpftes Budget hier zu früh/zu spät auffüllen.
        if tcb.depleted {
            tcb.next_refill = self.now + tcb.period as u64;
            self.depleted_count += 1;
        }
        tcb.queued = NOT_QUEUED;
        tcb.qnext = NIL;
        tcb.qprev = NIL;
        self.tcbs[s] = tcb;
        self.used += 1;
        CORE_LOAD[self.core].store(self.used, Ordering::Relaxed);
        self.migrations_in += 1;
        // Directory umhängen: ab jetzt löst der Thread auf diesem Kern auf.
        self.publish_dir(s);
        if m.was_ready {
            self.enqueue_ready(s);
        }
        Ok(())
    }

    /// Kandidat zum Abgeben an einen anderen Kern: ein **bereiter**, nicht laufender Thread
    /// ohne aktive Donation und ohne Idle-Rolle. `None`, wenn nichts migrierbar ist.
    ///
    /// Bewusst nur *bereite* Threads: blockierte hängen in IPC-Warteschlangen, deren
    /// Einträge `ThreadId`s sind — die überleben eine Migration zwar, aber sie zu
    /// verschieben bringt keinen Lastgewinn (sie verbrauchen keine CPU).
    pub fn migration_candidate(&self) -> Option<ThreadId> {
        for p in 0..NPRIO {
            let mut i = self.queues[p].head;
            while i != NIL {
                let s = i as usize;
                let t = &self.tcbs[s];
                i = t.qnext;
                if self.current == Some(s) || t.sc_donor.is_some() || t.sc_donee.is_some() {
                    continue;
                }
                // Der Idle-Thread (Slot des `init_core`-Kontextes) hat keinen eigenen Stack
                // und darf den Kern nie verlassen.
                if t.stack_len == 0 {
                    continue;
                }
                return Some(self.id(s));
            }
        }
        None
    }

    /// Timer-Tick (oder Reschedule-IPI): aktuellen Frame sichern, rundlaufend den
    /// nächsten Thread wählen und dessen Frame zur Wiederherstellung zurückgeben.
    ///
    /// `tick = true` bei einem **echten Zeitscheiben-Tick** (Timer/IPI): dann wird
    /// das **MCS-Budget** des laufenden Threads um einen Tick reduziert und erschöpfte
    /// Budgets werden nach Periodenablauf wieder aufgefüllt. `tick = false` bei einem
    /// freiwilligen `YIELD` (verbraucht kein Budget, rein Round-Robin).
    pub fn on_tick(&mut self, core: usize, frame: usize, tick: bool) -> usize {
        debug_assert_eq!(core, self.core);
        if tick {
            self.now += 1;
            // Refill-Scan NUR, wenn überhaupt ein Konto erschöpft ist (Normalfall: keins ->
            // der Tick kostet nichts, unabhängig von der Tabellengröße).
            if self.depleted_count > 0 {
                self.refill_depleted();
            }
        }
        if let Some(cur) = self.current {
            self.tcbs[cur].sp = frame;
            // Ein extern als blockiert markierter `current` (z. B. via `pause`/`SYS_PDCTL`)
            // wird NICHT wieder eingereiht -> er deplaniert sauber.
            let mut requeue = !self.tcbs[cur].blocked;
            // Gegen das **Konto** belasten (eigenes oder via Donation geliehenes).
            let acct = self.tcbs[cur].sc_donor.unwrap_or(cur);
            if tick && self.tcbs[acct].budget > 0 {
                // Eine Zeitscheibe verbraucht -> Budget des Kontos reduzieren.
                self.tcbs[acct].remaining = self.tcbs[acct].remaining.saturating_sub(1);
                if self.tcbs[acct].remaining == 0 {
                    // Konto erschöpft: bis zum Refill nicht mehr einplanen.
                    self.tcbs[acct].depleted = true;
                    self.tcbs[acct].next_refill = self.now + self.tcbs[acct].period as u64;
                    self.depleted_count += 1;
                    self.depletions += 1;
                    requeue = false;
                    if acct != cur {
                        // `cur` ist ein Donee, der gegen ein fremdes (erschöpftes) Konto
                        // lief -> auf den Refill blocken (nicht „verloren": blocked=true,
                        // der Refill des Kontos macht ihn wieder bereit).
                        self.tcbs[cur].blocked = true;
                    }
                }
            }
            if requeue {
                self.enqueue_ready(cur);
            }
        }
        match self.dequeue_highest() {
            Some(next) => {
                self.current = Some(next);
                self.tcbs[next].sp
            }
            None => frame, // nichts lauffähig (sollte nicht vorkommen: Idle ist immer dabei)
        }
    }

    /// Erschöpfte MCS-Konten, deren Periode abgelaufen ist, auffüllen und den wartenden
    /// Läufer wieder bereit machen. Läuft ein **Donee** (geliehener SC) gegen dieses Konto,
    /// wird der DONEE wieder bereit gemacht (das Konto selbst ist der blockierte Aufrufer).
    fn refill_depleted(&mut self) {
        for slot in 0..self.tcbs.len() {
            if self.tcbs[slot].used
                && self.tcbs[slot].budget > 0
                && self.tcbs[slot].depleted
                && self.now >= self.tcbs[slot].next_refill
            {
                self.tcbs[slot].remaining = self.tcbs[slot].budget;
                self.tcbs[slot].depleted = false;
                self.depleted_count -= 1;
                self.refills += 1;
                match self.tcbs[slot].sc_donee {
                    Some(d) if d != slot => {
                        // Der Donee war auf das Konto-Budget geblockt -> wieder bereit.
                        self.tcbs[d].blocked = false;
                        self.enqueue_ready(d);
                    }
                    _ => self.enqueue_ready(slot),
                }
            }
        }
    }

    /// Einem Thread einen **Scheduling Context** zuweisen: `budget` Ticks je `period`
    /// Ticks (`budget = 0` -> unbeschränkt/Round-Robin).
    pub fn set_budget(&mut self, tid: ThreadId, budget: u32, period: u32) -> bool {
        if let Some(s) = self.resolve(tid) {
            // War der Thread erschöpft, ist er weder laufend, noch bereit, noch blockiert —
            // er wartet NUR auf den Refill-Scan. Würden wir hier `depleted` löschen, ohne
            // ihn wieder einzureihen, wäre er für immer verloren (nie wieder einplanbar,
            // aber belegter TCB-Slot). Daher nach dem Reset wieder bereit machen.
            let was_depleted = self.tcbs[s].depleted;
            self.tcbs[s].budget = budget;
            self.tcbs[s].period = period.max(1);
            self.tcbs[s].remaining = budget;
            self.tcbs[s].next_refill = self.now + period as u64;
            self.tcbs[s].depleted = false;
            if was_depleted {
                self.depleted_count -= 1;
                if self.current != Some(s) && !self.tcbs[s].blocked {
                    self.enqueue_ready(s);
                }
            }
            true
        } else {
            false
        }
    }

    /// MCS-Telemetrie dieses Kerns: (Budget-Erschöpfungen, Refills). Für Tests.
    pub fn budget_stats(&self) -> (u64, u64) {
        (self.depletions, self.refills)
    }

    /// Eine **Budget-Donation beenden**: der laufende Thread (Server beim REPLY) gibt das
    /// geliehene Konto frei. Idempotent (No-Op ohne aktive Spende).
    pub fn end_donation(&mut self, core: usize) {
        debug_assert_eq!(core, self.core);
        if let Some(cur) = self.current {
            if let Some(acct) = self.tcbs[cur].sc_donor.take() {
                if self.tcbs[acct].sc_donee == Some(cur) {
                    self.tcbs[acct].sc_donee = None;
                }
            }
        }
    }

    /// Lebt `tid` auf diesem Kern (gültiges, belegtes Handle)? Für IPC-Audits.
    pub fn is_alive(&self, tid: ThreadId) -> bool {
        self.resolve(tid).is_some()
    }

    /// Read-only **Konsistenz-Audit** dieses Kern-Schedulers (Fuzzer-Oracle). Gibt `0`
    /// bei Konsistenz, sonst einen Anomalie-Code zurück. Unter dem `SCHEDS[core]`-Lock
    /// aufzurufen. Geprüft: 1=toter Eintrag in einer Ready-Liste, 2=blockierter Thread in
    /// einer Ready-Liste, 3=Verkettung nicht reziprok (Listenstruktur kaputt),
    /// 4=`current` steht zugleich in einer Ready-Liste, 5=Bitmap/Zähler inkonsistent,
    /// 6=Thread in der falschen Prioritäts-Liste, 7=verlorener Thread (lauffähig, aber in
    /// keiner Liste/nicht laufend), 8=Directory-Eintrag passt nicht zum TCB.
    pub fn audit(&self) -> u32 {
        for p in 0..NPRIO {
            let q = &self.queues[p];
            if ((self.bitmap >> p) & 1 == 1) != (q.count > 0) {
                return 5;
            }
            let mut i = q.head;
            let mut prev = NIL;
            let mut n = 0u32;
            while i != NIL {
                let s = i as usize;
                let Some(t) = self.tcbs.get(s) else {
                    return 1;
                };
                if !t.used {
                    return 1;
                }
                if t.blocked {
                    return 2;
                }
                if t.queued as usize != p || t.priority as usize != p {
                    return 6;
                }
                if t.qprev != prev {
                    return 3; // Rückverkettung stimmt nicht
                }
                if self.current == Some(s) {
                    return 4;
                }
                prev = i;
                i = t.qnext;
                n += 1;
                if n > q.count {
                    return 5; // längere Liste als gezählt (Zyklus/Leck)
                }
            }
            if n != q.count || q.tail != prev {
                return 5;
            }
        }
        // Verlorene Threads + Directory-Konsistenz.
        for local in 0..self.tcbs.len() {
            let t = &self.tcbs[local];
            if !t.used {
                continue;
            }
            // Der Directory-Eintrag MUSS auf genau diesen Kern + Slot zeigen.
            match dir_load(t.gid as usize) {
                Some(e)
                    if e & D_USED != 0
                        && ((e >> D_GEN_SHIFT) & D_GEN_MASK) as u32 == t.gen
                        && ((e >> D_CORE_SHIFT) & D_CORE_MASK) as usize == self.core
                        && (e & D_LOCAL_MASK) as usize == local => {}
                _ => return 8,
            }
            if !t.blocked && !t.depleted && self.current != Some(local) && t.queued == NOT_QUEUED {
                return 7;
            }
        }
        0
    }

    // --- intern ---

    /// Directory-Eintrag des TCBs an `local` auf diesen Kern/Slot setzen.
    fn publish_dir(&self, local: usize) {
        let t = &self.tcbs[local];
        if let Some(e) = DIRECTORY.get(t.gid as usize) {
            e.store(pack_dir(true, t.gen, self.core, local), Ordering::Release);
        }
    }

    /// Einen Thread (lokaler Index) in die Ready-Liste seiner Priorität einreihen (O(1)).
    /// No-Op, wenn er bereits eingereiht ist (Doppel-Einreihung wäre ein Listen-Zyklus).
    fn enqueue_ready(&mut self, local: usize) {
        if self.tcbs[local].queued != NOT_QUEUED {
            return;
        }
        let p = self.tcbs[local].priority as usize;
        let tail = self.queues[p].tail;
        self.tcbs[local].qprev = tail;
        self.tcbs[local].qnext = NIL;
        self.tcbs[local].queued = p as u8;
        if tail == NIL {
            self.queues[p].head = local as u32;
        } else {
            self.tcbs[tail as usize].qnext = local as u32;
        }
        self.queues[p].tail = local as u32;
        self.queues[p].count += 1;
        self.bitmap |= 1 << p;
    }

    /// Einen bereiten Thread aus seiner Prioritäts-Liste ausklinken (O(1)).
    fn remove_from_ready(&mut self, local: usize) {
        let p = self.tcbs[local].queued;
        if p == NOT_QUEUED {
            return;
        }
        let p = p as usize;
        let (prev, next) = (self.tcbs[local].qprev, self.tcbs[local].qnext);
        if prev == NIL {
            self.queues[p].head = next;
        } else {
            self.tcbs[prev as usize].qnext = next;
        }
        if next == NIL {
            self.queues[p].tail = prev;
        } else {
            self.tcbs[next as usize].qprev = prev;
        }
        self.tcbs[local].qnext = NIL;
        self.tcbs[local].qprev = NIL;
        self.tcbs[local].queued = NOT_QUEUED;
        self.queues[p].count -= 1;
        if self.queues[p].count == 0 {
            self.bitmap &= !(1 << p);
        }
    }

    /// Einen beendeten Thread aufzeichnen: TCB-Slot freigeben, Directory-Eintrag
    /// **sofort** ungültig machen (stale Handles sterben unmittelbar), Stack-Region + `gid`
    /// zum späteren Freigeben (`reap`) vormerken.
    fn record_zombie(&mut self, local: usize) {
        // Budget-Donation-Links lösen (vor dem Clear lesen): stirbt ein Konto, verliert
        // sein Donee die Spende; stirbt ein Donee, wird der Donee-Eintrag gelöscht.
        if let Some(d) = self.tcbs[local].sc_donee {
            self.tcbs[d].sc_donor = None;
        }
        if let Some(a) = self.tcbs[local].sc_donor {
            if self.tcbs[a].sc_donee == Some(local) {
                self.tcbs[a].sc_donee = None;
            }
        }
        self.remove_from_ready(local);
        let base = self.tcbs[local].stack_base;
        let len = self.tcbs[local].stack_len;
        let gid = self.tcbs[local].gid;
        let gen = self.tcbs[local].gen.wrapping_add(1) & (D_GEN_MASK as u32);
        // Directory: nicht mehr belegt, Generation hoch -> jedes alte Handle ist stale.
        if let Some(e) = DIRECTORY.get(gid as usize) {
            e.store(pack_dir(false, gen, 0, 0), Ordering::Release);
        }
        if self.tcbs[local].depleted {
            self.depleted_count -= 1;
        }
        self.tcbs[local] = Tcb::EMPTY;
        self.free.free(local);
        self.used -= 1;
        CORE_LOAD[self.core].store(self.used, Ordering::Relaxed);
        // Zombie IMMER aufzeichnen (auch ohne Stack): die `gid` muss zurück in die
        // Thread-Freiliste, und das darf nur außerhalb des Scheduler-Locks passieren.
        if self.zcount < self.zombies.len() {
            self.zombies[self.ztail] = Zombie { base, len, gid };
            self.ztail = (self.ztail + 1) % self.zombies.len();
            self.zcount += 1;
        }
    }

    /// Den nächsten Thread der höchsten nichtleeren Priorität entnehmen (O(1)).
    fn dequeue_highest(&mut self) -> Option<usize> {
        if self.bitmap == 0 {
            return None;
        }
        let p = (31 - self.bitmap.leading_zeros()) as usize; // höchstes gesetztes Bit
        let head = self.queues[p].head;
        if head == NIL {
            return None;
        }
        let local = head as usize;
        self.remove_from_ready(local);
        Some(local)
    }

    /// Einen freien TCB-Slot belegen (O(1) über die Freiliste) und ihm eine **globale
    /// `gid`** aus dem Thread-Directory geben.
    fn alloc_tcb(&mut self, sp: usize, priority: u8) -> Option<usize> {
        let i = self.free.alloc()?;
        // gid + Generation aus dem Directory (Leaf-Lock, s. `GID_FREE`).
        let gid = match GID_FREE.lock().alloc() {
            Some(g) => g,
            None => {
                self.free.free(i); // TCB-Slot zurückgeben, sonst leckt er
                return None;
            }
        };
        let gen = match dir_load(gid) {
            Some(e) => ((e >> D_GEN_SHIFT) & D_GEN_MASK) as u32,
            None => {
                self.free.free(i);
                GID_FREE.lock().free(gid);
                return None;
            }
        };
        self.tcbs[i] = Tcb {
            used: true,
            gid: gid as u32,
            gen,
            sp,
            priority,
            // MCS-Felder: standardmäßig unbeschränkt (budget = 0 -> Round-Robin).
            ..Tcb::EMPTY
        };
        self.used += 1;
        CORE_LOAD[self.core].store(self.used, Ordering::Relaxed);
        self.publish_dir(i);
        Some(i)
    }

    /// Globale `ThreadId` aus einem lokalen Slot-Index dieses Kerns.
    fn id(&self, local: usize) -> ThreadId {
        ThreadId {
            slot: self.tcbs[local].gid as usize,
            gen: self.tcbs[local].gen,
        }
    }
}

/// Scheduler-Operationen, wie sie der IPC-/Dispatch-Pfad braucht — abstrahiert von
/// der konkreten Instanz, damit der Kernel **kern-übergreifend** auflösen kann
/// (z. B. einen IPC-Partner auf einem anderen Kern wecken). Der Kernel stellt eine Facade
/// über alle per-Kern-Instanzen bereit, die je Operation **genau eine** Instanz sperrt
/// (nie zwei gleichzeitig) und beim kern-übergreifenden Wecken einen Reschedule-IPI
/// schickt.
///
/// `current_id`/`block_current`/`switch_to`/`exit_current`/`on_tick`/`kill` beziehen
/// sich auf den **aktuellen** Kern (`core`); `frame_of`/`unblock` dürfen einen
/// Thread auf **irgendeinem** Kern betreffen (kern-übergreifender IPC-Partner) — der
/// Kernel schlägt den Besitzer im Directory nach und prüft nach dem Sperren erneut.
pub trait SchedOps {
    fn current_id(&mut self, core: usize) -> ThreadId;
    fn frame_of(&mut self, tid: ThreadId) -> Option<usize>;
    fn block_current(&mut self, core: usize, frame: usize) -> usize;
    fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize;
    fn unblock(&mut self, tid: ThreadId);
    /// Einen bestimmten Thread extern pausieren (blockieren; mit `unblock` reversibel).
    fn pause(&mut self, tid: ThreadId);
    /// Einen bestimmten (nicht laufenden) Thread auf irgendeinem Kern beenden + abbauen
    /// (`SYS_PDCTL` STOP). Gibt `false`, falls er gerade läuft (erst pausieren).
    fn stop(&mut self, tid: ThreadId) -> bool;
    fn on_tick(&mut self, core: usize, frame: usize) -> usize;
    fn exit_current(&mut self, core: usize, frame: usize) -> usize;
    fn kill(&mut self, tid: ThreadId, core: usize) -> bool;
    /// Eine aktive **Budget-Donation** des laufenden Threads beenden (Server beim REPLY
    /// gibt das geliehene Konto frei). No-Op ohne aktive Spende.
    fn end_donation(&mut self, core: usize);
    /// Einen physischen Frame `[base, base+len)` in die VSpace des Aufrufers `caller`
    /// mappen (cap-gated; nur für isolierte PDs sinnvoll). `perm_code`: 0=Ro, 1=Rw,
    /// 2=Rx (aus den Cap-Rechten abgeleitet). Granularität nach `len` (2 MiB / 4 KiB).
    fn map_frame(&mut self, caller: ThreadId, base: u64, len: u64, perm_code: u8) -> bool;
    /// Einen zuvor gemappten Frame wieder aus der VSpace des Aufrufers entfernen.
    fn unmap_frame(&mut self, caller: ThreadId, base: u64, len: u64) -> bool;
}
