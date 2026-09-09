//! **rspd: die RSP-Protokollseite der gdbserver-PD (Debugger v2, Stufe 1).**
//!
//! Stand: `docs/plan-debugger.md` ist gelesen, v1 ist gebaut (`dbg`-Zeile, Syscalls
//! 21–25, `ERR_DEBUG_BUSY`/`ERR_NOT_DEBUGGABLE`). Offen aus Plan §9/§10b: die gdbserver-PD
//! mit RSP, Hardware-Breakpoints, Einzelschritt. Diese Kiste ist der erste, kernellose
//! Schritt davon: **reines Protokoll, keine Fremdberuehrung.**
//!
//! Was hier steht (alles `no_std` + `forbid(unsafe_code)`, ohne Abhaengigkeiten):
//! - [`pruefsumme`]/[`kodiere`]: Paket-Rahmung `$...#csum` mit Escape-Sequenzen.
//! - [`Parser`]: byteweise Zustandsmaschine (Start, Nutzlast, Escape, zwei
//!   Pruefsummen-Halbbytes). Gueltig → [`Ereignis::Paket`], defekt →
//!   [`Ereignis::PruefsummenFehler`] (die NAK-Pflicht liegt beim Aufrufer, s. [`NAK`]).
//! - [`klassifiziere`]/[`beantworte`]: Befehls-Skelett fuer `qSupported`, `?`, `g`, `G`,
//!   `m`, `M`, `c`, `s`. Jede Variante ist **benannt**, aber **nichts ist bedient**:
//!   jede Antwort ist die leere Fehlantwort `$#00` ("nicht unterstuetzt") — niemals `OK`,
//!   niemals ein erfundenes Registerwort (s. [`wird_bedient`]).
//!
//! Was hier ausdruecklich NICHT steht: welcher Syscall welchen RSP-Befehl bedienen wird.
//! Das steht in `ANBINDUNG.md` daneben. Was der ABI dafuer noch fehlt, steht NICHT als
//! Code hier, sondern als exakter Patch-Text in `abi-erweiterungen.diff` — die
//! Hauptinstanz baut ihn in `crates/caprock-abi` ein.

#![no_std]
#![forbid(unsafe_code)]

// Die Kiste ist `no_std` (sie laeuft spaeter in einer PD ohne OS). Der Testaufbau
// braucht `std` dafuer — nur im Test, der PD-Bau sieht es nie (dasselbe Muster wie
// `caprock-dma` / `caprock-wait` / `programs/lx-shim-demo`).
#[cfg(test)]
extern crate std;

/// Maximale Nutzdatenlaenge, die der Parser annimmt. Ein laengerer Rahmen ist kein
/// Paket, sondern das Ereignis [`Ereignis::ZuGross`] — ein unbegrenzter Puffer im
/// Debugger waere eine Latenz- und Speicherlücke, die niemand sieht, bis sie reisst.
pub const MAX_NUTZLAST: usize = 2048;

/// Bestaetigung auf der Leitung: das Gegenueber hat den Rahmen angenommen.
pub const ACK: u8 = b'+';
/// Ablehnung auf der Leitung: zu senden, wenn [`Ereignis::PruefsummenFehler`] eintritt.
pub const NAK: u8 = b'-';
/// Unterbrechung (Ctrl-C des Host-GDB): steht ausserhalb jeder Rahmung.
pub const UNTERBRECHUNG: u8 = 0x03;
/// Maske der Escape-Sequenzen: `}` gefolgt von `byte ^ 0x20`.
pub const ESCAPE_MASKE: u8 = 0x20;

/// Pruefsumme eines RSP-Rahmens: Summe der **unmaskierten** Nutzlast-Bytes modulo 256.
pub fn pruefsumme(nutzlast: &[u8]) -> u8 {
    let mut summe: u8 = 0;
    let mut i = 0;
    while i < nutzlast.len() {
        summe = summe.wrapping_add(nutzlast[i]);
        i += 1;
    }
    summe
}

/// Wert eines Hexzeichens (`0-9`, `a-f`, `A-F`). Das Gegenueber darf gross schreiben;
/// was wir selbst senden, ist immer klein (s. [`hex_zeichen`]).
pub fn hex_wert(zeichen: u8) -> Option<u8> {
    match zeichen {
        b'0'..=b'9' => Some(zeichen - b'0'),
        b'a'..=b'f' => Some(zeichen - b'a' + 10),
        b'A'..=b'F' => Some(zeichen - b'A' + 10),
        _ => None,
    }
}

/// Hexzeichen fuer ein Halbbyte, immer kleingeschrieben (kanonische Sendeseite).
pub fn hex_zeichen(halb: u8) -> u8 {
    debug_assert!(halb < 16);
    if halb < 10 {
        b'0' + halb
    } else {
        b'a' + (halb - 10)
    }
}

/// Ob ein Byte auf der Leitung maskiert werden muss (`}`, `$`, `#`).
pub fn braucht_escape(byte: u8) -> bool {
    matches!(byte, b'}' | b'$' | b'#')
}

/// Rahmungslaenge der kodierten Nutzlast: `$` + maskierte Bytes + `#` + zwei Hexstellen.
pub fn kodiert_len(nutzlast: &[u8]) -> usize {
    let mut n = 1 + 3; // `$` und `#csum`
    let mut i = 0;
    while i < nutzlast.len() {
        n += if braucht_escape(nutzlast[i]) { 2 } else { 1 };
        i += 1;
    }
    n
}

/// Rahmt `nutzlast` als `$...#csum` nach `aus`. Gibt die belegte Laenge zurueck oder
/// `None`, wenn `aus` zu klein ist (dann steht nichts Halbfertiges darin, was zaehlt,
/// ist der Rueckgabewert, nicht der Pufferinhalt).
pub fn kodiere(nutzlast: &[u8], aus: &mut [u8]) -> Option<usize> {
    let bedarf = kodiert_len(nutzlast);
    if aus.len() < bedarf {
        return None;
    }
    let mut p = 0;
    aus[p] = b'$';
    p += 1;
    let mut i = 0;
    while i < nutzlast.len() {
        let byte = nutzlast[i];
        if braucht_escape(byte) {
            aus[p] = b'}';
            aus[p + 1] = byte ^ ESCAPE_MASKE;
            p += 2;
        } else {
            aus[p] = byte;
            p += 1;
        }
        i += 1;
    }
    let summe = pruefsumme(nutzlast);
    aus[p] = b'#';
    aus[p + 1] = hex_zeichen(summe >> 4);
    aus[p + 2] = hex_zeichen(summe & 0x0f);
    p += 3;
    Some(p)
}

/// Die leere Fehlantwort `$#00` — RSP fuer "nicht unterstuetzt". Das ist die EINZIGE
/// Antwort, die diese Stufe kennt (s. [`beantworte`]).
pub fn kodiere_leer(aus: &mut [u8]) -> Option<usize> {
    kodiere(b"", aus)
}

/// Was ein zugefuehrtes Byte (oder ein abgeschlossener Rahmen) bedeutet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ereignis {
    /// `+`: das Gegenueber hat unseren letzten Rahmen angenommen.
    Bestaetigt,
    /// `-`: das Gegenueber will ihn erneut.
    Abgelehnt,
    /// `0x03`: Unterbrechung ausserhalb jeder Rahmung.
    Unterbrechung,
    /// Gueltiger Rahmen; die Zahl ist die Nutzlastlaenge in [`Parser::nutzlast`].
    Paket(usize),
    /// Rahmen mit falscher oder unlesbarer Pruefsumme — der Aufrufer sendet [`NAK`].
    PruefsummenFehler,
    /// Rahmen ueber [`MAX_NUTZLAST`] hinaus — verworfen, nichts übernommen.
    ZuGross,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zustand {
    Start,
    Nutzlast,
    Escape,
    SummeHi,
    SummeLo,
}

/// Byteweise RSP-Zustandsmaschine. Kein Speicher ausser dem Nutzlast-Fenster, keine
/// Allokation, kein Syscall — host-testbar per Konstruktion.
pub struct Parser {
    zustand: Zustand,
    puffer: [u8; MAX_NUTZLAST],
    len: usize,
    summe_hi: u8,
}

impl Parser {
    /// Leerer Parser im Wartezustand.
    pub fn neu() -> Self {
        Parser {
            zustand: Zustand::Start,
            puffer: [0; MAX_NUTZLAST],
            len: 0,
            summe_hi: 0,
        }
    }

    /// Die Nutzlast des zuletzt gemeldeten [`Ereignis::Paket`]. Gueltig bis zum
    /// naechsten `$` (Rahmenstart) oder [`Parser::verwerfe`] — der Aufrufer kopiert
    /// oder klassifiziert sofort; was danach darin steht, ist Geschichte.
    pub fn nutzlast(&self) -> &[u8] {
        &self.puffer[..self.len]
    }

    /// Bricht einen angefangenen Rahmen ab (Neusynchronisation von aussen).
    pub fn verwerfe(&mut self) {
        self.zustand = Zustand::Start;
        self.len = 0;
        self.summe_hi = 0;
    }

    fn zurueck(&mut self) {
        self.zustand = Zustand::Start;
        self.len = 0;
        self.summe_hi = 0;
    }

    /// Fuehrt ein Leitungsbyte zu. Gibt genau dann ein Ereignis zurueck, wenn das Byte
    /// eines abschliesst (`+`, `-`, `0x03`, zweite Pruefsummenstelle, Fehlerfall);
    /// andernfalls `None` (Rahmen unvollstaendig — warten, nicht raten).
    pub fn feed(&mut self, byte: u8) -> Option<Ereignis> {
        match self.zustand {
            Zustand::Start => match byte {
                b'$' => {
                    self.len = 0;
                    self.zustand = Zustand::Nutzlast;
                    None
                }
                ACK => Some(Ereignis::Bestaetigt),
                NAK => Some(Ereignis::Abgelehnt),
                UNTERBRECHUNG => Some(Ereignis::Unterbrechung),
                _ => None,
            },
            Zustand::Nutzlast => match byte {
                // Ein `$` mitten im Rahmen ist kein Datenbyte, sondern ein Neustart des
                // Gegenuebers (abgebrochene Sendung). Altes verwerfen, neu anfangen —
                // stillschweigendes Ankleben waere ein Rahmen, den niemand schickte.
                b'$' => {
                    self.len = 0;
                    None
                }
                b'#' => {
                    self.zustand = Zustand::SummeHi;
                    None
                }
                b'}' => {
                    self.zustand = Zustand::Escape;
                    None
                }
                _ => {
                    if self.len >= MAX_NUTZLAST {
                        self.zurueck();
                        return Some(Ereignis::ZuGross);
                    }
                    self.puffer[self.len] = byte;
                    self.len += 1;
                    None
                }
            },
            Zustand::Escape => {
                if self.len >= MAX_NUTZLAST {
                    self.zurueck();
                    return Some(Ereignis::ZuGross);
                }
                self.puffer[self.len] = byte ^ ESCAPE_MASKE;
                self.len += 1;
                self.zustand = Zustand::Nutzlast;
                None
            }
            Zustand::SummeHi => match hex_wert(byte) {
                Some(hi) => {
                    self.summe_hi = hi;
                    self.zustand = Zustand::SummeLo;
                    None
                }
                None => {
                    self.zurueck();
                    Some(Ereignis::PruefsummenFehler)
                }
            },
            Zustand::SummeLo => {
                let lo = match hex_wert(byte) {
                    Some(lo) => lo,
                    None => {
                        self.zurueck();
                        return Some(Ereignis::PruefsummenFehler);
                    }
                };
                let soll = (self.summe_hi << 4) | lo;
                let ist = pruefsumme(&self.puffer[..self.len]);
                let n = self.len;
                // Gemeldet, nicht geloescht: die Nutzlast bleibt bis zum naechsten
                // `$` lesbar (s. `nutzlast`) — auch im Fehlerfall, damit der Aufrufer
                // den verworfenen Rahmen noch benennen (loggen) kann.
                self.zustand = Zustand::Start;
                self.summe_hi = 0;
                if soll == ist {
                    Some(Ereignis::Paket(n))
                } else {
                    Some(Ereignis::PruefsummenFehler)
                }
            }
        }
    }

    /// Speist einen ganzen Rahmen und gibt das letzte Ereignis zurueck — `None`, wenn
    /// der Rahmen unvollstaendig war (dann steht der Parser mitten im Rahmen und
    /// wartet auf mehr, s. [`Parser::feed`]).
    pub fn rahmen(&mut self, rahmen: &[u8]) -> Option<Ereignis> {
        let mut letztes = None;
        let mut i = 0;
        while i < rahmen.len() {
            if let Some(e) = self.feed(rahmen[i]) {
                letztes = Some(e);
            }
            i += 1;
        }
        letztes
    }
}

/// RSP-Befehl, soweit das Skelett ihn benennt. Jede Variante ist eine Absichtserklaerung
/// mit korrekter Fehlantwort — keine ist bedient (s. [`wird_bedient`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Befehl {
    /// `qSupported...`: Leistungsabfrage des Host-GDB (Antwort heute: leer).
    QSupported,
    /// `?`: Anhaltegrund (spaeter: Sidecar + `KOPF_GEN`, kein Syscall).
    Frage,
    /// `g`: Register lesen (spaeter: Sidecar-Mapping, kein Syscall).
    LiesRegister,
    /// `G...`: Register schreiben (spaeter: `DEBUG_WRITE_REGS`).
    SchreibRegister,
    /// `mADDR,LEN`: Speicher lesen (spaeter: `DEBUG_READ_MEM`-Schleife).
    LiesSpeicher,
    /// `MADDR,LEN:...`: Speicher schreiben — braucht erst `DEBUG_WRITE_MEM` (Patch).
    SchreibSpeicher,
    /// `c[ADDR]`: Weiterlaufen (spaeter: `DEBUG_CONTINUE`).
    Fortsetzen,
    /// `s[ADDR]`: Einzelschritt — braucht erst `DEBUG_SINGLE_STEP` + `hal::debug` (Patch).
    Einzelschritt,
    /// Alles andere: unbekannt, Antwort ebenfalls leer (RSP-"nicht unterstuetzt").
    Unbekannt,
}

fn beginnt_mit(nutzlast: &[u8], praefix: &[u8]) -> bool {
    nutzlast.len() >= praefix.len() && &nutzlast[..praefix.len()] == praefix
}

/// Benennt die Absicht eines Rahmens. Parametrisierte Befehle (`m`, `M`, `c`, `s`,
/// `qSupported`) werden am Erstbyte bzw. Praefix erkannt; die Parameterauswertung
/// gehoert der bedienenden Stufe, nicht dem Skelett.
pub fn klassifiziere(nutzlast: &[u8]) -> Befehl {
    if nutzlast == b"?" {
        return Befehl::Frage;
    }
    if nutzlast == b"g" {
        return Befehl::LiesRegister;
    }
    if beginnt_mit(nutzlast, b"qSupported") {
        return Befehl::QSupported;
    }
    match nutzlast.first() {
        Some(b'G') => Befehl::SchreibRegister,
        Some(b'm') => Befehl::LiesSpeicher,
        Some(b'M') => Befehl::SchreibSpeicher,
        Some(b'c') => Befehl::Fortsetzen,
        Some(b's') => Befehl::Einzelschritt,
        _ => Befehl::Unbekannt,
    }
}

/// Ob die PD diesen Befehl heute bedient: **immer nein.** Diese Funktion existiert,
/// damit ein spaeterer Implementierungsschritt sie armweise auf `true` drehen muss —
/// ein Erfolgs-Stub, der `OK` meldet, faellt hier auf, weil er an dieser Stelle
/// vorbeimuesste. Jeder Arm steht einzeln, damit das Hinzufuegen einer Bedienung
/// genau einen Arm beruehrt.
pub fn wird_bedient(befehl: Befehl) -> bool {
    match befehl {
        Befehl::QSupported => false,
        Befehl::Frage => false,
        Befehl::LiesRegister => false,
        Befehl::SchreibRegister => false,
        Befehl::LiesSpeicher => false,
        Befehl::SchreibSpeicher => false,
        Befehl::Fortsetzen => false,
        Befehl::Einzelschritt => false,
        Befehl::Unbekannt => false,
    }
}

/// Beantwortet einen Rahmen: benennt ihn per [`klassifiziere`] und schreibt die
/// korrekte RSP-Fehlantwort — die leere Antwort `$#00` ("nicht unterstuetzt"). Gibt
/// die Antwortlaenge zurueck oder `None`, wenn `aus` dafuer (4 Bytes) nicht reicht.
pub fn beantworte(nutzlast: &[u8], aus: &mut [u8]) -> Option<usize> {
    match klassifiziere(nutzlast) {
        Befehl::QSupported => kodiere_leer(aus),
        Befehl::Frage => kodiere_leer(aus),
        Befehl::LiesRegister => kodiere_leer(aus),
        Befehl::SchreibRegister => kodiere_leer(aus),
        Befehl::LiesSpeicher => kodiere_leer(aus),
        Befehl::SchreibSpeicher => kodiere_leer(aus),
        Befehl::Fortsetzen => kodiere_leer(aus),
        Befehl::Einzelschritt => kodiere_leer(aus),
        Befehl::Unbekannt => kodiere_leer(aus),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hilfsparser, der einen Rahmen auf einmal speist und das Endergebnis liefert.
    fn parse_rahmen(rahmen: &[u8]) -> (Parser, Option<Ereignis>) {
        let mut p = Parser::neu();
        let e = p.rahmen(rahmen);
        (p, e)
    }

    #[test]
    fn pruefsumme_bekannt() {
        // "g" = 0x67; "OK" = 0x4F + 0x4B = 0x9A.
        assert_eq!(pruefsumme(b"g"), 0x67);
        assert_eq!(pruefsumme(b"OK"), 0x9A);
        assert_eq!(pruefsumme(b""), 0x00);
    }

    #[test]
    fn kodiere_einfach() {
        let mut aus = [0u8; 16];
        let n = kodiere(b"g", &mut aus).expect("passt");
        assert_eq!(&aus[..n], b"$g#67");
        assert_eq!(n, kodiert_len(b"g"));
    }

    #[test]
    fn kodiere_sendet_klein_hex() {
        // 0xFF braucht Buchstaben-Halbbytes — kanonisch klein.
        let mut aus = [0u8; 16];
        let n = kodiere(&[0xff], &mut aus).expect("passt");
        assert_eq!(&aus[..n], &[b'$', 0xff, b'#', b'f', b'f']);
    }

    #[test]
    fn kodiere_leer_ist_fehlantwort() {
        let mut aus = [0u8; 16];
        let n = kodiere_leer(&mut aus).expect("passt");
        assert_eq!(&aus[..n], b"$#00");
    }

    #[test]
    fn kodiere_escapet_sonderzeichen() {
        // `}`, `$`, `#` werden maskiert; die Pruefsumme laeuft ueber die UNMASKIERTEN
        // Bytes: 0x7D + 0x24 + 0x23 + 0x41 = 261 = 0x05.
        let nutzlast = [b'}', b'$', b'#', b'A'];
        let mut aus = [0u8; 32];
        let n = kodiere(&nutzlast, &mut aus).expect("passt");
        let erwartet: &[u8] = &[
            b'$', b'}', b']', b'}', 0x04, b'}', 0x03, b'A', b'#', b'0', b'5',
        ];
        assert_eq!(&aus[..n], erwartet);
    }

    #[test]
    fn runde_um_escapes() {
        let nutzlast = [b'}', b'$', b'#', b'A'];
        let mut rahmen = [0u8; 32];
        let n = kodiere(&nutzlast, &mut rahmen).expect("passt");
        let (p, e) = parse_rahmen(&rahmen[..n]);
        assert_eq!(e, Some(Ereignis::Paket(nutzlast.len())));
        assert_eq!(p.nutzlast(), &nutzlast);
    }

    #[test]
    fn gueltiges_paket_wird_angenommen() {
        let (p, e) = parse_rahmen(b"$g#67");
        assert_eq!(e, Some(Ereignis::Paket(1)));
        assert_eq!(p.nutzlast(), b"g");
    }

    #[test]
    fn pruefsummenfehler_wird_benannt_nicht_versteckt() {
        // Falsche Summe: kein Paket, sondern der benannte Fehler — der Aufrufer
        // schuldet dafuer NAK, und der Test prueft, dass hier kein Inhalt ankommt.
        let (p, e) = parse_rahmen(b"$g#00");
        assert_eq!(e, Some(Ereignis::PruefsummenFehler));
        assert_eq!(p.nutzlast(), b"g"); // Inhalt steht, gilt aber nicht.
        // Unlesbare Hexstelle ist dieselbe Klasse, kein dritter Weg.
        let (_, e2) = parse_rahmen(b"$g#6?");
        assert_eq!(e2, Some(Ereignis::PruefsummenFehler));
    }

    #[test]
    fn leitungssignale_ausserhalb_der_rahmung() {
        let mut p = Parser::neu();
        assert_eq!(p.feed(b'+'), Some(Ereignis::Bestaetigt));
        assert_eq!(p.feed(b'-'), Some(Ereignis::Abgelehnt));
        assert_eq!(p.feed(UNTERBRECHUNG), Some(Ereignis::Unterbrechung));
        // Fremdbytes im Wartezustand sind Rauschen, kein Ereignis.
        assert_eq!(p.feed(b' '), None);
    }

    #[test]
    fn unvollstaendig_wartet_statt_zu_raten() {
        let mut p = Parser::neu();
        for &b in b"$g" {
            assert_eq!(p.feed(b), None);
        }
        // ... und die Fortsetzung liefert das Paket nach.
        assert_eq!(p.feed(b'#'), None);
        assert_eq!(p.feed(b'6'), None);
        let e = p.feed(b'7');
        assert_eq!(e, Some(Ereignis::Paket(1)));
        assert_eq!(p.nutzlast(), b"g");
    }

    #[test]
    fn dollar_im_rahmen_synchronisiert_neu() {
        // Abgebrochene Sendung des Gegenuebers: `$g` + Neustart `$g#67`.
        let (p, e) = parse_rahmen(b"$g$g#67");
        assert_eq!(e, Some(Ereignis::Paket(1)));
        assert_eq!(p.nutzlast(), b"g");
    }

    #[test]
    fn parser_erholt_sich_nach_fehler() {
        let mut p = Parser::neu();
        assert_eq!(p.rahmen(b"$g#00"), Some(Ereignis::PruefsummenFehler));
        let e = p.rahmen(b"$g#67");
        assert_eq!(e, Some(Ereignis::Paket(1)));
        assert_eq!(p.nutzlast(), b"g");
    }

    #[test]
    fn gross_sprengt_nicht() {
        // 2048 + 1 Datenbytes ohne `#`: der Parser benennt den Ueberlauf und nimmt
        // nichts ueber — danach ist er wieder ansprechbar.
        let mut q = Parser::neu();
        assert_eq!(q.feed(b'$'), None);
        let mut gemeldet = None;
        let mut j = 0;
        while j < MAX_NUTZLAST + 1 {
            if let Some(e) = q.feed(b'A') {
                gemeldet = Some(e);
                break;
            }
            j += 1;
        }
        assert_eq!(gemeldet, Some(Ereignis::ZuGross));
        assert_eq!(q.rahmen(b"$g#67"), Some(Ereignis::Paket(1)));
    }

    #[test]
    fn unbekannter_befehl_antwortet_leer_nie_erfolg() {
        assert_eq!(klassifiziere(b"y"), Befehl::Unbekannt);
        assert!(!wird_bedient(Befehl::Unbekannt));
        let mut aus = [0u8; 16];
        let n = beantworte(b"y", &mut aus).expect("passt");
        assert_eq!(&aus[..n], b"$#00");
        // ... und schon gar kein Erfolg: kein OK, kein Signalsatz.
        assert!(!aus[..n].contains(&b'O'));
        assert!(!aus[..n].starts_with(b"$S"));
    }

    #[test]
    fn alle_v2_befehle_sind_benannte_nicht_implementierung() {
        // Jeder offene Befehl aus dem Auftrag ist benannt klassifiziert, unbedient
        // und antwortet mit der korrekten Fehlantwort — ein Stub, der Erfolg meldet,
        // bestuende diesen Test nicht.
        let faelle: &[(&[u8], Befehl)] = &[
            (b"qSupported:multiprocess+", Befehl::QSupported),
            (b"?", Befehl::Frage),
            (b"g", Befehl::LiesRegister),
            (b"G", Befehl::SchreibRegister),
            (b"m1000,10", Befehl::LiesSpeicher),
            (b"M1000,10:aa", Befehl::SchreibSpeicher),
            (b"c", Befehl::Fortsetzen),
            (b"s", Befehl::Einzelschritt),
        ];
        for (rahmen, erwartet) in faelle {
            let befehl = klassifiziere(rahmen);
            assert_eq!(befehl, *erwartet);
            assert!(!wird_bedient(befehl));
            let mut aus = [0u8; 16];
            let n = beantworte(rahmen, &mut aus).expect("passt");
            assert_eq!(&aus[..n], b"$#00", "Befehl {:?}", befehl);
        }
    }

    #[test]
    fn antwort_passt_in_vier_bytes_oder_gar_nicht() {
        let mut klein = [0u8; 3];
        assert_eq!(beantworte(b"g", &mut klein), None);
        let mut genau = [0u8; 4];
        let n = beantworte(b"g", &mut genau).expect("passt genau");
        assert_eq!(&genau[..n], b"$#00");
    }
}
