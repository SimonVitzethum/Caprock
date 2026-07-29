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
use sel4lake_hal::{self as hal, print, println, syscall::invoke};
use sel4lake_mem::Rights;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Rückfall-RAM-Obergrenze, falls der Bootloader keinen Speicherplan mitgibt.
const RAM_END_FALLBACK: u64 = 128 * 1024 * 1024;

/// Zeitscheibe (Hz) — wie auf aarch64.
const TICK_HZ: u64 = 100;

// --- Telemetrie der Demo-Threads ------------------------------------------------------------

#[cfg(feature = "selftest")]
const NWORKERS: usize = 3;
/// So viele Runden muss jeder Worker schaffen, damit „Präemption läuft" belegt ist.
#[cfg(feature = "selftest")]
const WORK_TARGET: u64 = 3;
#[cfg(feature = "selftest")]
static WORKER_ROUNDS: [AtomicU64; NWORKERS] = [const { AtomicU64::new(0) }; NWORKERS];
#[cfg(feature = "selftest")]
static IPC_RESULT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static IPC_DONE: AtomicBool = AtomicBool::new(false);

/// Worker: zählt Runden. Da er **nie** freiwillig abgibt, beweist steigender Fortschritt
/// aller Worker, dass der Timer-Interrupt sie gegeneinander verdrängt.
#[cfg(feature = "selftest")]
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
#[cfg(feature = "selftest")]
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
#[cfg(feature = "selftest")]
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
#[cfg(feature = "selftest")]
static USER_SYSCALLS: AtomicU64 = AtomicU64::new(0);

/// Ring-3-Arbeiter: ruft den Kernel per Syscall (`SYS_YIELD`) und zählt die Runden.
#[link_section = ".user_text"]
#[cfg(feature = "selftest")]
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
#[cfg(feature = "selftest")]
extern "C" fn ring3_intruder(_arg: usize) -> ! {
    // SAFETY(-Absicht): Genau dieser Zugriff SOLL fehlschlagen. Das Kernel-Image liegt bei
    // 1 MiB in supervisor-only Seiten; ein Ring-3-Lesezugriff dorthin muss #PF auslösen.
    unsafe {
        let v = core::ptr::read_volatile(0x10_0000 as *const u64);
        core::ptr::write_volatile(0x20_0000 as *mut u64, v); // nie erreicht
    }
    loop {}
}

// --- Isolierte Adressräume (ext-33) ---------------------------------------------------------
//
// Eine **isolierte** PD bekommt einen eigenen Adressraum, in dem NUR ihre eigene Region
// user-zugänglich ist. Der Test ist derselbe wie auf aarch64 (`vspace`): zwei Ring-3-Threads
// lesen **dieselbe** Adresse in fremdem RAM — der SAS-Thread darf (gemeinsamer Adressraum),
// der isolierte muss faulten.
/// Prüfadresse in fremdem User-RAM (wird vom Kernel beschrieben, s. `spawn_demo`).
#[link_section = ".user_data"]
#[cfg(feature = "selftest")]
static ISO_PROBE_ADDR: AtomicU64 = AtomicU64::new(0);
/// Der SAS-Thread konnte lesen (und meldet den Wert).
#[link_section = ".user_data"]
#[cfg(feature = "selftest")]
static SAS_READ_OK: AtomicU64 = AtomicU64::new(0);
/// Erwarteter Wert an der Prüfadresse.
#[cfg(feature = "selftest")]
const PROBE_MAGIC: u64 = 0x5E14_1A4E_0BED_C0DE;

/// SAS-Ring-3-Thread: liest die Prüfadresse — im gemeinsamen Adressraum ist das erlaubt.
#[link_section = ".user_text"]
#[cfg(feature = "selftest")]
extern "C" fn sas_probe(_arg: usize) -> ! {
    let a = ISO_PROBE_ADDR.load(Ordering::Relaxed);
    // SAFETY: `a` zeigt auf eine vom Kernel angelegte, im SAS-Modell user-lesbare RAM-Zelle.
    let v = unsafe { core::ptr::read_volatile(a as *const u64) };
    SAS_READ_OK.store(v, Ordering::Relaxed);
    loop {
        // SAFETY: freigegebener Syscall-Vektor (s. `ring3_worker`).
        unsafe {
            core::arch::asm!("int 0x80", in("rax") 0u64, in("rdi") 0u64,
                             lateout("rax") _, lateout("rdi") _, clobber_abi("sysv64"));
        }
    }
}

/// Isolierter Ring-3-Thread: liest **dieselbe** Adresse. In seinem eigenen Adressraum ist sie
/// nicht user-gemappt -> #PF -> der Kernel beendet ihn.
#[link_section = ".user_text"]
#[cfg(feature = "selftest")]
extern "C" fn iso_probe(_arg: usize) -> ! {
    let a = ISO_PROBE_ADDR.load(Ordering::Relaxed);
    // SAFETY(-Absicht): Genau dieser Zugriff SOLL fehlschlagen — er liegt außerhalb der Region
    // dieser PD und ist in ihrem Adressraum nicht user-zugänglich.
    unsafe {
        let v = core::ptr::read_volatile(a as *const u64);
        core::ptr::write_volatile(a as *mut u64, v); // nie erreicht
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
#[cfg(feature = "selftest")]
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

    // Isolierter Adressraum vs. SAS: beide lesen dieselbe fremde Adresse.
    let Some(probe) = system::alloc(4096, 4096) else {
        return false;
    };
    // SAFETY: frisch allozierte, identity-gemappte RAM-Seite des Kernels.
    unsafe { core::ptr::write_volatile(probe.base() as *mut u64, PROBE_MAGIC) };
    ISO_PROBE_ADDR.store(probe.base(), Ordering::Relaxed);
    if system::spawn_user(sas_probe as *const () as usize, 0, system::IDLE_PRIO).is_none() {
        return false;
    }
    if system::spawn_isolated(iso_probe as *const () as usize, 0, system::IDLE_PRIO).is_none() {
        println!("iso     : spawn_isolated fehlgeschlagen");
        return false;
    }

    #[cfg(feature = "selftest")]
    {
        // Cache-Partitionierung (todo A1): zwei PDs mit disjunkten Farbsaetzen. Der Test baut sie
        // sofort wieder ab -- geprueft wird die Zuteilung, nicht ihr Programm.
        let c = crate::colors::run_color(iso_probe as *const () as usize, system::IDLE_PRIO);
        if !c.usable {
            println!(
                "color   : SKIP -- {} Farbe(n) gemessen, unter 2 gibt es nichts zu trennen (QEMU meldet \
                 ohne echtes CPU-Modell keine Cache-Geometrie; mit -cpu Skylake-Client sind es 256)",
                c.colors
            );
        } else {
            println!(
                "color   : {} Farben, {} Partitionen, Region {} KiB · in_mask={} kernelseite={} disjunkt={} \
                 uebergross_abgewiesen={} bilanz={}",
                c.colors,
                crate::colors::PARTITIONS,
                crate::colors::region_bytes() / 1024,
                c.in_mask as u8,
                c.kernel_side_in_mask as u8,
                c.disjoint as u8,
                c.oversize_refused as u8,
                c.balanced as u8
            );
            println!(
                "color   : {} (zwei isolierte PDs teilen sich keine Cache-Farbe)",
                if c.ok { "ALL PASS" } else { "FAIL" }
            );
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

/// Badge, mit dem sich der Root-Task meldet (`programs/trusted/init`, `ROOT_BADGE`).
#[cfg(feature = "selftest")]
const ROOT_BADGE: u64 = 1 << 32;
/// Badge, mit dem sich `hello` meldet (`programs/userland/hello`, `HELLO_BADGE`).
#[cfg(feature = "selftest")]
const HELLO_BADGE: u64 = 0x4845_4C4F;
/// A-3.1: `SYS_CDELETE` hat die Loader-Cap geloescht **und** die Autoritaet war danach weg.
#[cfg(feature = "selftest")]
const CDELETE_GONE_BADGE: u64 = 1 << 33;
/// A-3.1: ein Cap mit abgeleiteten Kopien wird abgewiesen und bleibt benutzbar.
#[cfg(feature = "selftest")]
const CDELETE_CHILDREN_BADGE: u64 = 1 << 34;

/// Der akkumulierte Badge der Root-Notification (`0`, wenn keine endowt wurde).
#[cfg(feature = "selftest")]
fn root_badge() -> u64 {
    crate::loader::root_notification()
        .map(system::notification_pending)
        .unwrap_or(0)
}

/// Hat der Root-Task gelaufen **und** von sich aus die uebrige Startmenge geladen?
///
/// Beides muss belegt sein, und zwar getrennt: dass ein geladenes Programm laeuft, sagt noch
/// nichts darueber, ob seine Loader-Cap traegt. Erst das zweite Badge zeigt, dass ein
/// **Userland**-Programm ein weiteres Programm gestartet hat -- das ist die Aussage, wegen der
/// Stufe 1 des Plans existiert.
#[cfg(feature = "selftest")]
fn root_chain_done() -> bool {
    let b = root_badge();
    b & ROOT_BADGE != 0 && b & HELLO_BADGE == HELLO_BADGE
}

/// A-3.1: beide Ausgänge von `SYS_CDELETE` aus Ring 3 belegt — der erfolgreiche **und** der
/// abgelehnte. Ein Löschpfad, von dem nur der Erfolgsfall geprüft ist, sagt nichts darüber, ob er
/// im Zweifel zu viel löscht.
#[cfg(feature = "selftest")]
fn cdelete_done() -> bool {
    let b = root_badge();
    b & CDELETE_GONE_BADGE != 0 && b & CDELETE_CHILDREN_BADGE != 0
}

/// Sind alle Demo-Aussagen belegt? Dazu gehört, dass **jeder** Kern tickt — ein Kern, der
/// zwar bootet, aber keinen Timer-Interrupt bekommt, würde sonst unbemerkt bleiben.
///
/// **`archive` = liegt überhaupt ein Boot-Archiv vor** (B-1.6). Ohne Archiv gibt es keinen
/// Root-Task, also können [`root_chain_done`] und [`cdelete_done`] **prinzipiell** nicht wahr
/// werden — `test-qemu-x86.sh` bootet genau so, absichtlich (das Archiv prüft die Lade-Suite).
/// Standen sie trotzdem in der Bedingung, wurde `all_done()` dort **nie** wahr: der Bericht fiel
/// jedes Mal aus der Notbremse unten, also nach 50 Mio. Spins statt nach dem letzten Beleg. Damit
/// war jede knappe Aussage ein Rennen gegen einen Zähler — beobachtet am `iso`-Test, der bei
/// gleichem Bau mal `2x` faultete und mal `0x`, je nachdem, ob die Probe bis zum Ablauf drankam.
///
/// Eine Aussage, die diese Konfiguration nicht belegen **kann**, darf deshalb nicht dauerhaft
/// *verlangt* werden — sie ist nicht anwendbar. Gemeldet wird sie trotzdem, und zwar mit Grund
/// (`root : FAILURES (NoArchive)`); die Suite nimmt genau das seit B-1.5 ausdrücklich ab. Was
/// hier NICHT passiert: die Anforderung abschwächen, wenn ein Archiv da ist. Dann gilt sie voll.
#[cfg(feature = "selftest")]
fn all_done(archive: bool) -> bool {
    let workers = (0..NWORKERS).all(|i| WORKER_ROUNDS[i].load(Ordering::Relaxed) >= WORK_TARGET);
    let cores = (0..system::num_cores()).all(|c| hal::timer::ticks(c) > 0);
    let ring3 = USER_SYSCALLS.load(Ordering::Relaxed) > 0 && system::el0_fault_count() > 0;
    let iso = SAS_READ_OK.load(Ordering::Relaxed) == PROBE_MAGIC && system::iso_fault_count() > 0;
    let root = !archive || (root_chain_done() && cdelete_done());
    workers && IPC_DONE.load(Ordering::Acquire) && cores && ring3 && iso && root
}

/// Bericht + Abschaltung (das Testskript wertet die Marker aus).
#[cfg(feature = "selftest")]
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

    let sas = SAS_READ_OK.load(Ordering::Relaxed);
    let isof = system::iso_fault_count();
    println!(
        "iso     : SAS-Thread las {sas:#x} (erwartet {PROBE_MAGIC:#x}); isolierter Thread faultete {isof}x an derselben Adresse"
    );
    let iso_ok = sas == PROBE_MAGIC && isof > 0;
    println!(
        "iso     : {} (eigener Adressraum je PD: dieselbe Adresse ist fuer SAS lesbar, fuer die isolierte PD nicht)",
        if iso_ok { "ALL PASS" } else { "FAILURES" }
    );

    let b = root_badge();
    println!(
        "root    : Notification-Badge {b:#x} (Root-Task lief: {}; er selbst hat 'hello' nachgeladen: {})",
        b & ROOT_BADGE != 0,
        b & HELLO_BADGE == HELLO_BADGE
    );
    println!(
        "root    : {} (A-2.1: ein extern gebautes, aus dem signierten Manifest ausgewaehltes Programm laeuft -- und laedt seinerseits ueber SEINE Loader-Cap ein weiteres)",
        if root_chain_done() { "ALL PASS" } else { "FAILURES" }
    );
    println!(
        "cdelete : Loader-Cap geloescht und Autoritaet danach weg: {}; Cap mit abgeleiteten Kopien abgewiesen und weiter benutzbar: {}",
        b & CDELETE_GONE_BADGE != 0,
        b & CDELETE_CHILDREN_BADGE != 0
    );
    println!(
        "cdelete : {} (A-3.1: SYS_CDELETE aus Ring 3 -- beide Ausgaenge belegt, nicht nur der erfolgreiche)",
        if cdelete_done() { "ALL PASS" } else { "FAILURES" }
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
    crate::colors::report();

    // --- Speicher + arch-neutrale Selbsttests (identisch zu aarch64) ---
    let mbi = super::multiboot::MultibootInfo::new(multiboot_info);
    let ram_end = match mbi.and_then(|m| m.ram_end()) {
        Some(e) => {
            println!("mbi     : Speicherplan gelesen -> RAM bis {:#x} ({} MiB)", e, e >> 20);
            e
        }
        None => {
            println!("mbi     : kein Speicherplan -> Rueckfall {} MiB", RAM_END_FALLBACK >> 20);
            RAM_END_FALLBACK
        }
    };
    // --- Multiboot-Module (A-1.1) ---
    //
    // Die Startmenge kommt auf x86 als Multiboot-Module. Zwei Dinge muessen VOR der ersten
    // Allokation stehen, nicht danach:
    //
    // 1. Die Modulbereiche duerfen dem `PhysAllocator` nie als frei gemeldet werden. Sonst
    //    ueberschreibt die erste Allokation genau das Archiv, das der Kernel gleich lesen will --
    //    und zwar lautlos, weil ein ueberschriebenes Archiv einfach "kein gueltiges Archiv" ergibt.
    // 2. Der Loader muss wissen, WO das Archiv liegt. Auf ARM ist das eine Verabredung mit dem
    //    Testaufbau; hier sagt es der Bootloader erst zur Laufzeit.
    let mut mods = [super::multiboot::Module { start: 0, end: 0 }; super::multiboot::MAX_MODULES];
    let nmods = mbi.map(|m| m.modules(&mut mods)).unwrap_or(0);
    let claimed = mbi.map(|m| m.mods_claimed()).unwrap_or(0);
    if nmods > 0 {
        print!("mbi     : {nmods} Modul(e):");
        for m in &mods[..nmods] {
            print!(" [{:#x}..{:#x}) {} KiB", m.start, m.end, m.len() >> 10);
        }
        println!();
    }
    if claimed > nmods {
        // Eine stille Kuerzung sieht in jeder spaeteren Auswertung aus wie Vollstaendigkeit.
        println!(
            "mbi     : HINWEIS Bootloader meldet {claimed} Module, ausgewertet werden {nmods} (Grenze {} bzw. leere Eintraege verworfen)",
            super::multiboot::MAX_MODULES
        );
    }
    // Modul 0 ist das Boot-Archiv (Verabredung mit `test-qemu-x86.sh`: `-initrd boot-archive.bin`).
    if nmods > 0 {
        crate::loader::set_archive_span(mods[0].start, mods[0].len());
    }
    let free_base = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    /*
     * Den freien Speicher **absichtlich zerstueckelt** uebergeben, statt als einen Block.
     *
     * Fuer die Kern-Uebergabe an Linux (Variante B) kommt der Speicher als Sammlung dessen,
     * was der Wirt hergibt -- dort hoechstens 4 MiB am Stueck. Ob der Kernel damit umgehen
     * kann, ist keine Frage der Absicht, sondern eine Eigenschaft, die gelten muss; und ein
     * Pfad, der im Test nie zerstueckelten Speicher sieht, belegt sie nicht. Also sieht der
     * regulaere QEMU-Lauf ihn immer: dieselbe Menge Speicher, nur in acht Bereichen.
     *
     * Luecken entstehen dabei keine -- die Bereiche stossen aneinander. Der Allokator
     * verschmilzt sie beim Freigeben ohnehin wieder; geprueft wird der *Eingang*.
     */
    const SPLIT: u64 = 8;
    let total = ram_end - free_base;
    let chunk = (total / SPLIT) & !0xfff;
    let mut bi = super::bootinfo::HandoverInfo::empty();
    bi.ram_top = ram_end;
    let mut gross = 0u64; // Summe vor dem Ausschnitt
    for i in 0..SPLIT {
        let base = free_base + i * chunk;
        let len = if i == SPLIT - 1 { ram_end - base } else { chunk };
        gross += len;
        // Die Modulbereiche werden hier **ausgeschnitten**, nicht spaeter markiert: was nie als
        // frei gemeldet wurde, kann auch nicht vergeben werden (A-1.1).
        super::multiboot::subtract_holes(base, len, &mods[..nmods], &mut |b, l| {
            bi.push_region(b, l);
        });
    }
    // Wieviel durch den Ausschnitt wegfiel -- als Zahl, nicht als Gefuehl.
    let net = bi.mem_regions().iter().map(|r| r.len).sum::<u64>();
    let carved = gross - net;
    // Dieselbe Pruefung, die der Uebergabeweg vor jeder Benutzung faehrt -- damit sie im
    // regulaeren Lauf auch tatsaechlich einmal ausgefuehrt wird.
    if !bi.valid() {
        println!("mem     : FAILURES (HandoverInfo unplausibel)");
        hal::power::system_off();
    }
    let mut regions = [(0u64, 0u64); super::bootinfo::MAX_REGIONS];
    for (i, r) in bi.mem_regions().iter().enumerate() {
        regions[i] = (r.base, r.len);
    }
    system::init_mem_regions(&regions[..bi.n_regions as usize], bi.ram_top);
    println!(
        "mem     : {} Bereiche a ~{} MiB ueber HandoverInfo (Quelle {}), verworfen={}",
        bi.n_regions,
        chunk >> 20,
        if bi.source == super::bootinfo::BootSource::Handover { "Kern-Uebergabe" } else { "Multiboot" },
        system::mem_regions_dropped()
    );
    println!("mem     : freies RAM [{free_base:#x}, {ram_end:#x})");
    if carved > 0 {
        println!("mem     : {carved} Byte fuer Multiboot-Module ausgeschnitten (vor der ersten Allokation)");
    }
    #[cfg(feature = "selftest")]
    {
        // Das Ausschneiden traegt eine Sicherheitsaussage; der reale Lauf sieht davon nur EINEN
        // Fall. Die Grenzfaelle werden eingespeist (s. `multiboot::selftest`).
        let ok = super::multiboot::selftest();
        println!(
            "mbmod   : {} (Modulbereiche werden aus der Freiliste ausgeschnitten: Rand, Ueberlappung, unsortiert, Vollabdeckung)",
            if ok { "ALL PASS" } else { "FAILURES" }
        );
    }
    crate::loader::probe(); // was liegt im Archiv? (A-1.1: die Quelle ist jetzt auch auf x86 da)
    crate::loader::manifest_report(); // A-1.2..A-1.4: wer bekommt welche Autoritaet?
    #[cfg(feature = "selftest")]
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
    // --- PCI-Enumeration (die Firmware hat BARs/Bridges bereits konfiguriert) ---
    let mut ndev = 0usize;
    hal::pcie::dump_devices(&mut |bus, dev, ven, did, class| {
        ndev += 1;
        println!("pci     : {bus:02x}:{dev:02x}.0 {ven:04x}:{did:04x} class={class:#08x}");
    });
    let rng = hal::pcie::find(hal::pcie::VIRTIO_VENDOR, &hal::pcie::VIRTIO_RNG_DEVICES);
    match rng {
        Some(d) => println!(
            "pci     : {ndev} Geraet(e); virtio-rng gefunden (RID {:#06x}, MMIO-BAR {:#x}, Bus-Master {})",
            d.rid(),
            d.bars.iter().copied().find(|&b| b != 0).unwrap_or(0),
            hal::pcie::bus_master_enabled(&d)
        ),
        None => println!("pci     : {ndev} Geraet(e); kein virtio-rng"),
    }
    println!("pci     : {}", if ndev > 0 { "ALL PASS" } else { "FAILURES" });

    // --- IOMMU (VT-d): Bring-up mit Default-Block ---
    // Über denselben Weg wie auf ARM: der Kernel kennt nur das `DmaEnforcer`-Trait.
    let up = system::dma_enforcer_init();
    let iommu_ok = if hal::vtd::present() {
        println!(
            "iommu   : VT-d Version {:#x} CAP {:#x} (Root-Tabelle mit lauter 'not present' = Default-Block)",
            hal::vtd::version(),
            hal::vtd::cap()
        );
        // Schritt 1 des VT-d-Aufbaus: Fähigkeiten EINMAL lesen, protokollieren, und jede
        // spätere Bit-Entscheidung daraus ableiten statt an der Verwendungsstelle. Die Lektion
        // stammt von der ARM-Seite (`STE.S1STALLD` war nur unter einer Bedingung zulässig, und
        // die Einheit übersetzte deshalb gar nicht, ohne dass es jemand sagte).
        if let Some(c) = hal::vtd::VtdCaps::read() {
            println!(
                "vtdcaps : SAGAW {:#x} -> gewaehlt {} Bit ({} Level), MGAW {} Bit, Domains {}, CM={} RWBF={} ECAP.C={} QI={} IR={} SC={} ScalableMode={}",
                c.sagaw, c.agaw_bits, c.agaw_levels, c.mgaw_bits, c.num_domains,
                c.caching_mode, c.rwbf, c.coherent_walk, c.queued_invalidation,
                c.interrupt_remapping, c.snoop_control, c.scalable_mode
            );
            println!(
                "vtdcaps : Fault-Recording {} Register @ +{:#x}, Overflow(FSTS.PFO)={}; Eingangsgrenze {:#x}; brauchbar={}",
                c.num_fault_regs, c.fault_reg_offset, hal::vtd::fault_overflow(),
                c.input_limit(), c.usable()
            );
            // Was der Enforcer NICHT hat, gehoert genauso ins Log wie das, was er hat.
            println!(
                "vtdcaps : {} (Schritt 1: Faehigkeiten gelesen; Zuteilung je Gruppe, IR/CFI und RMRR-Ausschluss stehen aus -> attach liefert weiterhin None)",
                if c.usable() { "ALL PASS" } else { "FAILURES" }
            );
        }
        // Schritt 2: DMAR-Auswertung + Gruppenbildung. Der Selbsttest laeuft gegen eine
        // EINGESPEISTE Tabelle/Topologie -- auf dem realen Aufbau (flach, keine RMRR) wuerden
        // Ausschlusspfad und Gruppenfaelle nie ausgefuehrt.
        #[cfg(feature = "selftest")]
        {
            let st = super::dmar_selftest::run();
            println!(
                "vtdgrp  : Selbsttest: parse={} Catch-all-zuletzt={} Bridge-Scope-Subhierarchie={} Gruppen={} Alias-Mengen={} RMRR-ausgeschlossen={} Firmware-Muell-abgefangen={} Oracle={}",
                st.parse_ok, st.catch_all_last, st.bridge_scope_subtree, st.groups_ok,
                st.alias_ok, st.rmrr_excluded, st.malformed_caught, st.audit
            );
            super::dmar_selftest::report_real();
            println!("vtdgrp  : {}", if st.ok() { "ALL PASS" } else { "FAILURES" });
        }
        let inv = hal::vtd::invalidate_context_cache();
        println!(
            "iommu   : Uebersetzung aktiv={} (GSTS.TES), Kontext-Cache-Invalidierung quittiert={inv}, dma_audit={}",
            system::dma_enforcer().is_active(),
            system::dma_enforcer().audit()
        );
        up && system::dma_enforcer().is_active() && inv && system::dma_enforcer().audit() == 0
    } else {
        println!("iommu   : keine ACPI-DMAR -> Plattform ohne IOMMU");
        false
    };
    println!("iommu   : {}", if iommu_ok { "ALL PASS" } else { "SKIP/FAILURES" });
    let (nc, nthreads, per_core, tbl) = system::configure(ncpu);
    println!(
        "apic    : {} (LAPIC-ID {}); x2APIC adressiert 32-Bit-IDs, xAPIC nur 8 -> 255 Kerne",
        if hal::intc::x2apic_active() { "x2APIC (MSR-Pfad)" } else { "xAPIC (MMIO-Pfad)" },
        hal::intc::lapic_id()
    );
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

    // --- Zyklenzaehler (Stufe 1) ---
    //
    // Das Primitiv, das sowohl die per-Thread-Abrechnung als auch die Messung kritischer
    // Sektionen braucht. Geprueft wird hier nur, was ohne weitere Infrastruktur pruefbar ist:
    // dass der Zaehler laeuft, monoton ist, plausibel kalibriert und -- getrennt davon -- ob er
    // invariant ist. Der letzte Punkt ist keine Kosmetik: ein nicht-invarianter TSC aendert
    // seine Rate mit dem P-State, und Zeitdifferenzen waeren dann keine Zeit, sondern eine
    // Funktion des Taktverhaltens. Das sieht im Messlauf plausibel aus.
    {
        let inv = hal::timer::invariant_tsc();
        let hz = hal::timer::cycles_per_sec();
        let c0 = hal::timer::cycles();
        let mut spin = 0u64;
        while hal::timer::cycles().wrapping_sub(c0) < hz / 1000 {
            spin += 1; // ~1 ms
        }
        let c1 = hal::timer::cycles();
        let d = c1.wrapping_sub(c0);
        // Aufloesung: die kleinste messbare Differenz zweier aufeinanderfolgender Stempel.
        let a = hal::timer::cycles();
        let b = hal::timer::cycles();
        let grain = b.wrapping_sub(a);
        println!(
            "cycles  : invariant-TSC={inv} {} MHz; 1-ms-Fenster = {d} Zyklen ({spin} Iterationen); Aufloesung {grain} Zyklen (Tick-Uhr: {} Zyklen)",
            hz / 1_000_000,
            hz / TICK_HZ
        );
        // Invarianz ist **Telemetrie, keine Bestehensbedingung** -- dieselbe Mittelstellung wie
        // bei den undeklarierten Geraete-Adressbreiten. TCG unterstuetzt `invtsc` nicht
        // ("TCG doesn't support requested feature: CPUID[80000007h].EDX.invtsc"), die Emulation
        // kann die Eigenschaft also gar nicht zusagen. Sie zur Bedingung zu machen hiesse
        // entweder, den Test auf dieser Plattform dauerhaft rot zu lassen, oder die Pruefung
        // wegzulassen -- und Letzteres waere die stillschweigende Annahme, die hier gerade
        // vermieden wird. Gefuehrt und ausgewiesen: auf einer Plattform ohne Zusage sind
        // Zyklenzahlen ein Anhaltspunkt, keine Abrechnungsgrundlage.
        if !inv {
            // Wichtig: das ist eine Aussage ueber DIESE Plattform, nicht ueber den Entwurf.
            // Unter KVM mit `-cpu host,+invtsc` meldet dieselbe Pruefung `true`, und die
            // Zielhardware (Zen/EPYC, seit Zen durchgehend) hat Invariant TSC ohnehin. Ohne
            // diesen Zusatz verfestigt sich sonst die Lesart "wir koennen nicht abrechnen",
            // obwohl die Einschraenkung am Emulator haengt.
            println!("cycles  : HINWEIS invariant-TSC nicht zugesagt (TCG kann es nicht) -> Zyklenwerte hier indikativ; unter KVM/-cpu host,+invtsc und auf Zen/EPYC ist die Zusage vorhanden");
        }
        let ok = hz > 1_000_000 && d >= hz / 2000 && d <= hz / 250 && grain > 0;
        println!(
            "cycles  : {} (serialisierender Zeitstempel: rdtscp+lfence, gegen den PIT kalibriert, auf EINEM Kern gemessen; Invarianz gefuehrt statt angenommen)",
            if ok { "ALL PASS" } else { "FAILURES" }
        );
    }

    // --- Arch-neutrale DMA-Tests (ext-38) ---
    //
    // Dieselben Funktionen, die der ARM-Lauf ruft -- nicht nachgebaute. Bis hierher lagen sie in
    // `threads/mod.rs`, und das Modul ist aarch64-only: auf x86 haben sie nicht geskippt, es gab
    // sie nicht. Ein Test, den es auf einer Architektur nicht gibt, kann dort auch nicht gruen
    // werden; das Abnahmekriterium war so nicht einloesbar.
    #[cfg(feature = "selftest")]
    {
        let live_rid = hal::pcie::find(hal::pcie::VIRTIO_VENDOR, &hal::pcie::VIRTIO_RNG_DEVICES)
            .map(|d| d.rid())
            .unwrap_or(0);
        let w = crate::dmatests::run_dmawin(live_rid);
        println!("dmawin  : 32-Bit-Geraet-abgewiesen={} Fenster-voll-abgewiesen={} Kontext-danach-intakt={} balanciert={} Geraete-ohne-deklarierte-Adressbreite={}",
            w.narrow, w.exhausted, w.intact, w.balanced, w.undeclared);
        println!("dmawin  : {}", if w.ok { "ALL PASS" } else { "FAILURES" });
        let tk = crate::dmatests::run_dmatok(live_rid);
        println!("dmatok  : attach-installierte-Uebersetzung={} delete-ohne-detach-baut-ab={} Region-danach-frei={} unbestaetigte-Stilllegung-bleibt-pending={} Audit-Code-7-haelt={}",
            tk.attached, tk.tears, tk.freed, tk.pending, tk.audit7);
        println!("dmatok  : {}", if tk.ok { "ALL PASS" } else { "FAILURES" });
    }

    #[cfg(feature = "selftest")]
    {
        if !spawn_demo() {
            println!("bringup : FAILURES (Demo-Aufbau fehlgeschlagen)");
            hal::power::system_off();
        }
        println!("bringup : 3 Worker + 2 PDs (IPC-Server/Client) eingeplant");
    }

    // --- Root-Task (A-2.1) ---
    //
    // Steht BEWUSST ausserhalb von `selftest`: das hier ist die Aufgabe des Kernels, nicht seine
    // Pruefung. Ohne diesen Aufruf ist `--no-default-features` ein leerer Kernel (todo F2), und
    // genau das war der Grund, warum `selftest` bis hierher in `default` bleiben musste.
    let root_ok = crate::loader::start_root_task_reported();
    let _ = root_ok;

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

    hal::mmu::seal_cache_granule(); // alle Kerne haben gemeldet (auf x86 wirkungslos)

    // Ab hier schedult der Timer-Interrupt präemptiv; dieser Kontext ist der Idle-Thread.
    hal::cpu::local_irq_enable();
    #[cfg(feature = "selftest")]
    {
        // Einmal, nicht je Runde: `read_archive()` parst den Multiboot-Modulbereich, und diese
        // Schleife dreht Millionen Mal. Die Anwesenheit eines Archivs ändert sich zur Laufzeit
        // ohnehin nicht — sie steht mit dem Bootvorgang fest.
        let archive = crate::loader::read_archive().is_some();
        let mut spins: u64 = 0;
        loop {
            system::reap();
            if all_done(archive) {
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
    // OHNE `selftest` bleibt genau das hier uebrig, und das ist die ehrliche Aussage von todo F2:
    // der Kernel hat derzeit **keinen Nicht-Test-Zweck**. Es gibt kein Boot-Archiv auf x86 und
    // keinen Root-Task, dem die Wurzel-Caps uebergeben wuerden. Das Gating liefert also keinen
    // schlankeren Kernel, sondern einen leeren -- deshalb steht `selftest` weiterhin in `default`.
    // Diese Konfiguration wird trotzdem GEBAUT (test-qemu-x86.sh), damit sie nicht verrottet.
    #[cfg(not(feature = "selftest"))]
    {
        println!("bringup : Kernel-Kern steht; ohne Feature `selftest` gibt es keine Aufgabe (todo F2)");
        loop {
            system::reap();
            core::hint::spin_loop();
        }
    }
}
