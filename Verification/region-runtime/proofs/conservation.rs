// Caprock — Phase 3 (Region-Runtime), Schritt A/B: Ressourcen-Konservierung + Balance + Ownership.
//
// Funktionale Korrektheit ueber die Kani-Speichersicherheit hinaus (RegionView ist bereits memory-safe
// bewiesen, docs/verification.md): hier wird die RESSOURCEN-BILANZ des Region-Allokators bewiesen:
//   * Konservierung: kein Byte entsteht/verschwindet -- `free + Summe(lebende Regionen) == total`;
//   * Ownership / keine Doppel-Freigabe: `free_region` setzt eine lebende Region auf tot;
//   * Balance: `alloc` gefolgt von `free_region` stellt `free` exakt wieder her (kein Leak).
// Spiegelt die Laufzeit-`total_free`-Balance-Checks (churn/sasheap) -- aus „geprueft" wird „bewiesen".
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Eine allozierte Region: Groesse + lebend? (tot = freigegeben, Modell der linearen Ownership).
pub struct Region {
    pub size: nat,
    pub live: bool,
}

/// Der Region-Allokator: Gesamtkapazitaet, aktuell freie Bytes, die (lebenden/toten) Regionen.
pub struct Pool {
    pub total: nat,
    pub free: nat,
    pub regions: Seq<Region>,
}

/// Summe der Groessen der **lebenden** Regionen (= in Benutzung gebundene Bytes).
pub open spec fn live_bytes(regions: Seq<Region>) -> nat
    decreases regions.len(),
{
    if regions.len() == 0 {
        0
    } else {
        let last = regions.last();
        live_bytes(regions.drop_last()) + (if last.live { last.size } else { 0nat })
    }
}

/// **Konservierungs-Invariante:** `free + Summe(lebende Regionen) == total` — kein Byte entsteht oder
/// verschwindet; `free <= total`.
pub open spec fn pool_inv(p: Pool) -> bool {
    p.free + live_bytes(p.regions) == p.total
}

/// **Lemma:** eine Region anzuhaengen erhoeht `live_bytes` um ihre Groesse (falls lebend), sonst 0.
pub proof fn lemma_live_push(regions: Seq<Region>, r: Region)
    ensures live_bytes(regions.push(r)) == live_bytes(regions) + (if r.live { r.size } else { 0nat }),
{
    assert(regions.push(r).drop_last() =~= regions);
    assert(regions.push(r).last() == r);
}

/// **Lemma:** eine Region bei `i` ersetzen verschiebt `live_bytes` um die Beitragsdifferenz (Induktion).
pub proof fn lemma_live_update(regions: Seq<Region>, i: int, r: Region)
    requires 0 <= i < regions.len(),
    ensures live_bytes(regions.update(i, r)) + (if regions[i].live { regions[i].size } else { 0nat })
        == live_bytes(regions) + (if r.live { r.size } else { 0nat }),
    decreases regions.len(),
{
    let upd = regions.update(i, r);
    if i == regions.len() - 1 {
        assert(upd.drop_last() =~= regions.drop_last());
        assert(upd.last() == r);
        assert(regions.last() == regions[i]);
    } else {
        assert(upd.last() == regions.last());
        assert(upd.drop_last() =~= regions.drop_last().update(i, r));
        assert(regions.drop_last()[i] == regions[i]);
        lemma_live_update(regions.drop_last(), i, r);
    }
}

/// **alloc** (Spec): eine Region der Groesse `n` allozieren — `free -= n`, eine lebende Region anhaengen.
pub open spec fn alloc(p: Pool, n: nat) -> Pool {
    Pool {
        total: p.total,
        free: (p.free - n) as nat,
        regions: p.regions.push(Region { size: n, live: true }),
    }
}

/// **free_region** (Spec): eine Region `i` freigeben — `free += size`, Region auf tot setzen.
pub open spec fn free_region(p: Pool, i: int) -> Pool {
    Pool {
        total: p.total,
        free: p.free + p.regions[i].size,
        regions: p.regions.update(i, Region { size: p.regions[i].size, live: false }),
    }
}

/// **BEWEIS (alloc erhaelt Konservierung):** nach `alloc(n)` (mit `n <= free`) gilt weiter `free + live
/// == total`, `free` ist um `n` gesunken, und es gibt genau eine neue Region.
pub proof fn alloc_preserves(p: Pool, n: nat)
    requires
        pool_inv(p),
        n <= p.free,
    ensures
        pool_inv(alloc(p, n)),
        alloc(p, n).free == (p.free - n) as nat,
        alloc(p, n).regions.len() == p.regions.len() + 1,
{
    lemma_live_push(p.regions, Region { size: n, live: true });
}

/// **BEWEIS (free_region erhaelt Konservierung + keine Doppel-Freigabe):** das Freigeben einer
/// **lebenden** Region erhaelt `free + live == total`; `free` steigt um die Groesse. Die Vorbedingung
/// `live` verhindert Doppel-Freigabe (eine tote Region kann nicht erneut freigegeben werden).
pub proof fn free_preserves(p: Pool, i: int)
    requires
        pool_inv(p),
        0 <= i < p.regions.len(),
        p.regions[i].live,
    ensures
        pool_inv(free_region(p, i)),
        free_region(p, i).free == p.free + p.regions[i].size,
{
    lemma_live_update(p.regions, i, Region { size: p.regions[i].size, live: false });
}

/// **BEWEIS (Balance / kein Leak):** `alloc(n)` gefolgt vom Freigeben der gerade allozierten Region
/// stellt `free` **exakt** wieder her — eine balancierte Sequenz hinterlaesst kein gebundenes Byte.
pub proof fn alloc_free_balance(p: Pool, n: nat)
    requires
        pool_inv(p),
        n <= p.free,
    ensures
        pool_inv(free_region(alloc(p, n), (alloc(p, n).regions.len() - 1) as int)),
        free_region(alloc(p, n), (alloc(p, n).regions.len() - 1) as int).free == p.free,
{
    alloc_preserves(p, n);
    let pa = alloc(p, n);
    assert(pa.regions[(pa.regions.len() - 1) as int] == Region { size: n, live: true });
    free_preserves(pa, (pa.regions.len() - 1) as int);
}

fn main() {}

} // verus!
