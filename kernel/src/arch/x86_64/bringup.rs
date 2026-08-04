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

/// Z4a: die TIDs, die der Haltepunkt-Test braucht (`0` = keine).
#[cfg(feature = "selftest")]
static WORKER_TID0: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static IPC_SERVER_TID: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static FREEZE_OK: AtomicBool = AtomicBool::new(false);

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
        let Some(t) = system::spawn(worker as *const () as usize, i, system::IDLE_PRIO) else {
            return false;
        };
        if i == 0 {
            WORKER_TID0.store(t.to_raw(), Ordering::Release);
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
        // Gemeldet wird in `colors::report_color` — EINE Druckstelle fuer beide Hochlaufwege.
        crate::colors::report_color(&c);
        // B-4.2: die Streifenvergabe fuehrt Belegung -- erschoepft heisst FEHLSCHLAG, nicht
        // stille Wiederholung. Laeuft NACH `run_color` (das baut seine PDs sofort wieder ab) und
        // vor allem, was selbst Streifen belegt, also im Ruhezustand.
        STRIPE_ALLOC_OK.store(crate::colors::run_stripe_alloc(), Ordering::Release);
        // B-4.5: die WIRKUNG der Faerbung, nicht nur die Zuteilung. Traegt die Positivkontrolle
        // nicht (Maschine kann Verdraengung nicht zeigen, z. B. TCG), ist das ein SKIP und darf
        // die Abschlussbedingung nicht blockieren -- aber die Bilanz muss immer stimmen.
        let pp = crate::colors::run_prime_probe();
        crate::colors::report_prime_probe(&pp);
        PPROBE_OK.store(
            (!pp.ran || pp.balanced) && (!pp.sensitive || pp.ok),
            Ordering::Release,
        );
        // A-4.4: Versionssperre des Laders, beide Ausgaenge.
        IFACE_GATE_OK.store(crate::loader::run_iface_gate(), Ordering::Release);
        // A-4.2: der ruhende Punkt -- die Torlogik, alle Ausgaenge. Laeuft auf einem lokalen
        // Endpoint-Objekt, legt also keinen benutzten Endpoint still.
        QUIESCE_OK.store(system::run_quiesce(), Ordering::Release);
        // A-4.1: atomares Umbinden -- alle Ausgaenge, ebenfalls auf einem lokalen Objekt.
        REBIND_OK.store(system::run_rebind(), Ordering::Release);
        // D11: der Ueberlauf einer Endpoint-Warteschlange ist benannt, nicht still. Ebenfalls
        // auf einem lokalen Objekt -- ein benutzter Endpoint wuerde hier 32 Leichen behalten.
        EPFULL_OK.store(system::run_epfull(), Ordering::Release);
        // A-4.3: die Zustandsuebergabe, alle Ausgaenge. Auf einer EIGENEN Scratch-Region -- der
        // Test darf den Zustand, dessen Ueberleben `ckpt` belegt, nicht selbst anfassen.
        STATE_OK.store(system::run_state(), Ordering::Release);
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
    // Z4a: der IPC-Server steht spaeter in `RECV` -- er sieht von aussen ruhend aus und ist es
    // nicht. Genau daran wird die Absage geprueft.
    IPC_SERVER_TID.store(srv.to_raw(), Ordering::Release);

    // --- Z4 Stufe 2: das Subjekt bekommt einen UMFANG ------------------------------------------
    //
    // Ein Thread ohne PD hat einen leeren Cspace, und `classify_all` sagt dazu `Ok(())` -- was
    // „nichts spricht dagegen" heisst und nicht „es ist etwas dabei" (so steht es im Test von
    // `checkpoint.rs`). Ein Checkpoint ueber einen leeren Umfang belegte von Z4b genau nichts.
    //
    // Deshalb bekommt Worker 0 eine PD mit zwei Caps, und beide sind mit Absicht gewaehlt:
    //
    //   * eine **Speicher**-Cap -> wandert als `Region { len }`, ohne Physadresse. Die
    //     Zielmaschine legt sie hin, wo sie will, und faerbt neu.
    //   * eine **SchedContext**-Cap -> wandert MIT VORBEDINGUNG (`SameTickSemantics`). Ihre
    //     Zahlen sind Ticks, und ein Tick bedeutet auf einer Maschine ohne invarianten Zaehler
    //     etwas anderes (Z4f/B-5.1). Genau diese Vorbedingung steht spaeter im Checkpoint.
    //
    // **Am Ende von `spawn_demo`, nicht am Anfang:** die Farb- und Streifentests darueber messen
    // gegen eine Speicher-Baseline. Ein Test, der Speicher belegt, kippt baseline-empfindliche
    // Tests -- das hat dieses Projekt auf aarch64 schon bezahlt.
    if let (Some(mem), Some(pd)) = (system::alloc(4096, 4096), system::create_pd()) {
        if let (Ok(mcap), Ok(sc)) = (
            system::cap_install(mem),
            system::install_sched_context_cap(4, 10, Rights::RW),
        ) {
            if system::install_pd_cap(pd, 0, mcap) && system::install_pd_cap(pd, 1, sc) {
                system::bind_pd(pd, sel4lake_sched::ThreadId::from_raw(
                    WORKER_TID0.load(Ordering::Acquire),
                ));
                CKPT_PD.store(pd as u32 + 1, Ordering::Release);
            }
        }
    }
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

/// A-4.4: hat die Versionssperre die geaenderte Schnittstellenversion abgewiesen?
#[cfg(feature = "selftest")]
static IFACE_GATE_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// B-4.2: hat die Streifenvergabe den Erschoepfungsfall sauber abgewiesen?
#[cfg(feature = "selftest")]
static STRIPE_ALLOC_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// B-4.5: Prime+Probe. `true` heisst auch "nicht messbar" (SKIP) -- ein Fehlschlag ist nur ein
/// Lauf, in dem die Positivkontrolle TRAEGT und die Faerbung trotzdem nichts bewirkt.
static PPROBE_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.2: virtio-pci-Transport auf x86.
static VIRTIO_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.2, Teil 1: lief der Transport, solange die IOMMU noch nicht sperrte?
static VIRTIO_XPORT_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.2: virtio-blk (Sektor gelesen -- und nach dem VT-d-Aufbau nicht mehr).
static VBLK_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.2, virtio-blk Teil 1: kam der Sektor an, solange die IOMMU noch nicht sperrte?
static VBLK_READ_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.2: virtio-net (ARP-Anfrage raus, ARP-Antwort rein).
static VNET_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.1: hat eine geladene Treiber-PD ihr Geraet bedient?
/// **Welchen Dienst der KERNEL-Testclient anspricht** (A-5.4) — die `program_id` des Blockdienstes
/// aus dem Manifest der Lade-Suite.
///
/// Ein echter Client nennt seinen Dienst im **Manifest** (`service_id`, A-5.4); der Kernel-Test
/// hat keinen Manifest-Eintrag und muss die Wahl deshalb hier treffen. Dass er sie **treffen
/// muss**, ist der Punkt: seit es zwei Treiber-Dienste gibt, liefert `driver_service()` bei
/// Mehrdeutigkeit bewusst `None` statt „den zuletzt geladenen" — und genau daran ist dieser Test
/// beim ersten Lauf mit zwei Treibern aufgelaufen, was richtig war.
///
/// Der Kernel-**Lader** kennt diese Zahl nicht; sie steht in einem Testpfad, nicht in
/// `loader::reload_driver`.
const TEST_BLK_SERVICE_ID: u32 = 3;
/// Die Treiber-PD, deren Zuteilung der Test misst — dieselbe Komponente.
const TEST_BLK_PROGRAM_ID: u32 = 3;


/// **Z4a: der Haltepunkt — und der Beleg, dass er einer ist.**
///
/// Die naheliegende Pruefung waere „`freeze_thread` liefert `Frozen`". Die ist wertlos: sie sagt
/// nur, dass eine Funktion einen Wert zurueckgibt. Geprueft wird deshalb die **Wirkung**, und zwar
/// in beide Richtungen:
///
/// 1. Ein Worker zaehlt sichtbar hoch (`WORKER_ROUNDS`). **Vor** dem Einfrieren muss er sich
///    bewegen — sonst belegt „er steht" nichts, sondern beschreibt einen Thread, der nie lief.
///    Das ist dieselbe Positivkontrolle wie beim Kreuz-DMA-Nachweis (A-5.4).
/// 2. **Nach** dem Einfrieren muss der Zaehler ueber ein Beobachtungsfenster **unveraendert**
///    bleiben.
/// 3. Nach dem Auftauen muss er sich **wieder** bewegen. Ohne diesen dritten Schritt waere ein
///    `freeze`, das den Thread einfach kaputtmacht, von einem korrekten nicht zu unterscheiden.
///
/// Und die vierte Aussage, die keine Bewegung braucht: ein Thread mit **offener IPC-Beziehung**
/// wird abgewiesen (`Busy`), nicht eingefroren. Dafuer wird der IPC-Server genommen, der in `RECV`
/// steht — er ruht scheinbar, und genau das ist die Falle: „blockiert" und „ruhend" sehen von
/// aussen gleich aus und sind es nicht.
#[cfg(feature = "selftest")]
fn freeze_bericht() {
    use system::Freeze;
    // **Das Fenster muss laenger sein als ein Tick.** Die erste Fassung wartete 400 000
    // Leerdurchlaeufe -- unter KVM knapp eine Millisekunde, also kuerzer als die 10 ms bis zur
    // naechsten Verdraengung. Die Positivkontrolle meldete daraufhin `laeuft-vorher=false` bei
    // einem Worker, der voellig in Ordnung war: gemessen wurde nicht sein Stillstand, sondern die
    // Laenge des Fensters. Gezaehlt wird deshalb in **Ticks**, nicht in Schleifendurchlaeufen.
    let warten = || {
        let t0 = hal::timer::ticks(0);
        let mut wache = 0u64;
        while hal::timer::ticks(0) < t0 + 3 && wache < 200_000_000 {
            core::hint::spin_loop();
            wache += 1;
        }
    };
    let raw = WORKER_TID0.load(Ordering::Acquire);
    let Some(tid) = (raw != 0).then(|| sel4lake_sched::ThreadId::from_raw(raw)) else {
        println!("freeze  : SKIP (kein Worker-Thread -- ohne einen laufenden Thread gibt es nichts anzuhalten)");
        return;
    };

    // 1. Positivkontrolle: bewegt er sich ueberhaupt?
    let a0 = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    warten();
    let a1 = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    let laeuft = a1 > a0;

    // 2. Einfrieren. `StillRunning` ist vorübergehend -> wiederholen; `Busy` waere eine Absage.
    let mut erg = Freeze::StillRunning;
    for _ in 0..64 {
        erg = system::freeze_thread(tid);
        if erg != Freeze::StillRunning {
            break;
        }
        warten();
    }
    let eingefroren = erg == Freeze::Frozen;

    // 3. Steht er?
    let b0 = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    warten();
    warten();
    let b1 = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    let steht = b0 == b1;

    // 4. Und laeuft er nach dem Auftauen wieder?
    let aufgetaut = system::thaw_thread(tid);
    let c0 = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    warten();
    let c1 = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    let laeuft_wieder = c1 > c0;

    // 5. Ein Thread in einer IPC-Rolle wird ABGEWIESEN, nicht eingefroren.
    let sraw = IPC_SERVER_TID.load(Ordering::Acquire);
    let ipc_abgewiesen = if sraw != 0 {
        matches!(
            system::freeze_thread(sel4lake_sched::ThreadId::from_raw(sraw)),
            Freeze::Busy(_)
        )
    } else {
        true // nicht anwendbar
    };

    let ok = laeuft && eingefroren && steht && aufgetaut && laeuft_wieder && ipc_abgewiesen;
    println!(
        "freeze  : laeuft-vorher={laeuft} ({a0}->{a1}) eingefroren={eingefroren} \
         steht={steht} ({b0}->{b1}) aufgetaut={aufgetaut} laeuft-wieder={laeuft_wieder} \
         ({c0}->{c1}) IPC-Rolle-abgewiesen={ipc_abgewiesen}"
    );
    println!(
        "freeze  : {} (Z4a: ein Thread haelt an einer BENENNBAREN Grenze -- deplant, auf keinem \
         Kern, in keiner IPC-Rolle. Geprueft wird die WIRKUNG, nicht der Rueckgabewert: der \
         Zaehler muss sich vorher bewegen, danach stehen und nach dem Auftauen wieder laufen. \
         Ohne den ersten Schritt belegt 'er steht' nichts; ohne den dritten waere ein freeze, das \
         den Thread kaputtmacht, davon nicht zu unterscheiden. Ein Thread mit offener \
         IPC-Beziehung wird ABGEWIESEN -- 'blockiert' und 'ruhend' sehen von aussen gleich aus)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    // **Bewusst kein Konjunkt in `all_done()`.** Diese Pruefung laeuft IM Bericht -- was der
    // Bericht setzt, kann den Bericht nicht ausloesen (die Falle aus A-6.1, zweimal an einem Tag).
    // Gelesen wird die Zeile von der Suite; dass sie gelesen WIRD, ist die Lehre aus D5.
    FREEZE_OK.store(ok, Ordering::Release);
}

/// **Z4 Stufe 2: der Bericht ueber die Bootgrenze.**
///
/// Die Aussage, um die es geht, ist **eine**: der Zaehler in Lauf 2 startet bei dem Wert aus Lauf
/// 1, nicht bei 0. Alles andere in dieser Zeile ist das, was noetig ist, damit diese Aussage
/// ueberhaupt etwas belegt:
///
/// * **Die Nonce.** Ein Puffer mit den richtigen Bytes ist von einem beschriebenen Puffer nicht zu
///   unterscheiden. Der Fortschritt allein koennte in zwei Laeufen zufaellig gleich sein; die
///   Nonce ist der Zyklenzaehler beim Einfrieren und damit ein Wert, den Lauf 1 **erst erzeugt**
///   hat.
/// * **Der Kernel-Hash.** Er steht IM Checkpoint und wird beim Lesen geprueft. Ein Checkpoint
///   eines anderen Kernel-Images wird abgewiesen, nicht geladen (Z4f in klein).
/// * **Die Verweigerung.** Die Treiber-PD wird in jedem Lauf klassifiziert und faellt durch --
///   mit ihrem Kanal IM Umfang, damit als Grund nur die Geraete-Autoritaet uebrigbleibt. Ohne
///   diesen Fall belegte „es wurde gespeichert" nur, dass gespeichert wird.
/// * **„Er laeuft weiter."** Ein Wiederherstellen, das den Thread kaputtmacht, waere von einem
///   korrekten sonst nicht zu unterscheiden -- dieselbe dritte Stufe wie in `freeze_bericht`.
///
/// **Bewusst kein Konjunkt in `all_done()`** -- was der Bericht setzt, kann den Bericht nicht
/// ausloesen (A-6.1, zweimal an einem Tag). In der Abschlussbedingung steht nur der
/// **Fertig-Merker** `CKPT_DONE`, damit der Bericht nicht vor der Platten-E/A kommt; gelesen wird
/// die Zeile von der Suite.
#[cfg(feature = "selftest")]
fn ckpt_bericht() {
    let zustand = CKPT_STATE.load(Ordering::Acquire);
    let (rslot, rgrund) = (
        CKPT_REFUSED[0].load(Ordering::Acquire),
        CKPT_REFUSED[1].load(Ordering::Acquire),
    );
    if !CKPT_DONE.load(Ordering::Acquire) || zustand == CKPT_SKIP {
        println!(
            "bootckpt: SKIP (kein Blockdienst -- ohne Platte gibt es keine Bootgrenze, ueber die \
             etwas wandern koennte. Die Hauptsuite bootet ohne Archiv und damit ohne Treiber-PD)"
        );
        return;
    }
    let p = CKPT_PROGRESS.load(Ordering::Acquire);
    let nonce = CKPT_NONCE.load(Ordering::Acquire);
    let epoche = CKPT_EPOCH.load(Ordering::Acquire);
    let sp = CKPT_S_PROGRESS.load(Ordering::Acquire);
    let snonce = CKPT_S_NONCE.load(Ordering::Acquire);
    let sepoche = CKPT_S_EPOCH.load(Ordering::Acquire);
    let jetzt = WORKER_ROUNDS[0].load(Ordering::Relaxed);
    let vorher = CKPT_BEFORE.load(Ordering::Acquire);
    let nach = CKPT_AFTER.load(Ordering::Acquire);
    let caps = CKPT_CAPS.load(Ordering::Acquire);
    let pre = CKPT_PRECOND.load(Ordering::Acquire);
    // Die Verweigerung ist eine eigene Zeile: sie gilt in JEDEM Lauf und haengt nicht daran, ob
    // gespeichert oder wiederhergestellt wurde.
    println!(
        "bootckpt: Verweigerung: die Treiber-PD ist NICHT speicherbar -- Slot {rslot}, Grund \
         {rgrund} (1=Geraetefenster 2=Interrupt 3=DMA-Region 4=offene Antwort 5=Partner nicht im \
         Umfang 6=fremder Thread 7=fremde PD 8=Loader-Quelle; 0=nichts sprach dagegen, also \
         NICHTS gemessen). Ihr Kanal liegt IM Umfang -- was uebrigbleibt, ist Geraete-Autoritaet, \
         und die laesst sich durch keinen groesseren Umfang beheben"
    );
    // **Geraete-Autoritaet**, nicht irgendein Grund: 5 (Partner nicht im Umfang) waere durch einen
    // groesseren Umfang behebbar und belegte die Regel deshalb nicht.
    let verweigert_richtig = rgrund == 1 || rgrund == 2 || rgrund == 3;
    // **Jeder Lauf, der durchkommt, SCHREIBT** — auch der, der eben wiederhergestellt hat. Erst
    // dadurch waechst die Kette; ein Lauf, der nur liest, koennte nicht belegen, dass sein
    // Nachfolger etwas GEERBTES findet und nicht seinen eigenen Wert noch einmal.
    let io = |i: usize| CKPT_IO[i].load(Ordering::Acquire);
    let speichern_ok = || {
        CKPT_FROZEN.load(Ordering::Acquire)
            && sp > 0
            && snonce != 0
            && sepoche >= 1
            && CKPT_BYTES.load(Ordering::Acquire) > 0
            && io(1) == 0
            && io(2) == 0
            && caps > 0
            && CKPT_DELTA_OK.load(Ordering::Acquire)
            && verweigert_richtig
            // Der Thread laeuft nach dem Speichern weiter -- speichern ist kein Beenden.
            && jetzt > sp
    };
    let ok = match zustand {
        CKPT_SAVED => {
            println!(
                "bootckpt: gespeichert Sektor={CKPT_SECTOR} Fortschritt={sp} Nonce={snonce:#018x} \
                 Epoche={sepoche} Caps={caps} Vorbedingung={pre} Bytes={} Lesen={} Schreiben={} \
                 Flush={} eingefroren={} Zuwachs-erreicht={} Zaehler-jetzt={jetzt}",
                CKPT_BYTES.load(Ordering::Acquire),
                io(0), io(1), io(2),
                CKPT_FROZEN.load(Ordering::Acquire) as u8,
                CKPT_DELTA_OK.load(Ordering::Acquire) as u8
            );
            io(0) == 0 && sepoche == 1 && speichern_ok()
        }
        CKPT_RESTORED => {
            println!(
                "bootckpt: wiederhergestellt Sektor={CKPT_SECTOR} Fortschritt={p} \
                 Nonce={nonce:#018x} Epoche={epoche} Caps={caps} Vorbedingung={pre} \
                 (Kernel-Hash passt) eingefroren={} Zaehler-vorher={vorher} Zaehler-danach={nach}",
                CKPT_FROZEN.load(Ordering::Acquire) as u8
            );
            println!(
                "bootckpt: gespeichert Sektor={CKPT_SECTOR} Fortschritt={sp} Nonce={snonce:#018x} \
                 Epoche={sepoche} Caps={caps} Vorbedingung={pre} Bytes={} Lesen={} Schreiben={} \
                 Flush={} eingefroren={} Zuwachs-erreicht={} Zaehler-jetzt={jetzt}",
                CKPT_BYTES.load(Ordering::Acquire),
                io(0), io(1), io(2),
                CKPT_FROZEN.load(Ordering::Acquire) as u8,
                CKPT_DELTA_OK.load(Ordering::Acquire) as u8
            );
            io(0) == 0
                && p > 0
                && nonce != 0
                && epoche >= 1
                // **Der wiederhergestellte Wert steht IM THREAD**, nicht bloss im Bericht.
                && nach == p
                // Die Kette waechst: dieser Lauf schreibt ein Glied weiter und einen groesseren
                // Fortschritt. Ohne beides waere „wiederhergestellt" nicht von „neu angelegt" zu
                // unterscheiden.
                && sepoche == epoche + 1
                && sp > p
                && speichern_ok()
        }
        CKPT_REJECTED => {
            println!(
                "bootckpt: ABGEWIESEN Sektor={CKPT_SECTOR} Lesecode={} (1=kein Checkpoint \
                 2=Formatversion 3=Laenge 4=zu viele Caps 5=unbekannter Cap-Typ 6=Pruefsumme \
                 7=FREMDES KERNEL-IMAGE) -- der Sektor bleibt unveraendert, es wurde weder \
                 geladen noch ueberschrieben",
                CKPT_DECODE.load(Ordering::Acquire)
            );
            // Eine Abweisung ist ein **erfolgreicher** Ausgang, wenn sie den richtigen Grund hat.
            // Das Urteil hier ist trotzdem `false`: welcher Grund richtig ist, entscheidet der
            // Aufbau, und den kennt die Suite -- sie liest die Codezeile. Ein Kernel, der seine
            // eigene Abweisung als PASS meldete, koennte einen falsch abgewiesenen Lauf nicht von
            // einem richtig abgewiesenen unterscheiden.
            false
        }
        _ => {
            println!(
                "bootckpt: FEHLER (Lesecode={}, eingefroren={}) -- die Folge kam nicht durch",
                CKPT_DECODE.load(Ordering::Acquire),
                CKPT_FROZEN.load(Ordering::Acquire) as u8
            );
            false
        }
    };
    println!(
        "bootckpt: {} (Z4 Stufe 2: ein Thread wird eingefroren, sein Fortschritt ueber eine \
         BOOTGRENZE gespeichert und im naechsten Lauf wiederhergestellt. Derselbe Kernel macht \
         beides und entscheidet selbst welches -- er liest den Sektor. Der Transport ist der \
         vorhandene Blockdienst; der Kern ist Client. Gebunden ist der Checkpoint an das \
         Kernel-Image: ein fremder Hash wird ABGEWIESEN statt geladen)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
}

/// **A-5.3: hat der Selektor entschieden — oder die Fundreihenfolge?**
///
/// Die naheliegende Pruefung waere „der Treiber hat ein Geraet bekommen". Die ist wertlos: sie ist
/// auch dann wahr, wenn er das falsche bekam, und genau das ist der Fehler, den ein Selektor
/// verhindern soll. Geprueft wird deshalb dreierlei, und der erste Punkt ist der wichtigste:
///
/// 1. **Es gab ueberhaupt etwas zu entscheiden.** Bei nur einem angebotenen Geraet trifft jeder
///    Selektor und jeder Nicht-Selektor dieselbe Wahl — eine gruene Zeile hiesse dann nichts.
///    Weniger als zwei Angebote sind ein **SKIP mit Begruendung**, kein PASS.
/// 2. **Die Zuteilung passt auf den Selektor des Manifest-Eintrags** — Hersteller und Geraete-ID
///    aus dem Autoritaetsdokument gegen die tatsaechlich vergebene Instanz.
/// 3. **Das nicht gewaehlte Geraet ist noch frei.** Erst das macht aus „es passte" ein „es wurde
///    ausgewaehlt": es belegt, dass eine Alternative dastand und liegen blieb.
fn devsel_bericht() {
    let angeboten_vorher = ANGEBOTEN_VORHER.load(Ordering::Acquire);
    let mut zu = [(0u32, 0u32, 0u16, 0u16); system::MAX_OFFERED_DEVICES];
    let n_zu = system::driver_assignments(&mut zu);
    let frei = system::offered_device_count();

    if angeboten_vorher < 2 {
        println!(
            "devsel  : SKIP ({angeboten_vorher} Geraet(e) angeboten -- bei weniger als zweien gibt \
             es nichts zu entscheiden, und eine gruene Zeile hiesse hier nichts. Der Selektor ist \
             erst pruefbar, wenn eine Alternative dasteht, die liegen bleiben KANN)"
        );
        return;
    }
    let Some(man) = crate::loader::read_manifest() else {
        println!("devsel  : SKIP (kein Manifest -- ohne Autoritaetsdokument wird nichts zugeteilt)");
        return;
    };

    // **Jede** Zuteilung gegen den Selektor **ihres** Eintrags. Ein Sammelurteil ueber "die
    // Zuteilung" waere bei zwei Treibern wieder die Aussage, die auch bei vertauschten Geraeten
    // wahr ist -- und genau das soll der Selektor ausschliessen.
    let mut alle_passen = n_zu > 0;
    let mut ids_genannt = n_zu > 0;
    for &(pid, rid, ven, dev) in zu.iter().take(n_zu) {
        let Some(e) = (0..man.count()).filter_map(|i| man.entry(i)).find(|e| e.program_id == pid)
        else {
            alle_passen = false;
            continue;
        };
        // Der Klassenvergleich entfaellt: `driver_assignments` fuehrt die Klasse nicht mit, und
        // ein Platzhalter wuerde einen klassenbasierten Selektor stillschweigend durchwinken.
        if !e.device.matches(ven, dev, u32::MAX) {
            alle_passen = false;
        }
        if e.device.vendor == sel4lake_loader::manifest::ANY16
            && e.device.device == sel4lake_loader::manifest::ANY16
        {
            ids_genannt = false;
        }
        println!("devsel  : Eintrag {pid} verlangt {:04x}:{:04x}, bekam RID {rid:#06x} {ven:04x}:{dev:04x}", e.device.vendor, e.device.device);
    }
    // Die Bilanz: was vergeben wurde, fehlt im Angebot -- und zwar genau das.
    let bilanz = frei + n_zu == angeboten_vorher;
    let ok = alle_passen && ids_genannt && bilanz;
    println!(
        "devsel  : {} (A-5.3/A-5.4: {angeboten_vorher} angeboten, {n_zu} vergeben, {frei} frei. \
         JEDE Zuteilung wurde gegen den Selektor IHRES Manifest-Eintrags geprueft -- ein \
         Sammelurteil ueber 'die Zuteilung' waere bei zwei Treibern auch dann wahr, wenn die \
         Geraete vertauscht waeren)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
}

static DRIVER_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-5.3: wie viele Geräte angeboten waren, **bevor** die erste Zuteilung stattfand. Hinterher
/// nicht mehr feststellbar — ein vergebenes Gerät ist aus der Liste heraus.
static ANGEBOTEN_VORHER: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
/// A-6.1: traegt das Dienstprotokoll (Auskunft, Lesen, Schreiben, Flush, Bereichsfehler)?
static BLKDEV_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-6.2: wurde die Partitionstabelle richtig gelesen?
static PART_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// A-6.3: hat die Dateisystem-PD ihre Datei gelesen?
static FS_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Was `tools/mkgpt.py --file` ins Dateisystem legt: "SEL4LAKE-DATEIINHALT" (20 Byte).
const FS_DATEI_GROESSE: u64 = 20;
/// Die ersten acht Bytes davon, little-endian gelesen ("SEL4LAKE").
const FS_ERSTE_ACHT: u64 = 0x454B_414C_344C_4553;
/// A-6.4: auf diese Groesse schreibt die Probe die Datei — ueber einen zweiten Cluster hinaus.
/// Eine Schreibprobe, die in den vorhandenen Cluster passt, prueft die Kettenverlaengerung nicht.
const FS_NEU_GROESSE: u64 = 700;

// --- A-5.1: der Client des Treiber-DIENSTES ---------------------------------------------------
//
// Der Kernel ist hier **Client**, nicht Treiber. Das ist genau die Richtungsumkehr: bis A-5.1
// fuhr er den virtio-Handshake selbst; jetzt fragt er einen Dienst und bekommt eine Antwort.
// Zwischen den beiden Anfragen wird der Dienst **ausgetauscht** -- und der Client merkt davon
// nichts ausser einem weitergezaehlten Bedienungszaehler.
// Das Dienstprotokoll (A-6.1) -- muss zu `programs/hardware/virtio-blk` passen.
const OP_INFO: u64 = 0;
const OP_READ: u64 = 1;
const OP_WRITE: u64 = 3;
const OP_FLUSH: u64 = 4;
const OP_SCAN: u64 = 5;
/// Sektor, auf den die Probe schreibt — in der **zweiten** (rohen) Partition und **nicht** die
/// Magie selbst. Ein Test, der seine eigene Referenz ueberschreibt, prueft ab dem zweiten
/// Lauf etwas anderes als beim ersten.
const PROBE_SECTOR: u64 = 20002;
/// Erwartete Partitionstabelle des Testabbilds (`tools/mkgpt.py`).
const ERWARTET_PARTITIONEN: u64 = 2;
const ERWARTET_ERSTE_LBA: u64 = 34;
const ERWARTET_ERSTE_SEKTOREN: u64 = 19967;
/// Ergebnisse der Protokollfolge.
static DRV_SEQ: [core::sync::atomic::AtomicU64; 12] = [
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(u64::MAX),
];
/// Ergebnis der ersten Anfrage: `(Status, erste acht Byte, Kapazitaet, Bedienungszaehler)`.
static DRV_R1: [core::sync::atomic::AtomicU64; 4] = [
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
];
/// Ergebnis der zweiten Anfrage (nach dem Austausch).
static DRV_R2: [core::sync::atomic::AtomicU64; 4] = [
    core::sync::atomic::AtomicU64::new(u64::MAX),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
];
/// Das erste Wort im Datenpuffer des Treibers **am Ende seiner Dienstfolge** — erfasst dort, wo
/// die Aussage gilt, nicht im Bericht (s. `report_and_off`). `u64::MAX` = nie erfasst.
static DRV_DMA_WORT: AtomicU64 = AtomicU64::new(u64::MAX);
static DRV_CALL1_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static DRV_CALL2_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static DRV_SWAPPED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Wie weit die Dienst-/Austausch-Abfolge ist (0 = noch nichts, 1 = Client laeuft, 2 = getauscht).
static DRV_STEP: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Ergebnis des Austauschs, als Text fuer den Bericht.
static DRV_RELOAD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);

/// **Die Abfolge von A-5.1**, Schritt fuer Schritt aus der Hauptschleife des Hochlaufs getrieben.
///
/// Sie laeuft hier und nicht in einem eigenen Faden, weil ihre Schritte **aufeinander warten**
/// muessen: erst wenn der Treiber bereit ist, darf gefragt werden; erst wenn die erste Antwort da
/// ist, darf getauscht werden. Ein Faden, der das mit Schlaeuchen loest, waere schwerer zu lesen
/// als eine Zustandsmaschine, die bei jedem Durchlauf genau eine Bedingung prueft.
#[cfg(feature = "selftest")]
fn drv_service_step(archive: bool) {
    match DRV_STEP.load(Ordering::Acquire) {
        0 => {
            // **Ohne Boot-Archiv gibt es nie einen Treiber-Dienst.** Das muss hier entschieden
            // werden und nicht im Bericht: `BLKDEV_OK` steht in `all_done()`, und was der Bericht
            // setzt, kann den Bericht nicht ausloesen. Beim ersten Anlauf lief die Hauptsuite
            // deshalb in den Watchdog -- sie hat kein Archiv, also wartete die Zustandsmaschine
            // auf einen Dienst, der per Aufbau nie kommt.
            //
            // `driver_service().is_none()` allein taugt nicht als Kriterium: unmittelbar nach dem
            // Start ist es auch dann None, wenn gleich einer kommt. "Es gibt kein Archiv" ist die
            // Aussage, die von Anfang an feststeht.
            if !archive {
                BLKDEV_OK.store(true, Ordering::Release); // nicht anwendbar, nicht bestanden
                PART_OK.store(true, Ordering::Release);
                DRV_STEP.store(3, Ordering::Release);
                return;
            }
            // Warten, bis der Treiber sein Geraet aufgeloest hat und in `recv` steht. Vorher zu
            // fragen hiesse, auf eine Zusage zu bauen, die er noch nicht gegeben hat.
            let Some(svc) = crate::loader::driver_service_of(TEST_BLK_SERVICE_ID) else { return };
            let badge = system::notification_pending(svc.ntfn);
            if badge & DRV_READY_BADGE == 0 {
                return;
            }
            // **Erst die Dateisystem-PD, dann diese Folge.** Beide sind Clients desselben
            // Dienstes; liefen sie gleichzeitig, mischten sich ihre Anfragen, und der Austausch
            // fiele mitten in ein fremdes Gespraech. Der Bedienungszaehler waere dann auch keine
            // Aussage mehr ueber DIESE Folge.
            if crate::loader::client_notification().is_some() {
                let cb = crate::loader::client_notification()
                    .map(system::notification_pending)
                    .unwrap_or(0);
                if cb & crate::loader::CLIENT_NTFN_BADGE == 0 {
                    return;
                }
            }
            // Client-PD mit einer SEND-Cap auf den Kanal. Mehr braucht ein Client nicht -- er
            // kann rufen und sonst nichts, insbesondere den Dienst nicht empfangen.
            let Ok(ep_cap) = system::install_endpoint_cap(svc.ep as u32, Rights::WRITE) else {
                return;
            };
            let Some(cli_pd) = system::create_pd() else { return };
            if !system::install_pd_cap(cli_pd, 0, ep_cap) {
                return;
            }
            let Some(cli) = system::spawn(drv_client as *const () as usize, 0, system::IDLE_PRIO)
            else {
                return;
            };
            system::bind_pd(cli_pd, cli);
            DRV_STEP.store(1, Ordering::Release);
        }
        1 => {
            // Erst tauschen, wenn die erste Anfrage beantwortet ist -- sonst belegte der zweite
            // Aufruf nicht, dass eine ANDERE Fassung antwortet.
            if !DRV_CALL1_DONE.load(Ordering::Acquire) {
                return;
            }
            let out = crate::loader::reload_driver(TEST_BLK_SERVICE_ID);
            DRV_RELOAD.store(out as u32, Ordering::Release);
            DRV_SWAPPED.store(true, Ordering::Release);
            DRV_STEP.store(2, Ordering::Release);
        }
        2 => {
            // **Das Urteil wird HIER gebildet, nicht im Bericht.** Es steht in `all_done()`, und
            // ein Urteil, das erst im Bericht entsteht, koennte den Bericht nicht ausloesen --
            // der Lauf liefe in den Watchdog und druckte das Ergebnis trotzdem. Das sah beim
            // ersten Anlauf aus wie "gruen, aber gehangen" und war eine Zirkularitaet.
            if !DRV_CALL2_DONE.load(Ordering::Acquire) {
                return;
            }
            let q = |i: usize| DRV_SEQ[i].load(Ordering::Acquire);
            BLKDEV_OK.store(
                q(0) == 0
                    && q(1) == 32768
                    && q(2) == 512
                    && q(3) == 0
                    && q(4) == 0
                    && q(5) == 0
                    && q(6) == BLK_MAGIC
                    && q(7) == 3,
                Ordering::Release,
            );
            PART_OK.store(
                q(8) == 0
                    && q(9) == ERWARTET_PARTITIONEN
                    && q(10) == ERWARTET_ERSTE_LBA
                    && q(11) == ERWARTET_ERSTE_SEKTOREN,
                Ordering::Release,
            );
            // **Den Datenpuffer des Treibers JETZT erfassen, nicht im Bericht.** Genau hier gilt
            // die Aussage „der Blockdienst hat die Magie geliefert"; jeder spaetere Client (seit
            // Z4 Stufe 2 gibt es einen) legt dort seinen eigenen Inhalt ab.
            if let Some((dma_phys, _)) = system::driver_assignment(TEST_BLK_PROGRAM_ID) {
                // SAFETY: identity-gemappte, vom Kernel ausgeschnittene DMA-Region; nur lesend.
                let w = unsafe {
                    core::ptr::read_volatile((dma_phys + hal::virtio::blk::OFF_DATA) as *const u64)
                };
                DRV_DMA_WORT.store(w, Ordering::Release);
            }
            // **Der Checkpoint laeuft VOR dem Kreuz-DMA-Nachweis**, und das ist keine
            // Geschmacksfrage: A-5.4 liest das erste Wort der fremden DMA-Region vor und nach dem
            // Fremdversuch. Eine Platten-E/A dazwischen ginge durch genau diese Region -- der
            // „unabhaengige Zeuge" saehe dann eine Aenderung, die der Angreifer nicht gemacht hat.
            DRV_STEP.store(5, Ordering::Release);
        }
        // --- Z4 Stufe 2: den Sektor lesen und entscheiden, was dieser Lauf ist -----------------
        5 => {
            let Some(svc) = crate::loader::driver_service_of(TEST_BLK_SERVICE_ID) else {
                CKPT_DONE.store(true, Ordering::Release);
                DRV_STEP.store(3, Ordering::Release);
                return;
            };
            // Der Client braucht nur eine SEND-Cap auf den Kanal -- er kann rufen und sonst
            // nichts. Derselbe Aufbau wie `drv_client`; der Kernel ist auch hier Client.
            let Ok(ep_cap) = system::install_endpoint_cap(svc.ep as u32, Rights::WRITE) else {
                return;
            };
            let Some(cli_pd) = system::create_pd() else { return };
            if !system::install_pd_cap(cli_pd, 0, ep_cap) {
                return;
            }
            let Some(cli) = system::spawn(ckpt_client as *const () as usize, 0, system::IDLE_PRIO)
            else {
                return;
            };
            system::bind_pd(cli_pd, cli);
            CKPT_REQ.store(1, Ordering::Release);
            DRV_STEP.store(6, Ordering::Release);
        }
        6 => {
            if CKPT_ACK.load(Ordering::Acquire) < 1 {
                return;
            }
            let Some((shared, _)) = system::driver_shared_region(TEST_BLK_SERVICE_ID) else {
                CKPT_STATE.store(CKPT_ERROR, Ordering::Release);
                CKPT_DONE.store(true, Ordering::Release);
                DRV_STEP.store(3, Ordering::Release);
                return;
            };
            let mut sek = [0u8; 512];
            // SAFETY: identity-gemappte, vom Kernel ausgeschnittene Uebertragungsflaeche
            // (`SHARED_BYTES` >= 8 KiB); es werden 512 Byte daraus KOPIERT, nicht ausgewertet.
            // Die Auswertung passiert danach in einer abhaengigkeitsfreien Crate ohne `unsafe`.
            unsafe { core::ptr::copy_nonoverlapping(shared as *const u8, sek.as_mut_ptr(), 512) };

            // **Die Negativkontrolle laeuft in JEDEM Lauf mit, nicht nur im Fehlerfall.** Die
            // TREIBER-PD haelt echte Geraete-Autoritaet (MMIO-Fenster, DMA-Region) -- sie ist der
            // Fall, an dem Z4b etwas zu sagen hat. Ihr Kanal (Endpoint + Notification) liegt dabei
            // ausdruecklich IM Umfang: damit bleibt als Grund nur die Geraete-Autoritaet uebrig,
            // und die Absage ist keine, die ein groesserer Umfang beheben koennte.
            if let Some(svc) = crate::loader::driver_service_of(TEST_BLK_SERVICE_ID) {
                use sel4lake_cap::checkpoint::{classify_all, Scope};
                let mut kinds = [None; 16];
                let n = system::pd_object_kinds(svc.pd, &mut kinds);
                let (eps, ns) = ([svc.ep as u32], [svc.ntfn as u32]);
                let scope = Scope { endpoints: &eps, notifications: &ns, ..Scope::EMPTY };
                match classify_all(&kinds[..n], &scope) {
                    Err((slot, r)) => {
                        CKPT_REFUSED[0].store(slot as u32, Ordering::Release);
                        CKPT_REFUSED[1].store(ckpt_reason(r), Ordering::Release);
                    }
                    // `Ok` heisst hier NICHT „bestanden": eine Treiber-PD, deren Cspace nichts
                    // Geraetegebundenes enthaelt, hat gar keine Geraete-Autoritaet -- dann hat der
                    // Test nichts gemessen. Der Grund-Code 0 sagt genau das.
                    Ok(()) => {
                        CKPT_REFUSED[0].store(n as u32, Ordering::Release);
                        CKPT_REFUSED[1].store(0, Ordering::Release);
                    }
                }
            }

            let kh = crate::loader::kernel_code_hash();
            match sel4lake_cap::checkpoint::Image::decode(&sek, &kh) {
                Ok(img) => {
                    CKPT_DECODE.store(0, Ordering::Release);
                    CKPT_PROGRESS.store(img.progress, Ordering::Release);
                    CKPT_NONCE.store(img.nonce, Ordering::Release);
                    CKPT_EPOCH.store(img.epoch, Ordering::Release);
                    CKPT_CAPS.store(img.cap_count as u32, Ordering::Release);
                    CKPT_PRECOND.store(img.precondition_bits, Ordering::Release);
                    CKPT_STATE.store(CKPT_RESTORED, Ordering::Release);
                }
                Err(e) => {
                    CKPT_DECODE.store(ckpt_code(&e), Ordering::Release);
                    if e == sel4lake_cap::checkpoint::ImageError::NoImage {
                        CKPT_STATE.store(CKPT_SAVED, Ordering::Release); // Vorhaben, noch kein Befund
                    } else {
                        // **Abgewiesen -- und der Sektor bleibt, wie er ist.** Ihn hier zu
                        // ueberschreiben waere bequem und faelschte den naechsten Lauf: aus einem
                        // fremden Checkpoint wuerde stillschweigend ein eigener.
                        CKPT_STATE.store(CKPT_REJECTED, Ordering::Release);
                        CKPT_REQ.store(3, Ordering::Release);
                        CKPT_DONE.store(true, Ordering::Release);
                        DRV_STEP.store(3, Ordering::Release);
                        return;
                    }
                }
            }
            if CKPT_STATE.load(Ordering::Acquire) == CKPT_RESTORED {
                CKPT_FREEZE_DEADLINE.store(hal::timer::ticks(0) + 300, Ordering::Release);
                DRV_STEP.store(7, Ordering::Release);
            } else {
                // **Kaltstart: nichts einzufrieren.** Der erste Anlauf fror den Thread auch hier
                // ein -- und liess ihn dann im Wartezustand STEHEN. Der Zuwachs, auf den gewartet
                // wurde, konnte damit nie eintreten (`Zuwachs-erreicht=0`, 20 s Notschranke).
                // Einfrieren gehoert zum ANFASSEN des Zustands, nicht zum Warten darauf.
                CKPT_WAIT_BASE.store(WORKER_ROUNDS[0].load(Ordering::Relaxed), Ordering::Release);
                CKPT_WAIT_UNTIL.store(hal::timer::ticks(0) + 600, Ordering::Release);
                DRV_STEP.store(10, Ordering::Release);
            }
        }
        // --- Z4a am echten Gegenstand: einfrieren, Zustand anfassen, auftauen -------------------
        7 => {
            use system::Freeze;
            let raw = WORKER_TID0.load(Ordering::Acquire);
            let Some(tid) = (raw != 0).then(|| sel4lake_sched::ThreadId::from_raw(raw)) else {
                CKPT_STATE.store(CKPT_ERROR, Ordering::Release);
                CKPT_REQ.store(3, Ordering::Release);
                CKPT_DONE.store(true, Ordering::Release);
                DRV_STEP.store(3, Ordering::Release);
                return;
            };
            match system::freeze_thread(tid) {
                Freeze::Frozen => {}
                // **`StillRunning` ist voruebergehend, `Busy` ist eine Absage** (Z4a). Nur der
                // erste Fall wird wiederholt -- und auch der nicht ewig: die Schranke zaehlt in
                // TICKS, nicht in Umdrehungen. Eine Umdrehungszahl misst die Taktrate.
                Freeze::StillRunning
                    if hal::timer::ticks(0) < CKPT_FREEZE_DEADLINE.load(Ordering::Acquire) =>
                {
                    return;
                }
                _ => {
                    CKPT_STATE.store(CKPT_ERROR, Ordering::Release);
                    CKPT_REQ.store(3, Ordering::Release);
                    CKPT_DONE.store(true, Ordering::Release);
                    DRV_STEP.store(3, Ordering::Release);
                    return;
                }
            }
            CKPT_FROZEN.store(true, Ordering::Release);

            // **Wiederherstellen.** Der Zustand, um den es geht, IST dieser Zaehler -- er wird
            // gesetzt, waehrend der Thread steht. Ihn im Laufen zu setzen hiesse, gegen den
            // Thread zu schreiben, den man gerade wiederherstellt.
            //
            // Der Wert VOR dem Setzen wird mitgemeldet: er ist die Positivkontrolle der Suite.
            // Waeren gefundener und eigener Wert gleich, waere „gesetzt" von „nicht gesetzt"
            // nicht zu unterscheiden -- und genau das ist im Mutationslauf passiert.
            CKPT_BEFORE.store(WORKER_ROUNDS[0].load(Ordering::Relaxed), Ordering::Release);
            WORKER_ROUNDS[0].store(CKPT_PROGRESS.load(Ordering::Acquire), Ordering::Relaxed);
            CKPT_AFTER.store(WORKER_ROUNDS[0].load(Ordering::Relaxed), Ordering::Release);
            // **Sofort auftauen.** „Jetzt arbeiten lassen, dann erst speichern" heisst, dass der
            // Thread LAUFEN muss -- ein eingefrorener Thread erreicht keinen Zuwachs, und die
            // Wartestufe darunter wartete dann auf etwas, das per Aufbau nicht eintreten kann.
            let _ = system::thaw_thread(tid);
            CKPT_WAIT_BASE.store(WORKER_ROUNDS[0].load(Ordering::Relaxed), Ordering::Release);
            CKPT_WAIT_UNTIL.store(hal::timer::ticks(0) + 600, Ordering::Release);
            DRV_STEP.store(10, Ordering::Release);
        }
        // --- Warten, bis der Zuwachs da ist ----------------------------------------------------
        //
        // Gezaehlt wird in **Runden**, nicht in Ticks: der Zuwachs ist die Groesse, um die es geht,
        // und eine Zeitspanne haette ihn nur mittelbar erzeugt. Die Tick-Schranke daneben ist eine
        // Notbremse -- sie verhindert einen Haenger, ist aber kein Erfolgskriterium; ob der
        // Zuwachs wirklich zustande kam, steht in `CKPT_DELTA_OK` und im Bericht.
        10 => {
            let base = CKPT_WAIT_BASE.load(Ordering::Acquire);
            if WORKER_ROUNDS[0].load(Ordering::Relaxed) >= base + CKPT_MIN_DELTA {
                CKPT_DELTA_OK.store(true, Ordering::Release);
            } else if hal::timer::ticks(0) < CKPT_WAIT_UNTIL.load(Ordering::Acquire) {
                return;
            }
            CKPT_FREEZE_DEADLINE.store(hal::timer::ticks(0) + 300, Ordering::Release);
            DRV_STEP.store(11, Ordering::Release);
        }
        // --- Speichern: einfrieren, lesen, klassifizieren, schreiben ---------------------------
        11 => {
            use system::Freeze;
            let raw = WORKER_TID0.load(Ordering::Acquire);
            let Some(tid) = (raw != 0).then(|| sel4lake_sched::ThreadId::from_raw(raw)) else {
                CKPT_STATE.store(CKPT_ERROR, Ordering::Release);
                CKPT_REQ.store(3, Ordering::Release);
                CKPT_DONE.store(true, Ordering::Release);
                DRV_STEP.store(3, Ordering::Release);
                return;
            };
            let abbruch = |tid, tauen: bool| {
                if tauen {
                    let _ = system::thaw_thread(tid);
                }
                CKPT_STATE.store(CKPT_ERROR, Ordering::Release);
                CKPT_REQ.store(3, Ordering::Release);
                CKPT_DONE.store(true, Ordering::Release);
                DRV_STEP.store(3, Ordering::Release);
            };
            match system::freeze_thread(tid) {
                Freeze::Frozen => {}
                Freeze::StillRunning
                    if hal::timer::ticks(0) < CKPT_FREEZE_DEADLINE.load(Ordering::Acquire) =>
                {
                    return;
                }
                _ => {
                    abbruch(tid, false);
                    return;
                }
            }
            CKPT_FROZEN.store(true, Ordering::Release);
            // Der Fortschritt wird am STEHENDEN Thread gelesen: ein Wert, der waehrend des Lesens
            // weiterlaeuft, gehoert zu keinem Zeitpunkt.
            let p = WORKER_ROUNDS[0].load(Ordering::Relaxed);
            // Die Nonce muss dieser Lauf ERZEUGT haben. Der Zyklenzaehler beim Einfrieren ist der
            // billigste Wert, den kein Rest in einem Puffer haben kann.
            let nonce = hal::timer::cycles() | 1;
            let epoche = CKPT_EPOCH.load(Ordering::Acquire) + 1;
            let kh = crate::loader::kernel_code_hash();
            let pd = CKPT_PD.load(Ordering::Acquire);
            let mut kinds = [None; 16];
            let n = if pd == 0 {
                0
            } else {
                system::pd_object_kinds(pd as usize - 1, &mut kinds)
            };
            let scope = sel4lake_cap::checkpoint::Scope::EMPTY;
            match sel4lake_cap::checkpoint::Image::build(kh, p, nonce, epoche, &kinds[..n], &scope)
            {
                Ok(img) => {
                    let Some((shared, _)) = system::driver_shared_region(TEST_BLK_SERVICE_ID)
                    else {
                        abbruch(tid, true);
                        return;
                    };
                    let mut sek = [0u8; 512];
                    let len = img.encode(&mut sek).unwrap_or(0);
                    CKPT_BYTES.store(len as u32, Ordering::Release);
                    CKPT_CAPS.store(img.cap_count as u32, Ordering::Release);
                    CKPT_PRECOND.store(img.precondition_bits, Ordering::Release);
                    CKPT_S_PROGRESS.store(p, Ordering::Release);
                    CKPT_S_NONCE.store(nonce, Ordering::Release);
                    CKPT_S_EPOCH.store(epoche, Ordering::Release);
                    // SAFETY: wie beim Lesen -- identity-gemappte Uebertragungsflaeche, 512 Byte.
                    unsafe {
                        core::ptr::copy_nonoverlapping(sek.as_ptr(), shared as *mut u8, 512)
                    };
                    CKPT_REQ.store(2, Ordering::Release);
                    DRV_STEP.store(12, Ordering::Release);
                }
                Err((slot, r)) => {
                    // Das Subjekt selbst haelt etwas, das nicht mitwandern darf -> kein
                    // Checkpoint. Der Slot kommt mit; „irgendeine Cap" ist als Diagnose wertlos.
                    CKPT_REFUSED[0].store(slot as u32, Ordering::Release);
                    CKPT_REFUSED[1].store(ckpt_reason(r), Ordering::Release);
                    CKPT_STATE.store(CKPT_REJECTED, Ordering::Release);
                    let _ = system::thaw_thread(tid);
                    CKPT_REQ.store(3, Ordering::Release);
                    CKPT_DONE.store(true, Ordering::Release);
                    DRV_STEP.store(3, Ordering::Release);
                }
            }
        }
        12 => {
            if CKPT_ACK.load(Ordering::Acquire) < 2 {
                return;
            }
            let raw = WORKER_TID0.load(Ordering::Acquire);
            if raw != 0 {
                let tid = sel4lake_sched::ThreadId::from_raw(raw);
                // **Auftauen gehoert zum Speichern.** Ein Checkpoint, der sein Subjekt stehen
                // laesst, ist ein Abbruch mit Nebenwirkung -- und `freeze_bericht` weiter unten
                // braucht einen laufenden Thread als Positivkontrolle.
                let _ = system::thaw_thread(tid);
            }
            CKPT_REQ.store(3, Ordering::Release);
            CKPT_DONE.store(true, Ordering::Release);
            DRV_STEP.store(3, Ordering::Release);
        }
        // --- A-5.4: der Kreuz-DMA-Nachweis ----------------------------------------------------
        //
        // Erst hier, ganz am Ende: der Fremdversuch loest absichtlich VT-d-Faults aus, und die
        // duerfen keiner frueheren Messung in die Zahlen laufen.
        3 => {
            let Some(net) = crate::loader::driver_service_of(TEST_NET_SERVICE_ID) else {
                // Kein zweiter Treiber -> nicht anwendbar. **Nicht** bestanden: die Zeile sagt
                // das gleich selbst, und `all_done()` haengt an DMAISO_OK.
                DMAISO_OK.store(true, Ordering::Release);
                DRV_STEP.store(9, Ordering::Release);
                return;
            };
            if system::notification_pending(net.ntfn) & crate::loader::DRIVER_NTFN_BADGE == 0 {
                return; // die Netz-PD hat ihr Geraet noch nicht aufgeloest
            }
            let Some((victim_phys, _)) = system::driver_assignment(TEST_BLK_PROGRAM_ID) else {
                DMAISO_OK.store(true, Ordering::Release);
                DRV_STEP.store(9, Ordering::Release);
                return;
            };
            // Die **Gerätesicht** der fremden Region -- nicht ihre Physadresse. Ein Deskriptor
            // traegt IOVAs; die PA dort einzutragen waere ein anderer Fehler und ein anderer Test.
            let mut fenster = [(0u32, 0u32, 0u64, 0u64); system::MAX_OFFERED_DEVICES];
            let n = system::driver_windows(&mut fenster);
            let Some(&(_, _, fremd_iova, _)) =
                fenster.iter().take(n).find(|w| w.0 == TEST_BLK_PROGRAM_ID)
            else {
                DMAISO_OK.store(true, Ordering::Release);
                DRV_STEP.store(9, Ordering::Release);
                return;
            };
            // Der **unabhaengige Zeuge**: das erste Wort der fremden Region vor dem Versuch. Dass
            // der Treiber "nichts bekommen" meldet, ist eine Aussage des Angreifers ueber sich
            // selbst; dass beim Opfer nichts ankam, sieht nur, wer beim Opfer nachsieht.
            // SAFETY: identity-gemappte, vom Kernel ausgeschnittene DMA-Region; nur lesend.
            let vorher = unsafe { core::ptr::read_volatile(victim_phys as *const u64) };
            NET_VICTIM[0].store(vorher, Ordering::Release);
            NET_FOREIGN_IOVA.store(fremd_iova, Ordering::Release);
            let _ = hal::iommu::drain_faults(); // Zaehler auf null, s. o.

            let Ok(ep_cap) = system::install_endpoint_cap(net.ep as u32, Rights::WRITE) else {
                return;
            };
            let Some(cli_pd) = system::create_pd() else { return };
            if !system::install_pd_cap(cli_pd, 0, ep_cap) {
                return;
            }
            let Some(cli) = system::spawn(net_client as *const () as usize, 0, system::IDLE_PRIO)
            else {
                return;
            };
            system::bind_pd(cli_pd, cli);
            DRV_STEP.store(4, Ordering::Release);
        }
        4 => {
            if !NET_DONE.load(Ordering::Acquire) {
                return;
            }
            let victim_phys = system::driver_assignment(TEST_BLK_PROGRAM_ID).map(|(p, _)| p);
            // SAFETY: wie oben.
            let nachher =
                victim_phys.map_or(0, |p| unsafe { core::ptr::read_volatile(p as *const u64) });
            NET_VICTIM[1].store(nachher, Ordering::Release);
            NET_FAULTS.store(hal::iommu::drain_faults(), Ordering::Release);
            // **Das Urteil faellt HIER, nicht im Bericht.** `DMAISO_OK` steht in `all_done()` --
            // was der Bericht setzt, kann den Bericht nicht ausloesen. Genau das ist in diesem
            // Projekt schon zweimal an einem Tag passiert (A-6.1): der Lauf lief in den Watchdog
            // und druckte das Ergebnis trotzdem, und im Log sah das aus wie "gruen, aber gehangen".
            let f = |a: &[core::sync::atomic::AtomicU64; 4], i: usize| a[i].load(Ordering::Acquire);
            // 1. **Positivkontrolle**: derselbe Treiber, dasselbe Geraet, dieselbe Kette -- nur
            //    die eine Adresse ist die eigene. Ohne sie belegt der Rest nichts.
            let self_ok = f(&NET_SELF, 0) == 1 && f(&NET_SELF, 3) == 1;
            // 2. Der Fremdversuch liefert **keine Daten**.
            //
            //    Bewusst NICHT ueber `rx_used`. Das Bit kommt aus dem used-Ring und sagt, dass das
            //    Geraet den Deskriptor ABGEARBEITET hat -- gemessen ist es auch dann 1, wenn die
            //    Schreibung an der IOMMU scheiterte (QEMU legt den Puffer trotzdem zurueck). "Das
            //    Geraet hat gehandelt" und "die Daten sind angekommen" sind zwei verschiedene
            //    Aussagen, und nur die zweite gehoert hierher.
            let fremd_daten = f(&NET_FOREIGN, 3) == 1;
            // 3. Das **Opfer** ist unberuehrt -- am Ort, an den der Rahmen gegangen waere
            //    (`rx_buf_dev + 0`, also die Basis der fremden Region). Das prueft der Kernel
            //    selbst nach; die Meldung des Angreifers ist eine Aussage ueber ihn.
            let opfer_unberuehrt = NET_VICTIM[0].load(Ordering::Acquire) == nachher;
            // 4. Und ein **aktiver** Beleg, dass geblockt wurde: mindestens ein VT-d-Fault. Ohne
            //    ihn waere "keine Antwort" auch mit einem stummen Gegenueber vereinbar -- dann
            //    haette der Test nichts ueber die Isolierung gesagt, sondern ueber das Netz.
            let geblockt = NET_FAULTS.load(Ordering::Acquire) > 0;
            if self_ok {
                DMAISO_STATE.store(2, Ordering::Release);
                DMAISO_OK.store(!fremd_daten && opfer_unberuehrt && geblockt, Ordering::Release);
            } else {
                // Nicht messbar -- und deshalb kein Urteil. `all_done()` darf daran nicht
                // haengenbleiben, sonst wuerde aus "konnte nicht messen" ein Watchdog.
                DMAISO_STATE.store(1, Ordering::Release);
                DMAISO_OK.store(true, Ordering::Release);
            }
            DRV_STEP.store(9, Ordering::Release);
        }
        _ => {}
    }
}


// --- A-5.4: der Kreuz-DMA-Nachweis -------------------------------------------------------------
//
// Die Aussage, um die es geht: **das Geraet des einen Treibers kann nicht in die DMA-Region des
// anderen schreiben.** Bis A-5.3 war sie unpruefbar, weil immer nur eine Treiber-PD lief -- der
// Fall, der sie widerlegen koennte, kam gar nicht vor. Dieselbe Form wie `virtio-rng` vor A-5.2.
//
// Gemessen wird mit ZWEI Anfragen an dieselbe Netz-PD, und die erste ist die wichtigere:
//
//   1. `OP_SELF`    -- eine echte ARP-Transaktion in der EIGENEN Region. **Positivkontrolle.**
//                      Ohne sie hiesse "der Fremdzugriff kam nicht an" nur, dass nichts lief.
//   2. `OP_FOREIGN` -- derselbe Ablauf, aber der Empfangspuffer liegt unter der Gerätesicht des
//                      BLOCK-Treibers. Genau eine Adresse wandert; Ringe und Sendepuffer bleiben
//                      in der eigenen Region, damit das Geraet weiter laufen KANN.
//
// Warum die Netz-PD die fremde IOVA ueberhaupt genannt bekommt: eine Adresse zu **kennen** hilft
// nicht, wenn der Uebersetzungskontext des Geraets sie nicht aufloest -- und genau das ist die
// Messung. Ein Angreifer, der die Adresse raten muesste, bewiese nur, dass Raten schwer ist.
const OP_NET_SELF: u64 = 1;
const OP_NET_FOREIGN: u64 = 2;
/// `program_id` der Netz-PD im Manifest der Lade-Suite (s. `TEST_BLK_SERVICE_ID`).
const TEST_NET_SERVICE_ID: u32 = 5;
/// Ergebnisse: `[features_ok, tx_used, rx_used, arp_reply]` je Anfrage.
static NET_SELF: [core::sync::atomic::AtomicU64; 4] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; 4];
static NET_FOREIGN: [core::sync::atomic::AtomicU64; 4] =
    [const { core::sync::atomic::AtomicU64::new(u64::MAX) }; 4];
/// Die fremde Gerätesicht, die dem Netztreiber genannt wird — vom Kernel gesetzt, bevor der
/// Client laeuft.
static NET_FOREIGN_IOVA: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// VT-d-Faults, die waehrend des Fremdversuchs anfielen.
static NET_FAULTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Das erste Wort der fremden Region vor und nach dem Versuch — der **unabhaengige** Zeuge.
static NET_VICTIM: [core::sync::atomic::AtomicU64; 2] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 2];
static NET_DONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static DMAISO_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Wie das Ergebnis zu lesen ist: 0 = nicht gelaufen, 1 = **nicht messbar** (die Positivkontrolle
/// trug nicht), 2 = geurteilt.
///
/// Der Unterschied ist nicht kosmetisch. Traegt die Positivkontrolle nicht -- weil kein Gegenueber
/// antwortet, weil die Netzkarte fehlt --, dann belegt "der Fremdzugriff kam nicht an" **nichts**:
/// es kam ueberhaupt nichts an. Das als FAILURES zu melden zeigte auf die Isolierung statt auf den
/// Aufbau; als ALL PASS zu melden waere schlimmer. Also ein eigener Ausgang.
static DMAISO_STATE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Client-Thread der Netz-PD: Positivkontrolle, dann der Fremdversuch.
#[cfg(feature = "selftest")]
extern "C" fn net_client(_arg: usize) -> ! {
    let store = |slot: &[core::sync::atomic::AtomicU64; 4], r: sel4lake_hal::syscall::Ret| {
        for (a, v) in slot.iter().zip(r.msg) {
            a.store(if r.result == result::OK { v } else { u64::MAX }, Ordering::Release);
        }
    };
    store(&NET_SELF, invoke(sys::CALL, 0, [OP_NET_SELF, 0, 0, 0], 0));
    let fremd = NET_FOREIGN_IOVA.load(Ordering::Acquire);
    store(&NET_FOREIGN, invoke(sys::CALL, 0, [OP_NET_FOREIGN, fremd, 0, 0], 0));
    NET_DONE.store(true, Ordering::Release);
    loop {
        invoke(sys::YIELD, 0, [0; 4], 0);
    }
}

/// Badge, mit dem der Treiber "ich bin bereit" meldet (`B_READY` in seinem Quelltext).
const DRV_READY_BADGE: u64 = crate::loader::DRIVER_NTFN_BADGE;

/// Client-Thread: fragt den Treiber-Dienst, wartet auf den Austausch, fragt noch einmal.
#[cfg(feature = "selftest")]
extern "C" fn drv_client(_arg: usize) -> ! {
    let store = |slot: &[core::sync::atomic::AtomicU64; 4], m: [u64; 4]| {
        for (a, v) in slot.iter().zip(m) {
            a.store(v, Ordering::Release);
        }
    };
    let r1 = invoke(sys::CALL, 0, [OP_READ, MAGIC_LBA, 1, 0], 0);
    store(&DRV_R1, [r1.result, r1.msg[1], r1.msg[2], r1.msg[3]]);
    // `msg[0]` traegt den Dienststatus, `result` den der IPC. Beide gehoeren in die Bewertung:
    // eine geglueckte IPC mit Fehlerstatus ist etwas anderes als eine gescheiterte IPC.
    DRV_R1[0].store(if r1.result == result::OK { r1.msg[0] } else { u64::MAX }, Ordering::Release);
    DRV_CALL1_DONE.store(true, Ordering::Release);
    while !DRV_SWAPPED.load(Ordering::Acquire) {
        invoke(sys::YIELD, 0, [0; 4], 0);
    }
    let r2 = invoke(sys::CALL, 0, [OP_READ, MAGIC_LBA, 1, 0], 0);
    store(&DRV_R2, [r2.result, r2.msg[1], r2.msg[2], r2.msg[3]]);
    DRV_R2[0].store(if r2.result == result::OK { r2.msg[0] } else { u64::MAX }, Ordering::Release);

    // --- A-6.1: die volle Protokollfolge, an der NEUEN Fassung ------------------------------
    //
    // Sie laeuft nach dem Austausch, und das ist kein Zufall: was hier durchgeht, geht durch
    // einen Dienst, der eben ausgetauscht wurde. Ein Protokoll, das nur die erste Fassung
    // bedient, waere kein Dienst, sondern ein Startvorgang.
    let st = |r: sel4lake_hal::syscall::Ret| if r.result == result::OK { r.msg[0] } else { u64::MAX };
    // 1. Auskunft: Kapazitaet, Hoechstzahl je Anfrage, Sektorgroesse.
    let info = invoke(sys::CALL, 0, [OP_INFO, 0, 0, 0], 0);
    DRV_SEQ[0].store(st(info), Ordering::Release);
    DRV_SEQ[1].store(info.msg[1], Ordering::Release); // Kapazitaet
    DRV_SEQ[2].store(info.msg[3], Ordering::Release); // Sektorgroesse
    // 2. Den ersten Sektor der ersten Partition lesen -- dort steht die Magie.
    let rd0 = invoke(sys::CALL, 0, [OP_READ, MAGIC_LBA, 1, 0], 0);
    DRV_SEQ[3].store(st(rd0), Ordering::Release);
    // 3. Denselben Puffer auf einen ANDEREN Sektor schreiben. Das ist der Schreibpfad mit
    //    ECHTEN Daten -- kein erfundenes Muster, sondern was gerade von der Platte kam.
    let wr = invoke(sys::CALL, 0, [OP_WRITE, PROBE_SECTOR, 1, 0], 0);
    DRV_SEQ[4].store(st(wr), Ordering::Release);
    // 4. Dauerhaft machen. Ohne Flush ist "geschrieben" eine Aussage ueber einen Puffer.
    let fl = invoke(sys::CALL, 0, [OP_FLUSH, 0, 0, 0], 0);
    DRV_SEQ[5].store(st(fl), Ordering::Release);
    // 5. **Die Partitionstabelle lesen** (A-6.2). Sie laeuft VOR dem Rueckleseschritt, damit der
    //    Puffer des Treibers am Ende die Magie enthaelt und nicht die Eintragsliste -- der
    //    Kernel prueft ihn (`drv`), und ein Test, der seine eigene Vorbedingung zerstoert,
    //    meldet einen Fehler, den es nicht gibt.
    let sc = invoke(sys::CALL, 0, [OP_SCAN, 0, 0, 0], 0);
    DRV_SEQ[8].store(st(sc), Ordering::Release);
    DRV_SEQ[9].store(sc.msg[1], Ordering::Release);  // belegte Eintraege
    DRV_SEQ[10].store(sc.msg[2], Ordering::Release); // erste LBA
    DRV_SEQ[11].store(sc.msg[3], Ordering::Release); // Sektorzahl
    // 6. Zurueck lesen. Erst DAS belegt, dass geschrieben wurde -- ein quittiertes Schreiben
    //    ist eine Quittung, keine Daten.
    let rb = invoke(sys::CALL, 0, [OP_READ, PROBE_SECTOR, 1, 0], 0);
    DRV_SEQ[6].store(if st(rb) == 0 { rb.msg[1] } else { u64::MAX }, Ordering::Release);
    // 7. Und der Fall, der abgewiesen gehoert: ein Sektor jenseits der Platte. Ein Dienst, der
    //    nur den gueltigen Fall kann, hat keine Bereichspruefung -- er hatte bloss Glueck.
    let oob = invoke(sys::CALL, 0, [OP_READ, info.msg[1], 1, 0], 0);
    DRV_SEQ[7].store(st(oob), Ordering::Release);

    DRV_CALL2_DONE.store(true, Ordering::Release);
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

// --- Z4 Stufe 2: ein Thread ueber die BOOTGRENZE ------------------------------------------------
//
// Z4a haelt einen Thread an, Z4b sagt, was mitwandern darf. Was fehlte, war die Grenze, ueber die
// etwas wandert. Hier ist es die **Maschinengrenze in der Zeit**: derselbe Rechner, zwei Laeufe,
// dazwischen ein Neustart. Alles, was der Kernel im RAM haelt, ist weg; was bleibt, ist die Platte.
//
// **Derselbe Kernel macht beides und entscheidet selbst welches.** Kein Flag, keine zweite
// Konfiguration: er liest den Sektor und sieht, was dort steht. Ein Schalter waere bequemer und
// waere zugleich der Fehler -- ein Kernel, dem man sagen muss, ob er wiederherstellen soll, kann
// nicht merken, dass der Checkpoint gar nicht zu ihm gehoert.
//
// Vier Ausgaenge, und der vierte ist der wichtigste:
//
//   1. **Kein Checkpoint** (leerer Sektor) -> Lauf 1: einfrieren, speichern.
//   2. **Passt alles**                     -> Lauf 2: wiederherstellen.
//   3. **Struktur kaputt**                 -> abweisen, mit Code.
//   4. **Magie ja, Kernel-Hash NEIN**      -> ABWEISEN. Das ist Z4f in klein: ein Zustand, der in
//      eine Umgebung mit anderen Zusicherungen wandert, ist schlimmer als einer, der gar nicht
//      wandert -- denn niemand merkt es.
//
// **Der Transport ist der, den es schon gibt.** Der Blockdienst (A-6.1) nimmt `OP_WRITE` und
// kopiert die geteilte Uebertragungsflaeche auf die Platte; der Kernel legt seine Bytes hinein und
// ruft wie jeder andere Client. Ein eigener Speicherpfad im Kern waere ein zweiter Treiber und
// genau das, was A-5.1 abgeschafft hat.

/// **Der Sektor, auf dem der Checkpoint liegt.**
///
/// AUSSERHALB beider Partitionen und ausserhalb der GPT-Sicherungskopie: `tools/mkgpt.py` legt
/// Partition 1 auf 34..20000, Partition 2 auf 20001..32700, die Sicherungs-Eintraege auf
/// 32735..32766 und den Sicherungskopf auf 32767. Frei bleibt 32701..32734.
///
/// Das ist keine Kosmetik: die A-6-Pruefungen lesen GPT und FAT16 aus demselben Abbild, und
/// `tools/checkfat.py` liest es unabhaengig nach. Ein Checkpoint in einer Partition machte aus
/// einer bestandenen Dateisystempruefung eine, die zufaellig noch durchgeht.
const CKPT_SECTOR: u64 = 32710;

/// Was der Lauf mit dem Checkpoint gemacht hat.
const CKPT_SKIP: u32 = 0;
const CKPT_SAVED: u32 = 1;
const CKPT_RESTORED: u32 = 2;
const CKPT_REJECTED: u32 = 3;
const CKPT_ERROR: u32 = 4;

/// Die PD des Checkpoint-Subjekts (+1; 0 = keine). Ihr Cspace ist der **Umfang** (Z4b).
#[cfg(feature = "selftest")]
static CKPT_PD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(feature = "selftest")]
static CKPT_STATE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(CKPT_SKIP);
/// Ist die Folge durch? **Ein Fertig-Merker, kein Urteil** — er steht in `all_done()`, damit der
/// Bericht nicht vor der Platten-E/A kommt. Das Urteil steht dort ausdruecklich NICHT (s.
/// `ckpt_bericht`).
#[cfg(feature = "selftest")]
static CKPT_DONE: AtomicBool = AtomicBool::new(false);
/// **Der GEFUNDENE Zustand** (nur im Wiederherstellungsfall): Fortschritt, Nonce, Epoche.
#[cfg(feature = "selftest")]
static CKPT_PROGRESS: AtomicU64 = AtomicU64::new(0);
/// Ein Wert, den der schreibende Lauf **erst erzeugt** hat (Zyklenzaehler beim Einfrieren).
///
/// Ohne ihn waere „Lauf 2 fand den Wert aus Lauf 1" nicht von „im Puffer standen zufaellig die
/// richtigen Bytes" zu unterscheiden.
#[cfg(feature = "selftest")]
static CKPT_NONCE: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static CKPT_EPOCH: AtomicU64 = AtomicU64::new(0);
/// **Der GESCHRIEBENE Zustand.** Jeder Lauf, der durchkommt, schreibt einen -- auch der, der
/// gerade wiederhergestellt hat.
///
/// **Warum ueberhaupt:** der Fortschritt allein belegt nicht, dass er geerbt wurde. Gemessen
/// (2026-08-02, KVM): der Kernel erreicht an derselben Stelle des Hochlaufs 133, 148 und 151
/// Runden -- eine Streuung von rund 18 -- und ein Mutationslauf, in dem gar nichts
/// wiederhergestellt wurde, traf den gespeicherten Wert **exakt**. Ein reproduzierbarer Wert kann
/// nicht belegen, dass er von woanders kam.
///
/// Deshalb waechst der Zustand ueber die Kette: jeder Lauf laesst den wiederhergestellten Thread
/// noch **mindestens** [`CKPT_MIN_DELTA`] Runden arbeiten und speichert erst dann. Ab dem zweiten
/// Glied liegt der gespeicherte Wert damit strukturell ausserhalb dessen, was ein Lauf ohne
/// Wiederherstellung erreicht -- und die Frage „geerbt oder selbst gezaehlt?" ist entscheidbar.
#[cfg(feature = "selftest")]
static CKPT_S_PROGRESS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static CKPT_S_NONCE: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static CKPT_S_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Um so viele Runden muss der Thread nach dem Wiederherstellen weitergekommen sein, bevor
/// gespeichert wird. Rund das Fuenffache der gemessenen Lauf-zu-Lauf-Streuung.
#[cfg(feature = "selftest")]
const CKPT_MIN_DELTA: u64 = 100;
/// Hat der Thread den Zuwachs wirklich erreicht — oder lief nur die Notschranke ab?
#[cfg(feature = "selftest")]
static CKPT_DELTA_OK: AtomicBool = AtomicBool::new(false);
/// Der Zaehlerstand VOR dem Wiederherstellen — die Positivkontrolle der Suite: war ueberhaupt
/// etwas zu messen, oder waren gefundener und eigener Wert zufaellig gleich?
#[cfg(feature = "selftest")]
static CKPT_BEFORE: AtomicU64 = AtomicU64::new(0);
/// Zaehlerstand beim Beginn des Wartens + Tick-Notschranke dafuer.
#[cfg(feature = "selftest")]
static CKPT_WAIT_BASE: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "selftest")]
static CKPT_WAIT_UNTIL: AtomicU64 = AtomicU64::new(0);
/// Diagnosecode des Lesens (s. `ckpt_code`).
#[cfg(feature = "selftest")]
static CKPT_DECODE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Klassifizierte Caps und ihre Vorbedingungen (Z4b im Erzeugnis).
#[cfg(feature = "selftest")]
static CKPT_CAPS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);
#[cfg(feature = "selftest")]
static CKPT_PRECOND: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Laenge des geschriebenen Checkpoints in Byte.
#[cfg(feature = "selftest")]
static CKPT_BYTES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// **Die Negativkontrolle, die in JEDEM Lauf mitlaeuft:** `(Slot, Grund)` der Cap, an der das
/// Speichern der TREIBER-PD scheitert. `u32::MAX` = nicht gelaufen.
#[cfg(feature = "selftest")]
static CKPT_REFUSED: [core::sync::atomic::AtomicU32; 2] =
    [const { core::sync::atomic::AtomicU32::new(u32::MAX) }; 2];
/// Wurde das Subjekt wirklich eingefroren, bevor sein Zustand angefasst wurde?
#[cfg(feature = "selftest")]
static CKPT_FROZEN: AtomicBool = AtomicBool::new(false);
/// Der Zaehlerstand unmittelbar NACH dem Anfassen — die Vergleichsbasis fuer „er laeuft weiter".
#[cfg(feature = "selftest")]
static CKPT_AFTER: AtomicU64 = AtomicU64::new(0);
/// Statuszeilen der Platten-E/A: `[READ, WRITE, FLUSH]`.
#[cfg(feature = "selftest")]
static CKPT_IO: [AtomicU64; 3] = [const { AtomicU64::new(u64::MAX) }; 3];
/// Auftrag an den Checkpoint-Client (0 = nichts, 1 = lesen, 2 = schreiben+flush, 3 = enden).
#[cfg(feature = "selftest")]
static CKPT_REQ: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Was der Client davon erledigt hat.
#[cfg(feature = "selftest")]
static CKPT_ACK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Tick-Schranke fuer das Einfrieren (`StillRunning` ist voruebergehend — aber nicht ewig).
#[cfg(feature = "selftest")]
static CKPT_FREEZE_DEADLINE: AtomicU64 = AtomicU64::new(0);

/// **Client-Thread des Checkpoints.** Er tut genau das, was jeder andere Client tut: rufen.
///
/// Er laeuft als eigener Faden und nicht in der Zustandsmaschine, weil `CALL` **blockiert** — die
/// Hauptschleife des Hochlaufs darf das nicht. Die Auftraege kommen ueber `CKPT_REQ`, damit der
/// Kernel zwischen Lesen und Schreiben entscheiden kann, was ueberhaupt geschrieben wird.
#[cfg(feature = "selftest")]
extern "C" fn ckpt_client(_arg: usize) -> ! {
    let st = |r: sel4lake_hal::syscall::Ret| if r.result == result::OK { r.msg[0] } else { u64::MAX };
    loop {
        let req = CKPT_REQ.load(Ordering::Acquire);
        let ack = CKPT_ACK.load(Ordering::Acquire);
        if req == 3 {
            break;
        }
        if req == 1 && ack < 1 {
            CKPT_IO[0].store(st(invoke(sys::CALL, 0, [OP_READ, CKPT_SECTOR, 1, 0], 0)), Ordering::Release);
            CKPT_ACK.store(1, Ordering::Release);
        } else if req == 2 && ack < 2 {
            CKPT_IO[1].store(st(invoke(sys::CALL, 0, [OP_WRITE, CKPT_SECTOR, 1, 0], 0)), Ordering::Release);
            // **Ohne Flush ist „geschrieben" eine Aussage ueber einen Puffer.** Ueber eine
            // Bootgrenze ist das der ganze Unterschied: was nur im Geraetecache steht, ist beim
            // naechsten Start nicht da.
            CKPT_IO[2].store(st(invoke(sys::CALL, 0, [OP_FLUSH, 0, 0, 0], 0)), Ordering::Release);
            CKPT_ACK.store(2, Ordering::Release);
        } else {
            invoke(sys::YIELD, 0, [0; 4], 0);
        }
    }
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Diagnosecode fuer den Ausgang des Lesens — eine Zahl je Ursache, kein Sammel-Fehlschlag.
#[cfg(feature = "selftest")]
fn ckpt_code(e: &sel4lake_cap::checkpoint::ImageError) -> u32 {
    use sel4lake_cap::checkpoint::ImageError as E;
    match e {
        E::NoImage => 1,
        E::Version { .. } => 2,
        E::Truncated => 3,
        E::CapOverflow => 4,
        E::UnknownCapTag { .. } => 5,
        E::Checksum => 6,
        E::ForeignKernel => 7,
        E::BufferTooSmall => 8,
    }
}

/// Grund-Code fuer eine verweigerte Cap (`LocalReason`) — dieselbe Form, aus demselben Grund.
#[cfg(feature = "selftest")]
fn ckpt_reason(r: sel4lake_cap::checkpoint::LocalReason) -> u32 {
    use sel4lake_cap::checkpoint::LocalReason as L;
    match r {
        L::DeviceWindow => 1,
        L::InterruptLine => 2,
        L::DmaRegion => 3,
        L::PendingReply => 4,
        L::PeerNotInScope => 5,
        L::ThreadNotInScope => 6,
        L::PdNotInScope => 7,
        L::LoaderSource => 8,
    }
}

/// Die Magie, die die Testsuiten in Sektor 0 des Plattenabbilds legen ("SEL4LAKE", LE).
///
/// Warum ueberhaupt eine: ein Puffer voller Nullen ist von einem nie beschriebenen Puffer nicht
/// zu unterscheiden, und ein frisches Abbild besteht genau daraus. Ein Test, der nur "das Geraet
/// hat geantwortet" prueft, waere auch dann gruen, wenn der Datenpfad gar nichts uebertraegt.
const BLK_MAGIC: u64 = 0x454B_414C_344C_4553;

/// LBA, auf der die Magie liegt.
///
/// Die Testabbilder sind **echte GPTs** mit ZWEI Partitionen (A-6.2/A-6.3): die erste traegt ein
/// FAT16, die zweite ist roh. Die Magie liegt in der **zweiten** -- in die erste zu schreiben
/// hiesse, den Bootsektor des Dateisystems zu ueberschreiben. Zwei Tests, die sich dieselbe
/// Flaeche teilen, sind ein Riss, durch den beide fallen koennen.
///
/// **Beide** Suiten benutzen dasselbe Abbild-Werkzeug (`tools/mkgpt.py`). Zwei Suiten, die
/// dieselbe Platte verschieden aufsetzen, waeren derselbe Riss wie zwei, die dasselbe Geraet
/// verschieden aufsetzen -- das hat dieses Projekt am 2026-08-01 schon einmal bezahlt.
const MAGIC_LBA: u64 = 20001;

/// Adressen der Testumgebung fuer den ARP-Austausch (QEMU `-netdev user`): der eingebaute
/// Gateway liegt auf 10.0.2.2, der Gast bekommt 10.0.2.15.
///
/// Sie stehen **hier** und nicht im Treiber: `sel4lake-virtio` bekommt sie hereingereicht. Ein
/// Treiber, der die Adressen seiner Testumgebung kennt, ist keiner mehr -- und genau diese
/// Kenntnis ist das, was mit A-5.1 in die Treiber-PD bzw. deren Manifest wandert.
#[cfg(feature = "selftest")]
const NET_SRC_IP: [u8; 4] = [10, 0, 2, 15];
#[cfg(feature = "selftest")]
const NET_GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];

/// Eine vollstaendige virtio-rng-Anfrage auf einer frisch belegten Virtqueue-Region.
///
/// Gibt `(caps_gefunden, used_ring_fortgeschritten, gemeldete_bytes)`. Die Region wird hier
/// belegt und wieder freigegeben -- der Aufrufer soll sich um nichts kuemmern muessen ausser um
/// die Frage, die er stellt.
#[cfg(feature = "selftest")]
fn virtio_versuch(dev: Option<&hal::pcie::PciDevice>, max_poll: u64) -> (bool, bool, u32) {
    let Some(d) = dev else { return (false, false, 0) };
    let Some(cap) = system::alloc(4096, 4096) else { return (false, false, 0) };
    let base = cap.base();
    let r = match hal::virtio::probe(d) {
        // SAFETY: das BAR-Fenster unter 4 GiB ist identity-gemappt (`map_device_block_global`),
        // und die Virtqueue-Region stammt aus dem Allokator, gehoert also exklusiv uns.
        Some(rng) => {
            let (adv, w) = unsafe {
                rng.request_polled(base, base, base + hal::virtio::DATA_OFFSET, max_poll)
            };
            (true, adv, w)
        }
        None => (false, false, 0),
    };
    system::free(cap);
    r
}

/// Eine vollstaendige virtio-blk-Leseanfrage auf einer frisch belegten Region (A-5.2).
///
/// `None` heisst "kein Blockgeraet am Bus" -- ausdruecklich etwas anderes als ein Fehlschlag,
/// s. die Meldung an der Aufrufstelle.
#[cfg(feature = "selftest")]
fn virtio_blk_versuch(
    dev: Option<&hal::pcie::PciDevice>,
    max_poll: u64,
) -> Option<hal::virtio::blk::BlkResult> {
    let d = dev?;
    // **Diese Region sieht ein GERAET** (Bus-Master-DMA, vor dem VT-d-Aufbau als rohe
    // Physadresse). `system::alloc` steht hier als die vorsichtige Wahl -- **nicht**, weil das
    // Geraet oben nicht hinkaeme: gemessen (E-Rest 3d, `-m 3G`, Region oberhalb 4 GiB) liest
    // virtio-blk den Sektor korrekt, `Geraet-DMA=1`. Die Vermutung „das Geraet erreicht nur
    // GiB 0" war also falsch, und sie steht hier, damit sie nicht ein zweites Mal aufkommt.
    // Konservativ bleibt es trotzdem: die 32-Bit-Faehigkeit eines Geraets ist eine Eigenschaft
    // des Geraets, und die Angebotsliste enthaelt sie nicht (s. `dmawin`:
    // `Geraete-ohne-deklarierte-Adressbreite=1`).
    let cap = system::alloc(hal::virtio::blk::REGION_BYTES, 4096)?;
    let base = cap.base();
    let r = hal::virtio::probe_blk(d).map(|blk| {
        // SAFETY: das BAR-Fenster unter 4 GiB ist identity-gemappt (`map_device_block_global`),
        // und die Region stammt aus dem Allokator, gehoert also exklusiv uns.
        //
        // **Nicht Sektor 0.** Seit A-6.2 sind die Testabbilder echte GPTs: auf LBA 0 liegt der
        // schuetzende MBR, auf 1 der Kopf, auf 2..33 die Eintragsliste. Die Magie liegt auf dem
        // ersten Sektor der ersten Partition. Wer hier weiter 0 liest, liest den MBR -- und der
        // Test faellt durch, ohne dass am Geraetepfad irgendetwas fehlt.
        unsafe { blk.read_sector(base, base, MAGIC_LBA, max_poll) }
    });
    system::free(cap);
    r
}

/// Ein vollstaendiger ARP-Austausch ueber virtio-net auf einer frisch belegten Region (A-5.2).
#[cfg(feature = "selftest")]
fn virtio_net_versuch(
    dev: Option<&hal::pcie::PciDevice>,
    max_poll: u64,
) -> Option<hal::virtio::net::NetResult> {
    let d = dev?;
    // Geraete-sichtbar wie in `virtio_blk_versuch` -- s. dort.
    let cap = system::alloc(hal::virtio::net::REGION_BYTES, 4096)?;
    let base = cap.base();
    let r = hal::virtio::probe_net(d).map(|net| {
        // SAFETY: wie in `virtio_blk_versuch`.
        unsafe { net.arp_probe(base, base, NET_SRC_IP, NET_GATEWAY_IP, max_poll) }
    });
    system::free(cap);
    r
}

/// A-4.2: haelt der ruhende Punkt -- weist ein stillgelegter Endpoint neue Transaktionen ab?
#[cfg(feature = "selftest")]
static QUIESCE_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// A-4.1: bindet der Austausch atomar um -- ohne Zustand ohne Empfaenger dazwischen?
#[cfg(feature = "selftest")]
static REBIND_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "selftest")]
static EPFULL_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// A-4.3: weist die Zustandsuebergabe ein fremdes Layout ab, statt es fehlzuinterpretieren?
#[cfg(feature = "selftest")]
static STATE_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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
fn all_done(archive: bool, warum: Option<&mut [(&'static str, bool); DONE_FLAGS]>) -> bool {
    let workers = (0..NWORKERS).all(|i| WORKER_ROUNDS[i].load(Ordering::Relaxed) >= WORK_TARGET);
    let cores = (0..system::num_cores()).all(|c| hal::timer::ticks(c) > 0);
    let ring3 = USER_SYSCALLS.load(Ordering::Relaxed) > 0 && system::el0_fault_count() > 0;
    let iso = SAS_READ_OK.load(Ordering::Relaxed) == PROBE_MAGIC && system::iso_fault_count() > 0;
    let root = !archive || (root_chain_done() && cdelete_done());
    // B-4.2 gehoert in die Abschlussbedingung, nicht bloss in den Bericht: sonst waere ein
    // Fehlschlag genau die Sorte Zeile, die niemand liest.
    let stripes = STRIPE_ALLOC_OK.load(Ordering::Acquire);
    // B-4.5 in der Abschlussbedingung, aus demselben Grund wie B-4.2 daneben.
    let pprobe = PPROBE_OK.load(Ordering::Acquire);
    let virtio = VIRTIO_OK.load(Ordering::Acquire);
    // A-5.2: `blk` und `net` gehoeren aus demselben Grund hierher wie `virtio` daneben. Sie
    // duerfen dabei SKIPpen (kein Geraet am Bus -> `true`); was sie nicht duerfen, ist
    // durchfallen, ohne dass der Lauf davon abhaengt.
    let vblk = VBLK_OK.load(Ordering::Acquire);
    let vnet = VNET_OK.load(Ordering::Acquire);
    // A-5.1: die Dienst-/Austausch-Abfolge muss DURCH sein, bevor berichtet wird. `DRIVER_OK`
    // taugt dafuer nicht -- es wird IM Bericht gesetzt und koennte ihn deshalb nicht ausloesen.
    // Ohne diese Zeile kam der Bericht, bevor der Austausch ueberhaupt anlief, und meldete
    // "nicht bereit" ueber etwas, das noch gar nicht dran gewesen war.
    let drv_seq = crate::loader::driver_service_of(TEST_BLK_SERVICE_ID).is_none()
        || DRV_CALL2_DONE.load(Ordering::Acquire);
    // Z4 Stufe 2, aus demselben Grund wie `drv_seq` daneben: ein **Fertig-Merker**, kein Urteil.
    // Die Platten-E/A des Checkpoints muss durch sein, bevor berichtet wird -- sonst meldete der
    // Bericht „nicht gelaufen" ueber etwas, das noch gar nicht dran war. Das URTEIL steht
    // ausdruecklich nicht hier (s. `ckpt_bericht`).
    let ckpt_seq = crate::loader::driver_service_of(TEST_BLK_SERVICE_ID).is_none()
        || CKPT_DONE.load(Ordering::Acquire);
    let blkdev = BLKDEV_OK.load(Ordering::Acquire);
    let part = PART_OK.load(Ordering::Acquire);
    let iface = IFACE_GATE_OK.load(Ordering::Acquire);
    // A-4.2 aus demselben Grund wie B-4.2 in der Abschlussbedingung: eine Zusicherung, die nur
    // im Bericht steht, faellt beim Brechen niemandem auf.
    let quiesce = QUIESCE_OK.load(Ordering::Acquire);
    // A-4.1 aus demselben Grund: eine Zusicherung, die nur im Bericht steht, faellt beim
    // Brechen niemandem auf.
    let rebind = REBIND_OK.load(Ordering::Acquire);
    // D11 aus demselben Grund: eine Zusicherung, die nur im Bericht steht, faellt beim Brechen
    // niemandem auf.
    let epfull = EPFULL_OK.load(Ordering::Acquire);
    // A-4.3 aus demselben Grund: eine Zusicherung, die nur im Bericht steht, faellt beim Brechen
    // niemandem auf.
    let state = STATE_OK.load(Ordering::Acquire);
    // **Die Notbremse muss sagen koennen, WORAUF sie gewartet hat.**
    //
    // Bis hierher druckte sie "nicht alle Aussagen belegt" und sonst nichts. Damit ist ein
    // Haenger von einem nicht erfuellten Kriterium nicht zu unterscheiden -- man sieht, DASS es
    // nicht fertig wurde, und muss raten, woran. Beim Bau von A-6.2 hat genau das eine Runde
    // gekostet: alle Einzelzeilen standen auf ALL PASS, der Lauf lief trotzdem in die Notbremse.
    //
    // Der Grund wird deshalb ZURUECKGEGEBEN, nicht bloss verrechnet. Kostenlos ist das nicht ganz
    // (ein `&mut` je Runde), aber die Schleife dreht ohnehin Millionen Mal ohne etwas zu tun.
    if let Some(w) = warum {
        *w = [
            ("workers", workers),
            ("ipc", IPC_DONE.load(Ordering::Acquire)),
            ("cores", cores),
            ("ring3", ring3),
            ("iso", iso),
            ("root", root),
            ("stripes", stripes),
            ("pprobe", pprobe),
            ("virtio", virtio),
            ("vblk", vblk),
            ("vnet", vnet),
            ("drv", drv_seq),
            ("bootckpt", ckpt_seq),
            ("blkdev", blkdev),
            ("dmaiso", DMAISO_OK.load(Ordering::Acquire)),
            ("part", part),
            ("iface", iface),
            ("quiesce", quiesce),
            ("rebind", rebind),
            ("epfull", epfull),
            ("state", state),
        ];
    }
    workers
        && IPC_DONE.load(Ordering::Acquire)
        && cores
        && ring3
        && iso
        && root
        && stripes
        && pprobe
        && virtio
        && vblk
        && vnet
        && drv_seq
        && ckpt_seq
        && blkdev
        && DMAISO_OK.load(Ordering::Acquire)
        && part
        && iface
        && quiesce
        && rebind
        && epfull
        && state
}

/// Wie viele Einzelaussagen [`all_done`] prueft.
#[cfg(feature = "selftest")]
const DONE_FLAGS: usize = 21;

/// Bericht + Abschaltung (das Testskript wertet die Marker aus).
///
/// **`watchdog` = wir kamen über die Notbremse hierher, nicht über [`all_done`]** (B-1.8). Der
/// Unterschied MUSS im Marker stehen: vorher druckte diese Funktion `SELFTEST COMPLETE`
/// bedingungslos, also auch nach einem Abbruch. Damit konnte ausgerechnet der Marker, auf dem die
/// ganze Wiederholungsmessung steht (B-1.2/B-1.3 zählen ihn), einen vollständigen Lauf nicht von
/// einem abgelaufenen unterscheiden — im selben Log standen `WATCHDOG` und `SELFTEST COMPLETE`
/// untereinander. Der aarch64-Zweig macht es seit jeher richtig (`SELFTEST FAILED (watchdog)`);
/// das hier ist die Spiegelung, nicht eine neue Erfindung.
#[cfg(feature = "selftest")]
fn report_and_off(watchdog: bool) -> ! {
    // **E-Rest 3b/3d, und die Zahlen gehoeren an den SCHLUSS, nicht an den Hochlauf.** Zwei
    // Zahlen, weil es zwei verschiedene Lagen sind: „PD-abbildbar musste nach oben ausweichen"
    // heisst, GiB 0 ist voll und die Region liegt jetzt dort, wo eine PD sie NICHT sehen kann --
    // das ist ein Befund. „Reiner Kernel-Speicher musste nach unten" heisst nur, dass oben nichts
    // frei war, und ist harmlos. `0` heisst in beiden Faellen „kam nicht vor", nicht „geht
    // nicht" -- ohne diese Zeile saehe ein ungefahrener Pfad genauso aus wie ein tragender.
    let (pd_miss, kern_miss) = system::zone_misses();
    println!(
        "mem     : Zonen (E-Rest 3d) -- PD-abbildbar musste {pd_miss}x nach oben ausweichen, \
         reiner Kernel-Speicher {kern_miss}x nach unten (0 heisst 'kam nicht vor', nicht \
         'geht nicht'; ein PD-Ausweich waere ein Befund, ein Kernel-Ausweich ist harmlos)"
    );
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

    // A-3.4: wie nah kam der globale Cap-Space seiner Grenze? Der Hoechststand, nicht der
    // Endstand -- s. `system::cap_peaks`. Gemeldet wird auch, wie viele PDs mit VOLLEM Budget
    // ueberhaupt hineinpassen: `CAP_BUDGET_PER_PD` deckelt den Verbrauch EINER PD, die Summe
    // traegt seit A-3.4 die Dimensionierung (`CAP_SLOTS_TOTAL`). Nachgezaehlt wird sie von
    // `capsum` weiter unten.
    let (pslots, cslots, pobjs, cobjs) = system::cap_peaks();
    let pd_voll = cslots / sel4lake_microkit::CAP_BUDGET_PER_PD;
    println!(
        "capsz   : Cap-Slots Hoechststand {pslots}/{cslots}, Objekte {pobjs}/{cobjs}; bei vollem \
         Budget ({} Slots/PD) passen {pd_voll} PDs in die globale Tabelle",
        sel4lake_microkit::CAP_BUDGET_PER_PD
    );
    let capsz_ok = pslots < cslots && pobjs < cobjs;
    println!(
        "capsz   : {} (A-3.4: der globale Cap-Space wurde NICHT erschoepft -- gemessen am \
         Hoechststand, nicht am Endstand)",
        if capsz_ok { "ALL PASS" } else { "FAILURES" }
    );

    // A-3.4 Abschluss: die SUMME. Das Budget deckelt eine PD; dass daneben noch die Wurzel-Caps
    // des Kernels in die Tabelle passen, war eine Zahl (256) mit einem Kommentar daneben. Hier
    // wird sie nachgezaehlt -- nicht durch Abziehen (zwei PDs koennen denselben CapPtr halten,
    // dann zaehlt die Summe zu hoch und die Pruefung ginge faelschlich durch), sondern durch
    // Markieren jedes von einer PD gehaltenen Slots.
    let (nonbudget, reserve, sum_ok) = system::cap_nonbudget_slots();
    if nonbudget == usize::MAX {
        println!("capsum  : Summenpruefung KONNTE NICHT LAUFEN (Zaehlflaeche zu klein)");
    } else {
        println!(
            "capsum  : {nonbudget}/{reserve} Slots ausserhalb aller PD-Budgets (Kernel-Wurzelcaps); \
             Kapazitaet {cslots} = {} PD-Budgets + {reserve} Reserve",
            sel4lake_microkit::CAP_SLOTS_FOR_ALL_PDS
        );
    }
    println!(
        "capsum  : {} (A-3.4: der Kernel bleibt in seiner Reserve -- sonst bekommt eine PD \
         INNERHALB ihres Budgets keinen Slot mehr)",
        if sum_ok { "ALL PASS" } else { "FAILURES" }
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

    // --- A-5.1: hat ein Treiber IM USERLAND sein Geraet bedient? -------------------------------
    //
    // Zwei Aussagen, absichtlich aus zwei Quellen:
    //
    //  1. **Der Treiber meldet**, dass seine Transaktion durchlief (Badge in seiner endowten
    //     Notification). Das kann nur er wissen -- er hat den used-Ring gepollt.
    //  2. **Der Kernel prueft die Bytes** in der Region, die er ihm ausgegeben hat. Das kann nur
    //     er, denn er kennt die Magie des Abbilds.
    //
    // Keine der beiden allein genuegt. Meldet nur der Treiber, glaubt man dem Code, der es
    // behauptet. Pruefen nur die Bytes, koennte sie auch jemand anders geschrieben haben.
    if let Some((dma_phys, iova)) = system::driver_assignment(TEST_BLK_PROGRAM_ID) {
        // **Die Notification DIESES Dienstes** (A-5.4). `driver_notification()` ist ein einzelnes
        // Register: die zuletzt geladene Treiber-PD ueberschreibt es. Mit zwei Treibern las der
        // Bericht deshalb die Ablage der NETZ-PD und sah dort das v2-Badge des Blockdienstes
        // nicht -- `v2 meldete bereit=0` bei einem Austausch, der sauber gelaufen war.
        // Dieselbe Falle wie 2026-08-01 zwischen Root-Task, Treiber und Client, nur eine Rolle
        // weiter: Rollen, die sich melden, brauchen getrennte Ablagen.
        let badge = crate::loader::driver_service_of(TEST_BLK_SERVICE_ID)
            .map(|s| system::notification_pending(s.ntfn))
            .unwrap_or(0);
        let v1_bereit = badge & crate::loader::DRIVER_NTFN_BADGE != 0;
        let v2_bereit = badge & crate::loader::DRIVER_V2_BADGE != 0;
        // **Der Puffer eines Treibers gehoert dem LETZTEN Client, nicht der Aussage.**
        //
        // Hier stand bis Z4 Stufe 2 ein Lesezugriff im Bericht -- und damit maass die Zeile nicht,
        // was der Blockdienst geliefert hat, sondern was zuletzt durch seinen Datenpuffer ging.
        // Solange nur `drv_client` ihn benutzte, fiel das nicht auf; sein letzter Schritt war
        // ausdruecklich so gelegt, dass die Magie am Ende dort steht (s. Schritt 5 in
        // `drv_client`). Mit einem ZWEITEN Client (dem Checkpoint) stand dort dessen Sektor, und
        // die Zeile meldete `FAILURES` fuer einen Treiber, der alles richtig gemacht hatte.
        //
        // Der Wert wird deshalb **erfasst, wenn die Aussage gilt** (Ende der Dienstfolge,
        // `drv_service_step` Schritt 2) und im Bericht nur noch gelesen. Der Momentanwert wird
        // daneben gedruckt: er ist Diagnose, kein Kriterium.
        // SAFETY: `dma_phys` ist eine vom Kernel ausgeschnittene, identity-gemappte DMA-Region.
        let jetzt = unsafe {
            core::ptr::read_volatile((dma_phys + hal::virtio::blk::OFF_DATA) as *const u64)
        };
        let erfasst = DRV_DMA_WORT.load(Ordering::Acquire);
        let wort = if erfasst != u64::MAX { erfasst } else { jetzt };
        let g = |a: &[core::sync::atomic::AtomicU64; 4]| {
            [
                a[0].load(Ordering::Acquire),
                a[1].load(Ordering::Acquire),
                a[2].load(Ordering::Acquire),
                a[3].load(Ordering::Acquire),
            ]
        };
        let (r1, r2) = (g(&DRV_R1), g(&DRV_R2));
        let reload = DRV_RELOAD.load(Ordering::Acquire);
        println!(
            "drv     : Zuteilung: DMA-Region {dma_phys:#x}, Geraetesicht {iova:#x}; \
             Sektor {wort:#018x} (erwartet {BLK_MAGIC:#018x}; Puffer jetzt {jetzt:#018x} -- er \
             gehoert dem letzten Client und ist deshalb kein Kriterium)"
        );
        println!(
            "drv     : Anfrage 1 an v1: Status={} Bytes={:#018x} Sektoren={} bedient={}",
            r1[0], r1[1], r1[2], r1[3]
        );
        println!(
            "drv     : Austausch: Ergebnis={} (0=umgebunden OHNE Empfaengerluecke, 1=umgebunden, \
             2=kein Dienst, 3=nicht geladen, 4=nicht bereit, 5=abgewiesen); v1 meldete bereit={} \
             v2 meldete bereit={}",
            reload, v1_bereit as u8, v2_bereit as u8
        );
        println!(
            "drv     : Anfrage 2 an v2: Status={} Bytes={:#018x} Sektoren={} bedient={}",
            r2[0], r2[1], r2[2], r2[3]
        );
        let q = |i: usize| DRV_SEQ[i].load(Ordering::Acquire);
        println!(
            "blkdev  : INFO Status={} Kapazitaet={} Sektorgroesse={}; READ(0)={} \
             WRITE({PROBE_SECTOR})={} FLUSH={}; Rueckgelesen={:#018x} (erwartet {BLK_MAGIC:#018x}); \
             READ(jenseits der Platte)={} (erwartet 3 = Bereich)",
            q(0), q(1), q(2), q(3), q(4), q(5), q(6), q(7)
        );
        // A-6.3: was die Dateisystem-PD in die geteilte Flaeche gelegt hat.
        if let Some((shared_phys, _)) = system::driver_shared_region(TEST_BLK_SERVICE_ID) {
            // SAFETY: identity-gemappte, vom Kernel ausgeschnittene Region; nur lesend.
            let f = |i: u64| unsafe {
                core::ptr::read_volatile((shared_phys + 4096 + i * 8) as *const u64)
            };
            println!(
                "fs      : Status={} (0=Datei gelesen, 2=nicht gefunden, 3=Kette defekt, \
                 4=Kette zu kurz, {:#x}=PD lief nicht); Groesse={} erste acht Byte={:#018x} \
                 Cluster={}",
                f(0), u64::MAX, f(1), f(2), f(3)
            );
            println!(
                "fs      : Schreiben Status={} (0=gut, 10..24=Schritt der schieflief); \
                 Rueckgelesen Status={} Groesse={} (erwartet {FS_NEU_GROESSE}) geprueft={} Byte",
                f(4), f(5), f(6), f(7)
            );
            let ok = f(0) == 0
                && f(1) == FS_DATEI_GROESSE
                && f(2) == FS_ERSTE_ACHT
                // A-6.4: geschrieben, und beim Zurueckhlesen stimmte JEDES Byte.
                && f(4) == 0
                && f(5) == 0
                && f(6) == FS_NEU_GROESSE
                && f(7) == FS_NEU_GROESSE;
            println!(
                "fs      : {} (A-6.3: ein lesendes Dateisystem als EIGENE PD -- sie faehrt kein \
                 Geraet, sondern ruft den Blockdienst ueber dessen Kanal und liest die Bytes aus \
                 der geteilten Uebertragungsflaeche. GPT und FAT16 liegen in kernfreien Crates \
                 mit forbid(unsafe_code); der Kern kennt weder Partitionen noch Dateien. A-6.4: \
                 sie SCHREIBT auch -- ueber einen zweiten Cluster hinaus, in beide FAT-Kopien, \
                 mit Flush, und liest jedes Byte zurueck)",
                if ok { "ALL PASS" } else { "FAILURES" }
            );
            FS_OK.store(ok, Ordering::Release);
        }
        let blk_ok = BLKDEV_OK.load(Ordering::Acquire);
        println!(
            "blkdev  : {} (A-6.1: das Dienstprotokoll traegt -- Auskunft, Lesen, SCHREIBEN, \
             Flush, und ein Sektor jenseits der Platte wird ABGEWIESEN statt ans Geraet \
             durchgereicht. Der Rueckleseschritt ist der eigentliche Beleg: eine quittierte \
             Schreibanfrage ist eine Quittung, keine Daten)",
            if blk_ok { "ALL PASS" } else { "FAILURES" }
        );
        // Bei einem Fehlschlag traegt `msg[1]` den GRUND, nicht die Eintragszahl. Ein Feld, das
        // je nach Status etwas anderes bedeutet, muss auch je nach Status anders beschriftet
        // werden -- sonst steht im Log "belegte Eintraege=5", wo eine kaputte Kopf-Pruefsumme
        // gemeint ist.
        if q(8) == 0 {
            println!(
                "part    : GPT-Scan Status=0 belegte Eintraege={} (erwartet \
                 {ERWARTET_PARTITIONEN}); erste Partition LBA {} ueber {} Sektoren (erwartet \
                 {ERWARTET_ERSTE_LBA} / {ERWARTET_ERSTE_SEKTOREN})",
                q(9), q(10), q(11)
            );
        } else {
            println!(
                "part    : GPT-Scan Status={} ABGEWIESEN, Grund={} (1=zu kurz 2=Signatur \
                 3=Revision 4=Kopfgroesse 5=Kopf-CRC 6=Eintragsgroesse 7=Eintragszahl \
                 8=Eintrags-CRC)",
                q(8), q(9)
            );
        }
        println!(
            "part    : {} (A-6.2: die Partitionstabelle wird im BLOCKDIENST gelesen, nicht im \
             Kern -- `sel4lake-part` ist abhaengigkeitsfrei, ohne unsafe und host-getestet. \
             Geprueft werden beide Pruefsummen; die Eintragsliste passt nicht in eine Anfrage und \
             wird stueckweise gelesen, die Pruefsumme aber ueber das GANZE gebildet)",
            if PART_OK.load(Ordering::Acquire) { "ALL PASS" } else { "FAILURES" }
        );
        // Was hier zusammenkommen MUSS, und warum jedes Stueck einzeln zaehlt:
        //  * beide Anfragen liefen und lieferten die Magie  -> der Dienst bedient wirklich
        //  * `bedient` zaehlte 1 -> 2                       -> v2 hat die REGION geerbt, nicht
        //                                                      eine frische bekommen
        //  * v1 UND v2 meldeten sich bereit                 -> es waren zwei Fassungen
        //  * Austausch ohne Empfaengerluecke                -> A-4.1 am echten Dienst, nicht am
        //                                                      lokalen Objekt
        //  * IOVA != PA                                     -> die Gerätesicht ist eine eigene Achse
        let ok = r1[0] == 0
            && r2[0] == 0
            && r1[1] == BLK_MAGIC
            && r2[1] == BLK_MAGIC
            // **Relativ, nicht absolut.** Vor dieser Folge hat die Dateisystem-PD den Dienst
            // schon benutzt; ein fester Startwert waere eine Aussage ueber die Reihenfolge
            // aller Clients statt ueber den Austausch.
            && r1[3] >= 1
            && r2[3] == r1[3] + 1
            && v1_bereit
            && v2_bereit
            && reload == 0
            && wort == BLK_MAGIC
            && iova != 0
            && iova != dma_phys;
        println!(
            "drv     : {} (A-5.1: ein Treiber als DIENST ausserhalb des Kerns -- er loest sein \
             Geraet auf seiner EIGENEN Konfigurationsraum-Seite auf, wartet auf Anfragen und \
             liest Sektoren per Bus-Master-DMA. Der Kern hat enumeriert, zugeteilt und den \
             Empfaenger AUSGETAUSCHT -- ohne einen virtio-Schritt und ohne zu wissen, was der \
             Dienst treibt. Der Zaehler 1 -> 2 belegt, dass die neue Fassung dieselbe Region \
             geerbt hat)",
            if ok { "ALL PASS" } else { "FAILURES" }
        );
        DRIVER_OK.store(ok, Ordering::Release);
    } else {
        println!(
            "drv     : SKIP (keine Zuteilung erfolgt -- entweder war kein Geraet da, oder es \
             gibt keine Treiber-PD, die eines anfordert: ohne Boot-Archiv laeuft kein geladenes \
             Programm, und ohne Manifest-Eintrag mit mmio,dma verlangt keines Geraete-Autoritaet)"
        );
        DRIVER_OK.store(true, Ordering::Release); // nicht anwendbar, nicht bestanden
        println!("blkdev  : SKIP (kein Treiber-Dienst -- s. die Begruendung eine Zeile darueber)");
        println!("part    : SKIP (kein Treiber-Dienst -- s. die Begruendung zwei Zeilen darueber)");
        println!("fs      : SKIP (kein Treiber-Dienst -- s. die Begruendung drei Zeilen darueber)");
    }
    // --- A-5.4: die eigentliche Mandantenaussage -------------------------------------------
    //
    // Drei Zahlen, und keine davon reicht allein:
    //   * die **Positivkontrolle** (`OP_SELF`) muss getragen haben -- sonst hiesse "nichts kam an"
    //     nur, dass nichts lief;
    //   * der **Fremdversuch** darf keine Daten geliefert haben;
    //   * das **Opfer** darf sich nicht geaendert haben -- und das sieht nur, wer beim Opfer
    //     nachschaut. Dass der Angreifer "nichts bekommen" meldet, ist eine Aussage ueber ihn.
    //
    // Die VT-d-Faults sind Beiwerk, kein Kriterium: ein Geraet, dessen Uebersetzung fehlschlaegt,
    // MUSS keinen Fault erzeugen (der DMAR-Remapping-Modus darf still verwerfen). Sie werden
    // berichtet, weil ihre Zahl die Diagnose traegt.
    {
        let sf = |a: &[core::sync::atomic::AtomicU64; 4], i: usize| a[i].load(Ordering::Acquire);
        let (v0, v1) = (
            NET_VICTIM[0].load(Ordering::Acquire),
            NET_VICTIM[1].load(Ordering::Acquire),
        );
        let zustand = DMAISO_STATE.load(Ordering::Acquire);
        if zustand == 1 {
            println!(
                "dmaiso  : Positivkontrolle features={} tx={} rx={} arp={} -- sie traegt NICHT",
                sf(&NET_SELF, 0), sf(&NET_SELF, 1), sf(&NET_SELF, 2), sf(&NET_SELF, 3)
            );
            println!(
                "dmaiso  : SKIP -- nicht messbar: die Positivkontrolle traegt nicht (kein \
                 Gegenueber, das auf ARP antwortet, oder keine brauchbare Netzkarte). 'Der \
                 Fremdzugriff kam nicht an' belegt dann nichts -- es kam ueberhaupt nichts an. \
                 Das als FAILURES zu melden zeigte auf die Isolierung statt auf den Aufbau"
            );
        } else if zustand == 0 {
            println!(
                "dmaiso  : SKIP (kein zweiter Treiber -- ohne eine zweite Treiber-PD gibt es \
                 nichts, wovon getrennt werden koennte, und eine gruene Zeile hiesse nichts)"
            );
        } else {
            // Nur drucken -- das Urteil steht seit `drv_service_step` Schritt 4 fest.
            let ok = DMAISO_OK.load(Ordering::Acquire);
            println!(
                "dmaiso  : Positivkontrolle features={} tx={} rx={} arp={} · Fremdversuch \
                 features={} tx={} rx={} arp={} · Opfer {:#018x} -> {:#018x} · VT-d-Faults={}",
                sf(&NET_SELF, 0), sf(&NET_SELF, 1), sf(&NET_SELF, 2), sf(&NET_SELF, 3),
                sf(&NET_FOREIGN, 0), sf(&NET_FOREIGN, 1), sf(&NET_FOREIGN, 2), sf(&NET_FOREIGN, 3),
                v0, v1, NET_FAULTS.load(Ordering::Acquire)
            );
            println!(
                "dmaiso  : {} (A-5.4: das Geraet der EINEN Treiber-PD erreicht die DMA-Region der \
                 ANDEREN nicht. Die Positivkontrolle laeuft ueber denselben Treiber, dasselbe \
                 Geraet und dieselbe Deskriptorkette -- nur EINE Adresse wandert. Ohne sie waere \
                 'nichts kam an' kein Beleg, sondern die Beschreibung eines Geraets, das gar nicht \
                 laeuft. Und dass beim Opfer nichts ankam, prueft der Kernel selbst nach; die \
                 Meldung des Angreifers ist eine Aussage ueber ihn)",
                if ok { "ALL PASS" } else { "FAILURES" }
            );
        }
    }

    // --- E-Rest 3: bleibt die GETEILTE Geraete-Tabelle oberhalb 4 GiB sauber? ------------------
    //
    // Die Falle steht in CLAUDE.md und gilt fuer die neuen hohen Tabellen woertlich wie fuer
    // `ISO_PD_HIGH`: wer ein Geraetefenster in eine GETEILTE Tabelle schreibt, gibt es JEDER
    // isolierten PD -- lautlos, denn die Cap-Pruefung laeuft dabei korrekt durch. Eine Loesung,
    // die das Fenster allen PDs gibt, ist keine.
    //
    // Geprueft wird deshalb nicht die Absicht, sondern der Inhalt: in der geteilten Tabelle darf
    // ausschliesslich stehen, was der Hochlauf hineinschreibt (2-MiB-Geraeteblock, kein `US`).
    // Und weil "0 unzulaessige Eintraege" ueber nichts urteilt, solange nie eine PD ein Fenster
    // dort bekommen hat, steht die Zahl der PRIVATEN Kopien danebst: erst sie macht die Aussage
    // zu einem Testergebnis. Ohne Treiber-PD (nackte Suite) ist das ein SKIP mit Grund.
    {
        let (geteilt, privat, verstoesse) = hal::mmu::high_shared_audit();
        println!(
            "hiiso   : geteilte Geraete-Tabellen oberhalb 4 GiB={geteilt}, private Kopien fuer \
             isolierte PDs={privat}, unzulaessige Eintraege in den geteilten={verstoesse}"
        );
        println!(
            "hiiso   : {}",
            if verstoesse > 0 {
                "FAILURES"
            } else if privat == 0 {
                // Kein Urteil moeglich -- und das ist ein eigenes Ergebnis, kein Bestehen.
                "SKIP"
            } else {
                "ALL PASS"
            }
        );
    }

    // Z4 Stufe 2 **vor** `freeze_bericht`: jene Pruefung friert denselben Worker noch einmal ein
    // und taut ihn auf. Liefe sie zuerst, waere „der Zaehler lief nach dem Wiederherstellen
    // weiter" eine Aussage ueber ihr Auftauen statt ueber den Checkpoint.
    ckpt_bericht();

    freeze_bericht();

    // A-5.3 -- hier, nicht beim Start des Root-Tasks: die Treiber-PD entsteht erst, wenn `init`
    // laeuft und `SYS_LOAD` ruft.
    devsel_bericht();

    let sa = system::sched_audit_all();
    let cdt = system::cap_audit_cdt();
    let (walk, revops, limit) = system::cap_cdt_peaks();
    println!("audit   : sched_audit={sa} cdt_audit={cdt}");
    println!(
        "cdtlen  : B-5.5 Hoechststaende als OPERATIONSZAHL -- Abstieg {walk}/{limit} Schritte, \
         Revoke {revops}/{limit} Loeschungen (Schranke = Slotzahl: ein azyklischer Lauf besucht \
         keinen Slot zweimal). Zeit waere das falsche Mass -- sie haengt an Taktrate und \
         Emulation, nicht an der Struktur"
    );
    println!(
        "audit   : {}",
        if sa == 0 && cdt == 0 { "ALL PASS" } else { "FAILURES" }
    );

    // --- B-5.1: wird der Verbrauch gestempelt oder getickt? -------------------------------------
    //
    // Die Aussage, die hier fallen koennen muss: **es wird bei jeder Umplanung abgerechnet, nicht
    // nur beim Tick.** Genau das ist der Unterschied -- wer kurz vor dem Tick blockiert, hat
    // gerechnet und zahlte vorher nichts.
    //
    // Die Zahl, die das belegt, ist `Proben > Ticks`: die Tick-Rechnung kann hoechstens einmal je
    // Tick belasten. Jede Probe darueber hinaus ist ein Abrechnungsereignis ZWISCHEN zwei Ticks --
    // also genau die Zyklen, die vorher niemand zahlte.
    //
    // Und die Sprechprobe: ist die Zeitquelle nicht zugesichert, wird bewusst nichts gerechnet --
    // dann muss `rejected_source > 0` sein. Ein Kernel, in dem die Klammerung gar nicht gerufen
    // wird, sieht sonst genauso aus wie einer, der korrekt schweigt. Beide Zweige verlangen einen
    // BELEG, keiner laesst eine leere Statistik als Erfolg durchgehen.
    let (proben, zyklen, ticks, rw, unpl, kw, unsicher) = system::cycle_accounting(0);
    // **Welcher Zweig gilt, entscheidet die Maschine, nicht der Kernel.** Der erste Entwurf las
    // nur `unsicher == 0` und waehlte danach den Zweig -- damit haette ein vergessenes
    // `set_cycle_source` den Test bestanden: alles landete in `rejected_source`, der
    // Untrusted-Zweig war erfuellt, und der Bericht meldete gruen fuer einen Kernel, der gar
    // nicht abrechnet. Die Sollgroesse muss von aussen kommen: `invariant_tsc()` sagt, was die
    // Maschine kann. Sagt sie ja und der Scheduler rechnet trotzdem nicht, ist die Verdrahtung
    // kaputt -- und genau das soll die Zeile sehen.
    let hw = hal::timer::invariant_tsc();
    let anomalie = rw != 0 || unpl != 0 || kw != 0;
    let cyc_ok = !anomalie
        && if hw {
            // Die Maschine kann messen -> es MUSS gemessen werden, und zwar oefter als getickt.
            unsicher == 0 && proben > ticks && zyklen > 0
        } else {
            // Die Maschine kann es nicht -> es darf nichts gerechnet werden. Aber der Pfad muss
            // nachweislich laufen: der Ablehnungszaehler ist hier der Sprechbeleg.
            proben == 0 && zyklen == 0 && u64::from(unsicher) > ticks
        };
    println!(
        "cycacct : {} -- B-5.1: {proben} Zyklenproben gegen {ticks} Ticks ({zyklen} Zyklen \
         verbucht; verworfen: rueckwaerts={rw} unplausibel={unpl} Kernwechsel={kw} \
         Quelle-nicht-zugesichert={unsicher}; Maschine invariant={hw}). Proben > Ticks IST die \
         Aussage: die Tick-Rechnung kann hoechstens einmal je Tick belasten, jede weitere Probe \
         ist Rechenzeit, die vorher niemand zahlte. Welcher Zweig gilt, sagt die MASCHINE \
         (invariant_tsc) -- sonst bestuende ein Kernel, der gar nicht abrechnet, ueber den \
         Untrusted-Zweig",
        if cyc_ok { "ALL PASS" } else { "FAILURES" }
    );

    println!("x86_64 Stufe 4: Kernel-Kern laeuft (Selbsttests + Scheduler + cap-gesicherte IPC)");
    // Der Marker trennt die beiden Ausgänge -- s. Funktionsdoku. Ein Lauf, der aus der Notbremse
    // kommt, darf nicht denselben Satz drucken wie einer, der alles belegt hat.
    if watchdog {
        println!("== SELFTEST FAILED (watchdog) -> system_off ==");
    } else {
        println!("== SELFTEST COMPLETE -> system_off ==");
    }
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
    /// Wie viele `mmap`-Eintraege hoechstens ausgewertet werden. Klein und sichtbar -- die
    /// Struktur kommt vom Bootloader, und eine unbegrenzte Schleife ueber fremde Daten ist
    /// keine Auswertung, sondern ein Vertrauensvorschuss.
    const MAX_MMAP: usize = 16;
    let mut bi = super::bootinfo::HandoverInfo::empty();
    bi.ram_top = ram_end;
    /*
     * **E-Rest 3: der Speicherplan wird jetzt gelesen statt interpoliert.**
     *
     * Bisher stand hier `[free_base, ram_end)` als EIN Block. Das ist genau so lange richtig,
     * wie der Speicher zusammenhaengt -- und sobald QEMU RAM oberhalb 4 GiB anlegt, tut er das
     * nicht mehr: bei `-m 3G` liegen 2 GiB unter 4 GiB, 1 GiB ab 4 GiB, und dazwischen klafft
     * das PCI-Loch `0x8000_0000..0x1_0000_0000`. Der Kernel meldete es dem Allokator als freies
     * RAM -- gemessen `mem : freies RAM [0x1000000, 0x140000000)`, also 5070 MiB "frei" auf
     * einer 3-GiB-Maschine. Eine Allokation dort trifft Geraeteregister oder gar nichts, und
     * zwar erst dann, wenn genug Speicher verbraucht ist: also spaet, weit weg und ohne
     * erkennbaren Zusammenhang.
     *
     * `ram_top` bleibt der **hoechste** Wert (daran haengt die Wahl des IOVA-Fensters); die
     * Freiliste kommt aus den als verfuegbar gemeldeten Bereichen. Zwei Fragen, zwei Quellen.
     */
    let mut avail = [(0u64, 0u64); MAX_MMAP];
    let mut navail = mbi.map(|m| m.ram_regions(&mut avail)).unwrap_or(0);
    if navail == 0 {
        // Kein auswertbarer Plan -> die alte Annahme, aber ausgesprochen statt unterstellt.
        avail[0] = (free_base, ram_end.saturating_sub(free_base));
        navail = 1;
        println!("mem     : HINWEIS kein Bereichsplan -> Rueckfall auf einen Block");
    }
    let mut gross = 0u64; // Summe vor dem Ausschnitt
    // Einen Bereich in `SPLIT` Stuecke zerlegt in die Freiliste geben. Die Zerstueckelung ist
    // eine Aussage ueber den **Eingang** (s. o.); der Allokator verschmilzt angrenzende Stuecke
    // beim Einfuegen ohnehin sofort wieder.
    let mut einspeisen = |a: u64, b: u64, gross: &mut u64, bi: &mut super::bootinfo::HandoverInfo| {
        let chunk = ((b - a) / SPLIT) & !0xfff;
        for k in 0..SPLIT {
            let base = a + k * chunk;
            let len = if k == SPLIT - 1 { b - base } else { chunk };
            if len == 0 {
                continue;
            }
            *gross += len;
            // Die Modulbereiche werden hier **ausgeschnitten**, nicht spaeter markiert: was nie
            // als frei gemeldet wurde, kann auch nicht vergeben werden (A-1.1).
            super::multiboot::subtract_holes(base, len, &mods[..nmods], &mut |b, l| {
                bi.push_region(b, l);
            });
        }
    };
    let grenze = hal::mmu::LOW_MAPPED_END;
    // **Erst alles unter 4 GiB** -- der Speicher, den es auf jeder Maschine gibt. Die Reihenfolge
    // ist seit E-Rest 3b nur noch Ordnung im Bericht, keine Zusicherung mehr: welcher Bereich
    // eine Anforderung bedient, entscheidet der Zonenwunsch, nicht die Einspeisereihenfolge.
    for &(rb, rl) in &avail[..navail] {
        // Was unterhalb von `free_base` liegt, gehoert dem Kernel-Image und den Boot-Tabellen.
        let a = rb.max(free_base);
        let b = rb.saturating_add(rl).min(grenze);
        if b <= a {
            continue;
        }
        einspeisen(a, b, &mut gross, &mut bi);
    }
    /*
     * **Und jetzt der Teil oberhalb von 4 GiB -- seit E-Rest 3b ohne Bedingung.**
     *
     * Bis zum 2026-08-04 stand hier ein Behelf: der hohe Bereich kam nur in die Freiliste, wenn
     * er GROESSER war als der kleinste untere. Der Grund war eine unausgesprochene Eigenschaft,
     * auf die sich `alloc_dma_region` und die Region einer isolierten PD verliessen -- beide
     * MUESSEN in GiB 0 liegen (`vspace_map_block` bildet nur dort ab), fragten aber nach
     * "irgendeiner" Region und gaben bei Verfehlung auf, ohne ein zweites Mal zu fragen. Solange
     * der Speicher zusammenhing, belegte Best-Fit von unten und es ging gut; ein zweiter,
     * KLEINERER Bereich oben brach es (gemessen bei `-m 3G`: `dmawin`/`dmatok : FAILURES`,
     * `iso : spawn_isolated fehlgeschlagen`, waehrend 4G und 6G zufaellig gruen waren).
     *
     * Der Behelf war fail-closed und hat den nutzbaren Speicher fuer DMA-Regionen und isolierte
     * PDs bei 1 GiB gedeckelt -- fuer das Zielbild der haertere Deckel als die alte 4-GiB-Karte.
     *
     * Behoben ist es jetzt dort, wo die Ursache lag: `sel4lake_mem::alloc_below` kennt den
     * Zonenwunsch, und die drei Aufrufer nennen ihn (`system::gib0_zone`). Damit haengt nichts
     * mehr an der Belegungsordnung, und hoher Speicher geht vollstaendig in die Freiliste --
     * fuer alles, was keine PD-eigene Abbildung braucht (Kernel-Stacks, Heap, Slabs, Archiv).
     */
    let mut hoch_abgebildet = 0u64;
    let mut hoch_frei = 0u64;
    let mut hoch_unabgebildet = 0u64;
    for &(rb, rl) in &avail[..navail] {
        let a = rb.max(free_base).max(grenze);
        let b = rb.saturating_add(rl);
        if b <= a {
            continue;
        }
        // **Erst abbilden, dann verteilen.** In die Freiliste kann nur kommen, was
        // `adopt_high_ram` als RAM in die Karte gelegt hat -- nicht ein Byte mehr. Ohne diese
        // Kopplung waeren "was der Allokator ausgeben darf" und "was abgebildet ist" zwei
        // Zahlen, die zueinander passen muessen; so ist es eine.
        let Some((hb, hl)) = hal::mmu::adopt_high_ram(a, b - a) else {
            hoch_unabgebildet += b - a;
            continue;
        };
        hoch_abgebildet += hl;
        hoch_unabgebildet += (b - a) - hl;
        hoch_frei += hl;
        einspeisen(hb, hb + hl, &mut gross, &mut bi);
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
        "mem     : {} Bereiche / {} MiB ueber HandoverInfo (Quelle {}), verworfen={}",
        bi.n_regions,
        net >> 20,
        if bi.source == super::bootinfo::BootSource::Handover { "Kern-Uebergabe" } else { "Multiboot" },
        system::mem_regions_dropped()
    );
    print!(
        "mem     : freies RAM ab {free_base:#x} aus {navail} gemeldeten Bereich(en), \
         RAM-Oberkante {ram_end:#x}"
    );
    if hoch_abgebildet > 0 || hoch_unabgebildet > 0 {
        // Drei Zahlen, weil es drei verschiedene Lagen sind, und ein stillschweigend kleinerer
        // Speicher eine Fehldiagnose in Wartestellung ist: **abgebildet** (der Kernel kommt
        // heran), **davon vergeben** (der Allokator darf es ausgeben -- seit E-Rest 3b immer
        // alles Abgebildete; die beiden Zahlen sind gleich, und wenn sie es einmal nicht sind,
        // steht es hier), **nicht abgebildet** (kein vollstaendig gedecktes 1-GiB-Blatt oder
        // keine 1-GiB-Seiten auf dieser CPU).
        print!(
            " -- oberhalb 4 GiB: {} MiB abgebildet, davon {} MiB vergeben; {} MiB nicht abgebildet",
            hoch_abgebildet >> 20,
            hoch_frei >> 20,
            hoch_unabgebildet >> 20
        );
    }
    println!();
    if carved > 0 {
        println!("mem     : {carved} Byte fuer Multiboot-Module ausgeschnitten (vor der ersten Allokation)");
    }
    // Cap-Tabellen VOR dem ersten Cap (A-3.4): der Selbsttest weiter unten installiert bereits
    // welche, und ein nicht angehaengter Space haette Kapazitaet 0.
    let cap_bytes = system::configure_caps();
    let (cap_slots, cap_objs) = system::cap_capacity();
    println!(
        "cap     : {cap_slots} Slots / {cap_objs} Objekte / {} PDs, Tabellen {} KiB aus dem RAM (Summe aller PD-Budgets: {})",
        system::pd_capacity(),
        cap_bytes >> 10,
        sel4lake_microkit::CAP_SLOTS_FOR_ALL_PDS
    );
    // A-3.4 Teil 4: IPC-Tabellen VOR dem ersten Endpoint. Meldet sich selbst (`ipc :`).
    system::configure_ipc();
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

    // --- E-Rest 3: Registerfenster oberhalb von 4 GiB in die Karte holen ----------------------
    //
    // **Der Pfad von der BAR-Ermittlung zur Seitentabelle.** Sobald die Maschine Speicher
    // oberhalb 4 GiB hat, legt SeaBIOS die 64-Bit-BARs in ein Loch bei 448 GiB -- gemessen:
    // `cr2 = 0x70_0000_0014`, also `DEVICE_STATUS` des virtio-Transports, drei Zeilen nach
    // dieser hier. Die Karte endete bei 4 GiB, die Seite war schlicht nicht da.
    //
    // Abgebildet wird **gezielt**, nicht flaechig: nur die 2-MiB-Bloecke, in denen wirklich ein
    // BAR liegt. Eine flaechige 512-GiB-Karte waere billiger zu schreiben (512 PDPT-Eintraege,
    // kein zusaetzlicher Rahmen) -- und haette zur Folge, dass jede erfundene Adresse in 448 GiB
    // Nichts auf eine gueltige, beschreibbare Seite trifft.
    //
    // Die Zeile ist **sprechfaehig**: sie nennt beide Richtungen. Positivkontrolle -- jedes
    // abgebildete Fenster wird durch die Seitentabellen aufgeloest und muss auf sich selbst
    // zeigen. Negativkontrolle -- dieselbe Frage an ein GiB, das niemand abgebildet hat, muss
    // `None` liefern. Ohne die zweite waere ein `ALL PASS` auch dann zu haben, wenn die Karte
    // flaechig alles abbildet; ohne die erste hiesse "0 Fenster oberhalb 4 GiB" dasselbe wie
    // "alles in Ordnung", und das sind zwei verschiedene Aussagen.
    {
        let mut ueber4 = 0usize;
        let mut abgebildet = 0usize;
        let mut aufloesbar = 0usize;
        let mut erste = 0u64;
        hal::pcie::for_each_bar(&mut |base, len| {
            if base.saturating_add(len) <= hal::mmu::LOW_MAPPED_END {
                return; // unter 4 GiB: steht seit `mmu::init_primary`
            }
            ueber4 += 1;
            if erste == 0 {
                erste = base;
            }
            if hal::mmu::map_device_window_global(base, len) {
                abgebildet += 1;
                if hal::mmu::resolve_global(base) == Some(base) {
                    aufloesbar += 1;
                }
            }
        });
        // Das oberste GiB des einen benutzten PML4-Eintrags: dort bildet dieser Kernel
        // grundsaetzlich nichts ab. Faende der Aufloeser hier etwas, waere die Karte flaechig
        // geworden -- und die Positivkontrolle darueber wertlos.
        let leer = hal::mmu::resolve_global(511 * (1 << 30)).is_none();
        let (dev_gib, ram_gib, gib_seiten) = hal::mmu::high_map_report();
        println!(
            "himap   : BAR-Fenster oberhalb 4 GiB: {ueber4} gefunden, {abgebildet} abgebildet, \
             {aufloesbar} durch die Seitentabellen aufloesbar (erstes {erste:#x}); \
             hohe GiB: {dev_gib} Geraet + {ram_gib} RAM, 1-GiB-Seiten={gib_seiten}, \
             unabgebildetes GiB bleibt unabgebildet={leer}"
        );
        println!(
            "himap   : {}",
            if abgebildet == ueber4 && aufloesbar == ueber4 && leer { "ALL PASS" } else { "FAILURES" }
        );
    }

    // --- virtio-pci auf x86, Teil 1 von 2 (A-5.2) -------------------------------------------
    //
    // Bis 2026-08-01 lag der virtio-Treiber unter `hal/aarch64/`, obwohl nichts daran ARM-
    // spezifisch ist: virtio-pci ist ein PCI-Standard. Er liegt jetzt arch-neutral
    // (`hal::virtio`) und laeuft hier zum ersten Mal auf x86.
    //
    // **Diese Stelle ist bewusst gewaehlt: VOR dem VT-d-Aufbau.** Hier steht die Root-Tabelle
    // noch nicht auf Default-Block, das Geraet darf also DMAen. Belegt wird damit der TRANSPORT
    // in voller Laenge -- Capability-Liste, Handshake, Feature-Aushandlung einschliesslich
    // VIRTIO_F_ACCESS_PLATFORM, Virtqueue, und dass das Geraet wirklich Bytes liefert.
    //
    // Teil 2 steht nach dem IOMMU-Aufbau und prueft die Gegenrichtung. Zwei Teile, weil ein Test,
    // der nur den Erfolgsfall zeigt, nicht sagen kann, ob die Sperre danach wirkt -- und einer,
    // der nur die Sperre zeigt, nicht, ob ueberhaupt etwas funktioniert haette.
    #[cfg(feature = "selftest")]
    {
        let (ok, adv, w) = virtio_versuch(rng.as_ref(), 50_000_000);
        println!(
            "virtio  : Transport (vor VT-d): Caps={} Geraet-DMA={} ({} Byte)",
            ok as u8, adv as u8, w
        );
        VIRTIO_XPORT_OK.store(rng.is_none() || (ok && adv && w > 0), Ordering::Release);
    }

    // --- virtio-blk und virtio-net (A-5.2) --------------------------------------------------
    //
    // Der RNG belegt den Transport, aber nur eine Richtung: das Geraet SCHREIBT in unseren
    // Speicher. Ob es ihn auch LESEN kann, sagt er nicht -- er liest nie etwas von uns. Genau
    // daran haengen die beiden Geraete hier:
    //
    //   * `blk` schickt eine dreigliedrige Deskriptorkette. Glied 0 ist der Anfragekopf mit der
    //     Sektornummer, und ihn muss das GERAET LESEN. Kommt er nicht an, weiss es nicht einmal,
    //     was es liefern soll. Ein Sektor mit der richtigen Magie belegt beide Richtungen in
    //     einer Transaktion -- und dazu, dass das Geraet `next` verfolgt.
    //   * `net` hat ZWEI Queues mit verschiedenem `queue_notify_off`. Ein Treiber, der die
    //     Notify-Adresse der ersten fuer beide benutzt, weckt das Geraet auf der falschen Seite;
    //     bei einem Einqueue-Geraet kann dieser Fehler strukturell nicht auftreten.
    //
    // Beides steht wie der RNG-Transport VOR dem VT-d-Aufbau -- hier darf das Geraet DMAen.
    let blk_dev = hal::pcie::find(hal::pcie::VIRTIO_VENDOR, &hal::pcie::VIRTIO_BLK_DEVICES);
    // **Nicht mehr unter `selftest`** (A-5.3): dieses Geraet wird jetzt auch im Produktivkernel
    // ZUGETEILT, nicht nur getestet. Ein Fund, den nur die Pruefkonfiguration macht, waere eine
    // Zuteilung, die es im ausgelieferten Kernel gar nicht gibt.
    let net_dev = hal::pcie::find(hal::pcie::VIRTIO_VENDOR, &hal::pcie::VIRTIO_NET_DEVICES);
    #[cfg(feature = "selftest")]
    {
        match virtio_blk_versuch(blk_dev.as_ref(), 50_000_000) {
            Some(r) => {
                println!(
                    "vblk    : Lesen (vor VT-d): Caps={} Kapazitaet={} Sektor(en) Status={:#04x} \
                     geschrieben={} Byte Magie={:#018x} (erwartet {:#018x})",
                    r.features_ok as u8, r.capacity_sectors, r.status, r.written, r.first_word,
                    BLK_MAGIC
                );
                VBLK_READ_OK.store(r.ok(BLK_MAGIC) && r.capacity_sectors > 0, Ordering::Release);
            }
            // Kein Geraet ist KEIN Fehlschlag und auch kein Bestehen. Die Lade-Suite reicht
            // keine Platte herein; ein stilles PASS an dieser Stelle waere genau die Sorte
            // Zeile, die spaeter jemand als Beleg liest, obwohl nichts geprueft wurde.
            None => {
                println!("vblk    : SKIP (kein virtio-blk-Geraet am Bus)");
                VBLK_READ_OK.store(true, Ordering::Release); // nicht anwendbar, nicht bestanden
            }
        }
        match virtio_net_versuch(net_dev.as_ref(), 50_000_000) {
            Some(r) => {
                println!(
                    "vnet    : MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}; Caps={} \
                     Sendepuffer abgeholt={} Rahmen empfangen={} ({} Byte) ARP-Antwort von \
                     {}.{}.{}.{}={}",
                    r.mac[0], r.mac[1], r.mac[2], r.mac[3], r.mac[4], r.mac[5],
                    r.features_ok as u8, r.tx_used as u8, r.rx_used as u8, r.rx_len,
                    r.sender_ip[0], r.sender_ip[1], r.sender_ip[2], r.sender_ip[3],
                    r.arp_reply as u8
                );
                println!(
                    "vnet    : {} (A-5.2: virtio-net auf x86 -- zwei Queues mit getrennter \
                     Notify-Adresse; die ARP-ANTWORT auf die eigene Anfrage belegt beide \
                     Richtungen inhaltlich, ein bloss gefuellter Puffer koennte Restspeicher sein)",
                    if r.features_ok && r.tx_used && r.rx_used && r.arp_reply {
                        "ALL PASS"
                    } else {
                        "FAILURES"
                    }
                );
                VNET_OK.store(
                    r.features_ok && r.tx_used && r.rx_used && r.arp_reply,
                    Ordering::Release,
                );
            }
            None => {
                println!("vnet    : SKIP (keine virtio-net-Karte am Bus)");
                VNET_OK.store(true, Ordering::Release); // nicht anwendbar, s. `vblk` daneben
            }
        }
    }

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
                "vtdcaps : {} (Schritt 1: Faehigkeiten gelesen -- als Minimum ueber ALLE Einheiten (B-3.3), \
                 nicht nur ueber Einheit 0. IR/CFI stehen seit B-3.2; attach TEILT ZU (A-5.1: die \
                 Treiber-PD bekommt eine uebersetzte DMA-Region); die IOVA-Fensterwahl meidet den \
                 Interrupt-Nachrichtenbereich (B-3.4). Offen: QI und IR laufen weiterhin nur auf \
                 Einheit 0 -- fuer die Uebersetzung folgenlos, fuer Interrupt Remapping nicht)",
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
        // B-3.1: laeuft die Queued Invalidation, und traegt sie den Interrupt-Entry-Cache?
        // Der IEC ist der Punkt: fuer ihn gibt es KEINEN Registerpfad. Ohne ihn waere eine
        // geaenderte IRTE nie wirksam zu invalidieren -- und damit Interrupt Remapping (B-3.2)
        // nicht zulaessig, sondern nur scheinbar aktiv.
        let qi = hal::vtd::qi_active();
        let iec = hal::vtd::invalidate_iec_global();
        println!(
            "qi      : Queued Invalidation aktiv={qi}; Kontext-Cache ueber die Warteschlange \
             quittiert={inv}; Interrupt-Entry-Cache invalidiert={iec} (fuer den gibt es KEINEN \
             Registerpfad -- deshalb ist B-3.1 Vorbedingung von B-3.2, nicht Geschmackssache)"
        );
        println!(
            "qi      : {} (B-3.1: Invalidierung laeuft ueber die Warteschlange, und der \
             Interrupt-Entry-Cache ist erreichbar)",
            if qi && iec { "ALL PASS" } else { "FAILURES" }
        );
        // B-3.2: Interrupt Remapping aktiv UND Compatibility-Format zu. Beides zusammen, denn
        // IR mit erlaubtem CFI ist eine offene Tuer an der Seite: das Compatibility-Format ist
        // der alte, nicht-remappte Nachrichtenpfad und umgeht die Tabelle vollstaendig.
        let ir = hal::vtd::ir_active();
        let cfi = hal::vtd::cfi_blocked();
        println!(
            "ir      : Interrupt Remapping aktiv={ir} (GSTS.IRES), Compatibility-Format \
             abgeschaltet={cfi} (GSTS.CFIS==0); Tabelle mit lauter 'not present' = Default-Block, \
             ein Geraet ohne IRTE kann keinen Interrupt ausloesen"
        );
        println!(
            "ir      : {} (B-3.2: ohne IR koennte ein durchgereichtes Geraet beliebige \
             Interrupt-Nachrichten erzeugen -- MSI ist eine DMA-Schreibung, die die Uebersetzung \
             gar nicht ansieht)",
            if ir && cfi { "ALL PASS" } else { "FAILURES" }
        );
        up && system::dma_enforcer().is_active() && inv && qi && iec && ir && cfi
            && system::dma_enforcer().audit() == 0
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

    // --- virtio-pci auf x86, Teil 2 von 2 (A-5.2 / E) ---------------------------------------
    //
    // Dieselbe Anfrage, jetzt mit aufgebauter VT-d-Einheit und Root-Tabelle auf Default-Block.
    // Das Geraet hat KEINE Zuteilung (`attach` liefert auf x86 noch None -- B-3.3/B-3.4), sein
    // Bus-Master-Zugriff muss also ins Leere laufen. Kaeme er durch, waere das ein
    // Isolationsbruch, und dieser Test wuerde ihn sehen.
    //
    // Kurze Poll-Schranke: hier wird ein AUSBLEIBEN erwartet: auf eine Antwort zu warten, die per
    // Entwurf nie kommt, kostet sonst je Lauf hunderte Millisekunden.
    #[cfg(feature = "selftest")]
    {
        let vorher = hal::iommu::drain_faults();
        let (ok, adv, w) = virtio_versuch(rng.as_ref(), 2_000_000);
        let faults = hal::iommu::drain_faults();
        println!(
            "virtio  : Sperre (nach VT-d, Geraet nicht zugeteilt): Caps={} Geraet-DMA={} ({} Byte) \
             VT-d-Faults davor={} danach={}",
            ok as u8, adv as u8, w, vorher, faults
        );
        let blockiert = rng.is_none() || !adv;
        println!(
            "virtio  : {} (A-5.2: der virtio-pci-Transport laeuft auf x86 -- arch-neutraler Treiber, \
             Caps, Handshake, Feature-Aushandlung inkl. VIRTIO_F_ACCESS_PLATFORM, Virtqueue, echte \
             Geraete-DMA. Und nach dem VT-d-Aufbau kommt dasselbe Geraet OHNE Zuteilung nicht mehr \
             durch -- beide Richtungen belegt, nicht nur die bequeme)",
            if VIRTIO_XPORT_OK.load(Ordering::Acquire) && blockiert { "ALL PASS" } else { "FAILURES" }
        );
        VIRTIO_OK.store(VIRTIO_XPORT_OK.load(Ordering::Acquire) && blockiert, Ordering::Release);

        // Dieselbe Gegenprobe fuer das Blockgeraet -- und sie sagt etwas ANDERES als die daneben.
        //
        // Der RNG belegt, dass das Geraet nicht mehr in unseren Speicher SCHREIBEN kann. Dass es
        // ihn auch nicht mehr LESEN kann, folgt daraus nicht: das waere eine Annahme ueber die
        // Root-Tabelle ("not present sperrt beide Richtungen"), und Annahmen ueber
        // Hardwaresemantik haben in diesem Projekt schon zweimal danebengelegen (STE.S1STALLD,
        // GCMD als Read-Modify-Write). Hier ist sie pruefbar: bleibt das Statusbyte auf 0xff,
        // hat das Geraet nicht einmal den Anfragekopf zu Gesicht bekommen.
        if let Some(r) = virtio_blk_versuch(blk_dev.as_ref(), 2_000_000) {
            let faults2 = hal::iommu::drain_faults();
            println!(
                "vblk    : Sperre (nach VT-d, Geraet nicht zugeteilt): abgeschlossen={} \
                 Status={:#04x} (0xff = vom Geraet unberuehrt) VT-d-Faults={}",
                r.used_advanced as u8, r.status, faults2
            );
            let blk_blockiert = !r.used_advanced && r.status == 0xff;
            println!(
                "vblk    : {} (A-5.2: virtio-blk auf x86 -- dreigliedrige Deskriptorkette, das \
                 Geraet LIEST den Anfragekopf und liefert den angeforderten Sektor mit der \
                 erwarteten Magie. Nach dem VT-d-Aufbau erreicht es den Kopf nicht mehr -- damit \
                 ist auch die LESERICHTUNG gesperrt belegt, die der RNG-Test nicht zeigen kann)",
                if VBLK_READ_OK.load(Ordering::Acquire) && blk_blockiert {
                    "ALL PASS"
                } else {
                    "FAILURES"
                }
            );
            VBLK_OK.store(
                VBLK_READ_OK.load(Ordering::Acquire) && blk_blockiert,
                Ordering::Release,
            );
        } else {
            // Die SKIP-Zeile steht bereits oben (Teil 1) -- hier nur die Abschlussbedingung.
            VBLK_OK.store(true, Ordering::Release);
        }
    }

    // --- A-5.1: das Blockgeraet als zuteilbar anmelden -----------------------------------------
    //
    // Ab hier ist es **nicht mehr die Sache des Kerns**, was mit dem Geraet geschieht. Er hat es
    // enumeriert -- das ist seine Aufgabe, denn ein Lauf ueber alle Busse sieht jede Maschine --
    // und legt jetzt drei Tatsachen hin: diese Konfigurationsraum-Seite, dieses Registerfenster,
    // diese Requester-ID. Wer das bekommt, sagt das Manifest, nicht dieser Code.
    //
    // Der Kern weiss hier ausdruecklich **nicht**, dass es virtio ist. Er ruft `probe_transport`
    // nur, um herauszufinden, in WELCHEM BAR die Strukturen liegen, die er mitgeben muss -- eine
    // Frage der Zuteilung, keine des Bedienens. Die Zeile darunter waere ohne diesen Umweg
    // "irgendein BAR", und ein Treiber mit dem falschen Fenster faende sein Geraet nicht.
    // **A-5.3: es sind jetzt MEHRERE.** Bis A-5.2 wurde genau ein Geraet angeboten, mit der
    // ausdruecklichen Begruendung, dass jede Auswahl unter mehreren eine im Kernel versteckte
    // Politik waere, solange das Manifest nicht sagen kann, WELCHES. Der Selektor im Eintrag
    // (A-5.3) nimmt dieser Begruendung die Grundlage -- also wird jetzt alles angeboten, was
    // gefunden wurde, und die Auswahl steht im Autoritaetsdokument.
    //
    // Die Hersteller-/Geraete-/Klassen-IDs gehen **nur zur Auswahl** mit. Der Kern bedient das
    // Geraet weiterhin nicht und weiss nicht, was virtio ist; er reicht durch, was auf jeder
    // PCI-Maschine an derselben Stelle im Konfigurationsraum steht.
    let anbieten = |d: &hal::pcie::PciDevice| {
        let Some(t) = hal::virtio::probe_transport(d) else { return };
        let common = t.common_addr();
        let mut bar = 0u64;
        let mut bar_len = 0u64;
        for i in 0..6 {
            let b = d.bars[i];
            if b == 0 {
                continue;
            }
            let len = hal::pcie::bar_size(d, i);
            if len != 0 && common >= b && common < b + len {
                bar = b;
                bar_len = (len + 0xfff) & !0xfff;
                break;
            }
        }
        if bar == 0 {
            return;
        }
        let ok = system::offer_driver_device(system::DriverDevice {
            rid: d.rid(),
            cfg_page: hal::pcie::cfg_page(d),
            bar,
            bar_len,
            vendor: d.vendor,
            device: d.device,
            class: d.class,
        });
        println!(
            "devassign: zuteilbar: RID {:#06x} {:04x}:{:04x} Klasse {:#08x}, \
             Konfigurationsseite {:#x}, BAR {:#x}+{:#x}{}",
            d.rid(),
            d.vendor,
            d.device,
            d.class,
            hal::pcie::cfg_page(d),
            bar,
            bar_len,
            if ok { "" } else { " -- ABGEWIESEN, Angebotsliste voll" }
        );
    };
    if let Some(d) = blk_dev.as_ref() {
        anbieten(d);
    }
    if let Some(d) = net_dev.as_ref() {
        anbieten(d);
    }
    println!(
        "devassign: {} Geraet(e) angeboten (A-5.1/A-5.3: der Kern enumeriert und teilt zu; was fuer \
         Geraete das sind, weiss er nicht -- das Manifest sagt, WER WELCHES bekommt, und die \
         Reihenfolge dieser Liste ist ausdruecklich KEINE Zusage)",
        system::offered_device_count()
    );

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
        // **Drei unterscheidbare Faelle, nicht zwei.** Die Vorgaengerzeile druckte `=1` und
        // bedeutete damit wahlweise „geprueft und frei" oder „gar nicht geprueft" -- im schwachen
        // Zweig gab der Pruefer `true` zurueck, obwohl das Fenster `[0, 512 GiB)` den Sperrbereich
        // ENTHAELT. Wer aus einer solchen Zeile liest, kann den Unterschied nicht sehen; deshalb
        // steht das Urteil jetzt im Klartext und die Begruendung dahinter.
        println!(
            "dmawin  : B-3.4 Fenster gegen den Interrupt-Nachrichtenbereich \
             (0xFEE0_0000..0xFEF0_0000): {} -- dorthin geschriebene DMA waere fuer VT-d eine \
             Interrupt-Nachricht und wuerde gar nicht uebersetzt: kein Fault, keine Fehlerzeile, \
             nur Daten, die nirgends ankommen",
            crate::dmatests::msi_klartext(w.msi.policy)
        );
        println!(
            "dmawin  : B-3.4 belegte-DMA-Kontexte={} davon-schwaches-Fenster={} \
             Fenster-weicht-von-der-Politik-ab={} eingetragenes-Fenster-im-Sperrbereich={} \
             (das schwache Fenster ist [0, 512 GiB) und enthaelt den Sperrbereich -- die Zahl \
             erklaert ein VERLETZT, statt es zu ersetzen)",
            w.msi.live, w.msi.weak, w.msi.drift, w.msi.live_overlaps
        );
        println!("dmawin  : 32-Bit-Geraet-abgewiesen={} Fenster-voll-abgewiesen={} Kontext-danach-intakt={} balanciert={} Geraete-ohne-deklarierte-Adressbreite={}",
            w.narrow, w.exhausted, w.intact, w.balanced, w.undeclared);
        println!("dmawin  : {}", w.verdict().wort());
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
    //
    // Die Geraete-Zuteilung passiert **hier drin**: der Root-Task laedt die Treiber-PD, und
    // `endow_from_manifest` zieht ihr Geraet aus der Angebotsliste (A-5.3).
    // A-5.3: der Stand VOR jeder Zuteilung festhalten. Danach ist er nicht mehr feststellbar -- ein
    // vergebenes Geraet wird aus der Angebotsliste genommen, und ohne diese Zahl liesse sich
    // hinterher nicht mehr sagen, ob ueberhaupt eine Auswahl zu treffen war.
    //
    // **Berichtet wird spaeter.** `start_root_task_reported` LAEDT den Root-Task, es fuehrt ihn
    // nicht aus; die Treiber-PD entsteht erst, wenn `init` selbst laeuft und `SYS_LOAD` ruft. Ein
    // Bericht an dieser Stelle haette 0 Zuteilungen gesehen und sie fuer einen Fehlschlag gehalten
    // -- gemessen, nicht vermutet: genau das tat der erste Entwurf.
    ANGEBOTEN_VORHER.store(system::offered_device_count(), Ordering::Release);
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
            let Some(stack) = system::alloc_anywhere(AP_STACK_BYTES, 4096) else {
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
            // Die Notbremse steht VOR `reap()`, und sie zaehlt Ticks statt Umdrehungen.
            //
            // Beides aus einem Grund, den zwei Haenger am 2026-08-01 gezeigt haben (Laeufe 115
            // und 235 von 400): sie blieben nach `smp : 4 von 4 Kern(en) online` stehen, und
            // im Log stand KEINE WATCHDOG-Zeile. Unter KVM braucht diese Schleife fuer 50 Mio
            // Umdrehungen den Bruchteil einer Sekunde -- haette sie sich gedreht, waere die
            // Notbremse in 120 s laengst gefallen. Sie hat sich also nicht gedreht.
            //
            // `reap()` nimmt `SCHEDS[core].lock()` und `MEM.lock()`. Blockiert es dort, kam
            // der Zaehler frueher nie wieder an die Reihe: die Notbremse wurde von genau dem
            // ausgehungert, was sie ueberwachen soll. Steht sie davor, faengt sie jeden
            // Stillstand ausserhalb von `reap()` selbst -- insbesondere einen in `all_done()`.
            //
            // Ticks statt Umdrehungen, weil eine Umdrehungszahl von der Taktrate abhaengt:
            // 50 Mio sind unter KVM Millisekunden und unter TCG Minuten, dieselbe Zahl meint
            // also je nach Aufbau etwas anderes. Ticks kommen vom Timer-Interrupt und messen
            // Zeit. Der Zaehler bleibt als zweites Kriterium fuer den Fall, dass gar kein
            // Timer laeuft -- dann gaebe es keine Ticks, und eine reine Tick-Grenze wuerde nie
            // greifen.
            //
            // OFFEN (todo D0): blockiert `reap()` SELBST, hilft auch das nicht -- dieser Faden
            // kommt dann nirgends mehr an. Dafuer braeuchte es eine Notbremse im
            // Timer-Interrupt, also ausserhalb dieses Fadens.
            spins += 1;
            let sekunden = hal::timer::ticks(0) / 100; // 100 Hz
            if sekunden > 60 || spins > 5_000_000_000 {
                let mut w = [("", true); DONE_FLAGS];
                let _ = all_done(archive, Some(&mut w));
                println!(
                    "bringup : WATCHDOG — nicht alle Aussagen belegt (nach {sekunden}s, {spins} Umdrehungen)"
                );
                print!("bringup : offen waren:");
                for (name, ok) in w.iter() {
                    if !ok {
                        print!(" {name}");
                    }
                }
                println!();
                report_and_off(true);
            }
            system::reap();
            drv_service_step(archive);
            if all_done(archive, None) {
                report_and_off(false);
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
