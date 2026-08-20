//! **SMT topology: reading it, and the admission policy built on it** (Z6 stages 0+1, 2026-08-17).
//!
//! Arithmetic and classification, no hardware — the same reason `dmar.rs`, `cache_decode.rs` and
//! `iommu_health.rs` live where they live: a decode over injected register values is triggerable
//! with literals, a bring-up path needs a machine.
//!
//! ## Why this exists at all
//!
//! §12 of `docs/invariants.md` says the A1 colouring separates the **LLC** and explicitly not the
//! core-local structures — L1, L2, TLB, store buffer, branch predictor are shared between sibling
//! hyperthreads. That sentence has been true and *unenforced*: the scheduler read no topology, the
//! MADT loop brought every enabled LAPIC online, and two protection domains could share one
//! physical core at any instant.
//!
//! The decisive asymmetry, and the reason no amount of care in the switch path helps: **every
//! mitigation this kernel owns is switch-shaped.** Colouring picks cache sets, scrubbing happens
//! at a context switch — both need a moment at which one domain stops and another starts. SMT has
//! no such moment. The sibling runs *simultaneously*. This is also why the vendors' own MDS
//! mitigation (buffer clearing on kernel exit) does not cover the cross-sibling direction: there
//! is nothing "in between" to clear.
//!
//! So exactly one measure works, and it is scheduling: **a physical core belongs to at most one
//! trust domain at any instant.** Stage 1 implements the crude, complete form of that — at most
//! **one** logical CPU per physical core is admitted at all. Gang-scheduling siblings of the same
//! domain (Z6 stage 3) is the refinement that buys the throughput back; it needs a notion of trust
//! domain that this kernel does not yet have, and it must not be built before that notion exists.
//!
//! ## What this does NOT cover — and it is a decision, not an oversight
//!
//! Stage 1 removes sibling sharing entirely, so it covers tenant-against-tenant **and**
//! tenant-against-kernel. Stage 3 will not: once siblings of one domain run together, a syscall or
//! interrupt taken on one sibling runs **kernel** code alongside **user** code of the other — and
//! the kernel holds secrets of other tenants (the trusted-key store, foreign frames on the sidecar
//! path). Linux's core scheduling has exactly this hole; the sibling-stunning half that would
//! close it was the expensive part and never landed. For an IPC-heavy microkernel it is more
//! expensive still, because kernel entry is the common case rather than the rare one.
//!
//! That decision belongs in `docs/invariants.md` **before** the trust-domain concept is written,
//! because it determines what a trust domain has to mean for the scheduler. It is recorded there
//! under §12; this module implements the stage that has no such hole.
//!
//! ## `Unknown` is not `Single`
//!
//! *Null ist ein Befund, kein Messwert.* A topology we cannot read must not decode to "one thread
//! per core" — that is the F1/`NOSEL_TEXT` failure exactly: a measurement that fails looks like a
//! measurement that passed. It gets its own variant, and the policy treats it as *worst case*.

/// What the topology enumeration said. Three outcomes, and the third is the point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtTopology {
    /// Readable, and it reports one thread per physical core. No siblings exist.
    Single,
    /// Readable, `width` threads per physical core; two logical IDs are siblings iff they agree
    /// above the lowest `shift` bits.
    Multi { width: u8, shift: u8 },
    /// **Not readable.** Not the same thing as [`Single`](SmtTopology::Single) — we do not know
    /// whether siblings exist, and we could not name them if they did.
    Unknown,
}

impl SmtTopology {
    /// Threads per physical core, or `None` if the topology could not be read.
    pub fn width(&self) -> Option<u8> {
        match *self {
            SmtTopology::Single => Some(1),
            SmtTopology::Multi { width, .. } => Some(width),
            SmtTopology::Unknown => None,
        }
    }

    /// Bits that separate siblings, or `None` if unreadable.
    pub fn shift(&self) -> Option<u8> {
        match *self {
            SmtTopology::Single => Some(0),
            SmtTopology::Multi { shift, .. } => Some(shift),
            SmtTopology::Unknown => None,
        }
    }

    /// Did the enumeration produce a verdict at all? The **speaking test** of every caller: a
    /// policy that reports "nothing suppressed" is only meaningful if the topology was read.
    pub fn readable(&self) -> bool {
        !matches!(self, SmtTopology::Unknown)
    }

    /// The physical core a logical CPU sits on, or `None` when unreadable.
    pub fn physical_core(&self, logical_id: u32) -> Option<u32> {
        match *self {
            SmtTopology::Single => Some(logical_id),
            SmtTopology::Multi { shift, .. } => Some(logical_id >> shift),
            SmtTopology::Unknown => None,
        }
    }
}

// -------------------------------------------------------------------------------------------
// Decoding
// -------------------------------------------------------------------------------------------

/// Level type in `ECX[15:8]` of `CPUID.0Bh`/`1Fh`: the SMT level.
const LEVEL_SMT: u32 = 1;
/// Level type 0 terminates the enumeration.
const LEVEL_INVALID: u32 = 0;

/// Decode `CPUID` leaf `0Bh` (Extended Topology) or `1Fh` (V2), given the subleaf results in
/// order as `(eax, ebx, ecx, edx)`.
///
/// **Leaf `0Bh`, not leaf `4`.** `CPUID.4:EAX[25:14]` also yields the SMT width for the L1 level
/// and is already in a register in `cache.rs` — but leaf 4 is Intel's *deterministic cache
/// parameters* leaf, and AMD populates topology through entirely different leaves. `0Bh`/`1Fh` is
/// the architectural topology source on both vendors, and the likely deployment target is EPYC.
///
/// Every failure mode ends in [`SmtTopology::Unknown`]:
/// * no subleaves at all (leaf unsupported — `CPUID.0:EAX < 0Bh`),
/// * the first subleaf reports an invalid level type (leaf present but not populated),
/// * an SMT level whose processor count is `0` (level exists but is not enumerable).
pub fn decode_topology_leaf(subleaves: &[(u32, u32, u32, u32)]) -> SmtTopology {
    if subleaves.is_empty() {
        return SmtTopology::Unknown;
    }
    for &(eax, ebx, ecx, _edx) in subleaves {
        let level_type = (ecx >> 8) & 0xFF;
        if level_type == LEVEL_INVALID {
            break; // Ende der Aufzaehlung
        }
        if level_type != LEVEL_SMT {
            continue;
        }
        let count = ebx & 0xFFFF;
        if count == 0 {
            // Die Ebene existiert, ist aber nicht aufzaehlbar. Das ist genau der Fall, in dem
            // eine bequeme Fassung `1` zurueckgaebe -- also „kein SMT", ohne es gemessen zu haben.
            return SmtTopology::Unknown;
        }
        if count == 1 {
            return SmtTopology::Single;
        }
        let shift = (eax & 0x1F) as u8;
        if shift == 0 {
            // Breite > 1, aber keine Bits, die Geschwister trennen: die beiden Angaben
            // widersprechen sich. Zwei Quellen, die sich widersprechen, sind keine Quelle.
            return SmtTopology::Unknown;
        }
        return SmtTopology::Multi { width: count.min(u8::MAX as u32) as u8, shift };
    }
    SmtTopology::Unknown
}

/// Bit 24 of `MPIDR_EL1`: the lowest affinity level (Aff0) consists of multithreaded PEs.
const MPIDR_MT: u64 = 1 << 24;

/// Decode the aarch64 topology from `MPIDR_EL1`.
///
/// `MT == 0` is a real reading: Aff0 numbers physical PEs, no siblings exist. `MT == 1` is
/// **`Unknown` on purpose**, and for two independent reasons:
///
/// 1. `MPIDR_EL1` does not carry the thread count. ARM leaves it to firmware tables (PPTT), which
///    this kernel does not read.
/// 2. `hal::cpu::core_id()` is `MPIDR & 0xff`, i.e. Aff0 alone. With `MT == 1` Aff0 is the
///    *thread* index within a core, so two different cores' thread 0 both return `0` — the
///    logical-ID scheme itself collides, and every `SCHEDS[core]` index with it. Naming siblings
///    would be the smaller of the two problems.
///
/// Reporting `Unknown` there is therefore not caution, it is accuracy.
pub fn decode_mpidr(mpidr_el1: u64) -> SmtTopology {
    if mpidr_el1 & MPIDR_MT == 0 {
        SmtTopology::Single
    } else {
        SmtTopology::Unknown
    }
}

// -------------------------------------------------------------------------------------------
// The admission policy (stage 1)
// -------------------------------------------------------------------------------------------

/// Highest physical core ID [`CoreOccupancy`] can track. Beyond it the claim is **refused**, not
/// silently wrapped — a bitmap that wraps would report "free" for an occupied core.
pub const MAX_PHYSICAL_CORES: u32 = 256;

/// What happened to one admission attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// First logical CPU on this physical core — bring it up.
    Admit,
    /// Another logical CPU of the same physical core is already admitted. **This is the case the
    /// whole module exists for.**
    SiblingSuppressed,
    /// The topology could not be read, so siblings cannot be named. Only the boot CPU runs.
    TopologyUnknown,
    /// The physical core ID is beyond [`MAX_PHYSICAL_CORES`].
    OutOfRange,
}

/// Which physical cores already hold an admitted logical CPU. A 256-bit map rather than a list:
/// it is 32 bytes on the bring-up stack instead of a kilobyte, and the duplicate test is O(1).
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreOccupancy {
    bits: [u64; (MAX_PHYSICAL_CORES / 64) as usize],
}

impl CoreOccupancy {
    pub const fn new() -> Self {
        Self { bits: [0; (MAX_PHYSICAL_CORES / 64) as usize] }
    }

    /// Try to admit `logical_id` under `topo`. On [`Claim::Admit`] the physical core is marked
    /// occupied; every other outcome leaves the map untouched.
    pub fn claim(&mut self, topo: &SmtTopology, logical_id: u32) -> Claim {
        let Some(core) = topo.physical_core(logical_id) else {
            return Claim::TopologyUnknown;
        };
        if core >= MAX_PHYSICAL_CORES {
            return Claim::OutOfRange;
        }
        let (w, b) = ((core / 64) as usize, core % 64);
        if self.bits[w] & (1 << b) != 0 {
            return Claim::SiblingSuppressed;
        }
        self.bits[w] |= 1 << b;
        Claim::Admit
    }

    /// How many distinct physical cores are occupied.
    pub fn occupied(&self) -> u32 {
        self.bits.iter().map(|w| w.count_ones()).sum()
    }
}

/// **The verdict the report line prints.** Deliberately not a bare `bool`: the interesting failure
/// is not "two siblings ran", it is "the check could not have noticed".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionVerdict {
    /// The topology was readable.
    pub readable: bool,
    /// No two CPUs that came online share a physical core.
    ///
    /// **`None` means undecidable, and that is why this is not a `bool`.** With an unreadable
    /// topology the question cannot be answered at all, and encoding "could not check" as `false`
    /// — or worse, as `true` — is the `NOSEL_TEXT` failure: a measurement that did not happen
    /// reads like one that passed.
    pub no_shared_core: Option<bool>,
    /// The undecidable case is contained: at most the boot CPU is running.
    pub contained: bool,
}

impl AdmissionVerdict {
    pub fn ok(&self) -> bool {
        match self.no_shared_core {
            Some(v) => v,
            // Unentscheidbar -> es zaehlt nur, dass der Fall eingedaemmt ist.
            None => self.contained,
        }
    }
}

/// Judge a completed bring-up **by recomputing from the IDs that actually came online**.
///
/// Two properties of this function are the content:
///
/// 1. **It does not read the policy's own bookkeeping.** A fresh [`CoreOccupancy`] is filled from
///    `online`, so the check re-derives the answer from the raw logical IDs. Asking the policy's
///    map whether the policy was right is *ein Schreiber, der sein eigenes Ergebnis bestaetigt* —
///    it holds by construction and would survive any bug in [`CoreOccupancy::claim`].
/// 2. **It counts the opportunity, not the hit.** "Two tenants shared a physical core" is a rare
///    event; a checker that only speaks when it happens is silent in almost every run — the
///    `pdbind` lesson. "Does every online CPU own its physical core?" is decidable in *every* run,
///    on every machine, whether or not anything went wrong.
///
/// An ID beyond [`MAX_PHYSICAL_CORES`] counts as a violation rather than as an unremarkable skip:
/// it cannot be verified, and unverifiable is not the same as fine.
pub fn judge(topo: &SmtTopology, online: &[u32]) -> AdmissionVerdict {
    let mut fresh = CoreOccupancy::new();
    let mut shared = false;
    let mut decidable = topo.readable();
    for &id in online {
        match fresh.claim(topo, id) {
            Claim::Admit => {}
            Claim::SiblingSuppressed => shared = true,
            Claim::TopologyUnknown => decidable = false,
            Claim::OutOfRange => shared = true,
        }
    }
    AdmissionVerdict {
        readable: topo.readable(),
        no_shared_core: if decidable { Some(!shared) } else { None },
        contained: online.len() <= 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(eax, ebx, ecx, edx)` for one subleaf of leaf 0Bh.
    fn sub(shift: u32, count: u32, level_type: u32, idx: u32) -> (u32, u32, u32, u32) {
        (shift, count, (level_type << 8) | idx, 0)
    }

    // --- decode: leaf 0Bh -------------------------------------------------------------------

    #[test]
    fn two_way_smt_is_read_as_width_two_shift_one() {
        // Der uebliche Intel-/AMD-Fall: SMT-Ebene mit 2 Prozessoren, 1 Bit Verschiebung.
        let leaves = [sub(1, 2, LEVEL_SMT, 0), sub(4, 12, 2, 1), sub(0, 0, LEVEL_INVALID, 2)];
        assert_eq!(decode_topology_leaf(&leaves), SmtTopology::Multi { width: 2, shift: 1 });
    }

    #[test]
    fn one_thread_per_core_is_read_as_single() {
        let leaves = [sub(0, 1, LEVEL_SMT, 0), sub(3, 8, 2, 1), sub(0, 0, LEVEL_INVALID, 2)];
        assert_eq!(decode_topology_leaf(&leaves), SmtTopology::Single);
    }

    /// **Die tragende Unterscheidung.** Kein Blatt heisst *nicht* „ein Thread je Kern".
    #[test]
    fn an_absent_leaf_is_unknown_and_not_single() {
        assert_eq!(decode_topology_leaf(&[]), SmtTopology::Unknown);
        assert_ne!(decode_topology_leaf(&[]), SmtTopology::Single);
    }

    #[test]
    fn a_leaf_that_is_present_but_empty_is_unknown() {
        // Erstes Unterblatt meldet sofort Ebenentyp 0 -> nichts aufgezaehlt.
        assert_eq!(decode_topology_leaf(&[sub(0, 0, LEVEL_INVALID, 0)]), SmtTopology::Unknown);
    }

    #[test]
    fn an_smt_level_with_zero_processors_is_unknown() {
        // Der bequeme Fehler waere, `0` als `1` zu lesen und „kein SMT" zu melden.
        assert_eq!(decode_topology_leaf(&[sub(1, 0, LEVEL_SMT, 0)]), SmtTopology::Unknown);
    }

    #[test]
    fn width_above_one_without_a_shift_contradicts_itself_and_is_unknown() {
        assert_eq!(decode_topology_leaf(&[sub(0, 2, LEVEL_SMT, 0)]), SmtTopology::Unknown);
    }

    #[test]
    fn a_leaf_without_an_smt_level_is_unknown() {
        // Nur eine Core-Ebene, keine SMT-Ebene: nicht als „Single" raten.
        let leaves = [sub(3, 8, 2, 0), sub(0, 0, LEVEL_INVALID, 1)];
        assert_eq!(decode_topology_leaf(&leaves), SmtTopology::Unknown);
    }

    #[test]
    fn four_way_smt_shifts_by_two() {
        let leaves = [sub(2, 4, LEVEL_SMT, 0), sub(0, 0, LEVEL_INVALID, 1)];
        let t = decode_topology_leaf(&leaves);
        assert_eq!(t, SmtTopology::Multi { width: 4, shift: 2 });
        // Logische 0..3 liegen auf physischem Kern 0, 4..7 auf Kern 1.
        assert_eq!(t.physical_core(3), Some(0));
        assert_eq!(t.physical_core(4), Some(1));
    }

    // --- decode: MPIDR ----------------------------------------------------------------------

    #[test]
    fn mpidr_without_the_mt_bit_is_single() {
        assert_eq!(decode_mpidr(0x8000_0003), SmtTopology::Single);
    }

    #[test]
    fn mpidr_with_the_mt_bit_is_unknown_because_the_width_is_not_in_it() {
        assert_eq!(decode_mpidr(0x8100_0003), SmtTopology::Unknown);
    }

    // --- policy -----------------------------------------------------------------------------

    #[test]
    fn siblings_are_suppressed_and_distinct_cores_are_not() {
        let t = SmtTopology::Multi { width: 2, shift: 1 };
        let mut occ = CoreOccupancy::new();
        // LAPIC-IDs 0,1 sind Geschwister; 2,3 sind das naechste Paar.
        assert_eq!(occ.claim(&t, 0), Claim::Admit);
        assert_eq!(occ.claim(&t, 1), Claim::SiblingSuppressed);
        assert_eq!(occ.claim(&t, 2), Claim::Admit);
        assert_eq!(occ.claim(&t, 3), Claim::SiblingSuppressed);
        assert_eq!(occ.occupied(), 2, "vier logische CPUs, zwei physische Kerne");
    }

    #[test]
    fn without_smt_nothing_is_suppressed() {
        let t = SmtTopology::Single;
        let mut occ = CoreOccupancy::new();
        for id in 0..4 {
            assert_eq!(occ.claim(&t, id), Claim::Admit);
        }
        assert_eq!(occ.occupied(), 4);
    }

    /// **Fail-closed.** Ohne lesbare Topologie laesst sich kein Geschwister benennen — also faehrt
    /// nur der Bootkern.
    #[test]
    fn an_unreadable_topology_admits_nobody() {
        let t = SmtTopology::Unknown;
        let mut occ = CoreOccupancy::new();
        assert_eq!(occ.claim(&t, 1), Claim::TopologyUnknown);
        assert_eq!(occ.claim(&t, 2), Claim::TopologyUnknown);
        assert_eq!(occ.occupied(), 0);
    }

    #[test]
    fn a_core_id_beyond_the_map_is_refused_not_wrapped() {
        let t = SmtTopology::Single;
        let mut occ = CoreOccupancy::new();
        assert_eq!(occ.claim(&t, MAX_PHYSICAL_CORES), Claim::OutOfRange);
        assert_eq!(occ.claim(&t, MAX_PHYSICAL_CORES + 64), Claim::OutOfRange);
        assert_eq!(occ.occupied(), 0, "eine abgewiesene Forderung belegt nichts");
    }

    // --- the verdict ------------------------------------------------------------------------

    /// **Sprechprobe in beide Richtungen.** Ein Urteil, das nicht durchfallen kann, ist keins —
    /// und `judge` muss die Doppelbelegung sehen, ohne die Politik zu fragen.
    #[test]
    fn the_verdict_can_both_pass_and_fail() {
        let t = SmtTopology::Multi { width: 2, shift: 1 };
        // Zwei CPUs auf zwei verschiedenen physischen Kernen.
        let good = judge(&t, &[0, 2]);
        assert_eq!(good.no_shared_core, Some(true));
        assert!(good.ok());
        // 0 und 1 sind Geschwister -- das muss `judge` allein aus den IDs sehen.
        let bad = judge(&t, &[0, 1]);
        assert_eq!(bad.no_shared_core, Some(false));
        assert!(!bad.ok(), "zwei Geschwister online muessen durchfallen");
    }

    /// **Der Grund, warum `no_shared_core` ein `Option` ist.** Unentscheidbar darf weder als
    /// „bestanden" noch als „durchgefallen" erscheinen — sonst sieht eine ausgefallene Messung
    /// aus wie eine Aussage.
    #[test]
    fn an_unreadable_topology_makes_the_question_undecidable_not_false() {
        let v = judge(&SmtTopology::Unknown, &[0]);
        assert_eq!(v.no_shared_core, None);
        assert!(v.ok(), "Bootkern allein ist die zulaessige Behandlung");
        // Zwei CPUs ohne lesbare Topologie: nicht eingedaemmt, also nicht in Ordnung.
        assert!(!judge(&SmtTopology::Unknown, &[0, 1]).ok());
    }

    /// `judge` rechnet aus den ONLINE-IDs nach und liest die Buchfuehrung der Politik NICHT.
    /// Deshalb faellt es auf, wenn ein Geschwister trotz Politik online ist.
    #[test]
    fn the_check_does_not_trust_the_policys_own_map() {
        let t = SmtTopology::Multi { width: 2, shift: 1 };
        let mut occ = CoreOccupancy::new();
        // Die Politik hat sauber gearbeitet und nur 0 und 2 zugelassen ...
        assert_eq!(occ.claim(&t, 0), Claim::Admit);
        assert_eq!(occ.claim(&t, 2), Claim::Admit);
        assert_eq!(occ.occupied(), 2);
        // ... online ist aber trotzdem auch 1. Ein Pruefer, der `occ` befragt, saehe das nie.
        assert_eq!(judge(&t, &[0, 1, 2]).no_shared_core, Some(false));
    }

    /// Eine nicht pruefbare ID gilt als Verstoss, nicht als unauffaellig.
    #[test]
    fn an_unverifiable_id_counts_against_the_verdict() {
        let v = judge(&SmtTopology::Single, &[0, MAX_PHYSICAL_CORES]);
        assert_eq!(v.no_shared_core, Some(false));
    }
}
