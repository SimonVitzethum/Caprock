//! **Kernel-Kern-Bring-up auf x86_64** (ext-31, Stufe 4).
//!
//! Bis Stufe 3 war der x86-Zweig eine Kette von Hardware-Demos: Boot, Paging, IDT, LAPIC,
//! Ring-3-Round-Trip — jeweils direkt gegen die Hardware, ohne den eigentlichen Microkernel.
//! Hier läuft nun der **echte Kern**: derselbe `system.rs`, derselbe Capability-Space,
//! derselbe per-Kern-Scheduler, dasselbe cap-gesicherte IPC wie auf aarch64. Möglich wurde
//! das, weil `sel4lake-hal` jetzt architekturselektiv ist — der Kern selbst enthält kein
//! einziges `cfg(target_arch)`.
//!
//! Was hier läuft:
//!
//! | Prüfung | Aussage |
//! |---|---|
//! | `memtest`/`zerotest`/`captest`/`budget` | die arch-neutralen Selbsttests (Allokator, Datenremanenz, CDT/Refcounts, Cap-Budget) — **unverändert** dieselben wie auf ARM |
//! | `sched` | echte **Präemption**: der LAPIC-Timer verdrängt Threads über den Trap-Frame-Tausch |
//! | `ipc` | cap-gesicherter `CALL`/`RECV`/`REPLY` zwischen zwei Protection Domains |
//! | `audit` | Scheduler- + CDT-Audit nach dem Lauf sauber |
//!
//! Noch **nicht** auf x86 (s. `todo.md`): SMP (INIT-SIPI-SIPI), isolierte Adressräume
//! (PCID + per-VSpace-Tabellen), Ring-3-PDs im Kernel-Kern, Boot-Archiv/Loader, IOMMU.

use crate::system;
use sel4lake_abi::{result, sys};
use sel4lake_hal::{self as hal, println, syscall::invoke};
use sel4lake_mem::Rights;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// RAM-Obergrenze für den Allokator.
///
/// Der Multiboot-Speicherplan läge in der Info-Struktur, die der Bootloader in `EBX` übergibt;
/// das Trampolin reicht sie derzeit nicht durch. Bis dahin die im Testlauf zugesicherte Größe
/// (`qemu -m 512M`) — bewusst konservativ: zu klein ist harmlos, zu groß wäre es nicht.
const RAM_END: u64 = 512 * 1024 * 1024;

/// Zeitscheibe (Hz) — wie auf aarch64.
const TICK_HZ: u64 = 100;

// --- Telemetrie der Demo-Threads ------------------------------------------------------------

const NWORKERS: usize = 3;
/// So viele Runden muss jeder Worker schaffen, damit „Präemption läuft" belegt ist.
const WORK_TARGET: u64 = 3;
static WORKER_ROUNDS: [AtomicU64; NWORKERS] = [const { AtomicU64::new(0) }; NWORKERS];
static IPC_RESULT: AtomicU64 = AtomicU64::new(0);
static IPC_DONE: AtomicBool = AtomicBool::new(false);

/// Worker: zählt Runden. Da er **nie** freiwillig abgibt, beweist steigender Fortschritt
/// aller Worker, dass der Timer-Interrupt sie gegeneinander verdrängt.
extern "C" fn worker(arg: usize) -> ! {
    loop {
        WORKER_ROUNDS[arg].fetch_add(1, Ordering::Relaxed);
        for _ in 0..200_000 {
            core::hint::spin_loop();
        }
    }
}

/// IPC-Server: verdoppelt die erste Nachricht und antwortet. Erreichbar **nur** über die
/// Endpoint-Cap in Slot 0 seiner PD.
extern "C" fn ipc_server(_arg: usize) -> ! {
    loop {
        let m = invoke(sys::RECV, 0, [0; 4], 0);
        if m.result != result::OK {
            break;
        }
        invoke(sys::REPLY, 0, [2 * m.msg[0], 0, 0, 0], 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// IPC-Client: ruft den Server über seine Send-Cap und hält das Ergebnis fest.
extern "C" fn ipc_client(_arg: usize) -> ! {
    let r = invoke(sys::CALL, 0, [21, 0, 0, 0], 0);
    IPC_RESULT.store(r.msg[0], Ordering::Release);
    IPC_DONE.store(true, Ordering::Release);
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Alle Demo-Threads + PDs aufsetzen (vor dem Freigeben der Interrupts).
fn spawn_demo() -> bool {
    // Drei Worker auf dem Bootkern -> sie können nur durch Präemption alle vorankommen.
    for i in 0..NWORKERS {
        if system::spawn(worker as *const () as usize, i, system::IDLE_PRIO).is_none() {
            return false;
        }
    }

    // Cap-gesichertes IPC: ein Endpoint, zwei PDs. Der Server hält die RECV-, der Client die
    // SEND-Cap — ohne Cap ist der Endpoint für beide unsichtbar (das ist die Autoritätsregel).
    let Some(ep) = system::create_endpoint() else {
        return false;
    };
    let Ok(root) = system::install_endpoint_cap(ep as u32, Rights::RWX) else {
        return false;
    };
    let (Ok(recv), Ok(send)) = (
        system::cap_mint(root, Rights::READ, 0),
        system::cap_mint(root, Rights::WRITE, 0),
    ) else {
        return false;
    };
    let (Some(srv_pd), Some(cli_pd)) = (system::create_pd(), system::create_pd()) else {
        return false;
    };
    if !system::install_pd_cap(srv_pd, 0, recv) || !system::install_pd_cap(cli_pd, 0, send) {
        return false;
    }
    let (Some(srv), Some(cli)) = (
        system::spawn(ipc_server as *const () as usize, 0, system::IDLE_PRIO),
        system::spawn(ipc_client as *const () as usize, 0, system::IDLE_PRIO),
    ) else {
        return false;
    };
    system::bind_pd(srv_pd, srv);
    system::bind_pd(cli_pd, cli);
    true
}

/// Sind alle Demo-Aussagen belegt?
fn all_done() -> bool {
    let workers = (0..NWORKERS).all(|i| WORKER_ROUNDS[i].load(Ordering::Relaxed) >= WORK_TARGET);
    workers && IPC_DONE.load(Ordering::Acquire) && hal::timer::ticks(0) > 0
}

/// Bericht + Abschaltung (das Testskript wertet die Marker aus).
fn report_and_off() -> ! {
    let ticks = hal::timer::ticks(0);
    println!("sched   : core 0 ticks={ticks}");
    let rounds: [u64; NWORKERS] = core::array::from_fn(|i| WORKER_ROUNDS[i].load(Ordering::Relaxed));
    println!(
        "sched   : Worker-Runden {rounds:?} (jeder >= {WORK_TARGET} -> Timer verdraengt sie gegeneinander)"
    );
    let sched_ok = ticks > 0 && rounds.iter().all(|&r| r >= WORK_TARGET);
    println!("sched   : {}", if sched_ok { "ALL PASS" } else { "FAILURES" });

    let ipc = IPC_RESULT.load(Ordering::Acquire);
    println!("ipc     : CALL(21) ueber Endpoint-Cap -> {ipc} (erwartet 42)");
    println!("ipc     : {}", if ipc == 42 { "ALL PASS" } else { "FAILURES" });

    let sa = system::sched_audit_all();
    let cdt = system::cap_audit_cdt();
    println!("audit   : sched_audit={sa} cdt_audit={cdt}");
    println!(
        "audit   : {}",
        if sa == 0 && cdt == 0 { "ALL PASS" } else { "FAILURES" }
    );

    println!("x86_64 Stufe 4: Kernel-Kern laeuft (Selbsttests + Scheduler + cap-gesicherte IPC)");
    println!("== SELFTEST COMPLETE -> system_off ==");
    hal::power::system_off()
}

/// Einstieg des Kernel-Kerns auf x86_64 (vom Boot-Trampolin über `x86_rust_entry` gerufen).
pub fn run() -> ! {
    // --- Hardware in der Reihenfolge hochziehen, in der sie voneinander abhängt ---
    hal::console::init();
    println!("========================================");
    println!(" SEL4Lake — capability microkernel");
    println!(" x86_64 (Multiboot -> Long Mode)");
    println!("========================================");
    hal::exception::init(); // IDT: Faults ab hier diagnostizierbar
    hal::gdt::init(); // GDT + TSS (Selektoren für Trap-/Ring-Wechsel)
    hal::mmu::init_primary(); // 4-Level-Paging, W^X, CR0.WP
    let (m, c, w) = hal::mmu::sctlr_flags();
    println!("mmu     : identity-map, paging={} caches={} CR0.WP={}", m as u8, c as u8, w as u8);
    println!("arch    : x86_64 (CPL {} = Ring 0)", 1 - hal::cpu::current_el());

    hal::intc::init_dist(); // 8259-PIC stilllegen
    hal::intc::init_cpu(); // LAPIC aktivieren
    hal::timer::init(TICK_HZ);
    println!(
        "timer   : LAPIC-Timer {} Hz (Basis {} Hz, gegen PIT kalibriert), Vektor {}",
        TICK_HZ,
        hal::timer::freq(),
        hal::timer::TIMER_INTID
    );
    println!(
        "spec    : CSV2={} CSV3={} FEAT_SB={} · nospec-Indizes an",
        hal::cpu::csv2(),
        hal::cpu::csv3(),
        hal::cpu::sb_supported() as u8
    );

    // --- Speicher + arch-neutrale Selbsttests (identisch zu aarch64) ---
    let free_base = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    system::init_mem(free_base, RAM_END);
    println!("mem     : freies RAM [{free_base:#x}, {RAM_END:#x})");
    crate::selftest::run();

    // --- Kernel-Kern: Hooks, Tabellen, Scheduler ---
    system::set_hooks();
    let (nc, nthreads, per_core, tbl) = system::configure(1); // SMP folgt (s. todo.md)
    println!(
        "sched   : {nc} Kern, {nthreads} Thread-Slots ({per_core} hostbar), Tabellen {} KiB aus dem RAM",
        tbl >> 10
    );
    // Der Bootkern MUSS die Kern-ID haben, für die `configure` Tabellen angelegt hat —
    // sonst hätte sein Scheduler keinen Speicher und der erste Trap fände keinen Thread.
    let boot_core = hal::cpu::core_id();
    if boot_core >= nc {
        println!("bringup : FAILURES (Bootkern hat LAPIC-ID {boot_core}, erwartet < {nc})");
        hal::power::system_off();
    }
    let before = system::threads_available();
    system::init_core();
    let after = system::threads_available();
    println!("bringup : init_core: Thread-Slots {before} -> {after} (Idle belegt einen)");
    if !spawn_demo() {
        println!("bringup : FAILURES (Demo-Aufbau fehlgeschlagen)");
        hal::power::system_off();
    }
    println!("bringup : 3 Worker + 2 PDs (IPC-Server/Client) eingeplant");

    // Ab hier schedult der Timer-Interrupt präemptiv; dieser Kontext ist der Idle-Thread.
    hal::cpu::local_irq_enable();
    let mut spins: u64 = 0;
    loop {
        system::reap();
        if all_done() {
            report_and_off();
        }
        // Notbremse, damit ein hängender Test nicht ewig läuft (der Bericht zeigt dann, was fehlt).
        spins += 1;
        if spins > 50_000_000 {
            println!("bringup : WATCHDOG — nicht alle Aussagen belegt");
            report_and_off();
        }
        core::hint::spin_loop();
    }
}
