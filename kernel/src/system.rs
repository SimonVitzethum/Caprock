//! Vereinigter Kernzustand hinter **einem** Lock: Allokator, Capability-Space,
//! Scheduler, Endpoints und Protection Domains — plus die Trap-Hooks.
//!
//! Ein einziger Lock vermeidet jedes Lock-Ordering-Problem zwischen dem
//! Timer-Reschedule- und dem Syscall-/IPC-Pfad (beide laufen in Exception-
//! Kontext). Operationen, die mehrere Teilzustände brauchen (z. B. cap-delete:
//! cspace + Allokator, oder IPC: sched + cspace + eps + pds), arbeiten über
//! disjunkte Feld-Borrows desselben gesperrten Zustands.

use sel4lake_cap::{CapError, CapInfo, CapPtr, CapSpace};
use sel4lake_hal::{self as hal, exception::TrapFrame};
use sel4lake_ipc::EndpointTable;
use sel4lake_mem::{MemoryCap, PhysAllocator, Rights};
use sel4lake_microkit::PdTable;
use sel4lake_sched::{Scheduler, ThreadId};
use sel4lake_sync::SpinLock;

const STACK_SIZE: u64 = 64 * 1024;
/// Standardpriorität für Idle und gewöhnliche Demo-Threads (Round-Robin).
pub const IDLE_PRIO: u8 = 1;

struct System {
    phys: PhysAllocator,
    cspace: CapSpace,
    sched: Scheduler,
    eps: EndpointTable,
    pds: PdTable,
}

static SYSTEM: SpinLock<System> = SpinLock::new(System {
    phys: PhysAllocator::new(),
    cspace: CapSpace::new(),
    sched: Scheduler::new(),
    eps: EndpointTable::new(),
    pds: PdTable::new(),
});

// --- Trap-Hooks ---

fn reschedule(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    SYSTEM.lock().sched.on_tick(core, frame as usize) as *mut TrapFrame
}

fn syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    let mut guard = SYSTEM.lock();
    let s = &mut *guard; // disjunkte Feld-Borrows: sched/cspace/eps/pds gleichzeitig
    sel4lake_microkit::dispatch(frame as usize, core, &mut s.sched, &s.cspace, &mut s.eps, &s.pds)
        as *mut TrapFrame
}

/// Reschedule- + Syscall-Hook registrieren (einmalig, vor IRQs/Threads).
pub fn set_hooks() {
    hal::exception::set_reschedule_hook(reschedule);
    hal::exception::set_syscall_hook(syscall);
}

/// Freies RAM `[free_base, ram_end)` beim Allokator registrieren.
pub fn init_mem(free_base: u64, ram_end: u64) {
    SYSTEM.lock().phys.add_region(free_base, ram_end - free_base);
}

/// Boot-Kontext des aufrufenden Kerns als Idle-Thread registrieren (vor IRQs).
/// Der Idle-Thread läuft auf der Standardprincriorität `IDLE_PRIO`.
pub fn init_core() {
    let core = hal::cpu::core_id();
    SYSTEM.lock().sched.init_core(core, IDLE_PRIO);
}

// --- Allokator ---

pub fn alloc(size: u64, align: u64) -> Option<MemoryCap> {
    SYSTEM.lock().phys.alloc(size, align)
}
pub fn free(cap: MemoryCap) {
    SYSTEM.lock().phys.free(cap);
}
pub fn total_free() -> u64 {
    SYSTEM.lock().phys.total_free()
}
pub fn fragments() -> usize {
    SYSTEM.lock().phys.fragments()
}

// --- Capability-Space ---

pub fn cap_install(cap: MemoryCap) -> Result<CapPtr, CapError> {
    SYSTEM.lock().cspace.install_memory(cap)
}
pub fn cap_copy(src: CapPtr, rights: Rights) -> Result<CapPtr, CapError> {
    SYSTEM.lock().cspace.copy(src, rights)
}
pub fn cap_mint(src: CapPtr, rights: Rights, badge: u64) -> Result<CapPtr, CapError> {
    SYSTEM.lock().cspace.mint(src, rights, badge)
}
pub fn cap_move(src: CapPtr) -> Result<CapPtr, CapError> {
    SYSTEM.lock().cspace.move_cap(src)
}
pub fn cap_inspect(ptr: CapPtr) -> Option<CapInfo> {
    SYSTEM.lock().cspace.inspect(ptr)
}
pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {
    let mut guard = SYSTEM.lock();
    let s = &mut *guard;
    s.cspace.delete(&mut s.phys, ptr)
}
pub fn cap_revoke(ptr: CapPtr) -> Result<(), CapError> {
    let mut guard = SYSTEM.lock();
    let s = &mut *guard;
    s.cspace.revoke(&mut s.phys, ptr)
}

// --- Endpoints (Zugriff cap-getragen) ---

pub fn create_endpoint() -> Option<usize> {
    SYSTEM.lock().eps.create()
}
pub fn install_endpoint_cap(ep: u32, rights: Rights) -> Result<CapPtr, CapError> {
    SYSTEM.lock().cspace.install_endpoint(ep, rights)
}

// --- Threads / Protection Domains ---

/// Einen Thread mit Priorität `prio` auf dem aktuellen Kern erzeugen (Stack aus
/// dem Allokator).
pub fn spawn(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let mut guard = SYSTEM.lock();
    let s = &mut *guard;
    let top = {
        let stack = s.phys.alloc(STACK_SIZE, 16)?;
        (stack.base() + stack.len()) as usize
    };
    s.sched.spawn(core, entry, arg, top, prio)
}

pub fn create_pd() -> Option<usize> {
    SYSTEM.lock().pds.create()
}
pub fn bind_pd(pd: usize, tid: ThreadId) {
    SYSTEM.lock().pds.bind_thread(pd, tid);
}
pub fn install_pd_cap(pd: usize, slot: usize, cap: CapPtr) {
    SYSTEM.lock().pds.install_cap(pd, slot, cap);
}
pub fn clear_pd_cap(pd: usize, slot: usize) {
    SYSTEM.lock().pds.clear_cap(pd, slot);
}

/// Einen blockierten Endpoint-Empfänger zurückziehen (Hot-Reload).
pub fn endpoint_retire_receiver(ep: usize, tid: ThreadId) -> bool {
    SYSTEM.lock().eps.retire_receiver(ep, tid)
}
