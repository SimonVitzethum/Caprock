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
    /// **The named sub-region does not lie inside the Cap** (K1b, 2026-08-26).
    ///
    /// Its own name, and not [`StackRefusal::BadGeometry`]: *this region is unusable as a stack*
    /// and *you named a region you do not hold* have different fixes, and the second is the shape
    /// an attacker produces. See [`sub_region`].
    OutsideCap,
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
    /// **The window inside the Cap** — obtainable only from [`sub_region`], so the narrowing
    /// cannot be skipped by a caller who narrows in one place and judges in another.
    pub region: SubRegion,
    /// Does the region intersect any window a device can reach?
    pub device_reachable: bool,
    /// Does it overlap an existing mapping of the target VSpace?
    pub overlaps_existing: bool,
    /// Threads already bound to the target PD.
    pub threads_now: u32,
}

/// **A window inside a stack Cap, and the ONLY thing [`check_stack`] will judge** (K1b).
///
/// The field is private on purpose. `check_stack` needs the *narrowed* region — the kernel asks
/// "can a device reach this?" and "does this overlap a sibling?" about the window, not about the
/// whole Cap. Nothing stops a caller from narrowing in one place and judging in another, and then
/// *a check that half-happens in two places is two checks, and the second ages*. Since
/// [`sub_region`] is the only public way to obtain one, the narrowing cannot be skipped.
///
/// rustc checks **constructibility, not non-propagation**: inside this module the field is
/// writable, so there must be no second public constructor and no public field. There is neither,
/// and that sentence is the guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubRegion {
    base: u64,
    len: u64,
}

impl SubRegion {
    /// Physical base of the window.
    pub fn base(&self) -> u64 {
        self.base
    }
    /// Length of the window in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }
    /// Does this window overlap another one? Used by the kernel to keep sibling stacks in one
    /// arena apart — the case the whole sub-region idea creates.
    pub fn overlaps(&self, base: u64, len: u64) -> bool {
        if self.len == 0 || len == 0 {
            return false;
        }
        self.base < base.saturating_add(len) && base < self.base.saturating_add(self.len)
    }
}

/// **Narrow a stack Cap to the window the caller named** (K1b, 2026-08-26).
///
/// `(0, 0)` means *the whole region*, which is what every `SYS_SPAWN` written before the
/// sub-region existed encodes — the compatibility is in the encoding, not in a branch.
///
/// The two halves are **never** interpreted separately. A length of zero pages at a non-zero
/// offset is [`StackRefusal::OutsideCap`] and not "the whole Cap from the base": a request whose
/// two fields disagree is a request nobody wrote on purpose, and guessing which half was meant is
/// how a bounds check becomes a suggestion.
///
/// Every addition is checked. `offset_pages` and `length_pages` come out of a **user register**;
/// `off * PAGE` with `off = 2^33` wraps to something small and lands squarely inside the Cap.
pub fn sub_region(
    cap_base: u64,
    cap_len: u64,
    offset_pages: u64,
    length_pages: u64,
) -> Result<SubRegion, StackRefusal> {
    if offset_pages == 0 && length_pages == 0 {
        return Ok(SubRegion { base: cap_base, len: cap_len });
    }
    if length_pages == 0 {
        // Offset ohne Laenge: s. Funktionsdoku -- nicht raten.
        return Err(StackRefusal::OutsideCap);
    }
    let off = offset_pages.checked_mul(PAGE).ok_or(StackRefusal::OutsideCap)?;
    let len = length_pages.checked_mul(PAGE).ok_or(StackRefusal::OutsideCap)?;
    let base = cap_base.checked_add(off).ok_or(StackRefusal::OutsideCap)?;
    let end = base.checked_add(len).ok_or(StackRefusal::OutsideCap)?;
    // Die Cap-Obergrenze selbst kann nicht ueberlaufen (sie kommt aus dem Allokator) -- aber
    // „kann nicht" ist keine Pruefung, und eine ungeschuetzte Addition ist S3 woertlich.
    let cap_end = cap_base.checked_add(cap_len).ok_or(StackRefusal::OutsideCap)?;
    if end > cap_end {
        return Err(StackRefusal::OutsideCap);
    }
    Ok(SubRegion { base, len })
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
    let (base, len) = (p.region.base(), p.region.len());
    if len < MIN_STACK_BYTES || base % PAGE != 0 || len % PAGE != 0 {
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
    base.checked_add(len).ok_or(StackRefusal::BadGeometry)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eine Region von Hand -- im Testmodul erlaubt, weil er ein Kindmodul ist. Ausserhalb gibt
    /// es diesen Weg NICHT; das ist der Sinn des privaten Feldes.
    fn roh(base: u64, len: u64) -> SubRegion {
        SubRegion { base, len }
    }

    fn good() -> StackProposal {
        StackProposal {
            is_memory: true,
            writable: true,
            normal_space: true,
            region: roh(0x40_0000, 64 * 1024),
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
        p.region = roh(p.region.base(), 8); // auch zu klein und unausgerichtet
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
            ("zu klein", StackProposal { region: roh(0x40_0000, 4096), ..good() }, StackRefusal::BadGeometry),
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
        p.region = roh(0x40_0800, 64 * 1024); // 2 KiB versetzt
        assert_eq!(check_stack(&p), Err(StackRefusal::BadGeometry));
        let mut q = good();
        q.region = roh(0x40_0000, 64 * 1024 + 8); // Laenge kein Seitenvielfaches
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
        p.region = roh(u64::MAX - 0xFFF, 64 * 1024);
        assert_eq!(check_stack(&p), Err(StackRefusal::BadGeometry));
    }

    /// **Sprechprobe in beide Richtungen.** Ein Prädikat, das nicht bestehen kann, ist so wenig
    /// eine Prüfung wie eines, das nicht durchfallen kann.
    #[test]
    fn the_check_can_both_pass_and_fail() {
        assert!(check_stack(&good()).is_ok());
        assert!(check_stack(&StackProposal { is_memory: false, ..good() }).is_err());
    }

    // --- K1b: die Teilregion --------------------------------------------------------------

    /// `(0, 0)` ist die ganze Region — **bitgleich zu jedem Aufruf, den es vor der Teilregion
    /// gab**. Ohne diese Zeile waere die Vertraeglichkeit eine Behauptung.
    #[test]
    fn zero_means_the_whole_cap() {
        let r = sub_region(0x40_0000, 64 * 1024, 0, 0).unwrap();
        assert_eq!((r.base(), r.len()), (0x40_0000, 64 * 1024));
    }

    /// Vier Fenster zu je 16 KiB in einer 64-KiB-Cap: die Adressen, um die es geht.
    #[test]
    fn four_windows_tile_the_arena() {
        let (b, l) = (0x40_0000u64, 64 * 1024u64);
        let w: [SubRegion; 4] = core::array::from_fn(|i| {
            sub_region(b, l, (i as u64) * 4, 4).unwrap()
        });
        for (i, r) in w.iter().enumerate() {
            assert_eq!(r.base(), b + (i as u64) * 16 * 1024);
            assert_eq!(r.len(), 16 * 1024);
        }
        // paarweise disjunkt -- das ist die Eigenschaft, an der die ganze Aenderung haengt
        for i in 0..4 {
            for j in 0..4 {
                assert_eq!(w[i].overlaps(w[j].base(), w[j].len()), i == j, "{i} gegen {j}");
            }
        }
    }

    /// Ein Fenster, das ueber das Ende hinausragt, bekommt seinen EIGENEN Namen — nicht
    /// `BadGeometry`. Auch dann, wenn es nur um eine Seite hinausragt.
    #[test]
    fn a_window_past_the_end_is_named_as_such() {
        assert_eq!(sub_region(0x40_0000, 64 * 1024, 12, 5), Err(StackRefusal::OutsideCap));
        assert_eq!(sub_region(0x40_0000, 64 * 1024, 16, 4), Err(StackRefusal::OutsideCap));
        // genau buendig muss noch gehen -- eine Schranke, die einen gueltigen Fall abweist,
        // ist so falsch wie eine, die einen ungueltigen durchlaesst
        assert!(sub_region(0x40_0000, 64 * 1024, 12, 4).is_ok());
    }

    /// **Der Fall, um dessentwillen jede Addition geprueft ist.** `offset_pages` kommt aus einem
    /// USER-Register: `2^52` Seiten mal 4096 laeuft um und landete ohne `checked_mul` mitten in
    /// der Cap — eine Schrankenpruefung, die der Angreifer selbst erfuellt.
    #[test]
    fn an_offset_that_wraps_is_refused_not_wrapped() {
        assert_eq!(sub_region(0x40_0000, 64 * 1024, 1 << 52, 4), Err(StackRefusal::OutsideCap));
        assert_eq!(sub_region(0x40_0000, 64 * 1024, 4, 1 << 52), Err(StackRefusal::OutsideCap));
        assert_eq!(sub_region(u64::MAX - 0xFFF, 0x1000, 1, 4), Err(StackRefusal::OutsideCap));
    }

    /// Ein Offset ohne Laenge wird **abgewiesen**, nicht als „ganze Region" gelesen. Raten, welche
    /// Haelfte gemeint war, macht aus einer Schranke einen Vorschlag.
    #[test]
    fn an_offset_without_a_length_is_not_guessed() {
        assert_eq!(sub_region(0x40_0000, 64 * 1024, 4, 0), Err(StackRefusal::OutsideCap));
    }

    /// Die Teilregion geht durch dieselbe Geometriepruefung wie die ganze: ein 4-KiB-Fenster ist
    /// zu klein, auch wenn die Cap gross ist.
    #[test]
    fn a_window_still_has_to_be_a_usable_stack() {
        let p = StackProposal {
            region: sub_region(0x40_0000, 64 * 1024, 0, 1).unwrap(),
            ..good()
        };
        assert_eq!(check_stack(&p), Err(StackRefusal::BadGeometry));
    }

    /// Sprechprobe fuer `overlaps`: eine leere Region ueberlappt **nichts** — sonst waere jede
    /// Abfrage gegen einen leeren Tabelleneintrag ein Treffer, und die Ueberlappungspruefung
    /// wiese alles ab.
    #[test]
    fn an_empty_window_overlaps_nothing() {
        let r = sub_region(0x40_0000, 64 * 1024, 0, 4).unwrap();
        assert!(!r.overlaps(0x40_0000, 0));
        assert!(!SubRegion { base: 0x40_0000, len: 0 }.overlaps(0x40_0000, 16 * 1024));
        assert!(r.overlaps(0x40_0000 + 4096, 4096), "echte Ueberlappung muss sprechen");
    }
}
