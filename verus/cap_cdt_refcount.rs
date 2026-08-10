// Caprock — Verus-Pilot (Tier 2, funktionale Verifikation).
//
// Erste FORMALE SPEZIFIKATION einer bereits dokumentierten Kernel-Invariante: der Refcount-Anteil
// von `cap_audit_cdt` (crates/caprock-cap/src/space.rs, Codes 1-3). Zur Laufzeit prueft das Audit
// die Invariante nur an Quiescenz-Punkten; hier wird statisch + fuer ALLE Zustaende bewiesen, dass
// die Capability-Operationen `copy`/`delete` sie ERHALTEN.
//
// Modellierter Invariantenkern (audit_cdt Codes 1-3):
//   (1) jeder belegte Slot zeigt auf ein gueltiges, belegtes Objekt;
//   (2) refcount(o) == Anzahl belegter Slots, die auf o zeigen;
//   (3) ein Objekt ist belegt  <==>  refcount(o) > 0.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein Objekt der Objekttabelle: belegt-Flag + Referenzzaehler (Modell von `Object`).
pub struct Obj {
    pub used: bool,
    pub refcount: nat,
}

/// Ein Capability-Slot: belegt-Flag + Index des referenzierten Objekts (gueltig nur, wenn `used`).
pub struct Slot {
    pub used: bool,
    pub object: nat,
}

/// Der relevante Ausschnitt des Capability-Space: Objekt- + Slot-Tabelle.
pub struct CapTable {
    pub objects: Seq<Obj>,
    pub slots: Seq<Slot>,
}

/// Beitrag eines einzelnen Slots zum Referenzzaehler von Objekt `o` (1, falls belegt + zeigt auf o).
pub open spec fn contrib(sl: Slot, o: nat) -> nat {
    if sl.used && sl.object == o { 1nat } else { 0nat }
}

/// Anzahl **belegter** Slots, die auf Objekt `o` zeigen — genau das, was `audit_cdt` zur Laufzeit
/// in `refs[obj] += 1` zaehlt.
pub open spec fn refs_to(slots: Seq<Slot>, o: nat) -> nat
    decreases slots.len(),
{
    if slots.len() == 0 {
        0
    } else {
        refs_to(slots.drop_last(), o) + contrib(slots.last(), o)
    }
}

/// Die Refcount-Invariante (audit_cdt Codes 1-3) als Spezifikation.
pub open spec fn inv(t: CapTable) -> bool {
    // (1) jeder belegte Slot zeigt auf ein gueltiges, belegtes Objekt
    &&& (forall|s: int| #![trigger t.slots[s]] 0 <= s < t.slots.len() && t.slots[s].used
        ==> t.slots[s].object < t.objects.len() && t.objects[t.slots[s].object as int].used)
    // (2) refcount == Anzahl zeigender Slots; (3) belegt <==> refcount > 0
    &&& (forall|o: int| #![trigger t.objects[o]] 0 <= o < t.objects.len()
        ==> t.objects[o].refcount == refs_to(t.slots, o as nat)
            && (t.objects[o].used <==> t.objects[o].refcount > 0))
}

/// **Lemma:** einen Slot ans Ende anzuhaengen aendert `refs_to(o)` genau um `contrib(sl, o)`.
pub proof fn lemma_refs_push(slots: Seq<Slot>, sl: Slot, o: nat)
    ensures
        refs_to(slots.push(sl), o) == refs_to(slots, o) + contrib(sl, o),
{
    assert(slots.push(sl).drop_last() =~= slots);
    assert(slots.push(sl).last() == sl);
}

/// **Lemma:** den Slot an Index `i` zu ersetzen verschiebt `refs_to(o)` um die Differenz der
/// Beitraege (alter vs. neuer Slot). Per Induktion ueber die Slot-Anzahl.
pub proof fn lemma_refs_update(slots: Seq<Slot>, i: int, sl: Slot, o: nat)
    requires
        0 <= i < slots.len(),
    ensures
        refs_to(slots.update(i, sl), o) + contrib(slots[i], o)
            == refs_to(slots, o) + contrib(sl, o),
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

/// **Lemma:** zeigt ein belegter Slot auf `o`, so ist `refs_to(o) >= 1`. Per Induktion.
pub proof fn lemma_refs_member(slots: Seq<Slot>, s: int, o: nat)
    requires
        0 <= s < slots.len(),
        slots[s].used,
        slots[s].object == o,
    ensures
        refs_to(slots, o) >= 1,
    decreases slots.len(),
{
    if s == slots.len() - 1 {
        assert(slots.last() == slots[s]);
    } else {
        assert(slots.drop_last()[s] == slots[s]);
        lemma_refs_member(slots.drop_last(), s, o);
    }
}

/// **Lemma:** zeigt kein belegter Slot auf ein Objekt `o` jenseits aller gueltigen Indizes
/// (`o >= objects_len`, wie ein frisch angelegtes Objekt), so ist `refs_to(o) == 0`. Per Induktion.
pub proof fn lemma_refs_fresh(slots: Seq<Slot>, objects_len: nat, o: nat)
    requires
        o >= objects_len,
        forall|s: int| 0 <= s < slots.len() && (#[trigger] slots[s].used) ==> slots[s].object
            < objects_len,
    ensures
        refs_to(slots, o) == 0,
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

/// **BEWEIS:** `install` (ein NEUES Objekt anlegen + einen darauf zeigenden Slot: `refcount = 1`)
/// **erhaelt** die Refcount-Invariante.
pub proof fn install(t: CapTable) -> (t2: CapTable)
    requires
        inv(t),
    ensures
        inv(t2),
{
    let new_o: nat = t.objects.len() as nat;
    let new_slot = Slot { used: true, object: new_o };
    let objects2 = t.objects.push(Obj { used: true, refcount: 1 });
    let slots2 = t.slots.push(new_slot);
    let t2 = CapTable { objects: objects2, slots: slots2 };

    // Kein bestehender Slot zeigt auf das frische Objekt -> refs_to(t.slots, new_o) == 0.
    lemma_refs_fresh(t.slots, t.objects.len() as nat, new_o);
    // Effekt des angehaengten Slots auf refs_to fuer JEDES x.
    assert forall|x: nat| #![trigger refs_to(slots2, x)]
        refs_to(slots2, x) == refs_to(t.slots, x) + contrib(new_slot, x) by {
        lemma_refs_push(t.slots, new_slot, x);
    }
    // (1) Slot-Validitaet.
    assert forall|s: int| 0 <= s < slots2.len() && #[trigger] slots2[s].used implies slots2[s].object
        < objects2.len() && objects2[slots2[s].object as int].used by {
        if s < t.slots.len() {
            assert(slots2[s] == t.slots[s]);
        }
    }
    // (2)+(3).
    assert forall|x: int| 0 <= x < objects2.len() implies #[trigger] objects2[x].refcount
        == refs_to(slots2, x as nat) && (objects2[x].used <==> objects2[x].refcount > 0) by {
        if x < t.objects.len() {
            assert(objects2[x] == t.objects[x]);
        }
    }
    t2
}

/// **BEWEIS:** `copy` (eine bestehende Capability auf Objekt `o` duplizieren: neuer Slot + refcount++)
/// **erhaelt** die Refcount-Invariante.
pub proof fn copy(t: CapTable, o: nat) -> (t2: CapTable)
    requires
        inv(t),
        o < t.objects.len(),
        t.objects[o as int].used,
    ensures
        inv(t2),
{
    let new_slot = Slot { used: true, object: o };
    let slots2 = t.slots.push(new_slot);
    let objects2 = t.objects.update(
        o as int,
        Obj { used: true, refcount: t.objects[o as int].refcount + 1 },
    );
    let t2 = CapTable { objects: objects2, slots: slots2 };

    // (A) refs_to-Effekt des angehaengten Slots fuer JEDES Objekt x.
    assert forall|x: nat| #![trigger refs_to(slots2, x)]
        refs_to(slots2, x) == refs_to(t.slots, x) + contrib(new_slot, x) by {
        lemma_refs_push(t.slots, new_slot, x);
    }

    // (B) Invariantenteil (1): Slot-Validitaet bleibt erhalten.
    assert forall|s: int| 0 <= s < slots2.len() && #[trigger] slots2[s].used implies slots2[s].object
        < objects2.len() && objects2[slots2[s].object as int].used by {
        if s < t.slots.len() {
            assert(slots2[s] == t.slots[s]);
        }
    }

    // (C) Invariantenteil (2)+(3): refcount == refs_to + belegt<==>refcount>0, je Objekt.
    assert forall|x: int| 0 <= x < objects2.len() implies #[trigger] objects2[x].refcount
        == refs_to(slots2, x as nat) && (objects2[x].used <==> objects2[x].refcount > 0) by {
        if x != o {
            assert(objects2[x] == t.objects[x]);
        }
    }
    t2
}

/// **BEWEIS:** `delete` (den belegten Slot `i` loeschen: refcount--; bei 0 das Objekt freigeben)
/// **erhaelt** die Refcount-Invariante.
pub proof fn delete(t: CapTable, i: int) -> (t2: CapTable)
    requires
        inv(t),
        0 <= i < t.slots.len(),
        t.slots[i].used,
    ensures
        inv(t2),
{
    let o = t.slots[i].object;
    // Aus inv(t) (1): Slot i gueltig -> o < len, objects[o] belegt.
    assert(t.slots[i].object < t.objects.len() && t.objects[t.slots[i].object as int].used);
    // refcount(o) == refs_to(t.slots, o) (inv (2)) und >= 1 (Slot i zaehlt).
    lemma_refs_member(t.slots, i, o);
    let oldrc = t.objects[o as int].refcount;
    assert(oldrc == refs_to(t.slots, o));
    assert(oldrc >= 1);
    let newrc: nat = (oldrc - 1) as nat;

    let dead = Slot { used: false, object: 0 };
    let slots2 = t.slots.update(i, dead);
    let objects2 = t.objects.update(o as int, Obj { used: newrc > 0, refcount: newrc });
    let t2 = CapTable { objects: objects2, slots: slots2 };

    // (A) refs_to-Effekt des Loeschens fuer JEDES Objekt x: -1 fuer o, sonst unveraendert.
    assert forall|x: nat| #![trigger refs_to(slots2, x)]
        refs_to(slots2, x) + contrib(t.slots[i], x) == refs_to(t.slots, x) + contrib(dead, x) by {
        lemma_refs_update(t.slots, i, dead, x);
    }

    // (B) Invariantenteil (1): Slot-Validitaet.
    assert forall|s: int| 0 <= s < slots2.len() && #[trigger] slots2[s].used implies slots2[s].object
        < objects2.len() && objects2[slots2[s].object as int].used by {
        // s != i (slots2[i] ist unbelegt); alter Slot unveraendert + gueltig.
        assert(slots2[s] == t.slots[s]);
        let q = slots2[s].object;
        if q == o {
            // s zeigt belegt auf o -> refs_to(slots2, o) >= 1 -> newrc >= 1 -> objects2[o] belegt.
            lemma_refs_member(slots2, s, o);
        }
    }

    // (C) Invariantenteil (2)+(3).
    assert forall|x: int| 0 <= x < objects2.len() implies #[trigger] objects2[x].refcount
        == refs_to(slots2, x as nat) && (objects2[x].used <==> objects2[x].refcount > 0) by {
        if x != o {
            assert(objects2[x] == t.objects[x]);
        }
    }
    t2
}

fn main() {}

} // verus!
