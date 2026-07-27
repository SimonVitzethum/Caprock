#![no_std]
#![no_main]
#![feature(allocator_api)]
#![feature(btreemap_alloc)]
//! SEL4Lake kernel — bootable image entry point.
//!
//! Phase 1 (HAL): Boot, Exception-Vektoren, Identity-MMU + Caches (W^X), GICv2,
//! Timer, SMP-Bring-up. Phase 2: capability-basiertes physisches Speichermodell
//! (`sel4lake-mem`, hier per Selbsttest exerziert). Die Hardware-Spezifik liegt
//! in `sel4lake-hal`; diese Crate verdrahtet Boot-Trampolin und Init-Reihenfolge.

// ext-25: prozess-lokale Heaps (sel4lake-region) nutzen den allocator_api — `Box`/`Vec` werden
// stets mit EXPLIZITEM Allokator (`*_in(&heap)`) erzeugt. Es gibt bewusst KEINEN globalen Heap;
// der Global-Allocator unten ist ein Wächter, der versehentliche `Box::new`/`Vec::new` abfängt.
extern crate alloc;

/// Wächter-Global-Allocator: das SAS-Modell verlangt **prozess-lokale** Heap-Instanzen
/// (`Heap::new(source)` + `*_in(&heap)`). Ein impliziter globaler Heap existiert nicht.
struct NoGlobalHeap;
// SAFETY: niemals ein Block vergeben/freigegeben; jede Nutzung paniert (= Designfehler-Wächter).
unsafe impl core::alloc::GlobalAlloc for NoGlobalHeap {
    unsafe fn alloc(&self, _l: core::alloc::Layout) -> *mut u8 {
        panic!("kein globaler Heap im SAS — prozess-lokale Heap-Instanz nutzen (*_in)")
    }
    unsafe fn dealloc(&self, _p: *mut u8, _l: core::alloc::Layout) {}
}
#[global_allocator]
static GLOBAL: NoGlobalHeap = NoGlobalHeap;

mod arch;
mod loader;
mod panic;
mod selftest;
mod system;
mod threads;
/// Read-only TrustedSAS-Root-Key-DB (ext-28, ADR 0014) — autogeneriert von `tools/gen_trusted_key.py`,
/// in den Kernel kompiliert, nur per Firmware-/Kernel-Update änderbar (nicht per Syscall).
mod trusted_keys;

use sel4lake_hal::{self as hal, println};

/// Zielkonfiguration: ARM, 8 Kerne.
const NUM_CORES: usize = 8;
/// Stack-Größe je Sekundärkern (muss zur Reservierung in `linker.ld` passen).
const SEC_STACK_SIZE: u64 = 0x10000;

/// Periodische Tick-Rate des Timers (Hz). 100 Hz = 10-ms-Zeitscheiben.
const TICK_HZ: u64 = 100;

/// RAM-Layout der Zielplattform (Fallback; tatsächlich aus dem DTB gelesen).
const RAM_BASE: u64 = 0x4000_0000;
const RAM_END: u64 = RAM_BASE + 4 * 1024 * 1024 * 1024;

/// Von QEMU erzeugter Device Tree (eingebettet — siehe `sel4lake-dtb`).
static DTB_BYTES: &[u8] = include_bytes!("virt.dtb");

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
    println!("boot-x0 : {dtb_addr:#018x} (DTB-Zeiger; bei QEMU-ELF 0 -> DTB eingebettet)");
    println!("mmu     : identity-map, M={} C={} I={} (caches an)", m as u8, c as u8, i as u8);

    // Distributor global + Init des Primärkerns (core 0).
    hal::gic::init_dist();
    init_core_irqs();
    println!("core 0  : online (vectors, gic, timer @ {} Hz)", TICK_HZ);
    println!("timer   : CNTFRQ={} Hz, PPI {}", hal::timer::freq(), hal::timer::TIMER_INTID);

    // Spekulations-Eigenschaften der HW melden (ext-29). CSV2/CSV3 sagen, ob die HW von sich
    // aus gegen Spectre-v2 (Branch-Predictor über Kontexte) bzw. Meltdown immun ist; der Kernel
    // härtet unabhängig davon seine EL0-Indexpfade (`array_index_nospec`) und setzt beim
    // VSpace-Wechsel eine Spekulationsbarriere. NICHT abgedeckt bleiben Cache-/Timing-
    // Seitenkanäle zwischen PDs (keine Cache-Partitionierung) — s. docs/invariants.md.
    println!(
        "spec    : CSV2={} CSV3={} FEAT_SB={} · nospec-Indizes an, Barriere beim VSpace-Wechsel",
        hal::cpu::csv2(),
        hal::cpu::csv3(),
        hal::cpu::sb_supported() as u8
    );

    // RAM-Layout aus dem Device Tree lesen (statt fest verdrahtet).
    let (ram_base, ram_size) = sel4lake_dtb::Dtb::parse(DTB_BYTES)
        .and_then(|d| d.memory())
        .unwrap_or((RAM_BASE, RAM_END - RAM_BASE));
    let ram_end = ram_base + ram_size;
    println!(
        "dtb     : RAM base={ram_base:#x} size={} MiB (aus Device Tree)",
        ram_size >> 20
    );
    let dtb_ok = ram_base == RAM_BASE && ram_size == 4 * 1024 * 1024 * 1024;
    println!("dtb     : {}", if dtb_ok { "ALL PASS" } else { "FAILURES" });

    // Phase 2/3: capability-basiertes Speichermodell + Capability-Space.
    // User-RAM erst ab 2 MiB: die ersten 2 MiB sind die geteilte Kernel-L3 (von
    // jeder isolierten VSpace genutzt) und dürfen kein EL0-zugängliches RAM enthalten.
    let free_base = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    // ext-26: das oberste RAM-Fenster [MOD_BASE, ram_end) ist für das extern geladene Boot-Archiv
    // reserviert (QEMU `-device loader`); der Allokator bekommt es NICHT -> kein Konflikt.
    let alloc_end = ram_end.min(loader::MOD_BASE);
    system::init_mem(free_base, alloc_end);
    println!("mem     : freies RAM [{free_base:#x}, {alloc_end:#x})  (Loader-Fenster [{:#x}, {ram_end:#x}) reserviert)", loader::MOD_BASE);
    loader::probe(); // Boot-Archiv lesen + Module melden (L0; Laden folgt ab L1)
    selftest::run();

    // Phase 4–6: Scheduler + cap-gesicherte IPC + Protection Domains.
    system::set_hooks(); // Reschedule- + Syscall-Hook registrieren (vor IRQs)
    system::bind_cores(); // alle per-Kern-Scheduler an ihre Kern-ID binden (vor spawn)
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
        // Jeder Kern sammelt seine EIGENEN beendeten Threads ein (per-Kern-Reaping):
        // gibt deren Stacks an den Allokator zurück. So lecken auch Threads, die auf
        // einem Sekundärkern enden (z. B. lastbewusst platzierte), keinen Speicher.
        system::reap();
        hal::cpu::wfi();
    }
}
