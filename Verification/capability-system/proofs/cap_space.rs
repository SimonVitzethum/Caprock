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
//
// **Modell-Treue (Schritt C2b).** Die vier Schreibzugriffe unten spiegeln `CapSpace::unlink` +
// `CapSpace::release_slot` in `crates/sel4lake-cap/src/space.rs` Schritt fuer Schritt: gleiche
// Verzweigung, gleiche Reihenfolge, gleiche Feldzuweisung.
//
//     let mdb = self.slots[slot].mdb;                                   // `inode` unten
//     match mdb.prev_sibling {
//         Some(p) => self.slots[p].mdb.next_sibling = mdb.next_sibling,      // unlink1, Zweig 1
//         None => {
//             if let Some(par) = mdb.parent {
//                 self.slots[par].mdb.first_child = mdb.next_sibling;        // unlink1, Zweig 2
//             }
//         }
//     }
//     if let Some(n) = mdb.next_sibling {
//         self.slots[n].mdb.prev_sibling = mdb.prev_sibling;                 // unlink2
//     }
//     self.slots[slot].mdb = Mdb::EMPTY;                                     // unlink_slots
//     // ... release_slot(slot): self.slots[slot] = CapSlot::EMPTY (used=false)
//
// `tools/verus-modelltreue.sh` haelt diese Entsprechung fest und schlaegt an, wenn eine der
// beiden Seiten sich ohne die andere aendert.
//
// **Eine Stelle, an der der Code mehr tut als die Invariante verlangt** (Befund, kein Fehler):
// Zweig 2 schreibt `first_child[par] = next` sobald `prev is None && parent is Some` -- OHNE zu
// pruefen, ob `first_child[par]` ueberhaupt `Some(i)` war. Unter `cap_inv` ALLEIN folgt das nicht
// (Klausel 6 sagt nur die Gegenrichtung); erst die Reachability-Klausel 4r macht beides gleich.
// `cap_inv` bleibt trotzdem erhalten -- der Beweis unten fuehrt genau diesen Fall mit.

/// Der geloeschte Slot: `CapSlot::EMPTY` + `Mdb::EMPTY` (`rank` ist Ghost, also 0).
pub open spec fn dead_slot() -> Slot {
    Slot { used: false, object: 0, parent: None, first_child: None, next: None, prev: None, rank: 0 }
}

/// `unlink`, Schritt 1: `match prev { Some(p) => next[p] = next[i], None => first_child[par] = next[i] }`.
pub open spec fn unlink1(slots: Seq<Slot>, i: nat) -> Seq<Slot> {
    let nd = slots[i as int];
    if nd.prev is Some {
        let pv = nd.prev->Some_0;
        slots.update(pv as int, Slot { next: nd.next, ..slots[pv as int] })
    } else if nd.parent is Some {
        let par = nd.parent->Some_0;
        slots.update(par as int, Slot { first_child: nd.next, ..slots[par as int] })
    } else {
        slots
    }
}

/// `unlink`, Schritt 2: `if let Some(n) = next { prev[n] = prev[i] }` — auf dem Stand NACH Schritt 1.
pub open spec fn unlink2(slots: Seq<Slot>, i: nat) -> Seq<Slot> {
    let nd = slots[i as int];
    let s1 = unlink1(slots, i);
    if nd.next is Some {
        let nx = nd.next->Some_0;
        s1.update(nx as int, Slot { prev: nd.prev, ..s1[nx as int] })
    } else {
        s1
    }
}

/// `unlink`, Schritt 3 (+ `release_slot`): der Slot selbst wird geleert — **zuletzt**, wie im Code.
pub open spec fn unlink_slots(slots: Seq<Slot>, i: nat) -> Seq<Slot> {
    unlink2(slots, i).update(i as int, dead_slot())
}

// ---------------------------------------------------------------------------------------------
// Rahmen-Lemma: WAS die drei Schreibzugriffe NICHT anfassen.
// ---------------------------------------------------------------------------------------------

/// **BEWEIS:** `unlink` aendert ausschliesslich `next`/`prev`/`first_child` — und den Slot `i`
/// selbst. Fuer jeden anderen Slot bleiben `used`, `object`, `parent` und `rank` **unveraendert**.
/// Das ist die zweite Haelfte der Erreichbarkeitsaussage (s. [`unreachable_after_delete`]): ein
/// Eingriff, der sauber aushaengt und dabei ein fremdes Kind umhaengt, faellt hier durch.
pub proof fn lemma_unlink_frame(cs: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
    ensures
        unlink_slots(cs.slots, i).len() == cs.slots.len(),
        unlink1(cs.slots, i).len() == cs.slots.len(),
        unlink2(cs.slots, i).len() == cs.slots.len(),
        unlink_slots(cs.slots, i)[i as int] == dead_slot(),
        forall|s: int| #![trigger unlink_slots(cs.slots, i)[s]]
            0 <= s < cs.slots.len() && s != (i as int) ==> {
                &&& unlink_slots(cs.slots, i)[s].used == cs.slots[s].used
                &&& unlink_slots(cs.slots, i)[s].object == cs.slots[s].object
                &&& unlink_slots(cs.slots, i)[s].parent == cs.slots[s].parent
                &&& unlink_slots(cs.slots, i)[s].rank == cs.slots[s].rank
            },
{
    let nd = cs.slots[i as int];
    // Die drei Schreibziele liegen in der Tabelle — aus cap_inv, nicht angenommen.
    assert(nd.prev is Some ==> slot_live(cs, nd.prev->Some_0));
    assert(nd.next is Some ==> slot_live(cs, nd.next->Some_0));
    assert(nd.parent is Some ==> slot_live(cs, nd.parent->Some_0));
}

/// **BEWEIS:** die Blatt-Eigenschaft ist **keine zusaetzliche Annahme** — sie folgt aus
/// `no_children` + Klausel 6. (Deshalb faellt der Beweis nicht, wenn man die Vorbedingung
/// `first_child is None` streicht; s. Bericht zur Mutationsmessung.)
pub proof fn lemma_leaf_from_no_children(cs: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        forall|s: nat| slot_live(cs, s) ==> cs.slots[s as int].parent != Some(i),
    ensures
        cs.slots[i as int].first_child is None,
{
    if cs.slots[i as int].first_child is Some {
        let c = cs.slots[i as int].first_child->Some_0;
        assert(slot_live(cs, c) && cs.slots[c as int].parent == Some(i));
    }
}

// ---------------------------------------------------------------------------------------------
// Refcount: nur der geleerte Slot faellt weg.
// ---------------------------------------------------------------------------------------------

/// **BEWEIS:** die drei Verkettungs-Schreibzugriffe lassen `refs_to` unberuehrt (sie aendern weder
/// `used` noch `object`); allein `i -> dead_slot()` senkt den Zaehler um `contrib(slots[i], x)`.
pub proof fn lemma_unlink_refs(cs: CapSpace, i: nat, x: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
    ensures
        refs_to(unlink_slots(cs.slots, i), x) + contrib(cs.slots[i as int], x) == refs_to(cs.slots, x),
{
    let nd = cs.slots[i as int];
    let s1 = unlink1(cs.slots, i);
    let s2 = unlink2(cs.slots, i);
    lemma_unlink_frame(cs, i);

    if nd.prev is Some {
        let pv = nd.prev->Some_0;
        assert(slot_live(cs, pv));
        lemma_refs_update(cs.slots, pv as int, Slot { next: nd.next, ..cs.slots[pv as int] }, x);
    } else if nd.parent is Some {
        let par = nd.parent->Some_0;
        assert(slot_live(cs, par));
        lemma_refs_update(cs.slots, par as int, Slot { first_child: nd.next, ..cs.slots[par as int] }, x);
    }
    assert(refs_to(s1, x) == refs_to(cs.slots, x));
    assert(s1[i as int].used == nd.used && s1[i as int].object == nd.object);

    if nd.next is Some {
        let nx = nd.next->Some_0;
        assert(slot_live(cs, nx));
        lemma_refs_update(s1, nx as int, Slot { prev: nd.prev, ..s1[nx as int] }, x);
    }
    assert(refs_to(s2, x) == refs_to(cs.slots, x));
    assert(s2[i as int].used == nd.used && s2[i as int].object == nd.object);

    lemma_refs_update(s2, i as int, dead_slot(), x);
}

// ---------------------------------------------------------------------------------------------
// Die CDT-Klauseln, je eine eigene SMT-Query.
// ---------------------------------------------------------------------------------------------

/// **BEWEIS (4l + 7):** `parent` bleibt fuer jeden ueberlebenden Slot unveraendert, das Ziel ist
/// **nicht** der geloeschte Slot (`no_children`), und Objekt/Rang der Elternbeziehung stehen still.
pub proof fn lemma_del_parent(cs: CapSpace, cs2: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        forall|s: nat| slot_live(cs, s) ==> cs.slots[s as int].parent != Some(i),
        cs2.slots == unlink_slots(cs.slots, i),
    ensures
        forall|s: nat| #![trigger cs2.slots[s as int]]
            slot_live(cs2, s) && cs2.slots[s as int].parent is Some ==> {
                &&& slot_live(cs2, cs2.slots[s as int].parent->Some_0)
                &&& cs2.slots[cs2.slots[s as int].parent->Some_0 as int].object == cs2.slots[s as int].object
                &&& cs2.slots[cs2.slots[s as int].parent->Some_0 as int].rank < cs2.slots[s as int].rank
            },
{
    lemma_unlink_frame(cs, i);
    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].parent is Some implies {
        &&& slot_live(cs2, cs2.slots[s as int].parent->Some_0)
        &&& cs2.slots[cs2.slots[s as int].parent->Some_0 as int].object == cs2.slots[s as int].object
        &&& cs2.slots[cs2.slots[s as int].parent->Some_0 as int].rank < cs2.slots[s as int].rank
    } by {
        // Der geloeschte Slot ist tot -> s ist ein anderer, und er lebte schon vorher.
        assert(cs2.slots[i as int] == dead_slot());
        assert((s as int) != (i as int));
        assert(slot_live(cs, s));
        assert(cs2.slots[s as int].parent == cs.slots[s as int].parent);
        let p = cs.slots[s as int].parent->Some_0;
        assert(slot_live(cs, p));                            // Klausel 4l an s
        assert(cs.slots[s as int].parent != Some(i));        // no_children
        assert((p as int) != (i as int));
        assert(cs2.slots[p as int].used == cs.slots[p as int].used);
    }
}

/// **BEWEIS (5 + 4-sib):** `next` zeigt weiter auf einen lebenden Slot, dessen `prev` zurueckzeigt
/// und dessen `parent` derselbe ist. Zwei Faelle: `s` ist der Vorgaenger des geloeschten Slots
/// (dann erbt `s` dessen `next`), oder `s` ist unbeteiligt.
pub proof fn lemma_del_next(cs: CapSpace, cs2: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        cs2.slots == unlink_slots(cs.slots, i),
    ensures
        forall|s: nat| #![trigger cs2.slots[s as int]]
            slot_live(cs2, s) && cs2.slots[s as int].next is Some ==> {
                &&& slot_live(cs2, cs2.slots[s as int].next->Some_0)
                &&& cs2.slots[cs2.slots[s as int].next->Some_0 as int].prev == Some(s)
                &&& cs2.slots[cs2.slots[s as int].next->Some_0 as int].parent == cs2.slots[s as int].parent
            },
{
    let nd = cs.slots[i as int];
    let s1 = unlink1(cs.slots, i);
    let s2 = unlink2(cs.slots, i);
    lemma_unlink_frame(cs, i);

    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].next is Some implies {
        &&& slot_live(cs2, cs2.slots[s as int].next->Some_0)
        &&& cs2.slots[cs2.slots[s as int].next->Some_0 as int].prev == Some(s)
        &&& cs2.slots[cs2.slots[s as int].next->Some_0 as int].parent == cs2.slots[s as int].parent
    } by {
        assert(cs2.slots[i as int] == dead_slot());
        assert((s as int) != (i as int));
        assert(slot_live(cs, s));
        // Schritt 2 schreibt nur `prev`, Schritt 3 nur den Slot i -> `next` stammt aus Schritt 1.
        assert(cs2.slots[s as int].next == s1[s as int].next);
        let n = cs2.slots[s as int].next->Some_0;
        if nd.prev == Some(s) {
            // s ist der Vorgaenger von i und erbt dessen next.
            assert(s1[s as int].next == nd.next);
            assert(slot_live(cs, n) && cs.slots[n as int].prev == Some(i)
                && cs.slots[n as int].parent == nd.parent);      // Klausel 5/4-sib an i
            // n == i waere ein Selbst-Geschwister: dann prev[i]==Some(i), also s==i.
            assert((n as int) != (i as int));
            // n ist genau das nx aus Schritt 2 -> prev[n] wurde auf prev[i] == Some(s) gesetzt.
            assert(s2[n as int].prev == nd.prev);
            // parent: cs.slots[s].next == Some(i) -> Klausel 4-sib an s -> parent[i] == parent[s].
            assert(cs.slots[s as int].next == Some(i));
            assert(cs.slots[i as int].parent == cs.slots[s as int].parent);
        } else {
            // s ist unbeteiligt: sein next steht noch da, wo es stand.
            assert(s1[s as int].next == cs.slots[s as int].next);
            assert(slot_live(cs, n) && cs.slots[n as int].prev == Some(s)
                && cs.slots[n as int].parent == cs.slots[s as int].parent);   // Klausel 5/4-sib an s
            // n == i haette prev[i] == Some(s) verlangt — das ist gerade der andere Fall.
            assert((n as int) != (i as int));
            // nd.next == Some(n) haette prev[n] == Some(i) verlangt, aber prev[n] == Some(s), s != i.
            assert(nd.next != Some(n));
            assert(s2[n as int].prev == s1[n as int].prev);
            assert(s1[n as int].prev == cs.slots[n as int].prev);
        }
    }
}

/// **BEWEIS (5):** `prev` zeigt weiter auf einen lebenden Slot, dessen `next` zurueckzeigt.
pub proof fn lemma_del_prev(cs: CapSpace, cs2: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        cs2.slots == unlink_slots(cs.slots, i),
    ensures
        forall|s: nat| #![trigger cs2.slots[s as int]]
            slot_live(cs2, s) && cs2.slots[s as int].prev is Some ==> {
                &&& slot_live(cs2, cs2.slots[s as int].prev->Some_0)
                &&& cs2.slots[cs2.slots[s as int].prev->Some_0 as int].next == Some(s)
            },
{
    let nd = cs.slots[i as int];
    let s1 = unlink1(cs.slots, i);
    let s2 = unlink2(cs.slots, i);
    lemma_unlink_frame(cs, i);

    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].prev is Some implies {
        &&& slot_live(cs2, cs2.slots[s as int].prev->Some_0)
        &&& cs2.slots[cs2.slots[s as int].prev->Some_0 as int].next == Some(s)
    } by {
        assert(cs2.slots[i as int] == dead_slot());
        assert((s as int) != (i as int));
        assert(slot_live(cs, s));
        assert(cs2.slots[s as int].prev == s2[s as int].prev);
        let pp = cs2.slots[s as int].prev->Some_0;
        if nd.next == Some(s) {
            // s ist der Nachfolger von i und erbt dessen prev.
            assert(s2[s as int].prev == nd.prev);
            assert(slot_live(cs, pp) && cs.slots[pp as int].next == Some(i));   // Klausel 5 an i
            assert((pp as int) != (i as int));
            // pp ist genau das pv aus Schritt 1 -> next[pp] wurde auf next[i] == Some(s) gesetzt.
            assert(s1[pp as int].next == nd.next);
            assert(s2[pp as int].next == s1[pp as int].next);
        } else {
            assert(s2[s as int].prev == s1[s as int].prev);
            assert(s1[s as int].prev == cs.slots[s as int].prev);
            assert(slot_live(cs, pp) && cs.slots[pp as int].next == Some(s));   // Klausel 5 an s
            assert((pp as int) != (i as int));
            // nd.prev == Some(pp) haette next[pp] == Some(i) verlangt, aber next[pp] == Some(s).
            assert(nd.prev != Some(pp));
            assert(s1[pp as int].next == cs.slots[pp as int].next);
            assert(s2[pp as int].next == s1[pp as int].next);
        }
    }
}

/// **BEWEIS (6):** `first_child` bleibt Listenkopf — auch beim Elternknoten, dessen Kopf gerade
/// nachgezogen wurde.
pub proof fn lemma_del_first_child(cs: CapSpace, cs2: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        cs2.slots == unlink_slots(cs.slots, i),
    ensures
        forall|s: nat| #![trigger cs2.slots[s as int]]
            slot_live(cs2, s) && cs2.slots[s as int].first_child is Some ==> {
                &&& slot_live(cs2, cs2.slots[s as int].first_child->Some_0)
                &&& cs2.slots[cs2.slots[s as int].first_child->Some_0 as int].parent == Some(s)
                &&& cs2.slots[cs2.slots[s as int].first_child->Some_0 as int].prev is None
            },
{
    let nd = cs.slots[i as int];
    let s1 = unlink1(cs.slots, i);
    let s2 = unlink2(cs.slots, i);
    lemma_unlink_frame(cs, i);

    assert forall|s: nat| slot_live(cs2, s) && cs2.slots[s as int].first_child is Some implies {
        &&& slot_live(cs2, cs2.slots[s as int].first_child->Some_0)
        &&& cs2.slots[cs2.slots[s as int].first_child->Some_0 as int].parent == Some(s)
        &&& cs2.slots[cs2.slots[s as int].first_child->Some_0 as int].prev is None
    } by {
        assert(cs2.slots[i as int] == dead_slot());
        assert((s as int) != (i as int));
        assert(slot_live(cs, s));
        assert(cs2.slots[s as int].first_child == s1[s as int].first_child);
        let c = cs2.slots[s as int].first_child->Some_0;
        if nd.prev is None && nd.parent == Some(s) {
            // s ist der Elternknoten, dessen Kopf nachgezogen wurde: neuer Kopf ist next[i].
            assert(s1[s as int].first_child == nd.next);
            assert(slot_live(cs, c) && cs.slots[c as int].prev == Some(i)
                && cs.slots[c as int].parent == nd.parent);      // Klausel 5/4-sib an i
            assert((c as int) != (i as int));
            // c ist genau das nx aus Schritt 2 -> prev[c] wurde auf prev[i] == None gesetzt.
            assert(s2[c as int].prev == nd.prev);
        } else {
            assert(s1[s as int].first_child == cs.slots[s as int].first_child);
            assert(slot_live(cs, c) && cs.slots[c as int].parent == Some(s)
                && cs.slots[c as int].prev is None);             // Klausel 6 an s
            // c == i haette prev[i] is None und parent[i] == Some(s) verlangt — der andere Fall.
            assert((c as int) != (i as int));
            // nd.next == Some(c) haette prev[c] == Some(i) verlangt, aber prev[c] is None.
            assert(nd.next != Some(c));
            assert(s2[c as int].prev == s1[c as int].prev);
            assert(s1[c as int].prev == cs.slots[c as int].prev);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Kinderlisten-Erreichbarkeit: der geloeschte Slot ist von nirgends mehr zu erreichen —
// und KEIN fremdes Kind ist dabei umgehaengt worden.
// ---------------------------------------------------------------------------------------------

/// **SATZ (Erreichbarkeit nach dem Loeschen).** Nach `delete(i)` gilt beides zugleich:
///
/// 1. **kein lebender Slot zeigt noch auf `i`** — ueber **keine** der vier Kanten
///    (`parent`, `next`, `prev`, `first_child`), und `i` selbst ist tot;
/// 2. **der Elternknoten jedes anderen Slots ist unveraendert.**
///
/// Ohne (2) waere (1) wertlos: ein Eingriff, der sauber aushaengt und nebenbei ein fremdes Kind
/// umhaengt, erfuellte (1) und waere trotzdem falsch. Deshalb steht (2) hier und nicht daneben.
pub proof fn unreachable_after_delete(cs: CapSpace, cs2: CapSpace, i: nat)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        forall|s: nat| slot_live(cs, s) ==> cs.slots[s as int].parent != Some(i),
        cs2.slots == unlink_slots(cs.slots, i),
    ensures
        // (1a) der geloeschte Slot ist tot.
        !cs2.slots[i as int].used,
        // (1b) keine Kante eines lebenden Slots trifft ihn noch.
        forall|s: nat| #![trigger cs2.slots[s as int]]
            slot_live(cs2, s) ==> {
                &&& cs2.slots[s as int].parent != Some(i)
                &&& cs2.slots[s as int].next != Some(i)
                &&& cs2.slots[s as int].prev != Some(i)
                &&& cs2.slots[s as int].first_child != Some(i)
            },
        // (2) und der Elter aller uebrigen Slots steht still.
        forall|s: int| #![trigger cs2.slots[s]]
            0 <= s < cs.slots.len() && s != (i as int) ==> cs2.slots[s].parent == cs.slots[s].parent,
{
    let nd = cs.slots[i as int];
    let s1 = unlink1(cs.slots, i);
    let s2 = unlink2(cs.slots, i);
    lemma_unlink_frame(cs, i);

    assert forall|s: nat| slot_live(cs2, s) implies {
        &&& cs2.slots[s as int].parent != Some(i)
        &&& cs2.slots[s as int].next != Some(i)
        &&& cs2.slots[s as int].prev != Some(i)
        &&& cs2.slots[s as int].first_child != Some(i)
    } by {
        assert(cs2.slots[i as int] == dead_slot());
        assert((s as int) != (i as int));
        assert(slot_live(cs, s));
        // parent: unveraendert (Rahmen) + no_children.
        assert(cs2.slots[s as int].parent == cs.slots[s as int].parent);
        // next: stammt aus Schritt 1.
        assert(cs2.slots[s as int].next == s1[s as int].next);
        if nd.prev == Some(s) {
            // s erbt next[i]; waere das Some(i), so prev[i] == Some(i) (Klausel 5 an i) und s == i.
            assert(s1[s as int].next == nd.next);
            assert(nd.next == Some(i) ==> cs.slots[i as int].prev == Some(i));
        } else {
            // next[s] unveraendert; waere es Some(i), so prev[i] == Some(s) (Klausel 5 an s).
            assert(s1[s as int].next == cs.slots[s as int].next);
            assert(cs.slots[s as int].next == Some(i) ==> cs.slots[i as int].prev == Some(s));
        }
        // prev: stammt aus Schritt 2.
        assert(cs2.slots[s as int].prev == s2[s as int].prev);
        if nd.next == Some(s) {
            assert(s2[s as int].prev == nd.prev);
            assert(nd.prev == Some(i) ==> cs.slots[i as int].next == Some(i));
        } else {
            assert(s2[s as int].prev == s1[s as int].prev);
            assert(s1[s as int].prev == cs.slots[s as int].prev);
            assert(cs.slots[s as int].prev == Some(i) ==> cs.slots[i as int].next == Some(s));
        }
        // first_child: stammt aus Schritt 1.
        assert(cs2.slots[s as int].first_child == s1[s as int].first_child);
        if nd.prev is None && nd.parent == Some(s) {
            assert(s1[s as int].first_child == nd.next);
            assert(nd.next == Some(i) ==> cs.slots[i as int].prev == Some(i));
        } else {
            assert(s1[s as int].first_child == cs.slots[s as int].first_child);
            // waere first_child[s] == Some(i), so parent[i] == Some(s) und prev[i] is None
            // (Klausel 6 an s) — das ist gerade der andere Fall.
            assert(cs.slots[s as int].first_child == Some(i)
                ==> cs.slots[i as int].parent == Some(s) && cs.slots[i as int].prev is None);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Die Operation selbst.
// ---------------------------------------------------------------------------------------------

/// **BEWEIS:** `delete` (eine **Blatt**-Capability `i` löschen — Geschwister umhängen, ggf.
/// `first_child` des Elternknotens nachziehen, Slot leeren, `refcount--`, Objekt bei 0 freigeben)
/// **erhaelt die VOLLE Invariante** `cap_inv`.
///
/// **Dekomposition (s. README §10):** der Beweis setzt `no_children` voraus — dass **kein** belegter
/// Slot `i` als Elternknoten hat. Diese Tatsache **folgt aus der vollen Reachability-Invariante**
/// (Code 4r). Sie hier als Vorbedingung zu fuehren **trennt** „delete erhaelt cap_inv **gegeben**
/// Reachability" sauber vom (schwierigeren) Nachweis der Reachability selbst.
///
/// Die strukturelle Erhaltung liegt in **eigenen Lemmas** (`lemma_del_parent`/`_next`/`_prev`/
/// `_first_child`) statt in diesem Rumpf: eine Klausel je SMT-Query. In EINER Query gerechnet
/// lief dieser Beweis in die rlimit-Wand — nicht weil eine Klausel schwer waere, sondern weil
/// alle vier zusammen mit der Refcount-Rechnung in einem Kontext standen.
pub proof fn delete(cs: CapSpace, i: nat) -> (cs2: CapSpace)
    requires
        cap_inv(cs),
        slot_live(cs, i),
        // Blatt-Eigenschaft. Redundant (s. `lemma_leaf_from_no_children`), aber sie ist das,
        // was der Code an dieser Stelle geprueft hat — deshalb steht sie hier.
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

    let sD = unlink_slots(cs.slots, i);
    let objs2 = cs.objects.update(o as int, Object { used: newrc > 0, refcount: newrc });
    let cs2 = CapSpace { objects: objs2, slots: sD };

    lemma_unlink_frame(cs, i);
    assert(sD[i as int] == dead_slot());

    // --- refs_to: nur der geleerte Slot faellt weg.
    assert forall|x: nat| #![trigger refs_to(sD, x)]
        refs_to(sD, x) + contrib(inode, x) == refs_to(cs.slots, x) by {
        lemma_unlink_refs(cs, i, x);
    }

    // --- (1) Slot-Validitaet ---
    assert forall|s: int| 0 <= s < sD.len() && #[trigger] sD[s].used
        implies sD[s].object < objs2.len() && objs2[sD[s].object as int].used by {
        assert(s != (i as int));
        assert(sD[s].object == cs.slots[s].object);
        if sD[s].object == o { lemma_refs_member(sD, s, o); }
    }
    // --- (2)+(3) Refcount ---
    assert forall|x: int| 0 <= x < objs2.len()
        implies #[trigger] objs2[x].refcount == refs_to(sD, x as nat) && (objs2[x].used <==> objs2[x].refcount > 0) by {
        if x != o { assert(objs2[x] == cs.objects[x]); }
    }
    // --- (4l,5,6,7) je Klausel eine eigene Query ---
    lemma_del_parent(cs, cs2, i);
    lemma_del_next(cs, cs2, i);
    lemma_del_prev(cs, cs2, i);
    lemma_del_first_child(cs, cs2, i);
    // --- und der Erreichbarkeitssatz, damit er im selben Lauf mitgeprueft wird ---
    unreachable_after_delete(cs, cs2, i);
    cs2
}

fn main() {}

} // verus!
