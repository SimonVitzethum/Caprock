// SEL4Lake — Verus-Pilot (Tier 2), Teil 6: DMA-Regionen-Disjunktheit (`dma_audit`, ext-23/24).
//
// Formale Spezifikation der DMA-Bounds-Invariante (kernel/src/system.rs `dma_audit`, Invariante 1):
// kernel-ausgeschnittene DMA-Regionen sind **paarweise disjunkt** UND disjunkt von der reservierten
// Kernel-Region. Das ist die Grundlage der DMA-Use-after-free-/Isolations-Sicherheit (ein Geraet
// DMAt nur in seine eigene Region, ohne Kernel-/Fremdspeicher zu treffen). Bewiesen: `alloc_region`
// (eine neue Region nur einhaengen, wenn sie disjunkt zu allen bestehenden + zum Kernel ist)
// **erhaelt** die Disjunktheits-Invariante -- statisch + fuer ALLE Zustaende.
//
// Ein GEOMETRISCHER Invariantentyp (Intervall-Disjunktheit) -- wieder anders als CDT/Policy/W^X.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Eine kontiguierliche Region `[base, base+len)` (nat -> kein Adress-Overflow im Modell).
pub struct Region {
    pub base: nat,
    pub len: nat,
}

/// Sind zwei Regionen **disjunkt** (kein gemeinsames Byte)?  `a` endet vor `b` oder umgekehrt.
pub open spec fn disjoint(a: Region, b: Region) -> bool {
    a.base + a.len <= b.base || b.base + b.len <= a.base
}

/// Der DMA-Kontext: die reservierte Kernel-Region + die ausgeschnittenen DMA-Regionen.
pub struct DmaState {
    pub kernel: Region,
    pub regions: Seq<Region>,
}

/// **DMA-Disjunktheits-Invariante (`dma_audit` Invariante 1):** alle DMA-Regionen sind paarweise
/// disjunkt UND jede ist disjunkt von der Kernel-Region.
pub open spec fn dma_inv(s: DmaState) -> bool {
    &&& (forall|i: int|
        #![trigger s.regions[i]]
        0 <= i < s.regions.len() ==> disjoint(s.regions[i], s.kernel))
    &&& (forall|i: int, j: int|
        #![trigger s.regions[i], s.regions[j]]
        0 <= i < s.regions.len() && 0 <= j < s.regions.len() && i != j
            ==> disjoint(s.regions[i], s.regions[j]))
}

/// **BEWEIS:** `alloc_region` (eine neue DMA-Region `r` einhaengen — **nur**, wenn sie disjunkt von
/// der Kernel-Region UND von allen bestehenden Regionen ist) **erhaelt** die Disjunktheits-Invariante.
pub proof fn alloc_region(s: DmaState, r: Region) -> (s2: DmaState)
    requires
        dma_inv(s),
        disjoint(r, s.kernel),
        forall|i: int| 0 <= i < s.regions.len() ==> disjoint(#[trigger] s.regions[i], r),
    ensures
        dma_inv(s2),
{
    let s2 = DmaState { kernel: s.kernel, regions: s.regions.push(r) };

    // (1) jede Region disjunkt vom Kernel: alte Regionen unveraendert; die neue per Vorbedingung.
    assert forall|i: int| 0 <= i < s2.regions.len() implies disjoint(#[trigger] s2.regions[i],
        s2.kernel) by {
        if i < s.regions.len() {
            assert(s2.regions[i] == s.regions[i]);
        }
    }
    // (2) paarweise Disjunktheit: alte Paare unveraendert; ein Paar mit der neuen Region ist per
    //     Vorbedingung disjunkt (disjoint ist symmetrisch).
    assert forall|i: int, j: int|
        0 <= i < s2.regions.len() && 0 <= j < s2.regions.len() && i != j
        implies disjoint(#[trigger] s2.regions[i], #[trigger] s2.regions[j]) by {
        let last = s.regions.len();
        if i < last && j < last {
            assert(s2.regions[i] == s.regions[i] && s2.regions[j] == s.regions[j]);
        } else if i == last {
            assert(s2.regions[i] == r && s2.regions[j] == s.regions[j]);
        } else {
            assert(s2.regions[j] == r && s2.regions[i] == s.regions[i]);
        }
    }
    s2
}

fn main() {}

} // verus!
