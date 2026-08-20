//! **NUMA in the kernel** (Z8, N0–N4) — reading the topology, reporting it, and placing by it.
//!
//! The classification and every decoder live dependency-free in `caprock_hal::numa`; this module
//! is the glue: it holds the one topology, fills it from whichever source the architecture has,
//! prints the `numa` line and answers "which node".
//!
//! ## Why the report line matters more than usual here
//!
//! Under QEMU the topology is **emulated**: `-numa node,… -numa dist,…` hands the guest an SRAT
//! and a SLIT (x86) or `numa-node-id` properties (aarch64), but the "remote" node is ordinary host
//! memory at ordinary host speed. **Verifiable is where a page came from and where a thread runs;
//! the benefit is not.** No green line here may be read as "it is faster" — the same limitation as
//! the SMMU-under-QEMU finding (ADR 0008) and the SMT one (§12a).
//!
//! On top of that the development machine has exactly **one** node. Without `-numa` every run
//! reports a single node and the placement question never arises — vacuous in the same way the
//! `smt` line is vacuous under `threads=1`. `tools/numa-messen.sh` is the run where it is not.

use crate::system;
use caprock_hal::numa::{Node, PlacementStats, Topology};
use caprock_hal::println;
use caprock_sync::SpinLock;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The machine's topology. Written once during bring-up, read afterwards.
///
/// Behind a lock rather than a `static mut`: it is written before the APs run, but "before" is a
/// claim about ordering that nothing here enforces, and this kernel has already paid for exactly
/// that assumption once (D0 — a thread was runnable before it had its PD).
static TOPO: SpinLock<Topology> = SpinLock::new(Topology::new());

/// Has [`init`] run? Distinguishes "one node" from "never looked" — the whole point of
/// `Unaffiliated`, one level up.
static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Placement outcomes, counted **by effect**.
static EXACT: AtomicU64 = AtomicU64::new(0);
static OFF_NODE: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// Read the topology from ACPI (x86: SRAT + SLIT).
#[cfg(target_arch = "x86_64")]
pub fn init() {
    let mut t = Topology::new();
    if let Some(b) = caprock_hal::acpi::srat_table() {
        caprock_hal::numa::decode_srat(b, &mut t);
    }
    if let Some(b) = caprock_hal::acpi::slit_table() {
        caprock_hal::numa::decode_slit(b, &mut t);
    }
    *TOPO.lock() = t;
    INITIALISED.store(true, Ordering::Release);
}

/// Read the topology from the device tree (aarch64: `numa-node-id` on `memory@`/`cpu@`).
#[cfg(target_arch = "aarch64")]
pub fn init(dtb: &[u8]) {
    let mut t = Topology::new();
    if let Some(d) = caprock_dtb::Dtb::parse(dtb) {
        // Zwei Rueckrufe statt einer Struktur aus der DTB-Crate: die Regeln fuer Abschneiden und
        // Nicht-Zuordnung sollen fuer BEIDE Architekturen dieselben sein, und dafuer muessen sie
        // an einer Stelle stehen.
        let mut seen = 0usize;
        {
            let tt = &mut t;
            if let Some(n) = d.numa(
                |base, len, node| tt.add_range(base, len, node),
                |_id, _node| {},
            ) {
                seen += n;
            }
        }
        // Die CPU-Zuordnung in einem zweiten Lauf: `numa` nimmt zwei `FnMut`, und beide muessten
        // sonst gleichzeitig `&mut t` halten.
        {
            let tt = &mut t;
            let _ = d.numa(|_b, _l, _n| {}, |id, node| tt.add_cpu(id, node));
        }
        if seen > 0 {
            t.mark_readable();
        }
    }
    *TOPO.lock() = t;
    INITIALISED.store(true, Ordering::Release);
}

/// A copy of the topology (it is `Copy` and small enough that handing out a snapshot beats
/// handing out the lock).
pub fn topology() -> Topology {
    *TOPO.lock()
}

/// Which node holds this physical address?
pub fn node_of_pa(pa: u64) -> Node {
    TOPO.lock().node_of_pa(pa)
}

/// **Which node does this core sit on?** (N3)
///
/// Its own named quantity, never a meaning folded into the load counter — *ein Parameter, der zwei
/// Bedeutungen traegt* is the class that produced D9 and the `spawn_user` reap bug, and Z6 stage 1
/// has just paid for the same shape again (`CORE_LOAD` doubling as "is this core alive").
pub fn node_of_core(core: usize) -> Node {
    TOPO.lock().node_of_cpu(core as u32)
}

/// Which node does this logical CPU id sit on? (x86: LAPIC/x2APIC id.)
///
/// Separate from [`node_of_core`] although both are `u32` today: the LAPIC id and the scheduler's
/// core index are the same number only while APIC ids are dense from zero, which `hal::cpu::core_id`
/// documents as an assumption of this platform and not a law. Two names keep the day they diverge
/// from being silent.
pub fn node_of_cpu(apic_id: u32) -> Node {
    TOPO.lock().node_of_cpu(apic_id)
}

/// **The placement ladder** (N2) — allocate `size` bytes preferring node `want`.
///
/// The rungs are `caprock_hal::numa::YIELD_ORDER`, and the outcome is counted by **effect**: an
/// allocation that never had a node to miss is not a miss. The E-Rest 3b counter reported `1x` on
/// a machine that had no high memory at all, having counted a request that fitted nowhere.
///
/// **Zone is not in this ladder, and that is deliberate.** In `system::zoned_alloc` the zone is a
/// *preference with a fallback*, not a condition; the hard-condition path is `mem_alloc_below`,
/// which returns `None` rather than an unusable address. So "zone never yields" (todo Z8) is a
/// statement about the *condition* path, and this ladder sits on the *preference* path.
pub fn alloc_on_node(size: u64, align: u64, want: Node) -> Option<caprock_mem::MemoryCap> {
    let Node::At(n) = want else {
        // Kein Wunsch -> keine Leiter, und vor allem KEIN gezaehlter Fehlschlag.
        return system::alloc_anywhere(size, align);
    };
    let t = topology();
    if t.trustworthy() {
        let mut i = 0usize;
        while let Some((lo, hi)) = t.window_of(n, i) {
            if let Some(c) = system::alloc_in_window(size, align, lo, hi) {
                EXACT.fetch_add(1, Ordering::Relaxed);
                return Some(c);
            }
            i += 1;
        }
    }
    // Sprosse 2: der Knoten gibt nach. Gezaehlt wird das NUR, wenn wirklich woanders belegt
    // wurde -- „es war kein Platz" und „es kam von einem anderen Knoten" sind zwei Aussagen.
    match system::alloc_anywhere(size, align) {
        Some(c) => {
            match t.node_of_pa(c.base()) {
                Node::At(got) if got == n => EXACT.fetch_add(1, Ordering::Relaxed),
                _ => OFF_NODE.fetch_add(1, Ordering::Relaxed),
            };
            Some(c)
        }
        None => {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// N3 placement outcomes. Two counters, because "the node had no running core" and "the thread
/// went to a foreign node" are two statements — and only the second is a fact about placement.
static CORE_LOCAL: AtomicU64 = AtomicU64::new(0);
static CORE_REMOTE: AtomicU64 = AtomicU64::new(0);

/// Record one core placement: `local` = a core of the requested node was found.
pub fn note_core_placement(local: bool) {
    if local {
        CORE_LOCAL.fetch_add(1, Ordering::Relaxed);
    } else {
        CORE_REMOTE.fetch_add(1, Ordering::Relaxed);
    }
}

/// `(local, remote)` core placements.
pub fn core_placements() -> (u64, u64) {
    (CORE_LOCAL.load(Ordering::Relaxed), CORE_REMOTE.load(Ordering::Relaxed))
}

/// The counters, as the report line reads them.
pub fn stats() -> PlacementStats {
    PlacementStats {
        exact: EXACT.load(Ordering::Relaxed),
        off_node: OFF_NODE.load(Ordering::Relaxed),
        off_node_uncolored: 0,
        refused: REFUSED.load(Ordering::Relaxed),
    }
}

/// **The verdict of the `numa` line.**
///
/// Three ways it can fail, and none of them is "the machine has one node":
///
/// * `init` never ran — then every other number is meaningless and must not read as "flat".
/// * the topology is readable but **truncated** — a picture with holes that looks whole.
/// * something was placed and the placement went off-node **without** the topology being
///   trustworthy, i.e. we cannot even say where it went.
///
/// A single-node machine passes with `nodes=1`, and that is a *reading*, not an absence. The
/// difference is [`INITIALISED`].
pub fn urteil() -> bool {
    if !INITIALISED.load(Ordering::Acquire) {
        return false;
    }
    let t = topology();
    // Nicht lesbar ist erlaubt (die meisten Maschinen haben keine SRAT), abgeschnitten nicht:
    // das eine ist „keine Aussage", das andere ist „eine falsche Aussage".
    if t.truncated {
        return false;
    }
    let s = stats();
    // Wurde platziert, ohne dass die Topologie tragfaehig ist, ist die Buchfuehrung wertlos.
    if s.off_node > 0 && !t.trustworthy() {
        return false;
    }
    true
}

/// Print the `numa` line. Called once from each bring-up path.
pub fn bericht() {
    let t = topology();
    let s = stats();
    println!(
        "numa    : init={} readable={} trustworthy={} truncated={} nodes={} ranges={} cpus={} \
         distances={} covered={} MiB",
        INITIALISED.load(Ordering::Acquire),
        t.readable,
        t.trustworthy(),
        t.truncated,
        t.node_count(),
        t.range_count(),
        t.cpu_count(),
        t.distances_present,
        t.covered_bytes() >> 20
    );
    // Die Platzierungszahlen NACH WIRKUNG. `off_node=0` heisst „kam nicht vor", nicht „geht
    // nicht" -- und `speaking=false` heisst, dass diese Zeile ueber Platzierung gar nichts sagt.
    println!(
        "numa    : placed exact={} off_node={} refused={} speaking={} -- QEMU emuliert die \
         TOPOLOGIE, nicht die LATENZ: geprueft ist WOHER eine Seite kam, nie ob es schneller ist",
        s.exact,
        s.off_node,
        s.refused,
        s.speaking()
    );
    let (cl, cr) = core_placements();
    println!(
        "numa    : N3 core placement local={cl} remote={cr} -- 'remote' heisst NUR: der gewuenschte \
         Knoten hatte keinen laufenden Kern. 0/0 heisst 'kam nicht vor', nicht 'geht nicht'"
    );
    if t.distances_present && t.node_count() >= 2 {
        println!(
            "numa    : distance(0,0)={:?} distance(0,1)={:?} (ACPI: 10 = lokal)",
            t.distance(0, 0),
            t.distance(0, 1)
        );
    }
    println!("numa    : {}", if urteil() { "ALL PASS" } else { "FAILURES" });
}
