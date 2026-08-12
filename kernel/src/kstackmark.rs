//! **Die Stack-Wasserstandsmarke** — reichen 16 KiB, oder sind sie nur gross? (C4)
//!
//! # Der Unterschied, um den es geht
//!
//! Die `vorrat`-Zeile druckt seit dem 2026-08-10, wie **gross** die Kernel-Stacks sind:
//! `USER_KSTACK_SIZE = 16 KiB` je EL0-Thread, `STACK_SIZE = 64 KiB` je Kernel-Thread. Das ist eine
//! Zahl, kein Beleg. Sie beantwortet „was kostet ein schlafender Thread", aber nicht „was passiert
//! bei einem tiefen Kernelpfad" — und genau die zweite Frage entscheidet, ob man die Zahl senken
//! darf. **Ohne Wasserzeichen ist jede kleinere Zahl geraten.**
//!
//! Gemessen wird deshalb die **tatsaechlich benutzte** Tiefe: der Stack wird beim Anlegen mit
//! [`MUSTER`] gefuellt, und beim Tod des Threads (bzw. am Ende des Laufs) wird von **unten** her
//! gezaehlt, wieviele Worte das Muster noch tragen. Was noch Muster ist, war nie Stack.
//!
//! # Warum das Verfahren fail-closed ist
//!
//! Wenn die Fuellung ausfaellt, steht am Fuss des Stacks kein Muster — [`unberuehrt`] gibt dann
//! `0`, die benutzte Tiefe ist die **volle** Stackgroesse, und das Urteil faellt durch. Ein
//! Wasserzeichen, das „viel Luft" meldet, weil das Muster nie geschrieben wurde, ist damit
//! ausgeschlossen: der Ausfall der Messung sieht aus wie der schlimmste Messwert, nicht wie der
//! beste. Das ist die Umkehrung von „Schweigen als Erfolg", und sie ist Absicht.
//!
//! Der zweite Weg, auf dem eine solche Zeile still gruen wird, ist **gar nicht messen**. Dagegen
//! steht [`Urteil::gemessen`]: ein Hoechststand von `0` bei null Messungen ist von „reichlich
//! Luft" sonst nicht zu unterscheiden.
//!
//! Und der dritte: das Messgeraet selbst ist kaputt. Dagegen steht [`eichung`] — sie fuellt ein
//! Feld, beruehrt es bis zu einer **bekannten** Tiefe und verlangt, dass die Messung genau diese
//! Tiefe zurueckgibt; dazu die beiden Randfaelle (unberuehrt gefuellt -> volle Laenge, gar nicht
//! gefuellt -> `0`). Erst diese drei zusammen unterscheiden ein arbeitendes Messgeraet von einer
//! Funktion, die immer dieselbe Zahl liefert.
//!
//! # Was das Verfahren NICHT sieht
//!
//! * **Einen Stack, der wieder aufgeraeumt wurde.** Schreibt ein tiefer Pfad das Muster zufaellig
//!   an genau die Stelle zurueck, an der er stand, zaehlt sie wieder als unberuehrt. [`MUSTER`] ist
//!   deshalb kein Wert, den Kernelcode je hinlegt (kein Zeiger, keine kleine Zahl, keine `-1`).
//! * **Einen Stack, der zwischen zwei Messungen tiefer war.** Gemessen wird der Endstand je Stack;
//!   ueber die Zeit ist das trotzdem ein Hoechststand, weil das Muster einmal zerstoert bleibt.
//! * **Threads, die nie sterben und nie gefegt werden.** Deshalb fegt der Bericht am Schluss ueber
//!   alle **lebenden** Stacks (`system::kstack_marke_fegen`).

use core::sync::atomic::{AtomicUsize, Ordering};

/// Das Fuellmuster. ASCII `"WASSERST"` — bewusst **kein** Wert, den Kernelcode ablegt: keine
/// kleine Zahl, kein `!0`, keine gueltige Adresse (Bit 63 gesetzt waere Kernelhalbraum, ist es
/// nicht; als Zeiger gelesen liegt der Wert weit ausserhalb jedes abgebildeten GiB).
///
/// Die Wahl ist keine Kosmetik: das Verfahren zaehlt „noch Muster" als „nie benutzt". Ein Muster,
/// das der gemessene Code selbst hinschreiben koennte, meldete Luft, wo keine ist.
pub const MUSTER: u64 = 0x5741_5353_4552_5354;

/// Stackklasse 0: der 16-KiB-EL1-Stack eines **EL0**-Threads (`USER_KSTACK_SIZE`).
pub const KL_EL0: usize = 0;
/// Stackklasse 1: der 64-KiB-Stack eines **Kernel**-Threads (`STACK_SIZE`).
pub const KL_KERN: usize = 1;
/// Anzahl der Klassen.
pub const KLASSEN: usize = 2;

/// Klarname einer Klasse (fuer den Bericht).
pub fn klassenname(k: usize) -> &'static str {
    match k {
        KL_EL0 => "EL0-Kstack",
        KL_KERN => "Kernel-Thread",
        _ => "?",
    }
}

macro_rules! zaehlerfeld {
    ($name:ident, $init:expr) => {
        #[allow(clippy::declare_interior_mutable_const)]
        static $name: [AtomicUsize; KLASSEN] = [const { AtomicUsize::new($init) }; KLASSEN];
    };
}

zaehlerfeld!(GEFUELLT, 0);
zaehlerfeld!(GEMESSEN, 0);
zaehlerfeld!(TIEFE_MAX, 0);
zaehlerfeld!(FREI_MIN, usize::MAX);
zaehlerfeld!(ERSCHOEPFT, 0);
zaehlerfeld!(GROESSE, 0);

/// Hoechststand **sterbender** Threads (gemessen im Reclaim-Pfad) — je Klasse.
zaehlerfeld!(TIEFE_TOD, 0);
/// Hoechststand **lebender** Threads (gemessen beim Schlussfegen) — je Klasse.
zaehlerfeld!(TIEFE_LEBEND, 0);
/// Wieviele Messungen kamen aus dem Reclaim-Pfad (= vor dem Bericht, also vor dem Gatter)?
zaehlerfeld!(GEMESSEN_TOD, 0);
/// Thread-Slot des Rekordhalters (`usize::MAX` = keiner).
zaehlerfeld!(TIEFSTER_SLOT, usize::MAX);

/// **Warum die Herkunft mitgezaehlt wird.** „Hoechststand 832 B" beantwortet nicht, WELCHER Pfad
/// ihn erzeugt hat — und genau das ist die Frage, wenn man die Groesse senken will. Ein
/// sterbender Thread wurde zuletzt im Fault-/Exit-Pfad gemessen (`println!` mit Formatierung,
/// VSpace-Teardown); ein lebender im Zustand seines letzten Syscalls. Die beiden auseinander zu
/// halten kostet zwei Zaehler und macht aus einer Zahl eine Aussage.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Anlass {
    /// Der Thread stirbt (Exit, Kill, EL0-Fault) — gemessen in `reclaim_user_kstack`.
    /// Der Wert ist sein Thread-Slot; ohne ihn waere „Hoechststand 12008 B" eine Zahl ohne
    /// Subjekt, und der tiefste Pfad bliebe unauffindbar.
    Tod(usize),
    /// Der Lauf endet, der Thread lebt — gemessen beim Schlussfegen (Thread-Slot).
    Fegen(usize),
}

/// Ergebnis der [`eichung`] — Bitmaske, `0` = nie gelaufen. S. [`EICH_ALLE`].
static EICHUNG: AtomicUsize = AtomicUsize::new(0);

/// Bit 0: ein **ungefuelltes** Feld meldet `0` unberuehrte Bytes (der fail-closed-Fall).
pub const EICH_LEER: usize = 1;
/// Bit 1: ein **gefuelltes, unberuehrtes** Feld meldet die volle Laenge.
pub const EICH_VOLL: usize = 2;
/// Bit 2: ein bis zu **bekannter** Tiefe beruehrtes Feld meldet genau diese Tiefe.
pub const EICH_TIEFE: usize = 4;
/// Bit 3: die Eichung ist ueberhaupt gelaufen (Sprechprobe der Sprechprobe).
pub const EICH_GELAUFEN: usize = 8;
/// Alle vier Eichbits.
pub const EICH_ALLE: usize = EICH_LEER | EICH_VOLL | EICH_TIEFE | EICH_GELAUFEN;

/// Das Muster in `[base, base+len)` schreiben. Keine Buchfuehrung — die macht [`fuellen`].
///
/// # Safety
/// `base` muss auf `len` Bytes beschreibbaren, 8-Byte-ausgerichteten Speicher zeigen, den sonst
/// niemand gleichzeitig benutzt.
unsafe fn muster_schreiben(base: usize, len: usize) {
    let p = base as *mut u64;
    for i in 0..(len / 8) {
        // SAFETY: `i < len/8`, also innerhalb der zugesicherten Region. `write_volatile`, damit
        // die Schleife nicht wegoptimiert wird: fuer den Compiler ist der Stack eines noch nicht
        // laufenden Threads toter Speicher.
        unsafe { p.add(i).write_volatile(MUSTER) };
    }
}

/// Wieviele Bytes am **Fuss** der Region tragen noch das Muster? (Der Stack waechst nach unten,
/// unberuehrt ist also, was unten liegt.)
///
/// Gibt `0` zurueck, wenn schon das unterste Wort kein Muster traegt — das ist gleichbedeutend
/// mit „aufgebraucht **oder** nie gefuellt", und beide Lagen sollen dasselbe, naemlich das
/// schlechteste Urteil ausloesen.
///
/// # Safety
/// `base` muss auf `len` Bytes lesbaren, 8-Byte-ausgerichteten Speicher zeigen.
pub unsafe fn unberuehrt(base: usize, len: usize) -> usize {
    let p = base as *const u64;
    let worte = len / 8;
    let mut i = 0usize;
    // SAFETY: die Schleife bleibt in `[base, base+worte*8)`; `read_volatile`, weil der Speicher
    // fuer den Compiler uninitialisiert aussieht.
    while i < worte && unsafe { p.add(i).read_volatile() } == MUSTER {
        i += 1;
    }
    i * 8
}

/// Einen frisch belegten Stack mit dem Muster fuellen.
///
/// **MUSS vor dem ersten Gebrauch laufen** — also vor `init_thread_frame`, das den Startframe an
/// den Stack-Top legt. Danach gefuellt, waere der Frame ueberschrieben und der Thread spraenge
/// nach `MUSTER`.
///
/// # Safety
/// wie [`muster_schreiben`].
#[cfg(feature = "selftest")]
pub unsafe fn fuellen(klasse: usize, base: usize, len: usize) {
    if base == 0 || len < 8 || klasse >= KLASSEN {
        return;
    }
    // SAFETY: Zusicherung des Aufrufers, weitergereicht.
    unsafe { muster_schreiben(base, len) };
    GEFUELLT[klasse].fetch_add(1, Ordering::Relaxed);
    GROESSE[klasse].store(len, Ordering::Relaxed);
}

/// Ohne `selftest` kostet die Marke nichts — die Messmaschinerie gehoert nicht in den schlanken
/// Kernel (F1 prueft, dass der sich weiter bauen laesst).
///
/// # Safety
/// trivialerweise erfuellt (No-Op).
#[cfg(not(feature = "selftest"))]
#[inline(always)]
pub unsafe fn fuellen(_klasse: usize, _base: usize, _len: usize) {}

/// Einen Stack messen und in den Hoechststand seiner Klasse einrechnen. Gibt `(benutzt, frei)`.
///
/// # Safety
/// wie [`unberuehrt`]; die Region darf gerade nicht von einem **anderen** Kern beschrieben werden.
/// Vom sterbenden Thread selbst aufgerufen ist das erfuellt (er laeuft mit maskierten IRQs auf
/// eben diesem Stack und liest nur).
#[cfg(feature = "selftest")]
pub unsafe fn messen(klasse: usize, base: usize, len: usize, anlass: Anlass) -> (usize, usize) {
    if base == 0 || len < 8 || klasse >= KLASSEN {
        return (0, 0);
    }
    // SAFETY: Zusicherung des Aufrufers, weitergereicht.
    let frei = unsafe { unberuehrt(base, len) };
    let benutzt = len - frei;
    GEMESSEN[klasse].fetch_add(1, Ordering::Relaxed);
    GROESSE[klasse].store(len, Ordering::Relaxed);
    TIEFE_MAX[klasse].fetch_max(benutzt, Ordering::Relaxed);
    FREI_MIN[klasse].fetch_min(frei, Ordering::Relaxed);
    if frei == 0 {
        ERSCHOEPFT[klasse].fetch_add(1, Ordering::Relaxed);
    }
    // Der Rekordhalter wird mit seinem Slot festgehalten. Das Rennen dabei ist bekannt und
    // hingenommen: zwei Kerne, die gleichzeitig einen neuen Hoechststand melden, koennen Tiefe und
    // Slot verschraenken. Fuer eine DIAGNOSE reicht das (die Tiefe selbst ist ueber `fetch_max`
    // korrekt); ein Lock dafuer stuende im Sterbepfad jedes Threads.
    match anlass {
        Anlass::Tod(slot) => {
            if TIEFE_TOD[klasse].fetch_max(benutzt, Ordering::Relaxed) < benutzt {
                TIEFSTER_SLOT[klasse].store(slot, Ordering::Relaxed);
            }
            GEMESSEN_TOD[klasse].fetch_add(1, Ordering::Relaxed);
        }
        Anlass::Fegen(slot) => {
            if TIEFE_LEBEND[klasse].fetch_max(benutzt, Ordering::Relaxed) < benutzt
                && benutzt > TIEFE_TOD[klasse].load(Ordering::Relaxed)
            {
                TIEFSTER_SLOT[klasse].store(slot, Ordering::Relaxed);
            }
        }
    }
    (benutzt, frei)
}

/// Ohne `selftest`: No-Op.
///
/// # Safety
/// trivialerweise erfuellt (No-Op).
#[cfg(not(feature = "selftest"))]
#[inline(always)]
pub unsafe fn messen(_k: usize, _b: usize, _l: usize, _a: Anlass) -> (usize, usize) {
    (0, 0)
}

/// Wie [`messen`], aber **nur**, wenn die Region ueberhaupt eine von uns gefuellte ist (das
/// unterste Wort traegt das Muster). Fuer den Reap-Pfad, an dem Kernel-Thread-Stacks und
/// EL0-**User**-Stacks in derselben Liste liegen und dieselbe Groesse haben.
///
/// Gibt `true`, wenn gemessen wurde.
///
/// **Die Grenze dieses Erkenners gehoert dazu:** ein Kernel-Stack, der *restlos* aufgebraucht
/// waere, traegt am Fuss kein Muster mehr und wird hier NICHT erkannt — der schlimmste Fall ist
/// also unsichtbar. Er ist es allerdings nur hier: 64 KiB restlos zu verbrauchen hiesse, in den
/// darunterliegenden Speicher geschrieben zu haben, und das faellt lange vorher auf. Fuer die
/// EL0-Klasse ([`messen`] direkt am Kstack) gilt die Einschraenkung **nicht**.
#[cfg(feature = "selftest")]
pub unsafe fn messen_wenn_gefuellt(klasse: usize, base: usize, len: usize) -> bool {
    if base == 0 || len < 8 {
        return false;
    }
    // SAFETY: Zusicherung des Aufrufers.
    if unsafe { (base as *const u64).read_volatile() } != MUSTER {
        return false;
    }
    // SAFETY: dito.
    unsafe { messen(klasse, base, len, Anlass::Tod(usize::MAX)) };
    true
}

/// Ohne `selftest`: No-Op.
///
/// # Safety
/// trivialerweise erfuellt (No-Op).
#[cfg(not(feature = "selftest"))]
#[inline(always)]
pub unsafe fn messen_wenn_gefuellt(_klasse: usize, _base: usize, _len: usize) -> bool {
    false
}

// ------------------------------------------------------------------------------------------
// DIE EICHUNG — die Sprechprobe des MESSGERAETS, nicht des gemessenen Gegenstands
// ------------------------------------------------------------------------------------------
//
// Ohne sie waere „viel Luft" auch die Antwort eines Messgeraets, das immer die Stackgroesse
// zurueckgibt. Gefragt wird deshalb an einem Feld, dessen Tiefe **vorher feststeht**.

/// Woerter des Eichfeldes (2 KiB BSS).
const EICH_WORTE: usize = 256;
/// Die bekannte Tiefe, auf die die Eichung das Feld beruehrt (in Woertern).
const EICH_TIEFE_WORTE: usize = 37; // krumm mit Absicht: ein Fehler um eine Zweierpotenz faellt auf

#[repr(align(64))]
struct Eichfeld(core::cell::UnsafeCell<[u64; EICH_WORTE]>);
// SAFETY: das Feld wird ausschliesslich in `eichung()` benutzt, und die laeuft genau einmal
// (Latch ueber `EICHUNG`), bevor Sekundaerkerne Selbsttests fahren.
unsafe impl Sync for Eichfeld {}
static EICHFELD: Eichfeld = Eichfeld(core::cell::UnsafeCell::new([0; EICH_WORTE]));

/// Das Messgeraet an einer **bekannten** Tiefe pruefen. Einmal beim Hochlauf zu rufen; gibt die
/// Bitmaske (s. [`EICH_ALLE`]) zurueck.
#[cfg(feature = "selftest")]
pub fn eichung() -> usize {
    let base = EICHFELD.0.get() as usize;
    let len = EICH_WORTE * 8;
    let mut bits = EICH_GELAUFEN;

    // (a) **Ungefuellt** (BSS ist genullt): das unterste Wort ist kein Muster -> 0 unberuehrt.
    //     Das ist der Fall „die Fuellung ist ausgefallen", und er MUSS den schlechtesten Messwert
    //     ergeben, nicht den besten.
    // SAFETY: eigenes, exklusiv benutztes Feld.
    if unsafe { unberuehrt(base, len) } == 0 {
        bits |= EICH_LEER;
    }

    // (b) **Gefuellt, unberuehrt**: volle Laenge.
    // SAFETY: dito.
    unsafe { muster_schreiben(base, len) };
    // SAFETY: dito.
    if unsafe { unberuehrt(base, len) } == len {
        bits |= EICH_VOLL;
    }

    // (c) Bis zu einer **bekannten** Tiefe beruehrt: genau diese Tiefe. Beruehrt wird von oben,
    //     wie ein Stack: das oberste beruehrte Wort liegt bei `EICH_WORTE - EICH_TIEFE_WORTE`.
    let erstes = EICH_WORTE - EICH_TIEFE_WORTE;
    // SAFETY: `erstes < EICH_WORTE`; exklusives Feld.
    unsafe { (base as *mut u64).add(erstes).write_volatile(!MUSTER) };
    // SAFETY: dito.
    if unsafe { unberuehrt(base, len) } == erstes * 8 {
        bits |= EICH_TIEFE;
    }

    // Feld wieder nullen — es soll nicht als „gefuellter Stack" herumliegen.
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

/// Der Messstand einer Klasse.
#[derive(Clone, Copy)]
pub struct Marke {
    /// Wieviele Stacks dieser Klasse wurden gefuellt.
    pub gefuellt: usize,
    /// Wieviele wurden gemessen.
    pub gemessen: usize,
    /// Groesste je beobachtete **benutzte** Tiefe in Bytes (der Hoechststand ueber ALLE Stacks
    /// und den ganzen Lauf — nicht der Mittelwert).
    pub tiefe_max: usize,
    /// Kleinste je beobachtete **freie** Reserve in Bytes (`usize::MAX` = nie gemessen).
    pub frei_min: usize,
    /// Wieviele Stacks trugen am Fuss KEIN Muster (aufgebraucht **oder** nie gefuellt).
    pub erschoepft: usize,
    /// Die Stackgroesse dieser Klasse in Bytes (aus der Messung, nicht aus einer Konstante).
    pub groesse: usize,
    /// Hoechststand unter den **sterbenden** Threads (Fault-/Exit-Pfad).
    pub tiefe_tod: usize,
    /// Hoechststand unter den **lebenden** Threads (Schlussfegen).
    pub tiefe_lebend: usize,
    /// Wieviele Messungen fielen vor dem Bericht an — also **vor** dem Gatter.
    pub gemessen_tod: usize,
    /// Thread-Slot des Rekordhalters (`usize::MAX` = keiner).
    pub tiefster_slot: usize,
}

/// Den Messstand einer Klasse lesen.
pub fn marke(klasse: usize) -> Marke {
    let k = klasse.min(KLASSEN - 1);
    Marke {
        gefuellt: GEFUELLT[k].load(Ordering::Relaxed),
        gemessen: GEMESSEN[k].load(Ordering::Relaxed),
        tiefe_max: TIEFE_MAX[k].load(Ordering::Relaxed),
        frei_min: FREI_MIN[k].load(Ordering::Relaxed),
        erschoepft: ERSCHOEPFT[k].load(Ordering::Relaxed),
        groesse: GROESSE[k].load(Ordering::Relaxed),
        tiefe_tod: TIEFE_TOD[k].load(Ordering::Relaxed),
        tiefe_lebend: TIEFE_LEBEND[k].load(Ordering::Relaxed),
        gemessen_tod: GEMESSEN_TOD[k].load(Ordering::Relaxed),
        tiefster_slot: TIEFSTER_SLOT[k].load(Ordering::Relaxed),
    }
}

// ------------------------------------------------------------------------------------------
// DAS URTEIL — an EINER Stelle, damit `all_done` und der Bericht dieselbe Wirklichkeit lesen
// ------------------------------------------------------------------------------------------
//
// Dieselbe Bauform wie `vorrat_urteil` (bringup.rs): eine **reine Funktion ueber die laufenden
// Zaehler**. Sie darf gepollt werden, weil sie nichts misst — die Messung passiert an den
// Ereignissen (Fuellen beim Spawn, Messen beim Tod, Fegen am Schluss).
//
// **Die Schwelle ist gemessen, nicht gesetzt — und die erste Fassung war falsch.** Gemessen am
// 2026-08-10 auf diesem Zweig:
//
// | Suite | Hoechststand EL0-Kstack | Reserve |
// |---|---|---|
// | `test-qemu-x86.sh` (ohne Archiv) | **832 von 16384 B (5,0 %)** | 15552 B |
// | `test-qemu-x86-load.sh` (mit Archiv) | **12008 von 16384 B (73,2 %)** | **4376 B** |
//
// Die erste Fassung stand bei „hoechstens ein Viertel". Sie war aus der HAUPTSUITE hergeleitet
// (5 %) und in der LADE-SUITE unerfuellbar — dieselbe Form wie „eine Klassifikation, die nur gegen
// die Hauptsuite geprueft ist, prueft die Haelfte". Die Lade-Suite laedt Programme, und **`SYS_LOAD`
// verifiziert im Kernel eine Ed25519-Signatur und einen SHA-2-Hash auf dem Kernel-Stack des
// AUFRUFERS**. Die tiefsten Rahmen des Abbilds liegen genau dort
// (`vartime_double_scalar_mul_basepoint` 3528 B, `NafLookupTable::from` 1928 B,
// `ed25519 verify` 1384 B, dazu `load_into_pd_mit` 1416 B und `load_by_index` 888 B).
//
// Formuliert wird deshalb die **Reserve** und nicht der Verbrauch: die Reserve ist die Groesse,
// um die es sicherheitstechnisch geht, und sie bleibt richtig, wenn jemand `USER_KSTACK_SIZE`
// aendert. `1/8` von 16 KiB sind 2048 B gegen gemessene 4376 B — Faktor 2,1. Knapper waere die
// Zeile flatterhaft, weiter waere sie stumm.

/// Mindestens frei zu bleibende Reserve am **Fuss** des Stacks — als Anteil `1/N`.
pub const MIND_RESERVE_NENNER: usize = 8;

/// Wie viele EL0-Kstacks mindestens gemessen sein muessen, damit die Zeile ueberhaupt spricht.
///
/// **Diese Zahl hat schon einmal die Suite gerissen, und das gehoert hierher.** Die erste Fassung
/// stand bei 8 — und war UNERREICHBAR, genau die Form, die dieses Projekt bei der FP-Sonde bezahlt
/// hat. Der Grund: das Gatter (`all_done`) sieht nur die Messungen aus dem Reclaim-Pfad; die
/// Messungen der noch LEBENDEN Stacks entstehen erst im Bericht, also nach dem Gatter. In der
/// Hauptsuite sterben **4** EL0-Threads (gemessen: 9 Stacks insgesamt, 5 davon am Schluss gefegt).
/// Die Suite lief in den Watchdog mit `offen waren: kstack`.
///
/// Die Zahl gehoert deshalb an das, was sie leisten soll: **eine Sprechprobe**, nicht ein Mass fuer
/// die Beweislast. Sie faengt „es wurde ueberhaupt nicht gemessen" und schreibt nicht die Zahl der
/// Sonden fest — die Menge der Belege steht in der Berichtszeile, wo man sie lesen kann.
pub const MIND_MESSUNGEN: usize = 2;

/// Das Urteil der `kstack`-Zeile. Drei Konjunkte, jedes einzeln falsifizierbar:
///
/// 1. **Die Eichung traegt.** Sonst misst hier eine Funktion, die immer dasselbe sagt.
/// 2. **Es wurde ueberhaupt gemessen** (Sprechprobe am gepruefte Pfad, nicht an einer Ausnahme
///    darin). Ohne sie bestuende die Zeile auch ein System, in dem nie ein EL0-Thread lief.
/// 3. **Am Fuss jedes Stacks blieb die Mindestreserve stehen.** Das ist die Aussage; faellt die
///    Fuellung aus, ist die gemessene Reserve `0` und das Konjunkt faellt.
///
/// Gezaehlt wird bei (2) `gemessen_tod`, also **nur, was das Gatter ueberhaupt sehen kann** —
/// die Messungen des Schlussfegens entstehen erst im Bericht. `gemessen` zu nehmen hiesse, eine
/// Bedingung zu stellen, die zum Zeitpunkt ihrer Pruefung strukturell nicht erfuellbar ist.
pub fn urteil() -> bool {
    let m = marke(KL_EL0);
    let (irq_max, irq_n) = caprock_hal::exception::irq_tiefe();
    eichstand() == EICH_ALLE
        && m.gemessen_tod >= MIND_MESSUNGEN
        && m.groesse > 0
        && m.frei_min >= mindestreserve(m.groesse)
        // **DIE SUMMENBEDINGUNG -- der Unterschied zwischen statistisch und strukturell.**
        //
        // `frei_min >= reserve` sagt: die Hoechststaende, die VORKAMEN, liessen genug uebrig.
        // Der schlimmste Fall ist aber tiefste Aufrufkette PLUS tiefster Interrupt-Handler, der
        // genau am Scheitelpunkt eintrifft -- gewoehnlicher Kernelcode laeuft mit `IF=1`, beide
        // landen auf DEMSELBEN Stack. Ob QEMU diese Koinzidenz je gewuerfelt hat, weiss niemand.
        // Also werden die Summanden GETRENNT gemessen und addiert, statt auf den Wuerfel zu
        // hoffen -- dieselbe Lehre wie beim NMI.
        //
        // `irq_n > 0` ist die Sprechprobe: ohne sie waere ein nie gemessener zweiter Summand
        // eine erfundene Null, und die Summe eine Rechnung mit einer Zahl, die niemand erhoben
        // hat. `#DF`/`NMI`/`#MC` kommen NICHT dazu -- sie laufen auf eigenen IST-Staecken, und
        // genau das haben die gekauft.
        && irq_n > 0
        && (m.groesse as u64).saturating_sub(m.frei_min as u64) + irq_max
            + mindestreserve(m.groesse) as u64
            <= m.groesse as u64
}

/// Die drei Summanden der Stackrechnung: `(tiefster Pfad, tiefster IRQ-Handler, geforderte Reserve)`
/// — und wieviel davon die aktuelle Groesse noch traegt.
pub fn summe() -> (u64, u64, u64, u64, u64) {
    let m = marke(KL_EL0);
    let (irq_max, irq_n) = caprock_hal::exception::irq_tiefe();
    let pfad = (m.groesse as u64).saturating_sub(m.frei_min as u64);
    let res = mindestreserve(m.groesse) as u64;
    (pfad, irq_max, res, m.groesse as u64, irq_n)
}

/// Die geforderte Mindestreserve fuer eine Stackgroesse.
pub fn mindestreserve(groesse: usize) -> usize {
    groesse / MIND_RESERVE_NENNER
}
