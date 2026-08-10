// Caprock — Verus-Pilot (Tier 2), Teil 2: CDT-**Struktur**-Invariante.
//
// Zweite formale Spezifikation aus `cap_audit_cdt` (crates/caprock-cap/src/space.rs): die
// **Sibling-Konsistenz** (Code 5) der Capability-Derivation-Tree-Geschwisterliste — eine
// doppelt-verkettete Liste, deren `next_sibling`/`prev_sibling` **gegenseitige Inverse** sind
// (und nur auf gueltige, belegte Knoten zeigen). Bewiesen: die Listenoperationen `insert_before`
// (neuen Knoten am Listenkopf einfuegen) und `unlink` (Knoten entfernen) **erhalten** die Invariante.
//
// BEWUSST NICHT modelliert (dokumentierte Folgeschritte, schwieriger):
//   * die Eltern/Kind-Verkettung (Codes 4-lokal + 6) — analog, mehr Felder;
//   * die Kinderlisten-Erreichbarkeit aus Code 4 (Listen-Reachability);
//   * die Azyklizitaet der Eltern-Kette (Code 7) — braucht ein Wohlfundiertheits-Mass.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein Geschwisterlisten-Knoten: belegt-Flag + Vorwaerts-/Rueckwaerts-Verkettung (Modell von
/// `Mdb.next_sibling`/`prev_sibling`).
pub struct Node {
    pub used: bool,
    pub next: Option<nat>,
    pub prev: Option<nat>,
}

/// Die Knotentabelle (der relevante Ausschnitt des Capability-Space fuer die Geschwisterlisten).
pub struct DList {
    pub nodes: Seq<Node>,
}

/// Ist `i` ein gueltiger, belegter Knoten?
pub open spec fn live(c: DList, i: nat) -> bool {
    i < c.nodes.len() && c.nodes[i as int].used
}

/// **Sibling-Konsistenz (audit_cdt Code 5):** `next`/`prev` sind gegenseitige Inverse und zeigen
/// nur auf gueltige, belegte Knoten. (`s.next == Some(n)` ⟺ `n.prev == Some(s)`.)
pub open spec fn dll_inv(c: DList) -> bool {
    &&& (forall|s: nat|
        #![trigger c.nodes[s as int].next]
        live(c, s) && c.nodes[s as int].next is Some
            ==> live(c, c.nodes[s as int].next->Some_0)
                && c.nodes[c.nodes[s as int].next->Some_0 as int].prev == Some(s))
    &&& (forall|s: nat|
        #![trigger c.nodes[s as int].prev]
        live(c, s) && c.nodes[s as int].prev is Some
            ==> live(c, c.nodes[s as int].prev->Some_0)
                && c.nodes[c.nodes[s as int].prev->Some_0 as int].next == Some(s))
}

/// **BEWEIS:** einen frischen Knoten **vor** einen Listenkopf `h` einfuegen (`h.prev == None`)
/// **erhaelt** die Sibling-Konsistenz. Kernidee: weil `h` ein Kopf ist, zeigt **kein** Knoten per
/// `next` auf `h` (sonst waere `h.prev != None`) — das Setzen von `h.prev` bricht also nichts.
pub proof fn insert_before(c: DList, h: nat) -> (c2: DList)
    requires
        dll_inv(c),
        live(c, h),
        c.nodes[h as int].prev is None,
    ensures
        dll_inv(c2),
{
    let cnew: nat = c.nodes.len() as nat;
    let new_node = Node { used: true, next: Some(h), prev: None };
    // h bekommt den neuen Knoten als Vorgaenger.
    let old_h = c.nodes[h as int];
    let nodes1 = c.nodes.update(h as int, Node { prev: Some(cnew), ..old_h });
    let nodes2 = nodes1.push(new_node);
    let c2 = DList { nodes: nodes2 };

    // Kernfakt: in `c` zeigt kein belegter Knoten per `next` auf `h` (denn h.prev == None).
    assert forall|s: nat| live(c, s) implies c.nodes[s as int].next != Some(h) by {
        if c.nodes[s as int].next == Some(h) {
            // dll_inv: dann waere c.nodes[h].prev == Some(s) != None — Widerspruch.
        }
    }

    assert(dll_inv(c2)) by {
        assert forall|s: nat, n: nat|
            live(c2, s) && c2.nodes[s as int].next == Some(n)
            implies live(c2, n) && c2.nodes[n as int].prev == Some(s) by {
            // alte Knoten (ausser h) unveraendert; h.next unveraendert; neuer Knoten cnew.next==Some(h).
            assert(c2.nodes[s as int].used == (if s == cnew { true } else { c.nodes[s as int].used }));
        }
        assert forall|s: nat, p: nat|
            live(c2, s) && c2.nodes[s as int].prev == Some(p)
            implies live(c2, p) && c2.nodes[p as int].next == Some(s) by {
        }
    }
    c2
}

/// **BEWEIS:** einen Knoten `i` aus seiner Geschwisterliste **entfernen** (Nachbarn umhaengen,
/// `i` als unbelegt markieren) **erhaelt** die Sibling-Konsistenz.
pub proof fn unlink(c: DList, i: nat) -> (c2: DList)
    requires
        dll_inv(c),
        live(c, i),
    ensures
        dll_inv(c2),
{
    let nd = c.nodes[i as int];
    // Schritt 1: i als unbelegt markieren + seine Links kappen.
    let n0 = c.nodes.update(i as int, Node { used: false, next: None, prev: None });
    // Schritt 2: Vorgaenger p.next := i.next (falls vorhanden).
    let n1 = if nd.prev is Some {
        let p = nd.prev->0;
        n0.update(p as int, Node { next: nd.next, ..n0[p as int] })
    } else {
        n0
    };
    // Schritt 3: Nachfolger q.prev := i.prev (falls vorhanden).
    let n2 = if nd.next is Some {
        let q = nd.next->0;
        n1.update(q as int, Node { prev: nd.prev, ..n1[q as int] })
    } else {
        n1
    };
    let c2 = DList { nodes: n2 };
    assert(dll_inv(c2));
    c2
}

fn main() {}

} // verus!
