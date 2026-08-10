// Caprock — Verus-Pilot (Tier 2), Teil 3: vollstaendige CDT-**Struktur**-Invariante (lokal).
//
// Vereint die Sibling-Konsistenz (Code 5) mit der Eltern/Kind-Verkettung (audit_cdt Codes 4-lokal
// + 6) in EINEM Knotenmodell und beweist, dass `derive` (eine Capability ableiten: ein neues Kind am
// Kopf der Kinderliste des Elternknotens) die gesamte lokale Strukturinvariante ERHAELT.
//
// Modellierte Invariante (audit_cdt Codes 4-lokal, 5, 6):
//   (4l) parent==Some(p)      -> p belegt+gueltig, object[p]==object[s]   (Ableitung teilt das Objekt)
//   (5)  next/prev gegenseitige Inverse + gueltig+belegt
//   (6)  first_child==Some(c)  -> c belegt+gueltig, parent[c]==Some(s), prev[c]==None
//        (der Kopf einer Kinderliste hat keinen Vorgaenger — die lokale Kopplung, die `derive`
//         beweisbar macht; im realen Audit folgt sie aus Code 4 (Listen-Mitgliedschaft) + Code 5.)
//
// BEWUSST NICHT (dokumentierte, schwierigere Folgeschritte): Kinderlisten-Erreichbarkeit aus Code 4;
// Azyklizitaet der Eltern-Kette (Code 7, braucht ein Wohlfundiertheits-Mass).
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein CDT-Knoten (Modell von `Slot` + `Mdb`): belegt, referenziertes Objekt, Eltern-/Kind-/
/// Geschwister-Verkettung.
pub struct Node {
    pub used: bool,
    pub object: nat,
    pub parent: Option<nat>,
    pub first_child: Option<nat>,
    pub next: Option<nat>,
    pub prev: Option<nat>,
}

pub struct Cdt {
    pub nodes: Seq<Node>,
}

pub open spec fn live(c: Cdt, i: nat) -> bool {
    i < c.nodes.len() && c.nodes[i as int].used
}

/// Die lokale CDT-Strukturinvariante (audit_cdt Codes 4-lokal, 5, 6).
pub open spec fn cdt_inv(c: Cdt) -> bool {
    forall|s: nat|
        #![trigger c.nodes[s as int]]
        live(c, s) ==> {
            let nd = c.nodes[s as int];
            // (5) Sibling: next/prev gegenseitige Inverse
            &&& (nd.next is Some ==> live(c, nd.next->Some_0)
                && c.nodes[nd.next->Some_0 as int].prev == Some(s))
            &&& (nd.prev is Some ==> live(c, nd.prev->Some_0)
                && c.nodes[nd.prev->Some_0 as int].next == Some(s))
            // (6) first_child: gueltig, zeigt zurueck, und ist Listenkopf (prev==None)
            &&& (nd.first_child is Some ==> live(c, nd.first_child->Some_0)
                && c.nodes[nd.first_child->Some_0 as int].parent == Some(s)
                && c.nodes[nd.first_child->Some_0 as int].prev is None)
            // (4l) parent: gueltig, Ableitung teilt das Objekt
            &&& (nd.parent is Some ==> live(c, nd.parent->Some_0)
                && c.nodes[nd.parent->Some_0 as int].object == nd.object)
        }
}

/// **BEWEIS:** `derive` (eine Capability auf Objekt `object[p]` von `p` ableiten: neues Kind `c` am
/// Kopf der Kinderliste von `p`) **erhaelt** die CDT-Strukturinvariante.
///
/// Kernidee: der bisherige Listenkopf `h = p.first_child` hat laut Invariante (6) `prev==None`, wird
/// also von keinem `next` referenziert (5-Kontraposition) — das Einhaengen von `c` davor bricht
/// nichts. `c` erbt `object[p]` (4l), zeigt mit `parent` auf `p` und wird neuer `first_child` (6).
pub proof fn derive(c0: Cdt, p: nat) -> (c2: Cdt)
    requires
        cdt_inv(c0),
        live(c0, p),
    ensures
        cdt_inv(c2),
{
    let cnew: nat = c0.nodes.len() as nat;
    let pnode = c0.nodes[p as int];
    let old_head = pnode.first_child;

    let new_node = Node {
        used: true,
        object: pnode.object,
        parent: Some(p),
        first_child: None,
        next: old_head,
        prev: None,
    };

    // p bekommt cnew als neuen first_child.
    let n1 = c0.nodes.update(p as int, Node { first_child: Some(cnew), ..pnode });
    // Der bisherige Kopf (falls vorhanden) bekommt cnew als prev.
    let n2 = if old_head is Some {
        let h = old_head->Some_0;
        n1.update(h as int, Node { prev: Some(cnew), ..n1[h as int] })
    } else {
        n1
    };
    let n3 = n2.push(new_node);
    let c2 = Cdt { nodes: n3 };

    // Kernfakt: ein alter Listenkopf h (prev==None) wird von keinem belegten Knoten per next referenziert.
    if old_head is Some {
        let h = old_head->Some_0;
        // aus cdt_inv(c0) bei p: (6) -> live(h), c0.nodes[h].prev is None.
        assert(live(c0, h) && c0.nodes[h as int].prev is None);
        assert forall|s: nat| live(c0, s) implies c0.nodes[s as int].next != Some(h) by {
            if c0.nodes[s as int].next == Some(h) {
                // (5) -> c0.nodes[h].prev == Some(s) != None -> Widerspruch.
            }
        }
    }

    assert(cdt_inv(c2));
    c2
}

fn main() {}

} // verus!
