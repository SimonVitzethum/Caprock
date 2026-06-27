// SEL4Lake — Phase 1 (Capability-System), Schritt B+C: VEREINTES Modell + VOLLE Invariante.
//
// Ein einziges `CapSpace`-Modell (Objekt- + Slot-/CDT-Tabelle) mit der VOLLSTAENDIGEN
// `cap_audit_cdt`-Invariante als EINE `spec fn cap_inv` (Konjunktion der Klauseln 1-7). Jede
// Capability-Operation wird bewiesen, `cap_inv` zu ERHALTEN -- d. h. eine Operation erhaelt ALLE
// Invarianten zugleich (anders als die getrennten Pilot-Modelle in verus/cap_cdt_*.rs).
//
// Doku (eigenstaendig verstaendlich): Verification/capability-system/README.md. ADR 0015.
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein Objekt der Objekttabelle (Modell von `Object`): belegt + Referenzzaehler.
pub struct Object {
    pub used: bool,
    pub refcount: nat,
}

/// Ein Capability-Slot (Modell von `Slot` + `Mdb`): belegt, referenziertes Objekt, CDT-Verkettung,
/// + ein Ghost-`rank` als Wohlfundiertheits-Mass fuer die Azyklizitaet (Code 7).
pub struct Slot {
    pub used: bool,
    pub object: nat,
    pub parent: Option<nat>,
    pub first_child: Option<nat>,
    pub next: Option<nat>,
    pub prev: Option<nat>,
    pub rank: nat,
}

/// Der Capability-Space: Objekt- + Slot-Tabelle.
pub struct CapSpace {
    pub objects: Seq<Object>,
    pub slots: Seq<Slot>,
}

/// Ist Slot `i` gueltig + belegt? (CDT-Verkettungen zeigen auf Slot-Indizes.)
pub open spec fn slot_live(cs: CapSpace, i: nat) -> bool {
    i < cs.slots.len() && cs.slots[i as int].used
}

/// Beitrag eines Slots zum Referenzzaehler von Objekt `o`.
pub open spec fn contrib(sl: Slot, o: nat) -> nat {
    if sl.used && sl.object == o { 1nat } else { 0nat }
}

/// Anzahl belegter Slots, die auf Objekt `o` zeigen (= was `cap_audit_cdt` in `refs[obj]+=1` zaehlt).
pub open spec fn refs_to(slots: Seq<Slot>, o: nat) -> nat
    decreases slots.len(),
{
    if slots.len() == 0 { 0 } else { refs_to(slots.drop_last(), o) + contrib(slots.last(), o) }
}

/// **Die VOLLSTAENDIGE `cap_audit_cdt`-Invariante (Codes 1-3, 4-lokal, 5, 6, 7)** als eine Spezifikation.
pub open spec fn cap_inv(cs: CapSpace) -> bool {
    // (1) jeder belegte Slot zeigt auf ein gueltiges, belegtes Objekt
    &&& (forall|s: int| #![trigger cs.slots[s].used]
        0 <= s < cs.slots.len() && cs.slots[s].used
            ==> cs.slots[s].object < cs.objects.len() && cs.objects[cs.slots[s].object as int].used)
    // (2) refcount == Anzahl zeigender Slots; (3) belegt <==> refcount > 0
    &&& (forall|o: int| #![trigger cs.objects[o]]
        0 <= o < cs.objects.len()
            ==> cs.objects[o].refcount == refs_to(cs.slots, o as nat)
                && (cs.objects[o].used <==> cs.objects[o].refcount > 0))
    // (4-lokal, 5, 6, 7) CDT-Verkettung
    &&& (forall|s: nat| #![trigger cs.slots[s as int]]
        slot_live(cs, s) ==> {
            let nd = cs.slots[s as int];
            // (4l) parent gueltig, teilt das Objekt; (7) Eltern-Rang strikt kleiner (Azyklizitaet)
            &&& (nd.parent is Some ==> slot_live(cs, nd.parent->Some_0)
                && cs.slots[nd.parent->Some_0 as int].object == nd.object
                && cs.slots[nd.parent->Some_0 as int].rank < nd.rank)
            // (5) Sibling-Inverse
            &&& (nd.next is Some ==> slot_live(cs, nd.next->Some_0)
                && cs.slots[nd.next->Some_0 as int].prev == Some(s))
            &&& (nd.prev is Some ==> slot_live(cs, nd.prev->Some_0)
                && cs.slots[nd.prev->Some_0 as int].next == Some(s))
            // (6) first_child gueltig, zeigt zurueck, ist Listenkopf (prev==None)
            &&& (nd.first_child is Some ==> slot_live(cs, nd.first_child->Some_0)
                && cs.slots[nd.first_child->Some_0 as int].parent == Some(s)
                && cs.slots[nd.first_child->Some_0 as int].prev is None)
        })
}

// ============================ Zaehl-Lemmas (Refcount) ============================

/// Anhaengen eines Slots aendert `refs_to(o)` um `contrib(sl, o)`.
pub proof fn lemma_refs_push(slots: Seq<Slot>, sl: Slot, o: nat)
    ensures refs_to(slots.push(sl), o) == refs_to(slots, o) + contrib(sl, o),
{
    assert(slots.push(sl).drop_last() =~= slots);
    assert(slots.push(sl).last() == sl);
}

/// Zeigt kein belegter Slot auf `o >= objects_len` (frisches Objekt), so `refs_to(o) == 0`.
pub proof fn lemma_refs_fresh(slots: Seq<Slot>, objects_len: nat, o: nat)
    requires
        o >= objects_len,
        forall|s: int| 0 <= s < slots.len() && (#[trigger] slots[s].used) ==> slots[s].object < objects_len,
    ensures refs_to(slots, o) == 0,
    decreases slots.len(),
{
    if slots.len() != 0 {
        assert forall|s: int| 0 <= s < slots.drop_last().len() && (#[trigger] slots.drop_last()[s].used)
            implies slots.drop_last()[s].object < objects_len by {
            assert(slots.drop_last()[s] == slots[s]);
        }
        lemma_refs_fresh(slots.drop_last(), objects_len, o);
    }
}

// ============================ Operation: install ============================

/// **BEWEIS:** `install` (neues Objekt mit `refcount=1` + eine Wurzel-Capability darauf: ein neuer
/// Slot ohne CDT-Verkettung, `rank=0`) **erhaelt die VOLLE Invariante** `cap_inv`.
pub proof fn install(cs: CapSpace) -> (cs2: CapSpace)
    requires
        cap_inv(cs),
    ensures
        cap_inv(cs2),
{
    let new_o: nat = cs.objects.len() as nat;
    let root = Slot {
        used: true,
        object: new_o,
        parent: None,
        first_child: None,
        next: None,
        prev: None,
        rank: 0,
    };
    let objects2 = cs.objects.push(Object { used: true, refcount: 1 });
    let slots2 = cs.slots.push(root);
    let cs2 = CapSpace { objects: objects2, slots: slots2 };

    // Kein bestehender Slot zeigt auf das frische Objekt.
    lemma_refs_fresh(cs.slots, cs.objects.len() as nat, new_o);
    // Effekt des angehaengten Slots auf refs_to fuer jedes x.
    assert forall|x: nat| #![trigger refs_to(slots2, x)]
        refs_to(slots2, x) == refs_to(cs.slots, x) + contrib(root, x) by {
        lemma_refs_push(cs.slots, root, x);
    }
    // (1) Slot-Validitaet.
    assert forall|s: int| 0 <= s < slots2.len() && #[trigger] slots2[s].used
        implies slots2[s].object < objects2.len() && objects2[slots2[s].object as int].used by {
        if s < cs.slots.len() {
            assert(slots2[s] == cs.slots[s]);
        }
    }
    // (2)+(3) Refcount.
    assert forall|o: int| 0 <= o < objects2.len()
        implies #[trigger] objects2[o].refcount == refs_to(slots2, o as nat)
            && (objects2[o].used <==> objects2[o].refcount > 0) by {
        if o < cs.objects.len() {
            assert(objects2[o] == cs.objects[o]);
        }
    }
    // (4l,5,6,7) CDT: der neue Slot hat keine Verkettungen (alle None); alte Slots unveraendert + ihre
    // Verkettungen zeigen auf alte (unveraenderte) Slots.
    assert forall|s: nat| slot_live(cs2, s) implies {
        let nd = cs2.slots[s as int];
        &&& (nd.parent is Some ==> slot_live(cs2, nd.parent->Some_0)
            && cs2.slots[nd.parent->Some_0 as int].object == nd.object
            && cs2.slots[nd.parent->Some_0 as int].rank < nd.rank)
        &&& (nd.next is Some ==> slot_live(cs2, nd.next->Some_0) && cs2.slots[nd.next->Some_0 as int].prev == Some(s))
        &&& (nd.prev is Some ==> slot_live(cs2, nd.prev->Some_0) && cs2.slots[nd.prev->Some_0 as int].next == Some(s))
        &&& (nd.first_child is Some ==> slot_live(cs2, nd.first_child->Some_0)
            && cs2.slots[nd.first_child->Some_0 as int].parent == Some(s)
            && cs2.slots[nd.first_child->Some_0 as int].prev is None)
    } by {
        let cnew = cs.slots.len() as nat;
        if s != cnew {
            assert(cs2.slots[s as int] == cs.slots[s as int]);
        }
    }
    cs2
}

fn main() {}

} // verus!
