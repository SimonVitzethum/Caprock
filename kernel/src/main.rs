#![no_std]
#![no_main]
// Die allocator_api-Features braucht der (arch-agnostische) Kernel-Kern; auf dem x86_64-Branch ist
// in Stufe 0 nur Boot+Serial aktiv (Kern folgt) -> nur fuer aarch64 anfordern.
#![cfg_attr(target_arch = "aarch64", feature(allocator_api))]
#![cfg_attr(target_arch = "aarch64", feature(btreemap_alloc))]
//! Caprock kernel — bootable image entry point.
//!
//! Phase 1 (HAL): Boot, Exception-Vektoren, Identity-MMU + Caches (W^X), GICv2,
//! Timer, SMP-Bring-up. Phase 2: capability-basiertes physisches Speichermodell
//! (`caprock-mem`, hier per Selbsttest exerziert). Die Hardware-Spezifik liegt
//! in `caprock-hal`; diese Crate verdrahtet Boot-Trampolin und Init-Reihenfolge.

// ext-25: prozess-lokale Heaps (caprock-region) nutzen den allocator_api — `Box`/`Vec` werden
// stets mit EXPLIZITEM Allokator (`*_in(&heap)`) erzeugt. Es gibt bewusst KEINEN globalen Heap;
// der Global-Allocator unten ist ein Wächter, der versehentliche `Box::new`/`Vec::new` abfängt.
#[cfg(target_arch = "aarch64")]
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

/// Zwei Adressachsen (physisch / IOVA) als getrennte Typen — s. Moduldoku.
mod addr;
mod arch;
mod panic;
/// Seitenfarben / Cache-Partitionierung zwischen PDs (todo A1) — arch-neutral.
mod colors;
// Der Kernel-Kern (arch-agnostisch, nutzt aber die aarch64-HAL) ist auf dem x86_64-Branch in Stufe 0
// noch nicht aktiv — er wird Stufe fuer Stufe fuer x86_64 eingeschaltet (s. README-X86.md).
#[cfg(feature = "selftest")]
mod dmatests;
/// Z26, Vorbedingung 2: grosse, zusammenhaengende DMA mit Geraetesicht — und eine **benannte**
/// Absage statt `None`. Arch-neutral; die Klassifikation liegt host-getestet in `caprock-dma`.
mod grossdma;
/// Kernel-Glue des generischen Binary-Loaders. **Seit A-1 auf beiden Architekturen** — die
/// Archiv-Quelle ist nicht mehr ein fest verdrahtetes ARM-Fenster, sondern eine zur Laufzeit
/// gemeldete Spanne (auf x86 ein Multiboot-Modul).
mod loader;
/// Read-only Root-Schlüssel des **System-Manifests** (A-1.3) — autogeneriert von
/// `tools/gen_manifest_key.py`. Getrennt von [`trusted_keys`]: ein Zertifikat bezeugt die Herkunft
/// eines Binaries, ein Manifest die Zuteilung der ganzen Maschine.
mod manifest_keys;
#[cfg(feature = "selftest")]
mod selftest;
mod system;
#[cfg(all(target_arch = "aarch64", feature = "selftest"))]
mod threads;
/// Read-only TrustedSAS-Root-Key-DB (ext-28, ADR 0014) — autogeneriert von `tools/gen_trusted_key.py`,
/// in den Kernel kompiliert, nur per Firmware-/Kernel-Update änderbar (nicht per Syscall).
mod trusted_keys;

#[cfg(target_arch = "aarch64")]
use caprock_hal::{self as hal, println};

// --- aarch64-Kernel-Kern (auf dem x86_64-Branch noch inaktiv; Boot-Entry kommt aus arch). ---

/// Rückfall-Kernzahl, wenn der Device Tree keine `cpu@`-Knoten meldet.
#[cfg(target_arch = "aarch64")]
const FALLBACK_CORES: usize = 8;
/// Stack-Größe je Sekundärkern (zur Boot-Zeit aus dem RAM belegt).
#[cfg(target_arch = "aarch64")]
const SEC_STACK_SIZE: u64 = 0x10000;

/// Periodische Tick-Rate des Timers (Hz). 100 Hz = 10-ms-Zeitscheiben.
#[cfg(target_arch = "aarch64")]
const TICK_HZ: u64 = 100;

/// RAM-Layout der Zielplattform (Fallback; tatsächlich aus dem DTB gelesen).
#[cfg(target_arch = "aarch64")]
const RAM_BASE: u64 = 0x4000_0000;
#[cfg(target_arch = "aarch64")]
const RAM_END: u64 = RAM_BASE + 4 * 1024 * 1024 * 1024;

/// Von QEMU erzeugter Device Tree (eingebettet — siehe `caprock-dtb`).
#[cfg(target_arch = "aarch64")]
static DTB_BYTES: &[u8] = include_bytes!("virt.dtb");

#[cfg(target_arch = "aarch64")]
extern "C" {
    /// Sekundärkern-Einstieg (Assembler, `arch::aarch64::boot`).
    fn _start_secondary();
}

/// Basis der zur Boot-Zeit belegten Sekundär-Stacks (ext-30: nicht mehr im Linker
/// reserviert — bei bis zu 256 Kernen wären das 16 MiB tote Image-Größe, und die Kernzahl
/// steht erst zur Laufzeit fest).
#[cfg(target_arch = "aarch64")]
static SEC_STACKS_BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Stack-Spitze für Kern `core` (Slot `core` im belegten Block).
#[cfg(target_arch = "aarch64")]
fn secondary_stack_top(core: usize) -> u64 {
    SEC_STACKS_BASE.load(core::sync::atomic::Ordering::Relaxed) + (core as u64) * SEC_STACK_SIZE
}

/// Pro-Kern-Interrupt-Init (nach MMU). VBAR wird bereits vor der MMU gesetzt.
#[cfg(target_arch = "aarch64")]
fn init_core_irqs() {
    hal::intc::init_cpu(); // GIC-CPU-Interface (pro Kern)
    hal::timer::init(TICK_HZ); // Timer-PPI armieren (pro Kern)
}

/// Kernel-Eintritt des Primärkerns, gerufen vom Boot-Trampolin.
///
/// `dtb_addr` ist die physische Adresse des Device-Tree-Blobs (von QEMU in `x0`).
#[cfg(target_arch = "aarch64")]
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
    println!(" Caprock — capability microkernel");
    println!(" phase 1: HAL bring-up");
    println!("========================================");
    println!("arch    : aarch64 (running at EL{})", hal::cpu::current_el());
    println!("boot-x0 : {dtb_addr:#018x} (DTB-Zeiger; bei QEMU-ELF 0 -> DTB eingebettet)");
    println!("mmu     : identity-map, M={} C={} I={} (caches an)", m as u8, c as u8, i as u8);

    // Distributor global + Init des Primärkerns (core 0).
    hal::intc::init_dist();
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
    // Cache-Geometrie melden (todo A1/B-2.2). Steht bewusst NEBEN der `spec`-Zeile: beides sagt,
    // was die Hardware von sich aus hergibt, und beides ist eine Bring-up-Meldung, kein
    // Testbericht (todo F3). Bis hierher lief die aarch64-Fassung von `hal::cache` nie -- sie war
    // geschrieben und uebersetzt, aber nur der x86-Hochlauf rief sie.
    crate::colors::report();

    // RAM-Layout aus dem Device Tree lesen (statt fest verdrahtet).
    let (ram_base, ram_size) = caprock_dtb::Dtb::parse(DTB_BYTES)
        .and_then(|d| d.memory())
        .unwrap_or((RAM_BASE, RAM_END - RAM_BASE));
    let ram_end = ram_base + ram_size;
    println!(
        "dtb     : RAM base={ram_base:#x} size={} MiB (aus Device Tree)",
        ram_size >> 20
    );
    // Kernzahl ebenfalls aus dem Device Tree (ext-30) statt fest verdrahtet.
    let cores = caprock_dtb::Dtb::parse(DTB_BYTES)
        .and_then(|d| d.cpu_count())
        .filter(|&n| n > 0)
        .unwrap_or(FALLBACK_CORES)
        .min(caprock_sched::MAX_CORES);
    println!("dtb     : {cores} CPUs (aus Device Tree; Kernel-Obergrenze {})", caprock_sched::MAX_CORES);
    let dtb_ok = ram_base == RAM_BASE && ram_size == 4 * 1024 * 1024 * 1024 && cores > 0;
    println!("dtb     : {}", if dtb_ok { "ALL PASS" } else { "FAILURES" });

    // Phase 2/3: capability-basiertes Speichermodell + Capability-Space.
    // User-RAM erst ab 2 MiB: die ersten 2 MiB sind die geteilte Kernel-L3 (von
    // jeder isolierten VSpace genutzt) und dürfen kein EL0-zugängliches RAM enthalten.
    let free_base = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    // ext-26: das oberste RAM-Fenster [MOD_BASE, ram_end) ist für das extern geladene Boot-Archiv
    // reserviert (QEMU `-device loader`); der Allokator bekommt es NICHT -> kein Konflikt.
    let alloc_end = ram_end.min(loader::MOD_BASE);
    system::init_mem(free_base, alloc_end);
    // Cap-Tabellen VOR dem ersten Cap: der Selbsttest gleich darunter installiert bereits welche.
    let cap_bytes = system::configure_caps();
    println!("mem     : freies RAM [{free_base:#x}, {alloc_end:#x})  (Loader-Fenster [{:#x}, {ram_end:#x}) reserviert)", loader::MOD_BASE);
    let (cap_slots, cap_objs) = system::cap_capacity();
    println!(
        "cap     : {cap_slots} Slots / {cap_objs} Objekte, Tabellen {} KiB aus dem RAM (Summe aller PD-Budgets: {})",
        cap_bytes >> 10,
        caprock_microkit::CAP_SLOTS_FOR_ALL_PDS
    );
    // A-3.4 Teil 4: IPC-Tabellen VOR dem ersten Endpoint — der Selbsttest und die Bringup-Kanäle
    // reservieren gleich welche. Meldet sich selbst (`ipc :`).
    system::configure_ipc();
    loader::probe(); // Boot-Archiv lesen + Module melden (L0; Laden folgt ab L1)
    #[cfg(feature = "selftest")]
    selftest::run();

    // Phase 4–6: Scheduler + cap-gesicherte IPC + Protection Domains.
    system::set_hooks(); // Reschedule- + Syscall-Hook registrieren (vor IRQs)
    // Kerneltabellen zur Boot-Zeit dimensionieren + alle per-Kern-Scheduler binden (vor spawn).
    let (nc, nthreads, per_core, tbl_bytes) = system::configure(cores);
    println!(
        "sched   : {nc} Kerne, {nthreads} Thread-Slots ({per_core} hostbar je Kern), Tabellen {} KiB aus dem RAM",
        tbl_bytes >> 10
    );
    system::init_core(); // Boot-Kontext von core 0 wird Idle-Thread
    #[cfg(feature = "selftest")]
    {
        threads::spawn_demo(); // 2 PDs (Client/Server) + 3 Worker auf core 0
        println!("sched   : Round-Robin + cap-gesicherte IPC (2 PDs + 3 Worker + Idle)");
    }

    // --- Root-Task (A-2.1/A-2.2) ---
    //
    // Dieselbe Stelle wie auf x86 (`arch::x86_64::bringup`) und aus demselben Grund **ausserhalb**
    // von `selftest`: das ist die Aufgabe des Kernels, nicht seine Pruefung. Bis A-2.2 rief den
    // Loader auf ARM ausschliesslich `threads/mod.rs` — also nur der Testcode. Ohne diesen Aufruf
    // ist der `--no-default-features`-Kernel auf ARM tatsaechlich leer, und das Gating waere kein
    // schlankerer Kernel, sondern ein Kernel ohne Zweck.
    // D5: erst das Autoritaetsdokument melden, dann den Root-Task starten. Genau diese Zeile fehlte
    // auf ARM -- die x86-Seite druckt sie seit A-1.4 (`bringup`), hier lief der Manifest-Pfad
    // ungeprueft mit. Ohne sie waere „root : ALL PASS" die einzige Aussage ueber ein Dokument, an
    // dem die gesamte Anfangsverteilung von Autoritaet haengt.
    loader::manifest_report();
    let _root_ok = loader::start_root_task_reported();

    // Sekundärkerne via PSCI starten.
    println!("smp     : starte Kerne 1..{} via PSCI CPU_ON (hvc) ...", cores - 1);
    // Sekundär-Stacks aus dem RAM (ein Block, ein Slot je Kern; Slot 0 bleibt ungenutzt —
    // der Bootkern hat seinen Stack aus dem Linker-Image).
    let stacks = system::alloc_anywhere(cores as u64 * SEC_STACK_SIZE, SEC_STACK_SIZE)
        .expect("Sekundaer-Stacks: RAM erschoepft");
    SEC_STACKS_BASE.store(stacks.base(), core::sync::atomic::Ordering::Relaxed);
    let entry = _start_secondary as *const () as u64;
    for core in 1..cores {
        let target = core as u64; // MPIDR Aff0 = Kernindex (QEMU virt, ein Cluster)
        let r = hal::power::cpu_on(target, entry, secondary_stack_top(core));
        if r != hal::power::SUCCESS {
            println!("smp     : CPU_ON für Kern {core} fehlgeschlagen (status {r})");
        }
    }

    // Alle Kerne haben ihren `CTR_EL0.CWG` gemeldet -> die DMA-Granularität steht fest. Bis
    // hierher galt die architektonische Obergrenze (s. `mmu::seal_cache_granule`).
    hal::mmu::seal_cache_granule();

    hal::cpu::local_irq_enable();
    // Ab hier läuft core 0 als Idle-Thread; der Timer-Tick schedult preemptiv.
    #[cfg(feature = "selftest")]
    threads::demo_report_then_idle();
    // Ohne `selftest` gibt es keinen Bericht, auf den zu warten waere: core 0 wird Idle-Thread,
    // und was laeuft, laeuft im Root-Task. `idle()` reapt weiter (Stacks beendeter Threads) und
    // wartet per WFI — dieselbe Schleife wie auf jedem Sekundaerkern.
    #[cfg(not(feature = "selftest"))]
    {
        println!("bringup : Kernel-Kern steht; Aufgaben kommen ab hier aus dem Root-Task (A-2.2)");
        idle();
    }
}

/// Kernel-Eintritt jedes Sekundärkerns (gerufen aus `_start_secondary`).
#[cfg(target_arch = "aarch64")]
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
#[cfg(target_arch = "aarch64")]
fn idle() -> ! {
    loop {
        // Jeder Kern sammelt seine EIGENEN beendeten Threads ein (per-Kern-Reaping):
        // gibt deren Stacks an den Allokator zurück. So lecken auch Threads, die auf
        // einem Sekundärkern enden (z. B. lastbewusst platzierte), keinen Speicher.
        system::reap();
        hal::cpu::wfi();
    }
}
