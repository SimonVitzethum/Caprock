//! Kernzustand hinter **per-Kern-Scheduler-Locks** (`SCHEDS[core]`, je Kern eine Instanz mit
//! eigener TCB-Partition) und den **getrennten** Ressourcen-Locks (`CAPS` = Capability-Space + PDs;
//! `MEM` = physischer Allokator; `EPS[]`/`NTFNS[]` = Endpoints/Notifications; `VSPACES`; `DMA_CTX`)
//! — plus die Trap-Hooks.
//!
//! **Echte per-Kern-parallele Einplanung:** Der heiße Timer-/IPI-Reschedule-Pfad sperrt nur
//! `SCHEDS[core]` des eigenen Kerns — Kerne planen gleichzeitig ein, ohne sich gegenseitig zu
//! blockieren. Kern-übergreifendes Aufwecken ([`wake_remote`]) sperrt die **Ziel**instanz und
//! schickt einen Reschedule-IPI.
//!
//! **Lock-Ordnung (Rang-Hierarchie, totale Ordnung):** `CAPS` (R0) → `EPS`/`NTFNS`/`VSPACES`/
//! `DMA_CTX` (R1) → `SCHEDS[*]` (R2) → `Heap.inner` (R3) → `MEM` (R4, innerster). Nur aufsteigend
//! schachteln; `MEM` hält nie einen weiteren Lock. Der reine Reschedule-Pfad nimmt nur
//! `SCHEDS[core]` (+ atomares `FP_OWNER`) und kann an keinem Deadlock-Zyklus teilnehmen; kein Pfad
//! nimmt zwei verschiedene `SCHEDS[*]` gleichzeitig. Vollständige Herleitung + alle belegten
//! Schachtelungen: `docs/invariants.md` §1.

use sel4lake_cap::{CapError, CapInfo, CapPtr, DmaCoherence, DmaDir, ObjectKind};
use sel4lake_hal::{self as hal, exception::TrapFrame, fp::FpState, println};
use sel4lake_ipc::{Endpoint, Notification, NENDPOINTS, NNOTIFICATIONS};
use sel4lake_mem::{MemoryCap, PhysAllocator, PhysRegion, Rights};
use sel4lake_microkit::{Caps, Domain};
use sel4lake_region::heap::RegionSource;
use sel4lake_region::{Purpose, Region, RegionTag};
use sel4lake_loader::elf::{ElfImage, PF_W, PF_X};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use sel4lake_sched::{SchedOps, Scheduler, ThreadId, MAX_CORES};
use sel4lake_slab::{AtomicTable, Slab};
use sel4lake_sync::{RwSpinLock, SpinLock};

/// **Tatsächliche** Kernzahl (beim Boot gesetzt, s. [`configure`]). Alle per-Kern-Schleifen
/// laufen hierüber; die Compile-Zeit-Konstante [`MAX_CORES`] dimensioniert nur die Arrays
/// von *Locks*/Atomics, nicht die Datentabellen.
static NUM_CORES: AtomicUsize = AtomicUsize::new(1);

/// Stack-Größe eines Kernel-Threads (für Ressourcen-Buchhaltung in Tests).
pub fn stack_bytes() -> u64 {
    STACK_SIZE
}

/// Noch freie **Thread-Slots** im globalen Thread-Directory (Kapazitäts-Telemetrie).
pub fn threads_available() -> usize {
    sel4lake_sched::threads_available()
}

/// Gesamtzahl der Thread-Slots (Boot-Kapazität).
pub fn thread_capacity() -> usize {
    sel4lake_sched::thread_capacity()
}

/// Anzahl aktiver Kerne (Laufzeitwert).
pub fn num_cores() -> usize {
    NUM_CORES.load(Ordering::Relaxed)
}

const STACK_SIZE: u64 = 64 * 1024;
/// Standardpriorität für Idle und gewöhnliche Demo-Threads (Round-Robin).
pub const IDLE_PRIO: u8 = 1;

// EL0-User-Threads: Kernel-Stacks aus einem EL1-only Pool (Linker), User-Stacks
// aus dem EL0-zugänglichen RAM. Der Kernel-Stack MUSS EL1-only sein (sonst könnte
// der EL0-Thread seinen eigenen Kernel-Stack lesen/schreiben), daher der feste Pool
// im Kernel-Image (nicht aus dem EL0-zugänglichen MEM-Allokator).
const USER_KSTACK_SIZE: usize = 0x4000; // 16 KiB EL1-Kernel-Stack je EL0-Thread

/// Besitzer-Abbildung der **dynamisch aus `MEM` allozierten** EL0-Kernel-Stacks: `base_of[slot]`
/// = physische Basis des Kstacks des Threads `slot` (0 = keiner). Kstacks kommen NICHT mehr aus
/// einer festen Image-Region — die lag im 2-MiB-Kernel-Image und deckelte die gleichzeitige
/// EL0-Thread-Zahl hart. Jetzt aus dem allgemeinen RAM: Limit ist nur RAM + Thread-Zahl
/// (Thread-Kapazität), passend als Basis eines vollen OS. Beim Thread-Ende an `MEM` zurück (kein Leck).
/// EL1-only-Zugriff: der Kstack liegt im User-RAM-Fenster; in JEDER isolierten VSpace ist dieses
/// EL1-only (kernel_block-Spiegel) -> untrusted EL0 kommt nicht heran. Nur die globale SAS-Map
/// (TrustedSAS) sieht es EL0-RW — konsistent mit dem SAS-Vertrauensmodell.
struct KstackPool {
    /// Physische Basis des Kstacks je globalem Thread-Slot (`0` = keiner). Zur Boot-Zeit
    /// dimensioniert (Thread-Kapazität), s. [`configure`].
    base_of: Slab<u64>,
    live: usize,
}
/// Pool-Lock (nur `base_of`/`live`); NIE verschachtelt mit `MEM` gehalten (claim/reclaim nehmen die
/// Locks sequenziell) -> keine Sperrordnungsverletzung; `MEM` ist ohnehin innerster Rang.
static KSTACKS: SpinLock<KstackPool> = SpinLock::new(KstackPool {
    base_of: Slab::empty(),
    live: 0,
});

/// Einen EL0-Kernel-Stack (16 KiB, ausgerichtet) aus `MEM` allozieren. Gibt die **physische Basis**
/// zurück (identity-gemappt = EL1-SP-Region), oder `None` bei RAM-Erschöpfung.
fn claim_user_kstack() -> Option<usize> {
    let base = MEM.lock().alloc(USER_KSTACK_SIZE as u64, USER_KSTACK_SIZE as u64)?.base();
    KSTACKS.lock().live += 1;
    Some(base as usize)
}
/// Einen (noch keinem Thread zugeordneten) Kstack wieder an `MEM` freigeben (Fehlerpfad vor `record`).
fn release_user_kstack(base: usize) {
    MEM.lock().free_region(PhysRegion::new(base as u64, USER_KSTACK_SIZE as u64));
    let mut p = KSTACKS.lock();
    p.live = p.live.saturating_sub(1);
}
/// Den Kstack `base` dem Thread-Slot zuordnen (nach erfolgreichem spawn) -> Reclaim beim Thread-Ende.
fn record_user_kstack(thread_slot: usize, base: usize) {
    KSTACKS.lock().base_of[thread_slot] = base as u64;
}
/// Beim Thread-Ende: den ggf. zugeordneten Kstack an `MEM` zurückgeben. No-Op für EL1-Threads.
fn reclaim_user_kstack(thread_slot: usize) {
    let base = {
        let mut p = KSTACKS.lock();
        let b = p.base_of[thread_slot];
        if b != 0 {
            p.base_of[thread_slot] = 0;
            p.live = p.live.saturating_sub(1);
        }
        b
    };
    if base != 0 {
        MEM.lock().free_region(PhysRegion::new(base, USER_KSTACK_SIZE as u64));
    }
}
/// Virtuelle „freie Kstack-Kapazität" (jetzt RAM-begrenzt): `MAX_THREADS - live`. Für den
/// Reclaim-Test (spawnt, solange > 0) + Diagnose.
pub fn user_kstack_free_count() -> usize {
    sel4lake_sched::thread_capacity().saturating_sub(KSTACKS.lock().live)
}
/// Sticky: wurde jemals ein Syscall von EL0 (User-Thread) gesehen?
static EL0_SYSCALL_SEEN: AtomicBool = AtomicBool::new(false);
/// Zähler: wie oft hat der Kernel einen EL0-Fault abgefangen und den fehlerhaften
/// User-Thread isoliert (statt selbst anzuhalten)?
static EL0_FAULTS: AtomicUsize = AtomicUsize::new(0);
/// Zähler: wie oft faultete ein Thread, der in einer **isolierten VSpace** lief?
/// (Beleg, dass die Hardware-Adressraumtrennung Fremd-/unmapped-Zugriffe verhindert.)
static ISO_FAULTS: AtomicUsize = AtomicUsize::new(0);

// --- Lazy-FP-Zustand ---
//
// Pro Kern besitzt höchstens ein Thread die FP/SIMD-Register (der „FP-Owner").
// FP wird beim Kontextwechsel NICHT gesichert; erst ein FP-Trap (EC 0x07) eines
// Nicht-Owners aus EL0 löst Save (alter Owner) + Restore (neuer) aus. EL1 ist
// soft-float und berührt FP nie. Zähler `FP_SWITCHES` belegt, dass tatsächlich
// gewechselt wurde.
const FP_OWNER_NONE: u64 = u64::MAX;
/// Raw-`ThreadId` des FP-Owners je Kern (nur der jeweilige Kern schreibt seinen
/// Eintrag -> Atomics genügen, keine Sperre nötig).
#[allow(clippy::declare_interior_mutable_const)]
static FP_OWNER: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(FP_OWNER_NONE) }; MAX_CORES];
/// Per-Thread-Slot FP-Kontextpuffer. Zugriff ausschließlich unter dem SCHED-Lock
/// (fp_trap hält SCHED; spawn ebenfalls) -> für gleiche Slots serialisiert,
/// verschiedene Slots sind disjunkt. Lock-Ordnung: …->SCHED->FP_STATES (innerste).
static FP_STATES: SpinLock<Slab<FpState>> = SpinLock::new(Slab::empty());
/// Zähler abgeschlossener Lazy-FP-Owner-Wechsel (Save+Restore), für den Test.
static FP_SWITCHES: AtomicUsize = AtomicUsize::new(0);
/// Summe der per `reap` an den Allokator zurückgegebenen Stack-Bytes (monoton).
static REAPED_BYTES: AtomicU64 = AtomicU64::new(0);

// --- Per-Prozess-VSpaces (Weg C, Hybrid) ---
//
// Vertrauenswürdige PDs laufen in der globalen SAS-Map (TTBR0 = global_root, ASID 0);
// **isolierte** PDs in einer eigenen VSpace (eigene Wurzel + ASID), die nur den
// Kernel (EL1-only) + ihre eigene User-Region (EL0) mappt. Der Kontextwechsel setzt
// TTBR0 passend zum einlaufenden Thread.
/// Gepackter `TTBR0`-Wert (asid<<48 | root) je globalem Thread-Slot; `0` = globale
/// SAS-Map. Beim Spawn einer isolierten PD gesetzt.
#[allow(clippy::declare_interior_mutable_const)]
static VSPACE_OF: AtomicTable<AtomicU64> = AtomicTable::empty();

/// Gepacktes TTBR0 eines Thread-Slots lesen (0 = globale SAS-Map).
fn vspace_of(slot: usize) -> u64 {
    VSPACE_OF.get(slot).map(|v| v.load(Ordering::Relaxed)).unwrap_or(0)
}
/// Gepacktes TTBR0 eines Thread-Slots setzen.
fn set_vspace_of(slot: usize, packed: u64) {
    if let Some(v) = VSPACE_OF.get(slot) {
        v.store(packed, Ordering::Relaxed);
    }
}
/// Aktuell aktive VSpace (gepacktes TTBR0) je Kern; `0` = noch unbekannt. Vermeidet
/// einen TTBR0-Write, wenn die VSpace gleich bleibt (der häufige All-Trusted-Fall).
#[allow(clippy::declare_interior_mutable_const)]
static CURRENT_VSPACE: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(0) }; MAX_CORES];

/// **Per-Kern** Scheduler-Instanzen, jede hinter eigenem Lock. Der heiße
/// Timer-/IPI-Reschedule-Pfad sperrt nur `SCHEDS[core]` des eigenen Kerns -> echte
/// parallele Einplanung ohne globalen Lock. Kern-übergreifend (z. B. `wake_remote`)
/// sperrt der Kernel die **Ziel**instanz und schickt einen Reschedule-IPI.
#[allow(clippy::declare_interior_mutable_const)]
static SCHEDS: [SpinLock<Scheduler>; MAX_CORES] =
    [const { SpinLock::new(Scheduler::new()) }; MAX_CORES];

// --- Feinkörnige Ressourcen-Locks (ersetzen den einen RES-Lock) ---
//
// Sperrordnung (außen->innen): CAPS < {EPS[i], NTFNS[i], MEM} < SCHEDS[*] < FP_STATES.
// Der Reschedule-Pfad nimmt nur SCHEDS[core] (+ atomares FP_OWNER), wartet also nie
// auf einen anderen Lock. IPC auf verschiedenen Endpoints läuft parallel (je eigener
// EPS[i]-Lock). Die heiße Cap-Auflösung ist rein lesend und nimmt CAPS nur GETEILT
// (Read) -> Lookups auf verschiedenen Kernen laufen parallel; nur die seltenen
// Mutationen (install/copy/mint/move/delete/revoke/grant/PD-Ops) sperren CAPS exklusiv
// (Write). `read()` und `write()` liegen an derselben Ordnungsposition wie zuvor `lock()`.
/// Cap-Auflösung + -Verwaltung (CapSpace + PD-Tabelle) hinter einem Reader-Writer-Lock.
static CAPS: RwSpinLock<Caps> = RwSpinLock::new(Caps::new());
/// Physischer Allokator (Thread-Stacks etc.) — getrennt, blockiert IPC nicht.
static MEM: SpinLock<PhysAllocator> = SpinLock::new(PhysAllocator::new());
/// **Per-Endpoint** Locks: IPC auf verschiedenen Endpoints ist nebenläufig.
#[allow(clippy::declare_interior_mutable_const)]
static EPS: [SpinLock<Endpoint>; NENDPOINTS] =
    [const { SpinLock::new(Endpoint::EMPTY) }; NENDPOINTS];
/// **Per-Notification** Locks.
#[allow(clippy::declare_interior_mutable_const)]
static NTFNS: [SpinLock<Notification>; NNOTIFICATIONS] =
    [const { SpinLock::new(Notification::EMPTY) }; NNOTIFICATIONS];

// --- Trap-Hooks ---

fn reschedule(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    // Deferred-IRQ-Zustellung (ext-22, P5): pending Geräte-IRQs als Notification signalisieren
    // — VOR dem SCHEDS-Lock (signal nimmt NTFNS<SCHEDS; kein verschachtelter SCHEDS). Fast-
    // Check -> Null-Overhead auf dem heißen Timer-Pfad, wenn nichts pending ist.
    drain_pending_irqs();
    // Heißer Pfad: nur der Scheduler-Lock DIESES Kerns (parallel zu anderen Kernen).
    // Echter Zeitscheiben-Tick -> MCS-Budget des laufenden Threads belasten.
    let next = {
        let mut sched = SCHEDS[core].lock();
        let next = sched.on_tick(core, frame as usize, true);
        sync_fp_trap(core, &sched);
        sync_vspace(core, &sched);
        next
    }; // SCHEDS freigegeben — der Lastausgleich sperrt selbst (zwei Kerne).
    // Periodischer Lastausgleich (ext-30). Sicher an dieser Stelle: `balance_once` verschiebt
    // nie den **laufenden** Thread, der gerade gewählte `next`-Frame bleibt also gültig.
    if BALANCING.load(Ordering::Relaxed) {
        let n = BALANCE_TICK[core].fetch_add(1, Ordering::Relaxed) + 1;
        if n % BALANCE_INTERVAL_TICKS == 0 {
            balance_once();
        }
    }
    next as *mut TrapFrame
}

/// Callback für den Dispatch: eine Capability löschen (Finalisierung inkl. Speicher-
/// rückgabe und Abbruch finalisierter Reply-Calls). Wird **ohne** gehaltene Dispatch-Locks
/// gerufen (der Dispatch gibt CAPS/EPS vorher frei); `cap_delete` sperrt selbst CAPS+MEM
/// in der richtigen Ordnung. Ein Fehlschlag (Cap bereits weg) ist unkritisch.
/// SYS_LOAD-Callback in den Binary-Loader. Auf x86_64 gibt es (noch) kein Boot-Archiv —
/// dessen Fenster ist ARM-/QEMU-`virt`-spezifisch; das x86-Gegenstück wären Multiboot-Module.
/// Bis dahin schlägt `SYS_LOAD` dort sauber fehl, statt etwas Halbes zu laden.
#[cfg(target_arch = "aarch64")]
use crate::loader::load_by_index;
#[cfg(not(target_arch = "aarch64"))]
fn load_by_index(_index: u32, _endow: &[(usize, CapPtr)]) -> Option<usize> {
    None
}

fn dispatch_delete_cap(cap: sel4lake_cap::CapPtr) {
    let _ = cap_delete(cap);
}

fn syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    if hal::exception::frame_from_el0(frame as usize) {
        EL0_SYSCALL_SEEN.store(true, Ordering::Relaxed);
    }
    // Der Dispatch besorgt das Locking selbst (feinkörnig: CAPS für die kurze
    // Cap-Auflösung, dann per-Objekt-Lock; SchedOps je Op genau eine SCHEDS-Instanz).
    // Sperrordnung CAPS < EPS[i]/NTFNS[i] < SCHEDS -> deadlockfrei. IRQs im Trap
    // maskiert -> kein Preempt beim Lock-Halten.
    let mut ops = KernelSched;
    let next = sel4lake_microkit::dispatch(
        frame as usize,
        core,
        &mut ops,
        &CAPS,
        &EPS,
        &NTFNS,
        load_by_index, // ext-26: SYS_LOAD-Callback (cap-gegatet im Dispatch)
        dispatch_delete_cap,          // beim Grant verdrängte Cap freigeben (kein Slot-Leck)
    );
    // Der Syscall kann den laufenden Thread gewechselt haben (block/exit) -> FP-Trap
    // + VSpace passend zum neuen aktuellen Thread setzen.
    {
        let sched = SCHEDS[core].lock();
        sync_fp_trap(core, &sched);
        sync_vspace(core, &sched);
    }
    next as *mut TrapFrame
}

/// Facade über alle per-Kern-Scheduler-Instanzen für den IPC-/Dispatch-Pfad. Jede
/// Operation sperrt **genau eine** `SCHEDS`-Instanz (die des angegebenen Kerns bzw.
/// des Ziel-Threads) und gibt sie sofort wieder frei — nie zwei gleichzeitig. Beim
/// kern-übergreifenden Wecken (`unblock` auf einen Thread eines anderen Kerns)
/// schickt sie zusätzlich einen Reschedule-IPI an den Zielkern.
struct KernelSched;

/// Wie oft ein kernübergreifender Zugriff wiederholt wird, wenn der Thread genau zwischen
/// „Besitzer nachschlagen" und „Kern sperren" **migriert** ist. Migration ist selten und
/// endlich (der Balancer verschiebt einen Thread je Tick), zwei Wiederholungen sind schon
/// großzügig; die Schranke verhindert, dass ein pathologischer Fall hier festhängt.
const MIGRATION_RETRIES: usize = 4;

/// **Migrationssicherer Zugriff auf den besitzenden Kern eines Threads.**
///
/// Seit ext-30 ist der Kern eines Threads nicht mehr aus seiner `ThreadId` ableitbar,
/// sondern steht im Thread-Directory. Zwischen dem (lock-freien) Nachschlagen und dem
/// Sperren kann der Thread migrieren — dann gehört er dem gesperrten Kern nicht mehr und
/// `f` schlägt fehl. Hier wird genau dieser Fall erkannt (Besitzer hat gewechselt) und mit
/// dem **neuen** Besitzer wiederholt. Ein Fehlschlag ohne Besitzerwechsel ist ein echter
/// Fehlschlag (Thread tot / Operation unzulässig) und wird durchgereicht.
///
/// Gibt `(Ergebnis, Kern)` zurück — der Kern wird für den Reschedule-IPI gebraucht, der
/// **nach** dem Freigeben des Locks geschickt wird.
fn with_owner<R>(
    tid: ThreadId,
    mut f: impl FnMut(&mut Scheduler, usize) -> Option<R>,
) -> Option<(R, usize)> {
    for _ in 0..MIGRATION_RETRIES {
        let c = sel4lake_sched::owner_core(tid)?;
        if c >= num_cores() {
            return None;
        }
        let attempt = {
            let mut sched = SCHEDS[c].lock();
            f(&mut sched, c)
        }; // Lock hier freigegeben
        if let Some(r) = attempt {
            return Some((r, c));
        }
        // Fehlgeschlagen: nur wiederholen, wenn der Thread inzwischen woanders lebt.
        match sel4lake_sched::owner_core(tid) {
            Some(c2) if c2 != c => continue,
            _ => return None,
        }
    }
    None
}

/// Besitzender Kern eines Threads (lock-frei; kann beim Sperren bereits veraltet sein).
pub fn owner_core_of(tid: ThreadId) -> Option<usize> {
    sel4lake_sched::owner_core(tid)
}

/// Einen Reschedule-IPI an `core` schicken, falls es nicht der eigene ist.
fn kick(core: usize) {
    if core != hal::cpu::core_id() {
        hal::intc::send_sgi(core, hal::intc::IPI_RESCHED_INTID);
    }
}

impl SchedOps for KernelSched {
    fn current_id(&mut self, core: usize) -> ThreadId {
        SCHEDS[core].lock().current_id(core)
    }
    fn frame_of(&mut self, tid: ThreadId) -> Option<usize> {
        with_owner(tid, |s, _| s.frame_of(tid)).map(|(f, _)| f)
    }
    fn block_current(&mut self, core: usize, frame: usize) -> usize {
        SCHEDS[core].lock().block_current(core, frame)
    }
    fn switch_to(&mut self, core: usize, frame: usize, target: ThreadId) -> usize {
        SCHEDS[core].lock().switch_to(core, frame, target)
    }
    fn unblock(&mut self, tid: ThreadId) {
        if let Some((_, c)) = with_owner(tid, |s, _| s.unblock(tid).then_some(())) {
            kick(c);
        }
    }
    fn pause(&mut self, tid: ThreadId) {
        // Läuft das Ziel gerade auf einem anderen Kern, per Reschedule-IPI deplanen.
        if let Some((_, c)) = with_owner(tid, |s, _| s.pause(tid).then_some(())) {
            kick(c);
        }
    }
    fn stop(&mut self, tid: ThreadId) -> bool {
        // STOP = vollständiger Teardown: cross-core kill + (falls isoliert) VSpace/ASID
        // freigeben, damit ein gestopptes Backend/UserLand keine Ressourcen leakt.
        let asid = (vspace_of(tid.slot()) >> 48) as u16;
        let ok = kill_remote(tid);
        if ok && asid != 0 {
            vspace_teardown(asid);
            set_vspace_of(tid.slot(), 0);
        }
        ok
    }
    fn on_tick(&mut self, core: usize, frame: usize) -> usize {
        // YIELD ist freiwillig -> verbraucht kein MCS-Budget (tick = false).
        SCHEDS[core].lock().on_tick(core, frame, false)
    }
    fn exit_current(&mut self, core: usize, frame: usize) -> usize {
        // Tid des sich beendenden Threads vor dem Wechsel merken; IRQs sind im Trap
        // maskiert -> der aktuelle Thread ist über die kurzen Sperren stabil.
        let tid = SCHEDS[core].lock().current_id(core);
        let next = SCHEDS[core].lock().exit_current(core, frame);
        purge_ipc_queues(tid); // eager: aus allen IPC-Queues entfernen (keine SCHEDS gehalten)
        reclaim_user_kstack(tid.slot()); // EL0-Kernel-Stack-Pool-Slot zurückgeben (KSTACKS allein)
        next
    }
    fn kill(&mut self, tid: ThreadId, core: usize) -> bool {
        let ok = SCHEDS[core].lock().kill(tid, core);
        if ok {
            purge_ipc_queues(tid); // eager: tote IPC-Queue-Einträge vermeiden
            reclaim_user_kstack(tid.slot()); // getöteter EL0-Thread: Pool-Slot zurück
        }
        ok
    }
    fn end_donation(&mut self, core: usize) {
        SCHEDS[core].lock().end_donation(core);
    }
    fn map_frame(&mut self, caller: ThreadId, base: u64, len: u64, perm_code: u8) -> bool {
        // Nur isolierte PDs (ASID != 0). Granularität nach Frame-Größe (2-MiB-Block
        // oder 4-KiB-Seiten), Recht nach `perm_code` (0=Ro, 1=Rw, 2=Rx).
        let asid = (vspace_of(caller.slot()) >> 48) as u16;
        if asid == 0 || len == 0 || len % 4096 != 0 {
            return false;
        }
        let perm = match perm_code {
            2 => hal::mmu::UserPerm::Rx,
            1 => hal::mmu::UserPerm::Rw,
            _ => hal::mmu::UserPerm::Ro,
        };
        vspace_map(asid, base, len, perm)
    }
    fn unmap_frame(&mut self, caller: ThreadId, base: u64, len: u64) -> bool {
        let asid = (vspace_of(caller.slot()) >> 48) as u16;
        if asid == 0 || len == 0 || len % 4096 != 0 {
            return false;
        }
        vspace_unmap(asid, base, len)
    }
}

/// `CPACR_EL1.FPEN` für den **gerade aktuellen** Thread auf `core` setzen: FP an
/// EL0 trappen, falls dieser Thread NICHT der FP-Owner des Kerns ist (so trappt
/// sein erster FP-Zugriff und löst den Lazy-Owner-Wechsel aus); sonst FP freigeben.
/// Am Ende jedes Hooks aufzurufen, der den laufenden Thread gewechselt haben kann.
fn sync_fp_trap(core: usize, sched: &Scheduler) {
    let cur = sched.current_id(core).to_raw();
    let owner = FP_OWNER[core].load(Ordering::Relaxed);
    hal::fp::set_el0_trap(cur != owner);
}

/// `TTBR0` (Adressraum) für den **gerade aktuellen** Thread auf `core` setzen:
/// dessen isolierte VSpace, oder die globale SAS-Map (trusted). Schreibt TTBR0 nur,
/// wenn sich die VSpace ändert (vermeidet `isb` im häufigen All-Trusted-Fall). Am
/// Ende jedes Hooks aufzurufen, der den laufenden Thread gewechselt haben kann.
fn sync_vspace(core: usize, sched: &Scheduler) {
    let slot = sched.current_id(core).slot();
    let v = vspace_of(slot);
    let want = if v == 0 { hal::mmu::global_root() } else { v };
    if CURRENT_VSPACE[core].load(Ordering::Relaxed) != want {
        let root = want & ((1u64 << 48) - 1);
        let asid = (want >> 48) as u16;
        hal::mmu::set_user_vspace(root, asid);
        // Spekulations-Barriere an der Adressraum-Grenze: nach dem Wechsel darf keine noch
        // offene Spekulation aus dem VORHERIGEN Adressraum weiterlaufen (`SB`, sonst
        // `dsb sy; isb`). Nur im Wechselfall — der All-Trusted-Pfad bleibt unbelastet.
        hal::cpu::speculation_barrier();
        CURRENT_VSPACE[core].store(want, Ordering::Relaxed);
    }
}

/// FP-Trap-Hook (Lazy-FP): Ein EL0-Thread hat FP/SIMD benutzt, ohne Owner zu sein.
/// Alten Owner sichern, FP-Kontext dieses Threads laden, ihn zum Owner machen und
/// FP freigeben. Rückgabe: derselbe Frame — der `eret` wiederholt die getrappte
/// Instruktion, jetzt mit aktivem FP. Hält SCHED (current_id) und FP_STATES.
fn fp_trap(frame: *mut TrapFrame) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    let sched = SCHEDS[core].lock();
    let cur_id = sched.current_id(core);
    let cur = cur_id.to_raw();
    let cur_slot = cur_id.slot();
    let owner = FP_OWNER[core].load(Ordering::Relaxed);

    let mut states = FP_STATES.lock();
    if owner != FP_OWNER_NONE && owner != cur {
        // Live-Register gehören dem alten Owner -> in seinen Slot sichern.
        let prev_slot = ThreadId::from_raw(owner).slot();
        hal::fp::save(&mut states[prev_slot]);
        FP_SWITCHES.fetch_add(1, Ordering::Relaxed);
    }
    // FP-Kontext dieses Threads laden (bei Erststart genullt).
    hal::fp::restore(&states[cur_slot]);
    drop(states);

    FP_OWNER[core].store(cur, Ordering::Relaxed);
    hal::fp::set_el0_trap(false); // FP für diesen (jetzt Owner-)Thread freigeben
    frame
}

/// Per-Thread-Kernelzustand für einen frisch belegten Slot zurücksetzen: genullter
/// FP-Kontext, veraltete FP-Owner-Referenzen löschen, und die VSpace auf **global**
/// (SAS) zurücksetzen (eine vorher isolierte PD auf diesem Slot darf nicht
/// nachwirken). Unter gehaltenem SCHED aufzurufen, BEVOR der Thread laufen kann.
fn fp_reset_slot(slot: usize) {
    FP_STATES.lock()[slot] = FpState::new();
    for owner in FP_OWNER.iter() {
        let o = owner.load(Ordering::Relaxed);
        if o != FP_OWNER_NONE && (o & 0xffff_ffff) as usize == slot {
            owner.store(FP_OWNER_NONE, Ordering::Relaxed);
        }
    }
    set_vspace_of(slot, 0); // Default: globale SAS-Map
}

/// Anzahl abgeschlossener Lazy-FP-Owner-Wechsel (Save eines alten Owners).
pub fn fp_switch_count() -> usize {
    FP_SWITCHES.load(Ordering::Relaxed)
}

/// EL0-Fault-Hook: ein User-Thread hat einen synchronen Fault ausgelöst (Zugriff
/// auf EL1-only Kernel-Speicher, privilegierte Instruktion o. Ä.). Statt den
/// Kernel anzuhalten, wird der fehlerhafte Thread **beendet** und auf den nächsten
/// lauffähigen Thread gewechselt — der Kernel läuft weiter. Das ist der konkrete
/// Nachweis der EL0/EL1-Privileg-Trennung: User-Code kann den Kernel nicht
/// kompromittieren. Nur der SCHED-Lock (wie der Reschedule-Pfad).
fn el0_fault(frame: *mut TrapFrame, esr: u64, far: u64) -> *mut TrapFrame {
    let core = hal::cpu::core_id();
    let ec = (esr >> 26) & 0x3f;
    EL0_FAULTS.fetch_add(1, Ordering::Relaxed);
    let (next, tid) = {
        let mut sched = SCHEDS[core].lock();
        let tid = sched.current_id(core);
        if vspace_of(tid.slot()) != 0 {
            // Der fehlerhafte Thread lief in einer isolierten VSpace -> die
            // Adressraumtrennung hat einen Fremd-/unmapped-Zugriff verhindert.
            ISO_FAULTS.fetch_add(1, Ordering::Release);
        }
        println!(
            "el0-trap: User-Thread {:#x} faultete (EC={ec:#04x} FAR={far:#018x}) -> beendet, Kernel laeuft weiter",
            tid.to_raw()
        );
        let next = sched.exit_current(core, frame as usize);
        // Auf den nächsten Thread gewechselt -> FP-Trap + VSpace passend setzen.
        sync_fp_trap(core, &sched);
        sync_vspace(core, &sched);
        (next, tid)
    }; // SCHEDS freigegeben
    purge_ipc_queues(tid); // eager: faultenden Thread aus allen IPC-Queues entfernen
    reclaim_user_kstack(tid.slot()); // EL0-Kernel-Stack-Pool-Slot zurückgeben (KSTACKS allein)
    // War es eine isolierte PD, ihre VSpace abbauen. Sicher: `sync_vspace` oben hat
    // TTBR0 bereits auf den nächsten Thread umgeschaltet (nicht mehr die tote VSpace).
    let packed = vspace_of(tid.slot());
    if packed != 0 {
        vspace_teardown((packed >> 48) as u16);
        set_vspace_of(tid.slot(), 0);
    }
    next as *mut TrapFrame
}

/// Wie oft hat der Kernel einen EL0-Fault abgefangen und den Thread isoliert?
pub fn el0_fault_count() -> usize {
    EL0_FAULTS.load(Ordering::Relaxed)
}

/// Hat ein Thread in einer **isolierten VSpace** durch einen Fremd-/unmapped-Zugriff
/// gefaultet? (Hardware-erzwungene Adressraumtrennung, Weg C.)
pub fn iso_faulted() -> bool {
    ISO_FAULTS.load(Ordering::Acquire) > 0
}
/// Anzahl der Faults in isolierten VSpaces (Fremdzugriff bzw. unmapped Frame).
pub fn iso_fault_count() -> usize {
    ISO_FAULTS.load(Ordering::Acquire)
}

/// Reschedule-, Syscall-, EL0-Fault- + Lazy-FP-Hook registrieren (einmalig, vor
/// IRQs/Threads).
pub fn set_hooks() {
    hal::exception::set_reschedule_hook(reschedule);
    hal::exception::set_syscall_hook(syscall);
    hal::exception::set_fault_hook(el0_fault);
    hal::exception::set_irq_hook(irq_hook); // Geräte-IRQ-Hook (ext-22, P5)
    hal::exception::set_fp_hook(fp_trap);
}

/// Freies RAM `[free_base, ram_end)` beim Allokator registrieren.
pub fn init_mem(free_base: u64, ram_end: u64) {
    MEM.lock().add_region(free_base, ram_end - free_base);
}

/// **Thread-Slots je Kern**, die der Kernel im Mittel vorsieht. Die Gesamtkapazität ist
/// `cores * THREADS_PER_CORE`; jeder Kern kann durch Migration bis zum
/// [`MIGRATION_HEADROOM`]-fachen davon *hosten*.
const THREADS_PER_CORE: usize = 256;
/// Faktor, um den die **Hosting**-Kapazität eines Kerns über seinem Anteil liegt. Ohne
/// Reserve könnte kein Kern einen migrierten Thread aufnehmen, sobald alle Kerne ihren
/// Anteil ausgeschöpft haben.
const MIGRATION_HEADROOM: usize = 2;

/// **Kernel-Tabellen zur Boot-Zeit dimensionieren + anlegen** (ext-30).
///
/// Ersetzt die früheren `.bss`-Arrays mit Compile-Zeit-Konstanten (`[Tcb; PER_CORE]`,
/// `[FpState; MAX_THREADS]`, …). `cores` ist die **gemessene** Kernzahl (aus dem Device
/// Tree); daraus folgen Thread-Kapazität und Tabellengrößen. Danach sind alle
/// per-Kern-Scheduler an ihre Kern-ID gebunden — vom Bootkern **einmalig vor** dem
/// Erzeugen irgendwelcher Threads und **vor** dem Start der Sekundärkerne aufzurufen
/// (der Bootkern plant Threads auf noch nicht gestartete Kerne ein).
///
/// Gibt `(cores, threads_total, per_core_hosting, bytes)` zurück (Boot-Report).
pub fn configure(cores: usize) -> (usize, usize, usize, u64) {
    let cores = cores.clamp(1, MAX_CORES);
    NUM_CORES.store(cores, Ordering::Relaxed);
    let total = cores * THREADS_PER_CORE;
    let per_core = THREADS_PER_CORE * MIGRATION_HEADROOM;
    let mut bytes = 0u64;

    // Eine Tabelle belegen. Der Speicher gehört ab hier **dauerhaft** dem Kernel: die
    // MemoryCap wird bewusst fallen gelassen (kein `Drop` -> die Region kehrt nie in den
    // Allokator zurück). Kerneltabellen leben bis zum Reboot.
    let mut table = |size: usize, align: usize| -> *mut u8 {
        let cap = mem_alloc(size as u64, align as u64).expect("Kerneltabelle: RAM erschoepft");
        bytes += cap.len();
        cap.base() as *mut u8
    };

    // Thread-Directory (gid -> Kern/Slot) + gid-Freiliste.
    let dir = table(
        sel4lake_sched::directory_bytes(total),
        sel4lake_sched::directory_align().max(4096),
    );
    // SAFETY: frisch allozierter, exklusiver, korrekt ausgerichteter Speicher der
    // geforderten Größe, der bis zum Reboot lebt; einmaliger Aufruf beim Boot, bevor ein
    // anderer Kern läuft.
    unsafe { sel4lake_sched::attach_directory(dir, total) };

    // Per-Kern-Tabellen (TCBs + Zombie-Ring + Freiliste).
    let core_bytes = sel4lake_sched::core_storage_bytes(per_core);
    for c in 0..cores {
        let mem = table(core_bytes, sel4lake_sched::core_storage_align().max(4096));
        // SAFETY: wie oben; jede Instanz bekommt ihren **eigenen** Block (exklusiv).
        unsafe { SCHEDS[c].lock().attach_storage(c, mem, per_core) };
    }

    // Per-Thread-Tabellen des Kernels (über den globalen, migrationsstabilen Slot indiziert).
    let fp = table(
        total * core::mem::size_of::<FpState>(),
        core::mem::align_of::<FpState>().max(4096),
    );
    // SAFETY: wie oben (exklusiv, ausgerichtet, dauerhaft, einmalig).
    unsafe {
        FP_STATES
            .lock()
            .attach(fp as *mut FpState, total, |_| FpState::new())
    };
    let vs = table(
        total * core::mem::size_of::<AtomicU64>(),
        core::mem::align_of::<AtomicU64>().max(4096),
    );
    // SAFETY: wie oben; `VSPACE_OF` wird erst nach diesem Anhängen gelesen.
    unsafe {
        VSPACE_OF.attach(vs as *mut AtomicU64, total, |_| AtomicU64::new(0));
    }
    let ks = table(
        total * core::mem::size_of::<u64>(),
        core::mem::align_of::<u64>().max(4096),
    );
    // SAFETY: wie oben.
    unsafe {
        KSTACKS
            .lock()
            .base_of
            .attach(ks as *mut u64, total, |_| 0u64)
    };

    (cores, total, per_core, bytes)
}

/// Boot-Kontext des aufrufenden Kerns als Idle-Thread registrieren (vor IRQs).
pub fn init_core() {
    let core = hal::cpu::core_id();
    SCHEDS[core].lock().init_core(core, IDLE_PRIO);
}

// --- Allokator (MEM) ---

/// Physische Region `[base, base+len)` **nullen**.
///
/// Datenremanenz-Schutz: der Allokator vergibt Regionen wieder, die zuvor einem anderen
/// Subjekt gehörten (Thread-Stack, isolierte PD-Region, DMA-Puffer, geladene Segmente).
/// Ohne Nullung könnte der neue Eigentümer die Restdaten des alten lesen — eine
/// Vertraulichkeitslücke ÜBER Vertrauensgrenzen hinweg (und Boot-RAM enthält ohnehin
/// unbekannten Firmware-Inhalt). Deshalb wird **bei der Vergabe** genullt, nicht bei der
/// Rückgabe: das deckt auch fabrikfrisches RAM ab und ist gegen jeden Freigabepfad robust
/// (auch solche, die eine Region ohne `free` verlieren).
fn zero_phys(base: u64, len: u64) {
    // SAFETY: Der Bereich stammt direkt aus dem Allokator, gehört also exklusiv uns
    // (noch an kein Subjekt vergeben), liegt vollständig im identity-gemappten Normal-RAM
    // und wird nicht gleichzeitig benutzt. Rohspeicher-Initialisierung ist eine erlaubte
    // `unsafe`-Domäne (ADR 0021).
    unsafe { core::ptr::write_bytes(base as *mut u8, 0, len as usize) };
}

/// **Zentrale RAM-Allokation des Kernels: liefert stets genullten Speicher.**
/// Jede Region, die an ein Subjekt gehen kann, MUSS hierüber kommen (nicht direkt über
/// `MEM.lock().alloc`) — s. [`zero_phys`]. Genullt wird **außerhalb** des MEM-Locks: die
/// Region ist bereits exklusiv unsere, und das Nullen großer Blöcke soll den Allokator
/// nicht blockieren.
fn mem_alloc(size: u64, align: u64) -> Option<MemoryCap> {
    let cap = MEM.lock().alloc(size, align)?;
    zero_phys(cap.base(), cap.len());
    Some(cap)
}

pub fn alloc(size: u64, align: u64) -> Option<MemoryCap> {
    mem_alloc(size, align)
}
pub fn free(cap: MemoryCap) {
    MEM.lock().free(cap);
}
pub fn total_free() -> u64 {
    MEM.lock().total_free()
}
pub fn fragments() -> usize {
    MEM.lock().fragments()
}

// --- Capability-Space + PDs (CAPS) ---

/// Anzahl belegter Cap-Slots (Fuzzer-/Leak-Oracle).
#[cfg_attr(not(feature = "kernel-fuzz"), allow(dead_code))] // nur vom Fuzzer benutzt (ADR 0013)
pub fn cap_used_slots() -> usize {
    CAPS.read().cspace.used_slots()
}
/// Anzahl belegter Cap-Objekte (Fuzzer-/Leak-Oracle: kein Objekt ohne lebende Cap).
#[cfg_attr(not(feature = "kernel-fuzz"), allow(dead_code))] // nur vom Fuzzer benutzt (ADR 0013)
pub fn cap_used_objects() -> usize {
    CAPS.read().cspace.used_objects()
}
/// **CDT-/Refcount-Property-Oracle** (Fuzzer): `0` bei Konsistenz, sonst Anomalie-Code
/// (s. `CapSpace::audit_cdt`). Sichert: keine verlorenen Objekte, keine negativen/
/// falschen Refcounts, keine toten CDT-Knoten, keine Ableitung auf fremde Objekte,
/// Baumform.
pub fn cap_audit_cdt() -> u32 {
    CAPS.read().cspace.audit_cdt()
}

pub fn cap_install(cap: MemoryCap) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_memory(cap)
}
pub fn cap_copy(src: CapPtr, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.copy(src, rights)
}
pub fn cap_mint(src: CapPtr, rights: Rights, badge: u64) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.mint(src, rights, badge)
}
pub fn cap_move(src: CapPtr) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.move_cap(src)
}
pub fn cap_inspect(ptr: CapPtr) -> Option<CapInfo> {
    CAPS.read().cspace.inspect(ptr)
}
pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {
    // Finalisierung gibt ggf. Speicher zurück -> CAPS vor MEM (Sperrordnung). Beim
    // Finalisieren einer Reply-Cap wird der zugehörige Call abgebrochen (NACH dem
    // Freigeben von CAPS/MEM, da das Entblocken EPS<SCHEDS sperrt -> CAPS < EPS).
    let mut rf = sel4lake_cap::ReplyFinal::new();
    let r = {
        let mut caps = CAPS.write();
        let mut mem = MEM.lock();
        caps.cspace.delete(&mut mem, ptr, &mut rf)
    };
    abort_finalized_replies(&rf);
    r
}
pub fn cap_revoke(ptr: CapPtr) -> Result<(), CapError> {
    let mut rf = sel4lake_cap::ReplyFinal::new();
    let r = {
        let mut caps = CAPS.write();
        let mut mem = MEM.lock();
        caps.cspace.revoke(&mut mem, ptr, &mut rf)
    };
    abort_finalized_replies(&rf);
    r
}

/// Für jede beim Löschen/Revoke finalisierte Reply-Cap den ausstehenden Call abbrechen:
/// den noch wartenden Aufrufer mit `ERR_SERVER_GONE` entblocken (Revocation eines
/// Calls). Läuft OHNE gehaltenen CAPS/MEM-Lock (Ordnung CAPS < EPS < SCHEDS).
fn abort_finalized_replies(rf: &sel4lake_cap::ReplyFinal) {
    for (ep, caller_raw) in rf.iter() {
        endpoint_abort_call(ep as usize, ThreadId::from_raw(caller_raw));
    }
}

/// Einen konkreten ausstehenden Call an Endpoint `ep` abbrechen (Reply-Cap-Revocation):
/// ist `caller` noch der wartende Aufrufer, wird er mit `ERR_SERVER_GONE` entblockt.
pub fn endpoint_abort_call(ep: usize, caller: ThreadId) -> bool {
    if ep >= NENDPOINTS {
        return false;
    }
    let orphan = {
        let mut e = EPS[ep].lock();
        e.abort_call(caller)
    };
    if let Some(c) = orphan {
        unblock_with_error(c, sel4lake_abi::result::ERR_SERVER_GONE);
        true
    } else {
        false
    }
}

/// Eine **Reply-Capability** für den an Endpoint `ep` blockierten `caller` prägen
/// (first-class `ObjectKind::Reply`): die einmalige, revozierbare Autorität, diesen Call
/// abzubrechen. Wird die Cap gelöscht/revoked, wird der Aufrufer mit `ERR_SERVER_GONE`
/// entblockt (s. `cap_delete`/`cap_revoke`).
pub fn reply_cap_for(ep: usize, caller: ThreadId) -> Result<CapPtr, CapError> {
    CAPS.write()
        .cspace
        .install_reply(ep as u32, caller.to_raw(), Rights::WRITE)
}

// --- Test-Sonde: CAPS-Read-Concurrency (Verifikation des Reader-Writer-Locks) ---
//
// Beweist, dass der CAPS-Read-Lock mehrere Leser GLEICHZEITIG zulässt (mit dem alten
// exklusiven SpinLock strukturell unmöglich -> Höchststand stets 1). Zwei Sonden auf
// zwei Kernen synchronisieren sich über eine **zweiphasige Barriere innerhalb des
// gehaltenen Read-Locks**: keiner verlässt den Abschnitt, bevor beide angekommen sind
// (Phase 2), damit auch der Spätere den gemeinsamen Höchststand noch beobachtet.
static CAPLK_IN: AtomicU32 = AtomicU32::new(0); // aktuell gleichzeitig im Read-Abschnitt
static CAPLK_DEP: AtomicU32 = AtomicU32::new(0); // Abmarsch-Barriere (Phase 2)
static CAPLK_MAX: AtomicU32 = AtomicU32::new(0); // beobachteter Höchststand gleichzeitiger Leser

/// Beobachteter Höchststand gleichzeitiger CAPS-Leser (`>=2` beweist Read-Parallelität;
/// mit einem exklusiven Lock wäre er strukturell `1`).
pub fn caps_max_concurrent_readers() -> u32 {
    CAPLK_MAX.load(Ordering::Acquire)
}

/// Test-Sonde: nimmt den CAPS-**Read**-Lock und wartet (begrenzt durch `spin_limit`) an
/// einer Barriere, bis `want` Leser gleichzeitig im Read-Abschnitt sind. Gibt zurück, ob
/// das beobachtet wurde. Mit dem Reader-Writer-Lock kommen beide Sonden gleichzeitig
/// hinein (-> `true`); wäre der Lock exklusiv, blockierte die zweite Sonde -> die Barriere
/// läuft in `spin_limit` und beide melden `false`. Nimmt keinen weiteren Lock -> hält nur
/// den CAPS-Read-Lock (kein Sperrordnungsproblem). Im isolierten Test-Fenster aufzurufen.
pub fn caps_read_concurrency_probe(want: u32, spin_limit: u32) -> bool {
    let _g = CAPS.read();
    CAPLK_IN.fetch_add(1, Ordering::AcqRel);
    // Phase 1: ankommen + warten, bis beide Leser gleichzeitig drin sind.
    let mut seen = false;
    let mut s = 0u32;
    loop {
        let n = CAPLK_IN.load(Ordering::Acquire);
        CAPLK_MAX.fetch_max(n, Ordering::AcqRel);
        if n >= want {
            seen = true;
            break;
        }
        if s >= spin_limit {
            break;
        }
        s += 1;
        core::hint::spin_loop();
    }
    // Phase 2: Abmarsch-Barriere — erst gehen, wenn beide angekommen sind.
    CAPLK_DEP.fetch_add(1, Ordering::AcqRel);
    let mut s2 = 0u32;
    while CAPLK_DEP.load(Ordering::Acquire) < want && s2 < spin_limit {
        s2 += 1;
        core::hint::spin_loop();
    }
    CAPLK_IN.fetch_sub(1, Ordering::AcqRel);
    seen
}

// --- Endpoints / Notifications (per-Objekt-Locks) ---

/// Einen freien Endpoint-Slot reservieren (scannt die per-Endpoint-Locks).
pub fn create_endpoint() -> Option<usize> {
    for (i, ep) in EPS.iter().enumerate() {
        let mut e = ep.lock();
        if !e.is_used() {
            e.mark_used();
            return Some(i);
        }
    }
    None
}
pub fn install_endpoint_cap(ep: u32, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_endpoint(ep, rights)
}
pub fn create_notification() -> Option<usize> {
    for (i, n) in NTFNS.iter().enumerate() {
        let mut nt = n.lock();
        if !nt.is_used() {
            nt.mark_used();
            return Some(i);
        }
    }
    None
}
pub fn install_notification_cap(ntfn: u32, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_notification(ntfn, rights)
}

// --- Threads / Protection Domains ---

/// Einen Thread mit Priorität `prio` auf dem aktuellen Kern erzeugen (Stack aus
/// dem Allokator). Lock-Ordnung RES vor SCHEDS[core].
pub fn spawn(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    spawn_on_core(hal::cpu::core_id(), entry, arg, prio)
}

/// Den Kern mit der **geringsten Last** wählen (Anzahl belegter TCB-Slots). Sperrt
/// jede Scheduler-Instanz einzeln/kurz (nie zwei gleichzeitig).
pub fn least_loaded_core() -> usize {
    // **Lock-frei** (ext-30): die Last jedes Kerns steht in einem Atomic, das der jeweilige
    // Scheduler pflegt. Vorher sperrte diese Funktion JEDEN Kern-Scheduler nacheinander —
    // bei 256 Kernen 256 Lock-Zyklen je Spawn, und dabei stand die Einplanung aller Kerne.
    let mut best = 0usize;
    let mut best_load = usize::MAX;
    for c in 0..num_cores() {
        let load = sel4lake_sched::core_load(c);
        if load < best_load {
            best_load = load;
            best = c;
        }
    }
    best
}

// --- Thread-Migration (ext-30) -----------------------------------------------------------

/// Zähler erfolgreicher Migrationen (Telemetrie/Tests).
static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
/// Ist der **automatische** periodische Lastausgleich aktiv? Default **aus**: die
/// Migrationsmechanik ist vollständig und getestet, aber die Demo-/Testthreads dieses
/// Images setzen teils feste Kern-Affinität voraus (Cross-Core-IPC-Test, Budget-Donation,
/// Platzierungs-Telemetrie). Der Schalter trennt „Mechanismus fertig" von „Policy
/// standardmäßig an" — s. `todo.md` B4.
static BALANCING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Tick-Zähler je Kern für das Ausgleichs-Intervall.
static BALANCE_TICK: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(0) }; MAX_CORES];
/// Nur jeder n-te Tick prüft die Balance (Migration kostet Cache-Lokalität).
const BALANCE_INTERVAL_TICKS: u64 = 16;

/// Automatischen Lastausgleich ein-/ausschalten.
pub fn set_balancing(on: bool) {
    BALANCING.store(on, Ordering::Relaxed);
}

/// Anzahl bisher durchgeführter Thread-Migrationen.
pub fn migration_count() -> usize {
    MIGRATIONS.load(Ordering::Relaxed)
}

/// **Einen Thread des AKTUELLEN Kerns auf `dst` verschieben.**
///
/// Die Migration wird immer vom **abgebenden** Kern ausgeführt (Push, nicht Pull). Das ist
/// keine Bequemlichkeit, sondern notwendig: der Lazy-FP-Kontext eines Threads liegt
/// physisch in den FP-Registern **seines** Kerns. Nur dieser Kern kann ihn sichern — ein
/// fremder Kern könnte fremde FP-Register gar nicht lesen. Deshalb: FP hier lokal
/// ausspülen, dann übergeben.
///
/// Sperrordnung: beide Scheduler-Locks werden in **aufsteigender Kern-Reihenfolge**
/// genommen (zwei gleichzeitig migrierende Kerne können sich so nicht verklemmen).
///
/// Gibt `false` zurück, wenn der Thread nicht migrierbar ist (läuft gerade, aktive
/// Budget-Donation, fremder Kern) oder der Zielkern keine Kapazität hat.
pub fn migrate_to(tid: ThreadId, dst: usize) -> bool {
    let src = hal::cpu::core_id();
    if dst == src || dst >= num_cores() {
        return false;
    }
    if sel4lake_sched::owner_core(tid) != Some(src) {
        return false; // nur der besitzende Kern schiebt (s. o.)
    }
    // Lazy-FP: gehören die FP-Register dieses Kerns dem Migranten, MÜSSEN sie jetzt in
    // seinen (gid-indizierten, migrationsstabilen) Kontextpuffer gesichert werden —
    // andernfalls liefe er auf dem Zielkern mit fremdem FP-Zustand weiter.
    if FP_OWNER[src].load(Ordering::Relaxed) == tid.to_raw() {
        hal::fp::save(&mut FP_STATES.lock()[tid.slot()]);
        FP_OWNER[src].store(FP_OWNER_NONE, Ordering::Relaxed);
        // Der laufende Thread ist ab jetzt nicht mehr FP-Owner -> wieder trappen lassen.
        hal::fp::set_el0_trap(true);
    }
    let (lo, hi) = if src < dst { (src, dst) } else { (dst, src) };
    let ok = {
        let mut a = SCHEDS[lo].lock();
        let mut b = SCHEDS[hi].lock();
        let (source, target) = if src == lo {
            (&mut *a, &mut *b)
        } else {
            (&mut *b, &mut *a)
        };
        match source.detach_for_migration(tid) {
            None => false,
            Some(m) => match target.attach_migrated(m) {
                Ok(()) => true,
                // Zielkern voll -> zurück zum Quellkern (kein Thread geht verloren).
                Err(m) => {
                    let _ = source.attach_migrated(m);
                    false
                }
            },
        }
    }; // beide Locks freigegeben
    if ok {
        MIGRATIONS.fetch_add(1, Ordering::Relaxed);
        kick(dst); // Zielkern soll den Zugang zeitnah einplanen
    }
    ok
}

/// **Periodischer Lastausgleich** (ext-30): der aufrufende Kern gibt einen bereiten Thread
/// an einen deutlich weniger belasteten Kern ab.
///
/// Aus dem Tick-Pfad aufzurufen, aber bewusst **nicht bei jedem Tick** (der Aufrufer
/// entscheidet über das Intervall): Migration kostet Cache-Lokalität, deshalb erst ab einer
/// spürbaren Differenz (`IMBALANCE_THRESHOLD`) und höchstens ein Thread je Aufruf — das
/// dämpft Oszillation (zwei Kerne, die sich Threads gegenseitig zuschieben).
const IMBALANCE_THRESHOLD: usize = 2;

pub fn balance_once() -> bool {
    let src = hal::cpu::core_id();
    let my_load = sel4lake_sched::core_load(src);
    let dst = least_loaded_core();
    if dst == src || my_load < sel4lake_sched::core_load(dst) + IMBALANCE_THRESHOLD {
        return false;
    }
    let Some(cand) = SCHEDS[src].lock().migration_candidate() else {
        return false;
    };
    migrate_to(cand, dst)
}

/// **Lastbewusst** einen Thread erzeugen: auf dem aktuell am wenigsten ausgelasteten
/// Kern. Best-effort (die Last kann sich zwischen Auswahl und spawn ändern). Seit ext-30
/// ist die Platzierung nicht mehr endgültig — [`balance_once`] verschiebt Threads zur
/// Laufzeit nach.
pub fn spawn_balanced(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    spawn_on_core(least_loaded_core(), entry, arg, prio)
}

/// Wie [`spawn`], aber auf einem **bestimmten** Kern `core` (z. B. vom Bootkern aus,
/// um Arbeit über die Kerne zu verteilen). Der Zielkern muss vorab via
/// [`bind_cores`] gebunden sein. Lock-Ordnung RES vor SCHEDS[core].
pub fn spawn_on_core(core: usize, entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let (base, len) = {
        let stack = mem_alloc(STACK_SIZE, 16)?;
        (stack.base() as usize, stack.len() as usize)
    }; // MEM vor SCHEDS freigegeben (Ordnung MEM < SCHEDS)
    let mut sched = SCHEDS[core].lock();
    let tid = sched.spawn(core, entry, arg, base, len, prio)?;
    fp_reset_slot(tid.slot()); // frischer FP-Kontext, bevor der Thread laufen kann
    Some(tid)
}

/// Einen **EL0-User-Thread** erzeugen: Kernel-Stack aus dem EL1-only Pool (per
/// Free-List, beim Thread-Ende zurückgegeben), User-Stack aus dem EL0-zugänglichen
/// RAM. Der Thread läuft auf EL0 und kann nur per Syscall mit dem Kernel interagieren.
pub fn spawn_user(entry: usize, arg: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let kbase = claim_user_kstack()?; // EL0-Kernel-Stack aus MEM (16 KiB, ausgerichtet)
    let (user_base, user_len) = match mem_alloc(STACK_SIZE, 16) {
        Some(s) => (s.base() as usize, s.len() as usize),
        None => {
            release_user_kstack(kbase);
            return None;
        }
    }; // MEM vor SCHEDS freigegeben (Ordnung MEM < SCHEDS)
    let tid = {
        let mut sched = SCHEDS[core].lock();
        let r = sched.spawn_user(core, entry, arg, kbase, USER_KSTACK_SIZE, user_base, user_len, prio);
        if let Some(t) = r {
            fp_reset_slot(t.slot()); // frischer FP-Kontext, bevor der Thread laufen kann
        }
        r
    }; // SCHEDS freigegeben
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kbase); // Kstack dem Thread zuordnen
            Some(t)
        }
        None => {
            release_user_kstack(kbase);
            MEM.lock().free_region(PhysRegion::new(user_base as u64, user_len as u64));
            None
        }
    }
}

/// Statische Reserve an isolierten-VSpace-Slots (BSS: `VSPACES` = MAX_VSPACES × ~24 B). Die
/// **tatsächlich vergebene** Anzahl deckelt `create_vspace` auf `min(MAX_VSPACES, max_asid())`:
/// die HW-ASID-Breite (`TCR.AS`; 255 bei 8-Bit, 65535 bei FEAT_ASID16) ist die harte Grenze —
/// eine ASID darüber würde aliasen. Hoher Default, weil `create_vspace` ohnehin nur bis `usable`
/// scannt (O(usable)) und die HW-Grenze schützt. 4096 = 96 KiB BSS.
const MAX_VSPACES: usize = 4096;

/// Metadaten einer isolierten VSpace (für map/unmap + Teardown). Indiziert per
/// `asid - 1`.
#[derive(Clone, Copy)]
struct VSpaceEnt {
    used: bool,
    l1: u64,
    l2: u64,
}
static VSPACES: SpinLock<[VSpaceEnt; MAX_VSPACES]> =
    SpinLock::new([VSpaceEnt { used: false, l1: 0, l2: 0 }; MAX_VSPACES]);

/// Eine **leere** isolierte VSpace anlegen (Kernel EL1-only, kein User-Frame). Gibt
/// `(asid, l1_phys)` oder `None` (kein ASID/Speicher). Allokiert L1+L2 aus `MEM`
/// (zuerst, freigegeben), trägt dann die Metadaten ein (`MEM` und `VSPACES` nie
/// gleichzeitig gehalten -> keine Sperrordnungs-Inversion zu Teardown/map).
fn create_vspace() -> Option<(u16, u64)> {
    // ASID/VSpace-Slot aus der **Free-List** (VSPACES) belegen — wiederverwendbar
    // (kein monoton wachsender Zähler -> keine ASID-Leaks). Reservierung unter EINEM
    // Lock (Platzhalter), damit zwei Kerne nicht denselben Slot greifen.
    let asid = {
        // Nur die ersten `usable` Slots vergeben: ASID = Slot+1 darf die HW-ASID-Breite
        // (`max_asid()`, 255 oder 65535) NIE überschreiten, sonst aliast eine zu große ASID auf
        // eine andere VSpace (Isolationsbruch). MAX_VSPACES darf also größer als die HW-Grenze sein
        // (statische Reserve), genutzt wird aber nur bis `usable`.
        let usable = MAX_VSPACES.min(hal::mmu::max_asid() as usize);
        let mut t = VSPACES.lock();
        let i = t.iter().take(usable).position(|v| !v.used)?;
        t[i] = VSpaceEnt { used: true, l1: 0, l2: 0 }; // reserviert
        (i + 1) as u16
    };
    let a = mem_alloc(4096, 4096);
    let b = mem_alloc(4096, 4096);
    let (l1, l2) = match (a, b) {
        (Some(l1), Some(l2)) => (l1, l2),
        (a, b) => {
            let mut mem = MEM.lock();
            if let Some(c) = a {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            if let Some(c) = b {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            drop(mem);
            VSPACES.lock()[asid as usize - 1].used = false; // Slot zurückgeben
            return None;
        }
    };
    // Läuft in der globalen Map. Der `alloc`-Rückkanal deckt Architekturen mit einer
    // zusätzlichen Tabellenebene ab (x86_64: PML4 -> PDPT -> PD); schlägt er fehl, werden die
    // beiden bereits belegten Frames wieder freigegeben.
    let mut extra: Option<u64> = None;
    let ok = hal::mmu::vspace_create_base(l1.base(), l2.base(), &mut || {
        let f = mem_alloc(4096, 4096)?;
        extra = Some(f.base());
        Some(f.base())
    });
    if !ok {
        let mut mem = MEM.lock();
        mem.free_region(PhysRegion::new(l1.base(), 4096));
        mem.free_region(PhysRegion::new(l2.base(), 4096));
        if let Some(e) = extra {
            mem.free_region(PhysRegion::new(e, 4096));
        }
        drop(mem);
        VSPACES.lock()[asid as usize - 1].used = false;
        return None;
    }
    VSPACES.lock()[asid as usize - 1] = VSpaceEnt {
        used: true,
        l1: l1.base(),
        l2: l2.base(),
    };
    Some((asid, l1.base()))
}

/// Anzahl freier VSpace-/ASID-Slots (für die Leak-Prüfung des Churn-Tests).
pub fn free_vspaces() -> usize {
    VSPACES.lock().iter().filter(|v| !v.used).count()
}

/// **VMM-Property-Oracle** (read-only, Fuzzer): prüft jede belegte isolierte VSpace auf
/// die **W^X-Invariante** (keine EL0-Seite schreibbar+ausführbar) und strukturelle
/// Konsistenz (L3-Zeiger valide), sowie dass `l1`/`l2` belegter Einträge gesetzt sind.
/// Gibt `0` bei Konsistenz zurück, sonst: 1=W^X-Verletzung, 2=struktureller Defekt
/// (L3-Zeiger), 3=belegte VSpace ohne L1/L2 (inkonsistenter Slot). (ASID-Eindeutigkeit
/// ist durch den VSPACES-Index = ASID-1 baulich garantiert.) Sperrt `VSPACES` kurz und
/// liest die Tabellen über die Identity-Map.
pub fn vspace_audit() -> u32 {
    // Unter dem `VSPACES`-Lock walken: `vspace_wx_ok`/`vspace_device_wx_ok` lesen nur die Tabellen
    // (nehmen KEINE Locks) -> kein Deadlock, und race-frei (kein Teardown während des Walks). Früher
    // wurden die L1/L2-Adressen erst in `[u64; MAX_VSPACES]` auf dem Stack kopiert; das skaliert nicht
    // auf große MAX_VSPACES (Stack-Overflow) und war unnötig, da der Walk lock-frei ist.
    let t = VSPACES.lock();
    for v in t.iter() {
        if !v.used {
            continue;
        }
        if v.l1 == 0 || v.l2 == 0 {
            return 3;
        }
        let code = hal::mmu::vspace_wx_ok(v.l2);
        if code != 0 {
            return code;
        }
        // W^X auch für GiB-0-Device-Mappings (ext-22): keine EL0-Device-Seite ausführbar.
        if !hal::mmu::vspace_device_wx_ok(v.l1) {
            return 1;
        }
    }
    0
}

/// Anzahl belegter TCB-Slots auf `core` (für die Leak-Prüfung).
pub fn used_tcbs(core: usize) -> usize {
    SCHEDS[core].lock().load()
}

/// L2-Tabelle einer (gültigen) isolierten VSpace nachschlagen.
fn vspace_l2(asid: u16) -> Option<u64> {
    if asid == 0 || asid as usize > MAX_VSPACES {
        return None;
    }
    let v = VSPACES.lock()[asid as usize - 1];
    if v.used {
        Some(v.l2)
    } else {
        None
    }
}

/// Die L1-Wurzel der VSpace `asid` (für Device-MMIO-Mapping in GiB 0).
fn vspace_l1(asid: u16) -> Option<u64> {
    if asid == 0 || asid as usize > MAX_VSPACES {
        return None;
    }
    let v = VSPACES.lock()[asid as usize - 1];
    if v.used {
        Some(v.l1)
    } else {
        None
    }
}

/// Einen 2-MiB-Frame `phys` in die VSpace `asid` mappen (EL0-RW) + ASID flushen.
fn vspace_map_region(asid: u16, phys: u64) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    if hal::mmu::vspace_map_block(l2, phys) {
        hal::mmu::flush_asid(asid);
        true
    } else {
        false
    }
}

/// 4-KiB-Granularität: Region `[base, base+len)` (identity) mit `perm` in die VSpace
/// `asid` mappen. 2-MiB-ausgerichtete 2-MiB-Regionen mit RW/RX nutzen den
/// Block-Fastpath; sonst seitenweise (L3 wird bei Bedarf aus `MEM` angelegt).
fn vspace_map(asid: u16, base: u64, len: u64, perm: hal::mmu::UserPerm) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    let ok = if len == hal::mmu::ISO_REGION_SIZE
        && base % hal::mmu::ISO_REGION_SIZE == 0
        && perm != hal::mmu::UserPerm::Ro
    {
        match perm {
            hal::mmu::UserPerm::Rx => hal::mmu::vspace_map_code_block(l2, base),
            _ => hal::mmu::vspace_map_block(l2, base),
        }
    } else {
        let mut all = true;
        let mut p = base;
        while p < base + len {
            let mapped =
                hal::mmu::vspace_map_page(l2, p, perm, &mut || mem_alloc(4096, 4096).map(|c| c.base()));
            if !mapped {
                all = false;
                break;
            }
            p += 4096;
        }
        all
    };
    if ok {
        hal::mmu::flush_asid(asid);
    }
    ok
}

/// 4-KiB-Granularität: Region `[base, base+len)` aus der VSpace `asid` entfernen.
fn vspace_unmap(asid: u16, base: u64, len: u64) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    let ok = if len == hal::mmu::ISO_REGION_SIZE && base % hal::mmu::ISO_REGION_SIZE == 0 {
        hal::mmu::vspace_unmap_block(l2, base)
    } else {
        let mut all = true;
        let mut p = base;
        while p < base + len {
            if !hal::mmu::vspace_unmap_page(l2, p) {
                all = false;
            }
            p += 4096;
        }
        all
    };
    if ok {
        hal::mmu::flush_asid(asid);
    }
    ok
}

/// Kernel-Setup: Region `[base, base+len)` mit `perm_code` (0=Ro,1=Rw,2=Rx) in die
/// VSpace des Threads `tid` mappen (für Demos, die vorab feingranulare Seiten — z. B.
/// RW/RO/Guard — anlegen wollen). No-Op für nicht-isolierte Threads.
pub fn map_into_thread(tid: ThreadId, base: u64, len: u64, perm_code: u8) -> bool {
    let asid = (vspace_of(tid.slot()) >> 48) as u16;
    if asid == 0 {
        return false;
    }
    let perm = match perm_code {
        2 => hal::mmu::UserPerm::Rx,
        1 => hal::mmu::UserPerm::Rw,
        _ => hal::mmu::UserPerm::Ro,
    };
    vspace_map(asid, base, len, perm)
}

/// Kernel-Setup-Gegenstück zu [`map_into_thread`]: `[base, base+len)` aus der VSpace
/// des Threads `tid` wieder entfernen (Seiten auf EL1-only, TLB-Flush). Für den
/// Fuzzer/Tests, um den per-Seite-Unmap-Pfad explizit zu fahren. No-Op (false) für
/// nicht-isolierte Threads.
#[cfg_attr(not(feature = "kernel-fuzz"), allow(dead_code))] // nur vom Fuzzer benutzt (ADR 0013)
pub fn unmap_into_thread(tid: ThreadId, base: u64, len: u64) -> bool {
    let asid = (vspace_of(tid.slot()) >> 48) as u16;
    if asid == 0 {
        return false;
    }
    vspace_unmap(asid, base, len)
}

/// Eine isolierte VSpace abbauen: alle per-PD-L3-Tabellen **und** L1+L2 an `MEM`
/// zurückgeben, Eintrag freigeben, TLB der ASID flushen. Beim Thread-Ende einer
/// isolierten PD aufzurufen.
fn vspace_teardown(asid: u16) {
    if asid == 0 || asid as usize > MAX_VSPACES {
        return;
    }
    loaded_free(asid); // ext-26 L4: geladene Programm-Segment-Frames freigeben (sonst Leck)
    // Eintrag NUR LESEN (Slot noch NICHT freigeben): der Kontextwechsel flusht nicht, sondern
    // verlässt sich auf ASID-Tagging. Würde der Slot vor `flush_asid` freigegeben, könnte ein
    // anderer Kern die ASID via `create_vspace` sofort neu vergeben + benutzen, WÄHREND die alten
    // TLB-Einträge dieser ASID noch existieren -> das neue Programm sähe fremde Mappings. Auf realer
    // HW mit 16-Bit-ASIDs reproduzierbar (geladene PDs liefen falsch); 8-Bit maskierte es. Reihenfolge:
    // 1) TLB der ASID leeren, 2) Frames freigeben, 3) Slot freigeben (ab dann reusable).
    let ent = { VSPACES.lock()[asid as usize - 1] };
    if ent.used {
        hal::mmu::flush_asid(asid); // 1. TLB für diese ASID leeren (Tabellen noch intakt)
        let mut mem = MEM.lock();
        // 2. feingranulare L3-Tabellen (aus Seiten-Mappings) einsammeln.
        hal::mmu::vspace_collect_l3s(ent.l2, &mut |p| {
            mem.free_region(PhysRegion::new(p, 4096));
        });
        // GiB-0-Device-Tabellen (aus MMIO-Mappings, ext-22) einsammeln, falls vorhanden.
        hal::mmu::vspace_collect_device_tables(ent.l1, &mut |p| {
            mem.free_region(PhysRegion::new(p, 4096));
        });
        mem.free_region(PhysRegion::new(ent.l1, 4096));
        mem.free_region(PhysRegion::new(ent.l2, 4096));
        drop(mem);
    }
    // 3. Slot erst JETZT freigeben (nach Flush) -> keine Wiederverwendung mit veralteten Einträgen.
    VSPACES.lock()[asid as usize - 1] = VSpaceEnt { used: false, l1: 0, l2: 0 };
}

/// Einen **isolierten EL0-User-Thread** erzeugen (Weg C): eigene VSpace, die nur den
/// Kernel (EL1-only) + eine **private 2-MiB-Stack-Region** (EL0-RW) mappt. Sein Code
/// ist die geteilte `.user_text` (EL0-RX in jeder VSpace). Greift er auf **fremdes**
/// User-RAM zu, ist das in seiner VSpace EL1-only -> Fault -> Kernel beendet ihn.
/// Zur Laufzeit kann er weitere Frames per `MAP`-Syscall (cap-gated) hinzunehmen.
/// Gibt `(ThreadId, region_base)`. VSpace-Tabellen werden beim Thread-Ende abgebaut.
pub fn spawn_isolated(entry: usize, arg: usize, prio: u8) -> Option<(ThreadId, u64)> {
    let core = hal::cpu::core_id();
    let kbase = claim_user_kstack()?; // EL0-Kernel-Stack aus MEM (16 KiB, ausgerichtet)

    // Private 2-MiB-Stack-Region (2-MiB-ausgerichtet, in GiB 1).
    let region_sz = hal::mmu::ISO_REGION_SIZE;
    let (rbase, rlen) = match mem_alloc(region_sz, region_sz) {
        Some(r) => (r.base(), r.len()),
        None => {
            release_user_kstack(kbase);
            return None;
        }
    };
    if rbase < hal::mmu::USER_RAM_MIN || rbase + rlen > hal::mmu::GIB1_END {
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kbase);
        return None;
    }
    let Some((asid, l1)) = create_vspace() else {
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kbase);
        return None;
    };
    // Stack-Region in die neue VSpace mappen (EL0-RW).
    vspace_map_region(asid, rbase);
    let packed = ((asid as u64) << 48) | l1;

    // Thread: Kernel-Stack aus dem Pool, User-Stack = die gemappte Region.
    let (user_base, user_len) = (rbase as usize, rlen as usize);
    let tid = {
        let mut sched = SCHEDS[core].lock();
        let r = sched.spawn_user(core, entry, arg, kbase, USER_KSTACK_SIZE, user_base, user_len, prio);
        if let Some(t) = r {
            fp_reset_slot(t.slot()); // FP + VSPACE_OF[slot]=0 (global) zurücksetzen
        }
        r
    };
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kbase); // Kstack dem Thread zuordnen (reclaim!)
            set_vspace_of(t.slot(), packed); // ab jetzt isoliert
            Some((t, rbase))
        }
        None => {
            vspace_teardown(asid);
            MEM.lock().free_region(PhysRegion::new(rbase, rlen));
            release_user_kstack(kbase);
            None
        }
    }
}

/// Eine isolierte PD **vollständig abbauen** (für den Churn-/Leak-Test bzw. den
/// EXIT einer isolierten PD): den (nicht laufenden) Thread `tid` beenden + seinen
/// Stack einsammeln (an `MEM`), die VSpace abbauen (L1/L2/L3 an `MEM`, ASID-Slot
/// zurück), den Kernel-Stack-Pool-Slot zurückgeben und `VSPACE_OF` leeren. Gibt
/// damit **alle** PD-Ressourcen frei. `tid` muss auf dem aktuellen Kern liegen und
/// darf nicht der laufende Thread sein.
pub fn destroy_isolated(tid: ThreadId) {
    let core = hal::cpu::core_id();
    let asid = (vspace_of(tid.slot()) >> 48) as u16;
    SCHEDS[core].lock().kill(tid, core); // ready -> Zombie (TCB-Slot frei, Stack vorgemerkt)
    purge_ipc_queues(tid); // eager: tote IPC-Queue-Einträge vermeiden
    reap(); // Stack-Zombie an MEM zurueck (korrekte Lock-Ordnung: SCHEDS frei, dann MEM)
    if asid != 0 {
        vspace_teardown(asid); // L1/L2/L3 + geladene Segmente an MEM, ASID-Slot frei, TLB-Flush
        set_vspace_of(tid.slot(), 0);
    }
    reclaim_user_kstack(tid.slot());
}

/// Einen **geladenen Prozess vollständig abbauen** (ext-26, L4): die im Cspace seiner PD
/// installierten Caps löschen (delegierte CDT-Kopien → Refcount runter), den Thread + die VSpace +
/// die geladenen Segment-Frames + den Kernel-Stack abbauen ([`destroy_isolated`]) und den PD-Slot
/// freigeben. Danach ist die Ressourcen-Baseline wiederhergestellt (kein Leck). `tid` muss auf dem
/// aktuellen Kern liegen und darf nicht der laufende Thread sein.
pub fn destroy_loaded(tid: ThreadId, pd: usize) {
    let caps = CAPS.read().pds.caps_of(pd); // Snapshot; Read-Lock danach frei
    for cap in caps.iter().flatten() {
        let _ = cap_delete(*cap); // jeden installierten Cap löschen (CAPS.write intern)
    }
    destroy_isolated(tid);
    CAPS.write().pds.free(pd); // PD-Slot freigeben
}

/// Eine PD **ohne Thread** abbauen: alle in ihrem Cspace installierten Caps löschen
/// (Refcount runter, ggf. Finalisierung) und den PD-Slot freigeben. Für PDs, die nur als
/// Cap-Container existieren (Setup-Fehlerpfade, Selbsttests); PDs **mit** Thread bauen
/// [`destroy_loaded`] bzw. der PDCTL-STOP-Pfad ab.
pub fn destroy_pd(pd: usize) {
    let caps = CAPS.read().pds.caps_of(pd); // Snapshot; Read-Lock danach frei
    for cap in caps.iter().flatten() {
        let _ = cap_delete(*cap);
    }
    CAPS.write().pds.free(pd);
}

/// Einen 2-MiB-Frame `phys` als **EL0-RX-Code** in die VSpace `asid` mappen + flushen.
fn vspace_map_code_region(asid: u16, phys: u64) -> bool {
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    if hal::mmu::vspace_map_code_block(l2, phys) {
        hal::mmu::flush_asid(asid);
        true
    } else {
        false
    }
}

/// Eine isolierte PD mit **privat geladenem nativem Code** erzeugen: der Code
/// `[code, code+code_len)` wird in einen frischen Frame kopiert, der **EL0-RX** in
/// die eigene VSpace gemappt wird (W^X; nicht die geteilte `.user_text`). Dazu ein
/// privater EL0-RW-Stack. Der Thread startet am Anfang des Code-Frames. Beweist, dass
/// eine isolierte PD beliebigen, nicht-geteilten Code in ihrer eigenen VSpace
/// ausführt. Gibt die `ThreadId`.
pub fn spawn_isolated_native(code: *const u8, code_len: usize, prio: u8) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let kbase = claim_user_kstack()?; // EL0-Kernel-Stack aus MEM (16 KiB, ausgerichtet)
    let sz = hal::mmu::ISO_REGION_SIZE;

    // Code-Frame + Stack-Frame (beide 2-MiB-ausgerichtet, in GiB 1).
    let in_gib1 = |b: u64, l: u64| b >= hal::mmu::USER_RAM_MIN && b + l <= hal::mmu::GIB1_END;
    let ca = mem_alloc(sz, sz);
    let sa = mem_alloc(sz, sz);
    let (cf, sf) = match (ca, sa) {
        (Some(cf), Some(sf)) => (cf, sf),
        (ca, sa) => {
            let mut mem = MEM.lock();
            if let Some(c) = ca {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            if let Some(c) = sa {
                mem.free_region(PhysRegion::new(c.base(), c.len()));
            }
            drop(mem);
            release_user_kstack(kbase);
            return None;
        }
    };
    let (cbase, clen) = (cf.base(), cf.len());
    let (sbase, slen) = (sf.base(), sf.len());
    if !in_gib1(cbase, clen) || !in_gib1(sbase, slen) || code_len > clen as usize {
        let mut mem = MEM.lock();
        mem.free_region(PhysRegion::new(cbase, clen));
        mem.free_region(PhysRegion::new(sbase, slen));
        drop(mem);
        release_user_kstack(kbase);
        return None;
    }

    // Code in den Frame kopieren (globale Map: cbase ist EL0+EL1-RW -> beschreibbar),
    // dann I-Cache kohärent machen, bevor er ausgeführt wird.
    // SAFETY: `cbase` ist ein frisch allozierter, identity-gemappter RW-Frame; wir
    // kopieren genau `code_len` (<= clen) Bytes aus dem gültigen Quellpuffer.
    unsafe {
        core::ptr::copy_nonoverlapping(code, cbase as *mut u8, code_len);
    }
    hal::cpu::sync_code_range(cbase as usize, code_len);

    let Some((asid, l1)) = create_vspace() else {
        let mut mem = MEM.lock();
        mem.free_region(PhysRegion::new(cbase, clen));
        mem.free_region(PhysRegion::new(sbase, slen));
        drop(mem);
        release_user_kstack(kbase);
        return None;
    };
    vspace_map_code_region(asid, cbase); // Code: EL0-RX (W^X)
    vspace_map_region(asid, sbase); //       Stack: EL0-RW
    let packed = ((asid as u64) << 48) | l1;

    let tid = {
        let mut sched = SCHEDS[core].lock();
        // Entry = Anfang des privaten Code-Frames; User-Stack = privater Stack-Frame.
        let r = sched.spawn_user(
            core,
            cbase as usize,
            0,
            kbase,
            USER_KSTACK_SIZE,
            sbase as usize,
            slen as usize,
            prio,
        );
        if let Some(t) = r {
            fp_reset_slot(t.slot());
        }
        r
    };
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kbase); // Kstack dem Thread zuordnen (reclaim!)
            set_vspace_of(t.slot(), packed);
            // Der private Code-Frame ist KEINE Reap-Region (das ist der Stack) und haengt als
            // 2-MiB-Block (kein L3) am L2 -> vspace_teardown faende ihn nicht. Unter der ASID
            // registrieren, damit ein spaeteres destroy_isolated ihn via loaded_free freigibt
            // (sonst leckt der Code-Frame beim Abbau). Der Stack wird separat via Reap freigegeben.
            loaded_register(asid, &[(cbase, clen)]);
            Some(t)
        }
        None => {
            vspace_teardown(asid);
            let mut mem = MEM.lock();
            mem.free_region(PhysRegion::new(cbase, clen));
            mem.free_region(PhysRegion::new(sbase, slen));
            drop(mem);
            release_user_kstack(kbase);
            None
        }
    }
}

/// **Stack-VA-Fenster** geladener Programme (fest, in GiB 1, getrennt von der Code-Link-VA
/// `0x4100_0000`) + Größe. Der Stack wird **nicht-identity** an diese VA gemappt.
const LOADED_STACK_VA: u64 = 0x43F0_0000;
const LOADED_STACK_BYTES: u64 = 0x4000; // 16 KiB

/// `src` (filesz Bytes) nach `dst_phys` kopieren + `[src.len(), total)` nullen (`.bss` + Padding).
/// Die **einzige** unsafe-Stelle des Ladepfads (ADR 0011 §2): Kopieren bereits **validierter**
/// Segmente in den Zielspeicher.
fn copy_segment(dst_phys: u64, src: &[u8], total: usize) {
    // SAFETY: `dst_phys` ist ein frisch allozierter, identity-gemappter RW-Frame der Größe `total`
    // (auf Seiten aufgerundet, >= `src.len()`); es werden genau `total` Bytes im Frame geschrieben.
    unsafe {
        let dst = dst_phys as *mut u8;
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
        core::ptr::write_bytes(dst.add(src.len()), 0, total - src.len());
    }
}

// --- Register geladener Programm-Segmente (ext-26, L4) ---
//
// Geladene PT_LOAD-Segmente + der Stack-Frame sind kernel-ausgeschnittene RAM-Frames, die NICHT
// cap-getrackt sind (der MemoryCap-Deskriptor wird in `load_into_pd` verworfen, da das Eigentum
// an die geladene PD/VSpace uebergeht). Damit sie beim Teardown der VSpace NICHT lecken, werden
// sie hier je `asid` registriert und von [`vspace_teardown`] mitfreigegeben.
const NLOADED_IMG: usize = 16; //   gleichzeitig geladene Programme
const MAX_IMG_SEGS: usize = 8; //   Frames je Programm (Segmente + Stack)
#[derive(Clone, Copy)]
struct LoadedImage {
    asid: u16, // 0 = freier Slot
    nseg: usize,
    segs: [(u64, u64); MAX_IMG_SEGS], // (base, len)
}
impl LoadedImage {
    const EMPTY: LoadedImage = LoadedImage { asid: 0, nseg: 0, segs: [(0, 0); MAX_IMG_SEGS] };
}
static LOADED_IMAGES: SpinLock<[LoadedImage; NLOADED_IMG]> =
    SpinLock::new([LoadedImage::EMPTY; NLOADED_IMG]);

/// Die RAM-Frames `segs` eines geladenen Programms unter `asid` registrieren (für den Teardown).
fn loaded_register(asid: u16, segs: &[(u64, u64)]) {
    let mut t = LOADED_IMAGES.lock();
    if let Some(slot) = t.iter().position(|i| i.asid == 0) {
        let mut img = LoadedImage::EMPTY;
        img.asid = asid;
        for &s in segs.iter().take(MAX_IMG_SEGS) {
            img.segs[img.nseg] = s;
            img.nseg += 1;
        }
        t[slot] = img;
    }
}

/// Die registrierten RAM-Frames der `asid` an den Allokator zurückgeben + den Slot freigeben.
/// Snapshot ziehen, `LOADED_IMAGES` freigeben, DANN `MEM` (Rangordnung: nie beide gleichzeitig).
fn loaded_free(asid: u16) {
    let mut snap = [(0u64, 0u64); MAX_IMG_SEGS];
    let mut n = 0;
    {
        let mut t = LOADED_IMAGES.lock();
        if let Some(slot) = t.iter().position(|i| i.asid == asid && i.asid != 0) {
            n = t[slot].nseg;
            snap[..n].copy_from_slice(&t[slot].segs[..n]);
            t[slot] = LoadedImage::EMPTY;
        }
    }
    if n > 0 {
        let mut mem = MEM.lock();
        for &(base, len) in &snap[..n] {
            mem.free_region(PhysRegion::new(base, len));
        }
    }
}

/// Ein extern geladenes, **validiertes** ELF-Image in eine **vor-erstellte** isolierte PD laden +
/// starten (ext-26, L1c/L3, generischer Binary-Loader). Kopiert die `PT_LOAD`-Segmente an beliebige
/// RAM-Frames und mappt sie **W^X** an ihre Link-VAs ([`hal::mmu::vspace_map_page_at`]), legt einen
/// Stack an, endowt die `endow`-Caps (über `install_cap_checked` — Domänen-Policy + Audits bleiben
/// gültig), spawnt einen EL0-Thread am Entry und **bindet** ihn an `pd`. Die **PD-Erzeugung**
/// (Domäne, ggf. HardwareLand-Partner/Kanal, Autoritäts-Caps) liegt beim Aufrufer — so kann der
/// Loader UserLand (`load_elf`) wie HardwareLand-Backends (vor-erstellt) bedienen. Der Aufbau ab
/// dem Spawn läuft **IRQ-maskiert** (der Thread startet nicht vor Bindung + Endowment). Gibt die
/// `ThreadId`. Bei Fehler wird `pd` NICHT abgebaut (gehört dem Aufrufer).
pub fn load_into_pd(img: &ElfImage, pd: usize, endow: &[(usize, CapPtr)]) -> Option<ThreadId> {
    let core = hal::cpu::core_id();
    let kbase = claim_user_kstack()?; // EL0-Kernel-Stack aus MEM (16 KiB, ausgerichtet)
    let Some((asid, l1)) = create_vspace() else {
        release_user_kstack(kbase);
        return None;
    };
    let Some(l2) = vspace_l2(asid) else {
        vspace_teardown(asid);
        release_user_kstack(kbase);
        return None;
    };
    // `cleanup` gibt die bereits allozierten Segment-/Stack-**Daten**-Frames frei (vor dem
    // erfolgreichen `loaded_register` trackt sie NICHTS -> sonst Leck), baut die VSpace ab (gibt die
    // L1/L2/L3-Tabellen-Frames zurück) und gibt den EL0-Kernel-Stack zurück. Der MEM-Lock wird VOR
    // `vspace_teardown` freigegeben (das sperrt MEM selbst -> sonst Selbst-Deadlock).
    let cleanup = |asid: u16,
                   kbase: usize,
                   segs: &[(u64, u64)],
                   stack: Option<(u64, u64)>|
     -> Option<ThreadId> {
        {
            let mut mem = MEM.lock();
            for &(b, l) in segs {
                if l > 0 {
                    mem.free_region(PhysRegion::new(b, l));
                }
            }
            if let Some((b, l)) = stack {
                mem.free_region(PhysRegion::new(b, l));
            }
        }
        vspace_teardown(asid);
        release_user_kstack(kbase);
        None
    };

    // 1. PT_LOAD-Segmente: kopieren + W^X an Link-VA mappen (nicht-identity, vaddr -> beliebige pa).
    // Die Segment-Frames merken (für den Teardown via loaded_register; NICHT den Stack — der wird
    // beim Thread-Ende via Reap freigegeben).
    // Mehr Segmente als registrierbar (MAX_IMG_SEGS) würden beim Teardown lecken (loaded_register
    // merkt nur die ersten MAX_IMG_SEGS) -> **fail-closed**, bevor irgendetwas alloziert ist.
    if img.segments().count() > MAX_IMG_SEGS {
        return cleanup(asid, kbase, &[], None);
    }
    let mut seglist = [(0u64, 0u64); MAX_IMG_SEGS];
    let mut nrec = 0usize;
    for seg in img.segments() {
        let total = (((seg.memsz as u64) + 4095) & !4095) as usize;
        let Some(region) = mem_alloc(total as u64, 4096) else {
            return cleanup(asid, kbase, &seglist[..nrec], None);
        };
        let pa = region.base(); // MemoryCap-Drop = nur Deskriptor (kein Free); RAM bleibt belegt
        // VOR dem Mappen registrieren -> ein späterer Map-Fehler gibt diesen Frame mit frei.
        // (Die Segmentzahl ist oben auf MAX_IMG_SEGS begrenzt -> kein Überlauf von `seglist`.)
        seglist[nrec] = (pa, total as u64);
        nrec += 1;
        copy_segment(pa, img.segment_bytes(&seg), total);
        let perm = if seg.flags & PF_X != 0 {
            hal::mmu::UserPerm::Rx
        } else if seg.flags & PF_W != 0 {
            hal::mmu::UserPerm::Rw
        } else {
            hal::mmu::UserPerm::Ro
        };
        let mut off = 0u64;
        while (off as usize) < total {
            let mut a3 = || mem_alloc(4096, 4096).map(|c| c.base());
            if !hal::mmu::vspace_map_page_at(l2, seg.vaddr + off, pa + off, perm, &mut a3) {
                return cleanup(asid, kbase, &seglist[..nrec], None);
            }
            off += 4096;
        }
        if seg.flags & PF_X != 0 {
            hal::cpu::sync_code_range(pa as usize, total); // I-Cache kohärent vor der Ausführung
        }
    }

    // 2. Stack (nicht-identity an festes VA-Fenster, EL0-RW).
    let Some(stack_region) = mem_alloc(LOADED_STACK_BYTES, 4096) else {
        return cleanup(asid, kbase, &seglist[..nrec], None);
    };
    let stack_pa = stack_region.base();
    let mut off = 0u64;
    while off < LOADED_STACK_BYTES {
        let mut a3 = || mem_alloc(4096, 4096).map(|c| c.base());
        if !hal::mmu::vspace_map_page_at(l2, LOADED_STACK_VA + off, stack_pa + off, hal::mmu::UserPerm::Rw, &mut a3) {
            // Stack ist alloziert, aber noch NICHT als Reap-Region des Threads vermerkt -> mitfreigeben.
            return cleanup(asid, kbase, &seglist[..nrec], Some((stack_pa, LOADED_STACK_BYTES)));
        }
        off += 4096;
    }
    hal::mmu::flush_asid(asid);

    // 3. PD + Thread + Endowment IRQ-maskiert: der Thread darf nicht vor dem Setup starten.
    // DAIF SICHERN + maskieren (nicht unbedingt freigeben): load_elf laeuft sowohl mit IRQs an
    // (In-Kernel-Test) ALS AUCH im Syscall-Trap (SYS_LOAD, IRQs bereits maskiert) -- der Vorzustand
    // muss erhalten bleiben, sonst gaebe man IRQs mitten im Trap frei.
    let daif = hal::cpu::local_irq_save();
    let tid = {
        let mut sched = SCHEDS[core].lock();
        let r = sched.spawn_user_at(
            core,
            img.entry() as usize,
            0,
            kbase,
            USER_KSTACK_SIZE,
            (LOADED_STACK_VA + LOADED_STACK_BYTES) as usize, // EL0-SP (virtuell)
            stack_pa as usize,                               // Reap-Region (physisch)
            LOADED_STACK_BYTES as usize,
            IDLE_PRIO,
        );
        if let Some(t) = r {
            fp_reset_slot(t.slot());
        }
        r
    };
    let Some(tid) = tid else {
        hal::cpu::local_irq_restore(daif);
        // Spawn fehlgeschlagen -> der Stack ist noch nicht als Reap-Region eines Threads vermerkt;
        // Segmente + Stack mitfreigeben (loaded_register lief noch nicht).
        return cleanup(asid, kbase, &seglist[..nrec], Some((stack_pa, LOADED_STACK_BYTES)));
    };
    record_user_kstack(tid.slot(), kbase);
    set_vspace_of(tid.slot(), ((asid as u64) << 48) | l1); // ab jetzt isoliert
    loaded_register(asid, &seglist[..nrec]); // Segment-Frames fuer den Teardown merken (L4)
    bind_pd(pd, tid);
    for &(slot, cap) in endow {
        // Policy-geprüft (Domänen-Policy bleibt gültig). Lehnt die Policy die Cap ab (false), wurde
        // sie NICHT installiert -> die vom Dispatch erzeugte Kopie loeschen, sonst leckt sie (frisches
        // CDT-Blatt, nicht letzte Referenz -> delete_leaf senkt nur den Refcount). Keine Locks gehalten;
        // unter `daif` bleiben die IRQ-safe Locks maskiert.
        if !install_pd_cap(pd, slot, cap) {
            let _ = cap_delete(cap);
        }
    }
    hal::cpu::local_irq_restore(daif);
    Some(tid)
}

/// Wie [`load_into_pd`], aber **erzeugt** eine frische PD in `domain` (UserLand). Der bequeme Pfad
/// für UserLand-Programme (`SYS_LOAD`, In-Kernel-Tests). HardwareLand-Backends werden vom Aufrufer
/// vor-erstellt (Partner-Bindung + Kanal) und über [`load_into_pd`] geladen. Gibt `(ThreadId, pd)`.
pub fn load_elf(img: &ElfImage, domain: Domain, endow: &[(usize, CapPtr)]) -> Option<(ThreadId, usize)> {
    let pd = create_pd_in_domain(domain)?;
    match load_into_pd(img, pd, endow) {
        Some(tid) => Some((tid, pd)),
        None => {
            // load_into_pd baut `pd` bei Fehler bewusst NICHT ab (gehört dem Aufrufer) -> hier
            // freigeben, sonst leckt der PD-Slot. Zu diesem Zeitpunkt ist die PD weder gebunden
            // noch mit Caps bestückt (Endowment/bind_pd laufen erst nach allen Fehlerpfaden).
            CAPS.write().pds.free(pd);
            None
        }
    }
}

/// Den **akkumulierten Badge** einer Notification lesen, **ohne** ihn zu konsumieren (Test-/
/// Loader-Telemetrie: hat ein geladenes Programm signalisiert?). `0`, falls leer/ungültig.
pub fn notification_pending(ntfn: usize) -> u64 {
    if ntfn < NNOTIFICATIONS {
        NTFNS[ntfn].lock().pending_badge()
    } else {
        0
    }
}

/// Einen blockierten/geparkten Thread `tid` auf **seinem** (ggf. fremden) Kern
/// aufwecken: die Zielinstanz sperren, ihn bereit machen und — falls es ein
/// anderer Kern ist — einen Reschedule-IPI schicken, damit der Zielkern ihn
/// zeitnah einplant. Das ist der Cross-Core-Aufweck-Primitive.
pub fn wake_remote(tid: ThreadId) {
    if let Some((_, c)) = with_owner(tid, |s, _| s.unblock(tid).then_some(())) {
        kick(c);
    }
}

/// Wurde jemals ein Syscall von EL0 (echter User-Thread) ausgeführt?
pub fn el0_syscall_seen() -> bool {
    EL0_SYSCALL_SEEN.load(Ordering::Relaxed)
}

/// Eine Tcb-Capability für einen Thread prägen (cap-kontrolliertes `KILL`).
pub fn install_tcb_cap(tid: ThreadId, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_tcb(tid.to_raw(), rights)
}

/// Eine **Management-Capability** (`PdControl`, ext-22) für die Ziel-PD `pd` prägen: die
/// Autorität, deren Lifecycle via `SYS_PDCTL` zu steuern. Nur eine TrustedSas-PD darf sie
/// nutzen (im Dispatch geprüft); installiert wird sie cap-policy-geprüft (nur in TrustedSas).
pub fn install_pd_control_cap(pd: usize, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_pd_control(pd as u32, rights)
}

/// Eine **MMIO-Capability** (ext-22, HardwareLand) für die Geräte-Registerregion
/// `[phys, phys+len)` prägen — **nur kernelseitig** (es gibt bewusst keinen User-Syscall,
/// der beliebige MMIO-Caps erzeugt). Installiert wird sie cap-policy-geprüft nur in
/// HardwareLand-PDs (`install_cap_checked`).
pub fn install_mmio_cap(phys: u64, len: u64, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_mmio(phys, len, rights)
}

/// Eine **Loader-Capability** (ext-26) prägen: die Autorität, über `SYS_LOAD` ein Programm aus
/// `source` (0 = Boot-Archiv) zu laden. Nur kernelseitig geprägt; cap-policy-geprüft nur in
/// TrustedSas-PDs installierbar (`install_cap_checked`).
pub fn install_loader_cap(source: u32, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_loader(source, rights)
}

/// **Mapping-Art** (Konsolidierung K6) für [`map_region_into_thread`]: bestimmt Tabellen-Level,
/// Adressfenster und Speicher-Attribute der in eine isolierte VSpace eingeblendeten Region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// Geräte-MMIO: Device-nGnRnE, PXN|UXN, nG, in GiB 0 (`vspace_map_device`, L1-Wurzel).
    /// `ro` = schreibgeschützt.
    Device { ro: bool },
    /// DMA-RAM: in GiB 1 (`vspace_map_dma`, L2-Wurzel). `coherent` = Normal-WB (Cache-Maintenance
    /// nötig), sonst Normal-NC (ext-23-Default).
    Dma { coherent: bool },
}

/// Eine autorisierte Region `[phys, phys+len)` **art-spezifisch** in die isolierte VSpace des
/// Threads `tid` einblenden — der **eine** Eintrittspunkt (Konsolidierung K6: ersetzt
/// `map_mmio_into_thread`/`map_dma_into_thread`/`_ex`). [`MappingKind`] wählt Tabellen-Level +
/// Attribute. Generisch (keine geräte-spezifische Annahme); nur für isolierte PDs (ASID != 0 —
/// also Hardware/UserLand; HW-Caps sind ohnehin nur in HardwareLand installierbar). Gibt `false`
/// bei nicht-isolierter VSpace oder fehlgeschlagenem Mapping.
pub fn map_region_into_thread(tid: ThreadId, phys: u64, len: u64, kind: MappingKind) -> bool {
    let asid = (vspace_of(tid.slot()) >> 48) as u16;
    let mut alloc = || mem_alloc(4096, 4096).map(|c| c.base());
    let ok = match kind {
        MappingKind::Device { ro } => match vspace_l1(asid) {
            Some(l1) => hal::mmu::vspace_map_device(l1, phys, len, ro, &mut alloc),
            None => return false, // nicht isoliert / ungültige ASID
        },
        MappingKind::Dma { coherent } => match vspace_l2(asid) {
            Some(l2) => hal::mmu::vspace_map_dma(l2, phys, len, coherent, &mut alloc),
            None => return false,
        },
    };
    if ok {
        hal::mmu::flush_asid(asid);
    }
    ok
}

// --- IRQ-Caps + Deferred-IRQ-Zustellung (ext-22, P5) ---
//
// Ein Geräte-IRQ wird einem HardwareLand-Backend als Notification-Badge zugestellt. Der
// IRQ-Pfad ist **deadlock-frei** gehalten: `irq_hook` läuft im IRQ-Kontext und ist
// LOCK-FREI (nur Atomics + GIC-Maskierung); die eigentliche Zustellung (`drain_pending_irqs`)
// läuft im Reschedule-Pfad (IRQs im Trap maskiert -> kein Reentrancy) und nimmt NTFNS<SCHEDS.
const NIRQ_BIND: usize = 4;
#[allow(clippy::declare_interior_mutable_const)]
static IRQ_INTID: [AtomicU32; NIRQ_BIND] = [const { AtomicU32::new(u32::MAX) }; NIRQ_BIND];
#[allow(clippy::declare_interior_mutable_const)]
static IRQ_NTFN: [AtomicU32; NIRQ_BIND] = [const { AtomicU32::new(0) }; NIRQ_BIND];
#[allow(clippy::declare_interior_mutable_const)]
static IRQ_BADGE: [AtomicU64; NIRQ_BIND] = [const { AtomicU64::new(0) }; NIRQ_BIND];
#[allow(clippy::declare_interior_mutable_const)]
static IRQ_PENDING: [AtomicBool; NIRQ_BIND] = [const { AtomicBool::new(false) }; NIRQ_BIND];
static IRQ_ANY_PENDING: AtomicBool = AtomicBool::new(false);
static IRQ_DELIVERED: AtomicU64 = AtomicU64::new(0); // Telemetrie: zugestellte Geräte-IRQs

/// **Geräte-IRQ-Hook** (aus `exception.rs`, IRQ-Kontext, **LOCK-FREI**): ist `intid`
/// registriert, vermerken (pending) + am Distributor maskieren (kein Re-Trigger), `true`.
/// Sonst `false`. Nimmt KEINEN Lock — die Zustellung erfolgt deferred im Reschedule-Pfad.
fn irq_hook(intid: u32) -> bool {
    for i in 0..NIRQ_BIND {
        if IRQ_INTID[i].load(Ordering::Acquire) == intid {
            hal::intc::mask_intid(intid); // level-getriggerten Geräte-IRQ bis zum Drain sperren
            IRQ_PENDING[i].store(true, Ordering::Release);
            IRQ_ANY_PENDING.store(true, Ordering::Release);
            return true;
        }
    }
    false
}

/// Pending Geräte-IRQs zustellen: je Pending-Slot die gebundene Notification per
/// `signal_from_kernel` signalisieren (Sperrordnung NTFNS<SCHEDS, IRQs maskiert, KEIN
/// SCHEDS-Lock gehalten). Aus dem Reschedule-Pfad VOR dem SCHEDS-Lock. Fast-Check ->
/// Null-Overhead, wenn nichts pending ist.
fn drain_pending_irqs() {
    if !IRQ_ANY_PENDING.swap(false, Ordering::AcqRel) {
        return;
    }
    let mut ops = KernelSched;
    for i in 0..NIRQ_BIND {
        if IRQ_PENDING[i].swap(false, Ordering::AcqRel) {
            let ntfn = IRQ_NTFN[i].load(Ordering::Acquire) as usize;
            let badge = IRQ_BADGE[i].load(Ordering::Acquire);
            if ntfn < NNOTIFICATIONS {
                NTFNS[ntfn].lock().signal_from_kernel(&mut ops, badge);
                IRQ_DELIVERED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Eine **IRQ-Capability** prägen (ext-22, P5) — nur kernelseitig; installiert wird sie
/// cap-policy-geprüft nur in HardwareLand-PDs.
pub fn install_irq_cap(intid: u32, rights: Rights) -> Result<CapPtr, CapError> {
    CAPS.write().cspace.install_irq(intid, rights)
}

/// Einen Geräte-IRQ `intid` an die Notification `ntfn` (mit `badge`) **binden**, an `core`
/// routen und freigeben (ext-22, P5). Nutzt das HardwareLand-Backend (über die IRQ-Cap
/// autorisiert). Gibt `false`, wenn kein Bindungs-Slot frei ist.
pub fn bind_irq(intid: u32, ntfn: usize, badge: u64, core: usize) -> bool {
    for i in 0..NIRQ_BIND {
        if IRQ_INTID[i]
            .compare_exchange(u32::MAX, intid, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            IRQ_NTFN[i].store(ntfn as u32, Ordering::Release);
            IRQ_BADGE[i].store(badge, Ordering::Release);
            // SPI an den Ziel-Kern routen (GICD_ITARGETSR — fehlte bisher; lasttragend,
            // sensitivitaetsgeprueft: ohne dies erreicht der RTC-IRQ keinen Kern).
            hal::intc::route_spi(intid, core);
            hal::intc::enable_intid(intid);
            return true;
        }
    }
    false
}

/// Anzahl bisher zugestellter Geräte-IRQs (Telemetrie für den `irq`-Test).
pub fn irqs_delivered() -> u64 {
    IRQ_DELIVERED.load(Ordering::Acquire)
}

// --- DMA-Capabilities + DmaEnforcer-Abstraktion (ext-23) ---
//
// DMA ist die einzige HW-Cap-Kategorie, bei der ein bus-masterndes Gerät DIREKT Physikspeicher
// liest/schreibt (vorbei an der CPU-MMU). Der **Mechanismus** (DmaCap) ist vom **Enforcement-
// Treiber** entkoppelt: der Kernel erzwingt Ownership/Bounds/Lifetime/Audit hardware-unabhängig;
// ein `DmaEnforcer` setzt die Isolation ZUSÄTZLICH hardwareseitig durch. SMMUv3 ist in ext-23
// die einzige Implementierung (`SmmuV3Enforcer`, D2/D3); ein künftiger `NullIommuEnforcer` o.a.
// implementiert dasselbe Trait, OHNE öffentlichen Code (DmaCap, install_dma_cap, Mapping,
// Treiber, HardwareLand) zu berühren. Die Revoke-Reihenfolge läuft über die Abstraktion:
// `enforcer.detach` -> VSpace-Unmap -> `free_region` (DMA-use-after-free-sicher).

/// Eine generische **DMA-Bindung**: das Gerät mit `stream_id` darf die RAM-`region` als
/// DMA-Puffer nutzen (besessen von `backend_pd`). Enthält bewusst KEINE enforcer-/SMMU-
/// spezifischen Details — die Schnittstelle ist IOMMU-neutral.
#[derive(Clone, Copy)]
pub struct DmaBinding {
    pub stream_id: u32,
    pub region: PhysRegion,
    #[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
    pub backend_pd: usize,
    /// ext-24: Gerät liest nur (read-only -> schreibgeschützt). Default `false` (RW).
    pub ro: bool,
    /// ext-24: Coherent -> Normal-Cacheable. Default `false` (Non-Cacheable, ext-23-Verhalten).
    pub cacheable: bool,
}

impl DmaBinding {
    /// Rückwärtskompatible Bindung (ext-23-Semantik: bidirektional, non-cacheable).
    pub fn new(stream_id: u32, region: PhysRegion) -> Self {
        Self {
            stream_id,
            region,
            backend_pd: 0,
            ro: false,
            cacheable: false,
        }
    }
}

/// Treiber-Abstraktion für die **hardwareseitige DMA-Durchsetzung** (IOMMU-neutral). Die
/// einzige Stelle, die konkrete IOMMU-Register/Tabellen kennt, ist die jeweilige Impl
/// (`SmmuV3Enforcer`). Der öffentliche DMA-Pfad spricht ausschließlich dieses Trait an.
pub trait DmaEnforcer: Sync {
    /// Bring-up des Enforcers (einmalig beim Boot). `true` bei Erfolg / vorhandener HW.
    fn init(&self) -> bool;
    /// Durchsetzung für eine Bindung aktivieren: das Gerät darf danach NUR in `binding.region`
    /// DMAen, alles andere wird hardwareseitig abgewiesen. `false` bei Fehler.
    fn attach(&self, binding: &DmaBinding) -> bool;
    /// Durchsetzung entziehen (VOR VSpace-Unmap + `free_region`): danach kann das Gerät nicht
    /// mehr in die Region DMAen (DMA-use-after-free-sicher).
    fn detach(&self, binding: &DmaBinding);
    /// Durchsetzungs-Oracle: `0` = konsistent, sonst enforcer-spezifischer Anomalie-Code.
    fn audit(&self) -> u32;
    /// Ist die hardwareseitige Durchsetzung aktiv (HW vorhanden + initialisiert)?
    #[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
    fn is_active(&self) -> bool;
    /// **IOMMU-Stream-Gruppe** (ext-24): `member` soll fortan denselben Übersetzungskontext
    /// nutzen wie `leader` (z.B. Multi-Function-Gerät / Bridge ohne RID-Translation). Default:
    /// nicht unterstützt (`false`). `true` bei Erfolg.
    fn share_context(&self, _leader: u32, _member: u32) -> bool {
        false
    }
}

// --- SMMUv3-Enforcer (aarch64 / QEMU `virt`) -------------------------------------------------
//
// ext-31: Der SMMUv3 ist ARM-spezifisch. Das `DmaEnforcer`-Trait war von Anfang an
// IOMMU-neutral entworfen ("ein künftiger `NullIommuEnforcer` implementiert dasselbe Trait,
// OHNE öffentlichen Code zu berühren") — genau das wird hier eingelöst: der SMMU-Treiber ist
// aarch64-only, x86_64 bekommt (bis VT-d/AMD-Vi portiert sind) den Null-Enforcer. Der
// öffentliche DMA-Pfad (DmaCap, install_dma_cap, Mapping, Bounds, Lifetime, Audits) bleibt
// architekturunabhängig.
#[cfg(target_arch = "aarch64")]
mod smmu_enforcer_arm {
    use super::*;
    /// **SMMUv3-Enforcer** — die einzige `DmaEnforcer`-Implementierung in ext-23. Hält die
    /// Physadressen der Command-/Event-Queue + linearen Stream-Tabelle sowie den Command-Queue-
    /// PROD-Index. `init` (D2) bringt die SMMU hoch (Default-Abort); `attach`/`detach`
    /// (D3) programmieren je StreamID eine STE -> CD -> Stage-1-Tabelle. Das gesamte SMMU-Wissen
    /// liegt hier + in `hal::smmu`; der übrige Kernel kennt nur das `DmaEnforcer`-Trait.
    pub struct SmmuV3Enforcer {
        active: AtomicBool,
        strtab_phys: AtomicU64,
        cmdq_phys: AtomicU64,
        eventq_phys: AtomicU64,
        cmdq_prod: AtomicU32,
        sync_ok: AtomicBool, // CMD_SYNC-Round-Trip beim Bring-up gelang (D2-Spike)
    }

    impl SmmuV3Enforcer {
        pub const fn new() -> Self {
            Self {
                active: AtomicBool::new(false),
                strtab_phys: AtomicU64::new(0),
                cmdq_phys: AtomicU64::new(0),
                eventq_phys: AtomicU64::new(0),
                cmdq_prod: AtomicU32::new(0),
                sync_ok: AtomicBool::new(false),
            }
        }

        /// Einen kontiguierlichen, page-ausgerichteten, **genullten** RAM-Block der Größe `len`
        /// ausschneiden (für Queue-/Tabellen-Speicher der SMMU). `None` bei Erschöpfung.
        fn alloc_zeroed(len: u64) -> Option<u64> {
            let len = (len + 4095) & !4095;
            let base = mem_alloc(len, len.next_power_of_two().max(4096)).map(|c| c.base())?;
            // SAFETY: frisch allozierter, identity-gemappter RAM-Block; exklusiv hier beschrieben.
            unsafe { core::ptr::write_bytes(base as *mut u8, 0, len as usize) };
            hal::cpu::dsb_sy();
            Some(base)
        }

        /// CMD_SYNC-Round-Trip beim Bring-up gelungen? (D2-Spike-Telemetrie.)
        pub fn sync_ok(&self) -> bool {
            self.sync_ok.load(Ordering::Acquire)
        }
    }

    impl DmaEnforcer for SmmuV3Enforcer {
        fn init(&self) -> bool {
            if self.active.load(Ordering::Acquire) {
                return true; // idempotent
            }
            if !hal::smmu::present() {
                return false;
            }
            // Command-/Event-Queue + lineare Stream-Tabelle (genullt -> Default-Abort) ausschneiden.
            let Some(strtab) = Self::alloc_zeroed(hal::smmu::strtab_bytes()) else {
                return false;
            };
            let Some(cmdq) = Self::alloc_zeroed(hal::smmu::cmdq_bytes()) else {
                return false;
            };
            let Some(eventq) = Self::alloc_zeroed(hal::smmu::eventq_bytes()) else {
                return false;
            };
            self.strtab_phys.store(strtab, Ordering::Release);
            self.cmdq_phys.store(cmdq, Ordering::Release);
            self.eventq_phys.store(eventq, Ordering::Release);
            if !hal::smmu::bringup(strtab, cmdq, eventq) {
                return false;
            }
            // Spike: CMD_SYNC-Round-Trip beweist die Command-Queue-Mechanik.
            let (prod, ok) = hal::smmu::cmd_sync(cmdq, 0);
            self.cmdq_prod.store(prod, Ordering::Release);
            self.sync_ok.store(ok, Ordering::Release);
            self.active.store(true, Ordering::Release);
            ok
        }
        fn attach(&self, binding: &DmaBinding) -> bool {
            if !self.active.load(Ordering::Acquire) {
                return false;
            }
            let strtab = self.strtab_phys.load(Ordering::Acquire);
            let cmdq = self.cmdq_phys.load(Ordering::Acquire);
            let mut t = DMA_CTX.lock();
            // Kontext finden, der `stream_id` bereits führt; sonst neu anlegen (additiv: mehrere
            // Regionen je Kontext, mehrere StreamIDs je Gruppe).
            let mut ci = t
                .iter()
                .position(|c| c.used && c.sids.contains(&binding.stream_id));
            let is_new_ctx = ci.is_none();
            if ci.is_none() {
                let mut alloc = || SmmuV3Enforcer::alloc_zeroed(4096);
                let Some(l1) = hal::smmu::stage1_create(&mut alloc) else {
                    return false;
                };
                let Some(cd) = SmmuV3Enforcer::alloc_zeroed(hal::smmu::CD_BYTES) else {
                    // Stage-1 (l1) ist bereits alloziert -> freigeben, sonst Frame-Leck.
                    let mut mem = MEM.lock();
                    hal::smmu::free_stage1(l1, &mut |x| {
                        mem.free_region(PhysRegion::new(x, 4096));
                    });
                    return false;
                };
                hal::smmu::write_cd(cd, l1);
                let Some(slot) = t.iter().position(|c| !c.used) else {
                    // l1 + cd sind bereits alloziert (kein DmaCtx-Slot frei) -> beide freigeben.
                    let mut mem = MEM.lock();
                    hal::smmu::free_stage1(l1, &mut |x| {
                        mem.free_region(PhysRegion::new(x, 4096));
                    });
                    mem.free_region(PhysRegion::new(cd, 4096));
                    return false;
                };
                t[slot] = DmaCtx::EMPTY;
                t[slot].used = true;
                t[slot].l1 = l1;
                t[slot].cd = cd;
                t[slot].sids[0] = binding.stream_id;
                ci = Some(slot);
            }
            let slot = ci.unwrap();
            let (l1, cd) = (t[slot].l1, t[slot].cd);
            // Region richtungs-/kohärenz-spezifisch additiv in die Kontext-Stage-1-Tabelle einhängen.
            let mut alloc = || SmmuV3Enforcer::alloc_zeroed(4096);
            if !hal::smmu::stage1_map_region(
                l1,
                binding.region.base,
                binding.region.len,
                binding.ro,
                binding.cacheable,
                &mut alloc,
            ) {
                // Ein frisch angelegter Kontext ist jetzt halb aufgebaut (l1 + cd, keine Region, keine
                // STE) -> abbauen, sonst lecken Stage-1 + CD + der DmaCtx-Slot. Bestehender Kontext:
                // unveraendert lassen (er traegt weiter seine anderen Regionen).
                if is_new_ctx {
                    let mut mem = MEM.lock();
                    hal::smmu::free_stage1(l1, &mut |x| {
                        mem.free_region(PhysRegion::new(x, 4096));
                    });
                    mem.free_region(PhysRegion::new(cd, 4096));
                    drop(mem);
                    t[slot] = DmaCtx::EMPTY;
                }
                return false;
            }
            let Some(ri) = t[slot].regs.iter().position(|&(_, l)| l == 0) else {
                // Region wurde in die Stage-1-Tabelle gemappt, aber es ist kein regs-Slot frei (nur bei
                // einem BESTEHENDEN Kontext moeglich -> dessen regs sind voll; ein neuer Kontext hat
                // leere regs). Das Mapping zuruecknehmen, sonst bleibt es untracked in der Tabelle.
                hal::smmu::stage1_unmap_region(l1, binding.region.base, binding.region.len);
                return false;
            };
            t[slot].regs[ri] = (binding.region.base, binding.region.len);
            // Neuer Kontext: STE installieren. Sonst: nur TLBI+SYNC (STE zeigt schon auf den CD).
            // Bus-Master erst **nach** der Übersetzung scharf schalten (Reihenfolge: erst darf
            // es irgendwo hin, dann darf es überhaupt). Ohne das wäre ein Gerät nach einem
            // vollständigen Detach/Re-Attach-Zyklus tot — beim letzten Detach bleibt Bus-Master
            // bewusst aus, und niemand sonst setzt es wieder.
            hal::pcie::arm_bus_master(binding.stream_id);
            let prod = self.cmdq_prod.load(Ordering::Acquire);
            let (next, ok) = if is_new_ctx {
                let ste = hal::smmu::build_ste_stage1(cd);
                hal::smmu::write_ste_and_sync(strtab, cmdq, prod, binding.stream_id, &ste)
            } else {
                hal::smmu::tlbi_sync(cmdq, prod)
            };
            self.cmdq_prod.store(next, Ordering::Release);
            ok
        }
        fn detach(&self, binding: &DmaBinding) {
            if !self.active.load(Ordering::Acquire) {
                return;
            }
            // --- Gerät stilllegen, BEVOR die Übersetzung verschwindet (ext-35) ---
            //
            // Die Reihenfolge war bisher andersherum: erst unmappen, dann (nie) stilllegen. Das
            // hatte zwei Folgen. Erstens erzeugt ein noch aktives Gerät nach dem Unmap
            // Translation Faults statt Korruption — ungefährlich, aber auf mancher HW ein
            // Fault-Sturm, der echte Fehler verdeckt. Zweitens, und das ist der eigentliche
            // Punkt: das Entfernen der Übersetzung sagt **nichts** über bereits übersetzte,
            // unterwegs befindliche (posted) Writes. Genau die trifft `quiesce_by_rid`:
            // Bus-Master löschen (keine NEUEN Requests) + Config-Read vom Gerät (PCIe: eine
            // Completion überholt posted Writes nicht -> die alten sind danach zugestellt).
            //
            // **Arbeitsteilung, kein Ersatz:** Dieser Schritt deckt das *gutartige* Gerät mit
            // In-flight-Writes ab; gegen ein *kompromittiertes*, das `BME` ignoriert, wirkt
            // allein das Entfernen von Stage-1/STE weiter unten. Keiner der beiden Schritte
            // macht den anderen entbehrlich.
            //
            // **Serialisierung:** Der Zähler in `quiesce_by_rid` macht Verschachtelung sicher
            // (entwaffnen bei 0→1, wiederherstellen bei 1→0) — unabhängig davon, dass dieser Pfad
            // heute ohnehin unter `DMA_CTX` serialisiert ist. Ohne den Zähler könnte ein
            // nebenläufiger Teardown desselben Geräts das Bus-Master wieder scharf schalten,
            // bevor der andere seine Region entfernt hat; dessen Flush-Garantie wäre dann wertlos.
            let strtab = self.strtab_phys.load(Ordering::Acquire);
            let cmdq = self.cmdq_phys.load(Ordering::Acquire);
            let mut t = DMA_CTX.lock();
            hal::pcie::quiesce_by_rid(binding.stream_id);
            let Some(slot) = t
                .iter()
                .position(|c| c.used && c.sids.contains(&binding.stream_id))
            else {
                return;
            };
            let l1 = t[slot].l1;
            // Region aus der Stage-1-Tabelle entfernen (danach kann das Gerät NICHT mehr dorthin
            // DMAen) + TLBI+SYNC. DMA-use-after-free-sicher (vor VSpace-Unmap + free_region).
            hal::smmu::stage1_unmap_region(l1, binding.region.base, binding.region.len);
            if let Some(ri) = t[slot]
                .regs
                .iter()
                .position(|&r| r == (binding.region.base, binding.region.len))
            {
                t[slot].regs[ri] = (0, 0);
            }
            let prod = self.cmdq_prod.load(Ordering::Acquire);
            let (next, _) = hal::smmu::tlbi_sync(cmdq, prod);
            self.cmdq_prod.store(next, Ordering::Release);
            // Bus-Master wiederherstellen, **solange der Kontext noch Regionen trägt** — dann ist
            // das Gerät legitim weiter in Betrieb und die eben entfernte Region ist bereits nicht
            // mehr erreichbar. Hat es keine Region mehr, bleibt BME aus (fail-safe: ein Gerät ohne
            // jede Zuteilung soll auch keine Requests absetzen dürfen).
            // Bus-Master wiederherstellen, **solange der Kontext noch Regionen trägt** — dann ist
            // das Gerät legitim weiter in Betrieb und die eben entfernte Region ist ab dem
            // TLBI+SYNC ohnehin unerreichbar. Hat es keine Region mehr, bleibt Bus-Master **aus**
            // (fail-safe). Die Kopplung an die ATS-Entscheidung steht bei `release_quiesce`.
            let still_in_use = !t[slot].regs.iter().all(|&(_, l)| l == 0);
            hal::pcie::release_quiesce(binding.stream_id, still_in_use);
            // Letzte Region weg -> Kontext abbauen: alle STEs der Gruppe invalidieren, Stage-1 + CD frei.
            if t[slot].regs.iter().all(|&(_, l)| l == 0) {
                let mut p = self.cmdq_prod.load(Ordering::Acquire);
                for k in 0..MAX_CTX_SIDS {
                    let sid = t[slot].sids[k];
                    if sid != u32::MAX {
                        let (n, _) = hal::smmu::clear_ste_and_sync(strtab, cmdq, p, sid);
                        p = n;
                    }
                }
                self.cmdq_prod.store(p, Ordering::Release);
                let (cl1, ccd) = (t[slot].l1, t[slot].cd);
                {
                    let mut mem = MEM.lock();
                    hal::smmu::free_stage1(cl1, &mut |x| {
                        mem.free_region(PhysRegion::new(x, 4096));
                    });
                    mem.free_region(PhysRegion::new(ccd, 4096));
                }
                t[slot] = DmaCtx::EMPTY;
            }
        }
        fn audit(&self) -> u32 {
            if !self.active.load(Ordering::Acquire) {
                return 0; // nicht initialisiert -> keine Durchsetzungs-Aussage (D0/D1)
            }
            // Aktiv: keine globalen Fehler + keine unerwarteten Translation-Faults.
            if hal::smmu::gerror() != 0 {
                return 1;
            }
            if !hal::smmu::eventq_empty() {
                return 2;
            }
            0
        }
        fn is_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }
        fn share_context(&self, leader: u32, member: u32) -> bool {
            if !self.active.load(Ordering::Acquire) {
                return false;
            }
            let strtab = self.strtab_phys.load(Ordering::Acquire);
            let cmdq = self.cmdq_phys.load(Ordering::Acquire);
            let mut t = DMA_CTX.lock();
            let Some(slot) = t.iter().position(|c| c.used && c.sids.contains(&leader)) else {
                return false;
            };
            if t[slot].sids.contains(&member) {
                return true; // schon in der Gruppe
            }
            let Some(si) = t[slot].sids.iter().position(|&s| s == u32::MAX) else {
                return false; // Gruppe voll
            };
            let cd = t[slot].cd;
            let ste = hal::smmu::build_ste_stage1(cd);
            let prod = self.cmdq_prod.load(Ordering::Acquire);
            let (next, ok) = hal::smmu::write_ste_and_sync(strtab, cmdq, prod, member, &ste);
            self.cmdq_prod.store(next, Ordering::Release);
            if ok {
                t[slot].sids[si] = member;
            }
            ok
        }
    }

    
    // DMA-Kontext-Telemetrie (dma_ctx_region_count/sid_count/stage1) liegt in `mod testsupport`
    // (Konsolidierung K5: Test-/Telemetrie-API von der verifizierten Kernschnittstelle getrennt).

}
#[cfg(target_arch = "aarch64")]
pub use smmu_enforcer_arm::SmmuV3Enforcer;

/// **VT-d-Enforcer** (x86_64) — Gegenstück zum SMMUv3-Treiber auf ARM.
///
/// `init` bringt die Remapping-Einheit mit **Default-Block** hoch: Root-Tabelle mit lauter
/// „not present"-Einträgen, dann `SRTP` + `TE`. Ab da ist die Übersetzung aktiv und **jede**
/// nicht ausdrücklich zugeteilte DMA-Anforderung wird von der Hardware abgewiesen — genau die
/// Aussage, die der `smmu`-Test auf ARM prüft.
///
/// `attach` meldet (noch) `false`: die per-Gerät-Zuteilung (Kontext-Einträge +
/// Second-Level-Tabellen je Domäne) fehlt. Auf x86 lässt sich also derzeit **kein** DMA-Puffer
/// an ein Gerät binden — der Zustand ist aber **sicher** (geblockt statt ungeschützt), und das
/// System sagt es, statt eine Isolation vorzutäuschen. Die softwareseitigen Garantien
/// (Ownership, Bounds, Revoke-Reihenfolge, `dma_audit`) sind davon unberührt.
#[cfg(not(target_arch = "aarch64"))]
pub struct VtdEnforcer;

#[cfg(not(target_arch = "aarch64"))]
impl VtdEnforcer {
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(not(target_arch = "aarch64"))]
impl DmaEnforcer for VtdEnforcer {
    fn init(&self) -> bool {
        if !hal::vtd::discover() {
            return false; // Plattform ohne IOMMU
        }
        // Root-Tabelle: ein genulltes Frame = alle 256 Einträge „not present" = Default-Block.
        let Some(root) = mem_alloc(4096, 4096) else {
            return false;
        };
        hal::vtd::init(root.base())
    }
    fn attach(&self, _binding: &DmaBinding) -> bool {
        false // per-Gerät-Zuteilung noch nicht implementiert (s. Typ-Doku)
    }
    fn detach(&self, _binding: &DmaBinding) {}
    fn audit(&self) -> u32 {
        // Ist die Einheit hochgefahren, MUSS die Übersetzung aktiv sein — sonst liefe DMA
        // ungeschützt, obwohl der Kernel meint, sie sei an.
        if hal::vtd::present() && !hal::vtd::enabled() {
            1
        } else {
            0
        }
    }
    fn is_active(&self) -> bool {
        hal::vtd::enabled()
    }
}

// --- SMMU-Übersetzungskontexte (ext-24): je Kontext eine STE-Gruppe -> ein CD -> eine
// Stage-1-Tabelle, die MEHRERE Regionen abbildet. Ersetzt die 1:1-Bindungstabelle aus ext-23.
const NDMA_CTX: usize = 4; //      gleichzeitige Kontexte
const MAX_CTX_SIDS: usize = 4; //  StreamIDs je Kontext (Stream-Gruppe)
const MAX_CTX_REGS: usize = 8; //  Regionen je Kontext (Multi-Region / Scatter-Gather)

#[derive(Clone, Copy)]
struct DmaCtx {
    used: bool,
    l1: u64,                          // Stage-1-Wurzel
    cd: u64,                          // Context Descriptor
    sids: [u32; MAX_CTX_SIDS],        // StreamIDs (u32::MAX = leer)
    regs: [(u64, u64); MAX_CTX_REGS], // (base,len) je Region ((0,0) = leer)
}

impl DmaCtx {
    const EMPTY: DmaCtx = DmaCtx {
        used: false,
        l1: 0,
        cd: 0,
        sids: [u32::MAX; MAX_CTX_SIDS],
        regs: [(0, 0); MAX_CTX_REGS],
    };
}

static DMA_CTX: SpinLock<[DmaCtx; NDMA_CTX]> = SpinLock::new([DmaCtx::EMPTY; NDMA_CTX]);

/// Der globale DMA-Enforcer: auf aarch64 der SMMUv3-Treiber, sonst der Null-Enforcer. Ein
/// Wechsel tauscht nur diese Definition + den Accessor aus, ohne den öffentlichen DMA-Pfad zu
/// ändern (alle Aufrufer gehen über [`dma_enforcer`]).
#[cfg(target_arch = "aarch64")]
static DMA_ENFORCER: SmmuV3Enforcer = SmmuV3Enforcer::new();
#[cfg(not(target_arch = "aarch64"))]
static DMA_ENFORCER: VtdEnforcer = VtdEnforcer::new();

/// Zugriff auf den aktiven DMA-Enforcer (als Trait-Objekt — der öffentliche Pfad ist
/// enforcer-polymorph und SMMU-agnostisch).
pub fn dma_enforcer() -> &'static dyn DmaEnforcer {
    &DMA_ENFORCER
}

/// Den DMA-Enforcer initialisieren (Bring-up; idempotent). Bei SMMUv3: Queues/Stream-Tabelle
/// anlegen, Default-Abort, CR0 aktivieren. Gibt `true` bei Erfolg / aktiver Durchsetzung.
pub fn dma_enforcer_init() -> bool {
    dma_enforcer().init()
}

// SMMU-Diagnose-Accessors (smmu_present/idr0/sid_bits/enabled/sync_ok/eventq_empty/gerror) liegen
// in `mod testsupport` (Konsolidierung K5: SMMU-spezifische Test-/Bericht-Telemetrie getrennt).

/// Eine kontiguierliche **DMA-RAM-Region** (4-KiB-granular, in der mappbaren GiB-1-Region)
/// ausschneiden. `None` bei Erschöpfung oder wenn die Allokation nicht in GiB 1 liegt (dort
/// arbeitet [`hal::mmu::vspace_map_dma`]).
///
/// **Konsolidierung K3:** carvt über die **kanonische** [`KernelRegionSource`] (eine einzige
/// MEM-Carve-Stelle für besitzte Regionen), als `Purpose::Dma` getaggt. Anders als ein Heap-
/// `Region` (das seinen `MemoryCap` besitzt und über die `RegionSource` freigegeben wird) ist das
/// **Besitzmodell** einer DMA-Region bewusst (phys,len)-basiert: die Lebensdauer hängt an der
/// **DmaCap** — die Freigabe erfolgt beim Löschen der Cap (`delete_leaf` -> `free_region`) bzw.
/// über [`free_dma_region`] (roher Pfad), genau einmal. Daher wird der `Region`-Wrapper hier zu
/// einem reinen `MemoryCap`-Deskriptor aufgelöst (`into_cap`, **kein** Drop-Free) und nur die
/// `PhysRegion` weitergereicht. Siehe `docs/invariants.md` §2/§3.
pub fn alloc_dma_region(len: u64) -> Option<PhysRegion> {
    let region = KernelRegionSource.request(len as usize, Purpose::Dma)?;
    let pr = PhysRegion::new(region.phys(), region.len() as u64);
    if pr.base < hal::mmu::USER_RAM_MIN || pr.base + pr.len > hal::mmu::GIB1_END {
        KernelRegionSource.release(region); // nicht in GiB 1 -> zurückgeben (sonst nicht mappbar)
        return None;
    }
    // Eigentum geht an die DmaCap über (Freigabe nach (phys,len), s.o.). Den linearen
    // `MemoryCap` als reinen Deskriptor auflösen — KEIN Drop-Free.
    let _ = region.into_cap();
    Some(pr)
}

/// Eine **DMA-Capability** (ext-23, HardwareLand) über die kernel-ausgeschnittene Region
/// `[phys, phys+len)` prägen — **nur kernelseitig** (kein User-Syscall erzeugt DMA-Caps).
/// Installiert wird sie cap-policy-geprüft nur in HardwareLand-PDs (`install_cap_checked`).
pub fn install_dma_cap(phys: u64, len: u64, rights: Rights) -> Result<CapPtr, CapError> {
    dma_granule_ok(phys, len)?;
    CAPS.write().cspace.install_dma(phys, len, rights)
}

/// **Granularitätsbedingung eines DMA-Puffers** (ext-35).
///
/// Cache-Wartung arbeitet auf ganzen Zeilen: `dc civac` (nach einem Geräte-Write, bzw. bei
/// bidirektionalen Puffern) **verwirft** eine komplette Zeile. Liegt in einer angebrochenen
/// Randzeile fremder Speicher, verliert der seine noch nicht zurückgeschriebenen Daten — ein
/// Datenverlust außerhalb des Puffers, den weder Bounds-Prüfung noch IOMMU sehen (beide
/// betrachten den Puffer, nicht seine Nachbarschaft).
///
/// Die Bedingung wird **einheitlich** geprüft, nicht nur für die Richtungen, bei denen
/// invalidiert wird. Richtungsabhängig wäre ehrlicher gegenüber dem tatsächlich Gefährlichen,
/// aber eine Cap ist langlebig und ihre Richtung ein Feld: eine später erlaubte
/// `Bidirectional`-Nutzung dürfte sonst auf einer Ausrichtung sitzen, die nie dafür geprüft
/// wurde. Einheitlich strikt ist die Bedingung, die man in einem Jahr noch begründen kann.
///
/// Die Granularität liefert die Architektur ([`hal::mmu::dma_granule`]): `CTR_EL0.CWG` auf ARM,
/// `1` auf x86 (hardware-kohärent, keine Wartung) — dort ist die Prüfung damit trivial erfüllt.
fn dma_granule_ok(phys: u64, len: u64) -> Result<(), CapError> {
    let g = hal::mmu::dma_granule();
    if g <= 1 {
        return Ok(()); // kohärente Architektur: keine Wartung, keine Bedingung
    }
    if phys % g == 0 && len % g == 0 {
        Ok(())
    } else {
        Err(CapError::Unaligned)
    }
}

/// Wie [`install_dma_cap`], aber mit expliziter **DMA-Richtung** + **Cache-Kohärenz** (ext-24).
/// Die Cap kodiert damit die volle Autorität; der Enforcer mappt richtungsminimal (Read-Puffer
/// schreibgeschützt) und kohärenz-spezifisch (cacheable vs. non-cacheable).
pub fn install_dma_cap_ex(
    phys: u64,
    len: u64,
    dir: DmaDir,
    coherence: DmaCoherence,
    rights: Rights,
) -> Result<CapPtr, CapError> {
    dma_granule_ok(phys, len)?;
    CAPS.write().cspace.install_dma_ex(phys, len, dir, coherence, rights)
}

/// Die DMA-Attribute (Richtung, Kohärenz) einer DMA-Cap nachschlagen (für den Enforcer/Treiber:
/// richtungs-/kohärenz-spezifisches Mapping). `None`, wenn die Cap keine DMA-Cap ist.
pub fn dma_cap_attrs(cap: CapPtr) -> Option<(u64, u64, DmaDir, DmaCoherence)> {
    match CAPS.read().cspace.lookup(cap)?.0 {
        ObjectKind::Dma {
            phys,
            len,
            dir,
            coherence,
        } => Some((phys, len, dir, coherence)),
        _ => None,
    }
}

/// **DMA-Transfer vorbereiten** (ext-24, Cache-Maintenance, hardware-/geräteunabhängig): vor dem
/// Start eines Transfers in Richtung `dir` die nötige Cache-Wartung auf der (identity-gemappten)
/// Region ausführen. Bei `DeviceRead`/`Bidirectional`: Clean (CPU-Daten sichtbar machen). Bei
/// `DeviceWrite`: nichts (das Gerät schreibt; die CPU invalidiert in `dma_complete`). No-Op-sicher
/// für Non-Coherent-Puffer.
#[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
pub fn dma_prepare(handle: DmaHandle, dir: DmaDir) {
    match dir {
        DmaDir::DeviceRead | DmaDir::Bidirectional => {
            hal::mmu::dma_cache_clean(handle.iova, handle.len)
        }
        DmaDir::DeviceWrite => {}
    }
}

/// **DMA-Transfer abschließen** (ext-24): nach einem Geräte-Write (`DeviceWrite`/`Bidirectional`)
/// die CPU-Cache-Zeilen invalidieren, damit die CPU die vom Gerät geschriebenen Daten frisch liest.
#[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
pub fn dma_complete(handle: DmaHandle, dir: DmaDir) {
    match dir {
        DmaDir::DeviceWrite | DmaDir::Bidirectional => {
            hal::mmu::dma_cache_invalidate(handle.iova, handle.len)
        }
        DmaDir::DeviceRead => {}
    }
}

/// Eine zuvor gemappte DMA-Region wieder aus der VSpace des Threads entmappen (zurück auf
/// EL1-only) + ASID flushen. Teil der Revoke-Reihenfolge.
#[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
pub fn unmap_dma_from_thread(tid: ThreadId, phys: u64, len: u64) -> bool {
    let asid = (vspace_of(tid.slot()) >> 48) as u16;
    let Some(l2) = vspace_l2(asid) else {
        return false;
    };
    let mut p = phys;
    let mut ok = true;
    while p < phys + len {
        ok &= hal::mmu::vspace_unmap_page(l2, p);
        p += 4096;
    }
    hal::mmu::flush_asid(asid);
    ok
}

/// Die hardwareseitige DMA-Durchsetzung für `(stream_id, [base,len))` aktivieren (ext-23-API,
/// rückwärtskompatibel: bidirektional + non-cacheable). `true` bei Erfolg.
pub fn dma_enable(stream_id: u32, base: u64, len: u64) -> bool {
    dma_enforcer().attach(&DmaBinding::new(stream_id, PhysRegion::new(base, len)))
}

/// Die Durchsetzung für `(stream_id, [base,len))` wieder entziehen.
pub fn dma_disable(stream_id: u32, base: u64, len: u64) {
    dma_enforcer().detach(&DmaBinding::new(stream_id, PhysRegion::new(base, len)));
}

/// **Opakes DMA-Handle** (ext-24): die *gerätesichtbare* Adresse (IOVA) + Länge einer
/// angehängten Region. Backends programmieren das Gerät mit `iova` (heute identisch zur PA;
/// die Abstraktion erlaubt später Remap/Bounce/SG-Kompaktierung ohne API-Bruch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaHandle {
    pub iova: u64,
    pub len: u64,
}

/// Eine **DMA-Cap** (ext-24) an den Übersetzungskontext der `stream_id` **anhängen**: liest
/// Richtung + Kohärenz aus der Cap (cap-rein), mappt die Region richtungsminimal/kohärenz-
/// spezifisch in die (ggf. neue) Kontext-Stage-1-Tabelle und gibt ein [`DmaHandle`] zurück.
/// Mehrfaches `dma_attach` mit derselben `stream_id` (verschiedene Caps) hängt mehrere
/// Regionen an **denselben** Kontext (Multi-Region). `None` bei Fehler / falscher Cap.
pub fn dma_attach(stream_id: u32, dcap: CapPtr) -> Option<DmaHandle> {
    let (phys, len, dir, coherence) = dma_cap_attrs(dcap)?;
    let ro = dir == DmaDir::DeviceRead;
    let cacheable = coherence == DmaCoherence::Coherent;
    let ok = dma_enforcer().attach(&DmaBinding {
        stream_id,
        region: PhysRegion::new(phys, len),
        backend_pd: 0,
        ro,
        cacheable,
    });
    if ok {
        Some(DmaHandle { iova: phys, len }) // IOVA = PA (identisch); Abstraktion für später
    } else {
        None
    }
}

/// Eine zuvor per [`dma_attach`] angehängte Region wieder lösen (Stage-1-Eintrag entfernen +
/// TLBI; bei letzter Region des Kontexts: Kontext abbauen). `handle.iova` == PA.
pub fn dma_detach(stream_id: u32, handle: DmaHandle) {
    dma_enforcer().detach(&DmaBinding::new(
        stream_id,
        PhysRegion::new(handle.iova, handle.len),
    ));
}

/// **IOMMU-Stream-Gruppe** (ext-24): `member` nutzt fortan denselben Übersetzungskontext wie
/// `leader` (Multi-Function/SR-IOV/Bridge). `true` bei Erfolg.
pub fn dma_group_add(leader: u32, member: u32) -> bool {
    dma_enforcer().share_context(leader, member)
}

/// Prüfen, ob `[addr, addr+len)` (IOVA) **vollständig in einer angehängten Region** des Kontexts
/// der `stream_id` liegt (Level-1-Software-Disziplin über mehrere Regionen / Scatter-Gather).
fn dma_addr_in_context(stream_id: u32, addr: u64, len: u64) -> bool {
    let t = DMA_CTX.lock();
    t.iter()
        .find(|c| c.used && c.sids.contains(&stream_id))
        .map(|c| c.regs.iter().any(|&(b, l)| region_contains(b, l, addr, len)))
        .unwrap_or(false)
}

/// Ein **Scatter-Gather-Segment** (ext-24): ein Teilbereich `[offset, offset+len)` innerhalb der
/// per `handle` referenzierten DMA-Region. Geräte-Adresse = `handle.iova + offset`. Das
/// gerätespezifische Deskriptorformat (virtio-desc, NVMe-PRP/SGL, NIC-Ring) baut das Backend
/// **aus** validierten Segmenten — die SG-Logik selbst bleibt geräteunabhängig.
#[derive(Clone, Copy)]
pub struct DmaSgEntry {
    pub handle: DmaHandle,
    pub offset: u64,
    pub len: u64,
}

/// **Scatter-Gather-Liste validieren** (ext-24, Level-1): jedes Segment muss (a) innerhalb seines
/// Handles liegen (`offset+len <= handle.len`) und (b) dessen IOVA-Bereich in einer angehängten
/// Region des `stream_id`-Kontexts liegen. Der vertrauenswürdige Treiber MUSS dies vor dem
/// Programmieren einer SG-Anfrage aufrufen; die SMMU ist der Hardware-Backstop. `false`, wenn ein
/// Segment ausserhalb liegt.
pub fn dma_sg_validate(stream_id: u32, entries: &[DmaSgEntry]) -> bool {
    entries.iter().all(|e| {
        e.len > 0
            && e.offset.saturating_add(e.len) <= e.handle.len
            && dma_addr_in_context(stream_id, e.handle.iova + e.offset, e.len)
    })
}

// DmaPool (ext-24, ein Bump-Sub-Allokator über einen DmaHandle) wurde in der Konsolidierung K2
// entfernt: ein DMA-Sub-Puffer ist nur ein Teilbereich `[handle.iova + offset, +len)` einer
// angehängten Region; seine Disjunktheit/Bounds trägt bereits der kanonische Scatter-Gather-/
// Containment-Pfad ([`DmaSgEntry`] + [`dma_sg_validate`] -> [`region_contains`]). Ein Backend, das
// viele kleine Puffer schneidet, führt einen trivialen Offset-Cursor selbst — kein eigener
// öffentlicher Allokatortyp nötig (er duplizierte die Bump-Logik der Region-Runtime/SG).

/// **Kanonisches Region-Containment** (Konsolidierung K4): liegt `[addr, addr+len)` VOLLSTÄNDIG in
/// `[base, base+rlen)`? Die **eine** Grundlage aller DMA-Bounds-Prüfungen — Level-1-Software-
/// Disziplin ([`dma_addr_in_region`]), Multi-Region-Kontext ([`dma_addr_in_context`]) und
/// Scatter-Gather ([`dma_sg_validate`]) bauen alle darauf auf (keine duplizierte Formel mehr).
fn region_contains(base: u64, rlen: u64, addr: u64, len: u64) -> bool {
    len > 0 && addr >= base && addr.saturating_add(len) <= base.saturating_add(rlen)
}

/// **Level-1-Software-Disziplin** (ext-23): prüfen, dass ein Geräte-DMA-Zugriff `[addr, addr+len)`
/// VOLLSTÄNDIG in der DmaCap-Region `[base, base+rlen)` liegt. Der vertrauenswürdige Treiber MUSS
/// dies vor dem Programmieren JEDER Geräte-DMA-Adresse (Deskriptor/Register) aufrufen (backend-
/// direkt, kernel-auditiert). Hardware-unabhängig wirksam. Die SMMU (Level 2) ist der zusätzliche
/// **Hardware-Backstop**, falls ein kompromittiertes Backend diese Prüfung umgeht. Dünner Wrapper
/// um das kanonische [`region_contains`].
pub fn dma_addr_in_region(base: u64, rlen: u64, addr: u64, len: u64) -> bool {
    region_contains(base, rlen, addr, len)
}

/// Ergebnis der virtio-rng-DMA-Demo (ext-23, D4): echter Bus-Master-DMA in die DmaCap-Region +
/// **zweistufiger** Kronjuwel-Test (Level-1-Software-Bounds blockt Out-of-Window demonstrierbar;
/// Level-2-SMMU = Hardware-Backstop, unter QEMU für emulierte Geräte nicht beobachtbar).
#[derive(Clone, Copy, Default)]
pub struct VirtioDmaResult {
    pub found: bool,           // Gerät + virtio-Caps gefunden
    pub used_adv: bool,        // used-Ring fortgeschritten (Gerät hat geantwortet)
    pub written: u32,          // vom Gerät gemeldete Byte-Zahl
    pub rand0: u32,            // erste 4 zufällige Bytes (vom Gerät via DMA geschrieben)
    pub rand1: u32,            // nächste 4
    pub evtq_empty_good: bool, // SMMU-Event-Queue nach dem In-Window-DMA leer
    pub cj_sw_blocked: bool,   // Level 1: Software-Bounds wies den Out-of-Window-Deskriptor ab
    pub cj_sentinel_ok: bool,  // mit Level 1 aktiv: Out-of-Window-Ziel unverändert
    pub cj_unguarded_wrote: bool, // Sensitivität: OHNE die Prüfung schrieb das Gerät -> Prüfung lasttragend
    pub cj_smmu_enforced: bool, // Level 2: faultete die SMMU den ungeschützten Zugriff? (QEMU: false)
    pub audit_ok: bool,        // dma_audit==0 nach dem Aufräumen
}

#[cfg(target_arch = "aarch64")] // virtio-rng-PCI + SMMU: ARM-/QEMU-`virt`-spezifisch (ext-31)
#[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
/// **virtio-rng-DMA End-to-End** (ext-23, D4): das Gerät DMAt Zufallsbytes in die DmaCap-Region
/// (echter Bus-Master-DMA). Danach der **zweistufige Kronjuwel-Test**:
/// - **Level 1 (Software, demonstrierbar):** der vertrauenswürdige Treiber validiert jede
///   Deskriptor-Adresse via [`dma_addr_in_region`]; eine Out-of-Window-Adresse wird abgewiesen ->
///   das Gerät wird gar nicht erst programmiert -> das Ziel bleibt unverändert.
/// - **Sensitivität:** wird die Prüfung umgangen und der Out-of-Window-Deskriptor doch ausgegeben,
///   schreibt das Gerät das Ziel (Prüfung ist lasttragend). Auf realer HW würde hier die SMMU
///   (Level 2) faulten; QEMU übersetzt emulierte Geräte nicht durch die SMMU -> `cj_smmu_enforced`
///   ist unter QEMU `false` (ehrliche Telemetrie). Die Stage-1-STE ist dennoch installiert + greift
///   auf realer Hardware.
pub fn virtio_rng_dma_demo() -> VirtioDmaResult {
    let mut r = VirtioDmaResult::default();
    let Some(dev) = virtio_device() else {
        return r;
    };
    r.found = true;
    let rid = dev.rid();
    // DMA-Region (Virtqueue + Datenpuffer) ausschneiden + an die Geräte-StreamID binden (Level 2).
    let Some(region) = alloc_dma_region(0x4000) else {
        return r;
    };
    let base = region.base;
    let len = region.len;
    if !dma_enable(rid, base, len) {
        free_raw_region(base, len);
        return r;
    }
    hal::smmu::drain_eventq(); // sauberer Ausgangsstand
    let dlen = hal::virtio::DATA_LEN_BYTES as u64;

    // 1. In-Window: Treiber validiert die Zieladresse (Level 1, ok) -> Gerät DMAt Zufallsbytes.
    let data = base + hal::virtio::DATA_OFFSET;
    if dma_addr_in_region(base, len, data, dlen) {
        if let Some(rng) = hal::virtio::probe(&dev) {
            // SAFETY: kernel-/Trusted-seitiger virtio-Treiber; BAR ist global EL1-Device-gemappt,
            // die Virtqueue + der Datenpuffer liegen in der (identity-gemappten) DMA-Region.
            let (adv, wlen) = unsafe { rng.request(base, data) };
            r.used_adv = adv;
            r.written = wlen;
            let (w0, w1) = testsupport::peek_dma_words(data);
            r.rand0 = w0;
            r.rand1 = w1;
        }
    }
    r.evtq_empty_good = hal::smmu::eventq_empty();

    // 2. Kronjuwel (Sentinel-Page AUSSERHALB der DmaCap-Region). MEM-Lock VOR dem `if let`
    // freigeben (sonst Deadlock über das `if let`-Temporary -> free_raw_region re-lockt MEM).
    let sentinel_page = mem_alloc(4096, 4096).map(|c| c.base());
    if let Some(sent) = sentinel_page {
        const SENTINEL: u64 = 0xA5A5_A5A5_5A5A_5A5A;
        // SAFETY: frische, identity-gemappte RAM-Page; exklusiv hier beschrieben/gelesen.
        unsafe { core::ptr::write_volatile(sent as *mut u64, SENTINEL) };
        hal::cpu::dsb_sy();

        // 2a. Level 1: der Treiber validiert die Out-of-Window-Adresse -> ABGEWIESEN, kein Notify.
        r.cj_sw_blocked = !dma_addr_in_region(base, len, sent, dlen);
        // SAFETY: nur Lesezugriff. Da nicht programmiert, muss das Ziel unverändert sein.
        let after_guard = unsafe { core::ptr::read_volatile(sent as *const u64) };
        r.cj_sentinel_ok = after_guard == SENTINEL;

        // 2b. Sensitivität: Prüfung umgehen + Out-of-Window doch ausgeben. Beweist, dass die
        // Software-Prüfung lasttragend ist; testet zugleich den SMMU-Backstop (QEMU: nicht
        // beobachtbar -> Gerät schreibt; reale HW: SMMU faultet).
        hal::smmu::drain_eventq();
        if let Some(rng) = hal::virtio::probe(&dev) {
            let _ = unsafe { rng.request(base, sent) };
        }
        // SAFETY: s.o.
        let after_unguarded = unsafe { core::ptr::read_volatile(sent as *const u64) };
        r.cj_unguarded_wrote = after_unguarded != SENTINEL; // Prüfung war lasttragend
        r.cj_smmu_enforced = !hal::smmu::eventq_empty(); //  SMMU-Fault? (QEMU: false)
        hal::smmu::drain_eventq();
        free_raw_region(sent, 4096);
    }

    // 3. Aufräumen: Durchsetzung entziehen (STE invalidieren) + DMA-Region freigeben.
    dma_disable(rid, base, len);
    free_raw_region(base, len);
    r.audit_ok = dma_audit() == 0;
    r
}

/// Eine (noch nicht in eine DmaCap überführte) DMA-Region an den Allokator zurückgeben — für
/// Fehler-/Cleanup-Pfade, in denen eine `alloc_dma_region` nicht in eine Cap mündet.
#[cfg_attr(not(feature = "kernel-fuzz"), allow(dead_code))] // nur vom Fuzzer benutzt (ADR 0013)
pub fn free_dma_region(base: u64, len: u64) {
    free_raw_region(base, len);
}

// --- Prozess-Heap-Regionsquelle (ext-25) ---
//
// Der kernel-/Trusted-SAS-seitige `RegionSource`: bedient grow/shrink des prozess-lokalen
// Heaps direkt aus dem physischen Allokator (`MEM`). Ein EL0-Prozess würde dasselbe per Syscall
// marshallen — die `RegionSource`-Schnittstelle bleibt identisch (IOMMU-/Heap-neutral). Die
// Region trägt eine `MemoryCap` (lineares Eigentum); `release` gibt sie über `into_cap` zurück.
static REGION_ID: AtomicU32 = AtomicU32::new(1);

pub struct KernelRegionSource;

impl RegionSource for KernelRegionSource {
    fn request(&self, min_len: usize, purpose: Purpose) -> Option<Region> {
        let len = (min_len as u64 + 4095) & !4095;
        let cap = mem_alloc(len, 4096)?;
        let id = REGION_ID.fetch_add(1, Ordering::Relaxed);
        Some(Region::from_cap(cap, RegionTag::new(id, purpose)))
    }
    fn release(&self, region: Region) {
        MEM.lock().free(region.into_cap());
    }
}

// --- Hot-Reload-Zustand als Region (Konsolidierung O-B) ---
//
// Der zustandsbehaftete Hot-Reload-Test (Zähler-Service v1 -> v2) hielt seinen Zustand bisher in
// einer **roh** per `peek_u64`/`poke_u64` angesprochenen RAM-Adresse (`CS_STATE_BASE`). Er lebt nun
// in einer `Region` (`Purpose::HotReloadState`) und wird über die **sichere** RegionView-API
// gelesen/geschrieben — dasselbe Substrat wie der Prozess-Heap (ext-25), kein rohes `unsafe` im
// Testpfad. Beide Komponenten-Versionen (v1, v2) teilen DIESELBE Region (zero-copy; der Zustand
// überlebt den Tausch — genau die Hot-Reload-Invariante). Siehe ADR 0010.
static CS_STATE_REGION: SpinLock<Option<Region>> = SpinLock::new(None);

/// Die Hot-Reload-Zustandsregion (genullt) anlegen + global halten. Gibt die Phys-Basis zurück
/// (nur Telemetrie; der Zugriff läuft über [`hotreload_state_get`]/[`hotreload_state_set`]).
pub fn hotreload_state_alloc() -> Option<u64> {
    let region = KernelRegionSource.request(4096, Purpose::HotReloadState)?;
    let phys = region.phys();
    *CS_STATE_REGION.lock() = Some(region); // Lock-Guard fällt am `;` -> set() unten re-lockt sauber
    hotreload_state_set(0); // Zähler initialisieren (carve liefert nicht garantiert genullt)
    Some(phys)
}

/// Das erste `u64`-Wort der Hot-Reload-Zustandsregion über die **sichere** RegionView-API lesen.
pub fn hotreload_state_get() -> u64 {
    CS_STATE_REGION
        .lock()
        .as_mut()
        .and_then(|r| r.view().get::<u64>(0))
        .unwrap_or(0)
}

/// Das erste `u64`-Wort der Hot-Reload-Zustandsregion über die **sichere** RegionView-API schreiben.
pub fn hotreload_state_set(val: u64) {
    if let Some(r) = CS_STATE_REGION.lock().as_mut() {
        r.view().set::<u64>(0, val);
    }
}

/// Eine roh-allozierte RAM-Region (ohne Cap) an den Allokator zurückgeben (interner Test-/
/// Setup-Helfer für temporäre DMA-/Sentinel-Regionen). 4-KiB-granular.
fn free_raw_region(base: u64, len: u64) {
    let len = (len + 4095) & !4095;
    MEM.lock().free_region(PhysRegion::new(base, len));
}

/// Eine DMA-Bindung **sicher abbauen** (Revoke-Reihenfolge, ext-23, DMA-use-after-free-sicher):
/// (1) `enforcer.detach` (hardwareseitige Durchsetzung entziehen — SMMU-Invalidierung),
/// dann (2) aus der Backend-VSpace unmappen. Erst danach darf der Aufrufer die DmaCap löschen
/// (`delete_leaf` -> `free_region`). Nach Schritt 1 kann kein Gerät mehr in die Region DMAen.
#[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
pub fn revoke_dma(binding: &DmaBinding, tid: ThreadId) {
    dma_enforcer().detach(binding);
    unmap_dma_from_thread(tid, binding.region.base, binding.region.len);
}

/// **DMA-Policy-Oracle** (ext-23): `0` = konsistent, sonst Anomalie-Code:
/// - `1` = DmaCap-Bounds verletzt (nicht ausgerichtet/leer/außerhalb GiB-1-Fenster bzw.
///   überlappt das Kernel-Image — `floor` = Kernel-Image-Ende).
/// - `2` = zwei DmaCap-Regionen überlappen einander.
/// - `3` = der Enforcer meldet eine Durchsetzungs-Anomalie (`dma_enforcer().audit()`).
/// Wird in [`ipc_audit`] als Code `40 + dma_audit()` aggregiert.
pub fn dma_audit() -> u32 {
    let floor = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    let ceil = hal::mmu::GIB1_END;
    let code = CAPS.read().dma_bounds_audit(floor, ceil);
    if code != 0 {
        return code;
    }
    if dma_enforcer().audit() != 0 {
        return 3;
    }
    // Revoke-Ordnung-Invariante (docs/invariants.md §2, DMA-use-after-free-sicher): eine noch in
    // einem SMMU-Kontext gemappte Region darf NIE freigegeben sein. Wäre sie freigegeben (free
    // VOR detach), läge sie in der Free-Liste und überlappte sie -> Code 4. Funktioniert für
    // den cap-basierten (dma_attach) UND den rohen (dma_enable) Pfad. Snapshot von DMA_CTX ziehen
    // + Lock freigeben, DANN MEM prüfen (Rangordnung R1 vor R4, nie gleichzeitig gehalten).
    if !dma_ctx_regions_live() {
        return 4;
    }
    0
}

/// Hilfsprüfung für [`dma_audit`] Code 4: keine aktuell in einem `DMA_CTX` gemappte Region
/// überlappt freies RAM (sonst wurde sie freigegeben, während die SMMU-Stage-1 noch darauf zeigte).
/// Snapshot der Kontext-Regionen (DMA_CTX kurz sperren, kopieren, freigeben), danach gegen
/// `MEM.overlaps_free` — die beiden Locks werden NIE gleichzeitig gehalten (R1 vor R4).
fn dma_ctx_regions_live() -> bool {
    let mut snap = [(0u64, 0u64); NDMA_CTX * MAX_CTX_REGS];
    let mut n = 0;
    {
        let t = DMA_CTX.lock();
        for c in t.iter() {
            if c.used {
                for &(b, l) in c.regs.iter() {
                    if l != 0 {
                        snap[n] = (b, l);
                        n += 1;
                    }
                }
            }
        }
    } // DMA_CTX freigegeben
    let mem = MEM.lock();
    !snap[..n].iter().any(|&(b, l)| mem.overlaps_free(b, l))
}

/// **Test-/Telemetrie-API** (Konsolidierung K5) — bewusst von der verifizierten Kernschnittstelle
/// getrennt. Diese Funktionen werden NUR vom Selbsttest/Bericht (`threads.rs`) gelesen; sie tragen
/// keine Sicherheitsinvariante und sind nicht Teil der zu verifizierenden DMA-Kern-API. `use
/// super::*` bringt die (für Kindmodule sichtbaren) privaten Statics/Imports von `system` in Scope.
pub(crate) mod testsupport {
    use super::*;

    /// Anzahl Regionen im Kontext der StreamID. `0`, wenn kein Kontext.
    pub fn dma_ctx_region_count(stream_id: u32) -> usize {
        let t = DMA_CTX.lock();
        t.iter()
            .find(|c| c.used && c.sids.contains(&stream_id))
            .map(|c| c.regs.iter().filter(|&&(_, l)| l != 0).count())
            .unwrap_or(0)
    }

    /// Anzahl StreamIDs im Kontext der StreamID (Stream-Gruppen-Telemetrie).
    pub fn dma_ctx_sid_count(stream_id: u32) -> usize {
        let t = DMA_CTX.lock();
        t.iter()
            .find(|c| c.used && c.sids.contains(&stream_id))
            .map(|c| c.sids.iter().filter(|&&s| s != u32::MAX).count())
            .unwrap_or(0)
    }

    /// Die Stage-1-Wurzel des Kontexts der StreamID (struktureller Leaf-Test). `0` = keiner.
    pub fn dma_ctx_stage1(stream_id: u32) -> u64 {
        let t = DMA_CTX.lock();
        t.iter()
            .find(|c| c.used && c.sids.contains(&stream_id))
            .map(|c| c.l1)
            .unwrap_or(0)
    }

    #[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
    // --- SMMU-Diagnose-Accessors (ext-23, D2; SMMU-spezifisch, nur für den `smmu`-Test/Bericht) ---
    // ext-31: Die `smmu_*`-Accessors sind aarch64-only (auf x86 wären es VT-d/AMD-Vi-Register
    // — noch nicht portiert); die übrigen Helfer hier sind architekturunabhängig.
    pub fn smmu_present() -> bool {
        hal::smmu::present()
    }
    #[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
    pub fn smmu_idr0() -> u32 {
        hal::smmu::idr0()
    }
    #[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
    pub fn smmu_sid_bits() -> u32 {
        hal::smmu::sid_bits()
    }
    #[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
    pub fn smmu_enabled() -> bool {
        hal::smmu::enabled()
    }
    #[cfg(target_arch = "aarch64")] // SMMU-spezifischer Enforcer-Accessor (ext-31)
    pub fn smmu_sync_ok() -> bool {
        DMA_ENFORCER.sync_ok()
    }
    #[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
    pub fn smmu_eventq_empty() -> bool {
        hal::smmu::eventq_empty()
    }
    #[cfg(target_arch = "aarch64")] // SMMU/virtio: ARM-spezifisch (ext-31)
    pub fn smmu_gerror() -> u32 {
        hal::smmu::gerror()
    }

    /// Wie [`super::dma_audit`], aber mit explizit gewähltem `floor` (Bounds-Sensitivitätstest:
    /// ein zu hoher `floor` muss eine legitime Region als Out-of-Window melden).
    pub fn dma_audit_with_floor(floor: u64) -> u32 {
        CAPS.read().dma_bounds_audit(floor, hal::mmu::GIB1_END)
    }

    /// Die ersten beiden 32-bit-Worte einer (RAM-)DMA-Region über die **globale Identity-Map**
    /// lesen (der Kernel sieht alles RAM EL1-RW). Für den Kohärenz-Check: sieht der Kernel dieselben
    /// Bytes, die das Backend über seine EL0-Non-Cacheable-Abbildung geschrieben hat?
    pub fn peek_dma_words(phys: u64) -> (u32, u32) {
        // SAFETY: `phys` ist eine kernel-ausgeschnittene RAM-DMA-Region, in der globalen SAS-Map
        // identity-gemappt und gültig; nur lesender Zugriff auf die ersten 8 Bytes.
        unsafe {
            let p = phys as *const u32;
            (
                core::ptr::read_volatile(p),
                core::ptr::read_volatile(p.add(1)),
            )
        }
    }
}

#[cfg(target_arch = "aarch64")] // PCIe-ECAM des QEMU-`virt`-Boards (ext-31)
mod pcie_arm {
    use super::*;
    // --- PCIe-Enumeration (ext-23, D1; kernel-/Trusted-Setup) ---

    /// Das DMA-Beweisgerät (`virtio-rng-pci`) per ECAM finden + einrichten: ECAM **global** als
    /// EL1-Device mappen (jenseits der statischen GiB 0..8), Bus 0 nach Vendor `0x1af4` scannen,
    /// BARs dimensionieren+zuweisen, Memory-Space + **Bus-Master** aktivieren. Gibt das Gerät
    /// (inkl. RID = SMMU-StreamID) zurück. Reines kernel-/Trusted-Setup — kein User-Pfad.
    pub fn pcie_find_virtio() -> Option<hal::pcie::PciDevice> {
        hal::mmu::map_device_block_global(hal::pcie::ECAM_GIB);
        // Gezielt die virtio-RNG (nicht eine evtl. vorhandene Default-NIC, ebenfalls Vendor 0x1af4).
        let d = hal::pcie::find(hal::pcie::VIRTIO_VENDOR, &hal::pcie::VIRTIO_RNG_DEVICES);
        *VIRTIO_PCI.lock() = d; // für D4 (virtio-Treiber) cachen
        d
    }

    /// Das in [`pcie_find_virtio`] gefundene + eingerichtete virtio-RNG-Gerät (gecacht).
    static VIRTIO_PCI: SpinLock<Option<hal::pcie::PciDevice>> = SpinLock::new(None);
    pub fn virtio_device() -> Option<hal::pcie::PciDevice> {
        *VIRTIO_PCI.lock()
    }

    // dma_audit_with_floor + peek_dma_words liegen in `mod testsupport`
    // (Konsolidierung K5: Bounds-Sensitivitätstest + Kohärenz-Peek von der Kernschnittstelle getrennt).


}
#[cfg(target_arch = "aarch64")]
pub use pcie_arm::*;

// --- MCS Scheduling Contexts (Budget-basiertes Scheduling) ---
//
// CPU-Zeit wird **kapabilitätskontrolliert** vergeben: ein Scheduling-Context-Objekt
// (Budget/Periode) existiert nur als Cap im CapSpace. Erst die Vorlage einer gültigen
// SchedContext-Cap mit WRITE-Recht autorisiert, einem Thread dieses Budget zuzuweisen
// (`bind_sched_context`). Das Budget wird aus dem **Cap-Objekt** gelesen, nicht aus
// einem freien Argument — die Cap *ist* die Autorität (vgl. seL4 `SchedContext_Bind`).

/// Eine **Scheduling-Context-Capability** prägen: `budget` Ticks je `period` Ticks.
pub fn install_sched_context_cap(
    budget: u32,
    period: u32,
    rights: Rights,
) -> Result<CapPtr, CapError> {
    CAPS.write()
        .cspace
        .install_sched_context(budget, period, rights)
}

/// Einen Scheduling Context an einen Thread **binden**: die Cap `sc` auflösen, prüfen
/// dass sie ein `SchedContext` mit WRITE-Recht ist, und das darin gespeicherte Budget
/// auf dem Scheduler von `core` für `tid` setzen. Ohne gültige Cap keine Budget-
/// Autorität (gibt `false` zurück). Lock-Ordnung: CAPS vor SCHEDS[core].
pub fn bind_sched_context(sc: CapPtr, core: usize, tid: ThreadId) -> bool {
    let (budget, period) = {
        let caps = CAPS.read();
        match caps.cspace.lookup(sc) {
            Some((ObjectKind::SchedContext { budget, period }, rights, _))
                if rights.contains(Rights::WRITE) =>
            {
                (budget, period)
            }
            _ => return false, // keine (gültige) SchedContext-Cap -> keine Autorität
        }
    }; // CAPS vor SCHEDS freigegeben
    SCHEDS[core].lock().set_budget(tid, budget, period)
}

/// MCS-Telemetrie eines Kerns: `(Budget-Erschöpfungen, Refills)`. Für Tests.
pub fn budget_stats(core: usize) -> (u64, u64) {
    SCHEDS[core].lock().budget_stats()
}

/// Einen **nicht laufenden** Thread des **aktuellen** Kerns direkt beenden — dieselbe
/// Mechanik wie der cap-kontrollierte `KILL`-Syscall (`KernelSched::kill`), nur ohne
/// Cap-Vorlage (für kernelinterne Test-/Wartungspfade). Gibt `true` bei Erfolg. `kill`
/// ist kern-lokal: `tid` muss auf dem aufrufenden Kern liegen und darf nicht laufen.
pub fn kill_local(tid: ThreadId) -> bool {
    let core = hal::cpu::core_id();
    if sel4lake_sched::owner_core(tid) != Some(core) {
        return false;
    }
    let ok = SCHEDS[core].lock().kill(tid, core);
    if ok {
        purge_ipc_queues(tid); // eager: tote IPC-Queue-Einträge vermeiden
        reclaim_user_kstack(tid.slot()); // falls EL0-Thread: Pool-Slot zurück
    }
    ok
}

/// Beendete Threads **dieses Kerns** einsammeln: TCB-Slots freigeben und Stacks an
/// den Allokator zurückgeben (aus dem Idle-Thread). Erst die Zombies unter
/// `SCHEDS[core]` einsammeln, dann den Lock **freigeben** und unter `MEM` freigeben
/// — nie SCHEDS und MEM gleichzeitig halten (sonst Ordnungsinversion zu `spawn`).
pub fn reap() -> usize {
    reap_core(hal::cpu::core_id())
}

/// Beendete Threads des Kerns `core` einsammeln — auch **kern-übergreifend** aufrufbar
/// (z. B. der Fuzzer-Controller auf core 0 reapt Zombies belasteter Kerne, deren Idle
/// nie läuft -> sonst lecken die Stacks). Sperrordnung: erst `SCHEDS[core]` (Zombies in
/// einen Puffer), freigeben, dann `MEM` — nie beide gleichzeitig.
pub fn reap_core(core: usize) -> usize {
    // (base, len, gid) je Zombie: Stack-Region an MEM, `gid` an die Thread-Freiliste —
    // beides erst NACH dem Freigeben von SCHEDS (Sperrordnung).
    let mut zombies: [(usize, usize, u32); 8] = [(0, 0, 0); 8];
    let mut n = 0;
    {
        let mut sched = SCHEDS[core].lock();
        while n < zombies.len() {
            match sched.reap() {
                Some(z) => {
                    zombies[n] = z;
                    n += 1;
                }
                None => break,
            }
        }
    } // SCHEDS freigegeben
    if n > 0 {
        {
            let mut mem = MEM.lock();
            let mut bytes = 0u64;
            for &(base, len, _) in &zombies[..n] {
                if len > 0 {
                    mem.free_region(PhysRegion::new(base as u64, len as u64));
                    bytes += len as u64;
                }
            }
            REAPED_BYTES.fetch_add(bytes, Ordering::Relaxed);
        } // MEM freigegeben
        // Thread-Slots (`gid`) zurückgeben — erst jetzt kann eine `gid` neu vergeben werden.
        for &(_, _, gid) in &zombies[..n] {
            sel4lake_sched::release_gid(gid);
        }
    }
    n
}

/// Einen **nicht laufenden** Thread auf **irgendeinem** Kern beenden (für den Fuzzer-
/// Controller, der Aktoren auf anderen Kernen zu ungünstigen Zeiten killt). Schlägt
/// fehl (`false`), wenn der Thread gerade auf seinem Kern *läuft* (dann erneut
/// versuchen, sobald er blockiert/verdrängt ist). Entfernt ihn eager aus allen
/// IPC-Queues und weckt den Zielkern (IPI), damit er bald reapt.
pub fn kill_remote(tid: ThreadId) -> bool {
    let ok = with_owner(tid, |s, c| s.kill(tid, c).then_some(())).is_some();
    if ok {
        purge_ipc_queues(tid);
        reclaim_user_kstack(tid.slot());
        // Der Zielkern soll bald reapen; er kann inzwischen ein anderer sein — der IPI geht
        // an den Kern, auf dem der Kill tatsächlich stattfand.
        if let Some(c) = sel4lake_sched::owner_core(tid) {
            kick(c);
        }
    }
    ok
}

/// Summe der per [`reap`] an den Allokator zurückgegebenen Stack-Bytes (monoton) —
/// belegt, dass beendete Threads ihren Stack zurückgeben (robust gegen anderweitige
/// Allokationen, anders als ein absoluter `total_free`-Vergleich).
pub fn reaped_bytes() -> u64 {
    REAPED_BYTES.load(Ordering::Relaxed)
}

pub fn create_pd() -> Option<usize> {
    CAPS.write().pds.create()
}
/// Eine PD in einer bestimmten **Sicherheitsdomäne** anlegen (Domäne danach unveränderlich).
pub fn create_pd_in_domain(domain: Domain) -> Option<usize> {
    CAPS.write().pds.create_in_domain(domain)
}
/// Die (unveränderliche) Domäne einer PD.
pub fn pd_domain(pd: usize) -> Option<Domain> {
    CAPS.read().pds.domain_of(pd)
}
/// Ein **HardwareLand-Backend** anlegen (ext-22, P3): erzeugt einen dedizierten Endpoint +
/// eine Notification und eine HardwareLand-PD mit **unveränderlicher** Partner-Bindung an
/// `trusted_pd` (TrustedSas) und genau diesem Kanal. Gibt `(backend_pd, ep, ntfn)` zurück;
/// der Aufrufer prägt + installiert die Kanal-Caps (Backend: recv/signal — policy-geprüft auf
/// genau diesen Kanal; Trusted-Partner: send/wait). 1:N (mehrere Backends je Trusted) erlaubt.
pub fn create_hardware_backend(
    trusted_pd: usize,
    backend_id: u16,
) -> Option<(usize, usize, usize)> {
    let ep = create_endpoint()?;
    let Some(ntfn) = create_notification() else {
        // `ep` ist frisch reserviert + noch unreferenziert (keine Cap, kein Waiter) -> Slot
        // zuruecknehmen, sonst leckt der Endpoint-Slot.
        *EPS[ep].lock() = Endpoint::EMPTY;
        return None;
    };
    let backend_pd = match CAPS.write().pds.create_hardware_backend(
        trusted_pd,
        backend_id,
        ep as u32,
        ntfn as u32,
    ) {
        Some(pd) => pd,
        None => {
            // ep + ntfn sind noch unreferenziert -> beide Slots zuruecknehmen.
            *EPS[ep].lock() = Endpoint::EMPTY;
            *NTFNS[ntfn].lock() = Notification::EMPTY;
            return None;
        }
    };
    Some((backend_pd, ep, ntfn))
}
pub fn bind_pd(pd: usize, tid: ThreadId) {
    CAPS.write().pds.bind_thread(pd, tid);
}
/// Cap **policy-geprüft** in eine PD eintragen (Hardware-Caps nur HardwareLand, `PdControl`
/// nur TrustedSas). Gibt `false` zurück, wenn die Domänen-Policy es verbietet (kein Eintrag).
pub fn install_pd_cap(pd: usize, slot: usize, cap: CapPtr) -> bool {
    CAPS.write().install_cap_checked(pd, slot, cap)
}
pub fn clear_pd_cap(pd: usize, slot: usize) {
    CAPS.write().pds.clear_cap(pd, slot);
}

/// Einen blockierten Endpoint-Empfänger zurückziehen (Hot-Reload).
pub fn endpoint_retire_receiver(ep: usize, tid: ThreadId) -> bool {
    if ep < NENDPOINTS {
        EPS[ep].lock().retire_receiver(tid)
    } else {
        false
    }
}

/// **Reply-Liveness beim Quiescen** (Hot-Reload/Revocation OHNE Thread-Tod): wird der
/// Server `tid` als Reply-Owner von Endpoint `ep` zurückgezogen (z. B. seine Recv-Cap
/// entzogen / durch v2 ersetzt), während ein `caller` noch auf die Antwort wartet, wird
/// dieser Caller mit `ERR_SERVER_GONE` entblockt — sonst hinge er, weil der (lebende,
/// aber capless/ersetzte) Server nie mehr antwortet. Gibt `true`, falls ein Caller
/// entblockt wurde. Sperrordnung EPS vor SCHEDS (in `unblock_with_error`).
pub fn endpoint_quiesce_owner(ep: usize, tid: ThreadId) -> bool {
    if ep >= NENDPOINTS {
        return false;
    }
    let orphan = {
        let mut e = EPS[ep].lock();
        e.owner_died(tid)
    }; // EPS freigegeben, bevor SCHEDS gesperrt wird
    if let Some(caller) = orphan {
        unblock_with_error(caller, sel4lake_abi::result::ERR_SERVER_GONE);
        true
    } else {
        false
    }
}

/// **Reply-Cap-Server-Migration beim Hot-Reload:** überträgt eine ausstehende
/// Antwortpflicht des Servers `tid` (das Reload-Opfer) auf die nächste RECV-Instanz
/// desselben Endpoints. Der wartende Aufrufer wird NICHT abgebrochen, sondern wieder
/// als Sender eingereiht; die neue Server-Instanz (v2) übernimmt dieselbe Nachricht
/// und schließt den Call ab. Gibt `true`, falls migriert wurde. Sperrt NUR EPS[ep] —
/// es wird niemand entblockt (kein SCHEDS-Lock, keine Sperrordnungsfrage).
pub fn endpoint_migrate_owner(ep: usize, tid: ThreadId) -> bool {
    if ep >= NENDPOINTS {
        return false;
    }
    EPS[ep].lock().migrate_owner(tid)
}

/// **Eager-Cleanup beim Thread-Tod:** den (sterbenden) Thread `tid` aus ALLEN
/// Endpoint-Queues (senders/receivers/caller) und Notification-Waitern entfernen.
/// Verhindert tote TCBs in den festen Queues (Corpse-Fill -> verdrängte echte Sender)
/// und ein REPLY/SIGNAL in einen recycelten Frame. Aus den Todespfaden (kill/exit/
/// fault) aufzurufen — OHNE gehaltenen SCHEDS-Lock (Sperrordnung EPS/NTFNS < SCHEDS).
pub fn purge_ipc_queues(tid: ThreadId) {
    // Verwaiste Aufrufer (deren Reply-Owner gerade stirbt) einsammeln und NACH dem
    // Freigeben der EPS-Locks mit ERR_SERVER_GONE entblocken (kein verschachtelter
    // EPS->SCHEDS-Lock). Ein Thread kann (mehrfaches recv ohne reply) Reply-Owner von bis zu
    // NENDPOINTS Endpoints zugleich sein -> das Array MUSS so gross sein, sonst wuerden Waisen
    // jenseits der Kapazitaet still verworfen und ihre Aufrufer haengen dauerhaft (Liveness-Bug).
    let mut orphans: [Option<ThreadId>; NENDPOINTS] = [None; NENDPOINTS];
    let mut no = 0usize;
    for ep in EPS.iter() {
        let mut e = ep.lock();
        if e.is_used() {
            e.purge_thread(tid);
            if let Some(caller) = e.owner_died(tid) {
                if no < orphans.len() {
                    orphans[no] = Some(caller);
                    no += 1;
                }
            }
        }
    }
    for n in NTFNS.iter() {
        let mut nt = n.lock();
        if nt.is_used() {
            nt.purge_thread(tid);
        }
    }
    for caller in orphans.iter().flatten() {
        unblock_with_error(*caller, sel4lake_abi::result::ERR_SERVER_GONE);
    }
}

/// **Scheduler-Audit über alle Kerne** (ext-30): `0` = alle Kern-Scheduler strukturell
/// konsistent, sonst `100*Kern + Code` des ersten Fehlers (s. `Scheduler::audit`). Prüft
/// insbesondere, dass jeder Directory-Eintrag auf genau den Kern + Slot zeigt, auf dem der
/// TCB tatsächlich liegt (Code 8) — die zentrale Migrations-Invariante.
pub fn sched_audit_all() -> u32 {
    for c in 0..num_cores() {
        let code = SCHEDS[c].lock().audit();
        if code != 0 {
            return (c as u32) * 100 + code;
        }
    }
    0
}

/// Einen blockierten Thread mit einem **Fehlercode** in `x0` (statt `OK`) entblocken —
/// für die Reply-Liveness: ein `CALL`-Aufrufer, dessen Server (Reply-Owner) verschwand,
/// wird so entblockt und sieht den Fehler, statt dauerhaft zu hängen. Sperrt kurz die
/// Zielinstanz; bei fremdem Kern Reschedule-IPI.
fn unblock_with_error(caller: ThreadId, code: u64) {
    let done = with_owner(caller, |sched, _| {
        let frame = sched.frame_of(caller)?; // fremder Kern/tot -> ggf. wiederholen
        hal::exception::frame_set_reg(frame, sel4lake_abi::reg::SYSNO_RESULT, code);
        sched.unblock(caller);
        Some(())
    });
    if let Some((_, c)) = done {
        kick(c);
    }
}

/// Lebt `tid`? (Für IPC-Audits.) Sperrt die Zielinstanz kurz.
pub fn thread_alive(tid: ThreadId) -> bool {
    // Lock-frei über das Thread-Directory (ext-30): kein Sperren eines fremden Kerns nötig,
    // und immun dagegen, dass der Thread gerade migriert.
    sel4lake_sched::is_live(tid)
}

/// **IPC-Konsistenz-Oracle** (Fuzzer): jedes belegte Endpoint/Notification + jeden
/// Kern-Scheduler auf strukturelle Invarianten prüfen. Gibt `0` bei Konsistenz, sonst
/// einen Anomalie-Code: 1=toter TCB in Endpoint-Queue/caller, 2=Duplikat in Endpoint-
/// Queue, 3=toter Waiter in Notification, 10+n=Scheduler-Audit-Code n (s. `Scheduler::
/// audit`). Sperrt je Objekt einzeln; die Liveness-Prüfung verschachtelt EPS/NTFNS ->
/// SCHEDS (zulässige Ordnung), nie zwei Objekte gleichzeitig.
pub fn ipc_audit() -> u32 {
    let live = &mut |t: ThreadId| -> bool { sel4lake_sched::is_live(t) };
    for ep in EPS.iter() {
        let e = ep.lock();
        if e.is_used() {
            let (dead, dup) = e.audit(live);
            if dead {
                return 1;
            }
            if dup {
                return 2;
            }
        }
    }
    for n in NTFNS.iter() {
        let nt = n.lock();
        if nt.is_used() && nt.audit(live) {
            return 3;
        }
    }
    for c in 0..num_cores() {
        let code = SCHEDS[c].lock().audit();
        if code != 0 {
            return 10 + code;
        }
    }
    // CDT-/Refcount-Property (Cap-Churn-Events des IPC-Fuzzers laufen während IPC).
    let cdt = CAPS.read().cspace.audit_cdt();
    if cdt != 0 {
        return 20 + cdt;
    }
    // Domänen-Policy-Property (ext-22): Cap-Typen je Domäne + Domäne↔VSpace-Isolation.
    let dom = domain_audit();
    if dom != 0 {
        return 30 + dom;
    }
    // DMA-Policy-Property (ext-23): DmaCap-Bounds/Disjunktheit + Enforcer-Durchsetzung.
    let dma = dma_audit();
    if dma != 0 {
        return 40 + dma;
    }
    // Loader-Property (ext-26): kein geladenes Programm-Segment überlappt freies RAM.
    let ld = loader_audit();
    if ld != 0 {
        return 60 + ld;
    }
    0
}

/// **Loader-Property-Oracle** (ext-26, L5): `0` = konsistent, sonst Anomalie-Code:
/// - `1` = ein aktuell in einer geladenen VSpace gemapptes Segment überlappt **freies** RAM (es
///   wurde freigegeben, während es noch gemappt ist → Use-after-free). Spiegelt `dma_audit` Code 4.
/// Snapshot der registrierten Segmente ziehen (`LOADED_IMAGES` kurz sperren), dann gegen
/// `MEM.overlaps_free` (Rangordnung: nie beide Locks gleichzeitig).
pub fn loader_audit() -> u32 {
    let mut snap = [(0u64, 0u64); NLOADED_IMG * MAX_IMG_SEGS];
    let mut n = 0;
    {
        let t = LOADED_IMAGES.lock();
        for img in t.iter() {
            if img.asid != 0 {
                for &(b, l) in img.segs[..img.nseg].iter() {
                    if l != 0 {
                        snap[n] = (b, l);
                        n += 1;
                    }
                }
            }
        }
    } // LOADED_IMAGES freigegeben
    let mem = MEM.lock();
    if snap[..n].iter().any(|&(b, l)| mem.overlaps_free(b, l)) {
        return 1;
    }
    0
}

/// **Domänen-Policy-Oracle** (ext-22): `0` = konsistent, sonst Anomalie-Code (1 = HW-Cap in
/// Nicht-HardwareLand, 2 = `PdControl` in Nicht-TrustedSas, 3 = Domäne↔VSpace inkonsistent).
/// Die VSpace-Zugehörigkeit (global vs. isoliert) liegt in `VSPACE_OF` (nicht in `Caps`), daher
/// wird sie hier per Closure eingespeist. Sperrt nur `CAPS.read()` (VSPACE_OF ist atomar).
pub fn domain_audit() -> u32 {
    // Regel 3 (untrusted Domäne MUSS isoliert sein) gilt nur für **lebende** gebundene
    // Threads: ein gestoppter/getöteter Thread (dessen VSPACE_OF auf 0 zurückgesetzt wurde,
    // dessen PD-Bindung aber noch auf den toten tid zeigt) ist KEINE Verletzung.
    let is_live_global =
        |tid: ThreadId| thread_alive(tid) && vspace_of(tid.slot()) == 0;
    CAPS.read().domain_audit(&is_live_global)
}
