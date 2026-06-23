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
use sel4lake_abi::{result, sys, GRANT_FLAG, GRANT_RECV_SLOT};
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

// Thread-Lebenszyklus (Ausbaustufe 2).
static VICTIM_COUNT: AtomicU64 = AtomicU64::new(0);
static KILL_SNAP1: AtomicU64 = AtomicU64::new(0); // Victim-Zähler direkt nach KILL
static KILL_SNAP2: AtomicU64 = AtomicU64::new(0); // ... etwas später (muss gleich sein)
static DENIED_KILL: AtomicU64 = AtomicU64::new(0); // KILL ohne Cap (muss != OK sein)
static KILLER_DONE: AtomicBool = AtomicBool::new(false);
static REAPED: AtomicU64 = AtomicU64::new(0); // vom Manager eingesammelte Threads
static FREE_BASELINE: AtomicU64 = AtomicU64::new(0); // freies RAM nach allen Spawns
/// Lokaler Cap-Slot des Killers ohne Cap (Negativtest).
const NO_CAP_SLOT: u64 = 5;

// Notifications (asynchrone Signale).
const NOTIF_BADGE_VAL: u64 = 0xA5;
const NOTIF_ROUNDS: u64 = 5;
static NOTIF_GOT_BADGE: AtomicU64 = AtomicU64::new(0);
static NOTIF_COUNT: AtomicU64 = AtomicU64::new(0);
static PRODUCER_DONE: AtomicBool = AtomicBool::new(false);
static CONSUMER_DONE: AtomicBool = AtomicBool::new(false);

// Capability-Transfer in IPC (Broker delegiert dem Client eine Service-Cap).
static XFER_RESULT: AtomicU64 = AtomicU64::new(0);
static XFER_DONE: AtomicBool = AtomicBool::new(false);
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

    // Thread-Lebenszyklus: Victim (wird cap-kontrolliert getötet) + Killer-PD.
    let victim = system::spawn(victim as *const () as usize, 0, prio).expect("victim");
    let kill_cap = system::install_tcb_cap(victim, Rights::WRITE).expect("tcb cap");
    let killer_pd = system::create_pd().expect("killer pd");
    let killer = system::spawn(killer as *const () as usize, 0, prio).expect("killer");
    system::bind_pd(killer_pd, killer);
    system::install_pd_cap(killer_pd, 0, kill_cap); // Slot 0 = Tcb-Cap; Slot 5 leer

    // Notifications: ein Objekt, zwei abgeleitete Caps (Signal mit Badge, Wait).
    let ntfn = system::create_notification().expect("ntfn");
    let nroot = system::install_notification_cap(ntfn as u32, Rights::RWX).expect("ntfn cap");
    let signal_cap = system::cap_mint(nroot, Rights::WRITE, NOTIF_BADGE_VAL).expect("signal cap");
    let wait_cap = system::cap_mint(nroot, Rights::READ, 0).expect("wait cap");
    let producer_pd = system::create_pd().expect("producer pd");
    let consumer_pd = system::create_pd().expect("consumer pd");
    let producer = system::spawn(producer as *const () as usize, 0, prio).expect("producer");
    system::bind_pd(producer_pd, producer);
    system::install_pd_cap(producer_pd, 0, signal_cap);
    let consumer = system::spawn(consumer as *const () as usize, 0, prio).expect("consumer");
    system::bind_pd(consumer_pd, consumer);
    system::install_pd_cap(consumer_pd, 0, wait_cap);

    // Capability-Transfer: Service-Endpoint + Broker, der dem Client eine
    // svc-Send-Cap per REPLY delegiert.
    let svc_ep = system::create_endpoint().expect("svc ep");
    let svc_root = system::install_endpoint_cap(svc_ep as u32, Rights::RWX).expect("svc cap");
    let svc_recv = system::cap_mint(svc_root, Rights::READ, 0).expect("svc recv");
    let svc_send = system::cap_mint(svc_root, Rights::WRITE, 0).expect("svc send");
    let svc_pd = system::create_pd().expect("svc pd");
    system::install_pd_cap(svc_pd, 0, svc_recv);
    let svc = system::spawn(svc_server as *const () as usize, 0, prio).expect("svc thread");
    system::bind_pd(svc_pd, svc);

    let brk_ep = system::create_endpoint().expect("brk ep");
    let brk_root = system::install_endpoint_cap(brk_ep as u32, Rights::RWX).expect("brk cap");
    let brk_recv = system::cap_mint(brk_root, Rights::READ, 0).expect("brk recv");
    let brk_send = system::cap_mint(brk_root, Rights::WRITE, 0).expect("brk send");
    let brk_pd = system::create_pd().expect("brk pd");
    system::install_pd_cap(brk_pd, 0, brk_recv);
    system::install_pd_cap(brk_pd, 2, svc_send); // Slot 2 = die zu delegierende Cap
    let brk = system::spawn(broker_server as *const () as usize, 0, prio).expect("brk thread");
    system::bind_pd(brk_pd, brk);

    let xfer_pd = system::create_pd().expect("xfer pd");
    system::install_pd_cap(xfer_pd, 0, brk_send); // Slot 0 = Broker; Slot 1 wird per Grant gefüllt
    let xfer = system::spawn(xfer_client as *const () as usize, 0, prio).expect("xfer thread");
    system::bind_pd(xfer_pd, xfer);

    // Freies RAM mit allen Stacks (Baseline für die Rückgewinnungs-Prüfung).
    FREE_BASELINE.store(system::total_free(), Ordering::Relaxed);

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

/// Service-Server: verdreifacht (erreichbar nur über eine transferierte Cap).
extern "C" fn svc_server(_arg: usize) -> ! {
    loop {
        let m = invoke(sys::RECV, 0, [0; 4], 0);
        if m.result != result::OK {
            break;
        }
        invoke(sys::REPLY, 0, [3 * m.msg[0], 0, 0, 0], 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Broker-Server: delegiert dem Aufrufer per REPLY-Grant seine svc-Send-Cap (Slot 2).
extern "C" fn broker_server(_arg: usize) -> ! {
    loop {
        let m = invoke(sys::RECV, 0, [0; 4], 0);
        if m.result != result::OK {
            break;
        }
        // REPLY mit Grant der Cap an lokalem Slot 2.
        invoke(sys::REPLY, 0, [0; 4], GRANT_FLAG | 2);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Transfer-Client: holt sich beim Broker eine svc-Cap und nutzt sie dann.
extern "C" fn xfer_client(_arg: usize) -> ! {
    // 1) Broker rufen -> erhält per Grant eine svc-Send-Cap an GRANT_RECV_SLOT.
    let _ = invoke(sys::CALL, 0, [0; 4], 0);
    // 2) Service über die transferierte Cap rufen (vorher kein Zugriff).
    let r = invoke(sys::CALL, GRANT_RECV_SLOT as u64, [7, 0, 0, 0], 0);
    XFER_RESULT.store(r.msg[0], Ordering::Relaxed);
    XFER_DONE.store(true, Ordering::Release);
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Producer: signalisiert die Notification asynchron, bis der Consumer fertig ist.
extern "C" fn producer(_arg: usize) -> ! {
    while !CONSUMER_DONE.load(Ordering::Acquire) {
        let _ = invoke(sys::SIGNAL, 0, [0; 4], 0); // Notification-Cap an Slot 0
        for _ in 0..250_000 {
            core::hint::spin_loop();
        }
    }
    PRODUCER_DONE.store(true, Ordering::Release);
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Consumer: wartet `NOTIF_ROUNDS`-mal auf die Notification und prüft den Badge.
extern "C" fn consumer(_arg: usize) -> ! {
    let mut n: u64 = 0;
    while n < NOTIF_ROUNDS {
        let r = invoke(sys::WAIT, 0, [0; 4], 0);
        NOTIF_GOT_BADGE.store(r.badge, Ordering::Relaxed);
        n += 1;
        NOTIF_COUNT.store(n, Ordering::Relaxed);
    }
    CONSUMER_DONE.store(true, Ordering::Release);
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Victim: zählt fortlaufend hoch, bis es (cap-kontrolliert) getötet wird.
extern "C" fn victim(_arg: usize) -> ! {
    loop {
        VICTIM_COUNT.fetch_add(1, Ordering::Relaxed);
        for _ in 0..2_000 {
            core::hint::spin_loop();
        }
    }
}

/// Killer (PD mit Tcb-Cap): tötet das Victim, prüft das Einfrieren + den
/// Negativfall (KILL ohne Cap), und beendet sich danach selbst (EXIT).
extern "C" fn killer(_arg: usize) -> ! {
    for _ in 0..1_000_000 {
        core::hint::spin_loop(); // Victim etwas laufen lassen
    }
    // KILL über die Tcb-Cap an Slot 0.
    let _ = invoke(sys::KILL, 0, [0; 4], 0);
    KILL_SNAP1.store(VICTIM_COUNT.load(Ordering::Relaxed), Ordering::Relaxed);
    // Negativtest: KILL über leeren Slot -> verweigert.
    DENIED_KILL.store(invoke(sys::KILL, NO_CAP_SLOT, [0; 4], 0).result, Ordering::Relaxed);
    // Warten und erneut messen: das Victim darf nicht weitergezählt haben.
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }
    KILL_SNAP2.store(VICTIM_COUNT.load(Ordering::Relaxed), Ordering::Relaxed);
    KILLER_DONE.store(true, Ordering::Release);
    // Selbst beenden -> Stack/TCB werden zurückgewonnen.
    invoke(sys::EXIT, 0, [0; 4], 0);
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
        // Beendete Threads einsammeln (sicher: läuft auf dem Idle-Stack).
        let n = system::reap();
        if n > 0 {
            REAPED.fetch_add(n as u64, Ordering::Relaxed);
        }
        if !reloaded && BATCH1_DONE.load(Ordering::Acquire) {
            do_reload();
            RELOADED.store(true, Ordering::Release);
            reloaded = true;
        }
        if !reported && ALL_DONE.load(Ordering::Acquire) && all_done() {
            report();
            reported = true;
        }
        hal::cpu::wfi();
    }
}

fn all_done() -> bool {
    let workers = (0..NWORKERS).all(|i| WORKER_COUNTS[i].load(Ordering::Relaxed) >= THRESHOLD);
    let cores = (0..NUM_CORES).all(|c| hal::timer::ticks(c) > 0);
    let fp = (0..FP_WORKERS).all(|i| FP_DONE[i].load(Ordering::Acquire));
    let prio = (0..NPRIO_TEST).all(|i| PRIO_DONE[i].load(Ordering::Acquire));
    let life = KILLER_DONE.load(Ordering::Acquire) && REAPED.load(Ordering::Relaxed) >= 2;
    let notif = PRODUCER_DONE.load(Ordering::Acquire) && NOTIF_COUNT.load(Ordering::Relaxed) >= NOTIF_ROUNDS;
    let xfer = XFER_DONE.load(Ordering::Acquire);
    workers && cores && fp && prio && life && notif && xfer
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

    // Thread-Lebenszyklus: cap-kontrolliertes KILL + Selbst-EXIT + Rückgewinnung.
    let s1 = KILL_SNAP1.load(Ordering::Relaxed);
    let s2 = KILL_SNAP2.load(Ordering::Relaxed);
    let denied = DENIED_KILL.load(Ordering::Relaxed);
    let reaped = REAPED.load(Ordering::Relaxed);
    let freed = system::total_free().saturating_sub(FREE_BASELINE.load(Ordering::Relaxed));
    println!("life    : victim-count nach KILL={s1}, später={s2} (eingefroren: {})", s1 == s2);
    println!("life    : KILL ohne Cap -> result={denied} (verweigert: {})", denied != result::OK);
    println!("life    : {reaped} Threads eingesammelt, {} KiB Stack zurueckgewonnen", freed / 1024);
    let life_ok = s1 == s2 && denied != result::OK && reaped >= 2 && freed > 0;
    println!("life    : {}", if life_ok { "ALL PASS" } else { "FAILURES" });

    // Notifications: asynchrone Badge-Signale.
    let nb = NOTIF_GOT_BADGE.load(Ordering::Relaxed);
    let nc = NOTIF_COUNT.load(Ordering::Relaxed);
    println!("notif   : {nc} Signale empfangen, Badge={nb:#x} (erwartet {NOTIF_BADGE_VAL:#x})");
    let notif_ok = nc >= NOTIF_ROUNDS && nb == NOTIF_BADGE_VAL;
    println!("notif   : {}", if notif_ok { "ALL PASS" } else { "FAILURES" });

    // Capability-Transfer: Client nutzt eine per IPC vom Broker delegierte Cap.
    let xr = XFER_RESULT.load(Ordering::Relaxed);
    println!("xfer    : svc call(7) ueber transferierte Cap -> {xr} (erwartet 21)");
    println!("xfer    : {}", if xr == 21 { "ALL PASS" } else { "FAILURES" });

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
