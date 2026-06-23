//! Demo-Threads + Hot-Reload (Phase 4–7).
//!
//! Aufbau: ein **Endpoint** (stabile Cap-Identität), ein **Client**-PD (Send-Cap)
//! und ein **Server**-PD. Der Server v1 verdoppelt; nach einem **Hot-Reload**
//! (Phase 7) übernimmt Server v2 *denselben* Endpoint und verdreifacht — ohne
//! Kernel-Neustart und für den Client transparent (gleiche Send-Cap, gleicher
//! Endpoint). Drei Worker-Threads belegen weiterhin die Preemption.
//!
//! Der Reload-Manager ist der Idle-Thread des Primärkerns: Quiesce (Empfänger
//! zurückziehen + Recv-Cap entziehen) → Swap (v2 starten + Recv-Cap delegieren).

use crate::system;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use sel4lake_abi::{result, sys};
use sel4lake_hal::{self as hal, println, syscall::invoke};
use sel4lake_mem::Rights;
use sel4lake_sched::ThreadId;
use sel4lake_sync::SpinLock;

const NUM_CORES: usize = 8;
const NWORKERS: usize = 3;
const THRESHOLD: u64 = 3;
/// FP-Korrektheitstest: zwei Threads summieren `FP_ITERS`-mal `1.5` und werden
/// dabei gegenseitig preemptiert. Nur mit korrekter FP-Kontextsicherung bleibt
/// der (in einem FP-Register gehaltene) Akkumulator über Preemption erhalten.
const FP_WORKERS: usize = 2;
const FP_ITERS: u64 = 50_000;
/// Prioritätstest: drei Threads mit Prioritäten 4 > 3 > 2 (alle über der
/// Standardpriorität 1) führen Arbeit aus und parken sich danach. Der höher
/// priorisierte muss zuerst fertig werden.
const NPRIO_TEST: usize = 3;
const PRIO_WORK: u64 = 300_000;
/// Lokaler Cap-Index des Endpoints im jeweiligen PD-Cspace.
const EP_CAP: u64 = 0;

const B1_IN: [u64; 2] = [5, 6]; // Server v1: verdoppelt -> [10, 12]
const B2_IN: [u64; 2] = [5, 6]; // Server v2: verdreifacht -> [15, 18]

static WORKER_COUNTS: [AtomicU64; NWORKERS] = [const { AtomicU64::new(0) }; NWORKERS];
static FP_RESULT: [AtomicU64; FP_WORKERS] = [const { AtomicU64::new(0) }; FP_WORKERS];
static FP_DONE: [AtomicBool; FP_WORKERS] = [const { AtomicBool::new(false) }; FP_WORKERS];
static PRIO_SEQ: AtomicU64 = AtomicU64::new(0);
static PRIO_FINISH: [AtomicU64; NPRIO_TEST] = [const { AtomicU64::new(0) }; NPRIO_TEST];
static PRIO_DONE: [AtomicBool; NPRIO_TEST] = [const { AtomicBool::new(false) }; NPRIO_TEST];
static R1: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static R2: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static BATCH1_DONE: AtomicBool = AtomicBool::new(false);
static RELOADED: AtomicBool = AtomicBool::new(false);
static ALL_DONE: AtomicBool = AtomicBool::new(false);

/// Zustand, den der Reload-Manager braucht.
#[derive(Clone, Copy)]
struct ReloadInfo {
    ep: usize,
    v1: ThreadId,
    v1_pd: usize,
    v2_pd: usize,
}
static RELOAD_INFO: SpinLock<Option<ReloadInfo>> = SpinLock::new(None);

/// PDs + Endpoint + Caps anlegen und Demo-Threads (v1, Client, Worker) starten.
pub fn spawn_demo() {
    let ep = system::create_endpoint().expect("endpoint");
    let root = system::install_endpoint_cap(ep as u32, Rights::RWX).expect("ep cap");
    let send_cap = system::cap_mint(root, Rights::WRITE, 0).expect("send cap");
    let recv_cap_v1 = system::cap_mint(root, Rights::READ, 0).expect("recv cap v1");
    let recv_cap_v2 = system::cap_mint(root, Rights::READ, 0).expect("recv cap v2");

    let client_pd = system::create_pd().expect("client pd");
    let v1_pd = system::create_pd().expect("v1 pd");
    let v2_pd = system::create_pd().expect("v2 pd"); // Thread erst beim Reload

    system::install_pd_cap(client_pd, EP_CAP as usize, send_cap);
    system::install_pd_cap(v1_pd, EP_CAP as usize, recv_cap_v1);
    system::install_pd_cap(v2_pd, EP_CAP as usize, recv_cap_v2);

    let prio = system::IDLE_PRIO; // gewöhnliche Demo-Threads: Round-Robin mit Idle
    let v1 = system::spawn(server_v1 as *const () as usize, 0, prio).expect("v1 thread");
    system::bind_pd(v1_pd, v1);
    let client = system::spawn(client as *const () as usize, 0, prio).expect("client thread");
    system::bind_pd(client_pd, client);

    for id in 0..NWORKERS {
        system::spawn(worker as *const () as usize, id, prio);
    }
    for id in 0..FP_WORKERS {
        system::spawn(fp_worker as *const () as usize, id, prio);
    }
    // Prioritätstest: höhere Priorität (4) zuerst, dann 3, dann 2.
    for id in 0..NPRIO_TEST {
        system::spawn(prio_thread as *const () as usize, id, 4 - id as u8);
    }

    *RELOAD_INFO.lock() = Some(ReloadInfo { ep, v1, v1_pd, v2_pd });
}

/// Hot-Reload: Server v1 zurückziehen (Quiesce) und v2 starten (Swap).
fn do_reload() {
    let Some(info) = *RELOAD_INFO.lock() else {
        return;
    };
    // Quiesce: v1 als Empfänger zurückziehen (falls blockiert -> geparkt) und
    // ihm die Recv-Cap entziehen (falls er später erneut empfangen will -> Fehler).
    system::endpoint_retire_receiver(info.ep, info.v1);
    system::clear_pd_cap(info.v1_pd, EP_CAP as usize);

    // Swap: v2 starten + an seine (bereits mit Recv-Cap bestückte) PD binden.
    // Atomar gegen Preemption, damit v2 nicht vor dem Bind läuft.
    hal::cpu::local_irq_disable();
    if let Some(v2) = system::spawn(server_v2 as *const () as usize, 0, system::IDLE_PRIO) {
        system::bind_pd(info.v2_pd, v2);
    }
    hal::cpu::local_irq_enable();
}

// --- Demo-Threads ---

extern "C" fn worker(arg: usize) -> ! {
    let id = arg;
    loop {
        if id < NWORKERS {
            WORKER_COUNTS[id].fetch_add(1, Ordering::Relaxed);
        }
        for _ in 0..50_000 {
            core::hint::spin_loop();
        }
    }
}

/// FP-Worker: summiert `FP_ITERS`-mal `1.5` (Akkumulator in einem FP-Register).
/// Wird er mitten in der Berechnung preemptiert, muss der FP-Kontext erhalten
/// bleiben — sonst stimmt das Ergebnis nicht.
extern "C" fn fp_worker(arg: usize) -> ! {
    let id = arg;
    let mut acc: f64 = 0.0;
    let mut i: u64 = 0;
    while i < FP_ITERS {
        acc += 1.5;
        for _ in 0..20 {
            core::hint::spin_loop();
        }
        i += 1;
    }
    if id < FP_WORKERS {
        FP_RESULT[id].store(acc.to_bits(), Ordering::Relaxed);
        FP_DONE[id].store(true, Ordering::Release);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Prioritätstest-Thread: feste Arbeit, dann Reihenfolge festhalten und parken.
extern "C" fn prio_thread(arg: usize) -> ! {
    let id = arg;
    let mut i: u64 = 0;
    while i < PRIO_WORK {
        core::hint::spin_loop();
        i += 1;
    }
    if id < NPRIO_TEST {
        let pos = PRIO_SEQ.fetch_add(1, Ordering::AcqRel);
        PRIO_FINISH[id].store(pos, Ordering::Relaxed);
        PRIO_DONE[id].store(true, Ordering::Release);
    }
    // Selbst-parken: gibt die CPU dauerhaft an niedriger priorisierte Threads frei.
    invoke(sys::PARK, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Server v1: verdoppelt. Bei entzogener Cap (Recv-Fehler) zieht er sich zurück.
extern "C" fn server_v1(_arg: usize) -> ! {
    serve(2)
}

/// Server v2: verdreifacht (das „neu geladene" Modul).
extern "C" fn server_v2(_arg: usize) -> ! {
    serve(3)
}

/// Gemeinsame Server-Schleife: empfangen, mit `factor` multiplizieren, antworten.
fn serve(factor: u64) -> ! {
    loop {
        let m = invoke(sys::RECV, EP_CAP, [0; 4], 0);
        if m.result != result::OK {
            break; // Cap entzogen -> Komponente zurückgezogen
        }
        invoke(sys::REPLY, EP_CAP, [factor * m.msg[0], 0, 0, 0], 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Client: Batch 1 (bedient von v1), dann auf den Hot-Reload warten, dann
/// Batch 2 (bedient von v2) — durchgehend über dieselbe Send-Cap.
extern "C" fn client(_arg: usize) -> ! {
    for (i, &v) in B1_IN.iter().enumerate() {
        R1[i].store(invoke(sys::CALL, EP_CAP, [v, 0, 0, 0], 0).msg[0], Ordering::Relaxed);
    }
    BATCH1_DONE.store(true, Ordering::Release);

    while !RELOADED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    for (i, &v) in B2_IN.iter().enumerate() {
        R2[i].store(invoke(sys::CALL, EP_CAP, [v, 0, 0, 0], 0).msg[0], Ordering::Relaxed);
    }
    ALL_DONE.store(true, Ordering::Release);
    loop {
        core::hint::spin_loop();
    }
}

// --- Reload-Manager + Bericht (Idle-Thread des Primärkerns) ---

pub fn demo_report_then_idle() -> ! {
    let mut reloaded = false;
    let mut reported = false;
    loop {
        if !reloaded && BATCH1_DONE.load(Ordering::Acquire) {
            do_reload();
            RELOADED.store(true, Ordering::Release);
            reloaded = true;
        }
        if !reported && ALL_DONE.load(Ordering::Acquire) && cores_and_workers_ok() {
            report();
            reported = true;
        }
        hal::cpu::wfi();
    }
}

fn cores_and_workers_ok() -> bool {
    let workers = (0..NWORKERS).all(|i| WORKER_COUNTS[i].load(Ordering::Relaxed) >= THRESHOLD);
    let cores = (0..NUM_CORES).all(|c| hal::timer::ticks(c) > 0);
    let fp = (0..FP_WORKERS).all(|i| FP_DONE[i].load(Ordering::Acquire));
    let prio = (0..NPRIO_TEST).all(|i| PRIO_DONE[i].load(Ordering::Acquire));
    workers && cores && fp && prio
}

fn report() {
    let mut sched_ok = true;
    for c in 0..NUM_CORES {
        let t = hal::timer::ticks(c);
        println!("sched   : core {c} ticks={t}");
        if t == 0 {
            sched_ok = false;
        }
    }
    for i in 0..NWORKERS {
        let n = WORKER_COUNTS[i].load(Ordering::Relaxed);
        println!("sched   : worker {i} count={n}");
        if n < THRESHOLD {
            sched_ok = false;
        }
    }
    println!("sched   : {}", if sched_ok { "ALL PASS" } else { "FAILURES" });

    // FP/SIMD-Kontext über Preemption korrekt erhalten.
    let mut fp_ok = true;
    let expected = (FP_ITERS as f64) * 1.5;
    for i in 0..FP_WORKERS {
        let acc = f64::from_bits(FP_RESULT[i].load(Ordering::Relaxed));
        println!("fp      : worker {i} -> {} (erwartet {})", acc as u64, expected as u64);
        if acc != expected {
            fp_ok = false;
        }
    }
    println!("fp      : {}", if fp_ok { "ALL PASS" } else { "FAILURES" });

    // Prioritäten: höher priorisierter Thread (id 0, prio 4) muss zuerst fertig sein.
    for i in 0..NPRIO_TEST {
        println!(
            "prio    : id {i} (prio {}) fertig als #{}",
            4 - i,
            PRIO_FINISH[i].load(Ordering::Relaxed)
        );
    }
    let f0 = PRIO_FINISH[0].load(Ordering::Relaxed);
    let f1 = PRIO_FINISH[1].load(Ordering::Relaxed);
    let f2 = PRIO_FINISH[2].load(Ordering::Relaxed);
    let prio_ok = f0 < f1 && f1 < f2;
    println!("prio    : {}", if prio_ok { "ALL PASS" } else { "FAILURES" });

    // Batch 1 (Server v1, verdoppelt) — cap-gesicherte IPC funktioniert.
    let mut ipc_ok = true;
    for (i, &v) in B1_IN.iter().enumerate() {
        let r = R1[i].load(Ordering::Relaxed);
        println!("ipc     : v1 call({v}) -> {r} (erwartet {})", 2 * v);
        if r != 2 * v {
            ipc_ok = false;
        }
    }
    println!("ipc     : {}", if ipc_ok { "ALL PASS" } else { "FAILURES" });

    // Batch 2 (Server v2 nach Hot-Reload, verdreifacht) — gleicher Endpoint!
    let mut reload_ok = true;
    for (i, &v) in B2_IN.iter().enumerate() {
        let r = R2[i].load(Ordering::Relaxed);
        println!("reload  : v2 call({v}) -> {r} (erwartet {})", 3 * v);
        if r != 3 * v {
            reload_ok = false;
        }
    }
    println!(
        "reload  : {} (Komponente ohne Kernel-Neustart getauscht)",
        if reload_ok { "ALL PASS" } else { "FAILURES" }
    );
}
