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
            // (5) Sibling-Inverse + (4-sib) Geschwister teilen den Elternknoten
            &&& (nd.next is Some ==> slot_live(cs, nd.next->Some_0)
                && cs.slots[nd.next->Some_0 as int].prev == Some(s)
                && cs.slots[nd.next->Some_0 as int].parent == nd.parent)
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

/// Ersetzen von Slot `i` verschiebt `refs_to(o)` um die Beitragsdifferenz (Induktion). Folgerung:
/// aendert das Update weder `used` noch `object` (gleicher Beitrag), bleibt `refs_to` unveraendert.
/// (Fundament fuer die Loesch-Operationen `delete`/`revoke` — Schritt C2.)
pub proof fn lemma_refs_update(slots: Seq<Slot>, i: int, sl: Slot, o: nat)
    requires 0 <= i < slots.len(),
    ensures refs_to(slots.update(i, sl), o) + contrib(slots[i], o) == refs_to(slots, o) + contrib(sl, o),
    decreases slots.len(),
{
    let upd = slots.update(i, sl);
    if i == slots.len() - 1 {
        assert(upd.drop_last() =~= slots.drop_last());
        assert(upd.last() == sl);
        assert(slots.last() == slots[i]);
    } else {
        assert(upd.last() == slots.last());
        assert(upd.drop_last() =~= slots.drop_last().update(i, sl));
        assert(slots.drop_last()[i] == slots[i]);
        lemma_refs_update(slots.drop_last(), i, sl, o);
    }
}

/// Zeigt ein belegter Slot auf `o`, so `refs_to(o) >= 1` (Induktion).
pub proof fn lemma_refs_member(slots: Seq<Slot>, s: int, o: nat)
    requires 0 <= s < slots.len(), slots[s].used, slots[s].object == o,
    ensures refs_to(slots, o) >= 1,
    decreases slots.len(),
{
    if s == slots.len() - 1 {
        assert(slots.last() == slots[s]);
    } else {
        assert(slots.drop_last()[s] == slots[s]);
        lemma_refs_member(slots.drop_last(), s, o);
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

// ============================ Operation: copy ============================

/// **BEWEIS:** `copy` (eine Capability auf Slot `src` ableiten: neues Kind am Kopf der Kinderliste,
/// gleiches Objekt, `refcount++`, `rank = rank[src]+1`) **erhaelt die VOLLE Invariante** `cap_inv`.
/// Vereint Refcount (Objekt +1), Struktur (4l/5/6) und Azyklizitaet (7) in EINEM Beweis.
pub proof fn copy(cs: CapSpace, src: nat) -> (cs2: CapSpace)
    requires
        cap_inv(cs),
        slot_live(cs, src),
    ensures
        cap_inv(cs2),
{
    let o = cs.slots[src as int].object;
    let cnew: nat = cs.slots.len() as nat;
    let srcnode = cs.slots[src as int];
    let old_head = srcnode.first_child;
    let new_slot = Slot {
        used: true, object: o, parent: Some(src), first_child: None,
        next: old_head, prev: None, rank: srcnode.rank + 1,
    };
    let s1 = cs.slots.update(src as int, Slot { first_child: Some(cnew), ..srcnode });
    let s2 = if old_head is Some {
        let h = old_head->Some_0;
        s1.update(h as int, Slot { prev: Some(cnew), ..s1[h as int] })
    } else { s1 };
    let s3 = s2.push(new_slot);
    let objs2 = cs.objects.update(o as int, Object { used: true, refcount: cs.objects[o as int].refcount + 1 });
    let cs2 = CapSpace { objects: objs2, slots: s3 };

    // --- Schluesselfakten aus cap_inv(cs) ---
    // o gueltig + belegt; src zaehlt -> refs_to(o) >= 1 == altem refcount.
    assert(o < cs.objects.len() && cs.objects[o as int].used);
    lemma_refs_member(cs.slots, src as int, o);
    assert(cs.objects[o as int].refcount == refs_to(cs.slots, o));
    // old_head h (falls vorhanden) ist src's first_child: belegt, parent==src, prev==None.
    if old_head is Some {
        let h = old_head->Some_0;
        assert(slot_live(cs, h) && cs.slots[h as int].parent == Some(src) && cs.slots[h as int].prev is None);
        // prev[h]==None -> KEIN belegter Slot zeigt per next auf h.
        assert forall|t: nat| slot_live(cs, t) implies cs.slots[t as int].next != Some(h) by {
            if cs.slots[t as int].next == Some(h) {}  // -> prev[h]==Some(t) != None: Widerspruch
        }
    }

    // --- refs_to: die src/h-Updates aendern weder used noch object -> refs_to unveraendert; push +1 fuer o.
    assert forall|x: nat| #![trigger refs_to(s3, x)] refs_to(s3, x) == refs_to(cs.slots, x) + contrib(new_slot, x) by {
        // s1: update(src) mit gleichem used/object -> contrib gleich.
        lemma_refs_update(cs.slots, src as int, Slot { first_child: Some(cnew), ..srcnode }, x);
        if old_head is Some {
            let h = old_head->Some_0;
            lemma_refs_update(s1, h as int, Slot { prev: Some(cnew), ..s1[h as int] }, x);
        }
        lemma_refs_push(s2, new_slot, x);
    }

    // --- (1) Slot-Validitaet ---
    assert forall|s: int| 0 <= s < s3.len() && #[trigger] s3[s].used
        implies s3[s].object < objs2.len() && objs2[s3[s].object as int].used by {
        if s < cs.slots.len() {
            // alte Slots: object/used unveraendert (Updates aendern nur CDT-Links).
        }
    }
    // --- (2)+(3) Refcount ---
    assert forall|x: int| 0 <= x < objs2.len()
        implies #[trigger] objs2[x].refcount == refs_to(s3, x as nat) && (objs2[x].used <==> objs2[x].refcount > 0) by {
        if x != o { assert(objs2[x] == cs.objects[x]); }
    }
    // --- (4l,5,6,7) CDT-Struktur + Azyklizitaet ---
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
        // Verus fuehrt die Fallunterscheidung (s == cnew / src / h / sonst) ueber die obigen Fakten.
    }
    cs2
}

// ============================ Operation: mint ============================

/// **BEWEIS:** `mint` (eine Capability ableiten **mit reduzierten Rechten + Badge**) **erhaelt
/// `cap_inv`**. Bzgl. der CDT-/Refcount-Invariante ist `mint` **strukturell identisch zu `copy`**:
/// es erzeugt dasselbe Kind am selben Ort mit demselben Objekt + `refcount++`; Rechte/Badge sind
/// **nicht** Teil des Invariant-Modells (sie beeinflussen Autorität, nicht die Buchhaltung). Der
/// Beweis delegiert daher an [`copy`].
pub proof fn mint(cs: CapSpace, src: nat) -> (cs2: CapSpace)
    requires
        cap_inv(cs),
        slot_live(cs, src),
    ensures
        cap_inv(cs2),
{
    copy(cs, src)
}


// ============================ Operation: delete (Leaf) ============================

/// **BEWEIS:** `delete` (eine **Blatt**-Capability `i` löschen — keine Kinder: Geschwister umhängen,
/// ggf. `first_child` des Elternknotens nachziehen, `refcount--`, Objekt bei 0 freigeben)
/// **erhaelt die VOLLE Invariante** `cap_inv`.
///
/// **Dekomposition (s. README §10):** der Beweis setzt `no_children` voraus — dass **kein** belegter
/// Slot `i` als Elternknoten hat. Diese Tatsache **folgt aus der vollen Reachability-Invariante**
/// (Code 4r: ein Kind ist von `parent.first_child` aus erreichbar; ein Blatt mit `first_child==None`
/// hat daher keine Kinder). Sie hier als Vorbedingung zu fuehren **trennt** „delete erhaelt cap_inv
/// **gegeben** Reachability" sauber vom (schwierigeren) Nachweis der Reachability selbst. Die
/// strukturelle Erhaltung ist je CDT-Klausel einzeln gefuehrt (kleinere SMT-Queries).
#[verifier::rlimit(50)]
pub proof fn delete(cs: CapSpace, i: nat) -> (cs2: CapSpace)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        cs.slots[i as int].first_child is None,
        // no_children: aus der Reachability-Invariante (Code 4r) + Blatt-Eigenschaft.
        forall|s: nat| slot_live(cs, s) ==> cs.slots[s as int].parent != Some(i),
    ensures
        cap_inv(cs2),
{
    let inode = cs.slots[i as int];
    let o = inode.object;
    lemma_refs_member(cs.slots, i as int, o);
    assert(cs.objects[o as int].refcount == refs_to(cs.slots, o));
    let oldrc = cs.objects[o as int].refcount;
    assert(oldrc >= 1);
    let newrc: nat = (oldrc - 1) as nat;

    let dead = Slot { used: false, object: 0, parent: None, first_child: None, next: None, prev: None, rank: 0 };
    let sA = cs.slots.update(i as int, dead);
    let sB = if inode.prev is Some {
        let pv = inode.prev->Some_0;
        sA.update(pv as int, Slot { next: inode.next, ..sA[pv as int] })
    } else { sA };
    let sC = if inode.next is Some {
        let nx = inode.next->Some_0;
        sB.update(nx as int, Slot { prev: inode.prev, ..sB[nx as int] })
    } else { sB };
    let sD = if inode.parent is Some && cs.slots[inode.parent->Some_0 as int].first_child == Some(i) {
        let p = inode.parent->Some_0;
        sC.update(p as int, Slot { first_child: inode.next, ..sC[p as int] })
    } else { sC };
    let objs2 = cs.objects.update(o as int, Object { used: newrc > 0, refcount: newrc });
    let cs2 = CapSpace { objects: objs2, slots: sD };

    // --- Erhaltungs-Hilfsfakt: delete aendert NUR next/prev/first_child (+ used von i). Fuer s != i
    //     bleiben parent/object/rank/used unveraendert. (Kern fuer die strukturellen Klauseln.)
    assert forall|s: int| 0 <= s < sD.len() && s != i
        implies #[trigger] sD[s].parent == cs.slots[s].parent
            && sD[s].object == cs.slots[s].object
            && sD[s].rank == cs.slots[s].rank
            && sD[s].used == cs.slots[s].used by {}

    // --- refs_to: nur i->dead senkt o um 1; die Folge-Updates aendern nur CDT-Links (used/object gleich).
    assert forall|x: nat| #![trigger refs_to(sD, x)]
        refs_to(sD, x) == refs_to(cs.slots, x) + contrib(dead, x) - contrib(inode, x) by {
        lemma_refs_update(cs.slots, i as int, dead, x);
        if inode.prev is Some {
            let pv = inode.prev->Some_0;
            lemma_refs_update(sA, pv as int, Slot { next: inode.next, ..sA[pv as int] }, x);
        }
        if inode.next is Some {
            let nx = inode.next->Some_0;
            lemma_refs_update(sB, nx as int, Slot { prev: inode.prev, ..sB[nx as int] }, x);
        }
        if inode.parent is Some && cs.slots[inode.parent->Some_0 as int].first_child == Some(i) {
            let p = inode.parent->Some_0;
            lemma_refs_update(sC, p as int, Slot { first_child: inode.next, ..sC[p as int] }, x);
        }
    }

    // --- Schluesselfakten ---
    if inode.next is Some {
        let nx = inode.next->Some_0;
        assert(slot_live(cs, nx) && cs.slots[nx as int].prev == Some(i) && cs.slots[nx as int].parent == inode.parent);
    }

    // --- (1) Slot-Validitaet ---
    assert forall|s: int| 0 <= s < sD.len() && #[trigger] sD[s].used
        implies sD[s].object < objs2.len() && objs2[sD[s].object as int].used by {
        assert(sD[s].object == cs.slots[s].object);
        if sD[s].object == o { lemma_refs_member(sD, s, o); }
    }
    // --- (2)+(3) Refcount ---
    assert forall|x: int| 0 <= x < objs2.len()
        implies #[trigger] objs2[x].refcount == refs_to(sD, x as nat) && (objs2[x].used <==> objs2[x].refcount > 0) by {
        if x != o { assert(objs2[x] == cs.objects[x]); }
    }
    // --- (4l+7) parent: unveraendert; Ziel != i (no_children) -> belegt; object/rank unveraendert ---
    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].parent is Some
        implies slot_live(cs2, cs2.slots[s as int].parent->Some_0)
            && cs2.slots[cs2.slots[s as int].parent->Some_0 as int].object == cs2.slots[s as int].object
            && cs2.slots[cs2.slots[s as int].parent->Some_0 as int].rank < cs2.slots[s as int].rank by {}
    // --- (5+4-sib) next: Sibling-Inverse + geteilter Elternknoten ---
    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].next is Some
        implies slot_live(cs2, cs2.slots[s as int].next->Some_0)
            && cs2.slots[cs2.slots[s as int].next->Some_0 as int].prev == Some(s)
            && cs2.slots[cs2.slots[s as int].next->Some_0 as int].parent == cs2.slots[s as int].parent by {}
    // --- (5) prev: Sibling-Inverse ---
    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].prev is Some
        implies slot_live(cs2, cs2.slots[s as int].prev->Some_0)
            && cs2.slots[cs2.slots[s as int].prev->Some_0 as int].next == Some(s) by {}
    // --- (6) first_child: Listenkopf ---
    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].first_child is Some
        implies slot_live(cs2, cs2.slots[s as int].first_child->Some_0)
            && cs2.slots[cs2.slots[s as int].first_child->Some_0 as int].parent == Some(s)
            && cs2.slots[cs2.slots[s as int].first_child->Some_0 as int].prev is None by {}
    cs2
}

fn main() {}

} // verus!
