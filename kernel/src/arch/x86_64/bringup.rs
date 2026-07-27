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

/// Rückfall-RAM-Obergrenze, falls der Bootloader keinen Speicherplan mitgibt.
const RAM_END_FALLBACK: u64 = 128 * 1024 * 1024;

/// **Speicherplan aus der Multiboot-Info** — das x86-Gegenstück zum `/memory`-Knoten des DTB.
///
/// Gesucht wird das Ende des nutzbaren RAM: die größte als „available" (Typ 1) gemeldete
/// Region, die oberhalb von 1 MiB beginnt (darunter liegen BIOS-/Legacy-Bereiche). Alle
/// Zugriffe sind gegen die von der Struktur selbst gemeldeten Längen geprüft; ein fehlendes
/// oder unplausibles `mmap` führt zum Rückfallwert, nie zu einem Fehlzugriff.
fn ram_end_from_multiboot(info: u64) -> Option<u64> {
    if info == 0 {
        return None;
    }
    // SAFETY: Der Bootloader übergibt einen gültigen Zeiger auf seine Info-Struktur im
    // identity-gemappten Low-Memory; wir lesen nur die Felder, deren Vorhandensein `flags`
    // zusichert.
    let (flags, mmap_len, mmap_addr) = unsafe {
        (
            core::ptr::read_volatile(info as *const u32),
            core::ptr::read_volatile((info + 44) as *const u32) as u64,
            core::ptr::read_volatile((info + 48) as *const u32) as u64,
        )
    };
    if flags & (1 << 6) == 0 || mmap_len == 0 {
        return None; // kein Speicherplan vorhanden
    }
    let mut best = 0u64;
    let mut off = 0u64;
    while off + 24 <= mmap_len {
        let e = mmap_addr + off;
        // SAFETY: `e` liegt innerhalb des von `mmap_len` aufgespannten Bereichs (s. Schleife).
        let (size, base, len, kind) = unsafe {
            (
                core::ptr::read_volatile(e as *const u32) as u64,
                core::ptr::read_volatile((e + 4) as *const u64),
                core::ptr::read_volatile((e + 12) as *const u64),
                core::ptr::read_volatile((e + 20) as *const u32),
            )
        };
        if kind == 1 && base >= 0x10_0000 {
            best = best.max(base + len);
        }
        if size == 0 {
            break; // defekte Kette -> abbrechen statt weiterzuraten
        }
        off += size + 4; // `size` zählt sich selbst nicht mit
    }
    (best > 0x10_0000).then_some(best)
}

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

// --- Ring-3-Threads (ext-32) ---------------------------------------------------------------
//
// Der Kernel-Code liegt in supervisor-only Seiten; Ring-3-Code braucht eigene, `US`-markierte
// Seiten. Deshalb steht er — wie die EL0-Demos auf aarch64 — in `.user_text`, samt seiner
// Syscalls: eine Ring-3-Funktion darf keine Kernel-Funktion aufrufen (die Seite ist für sie
// nicht ausführbar), also ist der `int 0x80` hier direkt eingebettet.
/// Der Zähler wird **aus Ring 3** hochgezählt und muss deshalb in einer `US`-schreibbaren Seite
/// liegen — die Kernel-Statics (`.bss`/`.data`) sind supervisor-only.
#[link_section = ".user_data"]
static USER_SYSCALLS: AtomicU64 = AtomicU64::new(0);

/// Ring-3-Arbeiter: ruft den Kernel per Syscall (`SYS_YIELD`) und zählt die Runden.
#[link_section = ".user_text"]
extern "C" fn ring3_worker(_arg: usize) -> ! {
    loop {
        // SAFETY: `int 0x80` ist der für Ring 3 freigegebene Syscall-Vektor (IDT-Gate DPL 3);
        // der Kernel liest/schreibt nur die ABI-Register dieses Frames.
        unsafe {
            core::arch::asm!("int 0x80", in("rax") 0u64, in("rdi") 0u64,
                             lateout("rax") _, lateout("rdi") _, clobber_abi("sysv64"));
        }
        // Atomarer Zugriff auf eine `US`-schreibbare Seite (s. `USER_SYSCALLS`) — das ist eine
        // Instruktion, kein Aufruf in den (für Ring 3 nicht ausführbaren) Kernel-Code.
        USER_SYSCALLS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Ring-3-Eindringling: liest **Kernel**-Speicher. Muss faulten — der Kernel beendet ihn und
/// läuft weiter (das x86-Gegenstück zum `el0iso`-Test auf aarch64).
#[link_section = ".user_text"]
extern "C" fn ring3_intruder(_arg: usize) -> ! {
    // SAFETY(-Absicht): Genau dieser Zugriff SOLL fehlschlagen. Das Kernel-Image liegt bei
    // 1 MiB in supervisor-only Seiten; ein Ring-3-Lesezugriff dorthin muss #PF auslösen.
    unsafe {
        let v = core::ptr::read_volatile(0x10_0000 as *const u64);
        core::ptr::write_volatile(0x20_0000 as *mut u64, v); // nie erreicht
    }
    loop {}
}

/// Stackgröße je Sekundärkern (aus dem RAM belegt, wie auf aarch64 seit ext-30).
const AP_STACK_BYTES: u64 = 64 * 1024;

/// Einstieg eines **Sekundärkerns** (aus dem AP-Trampolin, bereits im Long Mode mit den
/// Seitentabellen des BSP und eigenem Stack).
///
/// Dieselbe Reihenfolge wie beim Bootkern, nur ohne die globalen Schritte (Seitentabellen,
/// PIC-Stilllegung, Kerneltabellen — die stehen schon).
extern "C" fn ap_entry() -> ! {
    hal::exception::init(); // IDT ist global, das IDTR-Register aber pro Kern
    hal::gdt::init_ap();
    hal::intc::init_cpu(); // eigener LAPIC
    hal::timer::init(TICK_HZ); // eigener Timer
    system::init_core(); // dieser Kontext wird der Idle-Thread dieses Kerns
    hal::power::ap_report_online();
    hal::cpu::local_irq_enable();
    loop {
        system::reap(); // jeder Kern sammelt seine eigenen Zombies ein
        hal::cpu::wfi();
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

    // Ring-3-Threads: einer, der ordentlich per Syscall arbeitet, und einer, der Kernel-Speicher
    // liest und dafür beendet werden muss.
    if system::spawn_user(ring3_worker as *const () as usize, 0, system::IDLE_PRIO).is_none() {
        return false;
    }
    if system::spawn_user(ring3_intruder as *const () as usize, 0, system::IDLE_PRIO).is_none() {
        return false;
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

/// Sind alle Demo-Aussagen belegt? Dazu gehört, dass **jeder** Kern tickt — ein Kern, der
/// zwar bootet, aber keinen Timer-Interrupt bekommt, würde sonst unbemerkt bleiben.
fn all_done() -> bool {
    let workers = (0..NWORKERS).all(|i| WORKER_ROUNDS[i].load(Ordering::Relaxed) >= WORK_TARGET);
    let cores = (0..system::num_cores()).all(|c| hal::timer::ticks(c) > 0);
    let ring3 = USER_SYSCALLS.load(Ordering::Relaxed) > 0 && system::el0_fault_count() > 0;
    workers && IPC_DONE.load(Ordering::Acquire) && cores && ring3
}

/// Bericht + Abschaltung (das Testskript wertet die Marker aus).
fn report_and_off() -> ! {
    let ticks = hal::timer::ticks(0);
    let mut all_tick = true;
    for c in 0..system::num_cores() {
        let t = hal::timer::ticks(c);
        println!("sched   : core {c} ticks={t}");
        all_tick &= t > 0;
    }
    let rounds: [u64; NWORKERS] = core::array::from_fn(|i| WORKER_ROUNDS[i].load(Ordering::Relaxed));
    println!(
        "sched   : Worker-Runden {rounds:?} (jeder >= {WORK_TARGET} -> Timer verdraengt sie gegeneinander)"
    );
    let sched_ok = ticks > 0 && all_tick && rounds.iter().all(|&r| r >= WORK_TARGET);
    println!("sched   : {}", if sched_ok { "ALL PASS" } else { "FAILURES" });

    let ipc = IPC_RESULT.load(Ordering::Acquire);
    println!("ipc     : CALL(21) ueber Endpoint-Cap -> {ipc} (erwartet 42)");
    println!("ipc     : {}", if ipc == 42 { "ALL PASS" } else { "FAILURES" });

    let syscalls = USER_SYSCALLS.load(Ordering::Relaxed);
    let faults = system::el0_fault_count();
    println!("ring3   : Ring-3-Thread machte {syscalls} Syscalls; abgefangene Ring-3-Faults: {faults}");
    let ring3_ok = syscalls > 0 && faults > 0 && system::el0_syscall_seen();
    println!(
        "ring3   : {} (Ring-3-Thread laeuft + syscallt; Zugriff auf Kernel-Speicher faultet, Thread beendet, Kernel laeuft weiter)",
        if ring3_ok { "ALL PASS" } else { "FAILURES" }
    );

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
pub fn run(multiboot_info: u64) -> ! {
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
    let ram_end = match ram_end_from_multiboot(multiboot_info) {
        Some(e) => {
            println!("mbi     : Speicherplan gelesen -> RAM bis {:#x} ({} MiB)", e, e >> 20);
            e
        }
        None => {
            println!("mbi     : kein Speicherplan -> Rueckfall {} MiB", RAM_END_FALLBACK >> 20);
            RAM_END_FALLBACK
        }
    };
    let free_base = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    system::init_mem(free_base, ram_end);
    println!("mem     : freies RAM [{free_base:#x}, {ram_end:#x})");
    crate::selftest::run();

    // --- Kernel-Kern: Hooks, Tabellen, Scheduler ---
    system::set_hooks();
    // Kernzahl aus der ACPI-MADT (x86-Gegenstück zu den `/cpus`-Knoten des DTB).
    let cpus = hal::acpi::cpus();
    let ncpu = cpus.as_ref().map(|c| c.count()).unwrap_or(1);
    println!("acpi    : {ncpu} CPU(s) laut MADT");
    if let Some((ecam, b0, b1)) = hal::acpi::pci_ecam() {
        println!("acpi    : PCI-ECAM @ {ecam:#x}, Busse {b0}..{b1}");
    }
    let (nc, nthreads, per_core, tbl) = system::configure(ncpu);
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
    system::init_core();
    if !spawn_demo() {
        println!("bringup : FAILURES (Demo-Aufbau fehlgeschlagen)");
        hal::power::system_off();
    }
    println!("bringup : 3 Worker + 2 PDs (IPC-Server/Client) eingeplant");

    // --- Sekundärkerne starten (INIT-SIPI-SIPI, s. `hal::power`) ---
    let mut online = 1usize;
    if let Some(list) = cpus.as_ref() {
        let boot_id = hal::cpu::core_id() as u8;
        for i in 0..list.count() {
            let Some(id) = list.id(i) else { continue };
            if id == boot_id {
                continue;
            }
            let Some(stack) = system::alloc(AP_STACK_BYTES, 4096) else {
                break;
            };
            let top = stack.base() + stack.len();
            if hal::power::cpu_on(id as u64, ap_entry as *const () as u64, top)
                == hal::power::SUCCESS
            {
                online += 1;
            } else {
                println!("smp     : CPU {id} hat sich nicht gemeldet");
            }
        }
    }
    println!("smp     : {online} von {nc} Kern(en) online");

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
