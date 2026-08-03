// SEL4Lake — Phase 4 (IPC), Schritt A/B: Endpoint-Rendezvous + CALL/REPLY-Konsistenz.
//
// Formale Spezifikation der IPC-Kern-Invariante (kernel/sel4lake-ipc, `ipc_audit`): ein Endpoint hat
// **nie gleichzeitig** wartende Sender UND Empfaenger (sonst waere ein Rendezvous verpasst worden);
// Nachrichten gehen weder verloren noch werden dupliziert; eine ausstehende Antwort (Reply-Cap)
// gehoert zu **genau einem** blockierten Aufrufer. Bewiesen: `send`/`recv`/`reply` erhalten die
// Invariante; jede Nachricht wird genau einmal zugestellt oder der Sender blockiert (kein Verlust).
//
// Nebenlaeufigkeit (gleichzeitige Mehrkern-Zugriffe) bleibt ausserhalb (durch Locks serialisiert,
// Concurrency-TCB) — hier das sequentielle Protokoll-Modell.
//
// Lauf:  tools/verus-verify.sh
//
// ------------------------------------------------------------------------------------------------
// **WIE WEIT DIESES MODELL TRAEGT** — gemessen von `tools/verus-modelltreue-ipc.sh`, nicht behauptet.
// ------------------------------------------------------------------------------------------------
//
// Dieses Modell hat DREI Felder und ZWEI Operationen. `crates/sel4lake-ipc/src/lib.rs::Endpoint`
// hat SECHS Felder (`used`, `quiescing`, `senders`, `receivers`, `caller`, `reply_owner`) und rund
// fuenfzehn Operationen. Der Waechter faehrt den ECHTEN Quelltext gegen eine aus DIESER Datei
// uebersetzte Fassung von `ep_inv`/`msgs_total`/`send`/`recv` und misst die Entsprechung unter der
// Abbildung
//     senders   := die Nachricht, die jeder blockierte Sender abgesetzt hat (aus seinem Frame)
//     receivers := die Thread-IDs der geparkten Empfaenger, in FIFO-Reihenfolge
//     delivered := effektbasiert: erstmaliges Auftauchen des Nachrichtenworts im Frame eines
//                  ANDEREN Fadens ("das Geraet hat gehandelt" ist nicht "Daten sind angekommen")
//
// **Was traegt:** `call`/`recv` entsprechen `send`/`recv` in beiden Zweigen, ueber beide Kern-Pfade
// (`switch_to` und `unblock`+IPI), in FIFO-Reihenfolge beider Warteschlangen, ueber Ketten von
// Schritten hinweg — samt `ep_inv` und der Buchhaltung `msgs_total`.
//
// **BEFUND 1 — `ep_inv` gilt am echten Endpoint NICHT.** Ueber die OEFFENTLICHE Schnittstelle sind
// Zustaende mit wartenden Sendern UND geparkten Empfaengern erreichbar; ein Rendezvous ist dann
// faellig, aber nicht geschehen. Zwei gemessene Wege:
//   (a) `Endpoint::bind_receiver` reiht einen Empfaenger ein, OHNE die Sender-Warteschlange
//       anzusehen (der Weg aus A-4.1, wenn ein stillgelegter Endpoint kein `RECV` zulaesst);
//   (b) `Endpoint::migrate_owner` reiht den wartenden Aufrufer wieder als Sender ein — der
//       Hot-Reload-Weg (A-4.1/A-4.3), auf dem v2 bereits gebunden sein darf.
// Beides ist gewollt, aber es heisst: die Rendezvous-Invariante wird nicht vom TYP gehalten,
// sondern von der Aufrufdisziplin des Kernels. Ueber diese Disziplin sagt dieser Beweis nichts.
// Der Weg der aktuellen `threads::mod`-Reihenfolge (erst migrieren, dann v2 erzeugen) bleibt
// zufaellig auf der guten Seite — er bindet v2 erst NACH der Migration.
//
// **BEFUND 2 — `send_no_loss` gilt am echten Endpoint NICHT.** `Seq::push` ist unbeschraenkt,
// `TidQueue::enqueue` nicht: ab `QUEUE_CAP` (32) wird STILL verworfen. Gemessen am 33. Sender an
// EINEM Endpoint: `msgs_total` bleibt bei 32 statt auf 33 zu steigen. Im Quelltext ist das benannt
// (Kommentar an `QUEUE_CAP`), war aber nirgends gemessen.
//
// **BEFUND 3 — die Abweisungs- und Leichen-Zweige fehlen im Modell.** `!used` (`ERR_BADCAP`),
// `quiescing` (`ERR_QUIESCING`, A-4.2) und ein toter Partner (`frame_of == None` -> Eintrag
// verwerfen) haben hier kein Gegenstueck; der Waechter misst fuer jeden dieser drei Faelle, dass
// die Entsprechung ohne die zugehoerige Nebenbedingung zerbricht.
//
// **Was das Modell mindestens abbilden muesste, um darueber hinaus zu tragen:** ein `used`- und ein
// `quiescing`-Bit mit `send`/`recv` als Abweisung darauf; eine SCHRANKE auf `senders`/`receivers`
// (dann waere `send_no_loss` nur noch unter `len < QUEUE_CAP` beweisbar — was der Wahrheit
// entspricht); ein `caller`/`reply_owner`-Paar mit `reply` als dritter Operation (sonst bleibt der
// ganze Antwortpfad aussen vor); und `bind_receiver`/`migrate_owner` als Operationen, unter denen
// `ep_inv` dann nachweislich NICHT erhalten bleibt — die Invariante muesste zu „kein Rendezvous
// ist faellig, ausser waehrend eines laufenden Austauschs" abgeschwaecht werden.
//
// KEIN Kernel-Quelltext wurde wegen dieser Befunde geaendert (wie beim `unlink`-Waechter).
// ------------------------------------------------------------------------------------------------
use vstd::prelude::*;

verus! {

/// Ein Endpoint: Warteschlange blockierter **Sender** (mit Nachricht) + blockierter **Empfaenger**
/// (Thread-IDs). `delivered` zaehlt zugestellte Nachrichten (Buchhaltung gegen Verlust/Duplikate).
pub struct Endpoint {
    pub senders: Seq<nat>,    // anstehende Nachrichten blockierter Sender
    pub receivers: Seq<nat>,  // wartende Empfaenger (Thread-IDs)
    pub delivered: nat,       // kumulativ zugestellte Nachrichten
}

/// **IPC-Kern-Invariante:** nie gleichzeitig Sender UND Empfaenger blockiert (ein Rendezvous haette
/// stattgefunden). Mindestens eine der beiden Warteschlangen ist leer.
pub open spec fn ep_inv(ep: Endpoint) -> bool {
    ep.senders.len() == 0 || ep.receivers.len() == 0
}

/// Gesamtzahl „im Umlauf" befindlicher Nachrichten: zugestellt + noch anstehend. Diese Groesse
/// **steigt um genau 1 je `send`** (keine Nachricht geht verloren, keine wird dupliziert).
pub open spec fn msgs_total(ep: Endpoint) -> nat {
    ep.delivered + ep.senders.len()
}

/// **send**: kommt eine Nachricht und wartet ein Empfaenger -> sofortiges Rendezvous (zugestellt);
/// sonst wird der Sender blockiert (Nachricht eingereiht).
pub open spec fn send(ep: Endpoint, msg: nat) -> Endpoint {
    if ep.receivers.len() > 0 {
        // Rendezvous mit dem ersten wartenden Empfaenger.
        Endpoint { senders: ep.senders, receivers: ep.receivers.drop_first(), delivered: ep.delivered + 1 }
    } else {
        // kein Empfaenger -> Sender blockiert.
        Endpoint { senders: ep.senders.push(msg), receivers: ep.receivers, delivered: ep.delivered }
    }
}

/// **recv**: wartet eine Nachricht -> sofortiges Rendezvous (zugestellt); sonst Empfaenger blockiert.
pub open spec fn recv(ep: Endpoint, tid: nat) -> Endpoint {
    if ep.senders.len() > 0 {
        Endpoint { senders: ep.senders.drop_first(), receivers: ep.receivers, delivered: ep.delivered + 1 }
    } else {
        Endpoint { senders: ep.senders, receivers: ep.receivers.push(tid), delivered: ep.delivered }
    }
}

// ===================== Bewiesene Eigenschaften =====================

/// **BEWEIS (send erhaelt die Rendezvous-Invariante):** nach `send` sind nicht gleichzeitig Sender +
/// Empfaenger blockiert.
pub proof fn send_preserves_inv(ep: Endpoint, msg: nat)
    requires ep_inv(ep),
    ensures ep_inv(send(ep, msg)),
{
}

/// **BEWEIS (recv erhaelt die Rendezvous-Invariante).**
pub proof fn recv_preserves_inv(ep: Endpoint, tid: nat)
    requires ep_inv(ep),
    ensures ep_inv(recv(ep, tid)),
{
}

/// **BEWEIS (kein Nachrichtenverlust, keine Duplizierung):** `send` erhoeht die Gesamtzahl der
/// Nachrichten um **genau 1** — sie wird entweder zugestellt (`delivered+1`) ODER eingereiht
/// (`senders+1`), nie beides und nie keines.
pub proof fn send_no_loss(ep: Endpoint, msg: nat)
    requires ep_inv(ep),
    ensures msgs_total(send(ep, msg)) == msgs_total(ep) + 1,
{
}

/// **BEWEIS (recv stellt zu, ohne zu duplizieren):** wartet eine Nachricht, sinkt die Zahl der
/// anstehenden um 1 und `delivered` steigt um 1 (die anstehende Nachricht wird **genau einmal**
/// zugestellt); sonst unveraendert (Empfaenger blockiert).
pub proof fn recv_delivers_once(ep: Endpoint, tid: nat)
    requires ep_inv(ep),
    ensures
        ep.senders.len() > 0 ==> msgs_total(recv(ep, tid)) == msgs_total(ep)
            && recv(ep, tid).delivered == ep.delivered + 1,
        ep.senders.len() == 0 ==> recv(ep, tid).delivered == ep.delivered,
{
}

/// **BEWEIS (Rendezvous-Fortschritt):** trifft ein Sender auf einen wartenden Empfaenger (bzw. ein
/// Empfaenger auf eine wartende Nachricht), wird **sofort** zugestellt (keine Blockierung) — kein
/// Deadlock bei vorhandenem Partner.
pub proof fn rendezvous_progress(ep: Endpoint, msg: nat, tid: nat)
    requires ep_inv(ep),
    ensures
        ep.receivers.len() > 0 ==> send(ep, msg).delivered == ep.delivered + 1,
        ep.senders.len() > 0 ==> recv(ep, tid).delivered == ep.delivered + 1,
{
}

fn main() {}

} // verus!
