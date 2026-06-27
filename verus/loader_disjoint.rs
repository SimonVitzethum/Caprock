// SEL4Lake — Verus-Pilot (Tier 2), Teil 8: Loader-Use-after-free-Schutz (`loader_audit`, ext-26 L5).
//
// Formale Spezifikation der `loader_audit`-Invariante (kernel/src/loader.rs / system.rs): KEIN
// registriertes geladenes Segment ueberlappt eine **freie** RAM-Region. Das verhindert
// Use-after-free: solange ein geladenes Programm ein Segment haelt, darf dessen RAM nicht als „frei"
// gelten (und damit anderweitig vergeben werden). Bewiesen: `free_ram` (eine Region nur dann zur
// Free-Liste zurueckgeben, wenn sie disjunkt von ALLEN geladenen Segmenten ist) **erhaelt** die
// Invariante -- statisch + fuer ALLE Zustaende.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

pub struct Region {
    pub base: nat,
    pub len: nat,
}

pub open spec fn disjoint(a: Region, b: Region) -> bool {
    a.base + a.len <= b.base || b.base + b.len <= a.base
}

/// Loader-Zustand: freie RAM-Regionen + die von geladenen Programmen gehaltenen Segment-Regionen.
pub struct State {
    pub free: Seq<Region>,
    pub segments: Seq<Region>,
}

/// **Loader-Invariante (`loader_audit`):** kein geladenes Segment ueberlappt eine freie Region.
pub open spec fn loader_inv(s: State) -> bool {
    forall|i: int, j: int|
        #![trigger s.segments[i], s.free[j]]
        0 <= i < s.segments.len() && 0 <= j < s.free.len()
            ==> disjoint(s.segments[i], s.free[j])
}

/// **BEWEIS:** `free_ram` (Region `r` zur Free-Liste zurueckgeben — **nur**, wenn `r` disjunkt von
/// allen geladenen Segmenten ist) **erhaelt** die Loader-Invariante (kein Use-after-free).
pub proof fn free_ram(s: State, r: Region) -> (s2: State)
    requires
        loader_inv(s),
        forall|i: int| 0 <= i < s.segments.len() ==> disjoint(#[trigger] s.segments[i], r),
    ensures
        loader_inv(s2),
{
    let s2 = State { free: s.free.push(r), segments: s.segments };
    assert forall|i: int, j: int|
        0 <= i < s2.segments.len() && 0 <= j < s2.free.len()
        implies disjoint(#[trigger] s2.segments[i], #[trigger] s2.free[j]) by {
        if j < s.free.len() {
            assert(s2.free[j] == s.free[j]);
        } else {
            assert(s2.free[j] == r);
        }
    }
    s2
}

fn main() {}

} // verus!
