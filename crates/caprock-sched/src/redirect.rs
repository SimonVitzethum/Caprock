//! **Umgeleitete Syscalls — der Kern des Primitivs** (Z26/A3, gebaut 2026-08-10).
//!
//! Ein Thread einer PD führt Fremdcode aus; sein Syscall-Eintritt wird nicht vom Caprock-Kernel
//! bearbeitet, sondern an eine **Persönlichkeits-PD** zugestellt. Das ist die vierte Vorbedingung
//! aus [Z26] und steht auf keinem anderen Weg — sie ist der Preis der Entscheidung „Weg A".
//!
//! **Das Ziel ist NICHT gerätespezifisch.** Der Kernel kennt genau einen Satz: „die Syscalls
//! dieses Threads gehen an Handler H". Keine ioctl-Nummer, keine Gerätedatei, kein GPU-Begriff
//! kommt in dieser Datei vor — NVIDIA/CUDA ist der teuerste Anwendungsfall einer beliebigen
//! Linux-Persönlichkeit, nicht ihr Gegenstand.
//!
//! ## Warum diese Datei abhängigkeitsfrei ist
//!
//! Dieselbe Begründung wie bei [`crate::cycles`] (B-5.1): die Fallen hier sind **Reihenfolgen und
//! Zahlenbereiche** — ein Zyklus in der Handler-Kette, ein Slot-Index um eins daneben, ein
//! Rückfall auf die native ABI nach dem Entzug einer Cap. Alle drei lassen sich mit **Literalen**
//! auslösen und brauchen weder Maschine noch QEMU. Der Rest (Cap-Auflösung, Frame-Transport,
//! Scheduler) steht anderswo; hier steht die **Entscheidung**.
//!
//! ## Der Frame-Transport ist ein SIDECAR, keine Register-Schreib-Cap
//!
//! Der erste Entwurf in Z26 gab dem Handler eine Cap, mit der er **fremde Ergebnisregister
//! schreibt**. Gebaut ist etwas anderes, und der Grund ist gemessen, nicht ästhetisch:
//!
//! | | Wörter | Bytes |
//! |---|---|---|
//! | `caprock_abi::MSG_WORDS` (register-basierte IPC) | **4** | 32 |
//! | `TrapFrame` x86_64 (15 GPR + Vektor/Fehler/rip/cs/rflags/rsp/ss) | **22** | **176** |
//! | `TrapFrame` aarch64 (x0..x30 + ELR + SPSR + SP_EL0) | **34** | **272** |
//!
//! Eine Nachricht mit vier Wörtern kann `rt_sigreturn` (ersetzt den **ganzen** Frame) und `clone`
//! (braucht einen **zweiten** Frame) strukturell nicht tragen. Also liegt der Frame in einem
//! geteilten Fenster — dem **Sidecar** —, und der Handler liest und schreibt ihn **als Speicher**.
//! Das ist die Form, die Fuchsias `zx_restricted_bind_state` nimmt, und sie ist hier aus drei
//! Gründen die bessere:
//!
//! 1. **Die Autorität ist eine Region, keine Fähigkeit über fremden Registerzustand.** Z26 rechnet
//!    ehrlich zusammen: „die Persönlichkeits-PD hält Frame-Schreibrecht, vollen Speicherzugriff,
//!    Vspace-Manipulation — sie IST der Kernel des Gastes." Die Sidecar-Form **halbiert die
//!    erste** dieser drei: es gibt keine Operation „schreibe die Register des Threads T". Es gibt
//!    ein Fenster, in dem die Frames **seiner eigenen** Gäste liegen.
//! 2. **Sie ist auditierbar.** „Wer darf fremde Register schreiben" ist über eine Cap-Art
//!    beantwortbar; „welchen Speicher hält diese PD" ist es ohnehin. Eine Region taucht in der
//!    Speicherbuchhaltung auf, ein Schreibrecht nur im Cap-Audit.
//! 3. **Sie ist nicht umlenkbar.** Eine Register-Schreib-Cap müsste im Kernel auf „nur an mich
//!    gebundene Threads" eingeschränkt werden (Z26 sagt das ausdrücklich) — also eine Prüfung, die
//!    bei jedem `REPLY` richtig sein muss. Der Sidecar-Slot ist diese Einschränkung **von selbst**:
//!    ein Slot gehört genau einem Gast, und der Kernel schreibt nur aus dem Slot in den Frame des
//!    Gastes, dem er gehört.
//!
//! **Was die Sidecar-Form NICHT kann, und es ist genau eine Sache:** sie braucht **einen Slot je
//! gebundenem Gast-Thread**, nicht einen je Handler-Thread. Bei Fuchsia gehört das Sidecar dem
//! Thread, der `restricted mode` betritt — es gibt dort nichts umzuhängen. Hier ist der Handler
//! eine eigene PD mit eigenen Threads, an der mehrere Gäste hängen; ein einziges Fenster für alle
//! wäre ein Rennen zwischen zwei gleichzeitigen Gast-Syscalls. Der Preis ist Speicher
//! ([`SLOT_BYTES`] je Gast) und eine Schranke ([`BindUrteil::KeinSlot`]), nicht Umhängen.
//!
//! ## Was dieses Primitiv NICHT vergibt — ausdrücklich
//!
//! Z26/Nachtrag 2 zählt vier Autoritäten auf, die eine Linux-Persönlichkeit braucht. Dieses
//! Primitiv liefert **eine und eine halbe**:
//!
//! | Autorität | hier | Folge, wenn sie fehlt |
//! |---|---|---|
//! | Frame **lesen und schreiben** | **ja**, über das Sidecar (ganzer Frame) | — `rt_sigreturn` und `clone` sind damit möglich |
//! | Gast-**Speicher** lesen/schreiben (Zeigerargumente) | **nein** | jeder Syscall mit Zeiger (`read`, `write`, `openat`, `ioctl`) ist **nicht implementierbar**, solange die Gast-Region nicht getrennt in die Handler-PD gemappt ist |
//! | **Vspace** des Gastes manipulieren | **nein** | `mmap`/`mprotect`/Demand-Paging sind **nicht implementierbar** |
//! | Faults **sehen** | **ja**, getrennte Cap ([`Anlass::Fault`]) | — |
//!
//! Die beiden Neins sind **kein Versehen und keine Sparsamkeit**: sie sind eigene Caps mit eigenen
//! Entwurfsfragen (welche Cap trägt „der ganze Adressraum von G"? ein Kernel-Kopierdienst kostet
//! TCB je Syscall). Sie hier mitzunehmen hiesse, drei Entscheidungen in einer Zeile zu treffen —
//! dieselbe Form wie ein Wert mit zwei Bedeutungen. Sie stehen als offener Punkt in `todo.md`.
//!
//! ## Der Adressraumwechsel ist eine OFFENE GRÖSSE, kein Nebensatz
//!
//! Bei Fuchsia teilen sich Gast und `starnix_kernel` **einen** Adressraum (untere Hälfte Gast,
//! obere Hälfte Supervisor); ein Syscall-Rundlauf kostet dort einen **Moduswechsel**, keinen
//! Adressraumwechsel. Diese Fassung setzt den Handler in eine **eigene PD** und wechselt damit
//! **zweimal je umgeleitetem Syscall**.
//!
//! **Das ist eine Entwurfsentscheidung und sie ist vor dem Bau gefallen**, mit Grund: Caprocks
//! isolierte PDs teilen sich die statischen oberen Tabellen (`ISO_PD_HIGH`, GiB 1..3). Einen
//! Handler dort einzublenden gäbe ihn **jeder** isolierten PD — genau die Falle, die in
//! `CLAUDE.md` unter „Geteilte Seitenverzeichnisse vertragen keine PD-spezifischen Einträge"
//! steht. Die Starnix-Form wäre hier also nicht billig, sondern verlangte private obere Tabellen
//! je Gast; und sie setzte den Kernel des Gastes **in den Adressraum des Gastes**, was der
//! Trennung widerspricht, um derentwillen das Ganze cap-förmig ist.
//!
//! Der Preis ist damit benannt und **nicht gemessen**: zwei Adressraumwechsel je Umlauf, auf x86
//! ohne PCID also zweimal ein vollständig geleerter TLB (Z18 (3)). Die Schwelle, ab der die
//! cap-förmige Fassung fällt, steht in `todo.md` Z26/A3 — **vor** der Messung.
//!
//! [Z26]: ../../../todo.md

#![allow(clippy::needless_range_loop)]

// ---------------------------------------------------------------------------------------
// Sidecar-Arithmetik
// ---------------------------------------------------------------------------------------

/// **Bytes je Sidecar-Slot.** Ein Slot fasst den Trap-Frame **eines** gebundenen Gast-Threads.
///
/// Die Zahl ist hergeleitet, nicht gewählt: der grösste Frame der beiden Architekturen ist
/// aarch64 mit 34 `u64` = 272 Bytes (x86_64: 22 `u64` = 176). 512 ist die nächste Zweierpotenz
/// darüber — die Potenz zählt, weil der Offset auf dem **heissen Pfad** eine Schiebung sein soll
/// und keine Multiplikation.
pub const SLOT_BYTES: usize = 512;

/// Grösster Frame, der in einen Slot passen muss (aarch64: 34 `u64`). Steht hier, damit die
/// Behauptung „512 reicht" **prüfbar** ist statt behauptet — s. `slot_fasst_beide_frames`.
pub const FRAME_MAX_BYTES: usize = 272;

/// Byte-Offset des Slots `slot` im Sidecar-Fenster.
///
/// **Ohne Schranke** — die Schranke ist [`slot_gueltig`], und sie steht getrennt, weil sie an der
/// **Vergabe** geprüft gehört und nicht erst beim Zugriff. Wer beides in eine Funktion legt, hat
/// eine Funktion, die im Fehlerfall entweder lügt (0 zurückgeben) oder panickt.
#[inline]
pub const fn slot_offset(slot: u16) -> usize {
    (slot as usize) * SLOT_BYTES
}

/// Ist `slot` in einem Fenster mit `slots` Plätzen gültig?
///
/// Der Fehler, gegen den das steht, ist ein Off-by-one: Slot `slots` läge **hinter** dem Fenster,
/// Slot `slots-1` ist der letzte. Ein Slot daneben heisst hier nicht „Absturz", sondern **ein Gast
/// schreibt in den Frame eines anderen** — die Klasse Fehler, die stumm bleibt.
#[inline]
pub const fn slot_gueltig(slot: u16, slots: u16) -> bool {
    slot < slots
}

/// Wie viele Slots passen in ein Fenster von `len` Bytes? (Abgerundet.)
#[inline]
pub const fn slots_in(len: u64) -> u16 {
    let n = len / (SLOT_BYTES as u64);
    if n > u16::MAX as u64 {
        u16::MAX
    } else {
        n as u16
    }
}

/// Deckt das Fenster `[0, len)` die Slots `0..slots` **vollständig** ab?
///
/// Die Richtung ist wichtig: geprüft wird, ob das **letzte Byte des letzten Slots** noch drin
/// liegt — nicht, ob der Slot-**Anfang** drin liegt. Ein Fenster von 600 Bytes hat Platz für
/// einen Slot, nicht für zwei, obwohl der zweite Slot bei Offset 512 anfängt.
#[inline]
pub const fn fenster_deckt(slots: u16, len: u64) -> bool {
    (slots as u64) * (SLOT_BYTES as u64) <= len
}

// ---------------------------------------------------------------------------------------
// Die Bindung
// ---------------------------------------------------------------------------------------

/// **„Die Syscalls dieses Threads gehen an H."** Steht im TCB, wandert also bei einer Migration
/// mit dem Thread mit (`Migrant` trägt den ganzen `Tcb`).
///
/// `sys_ep` und `fault_ep` sind **getrennt**, weil sie verschiedene Autoritäten sind: einen
/// Syscall zu beantworten heisst „ich bin der Kernel dieses Gastes", einen Seitenfehler zu sehen
/// heisst „ich verwalte seinen Speicher". Z26 nennt den überladenen Kanal als die Form, die dieses
/// Projekt dreimal bezahlt hat (`blocked`, die Park-Naht, `CR0.TS`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bindung {
    /// Endpoint, an dem die `SyscallHandler`-Cap hängt. [`KEIN_EP`] = kein Syscall-Handler.
    pub sys_ep: u32,
    /// Endpoint, an dem die `FaultHandler`-Cap hängt. [`KEIN_EP`] = kein Fault-Handler.
    pub fault_ep: u32,
    /// PD des Handlers — die Kante im Zyklus-Graphen.
    pub handler_pd: u16,
    /// Sidecar-Slot **dieses** Gastes. Genau einer, und nur der Kernel schreibt hinein.
    pub slot: u16,
}

/// „Kein Endpoint" — ein Wert, den die Endpoint-Tabelle nie vergibt.
pub const KEIN_EP: u32 = u32::MAX;

impl Bindung {
    /// Hat dieser Thread einen **Syscall**-Handler?
    #[inline]
    pub const fn hat_syscall(&self) -> bool {
        self.sys_ep != KEIN_EP
    }
    /// Hat dieser Thread einen **Fault**-Handler?
    #[inline]
    pub const fn hat_fault(&self) -> bool {
        self.fault_ep != KEIN_EP
    }
    /// Ist diese Bindung **wirkungslos** (weder Syscall noch Fault)? Eine solche Bindung darf gar
    /// nicht erst entstehen — sie wäre eine Kante im Zyklus-Graphen ohne Wirkung, also eine
    /// Absage, die nach Erfolg aussieht.
    #[inline]
    pub const fn ist_leer(&self) -> bool {
        !self.hat_syscall() && !self.hat_fault()
    }
}

/// Anlass einer Umleitung — steht in der Nachricht an den Handler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u64)]
pub enum Anlass {
    /// Der Gast hat einen Syscall abgesetzt.
    Syscall = 0,
    /// Der Gast hat gefaultet (Seitenfehler, illegaler Befehl, …).
    Fault = 1,
}

// ---------------------------------------------------------------------------------------
// Die Weiche — die Regel, ohne die es eine RECHTEAUSWEITUNG wäre
// ---------------------------------------------------------------------------------------

/// Wohin geht dieser Eintritt?
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weiche {
    /// Der Caprock-Kernel bearbeitet, wie ohne Umleitung.
    ///
    /// **Für Syscalls nur zulässig, wenn gar keine Bindung besteht.** Eine *weggefallene* Bindung
    /// darf nie hierher führen: aus dem Entzug einer Cap würde sonst eine **Beförderung** — der
    /// Gast spräche plötzlich direkt mit dem Kernel, mit der vollen nativen ABI, die ihm die
    /// Bindung gerade genommen hatte.
    Kernel,
    /// Zustellen an diesen Endpoint, mit diesem Sidecar-Slot.
    Handler { ep: u32, slot: u16 },
    /// Die Bindung besteht, der Handler ist weg → der Gast **faultet**, mit benanntem Code.
    /// Nie still: eine Absage ohne Namen ist von einem Hänger nicht zu unterscheiden.
    Fault(u64),
}

/// ABI-Code, mit dem ein Gast faultet, dessen Handler weggefallen ist.
/// Spiegelt `caprock_abi::result::ERR_HANDLER_GONE` — die Zahl steht hier noch einmal, weil diese
/// Datei abhängigkeitsfrei sein muss; `abi_codes_stimmen_ueberein` im Kernel hält beide zusammen.
pub const ERR_HANDLER_GONE: u64 = 11;

/// **Die Weiche für einen SYSCALL.**
///
/// Drei Fälle, und der dritte ist die ganze Regel:
///
/// | Bindung | Handler lebt | → |
/// |---|---|---|
/// | keine | — | [`Weiche::Kernel`] |
/// | ja | ja | [`Weiche::Handler`] |
/// | ja | **nein** | [`Weiche::Fault`] — **nicht** `Kernel` |
///
/// Die naheliegende Fassung des dritten Falls („dann halt wie früher") ist genau die
/// Rechteausweitung. Sie sieht aus wie Robustheit und ist das Gegenteil.
#[inline]
pub fn weiche_syscall(bindung: Option<Bindung>, handler_lebt: bool) -> Weiche {
    match bindung {
        None => Weiche::Kernel,
        Some(b) if !b.hat_syscall() => Weiche::Kernel,
        Some(_) if !handler_lebt => Weiche::Fault(ERR_HANDLER_GONE),
        Some(b) => Weiche::Handler {
            ep: b.sys_ep,
            slot: b.slot,
        },
    }
}

/// **Die Weiche für einen FAULT** — und sie ist nicht die Spiegelung der obigen.
///
/// Der Unterschied ist der Punkt: bei einem Syscall ist „der Kernel macht es" eine **Beförderung**
/// (der Gast bekommt die native ABI zurück), bei einem Fault ist es eine **Herabstufung** (der
/// Kernel tötet den Thread). Fail-closed heisst deshalb nicht in beiden Fällen dasselbe.
///
/// | Fault-Bindung | Handler lebt | → | warum |
/// |---|---|---|---|
/// | keine | — | [`Weiche::Kernel`] | der native Fault-Pfad gewährt nichts; er beendet |
/// | ja | ja | [`Weiche::Handler`] | |
/// | ja | **nein** | [`Weiche::Fault`] | der Gast stirbt **benannt** statt als gewöhnlicher Fault — „dein Kernel ist weg" und „du hast Mist gebaut" sind zwei Diagnosen |
#[inline]
pub fn weiche_fault(bindung: Option<Bindung>, handler_lebt: bool) -> Weiche {
    match bindung {
        None => Weiche::Kernel,
        Some(b) if !b.hat_fault() => Weiche::Kernel,
        Some(_) if !handler_lebt => Weiche::Fault(ERR_HANDLER_GONE),
        Some(b) => Weiche::Handler {
            ep: b.fault_ep,
            slot: b.slot,
        },
    }
}

// ---------------------------------------------------------------------------------------
// Das Zyklusverbot — BAUPFLICHT, nicht Doku (Z26, Nachtrag 3)
// ---------------------------------------------------------------------------------------

/// Urteil über eine gewünschte Bindung. **Jede Absage hat einen eigenen Namen** — eine stille
/// Absage wäre von einem Erfolg nicht zu unterscheiden, und bei einer Bindung heisst das: der
/// Gast läuft weiter mit der nativen ABI, während der Aufrufer glaubt, er sei umgeleitet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindUrteil {
    /// Zulässig.
    Ok,
    /// **A behandelt B behandelt A.** Der Handler (oder einer weiter oben in seiner Kette) wird
    /// selbst von der Gast-PD behandelt — ein Deadlock per Konstruktion: der Handler kann seinen
    /// eigenen `RECV` nicht absetzen, ohne dass der umgeleitet wird.
    Zyklus,
    /// Die Gast-PD ist ihr **eigener** Handler. Sonderfall von [`Self::Zyklus`] mit Kettenlänge 0,
    /// eigener Name, weil er die häufigste Tippfehler-Form ist und im Audit unterscheidbar sein
    /// soll.
    SelbstBindung,
    /// Die Gast-PD hat bereits einen **anderen** Handler. Eine PD hat höchstens einen Kernel;
    /// zwei Persönlichkeiten über einem Adressraum wären zwei Wahrheiten über denselben Speicher.
    /// (Dieselbe Bindung noch einmal ist [`Self::Ok`] — idempotent.)
    FremderHandler,
    /// Die Kette ist länger als es PDs gibt. Das heisst: es gibt **bereits** einen Zyklus, an dem
    /// die Gast-PD nicht beteiligt ist — also einen Kernelfehler, keinen Aufruferfehler.
    /// Fail-closed abgewiesen, aber **getrennt zählbar**: nach aussen derselbe ABI-Code, im Audit
    /// eine andere Zeile. Wer beides zusammenwirft, verliert genau den Fall, der einen Fehler im
    /// Kernel meldet.
    KetteZuLang,
    /// Kein freier Sidecar-Slot im Fenster des Handlers.
    KeinSlot,
    /// Die gewünschte Bindung wäre **wirkungslos** (weder Syscall- noch Fault-Handler). Sie
    /// entstünde als Kante im Graphen ohne Wirkung — eine Absage, die nach Erfolg aussieht.
    Wirkungslos,
}

/// **Prüft die gewünschte Kante `gast -> handler` im Handler-Graphen.**
///
/// Der Graph ist **funktional** — jede PD hat höchstens eine ausgehende Kante —, und das ist eine
/// Entwurfsentscheidung, keine Vereinfachung: eine PD ist EIN Adressraum und hat höchstens EINEN
/// Kernel. Aus der Funktionalität folgt, dass die Zyklusprüfung ein **Gang** ist und keine Suche:
/// kein Hilfsspeicher, keine Besuchsmarken, O(Kettenlänge) — auf dem Syscall-Pfad tragbar.
///
/// `kante(pd)` liefert die ausgehende Kante, `n_knoten` die Knotenzahl (die Schrittschranke).
/// **Bewusst eine Funktion und kein Array:** die PD-Tabelle hat `NPDS = 10 000` Einträge; sie in
/// einen Puffer zu kopieren, um darin zu laufen, wäre 20 KiB Stack je `SETHANDLER` — und ein
/// zweites Abbild einer Wahrheit, die schon existiert.
pub fn pruefe_bindung(
    gast_pd: u16,
    handler_pd: u16,
    hat_syscall: bool,
    hat_fault: bool,
    kante: impl Fn(u16) -> Option<u16>,
    n_knoten: usize,
    freie_slots: u16,
) -> BindUrteil {
    if !hat_syscall && !hat_fault {
        return BindUrteil::Wirkungslos;
    }
    if gast_pd == handler_pd {
        return BindUrteil::SelbstBindung;
    }
    // Schon gebunden? Dieselbe Kante noch einmal ist zulässig (idempotent, z. B. wenn ein zweiter
    // Thread derselben PD gebunden wird); eine ANDERE ist es nicht.
    if let Some(vorhanden) = kante(gast_pd) {
        if vorhanden != handler_pd {
            return BindUrteil::FremderHandler;
        }
    }
    if freie_slots == 0 {
        return BindUrteil::KeinSlot;
    }
    // Der Gang: vom Handler aus die Kette hinauf. Erreicht sie den Gast, wäre der Kreis
    // geschlossen. Die Schranke ist die Knotenzahl -- mehr Schritte als Knoten heisst, dass wir
    // schon in einem Kreis laufen, den es vor diesem Aufruf gab.
    let mut k = handler_pd;
    let mut schritte = 0usize;
    loop {
        if k == gast_pd {
            return BindUrteil::Zyklus;
        }
        let Some(naechster) = kante(k) else {
            return BindUrteil::Ok; // Kettenende erreicht, ohne den Gast zu treffen
        };
        k = naechster;
        schritte += 1;
        if schritte > n_knoten {
            return BindUrteil::KetteZuLang;
        }
    }
}

/// Länge der Handler-Kette ab `pd` — reine Auskunft für den Bericht (**nicht** für
/// Entscheidungen). `None` heisst „läuft im Kreis"; genau diese Unterscheidung fehlt einer Zahl,
/// die im Kreisfall einfach die Schranke zurückgibt.
pub fn kettenlaenge(pd: u16, kante: impl Fn(u16) -> Option<u16>, n_knoten: usize) -> Option<usize> {
    let mut k = pd;
    let mut n = 0usize;
    while let Some(naechster) = kante(k) {
        k = naechster;
        n += 1;
        if n > n_knoten {
            return None;
        }
    }
    Some(n)
}

// ---------------------------------------------------------------------------------------
// Tests — die Fallen mit Literalen, ohne Maschine
// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn b(sys: u32, fault: u32, hpd: u16, slot: u16) -> Bindung {
        Bindung {
            sys_ep: sys,
            fault_ep: fault,
            handler_pd: hpd,
            slot,
        }
    }

    // --- Sidecar ---------------------------------------------------------------------------

    #[test]
    fn slot_fasst_beide_frames() {
        // Die Behauptung „512 reicht" ist hier PRUEFBAR, nicht bloss im Kommentar. x86_64:
        // 22 u64 = 176; aarch64: 34 u64 = 272 -- die Zahlen stehen in den beiden HAL-Dateien.
        assert!(FRAME_MAX_BYTES <= SLOT_BYTES);
        assert_eq!(22 * 8, 176);
        assert_eq!(34 * 8, FRAME_MAX_BYTES);
        // Zweierpotenz: der Offset ist eine Schiebung, keine Multiplikation.
        assert_eq!(SLOT_BYTES & (SLOT_BYTES - 1), 0);
    }

    #[test]
    fn nachricht_kann_den_frame_nicht_tragen() {
        // Der gemessene Grund fuer das Sidecar: vier Nachrichtenwoerter gegen 22 bzw. 34.
        // `caprock_abi::MSG_WORDS` ist 4; die Zahl steht hier als Literal, weil diese Datei
        // abhaengigkeitsfrei ist -- `abi_codes_stimmen_ueberein` im Kernel haelt sie zusammen.
        let msg_words = 4usize;
        assert!(msg_words * 8 < 22 * 8, "x86-Frame passt nicht in eine Nachricht");
        assert!(msg_words * 8 < FRAME_MAX_BYTES, "aarch64-Frame erst recht nicht");
    }

    #[test]
    fn slot_offsets_ueberlappen_nicht() {
        for s in 0u16..64 {
            let a = slot_offset(s);
            let z = slot_offset(s + 1);
            assert_eq!(z - a, SLOT_BYTES);
            assert!(a + FRAME_MAX_BYTES <= z, "Slot {s} ragt in den naechsten");
        }
    }

    #[test]
    fn slot_schranke_ist_exklusiv() {
        // Der Off-by-one, gegen den die Funktion steht: Slot `slots` liegt HINTER dem Fenster.
        assert!(slot_gueltig(0, 1));
        assert!(!slot_gueltig(1, 1));
        assert!(slot_gueltig(7, 8));
        assert!(!slot_gueltig(8, 8));
        assert!(!slot_gueltig(0, 0), "ein Fenster ohne Plaetze hat keinen gueltigen Slot");
    }

    #[test]
    fn fenster_deckt_den_letzten_slot_ganz() {
        // 600 Bytes: der zweite Slot FAENGT bei 512 an, endet aber bei 1024 -- er passt nicht.
        // Wer nur den Anfang prueft, gibt hier zwei Plaetze frei und laesst einen Gast ueber die
        // Fensterkante schreiben.
        assert!(fenster_deckt(1, 600));
        assert!(!fenster_deckt(2, 600));
        assert!(fenster_deckt(2, 1024));
        assert_eq!(slots_in(600), 1);
        assert_eq!(slots_in(1024), 2);
        assert_eq!(slots_in(4096), 8);
        assert_eq!(slots_in(0), 0);
    }

    // --- Die Weiche ------------------------------------------------------------------------

    #[test]
    fn ohne_bindung_bearbeitet_der_kernel() {
        assert_eq!(weiche_syscall(None, true), Weiche::Kernel);
        assert_eq!(weiche_syscall(None, false), Weiche::Kernel);
        assert_eq!(weiche_fault(None, true), Weiche::Kernel);
    }

    #[test]
    fn mit_lebendem_handler_wird_umgeleitet() {
        let x = b(7, 9, 3, 2);
        assert_eq!(weiche_syscall(Some(x), true), Weiche::Handler { ep: 7, slot: 2 });
        assert_eq!(weiche_fault(Some(x), true), Weiche::Handler { ep: 9, slot: 2 });
    }

    #[test]
    fn weggefallener_handler_faultet_und_faellt_nicht_zurueck() {
        // **Die Regel, ohne die es eine Rechteausweitung waere.** Aus dem Entzug einer Cap darf
        // keine Befoerderung werden.
        let x = b(7, 9, 3, 2);
        assert_eq!(weiche_syscall(Some(x), false), Weiche::Fault(ERR_HANDLER_GONE));
        assert_eq!(weiche_fault(Some(x), false), Weiche::Fault(ERR_HANDLER_GONE));
    }

    #[test]
    fn bindung_vorhanden_heisst_niemals_kernel() {
        // Die Aussage ueber das GANZE Kreuzprodukt, nicht ueber drei Beispiele. Sie ist die
        // eigentliche Zusicherung des Primitivs: wer gebunden ist, erreicht den Caprock-Kernel
        // nicht mehr -- gleich, was mit dem Handler passiert ist.
        for &lebt in &[true, false] {
            for &sys in &[0u32, 1, 4242] {
                for &flt in &[0u32, 1, 4242] {
                    let x = b(sys, flt, 3, 0);
                    assert_ne!(
                        weiche_syscall(Some(x), lebt),
                        Weiche::Kernel,
                        "sys={sys} fault={flt} lebt={lebt}"
                    );
                    assert_ne!(weiche_fault(Some(x), lebt), Weiche::Kernel);
                }
            }
        }
    }

    #[test]
    fn halbe_bindung_leitet_nur_ihre_haelfte_um() {
        // Nur Syscall-Handler: Faults bleiben beim Kernel (der Gast stirbt nativ) -- und das ist
        // KEINE Ausweitung, denn der native Fault-Pfad gewaehrt nichts.
        let nur_sys = b(7, KEIN_EP, 3, 0);
        assert_eq!(weiche_syscall(Some(nur_sys), true), Weiche::Handler { ep: 7, slot: 0 });
        assert_eq!(weiche_fault(Some(nur_sys), true), Weiche::Kernel);
        assert_eq!(weiche_fault(Some(nur_sys), false), Weiche::Kernel);
        // Nur Fault-Handler: Syscalls bleiben nativ. Das ist die Form, in der ein Debugger oder
        // ein Speicherserver arbeitet, ohne Persoenlichkeit zu sein.
        let nur_flt = b(KEIN_EP, 9, 3, 0);
        assert_eq!(weiche_syscall(Some(nur_flt), true), Weiche::Kernel);
        assert_eq!(weiche_fault(Some(nur_flt), true), Weiche::Handler { ep: 9, slot: 0 });
    }

    #[test]
    fn leere_bindung_ist_erkennbar() {
        assert!(b(KEIN_EP, KEIN_EP, 3, 0).ist_leer());
        assert!(!b(0, KEIN_EP, 3, 0).ist_leer());
        assert!(!b(KEIN_EP, 0, 3, 0).ist_leer());
        // Endpoint 0 ist ein GUELTIGER Endpoint. Wer `0` als „keiner" nimmt, verliert ihn.
        assert!(b(0, 0, 3, 0).hat_syscall());
    }

    // --- Das Zyklusverbot ------------------------------------------------------------------

    /// Kantenmenge bauen: `paare` = (pd, handler_pd).
    fn g(n: usize, paare: &[(u16, u16)]) -> Vec<Option<u16>> {
        let mut k = vec![None; n];
        for &(a, h) in paare {
            k[a as usize] = Some(h);
        }
        k
    }

    /// Aus einer Kantentabelle die Zugriffsfunktion machen. Eine PD-Id ausserhalb der Tabelle
    /// liefert `None` -- genau das Verhalten, das der Kernel mit `handler_pd_of` hat.
    fn f(k: &[Option<u16>]) -> impl Fn(u16) -> Option<u16> + '_ {
        move |p: u16| k.get(p as usize).copied().flatten()
    }

    /// Kurzform: `pruefe_bindung` gegen eine Kantentabelle.
    fn pb(gast: u16, h: u16, sys: bool, flt: bool, k: &[Option<u16>], frei: u16) -> BindUrteil {
        pruefe_bindung(gast, h, sys, flt, f(k), k.len(), frei)
    }

    #[test]
    fn frische_bindung_ist_zulaessig() {
        let k = g(8, &[]);
        assert_eq!(pb(1, 2, true, false, &k, 4), BindUrteil::Ok);
    }

    #[test]
    fn selbstbindung_wird_abgewiesen() {
        let k = g(8, &[]);
        assert_eq!(pb(1, 1, true, true, &k, 4), BindUrteil::SelbstBindung);
    }

    #[test]
    fn zweierzyklus_wird_abgewiesen() {
        // A behandelt B; jetzt soll B auch A behandeln -> der Fall, den Nachtrag 3 nennt.
        let k = g(8, &[(2, 1)]); // PD 2 wird von PD 1 behandelt
        assert_eq!(pb(1, 2, true, false, &k, 4), BindUrteil::Zyklus);
    }

    #[test]
    fn laengerer_zyklus_wird_abgewiesen() {
        // 3 -> 2 -> 1 besteht; 1 -> 3 schloesse den Kreis. Eine Pruefung, die nur den DIREKTEN
        // Partner ansieht, laesst das durch -- und dann haengt die ganze Kette beim ersten
        // Syscall.
        let k = g(8, &[(3, 2), (2, 1)]);
        assert_eq!(pb(1, 3, true, false, &k, 4), BindUrteil::Zyklus);
    }

    #[test]
    fn gestapelte_persoenlichkeiten_bleiben_erlaubt() {
        // Die BILLIGE Absage aus Z26 („Threads einer PD mit Handler-Bindung duerfen selbst nicht
        // gebunden werden") verboete das hier -- 3 -> 2 -> 1 ist azyklisch und legitim: eine
        // Persoenlichkeit ueber einer Persoenlichkeit. Gebaut ist deshalb die allgemeine
        // Azyklizitaet, nicht die billige Form.
        let k = g(8, &[(2, 1)]);
        assert_eq!(pb(3, 2, true, false, &k, 4), BindUrteil::Ok);
        let k2 = g(8, &[(3, 2), (2, 1)]);
        assert_eq!(kettenlaenge(3, f(&k2), k2.len()), Some(2));
    }

    #[test]
    fn zweiter_handler_fuer_dieselbe_pd_wird_abgewiesen() {
        let k = g(8, &[(1, 2)]);
        assert_eq!(pb(1, 4, true, false, &k, 4), BindUrteil::FremderHandler);
        // ... dieselbe noch einmal ist idempotent (zweiter Thread derselben Gast-PD).
        assert_eq!(pb(1, 2, true, false, &k, 4), BindUrteil::Ok);
    }

    #[test]
    fn vorbestehender_zyklus_ist_unterscheidbar() {
        // 5 -> 6 -> 5 ist ein Kreis, an dem der Gast (1) nicht beteiligt ist. Der Gang laeuft in
        // ihn hinein und trifft den Gast nie. Ohne die Schrittschranke: Endlosschleife IM
        // SYSCALL-PFAD. Mit ihr: eine eigene, zaehlbare Meldung -- ein KERNELfehler, kein
        // Aufruferfehler, und die beiden duerfen nicht denselben Namen tragen.
        let k = g(8, &[(5, 6), (6, 5)]);
        assert_eq!(pb(1, 5, true, false, &k, 4), BindUrteil::KetteZuLang);
        assert_eq!(kettenlaenge(5, f(&k), k.len()), None);
    }

    #[test]
    fn wirkungslose_bindung_wird_abgewiesen() {
        // Weder Syscall- noch Fault-Handler: eine Kante ohne Wirkung. Sie durchzulassen hiesse,
        // eine Absage als Erfolg zu melden -- der Aufrufer glaubte, der Gast sei umgeleitet.
        let k = g(8, &[]);
        assert_eq!(pb(1, 2, false, false, &k, 4), BindUrteil::Wirkungslos);
    }

    #[test]
    fn ohne_slot_keine_bindung() {
        let k = g(8, &[]);
        assert_eq!(pb(1, 2, true, false, &k, 0), BindUrteil::KeinSlot);
    }

    #[test]
    fn unbekannte_pd_laeuft_nicht_ins_leere() {
        // `kanten.get` statt `kanten[..]`: eine PD-Id ausserhalb der Tabelle darf nicht panicken.
        // Sie kommt aus einer cap-geprueften Quelle -- aber „kommt aus einer geprueften Quelle"
        // ist genau die Annahme, die in diesem Projekt schon dreimal falsch war.
        let k = g(4, &[]);
        assert_eq!(pb(1, 99, true, false, &k, 4), BindUrteil::Ok);
        assert_eq!(pb(99, 1, true, false, &k, 4), BindUrteil::Ok);
        assert_eq!(kettenlaenge(99, f(&k), k.len()), Some(0));
    }

    #[test]
    fn jede_absage_hat_einen_eigenen_namen() {
        // Die Aussage, die den Bericht traegt: sechs unterscheidbare Absagen, keine zwei gleich.
        // Waeren zwei davon derselbe Wert, koennte der Audit sie nicht trennen -- und genau die
        // Trennung „Kernelfehler gegen Aufruferfehler" haengt daran.
        let alle = [
            BindUrteil::Ok,
            BindUrteil::Zyklus,
            BindUrteil::SelbstBindung,
            BindUrteil::FremderHandler,
            BindUrteil::KetteZuLang,
            BindUrteil::KeinSlot,
            BindUrteil::Wirkungslos,
        ];
        for i in 0..alle.len() {
            for j in (i + 1)..alle.len() {
                assert_ne!(alle[i], alle[j]);
            }
        }
    }
}
