// SEL4Lake — Phase 4 (IPC), Schritt A/B: Endpoint-Rendezvous, Abweisungstor und Reply-Token.
//
// Formale Spezifikation der IPC-Kern-Invarianten (`crates/sel4lake-ipc`, `ipc_audit`): wann ein
// Rendezvous faellig sein darf und wann nicht; dass keine Nachricht unbemerkt verschwindet; dass
// eine ausstehende Antwort zu **genau einem** blockierten Aufrufer gehoert und **genau einmal**
// konsumiert wird; und dass das Stilllegungstor (A-4.2) zwei UNTERSCHEIDBARE Abweisungsgruende
// liefert.
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
// Dieses Modell hat NEUN Felder und ACHT Operationen. `crates/sel4lake-ipc/src/lib.rs::Endpoint`
// hat SECHS Felder und rund fuenfzehn Operationen. Die sechs Codefelder haben hier alle ein
// Gegenstueck (`used`, `quiescing`, `senders`, `receivers`, `caller`, `reply_owner`); die drei
// zusaetzlichen (`delivered`, `rejected_senders`, `rejected_receivers`) sind **Buchhaltung ohne
// Gegenstueck im Code** — der Waechter fuellt sie effektbasiert (s. u.), nicht durch Ablesen.
//
// **Stand 2026-08-04 (D11):** die beiden Zaehler hiessen bis dahin `dropped_*` und zaehlten einen
// **Verlust** — der 33. Sender wurde blockiert und vergessen. Das ist behoben; sie zaehlen jetzt
// eine **Abweisung**: der Aufrufer bekommt `ERR_EP_FULL` und laeuft weiter. Der Unterschied ist
// nicht kosmetisch, er ist die ganze Aussage. Ein Verlust ist unbeobachtbar, eine Abweisung ist
// eine Antwort.
//
// Die Abbildung, unter der gemessen wird:
//     used, quiescing     := die gleichnamigen Felder
//     senders, receivers  := die Thread-IDs der blockierten Sender bzw. geparkten Empfaenger,
//                            in FIFO-Reihenfolge
//     caller, reply_owner := die gleichnamigen Felder (Thread-IDs)
//     delivered           := **effektbasiert**: erstmaliges Auftauchen des Nachrichtenworts im
//                            Frame eines ANDEREN Fadens ("das Geraet hat gehandelt" ist nicht
//                            "Daten sind angekommen")
//     rejected_*          := **effektbasiert**: ein Faden, den die Operation mit einem Code
//                            ungleich 0 zurueckgewiesen hat, OHNE ihn zu blockieren. Der Waechter
//                            misst dazu das Gegenteil: dass danach kein Faden blockiert ist, der
//                            in KEINER Warteschlange steht und kein Token haelt — das war D11
//     send_gate/recv_gate := der Ergebniscode, den `call`/`recv` in den Frame des Abgewiesenen
//                            schreiben; 1 <-> `ERR_BADCAP`, 2 <-> `ERR_QUIESCING`,
//                            3 <-> `ERR_EP_FULL`
//
// **Warum die Fehlercodes hier abstrakt sind (1/2/3 statt 1/8/9):** der Beweis soll nicht von
// ABI-Zahlen abhaengen, sondern davon, dass die Gruende UNTERSCHEIDBAR sind — "kommt gleich
// wieder" ist fuer einen Client eine andere Lage als "gibt es nicht" (A-4.2), und "gerade kein
// Platz" ist eine dritte: sie haengt an den anderen Wartenden statt am Endpoint, kann sofort
// wieder gelten, und wer stumpf wiederholt, verschaerft sie (D11). Die Zuordnung zu den echten
// Konstanten steht im Waechter und wird dort gemessen, samt der Gegenprobe, dass die drei
// ABI-Codes ueberhaupt paarweise verschieden sind.
//
// ------------------------------------------------------------------------------------------------
// **WAS HIER BEWUSST NICHT BEWIESEN WIRD — und warum**
// ------------------------------------------------------------------------------------------------
//
// **(1) Die starke Rendezvous-Invariante `ep_inv_strong` gilt am echten Endpoint NICHT.** Sie
// steht hier weiter, aber sie wird nicht als erhalten behauptet: `bind_receiver` und
// `migrate_owner` verletzen sie, und das ist unten **bewiesen** (`bind_receiver_breaks_strong_inv`,
// `migrate_owner_breaks_strong_inv`) statt kommentiert. Was wirklich haelt, ist `ep_inv` — die
// Abschwaechung "ausserhalb einer Stilllegung ist nie ein Rendezvous faellig". Damit steht die
// Aufrufdisziplin, die die Invariante traegt, als **Vorbedingung im Modell** und nicht mehr in
// einer Prosa-Zeile: beide Operationen erhalten `ep_inv` nur unter `ep.quiescing`.
// `end_quiesce_breaks_inv` zeigt die andere Haelfte derselben Disziplin: das Tor wieder zu
// oeffnen, waehrend ein Rendezvous faellig ist, verletzt auch `ep_inv`.
//
// **(2) "Keine Nachricht geht verloren" gilt nur UNTER der Kapazitaetsschranke** — und darueber
// gilt seit D11 etwas Staerkeres als vorher. `send_no_loss` traegt die Schranke weiterhin als
// Vorbedingung: unter ihr wird nichts einmal abgewiesen. Der Fall darueber ist nicht weggelassen,
// sondern als **Abweisung** bewiesen (`send_rejects_above_cap`, `recv_rejects_above_cap`): der
// Zustand des Endpoints bleibt unberuehrt (`core_eq`), `rejected_senders` steigt, und der
// Abgewiesene bekommt Code 3. Bis zum 2026-08-04 stand hier das Gegenteil — `send_drops_above_cap`
// bewies den **Verlust**, weil der Code ihn hatte.
//
// Der Beweis, um den es dabei geht, ist `send_never_strands`/`recv_never_strands`: unter offenem
// Tor gibt es nur zwei Ausgaenge, zugestellt/eingereiht oder abgewiesen-mit-Code. Einen dritten —
// weder eingereiht noch abgewiesen — gibt es nicht mehr. Genau der war D11. Dass "abgewiesen"
// auch "nicht blockiert" heisst, ist eine Aussage ueber den Scheduler und wird im Waechter
// **gemessen**, nicht hier bewiesen (s. Punkt 5).
//
// **(3) "Das Reply-Token geht nicht verloren" wird NICHT bewiesen, weil es falsch ist.**
// `recv_overwrites_token` beweist das Gegenteil: ein zweites `RECV` desselben Servers, bevor er
// geantwortet hat, ueberschreibt `caller`. Bewiesen ist nur, was haelt: das Token wird gemeinsam
// gesetzt und geloescht (`token_inv`), `reply` konsumiert es genau einmal, ein zweites `reply`
// findet nichts (`no_double_reply`), und `reply` bleibt vom Stilllegungstor unberuehrt
// (`reply_not_gated_by_quiescing`).
//
// **(4) Ausserhalb des Modells bleiben:** der Leichen-Zweig (`frame_of == None`), `purge_thread`,
// `owner_died`, `abort_call`, `retire_receiver`, `rebind_server`, `audit`, `Notification` und jede
// Nebenlaeufigkeit. Der Waechter misst fuer die ersten drei, dass die Entsprechung dort zerbricht
// (Gegenproben) — eine Luecke, die man sieht, ist keine Luecke, die man vergisst.
//
// **(5) Liveness ist keine Aussage dieses Modells.** Dass ein verworfener Sender **dauerhaft**
// blockiert bleibt (niemand weckt ihn je) ist eine Eigenschaft des Schedulers, nicht dieses
// Zustandsuebergangssystems. Sie ist gemessen, nicht bewiesen — s. `tools/verus-modelltreue-ipc.sh`.
//
// **Kernel-Quelltext und Modell:** bis zum 2026-08-03 war dieses Modell rein beschreibend — die
// Befunde standen hier, `crates/` blieb unberuehrt. Am 2026-08-04 hat sich das fuer **einen**
// Befund geaendert: D11 war kein Modellierungsspielraum, sondern ein Fehler (ein Faden hing
// dauerhaft, und jeder Pruefer meldete Ordnung). Der Code wurde behoben, und das Modell ist ihm
// gefolgt — nicht umgekehrt. Alle uebrigen Befunde stehen unveraendert als Beschreibung.
// ------------------------------------------------------------------------------------------------
use vstd::prelude::*;

verus! {

/// Ein Endpoint. Die ersten sechs Felder haben ein Gegenstueck in `sel4lake_ipc::Endpoint`, die
/// letzten drei sind Buchhaltung, die der Waechter effektbasiert fuellt.
pub struct Endpoint {
    /// Belegt (von `create` reserviert)? Sonst `ERR_BADCAP`.
    pub used: bool,
    /// Stillgelegt (A-4.2)? Dann keine NEUE Transaktion — `ERR_QUIESCING`.
    pub quiescing: bool,
    /// Blockierte Sender (Thread-IDs), FIFO.
    pub senders: Seq<nat>,
    /// Geparkte Empfaenger (Thread-IDs), FIFO.
    pub receivers: Seq<nat>,
    /// Der Aufrufer, der auf eine Antwort wartet (das Reply-Token).
    pub caller: Option<nat>,
    /// Der Server, der ihm die Antwort schuldet.
    pub reply_owner: Option<nat>,
    /// Kumulativ zugestellte Nachrichten.
    pub delivered: nat,
    /// Sender, die **mit einem Code abgewiesen** wurden, weil die Warteschlange voll war.
    /// Bis D11 hiess dieses Feld `rejected_senders` und zaehlte einen Verlust.
    pub rejected_senders: nat,
    /// Empfaenger, die abgewiesen wurden (dieselbe Schranke, andere Seite).
    pub rejected_receivers: nat,
}

/// **Die Warteschlangen-Schranke.** Muss mit `sel4lake_ipc::QUEUE_CAP` uebereinstimmen; der
/// Waechter prueft das, weil ein Modell mit der falschen Schranke genau den Fall verfehlt, um den
/// es hier geht.
pub open spec fn queue_cap() -> nat { 32 }

/// **Das Tor fuer eine NEUE Transaktion** (`CALL`/`RECV`): 0 = zulassen, sonst der Abweisungsgrund.
/// Die Zahlen sind abstrakt; entscheidend ist, dass die Gruende VERSCHIEDEN sind.
///
/// Dieses Tor haengt nur am **Zustand des Endpoints**. Der dritte Grund (voll) haengt zusaetzlich
/// an der Seite, von der aus gefragt wird — deshalb `send_gate`/`recv_gate` darunter und nicht ein
/// weiterer Zweig hier.
pub open spec fn gate(ep: Endpoint) -> nat {
    if !ep.used {
        1  // <-> ERR_BADCAP: "gibt es nicht"
    } else if ep.quiescing {
        2  // <-> ERR_QUIESCING: "kommt gleich wieder"
    } else {
        0
    }
}

/// **Das Tor aus Sicht eines Senders** (`CALL`): das Endpoint-Tor, und darueber hinaus die
/// Kapazitaetsschranke — aber nur, wenn kein Empfaenger wartet. Wartet einer, findet ein
/// Rendezvous statt und die Warteschlange wird gar nicht angefasst.
pub open spec fn send_gate(ep: Endpoint) -> nat {
    if gate(ep) != 0 {
        gate(ep)
    } else if ep.receivers.len() == 0 && ep.senders.len() >= queue_cap() {
        3  // <-> ERR_EP_FULL: "gerade kein Platz" (D11)
    } else {
        0
    }
}

/// **Das Tor aus Sicht eines Empfaengers** (`RECV`) — spiegelbildlich.
pub open spec fn recv_gate(ep: Endpoint) -> nat {
    if gate(ep) != 0 {
        gate(ep)
    } else if ep.senders.len() == 0 && ep.receivers.len() >= queue_cap() {
        3
    } else {
        0
    }
}

/// Gleichheit auf den **sechs Feldern mit Gegenstueck im Code** — die Buchhaltung bleibt aussen
/// vor. Damit laesst sich "die Operation hat den Endpoint nicht angefasst" sagen, ohne dass ein
/// hochgezaehlter Abweisungszaehler die Aussage kaputtmacht.
pub open spec fn core_eq(a: Endpoint, b: Endpoint) -> bool {
    a.used == b.used
        && a.quiescing == b.quiescing
        && a.senders == b.senders
        && a.receivers == b.receivers
        && a.caller == b.caller
        && a.reply_owner == b.reply_owner
}

/// **Die starke Rendezvous-Invariante:** nie gleichzeitig Sender UND Empfaenger blockiert. Sie
/// gilt am echten Endpoint NICHT (s. Kopf, Punkt 1) und wird deshalb nirgends als erhalten
/// behauptet — sie steht hier, weil ihr Bruch unten bewiesen wird.
pub open spec fn ep_inv_strong(ep: Endpoint) -> bool {
    ep.senders.len() == 0 || ep.receivers.len() == 0
}

/// **Die Invariante, die wirklich haelt:** ausserhalb einer Stilllegung ist nie ein Rendezvous
/// faellig. Waehrend einer Stilllegung darf beides zugleich anstehen — genau das brauchen
/// `bind_receiver` und `migrate_owner` (A-4.1/A-4.3).
pub open spec fn ep_inv(ep: Endpoint) -> bool {
    ep.quiescing || ep_inv_strong(ep)
}

/// **Token-Invariante:** `caller` und `reply_owner` werden stets gemeinsam gesetzt und geloescht.
/// Eine offene Antwortpflicht ohne Wartenden waere ein Token ohne Ziel; ein Wartender ohne
/// Antwortpflicht waere ein Client, dem niemand mehr schuldet.
pub open spec fn token_inv(ep: Endpoint) -> bool {
    (ep.caller is Some) == (ep.reply_owner is Some)
}

/// Gesamtzahl der Nachrichten, ueber die Buch gefuehrt wird: zugestellt + anstehend + **verworfen**.
/// Der dritte Summand ist der Unterschied zur alten Fassung: er macht den Verlust sichtbar, statt
/// ihn aus der Rechnung zu lassen.
pub open spec fn msgs_total(ep: Endpoint) -> nat {
    ep.delivered + ep.senders.len() + ep.rejected_senders
}

/// **send** (`CALL`): abgewiesen -> unveraendert; wartender Empfaenger -> Rendezvous, Token an den
/// Aufrufer; sonst einreihen. Ist kein Platz, wird **abgewiesen** (Code 3) statt eingereiht — und
/// der Abgewiesene blockiert nicht. Vor D11 stand hier das Gegenteil.
pub open spec fn send(ep: Endpoint, tid: nat) -> Endpoint {
    if gate(ep) != 0 {
        ep
    } else if send_gate(ep) != 0 {
        // Voll: der Endpoint bleibt unberuehrt, nur die Abweisung wird gezaehlt.
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders, receivers: ep.receivers,
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders + 1, rejected_receivers: ep.rejected_receivers,
        }
    } else if ep.receivers.len() > 0 {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders, receivers: ep.receivers.drop_first(),
            caller: Some(tid), reply_owner: Some(ep.receivers.first()),
            delivered: ep.delivered + 1,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    } else {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders.push(tid), receivers: ep.receivers,
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    }
}

/// **recv** (`RECV`): abgewiesen -> unveraendert; wartender Sender -> Rendezvous, Token an ihn
/// (das vorige wird dabei UEBERSCHRIEBEN, s. `recv_overwrites_token`); sonst parken. Ist kein
/// Platz, wird abgewiesen (Code 3) statt still verworfen.
pub open spec fn recv(ep: Endpoint, tid: nat) -> Endpoint {
    if gate(ep) != 0 {
        ep
    } else if recv_gate(ep) != 0 {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders, receivers: ep.receivers,
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers + 1,
        }
    } else if ep.senders.len() > 0 {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders.drop_first(), receivers: ep.receivers,
            caller: Some(ep.senders.first()), reply_owner: Some(tid),
            delivered: ep.delivered + 1,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    } else {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders, receivers: ep.receivers.push(tid),
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    }
}

/// **reply** (`REPLY`): das Token einmalig konsumieren. **Nicht** vom Stilllegungstor betroffen —
/// eine begonnene Transaktion darf abschliessen (A-4.2); nur `used` wird geprueft.
pub open spec fn reply(ep: Endpoint) -> Endpoint {
    if !ep.used {
        ep
    } else {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders, receivers: ep.receivers,
            caller: None, reply_owner: None,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    }
}

/// **bind_receiver** (A-4.1): eine Empfaenger-Instanz binden, ohne dass sie `RECV` ruft. Sieht die
/// Sender-Warteschlange NICHT an — daher der Bruch von `ep_inv_strong`. Ein Doppeleintrag wird
/// abgewiesen (das waere die Queue-Korruption, die `audit` meldet).
/// **Voll heisst hier: No-Op und `false` an den Aufrufer** (D11). Vorher meldete die Operation
/// `true`, waehrend der Eintrag verschwand — der Hot-Reload hielt die neue Instanz danach fuer
/// gebunden, obwohl der Endpoint sie nie gesehen hatte.
pub open spec fn bind_receiver(ep: Endpoint, tid: nat) -> Endpoint {
    if !ep.used || ep.receivers.contains(tid) || ep.receivers.len() >= queue_cap() {
        ep
    } else {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders, receivers: ep.receivers.push(tid),
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    }
}

/// **migrate_owner** (A-4.3): die Antwortpflicht des ausgetauschten Servers aufloesen, indem der
/// wartende Aufrufer wieder als **Sender** eingereiht wird. Sieht die Empfaenger-Warteschlange
/// nicht an — der zweite Weg zum Bruch von `ep_inv_strong`.
/// **Voll heisst hier: gar nichts tun** (D11) — und das ist die wichtigere Haelfte der Behebung.
/// Vorher wurde erst `caller` herausgenommen und `reply_owner` geloescht, dann eingereiht: schlug
/// das Einreihen fehl, war die Antwortpflicht weg **und** der Aufrufer in keiner Struktur mehr.
/// Er wartete auf eine Antwort, die niemand mehr schuldete, und die Operation meldete `true`.
/// Fail-closed heisst: die Antwortpflicht bleibt beim alten Besitzer stehen.
pub open spec fn migrate_owner(ep: Endpoint, old_owner: nat) -> Endpoint {
    if !ep.used || ep.reply_owner != Some(old_owner) || ep.caller is None
        || ep.senders.len() >= queue_cap() {
        ep
    } else {
        Endpoint {
            used: ep.used, quiescing: ep.quiescing,
            senders: ep.senders.push(ep.caller->Some_0), receivers: ep.receivers,
            caller: None, reply_owner: None,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    }
}

/// **begin_quiesce** (A-4.2): das Tor schliessen. Ein zweiter Aufruf ist wirkungslos — sonst
/// hielte ein zweiter Aufrufer den laufenden Austausch fuer seinen.
pub open spec fn begin_quiesce(ep: Endpoint) -> Endpoint {
    if !ep.used || ep.quiescing {
        ep
    } else {
        Endpoint {
            used: ep.used, quiescing: true,
            senders: ep.senders, receivers: ep.receivers,
            caller: ep.caller, reply_owner: ep.reply_owner,
            delivered: ep.delivered,
            rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
        }
    }
}

/// **end_quiesce**: das Tor wieder oeffnen. Erhaelt `ep_inv` NICHT (s. `end_quiesce_breaks_inv`).
pub open spec fn end_quiesce(ep: Endpoint) -> Endpoint {
    Endpoint {
        used: ep.used, quiescing: false,
        senders: ep.senders, receivers: ep.receivers,
        caller: ep.caller, reply_owner: ep.reply_owner,
        delivered: ep.delivered,
        rejected_senders: ep.rejected_senders, rejected_receivers: ep.rejected_receivers,
    }
}

// ===================== Bewiesene Eigenschaften =====================
//
// -- Die Rendezvous-Invariante ---------------------------------------------------------------

/// **BEWEIS:** `send` erhaelt `ep_inv`.
pub proof fn send_preserves_inv(ep: Endpoint, tid: nat)
    requires ep_inv(ep),
    ensures ep_inv(send(ep, tid)),
{
}

/// **BEWEIS:** `recv` erhaelt `ep_inv`.
pub proof fn recv_preserves_inv(ep: Endpoint, tid: nat)
    requires ep_inv(ep),
    ensures ep_inv(recv(ep, tid)),
{
}

/// **BEWEIS:** `reply` erhaelt `ep_inv` — es ruehrt keine der beiden Warteschlangen an.
pub proof fn reply_preserves_inv(ep: Endpoint)
    requires ep_inv(ep),
    ensures ep_inv(reply(ep)), reply(ep).senders == ep.senders, reply(ep).receivers == ep.receivers,
{
}

/// **BEWEIS (die Aufrufdisziplin als Vorbedingung, nicht als Kommentar):** `bind_receiver` erhaelt
/// `ep_inv` **nur** unter Stilllegung.
pub proof fn bind_receiver_keeps_inv_under_discipline(ep: Endpoint, tid: nat)
    requires ep.quiescing,
    ensures ep_inv(bind_receiver(ep, tid)),
{
}

/// **BEWEIS (die Gegenrichtung — die starke Invariante BRICHT):** ohne Stilllegung entsteht genau
/// der Zustand, den `ep_inv_strong` ausschliesst. Das ist kein Kommentar mehr, sondern eine
/// Zusicherung: waere die Operation je "repariert", schlaegt dieser Beweis fehl.
pub proof fn bind_receiver_breaks_strong_inv(ep: Endpoint, tid: nat)
    requires
        ep.used, ep_inv_strong(ep), ep.senders.len() > 0,
        ep.receivers.len() == 0, !ep.receivers.contains(tid),
    ensures !ep_inv_strong(bind_receiver(ep, tid)),
{
}

/// **BEWEIS:** `migrate_owner` erhaelt `ep_inv` nur unter Stilllegung.
pub proof fn migrate_owner_keeps_inv_under_discipline(ep: Endpoint, old_owner: nat)
    requires ep.quiescing,
    ensures ep_inv(migrate_owner(ep, old_owner)),
{
}

/// **BEWEIS:** `migrate_owner` bricht `ep_inv_strong` — der zweite Weg (A-4.3, Hot-Reload).
pub proof fn migrate_owner_breaks_strong_inv(ep: Endpoint, old_owner: nat)
    requires
        ep.used, ep_inv_strong(ep), ep.reply_owner == Some(old_owner), ep.caller is Some,
        ep.receivers.len() > 0, ep.senders.len() < queue_cap(),
    ensures !ep_inv_strong(migrate_owner(ep, old_owner)),
{
}

/// **BEWEIS (die andere Haelfte der Disziplin):** das Tor wieder zu oeffnen, waehrend ein
/// Rendezvous faellig ist, verletzt `ep_inv`. Wer nach `bind_receiver`/`migrate_owner`
/// `end_quiesce` ruft, ohne dass ein Austausch stattgefunden hat, hinterlaesst genau das.
pub proof fn end_quiesce_breaks_inv(ep: Endpoint)
    requires ep.quiescing, ep.senders.len() > 0, ep.receivers.len() > 0,
    ensures ep_inv(ep), !ep_inv(end_quiesce(ep)),
{
}

// -- Nachrichtenbuchhaltung und die Kapazitaetsschranke ----------------------------------------

/// **BEWEIS (unbedingte Buchhaltung):** jede Nachricht ist danach zugestellt ODER eingereiht ODER
/// als verworfen gezaehlt — nie zwei davon und nie keins. Das ist die schwaechere, aber
/// **unbedingt wahre** Aussage; sie sagt NICHTS darueber, ob die Nachricht ankommt.
pub proof fn send_accounts_every_message(ep: Endpoint, tid: nat)
    ensures
        gate(ep) == 0 ==> msgs_total(send(ep, tid)) == msgs_total(ep) + 1,
        gate(ep) != 0 ==> msgs_total(send(ep, tid)) == msgs_total(ep),
{
}

/// **BEWEIS (kein Verlust — UNTER der Schranke):** ist Platz (oder wartet ein Empfaenger), geht
/// nichts verloren. Die Vorbedingung ist der ganze Unterschied zur frueheren Fassung dieses
/// Beweises, die ohne sie am echten Endpoint schlicht falsch war.
pub proof fn send_no_loss(ep: Endpoint, tid: nat)
    requires
        ep_inv(ep), gate(ep) == 0,
        ep.receivers.len() > 0 || ep.senders.len() < queue_cap(),
    ensures
        msgs_total(send(ep, tid)) == msgs_total(ep) + 1,
        send(ep, tid).rejected_senders == ep.rejected_senders,
{
}

/// **BEWEIS (die ABWEISUNG — UEBER der Schranke, D11):** ab `queue_cap()` steht die Nachricht
/// danach **nicht** in der Warteschlange — aber der Endpoint bleibt vollstaendig unberuehrt
/// (`core_eq`), und der Sender bekommt Code 3.
///
/// Bis zum 2026-08-04 hiess dieser Beweis `send_drops_above_cap` und bewies den **Verlust**: der
/// Sender wurde blockiert, stand nirgends und wurde nie geweckt. Der Beweis war richtig — der Code
/// war falsch. Dass ein Beweis das Falsche beweisen kann, weil er dem Code treu ist, steht in
/// CLAUDE.md als eigene Falle.
pub proof fn send_rejects_above_cap(ep: Endpoint, tid: nat)
    requires gate(ep) == 0, ep.receivers.len() == 0, ep.senders.len() >= queue_cap(),
    ensures
        send_gate(ep) == 3,
        core_eq(send(ep, tid), ep),
        send(ep, tid).rejected_senders == ep.rejected_senders + 1,
        send(ep, tid).delivered == ep.delivered,
{
}

/// **BEWEIS:** dieselbe Schranke auf der Empfaengerseite — der 33. `RECV` parkt nicht, wird aber
/// abgewiesen statt vergessen.
pub proof fn recv_rejects_above_cap(ep: Endpoint, tid: nat)
    requires gate(ep) == 0, ep.senders.len() == 0, ep.receivers.len() >= queue_cap(),
    ensures
        recv_gate(ep) == 3,
        core_eq(recv(ep, tid), ep),
        recv(ep, tid).rejected_receivers == ep.rejected_receivers + 1,
{
}

/// **BEWEIS (D11, der Kern):** unter offenem Endpoint-Tor hat `send` genau ZWEI Ausgaenge — und
/// welcher es ist, sagt `send_gate`. Entweder der Sender ist zugestellt oder eingereiht (dann
/// steht er in einer Struktur des Endpoints und ist auffindbar), oder er ist mit einem Code
/// ungleich 0 abgewiesen (dann laeuft er weiter).
///
/// Der dritte Ausgang — weder eingereiht noch abgewiesen — war D11: der Faden hing dauerhaft,
/// `is_quiescent()` meldete ihn als ruhig, `audit` und `purge_thread` sahen ihn nicht. Dass es
/// ihn nicht mehr gibt, ist hier die Vollstaendigkeit der Fallunterscheidung; dass "abgewiesen"
/// auch "nicht blockiert" heisst, misst der Waechter.
pub proof fn send_never_strands(ep: Endpoint, tid: nat)
    requires gate(ep) == 0,
    ensures
        send_gate(ep) == 0 ==> send(ep, tid).delivered == ep.delivered + 1
            || send(ep, tid).senders == ep.senders.push(tid),
        send_gate(ep) != 0 ==> core_eq(send(ep, tid), ep)
            && send(ep, tid).rejected_senders == ep.rejected_senders + 1,
{
}

/// **BEWEIS:** dasselbe fuer den Empfaenger. Ein Server, dem das zustiesse, waere schlimmer als
/// ein Client — er wartet auf Arbeit, die ihm niemand mehr geben kann.
pub proof fn recv_never_strands(ep: Endpoint, tid: nat)
    requires gate(ep) == 0,
    ensures
        recv_gate(ep) == 0 ==> recv(ep, tid).delivered == ep.delivered + 1
            || recv(ep, tid).receivers == ep.receivers.push(tid),
        recv_gate(ep) != 0 ==> core_eq(recv(ep, tid), ep)
            && recv(ep, tid).rejected_receivers == ep.rejected_receivers + 1,
{
}

/// **BEWEIS (D11, die stille Erfolgsmeldung):** ist die Empfaengerqueue voll, ist `bind_receiver`
/// ein reines No-Op. Vorher aenderte es ebenfalls nichts — meldete aber Erfolg.
pub proof fn bind_receiver_full_is_noop(ep: Endpoint, tid: nat)
    requires ep.receivers.len() >= queue_cap(),
    ensures bind_receiver(ep, tid) == ep,
{
}

/// **BEWEIS (D11, der gefaehrlichste der vier):** ist die Senderqueue voll, laesst
/// `migrate_owner` die Antwortpflicht **stehen**. Vorher loeschte es sie und verlor den Aufrufer:
/// ein Client, der auf eine Antwort wartet, die niemand mehr schuldet.
pub proof fn migrate_owner_full_keeps_token(ep: Endpoint, old_owner: nat)
    requires ep.senders.len() >= queue_cap(),
    ensures
        migrate_owner(ep, old_owner) == ep,
        ep.caller is Some ==> migrate_owner(ep, old_owner).caller == ep.caller,
{
}

/// **BEWEIS (recv stellt genau einmal zu):** wartet ein Sender, sinkt die Zahl der anstehenden um
/// 1, `delivered` steigt um 1, und der Aufrufer steht danach **nicht mehr** in der Warteschlange,
/// sondern haelt das Token.
pub proof fn recv_delivers_once(ep: Endpoint, tid: nat)
    requires ep_inv(ep), gate(ep) == 0,
    ensures
        ep.senders.len() > 0 ==> msgs_total(recv(ep, tid)) == msgs_total(ep)
            && recv(ep, tid).delivered == ep.delivered + 1
            && recv(ep, tid).senders == ep.senders.drop_first()
            && recv(ep, tid).caller == Some(ep.senders.first())
            && recv(ep, tid).reply_owner == Some(tid),
        ep.senders.len() == 0 ==> recv(ep, tid).delivered == ep.delivered,
{
}

/// **BEWEIS (Rendezvous-Fortschritt):** ist ein Partner da und das Tor offen, wird **sofort**
/// zugestellt — kein Deadlock bei vorhandenem Partner.
pub proof fn rendezvous_progress(ep: Endpoint, msg: nat, tid: nat)
    requires ep_inv(ep), gate(ep) == 0,
    ensures
        ep.receivers.len() > 0 ==> send(ep, msg).delivered == ep.delivered + 1,
        ep.senders.len() > 0 ==> recv(ep, tid).delivered == ep.delivered + 1,
{
}

// -- Das Stilllegungstor (A-4.2) ---------------------------------------------------------------

/// **BEWEIS:** eine Abweisung ist **wirkungslos** — der Zustand aendert sich nicht. Ein Client, der
/// `ERR_QUIESCING` bekommt, hat nichts hinterlassen, das aufgeraeumt werden muesste.
pub proof fn gate_rejects_are_noops(ep: Endpoint, tid: nat)
    requires gate(ep) != 0,
    ensures send(ep, tid) == ep, recv(ep, tid) == ep,
{
}

/// **BEWEIS (der Kern von A-4.2):** die beiden Abweisungsgruende sind UNTERSCHEIDBAR. "Gibt es
/// nicht" und "kommt gleich wieder" verlangen vom Client verschiedene Reaktionen; denselben Code
/// zu liefern hiesse, ihm diesen Unterschied zu verschweigen.
pub proof fn gate_distinguishes(a: Endpoint, b: Endpoint)
    requires !a.used, b.used, b.quiescing,
    ensures gate(a) != 0, gate(b) != 0, gate(a) != gate(b),
{
}

/// **BEWEIS (D11 setzt A-4.2 fort):** es sind DREI unterscheidbare Gruende, nicht zwei. "Gibt es
/// nicht" (nie wieder versuchen), "kommt gleich wieder" (nach dem Austausch wieder versuchen) und
/// "gerade kein Platz" (haengt an den anderen Wartenden, nicht am Endpoint). Wer den dritten mit
/// dem zweiten zusammenwuerfe, naehme dem Client genau die Unterscheidung, die A-4.2 eingefuehrt
/// hat — und wer ihn gar nicht gaebe, haette D11.
pub proof fn gate_three_reasons_distinct(a: Endpoint, b: Endpoint, c: Endpoint)
    requires
        !a.used,
        b.used, b.quiescing,
        c.used, !c.quiescing, c.receivers.len() == 0, c.senders.len() >= queue_cap(),
    ensures
        send_gate(a) == 1, send_gate(b) == 2, send_gate(c) == 3,
        send_gate(a) != send_gate(b),
        send_gate(b) != send_gate(c),
        send_gate(a) != send_gate(c),
{
}

// -- Das Reply-Token --------------------------------------------------------------------------

/// **BEWEIS:** `send` erhaelt `token_inv`.
pub proof fn send_preserves_token_inv(ep: Endpoint, tid: nat)
    requires token_inv(ep),
    ensures token_inv(send(ep, tid)),
{
}

/// **BEWEIS:** `recv` erhaelt `token_inv`.
pub proof fn recv_preserves_token_inv(ep: Endpoint, tid: nat)
    requires token_inv(ep),
    ensures token_inv(recv(ep, tid)),
{
}

/// **BEWEIS:** `reply` erhaelt `token_inv` — beide Haelften fallen gemeinsam weg.
pub proof fn reply_preserves_token_inv(ep: Endpoint)
    requires token_inv(ep),
    ensures token_inv(reply(ep)),
{
}

/// **BEWEIS:** `migrate_owner` erhaelt `token_inv` — auch der Hot-Reload-Weg laesst keine halbe
/// Antwortpflicht stehen.
pub proof fn migrate_owner_preserves_token_inv(ep: Endpoint, old_owner: nat)
    requires token_inv(ep),
    ensures token_inv(migrate_owner(ep, old_owner)),
{
}

/// **BEWEIS:** `reply` konsumiert das Token — danach schuldet niemand mehr eine Antwort.
pub proof fn reply_consumes_token(ep: Endpoint)
    requires ep.used,
    ensures reply(ep).caller is None, reply(ep).reply_owner is None,
{
}

/// **BEWEIS (kein Doppel-Reply):** ein zweites `REPLY` findet kein Token und hat keine Wirkung.
pub proof fn no_double_reply(ep: Endpoint)
    ensures reply(reply(ep)) == reply(ep), ep.used ==> reply(ep).caller is None,
{
}

/// **BEWEIS (A-4.2, die Asymmetrie):** stillgelegt heisst *keine neue* Transaktion, nicht *keine*
/// Transaktion. `CALL`/`RECV` werden abgewiesen, `REPLY` wirkt weiter — sonst bliebe ein Aufrufer
/// genau deshalb haengen, weil der Endpoint stillgelegt wurde.
pub proof fn reply_not_gated_by_quiescing(ep: Endpoint)
    requires ep.used, ep.quiescing, ep.caller is Some,
    ensures gate(ep) != 0, reply(ep).caller is None,
{
}

/// **KEIN Beweis einer guten Eigenschaft, sondern der Beweis einer schlechten:** ein zweites
/// `RECV` desselben Servers, bevor er geantwortet hat, **ueberschreibt** das Token. Der vorige
/// Aufrufer verliert damit seinen Anspruch — er steht in keiner Warteschlange mehr und haelt kein
/// Token; niemand kann ihn noch wecken. Das steht hier, damit es nicht stillschweigend
/// verschwindet: waere es behoben, schluege dieser Beweis fehl.
pub proof fn recv_overwrites_token(ep: Endpoint, tid: nat)
    requires
        gate(ep) == 0, ep.senders.len() > 0, ep.caller is Some,
        ep.caller != Some(ep.senders.first()),
    ensures
        recv(ep, tid).caller == Some(ep.senders.first()),
        recv(ep, tid).caller != ep.caller,
{
}

fn main() {}

} // verus!
