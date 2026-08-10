// Caprock — Verus-Pilot (Tier 2), Teil 5: W^X-Invariante (`vspace_audit` / `vspace_wx_ok`, ext-21).
//
// Formale Spezifikation der **W^X**-Invariante (Write XOR eXecute): KEINE gemappte EL0-Seite ist
// gleichzeitig schreibbar UND ausfuehrbar. Das ist die zentrale Code-Integritaets-Eigenschaft (ein
// Angreifer kann keinen eigenen Code in eine beschreibbare Seite schreiben und dann ausfuehren).
// Bewiesen: das Mapping-Gate `map_page` (mappt nur mit W^X-konformen Rechten) **erhaelt** die
// Invariante -- statisch + fuer ALLE Zustaende.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein Seiteneintrag (Modell eines L3-PTE): gemappt? + Schreib-/Ausfuehrrechte.
pub struct Page {
    pub mapped: bool,
    pub writable: bool,
    pub executable: bool,
}

/// Eine (EL0-)VSpace: die Seitentabelle.
pub struct VSpace {
    pub pages: Seq<Page>,
}

/// **W^X-Invariante (`vspace_audit`):** keine gemappte Seite ist zugleich schreib- + ausfuehrbar.
pub open spec fn wx_inv(v: VSpace) -> bool {
    forall|i: int|
        #![trigger v.pages[i]]
        0 <= i < v.pages.len() && v.pages[i].mapped
            ==> !(v.pages[i].writable && v.pages[i].executable)
}

/// **BEWEIS:** `map_page` (Seite `i` mit Rechten `w`/`x` mappen — **nur**, wenn W^X eingehalten ist;
/// sonst Ablehnung ohne Aenderung) **erhaelt** die W^X-Invariante.
pub proof fn map_page(v: VSpace, i: int, w: bool, x: bool) -> (v2: VSpace)
    requires
        wx_inv(v),
        0 <= i < v.pages.len(),
    ensures
        wx_inv(v2),
{
    if !(w && x) {
        let v2 = VSpace {
            pages: v.pages.update(i, Page { mapped: true, writable: w, executable: x }),
        };
        assert forall|j: int| 0 <= j < v2.pages.len() && #[trigger] v2.pages[j].mapped
            implies !(v2.pages[j].writable && v2.pages[j].executable) by {
            if j != i {
                assert(v2.pages[j] == v.pages[j]);
            }
        }
        v2
    } else {
        // W^X verletzt -> abgelehnt (kein RWX-Mapping moeglich).
        v
    }
}

/// **BEWEIS:** eine Seite **schreibbar machen** (`make_writable`) ist nur dann W^X-erhaltend, wenn die
/// Seite **nicht ausfuehrbar** ist — das Gate erzwingt genau das. (Modelliert die Rechte-Verschaerfung
/// `restrict`/Remap; ein nachtraegliches W auf eine X-Seite wuerde W^X brechen und wird abgelehnt.)
pub proof fn make_writable(v: VSpace, i: int) -> (v2: VSpace)
    requires
        wx_inv(v),
        0 <= i < v.pages.len(),
        !v.pages[i].executable,
    ensures
        wx_inv(v2),
{
    let pg = v.pages[i];
    let v2 = VSpace { pages: v.pages.update(i, Page { writable: true, ..pg }) };
    assert forall|j: int| 0 <= j < v2.pages.len() && #[trigger] v2.pages[j].mapped
        implies !(v2.pages[j].writable && v2.pages[j].executable) by {
        if j != i {
            assert(v2.pages[j] == v.pages[j]);
        }
    }
    v2
}

fn main() {}

} // verus!
