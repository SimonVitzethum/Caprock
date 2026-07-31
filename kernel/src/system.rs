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
use sel4lake_ipc::{Endpoint, Notification, Quiescence, Rebind};
use sel4lake_mem::{MemoryCap, PhysAllocator, PhysRegion, Rights};
use sel4lake_microkit::{Caps, Domain};
use sel4lake_region::heap::RegionSource;
use sel4lake_region::{state, Purpose, Region, RegionTag};
use sel4lake_loader::elf::{ElfImage, PF_W, PF_X};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use crate::addr::{DmaRegion, Iova, Pa};
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
pub(crate) const USER_KSTACK_SIZE: usize = 0x4000; // 16 KiB EL1-Kernel-Stack je EL0-Thread

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
    claim_user_kstack_masked(None)
}
/// Wie [`claim_user_kstack`], aber optional aus einem Farbsatz (todo A1).
///
/// Der Kernel-Stack einer PD wird zwar vom Kernel benutzt, aber **im Namen dieses Subjekts** —
/// seine Cache-Zeilen tragen also dessen Zugriffsmuster. Ihn ungefärbt zu lassen hieße, die
/// Trennung an genau der Stelle aufzugeben, an der der Kernel für das Subjekt arbeitet.
/// 16 KiB sind vier Seiten und passen damit in jeden Streifen (kleinster Streifen: 16 Seiten).
fn claim_user_kstack_masked(mask: Option<sel4lake_mem::ColorMask>) -> Option<usize> {
    let (sz, al) = (USER_KSTACK_SIZE as u64, USER_KSTACK_SIZE as u64);
    let base = match mask {
        Some(m) => MEM.lock().alloc_colored(sz, al, crate::colors::count(), m)?.base(),
        None => MEM.lock().alloc(sz, al)?.base(),
    };
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
/// **Eine beim Boot einmalig befüllte Tabelle** (A-3.4 Teil 4).
///
/// Für die IPC-Tabellen taugt das sonst übliche `SpinLock<Slab<T>>` (so halten es `FP_STATES`
/// und `CAP_AUDIT`) **nicht**: ein Lock um die Tabelle würde jede IPC serialisieren und damit
/// genau die Eigenschaft aufheben, für die es die per-Endpoint-Locks gibt (ADR 0004:
/// „IPC auf verschiedenen Endpoints läuft parallel"). Auch ein `RwSpinLock` löst es nicht —
/// sein Read-Guard maskiert IRQs für seine ganze Lebensdauer, hier also über die gesamte IPC.
///
/// Die Tabelle braucht aber gar keinen Lock: sie wird **einmal** beim Boot befüllt und danach
/// nie wieder verändert. Verändert werden nur die *Elemente*, und die tragen ihren eigenen
/// Lock. Das ist dieselbe Bauform, die die HAL für ihre Seitentabellen benutzt
/// (`TableStore(UnsafeCell<PageTable>)`), nur typisiert.
///
/// **Der Vertrag, auf dem das steht** (die einzige `unsafe`-Stelle hier):
/// `attach` läuft genau einmal, auf dem Boot-Kern, **bevor** weitere Kerne starten und bevor
/// die erste IPC möglich ist. Danach gibt es nur noch `&Slab<T>` — geteilt, unveränderlich.
/// Wird der Aufruf vergessen, hat die Tabelle Länge 0: jeder Zugriff läuft in die
/// Schrankenprüfung und wird als `ERR_BADCAP` abgewiesen. Ein vergessener Aufruf fällt damit
/// als sauberer Fehler auf, nicht als Fehlzugriff.
struct BootSlab<T: 'static>(core::cell::UnsafeCell<Slab<T>>);

// SAFETY: Nach dem einmaligen `attach` (single-threaded, vor dem Start weiterer Kerne) gibt
// dieser Typ ausschließlich `&Slab<T>` heraus. Er verhält sich damit wie ein `&[T]`, den sich
// alle Kerne teilen -> `Sync` verlangt nur `T: Sync`, exakt wie bei `[T]`.
unsafe impl<T: Sync> Sync for BootSlab<T> {}

impl<T> BootSlab<T> {
    const fn new() -> Self {
        Self(core::cell::UnsafeCell::new(Slab::empty()))
    }

    /// Der Tabelle ihren Speicher geben.
    ///
    /// # Safety
    /// * Vertrag von [`Slab::attach`] (exklusiver, ausgerichteter, dauerhafter Speicher).
    /// * **Genau einmal**, auf dem Boot-Kern, bevor ein anderer Kern oder eine IPC laufen
    ///   kann — sonst existiert währenddessen ein `&mut` neben geteilten `&`.
    unsafe fn attach(&self, ptr: *mut T, len: usize, init: impl FnMut(usize) -> T) {
        // SAFETY: an den Aufrufer durchgereicht (s. Funktionsdoku): zu diesem Zeitpunkt gibt es
        // keinen zweiten Verweis auf die Tabelle, der `&mut` ist also exklusiv.
        unsafe { (*self.0.get()).attach(ptr, len, init) };
    }

    /// Die Tabelle geteilt lesen — nach dem Boot der einzige Zugang.
    fn get(&self) -> &Slab<T> {
        // SAFETY: nach dem Boot wird die Tabelle nicht mehr verändert (s. Typ-Doku); es kann
        // also kein `&mut` daneben existieren. Vor dem `attach` ist sie leer, aber gültig.
        unsafe { &*self.0.get() }
    }
}

/// **Per-Endpoint** Locks: IPC auf verschiedenen Endpoints ist nebenläufig.
///
/// Seit A-3.4 Teil 4 beim Boot dimensioniert (vorher `[SpinLock<Endpoint>; 32]` im `.bss`).
static EPS: BootSlab<SpinLock<Endpoint>> = BootSlab::new();
/// **Per-Notification** Locks. Wie [`EPS`] beim Boot dimensioniert.
static NTFNS: BootSlab<SpinLock<Notification>> = BootSlab::new();

/// Zugriff auf die Endpoint-Tabelle als Slice (Schranke = **reale** Kapazität).
fn eps() -> &'static [SpinLock<Endpoint>] {
    EPS.get().as_slice()
}

/// Zugriff auf die Notification-Tabelle als Slice.
fn ntfns() -> &'static [SpinLock<Notification>] {
    NTFNS.get().as_slice()
}

/// **Sammelflaeche fuer verwaiste Aufrufer** beim Thread-Tod (`purge_ipc_queues`, A-3.4 Teil 4).
///
/// Ein Eintrag je Endpoint — mehr Waisen kann ein einzelner sterbender Thread nicht erzeugen.
/// Frueher eine lokale Variable; bei 10 000 Endpoints waeren das 240 KiB Kernelstack im
/// Todespfad gewesen (s. `purge_ipc_queues`).
static IPC_ORPHANS: SpinLock<Slab<Option<ThreadId>>> = SpinLock::new(Slab::empty());

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
/// SYS_LOAD-Callback in den Binary-Loader (A-1.5). Seit A-1.1 gibt es das Boot-Archiv auf
/// **beiden** Architekturen: auf ARM in einem reservierten RAM-Fenster, auf x86 als
/// Multiboot-Modul. Wo keines vorliegt, schlägt der Aufruf sauber fehl (der Loader liefert
/// `None`), statt etwas Halbes zu laden.
use crate::loader::load_by_index;

fn dispatch_delete_cap(cap: sel4lake_cap::CapPtr) -> bool {
    cap_delete(cap).is_ok()
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
        eps(),
        ntfns(),
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
    init_mem_regions(&[(free_base, ram_end - free_base)], ram_end);
}

/// Freien Speicher als **Liste von Bereichen** registrieren.
///
/// Der Kernel bekam seinen Speicher bisher als genau ein `[free_base, ram_end)`. Das ist die
/// Sicht eines Kernels, der die Maschine allein besitzt — und genau die gilt nicht mehr, sobald
/// er neben einem anderen Betriebssystem auf uebergebenen Kernen laeuft: dort kommt der Speicher
/// als Sammlung dessen, was der Wirt hergibt (auf Linux hoechstens 4 MiB am Stueck, weil die
/// Buddy-Ordnung dort endet).
///
/// Der Allokator selbst fuehrt ohnehin eine Fragmentliste; die Einschraenkung lag allein in
/// dieser Funktion. `ram_top` bleibt separat, weil daran die Wahl des IOVA-Fensters haengt —
/// es muss oberhalb des **hoechsten** physischen Speichers liegen, nicht oberhalb des letzten
/// uebergebenen Bereichs.
pub fn init_mem_regions(regions: &[(u64, u64)], ram_top: u64) {
    RAM_TOP.store(ram_top, Ordering::Release);
    let mut mem = MEM.lock();
    for &(base, len) in regions {
        if len != 0 && !mem.add_region(base, len) {
            // Laut scheitern statt still weniger Speicher zu haben: ein verworfener Bereich
            // faellt sonst erst auf, wenn eine spaetere Allokation ohne erkennbaren Grund
            // fehlschlaegt.
            // Die serielle Ausgabe haengt am aarch64-HAL (s. `note_undeclared_device`).
            #[cfg(target_arch = "aarch64")]
            crate::println!(
                "mem     : WARNUNG Bereich 0x{:x}+0x{:x} nicht aufgenommen (Fragmentliste voll)",
                base,
                len
            );
            MEM_REGIONS_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Oberste physische RAM-Adresse (aus dem Boot). Grundlage für die Wahl des IOVA-Fensters:
/// oberhalb davon kann eine IOVA **nie** eine gültige PA sein.
static RAM_TOP: AtomicU64 = AtomicU64::new(0);

/// Wie viele beim Hochlauf angebotene Speicherbereiche **nicht** aufgenommen wurden.
///
/// Muss 0 sein. Ein verworfener Bereich faellt sonst erst auf, wenn eine spaetere Allokation
/// ohne erkennbaren Grund fehlschlaegt — gezaehlt statt stillschweigend hingenommen.
static MEM_REGIONS_DROPPED: AtomicU32 = AtomicU32::new(0);

/// Anzahl beim Hochlauf verworfener Speicherbereiche (muss 0 sein).
pub fn mem_regions_dropped() -> u32 {
    MEM_REGIONS_DROPPED.load(Ordering::Relaxed)
}

/// **Thread-Slots je Kern**, die der Kernel im Mittel vorsieht. Die Gesamtkapazität ist
/// `cores * THREADS_PER_CORE`; jeder Kern kann durch Migration bis zum
/// [`MIGRATION_HEADROOM`]-fachen davon *hosten*.
const THREADS_PER_CORE: usize = 256;

/// **Ziel-Thread-Kapazitaet des Systems** (A-3.4).
///
/// Bis hierher ergab sich die Kapazitaet als `cores * THREADS_PER_CORE` -- auf einer
/// 4-Kern-Maschine also 1024, und auf einer 2-Kern-Maschine die Haelfte. Das ist eine Eigenschaft
/// des **Testaufbaus**, keine Zusage des Systems: dieselbe Software haette je nach Maschine eine
/// andere Obergrenze, ohne dass es irgendwo steht.
///
/// Jetzt ist die Kapazitaet eine **Zusage**, und die Kernzahl teilt sie nur auf. Die Tabellen
/// dahinter sind seit ext-30 ohnehin boot-dimensioniert (`attach_*`), nicht `.bss` -- die Zahl
/// kostet also RAM, keine Struktur. Was sie kostet, meldet der Boot-Report (`bytes`), damit die
/// Entscheidung an einer gemessenen Groesse haengt und nicht an einem Gefuehl.
///
/// **Untergrenze bleibt `THREADS_PER_CORE` je Kern**: eine Maschine mit vielen Kernen soll nicht
/// weniger Threads je Kern haben als vorher.
pub const TARGET_THREADS: usize = 10_000;
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
    // Die Ziel-Kapazitaet wird auf die Kerne aufgeteilt, mindestens aber THREADS_PER_CORE je Kern
    // (aufgerundet, damit die Summe das Ziel nie unterschreitet).
    let je_kern = ((TARGET_THREADS + cores - 1) / cores).max(THREADS_PER_CORE);
    let total = cores * je_kern;
    let per_core = je_kern * MIGRATION_HEADROOM;
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

/// **Slot-Kapazität des globalen Capability-Space** (A-3.4).
///
/// Summe aller PD-Budgets plus Kernel-Reserve. Die Rechnung stand hier als
/// `CAP_SLOTS_FOR_ALL_PDS + 256` und damit an einer zweiten Stelle neben der Definition der
/// Budgets; sie ist nach `sel4lake_microkit::CAP_SLOTS_TOTAL` gezogen. Was die Reserve deckt und
/// warum sie neben der Summe steht, ist dort dokumentiert — **nachgezählt** wird sie von
/// `cap_nonbudget_slots`.
const CAP_SLOTS: usize = sel4lake_microkit::CAP_SLOTS_TOTAL;

/// **Objekt-Kapazität** — genauso viele wie Slots.
///
/// Bis A-3.4 stand das Verhältnis auf 2:1 (256 Slots / 128 Objekte), aus der Beobachtung heraus,
/// dass viele Caps Ableitungen auf dasselbe Objekt sind. Als Zusage taugt das nicht: `install`
/// erzeugt Slot **und** Objekt, ein System aus lauter Wurzel-Caps braucht also so viele Objekte wie
/// Slots. Mit 2:1 wäre „jede PD bekommt ihr Budget" an der Objekttabelle gescheitert statt an der
/// Slot-Tabelle — dieselbe Lücke, eine Ebene tiefer.
const CAP_OBJECTS: usize = CAP_SLOTS;

/// **Capability-Tabellen zur Boot-Zeit anlegen** (A-3.4, Teil 2) — Slot- und Objekttabelle des
/// globalen `CapSpace`, dazu der Finalisierungspuffer und die Zählfläche des CDT-Audits.
///
/// **Muss vor der ersten Cap-Installation laufen**, also vor `selftest::run()` und vor allem, was
/// Speicher als Cap ausgibt. Vorher hat der Space Kapazität 0 und jede Installation scheitert mit
/// `NoSlot` — ein vergessener Aufruf fällt damit als sauberer Fehler auf, nicht als Fehlzugriff.
///
/// Der Finalisierungspuffer kommt hier aus **derselben Quelle** wie die Tabellen — genau das hatte
/// A-3.3 vorbereitet, als es die Arrays aus `Finalized` in geliehene Slices verwandelte: die
/// Objekttabelle wachsen zu lassen, ohne den Kernelstack jedes Threads mitwachsen zu lassen.
///
/// Gibt die belegten Bytes zurück (Boot-Report).
pub fn configure_caps() -> u64 {
    let mut bytes = 0u64;
    // Wie in `configure`: der Speicher gehört ab hier dauerhaft dem Kernel (die MemoryCap wird
    // bewusst fallen gelassen — Kerneltabellen leben bis zum Reboot).
    let mut table = |size: usize, align: usize| -> *mut u8 {
        let cap = mem_alloc(size as u64, align as u64).expect("Cap-Tabelle: RAM erschoepft");
        bytes += cap.len();
        cap.base() as *mut u8
    };

    let slots_mem = table(
        CAP_SLOTS * core::mem::size_of::<sel4lake_cap::CapSlot>(),
        core::mem::align_of::<sel4lake_cap::CapSlot>().max(4096),
    );
    let objs_mem = table(
        CAP_OBJECTS * core::mem::size_of::<sel4lake_cap::Object>(),
        core::mem::align_of::<sel4lake_cap::Object>().max(4096),
    );
    let mut slots: Slab<sel4lake_cap::CapSlot> = Slab::empty();
    let mut objects: Slab<sel4lake_cap::Object> = Slab::empty();
    // SAFETY: frisch allozierter, exklusiver, korrekt ausgerichteter Speicher der geforderten
    // Größe, der bis zum Reboot lebt; einmaliger Aufruf beim Boot vor jeder Cap-Operation.
    unsafe {
        slots.attach(
            slots_mem as *mut sel4lake_cap::CapSlot,
            CAP_SLOTS,
            |_| sel4lake_cap::CapSlot::EMPTY,
        );
        objects.attach(
            objs_mem as *mut sel4lake_cap::Object,
            CAP_OBJECTS,
            |_| sel4lake_cap::Object::EMPTY,
        );
    }
    CAPS.write().cspace.attach(slots, objects);
    // Die Zusage "jede PD bekommt ihr Budget" haengt daran, dass die Tabelle wirklich so gross
    // ist wie gerechnet. `attach` nimmt entgegen, was der Aufrufer gibt -- eine zu kleine Tabelle
    // faellt sonst erst auf, wenn eine PD innerhalb ihres Budgets `NoSlot` bekommt, also
    // fruehestens unter Last und ohne Hinweis auf die Ursache.
    {
        let g = CAPS.read();
        let (have_slots, have_objs) = g.cspace.capacity();
        assert!(
            have_slots >= sel4lake_microkit::CAP_SLOTS_TOTAL && have_objs >= CAP_OBJECTS,
            "Cap-Tabellen kleiner als gerechnet: {have_slots}/{have_objs} Slots/Objekte"
        );
    }

    // Finalisierungspuffer + Enforcer-Kratzflächen: je ein Eintrag pro Objekt, denn mehr Objekte
    // als die Tabelle fasst kann eine einzelne `delete`/`revoke`-Operation nicht finalisieren.
    let n = CAP_OBJECTS;
    let items = table(n * core::mem::size_of::<(u32, u64)>(), 4096);
    let dma = table(n * core::mem::size_of::<(u64, u64)>(), 4096);
    let ok = table(n, 4096);
    let ctx = table(n * core::mem::size_of::<usize>(), 4096);
    {
        let mut b = FINALIZE.lock();
        // SAFETY: wie oben; jede Tabelle bekommt ihren **eigenen** Block (exklusiv).
        unsafe {
            b.items.attach(items as *mut (u32, u64), n, |_| (0, 0));
            b.dma.attach(dma as *mut (u64, u64), n, |_| (0, 0));
            b.ok.attach(ok as *mut bool, n, |_| false);
            b.ctx_of.attach(ctx as *mut usize, n, |_| usize::MAX);
        }
    }

    let refs = table(n * core::mem::size_of::<u32>(), 4096);
    // SAFETY: wie oben.
    unsafe { CAP_AUDIT.lock().attach(refs as *mut u32, n, |_| 0u32) };

    // Zaehlflaeche der Summenpruefung: ein Markierbit je SLOT (nicht je Objekt), s.
    // `cap_nonbudget_slots`.
    let seen = table(CAP_SLOTS, 4096);
    // SAFETY: wie oben.
    unsafe { CAP_SEEN.lock().attach(seen as *mut bool, CAP_SLOTS, |_| false) };

    // A-3.4 Teil 3: die PD-Tabelle. Sie war der Grund, warum 10000 Threads keine 10000 Tenants
    // waren -- `[Pd; 256]` im `.bss`. Ab hier gilt dasselbe wie fuer die Cap-Tabellen: die Zahl
    // kostet RAM, keine Struktur, und was sie kostet, steht im Boot-Report.
    let pds_mem = table(
        sel4lake_microkit::NPDS * core::mem::size_of::<sel4lake_microkit::Pd>(),
        core::mem::align_of::<sel4lake_microkit::Pd>().max(4096),
    );
    // SAFETY: frisch allozierter, exklusiver, ausgerichteter Speicher der geforderten Groesse,
    // der bis zum Reboot lebt; einmaliger Aufruf beim Boot VOR der ersten PD.
    unsafe {
        CAPS.write().pds.attach(
            pds_mem as *mut sel4lake_microkit::Pd,
            sel4lake_microkit::NPDS,
        )
    };

    bytes
}

/// **Endpoint-Kapazität** (A-3.4 Teil 4): einer je PD, dazu eine Reserve für die Kanäle, die
/// der Kernel selbst aufmacht (Bringup-Kanäle, HardwareLand-Backends, Testkanäle).
///
/// Die Reserve steht **neben** der Summe, nicht in ihr — aus demselben Grund wie bei `CAP_SLOTS`:
/// eine PD, die ihren Endpoint nimmt, soll dem Kernel keinen wegnehmen können.
const NEPS: usize = sel4lake_microkit::ENDPOINTS_FOR_ALL_PDS + 64;
/// **Notification-Kapazität** — wie [`NEPS`], eine je PD plus Reserve.
const NNTFNS: usize = sel4lake_microkit::NOTIFICATIONS_FOR_ALL_PDS + 64;

/// **IPC-Tabellen zur Boot-Zeit anlegen** (A-3.4 Teil 4) — Endpoints, Notifications und die
/// Sammelfläche für verwaiste Aufrufer.
///
/// **Muss vor der ersten IPC laufen** und vor allem, was einen Endpoint reserviert
/// (`create_endpoint`/`create_notification`, also vor `selftest::run()` und vor den
/// Bringup-Kanälen). Vorher haben die Tabellen Länge 0: `create_endpoint` findet keinen freien
/// Slot (`None`), der Dispatch weist jede Endpoint-Cap mit `ERR_BADCAP` ab. Ein vergessener
/// Aufruf fällt damit als sauberer Fehler auf, nicht als Fehlzugriff.
///
/// Gibt die belegten Bytes zurück (Boot-Report).
pub fn configure_ipc() -> u64 {
    let mut bytes = 0u64;
    let mut table = |size: usize, align: usize| -> *mut u8 {
        let cap = mem_alloc(size as u64, align as u64).expect("IPC-Tabelle: RAM erschoepft");
        bytes += cap.len();
        cap.base() as *mut u8
    };

    let eps_mem = table(
        NEPS * core::mem::size_of::<SpinLock<Endpoint>>(),
        core::mem::align_of::<SpinLock<Endpoint>>().max(4096),
    );
    let ntfns_mem = table(
        NNTFNS * core::mem::size_of::<SpinLock<Notification>>(),
        core::mem::align_of::<SpinLock<Notification>>().max(4096),
    );
    let orphans_mem = table(
        NEPS * core::mem::size_of::<Option<ThreadId>>(),
        core::mem::align_of::<Option<ThreadId>>().max(4096),
    );

    // SAFETY: frisch allozierter, exklusiver, korrekt ausgerichteter Speicher der geforderten
    // Größe, der bis zum Reboot lebt. Einmaliger Aufruf beim Boot auf dem Boot-Kern, bevor ein
    // weiterer Kern startet und bevor die erste IPC möglich ist — der Vertrag von
    // `BootSlab::attach`.
    unsafe {
        EPS.attach(eps_mem as *mut SpinLock<Endpoint>, NEPS, |_| {
            SpinLock::new(Endpoint::EMPTY)
        });
        NTFNS.attach(ntfns_mem as *mut SpinLock<Notification>, NNTFNS, |_| {
            SpinLock::new(Notification::EMPTY)
        });
        IPC_ORPHANS
            .lock()
            .attach(orphans_mem as *mut Option<ThreadId>, NEPS, |_| None);
    }

    // Gemeldet wird die **angehaengte** Kapazitaet, nicht `NEPS`/`NNTFNS`: eine Zahl, die neben
    // der Tabelle her existiert, kann von ihr abweichen — und genau daran haengt die Pruefung
    // der Testsuite.
    let (n_eps, n_ntfns) = ipc_capacity();
    println!(
        "ipc     : {n_eps} Endpoints / {n_ntfns} Notifications, Tabellen {} KiB aus dem RAM (eine PD, ein Endpoint: {} PDs)",
        bytes >> 10,
        sel4lake_microkit::ENDPOINTS_FOR_ALL_PDS,
    );
    bytes
}

/// Kapazität der IPC-Tabellen (Selbsttest/Telemetrie): `(Endpoints, Notifications)` — die
/// **tatsächlich** zugewiesene, nicht die Konstante.
pub fn ipc_capacity() -> (usize, usize) {
    (eps().len(), ntfns().len())
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

/// [`mem_alloc`] oder [`alloc_colored`], je nachdem ob ein Farbsatz vorgegeben ist. Beide
/// nullen; der Unterschied ist ausschliesslich die Farbbedingung.
fn mem_alloc_masked(
    size: u64,
    align: u64,
    mask: Option<sel4lake_mem::ColorMask>,
) -> Option<MemoryCap> {
    match mask {
        Some(m) => alloc_colored(size, align, m),
        None => mem_alloc(size, align),
    }
}

pub fn alloc(size: u64, align: u64) -> Option<MemoryCap> {
    mem_alloc(size, align)
}
/// Wie [`alloc`], aber jede Seite trägt eine Farbe aus `mask` (todo A1). Genullt wie jede
/// Region, die an ein Subjekt gehen kann.
pub fn alloc_colored(size: u64, align: u64, mask: sel4lake_mem::ColorMask) -> Option<MemoryCap> {
    let colors = crate::colors::count();
    let cap = MEM.lock().alloc_colored(size, align, colors, mask)?;
    zero_phys(cap.base(), cap.len());
    Some(cap)
}
pub fn free(cap: MemoryCap) {
    MEM.lock().free(cap);
}
pub fn total_free() -> u64 {
    MEM.lock().total_free()
}
/// Ist `[base, base+len)` vollständig frei? Präzise Leckprüfung für eine **bestimmte** Region —
/// im Gegensatz zu [`total_free`], das global ist und von der Nebenläufigkeit anderer Kerne
/// mitbewegt wird.
pub fn region_fully_free(base: u64, len: u64) -> bool {
    MEM.lock().fully_free(base, len)
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
    // Sperrordnung: CAP_AUDIT vor CAPS (s. dort). Code 8 = die Zählfläche war kürzer als die
    // Objekttabelle; dann konnte das Audit nicht laufen und meldet das, statt „konsistent" zu
    // sagen.
    let mut scratch = CAP_AUDIT.lock();
    CAPS.read().cspace.audit_cdt(scratch.as_mut_slice())
}

/// **Summenprüfung des Cap-Budgets** (A-3.4, Abschluss): wie viele belegte Slots gehen auf **kein**
/// PD-Budget, und passen sie in die Kernel-Reserve?
///
/// Liefert `(nicht_budgetiert, reserve, ok)`. `ok == false` heisst: der Kernel selbst hat mehr
/// Slots belegt als die Reserve deckt — dann kann eine PD *innerhalb* ihres Budgets abgewiesen
/// werden, obwohl `budget_allows` nichts zu beanstanden hat. Genau diese Richtung prüfte bisher
/// niemand; die Dimensionierung trug die Zusage allein.
///
/// `nicht_budgetiert == usize::MAX` markiert „die Prüfung konnte nicht laufen" (zu kleine
/// Zählfläche); `ok` ist dann `false`, nicht stillschweigend `true`.
pub fn cap_nonbudget_slots() -> (usize, usize, bool) {
    // Sperrordnung: CAP_SEEN vor CAPS (s. dort).
    let mut scratch = CAP_SEEN.lock();
    let g = CAPS.read();
    match g.nonbudget_slots(scratch.as_mut_slice()) {
        Some(n) => (n, sel4lake_microkit::CAP_SLOTS_KERNEL_RESERVE, n <= sel4lake_microkit::CAP_SLOTS_KERNEL_RESERVE),
        None => (usize::MAX, sel4lake_microkit::CAP_SLOTS_KERNEL_RESERVE, false),
    }
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
/// **Der Rückmeldepuffer der Cap-Finalisierung** (A-3.3) — einmal statisch, nicht je Aufruf auf
/// dem Kernelstack.
///
/// Bis hierher legte jeder `cap_delete`/`cap_revoke` ein `Finalized` als **lokale Variable** an:
/// zwei `[_; 128]`-Arrays, rund 4 KiB, plus die 2,2 KiB, die `dma_finalize` daneben nochmals
/// aufmachte. Damit hing die Objekttabelle an der Stackgröße — `NOBJECTS` zu vergrößern (A-3.4,
/// todo C3) hätte den Stack **jedes** Threads mitvergrößert. Genau das nennt todo A3 als
/// Nebenbedingung, und deshalb steht A-3.3 vor A-3.4.
///
/// Die Kapazität kommt aus der Cap-Crate ([`sel4lake_cap::MAX_FINALIZED`]) statt aus einer
/// zweiten Konstante hier; wächst die Tabelle, wächst der Puffer mit, ohne dass jemand daran
/// denken muss.
struct FinalizeBuf {
    /// `(ep, caller)`-Paare abzubrechender Calls.
    items: Slab<(u32, u64)>,
    /// Finalisierte DMA-Regionen `(phys, len)`.
    dma: Slab<(u64, u64)>,
    /// Ergebnis je Region aus `dma_enforcer().finalize` — abgebaut ja/nein.
    ok: Slab<bool>,
    /// Kratzfläche der Enforcer: je Region der Index des betroffenen Übersetzungskontexts.
    /// Lag bis A-3.3 in **jeder** `finalize`-Implementierung nochmals auf dem Stack.
    ctx_of: Slab<usize>,
}

/// **Sperrordnung: `FINALIZE` ist die äußerste.** Sie wird vor `CAPS` genommen und erst nach
/// `abort_finalized_replies` (EPS < SCHEDS) und `dma_finalize` wieder freigegeben — die Meldungen
/// liegen ja darin.
///
/// Das führt **keine neue Verklemmungsklasse** ein: `FINALIZE` nimmt ausschliesslich
/// `cap_delete`/`cap_revoke`, und beide nehmen unmittelbar danach ohnehin `CAPS.write()`. Ein
/// Pfad, der sich hier verklemmen könnte, verklemmte sich vorher schon an `CAPS`. Was sich
/// tatsächlich ändert: die beiden Operationen serialisieren jetzt auch gegeneinander — was sie
/// über `CAPS.write()` bereits taten.
static FINALIZE: SpinLock<FinalizeBuf> = SpinLock::new(FinalizeBuf {
    items: Slab::empty(),
    dma: Slab::empty(),
    ok: Slab::empty(),
    ctx_of: Slab::empty(),
});

/// **Zählfläche des CDT-Audits** (A-3.4) — ein `u32` je Objekt-Eintrag.
///
/// Lag bis hierher als `[0u32; NOBJECTS]` auf dem Stack von `audit_cdt`. Mit einer
/// boot-dimensionierten Objekttabelle geht das nicht mehr: die Größe steht erst zur Laufzeit fest,
/// und ein mitwachsendes Stack-Array wäre genau die Kopplung, die A-3.3 für den
/// Finalisierungspuffer aufgelöst hat.
///
/// **Sperrordnung wie bei [`FINALIZE`]:** wird *vor* `CAPS` genommen. Das führt keine neue
/// Verklemmungsklasse ein — `CAP_AUDIT` nimmt ausschliesslich [`cap_audit_cdt`], und die nimmt
/// unmittelbar danach `CAPS.read()`.
static CAP_AUDIT: SpinLock<Slab<u32>> = SpinLock::new(Slab::empty());

/// **Zählfläche der Summenprüfung** (A-3.4, Abschluss) — ein Markierbit je Cap-**Slot**.
///
/// Eigene Fläche statt Mitbenutzung von [`CAP_AUDIT`]: die dortige ist je *Objekt* dimensioniert
/// (heute gleich gross, aber aus einem anderen Grund) und wird von `audit_cdt` genullt. Zwei
/// Prüfungen, die sich einen Puffer teilen, laufen irgendwann ineinander — und dann meldet die
/// eine ein Ergebnis, das die andere überschrieben hat.
///
/// **Sperrordnung wie bei [`CAP_AUDIT`]:** vor `CAPS`, genommen ausschliesslich von
/// [`cap_nonbudget_slots`].
static CAP_SEEN: SpinLock<Slab<bool>> = SpinLock::new(Slab::empty());

/// Wie oft eine Finalisierungsmeldung mangels Kapazität verworfen wurde. **Muss 0 bleiben.**
static FINALIZE_OVERFLOW: AtomicU32 = AtomicU32::new(0);

/// Zähler der verworfenen Finalisierungsmeldungen (Audit-Code 70 in [`ipc_audit`]).
pub fn finalize_overflow_count() -> u32 {
    FINALIZE_OVERFLOW.load(Ordering::Relaxed)
}

/// Den Überlauf auswerten — **die Prüfung, die es bis A-3.3 nicht gab.**
///
/// `Finalized::overflowed()` existierte mit dem Kommentar „damit «kann nicht vorkommen» prüfbar
/// ist statt behauptet", und gerufen hat sie **niemand**. Solange die Arrays im Typ lagen, war das
/// verschmerzbar; seit der Puffer vom Aufrufer kommt, ist die Kapazität eine Entscheidung und
/// keine Typeigenschaft mehr. Ein Überlauf heisst konkret: ein in `CALL` blockierter Aufrufer wird
/// **nie** entblockt (Liveness-Bug) oder eine DMA-Region bleibt gemappt und belegt.
///
/// Deshalb laut und sofort, nicht nur als Zähler: ein Fehler dieser Klasse ist im Nachhinein an
/// nichts mehr zu erkennen — man sieht nur einen Thread, der steht.
fn note_finalize_overflow(rf: &sel4lake_cap::Finalized<'_>) {
    if rf.overflowed() {
        FINALIZE_OVERFLOW.fetch_add(1, Ordering::Relaxed);
        println!(
            "cap     : FAILURES Finalisierungspuffer uebergelaufen (Kapazitaet {}) -- Meldungen verworfen: blockierte CALL-Aufrufer bleiben stehen",
            CAPS.read().cspace.finalize_capacity()
        );
    }
}

pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {
    // Finalisierung gibt ggf. Speicher zurück -> CAPS vor MEM (Sperrordnung). Beim
    // Finalisieren einer Reply-Cap wird der zugehörige Call abgebrochen (NACH dem
    // Freigeben von CAPS/MEM, da das Entblocken EPS<SCHEDS sperrt -> CAPS < EPS).
    let mut buf = FINALIZE.lock();
    let b = &mut *buf;
    let mut rf = sel4lake_cap::Finalized::new(b.items.as_mut_slice(), b.dma.as_mut_slice());
    let r = {
        let mut caps = CAPS.write();
        let mut mem = MEM.lock();
        caps.cspace.delete(&mut mem, ptr, &mut rf)
    };
    abort_finalized_replies(&rf);
    dma_finalize(&rf, b.ok.as_mut_slice(), b.ctx_of.as_mut_slice());
    note_finalize_overflow(&rf);
    r
}
pub fn cap_revoke(ptr: CapPtr) -> Result<(), CapError> {
    let mut buf = FINALIZE.lock();
    let b = &mut *buf;
    let mut rf = sel4lake_cap::Finalized::new(b.items.as_mut_slice(), b.dma.as_mut_slice());
    let r = {
        let mut caps = CAPS.write();
        let mut mem = MEM.lock();
        caps.cspace.revoke(&mut mem, ptr, &mut rf)
    };
    abort_finalized_replies(&rf);
    dma_finalize(&rf, b.ok.as_mut_slice(), b.ctx_of.as_mut_slice());
    note_finalize_overflow(&rf);
    r
}

/// Für jede beim Löschen/Revoke finalisierte Reply-Cap den ausstehenden Call abbrechen:
/// den noch wartenden Aufrufer mit `ERR_SERVER_GONE` entblocken (Revocation eines
/// Calls). Läuft OHNE gehaltenen CAPS/MEM-Lock (Ordnung CAPS < EPS < SCHEDS).
fn abort_finalized_replies(rf: &sel4lake_cap::Finalized<'_>) {
    for (ep, caller_raw) in rf.iter() {
        endpoint_abort_call(ep as usize, ThreadId::from_raw(caller_raw));
    }
}

/// Einen konkreten ausstehenden Call an Endpoint `ep` abbrechen (Reply-Cap-Revocation):
/// ist `caller` noch der wartende Aufrufer, wird er mit `ERR_SERVER_GONE` entblockt.
pub fn endpoint_abort_call(ep: usize, caller: ThreadId) -> bool {
    if ep >= eps().len() {
        return false;
    }
    let orphan = {
        let mut e = eps()[ep].lock();
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
    for (i, ep) in eps().iter().enumerate() {
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
    for (i, n) in ntfns().iter().enumerate() {
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

/// Wie [`install_notification_cap`], aber mit einem **Badge** (A-2.1).
///
/// Das Badge einer Notification ist eine Eigenschaft der **Cap**, nicht der Nachricht:
/// `SYS_SIGNAL` verodert das Badge des benutzten Caps in `pending`, das `x2`-Wort des Aufrufs
/// spielt keine Rolle. Wer also unterscheiden will, *wer* signalisiert hat, muss beim Vergeben
/// unterschiedlich badgen — hier beim Endowment, und beim Weiterreichen in `SYS_LOAD`.
///
/// Zurück kommt die **geminzte** Cap; die ungebadgete bleibt als CDT-Eltern beim Kernel (sie zu
/// löschen ginge ohnehin nicht, solange ein Kind existiert, und sie ist die Wurzel, über die eine
/// spätere Rücknahme läuft).
pub fn install_notification_cap_badged(
    ntfn: u32,
    rights: Rights,
    badge: u64,
) -> Result<CapPtr, CapError> {
    let mut g = CAPS.write();
    let root = g.cspace.install_notification(ntfn, rights)?;
    g.cspace.mint(root, rights, badge)
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
    /// Belegter Farbstreifen dieser PD (B-4.2), `None` bei ungefärbten VSpaces.
    ///
    /// **Warum hier und nicht am Thread:** ein Streifen gehört dem *Adressraum*, nicht dem
    /// Ausführungsfaden — Region, Kernel-Stack und Seitentabellen der PD stammen daraus. Am
    /// Thread aufgehängt würde er bei mehreren Threads je PD mehrfach freigegeben oder gar nicht;
    /// an der VSpace hat er genau einen Anfang und genau ein Ende (`vspace_teardown`).
    stripe: Option<u32>,
}
static VSPACES: SpinLock<[VSpaceEnt; MAX_VSPACES]> =
    SpinLock::new([VSpaceEnt { used: false, l1: 0, l2: 0, stripe: None }; MAX_VSPACES]);

/// Eine **leere** isolierte VSpace anlegen (Kernel EL1-only, kein User-Frame). Gibt
/// `(asid, l1_phys)` oder `None` (kein ASID/Speicher). Allokiert L1+L2 aus `MEM`
/// (zuerst, freigegeben), trägt dann die Metadaten ein (`MEM` und `VSPACES` nie
/// gleichzeitig gehalten -> keine Sperrordnungs-Inversion zu Teardown/map).
fn create_vspace() -> Option<(u16, u64)> {
    create_vspace_masked(None)
}

/// Wie [`create_vspace`], aber die Seitentabellen kommen optional aus einem Farbsatz (todo A1).
///
/// Tabellenzeilen werden vom **Seitenlaufwerk der MMU** geladen und liegen im selben LLC wie
/// alles andere; ein Walk im Namen einer PD hinterlaesst also Spuren. Sie mitzufaerben kostet
/// nichts (zwei bzw. drei 4-KiB-Seiten) und schliesst einen Kanal, den man sonst uebersieht.
fn create_vspace_masked(mask: Option<sel4lake_mem::ColorMask>) -> Option<(u16, u64)> {
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
        t[i] = VSpaceEnt { used: true, l1: 0, l2: 0, stripe: None }; // reserviert
        (i + 1) as u16
    };
    let a = mem_alloc_masked(4096, 4096, mask);
    let b = mem_alloc_masked(4096, 4096, mask);
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
        // **Auch diese Ebene traegt die Maske.** Sie ging bisher ueber das ungefaerbte
        // `mem_alloc` — auf x86_64 (PML4 -> PDPT -> PD) lag damit eine der drei Tabellen einer
        // gefaerbten PD ausserhalb ihres Farbsatzes, und der Farbtest sah es nicht, weil er nur
        // `l1`/`l2` zurueckliest. Ein Seitenlauf der MMU im Namen der PD hinterlaesst dort
        // dieselben Spuren wie in den beiden anderen. Schlaegt die gefaerbte Zuteilung fehl,
        // scheitert das Anlegen der VSpace — kein stiller Rueckfall auf fremde Farben.
        let f = mem_alloc_masked(4096, 4096, mask)?;
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
        // Der Streifen wird nach dem Anlegen gesetzt (`vspace_bind_stripe`): hier ist noch nicht
        // entschieden, ob diese VSpace eine gefärbte PD trägt.
        stripe: None,
    };
    Some((asid, l1.base()))
}

/// **A-3.4-Telemetrie:** `(Slot-Hoechststand, Slot-Kapazitaet, Objekt-Hoechststand,
/// Objekt-Kapazitaet)` des globalen Cap-Space.
///
/// Der Endstand (`used_slots`) sagt ueber Erschoepfung nichts: ein Lauf, der zwischendurch an die
/// Grenze stiess und danach aufraeumte, sieht hinterher harmlos aus. Die Fairness-Zusage des
/// Cap-Budgets haengt aber am GLEICHZEITIGEN Verbrauch.
/// Kapazität `(Slots, Objekte)` des globalen Capability-Space (Boot-Report/Tests).
pub fn cap_capacity() -> (usize, usize) {
    CAPS.read().cspace.capacity()
}

/// Tatsaechliche PD-Kapazitaet (A-3.4 Teil 3) -- die angehaengte, nicht die Konstante.
pub fn pd_capacity() -> usize {
    CAPS.read().pds.capacity()
}

pub fn cap_peaks() -> (usize, usize, usize, usize) {
    let g = CAPS.read();
    let (ps, cs) = g.cspace.peak_slots();
    let (po, co) = g.cspace.peak_objects();
    (ps, cs, po, co)
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
    vspace_map_masked(asid, base, len, perm, None)
}

/// Wie [`vspace_map`], aber die bei Bedarf angelegten L3-Tabellen kommen aus `mask` (todo A1).
fn vspace_map_masked(
    asid: u16,
    base: u64,
    len: u64,
    perm: hal::mmu::UserPerm,
    mask: Option<sel4lake_mem::ColorMask>,
) -> bool {
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
                hal::mmu::vspace_map_page(l2, p, perm, &mut || {
                    mem_alloc_masked(4096, 4096, mask).map(|c| c.base())
                });
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
    VSPACES.lock()[asid as usize - 1] = VSpaceEnt { used: false, l1: 0, l2: 0, stripe: None };
    // 4. Farbstreifen zuletzt (B-4.2). **Nach** dem Slot, nicht davor: gäbe man den Streifen frei,
    // solange diese VSpace noch steht, könnte eine neue PD ihn belegen und läge kurzzeitig auf
    // denselben Farben wie eine noch existierende — genau die Überschneidung, die B-4.2 verhindern
    // soll, nur in einem schmalen Fenster statt dauerhaft.
    if let Some(i) = ent.stripe {
        crate::colors::release_stripe(i);
    }
}

/// Den belegten Farbstreifen an die Lebensdauer einer VSpace binden (B-4.2).
///
/// Getrennt von `create_vspace_masked`, weil dort nur die *Maske* bekannt ist, nicht die
/// Streifennummer — und die Freigabe braucht die Nummer. Wird der Aufruf vergessen, ist der
/// Streifen für immer belegt: nach vier vergessenen PDs entsteht keine gefärbte mehr. Deshalb
/// steht er unmittelbar neben dem `claim_stripe()` seines einzigen Aufrufers.
fn vspace_bind_stripe(asid: u16, stripe: u32) {
    if asid == 0 || asid as usize > MAX_VSPACES {
        return;
    }
    VSPACES.lock()[asid as usize - 1].stripe = Some(stripe);
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

/// **Farbige Variante von [`spawn_isolated`]** (todo A1): die private Region der PD wird aus
/// einem Farbsatz alloziert, der zu keinem anderen Satz überlappt — zwei so erzeugte PDs können
/// sich im Last-Level-Cache nicht gegenseitig verdrängen und damit auch nicht über Laufzeit
/// beobachten.
///
/// **Warum eine eigene Funktion und nicht ein Schalter in [`spawn_isolated`]:** dort ist die
/// Region 2 MiB groß und wird als *ein* Blockdeskriptor gemappt. 2 MiB sind 512 Seiten, also 512
/// aufeinanderfolgende Farben — bei den auf dem Testaufbau gemessenen 256 Farben überstreicht ein
/// einziger Block jede Farbe zweimal. Der Blockdeskriptor und die Färbung schließen einander
/// aus; das ist keine Nachlässigkeit, sondern Arithmetik. Diese Funktion nimmt deshalb die
/// kleinere Region ([`colors::region_bytes`]) und den **seitenweisen** Mapping-Pfad, der ohnehin
/// existiert. Der Preis steht damit im Code und nicht in einer Fußnote: statt eines PTE nun
/// `region_bytes / 4 KiB` Stück, plus eine L3-Tabelle.
///
/// **Was hier NICHT gefärbt ist:** der Kernel-Stack der PD (16 KiB aus dem Pool) und die
/// Seitentabellen. Beide gehören dem Kernel, nicht dem Subjekt; die Zusicherung lautet
/// ausdrücklich „die *private Region* zweier PDs teilt keine Cache-Farbe", nicht „die PDs teilen
/// keinerlei Cache-Zeile".
///
/// `None`, wenn die Maske leer ist, kein passend gefärbter Speicher frei ist, oder die
/// VSpace/Thread-Erzeugung scheitert. Gibt `(ThreadId, region_base)`.
pub fn spawn_isolated_colored(
    entry: usize,
    arg: usize,
    prio: u8,
    mask: sel4lake_mem::ColorMask,
) -> Option<(ThreadId, u64)> {
    // Ohne Streifennummer: der Aufrufer hat die Maske selbst gewaehlt (Selbsttest) und fuehrt
    // keine Belegung. Fuer PDs ist `spawn_isolated_colored_auto` der richtige Weg.
    spawn_isolated_colored_inner(entry, arg, prio, mask, None)
}

/// **Gefaerbte PD mit gefuehrter Streifenvergabe** (B-4.2) — der Weg, den B-4.1 zum Normalfall
/// macht.
///
/// Belegt einen freien Farbstreifen, bindet ihn an die Lebensdauer der VSpace und gibt ihn bei
/// jedem Fehlschlag wieder frei. **`None` heisst: kein Streifen frei** — und dann entsteht die PD
/// gar nicht erst. Es gibt bewusst keine ungefaerbte Rueckfallebene: eine Trennung, die unter
/// Last leise verschwindet, waere schlimmer als gar keine, weil niemand mehr weiss, welche PD
/// getrennt ist und welche nicht.
pub fn spawn_isolated_colored_auto(entry: usize, arg: usize, prio: u8) -> Option<(ThreadId, u64)> {
    let (i, mask) = crate::colors::claim_stripe()?;
    match spawn_isolated_colored_inner(entry, arg, prio, mask, Some(i)) {
        Some(r) => Some(r),
        None => {
            // Scheitert das Anlegen NACH dem Belegen, muss der Streifen zurueck -- sonst ist er
            // fuer immer weg, und nach vier Fehlschlaegen gibt es keine gefaerbte PD mehr.
            crate::colors::release_stripe(i);
            None
        }
    }
}

fn spawn_isolated_colored_inner(
    entry: usize,
    arg: usize,
    prio: u8,
    mask: sel4lake_mem::ColorMask,
    stripe: Option<u32>,
) -> Option<(ThreadId, u64)> {
    let core = hal::cpu::core_id();
    let colors = crate::colors::count();
    let region_sz = crate::colors::region_bytes();
    // Region, Kernel-Stack UND Seitentabellen dieser PD kommen aus demselben Streifen.
    let kbase = claim_user_kstack_masked(Some(mask))?;

    let cap = match MEM.lock().alloc_colored(region_sz, crate::colors::PAGE, colors, mask) {
        Some(c) => c,
        None => {
            release_user_kstack(kbase);
            return None;
        }
    };
    let (rbase, rlen) = (cap.base(), cap.len());
    zero_phys(rbase, rlen); // wie `mem_alloc`: nichts geht ungenullt an ein Subjekt
    if rbase < hal::mmu::USER_RAM_MIN || rbase + rlen > hal::mmu::GIB1_END {
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kbase);
        return None;
    }
    let Some((asid, l1)) = create_vspace_masked(Some(mask)) else {
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kbase);
        return None;
    };
    // Ab hier gibt `vspace_teardown(asid)` den Streifen selbst zurueck -- deshalb sofort binden
    // und nicht erst am Ende: jeder Fehlerausgang unten ruft teardown.
    if let Some(i) = stripe {
        vspace_bind_stripe(asid, i);
    }
    // Seitenweise statt Block — s. Funktionsdoku. `vspace_map` legt die L3 bei Bedarf an und
    // `vspace_teardown` sammelt sie beim Abbau wieder ein.
    if !vspace_map_masked(asid, rbase, rlen, hal::mmu::UserPerm::Rw, Some(mask)) {
        vspace_teardown(asid);
        MEM.lock().free_region(PhysRegion::new(rbase, rlen));
        release_user_kstack(kbase);
        return None;
    }
    let packed = ((asid as u64) << 48) | l1;

    let (user_base, user_len) = (rbase as usize, rlen as usize);
    let tid = {
        let mut sched = SCHEDS[core].lock();
        let r = sched.spawn_user(core, entry, arg, kbase, USER_KSTACK_SIZE, user_base, user_len, prio);
        if let Some(t) = r {
            fp_reset_slot(t.slot());
        }
        r
    };
    match tid {
        Some(t) => {
            record_user_kstack(t.slot(), kbase);
            set_vspace_of(t.slot(), packed);
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
    // Der Abbau einer PD ist **der** realistische Weg in „quiesziert nie": das Gerät hängt, und
    // niemand ist mehr da, der auf einen Fehlercode reagieren könnte. Hier darf der
    // Pending-Zustand entstehen, ohne als Anomalie gezählt zu werden — überall sonst will man
    // ihn sehen (`dma_pending_stats().1`).
    let _kill = KillScope::enter();
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

/// **Stack-VA-Fenster** geladener Programme (fest, getrennt von der Code-Link-VA) + Größe. Der
/// Stack wird **nicht-identity** an diese VA gemappt.
///
/// Die Adresse ist architekturabhängig, weil das mappbare VA-Fenster einer isolierten VSpace es
/// ist. Auf aarch64 liegt es in GiB 1 (Code bei `0x4100_0000`, Stack darüber). Auf x86 hat eine
/// isolierte VSpace ihr eigenes Page Directory **nur für GiB 0**; GiB 1..3 teilen sich alle
/// isolierten Adressräume statisch, und `vspace_map_page_at` weist deshalb jede VA ≥ 1 GiB ab.
/// Der aarch64-Wert wäre dort schlicht nicht mappbar — der Ladepfad schlüge beim Stack fehl, und
/// zwar erst nach dem Kopieren aller Segmente.
#[cfg(target_arch = "aarch64")]
const LOADED_STACK_VA: u64 = 0x43F0_0000;
/// x86: knapp unter der 1-GiB-Grenze, weit über der Code-Link-VA `0x2000_0000` (`user-x86.ld`).
#[cfg(not(target_arch = "aarch64"))]
const LOADED_STACK_VA: u64 = 0x3FF0_0000;
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
pub fn load_into_pd(
    img: &ElfImage,
    pd: usize,
    endow: &[(usize, CapPtr)],
    boot_arg: usize,
) -> Option<ThreadId> {
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
            boot_arg,
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
pub fn load_elf(
    img: &ElfImage,
    domain: Domain,
    endow: &[(usize, CapPtr)],
    boot_arg: usize,
) -> Option<(ThreadId, usize)> {
    let pd = create_pd_in_domain(domain)?;
    match load_into_pd(img, pd, endow, boot_arg) {
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
    if ntfn < ntfns().len() {
        ntfns()[ntfn].lock().pending_badge()
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
            if ntfn < ntfns().len() {
                ntfns()[ntfn].lock().signal_from_kernel(&mut ops, badge);
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
    /// **Physische** Basis der Region — was der Aufrufer besitzt. Die **IOVA** vergibt der
    /// Enforcer aus dem Fenster des Übersetzungskontexts und meldet sie über den Rückgabewert
    /// von [`DmaEnforcer::attach`] zurück; der Aufrufer kann sie nicht wählen.
    pub pa: Pa,
    pub len: u64,
    #[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
    pub backend_pd: usize,
    /// ext-24: Gerät liest nur (read-only -> schreibgeschützt). Default `false` (RW).
    pub ro: bool,
    /// ext-24: Coherent -> Normal-Cacheable. Default `false` (Non-Cacheable, ext-23-Verhalten).
    pub cacheable: bool,
}

impl DmaBinding {
    /// Rückwärtskompatible Bindung (ext-23-Semantik: bidirektional, non-cacheable).
    pub fn new(stream_id: u32, pa: Pa, len: u64) -> Self {
        Self {
            stream_id,
            pa,
            len,
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
    /// Gibt die **zugeteilte IOVA** zurück (`None` bei Fehlschlag). Der Aufrufer erfährt die
    /// Gerätesicht erst hier — sie stammt aus dem Fenster des Übersetzungskontexts.
    fn attach(&self, binding: &DmaBinding) -> Option<Iova>;
    /// Durchsetzung entziehen (VOR VSpace-Unmap + `free_region`): danach kann das Gerät nicht
    /// mehr in die Region DMAen (DMA-use-after-free-sicher).
    fn detach(&self, binding: &DmaBinding);
    /// **Gebündelte Finalisierung** (ext-37): alle genannten Regionen `(pa, len)` stilllegen,
    /// aus den Übersetzungstabellen entfernen und **einmal** synchronisieren. `ok[i]` sagt, ob
    /// Region `i` danach freigegeben werden darf.
    ///
    /// Gebündelt, weil ein `revoke` über einen CDT-Teilbaum N DMA-Caps auf einmal finalisiert:
    /// einzeln durchgereicht wären das N Quiesce/Flush/`CMD_SYNC`-Zyklen — dieselbe Form, die
    /// beim Multi-SID-Detach schon einmal die falsche war. Erst alle entwaffnen, alle spülen,
    /// alle unmappen, **ein** Sync, dann freigeben.
    ///
    /// Default: es gab nie eine Übersetzung (kein Enforcer / `attach` liefert `None`), also darf
    /// alles freigegeben werden. Das ist kein Freibrief, sondern die Wahrheit über x86 heute.
    ///
    /// `ctx_of` ist **geliehene Kratzfläche** (A-3.3): je Region der Index des betroffenen
    /// Übersetzungskontexts, gebraucht zwischen Phase 1 und Phase 4. Sie lag bis hierher als
    /// `[usize; MAX_FINALIZE]` auf dem Kernelstack — dieselbe Kopplung an `NOBJECTS`, die A-3.3
    /// beseitigt, nur eine Ebene tiefer. Wer sie nicht braucht (dieser Default), ignoriert sie.
    fn finalize(&self, regions: &[(u64, u64)], ok: &mut [bool], _ctx_of: &mut [usize]) {
        for b in ok.iter_mut().take(regions.len()) {
            *b = true;
        }
    }
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
        /// STE einer StreamID **direkt** überschreiben — ausschließlich für die
        /// Sensitivitätskontrolle des Kronjuwel-Tests (Bypass-STE, s. `build_ste_bypass`).
        /// Nicht Teil der Durchsetzungs-API: hier wird die Durchsetzung absichtlich aufgehoben.
        pub fn override_ste(&self, sid: u32, ste: &[u64; 8]) -> bool {
            if !self.active.load(Ordering::Acquire) {
                return false;
            }
            let strtab = self.strtab_phys.load(Ordering::Acquire);
            let cmdq = self.cmdq_phys.load(Ordering::Acquire);
            let prod = self.cmdq_prod.load(Ordering::Acquire);
            let (next, ok) = hal::smmu::write_ste_and_sync(strtab, cmdq, prod, sid, ste);
            self.cmdq_prod.store(next, Ordering::Release);
            ok
        }
        /// Die reguläre Stage-1-STE des Kontexts der StreamID wiederherstellen.
        pub fn restore_ste(&self, sid: u32) -> bool {
            let cd = {
                let t = DMA_CTX.lock();
                match t.iter().find(|c| c.used && c.sids.contains(&sid)) {
                    Some(c) => c.cd,
                    None => return false,
                }
            };
            self.override_ste(sid, &hal::smmu::build_ste_stage1(cd))
        }
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
        fn attach(&self, binding: &DmaBinding) -> Option<Iova> {
            if !self.active.load(Ordering::Acquire) {
                return None;
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
                    return None;
                };
                let Some(cd) = SmmuV3Enforcer::alloc_zeroed(hal::smmu::CD_BYTES) else {
                    // Stage-1 (l1) ist bereits alloziert -> freigeben, sonst Frame-Leck.
                    let mut mem = MEM.lock();
                    hal::smmu::free_stage1(l1, &mut |x| {
                        mem.free_region(PhysRegion::new(x, 4096));
                    });
                    return None;
                };
                hal::smmu::write_cd(cd, l1);
                let Some(slot) = t.iter().position(|c| !c.used) else {
                    // l1 + cd sind bereits alloziert (kein DmaCtx-Slot frei) -> beide freigeben.
                    let mut mem = MEM.lock();
                    hal::smmu::free_stage1(l1, &mut |x| {
                        mem.free_region(PhysRegion::new(x, 4096));
                    });
                    mem.free_region(PhysRegion::new(cd, 4096));
                    return None;
                };
                t[slot] = DmaCtx::EMPTY;
                t[slot].used = true;
                t[slot].l1 = l1;
                t[slot].cd = cd;
                t[slot].sids[0] = binding.stream_id;
                ctx_assign_window(&mut t[slot], slot); // IOVA-Fenster dieses Kontexts
                ci = Some(slot);
            }
            let slot = ci.unwrap();
            let (l1, cd) = (t[slot].l1, t[slot].cd);
            // **Hier entsteht die Gerätesicht**: eine IOVA aus dem Fenster des Kontexts, ohne
            // arithmetische Beziehung zur PA (kein `PA + Offset`) — sonst funktionierte ein
            // Passthrough-Enforcer, der die IOVA als PA liest, für einen Teil der Regionen
            // zufällig weiter, und das laute Scheitern fiele aus.
            let iova = match ctx_alloc_iova(
                &mut t[slot],
                slot,
                binding.len,
                device_addr_bits(binding.stream_id),
            ) {
                Ok(v) => v,
                Err(e) => {
                    note_iova_reject(e);
                    if is_new_ctx {
                        let mut mem = MEM.lock();
                        hal::smmu::free_stage1(l1, &mut |x| {
                            mem.free_region(PhysRegion::new(x, 4096));
                        });
                        mem.free_region(PhysRegion::new(cd, 4096));
                        drop(mem);
                        t[slot] = DmaCtx::EMPTY;
                    }
                    return None;
                }
            };
            let region = DmaRegion { iova, pa: binding.pa, len: binding.len };
            // Region richtungs-/kohärenz-spezifisch additiv in die Kontext-Stage-1-Tabelle einhängen.
            let mut alloc = || SmmuV3Enforcer::alloc_zeroed(4096);
            if !hal::smmu::stage1_map_region(
                l1,
                region.iova.raw(), // Index in der Tabelle = Gerätesicht
                region.pa.raw(),   // Blatt-Eintrag = CPU-Sicht
                region.len,
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
                return None;
            }
            let Some(ri) = t[slot].regs.iter().position(|r| r.is_empty()) else {
                // Region wurde in die Stage-1-Tabelle gemappt, aber es ist kein regs-Slot frei (nur bei
                // einem BESTEHENDEN Kontext moeglich -> dessen regs sind voll; ein neuer Kontext hat
                // leere regs). Das Mapping zuruecknehmen, sonst bleibt es untracked in der Tabelle.
                hal::smmu::stage1_unmap_region(l1, region.iova.raw(), region.len);
                return None;
            };
            t[slot].regs[ri] = region;
            // Neuer Kontext: STE installieren. Sonst: nur TLBI+SYNC (STE zeigt schon auf den CD).
            // Bus-Master erst **nach** der Übersetzung scharf schalten (Reihenfolge: erst darf
            // es irgendwo hin, dann darf es überhaupt). Ohne das wäre ein Gerät nach einem
            // vollständigen Detach/Re-Attach-Zyklus tot — beim letzten Detach bleibt Bus-Master
            // bewusst aus, und niemand sonst setzt es wieder.
            ctx_arm_bus_master(&mut t[slot], binding.stream_id);
            let prod = self.cmdq_prod.load(Ordering::Acquire);
            let (next, ok) = if is_new_ctx {
                let ste = hal::smmu::build_ste_stage1(cd);
                hal::smmu::write_ste_and_sync(strtab, cmdq, prod, binding.stream_id, &ste)
            } else {
                hal::smmu::tlbi_sync(cmdq, prod)
            };
            self.cmdq_prod.store(next, Ordering::Release);
            if ok {
                Some(region.iova)
            } else {
                None
            }
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
            let Some(slot) = t
                .iter()
                .position(|c| c.used && c.sids.contains(&binding.stream_id))
            else {
                return;
            };
            let _ = ctx_quiesce(&mut t[slot]);
            let l1 = t[slot].l1;
            // Region aus der Stage-1-Tabelle entfernen (danach kann das Gerät NICHT mehr dorthin
            // DMAen) + TLBI+SYNC. DMA-use-after-free-sicher (vor VSpace-Unmap + free_region).
            // Der Aufrufer kennt nur die **PA** — die IOVA steht in der aufgezeichneten Region.
            // (Sie aus der PA zu berechnen ginge nicht und soll auch nicht gehen: zwischen den
            // beiden Achsen besteht bewusst keine arithmetische Beziehung.)
            if let Some(ri) = t[slot]
                .regs
                .iter()
                .position(|r| !r.is_empty() && r.pa == binding.pa && r.len == binding.len)
            {
                hal::smmu::stage1_unmap_region(l1, t[slot].regs[ri].iova.raw(), t[slot].regs[ri].len);
                t[slot].regs[ri] = DmaRegion::EMPTY;
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
            // Bedingung **beim Restore** ausgewertet, nicht beim Entwaffnen gecacht: die
            // Pending-Intent-Mechanik kann Quiesce-Tiefe und Regionenzahl auseinanderziehen (ein
            // `attach` bei Tiefe > 0 erhöht die Regionen, ohne die Tiefe zu ändern). Effektiv gilt
            // damit  BME_neu = gespeichertes_BME  UND  Regionen(ctx) > 0,  beides zum Zeitpunkt
            // 1→0 unter demselben Lock.
            let still_in_use = !t[slot].regs.iter().all(|r| r.is_empty());
            ctx_release(&mut t[slot], still_in_use);
            // Letzte Region weg -> Kontext abbauen: alle STEs der Gruppe invalidieren, Stage-1 + CD frei.
            if t[slot].regs.iter().all(|r| r.is_empty()) {
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
        fn finalize(&self, regions: &[(u64, u64)], ok: &mut [bool], ctx_of: &mut [usize]) {
            if !self.active.load(Ordering::Acquire) {
                for b in ok.iter_mut().take(regions.len()) {
                    *b = true; // keine Durchsetzung aktiv -> es gab nie eine Übersetzung
                }
                return;
            }
            let strtab = self.strtab_phys.load(Ordering::Acquire);
            let cmdq = self.cmdq_phys.load(Ordering::Acquire);
            let mut t = DMA_CTX.lock();

            // Phase 1 — **alle** betroffenen Kontexte stilllegen (je Kontext alle StreamIDs).
            // Ein Kontext kann mehrere der Regionen tragen; die Stilllegung ist verschachtelbar,
            // also wird sie hier einmal je *Region* genommen und in Phase 4 ebenso oft gelöst.
            // `ctx_of` kommt vom Aufrufer (A-3.3). `quiesced` gab es hier als zweites Array
            // derselben Groesse -- gelesen wurde es nie ausser in der Zeile darunter; eine lokale
            // Variable tut dasselbe.
            let nfin = regions.len().min(ok.len()).min(ctx_of.len());
            for e in ctx_of[..nfin].iter_mut() {
                *e = usize::MAX;
            }
            for (i, &(pa, len)) in regions.iter().enumerate().take(nfin) {
                let Some(slot) = t.iter().position(|c| {
                    c.used
                        && c.regs
                            .iter()
                            .any(|r| !r.is_empty() && r.pa == Pa::new(pa) && r.len == len)
                }) else {
                    // Keine lebende Übersetzung (z. B. bereits explizit detacht) -> nichts zu
                    // entwaffnen, Freigabe zulässig.
                    ok[i] = true;
                    continue;
                };
                ctx_of[i] = slot;
                ok[i] = ctx_quiesce(&mut t[slot]);
            }

            // Phase 2 — alle Regionen unmappen (IOVA-Achse). Der Unmap ist **immer** zwingend,
            // auch wenn die Stilllegung nicht bestätigt ist: eine stehende Übersetzung auf eine
            // Region, die gleich freigegeben würde, ist genau der geräteseitige Use-after-free.
            // Zurückgehalten wird nur die *Freigabe*.
            for (i, &(pa, len)) in regions.iter().enumerate().take(nfin) {
                let slot = ctx_of[i];
                if slot == usize::MAX {
                    continue;
                }
                let l1 = t[slot].l1;
                if let Some(ri) = t[slot]
                    .regs
                    .iter()
                    .position(|r| !r.is_empty() && r.pa == Pa::new(pa) && r.len == len)
                {
                    hal::smmu::stage1_unmap_region(l1, t[slot].regs[ri].iova.raw(), t[slot].regs[ri].len);
                    t[slot].regs[ri] = DmaRegion::EMPTY;
                }
            }

            // Phase 3 — **ein** TLBI+SYNC für den ganzen Stapel.
            let prod = self.cmdq_prod.load(Ordering::Acquire);
            let (next, _) = hal::smmu::tlbi_sync(cmdq, prod);
            self.cmdq_prod.store(next, Ordering::Release);

            // Phase 4 — Stilllegung lösen und leere Kontexte abbauen.
            for i in 0..nfin {
                let slot = ctx_of[i];
                if slot == usize::MAX || !t[slot].used {
                    continue;
                }
                let still_in_use = !t[slot].regs.iter().all(|r| r.is_empty());
                ctx_release(&mut t[slot], still_in_use);
                if !still_in_use {
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
        }
        fn audit(&self) -> u32 {
            if !self.active.load(Ordering::Acquire) {
                return 0; // nicht initialisiert -> keine Durchsetzungs-Aussage (D0/D1)
            }
            // Aktiv: keine globalen Fehler + keine unerwarteten Translation-Faults.
            if hal::smmu::gerror() != 0 {
                return 1;
            }
            if !hal::iommu::faults_empty() {
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
    /// Ein genulltes, ausgerichtetes Frame für Root-/Kontext-/SLPT-Tabellen.
    fn alloc_zeroed(len: u64) -> Option<u64> {
        let len = (len + 4095) & !4095;
        let base = mem_alloc(len, len.next_power_of_two().max(4096)).map(|c| c.base())?;
        // SAFETY: frisch allozierter, identity-gemappter RAM-Block; exklusiv hier beschrieben.
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, len as usize) };
        Some(base)
    }
}

/// Domain-IDs werden **ab 1** vergeben.
///
/// Bei `CAP.CM == 1` — dem Zustand dieses Aufbaus — reserviert die Spezifikation DID 0 für das
/// Caching nicht-präsenter Einträge. Das ist damit kein theoretischer Fall, sondern eine
/// Bedingung, unter der die Vergabe steht.
#[cfg(not(target_arch = "aarch64"))]
static NEXT_DID: AtomicU32 = AtomicU32::new(1);

/// Die **Requester-IDs der Isolationsgruppe**, zu der `stream_id` gehört.
///
/// Die Gruppe ist die Isolationsgranularität, nicht das Gerät — und ihre RID-Menge enthält auch
/// die RID einer Bridge, hinter der die eigentlichen Endpunkte sitzen. Für die muss ein
/// Kontexteintrag existieren, obwohl dort kein zuteilbares Gerät ist: sonst kämen genau die
/// Transaktionen ungeleitet an, die die Bridge-RID tragen.
///
/// Umgekehrt beim Abbau — alle Einträge räumen — und beim Stilllegen: **alle** RIDs der Gruppe
/// entwaffnen, nicht nur die auslösende. Das ist die ARM-Falle aus `13810e9`, wörtlich; sie wird
/// hier dadurch vermieden, dass die Gruppen-RIDs in `DmaCtx::sids` landen und damit durch
/// dieselbe `ctx_quiesce`-Schleife laufen.
#[cfg(not(target_arch = "aarch64"))]
fn group_rids(stream_id: u32, out: &mut [u32]) -> usize {
    let Some(tbl) = hal::acpi::dmar_table() else {
        out[0] = stream_id;
        return 1;
    };
    let info = hal::dmar::parse(tbl);
    let mut topo = [hal::dmar::DevNode::EMPTY; hal::dmar::MAX_DEVS];
    let n = hal::pcie::read_topology(&mut topo);
    let g = hal::dmar::build_groups(&info, &topo[..n]);
    let Some(i) = (0..n).find(|&i| topo[i].rid() == stream_id) else {
        out[0] = stream_id;
        return 1;
    };
    if g.excluded[i].is_some() {
        return 0; // nicht zuteilbar (RMRR / keine Einheit / Gruppe über Einheiten)
    }
    let gi = g.group_of[i];
    let mut k = 0;
    for r in 0..g.n_aliases[gi] {
        if k < out.len() {
            out[k] = g.aliases[gi][r];
            k += 1;
        }
    }
    k
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
        let mut alloc = || VtdEnforcer::alloc_zeroed(4096);
        hal::vtd::init(root.base(), &mut alloc)
    }

    fn attach(&self, binding: &DmaBinding) -> Option<Iova> {
        let caps = hal::vtd::caps_common()?;
        if !caps.usable() || !hal::vtd::enabled() {
            return None;
        }
        let root = hal::vtd::root_table();
        if root == 0 {
            return None;
        }
        // Die RID-Menge der Gruppe — leer heißt: nicht zuteilbar, und das ist ein Fehlschlag,
        // keine Zuteilung mit Lücke.
        let mut rids = [0u32; MAX_CTX_SIDS];
        let n_rids = group_rids(binding.stream_id, &mut rids);
        if n_rids == 0 {
            return None;
        }
        let levels = caps.agaw_levels;
        let mut t = DMA_CTX.lock();
        let mut ci = t
            .iter()
            .position(|c| c.used && c.sids.contains(&binding.stream_id));
        let is_new_ctx = ci.is_none();
        if ci.is_none() {
            let slpt = Self::alloc_zeroed(4096)?;
            let Some(slot) = t.iter().position(|c| !c.used) else {
                let mut mem = MEM.lock();
                mem.free_region(PhysRegion::new(slpt, 4096));
                return None;
            };
            t[slot] = DmaCtx::EMPTY;
            t[slot].used = true;
            t[slot].l1 = slpt; //                      x86: Wurzel der Second-Level-Pagetable
            t[slot].cd = NEXT_DID.fetch_add(1, Ordering::Relaxed) as u64; // x86: Domain-ID
            // **Alle** RIDs der Gruppe eintragen, nicht nur die auslösende.
            for (k, &r) in rids[..n_rids].iter().enumerate() {
                t[slot].sids[k] = r;
            }
            ctx_assign_window(&mut t[slot], slot);
            ci = Some(slot);
        }
        let slot = ci.unwrap();
        let (slpt, did) = (t[slot].l1, t[slot].cd as u16);
        let iova = match ctx_alloc_iova(
            &mut t[slot],
            slot,
            binding.len,
            device_addr_bits(binding.stream_id),
        ) {
            Ok(v) => v,
            Err(e) => {
                note_iova_reject(e);
                if is_new_ctx {
                    let mut mem = MEM.lock();
                    mem.free_region(PhysRegion::new(slpt, 4096));
                    drop(mem);
                    t[slot] = DmaCtx::EMPTY;
                }
                return None;
            }
        };
        // Richtung fällt heraus statt hinzuzukommen: Präsenz *ist* R|W. `ro` heißt „das Gerät
        // liest nur" -> kein W-Bit, und der Puffer ist gegen das Gerät schreibgeschützt.
        let mut alloc = || VtdEnforcer::alloc_zeroed(4096);
        // SAFETY: `slpt` ist eine frisch genullte, identity-gemappte Tabellenwurzel; `alloc`
        // liefert ebensolche Frames.
        let mapped = unsafe {
            hal::vtd::slpt_map(
                slpt,
                levels,
                iova.raw(),
                binding.pa.raw(),
                binding.len,
                true,
                !binding.ro,
                &mut alloc,
            )
        };
        if !mapped {
            if is_new_ctx {
                let mut mem = MEM.lock();
                mem.free_region(PhysRegion::new(slpt, 4096));
                drop(mem);
                t[slot] = DmaCtx::EMPTY;
            }
            return None;
        }
        let region = DmaRegion { iova, pa: binding.pa, len: binding.len };
        let Some(ri) = t[slot].regs.iter().position(|r| r.is_empty()) else {
            // SAFETY: eben gemappt, dieselbe Tabelle.
            unsafe { hal::vtd::slpt_unmap(slpt, levels, iova.raw(), binding.len) };
            return None;
        };
        t[slot].regs[ri] = region;
        if is_new_ctx {
            for k in 0..MAX_CTX_SIDS {
                let r = t[slot].sids[k];
                if r == u32::MAX {
                    continue;
                }
                // SAFETY: `root` stammt aus dem Bring-up und ist identity-gemappt.
                if !unsafe { hal::vtd::context_set(root, r, slpt, did, levels, &mut alloc) } {
                    return None;
                }
            }
        }
        ctx_arm_bus_master(&mut t[slot], binding.stream_id);
        // Unbedingt invalidieren — Kontext-Cache vor IOTLB. Mit `CAP.CM == 1` ist das auch nach
        // dem **Anlegen** Pflicht; die konservative Variante hält auch dort, wo sie es nicht ist.
        if !hal::vtd::sync_tables() {
            return None;
        }
        Some(iova)
    }

    fn detach(&self, binding: &DmaBinding) {
        let Some(caps) = hal::vtd::caps_common() else { return };
        let root = hal::vtd::root_table();
        let mut t = DMA_CTX.lock();
        let Some(slot) = t
            .iter()
            .position(|c| c.used && c.sids.contains(&binding.stream_id))
        else {
            return;
        };
        let _ = ctx_quiesce(&mut t[slot]); // über ALLE RIDs der Gruppe
        let slpt = t[slot].l1;
        if let Some(ri) = t[slot]
            .regs
            .iter()
            .position(|r| !r.is_empty() && r.pa == binding.pa && r.len == binding.len)
        {
            // SAFETY: gültige Tabellenwurzel, Region stammt aus dieser Tabelle.
            unsafe {
                hal::vtd::slpt_unmap(slpt, caps.agaw_levels, t[slot].regs[ri].iova.raw(), t[slot].regs[ri].len)
            };
            t[slot].regs[ri] = DmaRegion::EMPTY;
        }
        hal::vtd::sync_tables();
        let still_in_use = !t[slot].regs.iter().all(|r| r.is_empty());
        ctx_release(&mut t[slot], still_in_use);
        if !still_in_use {
            vtd_teardown_ctx(&mut t[slot], root, caps.agaw_levels);
        }
    }

    fn finalize(&self, regions: &[(u64, u64)], ok: &mut [bool], ctx_of: &mut [usize]) {
        let Some(caps) = hal::vtd::caps_common() else {
            for b in ok.iter_mut().take(regions.len()) {
                *b = true; // keine Einheit -> es gab nie eine Übersetzung
            }
            return;
        };
        let root = hal::vtd::root_table();
        let levels = caps.agaw_levels;
        let mut t = DMA_CTX.lock();
        // Kratzflaeche vom Aufrufer (A-3.3) statt `[usize; MAX_FINALIZE]` auf dem Kernelstack.
        let nfin = regions.len().min(ok.len()).min(ctx_of.len());
        for e in ctx_of[..nfin].iter_mut() {
            *e = usize::MAX;
        }
        // Phase 1: alle betroffenen Kontexte stilllegen (je Kontext alle RIDs der Gruppe).
        for (i, &(pa, len)) in regions.iter().enumerate().take(nfin) {
            let Some(slot) = t.iter().position(|c| {
                c.used
                    && c.regs
                        .iter()
                        .any(|r| !r.is_empty() && r.pa == Pa::new(pa) && r.len == len)
            }) else {
                ok[i] = true;
                continue;
            };
            ctx_of[i] = slot;
            ok[i] = ctx_quiesce(&mut t[slot]);
        }
        // Phase 2: alle Regionen unmappen — immer, auch ohne bestätigte Stilllegung.
        for (i, &(pa, len)) in regions.iter().enumerate().take(nfin) {
            let slot = ctx_of[i];
            if slot == usize::MAX {
                continue;
            }
            let slpt = t[slot].l1;
            if let Some(ri) = t[slot]
                .regs
                .iter()
                .position(|r| !r.is_empty() && r.pa == Pa::new(pa) && r.len == len)
            {
                // SAFETY: gültige Tabellenwurzel.
                unsafe {
                    hal::vtd::slpt_unmap(slpt, levels, t[slot].regs[ri].iova.raw(), t[slot].regs[ri].len)
                };
                t[slot].regs[ri] = DmaRegion::EMPTY;
            }
        }
        // Phase 3: **eine** Invalidierung für den ganzen Stapel.
        hal::vtd::sync_tables();
        // Phase 4: Stilllegung lösen, leere Kontexte abbauen.
        for i in 0..nfin {
            let slot = ctx_of[i];
            if slot == usize::MAX || !t[slot].used {
                continue;
            }
            let still_in_use = !t[slot].regs.iter().all(|r| r.is_empty());
            ctx_release(&mut t[slot], still_in_use);
            if !still_in_use {
                vtd_teardown_ctx(&mut t[slot], root, levels);
            }
        }
    }

    fn audit(&self) -> u32 {
        // Ist die Einheit hochgefahren, MUSS die Übersetzung aktiv sein — sonst liefe DMA
        // ungeschützt, obwohl der Kernel meint, sie sei an.
        if hal::vtd::present() && !hal::vtd::enabled() {
            return 1;
        }
        if hal::vtd::present() && !hal::iommu::faults_empty() {
            return 2;
        }
        0
    }
    fn is_active(&self) -> bool {
        hal::vtd::enabled()
    }
}

/// Einen leeren Übersetzungskontext abbauen: **alle** Kontexteinträge der Gruppe räumen,
/// invalidieren, dann die Tabellen freigeben.
#[cfg(not(target_arch = "aarch64"))]
fn vtd_teardown_ctx(c: &mut DmaCtx, root: u64, levels: u32) {
    for k in 0..MAX_CTX_SIDS {
        let r = c.sids[k];
        if r != u32::MAX && root != 0 {
            // SAFETY: gültige Root-Tabelle.
            unsafe { hal::vtd::context_clear(root, r) };
        }
    }
    hal::vtd::sync_tables();
    let slpt = c.l1;
    {
        let mut mem = MEM.lock();
        // SAFETY: `slpt` ist die Wurzel dieses Kontexts, ab hier von niemandem mehr referenziert.
        unsafe {
            hal::vtd::slpt_free(slpt, levels, &mut |x| {
                mem.free_region(PhysRegion::new(x, 4096));
            })
        };
    }
    *c = DmaCtx::EMPTY;
}

/// Granularität der Guard-Bänder **und** der IOVA-Ausrichtung.
///
/// Bewusst die größte Block-Granularität der Stage-1 und nicht 4 KiB: läge eine Region nur
/// seitengenau von ihrer Nachbarin getrennt, könnte ein Block-Mapping über die Lücke
/// hinwegreichen — die Eindämmung stünde auf dem Papier und nicht in der Tabelle.
///
/// **Der Wert hängt an der Granule-Wahl.** 2 MiB ist der Block bei 4-KiB-Granule, und der
/// Kernel fährt Stage 1 ausschließlich mit 4 KiB (`CD.TG0 = 0`, s. `write_cd`). Bei 64-KiB-
/// Granule wäre der Block 512 MiB, und ein 2-MiB-Band ließe sich von einem Block-Mapping
/// überbrücken — dann wäre dieser Wert falsch, nicht bloß knapp. Wer `TG0` ändert, muss ihn
/// mitändern.
const IOVA_GUARD: u64 = 2 * 1024 * 1024;

/// Eingangsbreite der Stage-1 (`CD.T0SZ = 25` → 39 Bit). Jede IOVA muss darunter bleiben.
const S1_INPUT_LIMIT: u64 = 1u64 << 39;

/// Hat die SMMU-Event-Queue in diesem Lauf **nachweislich gesprochen**?
///
/// Ein Oracle, das über Abwesenheit entscheidet („keine Faults"), braucht als Vorbedingung den
/// Nachweis, dass es überhaupt sprechen kann. Zweimal in Folge hat genau diese Lücke hier
/// zugeschlagen: `dma_audit` Code 4 verglich vor der Korrektur gegen die falsche Achse und wäre
/// nach Schritt b still grün geblieben, und die Event-Queue wäre ohne `CD.R` strukturell leer
/// gewesen. Beide Male bedeutete Grün nichts.
///
/// Der Nachweis lässt sich beim Hochlauf **nicht** führen: ein Übersetzungsfehler entsteht nur
/// durch eine echte Bus-Master-Anforderung, und die SMMUv3 bietet keinen Weg, eine zu erzeugen
/// (`ATOS` liefert das Ergebnis ins `PAR`, nicht in die Event-Queue, und QEMU implementiert es
/// nicht). Die erreichbare Form ist deshalb der Nachweis **im selben Lauf**: der Negativtest
/// erzeugt einen echten `F_TRANSLATION`, und erst danach zählt „Queue leer" als Aussage.
static EVTQ_LIVENESS: AtomicBool = AtomicBool::new(false);

/// Erste Fensterbasis oberhalb des RAM, 2-MiB-ausgerichtet.
///
/// **Warum oberhalb des RAM:** dann kann eine IOVA nie zugleich eine gültige PA sein. Eine
/// vertauschte Achse ist damit nicht nur im Audit auffällig (`dma_audit` Code 5), sondern in
/// **jedem** Pfad sofort ein Fault — die Zahl ist in keiner der beiden Rollen plausibel.
///
/// Die Obergrenze ist die Eingangsbreite der Stage-1 (`CD.T0SZ = 25` → 39 Bit). `None`, wenn
/// oberhalb des RAM kein Fenster mehr in diese Breite passt — dann bekommt der Kontext ein
/// schwaches Fenster (s. `ctx_assign_window`).
fn strong_window_base() -> Option<u64> {
    let top = RAM_TOP.load(Ordering::Acquire);
    let base = (top + IOVA_GUARD - 1) & !(IOVA_GUARD - 1);
    // Reserve, damit auch ein paar Regionen hineinpassen.
    if base + 16 * IOVA_GUARD < S1_INPUT_LIMIT {
        Some(base)
    } else {
        None
    }
}

/// Fenstergröße je Kontext-Slot: der gesamte Raum zwischen RAM-Oberkante und Eingangsgrenze,
/// gleichmäßig auf die `NDMA_CTX` Slots verteilt.
///
/// Vorher war das eine feste 1-GiB-Konstante, und der Bump lief zusätzlich über einen globalen
/// Zähler, der jedem *neu angelegten* Kontext ein frisches Fenster gab. Beides zusammen ergab
/// zwei unnötig knappe Obergrenzen über die Lebenszeit des Kernels: ~256 Attach-Vorgänge je
/// Kontext (jede Region kostet mit Ausrichtung und Schutzband mindestens 4 MiB) und ~500
/// Kontext-**Erzeugungen** insgesamt. Die zweite war die schärfere und stand nirgends.
///
/// Jetzt hängt das Fenster am **Slot**, nicht an einer Erzeugung: `NDMA_CTX` Slots, `NDMA_CTX`
/// Fenster, fertig. Der Bump je Slot überlebt den Abbau eines Kontexts (s. `SLOT_IOVA_NEXT`),
/// damit eine IOVA auch über Kontextgrenzen hinweg nie wiederverwendet wird.
fn window_size(first: u64) -> u64 {
    ((S1_INPUT_LIMIT - first) / NDMA_CTX as u64) & !(IOVA_GUARD - 1)
}

/// Nächste freie IOVA **je Slot**, über Kontext-Lebenszeiten hinweg.
///
/// Der Bump wird beim Abbau eines Kontexts bewusst **nicht** zurückgesetzt. Täte er das, bekäme
/// der nächste Kontext desselben Slots dieselben IOVAs — und ein Gerät, dessen ATC-Eintrag oder
/// in-flight Request die alte Zuordnung trägt, träfe auf eine gültige Abbildung fremder
/// Regionen. Nicht-Wiederverwendung ist der Preis dafür, dass die Reihenfolge beim Teardown eine
/// bewiesene und keine erzwungene Eigenschaft ist (s. Teardown-Token in `todo.md`).
static SLOT_IOVA_NEXT: [AtomicU64; NDMA_CTX] = [const { AtomicU64::new(0) }; NDMA_CTX];

/// Dem Kontext im Slot `slot` sein IOVA-Fenster zuweisen.
///
/// Die Fenster liegen hintereinander, damit zwei Kontexte nie dieselbe IOVA vergeben — sonst
/// wäre die Bounds-Prüfung eines Treibers gegen den *eigenen* Kontext wertlos, sobald ein zweiter
/// dieselben Zahlen benutzt.
fn ctx_assign_window(c: &mut DmaCtx, slot: usize) {
    match strong_window_base() {
        Some(first) => {
            let size = window_size(first);
            c.iova_base = first + slot as u64 * size;
            c.iova_limit = c.iova_base + size;
            c.strong_window = true;
        }
        None => {
            // Kein Platz oberhalb des RAM: schwaches Fenster (die Trennung gilt weiterhin, aber
            // eine IOVA *könnte* zufällig wie eine PA aussehen). Auf dieser Plattform gibt es das
            // nicht; der Zweig existiert für 32-Bit-Geräte / kleine Eingangsbreiten.
            c.iova_base = 0;
            c.iova_limit = S1_INPUT_LIMIT;
            c.strong_window = false;
        }
    }
    // Bump des Slots fortsetzen, nicht neu beginnen.
    let prev = SLOT_IOVA_NEXT[slot].load(Ordering::Relaxed);
    c.iova_next = if prev >= c.iova_base && prev < c.iova_limit {
        prev
    } else {
        c.iova_base + IOVA_GUARD // führendes Guard-Band
    };
}

/// Warum eine IOVA-Vergabe scheiterte — die Unterscheidung ist der Punkt: „Fenster voll" ist ein
/// Betriebszustand mit Obergrenze, „Gerät zu schmal" ein Konfigurationsfehler der Plattform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IovaError {
    /// Der Bump des Slots hat das Fensterende erreicht (keine Wiederverwendung).
    WindowExhausted,
    /// Die IOVA läge oberhalb der per `CD.T0SZ` konfigurierten Eingangsbreite.
    InputWidth,
    /// Das Gerät kann die Adresse nicht absetzen (z. B. 32-Bit-DMA, Fenster oberhalb 4 GiB).
    DeviceAddrWidth,
}

/// Eine IOVA für `len` Bytes aus dem Fenster des Kontexts vergeben — `IOVA_GUARD`-ausgerichtet,
/// mit einem Guard-Band dahinter, nie wiederverwendet.
///
/// Drei Bedingungen, alle drei mit eigener Fehlerursache, weil sie verschiedene Dinge bedeuten:
/// das Fensterende (Betriebsgrenze), die Eingangsbreite der Stage-1 (Kernel-Konfiguration) und
/// die Adressbreite des Geräts (Plattform). Vor allem die letzte darf nicht stillschweigend
/// durchgehen: ein Gerät mit 32-Bit-DMA bekäme aus einem Fenster oberhalb des RAM eine Adresse,
/// die es gar nicht absetzen kann — der Bus schneidet sie ab, und die abgeschnittene Adresse
/// trifft irgendetwas anderes.
fn ctx_alloc_iova(c: &mut DmaCtx, slot: usize, len: u64, dev_bits: u32) -> Result<Iova, IovaError> {
    let start = (c.iova_next + IOVA_GUARD - 1) & !(IOVA_GUARD - 1);
    let span = (len + IOVA_GUARD - 1) & !(IOVA_GUARD - 1);
    let next = start
        .checked_add(span)
        .and_then(|e| e.checked_add(IOVA_GUARD)) // Guard dahinter
        .ok_or(IovaError::WindowExhausted)?;
    if next > c.iova_limit {
        return Err(IovaError::WindowExhausted);
    }
    if start + span > S1_INPUT_LIMIT {
        return Err(IovaError::InputWidth);
    }
    if dev_bits < 64 && start + span > (1u64 << dev_bits) {
        return Err(IovaError::DeviceAddrWidth);
    }
    c.iova_next = next;
    SLOT_IOVA_NEXT[slot].store(next, Ordering::Relaxed);
    Ok(Iova::new(start))
}

/// Wie viele Adressbits ein Gerät absetzen kann (`64` = keine Einschränkung bekannt).
const MAX_NARROW_DEVS: usize = 4;
static NARROW_DEVS: [AtomicU64; MAX_NARROW_DEVS] = [const { AtomicU64::new(0) }; MAX_NARROW_DEVS];

/// Einem Gerät eine **schmalere** DMA-Adressbreite zuschreiben (z. B. 32 Bit).
///
/// Ohne diese Angabe nimmt der Kernel 64 Bit an — die einzige Annahme, die nicht still
/// fehlschlägt: ist sie falsch, scheitert `dma_attach` laut, statt eine Adresse zu vergeben, die
/// das Gerät abschneidet. `false`, wenn die Tabelle voll ist.
pub fn dma_declare_device_addr_bits(stream_id: u32, bits: u32) -> bool {
    let v = ((stream_id as u64) << 32) | (bits as u64);
    for e in NARROW_DEVS.iter() {
        let cur = e.load(Ordering::Acquire);
        if cur >> 32 == stream_id as u64 || cur == 0 {
            e.store(v, Ordering::Release);
            return true;
        }
    }
    false
}

/// StreamIDs, für die **keine** Adressbreite deklariert wurde (geführt, nicht bloß angenommen).
static UNDECLARED_DEVS: [AtomicU64; MAX_NARROW_DEVS] =
    [const { AtomicU64::new(u64::MAX) }; MAX_NARROW_DEVS];
static UNDECLARED_COUNT: AtomicU32 = AtomicU32::new(0);

/// Adressbreite eines Geräts. Undeklariert heißt **nicht** stillschweigend „64", sondern
/// „unbekannt, angenommen 64, und das wird geführt".
///
/// Der Unterschied ist der Punkt: eine falsche 64-Bit-Annahme scheitert nicht laut, sondern der
/// Bus schneidet die Adresse ab — und die abgeschnittene Adresse trifft weder ein Schutzband noch
/// eine Prüfung. In einem Kernel, der Bus-Master fail-safe hält und `Unaligned` eine eigene
/// Fehlervariante gibt, darf das nicht die einzige unsichtbare Annahme sein. Jedes undeklarierte
/// Gerät wird deshalb einmal protokolliert und gezählt; `dma_undeclared_devices()` macht den
/// Zustand prüfbar, statt ihn dem Zufall zu überlassen.
///
/// Ein harter Fehlschlag wäre die noch strengere Wahl, ist aber heute nicht zumutbar: die
/// Adressbreite steht in keinem Konfigurationsregister, sie ist Treiberwissen. Die Mittelstellung
/// ist ehrlich — sie behauptet nicht, etwas zu wissen.
fn device_addr_bits(stream_id: u32) -> u32 {
    for e in NARROW_DEVS.iter() {
        let cur = e.load(Ordering::Acquire);
        if cur != 0 && cur >> 32 == stream_id as u64 {
            return cur as u32;
        }
    }
    note_undeclared_device(stream_id);
    64
}

fn note_undeclared_device(stream_id: u32) {
    for e in UNDECLARED_DEVS.iter() {
        let cur = e.load(Ordering::Acquire);
        if cur == stream_id as u64 {
            return; // schon geführt
        }
        if cur == u64::MAX {
            e.store(stream_id as u64, Ordering::Release);
            UNDECLARED_COUNT.fetch_add(1, Ordering::Relaxed);
            // Die serielle Ausgabe haengt am aarch64-HAL; auf x86 bleibt es beim Zaehler.
            #[cfg(target_arch = "aarch64")]
            crate::println!(
                "dma: StreamID 0x{:x} ohne deklarierte Adressbreite -- angenommen 64 Bit \
                 (dma_declare_device_addr_bits)",
                stream_id
            );
            return;
        }
    }
    UNDECLARED_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Zähler der laut abgewiesenen Zuteilungen, je Ursache (Telemetrie/Test).
static IOVA_REJECTS: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

fn note_iova_reject(e: IovaError) {
    let i = match e {
        IovaError::WindowExhausted => 0,
        IovaError::InputWidth => 1,
        IovaError::DeviceAddrWidth => 2,
    };
    IOVA_REJECTS[i].fetch_add(1, Ordering::Relaxed);
}

/// Position einer StreamID in der SID-Liste eines Kontexts.
fn sid_index(c: &DmaCtx, rid: u32) -> Option<usize> {
    c.sids.iter().position(|&s| s == rid)
}

/// **Alle** Geräte des Kontexts stilllegen (nur beim Übergang 0→1 je Gerät tatsächlich).
///
/// Über **alle** StreamIDs, nicht nur die des Teardowns: die Region ist im **Kontext** gemappt,
/// nicht an einer RID. Bei einer Stream-Gruppe (mehrere Geräte teilen eine Stage-1-Tabelle,
/// s. `share_context`) können in-flight Writes von **jedem** Gerät der Gruppe kommen — würde nur
/// das eine entwaffnet, wäre der Zähler korrekt und die Flush-Garantie trotzdem unvollständig:
/// die Region würde entfernt und freigegeben, während ein zweites Gerät desselben Kontexts noch
/// Writes unterwegs hat.
///
/// Reihenfolge: erst **alle** entwaffnen, dann **alle** spülen. So ist kein Gerät der Gruppe mehr
/// scharf, während ein anderes noch spült.
/// Gibt zurück, ob die Stilllegung **bestätigt** ist: für jede StreamID muss das Bus-Master-Bit
/// nach dem Löschen auch wirklich gelöscht sein, und das Konfigurations-Read (der Flush) muss
/// eine plausible Antwort liefern. Ein Gerät, das verschwunden ist (`0xFFFF`) oder sein
/// Command-Register nicht übernimmt, hat die Spülgarantie **nicht** erbracht — dann darf seine
/// Region nie zurück in den Allokator (ext-37, Pending-Zustand).
fn ctx_quiesce(c: &mut DmaCtx) -> bool {
    for k in 0..MAX_CTX_SIDS {
        if c.sids[k] == u32::MAX {
            continue;
        }
        if c.quiesce_depth[k] == 0 {
            c.saved_cmd[k] = hal::pcie::clear_bus_master(c.sids[k]);
        }
        c.quiesce_depth[k] += 1;
    }
    let mut confirmed = true;
    for k in 0..MAX_CTX_SIDS {
        if c.sids[k] == u32::MAX {
            continue;
        }
        let vendor = hal::pcie::flush_posted_writes(c.sids[k]);
        let cmd = hal::pcie::read_command(c.sids[k]);
        if vendor == 0xFFFF || cmd & hal::pcie::CMD_BUS_MASTER_BIT != 0 {
            confirmed = false;
        }
    }
    confirmed
}

/// Stilllegung aufheben (nur beim Übergang 1→0 tatsächlich).
///
/// `allow_bme`: darf das Gerät danach wieder Bus-Master sein? Zurückgeschrieben wird stets der
/// **gesicherte** Wert (ggf. ohne Bus-Master-Bit) — ein Gerät, das absichtlich aus war, darf ein
/// Teardown nicht einschalten.
///
/// **Kopplung an die ATS-Entscheidung** (`docs/invariants.md` §2b): Wiedereinschalten ist nur
/// solide, *weil* nach `CMD_TLBI`+`CMD_SYNC` keine gecachte Übersetzung mehr existiert. Mit
/// **ATS** existiert sie im ATC des Geräts — dann müsste hier eine ATC-Invalidierung davorstehen.
fn ctx_release(c: &mut DmaCtx, allow_bme: bool) {
    // Umgekehrte Reihenfolge zum Entwaffnen (symmetrischer Abbau).
    for k in (0..MAX_CTX_SIDS).rev() {
        if c.sids[k] == u32::MAX || c.quiesce_depth[k] == 0 {
            continue;
        }
        c.quiesce_depth[k] -= 1;
        if c.quiesce_depth[k] > 0 {
            continue; // ein anderer Teardown hält dieses Gerät noch still
        }
        let cmd = if allow_bme {
            c.saved_cmd[k]
        } else {
            c.saved_cmd[k] & !hal::pcie::CMD_BUS_MASTER_BIT
        };
        hal::pcie::write_command(c.sids[k], cmd);
    }
}

/// Bus-Master nach dem Installieren einer Übersetzung erteilen.
///
/// **Kein unbedingtes Schreiben.** Läuft parallel ein Teardown desselben Geräts (Zähler > 0),
/// würde ein direktes Scharfschalten dessen Flush-Garantie entwerten: das Gerät dürfte neue
/// Requests in eine Region absetzen, die der andere Pfad gerade entfernt und gleich freigibt —
/// genau das Fenster, das die Stilllegung schließt, nur über `attach` hereingekommen. Stattdessen
/// wird die **Absicht** im gesicherten Command-Wert vermerkt; das Restore beim Übergang 1→0
/// nimmt sie mit.
fn ctx_arm_bus_master(c: &mut DmaCtx, rid: u32) {
    let Some(k) = sid_index(c, rid) else { return };
    if c.quiesce_depth[k] > 0 {
        c.saved_cmd[k] |= hal::pcie::CMD_BUS_MASTER_BIT;
    } else {
        hal::pcie::arm_bus_master(rid);
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
    /// Verschachtelungstiefe der Stilllegung **je StreamID** (parallel zu `sids`).
    ///
    /// Warum hier und nicht in einer Seitentabelle der HAL: die RID gehört zu genau einem
    /// Kontext (`attach` sucht ihn über `sids.contains`), also ist der Zähler durch **denselben**
    /// Lock geschützt wie die RID selbst — er kann nicht aus dem Tritt geraten, wenn das
    /// Kontext-Locking später verfeinert wird, und er kann nicht überlaufen, weil seine
    /// Kapazität dieselbe Quelle hat wie die Kontextobergrenze (`NDMA_CTX * MAX_CTX_SIDS`).
    quiesce_depth: [u32; MAX_CTX_SIDS],
    /// Command-Register **vor** der ersten Stilllegung (Save/Restore, nie „auf 1 setzen").
    saved_cmd: [u16; MAX_CTX_SIDS],
    /// Basis des **IOVA-Fensters** dieses Kontexts (0 = noch nicht vergeben).
    ///
    /// Bewusst ein **Kontext-Attribut**, keine globale Konstante: die starke Eigenschaft
    /// („keine IOVA kann je eine gültige PA sein") verlangt eine Basis oberhalb des RAM, und die
    /// kann ein Gerät, das nur unterhalb 4 GiB adressiert, nicht immer haben. Welche Kontexte die
    /// starke Eigenschaft tragen, steht in `strong_window`.
    iova_base: u64,
    /// Nächste freie IOVA (Bump). **Wird nie zurückgesetzt**: Wiederverwendung ist bei einem
    /// 39-Bit-Fenster unnötig und würde Fragen nach veralteten Übersetzungen aufwerfen, die es
    /// so gar nicht erst gibt.
    iova_next: u64,
    /// Obergrenze des Fensters (exklusiv). Der Bump gibt nie zurück, also ist das zugleich die
    /// Lebenszeit-Obergrenze der Zuteilungen dieses Slots.
    iova_limit: u64,
    /// Liegt das Fenster oberhalb des RAM (dann ist eine vertauschte Achse **immer** ein Fault)?
    strong_window: bool,
    regs: [DmaRegion; MAX_CTX_REGS], // (base,len) je Region ((0,0) = leer)
}

impl DmaCtx {
    const EMPTY: DmaCtx = DmaCtx {
        used: false,
        l1: 0,
        cd: 0,
        sids: [u32::MAX; MAX_CTX_SIDS],
        quiesce_depth: [0; MAX_CTX_SIDS],
        saved_cmd: [0; MAX_CTX_SIDS],
        iova_base: 0,
        iova_next: 0,
        iova_limit: 0,
        strong_window: false,
        regs: [DmaRegion::EMPTY; MAX_CTX_REGS],
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
            // Cache-Wartung läuft über die **CPU**-Sicht: die IOVA existiert nur für das Gerät.
            hal::mmu::dma_cache_clean(handle.pa.raw(), handle.len)
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
            hal::mmu::dma_cache_invalidate(handle.pa.raw(), handle.len)
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
pub fn dma_enable(stream_id: u32, base: u64, len: u64) -> Option<Iova> {
    // Roher Pfad (ohne Cap): der Aufrufer nennt eine **physische** Adresse und bekommt die
    // **Gerätesicht** zurück. Er kann sie nicht selbst ausrechnen — sie stammt aus dem
    // IOVA-Fenster des Kontexts (ext-36 Schritt b) und hat mit `base` keinen Zusammenhang.
    dma_enforcer().attach(&DmaBinding::new(stream_id, Pa::new(base), len))
}

/// Die Durchsetzung für `(stream_id, [base,len))` wieder entziehen.
pub fn dma_disable(stream_id: u32, base: u64, len: u64) {
    dma_enforcer().detach(&DmaBinding::new(stream_id, Pa::new(base), len));
}

/// **Nachweis, dass eine DMA-Region abgebaut ist** (ext-37).
///
/// Der Token ist der einzige Weg zu [`free_dma_region`]. Er entsteht ausschließlich in
/// [`dma_finalize`], und zwar erst, nachdem für seine Region gilt: Gerät stillgelegt (bestätigt),
/// Übersetzung entfernt, `CMD_SYNC` durch. Vorher war diese Reihenfolge eine **bewiesene**
/// Vorbedingung des Gesamtsystems — `CapSpace::delete_leaf` rief `free_region` direkt, und dass
/// davor jemand aufgeräumt hatte, stand nirgends im Typ. Jetzt ist sie **erzwungen**.
///
/// Er trägt eine [`DmaRegion`], nicht eine `PhysRegion`: nach ext-36 gehören beide Achsen in den
/// Nachweis. Der Unmap ist auf der IOVA-Achse, der Free auf der PA-Achse — mit einer reinen
/// `PhysRegion` könnte der Token einen erfolgten Free bezeugen, ohne dass der Unmap darin
/// überhaupt vorkommt.
#[must_use = "ein Teardown-Token, der nicht eingelöst wird, ist eine geleakte Region"]
pub struct DmaTeardownToken {
    region: DmaRegion,
}

impl DmaTeardownToken {
    /// Die bezeugte Region — beide Achsen, für Diagnose und Audits.
    pub fn region(&self) -> DmaRegion {
        self.region
    }
}

/// Eine DMA-Region freigeben. **Nur** gegen einen [`DmaTeardownToken`].
fn free_dma_region(token: DmaTeardownToken) {
    free_raw_region(token.region.pa.raw(), token.region.len);
}

/// Regionen, deren Gerät die Stilllegung **nicht bestätigt** hat.
///
/// Ihre Übersetzung ist entfernt (das ist immer zwingend), aber ihre Physadresse geht **nie**
/// zurück in den Allokator: ohne bestätigte Spülung ist nicht auszuschließen, dass noch ein
/// übersetzter, unterwegs befindlicher Write ankommt. Ein Leck ist gegenüber einem
/// Use-after-free der richtige Failure-Mode — aber als **Entscheidung**, nicht als Versehen:
/// die Menge ist beschränkt, gezählt und auditiert (`dma_audit` Code 7).
const MAX_PENDING_DMA: usize = 16;
static PENDING_DMA: SpinLock<[DmaRegion; MAX_PENDING_DMA]> =
    SpinLock::new([DmaRegion::EMPTY; MAX_PENDING_DMA]);
/// Wie viele Regionen insgesamt in den Pending-Zustand gerieten (auch die, für die kein Platz
/// mehr war — die sind dann erst recht geleakt, aber nicht unbemerkt).
static PENDING_TOTAL: AtomicU32 = AtomicU32::new(0);
/// Davon außerhalb eines `KILL` — dort ist Pending erwartbar (eine sterbende PD mit hängendem
/// Gerät), überall sonst ist es eine Anomalie, die man sehen will.
static PENDING_UNEXPECTED: AtomicU32 = AtomicU32::new(0);

/// Läuft gerade ein `KILL`? Dort darf Pending entstehen, ohne als Anomalie zu zählen.
static IN_KILL: AtomicU32 = AtomicU32::new(0);

/// Markiert den laufenden Abbau einer PD/eines Threads (`KILL`).
pub struct KillScope;
impl KillScope {
    pub fn enter() -> Self {
        IN_KILL.fetch_add(1, Ordering::AcqRel);
        KillScope
    }
}
impl Drop for KillScope {
    fn drop(&mut self) {
        IN_KILL.fetch_sub(1, Ordering::AcqRel);
    }
}

fn park_pending(region: DmaRegion) {
    PENDING_TOTAL.fetch_add(1, Ordering::Relaxed);
    if IN_KILL.load(Ordering::Acquire) == 0 {
        PENDING_UNEXPECTED.fetch_add(1, Ordering::Relaxed);
    }
    let mut p = PENDING_DMA.lock();
    if let Some(e) = p.iter_mut().find(|r| r.is_empty()) {
        *e = region;
    }
    #[cfg(target_arch = "aarch64")]
    crate::println!(
        "dma: Region pa=0x{:x} len=0x{:x} bleibt dauerhaft pending -- Stilllegung nicht bestaetigt",
        region.pa.raw(),
        region.len
    );
}

/// **Finalisierung der beim Löschen/Revoke gemeldeten DMA-Regionen** (ext-37).
///
/// Läuft **ohne** gehaltenen CAPS/MEM-Lock — dieselbe Stelle und derselbe Grund wie bei
/// [`abort_finalized_replies`]: die Nachbereitung braucht Sperren, die unter CAPS nicht genommen
/// werden dürfen (hier `DMA_CTX` und, im Kontextabbau, `MEM`).
///
/// **Synchron, nicht aufgeschoben.** Der Preis ist eine geräteabhängige Laufzeit im `delete`;
/// der Gewinn ist, dass es den Pending-Zustand für den Normalfall **gar nicht gibt**. Ein
/// Zwischenzustand, den es meistens nicht gibt, ist schwerer richtig zu halten als einer, den es
/// nie gibt — und die Beschränktheits-Invariante der Pending-Menge muss dann nur noch den
/// pathologischen Fall tragen, für den sie gedacht war.
/// `ok` ist der Ergebnispuffer des Enforcers und kommt vom Aufrufer (A-3.3, s. [`FinalizeBuf`]).
/// Vorher lagen hier **zwei** weitere Arrays auf dem Kernelstack: eine Kopie der Regionen (nur um
/// aus dem Kollektor einen zusammenhängenden Slice zu machen — den liefert er jetzt selbst) und
/// das Ergebnisfeld.
fn dma_finalize(fin: &sel4lake_cap::Finalized<'_>, ok: &mut [bool], ctx_of: &mut [usize]) {
    let regs = fin.dma_regions();
    let n = regs.len().min(ok.len());
    if n == 0 {
        return;
    }
    dma_enforcer().finalize(&regs[..n], &mut ok[..n], ctx_of);
    for i in 0..n {
        let region = DmaRegion {
            iova: Iova::new(0), // die IOVA ist mit dem Unmap erloschen und kommt nie zurück
            pa: Pa::new(regs[i].0),
            len: regs[i].1,
        };
        if ok[i] {
            free_dma_region(DmaTeardownToken { region });
        } else {
            park_pending(region);
        }
    }
}

/// **Opakes DMA-Handle** (ext-24): die *gerätesichtbare* Adresse (IOVA) + Länge einer
/// angehängten Region. Backends programmieren das Gerät mit `iova` (heute identisch zur PA;
/// die Abstraktion erlaubt später Remap/Bounce/SG-Kompaktierung ohne API-Bruch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaHandle {
    /// Gerätesicht — steht im Deskriptor, wird gegen den Kontextinhalt geprüft.
    pub iova: Iova,
    pub len: u64,
    /// CPU-Sicht — **kernelprivat**: Cache-Wartung und Allokator-Buchhaltung laufen darüber.
    /// Ein Treiber hat hier nichts zu suchen; gäbe man sie heraus, wäre die Trennung der beiden
    /// Achsen wieder eine Konvention statt einer Grenze.
    pa: Pa,
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
    let assigned = dma_enforcer().attach(&DmaBinding {
        stream_id,
        pa: Pa::new(phys),
        len,
        backend_pd: 0,
        ro,
        cacheable,
    });
    if let Some(iova) = assigned {
        // Die IOVA kommt aus dem Fenster des Übersetzungskontexts (Schritt b) — der Aufrufer
        // konnte sie nicht wählen und erfährt sie erst hier.
        Some(DmaHandle {
            iova,
            len,
            pa: Pa::new(phys),
        })
    } else {
        None
    }
}

/// Eine zuvor per [`dma_attach`] angehängte Region wieder lösen (Stage-1-Eintrag entfernen +
/// TLBI; bei letzter Region des Kontexts: Kontext abbauen). `handle.iova` == PA.
pub fn dma_detach(stream_id: u32, handle: DmaHandle) {
    dma_enforcer().detach(&DmaBinding::new(
        stream_id,
        handle.pa,
        handle.len,
    ));
}

/// **IOMMU-Stream-Gruppe** (ext-24): `member` nutzt fortan denselben Übersetzungskontext wie
/// `leader` (Multi-Function/SR-IOV/Bridge). `true` bei Erfolg.
pub fn dma_group_add(leader: u32, member: u32) -> bool {
    dma_enforcer().share_context(leader, member)
}

/// Prüfen, ob `[addr, addr+len)` (IOVA) **vollständig in einer angehängten Region** des Kontexts
/// der `stream_id` liegt (Level-1-Software-Disziplin über mehrere Regionen / Scatter-Gather).
fn dma_addr_in_context(stream_id: u32, addr: Iova, len: u64) -> bool {
    let t = DMA_CTX.lock();
    t.iter()
        .find(|c| c.used && c.sids.contains(&stream_id))
        // **IOVA-Achse**: geprüft wird, was im Deskriptor stehen darf — also die Gerätesicht.
        .map(|c| {
            c.regs
                .iter()
                .any(|r| region_contains(r.iova.raw(), r.len, addr.raw(), len))
        })
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
            && dma_addr_in_context(stream_id, e.handle.iova.offset(e.offset), e.len)
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
pub fn dma_addr_in_region(base: Iova, rlen: u64, addr: Iova, len: u64) -> bool {
    // **IOVA-Achse.** Geprüft wird eine Adresse, die in einen Deskriptor geschrieben werden soll
    // — also die Gerätesicht. Diese Signatur war bis ext-36 vollständig `u64` und hätte den
    // Achsenwechsel klaglos überlebt: sie hätte ab Schritt b gegen die falsche Achse geprüft,
    // ohne dass irgendetwas bricht. Genau deshalb steht hier jetzt der Typ.
    region_contains(base.raw(), rlen, addr.raw(), len)
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
    pub cj_smmu_enforced: bool, // Level 2: faultete die SMMU den ungeschützten Zugriff?
    pub cj_evt_translation: bool, // ... und zwar als F_TRANSLATION (nicht irgendein Fault)
    pub cj_evt_sid_ok: bool,      // ... von der erwarteten StreamID
    pub cj_evt_input_ok: bool,    // ... auf genau der absichtlich eingetragenen PA
    pub cj_axes_differ: bool,     // Positivkontrolle der Trennung: IOVA != PA in diesem Lauf
    /// Sensitivitätskontrolle: mit **Bypass-STE** (Durchsetzung aufgehoben) schreibt dasselbe
    /// Gerät dieselbe Adresse tatsächlich. Ohne diese Kontrolle wäre das Ausbleiben des
    /// Schreibzugriffs auch mit einem stillgelegten Gerät vereinbar.
    pub cj_bypass_wrote: bool,
    /// Die Event-Queue hat in **diesem Lauf** nachweislich gesprochen (ein echter
    /// `F_TRANSLATION`). Erst damit ist „Queue leer" oben überhaupt eine Aussage.
    pub evtq_liveness: bool,
    /// Keine Konfigurationsfehler der Einheit (`C_BAD_STE`/`C_BAD_CD`/…) über den ganzen Lauf.
    pub cfg_errors_zero: bool,
    pub audit_ok: bool,        // dma_audit==0 nach dem Aufräumen
}

#[cfg(target_arch = "aarch64")] // virtio-rng-PCI + SMMU: ARM-/QEMU-`virt`-spezifisch (ext-31)
// A-2.2: benutzt `testsupport` (liegt hinter `selftest`), und der einzige Aufrufer ist
// `threads::mod.rs` — das Modul gibt es ohne das Feature nicht. Ohne dieses Gate uebersetzt der
// `--no-default-features`-Bau auf aarch64 gar nicht.
#[cfg(feature = "selftest")]
/// **virtio-rng-DMA End-to-End** (ext-23, D4): das Gerät DMAt Zufallsbytes in die DmaCap-Region
/// (echter Bus-Master-DMA). Danach der **zweistufige Kronjuwel-Test**:
/// - **Level 1 (Software, demonstrierbar):** der vertrauenswürdige Treiber validiert jede
///   Deskriptor-Adresse via [`dma_addr_in_region`]; eine Out-of-Window-Adresse wird abgewiesen ->
///   das Gerät wird gar nicht erst programmiert -> das Ziel bleibt unverändert.
/// - **Sensitivität + Level 2 (Hardware):** wird die Prüfung umgangen und die Adresse doch
///   ausgegeben, muss etwas Beobachtbares passieren — entweder schreibt das Gerät (dann trug
///   allein die Software), oder die SMMU weist ab. Seit `iommu_platform=on` (der Treiber
///   verlangt `VIRTIO_F_ACCESS_PLATFORM`) übersetzt QEMU das Gerät tatsächlich durch die SMMU,
///   und der zweite Fall tritt ein: geprüft wird dann nicht *dass* ein Event kam, sondern dass es
///   ein `F_TRANSLATION` der erwarteten StreamID auf **genau der absichtlich eingetragenen PA**
///   ist. Weil seit ext-36 Schritt b IOVA != PA gilt, ist eine PA im Deskriptor für das Gerät
///   nicht auflösbar — der Negativtest prüft damit die Achsentrennung selbst.
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
    // Was das Gerät sieht: die IOVA, die der Enforcer beim Anhängen aus dem Fenster des
    // Kontexts vergeben hat. Sie ist von `base` **verschieden** (Fensterbasis oberhalb des RAM)
    // — ab hier trägt die Demo die Trennung der beiden Achsen als Abnahme.
    let Some(dev_base) = dma_enable(rid, base, len) else {
        free_raw_region(base, len);
        return r;
    };
    hal::iommu::drain_faults(); // sauberer Ausgangsstand
    let dlen = hal::virtio::DATA_LEN_BYTES as u64;

    // 1. In-Window: Treiber validiert die Zieladresse (Level 1, ok) -> Gerät DMAt Zufallsbytes.
    let data = dev_base.offset(hal::virtio::DATA_OFFSET);
    if dma_addr_in_region(dev_base, len, data, dlen) {
        if let Some(rng) = hal::virtio::probe(&dev) {
            // SAFETY: kernel-/Trusted-seitiger virtio-Treiber; BAR ist global EL1-Device-gemappt,
            // die Virtqueue + der Datenpuffer liegen in der (identity-gemappten) DMA-Region.
            // Der virtio-Treiber braucht **beide** Achsen: die Virtqueue beschreibt er per CPU
            // (PA), ihre Adresse und die des Puffers programmiert er dem Gerät (IOVA).
            let (adv, wlen) = unsafe { rng.request(base, dev_base.raw(), data.raw()) };
            r.used_adv = adv;
            r.written = wlen;
            // Nachlesen tut die **CPU** -> PA-Achse.
            let (w0, w1) = testsupport::peek_dma_words(base + hal::virtio::DATA_OFFSET);
            r.rand0 = w0;
            r.rand1 = w1;
        }
    }
    r.evtq_empty_good = hal::iommu::faults_empty();

    // 2. Kronjuwel (Sentinel-Page AUSSERHALB der DmaCap-Region). MEM-Lock VOR dem `if let`
    // freigeben (sonst Deadlock über das `if let`-Temporary -> free_raw_region re-lockt MEM).
    let sentinel_page = mem_alloc(4096, 4096).map(|c| c.base());
    if let Some(sent) = sentinel_page {
        const SENTINEL: u64 = 0xA5A5_A5A5_5A5A_5A5A;
        // SAFETY: frische, identity-gemappte RAM-Page; exklusiv hier beschrieben/gelesen.
        unsafe { core::ptr::write_volatile(sent as *mut u64, SENTINEL) };
        hal::cpu::dsb_sy();

        // 2a. Level 1: der Treiber validiert die Out-of-Window-Adresse -> ABGEWIESEN, kein Notify.
        // Die Sentinel-Page liegt AUSSERHALB der Region. Geprüft wird, ob der Treiber diese
        // Adresse als Deskriptorziel abweist — also auf der Gerätesicht. Dass sie hier aus einer
        // PA gebildet wird, ist der Punkt des Tests: eine Adresse, die dem Gerät nie zugeteilt
        // wurde. (Ab Schritt b ist sie zusätzlich schon deshalb ungültig, weil sie gar nicht im
        // IOVA-Fenster liegt — dann prüft dieselbe Zeile zwei Dinge auf einmal.)
        r.cj_sw_blocked = !dma_addr_in_region(dev_base, len, Iova::new(sent), dlen);
        r.cj_axes_differ = dev_base.raw() != base; // Schritt b wirkt: IOVA != PA
        // SAFETY: nur Lesezugriff. Da nicht programmiert, muss das Ziel unverändert sein.
        let after_guard = unsafe { core::ptr::read_volatile(sent as *const u64) };
        r.cj_sentinel_ok = after_guard == SENTINEL;

        // 2b. Sensitivität: Prüfung umgehen + Out-of-Window doch ausgeben. Beweist, dass die
        // Software-Prüfung lasttragend ist; testet zugleich den SMMU-Backstop (QEMU: nicht
        // beobachtbar -> Gerät schreibt; reale HW: SMMU faultet).
        // Teil 1 des Negativtests: Event-Queue **vorher** leeren — sonst bestünde er an einem
        // Altbestand aus einem früheren Schritt.
        hal::iommu::drain_faults();
        if let Some(rng) = hal::virtio::probe(&dev) {
            // Die Virtqueue bleibt korrekt (CPU: PA, Gerät: IOVA) — **nur** die Zieladresse im
            // Deskriptor ist absichtlich eine **PA** statt einer IOVA. Genau das ist die
            // Verwechslung, die vor ext-36 Schritt b keinen Unterschied machte: seit die beiden
            // Achsen auseinanderliegen, ist `sent` im Fenster des Geräts nicht abgebildet.
            let _ = unsafe { rng.request(base, dev_base.raw(), sent) };
        }
        // SAFETY: s.o.
        let after_unguarded = unsafe { core::ptr::read_volatile(sent as *const u64) };
        r.cj_unguarded_wrote = after_unguarded != SENTINEL; // Prüfung war lasttragend
        // Teil 2: den Eintrag **prüfen**, nicht zählen. Ein Zähler bestünde auch an einem
        // beliebigen anderen Fault; belegt ist die Eigenschaft erst, wenn es ein
        // Übersetzungsfehler der erwarteten StreamID auf **genau der PA** ist, die oben
        // absichtlich als Gerätesicht eingetragen wurde.
        if let Some(ev) = hal::iommu::peek_fault() {
            r.cj_smmu_enforced = true;
            r.cj_evt_translation = ev.kind == hal::fault::FaultKind::Translation;
            r.cj_evt_sid_ok = ev.requester == rid;
            r.cj_evt_input_ok = ev.input_addr == sent;
            // **Queue-Liveness**: ein echter Übersetzungsfehler ist der Beleg, dass die Queue in
            // diesem Lauf überhaupt sprechen kann. Ohne ihn ruht jedes „keine Faults" auf
            // `CD.R` — einem Bit, das genau hier einmal gefehlt hat und das jemand später aus
            // Performancegründen wieder abschalten kann.
            if r.cj_evt_translation {
                EVTQ_LIVENESS.store(true, Ordering::Release);
                r.evtq_liveness = true;
            }
        }
        // Teil 3, Sensitivitätskontrolle: **ohne** Durchsetzung muss dasselbe Gerät dieselbe
        // Adresse wirklich schreiben. Die alte Kontrolle (`cj_unguarded_wrote`) ist eingeklappt,
        // seit die SMMU tatsächlich übersetzt: sie prüfte dann dieselbe Beobachtung wie die
        // Hauptaussage und konnte nicht mehr fehlschlagen, während diese besteht. Diese hier
        // kann es — sie ist rot, wenn das Gerät gar nicht mehr DMAt.
        //
        // Der Aufbau ist bewusst genau der eines „Passthrough-Enforcers für den Bringup":
        // Bypass-STE, Gerät läuft unübersetzt. Er steht sichtbar benannt im Test, damit er nicht
        // versehentlich in der Durchsetzung landet.
        hal::iommu::drain_faults();
        unsafe { core::ptr::write_volatile(sent as *mut u64, SENTINEL) };
        hal::cpu::dsb_sy();
        if DMA_ENFORCER.override_ste(rid, &hal::smmu::build_ste_bypass()) {
            if let Some(rng) = hal::virtio::probe(&dev) {
                // Unter Bypass gibt es **keine** Gerätesicht mehr: die Adressen des Geräts sind
                // physisch. Deshalb hier `base` in beiden Rollen — und genau das ist die
                // Konfiguration, in der die Achsentrennung wirkungslos ist.
                let _ = unsafe { rng.request(base, base, sent) };
            }
            // SAFETY: s.o.
            let after_bypass = unsafe { core::ptr::read_volatile(sent as *const u64) };
            r.cj_bypass_wrote = after_bypass != SENTINEL;
            DMA_ENFORCER.restore_ste(rid);
        }
        hal::iommu::drain_faults();
        free_raw_region(sent, 4096);
    }

    // 3. Aufräumen: Durchsetzung entziehen (STE invalidieren) + DMA-Region freigeben.
    dma_disable(rid, base, len);
    free_raw_region(base, len);
    r.audit_ok = dma_audit() == 0;
    r.cfg_errors_zero = hal::iommu::config_errors() == 0;
    r
}

/// Eine **nie angehängte** DMA-Region an den Allokator zurückgeben — für Fehler-/Cleanup-Pfade,
/// in denen eine `alloc_dma_region` gar nicht erst in eine Cap mündet.
///
/// Kein Token nötig und keiner möglich: die Region war nie in einer Übersetzungstabelle, also
/// gibt es nichts zu bezeugen. Deshalb der eigene Name — `free_dma_region` ist der
/// nachweispflichtige Pfad (ext-37).
#[cfg_attr(not(feature = "kernel-fuzz"), allow(dead_code))] // nur vom Fuzzer benutzt (ADR 0013)
pub fn free_unattached_dma_region(base: u64, len: u64) {
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

/// `program_id` des Zähler-Service (Hot-Reload-Testkomponente). Der Zustand gehört einem
/// **Programm**, nicht einer Instanz — genau deshalb überlebt er den Instanztausch.
pub const CS_PROGRAM_ID: u32 = 0xA43_0001;
/// Layout-Version der Zähler-Nutzlast: ein `u64` an Nutzlast-Offset 0.
///
/// Bewusst **getrennt** von der `iface_version` aus A-4.4: die eine beschreibt, was über den
/// Endpoint geht, die andere, was im Speicher liegt. v1 (+1) und v2 (+10) unterscheiden sich im
/// Verhalten, nicht im Zustandslayout — sie tragen dieselbe `STATE_VERSION`, und genau deshalb
/// darf v2 den Zähler von v1 fortsetzen.
pub const CS_STATE_VERSION: u32 = 1;
/// Nutzlastlänge des Zähler-Zustands (ein `u64`).
const CS_PAYLOAD_LEN: u32 = 8;

/// Die Hot-Reload-Zustandsregion anlegen: Kopf schreiben (A-4.3), Nutzlast nullen. Gibt die
/// Phys-Basis zurück (nur Telemetrie; der Zugriff läuft über [`hotreload_state_get`]/
/// [`hotreload_state_set`], die Übernahme über [`hotreload_state_attach`]).
pub fn hotreload_state_alloc() -> Option<u64> {
    let mut region = KernelRegionSource.request(4096, Purpose::HotReloadState)?;
    let phys = region.phys();
    // `init` nullt die Nutzlast selbst — `carve` liefert nicht garantiert genullten Speicher, und
    // ein Zustand aus Resten einer fremden Allokation ist von einem echten nicht zu unterscheiden.
    state::init(
        &mut region.view(),
        CS_PROGRAM_ID,
        CS_STATE_VERSION,
        CS_PAYLOAD_LEN,
    )
    .ok()?;
    *CS_STATE_REGION.lock() = Some(region);
    Some(phys)
}

/// Den Zustand **übernehmen** (A-4.3): der Kopf muss zu `program_id`/`state_version` passen, sonst
/// gibt es kein `Ok` — und damit keinen stillschweigend fehlinterpretierten Zustand. Erhöht bei
/// Erfolg den Übernahmezähler, macht die Übernahme also im Speicher sichtbar.
///
/// `NoState` heisst **Kaltstart**, nicht Fehler: der Aufrufer legt dann an, statt abzubrechen.
pub fn hotreload_state_attach(
    program_id: u32,
    state_version: u32,
) -> Result<state::Handover, state::StateError> {
    let mut guard = CS_STATE_REGION.lock();
    let Some(r) = guard.as_mut() else {
        return Err(state::StateError::NoState);
    };
    state::attach(&mut r.view(), program_id, state_version)
}

/// Die Übernahmezahl lesen, ohne zu übernehmen (Telemetrie/Selbsttest). `None`, wenn dort kein
/// gültiger Kopf steht.
pub fn hotreload_state_generation() -> Option<u32> {
    let mut guard = CS_STATE_REGION.lock();
    let r = guard.as_mut()?;
    state::generation(&r.view())
}

/// Das Zähler-Wort aus der **Nutzlast** der Hot-Reload-Region lesen (sichere RegionView-API).
/// Liegt hinter dem Kopf — ein Schreibfehler hier kann die Versionsangabe nicht überschreiben.
pub fn hotreload_state_get() -> u64 {
    CS_STATE_REGION
        .lock()
        .as_mut()
        .and_then(|r| state::payload(&mut r.view()).and_then(|p| p.get::<u64>(0)))
        .unwrap_or(0)
}

/// Das Zähler-Wort in die **Nutzlast** der Hot-Reload-Region schreiben (sichere RegionView-API).
pub fn hotreload_state_set(val: u64) {
    if let Some(r) = CS_STATE_REGION.lock().as_mut() {
        if let Some(mut p) = state::payload(&mut r.view()) {
            p.set::<u64>(0, val);
        }
    }
}

/// Eine roh-allozierte RAM-Region (ohne Cap) an den Allokator zurückgeben (interner Test-/
/// Setup-Helfer für temporäre DMA-/Sentinel-Regionen). 4-KiB-granular.
pub(crate) fn free_raw_region(base: u64, len: u64) {
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
    // VSpace-Unmap ist **CPU**-Sicht -> PA (die IOVA gehört der IOMMU, nicht der MMU).
    unmap_dma_from_thread(tid, binding.pa.raw(), binding.len);
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
    // Achsen-Wächter (ext-36): Der Vergleich oben läuft gegen die Freiliste des **physischen**
    // Allokators und ist nur dann aussagekräftig, wenn er physische Adressen sieht. Würde jemand
    // dort die IOVA einsetzen, überlappte ab Schritt b nie etwas — das Oracle bliebe bei jedem
    // Lauf grün, ohne noch irgendetwas zu prüfen. Deshalb hier explizit: jede erfasste Adresse
    // muss im RAM-Fenster liegen, das der Allokator überhaupt vergeben kann.
    if !dma_ctx_regions_are_physical() {
        return 5;
    }
    // Code 6: **Konfigurationsfehler der IOMMU**. `C_BAD_STE`/`C_BAD_CD` und Verwandte heißen,
    // dass die Einheit die Tabellen ablehnt und den Stream **gar nicht** übersetzt. Jede spätere
    // Aussage der Form „keine Faults beobachtet" wäre dann bedeutungslos — nicht weil nichts
    // passierte, sondern weil nichts passieren *konnte*. Genau dieser Zustand bestand hier
    // unbemerkt, solange das emulierte Gerät die SMMU ohnehin umging.
    #[cfg(target_arch = "aarch64")]
    if hal::iommu::config_errors() != 0 {
        return 6;
    }
    // Code 7: der **Pending-Zustand** ist eine eigene Invariante, kein Feld. Eine Region darin
    // ist weder frei noch übersetzt — genau die Konjunktion, die verletzt wäre, wenn die
    // Fädelung durch `Finalized` irgendwo abreißt: läge sie in der Freiliste, wäre sie trotz
    // unbestätigter Stilllegung neu vergebbar; stünde sie noch in einer Übersetzungstabelle,
    // wäre der zwingende Unmap ausgefallen.
    if !dma_pending_is_isolated() {
        return 7;
    }
    0
}

/// Wächter für [`dma_audit`] Code `7`.
fn dma_pending_is_isolated() -> bool {
    let mut snap = [DmaRegion::EMPTY; MAX_PENDING_DMA];
    let mut n = 0;
    {
        let p = PENDING_DMA.lock();
        for r in p.iter() {
            if !r.is_empty() {
                snap[n] = *r;
                n += 1;
            }
        }
    }
    if n == 0 {
        return true;
    }
    // (a) nicht in irgendeiner Übersetzungstabelle geführt
    {
        let t = DMA_CTX.lock();
        for r in snap[..n].iter() {
            if t.iter().filter(|c| c.used).any(|c| {
                c.regs
                    .iter()
                    .any(|q| !q.is_empty() && q.pa == r.pa && q.len == r.len)
            }) {
                return false;
            }
        }
    }
    // (b) nicht in der Freiliste des Allokators
    let mem = MEM.lock();
    !snap[..n]
        .iter()
        .any(|r| mem.overlaps_free(r.pa.raw(), r.len))
}

/// Wächter für [`dma_audit`] Code `5`: liegen die in den Kontexten geführten **PA**-Werte im
/// RAM-Fenster des Allokators? Schlägt an, sobald die Achse vertauscht wurde (eine IOVA aus einem
/// Fenster außerhalb des RAM erfüllt die Bedingung nicht) — der Fall, in dem Code 4 stillschweigend
/// nichts mehr prüft.
fn dma_ctx_regions_are_physical() -> bool {
    let floor = hal::mmu::kernel_end().max(hal::mmu::USER_RAM_MIN);
    let ceil = hal::mmu::GIB1_END;
    let t = DMA_CTX.lock();
    t.iter().filter(|c| c.used).all(|c| {
        c.regs.iter().filter(|r| !r.is_empty()).all(|r| {
            let b = r.pa.raw();
            b >= floor && b + r.len <= ceil
        })
    })
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
                for r in c.regs.iter() {
                    if r.len != 0 {
                        // **PA-Achse**: verglichen wird gegen die Freiliste des physischen
                        // Allokators. Mit der IOVA wäre der Vergleich ab Schritt b sinnlos —
                        // sie liegt in einem Fenster, das der Allokator nie vergibt, also
                        // überlappte NIE etwas und das Oracle wäre still blind geworden.
                        snap[n] = (r.pa.raw(), r.len);
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
#[cfg(feature = "selftest")]
pub(crate) mod testsupport {
    /// Kernel-Stack-Basis des Thread-Slots (0 = keiner). Fuer den Farbtest (todo A1): die
    /// Zusicherung „auch der Kernel-Stack der PD liegt in ihrem Farbsatz" muss nachpruefbar
    /// sein und nicht nur im Kommentar stehen.
    pub fn kstack_of(thread_slot: usize) -> u64 {
        super::KSTACKS.lock().base_of[thread_slot]
    }

    /// Die beiden obersten Seitentabellen der VSpace `asid` (`(l1, l2)`), oder `(0, 0)`.
    pub fn vspace_tables_of(asid: u16) -> (u64, u64) {
        if asid == 0 || asid as usize > super::MAX_VSPACES {
            return (0, 0);
        }
        let e = super::VSPACES.lock()[asid as usize - 1];
        if e.used {
            (e.l1, e.l2)
        } else {
            (0, 0)
        }
    }

    /// ASID des Thread-Slots (0 = nicht isoliert).
    pub fn asid_of(thread_slot: usize) -> u16 {
        (super::vspace_of(thread_slot) >> 48) as u16
    }

    use super::*;

    /// Anzahl Regionen im Kontext der StreamID. `0`, wenn kein Kontext.
    pub fn dma_ctx_region_count(stream_id: u32) -> usize {
        let t = DMA_CTX.lock();
        t.iter()
            .find(|c| c.used && c.sids.contains(&stream_id))
            .map(|c| c.regs.iter().filter(|r| !r.is_empty()).count())
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

    /// Den Bump des Kontexts kurz vor das Fensterende schieben, damit der Erschöpfungsfall
    /// prüfbar wird, ohne zehntausende Zuteilungen zu fahren. Gibt `false`, wenn es den Kontext
    /// nicht gibt.
    pub fn dma_ctx_exhaust_window(stream_id: u32) -> bool {
        let mut t = DMA_CTX.lock();
        match t.iter_mut().find(|c| c.used && c.sids.contains(&stream_id)) {
            Some(c) => {
                c.iova_next = c.iova_limit - IOVA_GUARD;
                true
            }
            None => false,
        }
    }

    /// Pending-DMA-Regionen: `(insgesamt, davon ausserhalb eines KILL)`.
    pub fn dma_pending_stats() -> (u32, u32) {
        (
            PENDING_TOTAL.load(Ordering::Relaxed),
            PENDING_UNEXPECTED.load(Ordering::Relaxed),
        )
    }

    /// Wie viele Geräte DMA betreiben, ohne dass ihre Adressbreite deklariert wurde.
    pub fn dma_undeclared_devices() -> u32 {
        UNDECLARED_COUNT.load(Ordering::Relaxed)
    }

    /// Zähler der laut abgewiesenen IOVA-Zuteilungen: (Fenster voll, Eingangsbreite, Gerät).
    pub fn dma_iova_rejects() -> (u32, u32, u32) {
        (
            IOVA_REJECTS[0].load(Ordering::Relaxed),
            IOVA_REJECTS[1].load(Ordering::Relaxed),
            IOVA_REJECTS[2].load(Ordering::Relaxed),
        )
    }

    /// Hat die Event-Queue in diesem Lauf nachweislich gesprochen?
    pub fn evtq_liveness_proven() -> bool {
        EVTQ_LIVENESS.load(Ordering::Acquire)
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
        hal::iommu::faults_empty()
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
        *eps()[ep].lock() = Endpoint::EMPTY;
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
            *eps()[ep].lock() = Endpoint::EMPTY;
            *ntfns()[ntfn].lock() = Notification::EMPTY;
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
    if ep < eps().len() {
        eps()[ep].lock().retire_receiver(tid)
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
    if ep >= eps().len() {
        return false;
    }
    let orphan = {
        let mut e = eps()[ep].lock();
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
    if ep >= eps().len() {
        return false;
    }
    eps()[ep].lock().migrate_owner(tid)
}

// -- A-4.2: der ruhende Punkt ----------------------------------------------------------
//
// Ein Austausch der Server-Instanz braucht einen Zeitpunkt, an dem feststeht, was offen ist.
// Ohne Stilllegung gibt es den nicht: zwischen der Frage „ist etwas offen?" und der Antwort
// kann auf einem anderen Kern ein CALL eintreffen, und die Antwort ist falsch, bevor sie
// gelesen wird. `endpoint_begin_quiesce` schliesst neue Transaktionen aus; erst danach ist
// `endpoint_quiescence_of`/`endpoint_is_idle` eine Aussage über mehr als den Moment.
//
// Der Begriff ist bewusst weiter gefasst als der Hot-Reload-Fall: `thread_quiescence`
// beantwortet dieselbe Frage systemweit und trägt damit später Z4a (Thread einfrieren).

/// **Endpoint stilllegen** (A-4.2): ab jetzt scheitern `CALL`/`RECV` mit `ERR_QUIESCING`,
/// `REPLY` bleibt erlaubt — die Menge der offenen Transaktionen kann nur noch schrumpfen.
/// Gibt `true`, falls der Endpoint dadurch **neu** stillgelegt wurde; `false`, wenn bereits
/// ein anderer Austausch läuft (dann darf der Aufrufer ihn nicht für seinen halten) oder der
/// Endpoint nicht belegt ist. Sperrt nur EPS[ep].
pub fn endpoint_begin_quiesce(ep: usize) -> bool {
    if ep >= eps().len() {
        return false;
    }
    eps()[ep].lock().begin_quiesce()
}

/// **Stilllegung beenden** (Austausch fertig oder abgebrochen). Gibt `true`, falls der
/// Endpoint stillgelegt war. Auf **jedem** Ausgang des Austauschs aufzurufen, auch dem
/// fehlgeschlagenen: ein Endpoint, der stillgelegt zurückbleibt, weist von da an jeden
/// `CALL` ab — ein Totalausfall, der aussieht wie ein laufender Austausch.
pub fn endpoint_end_quiesce(ep: usize) -> bool {
    if ep >= eps().len() {
        return false;
    }
    eps()[ep].lock().end_quiesce()
}

/// Ist dieser Endpoint gerade stillgelegt?
pub fn endpoint_is_quiescing(ep: usize) -> bool {
    ep < eps().len() && eps()[ep].lock().is_quiescing()
}

/// **Ruht der Endpoint als Ganzes?** Keine wartenden Sender/Empfänger, kein offenes
/// Reply-Token — die Bedingung, unter der ein Austausch niemanden trifft.
pub fn endpoint_is_idle(ep: usize) -> bool {
    ep >= eps().len() || eps()[ep].lock().is_idle()
}

/// **Ruhepunkt-Befund für einen Thread an einem Endpoint** — aufgeschlüsselt nach den vier
/// Rollen (siehe [`Quiescence`]).
pub fn endpoint_quiescence_of(ep: usize, tid: ThreadId) -> Quiescence {
    if ep >= eps().len() {
        return Quiescence::default();
    }
    eps()[ep].lock().quiescence_of(tid)
}

/// **Systemweiter Ruhepunkt-Befund für einen Thread** (A-4.2, und derselbe Begriff für Z4a):
/// steht `tid` in **irgendeiner** IPC-Struktur — Endpoint-Queues, Reply-Token, Reply-Pflicht
/// oder als Notification-Waiter? Die Rollen werden über alle Objekte verschmolzen, die
/// Aufschlüsselung bleibt erhalten.
///
/// **Grenze, und sie ist wichtig:** diese Auskunft ist eine Momentaufnahme, solange nicht
/// jeder betroffene Endpoint stillgelegt ist. Die Objekte werden nacheinander gesperrt (ein
/// Lock über alle wäre eine Sperrordnungs-Verletzung und bei 10064 Objekten ohnehin
/// untragbar), also kann sich ein früh gelesenes Objekt ändern, während ein spätes gelesen
/// wird. Für „ruht der Thread wirklich?" ist das nur mit vorheriger Stilllegung eine Antwort;
/// ohne sie ist es Telemetrie. Wer das verwechselt, friert einen laufenden Thread ein.
pub fn thread_quiescence(tid: ThreadId) -> Quiescence {
    let mut q = Quiescence::default();
    for i in 0..eps().len() {
        q = q.merge(eps()[i].lock().quiescence_of(tid));
    }
    for i in 0..ntfns().len() {
        q = q.merge(ntfns()[i].lock().quiescence_of(tid));
    }
    q
}

// -- A-4.1: atomares Umbinden ----------------------------------------------------------

/// **Die Server-Instanz eines Endpoints austauschen — Prüfung und Tausch unter EINEM Lock**
/// (A-4.1). Siehe [`Endpoint::rebind_server`] für die Begründung; hier zählt nur, dass es
/// **ein** `lock()` ist. Aus `endpoint_retire_receiver` + einem späteren `RECV` der neuen
/// Instanz zusammengesetzt, läge zwischen beiden ein Zustand ohne Empfänger — und ein `CALL`
/// darin wartet auf einen Server, dessen Existenz vom Gelingen des restlichen Austauschs
/// abhängt.
pub fn endpoint_rebind_server(ep: usize, old: ThreadId, new: ThreadId) -> Rebind {
    if ep >= eps().len() {
        return Rebind::NoEndpoint;
    }
    eps()[ep].lock().rebind_server(old, new)
}

/// **Eine Empfänger-Instanz binden, ohne dass sie selbst `RECV` ruft** (A-4.1). Vorbedingung:
/// `tid` ist blockiert geparkt — siehe [`Endpoint::bind_receiver`]. Der überlappende Weg
/// (`new` ruft sein `RECV` **vor** der Stilllegung) braucht diese Vorbedingung nicht.
pub fn endpoint_bind_receiver(ep: usize, tid: ThreadId) -> bool {
    ep < eps().len() && eps()[ep].lock().bind_receiver(tid)
}

/// **A-4.1-Selbsttest: die Torlogik des atomaren Umbindens, alle Ausgänge.**
///
/// Aus demselben Grund am lokalen Objekt wie [`run_quiesce`]: der Test darf keinen benutzten
/// Endpoint stilllegen. Die Empfängerqueue lässt sich hier — anders als Reply-Token und
/// Sender — über [`Endpoint::bind_receiver`] füllen, also sind **alle** Ausgänge erreichbar,
/// auch der überlappende Erfolgsfall. Nicht erreichbar bleiben `senders_waiting` und
/// `reply_open`: beide entstehen nur in `call`/`recv` und werden am echten offenen Call im
/// `rmig`-Szenario abgenommen.
#[cfg(feature = "selftest")]
pub fn run_rebind() -> bool {
    use sel4lake_ipc::RebindBlocked;
    let v1 = ThreadId::from_raw(0xA41_0001);
    let v2 = ThreadId::from_raw(0xA41_0002);
    let fremd = ThreadId::from_raw(0xA41_0003);
    let mut e = Endpoint::EMPTY;

    // Unbelegt: nichts umzubinden -- und der Grund wird benannt, nicht als "geht nicht"
    // verschwiegen.
    let leer_no_ep = e.rebind_server(v1, v2) == Rebind::NoEndpoint;
    let leer_nicht_bindbar = !e.bind_receiver(v1);

    e.mark_used();
    // **Die Kernabweisung:** ohne Stilllegung wird NICHT umgebunden, auch nicht an einem
    // ruhenden Endpoint. Der Befund waere sonst nur eine Momentaufnahme -- ein CALL auf einem
    // anderen Kern macht ihn falsch, bevor der Tausch geschieht.
    let ohne_ruhe_abgewiesen = e.rebind_server(v1, v2) == Rebind::NotQuiescing;

    e.begin_quiesce();
    // Stillgelegt, aber v1 ist gar nicht gebunden -> es gibt nichts abzuloesen.
    let ungebunden_gemeldet = e.rebind_server(v1, v2) == Rebind::NotReceiver;
    // Ein Austausch mit sich selbst meldet Erfolg, ohne einer zu sein -> eigener Ausgang.
    let selbsttausch_gemeldet = e.rebind_server(v1, v1) == Rebind::SameThread;

    // v1 binden; ein zweites Binden desselben Threads waere ein Duplikat in der Queue.
    let v1_gebunden = e.bind_receiver(v1);
    let doppelbindung_abgewiesen = !e.bind_receiver(v1);
    let v1_ist_empfaenger = e.quiescence_of(v1).as_receiver;

    // Ein FREMDER Empfaenger blockiert den Austausch: wer hier wartet, gehoert nicht zu den
    // beiden Instanzen und wuerde vom Tausch stillschweigend uebergangen.
    e.bind_receiver(fremd);
    let fremder_blockiert = e.rebind_server(v1, v2)
        == Rebind::Blocked(RebindBlocked {
            other_receiver: true,
            ..RebindBlocked::default()
        });
    e.retire_receiver(fremd);

    // **Der Erfolgsfall ohne Ueberlappung:** v2 wird hier eingereiht. Zulaessig, aber die
    // schwaechere Zusicherung -- der Aufrufer steht dafuer ein, dass v2 geparkt ist.
    let einfach = e.rebind_server(v1, v2) == Rebind::Done { overlapped: false };
    let v1_geloest = e.quiescence_of(v1).is_quiescent();
    let v2_gebunden = e.quiescence_of(v2).as_receiver;

    // **Der Erfolgsfall MIT Ueberlappung -- die starke Zusicherung.** Beide Instanzen sind
    // gebunden, das Loesen der alten hinterlaesst keine Luecke: zu keinem Zeitpunkt, auch
    // nicht innerhalb der Operation, hat der Endpoint null Empfaenger.
    e.bind_receiver(v1); // "v1" spielt hier die neue Instanz, v2 die alte
    let ueberlappend = e.rebind_server(v2, v1) == Rebind::Done { overlapped: true };
    let nur_noch_v1 = e.quiescence_of(v1).as_receiver && e.quiescence_of(v2).is_quiescent();
    // Und der Endpoint ist danach NICHT leer -- genau das ist die Aussage von A-4.1.
    let empfaenger_durchgehend = !e.is_idle();

    let ok = leer_no_ep
        && leer_nicht_bindbar
        && ohne_ruhe_abgewiesen
        && ungebunden_gemeldet
        && selbsttausch_gemeldet
        && v1_gebunden
        && doppelbindung_abgewiesen
        && v1_ist_empfaenger
        && fremder_blockiert
        && einfach
        && v1_geloest
        && v2_gebunden
        && ueberlappend
        && nur_noch_v1
        && empfaenger_durchgehend;
    println!(
        "rebind  : unbelegt -> NoEndpoint {leer_no_ep}/nicht bindbar {leer_nicht_bindbar}; OHNE \
         Stilllegung abgewiesen {ohne_ruhe_abgewiesen}; ungebunden gemeldet \
         {ungebunden_gemeldet}; Selbsttausch gemeldet {selbsttausch_gemeldet}; binden greift \
         {v1_gebunden}/Doppelbindung abgewiesen {doppelbindung_abgewiesen}/ist Empfaenger \
         {v1_ist_empfaenger}; FREMDER Empfaenger blockiert {fremder_blockiert}; Tausch ohne \
         Ueberlappung {einfach} (alt geloest {v1_geloest}, neu gebunden {v2_gebunden}); Tausch MIT \
         Ueberlappung {ueberlappend} (nur noch die neue {nur_noch_v1}, durchgehend ein Empfaenger \
         {empfaenger_durchgehend})"
    );
    println!(
        "rebind  : {} (A-4.1: Pruefung und Tausch unter EINEM Lock -- zwischen 'alter Server weg' \
         und 'neuer empfangsbereit' liegt kein Zustand ohne Empfaenger)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    ok
}

/// **A-4.3-Selbsttest: die Zustandsübergabe, alle Ausgänge.**
///
/// Läuft auf einer **eigenen** Scratch-Region, nicht auf der echten Zustandsregion des
/// Zähler-Service: der Test darf den Zustand, dessen Überleben er belegen soll, nicht selbst
/// anfassen. Er gibt die Region am Ende zurück.
///
/// Geprüft wird dieselbe Torlogik ([`state::attach`]), die der Reload-Pfad ausführt. Über den
/// regulären Pfad sind die Abweisungszweige heute **nicht** erreichbar — pro Boot gibt es genau
/// eine Fassung des Zustandslayouts, jede Übernahme liest also dieselbe Version. Erreichbar
/// werden sie erst, wenn ein Austausch zur Laufzeit ein anderes Layout mitbringt. Damit sie bis
/// dahin nicht ungeprüft bleiben (ungeprüft heisst: vermutlich kaputt, wenn sie zum ersten Mal
/// gebraucht werden), füttert der Selbsttest sie direkt — dieselbe Begründung wie bei
/// `iface_record_or_check` (A-4.4).
pub fn run_state() -> bool {
    const PROG: u32 = 0xA43_00FF;
    const VER: u32 = 7;
    const LEN: u32 = 16;

    let Some(mut region) = KernelRegionSource.request(4096, Purpose::HotReloadState) else {
        println!("state   : FAILURES (keine Scratch-Region)");
        return false;
    };

    // Vor `init` liegt dort kein Kopf: eine frische Region ist ein Kaltstart, kein Zustand der
    // Version 0. Der Allokator liefert nicht garantiert genullten Speicher -- deshalb erst nullen
    // und dann pruefen, sonst prueft dieser Fall den Zufall.
    region.view().fill(0);
    let vorher_kein_zustand =
        state::attach(&mut region.view(), PROG, VER) == Err(state::StateError::NoState);

    let angelegt = state::init(&mut region.view(), PROG, VER, LEN).is_ok();
    // Direkt nach dem Anlegen: null Uebernahmen. Ohne diesen Zaehler saehe ein still neu
    // angelegter Zustand genauso aus wie ein geerbter, der zufaellig dieselben Werte traegt.
    let gen0 = state::generation(&region.view()) == Some(0);

    // Die alte Fassung hinterlaesst etwas.
    let geschrieben = state::payload(&mut region.view())
        .map(|mut p| p.set::<u64>(0, 0xA43_BEEF))
        .unwrap_or(false);

    // **Der Erfolgsfall:** passende program_id UND state_version -> Uebernahme, Zaehler auf 1.
    let uebernommen = state::attach(&mut region.view(), PROG, VER)
        == Ok(state::Handover {
            generation: 1,
            payload_len: LEN,
        });
    // Und der Zustand ist wirklich da -- das ist die Aussage von A-4.3.
    let zustand_ueberlebt = state::payload(&mut region.view())
        .and_then(|p| p.get::<u64>(0))
        == Some(0xA43_BEEF);
    // Eine zweite Uebernahme zaehlt weiter, statt zurueckzusetzen: sonst waere die dritte
    // Fassung von der ersten nicht zu unterscheiden.
    let zaehlt_weiter = state::attach(&mut region.view(), PROG, VER)
        .map(|h| h.generation)
        == Ok(2);

    // **Die Kernabweisung:** anderes Layout -> KEIN Ok. Ohne sie laese die neue Fassung die Bytes
    // der alten in ihrem eigenen Sinn -- kein Datenverlust, sondern ein fehlinterpretierter
    // Zustand, und der faellt niemandem auf.
    let falsche_version = state::attach(&mut region.view(), PROG, VER + 1)
        == Err(state::StateError::VersionMismatch { found: VER });
    // Fremdes Programm: hier liegt der Zustand von jemand anderem, nicht "vermutlich passend".
    let fremdes_programm = state::attach(&mut region.view(), PROG + 1, VER)
        == Err(state::StateError::WrongProgram { found: PROG });
    // Und die Abweisungen haben den Zaehler NICHT erhoeht -- eine gescheiterte Uebernahme ist
    // keine.
    let abweisung_zaehlt_nicht = state::generation(&region.view()) == Some(2);

    // Der Kopf ist eine Behauptung ueber die Region, keine Tatsache: behauptet er mehr Nutzlast,
    // als die Region traegt, ist das ein eigener Befund und kein stillschweigend gekuerzter
    // Zugriff.
    let zu_gross = LEN as usize + 4096;
    region.view().set::<u32>(16, zu_gross as u32); // OFF_PAYLOAD_LEN
    let kopf_luegt = matches!(
        state::attach(&mut region.view(), PROG, VER),
        Err(state::StateError::Corrupt { .. })
    );

    // Eine Region, die kleiner ist als Kopf + gewuenschte Nutzlast, wird gar nicht erst angelegt.
    let zu_klein = state::init(&mut region.view(), PROG, VER, u32::MAX) == Err(state::StateError::TooSmall);

    KernelRegionSource.release(region);

    // Dass auch die ECHTE Zustandsregion einen gueltigen Kopf traegt, belegt dieser Test NICHT --
    // und zwar bewusst nicht mehr: die Region entsteht in der arch-neutralen Zaehler-Demo
    // (`threads::…`, Aufruf von `hotreload_state_alloc`), die auf x86 gar nicht laeuft. Hier
    // abgefragt war die Zusicherung nicht scharf, sondern nur an der falschen Stelle: sie fiel
    // durch, weil es die Region nicht gibt, nicht weil ihr Kopf fehlt. Belegt wird sie im
    // aarch64-Lauf durch `ckpt`: die Uebernahme-Generation 1 kann nur aus `state::attach` kommen,
    // und das gibt es ohne gueltigen Kopf nicht.
    let echte_region_hat_kopf = hotreload_state_generation();

    let ok = vorher_kein_zustand
        && angelegt
        && gen0
        && geschrieben
        && uebernommen
        && zustand_ueberlebt
        && zaehlt_weiter
        && falsche_version
        && fremdes_programm
        && abweisung_zaehlt_nicht
        && kopf_luegt
        && zu_klein;
    println!(
        "state   : frisch -> NoState {vorher_kein_zustand}; angelegt {angelegt} (Generation 0 \
         {gen0}, Nutzlast schreibbar {geschrieben}); UEBERNOMMEN {uebernommen} (Zustand ueberlebt \
         {zustand_ueberlebt}, zweite Uebernahme zaehlt weiter {zaehlt_weiter}); falsche \
         state_version abgewiesen {falsche_version}; fremde program_id abgewiesen \
         {fremdes_programm}; Abweisung zaehlt nicht {abweisung_zaehlt_nicht}; luegender Kopf \
         erkannt {kopf_luegt}; zu kleine Region abgewiesen {zu_klein}; echte Zustandsregion \
         {echte_region_hat_kopf:?} (auf x86 keine -- die Demo laeuft dort nicht; Beleg im \
         aarch64-Lauf via ckpt)"
    );
    println!(
        "state   : {} (A-4.3: der Zustand liegt in einer Region mit VERSIONIERTEM Kopf -- passt \
         das Layout nicht, wird abgewiesen statt fehlinterpretiert)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    ok
}

/// **A-4.2-Selbsttest: die Torlogik des ruhenden Punktes, alle Ausgänge.**
///
/// Fährt gegen [`Endpoint::gate_new_transaction`] statt gegen `call`/`recv`, aus dem dort
/// genannten Grund: über den regulären Pfad ist der Abweisungszweig nur während eines
/// laufenden Austauschs erreichbar und bliebe sonst bis zum ersten echten Hot-Reload
/// ungeprüft. Geprüft wird **dieselbe** Funktion, die `call`/`recv` ausführen.
///
/// Auf einem **lokalen** Endpoint-Objekt, nicht auf einem der echten: der Test darf keinen
/// Endpoint stilllegen, den gerade jemand benutzt. Der Rollenbefund
/// ([`Endpoint::quiescence_of`]) lässt sich hier nicht füllen — Queues und Reply-Token sind
/// privat und werden nur von `call`/`recv` gesetzt. Er wird deshalb am **echten** offenen
/// Call im Hot-Reload-Szenario (`rmig`) abgenommen, nicht an einer Attrappe.
#[cfg(feature = "selftest")]
pub fn run_quiesce() -> bool {
    use sel4lake_abi::result;
    let fremd = ThreadId::from_raw(0xA42_0001);
    let mut e = Endpoint::EMPTY;

    // Unbelegter Endpoint: nicht stilllegbar, und das Tor weist mit ERR_BADCAP ab -- NICHT mit
    // ERR_QUIESCING. Die beiden Gründe duerfen nicht verschwimmen: "gibt es nicht" ist fuer
    // einen Client eine andere Lage als "kommt gleich wieder".
    let leer_nicht_stilllegbar = !e.begin_quiesce();
    let leer_badcap = e.gate_new_transaction() == Some(result::ERR_BADCAP);
    // Ein unbelegter (recycelter) Endpoint darf keine Rollen melden, sonst schleppte er den
    // Befund seines Vorbesitzers weiter.
    let leer_ohne_rollen = e.quiescence_of(fremd).is_quiescent();

    e.mark_used();
    let frisch_offen = e.gate_new_transaction().is_none();
    let frisch_ruhig = e.is_idle();

    // Stilllegen greift; ein ZWEITER Aufruf meldet false. Das ist kein Schoenheitsfehler:
    // zwei gleichzeitige Austausche am selben Endpoint wuerden sich gegenseitig die Freigabe
    // ziehen -- der erste `end_quiesce` oeffnete das Tor mitten im zweiten Austausch.
    let erste_stilllegung = e.begin_quiesce();
    let zweite_abgewiesen = !e.begin_quiesce();
    let tor_zu = e.gate_new_transaction() == Some(result::ERR_QUIESCING);
    let meldet_still = e.is_quiescing();
    // Stillgelegt heisst NICHT beschaeftigt: ein Endpoint ohne offene Transaktion ruht auch
    // waehrend der Stilllegung -- genau das ist die Bedingung, unter der ausgetauscht werden
    // darf.
    let still_und_ruhig = e.is_idle();

    let freigabe = e.end_quiesce();
    let tor_wieder_offen = e.gate_new_transaction().is_none();
    let doppelte_freigabe_gemeldet = !e.end_quiesce();

    let ok = leer_nicht_stilllegbar
        && leer_badcap
        && leer_ohne_rollen
        && frisch_offen
        && frisch_ruhig
        && erste_stilllegung
        && zweite_abgewiesen
        && tor_zu
        && meldet_still
        && still_und_ruhig
        && freigabe
        && tor_wieder_offen
        && doppelte_freigabe_gemeldet;
    println!(
        "quiesce : unbelegt nicht stilllegbar {leer_nicht_stilllegbar}; unbelegt -> BADCAP (nicht \
         QUIESCING) {leer_badcap}; unbelegt ohne Rollen {leer_ohne_rollen}; frisch offen \
         {frisch_offen}/ruhig {frisch_ruhig}; stilllegen greift {erste_stilllegung}; ZWEITER \
         Austausch abgewiesen {zweite_abgewiesen}; Tor zu {tor_zu}; meldet still {meldet_still}; \
         still+ohne offene Transaktion = ruhig {still_und_ruhig}; freigeben greift {freigabe}; Tor \
         wieder offen {tor_wieder_offen}; doppelte Freigabe gemeldet {doppelte_freigabe_gemeldet}"
    );
    println!(
        "quiesce : {} (A-4.2: der ruhende Punkt -- waehrend eines Austauschs wird KEINE neue \
         Transaktion eroeffnet, laufende duerfen abschliessen)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    ok
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
    // `eps().len()` Endpoints zugleich sein -> die Flaeche MUSS so gross sein, sonst wuerden
    // Waisen jenseits der Kapazitaet still verworfen und ihre Aufrufer haengen dauerhaft
    // (Liveness-Bug).
    //
    // **A-3.4 Teil 4 — warum das hier keine lokale Variable mehr ist:** bis Teil 3 stand hier
    // `[Option<ThreadId>; NENDPOINTS]`, also 32 Eintraege = 768 Byte auf dem Kernelstack. Mit
    // 10 000 Endpoints waeren daraus 240 KiB geworden -- auf einem Stack, der ein Vielfaches
    // kleiner ist, und das im **Todespfad** eines Threads. Die Zusage der Zeile darueber
    // (`so gross wie die Endpoint-Zahl`) haette den Stack ueberrannt, statt Waisen zu verlieren:
    // ein Stack-Overflow statt eines Liveness-Bugs. Dieselbe Falle, die A-3.3 fuer den
    // Finalisierungspuffer aufgeloest hat -- die Flaeche kommt jetzt aus dem Boot-RAM.
    //
    // **Der Lock wird ueber die ganze Funktion gehalten**, und das ist Absicht: die Flaeche ist
    // jetzt geteilt statt lokal. Wer sie zum Einsammeln sperrt, sie zum Entblocken freigibt und
    // danach wieder liest, liest moeglicherweise die Waisen eines anderen Kerns -- ein zweiter
    // sterbender Thread wuerde die Eintraege dazwischen ueberschreiben. Der Preis ist, dass
    // Thread-Tode kernuebergreifend serialisieren; das ist ein seltener, kalter Pfad.
    //
    // Sperrordnung: IPC_ORPHANS ist damit **aeusserer** Lock ->
    // IPC_ORPHANS < EPS[i]/NTFNS[i] < SCHEDS. Kein anderer Pfad nimmt ihn, insbesondere keiner
    // mit gehaltenem EPS oder SCHEDS -> kein Zyklus.
    let mut orphans = IPC_ORPHANS.lock();
    let mut no = 0usize;
    for ep in eps().iter() {
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
    } // EPS-Locks sind hier alle wieder frei
    for n in ntfns().iter() {
        let mut nt = n.lock();
        if nt.is_used() {
            nt.purge_thread(tid);
        }
    }
    for i in 0..no {
        if let Some(caller) = orphans[i].take() {
            unblock_with_error(caller, sel4lake_abi::result::ERR_SERVER_GONE);
        }
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
    for ep in eps().iter() {
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
    for n in ntfns().iter() {
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
    let cdt = cap_audit_cdt();
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
