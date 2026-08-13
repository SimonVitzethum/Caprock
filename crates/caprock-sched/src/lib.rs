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
//! Rohspeicher-Domäne liegt in `caprock-slab`).

mod cycles;
pub use cycles::{CycleStats, Reject, Sample, Source, Stamp, MAX_PLAUSIBLE_SLICE};

/// **Umgeleitete Syscalls** (Z26/A3). Abhängigkeitsfrei und **als Datei** host-getestet — dieselbe
/// Begründung wie bei [`cycles`]: die Fallen sind Reihenfolgen und Zahlenbereiche, mit Literalen
/// auslösbar, ohne Maschine.
pub mod redirect;

use core::sync::atomic::{AtomicU64, Ordering};
use caprock_hal::exception::init_thread_frame;
use caprock_slab::{AtomicTable, FreeList, Slab};
use caprock_sync::SpinLock;

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

/// **Ist dieser globale Thread-Slot GERADE belegt — gleich von wem?** (D15-Melder)
///
/// [`is_live`] beantwortet eine andere Frage: „lebt *dieser* Thread noch" — es prueft `gid` **und**
/// Generation und meldet deshalb `false`, sobald der Slot an einen ANDEREN Thread weitergegangen
/// ist. Genau dieser Fall ist aber der gefaehrliche: eine spaete Aufraeumung, die nur die `gid`
/// (nicht die Identitaet) in der Hand haelt, trifft dann einen **fremden, lebenden** Thread.
///
/// Der Unterschied zaehlt in JEDEM Lauf, auch ohne Unglueck — er ist die **Gelegenheit**, nicht der
/// Treffer. Bei einer Trefferquote um 0,2 % ist ein Melder, der nur beim Unglueck spricht, in 443
/// von 444 Laeufen stumm.
pub fn slot_in_use(gid: usize) -> bool {
    dir_load(gid).map(|e| e & D_USED != 0).unwrap_or(false)
}

// --- D15-Melder: Zombie-Region unter den eigenen Fuessen -------------------------------------
//
// `record_zombie` legt die Stack-Region des Sterbenden in den Zombie-Ring; von dort gibt SIE JEDER
// Kern per `reap_core` an den Allokator zurueck (die Funktion ist ausdruecklich kern-uebergreifend).
// Beim Selbst-Ende (`exit_current`) ist das aber genau die Region, auf der der Kernel in diesem
// Moment noch rechnet: der Trap-Handler laeuft auf SP_EL1, und das ist bei einem EL1-Thread SEIN
// Stack. Zwischen `record_zombie` und dem `mov sp, x0` des Vektor-Epilogs darf ein fremder Kern die
// Region also holen — und jede Vergabe des Kernels nullt.
//
// Gezaehlt wird die **Gelegenheit** (liegt der eigene Rahmen in der Region, die gerade zum Abholen
// freigegeben wird?), nicht der Ausgang. Das ist in jedem Lauf pruefbar.
static ZOMBIE_GESAMT: AtomicU64 = AtomicU64::new(0);
static ZOMBIE_UNTER_FUESSEN: AtomicU64 = AtomicU64::new(0);

/// `(aufgezeichnete Zombies mit Region, davon unter den eigenen Fuessen)`.
pub fn zombie_fuss_stats() -> (u64, u64) {
    (
        ZOMBIE_GESAMT.load(Ordering::Relaxed),
        ZOMBIE_UNTER_FUESSEN.load(Ordering::Relaxed),
    )
}

/// Eine Adresse im **aktuellen** Stack-Rahmen (arch-neutral, ohne `asm!`).
#[inline(never)]
fn stapeladresse() -> usize {
    let anker = 0u8;
    core::hint::black_box(&anker) as *const u8 as usize
}

// Hier stand am 2026-08-13 zusaetzlich ein Kanarienvogel am Boden der Region, den der Kernel am
// spaetestmoeglichen Punkt gegenlas. Ergebnis ueber 4295 Fenster (108 Laeufe): **0 Diebstaehle**.
// Ausgebaut, weil er in eine zur Abholung freigegebene Region schreibt — ein Messwerkzeug, kein
// Kernelbestandteil. Die Gelegenheit zaehlt der Zaehler oben weiter, ohne einen Schreibzugriff.

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
    /// **Die Generation** — der Teil der Identitaet, den [`ThreadId::slot`] gerade NICHT traegt.
    ///
    /// Wer eine per-Thread-Tabelle ueber `slot()` indiziert, hat den Slot und nicht den Thread. Die
    /// Generation daneben zu legen macht aus einem Index wieder eine Identitaet — und genau das
    /// unterscheidet „ich raeume meinen Kstack auf" von „ich raeume den seines Nachfolgers auf".
    pub fn gen(self) -> u32 {
        self.gen
    }
}

// ---------------------------------------------------------------------------------------
// TCB
// ---------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------
// Der Blockadegrund ist eine MENGE (Z24)
// ---------------------------------------------------------------------------------------

/// **Warum ein Thread nicht laufen darf — als MENGE, nicht als Sammlung einzelner Bits.**
///
/// Lauffähig ist ein Thread **genau dann, wenn die Menge leer ist**. Ein Grund wird **einzeln**
/// entfernt, und eingereiht wird **nur bei leerer Menge**; ohne diesen zweiten Halbsatz wäre die
/// Menge bloss eine andere Schreibweise für dieselben Bits.
///
/// ## Warum das kein weiterer Wächter ist, sondern ein Umbau
///
/// Es war die **dritte** Instanz derselben Klasse: D9 spaltete `budget_blocked` von `blocked` ab,
/// Z22 P4 legte `parked` daneben, und die Naht `thaw × park` riss erneut. Jede Abspaltung
/// repariert die letzte Kollision und **stellt die nächste auf** — jede Stelle, die über „läuft
/// er?" urteilt, muss ab da *alle* Bits kennen, und die eine, die eines vergisst, ist ein neuer
/// stiller Pfad.
///
/// Mit der Menge ist der gefundene Fehler nicht behoben, sondern **unformulierbar**: `thaw`
/// entfernt `PAUSE`; ist `PARK` gesetzt, ist die Menge nicht leer, und der geparkte Thread läuft
/// nicht los.
///
/// ## Was NICHT hineingehört
///
/// [`Tcb::park_wake`] bleibt ein eigenes Feld. Es ist eine **Marke** („jemand hat geweckt, bevor
/// du schliefst"), kein Blockadegrund — es in die Menge zu ziehen wäre dieselbe Verwechslung noch
/// einmal, nur mit einem hübscheren Typ.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BlockReasons(u8);

impl BlockReasons {
    /// Wartet auf ein IPC-Rendezvous. Wecker: `unblock` (aus `send`/`reply`).
    pub const IPC: u8 = 1 << 0;
    /// Das belastete Konto ist leer. Wecker: `refill_depleted` — und **nur** der.
    pub const BUDGET: u8 = 1 << 1;
    /// Eine **Autoritätsentscheidung** (`SYS_PDCTL` PAUSE, später der Gruppenschnitt aus Z23).
    /// Wecker: `resume`/`thaw`.
    pub const PAUSE: u8 = 1 << 2;
    /// Der Thread hat `SYS_PARK` gerufen. Wecker: `unpark` — und **nur** der.
    pub const PARK: u8 = 1 << 3;
    /// **Der Syscall (oder Fault) dieses Threads liegt bei seiner Persönlichkeits-PD** (Z26/A3).
    /// Wecker: [`Scheduler::handler_reply`] — und **nur** der.
    ///
    /// ## Warum das von Tag eins ein Grund IST und kein Bit daneben
    ///
    /// Z26/Nachtrag 3 nennt diesen Fall die **fünfte Instanz** derselben Klasse — diesmal
    /// vorhersagbar statt gefunden. Läge er als eigenes Bit neben der Menge, wiederholte sich die
    /// Park-Naht wörtlich: `thaw` weckt einen Handler-Wartenden, oder das `unpark` eines
    /// Geschwisters verbraucht die Marke, und der Gast **läuft mit halbem Syscall weiter** — mit
    /// einem Frame, den sein Kernel noch nicht fertig beschrieben hat.
    ///
    /// In der Menge ist genau das **unformulierbar**: `resume` entfernt `PAUSE`, `unpark`
    /// entfernt `PARK`, `unblock` entfernt `IPC` — keiner von ihnen entfernt `HANDLER`, und
    /// eingereiht wird nur bei leerer Menge.
    pub const HANDLER: u8 = 1 << 4;
    /// **Der `SYS_LOAD` dieses Threads liegt beim Verifiziererthread** (C8).
    /// Wecker: [`Scheduler::load_reply`] — und **nur** der.
    ///
    /// ## Warum ein eigener Grund und kein Bit daneben
    ///
    /// Es ist die **sechste** Instanz derselben Klasse, und wie bei [`Self::HANDLER`] ist sie
    /// diesmal vorhergesagt statt gefunden. Läge sie als eigenes Bit neben der Menge, wiederholte
    /// sich die Park-Naht wörtlich: ein `resume` des Debuggers oder das `unblock` eines fremden
    /// IPC-Partners liesse den Aufrufer weiterlaufen, **bevor der Verifizierer sein Urteil in den
    /// Frame geschrieben hat** — er läse ein Ergebnisregister, das noch niemand gesetzt hat.
    ///
    /// In der Menge ist genau das unformulierbar: `resume` entfernt `PAUSE`, `unpark` entfernt
    /// `PARK`, `unblock` entfernt `IPC`, `handler_reply` entfernt `HANDLER` — keiner von ihnen
    /// entfernt `LOAD`, und eingereiht wird **nur bei leerer Menge**.
    ///
    /// ## Und warum NICHT `IPC`
    ///
    /// Der bequeme Weg wäre gewesen, den Auftrag über einen Endpoint zu schicken und den Aufrufer
    /// mit `IPC` blockieren zu lassen. Dann aber weckte ihn jedes `reply` **irgendeines** Servers,
    /// an dem er zufällig hängt, und `purge_ipc_queues` sähe eine Wartebeziehung, die es nicht
    /// gibt. Ein Grund, der zwei Lagen trägt, macht den Wecker unbestimmbar — das ist D9.
    pub const LOAD: u8 = 1 << 5;

    /// Leere Menge = lauffähig.
    pub const NONE: Self = Self(0);
    /// Darf dieser Thread laufen?
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    /// Ist dieser Grund gesetzt?
    pub fn has(self, grund: u8) -> bool {
        self.0 & grund != 0
    }
    /// Grund hinzufügen (idempotent).
    pub fn insert(&mut self, grund: u8) {
        self.0 |= grund;
    }
    /// **Einen** Grund entfernen — nie „alle".
    pub fn remove(&mut self, grund: u8) {
        self.0 &= !grund;
    }
    /// Rohbits, nur für Bericht und Modelltreue-Wächter.
    pub fn bits(self) -> u8 {
        self.0
    }
}

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
    /// **Warum** der Thread nicht laufen darf (Z24). Leer = lauffähig. Ersetzt die drei früheren
    /// Bits `blocked`/`budget_blocked`/`parked`, die einander verdeckten.
    reasons: BlockReasons,
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
    /// **Weckmarke**: jemand hat `unpark` gerufen. Ohne sie gaebe es ein verlorenes Wecken --
    /// wer die Bedingung prueft (falsch), dann geweckt wird, dann parkt, schliefe fuer immer.
    /// Genau die Stelle, an der ein Futex seinen Vergleichswert braucht; hier reicht eine Marke,
    /// weil sie **im selben Scheduler-Lock** gesetzt und geprueft wird wie die Blockade.
    park_wake: bool,
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
    /// **„Die Syscalls dieses Threads gehen an H"** (Z26/A3). `None` = an den Caprock-Kernel.
    ///
    /// Steht im TCB und **nicht** in der PD-Tabelle, aus zwei Gründen. Erstens ist die Bindung
    /// per Entwurf **je Thread**: eine PD kann Threads mit und ohne Persönlichkeit haben (der
    /// Ladeprozess einer Persönlichkeit ist selbst kein Gast). Zweitens wandert sie so bei einer
    /// Migration **umsonst** mit — `Migrant` trägt den ganzen `Tcb`. Läge sie neben dem Thread,
    /// wäre sie die nächste Stelle, die eine Migration vergisst.
    handler: Option<redirect::Bindung>,
}

impl Tcb {
    const EMPTY: Tcb = Tcb {
        used: false,
        gid: NIL,
        gen: 0,
        sp: 0,
        priority: 0,
        reasons: BlockReasons::NONE,
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
        park_wake: false,
        sc_donor: None,
        sc_donee: None,
        cyc: CycleStats::EMPTY,
        stamp: None,
        handler: None,
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
    ///
    /// **Der Grund kommt vom Aufrufer** (Z24). Dieser Weg wird von IPC *und* von
    /// [`park_current`](Self::park_current) benutzt; ein fest verdrahteter Grund hier wäre genau
    /// die Mehrdeutigkeit, die der Umbau beseitigt. `block_current` bleibt als **IPC-Fassung**,
    /// weil das der überwiegende Aufrufer ist und ein Argument an 40 Stellen nichts erklärt.
    pub fn block_current(&mut self, core: usize, frame: usize) -> usize {
        self.block_current_mit(core, frame, BlockReasons::IPC)
    }

    /// Wie [`block_current`](Self::block_current), aber mit **benanntem** Grund.
    pub fn block_current_mit(&mut self, core: usize, frame: usize, grund: u8) -> usize {
        debug_assert_eq!(core, self.core);
        let cur = self.current.expect("kein laufender Thread");
        self.tcbs[cur].sp = frame;
        self.tcbs[cur].reasons.insert(grund);
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
        self.tcbs[cur].reasons.insert(BlockReasons::IPC);
        let t = self.resolve(target).expect("Zielthread ungültig/fremder Kern");
        // **EINEN Grund entfernen, nicht alle** (Z24). Das Ziel wartete auf das Rendezvous; war es
        // zusätzlich pausiert oder erschöpft, bleibt es das -- vorher hätte `blocked = false`
        // beides mit weggewischt.
        self.tcbs[t].reasons.remove(BlockReasons::IPC);
        // **Und hier ist der Fastpath BEDINGT** (Z24). `switch_to` laesst das Ziel *unmittelbar*
        // laufen -- das darf nur, wer wirklich lauffaehig ist. Vorher stand hier
        // `blocked = false`, also „laufe, gleich was sonst gegen dich vorliegt": ein Thread, der
        // in `RECV` steht und **pausiert** wurde, lief beim naechsten `send` los, und die
        // Pausen-Entscheidung war still verloren. Genau die vierte Instanz, gegen die dieser
        // Umbau gebaut ist -- sie steckte im Fastpath, nicht in `unblock`.
        //
        // Ist noch ein Grund offen, ist die **Nachricht trotzdem zugestellt** (der IPC-Grund ist
        // weg); nur der Wechsel entfaellt, und der Aufrufer gibt an den naechsten *bereiten*
        // Thread ab. Der Empfaenger laeuft, sobald sein letzter Grund faellt.
        if !self.tcbs[t].reasons.is_empty() {
            let next = self
                .dequeue_highest()
                .expect("Idle-Thread sollte immer bereit sein");
            self.current = Some(next);
            return self.tcbs[next].sp;
        }
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

    /// **Der einzige Weg zurueck in die Ready-Queue** (Z24).
    ///
    /// Eingereiht wird **nur bei leerer Grund-Menge**. Das ist die Aussage, die den ganzen Umbau
    /// traegt: ohne sie waere die Menge bloss eine andere Schreibweise fuer dieselben Bits, und
    /// jeder Wecker koennte weiterhin eine fremde Entscheidung mit aufheben.
    ///
    /// `depleted` steht daneben und nicht darin: es ist eine Eigenschaft des **Kontos**, kein
    /// Grund des Threads -- ein Thread kann auf einem erschoepften Konto sitzen, ohne dass ihm
    /// selbst etwas vorliegt. Sein Wecker ist `refill_depleted`.
    fn wecke_falls_lauffaehig(&mut self, local: usize) {
        if self.tcbs[local].reasons.is_empty()
            && !self.tcbs[local].depleted
            && self.current != Some(local)
        {
            self.enqueue_ready(local);
        }
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
        if !self.tcbs[s].reasons.is_empty() {
            // **Der IPC-Grund faellt, und NUR er** (Z24). Die alte Fassung schrieb
            // `blocked = false` und musste deshalb vorher von Hand pruefen, ob nicht ein anderer
            // Grund dahintersteckt (H-b). Diese Handpruefung entfaellt: was `unblock` nicht
            // entfernt, bleibt stehen.
            let acct = self.tcbs[s].sc_donor.unwrap_or(s);
            if acct != s && self.tcbs[acct].depleted {
                // Zurueck in den Lauf ja -- aber nicht auf ein leeres Konto. Der IPC-Grund faellt,
                // dafuer kommt der Budget-Grund, und **der hat einen Wecker** (`refill_depleted`).
                // D10: **Erhöhung 2 von 2** von `budget_blocked_count`.
                self.tcbs[s].reasons.remove(BlockReasons::IPC);
                self.set_budget_blocked(s, true);
                return true;
            }
            self.tcbs[s].reasons.remove(BlockReasons::IPC);
            // **Erschöpft heisst: nicht einplanen** (D8, gemessen 2026-08-03). Bis dahin
            // reihte `unblock` bedingungslos ein, und ein erschöpfter Thread lief danach eine
            // volle Zeitscheibe auf leerem Konto -- mit einer bereitstehenden Alternative
            // daneben und `audit() == 0`. Erreichbar OHNE Cap: `switch_to` spendet beim
            // IPC-CALL das Konto des Aufrufers, `on_tick` belastet es und setzt `depleted` am
            // **blockierten** Aufrufer, `reply` ruft `unblock(caller)`.
            //
            // **Und hier steht die Aussage, die den ganzen Umbau traegt** (Z24): eingereiht wird
            // nur bei **leerer Menge**. Ohne diesen Halbsatz waere die Menge bloss eine andere
            // Schreibweise fuer dieselben Bits -- ein pausierter oder geparkter Thread liefe beim
            // naechsten `reply` seines Partners los, und die fremde Entscheidung waere still weg.
            // `depleted` bleibt zusaetzlich stehen: es ist eine Konto-Eigenschaft, kein Grund,
            // und sein Wecker ist `refill_depleted`.
            if self.tcbs[s].reasons.is_empty() && !self.tcbs[s].depleted {
                self.enqueue_ready(s);
            }
        }
        true
    }

    /// **Den laufenden Thread parken — es sei denn, eine Weckmarke liegt schon vor** (Z22, P4).
    ///
    /// Das ist die Grundlage, auf der eine PD `wait_event`, Completions und Mutex-Warteschlangen
    /// baut, **ohne je ein Kernelobjekt anzulegen**: die Warteschlange selbst ist eine Liste im
    /// Speicher der PD, der Kernel kennt nur „schlafe" und „wecke Thread T".
    ///
    /// ## Warum die Marke nicht weggelassen werden kann
    ///
    /// Ohne sie ist die Folge `Bedingung pruefen (falsch)` → `parken` unterbrechbar: setzt ein
    /// anderer Thread dazwischen die Bedingung und weckt, trifft das Wecken einen Thread, der noch
    /// nicht schlaeft — es verpufft, und danach schlaeft er fuer immer. Das ist dasselbe verlorene
    /// Wecken, gegen das ein Futex seinen Vergleichswert braucht. Hier genuegt eine Marke, weil
    /// Pruefung und Blockade **unter demselben Kern-Lock** stattfinden.
    ///
    /// `None` heisst „nicht blockiert, die Marke war da und ist verbraucht" — der Aufrufer laeuft
    /// weiter. `Some(sp)` ist der Stackpointer des naechsten Threads.
    pub fn park_current(&mut self, core: usize, frame: usize) -> Option<usize> {
        debug_assert_eq!(core, self.core);
        let cur = self.current.expect("kein laufender Thread");
        if self.tcbs[cur].park_wake {
            // Verbraucht: eine Marke weckt genau einmal. Sonst liefe der naechste `park` durch,
            // obwohl niemand mehr geweckt hat.
            self.tcbs[cur].park_wake = false;
            return None;
        }
        Some(self.block_current_mit(core, frame, BlockReasons::PARK))
    }

    /// **Einen geparkten Thread wecken — und die Marke IMMER hinterlegen** (Z22, P4).
    ///
    /// Die Marke wird auch dann gesetzt, wenn der Thread gar nicht schlaeft: genau das ist der
    /// Fall, in dem das Wecken sonst verlorenginge (er ist zwischen Pruefung und `park`).
    ///
    /// **Geweckt wird nur, wer WEGEN `park` blockiert ist.** Ein Thread, der in IPC wartet oder
    /// pausiert wurde, bleibt liegen — eine Blockade aufzuheben, deren Grund man nicht kennt, ist
    /// der Fehler aus D9, und er hat dort vier von fuenf Befunden erzeugt.
    ///
    /// Rückgabe wie [`unblock`](Self::unblock): konnte der Thread auf **diesem** Kern aufgelöst
    /// werden? `false` heisst „tot oder migriert" — der Aufrufer wiederholt beim neuen Besitzer.
    pub fn unpark(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        self.tcbs[s].park_wake = true;
        if self.tcbs[s].reasons.has(BlockReasons::PARK) {
            // Die Marke ist gesetzt UND er schlief -> sie hat ihre Wirkung hier getan und wird
            // sofort verbraucht. Bliebe sie stehen, liefe sein naechstes `park` grundlos durch.
            self.tcbs[s].park_wake = false;
            // **Genau EIN Grund faellt** (Z24). Die alte Fassung setzte `parked = false` und rief
            // dann `unblock`, das `blocked = false` schrieb -- also zwei Schreibvorgaenge fuer
            // eine Aussage, und der zweite konnte einen fremden Grund mitnehmen. Jetzt ist es
            // eine Zeile, und die D9-Aussage („`unpark` weckt keinen IPC-Wartenden") ist keine
            // eigene Regel mehr, sondern folgt daraus, dass PARK nicht IPC ist.
            self.tcbs[s].reasons.remove(BlockReasons::PARK);
            self.wecke_falls_lauffaehig(s);
        }
        true
    }

    /// Schläft dieser Thread **wegen `park`**? (Nur Telemetrie/Prüfung — der `park`-Nachweis in
    /// der Suite braucht die Unterscheidung zu einer IPC-Blockade.)
    pub fn is_parked(&self, tid: ThreadId) -> bool {
        self.resolve(tid)
            .is_some_and(|s| self.tcbs[s].reasons.has(BlockReasons::PARK))
    }

    /// Liegt für diesen Thread eine unverbrauchte Weckmarke vor? (Nur Telemetrie/Prüfung.)
    pub fn has_wake_token(&self, tid: ThreadId) -> bool {
        self.resolve(tid).is_some_and(|s| self.tcbs[s].park_wake)
    }

    /// **Die Grund-Menge dieses Threads, roh** (Z24) — für Diagnose, nicht für Entscheidungen.
    ///
    /// `None` heisst „auf DIESEM Kern nicht auflösbar" (tot oder migriert) und ist damit von
    /// „läuft, ohne Grund" (`Some(leer)`) unterscheidbar. Genau diese Unterscheidung fehlt einem
    /// `bool`, und sie ist die Frage, wenn eine PD schweigt: **gibt es den Thread überhaupt?**
    pub fn reasons_of(&self, tid: ThreadId) -> Option<BlockReasons> {
        self.resolve(tid).map(|s| self.tcbs[s].reasons)
    }

    /// Wurde dieser Thread **zugelassen** (D0)? `None` = nicht auflösbar.
    pub fn admitted_of(&self, tid: ThreadId) -> Option<bool> {
        self.resolve(tid).map(|s| self.tcbs[s].admitted)
    }

    /// Ist dieser Thread blockiert — **gleich aus welchem Grund**? (Nur Telemetrie/Prüfung.)
    ///
    /// Genau die Größe, die der D9-Nachweis braucht: ob `unpark` einen Thread aus seiner Blockade
    /// geholt hat, ist an `parked` **nicht** ablesbar (das Bit ist bei einem IPC-Wartenden ohnehin
    /// falsch, vorher wie nachher). Die erste Fassung des Prüfers las genau dort nach und hätte
    /// den Fehler nie gesehen.
    pub fn is_blocked(&self, tid: ThreadId) -> bool {
        self.resolve(tid)
            .is_some_and(|s| !self.tcbs[s].reasons.is_empty())
    }

    /// Einen **bestimmten** Thread dieses Kerns extern pausieren (`SYS_PDCTL` PAUSE).
    /// Rückgabe wie [`unblock`](Self::unblock): konnte der Thread hier aufgelöst werden?
    pub fn pause(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        // **H-b ist WEGGEFALLEN, nicht umgeschrieben** (Z24). Bis dahin stand hier
        // `set_budget_blocked(s, false)` mit der Begruendung „PAUSE UEBERNIMMT die Blockade" --
        // `pause` musste einen **fremden** Grund LOESCHEN, um den eigenen durchzusetzen, weil es
        // mit einem einzigen Bit keine andere Moeglichkeit gab. Der Preis stand im Kommentar
        // daneben: ein pausierter und wieder fortgesetzter Thread lief auf einem LEEREN Konto
        // weiter, weil sein Budget-Grund vergessen war.
        //
        // Mit der Menge verschwindet der Griff ersatzlos: `PAUSE` kommt dazu, `BUDGET` bleibt
        // stehen, `refill_depleted` entfernt spaeter `BUDGET`, und `PAUSE` bleibt.
        self.tcbs[s].reasons.insert(BlockReasons::PAUSE);
        self.remove_from_ready(s); // No-Op, falls er gerade `current` oder nicht eingereiht ist
        true
    }

    /// **Eine Pause aufheben** (`SYS_PDCTL` RESUME/START) — Z24.
    ///
    /// Entfernt `PAUSE` und **nur** das. Ein Thread, der zusaetzlich in IPC wartet oder auf einem
    /// leeren Konto sitzt, bleibt liegen: sein Wecker ist ein anderer, und ihn hier mitzuwecken
    /// waere genau der D9-Fehler von der anderen Seite.
    ///
    /// Rückgabe wie [`unblock`](Self::unblock): konnte der Thread auf **diesem** Kern aufgelöst
    /// werden?
    pub fn resume(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        self.tcbs[s].reasons.remove(BlockReasons::PAUSE);
        self.wecke_falls_lauffaehig(s);
        true
    }

    // -----------------------------------------------------------------------------------
    // Z26/A3: umgeleitete Syscalls
    // -----------------------------------------------------------------------------------

    /// **Die Handler-Bindung eines Threads setzen oder aufheben** (Z26/A3).
    ///
    /// Der Scheduler **entscheidet hier nichts** — Cap-Prüfung, Zyklusverbot und Slot-Vergabe
    /// liegen im Dispatch, wo die Caps und die PD-Tabelle sind. Diese Funktion ist die Ablage.
    /// Das ist Absicht: eine Prüfung, die an zwei Stellen steht, ist an einer davon irgendwann
    /// falsch.
    ///
    /// Rückgabe wie [`unblock`](Self::unblock): auf **diesem** Kern auflösbar?
    pub fn set_handler(&mut self, tid: ThreadId, bindung: Option<redirect::Bindung>) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        self.tcbs[s].handler = bindung;
        true
    }

    /// Die Handler-Bindung eines Threads **dieses** Kerns.
    pub fn handler_of(&self, tid: ThreadId) -> Option<redirect::Bindung> {
        self.resolve(tid).and_then(|s| self.tcbs[s].handler)
    }

    /// Die Handler-Bindung des **laufenden** Threads — der heisse Pfad: genau ein Zugriff je
    /// Syscall, ohne `resolve`.
    pub fn current_handler(&self, core: usize) -> Option<redirect::Bindung> {
        debug_assert_eq!(core, self.core);
        self.current.and_then(|c| self.tcbs[c].handler)
    }

    /// **Den laufenden Thread blockieren, weil sein Syscall bei seiner Persönlichkeits-PD liegt.**
    ///
    /// Geht durch [`block_current_mit`](Self::block_current_mit) — es ist keine neue Blockade-Art,
    /// sondern ein neuer **Grund** in derselben Menge. Genau das verlangt Z26/Nachtrag 3, und
    /// genau deshalb kann kein fremder Wecker ihn aufheben.
    pub fn block_for_handler(&mut self, core: usize, frame: usize) -> usize {
        self.block_current_mit(core, frame, BlockReasons::HANDLER)
    }

    /// **Einem bereits blockierten Thread den Handler-Grund anhängen** (Z26/A3).
    ///
    /// Die Zustellung läuft über den vorhandenen Endpoint-Transport; `call` hat den Gast mit
    /// `IPC` blockiert und weggewechselt. Der zweite Grund kommt hier dazu, und **er ist der
    /// wichtigere**: die IPC-Antwort des Handlers entfernt `IPC`, aber der Gast darf erst laufen,
    /// wenn sein Frame aus dem Sidecar zurückgeschrieben ist — und das meldet `handler_reply`.
    ///
    /// Ohne diesen zweiten Grund liefe der Gast **mit halbem Syscall** weiter, genau die Wirkung,
    /// die Z26/Nachtrag 3 vorhersagt.
    pub fn mark_handler_wait(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        self.tcbs[s].reasons.insert(BlockReasons::HANDLER);
        // **Und aus der Ready-Queue nehmen, falls er darin steht.** Zwischen `call` und hier kann
        // niemand laufen (IRQs sind im Trap maskiert, der Kern-Lock wird je Operation genommen) --
        // aber `call` kann den Gast auch OHNE Blockade zurückgelassen haben, wenn ein Handler
        // schon wartete und der Fastpath griff. Dann steht er bereit, und ein Grund ohne
        // Ausreihung waere ein Thread, der blockiert IST und trotzdem in der Liste steht: genau
        // Audit-Code 2.
        self.remove_from_ready(s);
        true
    }

    /// **Der EINZIGE Wecker des Handler-Grundes** (Z26/A3).
    ///
    /// Entfernt `HANDLER` und **nur** das. Ein Gast, der zusätzlich pausiert wurde oder auf einem
    /// leeren Konto sitzt, bleibt liegen — sein Kernel hat geantwortet, seine Zulassung steht
    /// deshalb noch aus.
    ///
    /// Rückgabe wie [`unblock`](Self::unblock).
    pub fn handler_reply(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        self.tcbs[s].reasons.remove(BlockReasons::HANDLER);
        self.wecke_falls_lauffaehig(s);
        true
    }

    /// Wartet dieser Thread auf seine Persönlichkeits-PD? (Nur Telemetrie/Prüfung.)
    pub fn is_handler_blocked(&self, tid: ThreadId) -> bool {
        self.resolve(tid)
            .is_some_and(|s| self.tcbs[s].reasons.has(BlockReasons::HANDLER))
    }

    /// **Den laufenden Thread blockieren, weil sein `SYS_LOAD` beim Verifizierer liegt** (C8).
    ///
    /// Geht durch [`block_current_mit`](Self::block_current_mit) — es ist keine neue Blockade-Art,
    /// sondern ein neuer **Grund** in derselben Menge.
    ///
    /// **Der Aufrufer MUSS das tun, bevor der Auftrag für den Verifizierer sichtbar wird.**
    /// Andernfalls kann der Verifizierer (auf einem anderen Kern) fertig sein, bevor der Aufrufer
    /// blockiert ist; sein [`load_reply`](Self::load_reply) liefe dann ins Leere, und der Aufrufer
    /// setzte danach einen Grund, den niemand mehr entfernt. Das ist ein verlorenes Wecken, und es
    /// gibt hier **keine** Weckmarke, die es auffinge — der Kernel hält die Auftragsschlange
    /// deshalb über beide Schritte gesperrt (s. `verifizierer::uebergeben`).
    pub fn block_for_load(&mut self, core: usize, frame: usize) -> usize {
        self.block_current_mit(core, frame, BlockReasons::LOAD)
    }

    /// **Der EINZIGE Wecker des Lade-Grundes** (C8).
    ///
    /// Entfernt `LOAD` und **nur** das. Ein Aufrufer, der zusätzlich pausiert wurde oder auf einem
    /// leeren Konto sitzt, bleibt liegen — sein Ergebnis steht im Frame, seine Zulassung nicht.
    ///
    /// Rückgabe wie [`unblock`](Self::unblock): auf **diesem** Kern auflösbar?
    pub fn load_reply(&mut self, tid: ThreadId) -> bool {
        let Some(s) = self.resolve(tid) else {
            return false;
        };
        self.tcbs[s].reasons.remove(BlockReasons::LOAD);
        self.wecke_falls_lauffaehig(s);
        true
    }

    /// Wartet dieser Thread auf den Verifizierer? (Nur Telemetrie/Prüfung — die C8-Zeile braucht
    /// die Unterscheidung zu einer IPC-Blockade, und zwar in **beide** Richtungen: der Überläufer
    /// darf sie NICHT haben.)
    pub fn is_load_blocked(&self, tid: ThreadId) -> bool {
        self.resolve(tid)
            .is_some_and(|s| self.tcbs[s].reasons.has(BlockReasons::LOAD))
    }

    /// Wie viele Threads dieses Kerns sind **gebunden**? (Sprechprobe: ein Prüfer, der über
    /// Abwesenheit einer Umleitung urteilt, muss belegen können, dass überhaupt gebunden wurde.)
    pub fn handler_bound_count(&self) -> usize {
        (0..self.tcbs.len())
            .filter(|&s| self.tcbs[s].used && self.tcbs[s].handler.is_some())
            .count()
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
        if self.tcbs[s].reasons.has(BlockReasons::BUDGET) {
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
        if tcb.reasons.has(BlockReasons::BUDGET) {
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
            let mut requeue = self.tcbs[cur].reasons.is_empty();
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
                        // **Der Waechter „nur wer nicht schon blockiert ist" ist WEG** (Z24): der
                        // Budget-Grund kommt einfach dazu, gleich was sonst vorliegt. Vorher war
                        // er noetig, weil `blocked = true` einen fremden Grund ueberschrieben und
                        // sein spaeteres `false` ihn geloescht haette.
                        // D10: **Erhöhung 1 von 2** von `budget_blocked_count`.
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
                            && self.tcbs[d].reasons.has(BlockReasons::BUDGET)
                            && self.tcbs[d].sc_donor == Some(slot)
                        {
                            // D10: **Senkung 2 von 4**. Der Budget-Grund faellt -- und eingereiht
                            // wird nur, wenn danach kein anderer mehr steht (Z24).
                            self.set_budget_blocked(d, false);
                            self.wecke_falls_lauffaehig(d);
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
                        // **Einen PAUSIERTEN Thread weckt der Refill nicht** -- und das ist
                        // seit Z24 keine eigene Regel mehr, sondern die leere Menge.
                        if self.current != Some(slot) {
                            self.wecke_falls_lauffaehig(slot);
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
                            && self.tcbs[d].reasons.has(BlockReasons::BUDGET)
                            && self.tcbs[d].sc_donor == Some(s)
                        {
                            // D10: **Senkung 3 von 4**.
                            self.set_budget_blocked(d, false);
                            self.wecke_falls_lauffaehig(d);
                        }
                    }
                }
                if self.current != Some(s) {
                    self.wecke_falls_lauffaehig(s);
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
                if !t.reasons.is_empty() {
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
            if t.reasons.has(BlockReasons::BUDGET) {
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
            if t.reasons.is_empty()
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
        if self.tcbs[local].reasons.has(BlockReasons::BUDGET) == an {
            return;
        }
        if an {
            self.tcbs[local].reasons.insert(BlockReasons::BUDGET);
        } else {
            self.tcbs[local].reasons.remove(BlockReasons::BUDGET);
        }
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
                if self.tcbs[d].reasons.has(BlockReasons::BUDGET) {
                    // D10: **Senkung 4 von 4**.
                    self.set_budget_blocked(d, false);
                    self.wecke_falls_lauffaehig(d);
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
        if self.tcbs[local].reasons.has(BlockReasons::BUDGET) {
            self.budget_blocked_count -= 1;
        }
        self.tcbs[local] = Tcb::EMPTY;
        self.free.free(local);
        self.used -= 1;
        CORE_LOAD[self.core].store(self.used, Ordering::Relaxed);
        // D15-Melder: siehe `zombie_fuss_stats`. Ab dem Einreihen darf JEDER Kern die Region
        // abholen und freigeben — auch die, auf der dieser Aufruf gerade steht.
        if len != 0 {
            ZOMBIE_GESAMT.fetch_add(1, Ordering::Relaxed);
            let hier = stapeladresse();
            if hier >= base && hier < base + len {
                ZOMBIE_UNTER_FUESSEN.fetch_add(1, Ordering::Relaxed);
            }
        }
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
    /// **Selbst-Park mit Weckmarke** (Z22, P4). `None` = nicht blockiert (Marke war da und ist
    /// verbraucht), der Aufrufer läuft weiter. Siehe [`Scheduler::park_current`].
    fn park_current(&mut self, core: usize, frame: usize) -> Option<usize>;
    /// **Einen geparkten Thread wecken**; die Marke wird immer hinterlegt, geweckt wird nur, wer
    /// wegen `park` blockiert ist. Siehe [`Scheduler::unpark`].
    fn unpark(&mut self, tid: ThreadId);
    /// Einen bestimmten Thread extern pausieren — reversibel mit [`Self::resume`], **nicht** mit
    /// `unblock`. Seit Z24 nennt jeder Wecker den Grund, den er aufhebt.
    fn pause(&mut self, tid: ThreadId);
    /// **Die Handler-Bindung des LAUFENDEN Threads** (Z26/A3) — der heisse Pfad.
    ///
    /// Wird **einmal je Syscall** gerufen, ganz oben im Dispatch, und ist damit die einzige neue
    /// Größe auf dem unbelasteten Pfad. Deshalb liest sie den laufenden Thread direkt statt über
    /// `current_id` + `handler_of` (zwei Sperrungen statt einer).
    fn current_handler(&mut self, core: usize) -> Option<redirect::Bindung>;
    /// Die Handler-Bindung eines **beliebigen** Threads (für `SYS_SETHANDLER` und das Audit).
    fn handler_of(&mut self, tid: ThreadId) -> Option<redirect::Bindung>;
    /// Die Bindung setzen/aufheben. `false` = Thread nicht auflösbar.
    fn set_handler(&mut self, tid: ThreadId, bindung: Option<redirect::Bindung>) -> bool;
    /// Den laufenden Thread blockieren, **weil sein Syscall beim Handler liegt**
    /// ([`BlockReasons::HANDLER`]).
    fn block_for_handler(&mut self, core: usize, frame: usize) -> usize;
    /// Einem **bereits blockierten** Thread den Handler-Grund zusätzlich anhängen.
    ///
    /// Getrennt von [`Self::block_for_handler`], weil die Zustellung über den vorhandenen
    /// Endpoint-Transport läuft: `call` blockiert den Gast mit `IPC` und wechselt weg — er ist
    /// danach nicht mehr `current`. Der zweite Grund muss also an einen **benannten** Thread,
    /// nicht an „den laufenden".
    fn mark_handler_wait(&mut self, tid: ThreadId);
    /// **Der einzige Wecker des Handler-Grundes.** Entfernt `HANDLER` und nur das.
    fn handler_reply(&mut self, tid: ThreadId);
    /// Eine **Pause** aufheben (`SYS_PDCTL` RESUME/START).
    ///
    /// Bis Z24 war das `unblock` — und genau daran hing der Fehler: `unblock` hob *irgendeine*
    /// Blockade auf, also auch eine, deren Grund der Aufrufer nicht kannte. Jetzt entfernt jeder
    /// Wecker **seinen** Grund, und `resume` entfernt `PAUSE`.
    fn resume(&mut self, tid: ThreadId);
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
