//! **Z4b: was darf mitwandern, und was auf keinen Fall** — die externe Darstellung einer Cap.
//!
//! Die Todo-Notiz nennt diese Stelle „die gefährlichste des ganzen Vorhabens", und sie hat recht.
//! Eine Cap ist im Betrieb ein **Index** in eine globale Tabelle plus eine CDT-Kante. Über
//! Maschinengrenzen bedeutet ein Index nichts — er bezeichnet auf der Zielmaschine *irgendetwas*,
//! und zwar mit vollen Rechten. Ein Checkpoint, der Indizes mitnimmt, ist keine Migration, sondern
//! eine Rechteausweitung mit Reisepass.
//!
//! ## Die Regel, in einem Satz
//!
//! **Was auf der Zielmaschine nicht dasselbe bezeichnen kann, wandert nicht mit — es wird
//! verweigert, nicht ersetzt.** Der bequeme Weg wäre, eine MMIO-Cap „auf das entsprechende Gerät
//! drüben" abzubilden. Es gibt kein entsprechendes Gerät; es gibt ein anderes.
//!
//! ## Warum die Entscheidung einen *Umfang* braucht
//!
//! Bei `Mmio`, `Irq`, `Dma` ist die Antwort für sich genommen klar. Bei einem **Endpoint** ist sie
//! es nicht: wandert der Partner mit, ist die Cap sinnvoll übertragbar; bleibt er zurück, zeigt
//! sie ins Leere — und ein Thread, der an einem toten Endpoint hängt, ist schlimmer als einer, der
//! gar nicht erst gestartet ist. Dieselbe Cap ist also mal übertragbar und mal nicht, und was
//! zutrifft, hängt am **Umfang des Checkpoints** ([`Scope`]), nicht an der Cap.
//!
//! Genau deshalb nimmt [`classify`] den Umfang als Argument. Eine Klassifikation ohne ihn wäre
//! kürzer und in der Hälfte der Fälle falsch.
//!
//! ## Was hier NICHT entschieden wird
//!
//! Ob der Zielrechner **taugt** (gleiche Architektur, gleiche Kernelversion, gleiche
//! Zeitquelle) — das ist Z4f und gehört nicht in eine Klassifikation je Cap. Die Verbindung
//! zwischen beiden ist [`ExternKind::SchedContext`]: seine Zahlen sind **Ticks**, und ein Tick
//! bedeutet auf einer Maschine ohne invarianten Zähler etwas anderes. Die Cap ist der Form nach
//! übertragbar und trägt deshalb eine **Vorbedingung** mit, statt sie zu verschweigen.

use crate::object::ObjectKind;

/// **Der Umfang eines Checkpoints**: was wandert mit?
///
/// Ohne diese Angabe lässt sich über Endpoints und Notifications nichts sagen (s. Moduldoku). Die
/// Listen enthalten die **IDs dieser Maschine** — sie werden nur zum *Entscheiden* gebraucht und
/// gehen nicht in die externe Darstellung ein.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    /// Endpoints, deren beide Seiten Teil des Checkpoints sind.
    ///
    /// **Bis 2026-08-25 war dieser Satz eine BEHAUPTUNG des Aufrufers** und wurde nie gegen den
    /// IPC-Zustand der Maschine gehalten: [`classify`] hat die Zahl gelesen und die Cap
    /// durchgelassen, und ein an diesem Endpoint blockierter Client, der zurueckbleibt, kam
    /// nirgends vor. Seit Z4d Stufe 1 prueft [`classify_cut`] ihn — die Liste sagt weiterhin, was
    /// mitwandern *soll*, und die beobachteten [`Edge`]s sagen, wer wirklich dort steht.
    pub endpoints: &'a [u32],
    /// Notifications, deren Signalgeber und Empfänger mitwandern. Dieselbe Pruefung wie bei
    /// [`Self::endpoints`], und **ein eigener Namensraum**: eine 3 hier deckt keine 3 dort.
    pub notifications: &'a [u32],
    /// Threads (gepacktes `ThreadId`-Raw), die mitwandern.
    pub threads: &'a [u64],
    /// PDs, die mitwandern.
    pub pds: &'a [u32],
}

impl Scope<'_> {
    /// Der leere Umfang: **nichts** wandert mit außer dem Thread selbst.
    ///
    /// Das ist die Stufe-1-Wahl aus Z4d („Migration nur ohne offene Transaktionen — einfach,
    /// ehrlich, wahrscheinlich richtig") und zugleich die sicherste Vorgabe: mit leerem Umfang
    /// verweigert [`classify`] jede Beziehungs-Cap, statt sie ins Leere zeigen zu lassen.
    ///
    /// **Und genau das hat die halbe Regel jahrelang verdeckt.** Weil hier nichts drinsteht, war
    /// „ein Endpoint im Umfang, dessen Partner draussen bleibt" von der einzigen Aufrufstelle aus
    /// gar nicht erreichbar — der ungeprueft gebliebene Fall sah aus wie ein Fall, den es nicht
    /// gibt. Er entsteht in dem Moment, in dem ein Umfang seinen ersten Endpoint nennt.
    ///
    /// **Das Subjekt steht NICHT in `threads`**, sondern wird [`classify_cut`] getrennt genannt.
    /// Ein Aufrufer darf es trotzdem eintragen (die Aufrufstelle im Kernel tut es), damit die
    /// Menge „wer wandert" an einer Stelle vollstaendig zu lesen ist.
    pub const EMPTY: Scope<'static> = Scope {
        endpoints: &[],
        notifications: &[],
        threads: &[],
        pds: &[],
    };
}

/// **Warum eine Cap nicht mitwandern darf.**
///
/// Bewusst je Grund ein eigener Wert und kein Sammel-`false`: „das Gerät gibt es dort nicht" und
/// „der Partner bleibt zurück" sind verschiedene Befunde, und nur der zweite lässt sich durch
/// einen größeren Umfang beheben. Ein Aufrufer, der beide gleich behandelt, sucht an der falschen
/// Stelle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocalReason {
    /// Ein Registerfenster **dieser** Maschine. Auf der Zielmaschine liegt an derselben
    /// Physadresse ein anderes Gerät oder gar keines.
    DeviceWindow,
    /// Eine Interruptnummer **dieses** Controllers.
    InterruptLine,
    /// Eine DMA-Region, die an **diesen** Übersetzungskontext angehängt ist. Sie mitzunehmen
    /// hieße, die Isolationszusage aus A-5.4 an der Maschinengrenze fallen zu lassen.
    DmaRegion,
    /// Eine Reply-Cap bezeichnet **einen konkreten blockierten Aufrufer**. Er bleibt hier.
    PendingReply,
    /// Eine **Handler-Bindung** (`SyscallHandler`/`FaultHandler`, Z26/A3): das Sidecar-Fenster
    /// hinter ihr trägt die halben Trap-Frames konkreter blockierter Gäste — und ein halber
    /// Syscall ist auf der Zielmaschine kein halber Syscall, sondern **Datenmüll in
    /// Registerform** (der Frame ist architekturspezifisch: x86_64 22 Wörter, aarch64 34).
    ///
    /// **Bis zum 2026-08-13 trug dieser Fall den Grund [`Self::PendingReply`]** — eine benannte
    /// Ungenauigkeit: der Refusal-*Grund* stimmte, die *Cap-Art* nicht. Sie stand deshalb, weil
    /// ein neuer `LocalReason` den erschöpfenden `match` in `ckpt_reason`
    /// (`kernel/src/arch/x86_64/bringup.rs`) bricht — genau das ist der Sinn eines erschöpfenden
    /// `match`, und er hat gewirkt: die Zeile dort ist jetzt nachgetragen.
    ///
    /// Der eigene Name ist keine Kosmetik: die Verweigerung ist **nicht behebbar**, und
    /// `PendingReply` liest sich als „lass den Aufrufer antworten, dann geht es". Bei einer
    /// Handler-Bindung hilft das nichts — sie bleibt auch dann verweigert, wenn Endpoint und
    /// Gäste vollständig im Umfang liegen.
    HandlerBinding,
    /// Der Partner (Endpoint/Notification) ist **nicht** Teil des Checkpoints — die Cap zeigte
    /// nach dem Transfer ins Leere. Behebbar: den Partner in den Umfang aufnehmen.
    PeerNotInScope,
    /// Die Cap bezeichnet einen **anderen Thread**, der nicht mitwandert.
    ThreadNotInScope,
    /// Die Cap steuert eine **andere PD**, die nicht mitwandert.
    PdNotInScope,
    /// Eine Loader-Cap zeigt auf die Startmenge **dieser** Maschine. Dieselbe Quellennummer
    /// bezeichnet dort ein anderes Archiv — und damit andere Programme.
    LoaderSource,
    /// **Debug-Autoritaet wandert nicht mit** (Z6b).
    ///
    /// Eigener Grund und nicht `PdNotInScope`, obwohl beides PDs bezeichnet: „nimm die PD in den
    /// Umfang auf" waere hier die **falsche Behebung**. Die Verweigerung haengt nicht am Umfang,
    /// sondern daran, dass die Praegung eine benannte, protokollierte, beim Mandanten sichtbare
    /// Handlung auf **dieser** Maschine war. Sie ueber eine Maschinengrenze zu tragen hiesse, dass
    /// drueben jemand Debug-Autoritaet ueber eine PD haelt, ohne dass sie dort je gepraegt wurde —
    /// womit die Zusage aus Z6b §0 auf der Zielmaschine schlicht nicht mehr gilt.
    ///
    /// Der Umfang wird deshalb gar nicht erst befragt, wie bei `HandlerBinding` und anders als bei
    /// `PdControl`: hier waere auch der geprüfte Grund der falsche.
    DebugAuthority,
    /// **Eine Zahl DIESER Maschine** (Z23/S4): eine Kernnummer, ein Zyklenstempel.
    ///
    /// Sie zeigt drueben auf einen anderen Kern oder auf gar keinen, und ein Zyklenwert gehoert zu
    /// **diesem** Zaehler. Eigener Grund und nicht `DeviceWindow`, obwohl beides „maschinenlokal"
    /// heisst: ein Geraetefenster ist drueben **nicht herstellbar**, eine Kernaffinitaet dagegen
    /// **entsteht drueben neu**. Wer die beiden gleich behandelt, sucht an der falschen Stelle —
    /// dieselbe Begruendung, aus der `LocalReason` ueberhaupt aufgeschluesselt ist.
    MachineLocalNumber,
}

/// Eine Vorbedingung, die auf der **Zielmaschine** gelten muss (Z4f).
///
/// Sie steht hier und nicht in einer Prüfung, weil sie nicht je Cap entscheidbar ist: ob die
/// Zielmaschine dieselbe Zeitsemantik hat, weiß nur der, der beide kennt. Verschweigen wäre die
/// Alternative — und dann wanderte ein Thread mit einem Budget, das drüben etwas anderes bedeutet,
/// und die Abrechnung würde **still** falsch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Precondition {
    /// Die Zielmaschine muss dieselbe Tick-Semantik haben (Z4f/B-5.1).
    SameTickSemantics,
}

/// **Die externe Darstellung** einer übertragbaren Cap — ohne eine einzige maschinenlokale Zahl.
///
/// Kein Tabellenindex, keine Physadresse, keine Endpoint-ID. Was hier steht, hat auf jeder Maschine
/// dieselbe Bedeutung; alles andere ist gar nicht erst drin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExternKind {
    /// Speicher — **als Inhalt**, nicht als Adresse. Die Zielmaschine legt ihn irgendwo hin;
    /// insbesondere färbt sie neu (Farbe ist maschinenlokal, s. Z4c — ein Argument dafür, sie nie
    /// in die ABI zu heben).
    Region { len: u64 },
    /// Ein Endpoint, bezeichnet über seinen **Platz im Checkpoint**, nicht über eine ID.
    Endpoint { scope_index: u32 },
    /// Eine Notification, ebenso.
    Notification { scope_index: u32 },
    /// Ein Thread des Checkpoints, ebenso.
    Tcb { scope_index: u32 },
    /// Ein CPU-Budget. Die Zahlen sind **Ticks** — s. [`Precondition::SameTickSemantics`].
    SchedContext { budget: u32, period: u32 },
}

/// Das Ergebnis der Klassifikation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transfer {
    /// Darf mitwandern. `precondition` ist `Some`, wenn die Zielmaschine etwas zusichern muss.
    Portable {
        kind: ExternKind,
        precondition: Option<Precondition>,
    },
    /// Darf **nicht** mitwandern, mit Grund.
    Refused(LocalReason),
}

impl Transfer {
    /// Kurzform für Aufrufer, die nur die Richtung brauchen.
    pub fn is_portable(&self) -> bool {
        matches!(self, Transfer::Portable { .. })
    }
}

fn position(haystack: &[u32], needle: u32) -> Option<u32> {
    haystack.iter().position(|&x| x == needle).map(|i| i as u32)
}

/// **Darf diese Cap mitwandern?** — die eine Regel, an der Z4b hängt.
///
/// `scope` sagt, was sonst noch Teil des Checkpoints ist; ohne ihn ist über Beziehungs-Caps nichts
/// zu entscheiden (s. Moduldoku).
///
/// **Fail-closed:** was hier nicht ausdrücklich als übertragbar aufgeführt ist, wird verweigert.
/// Ein neuer Objekttyp, den niemand betrachtet hat, wandert damit **nicht** mit — statt
/// stillschweigend durchzurutschen, weil er in keiner Ablehnungsliste stand. Die Richtung ist
/// dieselbe wie bei `has_unknown_authority` im Manifest: ein unbekanntes Bit ist ein Grund
/// abzulehnen, kein Grund zu ignorieren.
pub fn classify(kind: &ObjectKind, scope: &Scope) -> Transfer {
    match *kind {
        ObjectKind::Memory(r) => Transfer::Portable {
            kind: ExternKind::Region { len: r.len },
            precondition: None,
        },
        ObjectKind::SchedContext { budget, period } => Transfer::Portable {
            kind: ExternKind::SchedContext { budget, period },
            precondition: Some(Precondition::SameTickSemantics),
        },
        ObjectKind::Endpoint(id) => match position(scope.endpoints, id) {
            Some(i) => Transfer::Portable {
                kind: ExternKind::Endpoint { scope_index: i },
                precondition: None,
            },
            None => Transfer::Refused(LocalReason::PeerNotInScope),
        },
        ObjectKind::Notification(id) => match position(scope.notifications, id) {
            Some(i) => Transfer::Portable {
                kind: ExternKind::Notification { scope_index: i },
                precondition: None,
            },
            None => Transfer::Refused(LocalReason::PeerNotInScope),
        },
        ObjectKind::Tcb(raw) => match scope.threads.iter().position(|&x| x == raw) {
            Some(i) => Transfer::Portable {
                kind: ExternKind::Tcb { scope_index: i as u32 },
                precondition: None,
            },
            None => Transfer::Refused(LocalReason::ThreadNotInScope),
        },
        ObjectKind::PdControl { pd } => match position(scope.pds, pd) {
            // **Auch im Umfang bleibt sie verweigert.** Eine PdControl-Cap ist die Autorität über
            // den Lebenszyklus einer PD; sie über eine Maschinengrenze zu tragen hiesse, dass ein
            // wandernder Thread drueben eine PD steuern darf, die dort neu entsteht. Das mag
            // richtig sein -- entschieden ist es nicht, und bis dahin ist Verweigern die
            // ehrliche Antwort. Der Umfang wird trotzdem geprueft, damit der GRUND stimmt.
            Some(_) | None => Transfer::Refused(LocalReason::PdNotInScope),
        },
        ObjectKind::Reply { .. } => Transfer::Refused(LocalReason::PendingReply),
        ObjectKind::Mmio { .. } => Transfer::Refused(LocalReason::DeviceWindow),
        ObjectKind::Irq { .. } => Transfer::Refused(LocalReason::InterruptLine),
        ObjectKind::Dma { .. } => Transfer::Refused(LocalReason::DmaRegion),
        ObjectKind::Loader { .. } => Transfer::Refused(LocalReason::LoaderSource),
        // Z26/A3. **Auch wenn Endpoint und Gäste im Umfang lägen**, bleibt sie verweigert: das
        // Sidecar enthält halbe Trap-Frames, und ein halber Syscall ist auf der Zielmaschine kein
        // halber Syscall, sondern Datenmüll in Registerform. Der Umfang wird deshalb gar nicht
        // erst befragt — anders als bei `PdControl`, wo er geprüft wird, damit der GRUND stimmt;
        // hier wäre auch der geprüfte Grund der falsche.
        //
        // Seit 2026-08-13 mit **eigenem** Grund. Vorher stand hier `PendingReply` — der
        // Refusal-GRUND stimmte, die Cap-Art nicht; und der falsche Name führt zur falschen
        // Behebung („lass den Aufrufer antworten"), die es hier nicht gibt.
        ObjectKind::SyscallHandler { .. } | ObjectKind::FaultHandler { .. } => {
            Transfer::Refused(LocalReason::HandlerBinding)
        }
        // Z6b. Fail-closed, mit eigenem Grund — s. `LocalReason::DebugAuthority`.
        ObjectKind::Debuggable { .. } => Transfer::Refused(LocalReason::DebugAuthority),
    }
}

/// **Darf der ganze Cspace mitwandern?** `Ok(())` oder der erste Grund, der dagegen spricht.
///
/// Gibt den **Slot** mit zurück: „irgendeine Cap ist nicht übertragbar" ist als Diagnose wertlos,
/// wenn ein Cspace dreißig Einträge hat.
pub fn classify_all(
    kinds: &[Option<ObjectKind>],
    scope: &Scope,
) -> Result<(), (usize, LocalReason)> {
    for (slot, k) in kinds.iter().enumerate() {
        if let Some(k) = k {
            if let Transfer::Refused(r) = classify(k, scope) {
                return Err((slot, r));
            }
        }
    }
    Ok(())
}

// ================================================================================================
// Z4d stage 1: an open transaction must not cross the cut
// ================================================================================================
//
// [`classify`] decides per CAP. For the question Z4d asks that is not enough, and the gap is not
// academic. [`Scope::endpoints`] is documented as "endpoints whose BOTH sides are part of the
// checkpoint" — but it is a list the caller wrote down, and until now nothing ever held it against
// the IPC state the machine is actually in. A named endpoint was **believed**; a client blocked at
// it who stays behind was never looked at.
//
// That is this repository's oldest failure shape in a new place: `ep_inv` "hielt nicht der Typ,
// sondern die Aufrufdisziplin", and a guard "prueft die EXISTENZ eines Grundes, nie seine
// WAHRHEIT". `Scope::EMPTY` hid it — with an empty scope every relationship cap is refused, so the
// unchecked half was unreachable from the only call site. An unreachable hole is still a hole, and
// the moment a scope names its first endpoint it becomes a reachable one.
//
// ## The rule, and it is an EQUIVALENCE
//
// For every role a thread actually holds at a channel:
//
// ```text
//     the channel migrates   <=>   the thread holding that role migrates
// ```
//
// Both directions are refusals, and they are **different** refusals — the same reason `LocalReason`
// is broken up at all. "The participant migrates, his channel stays" is Z4d verbatim: a thread with
// an open `CALL` leaves a waiting server behind. "The channel migrates, this participant stays" is
// the other half, and it is the one nobody had written down: the endpoint travels, and the client
// blocked at it waits forever on a rendezvous point that is no longer on this machine.
//
// **No role is exempt, and that is deliberate.** A thread waiting in `RECV` has no partner
// ([`Endpoint::partner_of`](../../caprock_ipc/struct.Endpoint.html#method.partner_of) returns
// `None` for it, and inventing one would be worse than naming none) — so the tempting reading is
// that letting it travel harms nobody. It harms itself: after the move it waits on a channel it no
// longer has, and no wakeup can ever reach it. Same for the mirror case. One rule, no exceptions,
// no list of special cases to keep in step.
//
// ## Why the edges are a FINDING and not an argument
//
// An [`Edge`] is what the kernel **observed** at a channel, not what the caller intends. That
// separation is the whole point: the scope is the claim, the edges are the measurement, and this
// module holds one against the other. A caller who could pass the edges he wishes for would be
// back at believing his own list.

/// A channel of this machine — the two kinds a thread can block on.
///
/// The id is a **local** table index and never enters the external representation; like the lists
/// in [`Scope`] it exists only to decide. What crosses the machine boundary is
/// [`ExternKind::Endpoint`] with its `scope_index`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    /// An endpoint (synchronous rendezvous).
    Endpoint(u32),
    /// A notification (asynchronous badge).
    Notification(u32),
}

/// The role a thread holds at a channel — the four of [`crate::checkpoint`]'s counterpart
/// `Quiescence`, spelled out one per edge instead of merged into a bit set.
///
/// One edge per role, not one edge per thread: a refusal that says *which* role crosses the cut is
/// a diagnosis, and one that says "something crosses" is not. The rule below does not read this
/// field — it decides on the two memberships alone — but the refusal names the edge, and the edge
/// names the role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeRole {
    /// Blocked sender: has issued `CALL`, no server has taken it yet.
    Sender,
    /// Blocked receiver: waiting in `RECV` (or in `WAIT` at a notification).
    Receiver,
    /// Waiting as a caller for a reply — the reply token points at him.
    Caller,
    /// Owes a reply as a server.
    ReplyOwner,
}

/// **One observed role at one channel.** The measurement the cut rule judges.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    /// Where the role is held.
    pub channel: Channel,
    /// Who holds it — a packed `ThreadId` raw value, the same encoding as [`Scope::threads`].
    pub thread: u64,
    /// Which of the four roles.
    pub role: EdgeRole,
}

/// **Why an open relationship forbids this cut** (Z4d stage 1).
///
/// Two reasons and not one `false`, because they call for opposite fixes: the first is repaired by
/// taking the channel into the scope, the second by taking the peer into it — or by waiting for his
/// transaction to finish. A caller who treats them alike looks in the wrong place, which is exactly
/// why [`LocalReason`] is broken up too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CutRefusal {
    /// **The participant migrates, his channel stays behind.** Z4d verbatim: a thread with an open
    /// `CALL` leaves a waiting server, a thread in `RECV` leaves itself waiting on nothing.
    ChannelNotInScope,
    /// **The channel migrates, this participant stays behind.** The half that had no name: the
    /// rendezvous point leaves the machine and whoever blocks at it never hears from it again.
    PeerNotInScope,
}

/// Does this thread travel? The subject travels by definition — everything else must be listed.
fn thread_in_scope(thread: u64, subject: u64, scope: &Scope) -> bool {
    thread == subject || scope.threads.contains(&thread)
}

/// Does this channel travel?
fn channel_in_scope(channel: Channel, scope: &Scope) -> bool {
    match channel {
        Channel::Endpoint(id) => scope.endpoints.contains(&id),
        Channel::Notification(id) => scope.notifications.contains(&id),
    }
}

/// **Does this one observed role survive the cut?** `None` means it does.
///
/// The fourth case — neither end travels — is **not** a refusal. Two strangers holding a
/// transaction with each other are none of this checkpoint's business, and refusing them would make
/// every checkpoint on a busy machine impossible. The kernel does not normally hand such edges in;
/// the rule stays total anyway, because a rule with an unhandled case is decided by whoever calls
/// it.
pub fn classify_edge(edge: &Edge, subject: u64, scope: &Scope) -> Option<CutRefusal> {
    match (
        channel_in_scope(edge.channel, scope),
        thread_in_scope(edge.thread, subject, scope),
    ) {
        (true, true) | (false, false) => None,
        (false, true) => Some(CutRefusal::ChannelNotInScope),
        (true, false) => Some(CutRefusal::PeerNotInScope),
    }
}

/// **Is the cut clean?** `Ok(())`, or the first edge that crosses it and why.
///
/// The **index** comes back with the reason for the same reason [`classify_all`] returns the slot:
/// "some relationship crosses the cut" is worthless as a diagnosis, and the index names channel,
/// thread and role at once.
pub fn classify_cut(
    subject: u64,
    edges: &[Edge],
    scope: &Scope,
) -> Result<(), (usize, CutRefusal)> {
    for (i, e) in edges.iter().enumerate() {
        if let Some(r) = classify_edge(e, subject, scope) {
            return Err((i, r));
        }
    }
    Ok(())
}

/// **Why no checkpoint was built.**
///
/// Two shapes and not one, because a cap refusal and a cut refusal are answered differently: the
/// first is a property of the Cspace and does not change by waiting, the second may dissolve on its
/// own as soon as the transaction completes. Flattening them would tell a caller to wait for
/// something that never moves, or to give up on something that would have been ready in a tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BuildRefusal {
    /// A cap of the Cspace must not travel — slot and reason (Z4b).
    Cap(usize, LocalReason),
    /// An open IPC relationship crosses the cut — edge index and reason (Z4d stage 1).
    Cut(usize, CutRefusal),
}

// ================================================================================================
// Z4 Stufe 2: die **äussere Darstellung** — ein Checkpoint als Bytefolge
// ================================================================================================
//
// Bis hierher war Z4b eine Regel ohne Erzeugnis: [`classify`] sagt, was mitwandern *darf*, und
// [`ExternKind`] beschreibt, wie es drüben heisst. Was fehlte, war das Ding selbst — eine Folge
// von Bytes, die eine Maschine schreibt und eine andere liest.
//
// ## Warum das Format hier steht und nicht im Kern
//
// Aus demselben Grund wie `caprock-part` und `caprock-fat`: **fremde Bytes werden nirgends mit
// Kernprivileg interpretiert.** Ein Checkpoint kommt von einer Platte, später über ein Netz; er
// ist Eingabe, nicht Zustand. Der Parser ist deshalb abhängigkeitsfrei, ohne `unsafe`, und läuft
// host-getestet (`tools/host-tests.sh cap`) — dieselbe Form wie beim Manifest.
//
// ## Was das Format NICHT leistet
//
// Es ist **nicht authentifiziert**. Die Prüfsumme sagt „strukturell heil", nicht „von wem". Das
// ist Z4e (Transport + Vertrauen) und hängt an Z7; solange es das nicht gibt, ist ein Checkpoint
// nur so vertrauenswürdig wie das Medium, auf dem er liegt. Diese Zeile steht hier, damit die
// Prüfsumme nicht später für eine Signatur gehalten wird.
//
// ## Die Bindung an das Kernel-Image ist Teil des Formats
//
// [`Image::kernel_hash`] ist keine Diagnose, sondern eine **Zulassungsbedingung** (Z4f in klein):
// ein Checkpoint gehört an genau das Kernel-Image, unter dem er entstand. Wandert er in eine
// Umgebung mit anderen Zusicherungen, merkt es sonst niemand — und das ist die teuerste Form von
// „es lief ja".

/// Kennung am Anfang jedes Checkpoints. Ohne sie ist ein nie beschriebener Sektor (lauter Nullen)
/// von einem beschriebenen nicht zu unterscheiden.
pub const MAGIC: [u8; 8] = *b"SL4KCKPT";

/// Formatversion. Ein Leser, der eine andere findet, **weist ab** — er rät nicht.
pub const FORMAT_VERSION: u32 = 1;

/// Höchstzahl externer Caps in einem Checkpoint (= Cspace-Grösse einer PD).
pub const MAX_CAPS: usize = 16;

/// Offsets — feste Breiten, Little-Endian, wie beim Manifest.
const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 8;
const OFF_BODY_LEN: usize = 12;
const OFF_BODY: usize = 16;
/// Länge des Rumpfes **ohne** Caps: Hash (32) + Fortschritt (8) + Nonce (8) + Epoche (8) +
/// Zahl (4) + Bits (4).
const BODY_FIXED: usize = 32 + 8 + 8 + 8 + 4 + 4;
/// Offsets im Rumpf, relativ zu [`OFF_BODY`].
const B_PROGRESS: usize = 32;
const B_NONCE: usize = 40;
const B_EPOCH: usize = 48;
const B_CAPCOUNT: usize = 56;
const B_PRECOND: usize = 60;
/// Je Cap: Typ-Tag (4) + Füllung (4) + Wert (8).
const CAP_BYTES: usize = 16;

/// Grösse eines Checkpoints mit `n` Caps — Kopf + Rumpf + Prüfsumme.
pub const fn image_bytes(n: usize) -> usize {
    OFF_BODY + BODY_FIXED + n * CAP_BYTES + 4
}

/// **Warum eine Bytefolge kein Checkpoint ist.**
///
/// Bewusst je Grund ein eigener Wert, und [`NoImage`](ImageError::NoImage) ist ausdrücklich
/// **kein Fehler**: ein leerer Sektor heisst „Kaltstart", nicht „kaputt". Die beiden gleich zu
/// behandeln hiesse, jeden ersten Lauf als Störung zu melden — oder, schlimmer, jeden kaputten
/// Checkpoint als Kaltstart durchgehen zu lassen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageError {
    /// Keine Kennung — hier liegt kein Checkpoint. **Kaltstart**, kein Defekt.
    NoImage,
    /// Kennung ja, Formatversion nein. Wird abgewiesen; ein Leser, der eine unbekannte Version
    /// „so gut es geht" liest, liest Felder an falschen Stellen.
    Version { found: u32 },
    /// Die Bytes reichen nicht für das, was der Kopf behauptet — oder die Längenfelder passen
    /// nicht zueinander. Eine Länge aus fremden Bytes ist eine **Behauptung**, keine Tatsache.
    Truncated,
    /// Mehr Caps als [`MAX_CAPS`].
    CapOverflow,
    /// Ein Cap-Typ-Tag, das dieses Format nicht kennt. Fail-closed wie [`classify`].
    UnknownCapTag { tag: u32 },
    /// Prüfsumme passt nicht — die Bytes sind unterwegs beschädigt worden.
    Checksum,
    /// **Der wichtigste Ausgang:** strukturell heil, aber unter einem ANDEREN Kernel-Image
    /// entstanden. Der Zustand wird nicht geladen (Z4f). Ein Checkpoint, der die Umgebung
    /// wechselt, wechselt die Zusicherungen mit — und niemand merkt es.
    ForeignKernel,
    /// Der Zielpuffer ist zu klein (nur beim Schreiben).
    BufferTooSmall,
}

/// **Ein Checkpoint, entpackt.** Enthält keine einzige maschinenlokale Zahl — was hier steht, hat
/// auf jeder Maschine dieselbe Bedeutung (s. [`ExternKind`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Image {
    /// SHA-256 des Kernel-Codes, unter dem der Checkpoint entstand.
    pub kernel_hash: [u8; 32],
    /// Der **Fortschritt** des Threads — der Zustand, um den es geht.
    pub progress: u64,
    /// Ein Wert, den der schreibende Lauf **erst erzeugt** hat. Ohne ihn wäre ein wiedergefundener
    /// Fortschritt von einem Rest im Puffer nicht zu unterscheiden: „die richtigen Bytes" und
    /// „diese Bytes wurden geschrieben" sehen gleich aus.
    pub nonce: u64,
    /// **Das wievielte Glied der Kette.** Ein Kaltstart schreibt 1, jede Wiederherstellung
    /// schreibt eine mehr.
    ///
    /// Sie steht hier, weil der Fortschritt allein die Kette nicht belegt: er ist eine Zahl, die
    /// ein Lauf auch ohne jeden Checkpoint erreicht. **Gemessen** (2026-08-02, KVM): derselbe
    /// Kernel erreicht an derselben Stelle des Hochlaufs in zwei Läufen 148 und 151 Runden — und
    /// in einem Mutationslauf exakt denselben Wert wie der gespeicherte. Ein reproduzierbarer
    /// Wert kann nicht belegen, dass er geerbt wurde.
    pub epoch: u64,
    /// Das Ergebnis der Cap-Klassifikation — nur Übertragbares, denn sonst gäbe es den
    /// Checkpoint nicht (s. [`Image::build`]).
    pub caps: [Option<ExternKind>; MAX_CAPS],
    /// Wie viele davon belegt sind.
    pub cap_count: usize,
    /// Vorbedingungen, die auf der Zielmaschine gelten müssen — Bit 0 = [`Precondition::SameTickSemantics`].
    pub precondition_bits: u32,
}

/// Bit 0 der Vorbedingungen: [`Precondition::SameTickSemantics`].
pub const PRECOND_SAME_TICK: u32 = 1;

fn tag_of(k: &ExternKind) -> (u32, u64) {
    match *k {
        ExternKind::Region { len } => (1, len),
        ExternKind::Endpoint { scope_index } => (2, scope_index as u64),
        ExternKind::Notification { scope_index } => (3, scope_index as u64),
        ExternKind::Tcb { scope_index } => (4, scope_index as u64),
        ExternKind::SchedContext { budget, period } => {
            (5, budget as u64 | ((period as u64) << 32))
        }
    }
}

fn kind_of(tag: u32, v: u64) -> Result<ExternKind, ImageError> {
    Ok(match tag {
        1 => ExternKind::Region { len: v },
        2 => ExternKind::Endpoint { scope_index: v as u32 },
        3 => ExternKind::Notification { scope_index: v as u32 },
        4 => ExternKind::Tcb { scope_index: v as u32 },
        5 => ExternKind::SchedContext {
            budget: v as u32,
            period: (v >> 32) as u32,
        },
        _ => return Err(ImageError::UnknownCapTag { tag }),
    })
}

/// CRC-32 (IEEE, reflektiert — bit-gleich mit Pythons `zlib.crc32`).
///
/// **Absichtlich dieselbe Funktion, die jedes Werkzeug hat.** Nur so kann ein *unabhängiger*
/// Leser in einer anderen Sprache einen Checkpoint erzeugen oder nachprüfen — genau das braucht
/// der Negativfall „fremdes Kernel-Image", und genau das ist die Lehre aus `tools/checkfat.py`:
/// ein Schreiber, der sein eigenes Ergebnis bestätigt, bestätigt nichts.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let m = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & m);
        }
    }
    !crc
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn le64(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

impl Image {
    /// **Einen Checkpoint bauen — oder ihn verweigern.**
    ///
    /// Hier laufen die beiden Hälften von Z4 zusammen: `kinds` sind die Caps, die der Thread
    /// hält; [`classify_all`] entscheidet, ob sie mitdürfen. Ist auch nur eine nicht übertragbar,
    /// entsteht **kein** Checkpoint — mit Slot und Grund, nicht mit einem Sammel-`false`.
    ///
    /// Das ist die Richtung, die zählt: nicht „speichern und drüben sehen, was passt", sondern
    /// **die Absage vor dem Speichern**. Ein Checkpoint, der eine MMIO-Cap enthält, ist auf der
    /// Zielmaschine eine Rechteausweitung mit Reisepass.
    ///
    /// ## The second gate: `subject` and `edges` (Z4d stage 1)
    ///
    /// Caps are only half the question. The other half is the IPC state the machine is in, and it
    /// belongs **here**, in the artifact — not in whoever calls this. Until 2026-08-25 the
    /// stage-1 promise ("migration only without open transactions") rested entirely on the caller
    /// having called `freeze_thread` first and honoured its answer: a property held by call
    /// discipline, which is precisely how `ep_inv` once looked green while proving nothing. A
    /// caller who forgets the freeze got a checkpoint, and the thread on the other end of his open
    /// `CALL` was never mentioned.
    ///
    /// So `build` now asks for the **observed** edges as well and refuses on the first one that
    /// crosses the cut ([`classify_cut`]). Passing an empty slice is not a way around it — it is a
    /// claim that nothing was observed, and it is the caller's claim to make, and wrong on a
    /// machine where something was. The kernel-side collector says how many edges it looked at, and
    /// the check line prints that number: **an empty run is not a test result.**
    pub fn build(
        kernel_hash: [u8; 32],
        progress: u64,
        nonce: u64,
        epoch: u64,
        kinds: &[Option<ObjectKind>],
        scope: &Scope,
        subject: u64,
        edges: &[Edge],
    ) -> Result<Image, BuildRefusal> {
        classify_all(kinds, scope).map_err(|(s, r)| BuildRefusal::Cap(s, r))?;
        classify_cut(subject, edges, scope).map_err(|(i, r)| BuildRefusal::Cut(i, r))?;
        let mut img = Image {
            kernel_hash,
            progress,
            nonce,
            epoch,
            caps: [None; MAX_CAPS],
            cap_count: 0,
            precondition_bits: 0,
        };
        for (slot, k) in kinds.iter().enumerate() {
            let Some(k) = k else { continue };
            match classify(k, scope) {
                Transfer::Portable { kind, precondition } => {
                    if img.cap_count == MAX_CAPS {
                        // Mehr Caps als das Format trägt. **Kein stilles Kürzen**: ein
                        // Checkpoint mit weniger Autorität als der Thread hatte, bricht drüben
                        // an einer Stelle, die niemand mit dem Speichern in Verbindung bringt.
                        // Derselbe Befund wie der fehlende Endowment-Slot beim Hot-Reload.
                        return Err(BuildRefusal::Cap(slot, LocalReason::PeerNotInScope));
                    }
                    img.caps[img.cap_count] = Some(kind);
                    img.cap_count += 1;
                    if precondition == Some(Precondition::SameTickSemantics) {
                        img.precondition_bits |= PRECOND_SAME_TICK;
                    }
                }
                // Kann nicht auftreten -- `classify_all` oben hat schon abgebrochen. Trotzdem
                // behandelt: ein `unreachable!()` im Kern ist ein Panic, und ein Panic hier
                // hinge an einer Eingabe von der Platte.
                Transfer::Refused(r) => return Err(BuildRefusal::Cap(slot, r)),
            }
        }
        Ok(img)
    }

    /// Die Bytes, die dieser Checkpoint belegt.
    pub fn encoded_len(&self) -> usize {
        image_bytes(self.cap_count)
    }

    /// **Schreiben.** Feste Breiten, Little-Endian; die Prüfsumme deckt Kopf **und** Rumpf ab.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, ImageError> {
        let total = self.encoded_len();
        if out.len() < total {
            return Err(ImageError::BufferTooSmall);
        }
        let body_len = BODY_FIXED + self.cap_count * CAP_BYTES;
        out[..total].fill(0);
        out[OFF_MAGIC..OFF_MAGIC + 8].copy_from_slice(&MAGIC);
        out[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        out[OFF_BODY_LEN..OFF_BODY_LEN + 4].copy_from_slice(&(body_len as u32).to_le_bytes());
        let mut o = OFF_BODY;
        out[o..o + 32].copy_from_slice(&self.kernel_hash);
        o += 32;
        out[o..o + 8].copy_from_slice(&self.progress.to_le_bytes());
        o += 8;
        out[o..o + 8].copy_from_slice(&self.nonce.to_le_bytes());
        o += 8;
        out[o..o + 8].copy_from_slice(&self.epoch.to_le_bytes());
        o += 8;
        out[o..o + 4].copy_from_slice(&(self.cap_count as u32).to_le_bytes());
        o += 4;
        out[o..o + 4].copy_from_slice(&self.precondition_bits.to_le_bytes());
        o += 4;
        for k in self.caps.iter().take(self.cap_count).flatten() {
            let (tag, v) = tag_of(k);
            out[o..o + 4].copy_from_slice(&tag.to_le_bytes());
            o += 8; // Tag + Füllung
            out[o..o + 8].copy_from_slice(&v.to_le_bytes());
            o += 8;
        }
        debug_assert_eq!(o, OFF_BODY + body_len);
        let crc = crc32(&out[..o]);
        out[o..o + 4].copy_from_slice(&crc.to_le_bytes());
        Ok(total)
    }

    /// **Lesen — und zwar in dieser Reihenfolge.**
    ///
    /// 1. Kennung  → sonst [`ImageError::NoImage`] (Kaltstart, kein Defekt),
    /// 2. Version  → sonst abweisen, nicht raten,
    /// 3. Längen   → sie müssen zueinander **und** zur Bytefolge passen (fremde Bytes),
    /// 4. Prüfsumme → strukturell heil?
    /// 5. **Kernel-Bindung** → gehört dieser Zustand hierher?
    ///
    /// Die Reihenfolge ist die Aussage: erst wenn 1–4 stehen, ist ein Fehlschlag bei 5 wirklich
    /// „ein Checkpoint eines ANDEREN Kernels" und nicht bloss „kaputte Bytes". Andersherum
    /// gelesen hiesse jeder verwehte Sektor „fremdes Image" — ein Befund, der auf die falsche
    /// Ursache zeigt.
    pub fn decode(bytes: &[u8], expect_kernel: &[u8; 32]) -> Result<Image, ImageError> {
        if bytes.len() < OFF_BODY {
            return Err(ImageError::Truncated);
        }
        if bytes[OFF_MAGIC..OFF_MAGIC + 8] != MAGIC {
            return Err(ImageError::NoImage);
        }
        let version = le32(bytes, OFF_VERSION);
        if version != FORMAT_VERSION {
            return Err(ImageError::Version { found: version });
        }
        let body_len = le32(bytes, OFF_BODY_LEN) as usize;
        // Die behauptete Länge muss (a) mindestens den festen Teil fassen, (b) genau auf
        // Cap-Grenzen aufgehen und (c) samt Prüfsumme in die Bytefolge passen. Jede dieser drei
        // Prüfungen einzeln lässt sich mit einer gebastelten Länge umgehen.
        if body_len < BODY_FIXED || (body_len - BODY_FIXED) % CAP_BYTES != 0 {
            return Err(ImageError::Truncated);
        }
        let total = OFF_BODY + body_len + 4;
        if bytes.len() < total {
            return Err(ImageError::Truncated);
        }
        let cap_count = le32(bytes, OFF_BODY + B_CAPCOUNT) as usize;
        if cap_count > MAX_CAPS {
            return Err(ImageError::CapOverflow);
        }
        // Die **zweite** Längenquelle muss zur ersten passen. Zwei Felder, die dasselbe sagen
        // sollen und es nicht tun, sind der klassische Parser-Einstieg.
        if BODY_FIXED + cap_count * CAP_BYTES != body_len {
            return Err(ImageError::Truncated);
        }
        let stored = le32(bytes, OFF_BODY + body_len);
        if crc32(&bytes[..OFF_BODY + body_len]) != stored {
            return Err(ImageError::Checksum);
        }
        let mut kernel_hash = [0u8; 32];
        kernel_hash.copy_from_slice(&bytes[OFF_BODY..OFF_BODY + 32]);
        if kernel_hash != *expect_kernel {
            return Err(ImageError::ForeignKernel);
        }
        let mut img = Image {
            kernel_hash,
            progress: le64(bytes, OFF_BODY + B_PROGRESS),
            nonce: le64(bytes, OFF_BODY + B_NONCE),
            epoch: le64(bytes, OFF_BODY + B_EPOCH),
            caps: [None; MAX_CAPS],
            cap_count,
            precondition_bits: le32(bytes, OFF_BODY + B_PRECOND),
        };
        let mut o = OFF_BODY + BODY_FIXED;
        for i in 0..cap_count {
            img.caps[i] = Some(kind_of(le32(bytes, o), le64(bytes, o + 8))?);
            o += CAP_BYTES;
        }
        Ok(img)
    }
}


// ================================================================================================
// Z23/S4: derselbe Mechanismus, angewandt auf THREADZUSTAND
// ================================================================================================
//
// Bis hierher klassifiziert dieses Modul **Caps**. S4 verlangt nichts Neues, sondern dieselbe Regel
// eine Ebene tiefer: *was drueben nicht dasselbe bezeichnen kann, wandert nicht mit — und wird
// **benannt** abgewiesen.* Ein neues Verfahren zu erfinden waere der Fehler; `classify` gibt es.
//
// **Warum das VOR dem Migrationscode steht.** Heute wandert nichts: `Image` traegt `progress`,
// `nonce`, `epoch`, `caps` — eine Anwendungsgroesse und die Cap-Klassifikation. Kein Trap-Frame,
// keine Register, kein Stack. „Resume-Latenz" und „Live-Migration" haben damit **kein messbares
// Objekt**, und eine Zahl darauf misst etwas anderes, als „Resume" in einer PaaS-Zusage bedeutet.
// Die Aufzaehlung hier ist die Voraussetzung dafuer, dass es eines geben kann.

/// **Ein Stueck Threadzustand.** Geschlossene Aufzaehlung — und das ist der ganze Zweck: eine Liste,
/// die man vergessen kann, ist keine. Ein neues Feld im TCB, das hier fehlt, faellt beim
/// `match` auf, nicht beim ersten Thaw mit falschen Registern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadStatePart {
    /// Trap-Frame: Allzweckregister, PC, SP, Flags — **und der Ring** (`cs`/`ss` bzw. `spsr`).
    Registers,
    /// **Der FP-Zustand** — und er liegt seit dem Eager-Umbau im `FP_STATES`-Slot, **nicht** im
    /// Trap-Frame. Wird er nicht genannt, wandert ein Thread **ohne seine XMM** und rechnet nach
    /// dem Thaw mit fremden oder genullten Registern weiter: der stille Registerverlust, nur ueber
    /// die Bootgrenze.
    FpState,
    /// Der Stackinhalt.
    Stack { len: u64 },
    /// Prioritaet.
    Priority,
    /// MCS-Konto: Budget, Periode, Rest.
    Account,
    /// **Der Blockadegrund** (die Grund-Menge aus Z24). Ohne ihn liefe ein Thread drueben los, der
    /// hier auf etwas wartet — oder bliebe liegen, ohne dass jemand seinen Wecker kennt.
    BlockReasons,
    /// **Die Park-Weckmarke** — die Schuld aus Z22 P4. Sie ist kein Grund, sondern eine Marke, und
    /// genau deshalb faellt sie beim Aufzaehlen der Gruende durchs Raster.
    ParkWake,
    /// Ein offenes Reply-Token, ueber den Platz im Umfang bezeichnet.
    ReplyToken { endpoint: u32 },
    /// Anstehende Notification-Badges.
    PendingBadges,
    /// Die Bindung an eine Persoenlichkeits-PD (Z26/A3).
    HandlerBinding { handler_pd: u32 },
    /// Kernaffinitaet — eine **Nummer dieser Maschine**.
    CoreAffinity,
    /// Der offene Zyklenstempel (B-5.1) — eine Zahl **dieses** Zaehlers.
    CycleStamp,
    /// Der Kernel-Stack des EL0-Threads (C4/C8).
    KernelStack,
}

/// Das Ergebnis der Zustands-Klassifikation.
///
/// **`secret` ist die Entscheidung, die JETZT faellt und nicht spaeter implizit im
/// Migrationscode.** Ein `Image` mit `progress` und Cap-Klassen war ein **Metadatum**; eines mit
/// Registern, Stack und Speicherinhalt ist ein **Datentraeger**. Bei Migration verlaesst das Bild
/// die Maschine — wer es lesen darf, wo es liegt, ob es ruhend verschluesselt ist, ist dieselbe
/// Sorte Entscheidung wie `SVT`/`SID` bei der IRTE: Autoritaet, vorab zu spezifizieren.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadTransfer {
    /// Wandert mit. `secret` heisst: dieses Stueck kann Schluesselmaterial enthalten.
    Portable {
        secret: bool,
        precondition: Option<Precondition>,
    },
    /// Wandert **nicht** mit, mit Grund.
    Refused(LocalReason),
}

impl ThreadTransfer {
    pub fn is_portable(&self) -> bool {
        matches!(self, ThreadTransfer::Portable { .. })
    }
    /// Traegt dieses Stueck Geheimnisse?
    pub fn is_secret(&self) -> bool {
        matches!(self, ThreadTransfer::Portable { secret: true, .. })
    }
}

/// **Darf dieses Stueck Threadzustand mitwandern?**
///
/// **Fail-closed wie [`classify`]:** was hier nicht ausdruecklich als uebertragbar aufgefuehrt ist,
/// wird verweigert. Weil die Aufzaehlung geschlossen ist, gibt es diesen Fall im `match` gar nicht
/// — der Compiler erzwingt die Entscheidung beim naechsten neuen Feld.
pub fn classify_thread_part(part: &ThreadStatePart, scope: &Scope) -> ThreadTransfer {
    use ThreadStatePart as P;
    match *part {
        // **Register tragen Geheimnisse** — und der FP-Zustand ist genau das Material,
        // dessentwegen der Eager-Umbau beschlossen wurde.
        P::Registers | P::FpState => ThreadTransfer::Portable {
            secret: true,
            precondition: None,
        },
        P::Stack { .. } | P::KernelStack => ThreadTransfer::Portable {
            secret: true,
            precondition: None,
        },
        P::Priority | P::BlockReasons | P::ParkWake | P::PendingBadges => {
            ThreadTransfer::Portable {
                secret: false,
                precondition: None,
            }
        }
        // Dieselbe Vorbedingung wie bei der SchedContext-Cap, und aus demselben Grund: die Zahlen
        // sind **Ticks**, und ein Tick bedeutet auf einer anderen Maschine etwas anderes.
        P::Account => ThreadTransfer::Portable {
            secret: false,
            precondition: Some(Precondition::SameTickSemantics),
        },
        // Beziehungen: nur, wenn die Gegenseite im Umfang liegt -- woertlich die Regel aus
        // `classify`. Ein Reply-Token ins Leere laesst drueben einen Aufrufer haengen.
        P::ReplyToken { endpoint } => match position(scope.endpoints, endpoint) {
            Some(_) => ThreadTransfer::Portable {
                secret: false,
                precondition: None,
            },
            None => ThreadTransfer::Refused(LocalReason::PeerNotInScope),
        },
        P::HandlerBinding { handler_pd } => match position(scope.pds, handler_pd) {
            Some(_) => ThreadTransfer::Portable {
                secret: false,
                precondition: None,
            },
            None => ThreadTransfer::Refused(LocalReason::PdNotInScope),
        },
        // **Nummern dieser Maschine.** Eine Kernnummer bezeichnet drueben einen anderen Kern (oder
        // keinen); ein Zyklenstempel gehoert zu diesem Zaehler. Beides ist kein Mangel, sondern
        // etwas, das drueben **neu entsteht** -- und deshalb eine Absage mit Grund und kein
        // stillschweigendes Weglassen.
        P::CoreAffinity => ThreadTransfer::Refused(LocalReason::MachineLocalNumber),
        P::CycleStamp => ThreadTransfer::Refused(LocalReason::MachineLocalNumber),
    }
}

/// **Traegt ein Bild aus diesen Teilen Geheimnisse?**
///
/// Die Regel, die ab sofort im Plan steht: *Bild enthaelt Registerzustand ⇒ vertraulich; Ablage-
/// und Transportregel steht, bevor gebaut wird.* Diese Funktion ist die maschinenlesbare Fassung —
/// eine Regel in Prosa hat kein Gatter.
pub fn image_is_confidential(parts: &[ThreadStatePart], scope: &Scope) -> bool {
    parts
        .iter()
        .any(|p| classify_thread_part(p, scope).is_secret())
}

/// **Die vollstaendige Liste** dessen, was ein Bild mit Threadzustand tragen muss.
///
/// Sie steht hier und nicht im Migrationscode, damit die Frage „ist etwas vergessen worden?" **eine**
/// Antwort hat. `ReplyToken` und `HandlerBinding` fehlen bewusst: sie tragen eine ID und lassen sich
/// nicht ohne Kontext auffuehren -- der Aufrufer haengt sie an.
pub const THREAD_STATE_PARTS: [ThreadStatePart; 10] = [
    ThreadStatePart::Registers,
    ThreadStatePart::FpState,
    ThreadStatePart::Stack { len: 0 },
    ThreadStatePart::KernelStack,
    ThreadStatePart::Priority,
    ThreadStatePart::Account,
    ThreadStatePart::BlockReasons,
    ThreadStatePart::ParkWake,
    ThreadStatePart::PendingBadges,
    ThreadStatePart::CoreAffinity,
];

#[cfg(test)]
mod tests {
    use super::*;
    use caprock_mem::PhysRegion;

    fn region(len: u64) -> ObjectKind {
        ObjectKind::Memory(PhysRegion::new(0x1000, len))
    }

    /// **Die Kernaussage von Z4b:** Geräte-Autorität wandert NICHT, und zwar mit unterscheidbaren
    /// Gründen. Der bequeme Weg wäre, eine MMIO-Cap „auf das entsprechende Gerät drüben"
    /// abzubilden — es gibt kein entsprechendes Gerät, es gibt ein anderes.
    #[test]
    fn geraete_autoritaet_wandert_nicht() {
        let s = Scope::EMPTY;
        assert_eq!(
            classify(&ObjectKind::Mmio { phys: 0xfe00_0000, len: 0x1000 }, &s),
            Transfer::Refused(LocalReason::DeviceWindow)
        );
        assert_eq!(
            classify(&ObjectKind::Irq { intid: 33 }, &s),
            Transfer::Refused(LocalReason::InterruptLine)
        );
        assert_eq!(
            classify(&ObjectKind::Dma { phys: 0x3c0_0000, len: 0x4000, dir: crate::DmaDir::Bidirectional, coherence: crate::DmaCoherence::NonCoherent }, &s),
            Transfer::Refused(LocalReason::DmaRegion)
        );
        // Die drei Gruende sind VERSCHIEDEN -- ein Sammel-`false` haette hier denselben Wert und
        // liesse den Aufrufer an der falschen Stelle suchen.
        assert_ne!(
            classify(&ObjectKind::Mmio { phys: 0, len: 1 }, &s),
            classify(&ObjectKind::Irq { intid: 1 }, &s)
        );
    }

    /// **Z26/A3: eine Handler-Cap wandert NIE — auch nicht mit vollem Umfang.**
    ///
    /// Der Unterschied zu einer Endpoint-Cap ist der Punkt: die ist *behebbar* verweigert (nimm
    /// den Partner in den Umfang), diese nicht. Im Sidecar stehen halbe Trap-Frames — 22 Wörter
    /// auf x86_64, 34 auf aarch64, mit verschiedener Bedeutung —, und die andere Hälfte des
    /// Syscalls ist ein Thread, der hier blockiert bleibt.
    ///
    /// **Und der Grund hat seit dem 2026-08-13 einen eigenen Namen** ([`LocalReason::HandlerBinding`]).
    /// Bis dahin stand hier `PendingReply` — der Test pinnte die Ungenauigkeit fest, damit die
    /// Umstellung sichtbar wird statt still. Sie ist gekommen, und er ist mitgewandert; die
    /// zusätzliche Zeile unten hält die beiden Gründe auseinander, denn genau darum ging es.
    #[test]
    fn handler_caps_wandern_nicht() {
        // Voller Umfang: Endpoint 7 IST im Umfang -- eine gewoehnliche Endpoint-Cap darauf waere
        // portabel. Die Handler-Cap auf denselben Endpoint ist es nicht.
        let eps = [7u32];
        let voll = Scope { endpoints: &eps, ..Scope::EMPTY };
        assert!(classify(&ObjectKind::Endpoint(7), &voll).is_portable());
        let sys = ObjectKind::SyscallHandler { ep: 7, pd: 3, sidecar: 0x20_0000, len: 0x1000 };
        let flt = ObjectKind::FaultHandler { ep: 7, pd: 3, sidecar: 0x20_0000, len: 0x1000 };
        assert_eq!(classify(&sys, &voll), Transfer::Refused(LocalReason::HandlerBinding));
        assert_eq!(classify(&flt, &voll), Transfer::Refused(LocalReason::HandlerBinding));
        assert!(!classify(&sys, &voll).is_portable());
        assert!(!classify(&flt, &voll).is_portable());
        // **Die beiden Gruende sind unterscheidbar** -- das ist der ganze Grund fuer den eigenen
        // Namen. Eine Reply-Cap ist verweigert, weil ein konkreter Aufrufer hier blockiert;
        // eine Handler-Cap, weil ihr Fenster halbe Frames traegt. Wer beides zusammenwirft,
        // schickt den Betreiber in die falsche Behebung.
        assert_ne!(LocalReason::HandlerBinding, LocalReason::PendingReply);
        assert_eq!(
            classify(&ObjectKind::Reply { ep: 7, caller: 1 }, &voll),
            Transfer::Refused(LocalReason::PendingReply)
        );
    }

    /// **Dieselbe Cap, zwei Antworten** — je nach Umfang. Genau deshalb nimmt `classify` ihn.
    #[test]
    fn endpoint_haengt_am_umfang() {
        let ohne = Scope::EMPTY;
        assert_eq!(
            classify(&ObjectKind::Endpoint(7), &ohne),
            Transfer::Refused(LocalReason::PeerNotInScope)
        );
        let mit = Scope { endpoints: &[3, 7, 9], ..Scope::EMPTY };
        // Und der Index ist der Platz IM CHECKPOINT, nicht die Endpoint-ID dieser Maschine --
        // sonst waere die maschinenlokale Zahl doch wieder mitgewandert, nur getarnt.
        assert_eq!(
            classify(&ObjectKind::Endpoint(7), &mit),
            Transfer::Portable { kind: ExternKind::Endpoint { scope_index: 1 }, precondition: None }
        );
    }

    /// Speicher wandert als **Inhalt**, nicht als Adresse: in der externen Darstellung kommt keine
    /// Physadresse vor. Die Zielmaschine legt ihn hin, wo sie will, und färbt neu.
    #[test]
    fn speicher_wandert_ohne_adresse() {
        let t = classify(&region(0x10000), &Scope::EMPTY);
        assert_eq!(
            t,
            Transfer::Portable { kind: ExternKind::Region { len: 0x10000 }, precondition: None }
        );
        // Zwei Regionen gleicher Laenge an verschiedenen Physadressen sind extern GLEICH -- das
        // ist der Beleg dafuer, dass die Adresse nicht mitgeht.
        let a = classify(&ObjectKind::Memory(PhysRegion::new(0x1000, 0x2000)), &Scope::EMPTY);
        let b = classify(&ObjectKind::Memory(PhysRegion::new(0x9_0000, 0x2000)), &Scope::EMPTY);
        assert_eq!(a, b);
    }

    /// Ein Budget ist der Form nach uebertragbar und traegt eine **Vorbedingung** -- die Zahlen
    /// sind Ticks, und ein Tick bedeutet auf einer Maschine ohne invarianten Zaehler etwas
    /// anderes (Z4f, B-5.1). Sie zu verschweigen hiesse, die Abrechnung still falsch werden zu
    /// lassen.
    #[test]
    fn budget_traegt_eine_vorbedingung() {
        let t = classify(&ObjectKind::SchedContext { budget: 10, period: 100 }, &Scope::EMPTY);
        match t {
            Transfer::Portable { kind, precondition } => {
                assert_eq!(kind, ExternKind::SchedContext { budget: 10, period: 100 });
                assert_eq!(precondition, Some(Precondition::SameTickSemantics));
            }
            _ => panic!("ein Budget muss uebertragbar sein -- ohne kann der Thread drueben nicht laufen"),
        }
    }

    /// Eine Reply-Cap bezeichnet einen konkreten blockierten Aufrufer. Der bleibt hier, egal wie
    /// gross der Umfang ist -- das ist Z4d Stufe 1: Migration nur ohne offene Transaktionen.
    #[test]
    fn offene_transaktion_wandert_nie() {
        let gross = Scope { endpoints: &[1, 2, 3], threads: &[42], ..Scope::EMPTY };
        assert_eq!(
            classify(&ObjectKind::Reply { ep: 1, caller: 42 }, &gross),
            Transfer::Refused(LocalReason::PendingReply)
        );
    }

    /// Der ganze Cspace auf einmal — und der **Slot** kommt mit. „Irgendeine Cap ist nicht
    /// uebertragbar" ist als Diagnose wertlos, wenn ein Cspace dreissig Eintraege hat.
    #[test]
    fn ganzer_cspace_nennt_den_slot() {
        let scope = Scope { endpoints: &[5], ..Scope::EMPTY };
        let kinds = [
            Some(region(0x1000)),
            Some(ObjectKind::Endpoint(5)),
            None,
            Some(ObjectKind::Mmio { phys: 0xfe00_0000, len: 0x1000 }),
        ];
        assert_eq!(classify_all(&kinds, &scope), Err((3, LocalReason::DeviceWindow)));
        // Ohne die MMIO-Cap geht es durch.
        assert_eq!(classify_all(&kinds[..3], &scope), Ok(()));
    }

    /// **Ein leerer Cspace ist uebertragbar** — und das ist bewusst so, aber es ist auch die
    /// Stelle, an der ein Aufrufer sich taeuschen kann: `Ok(())` heisst „nichts spricht dagegen",
    /// nicht „es ist etwas dabei".
    #[test]
    fn leer_heisst_nichts_spricht_dagegen() {
        assert_eq!(classify_all(&[], &Scope::EMPTY), Ok(()));
        assert_eq!(classify_all(&[None, None], &Scope::EMPTY), Ok(()));
    }

    // --- Z4 Stufe 2: das Format ---------------------------------------------------------------

    const K1: [u8; 32] = [0x11; 32];
    const K2: [u8; 32] = [0x22; 32];

    /// The migrating thread of the format tests. They are about bytes, not about relationships —
    /// so they hand in a subject and **no** edges, which is the honest claim "nothing was observed
    /// here", not a way past the gate.
    const SUBJ: u64 = 7;

    fn bau(progress: u64, nonce: u64) -> Image {
        Image::build(
            K1,
            progress,
            nonce,
            3,
            &[
                Some(region(0x1000)),
                Some(ObjectKind::SchedContext { budget: 4, period: 10 }),
            ],
            &Scope::EMPTY,
            SUBJ,
            &[],
        )
        .expect("beide Caps sind uebertragbar")
    }

    /// Schreiben und Lesen sind zueinander invers — und die **Vorbedingung** kommt mit. Ohne sie
    /// wanderte ein Budget in Ticks auf eine Maschine, auf der ein Tick etwas anderes heisst.
    #[test]
    fn hin_und_zurueck() {
        let img = bau(4711, 0xDEAD_BEEF_CAFE_0001);
        assert_eq!(img.cap_count, 2);
        assert_eq!(img.epoch, 3);
        assert_eq!(img.precondition_bits, PRECOND_SAME_TICK);
        let mut buf = [0u8; 512];
        let n = img.encode(&mut buf).unwrap();
        assert_eq!(n, image_bytes(2));
        assert_eq!(Image::decode(&buf[..n], &K1), Ok(img));
        // Und der Rest des Sektors geht die Sache nichts an: ein Checkpoint traegt SEINE Laenge.
        assert_eq!(Image::decode(&buf, &K1), Ok(img));
    }

    /// **Der wichtigste Ausgang.** Struktur heil, Pruefsumme heil, Kernel-Image ein anderes ->
    /// ABGEWIESEN. Das ist Z4f in klein: ein Zustand, der die Umgebung wechselt, wechselt die
    /// Zusicherungen mit, und ohne diese Pruefung merkt es niemand.
    #[test]
    fn fremdes_kernel_image_wird_abgewiesen() {
        let img = bau(4711, 7);
        let mut buf = [0u8; 512];
        let n = img.encode(&mut buf).unwrap();
        assert_eq!(Image::decode(&buf[..n], &K2), Err(ImageError::ForeignKernel));
        // Die Gegenprobe: mit dem RICHTIGEN Hash geht dieselbe Bytefolge durch. Ohne sie
        // belegte die Abweisung nur, dass `decode` ueberhaupt etwas ablehnt.
        assert!(Image::decode(&buf[..n], &K1).is_ok());
    }

    /// **Ein leerer Sektor ist ein Kaltstart, kein Defekt** — und das muss ein eigener Ausgang
    /// sein. Wer beides gleich behandelt, meldet entweder jeden ersten Lauf als Stoerung oder
    /// laesst jeden kaputten Checkpoint als Kaltstart durchgehen.
    #[test]
    fn leerer_sektor_ist_kein_defekt() {
        assert_eq!(Image::decode(&[0u8; 512], &K1), Err(ImageError::NoImage));
        assert_eq!(Image::decode(&[], &K1), Err(ImageError::Truncated));
        assert_eq!(Image::decode(&[0u8; 4], &K1), Err(ImageError::Truncated));
    }

    /// Ein gekipptes Bit **irgendwo** faellt auf — und zwar als Pruefsummenfehler, nicht als
    /// falscher Inhalt. Geprueft wird jede Byteposition, nicht eine ausgesuchte.
    #[test]
    fn jedes_gekippte_byte_faellt_auf() {
        let img = bau(0x0102_0304_0506_0708, 0x1122_3344_5566_7788);
        let mut buf = [0u8; 512];
        let n = img.encode(&mut buf).unwrap();
        for i in 0..n {
            let mut k = buf;
            k[i] ^= 0xFF;
            let r = Image::decode(&k[..n], &K1);
            assert!(
                r != Ok(img),
                "Byte {i} gekippt und der Checkpoint kam unveraendert zurueck"
            );
            // Das Hash-Feld ist der einzige Bereich, in dem ein Kippen VOR der Pruefsumme
            // erkannt wuerde -- dort ist die Aussage aber dieselbe: nicht geladen.
        }
    }

    /// **Eine Laenge aus fremden Bytes ist eine Behauptung.** Drei Wege, sie zu faelschen, und
    /// alle drei muessen scheitern: zu kurz, nicht auf Cap-Grenze, und zwei Felder, die
    /// einander widersprechen.
    #[test]
    fn geloegene_laengen_tragen_nicht() {
        let img = bau(1, 2);
        let mut buf = [0u8; 512];
        let n = img.encode(&mut buf).unwrap();

        let mut a = buf;
        a[OFF_BODY_LEN..OFF_BODY_LEN + 4].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(Image::decode(&a[..n], &K1), Err(ImageError::Truncated));

        let mut b = buf;
        b[OFF_BODY_LEN..OFF_BODY_LEN + 4]
            .copy_from_slice(&((BODY_FIXED + 3) as u32).to_le_bytes());
        assert_eq!(Image::decode(&b[..n], &K1), Err(ImageError::Truncated));

        // Rumpflaenge sagt 2 Caps, das Zahlfeld sagt 1 -> Widerspruch.
        let mut c = buf;
        c[OFF_BODY + B_CAPCOUNT..OFF_BODY + B_CAPCOUNT + 4].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(Image::decode(&c[..n], &K1), Err(ImageError::Truncated));

        // Und eine Laenge, die ueber die Bytefolge hinausgreift.
        let mut d = buf;
        d[OFF_BODY_LEN..OFF_BODY_LEN + 4]
            .copy_from_slice(&((BODY_FIXED + 400 * CAP_BYTES) as u32).to_le_bytes());
        assert_eq!(Image::decode(&d[..n], &K1), Err(ImageError::Truncated));
    }

    /// Eine unbekannte Formatversion wird **abgewiesen**, nicht „so gut es geht" gelesen — sonst
    /// stehen die Felder an anderen Stellen, und der Zustand kommt still verschoben an.
    #[test]
    fn fremde_version_wird_nicht_geraten() {
        let img = bau(1, 2);
        let mut buf = [0u8; 512];
        let n = img.encode(&mut buf).unwrap();
        buf[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            Image::decode(&buf[..n], &K1),
            Err(ImageError::Version { found: 99 })
        );
    }

    /// **Die Verweigerungsregel greift beim SPEICHERN**, nicht erst drueben. Eine DMA-Cap im
    /// Umfang heisst: es entsteht kein Checkpoint — mit Slot und Grund.
    #[test]
    fn eine_dma_cap_verhindert_den_checkpoint() {
        let kinds = [
            Some(region(0x1000)),
            Some(ObjectKind::Dma {
                phys: 0x3c0_0000,
                len: 0x4000,
                dir: crate::DmaDir::Bidirectional,
                coherence: crate::DmaCoherence::NonCoherent,
            }),
        ];
        assert_eq!(
            Image::build(K1, 1, 2, 1, &kinds, &Scope::EMPTY, SUBJ, &[]),
            Err(BuildRefusal::Cap(1, LocalReason::DmaRegion))
        );
        // Und die Gegenprobe: ohne sie entsteht einer. Sonst belegte die Absage nur, dass
        // `build` ueberhaupt fehlschlagen kann.
        assert!(Image::build(K1, 1, 2, 1, &kinds[..1], &Scope::EMPTY, SUBJ, &[]).is_ok());
    }

    /// Ein zu kleiner Zielpuffer ist ein eigener Ausgang — kein halb geschriebener Checkpoint.
    #[test]
    fn zu_kleiner_puffer_schreibt_nichts() {
        let img = bau(1, 2);
        let mut klein = [0u8; 16];
        assert_eq!(img.encode(&mut klein), Err(ImageError::BufferTooSmall));
        assert_eq!(klein, [0u8; 16]);
    }

    /// Die Pruefsumme ist die **des Werkzeugs**, nicht eine eigene: `zlib.crc32(b"123456789")`
    /// ist 0xCBF43926. Ein Checkpoint, den nur dieser Code nachrechnen kann, laesst sich von
    /// aussen weder pruefen noch herstellen — und genau das braucht der Negativfall.
    #[test]
    fn crc32_ist_der_uebliche() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    // -- Z23/S4: Threadzustand ------------------------------------------------------------------

    #[test]
    fn fp_zustand_ist_aufgezaehlt_und_geheim() {
        // Die Zeile, um die es geht: der FP-Zustand liegt seit dem Eager-Umbau NICHT im
        // Trap-Frame. Faellt er aus der Liste, wandert ein Thread ohne seine XMM.
        assert!(THREAD_STATE_PARTS.contains(&ThreadStatePart::FpState));
        assert!(classify_thread_part(&ThreadStatePart::FpState, &Scope::EMPTY).is_secret());
    }

    #[test]
    fn park_marke_ist_aufgezaehlt() {
        // Die Schuld aus Z22 P4: sie ist kein GRUND, sondern eine MARKE -- und faellt deshalb beim
        // Aufzaehlen der Gruende durchs Raster.
        assert!(THREAD_STATE_PARTS.contains(&ThreadStatePart::ParkWake));
        assert!(THREAD_STATE_PARTS.contains(&ThreadStatePart::BlockReasons));
    }

    #[test]
    fn ein_bild_mit_registern_ist_vertraulich() {
        assert!(image_is_confidential(
            &[ThreadStatePart::Priority, ThreadStatePart::Registers],
            &Scope::EMPTY
        ));
        // ... und eines ohne nicht. **Beide Richtungen**, sonst prueft der Test eine Konstante.
        assert!(!image_is_confidential(
            &[ThreadStatePart::Priority, ThreadStatePart::Account],
            &Scope::EMPTY
        ));
    }

    #[test]
    fn die_volle_liste_ist_vertraulich() {
        // Die praktische Folge: **jedes** vollstaendige Bild traegt Geheimnisse. Es gibt keinen
        // Migrationsfall, in dem die Ablage- und Transportregel entfaellt.
        assert!(image_is_confidential(&THREAD_STATE_PARTS, &Scope::EMPTY));
    }

    #[test]
    fn maschinenlokale_zahlen_werden_benannt_abgewiesen() {
        for p in [ThreadStatePart::CoreAffinity, ThreadStatePart::CycleStamp] {
            assert_eq!(
                classify_thread_part(&p, &Scope::EMPTY),
                ThreadTransfer::Refused(LocalReason::MachineLocalNumber),
                "{p:?} muss mit GRUND abgewiesen werden, nicht stillschweigend fehlen"
            );
        }
    }

    #[test]
    fn beziehungen_haengen_am_umfang_und_zwar_in_beide_richtungen() {
        let tok = ThreadStatePart::ReplyToken { endpoint: 7 };
        // ohne Umfang: Absage mit Grund
        assert_eq!(
            classify_thread_part(&tok, &Scope::EMPTY),
            ThreadTransfer::Refused(LocalReason::PeerNotInScope)
        );
        // mit Umfang: geht -- **die Positivkontrolle**. Ohne sie belegt die Absage nur, dass
        // irgendetwas nicht ging.
        let scope = Scope {
            endpoints: &[7],
            ..Scope::EMPTY
        };
        assert!(classify_thread_part(&tok, &scope).is_portable());
        // und ein ANDERER Endpoint bleibt abgewiesen -- sonst waere „Umfang" ein Freibrief.
        assert!(!classify_thread_part(
            &ThreadStatePart::ReplyToken { endpoint: 8 },
            &scope
        )
        .is_portable());
    }

    #[test]
    fn das_konto_traegt_dieselbe_vorbedingung_wie_seine_cap() {
        // Zwei Stellen, eine Aussage: die Zahlen sind Ticks. Liefen sie auseinander, waere die
        // Cap uebertragbar und der Zustand nicht -- oder umgekehrt.
        assert_eq!(
            classify_thread_part(&ThreadStatePart::Account, &Scope::EMPTY),
            ThreadTransfer::Portable {
                secret: false,
                precondition: Some(Precondition::SameTickSemantics)
            }
        );
        assert_eq!(
            classify(
                &ObjectKind::SchedContext {
                    budget: 1,
                    period: 2
                },
                &Scope::EMPTY
            ),
            Transfer::Portable {
                kind: ExternKind::SchedContext {
                    budget: 1,
                    period: 2
                },
                precondition: Some(Precondition::SameTickSemantics)
            }
        );
    }
    // -- Z4d stage 1: the cut ------------------------------------------------------------------

    /// The migrating thread of the cut tests, and a peer that stays behind.
    const MIGRANT: u64 = 100;
    const BLEIBT: u64 = 200;

    fn kante(ch: Channel, thread: u64, role: EdgeRole) -> Edge {
        Edge { channel: ch, thread, role }
    }

    /// **Z4d verbatim.** A thread with an open `CALL` at an endpoint that stays behind leaves a
    /// waiting server — and there is no scope size that makes it right except taking the channel
    /// along.
    #[test]
    fn offener_call_ohne_seinen_kanal_wird_abgewiesen() {
        let e = kante(Channel::Endpoint(3), MIGRANT, EdgeRole::Caller);
        assert_eq!(
            classify_edge(&e, MIGRANT, &Scope::EMPTY),
            Some(CutRefusal::ChannelNotInScope)
        );
        // The positive control, and without it the refusal only shows that something failed:
        // with the channel in scope the very same edge passes.
        let mit = Scope { endpoints: &[3], ..Scope::EMPTY };
        assert_eq!(classify_edge(&e, MIGRANT, &mit), None);
    }

    /// **The half that had no name.** The endpoint travels, a client blocked at it does not — and
    /// he waits forever on a rendezvous point that left the machine. This is the case
    /// `Scope::endpoints` merely *asserted* away ("endpoints whose both sides are part of the
    /// checkpoint") and nobody ever checked.
    #[test]
    fn ein_kanal_ohne_seinen_teilnehmer_wird_abgewiesen() {
        let mit = Scope { endpoints: &[3], ..Scope::EMPTY };
        let fremd = kante(Channel::Endpoint(3), BLEIBT, EdgeRole::Sender);
        assert_eq!(
            classify_edge(&fremd, MIGRANT, &mit),
            Some(CutRefusal::PeerNotInScope)
        );
        // ... and it is repaired by taking the peer along, not by anything else. That is why the
        // two reasons are separate values.
        let beide = Scope { endpoints: &[3], threads: &[BLEIBT], ..Scope::EMPTY };
        assert_eq!(classify_edge(&fremd, MIGRANT, &beide), None);
    }

    /// **Two strangers are none of our business.** Neither end travels, so the transaction is not
    /// cut. Refusing it would make a checkpoint impossible on any machine that is doing something.
    #[test]
    fn fremde_beziehung_geht_den_schnitt_nichts_an() {
        let e = kante(Channel::Endpoint(9), BLEIBT, EdgeRole::ReplyOwner);
        assert_eq!(classify_edge(&e, MIGRANT, &Scope::EMPTY), None);
    }

    /// **No role is exempt** — including the one with no partner. A receiver waiting in `RECV`
    /// leaves nobody hanging, and is still refused: after the move *he* waits on a channel he no
    /// longer has. Same for a notification waiter, which is why `Channel` has two variants.
    #[test]
    fn keine_rolle_ist_ausgenommen() {
        for r in [
            EdgeRole::Sender,
            EdgeRole::Receiver,
            EdgeRole::Caller,
            EdgeRole::ReplyOwner,
        ] {
            assert_eq!(
                classify_edge(&kante(Channel::Endpoint(3), MIGRANT, r), MIGRANT, &Scope::EMPTY),
                Some(CutRefusal::ChannelNotInScope),
                "{r:?} must not travel without its channel"
            );
        }
        let n = kante(Channel::Notification(3), MIGRANT, EdgeRole::Receiver);
        assert_eq!(
            classify_edge(&n, MIGRANT, &Scope::EMPTY),
            Some(CutRefusal::ChannelNotInScope)
        );
        // An endpoint numbered 3 in the scope does **not** cover notification 3. Two namespaces,
        // and conflating them would let a checkpoint travel on the strength of an unrelated id.
        let eps_only = Scope { endpoints: &[3], ..Scope::EMPTY };
        assert_eq!(
            classify_edge(&n, MIGRANT, &eps_only),
            Some(CutRefusal::ChannelNotInScope)
        );
    }

    /// The whole edge list at once — and the **index** comes back, for the same reason
    /// `classify_all` returns the slot.
    #[test]
    fn der_schnitt_nennt_die_kante() {
        let scope = Scope { endpoints: &[3], threads: &[BLEIBT], ..Scope::EMPTY };
        let edges = [
            kante(Channel::Endpoint(3), MIGRANT, EdgeRole::Caller),
            kante(Channel::Endpoint(3), BLEIBT, EdgeRole::ReplyOwner),
            kante(Channel::Endpoint(4), MIGRANT, EdgeRole::Sender),
        ];
        assert_eq!(
            classify_cut(MIGRANT, &edges, &scope),
            Err((2, CutRefusal::ChannelNotInScope))
        );
        // The first two are the *contained* relationship: caller and reply owner both travel, and
        // so does their endpoint. That is the group-cut rule of Z23/S3 in the checkpoint's words —
        // a relationship with both ends inside the cut is not an open relationship of the cut.
        assert_eq!(classify_cut(MIGRANT, &edges[..2], &scope), Ok(()));
        // An empty list is a claim, not a proof. It passes — and the kernel-side collector prints
        // how many edges it looked at, so that "nothing observed" cannot pass for "nothing there".
        assert_eq!(classify_cut(MIGRANT, &[], &Scope::EMPTY), Ok(()));
    }

    /// **The gate sits in `build`, not in the caller.** That is the whole point of Z4d stage 1:
    /// until 2026-08-25 the promise rested on whoever called `freeze_thread` first, which is call
    /// discipline and not structure.
    #[test]
    fn build_verweigert_den_gekreuzten_schnitt() {
        let kinds = [Some(region(0x1000))];
        let edges = [kante(Channel::Endpoint(3), MIGRANT, EdgeRole::Caller)];
        assert_eq!(
            Image::build(K1, 1, 2, 1, &kinds, &Scope::EMPTY, MIGRANT, &edges),
            Err(BuildRefusal::Cut(0, CutRefusal::ChannelNotInScope))
        );
        // The counter-proof: the same caps, the same subject, no crossing edge -> a checkpoint
        // exists. Without it the refusal would only show that `build` can fail at all.
        assert!(Image::build(K1, 1, 2, 1, &kinds, &Scope::EMPTY, MIGRANT, &[]).is_ok());
    }

    /// **A cap refusal and a cut refusal stay apart.** They are answered differently: a DMA cap
    /// never becomes portable by waiting, an open transaction may end on its own in a tick.
    #[test]
    fn cap_grund_und_schnitt_grund_sind_unterscheidbar() {
        let kinds = [Some(ObjectKind::Mmio { phys: 0xfe00_0000, len: 0x1000 })];
        let edges = [kante(Channel::Endpoint(3), MIGRANT, EdgeRole::Caller)];
        // Both are wrong at once — and the caps are judged first, because that is the refusal that
        // does not dissolve on its own.
        assert_eq!(
            Image::build(K1, 1, 2, 1, &kinds, &Scope::EMPTY, MIGRANT, &edges),
            Err(BuildRefusal::Cap(0, LocalReason::DeviceWindow))
        );
        assert_eq!(
            Image::build(K1, 1, 2, 1, &[], &Scope::EMPTY, MIGRANT, &edges),
            Err(BuildRefusal::Cut(0, CutRefusal::ChannelNotInScope))
        );
    }

    /// The reply **token** in the thread state and the reply **edge** at the endpoint must agree.
    /// They are two descriptions of one relationship; if they could disagree, one of them would be
    /// decorative — and it would be the one nobody reads.
    #[test]
    fn reply_token_und_reply_kante_urteilen_gleich() {
        for ep in [3u32, 4u32] {
            let scope = Scope { endpoints: &[3], ..Scope::EMPTY };
            let token_ok =
                classify_thread_part(&ThreadStatePart::ReplyToken { endpoint: ep }, &scope)
                    .is_portable();
            let kante_ok = classify_edge(
                &kante(Channel::Endpoint(ep), MIGRANT, EdgeRole::Caller),
                MIGRANT,
                &scope,
            )
            .is_none();
            assert_eq!(token_ok, kante_ok, "endpoint {ep}: two verdicts on one relationship");
        }
    }
}
