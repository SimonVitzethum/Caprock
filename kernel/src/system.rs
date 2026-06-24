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

use sel4lake_cap::{CapError, CapInfo, CapPtr, CapSpace};
use sel4lake_hal::{self as hal, exception::TrapFrame, fp::FpState, println};
use sel4lake_ipc::{EndpointTable, NotificationTable};
use sel4lake_mem::{MemoryCap, PhysAllocator, PhysRegion, Rights};
use sel4lake_microkit::PdTable;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use sel4lake_sched::{SchedOps, Scheduler, ThreadId, MAX_THREADS};
use sel4lake_sync::SpinLock;

/// Anzahl Kerne (für die per-Kern FP-Owner-Tabelle). Muss ≥ der realen Kernzahl sein.
const NUM_CORES: usize = 8;

const STACK_SIZE: u64 = 64 * 1024;
/// Standardpriorität für Idle und gewöhnliche Demo-Threads (Round-Robin).
pub const IDLE_PRIO: u8 = 1;

// EL0-User-Threads: Kernel-Stacks aus einem EL1-only Pool (Linker), User-Stacks
// aus dem EL0-zugänglichen RAM.
extern "C" {
    static __user_kstacks_bottom: u8;
}
const USER_KSTACK_SIZE: usize = 0x4000; // 16 KiB (muss zur Linker-Reservierung passen)
const USER_KSTACK_COUNT: usize = 8;
static USER_KSTACK_NEXT: AtomicUsize = AtomicUsize::new(0);
/// Sticky: wurde jemals ein Syscall von EL0 (User-Thread) gesehen?
static EL0_SYSCALL_SEEN: AtomicBool = AtomicBool::new(false);
/// Zähler: wie oft hat der Kernel einen EL0-Fault abgefangen und den fehlerhaften
/// User-Thread isoliert (statt selbst anzuhalten)?
static EL0_FAULTS: AtomicUsize = AtomicUsize::new(0);

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

/// Geteilte Ressourcen (alles außer dem Scheduler).
struct Resources {
    phys: PhysAllocator,
    cspace: CapSpace,
    eps: EndpointTable,
    ntfns: NotificationTable,
    pds: PdTable,
}

/// **Per-Kern** Scheduler-Instanzen, jede hinter eigenem Lock. Der heiße
/// Timer-/IPI-Reschedule-Pfad sperrt nur `SCHEDS[core]` des eigenen Kerns -> echte
/// parallele Einplanung ohne globalen Lock. Kern-übergreifend (z. B. `wake_remote`)
/// sperrt der Kernel die **Ziel**instanz und schickt einen Reschedule-IPI.
#[allow(clippy::declare_interior_mutable_const)]
static SCHEDS: [SpinLock<Scheduler>; NUM_CORES] =
    [const { SpinLock::new(Scheduler::new()) }; NUM_CORES];
/// Ressourcen-Lock. **Lock-Ordnung: RES vor SCHEDS[*].**
static RES: SpinLock<Resources> = SpinLock::new(Resources {
    phys: PhysAllocator::new(),
    cspace: CapSpace::new(),
    eps: EndpointTable::new(),
    ntfns: NotificationTable::new(),
    pds: PdTable::new(),
});

// --- Trap-Hooks ---

fn reschedule(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    // Heißer Pfad: nur der Scheduler-Lock DIESES Kerns (parallel zu anderen Kernen).
    let mut sched = SCHEDS[core].lock();
    let next = sched.on_tick(core, frame as usize);
    sync_fp_trap(core, &sched);
    next as *mut TrapFrame
}

fn syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    if hal::exception::frame_from_el0(frame as usize) {
        EL0_SYSCALL_SEEN.store(true, Ordering::Relaxed);
    }
    // Lock-Ordnung RES vor SCHEDS[*]. Der Dispatch hält `RES` (serialisiert IPC) und
    // greift über die `KernelSched`-Facade auf die Scheduler zu — je Operation
    // **genau eine** `SCHEDS`-Instanz (auch kern-übergreifend für IPC-Partner auf
    // anderen Kernen), nie zwei gleichzeitig -> deadlockfrei. IRQs sind im Trap
    // maskiert, also kann dieser Kern beim Lock-Halten nicht preemptiert werden.
    let mut res = RES.lock();
    let mut ops = KernelSched;
    let r = &mut *res;
    let next = sel4lake_microkit::dispatch(
        frame as usize,
        core,
        &mut ops,
        &mut r.cspace,
        &mut r.eps,
        &mut r.ntfns,
        &mut r.pds,
    );
    // Der Syscall kann den laufenden Thread gewechselt haben (block/exit) -> FP-Trap
    // passend zum neuen aktuellen Thread setzen.
    sync_fp_trap(core, &SCHEDS[core].lock());
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
        SCHEDS[core].lock().on_tick(core, frame)
    }
    fn exit_current(&mut self, core: usize, frame: usize) -> usize {
        SCHEDS[core].lock().exit_current(core, frame)
    }
    fn kill(&mut self, tid: ThreadId, core: usize) -> bool {
        SCHEDS[core].lock().kill(tid, core)
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

/// Genullten FP-Kontext für einen frisch belegten Slot setzen und veraltete
/// Owner-Referenzen auf diesen Slot löschen (Slot-Wiederverwendung nach Thread-Ende).
/// Unter gehaltenem SCHED aufzurufen, BEVOR der Thread laufen kann.
fn fp_reset_slot(slot: usize) {
    FP_STATES.lock()[slot] = FpState::new();
    for owner in FP_OWNER.iter() {
        let o = owner.load(Ordering::Relaxed);
        if o != FP_OWNER_NONE && (o & 0xffff_ffff) as usize == slot {
            owner.store(FP_OWNER_NONE, Ordering::Relaxed);
        }
    }
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
    let mut sched = SCHEDS[core].lock();
    let tid = sched.current_id(core).to_raw();
    println!(
        "el0-trap: User-Thread {tid:#x} faultete (EC={ec:#04x} FAR={far:#018x}) -> beendet, Kernel laeuft weiter"
    );
    let next = sched.exit_current(core, frame as usize);
    // Auf den nächsten Thread gewechselt -> FP-Trap passend setzen.
    sync_fp_trap(core, &sched);
    next as *mut TrapFrame
}

/// Wie oft hat der Kernel einen EL0-Fault abgefangen und den Thread isoliert?
pub fn el0_fault_count() -> usize {
    EL0_FAULTS.load(Ordering::Relaxed)
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
    RES.lock().phys.add_region(free_base, ram_end - free_base);
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

// --- Allokator (RES) ---

pub fn alloc(size: u64, align: u64) -> Option<MemoryCap> {
    RES.lock().phys.alloc(size, align)
}
pub fn free(cap: MemoryCap) {
    RES.lock().phys.free(cap);
}
pub fn total_free() -> u64 {
    RES.lock().phys.total_free()
}
pub fn fragments() -> usize {
    RES.lock().phys.fragments()
}

// --- Capability-Space (RES) ---

pub fn cap_install(cap: MemoryCap) -> Result<CapPtr, CapError> {
    RES.lock().cspace.install_memory(cap)
}
pub fn cap_copy(src: CapPtr, rights: Rights) -> Result<CapPtr, CapError> {
    RES.lock().cspace.copy(src, rights)
}
pub fn cap_mint(src: CapPtr, rights: Rights, badge: u64) -> Result<CapPtr, CapError> {
    RES.lock().cspace.mint(src, rights, badge)
}
pub fn cap_move(src: CapPtr) -> Result<CapPtr, CapError> {
    RES.lock().cspace.move_cap(src)
}
pub fn cap_inspect(ptr: CapPtr) -> Option<CapInfo> {
    RES.lock().cspace.inspect(ptr)
}
pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {
    let mut res = RES.lock();
    let r = &mut *res;
    r.cspace.delete(&mut r.phys, ptr)
}
pub fn cap_revoke(ptr: CapPtr) -> Result<(), CapError> {
    let mut res = RES.lock();
    let r = &mut *res;
    r.cspace.revoke(&mut r.phys, ptr)
}

// --- Endpoints / Notifications (RES) ---

pub fn create_endpoint() -> Option<usize> {
    RES.lock().eps.create()
}
pub fn install_endpoint_cap(ep: u32, rights: Rights) -> Result<CapPtr, CapError> {
    RES.lock().cspace.install_endpoint(ep, rights)
}
pub fn create_notification() -> Option<usize> {
    RES.lock().ntfns.create()
}
pub fn install_notification_cap(ntfn: u32, rights: Rights) -> Result<CapPtr, CapError> {
    RES.lock().cspace.install_notification(ntfn, rights)
}

// --- Threads / Protection Domains ---

/// Einen Thread mit Priorität `prio` auf dem aktuellen Kern erzeugen (Stack aus
/// dem Allokator). Lock-Ordnung RES vor SCHEDS[core].
pub fn spawn(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    spawn_on_core(hal::cpu::core_id(), entry, arg, prio)
}

/// Wie [`spawn`], aber auf einem **bestimmten** Kern `core` (z. B. vom Bootkern aus,
/// um Arbeit über die Kerne zu verteilen). Der Zielkern muss vorab via
/// [`bind_cores`] gebunden sein. Lock-Ordnung RES vor SCHEDS[core].
pub fn spawn_on_core(core: usize, entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let mut res = RES.lock();
    let (base, len) = {
        let stack = res.phys.alloc(STACK_SIZE, 16)?;
        (stack.base() as usize, stack.len() as usize)
    };
    let mut sched = SCHEDS[core].lock();
    let tid = sched.spawn(core, entry, arg, base, len, prio)?;
    fp_reset_slot(tid.slot()); // frischer FP-Kontext, bevor der Thread laufen kann
    Some(tid)
}

/// Einen **EL0-User-Thread** erzeugen: Kernel-Stack aus dem EL1-only Pool,
/// User-Stack aus dem EL0-zugänglichen RAM. Der Thread läuft auf EL0 und kann nur
/// per Syscall mit dem Kernel interagieren. Lock-Ordnung RES vor SCHED.
pub fn spawn_user(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let idx = USER_KSTACK_NEXT.fetch_add(1, Ordering::Relaxed);
    if idx >= USER_KSTACK_COUNT {
        return None;
    }
    let kbase = core::ptr::addr_of!(__user_kstacks_bottom) as usize + idx * USER_KSTACK_SIZE;
    let mut res = RES.lock();
    let (user_base, user_len) = {
        let s = res.phys.alloc(STACK_SIZE, 16)?;
        (s.base() as usize, s.len() as usize)
    };
    let mut sched = SCHEDS[core].lock();
    let tid = sched.spawn_user(core, entry, arg, kbase, USER_KSTACK_SIZE, user_base, user_len, prio)?;
    fp_reset_slot(tid.slot()); // frischer FP-Kontext, bevor der Thread laufen kann
    Some(tid)
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
    RES.lock().cspace.install_tcb(tid.to_raw(), rights)
}

/// Beendete Threads **dieses Kerns** einsammeln: TCB-Slots freigeben und Stacks an
/// den Allokator zurückgeben (aus dem Idle-Thread). Lock-Ordnung RES vor SCHEDS[core].
pub fn reap() -> usize {
    let core = hal::cpu::core_id();
    let mut res = RES.lock();
    let mut sched = SCHEDS[core].lock();
    let mut n = 0;
    while let Some((base, len)) = sched.reap() {
        res.phys.free_region(PhysRegion::new(base as u64, len as u64));
        n += 1;
    }
    n
}

pub fn create_pd() -> Option<usize> {
    RES.lock().pds.create()
}
pub fn bind_pd(pd: usize, tid: ThreadId) {
    RES.lock().pds.bind_thread(pd, tid);
}
pub fn install_pd_cap(pd: usize, slot: usize, cap: CapPtr) {
    RES.lock().pds.install_cap(pd, slot, cap);
}
pub fn clear_pd_cap(pd: usize, slot: usize) {
    RES.lock().pds.clear_cap(pd, slot);
}

/// Einen blockierten Endpoint-Empfänger zurückziehen (Hot-Reload).
pub fn endpoint_retire_receiver(ep: usize, tid: ThreadId) -> bool {
    RES.lock().eps.retire_receiver(ep, tid)
}
