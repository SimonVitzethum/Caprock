// Caprock — Verus-Pilot (Tier 2), Teil 7: CDT-**Azyklizitaet** (`audit_cdt` Code 7).
//
// Die schwierigste CDT-Invariante: die Eltern-Kette enthaelt **keine Zyklen**. Bewiesen ueber die
// Standardtechnik eines **Wohlfundiertheits-Masses** -- ein `rank` je Knoten, der entlang `parent`
// **strikt faellt**. Da `rank: nat` nach unten beschraenkt ist, terminiert jede Eltern-Kette ->
// kein Zyklus. Bewiesen: `derive` (Kind mit `rank = rank[parent]+1`) erhaelt die Rang-Monotonie,
// und die Monotonie schliesst Selbst-Eltern + 2-Zyklen konkret aus (das Zertifikat fuer Code 7).
//
// (Im realen `audit_cdt` wird Code 7 per beschraenkter Traversierung geprueft; der Rang ist das
// rigorose Korrektheits-Zertifikat dieser Pruefung.)
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein Knoten mit Eltern-Verkettung + Wohlfundiertheits-Rang (Ghost-Mass fuer die Azyklizitaet).
pub struct Node {
    pub used: bool,
    pub parent: Option<nat>,
    pub rank: nat,
}

pub struct Cdt {
    pub nodes: Seq<Node>,
}

pub open spec fn live(c: Cdt, i: nat) -> bool {
    i < c.nodes.len() && c.nodes[i as int].used
}

/// **Rang-Monotonie (Azyklizitaets-Zertifikat, audit_cdt Code 7):** der Elternknoten hat strikt
/// kleineren Rang. Entlang `parent` faellt der Rang also strikt -> keine endliche oder unendliche
/// Eltern-Kette kann zu sich zuruegkkehren.
pub open spec fn rank_inv(c: Cdt) -> bool {
    forall|s: nat|
        #![trigger c.nodes[s as int]]
        live(c, s) && c.nodes[s as int].parent is Some
            ==> live(c, c.nodes[s as int].parent->Some_0)
                && c.nodes[c.nodes[s as int].parent->Some_0 as int].rank < c.nodes[s as int].rank
}

/// Der `k`-te Vorfahre von `s` entlang `parent` (`Some(s)` fuer k=0; `None`, falls die Kette vorher
/// endet). Macht „Eltern-Kette" explizit, um die **allgemeine** Azyklizitaet zu formulieren.
pub open spec fn ancestor(c: Cdt, s: nat, k: nat) -> Option<nat>
    decreases k,
{
    if k == 0 {
        Some(s)
    } else {
        match ancestor(c, s, (k - 1) as nat) {
            Some(a) => if live(c, a) { c.nodes[a as int].parent } else { None },
            None => None,
        }
    }
}

/// **Lemma:** jeder **echte** Vorfahre (`k >= 1`) hat strikt kleineren Rang als `s`. Per Induktion
/// ueber die Kettenlaenge `k`; nutzt die Rang-Monotonie in jedem Schritt.
pub proof fn ancestor_rank_decreases(c: Cdt, s: nat, k: nat)
    requires
        rank_inv(c),
        live(c, s),
        k >= 1,
        ancestor(c, s, k) is Some,
    ensures
        live(c, ancestor(c, s, k)->Some_0),
        c.nodes[ancestor(c, s, k)->Some_0 as int].rank < c.nodes[s as int].rank,
    decreases k,
{
    if k == 1 {
        // ancestor(s,1) == parent[s]; rank_inv bei s liefert die Aussage direkt.
    } else {
        ancestor_rank_decreases(c, s, (k - 1) as nat);
        // a := ancestor(s,k-1): live + rank[a] < rank[s]. ancestor(s,k) == parent[a];
        // rank_inv bei a: live(parent[a]) + rank[parent[a]] < rank[a] < rank[s].
    }
}

/// **KOROLLAR (AZYKLIZITAET, allgemein):** kein Knoten ist sein eigener `k`-ter Vorfahre (fuer
/// **beliebiges** `k >= 1`) — d. h. die Eltern-Kette enthaelt **keinen Zyklus irgendeiner Laenge**.
pub proof fn not_own_ancestor(c: Cdt, s: nat, k: nat)
    requires
        rank_inv(c),
        live(c, s),
        k >= 1,
    ensures
        ancestor(c, s, k) != Some(s),
{
    if ancestor(c, s, k) == Some(s) {
        ancestor_rank_decreases(c, s, k); // ergaebe rank[s] < rank[s] -> Widerspruch.
    }
}

/// **KOROLLAR (kein 1-Zyklus):** kein Knoten ist sein eigener Elternknoten.
pub proof fn no_self_parent(c: Cdt, s: nat)
    requires
        rank_inv(c),
        live(c, s),
    ensures
        c.nodes[s as int].parent != Some(s),
{
    // Waere parent[s]==Some(s), so rank[s] < rank[s] (rank_inv) -> Widerspruch.
}

/// **KOROLLAR (kein 2-Zyklus):** der Elternknoten eines Knotens hat diesen nicht als Elternknoten.
pub proof fn no_2cycle(c: Cdt, s: nat)
    requires
        rank_inv(c),
        live(c, s),
        c.nodes[s as int].parent is Some,
    ensures
        ({
            let p = c.nodes[s as int].parent->Some_0;
            c.nodes[p as int].parent != Some(s)
        }),
{
    // rank[p] < rank[s]; waere parent[p]==Some(s), so rank[s] < rank[p] -> Widerspruch (rank[s]<rank[s]).
}

/// **BEWEIS:** `derive` (Kind `c` von `p` ableiten, mit `rank[c] = rank[p] + 1`) **erhaelt** die
/// Rang-Monotonie — und damit die Azyklizitaet.
pub proof fn derive(c0: Cdt, p: nat) -> (c2: Cdt)
    requires
        rank_inv(c0),
        live(c0, p),
    ensures
        rank_inv(c2),
{
    let child = Node {
        used: true,
        parent: Some(p),
        rank: c0.nodes[p as int].rank + 1,
    };
    let c2 = Cdt { nodes: c0.nodes.push(child) };

    assert forall|s: nat|
        live(c2, s) && c2.nodes[s as int].parent is Some
        implies live(c2, c2.nodes[s as int].parent->Some_0)
            && c2.nodes[c2.nodes[s as int].parent->Some_0 as int].rank < c2.nodes[s as int].rank by {
        let cnew = c0.nodes.len() as nat;
        if s == cnew {
            // neuer Knoten: parent==p, rank[p] < rank[p]+1 == rank[c]. live(p) aus Vorbedingung.
            assert(c2.nodes[s as int] == child);
        } else {
            // alter Knoten unveraendert; sein Elternknoten unveraendert (push aendert keine Indizes < len).
            assert(c2.nodes[s as int] == c0.nodes[s as int]);
        }
    }
    c2
}

fn main() {}

} // verus!
