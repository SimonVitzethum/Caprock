//! Kernzustand hinter **per-Kern-Scheduler-Locks** (`SCHEDS[core]`, je Kern eine
//! Instanz mit eigener TCB-Partition) und dem **einen** Ressourcen-Lock (`RES` =
//! Allokator, Capability-Space, Endpoints, Notifications, Protection Domains) —
//! plus die Trap-Hooks.
//!
//! **Echte per-Kern-parallele Einplanung:** Der heiße Timer-/IPI-Reschedule-Pfad
//! sperrt nur `SCHEDS[core]` des eigenen Kerns — Kerne planen gleichzeitig ein,
//! ohne sich gegenseitig zu blockieren. Kern-übergreifendes Aufwecken
//! ([`wake_remote`]) sperrt die **Ziel**instanz und schickt einen Reschedule-IPI.
//!
//! **Lock-Ordnung:** `RES` vor `SCHEDS[*]`, und `SCHEDS[*]` vor `FP_STATES`. Der
//! reine Reschedule-Pfad nimmt nur `SCHEDS[core]` (+ atomares `FP_OWNER`), wartet
//! also nie auf einen anderen Lock und kann an keinem Deadlock-Zyklus teilnehmen.
//! IPC sperrt `RES` dann `SCHEDS[core]` (kern-lokale Endpoint-Teilnehmer). Kein
//! Pfad nimmt zwei verschiedene `SCHEDS[*]` gleichzeitig.

use sel4lake_cap::{CapError, CapInfo, CapPtr, ObjectKind};
use sel4lake_hal::{self as hal, exception::TrapFrame, fp::FpState, println};
use sel4lake_ipc::{Endpoint, Notification, NENDPOINTS, NNOTIFICATIONS};
use sel4lake_mem::{MemoryCap, PhysAllocator, PhysRegion, Rights};
use sel4lake_microkit::Caps;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use sel4lake_sched::{SchedOps, Scheduler, ThreadId, MAX_THREADS};
use sel4lake_sync::SpinLock;

/// Anzahl Kerne (für die per-Kern FP-Owner-Tabelle). Muss ≥ der realen Kernzahl sein.
const NUM_CORES: usize = 8;

const STACK_SIZE: u64 = 64 * 1024;
/// Standardpriorität für Idle und gewöhnliche Demo-Threads (Round-Robin).
pub const IDLE_PRIO: u8 = 1;

// EL0-User-Threads: Kernel-Stacks aus einem EL1-only Pool (Linker), User-Stacks
// aus dem EL0-zugänglichen RAM. Der Kernel-Stack MUSS EL1-only sein (sonst könnte
// der EL0-Thread seinen eigenen Kernel-Stack lesen/schreiben), daher der feste Pool
// im Kernel-Image (nicht aus dem EL0-zugänglichen MEM-Allokator).
extern "C" {
    static __user_kstacks_bottom: u8;
}
const USER_KSTACK_SIZE: usize = 0x4000; // 16 KiB (muss zur Linker-Reservierung passen)
const USER_KSTACK_COUNT: usize = 16;
const KSTACK_NONE: u16 = u16::MAX;

/// Free-List + Besitzer-Abbildung des EL0-Kernel-Stack-Pools. Beim Thread-Ende wird
/// der Slot zurückgegeben (statt geleakt), sodass über die Laufzeit beliebig viele
/// (nicht gleichzeitig > Pool) EL0-Threads erzeugt werden können.
struct KstackPool {
    /// `free[i]` = Slot `i` verfügbar.
    free: [bool; USER_KSTACK_COUNT],
    /// Pool-Slot je globalem Thread-Slot (`KSTACK_NONE` = keiner).
    slot_of: [u16; MAX_THREADS],
}
/// Pool-Lock. Wird stets **allein** gehalten (nie verschachtelt mit anderen Locks)
/// -> keine Sperrordnungs-Beschränkung.
static KSTACKS: SpinLock<KstackPool> = SpinLock::new(KstackPool {
    free: [true; USER_KSTACK_COUNT],
    slot_of: [KSTACK_NONE; MAX_THREADS],
});

/// Einen freien Kernel-Stack-Pool-Slot reservieren (Free-List). `None` = Pool voll.
fn claim_user_kstack() -> Option<usize> {
    let mut p = KSTACKS.lock();
    let idx = p.free.iter().position(|&f| f)?;
    p.free[idx] = false;
    Some(idx)
}
/// Einen (noch keinem Thread zugeordneten) Pool-Slot wieder freigeben (Fehlerpfad).
fn release_user_kstack(idx: usize) {
    KSTACKS.lock().free[idx] = true;
}
/// Den Pool-Slot `idx` dem Thread-Slot `thread_slot` zuordnen (nach erfolgreichem spawn).
fn record_user_kstack(thread_slot: usize, idx: usize) {
    KSTACKS.lock().slot_of[thread_slot] = idx as u16;
}
/// Beim Thread-Ende: den ggf. zugeordneten Kernel-Stack-Pool-Slot zurückgeben.
/// No-Op für EL1-Threads (kein Pool-Slot). Stets allein gesperrt.
fn reclaim_user_kstack(thread_slot: usize) {
    let mut p = KSTACKS.lock();
    let i = p.slot_of[thread_slot];
    if i != KSTACK_NONE {
        p.free[i as usize] = true;
        p.slot_of[thread_slot] = KSTACK_NONE;
    }
}
/// Anzahl aktuell freier Kernel-Stack-Pool-Slots (für den Reclaim-Test).
pub fn user_kstack_free_count() -> usize {
    KSTACKS.lock().free.iter().filter(|&&f| f).count()
}
/// Sticky: wurde jemals ein Syscall von EL0 (User-Thread) gesehen?
static EL0_SYSCALL_SEEN: AtomicBool = AtomicBool::new(false);
/// Zähler: wie oft hat der Kernel einen EL0-Fault abgefangen und den fehlerhaften
/// User-Thread isoliert (statt selbst anzuhalten)?
static EL0_FAULTS: AtomicUsize = AtomicUsize::new(0);
/// Zähler: wie oft faultete ein Thread, der in einer **isolierten VSpace** lief?
/// (Beleg, dass die Hardware-Adressraumtrennung Fremd-/unmapped-Zugriffe verhindert.)
static ISO_FAULTS: AtomicUsize = AtomicUsize::new(0);

// --- Lazy-FP-Zustand ---
//
// Pro Kern besitzt höchstens ein Thread die FP/SIMD-Register (der „FP-Owner").
// FP wird beim Kontextwechsel NICHT gesichert; erst ein FP-Trap (EC 0x07) eines
// Nicht-Owners aus EL0 löst Save (alter Owner) + Restore (neuer) aus. EL1 ist
// soft-float und berührt FP nie. Zähler `FP_SWITCHES` belegt, dass tatsächlich
// gewechselt wurde.
const FP_OWNER_NONE: u64 = u64::MAX;
/// Raw-`ThreadId` des FP-Owners je Kern (nur der jeweilige Kern schreibt seinen
/// Eintrag -> Atomics genügen, keine Sperre nötig).
#[allow(clippy::declare_interior_mutable_const)]
static FP_OWNER: [AtomicU64; NUM_CORES] = [const { AtomicU64::new(FP_OWNER_NONE) }; NUM_CORES];
/// Per-Thread-Slot FP-Kontextpuffer. Zugriff ausschließlich unter dem SCHED-Lock
/// (fp_trap hält SCHED; spawn ebenfalls) -> für gleiche Slots serialisiert,
/// verschiedene Slots sind disjunkt. Lock-Ordnung: …->SCHED->FP_STATES (innerste).
static FP_STATES: SpinLock<[FpState; MAX_THREADS]> =
    SpinLock::new([const { FpState::new() }; MAX_THREADS]);
/// Zähler abgeschlossener Lazy-FP-Owner-Wechsel (Save+Restore), für den Test.
static FP_SWITCHES: AtomicUsize = AtomicUsize::new(0);
/// Summe der per `reap` an den Allokator zurückgegebenen Stack-Bytes (monoton).
static REAPED_BYTES: AtomicU64 = AtomicU64::new(0);

// --- Per-Prozess-VSpaces (Weg C, Hybrid) ---
//
// Vertrauenswürdige PDs laufen in der globalen SAS-Map (TTBR0 = global_root, ASID 0);
// **isolierte** PDs in einer eigenen VSpace (eigene Wurzel + ASID), die nur den
// Kernel (EL1-only) + ihre eigene User-Region (EL0) mappt. Der Kontextwechsel setzt
// TTBR0 passend zum einlaufenden Thread.
/// Gepackter `TTBR0`-Wert (asid<<48 | root) je globalem Thread-Slot; `0` = globale
/// SAS-Map. Beim Spawn einer isolierten PD gesetzt.
#[allow(clippy::declare_interior_mutable_const)]
static VSPACE_OF: [AtomicU64; MAX_THREADS] = [const { AtomicU64::new(0) }; MAX_THREADS];
/// Aktuell aktive VSpace (gepacktes TTBR0) je Kern; `0` = noch unbekannt. Vermeidet
/// einen TTBR0-Write, wenn die VSpace gleich bleibt (der häufige All-Trusted-Fall).
#[allow(clippy::declare_interior_mutable_const)]
static CURRENT_VSPACE: [AtomicU64; NUM_CORES] = [const { AtomicU64::new(0) }; NUM_CORES];

/// **Per-Kern** Scheduler-Instanzen, jede hinter eigenem Lock. Der heiße
/// Timer-/IPI-Reschedule-Pfad sperrt nur `SCHEDS[core]` des eigenen Kerns -> echte
/// parallele Einplanung ohne globalen Lock. Kern-übergreifend (z. B. `wake_remote`)
/// sperrt der Kernel die **Ziel**instanz und schickt einen Reschedule-IPI.
#[allow(clippy::declare_interior_mutable_const)]
static SCHEDS: [SpinLock<Scheduler>; NUM_CORES] =
    [const { SpinLock::new(Scheduler::new()) }; NUM_CORES];

// --- Feinkörnige Ressourcen-Locks (ersetzen den einen RES-Lock) ---
//
// Sperrordnung (außen->innen): CAPS < {EPS[i], NTFNS[i], MEM} < SCHEDS[*] < FP_STATES.
// Der Reschedule-Pfad nimmt nur SCHEDS[core] (+ atomares FP_OWNER), wartet also nie
// auf einen anderen Lock. IPC auf verschiedenen Endpoints läuft parallel (je eigener
// EPS[i]-Lock); nur die kurze Cap-Auflösung serialisiert auf CAPS.
/// Cap-Auflösung + -Verwaltung (CapSpace + PD-Tabelle) hinter einem Lock.
static CAPS: SpinLock<Caps> = SpinLock::new(Caps::new());
/// Physischer Allokator (Thread-Stacks etc.) — getrennt, blockiert IPC nicht.
static MEM: SpinLock<PhysAllocator> = SpinLock::new(PhysAllocator::new());
/// **Per-Endpoint** Locks: IPC auf verschiedenen Endpoints ist nebenläufig.
#[allow(clippy::declare_interior_mutable_const)]
static EPS: [SpinLock<Endpoint>; NENDPOINTS] =
    [const { SpinLock::new(Endpoint::EMPTY) }; NENDPOINTS];
/// **Per-Notification** Locks.
#[allow(clippy::declare_interior_mutable_const)]
static NTFNS: [SpinLock<Notification>; NNOTIFICATIONS] =
    [const { SpinLock::new(Notification::EMPTY) }; NNOTIFICATIONS];

// --- Trap-Hooks ---

fn reschedule(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    // Heißer Pfad: nur der Scheduler-Lock DIESES Kerns (parallel zu anderen Kernen).
    // Echter Zeitscheiben-Tick -> MCS-Budget des laufenden Threads belasten.
    let mut sched = SCHEDS[core].lock();
    let next = sched.on_tick(core, frame as usize, true);
    sync_fp_trap(core, &sched);
    sync_vspace(core, &sched);
    next as *mut TrapFrame
}

fn syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    if hal::exception::frame_from_el0(frame as usize) {
        EL0_SYSCALL_SEEN.store(true, Ordering::Relaxed);
    }
    // Der Dispatch besorgt das Locking selbst (feinkörnig: CAPS für die kurze
    // Cap-Auflösung, dann per-Objekt-Lock; SchedOps je Op genau eine SCHEDS-Instanz).
    // Sperrordnung CAPS < EPS[i]/NTFNS[i] < SCHEDS -> deadlockfrei. IRQs im Trap
    // maskiert -> kein Preempt beim Lock-Halten.
    let mut ops = KernelSched;
    let next = sel4lake_microkit::dispatch(frame as usize, core, &mut ops, &CAPS, &EPS, &NTFNS);
    // Der Syscall kann den laufenden Thread gewechselt haben (block/exit) -> FP-Trap
    // + VSpace passend zum neuen aktuellen Thread setzen.
    {
        let sched = SCHEDS[core].lock();
        sync_fp_trap(core, &sched);
        sync_vspace(core, &sched);
    }
    next as *mut TrapFrame
}

/// Facade über alle per-Kern-Scheduler-Instanzen für den IPC-/Dispatch-Pfad. Jede
/// Operation sperrt **genau eine** `SCHEDS`-Instanz (die des angegebenen Kerns bzw.
/// des Ziel-Threads) und gibt sie sofort wieder frei — nie zwei gleichzeitig. Beim
/// kern-übergreifenden Wecken (`unblock` auf einen Thread eines anderen Kerns)
/// schickt sie zusätzlich einen Reschedule-IPI an den Zielkern.
struct KernelSched;

impl SchedOps for KernelSched {
    fn current_id(&mut self, core: usize) -> ThreadId {
        SCHEDS[core].lock().current_id(core)
    }
    fn frame_of(&mut self, tid: ThreadId) -> Option<usize> {
        SCHEDS[tid.core()].lock().frame_of(tid)
    }
    fn block_current(&mut self, core: usize, frame: usize) -> usize {
        SCHEDS[core].lock().block_current(core, frame)
    }
    fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize {
        SCHEDS[core].lock().switch_to(core, frame, target)
    }
    fn unblock(&mut self, tid: ThreadId) {
        let target = tid.core();
        SCHEDS[target].lock().unblock(tid);
        if target != hal::cpu::core_id() {
            hal::gic::send_sgi(target, hal::gic::IPI_RESCHED_INTID);
        }
    }
    fn on_tick(&mut self, core: usize, frame: usize) -> usize {
        // YIELD ist freiwillig -> verbraucht kein MCS-Budget (tick = false).
        SCHEDS[core].lock().on_tick(core, frame, false)
    }
    fn exit_current(&mut self, core: usize, frame: usize) -> usize {
        // Tid des sich beendenden Threads vor dem Wechsel merken; IRQs sind im Trap
        // maskiert -> der aktuelle Thread ist über die kurzen Sperren stabil.
        let tid = SCHEDS[core].lock().current_id(core);
        let next = SCHEDS[core].lock().exit_current(core, frame);
        purge_ipc_queues(tid); // eager: aus allen IPC-Queues entfernen (keine SCHEDS gehalten)
        reclaim_user_kstack(tid.slot()); // EL0-Kernel-Stack-Pool-Slot zurückgeben (KSTACKS allein)
        next
    }
    fn kill(&mut self, tid: ThreadId, core: usize) -> bool {
        let ok = SCHEDS[core].lock().kill(tid, core);
        if ok {
            purge_ipc_queues(tid); // eager: tote IPC-Queue-Einträge vermeiden
            reclaim_user_kstack(tid.slot()); // getöteter EL0-Thread: Pool-Slot zurück
        }
        ok
    }
    fn map_frame(&mut self, caller: ThreadId, base: u64, len: u64, perm_code: u8) -> bool {
        // Nur isolierte PDs (ASID != 0). Granularität nach Frame-Größe (2-MiB-Block
        // oder 4-KiB-Seiten), Recht nach `perm_code` (0=Ro, 1=Rw, 2=Rx).
        let asid = (VSPACE_OF[caller.slot()].load(Ordering::Relaxed) >> 48) as u16;
        if asid == 0 || len == 0 || len % 4096 != 0 {
            return false;
        }
        let perm = match perm_code {
            2 => hal::mmu::UserPerm::Rx,
            1 => hal::mmu::UserPerm::Rw,
            _ => hal::mmu::UserPerm::Ro,
        };
        vspace_map(asid, base, len, perm)
    }
    fn unmap_frame(&mut self, caller: ThreadId, base: u64, len: u64) -> bool {
        let asid = (VSPACE_OF[caller.slot()].load(Ordering::Relaxed) >> 48) as u16;
        if asid == 0 || len == 0 || len % 4096 != 0 {
            return false;
        }
        vspace_unmap(asid, base, len)
    }
}

/// `CPACR_EL1.FPEN` für den **gerade aktuellen** Thread auf `core` setzen: FP an
/// EL0 trappen, falls dieser Thread NICHT der FP-Owner des Kerns ist (so trappt
/// sein erster FP-Zugriff und löst den Lazy-Owner-Wechsel aus); sonst FP freigeben.
/// Am Ende jedes Hooks aufzurufen, der den laufenden Thread gewechselt haben kann.
fn sync_fp_trap(core: usize, sched: &Scheduler) {
    let cur = sched.current_id(core).to_raw();
    let owner = FP_OWNER[core].load(Ordering::Relaxed);
    hal::fp::set_el0_trap(cur != owner);
}

/// `TTBR0` (Adressraum) für den **gerade aktuellen** Thread auf `core` setzen:
/// dessen isolierte VSpace, oder die globale SAS-Map (trusted). Schreibt TTBR0 nur,
/// wenn sich die VSpace ändert (vermeidet `isb` im häufigen All-Trusted-Fall). Am
/// Ende jedes Hooks aufzurufen, der den laufenden Thread gewechselt haben kann.
fn sync_vspace(core: usize, sched: &Scheduler) {
    let slot = sched.current_id(core).slot();
    let v = VSPACE_OF[slot].load(Ordering::Relaxed);
    let want = if v == 0 { hal::mmu::global_root() } else { v };
    if CURRENT_VSPACE[core].load(Ordering::Relaxed) != want {
        let root = want & ((1u64 << 48) - 1);
        let asid = (want >> 48) as u16;
        hal::mmu::set_user_vspace(root, asid);
        CURRENT_VSPACE[core].store(want, Ordering::Relaxed);
    }
}

/// FP-Trap-Hook (Lazy-FP): Ein EL0-Thread hat FP/SIMD benutzt, ohne Owner zu sein.
/// Alten Owner sichern, FP-Kontext dieses Threads laden, ihn zum Owner machen und
/// FP freigeben. Rückgabe: derselbe Frame — der `eret` wiederholt die getrappte
/// Instruktion, jetzt mit aktivem FP. Hält SCHED (current_id) und FP_STATES.
fn fp_trap(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    let sched = SCHEDS[core].lock();
    let cur_id = sched.current_id(core);
    let cur = cur_id.to_raw();
    let cur_slot = cur_id.slot();
    let owner = FP_OWNER[core].load(Ordering::Relaxed);

    let mut states = FP_STATES.lock();
    if owner != FP_OWNER_NONE && owner != cur {
        // Live-Register gehören dem alten Owner -> in seinen Slot sichern.
        let prev_slot = ThreadId::from_raw(owner).slot();
        hal::fp::save(&mut states[prev_slot]);
        FP_SWITCHES.fetch_add(1, Ordering::Relaxed);
    }
    // FP-Kontext dieses Threads laden (bei Erststart genullt).
    hal::fp::restore(&states[cur_slot]);
    drop(states);

    FP_OWNER[core].store(cur, Ordering::Relaxed);
    hal::fp::set_el0_trap(false); // FP für diesen (jetzt Owner-)Thread freigeben
    frame
}

/// Per-Thread-Kernelzustand für einen frisch belegten Slot zurücksetzen: genullter
/// FP-Kontext, veraltete FP-Owner-Referenzen löschen, und die VSpace auf **global**
/// (SAS) zurücksetzen (eine vorher isolierte PD auf diesem Slot darf nicht
/// nachwirken). Unter gehaltenem SCHED aufzurufen, BEVOR der Thread laufen kann.
fn fp_reset_slot(slot: usize) {
    FP_STATES.lock()[slot] = FpState::new();
    for owner in FP_OWNER.iter() {
        let o = owner.load(Ordering::Relaxed);
        if o != FP_OWNER_NONE && (o & 0xffff_ffff) as usize == slot {
            owner.store(FP_OWNER_NONE, Ordering::Relaxed);
        }
    }
    VSPACE_OF[slot].store(0, Ordering::Relaxed); // Default: globale SAS-Map
}

/// Anzahl abgeschlossener Lazy-FP-Owner-Wechsel (Save eines alten Owners).
pub fn fp_switch_count() -> usize {
    FP_SWITCHES.load(Ordering::Relaxed)
}

/// EL0-Fault-Hook: ein User-Thread hat einen synchronen Fault ausgelöst (Zugriff
/// auf EL1-only Kernel-Speicher, privilegierte Instruktion o. Ä.). Statt den
/// Kernel anzuhalten, wird der fehlerhafte Thread **beendet** und auf den nächsten
/// lauffähigen Thread gewechselt — der Kernel läuft weiter. Das ist der konkrete
/// Nachweis der EL0/EL1-Privileg-Trennung: User-Code kann den Kernel nicht
/// kompromittieren. Nur der SCHED-Lock (wie der Reschedule-Pfad).
fn el0_fault(frame: *mut TrapFrame, esr: u64, far: u64) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    let ec = (esr >> 26) & 0x3f;
    EL0_FAULTS.fetch_add(1, Ordering::Relaxed);
    let (next, tid) = {
        let mut sched = SCHEDS[core].lock();
        let tid = sched.current_id(core);
        if VSPACE_OF[tid.slot()].load(Ordering::Relaxed) != 0 {
            // Der fehlerhafte Thread lief in einer isolierten VSpace -> die
            // Adressraumtrennung hat einen Fremd-/unmapped-Zugriff verhindert.
            ISO_FAULTS.fetch_add(1, Ordering::Release);
        }
        println!(
            "el0-trap: User-Thread {:#x} faultete (EC={ec:#04x} FAR={far:#018x}) -> beendet, Kernel laeuft weiter",
            tid.to_raw()
        );
        let next = sched.exit_current(core, frame as usize);
        // Auf den nächsten Thread gewechselt -> FP-Trap + VSpace passend setzen.
        sync_fp_trap(core, &sched);
        sync_vspace(core, &sched);
        (next, tid)
    }; // SCHEDS freigegeben
    purge_ipc_queues(tid); // eager: faultenden Thread aus allen IPC-Queues entfernen
    reclaim_user_kstack(tid.slot()); // EL0-Kernel-Stack-Pool-Slot zurückgeben (KSTACKS allein)
    // War es eine isolierte PD, ihre VSpace abbauen. Sicher: `sync_vspace` oben hat
    // TTBR0 bereits auf den nächsten Thread umgeschaltet (nicht mehr die tote VSpace).
    let packed = VSPACE_OF[tid.slot()].load(Ordering::Relaxed);
    if packed != 0 {
        vspace_teardown((packed >> 48) as u16);
        VSPACE_OF[tid.slot()].store(0, Ordering::Relaxed);
    }
    next as *mut TrapFrame
}

/// Wie oft hat der Kernel einen EL0-Fault abgefangen und den Thread isoliert?
pub fn el0_fault_count() -> usize {
    EL0_FAULTS.load(Ordering::Relaxed)
}

/// Hat ein Thread in einer **isolierten VSpace** durch einen Fremd-/unmapped-Zugriff
/// gefaultet? (Hardware-erzwungene Adressraumtrennung, Weg C.)
pub fn iso_faulted() -> bool {
    ISO_FAULTS.load(Ordering::Acquire) > 0
}
/// Anzahl der Faults in isolierten VSpaces (Fremdzugriff bzw. unmapped Frame).
pub fn iso_fault_count() -> usize {
    ISO_FAULTS.load(Ordering::Acquire)
}

/// Reschedule-, Syscall-, EL0-Fault- + Lazy-FP-Hook registrieren (einmalig, vor
/// IRQs/Threads).
pub fn set_hooks() {
    hal::exception::set_reschedule_hook(reschedule);
    hal::exception::set_syscall_hook(syscall);
    hal::exception::set_fault_hook(el0_fault);
    hal::exception::set_fp_hook(fp_trap);
}

/// Freies RAM `[free_base, ram_end)` beim Allokator registrieren.
pub fn init_mem(free_base: u64, ram_end: u64) {
    MEM.lock().add_region(free_base, ram_end - free_base);
}

/// **Alle** per-Kern-Scheduler-Instanzen an ihre Kern-ID binden. Vom Bootkern
/// **einmalig vor** dem Erzeugen irgendwelcher Threads aufzurufen — damit der
/// Bootkern Threads auf noch nicht gestartete Kerne einplanen kann
/// ([`spawn_on_core`]), bevor deren `init_core` (Idle-Anlage) dort läuft.
pub fn bind_cores() {
    for (c, s) in SCHEDS.iter().enumerate() {
        s.lock().bind_core(c);
    }
}

/// Boot-Kontext des aufrufenden Kerns als Idle-Thread registrieren (vor IRQs).
pub fn init_core() {
    let core = hal::cpu::core_id();
    SCHEDS[core].lock().init_core(core, IDLE_PRIO);
}

// --- Allokator (MEM) ---

pub fn alloc(size: u64, align: u64) -> Option<MemoryCap> {
    MEM.lock().alloc(size, align)
}
pub fn free(cap: MemoryCap) {
    MEM.lock().free(cap);
}
pub fn total_free() -> u64 {
    MEM.lock().total_free()
}
pub fn fragments() -> usize {
    MEM.lock().fragments()
}

// --- Capability-Space + PDs (CAPS) ---

/// Anzahl belegter Cap-Slots (Fuzzer-/Leak-Oracle).
pub fn cap_used_slots() -> usize {
    CAPS.lock().cspace.used_slots()
}
/// Anzahl belegter Cap-Objekte (Fuzzer-/Leak-Oracle: kein Objekt ohne lebende Cap).
pub fn cap_used_objects() -> usize {
    CAPS.lock().cspace.used_objects()
}
/// **CDT-/Refcount-Property-Oracle** (Fuzzer): `0` bei Konsistenz, sonst Anomalie-Code
/// (s. `CapSpace::audit_cdt`). Sichert: keine verlorenen Objekte, keine negativen/
/// falschen Refcounts, keine toten CDT-Knoten, keine Ableitung auf fremde Objekte,
/// Baumform.
pub fn cap_audit_cdt() -> u32 {
    CAPS.lock().cspace.audit_cdt()
}

pub fn cap_install(cap: MemoryCap) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.install_memory(cap)
}
pub fn cap_copy(src: CapPtr, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.copy(src, rights)
}
pub fn cap_mint(src: CapPtr, rights: Rights, badge: u64) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.mint(src, rights, badge)
}
pub fn cap_move(src: CapPtr) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.move_cap(src)
}
pub fn cap_inspect(ptr: CapPtr) -> Option<CapInfo> {
    CAPS.lock().cspace.inspect(ptr)
}
pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {
    // Finalisierung gibt ggf. Speicher zurück -> CAPS vor MEM (Sperrordnung).
    let mut caps = CAPS.lock();
    let mut mem = MEM.lock();
    caps.cspace.delete(&mut mem, ptr)
}
pub fn cap_revoke(ptr: CapPtr) -> Result<(), CapError> {
    let mut caps = CAPS.lock();
    let mut mem = MEM.lock();
    caps.cspace.revoke(&mut mem, ptr)
}

// --- Endpoints / Notifications (per-Objekt-Locks) ---

/// Einen freien Endpoint-Slot reservieren (scannt die per-Endpoint-Locks).
pub fn create_endpoint() -> Option<usize> {
    for (i, ep) in EPS.iter().enumerate() {
        let mut e = ep.lock();
        if !e.is_used() {
            e.mark_used();
            return Some(i);
        }
    }
    None
}
pub fn install_endpoint_cap(ep: u32, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.install_endpoint(ep, rights)
}
pub fn create_notification() -> Option<usize> {
    for (i, n) in NTFNS.iter().enumerate() {
        let mut nt = n.lock();
        if !nt.is_used() {
            nt.mark_used();
            return Some(i);
        }
    }
    None
}
pub fn install_notification_cap(ntfn: u32, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.install_notification(ntfn, rights)
}

// --- Threads / Protection Domains ---

/// Einen Thread mit Priorität `prio` auf dem aktuellen Kern erzeugen (Stack aus
/// dem Allokator). Lock-Ordnung RES vor SCHEDS[core].
pub fn spawn(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    spawn_on_core(hal::cpu::core_id(), entry, arg, prio)
}

/// Den Kern mit der **geringsten Last** wählen (Anzahl belegter TCB-Slots). Sperrt
/// jede Scheduler-Instanz einzeln/kurz (nie zwei gleichzeitig).
pub fn least_loaded_core() -> usize {
    let mut best = 0usize;
    let mut best_load = usize::MAX;
    for (c, s) in SCHEDS.iter().enumerate() {
        let load = s.lock().load();
        if load < best_load {
            best_load = load;
            best = c;
        }
    }
    best
}

/// **Lastbewusst** einen Thread erzeugen: auf dem aktuell am wenigsten ausgelasteten
/// Kern. Best-effort (die Last kann sich zwischen Auswahl und spawn ändern). Threads
/// bleiben danach kern-gebunden — es gibt keine Laufzeit-Migration (s. ext-10).
pub fn spawn_balanced(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    spawn_on_core(least_loaded_core(), entry, arg, prio)
}

/// Wie [`spawn`], aber auf einem **bestimmten** Kern `core` (z. B. vom Bootkern aus,
/// um Arbeit über die Kerne zu verteilen). Der Zielkern muss vorab via
/// [`bind_cores`] gebunden sein. Lock-Ordnung RES vor SCHEDS[core].
pub fn spawn_on_core(core: usize, entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let (base, len) = {
        let stack = MEM.lock().alloc(STACK_SIZE, 16)?;
        (stack.base() as usize, stack.len() as usize)
    }; // MEM vor SCHEDS freigegeben (Ordnung MEM < SCHEDS)
    let mut sched = SCHEDS[core].lock();
    let tid = sched.spawn(core, entry, arg, base, len, prio)?;
    fp_reset_slot(tid.slot()); // frischer FP-Kontext, bevor der Thread laufen kann
    Some(tid)
}

/// Einen **EL0-User-Thread** erzeugen: Kernel-Stack aus dem EL1-only Pool (per
/// Free-List, beim Thread-Ende zurückgegeben), User-Stack aus dem EL0-zugänglichen
/// RAM. Der Thread läuft auf EL0 und kann nur per Syscall mit dem Kernel interagieren.
pub fn spawn_user(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let kidx = claim_user_kstack()?; // freien Pool-Slot reservieren
    let kbase = core::ptr::addr_of!(__user_kstacks_bottom) as usize + kidx * USER_KSTACK_SIZE;
    let (user_base, user_len) = match MEM.lock().alloc(STACK_SIZE, 16) {
        Some(s) => (s.base() as usize, s.len() as usize),
        None => {
            release_user_kstack(kidx);
            return None;
        }
    }; // MEM vor SCHEDS freigegeben (Ordnung MEM < SCHEDS)
    let tid = {
        let mut sched = SCHEDS[core].lock();
        let r = sched.spawn_user(core, entry, arg, kbase, USER_KSTACK_SIZE, user_base, user_len, prio);
        if let Some(t) = r {
            fp_reset_slot(t.slot()); // frischer FP-Kontext, bevor der Thread laufen kann
        }
        r
    }; // SCHEDS freigegeben
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kidx); // Pool-Slot dem Thread zuordnen
            Some(t)
        }
        None => {
            release_user_kstack(kidx);
            MEM.lock().free_region(PhysRegion::new(user_base as u64, user_len as u64));
            None
        }
    }
}

/// Anzahl gleichzeitig verwaltbarer isolierter VSpaces (= max. ASID).
const MAX_VSPACES: usize = 16;

/// Metadaten einer isolierten VSpace (für map/unmap + Teardown). Indiziert per
/// `asid - 1`.
#[derive(Clone, Copy)]
struct VSpaceEnt {
    used: bool,
    l1: u64,
    l2: u64,
}
static VSPACES: SpinLock<[VSpaceEnt; MAX_VSPACES]> =
    SpinLock::new([VSpaceEnt { used: false, l1: 0, l2: 0 }; MAX_VSPACES]);

/// Eine **leere** isolierte VSpace anlegen (Kernel EL1-only, kein User-Frame). Gibt
/// `(asid, l1_phys)` oder `None` (kein ASID/Speicher). Allokiert L1+L2 aus `MEM`
/// (zuerst, freigegeben), trägt dann die Metadaten ein (`MEM` und `VSPACES` nie
/// gleichzeitig gehalten -> keine Sperrordnungs-Inversion zu Teardown/map).
fn create_vspace() -> Option<(u16, u64)> {
    // ASID/VSpace-Slot aus der **Free-List** (VSPACES) belegen — wiederverwendbar
    // (kein monoton wachsender Zähler -> keine ASID-Leaks). Reservierung unter EINEM
    // Lock (Platzhalter), damit zwei Kerne nicht denselben Slot greifen.
    let asid = {
        let mut t = VSPACES.lock();
        let i = t.iter().position(|v| !v.used)?;
        t[i] = VSpaceEnt { used: true, l1: 0, l2: 0 }; // reserviert
        (i + 1) as u16
    };
    let a = MEM.lock().alloc(4096, 4096);
    let b = MEM.lock().alloc(4096, 4096);
    let (l1, l2) = match (a, b) {
        (Some(l1), Some(l2)) => (l1, l2),
        (a, b) => {
            let mut mem = MEM.lock();
            if let Some(c) = a {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            if let Some(c) = b {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            drop(mem);
            VSPACES.lock()[asid as usize - 1].used = false; // Slot zurückgeben
            return None;
        }
    };
    hal::mmu::vspace_create_base(l1.base(), l2.base()); // läuft in globaler Map
    VSPACES.lock()[asid as usize - 1] = VSpaceEnt {
        used: true,
        l1: l1.base(),
        l2: l2.base(),
    };
    Some((asid, l1.base()))
}

/// Anzahl freier VSpace-/ASID-Slots (für die Leak-Prüfung des Churn-Tests).
pub fn free_vspaces() -> usize {
    VSPACES.lock().iter().filter(|v| !v.used).count()
}

/// Anzahl belegter TCB-Slots auf `core` (für die Leak-Prüfung).
pub fn used_tcbs(core: usize) -> usize {
    SCHEDS[core].lock().load()
}

/// L2-Tabelle einer (gültigen) isolierten VSpace nachschlagen.
fn vspace_l2(asid: u16) -> Option<u64> {
    if asid == 0 || asid as usize > MAX_VSPACES {
        return None;
    }
    let v = VSPACES.lock()[asid as usize - 1];
    if v.used {
        Some(v.l2)
    } else {
        None
    }
}

/// Einen 2-MiB-Frame `phys` in die VSpace `asid` mappen (EL0-RW) + ASID flushen.
fn vspace_map_region(asid: u16, phys: u64) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    if hal::mmu::vspace_map_block(l2, phys) {
        hal::mmu::flush_asid(asid);
        true
    } else {
        false
    }
}

/// 4-KiB-Granularität: Region `[base, base+len)` (identity) mit `perm` in die VSpace
/// `asid` mappen. 2-MiB-ausgerichtete 2-MiB-Regionen mit RW/RX nutzen den
/// Block-Fastpath; sonst seitenweise (L3 wird bei Bedarf aus `MEM` angelegt).
fn vspace_map(asid: u16, base: u64, len: u64, perm: hal::mmu::UserPerm) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    let ok = if len == hal::mmu::ISO_REGION_SIZE
        && base % hal::mmu::ISO_REGION_SIZE == 0
        && perm != hal::mmu::UserPerm::Ro
    {
        match perm {
            hal::mmu::UserPerm::Rx => hal::mmu::vspace_map_code_block(l2, base),
            _ => hal::mmu::vspace_map_block(l2, base),
        }
    } else {
        let mut all = true;
        let mut p = base;
        while p < base + len {
            let mapped =
                hal::mmu::vspace_map_page(l2, p, perm, &mut || MEM.lock().alloc(4096, 4096).map(|c| c.base()));
            if !mapped {
                all = false;
                break;
            }
            p += 4096;
        }
        all
    };
    if ok {
        hal::mmu::flush_asid(asid);
    }
    ok
}

/// 4-KiB-Granularität: Region `[base, base+len)` aus der VSpace `asid` entfernen.
fn vspace_unmap(asid: u16, base: u64, len: u64) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    let ok = if len == hal::mmu::ISO_REGION_SIZE && base % hal::mmu::ISO_REGION_SIZE == 0 {
        hal::mmu::vspace_unmap_block(l2, base)
    } else {
        let mut all = true;
        let mut p = base;
        while p < base + len {
            if !hal::mmu::vspace_unmap_page(l2, p) {
                all = false;
            }
            p += 4096;
        }
        all
    };
    if ok {
        hal::mmu::flush_asid(asid);
    }
    ok
}

/// Kernel-Setup: Region `[base, base+len)` mit `perm_code` (0=Ro,1=Rw,2=Rx) in die
/// VSpace des Threads `tid` mappen (für Demos, die vorab feingranulare Seiten — z. B.
/// RW/RO/Guard — anlegen wollen). No-Op für nicht-isolierte Threads.
pub fn map_into_thread(tid: ThreadId, base: u64, len: u64, perm_code: u8) -> bool {
    let asid = (VSPACE_OF[tid.slot()].load(Ordering::Relaxed) >> 48) as u16;
    if asid == 0 {
        return false;
    }
    let perm = match perm_code {
        2 => hal::mmu::UserPerm::Rx,
        1 => hal::mmu::UserPerm::Rw,
        _ => hal::mmu::UserPerm::Ro,
    };
    vspace_map(asid, base, len, perm)
}

/// Kernel-Setup-Gegenstück zu [`map_into_thread`]: `[base, base+len)` aus der VSpace
/// des Threads `tid` wieder entfernen (Seiten auf EL1-only, TLB-Flush). Für den
/// Fuzzer/Tests, um den per-Seite-Unmap-Pfad explizit zu fahren. No-Op (false) für
/// nicht-isolierte Threads.
pub fn unmap_into_thread(tid: ThreadId, base: u64, len: u64) -> bool {
    let asid = (VSPACE_OF[tid.slot()].load(Ordering::Relaxed) >> 48) as u16;
    if asid == 0 {
        return false;
    }
    vspace_unmap(asid, base, len)
}

/// Eine isolierte VSpace abbauen: alle per-PD-L3-Tabellen **und** L1+L2 an `MEM`
/// zurückgeben, Eintrag freigeben, TLB der ASID flushen. Beim Thread-Ende einer
/// isolierten PD aufzurufen.
fn vspace_teardown(asid: u16) {
    if asid == 0 || asid as usize > MAX_VSPACES {
        return;
    }
    let ent = {
        let mut t = VSPACES.lock();
        let e = t[asid as usize - 1];
        t[asid as usize - 1] = VSpaceEnt { used: false, l1: 0, l2: 0 };
        e
    };
    if ent.used {
        let mut mem = MEM.lock();
        // Zuerst die feingranularen L3-Tabellen (aus Seiten-Mappings) einsammeln.
        hal::mmu::vspace_collect_l3s(ent.l2, &mut |p| {
            mem.free_region(PhysRegion::new(p, 4096));
        });
        mem.free_region(PhysRegion::new(ent.l1, 4096));
        mem.free_region(PhysRegion::new(ent.l2, 4096));
        drop(mem);
        hal::mmu::flush_asid(asid);
    }
}

/// Einen **isolierten EL0-User-Thread** erzeugen (Weg C): eigene VSpace, die nur den
/// Kernel (EL1-only) + eine **private 2-MiB-Stack-Region** (EL0-RW) mappt. Sein Code
/// ist die geteilte `.user_text` (EL0-RX in jeder VSpace). Greift er auf **fremdes**
/// User-RAM zu, ist das in seiner VSpace EL1-only -> Fault -> Kernel beendet ihn.
/// Zur Laufzeit kann er weitere Frames per `MAP`-Syscall (cap-gated) hinzunehmen.
/// Gibt `(ThreadId, region_base)`. VSpace-Tabellen werden beim Thread-Ende abgebaut.
pub fn spawn_isolated(entry: usize, arg: usize, prio: u8) -> Option<(ThreadId, u64)> {
    let core = hal::cpu::core_id();
    let kidx = claim_user_kstack()?;
    let kbase = core::ptr::addr_of!(__user_kstacks_bottom) as usize + kidx * USER_KSTACK_SIZE;

    // Private 2-MiB-Stack-Region (2-MiB-ausgerichtet, in GiB 1).
    let region_sz = hal::mmu::ISO_REGION_SIZE;
    let (rbase, rlen) = match MEM.lock().alloc(region_sz, region_sz) {
        Some(r) => (r.base(), r.len()),
        None => {
            release_user_kstack(kidx);
            return None;
        }
    };
    if rbase < hal::mmu::USER_RAM_MIN || rbase + rlen > hal::mmu::GIB1_END {
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kidx);
        return None;
    }
    let Some((asid, l1)) = create_vspace() else {
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kidx);
        return None;
    };
    // Stack-Region in die neue VSpace mappen (EL0-RW).
    vspace_map_region(asid, rbase);
    let packed = ((asid as u64) << 48) | l1;

    // Thread: Kernel-Stack aus dem Pool, User-Stack = die gemappte Region.
    let (user_base, user_len) = (rbase as usize, rlen as usize);
    let tid = {
        let mut sched = SCHEDS[core].lock();
        let r = sched.spawn_user(core, entry, arg, kbase, USER_KSTACK_SIZE, user_base, user_len, prio);
        if let Some(t) = r {
            fp_reset_slot(t.slot()); // FP + VSPACE_OF[slot]=0 (global) zurücksetzen
        }
        r
    };
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kidx); // Pool-Slot dem Thread zuordnen (reclaim!)
            VSPACE_OF[t.slot()].store(packed, Ordering::Relaxed); // ab jetzt isoliert
            Some((t, rbase))
        }
        None => {
            vspace_teardown(asid);
            MEM.lock().free_region(PhysRegion::new(rbase, rlen));
            release_user_kstack(kidx);
            None
        }
    }
}

/// Eine isolierte PD **vollständig abbauen** (für den Churn-/Leak-Test bzw. den
/// EXIT einer isolierten PD): den (nicht laufenden) Thread `tid` beenden + seinen
/// Stack einsammeln (an `MEM`), die VSpace abbauen (L1/L2/L3 an `MEM`, ASID-Slot
/// zurück), den Kernel-Stack-Pool-Slot zurückgeben und `VSPACE_OF` leeren. Gibt
/// damit **alle** PD-Ressourcen frei. `tid` muss auf dem aktuellen Kern liegen und
/// darf nicht der laufende Thread sein.
pub fn destroy_isolated(tid: ThreadId) {
    let core = hal::cpu::core_id();
    let asid = (VSPACE_OF[tid.slot()].load(Ordering::Relaxed) >> 48) as u16;
    SCHEDS[core].lock().kill(tid, core); // ready -> Zombie (TCB-Slot frei, Stack vorgemerkt)
    purge_ipc_queues(tid); // eager: tote IPC-Queue-Einträge vermeiden
    reap(); // Stack-Zombie an MEM zurueck (korrekte Lock-Ordnung: SCHEDS frei, dann MEM)
    if asid != 0 {
        vspace_teardown(asid); // L1/L2/L3 an MEM, ASID-Slot frei, TLB-Flush
        VSPACE_OF[tid.slot()].store(0, Ordering::Relaxed);
    }
    reclaim_user_kstack(tid.slot());
}

/// Einen 2-MiB-Frame `phys` als **EL0-RX-Code** in die VSpace `asid` mappen + flushen.
fn vspace_map_code_region(asid: u16, phys: u64) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    if hal::mmu::vspace_map_code_block(l2, phys) {
        hal::mmu::flush_asid(asid);
        true
    } else {
        false
    }
}

/// Eine isolierte PD mit **privat geladenem nativem Code** erzeugen: der Code
/// `[code, code+code_len)` wird in einen frischen Frame kopiert, der **EL0-RX** in
/// die eigene VSpace gemappt wird (W^X; nicht die geteilte `.user_text`). Dazu ein
/// privater EL0-RW-Stack. Der Thread startet am Anfang des Code-Frames. Beweist, dass
/// eine isolierte PD beliebigen, nicht-geteilten Code in ihrer eigenen VSpace
/// ausführt. Gibt die `ThreadId`.
pub fn spawn_isolated_native(code: *const u8, code_len: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let kidx = claim_user_kstack()?;
    let kbase = core::ptr::addr_of!(__user_kstacks_bottom) as usize + kidx * USER_KSTACK_SIZE;
    let sz = hal::mmu::ISO_REGION_SIZE;

    // Code-Frame + Stack-Frame (beide 2-MiB-ausgerichtet, in GiB 1).
    let in_gib1 = |b: u64, l: u64| b >= hal::mmu::USER_RAM_MIN && b + l <= hal::mmu::GIB1_END;
    let ca = MEM.lock().alloc(sz, sz);
    let sa = MEM.lock().alloc(sz, sz);
    let (cf, sf) = match (ca, sa) {
        (Some(cf), Some(sf)) => (cf, sf),
        (ca, sa) => {
            let mut mem = MEM.lock();
            if let Some(c) = ca {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            if let Some(c) = sa {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            drop(mem);
            release_user_kstack(kidx);
            return None;
        }
    };
    let (cbase, clen) = (cf.base(), cf.len());
    let (sbase, slen) = (sf.base(), sf.len());
    if !in_gib1(cbase, clen) || !in_gib1(sbase, slen) || code_len > clen as usize {
        let mut mem = MEM.lock();
        mem.free_region(PhysRegion::new(cbase, clen));
        mem.free_region(PhysRegion::new(sbase, slen));
        drop(mem);
        release_user_kstack(kidx);
        return None;
    }

    // Code in den Frame kopieren (globale Map: cbase ist EL0+EL1-RW -> beschreibbar),
    // dann I-Cache kohärent machen, bevor er ausgeführt wird.
    // SAFETY: `cbase` ist ein frisch allozierter, identity-gemappter RW-Frame; wir
    // kopieren genau `code_len` (<= clen) Bytes aus dem gültigen Quellpuffer.
    unsafe {
        core::ptr::copy_nonoverlapping(code, cbase as *mut u8, code_len);
    }
    hal::cpu::sync_code_range(cbase as usize, code_len);

    let Some((asid, l1)) = create_vspace() else {
        let mut mem = MEM.lock();
        mem.free_region(PhysRegion::new(cbase, clen));
        mem.free_region(PhysRegion::new(sbase, slen));
        drop(mem);
        release_user_kstack(kidx);
        return None;
    };
    vspace_map_code_region(asid, cbase); // Code: EL0-RX (W^X)
    vspace_map_region(asid, sbase); //       Stack: EL0-RW
    let packed = ((asid as u64) << 48) | l1;

    let tid = {
        let mut sched = SCHEDS[core].lock();
        // Entry = Anfang des privaten Code-Frames; User-Stack = privater Stack-Frame.
        let r = sched.spawn_user(
            core,
            cbase as usize,
            0,
            kbase,
            USER_KSTACK_SIZE,
            sbase as usize,
            slen as usize,
            prio,
        );
        if let Some(t) = r {
            fp_reset_slot(t.slot());
        }
        r
    };
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kidx); // Pool-Slot dem Thread zuordnen (reclaim!)
            VSPACE_OF[t.slot()].store(packed, Ordering::Relaxed);
            Some(t)
        }
        None => {
            vspace_teardown(asid);
            let mut mem = MEM.lock();
            mem.free_region(PhysRegion::new(cbase, clen));
            mem.free_region(PhysRegion::new(sbase, slen));
            drop(mem);
            release_user_kstack(kidx);
            None
        }
    }
}

/// Einen blockierten/geparkten Thread `tid` auf **seinem** (ggf. fremden) Kern
/// aufwecken: die Zielinstanz sperren, ihn bereit machen und — falls es ein
/// anderer Kern ist — einen Reschedule-IPI schicken, damit der Zielkern ihn
/// zeitnah einplant. Das ist der Cross-Core-Aufweck-Primitive.
pub fn wake_remote(tid: ThreadId) {
    let target = tid.core();
    SCHEDS[target].lock().unblock(tid);
    if target != hal::cpu::core_id() {
        hal::gic::send_sgi(target, hal::gic::IPI_RESCHED_INTID);
    }
}

/// Wurde jemals ein Syscall von EL0 (echter User-Thread) ausgeführt?
pub fn el0_syscall_seen() -> bool {
    EL0_SYSCALL_SEEN.load(Ordering::Relaxed)
}

/// Eine Tcb-Capability für einen Thread prägen (cap-kontrolliertes `KILL`).
pub fn install_tcb_cap(tid: ThreadId, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.lock().cspace.install_tcb(tid.to_raw(), rights)
}

// --- MCS Scheduling Contexts (Budget-basiertes Scheduling) ---
//
// CPU-Zeit wird **kapabilitätskontrolliert** vergeben: ein Scheduling-Context-Objekt
// (Budget/Periode) existiert nur als Cap im CapSpace. Erst die Vorlage einer gültigen
// SchedContext-Cap mit WRITE-Recht autorisiert, einem Thread dieses Budget zuzuweisen
// (`bind_sched_context`). Das Budget wird aus dem **Cap-Objekt** gelesen, nicht aus
// einem freien Argument — die Cap *ist* die Autorität (vgl. seL4 `SchedContext_Bind`).

/// Eine **Scheduling-Context-Capability** prägen: `budget` Ticks je `period` Ticks.
pub fn install_sched_context_cap(
    budget: u32,
    period: u32,
    rights: Rights,
) -> Result<CapPtr, CapError> {
    CAPS.lock()
        .cspace
        .install_sched_context(budget, period, rights)
}

/// Einen Scheduling Context an einen Thread **binden**: die Cap `sc` auflösen, prüfen
/// dass sie ein `SchedContext` mit WRITE-Recht ist, und das darin gespeicherte Budget
/// auf dem Scheduler von `core` für `tid` setzen. Ohne gültige Cap keine Budget-
/// Autorität (gibt `false` zurück). Lock-Ordnung: CAPS vor SCHEDS[core].
pub fn bind_sched_context(sc: CapPtr, core: usize, tid: ThreadId) -> bool {
    let (budget, period) = {
        let caps = CAPS.lock();
        match caps.cspace.lookup(sc) {
            Some((ObjectKind::SchedContext { budget, period }, rights, _))
                if rights.contains(Rights::WRITE) =>
            {
                (budget, period)
            }
            _ => return false, // keine (gültige) SchedContext-Cap -> keine Autorität
        }
    }; // CAPS vor SCHEDS freigegeben
    SCHEDS[core].lock().set_budget(tid, budget, period)
}

/// MCS-Telemetrie eines Kerns: `(Budget-Erschöpfungen, Refills)`. Für Tests.
pub fn budget_stats(core: usize) -> (u64, u64) {
    SCHEDS[core].lock().budget_stats()
}

/// Einen **nicht laufenden** Thread des **aktuellen** Kerns direkt beenden — dieselbe
/// Mechanik wie der cap-kontrollierte `KILL`-Syscall (`KernelSched::kill`), nur ohne
/// Cap-Vorlage (für kernelinterne Test-/Wartungspfade). Gibt `true` bei Erfolg. `kill`
/// ist kern-lokal: `tid` muss auf dem aufrufenden Kern liegen und darf nicht laufen.
pub fn kill_local(tid: ThreadId) -> bool {
    let core = hal::cpu::core_id();
    if tid.core() != core {
        return false;
    }
    let ok = SCHEDS[core].lock().kill(tid, core);
    if ok {
        purge_ipc_queues(tid); // eager: tote IPC-Queue-Einträge vermeiden
        reclaim_user_kstack(tid.slot()); // falls EL0-Thread: Pool-Slot zurück
    }
    ok
}

/// Beendete Threads **dieses Kerns** einsammeln: TCB-Slots freigeben und Stacks an
/// den Allokator zurückgeben (aus dem Idle-Thread). Erst die Zombies unter
/// `SCHEDS[core]` einsammeln, dann den Lock **freigeben** und unter `MEM` freigeben
/// — nie SCHEDS und MEM gleichzeitig halten (sonst Ordnungsinversion zu `spawn`).
pub fn reap() -> usize {
    reap_core(hal::cpu::core_id())
}

/// Beendete Threads des Kerns `core` einsammeln — auch **kern-übergreifend** aufrufbar
/// (z. B. der Fuzzer-Controller auf core 0 reapt Zombies belasteter Kerne, deren Idle
/// nie läuft -> sonst lecken die Stacks). Sperrordnung: erst `SCHEDS[core]` (Zombies in
/// einen Puffer), freigeben, dann `MEM` — nie beide gleichzeitig.
pub fn reap_core(core: usize) -> usize {
    let mut zombies: [(usize, usize); 8] = [(0, 0); 8];
    let mut n = 0;
    {
        let mut sched = SCHEDS[core].lock();
        while n < zombies.len() {
            match sched.reap() {
                Some(z) => {
                    zombies[n] = z;
                    n += 1;
                }
                None => break,
            }
        }
    } // SCHEDS freigegeben
    if n > 0 {
        let mut mem = MEM.lock();
        let mut bytes = 0u64;
        for &(base, len) in &zombies[..n] {
            mem.free_region(PhysRegion::new(base as u64, len as u64));
            bytes += len as u64;
        }
        REAPED_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
    n
}

/// Einen **nicht laufenden** Thread auf **irgendeinem** Kern beenden (für den Fuzzer-
/// Controller, der Aktoren auf anderen Kernen zu ungünstigen Zeiten killt). Schlägt
/// fehl (`false`), wenn der Thread gerade auf seinem Kern *läuft* (dann erneut
/// versuchen, sobald er blockiert/verdrängt ist). Entfernt ihn eager aus allen
/// IPC-Queues und weckt den Zielkern (IPI), damit er bald reapt.
pub fn kill_remote(tid: ThreadId) -> bool {
    let c = tid.core();
    if c >= NUM_CORES {
        return false;
    }
    let ok = SCHEDS[c].lock().kill(tid, c);
    if ok {
        purge_ipc_queues(tid);
        reclaim_user_kstack(tid.slot());
        if c != hal::cpu::core_id() {
            hal::gic::send_sgi(c, hal::gic::IPI_RESCHED_INTID);
        }
    }
    ok
}

/// Summe der per [`reap`] an den Allokator zurückgegebenen Stack-Bytes (monoton) —
/// belegt, dass beendete Threads ihren Stack zurückgeben (robust gegen anderweitige
/// Allokationen, anders als ein absoluter `total_free`-Vergleich).
pub fn reaped_bytes() -> u64 {
    REAPED_BYTES.load(Ordering::Relaxed)
}

pub fn create_pd() -> Option<usize> {
    CAPS.lock().pds.create()
}
pub fn bind_pd(pd: usize, tid: ThreadId) {
    CAPS.lock().pds.bind_thread(pd, tid);
}
pub fn install_pd_cap(pd: usize, slot: usize, cap: CapPtr) {
    CAPS.lock().pds.install_cap(pd, slot, cap);
}
pub fn clear_pd_cap(pd: usize, slot: usize) {
    CAPS.lock().pds.clear_cap(pd, slot);
}

/// Einen blockierten Endpoint-Empfänger zurückziehen (Hot-Reload).
pub fn endpoint_retire_receiver(ep: usize, tid: ThreadId) -> bool {
    if ep < NENDPOINTS {
        EPS[ep].lock().retire_receiver(tid)
    } else {
        false
    }
}

/// **Eager-Cleanup beim Thread-Tod:** den (sterbenden) Thread `tid` aus ALLEN
/// Endpoint-Queues (senders/receivers/caller) und Notification-Waitern entfernen.
/// Verhindert tote TCBs in den festen Queues (Corpse-Fill -> verdrängte echte Sender)
/// und ein REPLY/SIGNAL in einen recycelten Frame. Aus den Todespfaden (kill/exit/
/// fault) aufzurufen — OHNE gehaltenen SCHEDS-Lock (Sperrordnung EPS/NTFNS < SCHEDS).
pub fn purge_ipc_queues(tid: ThreadId) {
    // Verwaiste Aufrufer (deren Reply-Owner gerade stirbt) einsammeln und NACH dem
    // Freigeben der EPS-Locks mit ERR_SERVER_GONE entblocken (kein verschachtelter
    // EPS->SCHEDS-Lock). Ein Thread ist Reply-Owner von höchstens wenigen Endpoints.
    let mut orphans: [Option<ThreadId>; 8] = [None; 8];
    let mut no = 0usize;
    for ep in EPS.iter() {
        let mut e = ep.lock();
        if e.is_used() {
            e.purge_thread(tid);
            if let Some(caller) = e.owner_died(tid) {
                if no < orphans.len() {
                    orphans[no] = Some(caller);
                    no += 1;
                }
            }
        }
    }
    for n in NTFNS.iter() {
        let mut nt = n.lock();
        if nt.is_used() {
            nt.purge_thread(tid);
        }
    }
    for caller in orphans.iter().flatten() {
        unblock_with_error(*caller, sel4lake_abi::result::ERR_SERVER_GONE);
    }
}

/// Einen blockierten Thread mit einem **Fehlercode** in `x0` (statt `OK`) entblocken —
/// für die Reply-Liveness: ein `CALL`-Aufrufer, dessen Server (Reply-Owner) verschwand,
/// wird so entblockt und sieht den Fehler, statt dauerhaft zu hängen. Sperrt kurz die
/// Zielinstanz; bei fremdem Kern Reschedule-IPI.
fn unblock_with_error(caller: ThreadId, code: u64) {
    let c = caller.core();
    {
        let mut sched = SCHEDS[c].lock();
        if let Some(frame) = sched.frame_of(caller) {
            hal::exception::frame_set_reg(frame, sel4lake_abi::reg::SYSNO_RESULT, code);
        }
        sched.unblock(caller);
    } // SCHEDS freigegeben
    if c != hal::cpu::core_id() {
        hal::gic::send_sgi(c, hal::gic::IPI_RESCHED_INTID);
    }
}

/// Lebt `tid`? (Für IPC-Audits.) Sperrt die Zielinstanz kurz.
pub fn thread_alive(tid: ThreadId) -> bool {
    SCHEDS[tid.core()].lock().is_alive(tid)
}

/// **IPC-Konsistenz-Oracle** (Fuzzer): jedes belegte Endpoint/Notification + jeden
/// Kern-Scheduler auf strukturelle Invarianten prüfen. Gibt `0` bei Konsistenz, sonst
/// einen Anomalie-Code: 1=toter TCB in Endpoint-Queue/caller, 2=Duplikat in Endpoint-
/// Queue, 3=toter Waiter in Notification, 10+n=Scheduler-Audit-Code n (s. `Scheduler::
/// audit`). Sperrt je Objekt einzeln; die Liveness-Prüfung verschachtelt EPS/NTFNS ->
/// SCHEDS (zulässige Ordnung), nie zwei Objekte gleichzeitig.
pub fn ipc_audit() -> u32 {
    let live = &mut |t: ThreadId| -> bool { SCHEDS[t.core()].lock().is_alive(t) };
    for ep in EPS.iter() {
        let e = ep.lock();
        if e.is_used() {
            let (dead, dup) = e.audit(live);
            if dead {
                return 1;
            }
            if dup {
                return 2;
            }
        }
    }
    for n in NTFNS.iter() {
        let nt = n.lock();
        if nt.is_used() && nt.audit(live) {
            return 3;
        }
    }
    for c in 0..NUM_CORES {
        let code = SCHEDS[c].lock().audit();
        if code != 0 {
            return 10 + code;
        }
    }
    // CDT-/Refcount-Property (Cap-Churn-Events des IPC-Fuzzers laufen während IPC).
    let cdt = CAPS.lock().cspace.audit_cdt();
    if cdt != 0 {
        return 20 + cdt;
    }
    0
}
