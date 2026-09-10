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
//!   niemals ein erfundenes Registerwort (s. [`wird_bedient`]). Das ist die Stufe 1
//!   (kernellos); die Stufe 2 mit Backend steht darunter: [`bediene`] beantwortet
//!   `qSupported`/`?` ehrlich und bedient `g`/`m`/`M` über [`Backend`], während
//!   `c`/`s`/`Z`/`G` benannt unbedient bleiben.
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
///
/// Das ist die Stufe-1-Antwort (kernellos, ohne Backend); mit Backend s. [`bediene`].
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

// --- Stufe 2 (RSP-WIRE): Bedienung über ein injizierbares Backend ---------------------------
//
// `beantworte` oben bleibt die kernellose Stufe-1-Antwort (immer leer). Was hier folgt,
// ist die Stufe 2: `qSupported`/`?` werden ehrlich beantwortet, `m`/`M`/`g` laufen über
// das [`Backend`-Trait gegen die echten DEBUG-Syscalls — `c`/`s`/`Z`/`G` bleiben benannt
// unbedient (s. `ANBINDUNG.md`, Anhang "Stufe 2").
//
// ## Warum ein eigener Mini-Wrapper statt `libcaprock` (gelesen, nicht geraten)
//
// `libcaprock` ist das Muster für die Aufrufform (Nummer spiegeln, Client-Gatter vor dem
// Syscall), aber kein passender Dep für diese Kiste, aus drei Gründen:
// 1. Diese Kiste steht unter `forbid(unsafe_code)`. `libcaprock::invoke` enthält SVC-asm
//    (`unsafe`) — ein Dep zöge `unsafe` in eine PD-Kiste, deren ganze Aussage "reines
//    Protokoll" ist.
// 2. Diese Kiste ist standalone (kein Workspace-Member, isolierte `/tmp`-Tests wie
//    `tools/host-tests.sh` sie für dep-tragende Crates fährt). Ein Pfad-Dep auf den
//    Workspace bräche genau diese Isolation.
// 3. Der echte SVC steht hier ohnehin nicht: der spätere PD-Eintritt ruft die Syscalls,
//    diese Stufe definiert nur Form (Nummern, Deckel) + injizierbares Backend und bleibt
//    damit host-testbar. Die Nummern sind gespiegelt, die EINZIGE Wahrheit ist
//    `caprock_abi::sys` — dieselbe Spiegeldisziplin wie `libcaprock` selbst.
//

/// Gespiegelte Syscall-Nummern der Debug-Pfade, die diese Stufe bedient oder benennt.
///
/// Spiegel von `caprock_abi::sys` (dort die EINZIGE Wahrheit): `DEBUG_READ_MEM` (24) und
/// `DEBUG_WRITE_MEM` (33) tragen `m`/`M`; `DEBUG_SINGLE_STEP` (34) und `DEBUG_HWBREAK`
/// (35) fehlen kernel-seitig noch (`ERR_BADSYS`, `hal::debug` fehlt — s. `ANBINDUNG.md`),
/// deshalb bleiben `s`/`Z` benannt unbedient. `DEBUG_CONTINUE` (23) existiert, trägt `c`
/// aber bewusst noch nicht (Halt-Disziplin des Ziels gehört dem PD-Eintritt, nicht dem
/// Protokoll — ein `CONTINUE` ohne gehaltenes Ziel wäre geraten).
pub mod sys {
    /// Angehaltenen Thread weiterlaufen lassen (v1, verdrahtet — `c` nutzt ihn noch nicht).
    pub const DEBUG_CONTINUE: u64 = 23;
    /// Zielspeicher lesen (v1, verdrahtet — trägt `m`).
    pub const DEBUG_READ_MEM: u64 = 24;
    /// Registerwort schreiben (v1, verdrahtet — `G` nutzt es noch nicht, Maske s. Plan §3b).
    pub const DEBUG_WRITE_REGS: u64 = 25;
    /// Zielspeicher schreiben (verdrahtet: `debug_write_mem`, `WRITE_MAX`-Deckel — trägt `M`).
    pub const DEBUG_WRITE_MEM: u64 = 33;
    /// Einen Schritt tun (fail-closed: `ERR_BADSYS`, kein `hal::debug` — trägt `s` noch nicht).
    pub const DEBUG_SINGLE_STEP: u64 = 34;
    /// Hardware-Breakpoint (fail-closed: `ERR_BADSYS`, kein `hal::debug` — trägt `Z` noch nicht).
    pub const DEBUG_HWBREAK: u64 = 35;
}

/// Benannte Kapazitäten der Debug-Transfers (Spiegel von `caprock_abi::debug`).
///
/// Ein RSP-Paket, das mehr verlangt, schleift die PD in Stücken dieser Größe — ein
/// unbegrenzter Kernel-Lauf unter Sperre wäre ein Latenzloch, das niemand sieht, bis es
/// ein Hänger ist. Die Schleife steht in [`bediene_m`]/[`bediene_gross_m`], nicht im Kernel.
pub mod debug {
    /// Höchstens so viele Bytes je `DEBUG_READ_MEM`-Aufruf.
    pub const READ_MAX: u64 = 512;
    /// Höchstens so viele Bytes je `DEBUG_WRITE_MEM`-Aufruf.
    pub const WRITE_MAX: u64 = 512;
}

/// Ehrliche `qSupported`-Antwort: nur was wirklich geht.
///
/// `PacketSize=2048` nennt die einzige harte Zahl dieser Kiste ([`MAX_NUTZLAST`]). Kein
/// `QStartNoAckMode+`, kein `multiprocess+`, kein `qXfer` — was nicht bedient wird, wird
/// auch nicht angeboten (ein angebotenes Feature, das danach `$#00` antwortet, wäre ein
/// Versprechen ohne Mechanik).
pub const QSUPPORTED_ANTWORT: &[u8] = b"PacketSize=2048";

/// Anhaltegrund auf `?`: `S05` (SIGTRAP) — der kanonische Stoppgrund eines Debuggers
/// (Breakpoint/Schritt/Halt), ohne erfundene Thread-Id und ohne erfundenes Registerwort.
pub const ANHALTEGRUND: &[u8] = b"S05";

/// Backend-Fehler auf der Leitung: `E01`.
///
/// Fail-closed-Regel dieser Stufe: Backend-Absage → `E01` (benannt, RSP-Standard);
/// Fehlbedienung (`c`/`s`/`Z`/`G`, kernel-seitig fehlend) und missgebildete Anfrage →
/// leer (`$#00`, "nicht unterstützt"). Ein Fehler, der wie "nicht unterstützt" aussähe
/// (oder umgekehrt), wäre eine Diagnose, die lügt.
pub const FEHLER_BACKEND: &[u8] = b"E01";

/// Der Zielzugang der Bedienungsstufe: drei Operationen, keine Syscalls hier.
///
/// Der PD-Eintritt implementiert dieses Trait gegen die echten Syscalls (`sys::DEBUG_*`
/// mit den `debug::*_MAX`-Deckeln); die Host-Tests implementieren es gegen ein Fake.
/// Was das Trait NICHT enthält, ist Absicht: `c`/`s`/`Z` haben hier keinen Arm, weil
/// 34/35 kernel-seitig fehlen (s. [`sys`]) — ein Arm ohne Mechanik wäre ein Stub, der
/// Erfolg meldet.
pub trait Backend {
    /// `len` Bytes ab Ziel-VA `va` nach `out` lesen (`out` mind. `len`).
    /// Gibt übertragene Bytes zurück — weniger als `len` heisst Lücke im Ziel (gesehen,
    /// nicht geraten), `Err` heisst Absage (Cap/Recht/Puffer).
    fn read_mem(&mut self, va: u64, len: u64, out: &mut [u8]) -> Result<u64, u64>;
    /// `data` ab Ziel-VA `va` schreiben. Gibt übertragene Bytes zurück (`Err` = Absage).
    fn write_mem(&mut self, va: u64, data: &[u8]) -> Result<u64, u64>;
    /// Registerblock des gehaltenen Ziels nach `out` lesen (produktiv: Sidecar-Mapping,
    /// read-only — bewusst KEIN Syscall, s. ABI: "Frame reading has no syscall").
    /// Gibt die Blob-Länge zurück (`Err` = nicht gehalten/kein Ziel).
    fn read_regs(&mut self, out: &mut [u8]) -> Result<usize, u64>;
}

/// Beantwortet einen Rahmen MIT Backend: verdrahtet was geht, benennt den Rest.
///
/// - `qSupported...` → [`QSUPPORTED_ANTWORT`], `?` → [`ANHALTEGRUND`] (kein Syscall,
///   Protokollebene — ehrlich, weil wirklich bedient).
/// - `g` → [`Backend::read_regs`] (Hex-Blob; produktiv Sidecar, kein Syscall 24).
/// - `mADDR,LEN` → [`Backend::read_mem`] in `READ_MAX`-Stücken (Syscall 24).
/// - `MADDR,LEN:HEX` → [`Backend::write_mem`] in `WRITE_MAX`-Stücken (Syscall 33),
///   Erfolg ist `OK`.
/// - `c`/`s`/`Z`/`G`/alles andere → leer (`$#00`): benannt, nicht bedient (34/35 fehlen,
///   `c` wartet auf die Halt-Disziplin des PD-Eintritts).
///
/// Gibt die Antwortlänge zurück oder `None`, wenn `aus` nicht reicht (dann steht nichts
/// Halbfertiges darin — was zählt, ist der Rückgabewert, nicht der Pufferinhalt).
pub fn bediene(nutzlast: &[u8], backend: &mut impl Backend, aus: &mut [u8]) -> Option<usize> {
    match klassifiziere(nutzlast) {
        Befehl::QSupported => kodiere(QSUPPORTED_ANTWORT, aus),
        Befehl::Frage => kodiere(ANHALTEGRUND, aus),
        Befehl::LiesRegister => bediene_g(backend, aus),
        Befehl::LiesSpeicher => bediene_m(nutzlast, backend, aus),
        Befehl::SchreibSpeicher => bediene_gross_m(nutzlast, backend, aus),
        Befehl::SchreibRegister => kodiere_leer(aus),
        Befehl::Fortsetzen => kodiere_leer(aus),
        Befehl::Einzelschritt => kodiere_leer(aus),
        Befehl::Unbekannt => kodiere_leer(aus),
    }
}

/// Hex-kodiert `daten` nach `aus` (2 Zeichen je Byte, klein). `None`, wenn `aus` nicht
/// reicht — die Sendeseite von `g`/`m` ohne Allokation.
fn hex_kodiere(daten: &[u8], aus: &mut [u8]) -> Option<usize> {
    if aus.len() < 2 * daten.len() {
        return None;
    }
    let mut i = 0;
    while i < daten.len() {
        aus[2 * i] = hex_zeichen(daten[i] >> 4);
        aus[2 * i + 1] = hex_zeichen(daten[i] & 0x0f);
        i += 1;
    }
    Some(2 * daten.len())
}

/// Liest bis zu 16 Hexziffern als `u64`. `None` bei leer/zu lang/ungültig/Überlauf —
/// eine Adresse, die man nicht aussprechen kann, wird abgewiesen, nicht gerundet.
fn parse_hex_u64(ziffern: &[u8]) -> Option<u64> {
    if ziffern.is_empty() || ziffern.len() > 16 {
        return None;
    }
    let mut wert: u64 = 0;
    let mut i = 0;
    while i < ziffern.len() {
        let z = hex_wert(ziffern[i])? as u64;
        wert = wert.checked_mul(16)?.checked_add(z)?;
        i += 1;
    }
    Some(wert)
}

/// Zerlegt `mADDR,LEN` in (Adresse, Länge). `None` = missgebildet (Antwort: leer).
fn parse_m(nutzlast: &[u8]) -> Option<(u64, u64)> {
    if nutzlast.first() != Some(&b'm') {
        return None;
    }
    let rest = &nutzlast[1..];
    let mut komma = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b',' {
            komma = Some(i);
            break;
        }
        i += 1;
    }
    let komma = komma?;
    let addr = parse_hex_u64(&rest[..komma])?;
    let len = parse_hex_u64(&rest[komma + 1..])?;
    Some((addr, len))
}

/// Zerlegt `MADDR,LEN:HEXDaten` in (Adresse, Länge, Offset der Hexdaten). `None` =
/// missgebildet (Antwort: leer) — der Längenabgleich gegen die Hexdaten steht in
/// [`bediene_gross_m`] (eigene Absage, eigene Stelle).
fn parse_gross_m(nutzlast: &[u8]) -> Option<(u64, u64, usize)> {
    if nutzlast.first() != Some(&b'M') {
        return None;
    }
    let rest = &nutzlast[1..];
    let mut komma = None;
    let mut doppel = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b',' && komma.is_none() {
            komma = Some(i);
        }
        if rest[i] == b':' {
            doppel = Some(i);
            break;
        }
        i += 1;
    }
    let (komma, doppel) = (komma?, doppel?);
    if doppel < komma {
        return None;
    }
    let addr = parse_hex_u64(&rest[..komma])?;
    let len = parse_hex_u64(&rest[komma + 1..doppel])?;
    Some((addr, len, 1 + doppel + 1))
}

/// `g`: Registerblock lesen, als Hex-Blob antworten.
///
/// Der Block kommt aus [`Backend::read_regs`] (produktiv Sidecar-read-only, kein Syscall;
/// die Form — ein gedeckelter Blocktransfer — ist `DEBUG_READ_MEM`-artig, die Nummer 24
/// steht hier bewusst NICHT: wer `g` an 24 hinge, läse Zielspeicher statt Registerframe).
/// Backend-Absage → [`FEHLER_BACKEND`]; ein Backend, das mehr meldet als der Puffer
/// fasst, lügt → ebenfalls [`FEHLER_BACKEND`] (trauen, nicht vertrauen).
fn bediene_g(backend: &mut impl Backend, aus: &mut [u8]) -> Option<usize> {
    let mut roh = [0u8; 512];
    let n = match backend.read_regs(&mut roh) {
        Ok(n) => n,
        Err(_) => return kodiere(FEHLER_BACKEND, aus),
    };
    if n > roh.len() {
        return kodiere(FEHLER_BACKEND, aus);
    }
    let mut hex = [0u8; 1024];
    let m = hex_kodiere(&roh[..n], &mut hex)?;
    kodiere(&hex[..m], aus)
}

/// `mADDR,LEN`: Zielspeicher lesen, als Hex antworten.
///
/// Die PD schleift in `READ_MAX`-Stücken (Aufrufform wie `libcaprock::load`: gespiegelte
/// Nummer, Client-seitige Form, der Syscall selbst steht im PD-Eintritt). Teillänge
/// heisst Lücke im Ziel: die Antwort ist dann kürzer (gesehen, nicht geraten) — ein
/// fehlender Aufrufer-Puffer ist [`FEHLER_BACKEND`]. Missgebildet → leer.
fn bediene_m(nutzlast: &[u8], backend: &mut impl Backend, aus: &mut [u8]) -> Option<usize> {
    let (mut addr, len) = match parse_m(nutzlast) {
        Some(v) => v,
        None => return kodiere_leer(aus),
    };
    if len == 0 {
        return kodiere(b"", aus);
    }
    // Gerahmte Antwort: `$` + 2 Zeichen je Byte (Hex braucht nie Escape) + `#csum`.
    if len > (aus.len() as u64).saturating_sub(4) / 2 {
        return None;
    }
    aus[0] = b'$';
    let mut p = 1usize;
    let mut summe: u8 = 0;
    let mut roh = [0u8; 64];
    let mut rest = len;
    while rest > 0 {
        let stueck = if rest < roh.len() as u64 {
            rest
        } else {
            roh.len() as u64
        };
        let n = match backend.read_mem(addr, stueck, &mut roh[..stueck as usize]) {
            Ok(n) => n,
            Err(_) => return kodiere(FEHLER_BACKEND, aus),
        };
        if n > stueck {
            return kodiere(FEHLER_BACKEND, aus);
        }
        if n == 0 {
            break; // Lücke im Ziel — bisherige Zahl gilt, kein Raten
        }
        let mut i = 0u64;
        while i < n {
            let byte = roh[i as usize];
            let hi = hex_zeichen(byte >> 4);
            let lo = hex_zeichen(byte & 0x0f);
            aus[p] = hi;
            summe = summe.wrapping_add(hi);
            p += 1;
            aus[p] = lo;
            summe = summe.wrapping_add(lo);
            p += 1;
            i += 1;
        }
        addr = addr.wrapping_add(n);
        rest -= n;
        if n < stueck {
            break; // Teillänge = Lücke danach — kurz antworten, nicht weiterlesen
        }
    }
    aus[p] = b'#';
    p += 1;
    aus[p] = hex_zeichen(summe >> 4);
    p += 1;
    aus[p] = hex_zeichen(summe & 0x0f);
    p += 1;
    Some(p)
}

/// `MADDR,LEN:HEXDaten`: Zielspeicher schreiben, Erfolg ist `OK`.
///
/// Spiegel von [`bediene_m`] (eigene Kapazität je Richtung, keine Zahl mit zwei
/// Bedeutungen): die Hexdaten werden in 64-Byte-Rohstücken dekodiert und je Stück über
/// [`Backend::write_mem`] geschrieben (Syscall 33, `WRITE_MAX`-Deckel im Eintritt).
/// Regeln, je eigene Stelle: Längenwiderspruch (`2*LEN != Hexdaten`) → leer (Absage,
/// kein Kürzen); ungültige Hexziffer → leer; Backend-Teillänge (Lücke) →
/// [`FEHLER_BACKEND`] (kein Teil-`OK` — ein halber Schreibbefehl ist kein Erfolg).
fn bediene_gross_m(
    nutzlast: &[u8],
    backend: &mut impl Backend,
    aus: &mut [u8],
) -> Option<usize> {
    let (mut addr, len, hex_ab) = match parse_gross_m(nutzlast) {
        Some(v) => v,
        None => return kodiere_leer(aus),
    };
    let hexdaten = nutzlast.get(hex_ab..)?;
    let erwartet = match len.checked_mul(2) {
        Some(e) => e,
        None => return kodiere_leer(aus),
    };
    if hexdaten.len() as u64 != erwartet {
        return kodiere_leer(aus);
    }
    if len == 0 {
        return kodiere(b"OK", aus);
    }
    let mut roh = [0u8; 64];
    let mut pos = 0usize;
    while pos < hexdaten.len() {
        let rest_hex = hexdaten.len() - pos;
        let stueck_hex = if rest_hex < 128 { rest_hex } else { 128 };
        let stueck = stueck_hex / 2;
        let mut j = 0;
        while j < stueck {
            let hi = match hex_wert(hexdaten[pos + 2 * j]) {
                Some(h) => h,
                None => return kodiere_leer(aus),
            };
            let lo = match hex_wert(hexdaten[pos + 2 * j + 1]) {
                Some(l) => l,
                None => return kodiere_leer(aus),
            };
            roh[j] = (hi << 4) | lo;
            j += 1;
        }
        match backend.write_mem(addr, &roh[..stueck]) {
            Ok(n) if n == stueck as u64 => {}
            _ => return kodiere(FEHLER_BACKEND, aus),
        }
        addr = addr.wrapping_add(stueck as u64);
        pos += stueck_hex;
    }
    kodiere(b"OK", aus)
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

    // --- Stufe 2 (RSP-WIRE): Bedienung mit Fake-Backend ----------------------------------
    //
    // Das Fake spiegelt die Kernel-Semantik (Lücke = Teillänge, Absage = Err), nicht den
    // Kernel: 256 Bytes Bild ab VA 0x1000, 16 Bytes Registerblob. Was hier bewiesen wird,
    // ist die PD-Seite (Parsen, Schleifen, Antworten) — der Syscall selbst steht im
    // PD-Eintritt.

    /// Fake-Ziel: 256 Bytes ab 0x1000 (`speicher[i] == i`), 16 Bytes Register (`a0..af`).
    struct FakeBackend {
        speicher: [u8; 256],
        register: [u8; 16],
    }

    impl FakeBackend {
        const BASIS: u64 = 0x1000;

        fn neu() -> Self {
            let mut f = FakeBackend {
                speicher: [0; 256],
                register: [0; 16],
            };
            let mut i = 0;
            while i < 256 {
                f.speicher[i] = i as u8;
                i += 1;
            }
            let mut j = 0;
            while j < 16 {
                f.register[j] = 0xa0 + j as u8;
                j += 1;
            }
            f
        }
    }

    impl super::Backend for FakeBackend {
        fn read_mem(&mut self, va: u64, len: u64, out: &mut [u8]) -> Result<u64, u64> {
            if (out.len() as u64) < len {
                return Err(1);
            }
            let mut n = 0u64;
            while n < len {
                let addr = match va.checked_add(n) {
                    Some(a) => a,
                    None => break,
                };
                let off = match addr.checked_sub(Self::BASIS) {
                    Some(o) => o,
                    None => break, // vor dem Bild: Lücke, bisherige Zahl gilt
                };
                if off >= self.speicher.len() as u64 {
                    break; // hinter dem Bild: Lücke, bisherige Zahl gilt
                }
                out[n as usize] = self.speicher[off as usize];
                n += 1;
            }
            Ok(n)
        }

        fn write_mem(&mut self, va: u64, daten: &[u8]) -> Result<u64, u64> {
            let mut n = 0u64;
            while n < daten.len() as u64 {
                let addr = match va.checked_add(n) {
                    Some(a) => a,
                    None => break,
                };
                let off = match addr.checked_sub(Self::BASIS) {
                    Some(o) => o,
                    None => break,
                };
                if off >= self.speicher.len() as u64 {
                    break;
                }
                self.speicher[off as usize] = daten[n as usize];
                n += 1;
            }
            Ok(n)
        }

        fn read_regs(&mut self, out: &mut [u8]) -> Result<usize, u64> {
            if out.len() < self.register.len() {
                return Err(1);
            }
            let mut i = 0;
            while i < self.register.len() {
                out[i] = self.register[i];
                i += 1;
            }
            Ok(self.register.len())
        }
    }

    /// Rahmt-prüft eine `bediene`-Antwort: gültiger Rahmen, gibt die Nutzdaten zurück.
    fn bediente_nutzdaten(rahmen: &[u8]) -> std::vec::Vec<u8> {
        let mut p = Parser::neu();
        let e = p.rahmen(rahmen);
        assert_eq!(e, Some(Ereignis::Paket(p.nutzlast().len())));
        p.nutzlast().to_vec()
    }

    #[test]
    fn m_rundweg_liest_ueber_read_mem() {
        // speicher[0..4] == [0,1,2,3] → "00010203".
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 64];
        let n = bediene(b"m1000,4", &mut backend, &mut aus).expect("passt");
        assert_eq!(bediente_nutzdaten(&aus[..n]), b"00010203");
    }

    #[test]
    fn m_rundweg_schleift_ueber_chunks() {
        // 200 Bytes > ein 64er-Rohstück: beweist die PD-Schleife, nicht einen Aufruf.
        // Prüfsumme läuft über die Antwort mit (Parser nähme einen Zahlendreher nicht an).
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 512];
        let n = bediene(b"m1000,c8", &mut backend, &mut aus).expect("passt");
        let nutz = bediente_nutzdaten(&aus[..n]);
        assert_eq!(nutz.len(), 400);
        assert_eq!(&nutz[..8], b"00010203");
        assert_eq!(&nutz[396..], b"c6c7"); // Byte 198/199 = 0xc6/0xc7
        // Lücke: ab 0x1100 steht nichts mehr — kurze Antwort statt Raten.
        let mut aus2 = [0u8; 64];
        let n2 = bediene(b"m10ff,4", &mut backend, &mut aus2).expect("passt");
        assert_eq!(bediente_nutzdaten(&aus2[..n2]), b"ff");
        // Missgebildet ist keine Bedienung: leer, nie Erfolg.
        let mut aus3 = [0u8; 16];
        let n3 = bediene(b"mkein-komma", &mut backend, &mut aus3).expect("passt");
        assert_eq!(&aus3[..n3], b"$#00");
    }

    #[test]
    fn gross_m_rundweg_schreibt_ueber_write_mem() {
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 16];
        let n = bediene(b"M1000,4:deadbeef", &mut backend, &mut aus).expect("passt");
        assert_eq!(&aus[..n], b"$OK#9a");
        assert_eq!(&backend.speicher[0..4], &[0xde, 0xad, 0xbe, 0xef]);
        // Rücklesen über `m` beweist den Rundweg, nicht nur die Antwort.
        let mut aus2 = [0u8; 64];
        let n2 = bediene(b"m1000,4", &mut backend, &mut aus2).expect("passt");
        assert_eq!(bediente_nutzdaten(&aus2[..n2]), b"deadbeef");
        // Längenwiderspruch: Absage (leer), kein Kürzen auf "dead".
        let mut aus3 = [0u8; 16];
        let n3 = bediene(b"M1000,4:dead", &mut backend, &mut aus3).expect("passt");
        assert_eq!(&aus3[..n3], b"$#00");
        assert_eq!(&backend.speicher[0..4], &[0xde, 0xad, 0xbe, 0xef]); // unberührt
        // Ungültige Ziffer: ebenfalls leer, ebenfalls ohne Wirkung.
        let n4 = bediene(b"M1000,1:zz", &mut backend, &mut aus3).expect("passt");
        assert_eq!(&aus3[..n4], b"$#00");
        // Schreiben in die Lücke (hinter dem Bild): E01, kein Teil-OK.
        // Prüfsumme: 'E'+'0'+'1' = 0x45+0x30+0x31 = 0xA6.
        let n5 = bediene(b"M1200,2:4142", &mut backend, &mut aus3).expect("passt");
        assert_eq!(&aus3[..n5], b"$E01#a6");
    }

    #[test]
    fn gross_m_rundweg_schleift_ueber_chunks() {
        // 200 Bytes Nutzdaten (400 Hexzeichen) > ein 64er-Dekodierstück: beweist die
        // Schreib-Schleife der PD. Aufgebaut mit std — Testseite, nicht PD-Seite.
        let muster: std::vec::Vec<u8> = (0..200u32).map(|i| (i & 0xff) as u8).collect();
        let mut hex = std::string::String::new();
        for b in muster.iter() {
            hex.push(hex_zeichen(b >> 4) as char);
            hex.push(hex_zeichen(b & 0x0f) as char);
        }
        let anfrage = std::format!("M1000,c8:{}", hex);
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 16];
        let n = bediene(anfrage.as_bytes(), &mut backend, &mut aus).expect("passt");
        assert_eq!(&aus[..n], b"$OK#9a");
        assert_eq!(&backend.speicher[..200], &muster[..]);
    }

    #[test]
    fn g_rundweg_liest_registerblob() {
        // Register a0..af als Hex-Blob — produktiv Sidecar (kein Syscall 24), hier Fake.
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 64];
        let n = bediene(b"g", &mut backend, &mut aus).expect("passt");
        assert_eq!(
            bediente_nutzdaten(&aus[..n]),
            b"a0a1a2a3a4a5a6a7a8a9aaabacadaeaf"
        );
    }

    #[test]
    fn qsupported_nennt_nur_was_geht() {
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 32];
        let n = bediene(b"qSupported:multiprocess+", &mut backend, &mut aus).expect("passt");
        assert_eq!(bediente_nutzdaten(&aus[..n]), b"PacketSize=2048");
    }

    #[test]
    fn frage_nennt_s05() {
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 16];
        let n = bediene(b"?", &mut backend, &mut aus).expect("passt");
        assert_eq!(bediente_nutzdaten(&aus[..n]), b"S05");
    }

    #[test]
    fn c_s_z_bleiben_benannt_unbedient() {
        // `c` (CONTINUE=23 existiert, Halt-Disziplin fehlt), `s` (34 fail-closed),
        // `Z` (35 fail-closed, Z0 nie per Design), `G` (Maske fehlt): alle leer.
        // Doku welcher Syscall fehlt: s. sys-Modul + ANBINDUNG.md, Anhang "Stufe 2".
        let faelle: &[&[u8]] = &[
            b"c",
            b"c1000",
            b"s",
            b"s1000",
            b"Z0,1000,4",
            b"Z1,1000,4",
            b"z1,1000,4",
            b"G",
        ];
        let mut backend = FakeBackend::neu();
        for rahmen in faelle {
            let mut aus = [0u8; 16];
            let n = bediene(rahmen, &mut backend, &mut aus).expect("passt");
            assert_eq!(&aus[..n], b"$#00", "Rahmen {:?}", rahmen);
        }
    }

    #[test]
    fn paket_in_paket_bleibt_schichtentreu() {
        // Ein kompletter innerer Rahmentext als Nutzdaten: maskiert auf der Leitung,
        // entmaskiert im Parser, benannt (Unbekannt → leer) in der Bedienung — die
        // innere `$...#`-Form löst aussen NICHTS aus (kein Registerlesen, kein OK).
        let innen = b"$g#67";
        let mut leitung = [0u8; 32];
        let rn = kodiere(innen, &mut leitung).expect("passt");
        let mut p = Parser::neu();
        assert_eq!(p.rahmen(&leitung[..rn]), Some(Ereignis::Paket(innen.len())));
        assert_eq!(p.nutzlast(), innen);
        let mut backend = FakeBackend::neu();
        let mut aus = [0u8; 16];
        let n = bediene(p.nutzlast(), &mut backend, &mut aus).expect("passt");
        assert_eq!(&aus[..n], b"$#00");
    }
}
