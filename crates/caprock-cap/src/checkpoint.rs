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
    pub endpoints: &'a [u32],
    /// Notifications, deren Signalgeber und Empfänger mitwandern.
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
    pub fn build(
        kernel_hash: [u8; 32],
        progress: u64,
        nonce: u64,
        epoch: u64,
        kinds: &[Option<ObjectKind>],
        scope: &Scope,
    ) -> Result<Image, (usize, LocalReason)> {
        classify_all(kinds, scope)?;
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
                        return Err((slot, LocalReason::PeerNotInScope));
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
                Transfer::Refused(r) => return Err((slot, r)),
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
            Image::build(K1, 1, 2, 1, &kinds, &Scope::EMPTY),
            Err((1, LocalReason::DmaRegion))
        );
        // Und die Gegenprobe: ohne sie entsteht einer. Sonst belegte die Absage nur, dass
        // `build` ueberhaupt fehlschlagen kann.
        assert!(Image::build(K1, 1, 2, 1, &kinds[..1], &Scope::EMPTY).is_ok());
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
}
