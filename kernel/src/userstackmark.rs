//! **Die EL0-Wasserstandsmarke** — wie tief ist der USER-Stack wirklich? (C7b)
//!
//! # Die Frage, und warum sie eine andere ist als bei [`crate::kstackmark`]
//!
//! `kstackmark` misst den **EL1**-Stack eines EL0-Threads (16 KiB, Kernelseite). Diese Datei misst
//! die **private Region** einer isolierten PD — und die ist der **EL0-Stack EINES Threads**. Sie
//! ist mit 2 MiB der grösste Einzelposten je Mandant (C7b: 2048 von rund 2076 KiB) und damit der
//! Hebel für die Mandantendichte. Ohne diese Zahl ist jede kleinere Regionsgrösse **geraten** —
//! dieselbe Lage wie beim EL1-Stack vor der Wasserstandsmarke.
//!
//! # Warum es hier KEIN Füllmuster gibt
//!
//! Die Region wird bei der Zuteilung mit `zero_phys` **genullt** — das ist keine Messvorkehrung,
//! sondern die Datenremanenz-Zusicherung (`zerotest`): nichts geht ungenullt an ein Subjekt. Das
//! **höchste von Null verschiedene Byte IST damit der Wasserstand**, und die Messung ist umsonst
//! zu haben. Ein zusätzliches Muster wäre nicht nur überflüssig, es wäre **schädlich**: es stünde
//! genau der Nullung entgegen, die das Subjekt vorfinden soll.
//!
//! Der Preis dieser Wahl steht unten unter „Was das Verfahren NICHT sieht" — er ist gemessen und
//! nicht weggeredet.
//!
//! # Warum das Verfahren fail-closed ist
//!
//! Fällt die **Nullung** aus, trägt der Fuss der Region Fremdbytes; [`unberuehrt`] gibt dann `0`,
//! die benutzte Tiefe ist die **volle** Regionsgrösse, und das Urteil fällt durch. Der Ausfall der
//! Messvorbedingung sieht aus wie der **schlimmste** Messwert, nicht wie der beste — dieselbe
//! Umkehrung von „Schweigen als Erfolg" wie bei `kstackmark`.
//!
//! Der zweite Weg, auf dem eine solche Zeile still grün wird, ist **gar nicht messen**. Dagegen
//! stehen zwei Konjunkte: eine Mindestzahl von Messungen **vor** dem Gatter und ein Höchststand
//! `> 0`. Bei der Null-Fassung ist der zweite unverzichtbar — anders als beim Muster ist „nie
//! benutzt" hier ein *plausibler* Messwert (ein Thread, der nie eine Instruktion ausführt, hinter-
//! lässt eine vollständig genullte Region), und ohne diesen Punkt wäre er von „viel Luft" nicht zu
//! unterscheiden. Genau das ist der Fehler, der die alte Kurvenzeile entwertet hat: sie zählte
//! PDs, deren Thread sofort starb.
//!
//! Und der dritte: das Messgerät selbst ist kaputt. Dagegen stehen **zwei** Proben, und sie prüfen
//! Verschiedenes:
//!
//! * [`eichung`] — das **Messgerät** an einem Feld bekannter Tiefe (drei Fälle, s. dort).
//! * die **Tiefensonde** ([`sonde_messen`]) — der **gemessene Pfad**: ein echter EL0-Thread
//!   berührt seinen echten Stack bis zu einer bekannten krummen Tiefe, und die Marke muss ihn mit
//!   genau dieser Tiefe wiederfinden. Sie prüft, was die Eichung strukturell nicht kann: dass die
//!   Buchführung (Slot → Basis/Länge) auf die **richtige** Region zeigt. Ein Registereintrag, der
//!   auf eine fremde Region zeigt, bestünde jede Eichung.
//!
//! # Was das Verfahren NICHT sieht
//!
//! * **Eine mit NULL beschriebene Stackstelle.** Das Muster-Verfahren von `kstackmark` erkennt
//!   *jeden* Schreibzugriff; das Null-Verfahren nur solche mit einem von Null verschiedenen Wert.
//!   Ein `mov qword [rsp-8], 0` ist unsichtbar. Der gemessene Wert ist damit eine **Untergrenze**
//!   der wahren Tiefe — und deshalb wird die Regionsgrösse nicht knapp an den Höchststand gelegt,
//!   sondern über die Summenbedingung mit einer Reserve, die ein Vielfaches davon beträgt.
//! * **Eine Region, deren Thread nie lief.** Sie meldet Tiefe `0`. Das ist kein Messfehler,
//!   sondern eine Tatsache über den Thread — sie wird **gezählt** ([`Marke::nie_benutzt`]) und
//!   gedruckt, statt in den Höchststand einzugehen und dort unsichtbar zu werden.
//! * **Threads, die nie sterben und nie gefegt werden.** Deshalb fegt der Bericht am Schluss über
//!   alle **lebenden** Regionen (`system::userstack_marke_fegen`).

use core::sync::atomic::{AtomicUsize, Ordering};

/// Wie viele Bytes am **Fuss** der Region sind noch **null**? (Der Stack wächst nach unten;
/// unberührt ist also, was unten liegt.)
///
/// Gibt `0` zurück, wenn schon das unterste Wort nicht null ist — das heisst „aufgebraucht
/// **oder** nie genullt", und beide Lagen sollen dasselbe, nämlich das schlechteste Urteil
/// auslösen.
///
/// # Safety
/// `base` muss auf `len` Bytes lesbaren, 8-Byte-ausgerichteten Speicher zeigen.
pub unsafe fn unberuehrt(base: usize, len: usize) -> usize {
    let p = base as *const u64;
    let worte = len / 8;
    let mut i = 0usize;
    // SAFETY: die Schleife bleibt in `[base, base+worte*8)`; `read_volatile`, weil der Speicher
    // für den Compiler uninitialisiert aussieht.
    while i < worte && unsafe { p.add(i).read_volatile() } == 0 {
        i += 1;
    }
    i * 8
}

macro_rules! zaehler {
    ($name:ident, $init:expr) => {
        static $name: AtomicUsize = AtomicUsize::new($init);
    };
}

zaehler!(REGISTRIERT, 0);
zaehler!(GEMESSEN, 0);
zaehler!(GEMESSEN_TOD, 0);
zaehler!(TIEFE_MAX, 0);
zaehler!(TIEFE_TOD, 0);
zaehler!(TIEFE_LEBEND, 0);
zaehler!(TIEFSTER_SLOT, usize::MAX);
/// Grösse der Region, in der der Höchststand gemessen wurde (die Klasse fasst **verschiedene**
/// Grössen — 2 MiB isoliert, ein Farbstreifenstück gefärbt, 16 KiB geladen, 64 KiB SAS).
zaehler!(TIEFSTE_GROESSE, 0);
/// Die **kleinste** je gemessene Region. Sie und nicht „die" Grösse trägt die Summenbedingung:
/// eine Klasse mit mehreren Grössen ist genau so tragfähig wie ihr kleinstes Mitglied.
zaehler!(GROESSE_MIN, usize::MAX);
zaehler!(GROESSE_MAX, 0);
/// Regionen, deren Fuss NICHT null war — „aufgebraucht oder nie genullt".
zaehler!(ERSCHOEPFT, 0);
/// Regionen mit Tiefe 0: ihr Thread hat nie ein von Null verschiedenes Byte auf den Stack gelegt.
zaehler!(NIE_BENUTZT, 0);
/// Höchster gemessener **Füllgrad** in Promille (`benutzt * 1000 / len`) — die Grösse, die über
/// verschieden grosse Regionen hinweg vergleichbar ist.
zaehler!(FUELL_MAX_PROMILLE, 0);

/// Anlass einer Messung — dieselbe Unterscheidung wie bei `kstackmark`, und aus demselben Grund:
/// nur die Messungen aus dem Sterbepfad liegen **vor** dem Gatter.
#[derive(Clone, Copy)]
pub enum Anlass {
    /// Der Thread stirbt; seine Region geht gleich an den Allokator zurück (`reap_core`).
    Tod(usize),
    /// Der Lauf endet, der Thread lebt — Schlussfegen bzw. Sondenmessung.
    Lebend(usize),
}

/// Eine EL0-Region messen und einrechnen. Gibt `(benutzt, frei)`.
///
/// # Safety
/// `base`/`len` müssen eine gültige, identisch abgebildete RAM-Region beschreiben, die gerade
/// nicht von einem anderen Kern **freigegeben** wird. Ein gleichzeitig darauf rechnender Thread
/// ist unschädlich: gelesen wird nur, und ein tiefer werdender Stack kann den Messwert nur zu
/// **klein** machen, nie zu gross.
#[cfg(feature = "selftest")]
pub unsafe fn messen(base: usize, len: usize, anlass: Anlass) -> (usize, usize) {
    if base == 0 || len < 8 {
        return (0, 0);
    }
    // SAFETY: Zusicherung des Aufrufers, weitergereicht.
    let frei = unsafe { unberuehrt(base, len) };
    let benutzt = len - frei;
    GEMESSEN.fetch_add(1, Ordering::Relaxed);
    GROESSE_MIN.fetch_min(len, Ordering::Relaxed);
    GROESSE_MAX.fetch_max(len, Ordering::Relaxed);
    if frei == 0 {
        ERSCHOEPFT.fetch_add(1, Ordering::Relaxed);
    }
    if benutzt == 0 {
        NIE_BENUTZT.fetch_add(1, Ordering::Relaxed);
    }
    FUELL_MAX_PROMILLE.fetch_max(benutzt * 1000 / len, Ordering::Relaxed);
    // Der Rekordhalter wird mit Slot **und Regionsgrösse** festgehalten. Ohne die Grösse wäre
    // „Höchststand 6280 B" in einer Klasse mit vier verschiedenen Grössen nicht einzuordnen. Das
    // Rennen dabei ist dasselbe wie bei `kstackmark` und aus demselben Grund hingenommen: die
    // Tiefe selbst ist über `fetch_max` korrekt, die Zuordnung ist Diagnose.
    if TIEFE_MAX.fetch_max(benutzt, Ordering::Relaxed) < benutzt {
        TIEFSTER_SLOT.store(
            match anlass {
                Anlass::Tod(s) | Anlass::Lebend(s) => s,
            },
            Ordering::Relaxed,
        );
        TIEFSTE_GROESSE.store(len, Ordering::Relaxed);
    }
    match anlass {
        Anlass::Tod(_) => {
            TIEFE_TOD.fetch_max(benutzt, Ordering::Relaxed);
            GEMESSEN_TOD.fetch_add(1, Ordering::Relaxed);
        }
        Anlass::Lebend(_) => {
            TIEFE_LEBEND.fetch_max(benutzt, Ordering::Relaxed);
        }
    }
    (benutzt, frei)
}

/// Ohne `selftest`: No-Op — die Messmaschinerie gehört nicht in den schlanken Kernel (F1).
///
/// # Safety
/// trivialerweise erfüllt (No-Op).
#[cfg(not(feature = "selftest"))]
#[inline(always)]
pub unsafe fn messen(_b: usize, _l: usize, _a: Anlass) -> (usize, usize) {
    (0, 0)
}

/// Eine registrierte Region buchen (Sprechprobe der **Registrierseite**).
pub fn registriert() {
    REGISTRIERT.fetch_add(1, Ordering::Relaxed);
}

// ------------------------------------------------------------------------------------------
// DIE EICHUNG — die Sprechprobe des MESSGERAETS
// ------------------------------------------------------------------------------------------

/// Ergebnis der [`eichung`] — Bitmaske, `0` = nie gelaufen.
static EICHUNG: AtomicUsize = AtomicUsize::new(0);

/// Bit 0: ein **nicht genulltes** Feld meldet `0` unberührte Bytes (der fail-closed-Fall).
pub const EICH_SCHMUTZ: usize = 1;
/// Bit 1: ein **genulltes, unberührtes** Feld meldet die volle Länge.
pub const EICH_VOLL: usize = 2;
/// Bit 2: ein bis zu **bekannter** Tiefe berührtes Feld meldet genau diese Tiefe.
pub const EICH_TIEFE: usize = 4;
/// Bit 3: die Eichung ist überhaupt gelaufen (Sprechprobe der Sprechprobe).
pub const EICH_GELAUFEN: usize = 8;
/// Alle vier Eichbits.
pub const EICH_ALLE: usize = EICH_SCHMUTZ | EICH_VOLL | EICH_TIEFE | EICH_GELAUFEN;

/// Wörter des Eichfeldes (2 KiB BSS).
const EICH_WORTE: usize = 256;
/// Die bekannte Tiefe, auf die die Eichung das Feld berührt (in Wörtern) — krumm mit Absicht:
/// ein Fehler um eine Zweierpotenz fällt auf.
const EICH_TIEFE_WORTE: usize = 37;
/// Der Schmutzwert für Fall (a). Ein von Null verschiedener Wert, sonst nichts Besonderes — es
/// geht um „nicht null", nicht um ein Muster.
const SCHMUTZ: u64 = 0xDEAD_BEEF_CAFE_F00D;

#[repr(align(64))]
struct Eichfeld(core::cell::UnsafeCell<[u64; EICH_WORTE]>);
// SAFETY: das Feld wird ausschliesslich in `eichung()` benutzt, und die läuft genau einmal
// (Latch über `EICHUNG`), bevor Sekundärkerne Selbsttests fahren.
unsafe impl Sync for Eichfeld {}
static EICHFELD: Eichfeld = Eichfeld(core::cell::UnsafeCell::new([0; EICH_WORTE]));

/// Das Messgerät an einer **bekannten** Tiefe prüfen. Einmal beim Hochlauf zu rufen.
///
/// Die drei Fälle sind nicht austauschbar: (a) ist der fail-closed-Nachweis, (b) der triviale
/// Vollausschlag — und erst (c) unterscheidet ein arbeitendes Messgerät von einer Funktion, die
/// nur zwei Zahlen kennt.
#[cfg(feature = "selftest")]
pub fn eichung() -> usize {
    let base = EICHFELD.0.get() as usize;
    let len = EICH_WORTE * 8;
    let mut bits = EICH_GELAUFEN;

    // (a) **Nicht genullt**: das unterste Wort ist kein Null -> 0 unberührt. Das ist der Fall
    //     „die Nullung ist ausgefallen", und er MUSS den schlechtesten Messwert ergeben.
    // SAFETY: eigenes, exklusiv benutztes Feld.
    unsafe {
        for i in 0..EICH_WORTE {
            (base as *mut u64).add(i).write_volatile(SCHMUTZ);
        }
    }
    // SAFETY: dito.
    if unsafe { unberuehrt(base, len) } == 0 {
        bits |= EICH_SCHMUTZ;
    }

    // (b) **Genullt, unberührt**: volle Länge.
    // SAFETY: dito.
    unsafe { core::ptr::write_bytes(base as *mut u8, 0, len) };
    // SAFETY: dito.
    if unsafe { unberuehrt(base, len) } == len {
        bits |= EICH_VOLL;
    }

    // (c) Bis zu einer **bekannten** Tiefe berührt: genau diese Tiefe. Berührt wird von oben, wie
    //     ein Stack; das unterste berührte Wort liegt bei `EICH_WORTE - EICH_TIEFE_WORTE`.
    let erstes = EICH_WORTE - EICH_TIEFE_WORTE;
    // SAFETY: `erstes < EICH_WORTE`; exklusives Feld.
    unsafe { (base as *mut u64).add(erstes).write_volatile(SCHMUTZ) };
    // SAFETY: dito.
    if unsafe { unberuehrt(base, len) } == erstes * 8 {
        bits |= EICH_TIEFE;
    }

    // Feld wieder nullen — es soll nicht als „benutzte Region" herumliegen.
    // SAFETY: dito.
    unsafe { core::ptr::write_bytes(base as *mut u8, 0, len) };

    EICHUNG.store(bits, Ordering::Release);
    bits
}

/// Ohne `selftest`: nicht vorhanden (die Eichung ist Testmaschinerie).
#[cfg(not(feature = "selftest"))]
pub fn eichung() -> usize {
    0
}

/// Stand der Eichung (`0` = nie gelaufen).
pub fn eichstand() -> usize {
    EICHUNG.load(Ordering::Acquire)
}

// ------------------------------------------------------------------------------------------
// DIE TIEFENSONDE — die Sprechprobe des GEMESSENEN PFADES
// ------------------------------------------------------------------------------------------
//
// Die Eichung prüft das Messgerät an einem Feld im BSS. Sie kann strukturell **nicht** prüfen,
// ob die Buchführung auf die richtige Region zeigt: ein Registereintrag, der auf eine fremde
// (genullte) Region verweist, meldet brav „viel Luft" und besteht jede Eichung. Deshalb gibt es
// die Sonde — ein echter EL0-Thread berührt seinen echten Stack bis zu einer bekannten Tiefe, und
// hier wird nachgesehen, ob die Marke ihn mit genau dieser Tiefe wiederfindet.

/// Sondenergebnis. Bit 63 = gemessen; Bit 0 = die Tiefe stimmt; Bits 8.. = gemessene Tiefe.
static SONDE: AtomicUsize = AtomicUsize::new(0);

/// Die Tiefe, auf die die Sonde ihren Stack berührt — **krumm mit Absicht** (eine Zweierpotenz
/// verwechselte man mit einer Rahmengrösse).
pub const SONDE_TIEFE: usize = 6152;
/// Wieviel darf die Messung über der Sondentiefe liegen? Der Frame der Sonde selbst und die
/// Rahmen ihres Aufrufwegs zählen mit; nach oben ist die Grenze eine Aussage: die Messung darf
/// **nicht** einfach die ganze Region als benutzt melden.
pub const SONDE_SCHLUPF: usize = 4096;

/// Das Ergebnis der Sondenmessung ablegen (aus `system::userstack_sonde_pruefen`).
pub fn sonde_melden(benutzt: usize) {
    let ok = benutzt >= SONDE_TIEFE && benutzt <= SONDE_TIEFE + SONDE_SCHLUPF;
    SONDE.store((1 << 63) | (benutzt << 8) | usize::from(ok), Ordering::Release);
}

/// `(gemessen, ok, gemessene Tiefe)`.
pub fn sonde_stand() -> (bool, bool, usize) {
    let v = SONDE.load(Ordering::Acquire);
    (v >> 63 != 0, v & 1 != 0, (v >> 8) & ((1 << 40) - 1))
}

// ------------------------------------------------------------------------------------------
// DER MESSSTAND
// ------------------------------------------------------------------------------------------

/// Der Messstand der EL0-Klasse.
#[derive(Clone, Copy)]
pub struct Marke {
    /// Wieviele EL0-Regionen wurden überhaupt registriert (Sprechprobe der Registrierseite).
    pub registriert: usize,
    /// Wieviele wurden gemessen.
    pub gemessen: usize,
    /// Wieviele davon vor dem Bericht (Sterbepfad) — **die** sieht das Gatter.
    pub gemessen_tod: usize,
    /// Grösste je beobachtete **benutzte** Tiefe in Bytes.
    pub tiefe_max: usize,
    /// Höchststand unter den sterbenden Threads.
    pub tiefe_tod: usize,
    /// Höchststand unter den lebenden Threads (Schlussfegen/Sonde).
    pub tiefe_lebend: usize,
    /// Thread-Slot des Rekordhalters (`usize::MAX` = keiner).
    pub tiefster_slot: usize,
    /// Regionsgrösse des Rekordhalters.
    pub tiefste_groesse: usize,
    /// Kleinste gemessene Region (`usize::MAX` = keine).
    pub groesse_min: usize,
    /// Grösste gemessene Region.
    pub groesse_max: usize,
    /// Regionen mit nicht genulltem Fuss (aufgebraucht **oder** nie genullt).
    pub erschoepft: usize,
    /// Regionen, deren Thread nie ein von Null verschiedenes Byte hinterliess.
    pub nie_benutzt: usize,
    /// Höchster Füllgrad in Promille.
    pub fuell_max_promille: usize,
}

/// Den Messstand lesen.
pub fn marke() -> Marke {
    Marke {
        registriert: REGISTRIERT.load(Ordering::Relaxed),
        gemessen: GEMESSEN.load(Ordering::Relaxed),
        gemessen_tod: GEMESSEN_TOD.load(Ordering::Relaxed),
        tiefe_max: TIEFE_MAX.load(Ordering::Relaxed),
        tiefe_tod: TIEFE_TOD.load(Ordering::Relaxed),
        tiefe_lebend: TIEFE_LEBEND.load(Ordering::Relaxed),
        tiefster_slot: TIEFSTER_SLOT.load(Ordering::Relaxed),
        tiefste_groesse: TIEFSTE_GROESSE.load(Ordering::Relaxed),
        groesse_min: GROESSE_MIN.load(Ordering::Relaxed),
        groesse_max: GROESSE_MAX.load(Ordering::Relaxed),
        erschoepft: ERSCHOEPFT.load(Ordering::Relaxed),
        nie_benutzt: NIE_BENUTZT.load(Ordering::Relaxed),
        fuell_max_promille: FUELL_MAX_PROMILLE.load(Ordering::Relaxed),
    }
}

// ------------------------------------------------------------------------------------------
// DAS URTEIL — an EINER Stelle, damit `all_done` und der Bericht dieselbe Wirklichkeit lesen
// ------------------------------------------------------------------------------------------

/// Mindestens frei zu bleibende Reserve am **Fuss** der Region — als Anteil `1/N`.
///
/// Dieselbe Form wie bei `kstackmark`, und aus demselben Grund: die Reserve ist die Grösse, um
/// die es geht, und sie bleibt richtig, wenn jemand die Regionsgrösse ändert.
pub const MIND_RESERVE_NENNER: usize = 8;

/// Wieviele EL0-Regionen mindestens **vor dem Gatter** gemessen sein müssen.
///
/// Eine Sprechprobe, kein Mass für die Beweislast (dieselbe Begründung wie bei
/// `kstackmark::MIND_MESSUNGEN`, das an dieser Stelle schon einmal die Suite gerissen hat: das
/// Gatter sieht nur den Sterbepfad, das Schlussfegen entsteht erst im Bericht).
pub const MIND_MESSUNGEN: usize = 2;

/// Die geforderte Mindestreserve für eine Regionsgrösse.
pub fn mindestreserve(groesse: usize) -> usize {
    groesse / MIND_RESERVE_NENNER
}

/// Das Urteil der `ustack`-Zeile. Fünf Konjunkte, jedes einzeln falsifizierbar:
///
/// 1. **Die Eichung trägt.** Sonst misst hier eine Funktion, die immer dasselbe sagt.
/// 2. **Die Tiefensonde ist wiedergefunden worden** — auf dem echten Pfad, mit ihrer bekannten
///    Tiefe. Das ist der Punkt, den die Eichung nicht leisten kann.
/// 3. **Es wurde vor dem Gatter überhaupt gemessen** (Sprechprobe am geprüften Pfad).
/// 4. **Keine Region hatte einen nicht genullten Fuss.** Fällt die Nullung aus, fällt dieses
///    Konjunkt — ein Wasserzeichen, das „viel Luft" meldet, weil nie genullt wurde, ist damit
///    ausgeschlossen.
/// 5. **Die Summenbedingung**: tiefster gemessener Pfad + geforderte Reserve ≤ **kleinste**
///    Region der Klasse. Eine Grösse, die nur den beobachteten Höchststand trägt, ist
///    statistisch; die Summe macht sie strukturell — und sie wird gegen die **kleinste** Region
///    gestellt, weil eine Klasse aus mehreren Grössen so tragfähig ist wie ihr kleinstes Mitglied.
pub fn urteil() -> bool {
    let m = marke();
    let (sonde_gemessen, sonde_ok, _) = sonde_stand();
    eichstand() == EICH_ALLE
        && sonde_gemessen
        && sonde_ok
        && m.gemessen_tod >= MIND_MESSUNGEN
        && m.groesse_min != usize::MAX
        && m.erschoepft == 0
        && m.tiefe_max + mindestreserve(m.groesse_min) <= m.groesse_min
}

/// Die drei Summanden der EL0-Rechnung: `(tiefster Pfad, zweiter Summand, geforderte Reserve,
/// kleinste Region)`.
///
/// **Der zweite Summand ist auf EL0 nachweislich NULL, und das ist eine Aussage über die
/// Architektur, keine Bequemlichkeit.** Auf dem EL1-Stack addiert `kstackmark` den tiefsten
/// Interrupt-Handler dazu, weil gewöhnlicher Kernelcode mit freigegebenen Interrupts läuft und
/// ein Handler dann auf **demselben** Stack landet. Auf EL0 gibt es dieses Gegenstück nicht: ein
/// Interrupt oder eine Exception aus Ring 3 wechselt **immer** den Stack — x86-64 lädt `RSP0` aus
/// der TSS, aarch64 läuft der Kernel auf `SP_EL1`. Nichts wird je unterhalb des User-`RSP`
/// abgelegt. Der einzige Mechanismus, der das ändern würde, sind **Signal-Handler auf dem
/// User-Stack**, und die gibt es in diesem System nicht (es gibt keinen Signal-Zustelldienst).
/// Kommt einer, ist dies die Stelle, an der sein Rahmen dazugehört.
pub fn summe() -> (u64, u64, u64, u64) {
    let m = marke();
    let gr = if m.groesse_min == usize::MAX { 0 } else { m.groesse_min };
    (m.tiefe_max as u64, 0, mindestreserve(gr) as u64, gr as u64)
}
