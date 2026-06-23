#![no_std]
#![no_main]
//! SEL4Lake kernel — bootable image entry point.
//!
//! Phase 1 (HAL): Boot, Exception-Vektoren, Identity-MMU + Caches (W^X), GICv2,
//! Timer, SMP-Bring-up. Phase 2: capability-basiertes physisches Speichermodell
//! (`sel4lake-mem`, hier per Selbsttest exerziert). Die Hardware-Spezifik liegt
//! in `sel4lake-hal`; diese Crate verdrahtet Boot-Trampolin und Init-Reihenfolge.

mod arch;
mod panic;
mod selftest;
mod system;
mod threads;

use sel4lake_hal::{self as hal, println};

/// Zielkonfiguration: ARM, 8 Kerne.
const NUM_CORES: usize = 8;
/// Stack-Größe je Sekundärkern (muss zur Reservierung in `linker.ld` passen).
const SEC_STACK_SIZE: u64 = 0x10000;

/// Periodische Tick-Rate des Timers (Hz). 100 Hz = 10-ms-Zeitscheiben.
const TICK_HZ: u64 = 100;

/// RAM-Layout der Zielplattform (QEMU `virt`, 4 GiB): [0x4000_0000, +4 GiB).
const RAM_BASE: u64 = 0x4000_0000;
const RAM_END: u64 = RAM_BASE + 4 * 1024 * 1024 * 1024;

extern "C" {
    /// Sekundärkern-Einstieg (Assembler, `arch::aarch64::boot`).
    fn _start_secondary();
    /// Basis der im Linker reservierten Sekundär-Stacks.
    static __sec_stacks_bottom: u8;
}

/// Stack-Spitze für Kern `core` (Slot `core` im reservierten Bereich).
fn secondary_stack_top(core: usize) -> u64 {
    let base = core::ptr::addr_of!(__sec_stacks_bottom) as u64;
    base + (core as u64 + 1) * SEC_STACK_SIZE
}

/// Pro-Kern-Interrupt-Init (nach MMU). VBAR wird bereits vor der MMU gesetzt.
fn init_core_irqs() {
    hal::gic::init_cpu(); // GIC-CPU-Interface (pro Kern)
    hal::timer::init(TICK_HZ); // Timer-PPI armieren (pro Kern)
}

/// Kernel-Eintritt des Primärkerns, gerufen vom Boot-Trampolin.
///
/// `dtb_addr` ist die physische Adresse des Device-Tree-Blobs (von QEMU in `x0`).
#[no_mangle]
pub extern "C" fn kernel_main(dtb_addr: u64) -> ! {
    // Vor der MMU sind Atomics/Spinlocks nicht wohldefiniert -> lock-freie Ausgabe.
    hal::console::emit_raw("\nboot: primary core up (pre-MMU)\n");

    // Exception-Vektoren früh setzen, damit Faults während des MMU-Bring-ups
    // diagnostiziert werden (Dump ist lock-frei).
    hal::exception::init();

    // MMU + Caches zuerst: danach sind Atomics/der Konsolen-Lock wohldefiniert.
    hal::mmu::init_primary();

    let (m, c, i) = hal::mmu::sctlr_flags();
    println!("========================================");
    println!(" SEL4Lake — capability microkernel");
    println!(" phase 1: HAL bring-up");
    println!("========================================");
    println!("arch    : aarch64 (running at EL{})", hal::cpu::current_el());
    println!("dtb     : {dtb_addr:#018x}");
    println!("mmu     : identity-map, M={} C={} I={} (caches an)", m as u8, c as u8, i as u8);

    // Distributor global + Init des Primärkerns (core 0).
    hal::gic::init_dist();
    init_core_irqs();
    println!("core 0  : online (vectors, gic, timer @ {} Hz)", TICK_HZ);
    println!("timer   : CNTFRQ={} Hz, PPI {}", hal::timer::freq(), hal::timer::TIMER_INTID);

    // Phase 2/3: capability-basiertes Speichermodell + Capability-Space.
    let free_base = hal::mmu::kernel_end();
    system::init_mem(free_base, RAM_END);
    println!("mem     : freies RAM [{free_base:#x}, {RAM_END:#x})");
    selftest::run();

    // Phase 4–6: Scheduler + cap-gesicherte IPC + Protection Domains.
    system::set_hooks(); // Reschedule- + Syscall-Hook registrieren (vor IRQs)
    system::init_core(); // Boot-Kontext von core 0 wird Idle-Thread
    threads::spawn_demo(); // 2 PDs (Client/Server) + 3 Worker auf core 0
    println!("sched   : Round-Robin + cap-gesicherte IPC (2 PDs + 3 Worker + Idle)");

    // Sekundärkerne via PSCI starten.
    println!("smp     : starte Kerne 1..{} via PSCI CPU_ON (hvc) ...", NUM_CORES - 1);
    let entry = _start_secondary as *const () as u64;
    for core in 1..NUM_CORES {
        let target = core as u64; // MPIDR Aff0 = Kernindex (QEMU virt, ein Cluster)
        let r = hal::psci::cpu_on(target, entry, secondary_stack_top(core));
        if r != hal::psci::SUCCESS {
            println!("smp     : CPU_ON für Kern {core} fehlgeschlagen (status {r})");
        }
    }

    hal::cpu::local_irq_enable();
    // Ab hier läuft core 0 als Idle-Thread; der Timer-Tick schedult preemptiv.
    threads::demo_report_then_idle();
}

/// Kernel-Eintritt jedes Sekundärkerns (gerufen aus `_start_secondary`).
#[no_mangle]
pub extern "C" fn kernel_secondary_main() -> ! {
    // Vektoren vor der MMU setzen (Fault-Diagnose), dann MMU (gemeinsame Tabelle)
    // aktivieren -> erst danach Atomics/Konsole gültig.
    hal::exception::init();
    hal::mmu::init_secondary();
    init_core_irqs();

    let core = hal::cpu::core_id();
    println!("core {core}  : online (EL{}, MMU on)", hal::cpu::current_el());

    // Boot-Kontext dieses Kerns als Idle-Thread registrieren (vor IRQ-Freigabe).
    system::init_core();
    hal::cpu::local_irq_enable();
    idle();
}

/// Idle-Schleife: auf Interrupts warten (Low-Power).
fn idle() -> ! {
    loop {
        hal::cpu::wfi();
    }
}
