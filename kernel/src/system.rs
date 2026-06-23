//! Kernzustand hinter **zwei** Locks: dem Scheduler-Lock (`SCHED`) und dem
//! Ressourcen-Lock (`RES` = Allokator, Capability-Space, Endpoints,
//! Notifications, Protection Domains) — plus die Trap-Hooks.
//!
//! Trennung der Granularität (Per-Kern-Locks, Teil 1): Der heiße
//! Timer-Reschedule-Pfad sperrt **nur** `SCHED` und blockiert damit keine reinen
//! Ressourcen-Operationen (Cap-/Speicher-/PD-Verwaltung) auf anderen Kernen.
//! Operationen, die beides brauchen (IPC, spawn, reap), sperren in **fester
//! Reihenfolge `RES` vor `SCHED`** — kein Pfad sperrt `SCHED` vor `RES`, daher
//! deadlockfrei. (Echte per-Kern-*parallele* Scheduler-Instanzen sind der nächste
//! Schritt; sie brauchen per-Kern-TCB-Partitionierung + Cross-Core-IPI-Unblock.)

use sel4lake_cap::{CapError, CapInfo, CapPtr, CapSpace};
use sel4lake_hal::{self as hal, exception::TrapFrame};
use sel4lake_ipc::{EndpointTable, NotificationTable};
use sel4lake_mem::{MemoryCap, PhysAllocator, PhysRegion, Rights};
use sel4lake_microkit::PdTable;
use sel4lake_sched::{Scheduler, ThreadId};
use sel4lake_sync::SpinLock;

const STACK_SIZE: u64 = 64 * 1024;
/// Standardpriorität für Idle und gewöhnliche Demo-Threads (Round-Robin).
pub const IDLE_PRIO: u8 = 1;

/// Geteilte Ressourcen (alles außer dem Scheduler).
struct Resources {
    phys: PhysAllocator,
    cspace: CapSpace,
    eps: EndpointTable,
    ntfns: NotificationTable,
    pds: PdTable,
}

/// Scheduler-Lock (heißer Pfad; getrennt von den Ressourcen).
static SCHED: SpinLock<Scheduler> = SpinLock::new(Scheduler::new());
/// Ressourcen-Lock. **Lock-Ordnung: RES vor SCHED.**
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
    // Heißer Pfad: nur der Scheduler-Lock.
    SCHED.lock().on_tick(core, frame as usize) as *mut TrapFrame
}

fn syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    // Lock-Ordnung RES vor SCHED.
    let mut res = RES.lock();
    let mut sched = SCHED.lock();
    let r = &mut *res;
    sel4lake_microkit::dispatch(
        frame as usize,
        core,
        &mut sched,
        &mut r.cspace,
        &mut r.eps,
        &mut r.ntfns,
        &mut r.pds,
    ) as *mut TrapFrame
}

/// Reschedule- + Syscall-Hook registrieren (einmalig, vor IRQs/Threads).
pub fn set_hooks() {
    hal::exception::set_reschedule_hook(reschedule);
    hal::exception::set_syscall_hook(syscall);
}

/// Freies RAM `[free_base, ram_end)` beim Allokator registrieren.
pub fn init_mem(free_base: u64, ram_end: u64) {
    RES.lock().phys.add_region(free_base, ram_end - free_base);
}

/// Boot-Kontext des aufrufenden Kerns als Idle-Thread registrieren (vor IRQs).
pub fn init_core() {
    let core = hal::cpu::core_id();
    SCHED.lock().init_core(core, IDLE_PRIO);
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
/// dem Allokator). Lock-Ordnung RES vor SCHED.
pub fn spawn(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let mut res = RES.lock();
    let (base, len) = {
        let stack = res.phys.alloc(STACK_SIZE, 16)?;
        (stack.base() as usize, stack.len() as usize)
    };
    SCHED.lock().spawn(core, entry, arg, base, len, prio)
}

/// Eine Tcb-Capability für einen Thread prägen (cap-kontrolliertes `KILL`).
pub fn install_tcb_cap(tid: ThreadId, rights: Rights) -> Result<CapPtr, CapError> {
    RES.lock().cspace.install_tcb(tid.to_raw(), rights)
}

/// Beendete Threads einsammeln: TCB-Slots freigeben und Stacks an den Allokator
/// zurückgeben (aus dem Idle-Thread). Lock-Ordnung RES vor SCHED.
pub fn reap() -> usize {
    let mut res = RES.lock();
    let mut sched = SCHED.lock();
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
