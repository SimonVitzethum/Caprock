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

mod cycles;
pub use cycles::{CycleStats, Reject, Sample, Source, Stamp, MAX_PLAUSIBLE_SLICE};

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
    /// Wurde der Thread nach [`Scheduler::spawn_parked`] **zugelassen**? Siehe D0: ein `bind_pd`
    /// auf einen bereits zugelassenen Thread ist die Reihenfolge, die den Fehler erzeugt hat.
    admitted: bool,
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
    /// H-b: blockiert, WEIL das belastete Konto leer ist -- im Unterschied zu einer Blockade
    /// aus IPC oder PAUSE. `blocked` allein kann das nicht sagen, und genau daran haengt der
    /// Donee-Zweig: er hebt eine Blockade auf, deren Grund er nicht kennt.
    budget_blocked: bool,
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
    // --- Zyklenabrechnung (B-5.1) ---
    /// Verbrauchte Zyklen + die Gründe, warum die Summe unvollständig sein könnte.
    /// Wandert bei einer Migration **mit** (der `Migrant` trägt den ganzen `Tcb`) — es ist der
    /// Lebensverbrauch des Threads, keine Eigenschaft des Kerns.
    cyc: CycleStats,
    /// Offener Stempel, solange der Thread **läuft**. `None` heisst „läuft gerade nicht".
    stamp: Option<Stamp>,
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
        // **Ein Bit fuer „darf laufen"** (D0). Es traegt GENAU einen Grund -- der Scheduler kennt
        // schon ein `blocked`, das frueher drei Bedeutungen hatte (s. D9), und ein viertes waere
        // derselbe Fehler noch einmal. Hier geht es nur um die Zulassung nach `spawn_parked`.
        admitted: false,
        qnext: NIL,
        qprev: NIL,
        budget: 0,
        period: 0,
        remaining: 0,
        next_refill: 0,
        depleted: false,
        budget_blocked: false,
        sc_donor: None,
        sc_donee: None,
        cyc: CycleStats::EMPTY,
        stamp: None,
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
    /// Taugt die Zyklenquelle als Zeitachse? **Vorgabe `Untrusted`** (B-5.1): solange der
    /// Kernel die Invarianz nicht ausdrücklich zugesichert hat, wird nichts abgerechnet.
    /// Die sichere Vorgabe ist „ich weiss es nicht", nicht „wird schon stimmen" — auf einer
    /// gemieteten Maschine ist Letzteres eine erfundene Rechnung.
    cycle_source: Source,
    /// Kernweite Summe derselben Proben (Telemetrie; die Ablehnungsgründe interessieren hier
    /// besonders, weil sie an der **Maschine** hängen und nicht am Thread).
    cyc_core: CycleStats,
    /// Belegte TCB-Slots (O(1)-`load()`); wird nach `CORE_LOAD` gespiegelt.
    used: usize,
    /// Anzahl aktuell **erschöpfter** MCS-Konten. Ist sie 0, entfällt der Refill-Scan
    /// vollständig — der Normalfall (kein Budget) kostet damit nichts je Tick.
    depleted_count: usize,
    /// Anzahl Threads mit gesetztem [`Tcb::budget_blocked`] (D10). Ist sie 0, entfällt der
    /// **Weckelauf** in [`refill_depleted`](Self::refill_depleted) und
    /// [`set_budget`](Self::set_budget) — beide kosteten seit H-b einen vollen
    /// Tabellendurchlauf **je aufgefülltem Konto** (gemessen: 10 000 Iterationen bei 10 000
    /// Slots, und 1 000 000 in **einem** Timer-Interrupt, sobald 100 Konten im selben Tick
    /// auffüllen — `tools/sched-erschoepfung-messen.sh`, L1/L3).
    ///
    /// **Bewegt wird er an genau zwei Zeilen**, beide in
    /// [`set_budget_blocked`](Self::set_budget_blocked), und beide sind an einen echten
    /// Zustandswechsel des Feldes gebunden. Das ist kein Stil, sondern die Lehre aus
    /// `depleted_count` (D8/M5): der wurde mehrfach erhöht und nur einmal gesenkt, kehrte nie
    /// auf 0 zurück, und der Scan lief dadurch in **jedem** Tick — bei einem Zähler, der
    /// über „läuft der teure Pfad?" entscheidet, ist ein Lügen dasselbe wie kein Zähler.
    /// Wer ihn nachzählt: [`audit`](Self::audit) (Code **10**) und die L9-Reihe des
    /// Messwerkzeugs — zwei voneinander unabhängige Nachzählungen.
    ///
    /// Drei Stellen überschreiben einen `Tcb` **als Ganzes** und laufen deshalb nicht über
    /// den Helfer; sie führen den Zähler von Hand nach: [`record_zombie`](Self::record_zombie)
    /// (der Sterbende selbst), [`detach_for_migration`](Self::detach_for_migration) (der
    /// Abwandernde) und [`attach_migrated`](Self::attach_migrated) (der Ankommende).
    budget_blocked_count: usize,
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
            cycle_source: Source::Untrusted,
            cyc_core: CycleStats::EMPTY,
            used: 0,
            depleted_count: 0,
            budget_blocked_count: 0,
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
        let t = self.spawn_parked(core, entry, arg, stack_base, stack_len, priority)?;
        let _ = self.admit(t);
        Some(t)
    }

    /// **Erzeugen, aber noch NICHT laufen lassen** (D0, 2026-08-07).
    ///
    /// Der Unterschied zu [`Scheduler::spawn`] ist eine einzige Zeile — das fehlende
    /// `enqueue_ready` — und er ist der Kern von D0. Ein Thread, der ab `spawn` lauffähig ist,
    /// kann **auf einem anderen Kern anlaufen, bevor der Aufrufer ihm seine PD gegeben hat**.
    /// Genau das war der Fehler: der IPC-Server der x86-Suite lief 9-mal in 50 000 Läufen in sein
    /// erstes `RECV`, bekam `ERR_NOPD` und verließ seine Schleife für immer.
    ///
    /// Die Reihenfolge „erst lauffähig, dann Autorität" ist nicht durch Sorgfalt zu retten: die
    /// Lücke zwischen den beiden Aufrufen ist beliebig kurz und trifft trotzdem. Deshalb gibt es
    /// hier zwei Funktionen statt einer Reihenfolge — wer eine PD braucht, ruft
    /// [`Scheduler::admit`] erst, wenn sie steht.
    ///
    /// **Kein `park: bool`-Parameter.** Ein Schalter wäre wieder ein wählbarer Grund, und die
    /// bequeme Belegung wäre die falsche. Zwei Namen sind nicht zu verwechseln.
    pub fn spawn_parked(
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
        Some(self.id(t))
    }

    /// Einen geparkten Thread **zulassen**: ab hier darf er laufen.
    ///
    /// Gibt `false` zurück, wenn die `tid` nicht (mehr) auf diesem Kern auflösbar ist. Zweimal
    /// zulassen ist kein Fehler, aber auch kein zweites Einreihen — `enqueue_ready` ist gegen
    /// Doppeleinträge gesichert (s. `remove_from_ready` davor).
    #[must_use = "ein nicht zugelassener Thread laeuft nie -- genau das war D0, nur andersherum"]
    pub fn admit(&mut self, tid: ThreadId) -> bool {
        let Some(t) = self.resolve(tid) else {
            return false;
        };
        // **Nur `enqueue_ready`, kein `remove_from_ready` davor.** Die Funktion ist gegen
        // Doppeleintraege gesichert (`queued != NOT_QUEUED -> return`); ein Entfernen davor waere
        // kein Schutz, sondern eine stille Aenderung: der Thread landete am ENDE seiner
        // Prioritaetsschlange, obwohl er schon vorne stand.
        self.tcbs[t].admitted = true;
        self.enqueue_ready(t);
        true
    }

    /// Die **tatsaechliche** Prioritaet eines Threads. `None`, wenn die `tid` hier nicht auflösbar ist.
    ///
    /// Fuer den Z11c-Nachweis: dass das Manifest eine Prioritaet NENNT, heisst nicht, dass der
    /// Scheduler sie hat. Der Ladepfad zu fragen waere ein Schreiber, der sein eigenes Ergebnis
    /// bestaetigt -- gelesen wird deshalb der TCB.
    pub fn priority_of(&self, tid: ThreadId) -> Option<u8> {
        self.resolve(tid).map(|t| self.tcbs[t].priority)
    }

    /// Darf dieser Thread schon laufen? `None`, wenn die `tid` auf diesem Kern nicht auflösbar ist.
    ///
    /// Der Kernel fragt das in `bind_pd`, um die D0-Reihenfolge **zaehlbar** zu machen: eine PD,
    /// die an einen bereits zugelassenen Thread gebunden wird, kommt zu spaet -- vielleicht nur um
    /// Nanosekunden, aber genau die haben 9-mal in 50 000 Laeufen gereicht.
    pub fn is_admitted(&self, tid: ThreadId) -> Option<bool> {
        self.resolve(tid).map(|t| self.tcbs[t].admitted)
    }

    /// **Einen EL0-User-Thread erzeugen — mit GETRENNTEM EL0-SP und Reap-Region.**
    ///
    /// Bis zum 2026-08-04 gab es daneben ein `spawn_user`, das **einen** Wert fuer beides nahm:
    /// den Stackzeiger, den EL0 sieht, und die RAM-Region, die beim Thread-Tod an den Allokator
    /// zurueckgeht. Solange jede User-Region identisch abgebildet war (VA == PA), war das
    /// dieselbe Zahl -- und deshalb war die Vermengung unsichtbar. Beim Umbau auf ein VA-Fenster
    /// wurde sie zu einem `#PF` im **Kernel**: der Reap-Pfad gab eine virtuelle Adresse als
    /// Physadresse frei.
    ///
    /// Die Funktion ist deshalb geloescht statt repariert. Wer hier vorbeikommt, muss beide Werte
    /// hinschreiben — auch dort, wo sie zufaellig gleich sind (SAS-Threads). Ein Parameter, der
    /// zwei Bedeutungen traegt, ist so lange harmlos, wie die beiden zufaellig gleich sind.
    ///
    /// `el0_sp` ist eine **virtuelle** Adresse (der Stackzeiger, den EL0 sieht),
    /// `reap_base`/`reap_len` sind **physisch** (die Region, die freigegeben wird). Beim
    /// Binary-Loader (ext-26) und bei isolierten PDs (E-Rest 3d) liegen sie auseinander; bei
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
        let t = self.spawn_user_at_parked(
            core, entry, arg, kstack_base, kstack_len, el0_sp, reap_base, reap_len, priority,
        )?;
        let _ = self.admit(t);
        Some(t)
    }

    /// Wie [`Scheduler::spawn_user_at`], aber **noch nicht lauffähig** — s. [`Scheduler::spawn_parked`].
    ///
    /// Diese Fassung ist die wichtigere von beiden: `load_into_pd` machte den Thread eines
    /// geladenen Programms lauffähig, band **danach** seine PD und installierte **danach** die
    /// Endowment-Caps. Eine Treiber-PD konnte also anlaufen, bevor sie irgendeine Autorität hatte.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_user_at_parked(
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
            // H-b: WARUM ist er blockiert? Eine Blockade auf ein leeres Konto hebt nur der
            // Refill auf. Der Waechter ist hier -- anders als G-a -- gefahrlos, weil der Wecker
            // BENANNT ist: `refill_depleted` weckt genau `budget_blocked`.
            if self.tcbs[s].budget_blocked {
                return true;
            }
            let acct = self.tcbs[s].sc_donor.unwrap_or(s);
            if acct != s && self.tcbs[acct].depleted {
                // Zurueck in den Lauf ja -- aber nicht auf ein leeres Konto. Die Blockade
                // wechselt den GRUND, und der neue Grund hat einen Wecker.
                // D10: **Erhöhung 2 von 2** von `budget_blocked_count`.
                self.set_budget_blocked(s, true);
                return true;
            }
            self.tcbs[s].blocked = false;
            // **Erschöpft heisst: nicht einplanen** (D8, gemessen 2026-08-03). Bis hierher
            // reihte `unblock` bedingungslos ein, und ein erschöpfter Thread lief danach eine
            // volle Zeitscheibe auf leerem Konto -- mit einer bereitstehenden Alternative
            // daneben und `audit() == 0`. Erreichbar OHNE Cap: `switch_to` spendet beim
            // IPC-CALL das Konto des Aufrufers, `on_tick` belastet es und setzt `depleted` am
            // **blockierten** Aufrufer, `reply` ruft `unblock(caller)`.
            //
            // Der Wächter gehört INNERHALB des Rumpfes. Die naheliegende Fassung
            // `if blocked && !depleted { .. }` ist gemessen SCHÄDLICH: sie überspringt auch
            // `blocked = false`, das RESUME wird verschluckt, und zusammen mit dem Wächter in
            // `refill_depleted` verhungert der Thread vollständig (0 Ticks mit Budget).
            //
            // Wer ihn danach einreiht, ist `refill_depleted` -- gemessen (2 Refills, 6 Ticks
            // mit echtem Budget, am Ende nicht blockiert).
            if !self.tcbs[s].depleted {
                self.enqueue_ready(s);
            }
        }
        true
    }

    /// Einen **bestimmten** Thread dieses Kerns extern pausieren (`SYS_PDCTL` PAUSE).
    /// Rückgabe wie [`unblock`](Self::unblock): konnte der Thread hier aufgelöst werden?
    pub fn pause(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        // H-b: PAUSE UEBERNIMMT die Blockade. Ohne diese Zeile ist sie ein No-Op an einem
        // Thread, der schon auf sein Konto-Budget geblockt ist (er ist ja `blocked`) -- und der
        // Refill hebt sie dann mit auf, obwohl PAUSE Erfolg gemeldet hat.
        // D10: **Senkung 1 von 4** von `budget_blocked_count`.
        self.set_budget_blocked(s, false);
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
        // Der offene Zyklenstempel wird **absichtlich nicht** geloescht (B-5.1). Ein migrierender
        // Thread laeuft nicht (s. oben), sein Stempel ist also normalerweise schon geschlossen.
        // Ist er es nicht, hat der Kernel `charge_current` vergessen -- und dann soll die naechste
        // Messung auf dem Zielkern als `Reject::CoreChanged` **auffallen** statt still zu
        // verschwinden. Loeschen hiesse, den einzigen Hinweis auf den vergessenen Aufruf zu tilgen.
        // Lokale Verkettung/Slot-Bezüge gehören dem Quellkern -> zurücksetzen.
        tcb.queued = NOT_QUEUED;
        tcb.qnext = NIL;
        tcb.qprev = NIL;
        // Slot freigeben (die `gid` bleibt dem Thread!). `gen` NICHT erhöhen — die
        // Identität des Threads bleibt bestehen, er wechselt nur den Kern.
        if self.tcbs[s].depleted {
            self.depleted_count -= 1;
        }
        // D10: dritte Stelle, die einen `Tcb` als Ganzes ueberschreibt. Heute **strukturell
        // unerreichbar** -- `budget_blocked` wird nur an Threads mit `sc_donor` gesetzt, und
        // der Aufruf ist oben schon an `sc_donor.is_some()` gescheitert. Die Zeile steht
        // trotzdem hier: die Richtigkeit des Zaehlers soll nicht davon abhaengen, dass diese
        // Herleitung jede kuenftige Aenderung ueberlebt.
        if self.tcbs[s].budget_blocked {
            self.budget_blocked_count -= 1;
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
        // D10: Gegenstueck zu `detach_for_migration` -- ein ankommender `Tcb` bringt sein
        // `budget_blocked` mit, ohne den Helfer zu durchlaufen. Heute ebenso unerreichbar
        // (was nicht abwandern kann, kommt auch nicht an), und aus demselben Grund hier.
        if tcb.budget_blocked {
            self.budget_blocked_count += 1;
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
            // der Tick kostet nichts, unabhängig von der Tabellengröße). **Gemessen**
            // (`tools/sched-erschoepfung-messen.sh`, L0): 0 Iterationen über 200 Ticks, bei
            // 32 wie bei 10 000 Slots.
            //
            // Was der Satz NICHT sagt, und ab hier steht es dabei (D10):
            //  * Ist **irgendein** Konto erschöpft, kostet jeder Tick einen vollen
            //    Tabellendurchlauf — 10 000 Iterationen bei 10 000 Slots, und zwar für
            //    jeden Tick bis zum Refill (L1: `aussen_je_wartetakt`). Das ist die äußere
            //    Schleife und war schon vor der Budget-Donation so.
            //  * Der **Weckelauf** in `refill_depleted` kostet noch einmal so viel je
            //    aufgefülltem Konto. Seit D10 läuft er nur bei `budget_blocked_count > 0`;
            //    ohne diesen Wächter kostete ein Tick, in dem 100 Konten zugleich auffüllen,
            //    1 000 000 Iterationen (L3, und dieser Fall ist erreichbar).
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
                    if acct != cur && !self.tcbs[cur].blocked {
                        // H-b: nur wer nicht schon aus einem ANDEREN Grund blockiert ist, wird
                        // hier blockiert -- und der Grund wird mitgeschrieben.
                        // D10: **Erhöhung 1 von 2** von `budget_blocked_count`.
                        self.tcbs[cur].blocked = true;
                        self.set_budget_blocked(cur, true);
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
                // H-b: die Spende ist ein STAPEL. `sc_donee` ist nur ihre Spitze, wird vom
                // zweiten CALL ueberschrieben und vom inneren REPLY geloescht -- geweckt wird
                // deshalb, wer WEGEN DIESES KONTOS blockiert ist, und nur der.
                //
                // **D10: der Weckelauf nur, wenn ueberhaupt jemand budget-blockiert ist.** Er
                // kostet einen VOLLEN Tabellendurchlauf, und zwar je aufgefuelltem Konto --
                // gemessen 10 000 Iterationen bei 10 000 Slots, und 1 000 000 in EINEM
                // Timer-Interrupt, sobald 100 Konten im selben Tick auffuellen (erreichbar,
                // s. `tools/sched-erschoepfung-messen.sh` L3). Der Normalfall ist: niemand.
                //
                // **Was hier NICHT stehen darf:** `if self.tcbs[slot].sc_donee.is_some()`.
                // Genau dann ist `sc_donee` `None`, waehrend Donees warten (D5, die
                // verschachtelte Spende) -- die Abkuerzung risse D5 wieder auf.
                if self.budget_blocked_count > 0 {
                    for d in 0..self.tcbs.len() {
                        if d != slot
                            && self.tcbs[d].used
                            && self.tcbs[d].budget_blocked
                            && self.tcbs[d].sc_donor == Some(slot)
                        {
                            // D10: **Senkung 2 von 4**.
                            self.set_budget_blocked(d, false);
                            self.tcbs[d].blocked = false;
                            self.enqueue_ready(d);
                        }
                    }
                }
                match self.tcbs[slot].sc_donee {
                    Some(d) if d != slot => {
                        let _ = d; // erledigt der Lauf darueber
                    }
                    // **Einen PAUSIERTEN Thread weckt der Refill nicht** (D8/M4, gemessen
                    // 2026-08-03). Bis hierher reihte der Refill bedingungslos ein: erschöpfen,
                    // PAUSE, 100 Ticks warten -- und der pausierte Thread (`blocked = 1`) wurde
                    // `current` und verbrauchte eine Zeitscheibe, `audit() == 0`. Dafür brauchte
                    // es nicht einmal ein `unblock`; PAUSE hielt schlicht nicht.
                    // Der `current`-Teil verhindert zusätzlich Audit-Code 4 (`current` steht
                    // zugleich in einer Ready-Liste), wenn der Refill einen gerade laufenden
                    // erschöpften Thread trifft.
                    _ => {
                        if !self.tcbs[slot].blocked && self.current != Some(slot) {
                            self.enqueue_ready(slot);
                        }
                    }
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
                // H-b: mit `depleted` verschwindet der Anlass, aus dem der Donee je wieder
                // geweckt wuerde -- also hier wecken.
                // D10: derselbe Waechter wie im Refill -- der Lauf ist O(n), der Normalfall
                // ist „niemand ist budget-blockiert" (gemessen: 10 000 Iterationen je
                // `set_budget` auf ein erschoepftes Konto bei 10 000 Slots).
                if self.budget_blocked_count > 0 {
                    for d in 0..self.tcbs.len() {
                        if d != s
                            && self.tcbs[d].used
                            && self.tcbs[d].budget_blocked
                            && self.tcbs[d].sc_donor == Some(s)
                        {
                            // D10: **Senkung 3 von 4**.
                            self.set_budget_blocked(d, false);
                            self.tcbs[d].blocked = false;
                            self.enqueue_ready(d);
                        }
                    }
                }
                if self.current != Some(s) && !self.tcbs[s].blocked {
                    self.enqueue_ready(s);
                }
            }
            true
        } else {
            false
        }
    }

    /// Wie viele **Ticks** dieser Kern gesehen hat.
    ///
    /// Die Vergleichszahl zur Zyklenabrechnung (B-5.1): die Tick-Rechnung kann höchstens einmal
    /// je Tick belasten. Liegt die Zahl der Zyklenproben deutlich darüber, sind genau so viele
    /// Abrechnungsereignisse **zwischen** den Ticks passiert — und die fielen vorher weg.
    pub fn ticks(&self) -> u64 {
        self.now
    }

    /// MCS-Telemetrie dieses Kerns: (Budget-Erschöpfungen, Refills). Für Tests.
    pub fn budget_stats(&self) -> (u64, u64) {
        (self.depletions, self.refills)
    }

    // --- Zyklenabrechnung (B-5.1) -------------------------------------------------------------
    //
    // **Bewusst additiv.** `on_tick`/`switch_to`/`block_current` behalten ihre Signatur; die
    // Abrechnung klammert sie ein. Der Grund ist nicht Bequemlichkeit: die Uhr gehoert dem
    // Kernel (nur er kennt `hal::timer::cycles()` und die Invarianz-Zusage), die Rechnung
    // gehoert hierher. Wuerde diese Crate die Uhr selbst lesen, waere die Arithmetik an ein
    // Ziel gebunden und damit genau das nicht mehr, was sie sein muss: host-pruefbar.
    //
    // Die Tick-Rechnung (`remaining`) bleibt **unangetastet**. B-5.1 liefert die Messung; die
    // Verdraengung von der Messung zu entkoppeln ist B-5.2. Beides in einem Schritt zu aendern
    // hiesse, MCS umzubauen, waehrend man die Messung erst einfuehrt.

    /// Die Zyklenquelle als **invariant** zusichern — oder die Zusicherung zurueckziehen.
    ///
    /// Nur der Kernel kann das beantworten (x86: `CPUID.80000007H:EDX.InvariantTSC`, unter TCG
    /// nicht zugesichert). Ohne diesen Aufruf bleibt es bei [`Source::Untrusted`], und dann wird
    /// **nichts** abgerechnet — sichtbar an den Ablehnungszaehlern, nicht als stille Null.
    pub fn set_cycle_source(&mut self, source: Source) {
        self.cycle_source = source;
    }

    /// Den laufenden Thread bis `now` belasten und seinen Stempel schliessen.
    ///
    /// **Vor** jeder Umplanung zu rufen (`on_tick`, `switch_to`, `block_current`,
    /// `exit_current`, `yield`). Ohne offenen Stempel ein No-Op — ein doppelter Aufruf rechnet
    /// also nicht doppelt ab.
    pub fn charge_current(&mut self, core: usize, now: u64) {
        debug_assert_eq!(core, self.core);
        let Some(cur) = self.current else { return };
        let Some(prev) = self.tcbs[cur].stamp.take() else {
            return;
        };
        let s = cycles::measure(
            prev,
            Stamp { cycles: now, core: core as u16 },
            self.cycle_source,
            MAX_PLAUSIBLE_SLICE,
        );
        // Gegen dasselbe **Konto** wie die Tick-Rechnung: laeuft der Thread auf einem
        // geliehenen Scheduling-Context (Donation), zahlt der Spender. Sonst haetten
        // Zyklen- und Tick-Rechnung zwei verschiedene Schuldner, und die Monitoring-Cap
        // zeigte etwas anderes als das Budget durchsetzt.
        let acct = self.tcbs[cur].sc_donor.unwrap_or(cur);
        self.tcbs[acct].cyc.apply(s);
        self.cyc_core.apply(s);
    }

    /// Fuer den (nach der Umplanung) laufenden Thread einen neuen Stempel setzen.
    /// **Nach** jeder Umplanung zu rufen.
    pub fn stamp_current(&mut self, core: usize, now: u64) {
        debug_assert_eq!(core, self.core);
        if let Some(cur) = self.current {
            self.tcbs[cur].stamp = Some(Stamp { cycles: now, core: core as u16 });
        }
    }

    /// Das Zyklenkonto eines Threads (Grundlage der Monitoring-Cap).
    ///
    /// `None` bei ungueltigem Handle. Beachte [`CycleStats::measurable`]: ist es `false`, ist
    /// `consumed` keine gemessene Null, sondern eine Leerstelle.
    pub fn consumed_cycles(&self, tid: ThreadId) -> Option<CycleStats> {
        self.resolve(tid).map(|s| self.tcbs[s].cyc)
    }

    /// Kernweite Zyklentelemetrie — vor allem die Ablehnungsgruende: sie haengen an der
    /// **Maschine** (Quelle nicht invariant, Zaehler springt) und nicht am einzelnen Thread.
    pub fn cycle_stats(&self) -> CycleStats {
        self.cyc_core
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
    /// keiner Liste/nicht laufend), 8=Directory-Eintrag passt nicht zum TCB,
    /// 9=**erschöpfter** Thread steht in einer Ready-Liste (D8, seit 2026-08-03 — die
    /// Gegenrichtung zu 7; ohne sie war der Zustand unbeobachtbar, der `mcs_bound` widerlegt),
    /// 10=`budget_blocked_count` weicht von der **Nachzählung** der Tabelle ab (D10).
    ///
    /// Zu 10: der Zähler entscheidet, ob der teure Weckelauf überhaupt läuft. Lügt er nach
    /// oben, läuft der Lauf für immer (die `depleted_count`-Form aus D8/M5); lügt er nach
    /// unten, bleibt ein Donee liegen, den niemand mehr weckt (die D5-Form). Beide Richtungen
    /// sind ohne diese Zeile unbeobachtbar — genau der Zustand, den D8/M5 hatte.
    /// **Zur Nummer:** `kernel/src/system.rs` meldet Scheduler-Codes als `10 + code`, die
    /// nächste Kategorie beginnt bei `20 + cdt` mit `cdt >= 1`. Damit ist `10` die letzte
    /// Zahl, die dort nicht mit einer anderen Kategorie zusammenfällt.
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
                // **Die Gegenrichtung zu Code 7** (D8/B2, 2026-08-03). Code 7 meldet
                // „lauffähig, aber in keiner Liste"; dass ein ERSCHÖPFTER Thread in einer Liste
                // steht, prüfte bis hierher niemand -- genau der Zustand, der `mcs_bound`
                // widerlegt, war damit unbeobachtbar. Dieselbe Form wie die leere
                // Ereigniswarteschlange ohne `CD.R`: die Aussage sah wahr aus, weil der Fall,
                // der sie widerlegt, nirgends abgefragt wurde.
                if t.depleted {
                    return 9;
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
        // Verlorene Threads + Directory-Konsistenz. Die D10-Nachzählung läuft in DIESER
        // Schleife mit — sie kostet damit nichts über das hinaus, was `audit` ohnehin tut.
        let mut bb = 0usize;
        for local in 0..self.tcbs.len() {
            let t = &self.tcbs[local];
            if !t.used {
                continue;
            }
            if t.budget_blocked {
                bb += 1;
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
            // **`t.admitted` gehoert in diese Bedingung** (2026-08-07, gefunden vom Audit selbst).
            //
            // Vor der D0-Behebung konnte ein benutzter, nicht blockierter, nicht laufender TCB
            // nicht ausserhalb jeder Warteschlange stehen -- `spawn` reihte sofort ein. Seit
            // `spawn_parked` gibt es diesen Zustand, und er ist RICHTIG: ein geparkter Thread
            // wartet darauf, dass sein Erzeuger ihm PD, Caps und Mappings gibt.
            //
            // Der Audit meldete ihn als Code 7 -- zu Recht, denn seine Bedingung kannte das Parken
            // nicht. Gemessen: 1 Abweichung in 600 aarch64-Laeufen (`scale : FAILURES`,
            // `sched_audit=7`, sonst alles identisch), waehrend das Bild in 56 895 x86-Laeufen nie
            // auftrat -- auf x86 gibt es 3 Zulassungsstellen, auf aarch64 70.
            //
            // **Die Schaerfe bleibt.** Fuer alles, wofuer Code 7 gebaut wurde (D8: ein erschoepfter
            // Thread, der ueber `unblock` auf leerem Konto lauffaehig wird), gilt `admitted == true`
            // -- die Bedingung greift dort unveraendert. Ausgenommen ist ausschliesslich der
            // Zustand zwischen `spawn_parked` und `admit`.
            if !t.blocked
                && !t.depleted
                && t.admitted
                && self.current != Some(local)
                && t.queued == NOT_QUEUED
            {
                return 7;
            }
        }
        // D10: die **unabhängige** Nachzählung gegen den Zähler. Ein `budget_blocked` an einem
        // freien Slot zählt hier nicht mit — `Tcb::EMPTY` hat das Feld auf `false`, und die
        // drei Bulk-Stellen führen den Zähler beim Freigeben nach.
        if bb != self.budget_blocked_count {
            return 10;
        }
        0
    }

    // --- intern ---

    /// [`Tcb::budget_blocked`] setzen/löschen — **die einzige Stelle**, die das Feld ändert,
    /// und damit die einzige, die [`Scheduler::budget_blocked_count`] bewegt (D10).
    ///
    /// Der Zähler wird hier an einen **echten Zustandswechsel** gebunden (`== an` -> raus).
    /// Genau das fehlte `depleted_count`: dort standen Erhöhung und Senkung an verschiedenen
    /// Stellen, die Erhöhung lief mehrfach, und der Zähler kam nie auf 0 zurück (D8/M5).
    /// Ein Zähler, der über „läuft der teure Pfad?" entscheidet, ist dann kein Zähler mehr,
    /// sondern eine dauerhaft wahre Bedingung.
    fn set_budget_blocked(&mut self, local: usize, an: bool) {
        if self.tcbs[local].budget_blocked == an {
            return;
        }
        self.tcbs[local].budget_blocked = an;
        if an {
            self.budget_blocked_count += 1;
        } else {
            self.budget_blocked_count -= 1;
        }
    }

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
        // H-b: ALLE Empfaenger dieser Spende loesen, nicht nur die Spitze -- und wer auf
        // dieses Konto geblockt war, verliert mit ihm seinen Wecker und muss hier frei.
        //
        // **D10: dieser Lauf bekommt bewusst KEINEN `budget_blocked_count`-Waechter.** Er tut
        // zwei Dinge, und nur eines davon haengt an `budget_blocked`: das Loesen von
        // `sc_donor` muss **immer** laufen, sonst zeigt ein Donee auf einen freigegebenen und
        // sogleich wiederverwendeten Slot und wird gegen ein fremdes Konto belastet. Gemessen
        // kostet der Lauf einen vollen Tabellendurchlauf **je Thread-Tod** (10 000 Iterationen
        // bei 10 000 Slots, `sched-erschoepfung-messen.sh` L4) -- nicht im Tick-Pfad. Wer ihn
        // bezahlt bekommen will, braucht eine **Liste** der Donees je Konto; ein Zaehler kann
        // die Frage „wer zeigt auf mich?" nicht beantworten.
        for d in 0..self.tcbs.len() {
            if d != local && self.tcbs[d].used && self.tcbs[d].sc_donor == Some(local) {
                self.tcbs[d].sc_donor = None;
                if self.tcbs[d].budget_blocked {
                    // D10: **Senkung 4 von 4**.
                    self.set_budget_blocked(d, false);
                    self.tcbs[d].blocked = false;
                    self.enqueue_ready(d);
                }
            }
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
        // D10: der Sterbende SELBST kann budget-blockiert sein (ein Donee, dessen Konto leer
        // ist, wird gekillt). Die Zuweisung darunter loescht das Feld, ohne den Helfer zu
        // durchlaufen -- eine der drei Stellen, die den Zaehler von Hand nachfuehren.
        if self.tcbs[local].budget_blocked {
            self.budget_blocked_count -= 1;
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
    /// Ein **Geräte-Fenster** (MMIO-Register oder DMA-RAM) in die VSpace des Aufrufers mappen —
    /// A-5.1, der Weg, auf dem ein Treiber im Userland an seine Hardware kommt.
    ///
    /// Getrennt von [`Self::map_frame`], weil die **Speicherattribute** verschieden sind und nicht
    /// aus den Cap-Rechten folgen: Geräteregister dürfen nicht gecacht und nicht spekulativ
    /// gelesen werden, DMA-RAM schon (bzw. nicht, je nach Kohärenz). Beides über einen Parameter
    /// zu fahren, der „Rechte" heißt, hätte die Attribute an die Rechte gekoppelt — zwei Dinge,
    /// die nichts miteinander zu tun haben.
    ///
    /// `dma`: `None` = MMIO-Register, `Some(coherent)` = DMA-RAM.
    /// Rückgabe: `None` bei Fehlschlag, sonst die **Gerätesicht** der Region (IOVA) — `0`, wenn es
    /// keine gibt (MMIO). Ein Treiber braucht beide Achsen und bekommt sie hier getrennt.
    fn map_window(
        &mut self,
        caller: ThreadId,
        base: u64,
        len: u64,
        ro: bool,
        dma: Option<bool>,
    ) -> Option<u64>;
    /// Ein zuvor per [`Self::map_window`] gemapptes Geräte-Fenster wieder entfernen.
    fn unmap_window(&mut self, caller: ThreadId, base: u64, len: u64) -> bool;
}
