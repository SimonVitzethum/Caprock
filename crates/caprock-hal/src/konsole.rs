//! **Die Schreibordnung der Debug-Konsole — arch-neutral.** (C9b)
//!
//! # Der Befund, und die Messung, die den naheliegenden Weg ausgeschlossen hat
//!
//! Bis zum 2026-08-13 hielt `_print` auf beiden Architekturen **eine** `SpinLock`-Haltung ueber
//! das gesamte `write_fmt`. `SpinLock::lock` maskiert die IRQs des eigenen Kerns — also maskierte
//! **jedes `println!`** die Interrupts so lange, wie die Zeile zum UART brauchte. Gemessen
//! (x86 unter KVM, `sperre`-Zeile): **69 100 174 Zyklen = 2464 Promille eines Timer-Ticks**, also
//! das 2,5-Fache der Schwelle, ab der ein Praemptionsverlust *beweisbar* ist (todo C9b).
//!
//! Der Auftrag nannte drei Wege. **Zwei davon sind durch eine Messung ausgeschieden**, und das
//! ist die eigentliche Begruendung fuer die hier gebaute Fassung. Gemessen wurde je Byte:
//!
//! | Anteil | Zyklen je Byte | Anteil an der Zeile |
//! |---|---|---|
//! | `core::fmt` (Formatierung, 12 Argumente) | **1,2** | **0,003 %** |
//! | `inb(LSR)` allein (ein Poll) | 14 847 | — |
//! | Poll + `outb` (ein ausgegebenes Byte) | **40 754** | **99,997 %** |
//!
//! * **„Die Formatierung aus der Sperre heben"** haette also 0,003 % der Haltung entfernt. Der
//!   Weg klingt richtig und ist an dieser Stelle wirkungslos — die Kosten stecken nicht in
//!   `core::fmt`, sondern in zwei VM-Exits je Byte.
//! * **„Die Sperre je ZEILE nehmen"** ist bereits der Ist-Zustand: der Hoechststand stammt aus
//!   **einer** `println!`-Zeile (1623 Zeichen). Feiner als eine Zeile ist die Aufteilung, die
//!   wirkt.
//! * Und die Zahl ist **keine Eigenschaft von QEMU**: auf echtem Blech kostet ein Byte bei
//!   115 200 Baud rund 86,8 us, eine 1623-Byte-Zeile also **141 ms = 14 Ticks**. Eine
//!   synchron-pollend ausgegebene lange Zeile kann unter einer Maske nirgends unter einem Tick
//!   bleiben. Die Maske muss weg, nicht die Formatierung.
//!
//! # Was hier steht: BLOCKWEISE Haltung unter einem Besitzrecht ueber die ganze Nachricht
//!
//! Zwei Dinge, die vorher **ein** Mechanismus waren, sind hier getrennt:
//!
//! 1. **Die Unteilbarkeit einer Nachricht** traegt ein Besitzrecht ([`Schreibordnung::besitzer`]),
//!    ein einfaches `compare_exchange` **ohne jede IRQ-Maskierung**. Es wird ueber das ganze
//!    `write_fmt` gehalten — zwei Kerne koennen ihre Zeilen also nach wie vor nicht ineinander
//!    schieben.
//! 2. **Der Portzugriff** ist mit der `SpinLock`-Haltung geschuetzt, aber nur noch je
//!    [`BLOCK`] ausgegebener Bytes. Zwischen zwei Bloecken sind die IRQs wieder offen.
//!
//! **Der gewoehnliche Weg hat keinen Puffer**, und das ist eine Entscheidung: `write_fmt`
//! schreibt weiterhin synchron durch bis zum Port, nur eben in Bloecken. Wer `println!`
//! zurueckkehren sieht, hat die Bytes draussen — wie vorher. Einen Puffer gibt es ausschliesslich
//! fuer den einen Fall, der sonst eine Zeile ZERREISST (s. [`Schreibordnung::nachtragen`]), und
//! er kann nichts verschlucken: passt eine Nachricht nicht hinein, geht sie **roh** hinaus und
//! wird gezaehlt.
//!
//! Und aus demselben Grund bleibt die `SpinLock`-Haltung ueberhaupt bestehen, statt das
//! Besitzrecht allein arbeiten zu lassen: **so bleibt die Konsole fuer die Sperrhaltedauer-Marke
//! sichtbar.** Wer diese Datei eines Tages wieder zu einer Haltung zusammenzieht, bekommt sofort
//! wieder eine Zahl mit Adresse. Ein Pfad, der aus der Messung herausfaellt, ist nicht behoben —
//! er ist unbeobachtet.
//!
//! # Die Verklemmung, die dabei entstehen KOENNTE — und warum sie es nicht tut
//!
//! Zwischen zwei Bloecken sind die IRQs offen. Trifft dort eine Unterbrechung ein, die selbst
//! `println!` ruft, so faende sie das Besitzrecht in der Hand **des unterbrochenen Kontextes auf
//! demselben Kern** — und der laeuft erst weiter, wenn die Unterbrechung zurueckkehrt. Wer hier
//! wartet, wartet fuer immer. Das ist woertlich der reentrante Ticket-Deadlock, gegen den
//! `SpinLock::lock` maskiert (s. `caprock_sync`).
//!
//! [`Schreibordnung::nehmen`] entscheidet deshalb an genau **einer** Bedingung:
//!
//! ```text
//! Halter sitzt auf MEINEM Kern  UND  ich kam mit MASKIERTEN IRQs herein
//!     -> ich bin seine Unterbrechung -> NICHT warten, roh ausgeben, ZAEHLEN
//! ```
//!
//! Der zweite Halbsatz ist der Punkt. Ohne ihn traefe die Regel auch den harmlosen Fall „ein
//! *anderer* Faden auf meinem Kern haelt gerade" — der loest sich von selbst, weil ein Warten
//! mit offenen IRQs praemptierbar ist und der Halter wieder drankommt. Ihn ebenfalls auf die
//! rohe Ausgabe zu schicken hiesse, Ausgabe zu verwuerfeln, wo Warten gereicht haette.
//!
//! **Und dieser Rueckritt hat GEMESSEN Schaden angerichtet, bevor er einen Nachtrag bekam.**
//! Die erste Fassung gab in diesem Fall einfach roh aus. Ergebnis in 8 Suitenlaeufen: zweimal
//! eine zerrissene Zeile, und einmal fiel der Riss mitten in das **Ergebniswort** einer
//! Pruefzeile (`isohigh : ` ohne `SKIP`) — die Zeile fiel aus der Ergebnissignatur, und der Lauf
//! wich ab. Das ist die schlimmere Richtung: kein sichtbarer Fehler, sondern ein verlorener
//! Beleg. Gefunden hat es nicht das Gegenlesen, sondern der Zaehler [`Stand::risse`] zusammen mit
//! einer Marke im Protokoll.
//!
//! Seither wird die fremde Nachricht **nachgetragen** statt dazwischengeschrieben: sie geht
//! vollstaendig hinaus, unmittelbar nachdem der Halter seine eigene beendet hat. Beide Zeilen
//! bleiben ganz; getauscht wird nur ihre Reihenfolge. Was das kostet und wo seine Grenze liegt,
//! steht bei [`Schreibordnung::nachtragen`].
//!
//! # Die Notbremse — und warum sie zaehlt statt zu schweigen
//!
//! Ein Warten auf einen Halter auf demselben Kern loest sich nur auf, wenn der Halter wieder
//! eingeplant wird. Bei strikten Prioritaeten ist das nicht garantiert (dieses Projekt hat den
//! Fall schon gehabt: ein pollender Treiber auf hoeherer Prioritaet liess seinen Client
//! verhungern). Ein *Haenger in der Konsole* waere der denkbar schlechteste Ausgang — er saehe
//! aus wie D0/D6 und waere so gut wie nicht aufzuloesen. Nach [`WARTEKONTINGENT`] Runden gibt
//! [`Schreibordnung::nehmen`] deshalb auf, gibt roh aus und **zaehlt es getrennt**
//! ([`Stand::notbremse`]). Aufgeben ohne Zaehler waere ein stiller Ausgang; das Kontingent ohne
//! Aufgeben waere ein Haenger.

use caprock_sync::SpinLock;
use core::cell::UnsafeCell;
use core::fmt;
use core::panic::Location;
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

/// **Wieviele ausgegebene Bytes am Stueck unter der Portsperre liegen — EINES.**
///
/// Die Zahl stand erst auf 16 (die Tiefe des Sende-FIFOs), dann auf 4, und beide Male war die
/// Begruendung eine **Rechnung** ueber die Kosten je Byte. Die Messung hat beide widerlegt, und
/// zwar auf eine Art, die die Rechnung gar nicht kennen konnte:
///
/// | `BLOCK` | laengste gemessene Blockhaltung |
/// |---|---|
/// | 16 | 58 058 620 · 60 627 502 Zyklen |
/// | 4  | 58 386 592 · 63 059 691 Zyklen |
///
/// **Die Zahl haengt nicht an `BLOCK`** — sie ist also keine Bytekosten-Groesse. Der Grund steht
/// im Treiber: `put_byte` **pollt `LSR.THRE` und schreibt erst dann**, und das Warten lag mit
/// unter der Sperre. Steht der Sende-FIFO voll (Ausgabeflut) oder haengt das Backend des Wirts
/// (I/O-Last, D13), dann wartet dieser Poll Millisekunden — mit maskierten IRQs, ganz gleich wie
/// wenige Bytes der Block umfasst. Eine Schranke ueber der BYTEZAHL kann eine Groesse nicht
/// binden, die an der ZEIT bis `THRE` haengt.
///
/// Deshalb zwei Aenderungen, die zusammengehoeren: das Warten auf `THRE` liegt jetzt **vor** der
/// Maske (s. [`Blockschreiber::ausgeben`]), und ein Block ist **ein Byte**. Damit umfasst das
/// maskierte Fenster genau einen `outb` auf einen aufnahmebereiten Port — die kleinste Einheit,
/// die dieser Treiber ueberhaupt hat. Ein groesserer Block waere hier nicht bloss laenger,
/// sondern **unsicher**: nach EINEM `THRE` mehr als ein Byte zu schreiben hiesse, sich auf eine
/// FIFO-Tiefe zu verlassen (16550: 16 Byte ab `THRE`; PL011: `TXFF` sagt nur „ein Platz frei") —
/// und wer sich da irrt, verliert Bytes **still**.
pub(crate) const BLOCK: usize = 1;

/// Runden, die ein Schreiber auf das Besitzrecht wartet, bevor die Notbremse greift.
///
/// Grosszuegig gegen den laengsten *legitimen* Fall gerechnet: eine volle Zeile dauert unter KVM
/// rund 69 Mio. Zyklen, eine Warterunde (ein `load` + `spin_loop`) liegt in der Groessenordnung
/// von zehn Zyklen. Das Kontingent traegt damit ein Vielfaches der laengsten Zeile und greift im
/// gesunden Betrieb nie — gemessen ist es in keinem Lauf angesprochen worden.
const WARTEKONTINGENT: u64 = 1 << 30;

/// „Niemand schreibt gerade." Kein gueltiger Kernindex.
const FREI: usize = usize::MAX;

/// **Der Nachtragspuffer** — Platz fuer die Nachrichten, die waehrend einer laufenden Ausgabe
/// aus dem Trap-Kontext kommen.
///
/// Bemessen an der laengsten Zeile dieses Kernels (1623 Zeichen) mal zwei, plus Luft: eine
/// laufende Ausgabe wird hoechstens von einer Handvoll Diagnosezeilen unterbrochen. Ein
/// Ueberlauf ist damit praktisch unerreichbar -- und wenn er kommt, wird er **gezaehlt und roh
/// ausgegeben**, nicht verschluckt.
const NACHTRAG: usize = 4096;

/// Der Treiber, den die Architektur hereinreicht — zwei Funktionszeiger statt einer Generik.
///
/// Die Trennung ist der Kern der C9b-Behebung: **`bereit` darf beliebig lange warten und laeuft
/// deshalb OHNE Maske, `senden` ist kurz und laeuft unter ihr.** Solange beides in einer
/// Funktion steckte (`put_byte` = pollen und schreiben), lag das Warten zwangslaeufig mit unter
/// der Sperre, und keine Blockgroesse konnte das beschraenken.
#[derive(Clone, Copy)]
pub(crate) struct Treiber {
    /// Nimmt der Port jetzt ein Byte an? (16550: `LSR.THRE`; PL011: `!FR.TXFF`.)
    pub bereit: fn() -> bool,
    /// Ein Byte hinausschreiben. **Darf selbst noch pollen** — nach einem erfolgreichen
    /// [`Treiber::bereit`] kehrt der Poll sofort zurueck, weil in dieser Ordnung immer nur EIN
    /// Schreiber zugleich am Port steht. Der Treiber bleibt damit fuer sich genommen korrekt,
    /// auch wenn ihn jemand ohne `bereit` benutzt (der Panikpfad tut genau das).
    pub senden: fn(u8),
}

/// **Die CR/LF-Regel steht genau EINMAL.** Beide Architekturen und beide Pfade (gesperrt wie
/// roh) gehen hier durch; zwei Stellen mit derselben Regel waeren ein Riss, durch den ein
/// abweichendes Zeilenende faellt.
#[inline]
fn byte_ausgeben(t: Treiber, b: u8) {
    if b == b'\n' {
        (t.senden)(b'\r');
    }
    (t.senden)(b);
}

/// **Rohe, ungesperrte Ausgabe** — fuer den frueheren Bring-up, den Panikpfad und den
/// Rueckritt aus [`Schreibordnung::nehmen`]. Nimmt nichts, wartet auf nichts, kann nicht
/// verklemmen.
pub(crate) fn roh(t: Treiber, s: &str) {
    for b in s.bytes() {
        byte_ausgeben(t, b);
    }
}

/// Was die Schreibordnung gesehen hat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stand {
    /// Wieviele Bloecke insgesamt hinausgingen. **Sprechprobe:** `0` heisst, die Ordnung ist nie
    /// gelaufen — und dann sagen alle anderen Zahlen nichts.
    pub bloecke: u64,
    /// Wie oft ein Schreiber das Besitzrecht **besetzt angetroffen** hat. Das ist die
    /// GELEGENHEIT, nicht das Unglueck: sie ist in jedem Lauf zaehlbar, waehrend der Schaden
    /// selten ist (dieselbe Bauform wie der `pdbind`-Waechter).
    pub besetzt: u64,
    /// Wie oft auf die **rohe** Ausgabe zurueckgetreten wurde. **An dieser Zahl haengt
    /// „verwuerfelt": nur hier kann Ausgabe ineinander geraten.** Muss 0 sein.
    pub rueckritt: u64,
    /// **Die Stelle des ERSTEN Rueckritts** (`("", 0)` = keiner). Eine Zahl ohne Ort ist ein
    /// Alarm ohne Adresse — dieselbe Lehre, aus der die Sperrhaltedauer-Marke ihr
    /// `#[track_caller]` hat. Hier ist es die Stelle des `println!`, nicht die von `_print`.
    pub rueckritt_stelle: (&'static str, u32),
    /// **RISSE: Rueckritte, bei denen der Halter schon Bytes geschrieben hatte.**
    ///
    /// Das ist die Zahl, um die es bei „verwuerfelt" wirklich geht, und sie ist scharf von
    /// [`Stand::rueckritt`] getrennt. Ein Rueckritt an sich schadet nicht: hatte der Halter noch
    /// kein Byte draussen, geht die fremde Nachricht **vollstaendig vor** seiner hinaus, und
    /// beide Zeilen bleiben ganz. Erst wenn er schon mittendrin war, zerreisst die fremde
    /// Nachricht seine Zeile — und **nur dann** liest eine zeilenweise lesende Suite Unsinn.
    ///
    /// Die beiden zu vermengen waere derselbe Fehler wie „`rx_used` heisst, Daten sind
    /// angekommen": eine Zahl, die einen Vorgang zaehlt, beantwortet die Frage nach der Wirkung
    /// nicht.
    pub risse: u64,
    /// Davon: das Wartekontingent war erschoepft. Muss 0 sein.
    pub notbremse: u64,
    /// **Die laengste einzelne Blockhaltung, in Zyklen** — also die IRQ-maskierte Dauer, um die
    /// es bei C9b geht, an ihrer Quelle gemessen.
    ///
    /// **Warum diese Zahl hier entsteht und nicht bei der Sperrhaltedauer-Marke:** die kennt nur
    /// EIN Maximum je Kanal. Sobald die Konsole nicht mehr die laengste Haltung des Systems ist —
    /// und genau das ist das Ziel —, verschwindet sie aus deren Sicht, und der Erfolg waere nur
    /// noch **auszurechnen** statt zu messen. Ein Pruefer, der die gepruefte Groesse nachrechnet,
    /// prueft eine zweite Wirklichkeit (Fallenliste). Die beiden Zahlen bleiben vergleichbar: sie
    /// stammen aus derselben Uhr, und diese hier muss stets `<= ` dem ungefilterten Hoechststand
    /// der Marke sein.
    pub block_max: u64,
}

/// Die Schreibordnung einer Konsole (je Architektur eine Instanz als `static`).
pub(crate) struct Schreibordnung {
    /// Serialisiert den **Portzugriff** — kurz gehalten, ein Block je Haltung. Der einzige
    /// `SpinLock` dieser Datei, und damit die Stelle, an der die Sperrhaltedauer-Marke die
    /// Konsole weiterhin sieht.
    port: SpinLock<()>,
    /// Wer gerade eine **Nachricht** schreibt (Kernindex), oder [`FREI`]. Ohne IRQ-Maskierung —
    /// das ist der ganze Punkt.
    besitzer: AtomicUsize,
    bloecke: AtomicU64,
    besetzt: AtomicU64,
    rueckritt: AtomicU64,
    /// Die Stelle des **ersten** Rueckritts. Der erste, nicht der letzte: er ist der, den man
    /// untersuchen will, und er ueberschreibt sich nicht selbst.
    rueckritt_stelle: AtomicPtr<Location<'static>>,
    risse: AtomicU64,
    /// Die Bytes, die waehrend einer laufenden Ausgabe aus dem Trap-Kontext kamen. Sie gehen
    /// **nach** der laufenden Nachricht hinaus, ganz statt zerrissen.
    nachtrag: [UnsafeCell<u8>; NACHTRAG],
    /// Wieviele davon belegt sind. Reserviert wird mit `fetch_add`, damit auch ein
    /// verschachtelter Aufrufer einen eigenen, ueberschneidungsfreien Bereich bekommt.
    nachtrag_len: AtomicUsize,
    notbremse: AtomicU64,
    block_max: AtomicU64,
}

impl Schreibordnung {
    pub(crate) const fn neu() -> Self {
        Self {
            port: SpinLock::new(()),
            besitzer: AtomicUsize::new(FREI),
            bloecke: AtomicU64::new(0),
            besetzt: AtomicU64::new(0),
            rueckritt: AtomicU64::new(0),
            rueckritt_stelle: AtomicPtr::new(core::ptr::null_mut()),
            risse: AtomicU64::new(0),
            nachtrag: [const { UnsafeCell::new(0) }; NACHTRAG],
            nachtrag_len: AtomicUsize::new(0),
            notbremse: AtomicU64::new(0),
            block_max: AtomicU64::new(0),
        }
    }

    pub(crate) fn stand(&self) -> Stand {
        let p = self.rueckritt_stelle.load(Ordering::Acquire);
        let stelle = if p.is_null() {
            ("", 0)
        } else {
            // SAFETY: in `rueckritt_stelle` landet ausschliesslich ein `&'static Location` aus
            // `Location::caller()`; der Zeiger wird als Ganzes atomar geschrieben und gelesen.
            let l: &'static Location<'static> = unsafe { &*(p as *const Location<'static>) };
            (l.file(), l.line())
        };
        Stand {
            bloecke: self.bloecke.load(Ordering::Relaxed),
            besetzt: self.besetzt.load(Ordering::Relaxed),
            rueckritt: self.rueckritt.load(Ordering::Relaxed),
            rueckritt_stelle: stelle,
            risse: self.risse.load(Ordering::Relaxed),
            notbremse: self.notbremse.load(Ordering::Relaxed),
            block_max: self.block_max.load(Ordering::Relaxed),
        }
    }

    /// Einen Rueckritt verbuchen — mit seiner Stelle, und zwar der **ersten**.
    fn rueckritt_buchen(&self, stelle: &'static Location<'static>) {
        self.rueckritt.fetch_add(1, Ordering::Relaxed);
        let _ = self.rueckritt_stelle.compare_exchange(
            core::ptr::null_mut(),
            stelle as *const _ as *mut _,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }

    /// **Eine Nachricht NACHTRAGEN statt sie dazwischenzuschreiben.**
    ///
    /// Der Aufrufer kann nicht warten (der Halter sitzt auf seinem Kern und laeuft erst weiter,
    /// wenn dieser Trap zurueckkehrt), und dazwischenschreiben zerreisst die laufende Zeile.
    /// **Gemessen war das kein theoretischer Schaden:** in 8 Suitenlaeufen kam es zweimal vor,
    /// und einmal fiel der Riss mitten in das Ergebniswort einer Pruefzeile (`isohigh : ` ohne
    /// `SKIP`) — die Zeile verschwand aus der Ergebnissignatur, und der Lauf wich ab. Genau die
    /// schlimmere Richtung: nicht ein sichtbarer Fehler, sondern ein verlorener Beleg.
    ///
    /// **Warum das trotzdem nichts verschlucken kann.** Die Laenge wird ZUERST bestimmt (das
    /// Formatieren kostet gemessene 1,2 Zyklen je Byte, ist also gegenueber der Ausgabe umsonst),
    /// und erst dann wird entschieden:
    ///
    /// * passt sie in den freien Rest des Puffers -> nachtragen, der Halter gibt sie unmittelbar
    ///   nach seiner eigenen Nachricht aus (beide ganz, nur die Reihenfolge tauscht);
    /// * passt sie nicht -> **roh ausgeben** und als RISS zaehlen. Ein Ueberlauf, der schweigt,
    ///   waere die Falle, gegen die dieses Modul ohne Puffer angetreten war; ein Ueberlauf, der
    ///   spricht, ist ein benannter Ausgang.
    fn nachtragen(&self, put: Treiber, args: fmt::Arguments) {
        use fmt::Write;
        let mut zaehler = Laengenzaehler(0);
        let _ = zaehler.write_fmt(args);
        let n = zaehler.0;
        let start = self.nachtrag_len.fetch_add(n, Ordering::AcqRel);
        if start.saturating_add(n) > NACHTRAG {
            // Kein Platz: die Reservierung zuruecknehmen und roh ausgeben. Das ZERREISST und
            // wird deshalb als Riss gebucht -- mit lauter Marke, damit kein Bruchstueck als
            // vollstaendige Zeile durchgeht.
            self.nachtrag_len.fetch_sub(n, Ordering::AcqRel);
            self.risse.fetch_add(1, Ordering::Relaxed);
            roh(put, "\n[konsole: ZEILE ZERRISSEN -- Nachtragspuffer voll]\n");
            let mut w = Rohschreiber { put };
            let _ = w.write_fmt(args);
            return;
        }
        let mut w = Nachtragschreiber { o: self, pos: start };
        let _ = w.write_fmt(args);
    }

    /// Ein Byte in den Nachtragspuffer legen (Bereich ist reserviert, s. [`Self::nachtragen`]).
    ///
    /// **Der `else`-Zweig ist der Punkt.** Der Bereich IST reserviert -- [`Laengenzaehler`] und
    /// [`Nachtragschreiber`] wenden dieselbe CR/LF-Regel an, also stimmt die vorher bestimmte
    /// Laenge mit der geschriebenen ueberein. Aber genau das ist eine Rechnung, und wer sich auf
    /// eine Rechnung verlaesst, hat ein `if` ohne `else` gebaut: gingen die beiden je
    /// auseinander, verschwaenden Bytes **still**. Deshalb faellt ein Zugriff ausserhalb hier
    /// nicht durch, sondern wird als RISS gebucht -- die Zahl, die gattert.
    fn nachtrag_setzen(&self, i: usize, b: u8) {
        match self.nachtrag.get(i) {
            // SAFETY: `i` liegt in dem Bereich, den `nachtragen` per `fetch_add` exklusiv
            // reserviert hat, und der Puffer wird nur auf dem Kern angefasst, der die Ordnung
            // haelt (s. das `unsafe impl Sync` am Dateiende).
            Some(z) => unsafe { *z.get() = b },
            None => {
                self.risse.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// **Den Nachtrag ausgeben — vom Halter, unmittelbar bevor er die Ordnung freigibt.**
    ///
    /// Die Schleife liest die Laenge **jedes Mal neu**: waehrend der Ausgabe sind die IRQs
    /// zwischen den Bloecken offen, es kann also noch etwas dazukommen. Zurueckgesetzt wird nur,
    /// wenn sich seit dem letzten Blick nichts geaendert hat — ein blindes `store(0)` waere genau
    /// der Weg, auf dem doch etwas verschwaende.
    fn nachtrag_ausgeben(&self, put: Treiber) {
        let mut gesendet = 0usize;
        loop {
            // **Verglichen wird gegen den UNGEKUERZTEN Wert.** Stuende in `nachtrag_len` je mehr
            // als `NACHTRAG` (heute unerreichbar -- der Ueberlaeufer nimmt seine Reservierung
            // zurueck), dann traefe ein `compare_exchange` gegen die gekuerzte Zahl nie zu, und
            // diese Schleife drehte fuer immer. Ein Haenger IN DER KONSOLE ist der denkbar
            // schlechteste Ausgang: das Werkzeug, mit dem man ihn untersuchen wuerde, ist genau
            // das, was steht.
            let roh_n = self.nachtrag_len.load(Ordering::Acquire);
            let n = roh_n.min(NACHTRAG);
            if n <= gesendet {
                if self
                    .nachtrag_len
                    .compare_exchange(roh_n, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            // Die Bytes liegen fertig aufbereitet im Puffer (das CR steht schon drin), gehen also
            // OHNE die CR/LF-Regel hinaus -- ein zweiter Durchgang verdoppelte jedes CR.
            // SAFETY: `[gesendet..n)` liegt innerhalb des beschriebenen Bereichs (`n` kommt aus
            // `nachtrag_len` und ist auf `NACHTRAG` gedeckelt), und der Puffer wird nur auf dem
            // Kern angefasst, der die Ordnung haelt (s. `unsafe impl Sync` am Dateiende).
            let stueck: &[u8] =
                unsafe { core::slice::from_raw_parts(self.nachtrag[gesendet].get(), n - gesendet) };
            Blockschreiber { o: self, put }.ausgeben(stueck, false);
            gesendet = n;
        }
    }

    /// **Eine formatierte Nachricht ausgeben.**
    ///
    /// * `kern` — der eigene Kernindex (billig zu beschaffen; auf x86 ausdruecklich **nicht**
    ///   ueber `cpuid`, das waere unter KVM ein VM-Exit).
    /// * `irqs_an` — waren die IRQs beim Eintritt freigegeben? Entscheidet ueber den einen Fall,
    ///   in dem Warten eine Verklemmung waere (s. Moduldoku).
    #[track_caller]
    pub(crate) fn drucken(
        &self,
        kern: usize,
        irqs_an: bool,
        put: Treiber,
        args: fmt::Arguments,
    ) {
        use fmt::Write;
        // `#[track_caller]` reicht hier bis zum `println!` durch (`_print` traegt es ebenfalls) —
        // die Stelle, die ein Untersuchender braucht, ist die des Aufrufs und nicht die von
        // `_print`.
        match self.nehmen(kern, irqs_an, Location::caller(), put) {
            Some(besitz) => {
                let mut w = Blockschreiber { o: self, put };
                let _ = w.write_fmt(args);
                drop(besitz);
            }
            None => self.nachtragen(put, args),
        }
    }

    /// Das Besitzrecht nehmen — oder begruendet aufgeben.
    fn nehmen(
        &self,
        kern: usize,
        irqs_an: bool,
        stelle: &'static Location<'static>,
        put: Treiber,
    ) -> Option<Besitz<'_>> {
        let mut runden: u64 = 0;
        loop {
            if self
                .besitzer
                .compare_exchange_weak(FREI, kern, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(Besitz { o: self, put });
            }
            if runden == 0 {
                // Die GELEGENHEIT, einmal je Nachricht -- nicht je Warterunde, sonst zaehlte die
                // Zahl die Dauer und nicht die Haeufigkeit.
                self.besetzt.fetch_add(1, Ordering::Relaxed);
            }
            // Der EINE Fall, in dem Warten nie endet (s. Moduldoku).
            if !irqs_an && self.besitzer.load(Ordering::Acquire) == kern {
                self.rueckritt_buchen(stelle);
                return None;
            }
            runden += 1;
            if runden > WARTEKONTINGENT {
                self.notbremse.fetch_add(1, Ordering::Relaxed);
                self.rueckritt_buchen(stelle);
                return None;
            }
            core::hint::spin_loop();
        }
    }
}

/// Das gehaltene Besitzrecht. **Mit `Drop`**, damit auch ein Abbruch mitten in `write_fmt` die
/// Konsole nicht dauerhaft verriegelt zuruecklaesst — eine tote Konsole waere ein Ausfall ohne
/// Meldung, weil das Melden selbst durch sie liefe.
struct Besitz<'a> {
    o: &'a Schreibordnung,
    put: Treiber,
}

impl Drop for Besitz<'_> {
    fn drop(&mut self) {
        // **Erst nachtragen, dann freigeben** -- und die Reihenfolge ist der Punkt: gaeben wir
        // zuerst frei, koennte ein anderer Kern die Ordnung nehmen und seine Nachricht vor den
        // Nachtrag schieben. Der waere dann nicht falsch, aber noch weiter aus der Zeitfolge.
        self.o.nachtrag_ausgeben(self.put);
        self.o.besitzer.store(FREI, Ordering::Release);
    }
}

/// Schreibt **blockweise**: je [`BLOCK`] ausgegebener Bytes eine kurze Portsperre.
struct Blockschreiber<'a> {
    o: &'a Schreibordnung,
    put: Treiber,
}

impl Blockschreiber<'_> {
    /// Bytes blockweise ausgeben.
    ///
    /// `aufbereiten` = die CR/LF-Regel anwenden. **Der Nachtragspuffer enthaelt bereits
    /// aufbereitete Bytes**; ihn ein zweites Mal durch die Regel zu schicken verdoppelte jedes
    /// CR. Ein `bool` statt zweier fast gleicher Schleifen, damit die Blockrechnung darunter nur
    /// einmal existiert.
    fn ausgeben(&mut self, bytes: &[u8], aufbereiten: bool) {
        let mut rest = bytes;
        while !rest.is_empty() {
            // **Gezaehlt werden AUSGEGEBENE Bytes, nicht Eingabebytes.** Ein `\n` wird zu zwei
            // Bytes; wer die Eingabe blockt, haette einen Block, der im schlimmsten Fall doppelt
            // so lange haelt wie gerechnet -- also eine Schranke, die ihre eigene Groesse nicht
            // kennt.
            let mut ein = 0usize;
            let mut aus = 0usize;
            while ein < rest.len() {
                let n = if aufbereiten && rest[ein] == b'\n' { 2 } else { 1 };
                if aus + n > BLOCK {
                    break;
                }
                aus += n;
                ein += 1;
            }
            // Kann bei `BLOCK >= 2` nicht eintreten; steht da, damit ein gesenktes `BLOCK` eine
            // kurze Sperre erzeugt und keine Endlosschleife.
            if ein == 0 {
                ein = 1;
            }
            // **Die Blockhaltung misst sich selbst.** Zwei Zeitstempel je Block kosten rund 100
            // Zyklen gegen rund 650 000 fuer den Block -- 0,015 %, und dafuer ist die Zahl im
            // Bericht gemessen statt gerechnet.
            //
            // **Die Maske wird HIER genommen und nicht dem `SpinLock` ueberlassen** -- und das ist
            // kein Beiwerk, sondern der Unterschied zwischen einer Messung und einer Zahl. Die
            // erste Fassung stempelte vor `lock()` mit noch OFFENEN IRQs: faellt dorthin eine
            // Unterbrechung mit Umplanung, steht eine ganze fremde Zeitscheibe in der Zahl.
            // Gemessen wurden so **200 491 357 Zyklen fuer 16 Bytes** (7149 Promille eines Ticks)
            // -- und die Zeile fiel durch, obwohl die MASKIERTE Dauer klein war. Ein Fenster, das
            // groesser ist als die Eigenschaft, misst die Umgebung.
            //
            // So ist das gemessene Fenster **genau** das maskierte, und zwar ein wenig groesser
            // als das der Sperrhaltedauer-Marke (Eintritt in `lock()` und Freigabe des Guards
            // liegen mit darin). Ueberschaetzen ist die Richtung, die zu einer Schranke passt.
            // **Erst warten, dann maskieren.** Das Warten auf `THRE` ist der lange Teil (bei
            // gesaettigter Ausgabe die Leitungszeit, bei haengendem Backend des Wirts
            // Millisekunden); es laeuft hier mit OFFENEN IRQs und ist damit praemptierbar. Erst
            // wenn der Port aufnahmebereit ist, faellt die Maske -- und dann steht darunter nur
            // noch ein `outb`.
            for &b in &rest[..ein] {
                let _ = b;
                while !(self.put.bereit)() {
                    core::hint::spin_loop();
                }
            }
            let vorher = crate::cpu::local_irq_save();
            let t0 = crate::timer::cycles();
            {
                let _g = self.o.port.lock();
                for &b in &rest[..ein] {
                    if aufbereiten {
                        byte_ausgeben(self.put, b);
                    } else {
                        (self.put.senden)(b);
                    }
                }
            }
            let dauer = crate::timer::cycles().saturating_sub(t0);
            if dauer > self.o.block_max.load(Ordering::Relaxed) {
                self.o.block_max.fetch_max(dauer, Ordering::Relaxed);
            }
            self.o.bloecke.fetch_add(1, Ordering::Relaxed);
            crate::cpu::local_irq_restore(vorher);
            rest = &rest[ein..];
        }
    }
}

impl fmt::Write for Blockschreiber<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.ausgeben(s.as_bytes(), true);
        Ok(())
    }
}

/// Schreibt roh — ohne Sperre, ohne Besitzrecht, ohne Warten.
struct Rohschreiber {
    put: Treiber,
}

impl fmt::Write for Rohschreiber {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        roh(self.put, s);
        Ok(())
    }
}

/// Zaehlt nur, wie lang die formatierte Nachricht wird — ohne ein Byte auszugeben.
struct Laengenzaehler(usize);

impl fmt::Write for Laengenzaehler {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // Ausgegeben wird spaeter mit CR vor jedem LF; die Laenge muss das mitzaehlen, sonst
        // passt die Reservierung nicht zu dem, was hineingeschrieben wird.
        self.0 = self
            .0
            .saturating_add(s.len() + s.bytes().filter(|&b| b == b'\n').count());
        Ok(())
    }
}

/// Schreibt in den Nachtragspuffer — **fertig aufbereitet**, also mit CR vor jedem LF, damit die
/// Ausgabe spaeter ein reines Kopieren ist und die CR/LF-Regel nur an einer Stelle wirkt.
struct Nachtragschreiber<'a> {
    o: &'a Schreibordnung,
    pos: usize,
}

impl fmt::Write for Nachtragschreiber<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.o.nachtrag_setzen(self.pos, b'\r');
                self.pos += 1;
            }
            self.o.nachtrag_setzen(self.pos, b);
            self.pos += 1;
        }
        Ok(())
    }
}

// SAFETY: Der Nachtragspuffer wird ausschliesslich auf dem Kern angefasst, der die Ordnung
// gerade haelt -- der Rueckritt setzt `besitzer == eigener Kern` voraus, und ausgegeben wird er
// nur vom Halter selbst. Auf diesem Kern laeuft immer nur EIN Kontext (ein verschachtelter Trap
// laeuft vollstaendig ab, bevor der unterbrochene weitermacht), und ueberschneidungsfreie
// Bereiche stellt die Reservierung per `fetch_add` sicher. `u8` ist `Send`.
unsafe impl Sync for Schreibordnung {}
