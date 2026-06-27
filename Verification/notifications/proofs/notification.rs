// SEL4Lake — Phase 6 (Notifications), Schritt A/B: Badge-Akkumulation + Single-Waiter-Rendezvous.
//
// Formale Spezifikation der Notification-Kern-Invariante (crate sel4lake-ipc, `Notification`,
// `Notification::audit`): ein Notification-Objekt akkumuliert Badge-Bits per ODER (`signal`) und hat
// **hoechstens einen** blockierten Wartenden. Bewiesen: kein Signal geht verloren (jedes signalisierte
// Bit ist danach entweder im pending-Wort ODER an den geweckten Wartenden zugestellt); ein blockierter
// Wartender sitzt **nie** auf unzugestellten Signalen (`waiter is Some ==> pending leer`); `wait`
// holt das gesamte pending-Wort ab und leert es (kein Rest, kein Duplikat); `purge` (eager Cleanup
// eines sterbenden Wartenden) erhaelt die Invariante.
//
// Sequentielles Modell (eigener Lock je Objekt); Nebenlaeufigkeit + kern-uebergreifendes Wecken
// (unblock+IPI) bleiben ausserhalb (Concurrency-/HAL-TCB, ADR 0020). Badge-Bits als `Set<nat>`.
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Ein Notification-Objekt: pending = akkumulierte, noch nicht abgeholte Badge-Bits; `waiter` = der
/// (einzige) blockierte Konsument. `id` modelliert die Thread-ID des Wartenden abstrakt.
pub struct Ntfn {
    pub used: bool,
    pub pending: Set<nat>,        // ODER-akkumulierte Badge-Bits
    pub waiter: Option<nat>,      // hoechstens ein blockierter Wartender
}

/// Leere Bit-Menge.
pub open spec fn empty() -> Set<nat> { Set::empty() }

/// **Notification-Kern-Invariante:** ein blockierter Wartender sitzt nie auf unzugestellten Signalen —
/// gäbe es ein pending-Bit, wäre er sofort geweckt worden. Also: `waiter is Some ==> pending leer`.
pub open spec fn ntfn_inv(n: Ntfn) -> bool {
    n.waiter is Some ==> (n.pending =~= empty())
}

/// Ausgabe von `signal`: neuer Zustand + die (ggf. leere) an den geweckten Wartenden **zugestellte**
/// Bit-Menge (Buchhaltung gegen Signalverlust).
pub struct SignalOut {
    pub n: Ntfn,
    pub delivered: Set<nat>,
}

/// **signal:** `badge`-Bits ins pending-Wort ODERn und einen etwaigen Wartenden wecken (ihm das
/// gesamte pending-Wort zustellen + leeren). Nicht blockierend.
pub open spec fn signal(n: Ntfn, badge: Set<nat>) -> SignalOut {
    let p = n.pending.union(badge);
    if n.waiter is Some {
        // Rendezvous: der Wartende erhaelt das gesamte (akkumulierte) Wort, pending wird geleert.
        SignalOut { n: Ntfn { pending: empty(), waiter: None, ..n }, delivered: p }
    } else {
        // kein Wartender: Bits akkumulieren.
        SignalOut { n: Ntfn { pending: p, ..n }, delivered: empty() }
    }
}

/// Ausgabe von `wait`: neuer Zustand + die abgeholte Bit-Menge (`consumed`) + ob blockiert wurde.
pub struct WaitOut {
    pub n: Ntfn,
    pub consumed: Set<nat>,
    pub blocked: bool,
}

/// **wait:** liegt ein pending-Wort vor -> sofort abholen (+ leeren); sonst blockieren (Wartenden
/// vermerken). `tid` = der aufrufende Thread.
pub open spec fn wait(n: Ntfn, tid: nat) -> WaitOut {
    if !(n.pending =~= empty()) {
        WaitOut { n: Ntfn { pending: empty(), ..n }, consumed: n.pending, blocked: false }
    } else {
        WaitOut { n: Ntfn { waiter: Some(tid), ..n }, consumed: empty(), blocked: true }
    }
}

/// **purge:** einen sterbenden Wartenden entfernen (eager Cleanup; verhindert ein SIGNAL in einen
/// recycelten Frame).
pub open spec fn purge(n: Ntfn) -> Ntfn {
    Ntfn { waiter: None, ..n }
}

// ===================== Bewiesene Eigenschaften =====================

/// **BEWEIS (signal erhaelt die Invariante).**
pub proof fn signal_preserves_inv(n: Ntfn, badge: Set<nat>)
    requires ntfn_inv(n),
    ensures ntfn_inv(signal(n, badge).n),
{
    // waiter-Fall: neuer waiter = None -> vakuous. kein-waiter-Fall: waiter bleibt None.
}

/// **BEWEIS (kein Signalverlust):** jedes signalisierte Bit ist danach **entweder** im pending-Wort
/// **oder** an den geweckten Wartenden zugestellt — nie verloren.
pub proof fn signal_no_loss(n: Ntfn, badge: Set<nat>)
    requires ntfn_inv(n),
    ensures badge.subset_of(signal(n, badge).n.pending.union(signal(n, badge).delivered)),
{
    let out = signal(n, badge);
    assert(n.pending.union(badge).subset_of(out.n.pending.union(out.delivered)));
}

/// **BEWEIS (signal weckt den Wartenden vollstaendig):** liegt ein Wartender vor, erhaelt er das
/// gesamte akkumulierte Wort (inkl. der neuen Bits) und das Objekt hat danach keinen Wartenden mehr.
pub proof fn signal_wakes_waiter(n: Ntfn, badge: Set<nat>)
    requires ntfn_inv(n), n.waiter is Some,
    ensures
        signal(n, badge).delivered =~= n.pending.union(badge),
        signal(n, badge).n.waiter is None,
        signal(n, badge).n.pending =~= empty(),
{
}

/// **BEWEIS (wait erhaelt die Invariante).**
pub proof fn wait_preserves_inv(n: Ntfn, tid: nat)
    requires ntfn_inv(n),
    ensures ntfn_inv(wait(n, tid).n),
{
    // pending-Fall: neuer waiter = alter waiter; war er Some, war pending leer (inv) -> Widerspruch
    // zur pending!=leer-Bedingung, also waiter None. block-Fall: pending leer -> inv gilt.
}

/// **BEWEIS (wait holt alles ab, ohne Rest/Duplikat):** liegt ein pending-Wort vor, ist die abgeholte
/// Menge **genau** das alte pending-Wort und das Objekt ist danach leer (genau-einmal-Konsum).
pub proof fn wait_consumes_all(n: Ntfn, tid: nat)
    requires ntfn_inv(n), !(n.pending =~= empty()),
    ensures
        wait(n, tid).consumed =~= n.pending,
        wait(n, tid).n.pending =~= empty(),
        !wait(n, tid).blocked,
{
}

/// **BEWEIS (Fortschritt):** bei vorhandenem pending-Wort blockiert `wait` **nicht** (kein unnoetiges
/// Blockieren trotz wartender Signale).
pub proof fn wait_progress(n: Ntfn, tid: nat)
    requires ntfn_inv(n), !(n.pending =~= empty()),
    ensures !wait(n, tid).blocked,
{
}

/// **BEWEIS (purge erhaelt die Invariante):** das Entfernen des Wartenden kann die Invariante nicht
/// verletzen (waiter danach None -> vakuous).
pub proof fn purge_preserves_inv(n: Ntfn)
    requires ntfn_inv(n),
    ensures ntfn_inv(purge(n)),
{
}

fn main() {}

} // verus!
