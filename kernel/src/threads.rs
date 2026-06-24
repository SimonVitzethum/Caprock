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
use sel4lake_mem::{peek_u64, poke_u64, Rights};
use sel4lake_sched::ThreadId;
use sel4lake_sync::SpinLock;

const NUM_CORES: usize = 8;
const NWORKERS: usize = 3;
const THRESHOLD: u64 = 3;
/// Lazy-FP-Test: zwei **echte EL0-User-Threads** halten je ein eindeutiges Muster
/// in FP-Registern (d0..d3) und geben per `YIELD` gegenseitig ab. Der Kernel ist
/// soft-float und FP trappt nur an EL0 -> der Lazy-Owner-Wechsel (FP-Trap EC 0x07)
/// muss das Muster über jede Abgabe hinweg korrekt sichern/wiederherstellen, sonst
/// erkennt der Thread eine Korruption und meldet KEINEN Erfolg.
const FP_WORKERS: usize = 2;
const FP_CHECK_ITERS: u64 = 200; // Iterationen je Thread, jede mit YIELD (Owner-Wechsel)
const FP_PRIO: u8 = 5; // eigene, höchste Demo-Priorität -> striktes A<->B-Ping-Pong
const FP_PATTERN: [u64; FP_WORKERS] = [0xA5A5_5A5A_3C3C_C3C3, 0x1234_5678_9ABC_DEF0];
const FP_ALL_OK: u64 = (1 << FP_WORKERS) - 1;
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
/// Erfolgs-Maske der EL0-FP-Threads (Bit i = Thread i meldete unbeschädigtes Muster).
static FP_OK_MASK: AtomicU64 = AtomicU64::new(0);
static FP_COLLECTOR_DONE: AtomicBool = AtomicBool::new(false);
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

// EL0-Userland: ein echter EL0-User-Thread ruft per Syscall einen EL1-Server.
const USER_MAGIC: u64 = 0xC0DE;
static USER_RECV: AtomicU64 = AtomicU64::new(0);

// Reclaim-Test: transiente EL0-Exiter erzeugen, mehr als der Kernel-Stack-Pool (8)
// gleichzeitig fasst — gelingt nur, wenn beim Thread-Ende der Pool-Slot zurückkommt.
// Churn-/Leak-Test (Härtung Ziel 2): tausende spawn/destroy-Zyklen isolierter PDs;
// danach müssen MEM/TCB/ASID/kstack-Stände exakt zur Baseline zurückkehren.
const CHURN_TARGET: u32 = 2000;
static CHURN_DONE: AtomicBool = AtomicBool::new(false);
static CHURN_OK: AtomicBool = AtomicBool::new(false);

const RECLAIM_TARGET: u64 = 16;
const RECLAIM_PRIO: u8 = 6; // höchste Demo-Prio: Exiter läuft sofort + beendet sich
static RECLAIM_SPAWNED: AtomicU64 = AtomicU64::new(0);

// Lastausgleich: lastbewusst platzierte Worker landen auf den am wenigsten
// belasteten Kernen (nicht alle auf dem Bootkern) -> Verteilung über die Kerne.
const BALANCED_TARGET: u64 = 16;
static BALANCED_SPAWNED: AtomicU64 = AtomicU64::new(0);
static BALANCED_PLACE: [AtomicU64; NUM_CORES] = [const { AtomicU64::new(0) }; NUM_CORES];

// Weg C (Hybrid): isolierte EL0-PD mit eigener VSpace vs. vertrauenswürdige SAS-PD.
// Beide versuchen, dieselbe fremde RAM-Adresse X zu lesen: die SAS-PD darf (X ist im
// SAS EL0-RW), die isolierte PD faultet (X ist in ihrer VSpace EL1-only -> Kernel
// beendet sie). Belegt: nur per-Prozess-VSpace gibt echte User<->User-Trennung.
const ISO_SECRET: u64 = 0x5E_C4E7_5E_C4E7;
const ISO_BADGE_RAN: u64 = 1 << 0; // isolierte PD lief + IPC ging (vor dem Fault)
const ISO_BADGE_READ: u64 = 1 << 1; // isolierte PD las X (DARF NIE kommen)
const ISO_BADGE_TRUSTED: u64 = 1 << 2; // SAS-PD las X erfolgreich
const ISO_BADGE_MAPPED: u64 = 1 << 3; // VMM-Probe: Frame gemappt + RW erfolgreich
const ISO_BADGE_SHARED: u64 = 1 << 4; // Reader las den Wert des Writers via Shared Frame
const ISO_BADGE_NATIVE: u64 = 1 << 5; // privat geladener Code lief in eigener VSpace
const ISO_BADGE_PAGES: u64 = 1 << 6; // 4-KiB-Seiten: RW-Schreiben + RO-Lesen erfolgreich
static ISO_MASK: AtomicU64 = AtomicU64::new(0); // gesammelte Badges
static ISO_SECRET_ADDR: AtomicU64 = AtomicU64::new(0); // Adresse X (fremdes RAM)

// Shared-Memory-IPC: zwei isolierte PDs teilen einen Frame F (in beide VSpaces
// gemappt). Der Writer schreibt SHM_SECRET, der Reader liest es (nach Notification).
// Der Wert ist in den shm_writer/shm_reader-Asm-Blöcken als Immediate kodiert; diese
// Konstante dokumentiert ihn (Quelle der Wahrheit für den Kommentar).
#[allow(dead_code)]
const SHM_SECRET: u64 = 0x5A5A_1234_5678_9ABC;

// Kern-übergreifende synchrone IPC: Client auf core 0 ruft (CALL) einen Server auf
// core 2; Antwort (REPLY) geht zurück über die Kerngrenze. Beide Richtungen wecken
// den Partner per Cross-Core-IPI.
const XIPC_N: usize = 3;
const XIPC_IN: [u64; XIPC_N] = [3, 5, 7];
const XIPC_FACTOR: u64 = 7; // Server antwortet mit FACTOR * Eingabe
const XIPC_SERVER_CORE: usize = 2;
static XIPC_RESULT: [AtomicU64; XIPC_N] = [const { AtomicU64::new(0) }; XIPC_N];
static XIPC_DONE: AtomicBool = AtomicBool::new(false);

// Per-Kern-paralleler Scheduler: je ein Worker auf den Sekundärkernen 1..NUM_CORES
// läuft PARALLEL (eigene Scheduler-Instanz je Kern, kein globaler Lock); ein
// Parker auf core 1 wird kern-übergreifend von core 0 per IPI geweckt.
const SMP_WORK_TARGET: u64 = 5;
static XCORE_PROGRESS: [AtomicU64; NUM_CORES] = [const { AtomicU64::new(0) }; NUM_CORES];
static XCORE_PARKED: AtomicBool = AtomicBool::new(false); // Parker hat sich blockiert
static XCORE_WOKEN: AtomicBool = AtomicBool::new(false); // Parker nach IPI-Wake fortgesetzt
static XCORE_PARKER_TID: AtomicU64 = AtomicU64::new(u64::MAX); // raw ThreadId des Parkers

// Stateful Hot-Reload: Zähler-Service, dessen Zustand (in einer Memory-Region)
// den Komponententausch v1(+1) -> v2(+10) überlebt.
static CS_STATE_BASE: AtomicU64 = AtomicU64::new(0);
static CS_R1: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3]; // v1: 1,2,3
static CS_R2: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2]; // v2: 13,23 (Zustand erhalten)
static CS_BATCH1_DONE: AtomicBool = AtomicBool::new(false);
static CS_RELOADED: AtomicBool = AtomicBool::new(false);
static CS_DONE: AtomicBool = AtomicBool::new(false);
static CS_RELOAD_INFO: SpinLock<Option<ReloadInfo>> = SpinLock::new(None);
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
    // Lazy-FP: ein EL1-Kollektor + zwei EL0-FP-User-Threads (höchste Demo-Prio ->
    // striktes Ping-Pong über YIELD, das jede Iteration einen Owner-Wechsel erzwingt).
    let fp_ntfn = system::create_notification().expect("fp ntfn");
    let fp_nroot = system::install_notification_cap(fp_ntfn as u32, Rights::RWX).expect("fp ntfn cap");
    let fp_wait = system::cap_mint(fp_nroot, Rights::READ, 0).expect("fp wait cap");
    let fp_coll_pd = system::create_pd().expect("fp collector pd");
    system::install_pd_cap(fp_coll_pd, 0, fp_wait);
    let fp_coll = system::spawn(fp_collector as *const () as usize, 0, prio).expect("fp collector");
    system::bind_pd(fp_coll_pd, fp_coll);
    let fp_entry = core::ptr::addr_of!(user_fp_entry) as usize;
    for id in 0..FP_WORKERS {
        let sig = system::cap_mint(fp_nroot, Rights::WRITE, 1 << id).expect("fp signal cap");
        let pd = system::create_pd().expect("fp pd");
        system::install_pd_cap(pd, 0, sig);
        let t = system::spawn_user(fp_entry, FP_PATTERN[id] as usize, FP_PRIO)
            .expect("fp user thread");
        system::bind_pd(pd, t);
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

    // Stateful Hot-Reload: Zähler-Service, dessen Zustand in einer Memory-Region
    // liegt und den Tausch v1(+1) -> v2(+10) überlebt.
    let cs_state = {
        let s = system::alloc(4096, 16).expect("cs state");
        s.base()
    };
    poke_u64(cs_state, 0); // Zähler initialisieren
    CS_STATE_BASE.store(cs_state, Ordering::Relaxed);
    let cs_ep = system::create_endpoint().expect("cs ep");
    let cs_root = system::install_endpoint_cap(cs_ep as u32, Rights::RWX).expect("cs cap");
    let cs_send = system::cap_mint(cs_root, Rights::WRITE, 0).expect("cs send");
    let cs_recv1 = system::cap_mint(cs_root, Rights::READ, 0).expect("cs recv1");
    let cs_recv2 = system::cap_mint(cs_root, Rights::READ, 0).expect("cs recv2");
    let cs_v1_pd = system::create_pd().expect("cs v1 pd");
    let cs_v2_pd = system::create_pd().expect("cs v2 pd");
    let cs_client_pd = system::create_pd().expect("cs client pd");
    system::install_pd_cap(cs_v1_pd, EP_CAP as usize, cs_recv1);
    system::install_pd_cap(cs_v2_pd, EP_CAP as usize, cs_recv2);
    system::install_pd_cap(cs_client_pd, EP_CAP as usize, cs_send);
    let cs_v1 = system::spawn(counter_v1 as *const () as usize, cs_state as usize, prio).expect("cs v1");
    system::bind_pd(cs_v1_pd, cs_v1);
    let csc = system::spawn(cs_client as *const () as usize, 0, prio).expect("cs client");
    system::bind_pd(cs_client_pd, csc);
    *CS_RELOAD_INFO.lock() = Some(ReloadInfo {
        ep: cs_ep,
        v1: cs_v1,
        v1_pd: cs_v1_pd,
        v2_pd: cs_v2_pd,
    });

    // EL0-Userland: ein EL1-Server + ein echter EL0-User-Thread, der per Syscall ruft.
    let uep = system::create_endpoint().expect("user ep");
    let uroot = system::install_endpoint_cap(uep as u32, Rights::RWX).expect("user ep cap");
    let usend = system::cap_mint(uroot, Rights::WRITE, 0).expect("user send");
    let urecv = system::cap_mint(uroot, Rights::READ, 0).expect("user recv");
    let usrv_pd = system::create_pd().expect("user server pd");
    system::install_pd_cap(usrv_pd, 0, urecv);
    let usrv = system::spawn(user_server as *const () as usize, 0, prio).expect("user server");
    system::bind_pd(usrv_pd, usrv);
    let user_pd = system::create_pd().expect("user pd");
    system::install_pd_cap(user_pd, 0, usend);
    let ut = system::spawn_user(user_entry as *const () as usize, 0, prio).expect("user thread");
    system::bind_pd(user_pd, ut);

    // EL0-Isolation: ein bösartiger EL0-Thread liest EL1-only Kernel-Speicher. Der
    // Kernel muss ihn isolieren (Thread beenden) statt anzuhalten.
    let bad_pd = system::create_pd().expect("bad user pd");
    let bad = system::spawn_user(bad_user as *const () as usize, 0, prio).expect("bad user thread");
    system::bind_pd(bad_pd, bad);

    // Per-Kern-paralleler Scheduler: je einen Worker auf JEDEN Sekundärkern (1..N)
    // einplanen (die Kerne booten gleich per PSCI und picken ihn auf). Sie laufen
    // parallel zu core 0 — jeder Kern schedult über seine eigene Instanz.
    for c in 1..NUM_CORES {
        system::spawn_on_core(c, xcore_worker as *const () as usize, c, prio);
    }
    // Cross-Core-Wake: ein Parker auf core 1 (höhere Prio als Idle), den core 0
    // später per IPI weckt.
    if let Some(parker) = system::spawn_on_core(1, xcore_parker as *const () as usize, 0, 2) {
        XCORE_PARKER_TID.store(parker.to_raw(), Ordering::Relaxed);
    }

    // Kern-übergreifende synchrone IPC: Server auf core 2, Client auf core 0, ein
    // Endpoint. CALL und REPLY queren die Kerngrenze (Cross-Core-Rendezvous + IPI).
    let xep = system::create_endpoint().expect("xipc ep");
    let xroot = system::install_endpoint_cap(xep as u32, Rights::RWX).expect("xipc ep cap");
    let xsend = system::cap_mint(xroot, Rights::WRITE, 0).expect("xipc send");
    let xrecv = system::cap_mint(xroot, Rights::READ, 0).expect("xipc recv");
    let xsrv_pd = system::create_pd().expect("xipc server pd");
    system::install_pd_cap(xsrv_pd, 0, xrecv);
    let xsrv = system::spawn_on_core(XIPC_SERVER_CORE, xipc_server as *const () as usize, 0, prio)
        .expect("xipc server");
    system::bind_pd(xsrv_pd, xsrv);
    let xcli_pd = system::create_pd().expect("xipc client pd");
    system::install_pd_cap(xcli_pd, 0, xsend);
    let xcli = system::spawn_on_core(0, xipc_client as *const () as usize, 0, prio).expect("xipc client");
    system::bind_pd(xcli_pd, xcli);

    // --- Weg C (Hybrid): isolierte PD vs. SAS-PD lesen dieselbe fremde Adresse X ---
    // X ist eine fremde RAM-Adresse (eigene kleine Region) mit einem Geheimwert. Die
    // MemoryCap wird verworfen -> die Region bleibt belegt (der Allokator reklamiert
    // nur per free), niemand sonst bekommt sie. Die Proben lesen X roh (kein Cap nötig).
    let xaddr = system::alloc(4096, 4096).expect("secret region").region().base;
    poke_u64(xaddr, ISO_SECRET); // Kernel (EL1) schreibt das Geheimnis
    ISO_SECRET_ADDR.store(xaddr, Ordering::Relaxed);

    // Notification + Kollektor; jede Probe bekommt eine SIGNAL-Cap mit eigenem Badge.
    let intf = system::create_notification().expect("iso ntfn");
    let introot = system::install_notification_cap(intf as u32, Rights::RWX).expect("iso ntfn cap");
    let iso_wait = system::cap_mint(introot, Rights::READ, 0).expect("iso wait");
    let icoll_pd = system::create_pd().expect("iso collector pd");
    system::install_pd_cap(icoll_pd, 0, iso_wait);
    let icoll = system::spawn(iso_collector as *const () as usize, 0, prio).expect("iso collector");
    system::bind_pd(icoll_pd, icoll);

    // Vertrauenswürdige SAS-Probe: liest X (erlaubt) und meldet Badge TRUSTED.
    let t_sig = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_TRUSTED).expect("trusted sig");
    let t_pd = system::create_pd().expect("trusted probe pd");
    system::install_pd_cap(t_pd, 0, t_sig);
    let tp = system::spawn_user(trusted_probe as *const () as usize, xaddr as usize, prio)
        .expect("trusted probe");
    system::bind_pd(t_pd, tp);

    // Isolierte Probe (eigene VSpace): Slot 0 = Badge RAN, Slot 1 = Badge READ.
    let i_sig0 = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_RAN).expect("iso sig0");
    let i_sig1 = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_READ).expect("iso sig1");
    let i_pd = system::create_pd().expect("iso probe pd");
    system::install_pd_cap(i_pd, 0, i_sig0);
    system::install_pd_cap(i_pd, 1, i_sig1);
    if let Some((ip, _region)) = system::spawn_isolated(iso_probe as *const () as usize, xaddr as usize, prio) {
        system::bind_pd(i_pd, ip);
    }

    // Allgemeiner VMM: eine isolierte PD mappt/entmappt einen **4-KiB**-Frame G per
    // Syscall (cap-gated, feingranular). Slot 0 = Memory-Cap fuer G, Slot 1 = SIGNAL.
    let gframe = system::alloc(4096, 4096).expect("g frame");
    let gbase = gframe.region().base;
    let groot = system::cap_install(gframe).expect("g cap");
    let gmem = system::cap_mint(groot, Rights::WRITE, 0).expect("g mem cap");
    let vmm_sig = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_MAPPED).expect("vmm sig");
    let vmm_pd = system::create_pd().expect("vmm pd");
    system::install_pd_cap(vmm_pd, 0, gmem); // Slot 0 = Memory-Cap (MAP/UNMAP)
    system::install_pd_cap(vmm_pd, 1, vmm_sig); // Slot 1 = SIGNAL (Badge MAPPED)
    if let Some((vp, _)) = system::spawn_isolated(vmm_probe as *const () as usize, gbase as usize, prio) {
        system::bind_pd(vmm_pd, vp);
    }

    // Shared-Memory-IPC: zwei isolierte PDs teilen den Frame F (in BEIDE VSpaces
    // gemappt, identity -> gleiche Adresse). Writer schreibt SHM_SECRET + signalisiert;
    // Reader wartet, liest F und meldet Erfolg. Zero-Copy ueber die Isolationsgrenze,
    // nur ueber cap-gewaehrten Frame + IPC; die VSpaces teilen sonst nichts.
    let fframe = system::alloc(hal::mmu::ISO_REGION_SIZE, hal::mmu::ISO_REGION_SIZE).expect("shared F");
    let fbase = fframe.region().base; // Adresse des Frames (identity-VA in beiden PDs)
    let froot = system::cap_install(fframe).expect("F cap"); // EINE Cap fuer denselben Frame
    let shmn = system::create_notification().expect("shm ntfn");
    let shmnroot = system::install_notification_cap(shmn as u32, Rights::RWX).expect("shm ntfn cap");
    // Writer-PD: Slot 0 = F-Cap, Slot 1 = shm-SIGNAL (Badge != 0, sonst ginge ein
    // Signal vor dem WAIT des Readers verloren -> pending bliebe 0).
    let w_f = system::cap_mint(froot, Rights::WRITE, 0).expect("w F");
    let w_sig = system::cap_mint(shmnroot, Rights::WRITE, 1).expect("w sig");
    let w_pd = system::create_pd().expect("shm writer pd");
    system::install_pd_cap(w_pd, 0, w_f);
    system::install_pd_cap(w_pd, 1, w_sig);
    if let Some((wp, _)) = system::spawn_isolated(shm_writer as *const () as usize, fbase as usize, prio) {
        system::bind_pd(w_pd, wp);
    }
    // Reader-PD: Slot 0 = F-Cap (derselbe Frame!), Slot 1 = shm-WAIT, Slot 2 = SIGNAL.
    let r_f = system::cap_mint(froot, Rights::WRITE, 0).expect("r F");
    let r_wait = system::cap_mint(shmnroot, Rights::READ, 0).expect("r wait");
    let r_done = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_SHARED).expect("r done");
    let r_pd = system::create_pd().expect("shm reader pd");
    system::install_pd_cap(r_pd, 0, r_f);
    system::install_pd_cap(r_pd, 1, r_wait);
    system::install_pd_cap(r_pd, 2, r_done);
    if let Some((rp, _)) = system::spawn_isolated(shm_reader as *const () as usize, fbase as usize, prio) {
        system::bind_pd(r_pd, rp);
    }

    // Natives Code-Laden: eine isolierte PD fuehrt PRIVAT geladenen Code (Kopie des
    // native_template) aus einer eigenen EL0-RX-Region aus (nicht die geteilte
    // .user_text). Slot 0 = SIGNAL-Cap (Badge NATIVE).
    let nat_sig = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_NATIVE).expect("nat sig");
    let nat_pd = system::create_pd().expect("native pd");
    system::install_pd_cap(nat_pd, 0, nat_sig);
    if let Some(nt) =
        system::spawn_isolated_native(native_template as *const () as *const u8, 64, prio)
    {
        system::bind_pd(nat_pd, nt);
    }

    // 4-KiB-Seiten: eine isolierte PD bekommt eine 12-KiB-Region feingranular gemappt
    // -- P (RW), P+4KiB (RO, vorbefuellt), P+8KiB (Guard, ungemappt). Beweist mehrere
    // einzelne Seiten mit gemischten Rechten + Guard-Page-Fault.
    let preg = system::alloc(3 * 4096, 4096).expect("page region").region().base;
    poke_u64(preg + 4096, 0x5EAD_DA7A); // RO-Seite vorbefuellen (Inhalt fuer den Reader)
    let p_sig = system::cap_mint(introot, Rights::WRITE, ISO_BADGE_PAGES).expect("page sig");
    let p_pd = system::create_pd().expect("page pd");
    system::install_pd_cap(p_pd, 0, p_sig);
    if let Some((pp, _)) = system::spawn_isolated(page_probe as *const () as usize, preg as usize, prio) {
        system::bind_pd(p_pd, pp);
        system::map_into_thread(pp, preg, 4096, 1); // P: RW
        system::map_into_thread(pp, preg + 4096, 4096, 0); // P+4KiB: RO
        // P+8KiB bleibt ungemappt (Guard).
    }

    *RELOAD_INFO.lock() = Some(ReloadInfo { ep, v1, v1_pd, v2_pd });
}

/// Generischer Hot-Reload-Swap: v1 zurückziehen (Quiesce: Empfänger entfernen +
/// Recv-Cap entziehen), dann v2 (mit Argument) starten und an seine PD binden.
fn reload_swap(info: ReloadInfo, v2_entry: usize, v2_arg: usize) {
    system::endpoint_retire_receiver(info.ep, info.v1);
    system::clear_pd_cap(info.v1_pd, EP_CAP as usize);
    // Atomar gegen Preemption, damit v2 nicht vor dem Bind läuft.
    hal::cpu::local_irq_disable();
    if let Some(v2) = system::spawn(v2_entry, v2_arg, system::IDLE_PRIO) {
        system::bind_pd(info.v2_pd, v2);
    }
    hal::cpu::local_irq_enable();
}

/// Stateless Hot-Reload (Phase 7): Server v1 (verdoppelt) -> v2 (verdreifacht).
fn do_reload() {
    if let Some(info) = *RELOAD_INFO.lock() {
        reload_swap(info, server_v2 as *const () as usize, 0);
    }
}

/// Stateful Hot-Reload: Zähler-Service v1 (+1) -> v2 (+10); der Zustand (in der
/// Memory-Region bei CS_STATE_BASE) bleibt erhalten (zero-copy, dieselbe Region).
fn do_cs_reload() {
    if let Some(info) = *CS_RELOAD_INFO.lock() {
        let state = CS_STATE_BASE.load(Ordering::Relaxed) as usize;
        reload_swap(info, counter_v2 as *const () as usize, state);
    }
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

// Die EL0-User-Programme kodieren die Syscall-Nummern als Immediates; hier
// absichern, dass sie zur ABI passen.
const _: () = assert!(
    sys::YIELD == 0
        && sys::EXIT == 6
        && sys::SIGNAL == 8
        && sys::WAIT == 9
        && sys::PARK == 5
        && sys::MAP == 10
        && sys::UNMAP == 11
);

/// **EL0-Exiter** (Sektion `.user_text`, EL0-ausführbar): beendet sich sofort per
/// `EXIT`-Syscall. Für den Reclaim-Test transient erzeugt; beim Exit gibt der Kernel
/// seinen EL1-only Kernel-Stack-Pool-Slot zurück, sodass er wiederverwendbar ist.
#[link_section = ".user_text"]
extern "C" fn exiter_entry(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; `svc` ist die einzige Kernel-Interaktion.
    unsafe {
        core::arch::asm!(
            "mov x0, #6", // sys::EXIT
            "svc #0",
            options(noreturn),
        );
    }
}

// **EL0-FP-User-Programm** (`user_fp_entry`, Sektion `.user_text`, EL0-ausführbar).
// Lädt das Muster (Argument in x0) in d0..d3 und prüft in jeder Iteration, dass es
// erhalten ist; danach gibt es per `YIELD` ab (der andere FP-Thread stiehlt dabei
// die FP-Register). Nur korrektes Lazy-Save/Restore lässt das Muster jede Abgabe
// überleben. Bei Erfolg `SIGNAL` (Slot 0) an den Kollektor, sonst stilles Parken
// (Test scheitert per Timeout).
//
// Eigener `global_asm!`-Block mit `.arch armv8-a`, damit die `fmov`-Instruktionen
// trotz `-neon`-Kernel assemblieren — eigene Übersetzungseinheit, daher kein
// soft-float/NEON-ABI-Problem (anders als `#[target_feature]`) und kein Leck der
// `.arch`-Direktive in den Modulstrom. Register: x9=Muster, x10=Zähler, x11=Puffer;
// der Kernel-Trap sichert x0..x30, daher überleben sie die `svc`.
core::arch::global_asm!(
    r#"
.arch armv8-a
.section .user_text,"ax"
.globl user_fp_entry
user_fp_entry:
    mov   x9, x0                 // x9 = Muster (Entry-Argument)
    fmov  d0, x9                 // Muster in d0..d3
    fmov  d1, x9
    fmov  d2, x9
    fmov  d3, x9
    mov   x10, #{iters}
20:
    fmov  x11, d0                // Muster prüfen (nach jeder Abgabe erhalten?)
    cmp   x11, x9
    b.ne  30f
    fmov  x11, d1
    cmp   x11, x9
    b.ne  30f
    fmov  x11, d2
    cmp   x11, x9
    b.ne  30f
    fmov  x11, d3
    cmp   x11, x9
    b.ne  30f
    mov   x0, #0                 // sys::YIELD -> FP-Owner abgeben
    svc   #0
    subs  x10, x10, #1
    b.ne  20b
    mov   x0, #8                 // sys::SIGNAL (Slot 0): Erfolg melden
    mov   x1, #0
    svc   #0
10:
    mov   x0, #5                 // sys::PARK (dauerhaft)
    svc   #0
    b     10b
30:
    mov   x0, #5                 // Korruption erkannt: ohne SIGNAL parken
    svc   #0
    b     30b
"#,
    iters = const FP_CHECK_ITERS,
);

extern "C" {
    /// Entry-Symbol des EL0-FP-User-Programms (siehe `global_asm!` oben).
    static user_fp_entry: u8;
}

/// FP-Kollektor (EL1): wartet auf die Erfolgs-Notifications der EL0-FP-Threads und
/// sammelt ihre Badges (Bit i je Thread). Sobald alle gemeldet haben, ist der
/// Lazy-FP-Test bestanden.
extern "C" fn fp_collector(_arg: usize) -> ! {
    loop {
        let r = invoke(sys::WAIT, 0, [0; 4], 0);
        if r.result == result::OK {
            let mask = FP_OK_MASK.fetch_or(r.badge, Ordering::Relaxed) | r.badge;
            if mask == FP_ALL_OK {
                FP_COLLECTOR_DONE.store(true, Ordering::Release);
            }
        }
    }
}

/// Per-Kern-Worker (EL1): läuft auf einem **Sekundärkern** und macht Fortschritt,
/// während core 0 seine eigene Demo abarbeitet — Beleg für parallele Einplanung
/// (jeder Kern schedult über seine eigene Scheduler-Instanz, kein globaler Lock).
/// `arg` = die Kern-ID (Index in `XCORE_PROGRESS`). Danach selbst parken.
extern "C" fn xcore_worker(arg: usize) -> ! {
    let core = arg;
    let mut n = 0;
    while n < SMP_WORK_TARGET {
        // Moderate Arbeit: unter single-threaded QEMU-TCG teilen sich alle Kerne
        // EINE Host-CPU, daher die Sekundärkern-Last bewusst klein halten.
        for _ in 0..20_000 {
            core::hint::spin_loop();
        }
        n += 1;
        if core < NUM_CORES {
            XCORE_PROGRESS[core].store(n, Ordering::Relaxed);
        }
    }
    loop {
        invoke(sys::PARK, 0, [0; 4], 0);
    }
}

/// **Isolierte EL0-Probe** (`.user_text`, läuft in eigener VSpace). x0 = Adresse X
/// (fremdes RAM). Meldet erst per `SIGNAL` (Slot 0, Badge RAN), dass sie auf EL0 in
/// ihrer VSpace läuft und IPC funktioniert; **dann** liest sie X — das ist in ihrer
/// VSpace EL1-only → Fault → der Kernel beendet sie. Der zweite `SIGNAL` (Slot 1,
/// Badge READ) wird daher NIE erreicht (käme er, wäre die Isolation gebrochen).
#[link_section = ".user_text"]
extern "C" fn iso_probe(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; `svc`/`ldr` sind die einzigen Operationen.
    unsafe {
        core::arch::asm!(
            "mov x9, x0",   // x9 = X (fremde Adresse)
            "mov x0, #8",   // sys::SIGNAL
            "mov x1, #0",   // Slot 0 (Badge RAN)
            "svc #0",
            "ldr x2, [x9]", // X lesen -> FAULT in isolierter VSpace (EL1-only)
            "mov x0, #8",   // (unerreichbar bei intakter Isolation) Slot 1 (Badge READ)
            "mov x1, #1",
            "svc #0",
        "1:",
            "mov x0, #5",   // sys::PARK
            "svc #0",
            "b 1b",
            options(noreturn),
        );
    }
}

/// **Vertrauenswürdige EL0-Probe** (`.user_text`, läuft in der globalen SAS-Map).
/// x0 = Adresse X. Liest X (im SAS EL0-RW → erlaubt) und meldet Erfolg per `SIGNAL`
/// (Slot 0, Badge TRUSTED). Kontrast zur isolierten Probe: **dieselbe** Adresse.
#[link_section = ".user_text"]
extern "C" fn trusted_probe(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code (SAS); `ldr` auf X ist hier erlaubt.
    unsafe {
        core::arch::asm!(
            "mov x9, x0",
            "ldr x2, [x9]", // X lesen (im SAS erlaubt)
            "mov x0, #8",   // sys::SIGNAL Slot 0 (Badge TRUSTED)
            "mov x1, #0",
            "svc #0",
        "1:",
            "mov x0, #5",   // sys::PARK
            "svc #0",
            "b 1b",
            options(noreturn),
        );
    }
}

/// **VMM-Probe** (`.user_text`, isolierte VSpace): demonstriert `map`/`unmap` per
/// Syscall. x0 = Frame-Adresse G (identity). Slot 0 = Memory-Cap für G, Slot 1 =
/// SIGNAL-Cap. Ablauf: `MAP` Slot 0 → G ist EL0-RW → schreibt+liest G (Roundtrip) →
/// `SIGNAL` Badge MAPPED → `UNMAP` Slot 0 → liest G erneut → **Fault** (unmapped) →
/// Kernel beendet sie + baut die VSpace ab. Beweist: map macht G zugänglich, unmap
/// entzieht es wieder — alles cap-gated in der eigenen VSpace.
#[link_section = ".user_text"]
extern "C" fn vmm_probe(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; `svc`/`ldr`/`str` auf den selbst gemappten Frame.
    unsafe {
        core::arch::asm!(
            "mov x9, x0",                   // x9 = G (Frame-Adresse)
            "mov x0, #10",                  // sys::MAP
            "mov x1, #0",                   // Slot 0 (Memory-Cap für G)
            "svc #0",
            "movz x10, #0xBEEF",            // x10 = 0xDEADBEEF (Testmuster)
            "movk x10, #0xDEAD, lsl #16",
            "str x10, [x9]",                // in den gemappten Frame schreiben
            "ldr x11, [x9]",                // zurücklesen
            "cmp x11, x10",
            "b.ne 3f",                      // Roundtrip fehlgeschlagen -> kein SIGNAL
            "mov x0, #8",                   // sys::SIGNAL Slot 1 (Badge MAPPED)
            "mov x1, #1",
            "svc #0",
            "mov x0, #11",                  // sys::UNMAP Slot 0
            "mov x1, #0",
            "svc #0",
            "ldr x11, [x9]",                // nach UNMAP lesen -> FAULT -> beendet
        "3:",
            "mov x0, #5",                   // sys::PARK
            "svc #0",
            "b 3b",
            options(noreturn),
        );
    }
}

/// **Shared-Memory-Writer** (`.user_text`, isolierte VSpace). x0 = Adresse des
/// geteilten Frames F. Mappt F (Slot 0 = Memory-Cap), schreibt `SHM_SECRET` hinein
/// und signalisiert (Slot 1, shm-Notification), dass der Reader lesen darf. F ist in
/// **beide** isolierte VSpaces gemappt (identity, gleiche Adresse) -> Zero-Copy über
/// die Isolationsgrenze, ohne dass die VSpaces sonst etwas teilen.
#[link_section = ".user_text"]
extern "C" fn shm_writer(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; `str` nur auf den selbst gemappten Shared-Frame.
    unsafe {
        core::arch::asm!(
            "mov x9, x0",
            "mov x0, #10", // sys::MAP Slot 0 (Memory-Cap für F)
            "mov x1, #0",
            "svc #0",
            "movz x10, #0x9ABC", // x10 = SHM_SECRET
            "movk x10, #0x5678, lsl #16",
            "movk x10, #0x1234, lsl #32",
            "movk x10, #0x5A5A, lsl #48",
            "str x10, [x9]", // in den geteilten Frame schreiben
            "mov x0, #8",  // sys::SIGNAL Slot 1 (shm-Notification -> Reader wecken)
            "mov x1, #1",
            "svc #0",
        "1:",
            "mov x0, #5", // sys::PARK
            "svc #0",
            "b 1b",
            options(noreturn),
        );
    }
}

/// **Shared-Memory-Reader** (`.user_text`, **andere** isolierte VSpace). x0 = F.
/// Mappt F (Slot 0), wartet auf den Writer (Slot 1, `WAIT`), liest F und meldet bei
/// `SHM_SECRET` Erfolg (Slot 2, Badge SHARED an den Kollektor). Beweis: der Reader
/// hat den Wert des Writers über den geteilten Frame gelesen — die VSpaces sind
/// sonst vollständig getrennt, Kommunikation lief über IPC + cap-gewährten Frame.
#[link_section = ".user_text"]
extern "C" fn shm_reader(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; `ldr` nur auf den selbst gemappten Shared-Frame.
    unsafe {
        core::arch::asm!(
            "mov x9, x0",
            "mov x0, #10", // sys::MAP Slot 0 (Memory-Cap für F)
            "mov x1, #0",
            "svc #0",
            "mov x0, #9",  // sys::WAIT Slot 1 (blockiert bis Writer signalisiert)
            "mov x1, #1",
            "svc #0",
            "ldr x11, [x9]", // geteilten Frame lesen
            "movz x10, #0x9ABC",
            "movk x10, #0x5678, lsl #16",
            "movk x10, #0x1234, lsl #32",
            "movk x10, #0x5A5A, lsl #48",
            "cmp x11, x10",
            "b.ne 3f",
            "mov x0, #8",  // sys::SIGNAL Slot 2 (Badge SHARED an Kollektor)
            "mov x1, #2",
            "svc #0",
        "3:",
            "mov x0, #5", // sys::PARK
            "svc #0",
            "b 3b",
            options(noreturn),
        );
    }
}

/// Dummy-Entry für den Churn-Test: wird nie ausgeführt (die PD wird sofort nach dem
/// Spawn wieder zerstört). Nur eine Adresse in `.user_text`.
#[link_section = ".user_text"]
extern "C" fn churn_dummy(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code (unerreichbar).
    unsafe {
        core::arch::asm!("1:", "mov x0, #5", "svc #0", "b 1b", options(noreturn));
    }
}

/// **4-KiB-Seiten-Probe** (`.user_text`, isolierte VSpace). x0 = Basis P einer
/// 12-KiB-Region, vom Kernel feingranular gemappt: P=RW, P+4 KiB=RO (vorbefüllt),
/// P+8 KiB=**Guard** (ungemappt). Die Probe schreibt P (RW), liest P+4 KiB (RO) und
/// meldet Badge PAGES (Beleg: mehrere einzelne Seiten mit gemischten Rechten); dann
/// schreibt sie die Guard-Seite -> **Fault** (ungemappte Seite) -> beendet. Zeigt
/// feingranulare Rechte + Guard Pages.
#[link_section = ".user_text"]
extern "C" fn page_probe(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; Zugriffe nur auf die eigenen gemappten Seiten.
    unsafe {
        core::arch::asm!(
            "mov x9, x0",
            "movz x10, #0x1234",
            "str x10, [x9]",          // P (RW) schreiben
            "ldr x11, [x9, #4096]",   // P+4KiB (RO) lesen -> ok
            "mov x0, #8",             // sys::SIGNAL Slot 0 (Badge PAGES)
            "mov x1, #0",
            "svc #0",
            "str x10, [x9, #8192]",   // P+8KiB (Guard, ungemappt) schreiben -> FAULT
        "3:",
            "mov x0, #5",             // sys::PARK
            "svc #0",
            "b 3b",
            options(noreturn),
        );
    }
}

/// **Natives Programm-Template** (`.user_text`). Diese Bytes werden in einen privaten
/// Code-Frame **kopiert** und dort (EL0-RX, eigene VSpace) ausgeführt — NICHT die
/// geteilte `.user_text`. Es signalisiert (Slot 0, Badge NATIVE) und parkt. Reines
/// position-unabhängiges Inline-Asm (svc + Immediates + relativer Branch), daher an
/// einer beliebigen Adresse lauffähig.
#[link_section = ".user_text"]
extern "C" fn native_template(_arg: usize) -> ! {
    // SAFETY: reiner EL0-User-Code; nur `svc`.
    unsafe {
        core::arch::asm!(
            "mov x0, #8", // sys::SIGNAL
            "mov x1, #0", // Slot 0 (Badge NATIVE)
            "svc #0",
        "1:",
            "mov x0, #5", // sys::PARK
            "svc #0",
            "b 1b",
            options(noreturn),
        );
    }
}

/// Kollektor (EL1) für die Weg-C-Demo: sammelt die Badges der Proben.
extern "C" fn iso_collector(_arg: usize) -> ! {
    loop {
        let r = invoke(sys::WAIT, 0, [0; 4], 0);
        if r.result == result::OK {
            ISO_MASK.fetch_or(r.badge, Ordering::Relaxed);
        }
    }
}

/// Lastausgleich-Worker (EL1): wird lastbewusst auf einem Kern platziert und parkt
/// dort (belegt einen Slot dieses Kerns -> macht die Verteilung sichtbar).
extern "C" fn balanced_worker(_arg: usize) -> ! {
    loop {
        invoke(sys::PARK, 0, [0; 4], 0);
    }
}

/// Cross-Core-Park/Wake-Thread (EL1, auf core 1): blockiert sich per `PARK`; core 0
/// weckt ihn kern-übergreifend per `wake_remote` (Reschedule-IPI). Setzt nach dem
/// Aufwachen `XCORE_WOKEN` — Beleg für den IPI-getriebenen Cross-Core-Unblock.
extern "C" fn xcore_parker(_arg: usize) -> ! {
    XCORE_PARKED.store(true, Ordering::Release);
    invoke(sys::PARK, 0, [0; 4], 0); // blockiert; von core 0 via IPI geweckt
    XCORE_WOKEN.store(true, Ordering::Release);
    loop {
        invoke(sys::PARK, 0, [0; 4], 0);
    }
}

/// Kern-übergreifender IPC-Server (EL1, auf core 2): empfängt per `RECV` (Slot 0)
/// und antwortet mit `XIPC_FACTOR * msg[0]`. Aufrufer liegt auf core 0 -> Nachricht
/// und Antwort queren die Kerngrenze (Cross-Core-Rendezvous + IPI-Wake).
extern "C" fn xipc_server(_arg: usize) -> ! {
    loop {
        let m = invoke(sys::RECV, 0, [0; 4], 0);
        if m.result != result::OK {
            break;
        }
        invoke(sys::REPLY, 0, [XIPC_FACTOR * m.msg[0], 0, 0, 0], 0);
    }
    loop {
        invoke(sys::PARK, 0, [0; 4], 0);
    }
}

/// Kern-übergreifender IPC-Client (EL1, auf core 0): ruft den Server auf core 2
/// per `CALL` (Slot 0) und prüft die Antworten.
extern "C" fn xipc_client(_arg: usize) -> ! {
    for (i, &v) in XIPC_IN.iter().enumerate() {
        let r = invoke(sys::CALL, 0, [v, 0, 0, 0], 0);
        XIPC_RESULT[i].store(r.msg[0], Ordering::Relaxed);
    }
    XIPC_DONE.store(true, Ordering::Release);
    loop {
        invoke(sys::PARK, 0, [0; 4], 0);
    }
}

/// Zähler-Service v1: liest den Zustand aus der Region, addiert 1, schreibt zurück.
extern "C" fn counter_v1(arg: usize) -> ! {
    counter_serve(arg as u64, 1)
}

/// Zähler-Service v2 (neu geladen): addiert 10 — setzt den Zustand von v1 fort.
extern "C" fn counter_v2(arg: usize) -> ! {
    counter_serve(arg as u64, 10)
}

fn counter_serve(state: u64, delta: u64) -> ! {
    loop {
        let m = invoke(sys::RECV, 0, [0; 4], 0);
        if m.result != result::OK {
            break; // Cap entzogen -> zurückgezogen
        }
        let c = peek_u64(state) + delta; // Zustand in der Memory-Region
        poke_u64(state, c);
        invoke(sys::REPLY, 0, [c, 0, 0, 0], 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Zähler-Client: 3 Aufrufe (von v1 bedient), Reload abwarten, 2 weitere (v2).
extern "C" fn cs_client(_arg: usize) -> ! {
    for r in CS_R1.iter() {
        r.store(invoke(sys::CALL, 0, [0; 4], 0).msg[0], Ordering::Relaxed);
    }
    CS_BATCH1_DONE.store(true, Ordering::Release);
    while !CS_RELOADED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    for r in CS_R2.iter() {
        r.store(invoke(sys::CALL, 0, [0; 4], 0).msg[0], Ordering::Relaxed);
    }
    CS_DONE.store(true, Ordering::Release);
    invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// EL1-Server für den EL0-User-Thread: empfängt dessen `CALL` und merkt sich den
/// Wert (Beleg, dass der EL0-Syscall durchkam), antwortet leer.
extern "C" fn user_server(_arg: usize) -> ! {
    loop {
        let m = invoke(sys::RECV, 0, [0; 4], 0);
        if m.result != result::OK {
            break;
        }
        USER_RECV.store(m.msg[0], Ordering::Relaxed);
        invoke(sys::REPLY, 0, [0; 4], 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// **EL0-User-Programm** (eigene Sektion `.user_text`, EL0-ausführbar). Es darf
/// NUR Syscalls ausführen (kein Kernelzugriff): ein `CALL` mit `USER_MAGIC` über
/// den Endpoint an lokalem Cap-Slot 0, danach Selbst-Park. Reines Inline-`svc`,
/// damit kein EL1-Kernelcode aufgerufen wird.
#[link_section = ".user_text"]
extern "C" fn user_entry(_arg: usize) -> ! {
    // SAFETY: EL0-User-Code; `svc` ist die einzige erlaubte Kernel-Interaktion.
    unsafe {
        // CALL(slot 0, msg0 = USER_MAGIC)
        core::arch::asm!(
            "svc #0",
            inout("x0") sys::CALL => _,
            inout("x1") 0u64 => _,
            inout("x2") USER_MAGIC => _,
            lateout("x3") _, lateout("x4") _, lateout("x5") _, lateout("x6") _,
            options(nostack),
        );
        // Danach dauerhaft parken (kein weiterer Kernelzugriff nötig).
        core::arch::asm!(
            "svc #0",
            in("x0") sys::PARK,
            options(noreturn, nostack),
        );
    }
}

/// **Bösartiges EL0-User-Programm** (Sektion `.user_text`, EL0-ausführbar). Es
/// versucht, EL1-only Kernel-Speicher (die Kernel-`.text` an `0x4008_0000`) zu
/// lesen. Da diese Seite nur EL1-Zugriff erlaubt, MUSS das aus EL0 einen Data
/// Abort auslösen. Der Kernel fängt ihn ab, beendet **nur diesen Thread** und
/// läuft weiter — der Nachweis, dass User-Code den Kernel nicht kompromittieren
/// kann. Reines Inline-Asm, kein EL1-Kernelcode.
#[link_section = ".user_text"]
extern "C" fn bad_user(_arg: usize) -> ! {
    // SAFETY: EL0-User-Code. Der Load auf eine EL1-only Adresse faultet
    // garantiert und kehrt nie zurück (Kernel beendet den Thread im Fault-Hook).
    unsafe {
        let kernel_addr: usize = 0x4008_0000; // Kernel-.text (EL1-only gemappt)
        let mut v: u64;
        core::arch::asm!(
            "ldr {v}, [{a}]",
            a = in(reg) kernel_addr,
            v = out(reg) v,
            options(nostack),
        );
        // Wird nie erreicht. Den geladenen Wert "verbrauchen", damit der Compiler
        // den Load nicht wegoptimiert.
        core::arch::asm!("svc #0", in("x0") sys::PARK, in("x2") v, options(nostack));
    }
    loop {
        // SAFETY: Fallback (unerreichbar) — parken statt EL1-Code zu berühren.
        unsafe {
            core::arch::asm!("svc #0", in("x0") sys::PARK, options(nostack));
        }
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
    let mut cs_reloaded = false;
    let mut reported = false;
    let mut dbg_ticks = 0u32;
    let mut dbg_printed = false;
    loop {
        // Diagnose: falls der Bericht ausbleibt, nach ~25 s die ausstehenden
        // Bedingungen einmalig ausgeben (zeigt, welcher Test haengt).
        dbg_ticks += 1;
        if !reported && !dbg_printed && dbg_ticks > 250 {
            dbg_printed = true;
            let m = ISO_MASK.load(Ordering::Relaxed);
            println!("DBG pending: workers={} fp={} prio={} life={} notif={} xfer={} ckpt={} el0={} smp={} xipc={} reclaim={} balance={} vspace={} vmm={} shm={} native={} pages4k={} churn={}",
                (0..NWORKERS).all(|i| WORKER_COUNTS[i].load(Ordering::Relaxed) >= THRESHOLD),
                FP_COLLECTOR_DONE.load(Ordering::Acquire) && system::fp_switch_count() > 0,
                (0..NPRIO_TEST).all(|i| PRIO_DONE[i].load(Ordering::Acquire)),
                KILLER_DONE.load(Ordering::Acquire) && REAPED.load(Ordering::Relaxed) >= 2,
                PRODUCER_DONE.load(Ordering::Acquire) && NOTIF_COUNT.load(Ordering::Relaxed) >= NOTIF_ROUNDS,
                XFER_DONE.load(Ordering::Acquire),
                CS_DONE.load(Ordering::Acquire),
                USER_RECV.load(Ordering::Relaxed) == USER_MAGIC && system::el0_syscall_seen(),
                (1..NUM_CORES).all(|c| XCORE_PROGRESS[c].load(Ordering::Relaxed) >= SMP_WORK_TARGET) && XCORE_WOKEN.load(Ordering::Acquire),
                XIPC_DONE.load(Ordering::Acquire),
                RECLAIM_SPAWNED.load(Ordering::Relaxed) >= RECLAIM_TARGET,
                BALANCED_SPAWNED.load(Ordering::Relaxed) >= BALANCED_TARGET,
                (m & ISO_BADGE_RAN != 0) && (m & ISO_BADGE_TRUSTED != 0) && system::iso_faulted(),
                (m & ISO_BADGE_MAPPED != 0) && system::iso_fault_count() >= 2,
                m & ISO_BADGE_SHARED != 0,
                m & ISO_BADGE_NATIVE != 0,
                m & ISO_BADGE_PAGES != 0,
                CHURN_DONE.load(Ordering::Acquire) && CHURN_OK.load(Ordering::Acquire),
            );
        }
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
        if !cs_reloaded && CS_BATCH1_DONE.load(Ordering::Acquire) {
            do_cs_reload();
            CS_RELOADED.store(true, Ordering::Release);
            cs_reloaded = true;
        }
        // Cross-Core-Wake: sobald der Parker (auf core 1) blockiert ist, ihn per
        // IPI wecken. Idempotent + Wiederholung pro Tick -> race-frei (trifft ein
        // Wake den noch nicht blockierten Parker, weckt der nächste ihn).
        if XCORE_PARKED.load(Ordering::Acquire) && !XCORE_WOKEN.load(Ordering::Acquire) {
            let raw = XCORE_PARKER_TID.load(Ordering::Relaxed);
            if raw != u64::MAX {
                system::wake_remote(ThreadId::from_raw(raw));
            }
        }

        // Reclaim-Test: nach dem FP-Test transiente EL0-Exiter spawnen — insgesamt
        // mehr (RECLAIM_TARGET) als der Kernel-Stack-Pool gleichzeitig fasst. Nur ein
        // freier Slot wird belegt; jeder Exiter beendet sich (Prio 6 -> sofort) und
        // gibt seinen Slot zurück. Gelingen alle TARGET Spawns, funktioniert Reclaim.
        if FP_COLLECTOR_DONE.load(Ordering::Acquire)
            && RECLAIM_SPAWNED.load(Ordering::Relaxed) < RECLAIM_TARGET
        {
            // Pro Tick alle gerade freien Slots belegen (Batch); die Exiter (Prio 6)
            // beenden sich beim nächsten wfi-Tick und geben ihre Slots zurück.
            // Atomar gegen Preemption (Locks im Thread-Kontext), wie beim Reload.
            hal::cpu::local_irq_disable();
            while RECLAIM_SPAWNED.load(Ordering::Relaxed) < RECLAIM_TARGET
                && system::user_kstack_free_count() > 0
            {
                if system::spawn_user(exiter_entry as *const () as usize, 0, RECLAIM_PRIO).is_some() {
                    RECLAIM_SPAWNED.fetch_add(1, Ordering::Relaxed);
                } else {
                    break;
                }
            }
            hal::cpu::local_irq_enable();
        }

        // Lastausgleich: nach dem Reclaim-Test BALANCED_TARGET Worker lastbewusst
        // platzieren. Jeder landet auf dem gerade am wenigsten belasteten Kern (die
        // Last steigt mit jedem Spawn) -> Verteilung über die Kerne statt alle auf
        // dem Bootkern. Wir merken uns den gewählten Kern je Worker.
        if RECLAIM_SPAWNED.load(Ordering::Relaxed) >= RECLAIM_TARGET
            && BALANCED_SPAWNED.load(Ordering::Relaxed) < BALANCED_TARGET
        {
            hal::cpu::local_irq_disable();
            while BALANCED_SPAWNED.load(Ordering::Relaxed) < BALANCED_TARGET {
                match system::spawn_balanced(balanced_worker as *const () as usize, 0, system::IDLE_PRIO) {
                    Some(t) => {
                        BALANCED_PLACE[t.core()].fetch_add(1, Ordering::Relaxed);
                        BALANCED_SPAWNED.fetch_add(1, Ordering::Relaxed);
                    }
                    None => break,
                }
            }
            hal::cpu::local_irq_enable();
        }
        // Churn-/Leak-Test: sobald die isolierten Demos durch sind (page-Badge +
        // die 3 faultenden isolierten PDs abgebaut), tausende spawn/destroy-Zyklen
        // isolierter PDs fahren und pruefen, dass alle Ressourcenstaende exakt zur
        // Baseline zurueckkehren. IRQs aus -> kein anderer Thread perturbiert die
        // Zaehlung (Messung sauber); der Test ist kurz.
        if !CHURN_DONE.load(Ordering::Acquire)
            && (ISO_MASK.load(Ordering::Relaxed) & ISO_BADGE_PAGES != 0)
            && system::iso_fault_count() >= 3
        {
            hal::cpu::local_irq_disable();
            // Vor dem Snapshot alle noch ausstehenden Zombies einsammeln (z. B. von
            // den faultenden isolierten Proben), sonst gäbe die erste reap()-Aufrufung
            // im Churn deren Stacks frei -> Schein-Leak.
            while system::reap() > 0 {}
            let mem0 = system::total_free();
            let tcb0 = system::used_tcbs(0);
            let vs0 = system::free_vspaces();
            let ks0 = system::user_kstack_free_count();
            let mut ok = true;
            for _ in 0..CHURN_TARGET {
                match system::spawn_isolated(churn_dummy as *const () as usize, 0, system::IDLE_PRIO) {
                    Some((tid, _)) => system::destroy_isolated(tid),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            let leak = system::total_free() != mem0
                || system::used_tcbs(0) != tcb0
                || system::free_vspaces() != vs0
                || system::user_kstack_free_count() != ks0;
            hal::cpu::local_irq_enable();
            CHURN_OK.store(ok && !leak, Ordering::Release);
            CHURN_DONE.store(true, Ordering::Release);
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
    let fp = FP_COLLECTOR_DONE.load(Ordering::Acquire) && system::fp_switch_count() > 0;
    let prio = (0..NPRIO_TEST).all(|i| PRIO_DONE[i].load(Ordering::Acquire));
    let life = KILLER_DONE.load(Ordering::Acquire) && REAPED.load(Ordering::Relaxed) >= 2;
    let notif = PRODUCER_DONE.load(Ordering::Acquire) && NOTIF_COUNT.load(Ordering::Relaxed) >= NOTIF_ROUNDS;
    let xfer = XFER_DONE.load(Ordering::Acquire);
    let ckpt = CS_DONE.load(Ordering::Acquire);
    let el0 = USER_RECV.load(Ordering::Relaxed) == USER_MAGIC && system::el0_syscall_seen();
    let el0iso = system::el0_fault_count() >= 1;
    // SMP: alle Sekundärkerne haben ihren Worker abgearbeitet (parallele Einplanung)
    // und der Parker wurde kern-übergreifend per IPI geweckt.
    let smp = (1..NUM_CORES).all(|c| XCORE_PROGRESS[c].load(Ordering::Relaxed) >= SMP_WORK_TARGET)
        && XCORE_WOKEN.load(Ordering::Acquire);
    let xipc = XIPC_DONE.load(Ordering::Acquire);
    // Reclaim: alle transienten EL0-Exiter wurden erzeugt (Pool-Slots wiederverwendet).
    let reclaim = RECLAIM_SPAWNED.load(Ordering::Relaxed) >= RECLAIM_TARGET;
    // Lastausgleich: alle lastbewusst platzierten Worker erzeugt.
    let balanced = BALANCED_SPAWNED.load(Ordering::Relaxed) >= BALANCED_TARGET;
    // Weg C: SAS-Probe las X (TRUSTED-Badge), isolierte Probe lief+IPC (RAN-Badge)
    // und faultete dann beim Fremdzugriff (iso_faulted).
    let m = ISO_MASK.load(Ordering::Relaxed);
    let vspace = (m & ISO_BADGE_RAN != 0) && (m & ISO_BADGE_TRUSTED != 0) && system::iso_faulted();
    // VMM: die VMM-Probe mappte+beschrieb einen Frame (MAPPED) und faultete nach dem
    // UNMAP -> beide isolierten Proben (iso + vmm) faulteten => iso_fault_count >= 2.
    let vmm = (m & ISO_BADGE_MAPPED != 0) && system::iso_fault_count() >= 2;
    // Shared-Memory-IPC: der Reader las den Wert des Writers via geteiltem Frame.
    let shm = m & ISO_BADGE_SHARED != 0;
    // Natives Code-Laden: privat geladener Code lief in eigener VSpace.
    let native = m & ISO_BADGE_NATIVE != 0;
    // 4-KiB-Seiten: RW-Schreiben + RO-Lesen einzelner Seiten erfolgreich.
    let pages4k = m & ISO_BADGE_PAGES != 0;
    // Churn/Leak: tausende spawn/destroy-Zyklen ohne Ressourcen-Leck.
    let churn = CHURN_DONE.load(Ordering::Acquire) && CHURN_OK.load(Ordering::Acquire);
    workers && cores && fp && prio && life && notif && xfer && ckpt && el0 && el0iso && smp && xipc
        && reclaim && balanced && vspace && vmm && shm && native && pages4k && churn
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

    // Lazy-FP: EL0-FP-Threads behielten ihr Muster über jede FP-Owner-Abgabe.
    let fp_mask = FP_OK_MASK.load(Ordering::Relaxed);
    let fp_sw = system::fp_switch_count();
    println!("fp      : EL0-FP-Threads-OK={fp_mask:#04b}/{FP_ALL_OK:#04b}, Lazy-FP-Owner-Wechsel={fp_sw}");
    let fp_ok = fp_mask == FP_ALL_OK && fp_sw > 0;
    println!("fp      : {} (Lazy-FP: FP-Kontext ueber Owner-Wechsel erhalten)", if fp_ok { "ALL PASS" } else { "FAILURES" });

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
    // Zurückgewonnener Stack = monotone Summe der vom Reaper freigegebenen Bytes
    // (robust gegen die Speicher-Churn von Reclaim-/Balance-Test, anders als ein
    // absoluter total_free-Vergleich).
    let freed = system::reaped_bytes();
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

    // EL0-Userland: echter EL0-Thread hat per Syscall einen EL1-Server gerufen.
    let urecv = USER_RECV.load(Ordering::Relaxed);
    let el0_seen = system::el0_syscall_seen();
    println!("el0     : EL1-Server empfing {urecv:#x} (erwartet {USER_MAGIC:#x}); EL0-Syscall gesehen: {el0_seen}");
    let el0_ok = urecv == USER_MAGIC && el0_seen;
    println!("el0     : {} (User-Thread laeuft auf EL0, nur via Syscall)", if el0_ok { "ALL PASS" } else { "FAILURES" });

    // EL0-Isolation: bösartiger EL0-Thread las Kernel-Speicher -> isoliert, Kernel lebt.
    let faults = system::el0_fault_count();
    println!("el0iso  : EL0-Faults abgefangen={faults} (Kernel laeuft -> dieser Bericht beweist es)");
    let iso_ok = faults >= 1;
    println!("el0iso  : {} (EL0-Zugriff auf Kernel-Speicher faultet, Thread beendet, Kernel ueberlebt)", if iso_ok { "ALL PASS" } else { "FAILURES" });

    // Stateful Hot-Reload: Zustand (Zähler) bleibt über v1->v2 erhalten.
    let r1 = [
        CS_R1[0].load(Ordering::Relaxed),
        CS_R1[1].load(Ordering::Relaxed),
        CS_R1[2].load(Ordering::Relaxed),
    ];
    let r2 = [CS_R2[0].load(Ordering::Relaxed), CS_R2[1].load(Ordering::Relaxed)];
    println!("ckpt    : v1(+1)={r1:?} -> Reload -> v2(+10)={r2:?}");
    let ckpt_ok = r1 == [1, 2, 3] && r2 == [13, 23];
    println!(
        "ckpt    : {} (Zustand ueber Komponententausch erhalten)",
        if ckpt_ok { "ALL PASS" } else { "FAILURES" }
    );

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

    // Per-Kern-paralleler Scheduler: jeder Sekundärkern hat seinen Worker parallel
    // abgearbeitet; der Parker wurde kern-übergreifend per IPI geweckt.
    let mut smp_ok = true;
    for c in 1..NUM_CORES {
        let p = XCORE_PROGRESS[c].load(Ordering::Relaxed);
        println!("smp     : core {c} Worker-Fortschritt={p}/{SMP_WORK_TARGET}");
        if p < SMP_WORK_TARGET {
            smp_ok = false;
        }
    }
    let woken = XCORE_WOKEN.load(Ordering::Acquire);
    println!("smp     : Cross-Core-IPI-Wake (core 0 -> core 1 Parker) erfolgreich={woken}");
    if !woken {
        smp_ok = false;
    }
    println!(
        "smp     : {} (per-Kern-parallele Einplanung + IPI-Cross-Core-Wake)",
        if smp_ok { "ALL PASS" } else { "FAILURES" }
    );

    // Kern-übergreifende synchrone IPC: Client (core 0) <-> Server (core 2).
    let mut xipc_ok = true;
    for (i, &v) in XIPC_IN.iter().enumerate() {
        let r = XIPC_RESULT[i].load(Ordering::Relaxed);
        println!("xipc    : core0->core{XIPC_SERVER_CORE} call({v}) -> {r} (erwartet {})", XIPC_FACTOR * v);
        if r != XIPC_FACTOR * v {
            xipc_ok = false;
        }
    }
    println!(
        "xipc    : {} (synchrone IPC ueber Kerngrenze, CALL+REPLY je per IPI)",
        if xipc_ok { "ALL PASS" } else { "FAILURES" }
    );

    // Reclaim: transiente EL0-Threads (mehr als der 8er-Pool) liefen dank
    // Rückgabe der Kernel-Stack-Pool-Slots beim Thread-Ende.
    let spawned = RECLAIM_SPAWNED.load(Ordering::Relaxed);
    let free = system::user_kstack_free_count();
    println!("reclaim : {spawned} transiente EL0-Threads erzeugt (Pool=8), jetzt {free} Slots frei");
    let reclaim_ok = spawned >= RECLAIM_TARGET && free >= 4;
    println!(
        "reclaim : {} (EL0-Kernel-Stack-Pool-Slots werden beim Thread-Ende zurueckgegeben)",
        if reclaim_ok { "ALL PASS" } else { "FAILURES" }
    );

    // Lastausgleich: lastbewusst platzierte Worker verteilten sich über die Kerne.
    let mut place = [0u64; NUM_CORES];
    let mut distinct = 0usize;
    let mut maxp = 0u64;
    for (c, p) in place.iter_mut().enumerate() {
        *p = BALANCED_PLACE[c].load(Ordering::Relaxed);
        if *p > 0 {
            distinct += 1;
        }
        if *p > maxp {
            maxp = *p;
        }
    }
    let bspawned = BALANCED_SPAWNED.load(Ordering::Relaxed);
    println!("balance : {bspawned} Worker verteilt {place:?} (Kerne genutzt={distinct}, max/Kern={maxp})");
    // Balanciert: alle platziert, über mehrere Kerne gestreut, kein Kern überladen
    // (ohne Lastausgleich landeten alle auf dem Bootkern -> distinct=1, max=TARGET).
    let balance_ok = bspawned >= BALANCED_TARGET && distinct >= 5 && maxp <= 4;
    println!(
        "balance : {} (lastbewusste Platzierung verteilt Threads ueber die Kerne)",
        if balance_ok { "ALL PASS" } else { "FAILURES" }
    );

    // Weg C (Hybrid): isolierte PD vs. SAS-PD beim Zugriff auf dieselbe Adresse X.
    let m = ISO_MASK.load(Ordering::Relaxed);
    let x = ISO_SECRET_ADDR.load(Ordering::Relaxed);
    let iso_faulted = system::iso_faulted();
    let ran = m & ISO_BADGE_RAN != 0;
    let read = m & ISO_BADGE_READ != 0;
    let trusted = m & ISO_BADGE_TRUSTED != 0;
    println!("vspace  : X={x:#x}; SAS-Probe las X={trusted}; isol. Probe lief+IPC={ran}, faultete={iso_faulted}, las-X={read}");
    // Bestanden: SAS-PD darf X lesen; isolierte PD lief + meldete sich per IPC,
    // wurde beim Fremdzugriff hardware-seitig gefaultet und las X NIE.
    let vspace_ok = trusted && ran && iso_faulted && !read;
    println!(
        "vspace  : {} (per-Prozess-VSpace: echte User<->User-Trennung, nur IPC)",
        if vspace_ok { "ALL PASS" } else { "FAILURES" }
    );

    // Allgemeiner VMM: map/unmap per Syscall + VSpace-Teardown.
    let mapped = m & ISO_BADGE_MAPPED != 0;
    let faults = system::iso_fault_count();
    println!("vmm     : VMM-Probe mappte+beschrieb Frame={mapped}; isolierte Faults gesamt={faults} (>=2 => unmap wirkte)");
    let vmm_ok = mapped && faults >= 2;
    println!(
        "vmm     : {} (Frame-Caps + map/unmap-Syscalls + VSpace-Teardown)",
        if vmm_ok { "ALL PASS" } else { "FAILURES" }
    );

    // Shared-Memory-IPC: Reader las den Wert des Writers ueber den geteilten Frame.
    let shared = m & ISO_BADGE_SHARED != 0;
    println!("shm     : Reader las Writer-Wert via geteiltem Frame (zwei isol. VSpaces)={shared}");
    println!(
        "shm     : {} (Shared-Memory-IPC: ein Frame in zwei isolierte VSpaces, cap-gewaehrt)",
        if shared { "ALL PASS" } else { "FAILURES" }
    );

    // Natives Code-Laden: privat geladener Code lief in eigener EL0-RX-Region.
    let native = m & ISO_BADGE_NATIVE != 0;
    println!("native  : privat geladener Code lief in eigener VSpace (nicht geteilte .user_text)={native}");
    println!(
        "native  : {} (natives Code-Laden je isolierter VSpace, EL0-RX/W^X + I-Cache-Sync)",
        if native { "ALL PASS" } else { "FAILURES" }
    );

    // 4-KiB-Seiten: mehrere einzelne Seiten mit gemischten Rechten + Guard-Page.
    let pages4k = m & ISO_BADGE_PAGES != 0;
    println!("pages4k : einzelne 4-KiB-Seiten RW-Schreiben+RO-Lesen ok={pages4k} (Guard-Seite faultet danach)");
    println!(
        "pages4k : {} (4-KiB-Mappings: gemischte RW/RO-Rechte + Guard Pages + L3)",
        if pages4k { "ALL PASS" } else { "FAILURES" }
    );

    // Churn/Leak: tausende spawn/destroy-Zyklen, Ressourcen kehren zur Baseline.
    let churn = CHURN_DONE.load(Ordering::Acquire) && CHURN_OK.load(Ordering::Acquire);
    println!("churn   : {CHURN_TARGET} spawn/destroy-Zyklen isolierter PDs; MEM/TCB/ASID/kstack zurueck=Baseline: {churn}");
    println!(
        "churn   : {} (keine Mapping-/ASID-/TCB-/Speicher-Leaks ueber tausende Zyklen)",
        if churn { "ALL PASS" } else { "FAILURES" }
    );
}
