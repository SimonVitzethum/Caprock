//! **The edge checks of `SYS_SPAWN`** (K1a, 2026-08-17) — a pure function over injected values.
//!
//! Six conditions decide whether a Cap the caller names may become a second thread's stack. They
//! live here rather than inside the dispatch for the reason `dmar.rs`, `irte.rs` and
//! `iommu_health.rs` live where they live: **they are arithmetic and classification, not
//! hardware**, and arithmetic is triggerable with literals while a kernel path needs a machine.
//!
//! ## Every refusal has its OWN name
//!
//! *Whoever introduces a capacity must NAME the overflow* (D11). A single blanket refusal would
//! make "your stack is 2 KiB" and "a device can write your stack" indistinguishable — and the
//! second is an attack while the first is a typo. Worse, it would make the DMA case **invisible**:
//! nobody greps a log for a refusal that has no word of its own.
//!
//! ## Why the DMA condition is here at all
//!
//! A stack a device can write is the `by ops` placement rule turned into an attack. The return
//! address is data; whoever can write it chooses where the thread goes next. Caprock places DMA
//! regions in IOVA windows above `RAM_TOP` and keeps `Pa` and `Iova` as separate types precisely
//! so that this cannot happen by accident — but `SYS_SPAWN` takes a Cap the *caller* names, so
//! here it must be **checked**, not assumed.

/// Why a proposed stack was refused. Ordered as the checks run; the first failure wins, and the
/// order is content: an unusable Cap is reported before its geometry, and geometry before
/// placement, so a log never complains about the size of a region the caller does not even hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackRefusal {
    /// The slot is empty, or the Cap behind it is not a memory region.
    NotMemory,
    /// The Cap does not carry both read and write.
    NotWritable,
    /// The Cap is not in the `normal` address space (device or DMA space).
    WrongSpace,
    /// The region lies inside a window a device can reach. **This one is an attack, not a
    /// mistake** — see the module doc.
    DeviceReachable,
    /// Smaller than [`MIN_STACK_BYTES`], or the region is not page-aligned, or its length is not
    /// a multiple of the page size.
    BadGeometry,
    /// The region overlaps something already mapped in the target address space.
    Overlaps,
    /// The PD already holds [`MAX_THREADS_PER_PD`] threads.
    ThreadLimit,
}

/// The smallest stack `SYS_SPAWN` accepts.
///
/// **16 KiB, and the number is not free.** `LOADED_STACK_BYTES` is 16 KiB today, and Z19/A3 notes
/// that the usual main-thread default elsewhere is 8 MiB — that gap is a real open item. But a
/// *minimum* is a different quantity from a *default*: it only has to make a signal frame plus a
/// few frames of Rust survivable. Below one page the guard-page arithmetic stops meaning anything.
pub const MIN_STACK_BYTES: u64 = 16 * 1024;

/// Page size the geometry check works in.
pub const PAGE: u64 = 4096;

/// **The named capacity** (D11). A PD that may create threads without bound is a DoS channel —
/// the same shape as `AUFTRAEGE_MAX` in C8, and the overflow gets `ERR_THREAD_LIMIT` while the
/// caller **keeps running**.
pub const MAX_THREADS_PER_PD: u32 = 64;

/// What the dispatch has already resolved, handed in so that this function touches no lock.
#[derive(Clone, Copy, Debug)]
pub struct StackProposal {
    /// Did the slot resolve to `ObjectKind::Memory`?
    pub is_memory: bool,
    /// Does the Cap carry read **and** write?
    pub writable: bool,
    /// Is it in the `normal` space (not device, not DMA)?
    pub normal_space: bool,
    /// Physical base and length of the region.
    pub base: u64,
    pub len: u64,
    /// Does the region intersect any window a device can reach?
    pub device_reachable: bool,
    /// Does it overlap an existing mapping of the target VSpace?
    pub overlaps_existing: bool,
    /// Threads already bound to the target PD.
    pub threads_now: u32,
}

/// The judgement. `Ok(top_of_stack)` gives the initial stack pointer — the region's **end**,
/// because stacks grow down on both architectures this kernel targets.
///
/// The order of the checks is deliberate and is the content of this module (see
/// [`StackRefusal`]).
pub fn check_stack(p: &StackProposal) -> Result<u64, StackRefusal> {
    if !p.is_memory {
        return Err(StackRefusal::NotMemory);
    }
    if !p.writable {
        return Err(StackRefusal::NotWritable);
    }
    if !p.normal_space {
        return Err(StackRefusal::WrongSpace);
    }
    // Vor der Geometrie: eine geraeteerreichbare Region ist auch dann abzulehnen, wenn sie
    // perfekt ausgerichtet und gross genug ist. Andersherum haette ein Angreifer eine
    // Fehlermeldung, die ihm sagt, woran es noch fehlt.
    if p.device_reachable {
        return Err(StackRefusal::DeviceReachable);
    }
    if p.len < MIN_STACK_BYTES || p.base % PAGE != 0 || p.len % PAGE != 0 {
        return Err(StackRefusal::BadGeometry);
    }
    if p.overlaps_existing {
        return Err(StackRefusal::Overlaps);
    }
    if p.threads_now >= MAX_THREADS_PER_PD {
        return Err(StackRefusal::ThreadLimit);
    }
    // Stapel wachsen nach unten: der Startzeiger ist das ENDE. `base + len` kann nicht
    // ueberlaufen, weil `len` aus einer Cap stammt, die der Allokator vergeben hat -- aber
    // „kann nicht" ist keine Pruefung, und eine ungeschuetzte Addition ist S3 woertlich.
    p.base.checked_add(p.len).ok_or(StackRefusal::BadGeometry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> StackProposal {
        StackProposal {
            is_memory: true,
            writable: true,
            normal_space: true,
            base: 0x40_0000,
            len: 64 * 1024,
            device_reachable: false,
            overlaps_existing: false,
            threads_now: 1,
        }
    }

    #[test]
    fn a_good_proposal_yields_the_top_of_the_region() {
        assert_eq!(check_stack(&good()), Ok(0x40_0000 + 64 * 1024));
    }

    /// **The check that carries the module.** A perfectly formed region that a device can write
    /// must still be refused — and with its own name, so the log says *attack shape*, not *typo*.
    #[test]
    fn a_device_reachable_region_is_refused_even_when_otherwise_perfect() {
        let mut p = good();
        p.device_reachable = true;
        assert_eq!(check_stack(&p), Err(StackRefusal::DeviceReachable));
    }

    /// The DMA refusal comes **before** geometry: otherwise the error message would tell an
    /// attacker what else to fix.
    #[test]
    fn dma_beats_geometry_in_the_ordering() {
        let mut p = good();
        p.device_reachable = true;
        p.len = 8; // auch zu klein und unausgerichtet
        assert_eq!(
            check_stack(&p),
            Err(StackRefusal::DeviceReachable),
            "die gefaehrlichere Absage muss gewinnen"
        );
    }

    #[test]
    fn each_refusal_is_reachable_and_distinct() {
        let cases: [(&str, StackProposal, StackRefusal); 6] = [
            ("kein Speicher", StackProposal { is_memory: false, ..good() }, StackRefusal::NotMemory),
            ("nicht schreibbar", StackProposal { writable: false, ..good() }, StackRefusal::NotWritable),
            ("falscher Raum", StackProposal { normal_space: false, ..good() }, StackRefusal::WrongSpace),
            ("zu klein", StackProposal { len: 4096, ..good() }, StackRefusal::BadGeometry),
            ("ueberlappt", StackProposal { overlaps_existing: true, ..good() }, StackRefusal::Overlaps),
            ("Grenze", StackProposal { threads_now: MAX_THREADS_PER_PD, ..good() }, StackRefusal::ThreadLimit),
        ];
        for (name, p, want) in cases {
            assert_eq!(check_stack(&p), Err(want), "{name}");
        }
    }

    /// Unausgerichtet ist ein eigener Fall — eine Region, die gross genug ist, aber schief liegt,
    /// macht die Wachseiten-Arithmetik bedeutungslos.
    #[test]
    fn misalignment_is_caught_even_at_a_generous_size() {
        let mut p = good();
        p.base = 0x40_0800; // 2 KiB versetzt
        assert_eq!(check_stack(&p), Err(StackRefusal::BadGeometry));
        let mut q = good();
        q.len = 64 * 1024 + 8; // Laenge kein Seitenvielfaches
        assert_eq!(check_stack(&q), Err(StackRefusal::BadGeometry));
    }

    /// Die Grenze greift **bei** `MAX`, nicht erst darueber — ein Off-by-one hier hiesse ein
    /// Thread mehr als zugesagt, und die Zusage ist die Schranke.
    #[test]
    fn the_limit_bites_at_max_not_above_it() {
        let mut p = good();
        p.threads_now = MAX_THREADS_PER_PD - 1;
        assert!(check_stack(&p).is_ok(), "einer unter der Grenze muss noch gehen");
        p.threads_now = MAX_THREADS_PER_PD;
        assert_eq!(check_stack(&p), Err(StackRefusal::ThreadLimit));
    }

    /// S3 wörtlich: keine ungeschützte Addition. Eine Region am Adressraumende darf keinen
    /// Stackzeiger erzeugen, der umläuft.
    #[test]
    fn an_end_of_space_region_does_not_wrap() {
        let mut p = good();
        p.base = u64::MAX - 0xFFF;
        p.len = 64 * 1024;
        assert_eq!(check_stack(&p), Err(StackRefusal::BadGeometry));
    }

    /// **Sprechprobe in beide Richtungen.** Ein Prädikat, das nicht bestehen kann, ist so wenig
    /// eine Prüfung wie eines, das nicht durchfallen kann.
    #[test]
    fn the_check_can_both_pass_and_fail() {
        assert!(check_stack(&good()).is_ok());
        assert!(check_stack(&StackProposal { is_memory: false, ..good() }).is_err());
    }
}
