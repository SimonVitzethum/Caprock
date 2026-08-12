//! **Die Sperrhaltedauer-Marke** — wie lange war der Kern am Stueck nicht praemptierbar? (C9)
//!
//! # Der Anlass, und warum er eine KLASSE ist
//!
//! Beim Bau des C8-Verifizierers stand eine Zeile im Baum:
//!
//! ```ignore
//! while let Some(a) = SCHLANGE.lock().entnehmen() { .. laden(&a) .. }
//! ```
//!
//! Der Guard eines `while let`-Scrutinees lebt bis zum **Ende des Rumpfes**, und
//! `SpinLock::lock` maskiert die IRQs des eigenen Kerns. Die gesamte Ed25519-/SHA-2-Pruefung
//! lief damit mit gesperrten Interrupts — also genau die Fassung „Praemption fuer die Dauer
//! aus", die der Entwurf ausdruecklich verworfen hatte, hereingeholt durch eine
//! Temporaries-Lebensdauer statt durch eine Entscheidung.
//!
//! **Keine Pruefzeile hat es gesehen.** Die Suite war gruen, jeder Messwert stimmte. Gefunden
//! wurde es mit einem `Drop`-Zeugen von Hand — also durch Glueck. Dieselbe Wurzel hat dieses
//! Projekt schon einmal bezahlt (`match lock() { .. None => lock() }`, ein Selbst-Deadlock im
//! seltenen Zweig), dort mit einem Haenger als Schaden, hier mit einem Latenzloch.
//!
//! Der Unterschied zwischen den beiden Schaeden ist der Grund fuer dieses Modul: **ein Haenger
//! meldet sich, ein Latenzloch nicht.** Sperrhaltedauer war eine unsichtbare Groesse. Sie ist
//! jetzt eine Zahl mit Schwelle.
//!
//! # Die Schwelle, und warum sie hergeleitet ist
//!
//! Der Timer laeuft mit [`TICK_HZ`]`= 100` (10 ms). Wer laenger als **einen Tick** mit
//! maskierten Interrupts laeuft, verliert nachweisbar mindestens einen Tick: der Interrupt
//! kommt, wird nicht zugestellt, und die Umplanung faellt aus. Das ist keine Faustregel und kein
//! Gefuehl, sondern die Latenzzusage eines Mikrokerns in einer Zeile:
//!
//! ```text
//! SCHWELLE = cycles_per_sec() / TICK_HZ        // ein Tick, in den Einheiten der Messung
//! ```
//!
//! Beide Groessen kommen aus **einer** Quelle: `cycles_per_sec()` beschreibt denselben Zaehler,
//! den [`caprock_sync::sperrwacht::zyklen`] liest (x86 TSC, aarch64 `CNTPCT_EL0`), und
//! [`TICK_HZ`] ist der Wert, mit dem `hal::timer::init` den Timer wirklich programmiert hat —
//! nicht eine zweite Zahl daneben (`tickrate_setzen` wird an genau dieser Stelle gerufen).
//!
//! **Die Schwelle ist bewusst grosszuegig**, und das ist eine Aussage und keine Nachlaessigkeit:
//! sie ist die Grenze, ab der ein Verlust **beweisbar** ist. Ein gesundes System liegt um
//! Groessenordnungen darunter, und genau deshalb druckt die Zeile den Hoechststand zusaetzlich
//! als **Promille eines Ticks** — eine Verschlechterung von 0,1 % auf 40 % ist damit lesbar,
//! obwohl das Urteil noch gruen steht.
//!
//! # Warum die Zahl einen ORT hat
//!
//! Ohne Angabe, WO, ist ein Hoechststand ein Alarm ohne Adresse. `SpinLock::lock` traegt
//! deshalb `#[track_caller]`, und die Marke haelt Datei + Zeile des Aufrufers fest. Dieselbe
//! Lehre wie bei „Hoechststand 12008 B" ohne Subjekt: der tiefste Pfad blieb unauffindbar.
//!
//! # Fail-closed — die vier Wege, auf denen diese Zeile still gruen wuerde
//!
//! 1. **Gar nicht gemessen** (Feature aus, Architektur ohne Zaehler). Dagegen
//!    [`caprock_sync::sperrwacht::MESSUNG_VORHANDEN`] und die Sprechprobe [`MIND_MESSUNGEN`];
//!    die Zeile schreibt dann „NICHT GEMESSEN" hin und faellt durch.
//! 2. **Das Messgeraet trennt nicht** (eine Funktion, die immer dieselbe Zahl liefert). Dagegen
//!    die [`eichung`]: zwei Haltungen bekannter, *verschiedener* Dauer an *verschiedenen*
//!    Zeilen, und die Marke muss beiden folgen.
//! 3. **Die Marke wird leergeraeumt.** Dagegen zwei Ratschen: genau ein Hochlauf-Abschluss und
//!    genau eine Eich-Verwerfung, beide von der Eichung, keine dritte.
//! 4. **Der Zeitgeber laeuft rueckwaerts** und `wrapping_sub` macht daraus `2^64` oder `0`.
//!    Dagegen `RUECKWAERTS` in der Wacht: rueckwaerts heisst **verworfen und gezaehlt**, und das
//!    Urteil verlangt `0`.
//!
//! # Reichweite: die Eichung laeuft ueberall, das GATTER nur auf x86
//!
//! [`eichung`] und [`probe`] laufen aus `selftest::run()`, also auf **beiden** Architekturen (auf
//! aarch64 besteht die Eichung ebenfalls vollstaendig). [`bericht`] und der `sperre`-Eintrag in
//! `all_done()` stehen dagegen nur im x86-Hochlaufweg -- dieselbe Einordnung wie `kstackmark`.
//!
//! **Zwei Gruende, beide benennbar** (todo C9d): die Schuldliste unten enthaelt mit
//! `console.rs` einen **x86-Pfad** -- aarch64 hat einen anderen Konsolentreiber, dessen Zahl
//! niemand gemessen hat, und eine Schuld zu uebernehmen, die man nicht gemessen hat, waere ein
//! Freibrief. Und die aarch64-Suite ist auf diesem Zweig **vorbestehend rot** (`color : FAILURES`
//! mit lauter Nullen, 3 von 3 auch auf dem unveraenderten Baum, todo C9e) -- ein Gatter dort waere
//! nicht abnehmbar, und ein nicht abnehmbares Gatter faerbt die Suite, statt sie zu schaerfen.
//!
//! # Die Gegenprobe ist der eigentliche Beleg
//!
//! [`probe`] faehrt die Falle. Mit `--features sperrmark-gegenprobe` steht dort woertlich das
//! `while let Some(x) = PROBE.lock().entnehmen()`, ohne sie die Fassung mit Funktionsgrenze.
//! **Die beiden Zweige unterscheiden sich in NICHTS ausser der Lebensdauer des Guards** — dieselbe
//! Schlange, dieselbe Sperre, dieselbe Arbeit, dieselbe Anzahl Durchlaeufe. Eine Gegenprobe, die
//! zwei Dinge zugleich aendert, misst die Reihenfolge der Pruefungen und nicht die Eigenschaft.

use caprock_hal::println;
use caprock_sync::sperrwacht;
use caprock_sync::SpinLock;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Die Tickrate, mit der der Timer wirklich programmiert wurde (`0` = nie gesetzt).
///
/// **Eine Zahl, eine Quelle.** Gesetzt wird sie unmittelbar neben `hal::timer::init(TICK_HZ)`;
/// eine hier hartkodierte `100` waere ein zweites Gedaechtnis fuer dieselbe Tatsache — genau die
/// Klasse, die `MELDESTELLEN` gekostet hat.
static TICK_HZ: AtomicU64 = AtomicU64::new(0);

/// Die Tickrate hinterlegen. **Neben `hal::timer::init` zu rufen, mit demselben Ausdruck.**
pub fn tickrate_setzen(hz: u64) {
    TICK_HZ.store(hz, Ordering::Release);
}

/// Die Tickrate (`0` = nie gesetzt -> das Urteil faellt durch).
pub fn tickrate() -> u64 {
    TICK_HZ.load(Ordering::Acquire)
}

/// **Die Schwelle: ein Timer-Tick, in Zyklen.** `0` heisst „nicht bestimmbar" (Tickrate nicht
/// gesetzt oder Zeitgeber nicht kalibriert) — und `0` ist keine Schwelle, sondern ein Befund.
///
/// Ein einseitiger Vergleich `x < schwelle` waere gruen, sobald `schwelle` gross und `x` null
/// ist; deshalb steht `schwelle() > 0` als eigenes Konjunkt im [`urteil`], und die Sprechprobe
/// verlangt zusaetzlich gemessene Haltungen.
pub fn schwelle() -> u64 {
    let hz = tickrate();
    if hz == 0 {
        return 0;
    }
    caprock_hal::timer::cycles_per_sec() / hz
}

// ----------------------------------------------------------------------------------------------
// DIE ERKLAERTEN LANGHALTER -- eine Menge von NAMEN mit einem Deckel, und eine Ratsche
// ----------------------------------------------------------------------------------------------
//
// **Der erste Lauf dieser Marke hat DREI echte Befunde geliefert**, und alle drei stehen hier,
// statt stillschweigend ausgenommen zu werden. Keiner war vorher irgendwo eine Zahl.
//
// **Und die Reihenfolge ist der Punkt:** (2) wurde erst sichtbar, als (1) benannt war, und (3)
// erst, als (2) es war. Ein Maximum verdeckt, was kleiner ist -- deshalb hat die Marke einen
// zweiten, BEREINIGTEN Kanal. Die Aufzaehlung terminiert auch: unterhalb der drei liegt der
// naechste Halter bei 270 Promille eines Ticks (`loader.rs:1392`, und das ist ein `println!`
// unter `IFACE_SEEN` -- also (2) durch eine Verschachtelung hindurch). Ab dort ist der Abstand
// zur Schwelle Faktor 3,7, und die Liste kann aufhoeren.
//
// **(1) `kernel/src/colors.rs:982` -- 1,17 Mrd. Zyklen, rund 420 ms, 42 Timer-Ticks.**
// `run_prime_probe` (B-4.5) nimmt dort `PP_ARENA.lock()` und gibt es erst 162 Zeilen spaeter
// wieder frei; dazwischen liegt die **gesamte** Prime+Probe-Messung: `system::alloc` (nimmt die
// MEM-Sperre UNTER der Arena-Sperre), das Suchen farbreiner Laeufe, Tausende von
// Zeigerkettenlaeufen und die Rueckgabe. Genau die Form des C8-Befundes, nur ohne `while let`:
// nicht die Sperre ist zu lang, sondern die ARBEIT liegt darunter.
//
// **(3) `kernel/src/system.rs:9198` -- 20,4 Mio. Zyklen, rund 7,3 ms, 0,73 Ticks.**
// `purge_ipc_for_thread` haelt `IPC_ORPHANS` als AEUSSEREN Lock ueber einen Sweep von
// O(Endpoints + Notifications) = 20 128 Einzelsperrungen -- und zwar **je Thread-Tod**. Die
// ITERATIONSZAHL stand dort schon im Kommentar (C4/D10); was nirgends stand, ist die daraus
// folgende **Latenz**. Dieser Posten ist der unangenehmste der drei, weil er als einziger auf
// einem reinen Produktivpfad liegt und schon heute bei drei Vierteln der Schwelle steht.
//
// **(2) `crates/caprock-hal/src/x86_64/console.rs:87` -- 64 Mio. Zyklen, rund 23 ms, 2,3 Ticks.**
// `_print` haelt `CONSOLE` ueber das gesamte `write_fmt`, und der 16550 wird **pollend** bedient
// (Warten auf THR-empty je Byte). Damit maskiert **jedes `println!`** die Interrupts so lange, wie
// die Zeile zum UART braucht -- und die Berichtszeilen dieses Kernels sind vierstellig lang. Das
// ist die unangenehmere der beiden Zahlen: `colors.rs` ist Pruefcode, `_print` steht im
// Produktivkernel und wird aus dem Panikpfad, dem Hochlauf und jedem Bericht gerufen.
//
// **Warum beide als Schuld stehen und nicht als Behebung.** (1) traegt echte Exklusivitaet (der
// Rueckspeicher ist ein grosser `static`), und den Abschnitt aufzutrennen heisst, eine
// 160-Zeilen-Funktion mit sorgfaeltig begruendeter Semantik umzubauen. (2) ist ein Entwurfspunkt
// mit eigener Abwaegung: eine gepufferte Konsole verliert im Panikfall die letzten Zeilen, und
// genau die braucht man dort. Beides sind eigene Arbeiten mit eigener Gegenprobe, keine
// Nebenwirkung dieser. Als Schuld sind sie **benannt, datiert, gedeckelt und im Bericht sichtbar**;
// als stille Ausnahme waeren sie unauffindbar. Sie stehen als C9a/C9b in `todo.md`.
//
// **Jeder Posten hat seinen EIGENEN Deckel, und jeder ist eine Ratsche.** Ein gemeinsamer Deckel
// waere eine Zahl fuer zwei Tatsachen -- der grosszuegigere deckte den anderen mit ab, und die
// Konsole koennte um das Achtzehnfache wachsen, ohne dass eine Zeile spricht.
//
// **Und die Schuldner verdecken nichts:** die eigentliche Schwelle laeuft gegen den BEREINIGTEN
// Kanal, aus dem beide Dateien ausgenommen sind. Ohne diese Trennung waere die Marke ab heute
// blind fuer alles unter 420 ms -- eine Zeile, die spricht und nichts mehr sieht. Genau so ist
// (2) ueberhaupt erst sichtbar geworden: (1) hatte es verdeckt.

/// Die erklaerten Langhalter. **Eine Menge von Namen mit Gruenden und je eigenem Deckel, keine
/// Zahl** — eine Kardinalzahl griffe gegen Zuwachs, nicht gegen Austausch (`IDENTITY_DEBTS`).
static SCHULDEN: sperrwacht::Schuldliste = sperrwacht::Schuldliste {
    posten: &[
        sperrwacht::Schuldposten {
            datei: "kernel/src/colors.rs",
            // Gemessen 1,006 / 1,166 / 1,174 Mrd. in drei Laeufen; die Streuung stammt aus der
            // Wirtslast (D13). Faktor 1,3 ueber dem schlechtesten -- knapper waere die Zeile
            // flatterhaft, weiter waere sie stumm.
            deckel: 1_500_000_000,
            grund: "B-4.5 run_prime_probe haelt PP_ARENA ueber die GANZE Messung (alloc unter der \
                    Arena-Sperre, Kettenlaeufe, Rueckgabe) -- todo C9a",
        },
        sperrwacht::Schuldposten {
            datei: "kernel/src/system.rs",
            // Gemessen 13,8 / 20,4 / 20,4 / 21,0 Mio. in vier Laeufen (494..748 Promille eines
            // Ticks). Der Deckel liegt bei 24 Mio. = 856 Promille -- also ENGER als die Schwelle
            // selbst. Dieser Posten ist kein Freibrief nach oben, sondern eine Fessel: er haelt
            // die Zahl fest, statt sie bis an den Tick wandern zu lassen.
            deckel: 24_000_000,
            grund: "purge_ipc_for_thread haelt IPC_ORPHANS ueber O(Endpoints+Notifications) = \
                    20 128 Einzelsperrungen JE THREAD-TOD. Als einziger der drei ein \
                    PRODUKTIVpfad. Deckel ENGER als die Schwelle (856 statt 1000 Promille) -- \
                    todo C9c",
        },
        sperrwacht::Schuldposten {
            datei: "crates/caprock-hal/src/x86_64/console.rs",
            // Gemessen 64,2 Mio.; Faktor 1,25. Die Zahl haengt an der LAENGE der laengsten
            // Berichtszeile, nicht an einer Struktur -- wer eine lange Zeile hinzufuegt, hebt sie.
            deckel: 80_000_000,
            grund: "_print haelt CONSOLE ueber das ganze write_fmt, und der 16550 wird pollend \
                    bedient: jedes println! maskiert IRQs fuer die Dauer der Zeile -- todo C9b",
        },
    ],
};

/// Konnte die Schuldliste hinterlegt werden? `false` = mehr Posten als Messplaetze, dann waeren
/// die ueberzaehligen **ausgenommen, ohne geprueft zu werden** — das Urteil faellt darueber durch.
static SCHULDEN_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Die Schuldliste hinterlegen. **Einmal beim Hochlauf, vor SMP** (aus [`eichung`]).
fn schulden_anmelden() {
    SCHULDEN_OK.store(sperrwacht::schulden_setzen(&SCHULDEN), Ordering::Release);
}

/// Die erklaerten Langhalter (fuer den Bericht).
pub fn schuldner() -> &'static [sperrwacht::Schuldposten] {
    SCHULDEN.posten
}

/// Bleibt **jeder** erklaerte Schuldner unter seinem eigenen Deckel?
///
/// Ohne diesen Halbsatz waere die Schuldliste ein Freibrief nach oben: ausgenommen von der
/// Schwelle **und** von jeder anderen Schranke.
pub fn schulden_unter_deckel() -> bool {
    SCHULDEN_OK.load(Ordering::Acquire)
        && SCHULDEN
            .posten
            .iter()
            .enumerate()
            .all(|(i, p)| sperrwacht::schuld_hoechststand(i) <= p.deckel)
}

/// Mindestzahl gemessener Sperrhaltungen, damit die Zeile ueberhaupt spricht.
///
/// Reine **Sprechprobe**, kein Mass fuer die Beweislast: sie faengt „es wurde gar nicht
/// gemessen" und schreibt nicht die Zahl der Sonden fest. Erreichbar mit Abstand — schon der
/// Hochlauf nach der Eichung nimmt Tausende von Sperren; die Zahl liegt bewusst zwei
/// Groessenordnungen darunter, damit sie nicht eines Tages zur unerfuellbaren Bedingung wird
/// (die Falle, die die FP-Sonde gekostet hat).
pub const MIND_MESSUNGEN: u64 = 100;

// ----------------------------------------------------------------------------------------------
// DIE EICHUNG — die Sprechprobe des MESSGERAETS, nicht des gemessenen Gegenstands
// ----------------------------------------------------------------------------------------------

/// Bit 0: die Eichung ist ueberhaupt gelaufen (Sprechprobe der Sprechprobe).
pub const EICH_GELAUFEN: usize = 1;
/// Bit 1: der Zeitgeber laeuft (zwei Stempel um eine Warteschleife herum sind verschieden).
pub const EICH_ZEIT: usize = 2;
/// Bit 2: nach dem Hochlauf-Abschluss ist der Kanal wirklich leer (`max == 0`, `gesehen == 0`).
pub const EICH_LEER: usize = 4;
/// Bit 3: eine Haltung **bekannter** Dauer wird mit mindestens dieser Dauer gemeldet.
pub const EICH_MISST: usize = 8;
/// Bit 4: sie wird auch nicht masslos ueberschaetzt (Groessenordnung stimmt).
pub const EICH_BAND: usize = 16;
/// Bit 5: die gemeldete **Stelle** folgt dem Aufrufer — zwei Haltungen an zwei Zeilen ergeben
/// zwei verschiedene Zeilen, beide in dieser Datei.
pub const EICH_STELLE: usize = 32;
/// Alle sechs Eichbits.
pub const EICH_ALLE: usize = EICH_GELAUFEN | EICH_ZEIT | EICH_LEER | EICH_MISST | EICH_BAND | EICH_STELLE;

/// Ergebnis der [`eichung`] (`0` = nie gelaufen).
static EICHUNG: AtomicUsize = AtomicUsize::new(0);
/// Die bei der Eichung gemessene Dauer der langen Probehaltung (fuer den Bericht).
static EICH_GEMESSEN: AtomicU64 = AtomicU64::new(0);
/// Die dabei geforderte Dauer (fuer den Bericht).
static EICH_GEFORDERT: AtomicU64 = AtomicU64::new(0);

/// Wie klein die Eich-Haltung gegenueber der Schwelle ist. `1/64` Tick sind auf einer 3-GHz-
/// Maschine rund 156 us — lang genug, um jede natuerliche Haltung des Hochlaufs zu ueberragen,
/// und zwei Groessenordnungen unter der Schwelle, damit die Eichung das Urteil nicht faerbt.
const EICH_TEILER: u64 = 64;

/// Obergrenze des Plausibilitaetsbandes: mehr als das 32-Fache der geforderten Dauer heisst,
/// dass Zeitgeber und Schwelle **nicht dieselbe Einheit** haben — der Fehler, den man sonst erst
/// bemerkt, wenn die Zeile grundlos rot oder grundlos gruen ist.
const EICH_BAND_FAKTOR: u64 = 32;

/// Die Sperre der Eichung/Probe. Eigenes Objekt, damit die Eichung keine echte Sperre des
/// Kerns fuer Millisekunden haelt.
static PROBE: SpinLock<Probenschlange> = SpinLock::new(Probenschlange::LEER);

/// Eine winzige Schlange — nur damit `entnehmen()` etwas zu tun hat und die Falle **woertlich**
/// die Form des C8-Befundes bekommt.
struct Probenschlange {
    posten: [Option<u64>; PROBE_POSTEN],
}

/// Wieviele Posten die Probe abarbeitet.
const PROBE_POSTEN: usize = 3;

impl Probenschlange {
    const LEER: Self = Self {
        posten: [None; PROBE_POSTEN],
    };

    fn fuellen(&mut self) {
        for (i, p) in self.posten.iter_mut().enumerate() {
            *p = Some(i as u64);
        }
    }

    fn entnehmen(&mut self) -> Option<u64> {
        self.posten.iter_mut().find(|p| p.is_some()).and_then(|p| p.take())
    }
}

/// Busy-Wait von `dauer` Zyklen — die „Arbeit". Misst mit **derselben** Uhr wie die Marke,
/// sonst waere die geforderte Dauer in einer anderen Einheit als die gemessene.
fn warten(dauer: u64) {
    let t0 = sperrwacht::zyklen();
    // Ohne Zeitgeber (Host/Loom) waere das eine Endlosschleife -> gar nicht erst betreten.
    if !sperrwacht::MESSUNG_VORHANDEN {
        return;
    }
    while sperrwacht::zyklen().wrapping_sub(t0) < dauer {
        core::hint::spin_loop();
    }
}

/// **Das Messgeraet an bekannten Dauern pruefen.** Einmal beim Hochlauf zu rufen, vor SMP.
///
/// Ablauf, und jeder Schritt beantwortet eine Art, auf die diese Zeile still gruen wuerde:
///
/// 1. **Laeuft der Zeitgeber ueberhaupt?** (`EICH_ZEIT`)
/// 2. **Hochlauf abschliessen** — der bis hierher gemessene Hoechststand wandert nach
///    `max_hochlauf` und wird **gerettet, nicht verworfen**; der Live-Kanal faengt sauber an.
///    Danach muss er leer sein (`EICH_LEER`).
/// 3. **Eine kurze Haltung bekannter Dauer** -> mindestens diese Dauer (`EICH_MISST`).
/// 4. **Eine dreimal so lange Haltung an einer ANDEREN Zeile** -> die Zahl waechst mit, bleibt
///    in der Groessenordnung (`EICH_BAND`), und die gemeldete Zeile **wechselt mit**
///    (`EICH_STELLE`). Ohne Schritt 4 bestuende die Eichung auch eine Marke, die eine feste Zahl
///    und eine feste Adresse zurueckgibt.
/// 5. **Die Eich-Haltungen verwerfen** — sie sind ein Artefakt des Messgeraets und duerfen die
///    Berichtszeile nicht auf Dauer als laengsten Halter zieren. Verworfen wird vom MESSENDEN,
///    genau einmal, und die Zahl steht im Bericht.
pub fn eichung() -> usize {
    // Zuerst die Schuldliste, dann messen: eine Liste, die nach dem ersten Hoechststand kommt,
    // wirkte auf ihn nicht mehr.
    schulden_anmelden();
    // **Der Aufschlag zuerst, und VOR jedem Ausstieg** -- nur so liefert auch ein Bau ohne das
    // Feature seine Vergleichszahl, und ohne die gibt es kein A/B. Seine 20 000 winzigen
    // Haltungen (rund 160 Zyklen) fallen in den Hochlauf-Kanal, dessen Hoechststand vom
    // Konsolenschreiber um fuenf Groessenordnungen dominiert wird.
    aufschlag_messen();
    let mut bits = EICH_GELAUFEN;
    let s0 = schwelle();
    if s0 == 0 || !sperrwacht::MESSUNG_VORHANDEN {
        // Ohne Schwelle/Zeitgeber gibt es nichts zu eichen. `bits` bleibt unvollstaendig ->
        // das Urteil faellt durch, und die Zeile sagt warum.
        EICHUNG.store(bits, Ordering::Release);
        return bits;
    }

    // (1) Der Zeitgeber laeuft.
    let t0 = sperrwacht::zyklen();
    warten(s0 / 4096);
    if sperrwacht::zyklen() > t0 {
        bits |= EICH_ZEIT;
    }

    // (2) Hochlauf abschliessen; der Live-Kanal muss danach leer sein.
    sperrwacht::hochlauf_abschliessen();
    let leer = sperrwacht::stand();
    if leer.max == 0 && leer.gesehen == 0 {
        bits |= EICH_LEER;
    }

    // (3) Kurze Haltung bekannter Dauer.
    let kurz = s0 / EICH_TEILER;
    {
        let mut g = PROBE.lock();
        g.fuellen();
        warten(kurz);
    }
    let nach_kurz = sperrwacht::stand();
    if nach_kurz.max >= kurz {
        bits |= EICH_MISST;
    }

    // (4) Dreimal so lange, an einer ANDEREN Zeile.
    let lang = kurz * 3;
    {
        let mut g = PROBE.lock();
        let _ = g.entnehmen();
        warten(lang);
    }
    let nach_lang = sperrwacht::stand();
    if nach_lang.max >= lang && nach_lang.max <= lang.saturating_mul(EICH_BAND_FAKTOR) {
        bits |= EICH_BAND;
    }
    // Die Zeile MUSS gewandert sein: beide Haltungen stehen in dieser Datei, aber an
    // verschiedenen Zeilen. Eine Marke, die eine feste Adresse zurueckgibt, faellt hier durch.
    if nach_kurz.datei == file!()
        && nach_lang.datei == file!()
        && nach_kurz.zeile != 0
        && nach_lang.zeile != 0
        && nach_kurz.zeile != nach_lang.zeile
    {
        bits |= EICH_STELLE;
    }

    EICH_GEFORDERT.store(lang, Ordering::Relaxed);
    // (5) Artefakt der Eichung verwerfen -- die Zahl bleibt im Bericht erhalten.
    EICH_GEMESSEN.store(sperrwacht::eichung_verwerfen(), Ordering::Relaxed);

    // Die Schlange fuer die spaetere Probe wieder leeren.
    {
        let mut g = PROBE.lock();
        while g.entnehmen().is_some() {}
    }

    EICHUNG.store(bits, Ordering::Release);
    bits
}

/// Stand der Eichung (`0` = nie gelaufen).
pub fn eichstand() -> usize {
    EICHUNG.load(Ordering::Acquire)
}

/// Was die Eichung gefordert und gemessen hat (fuer den Bericht).
pub fn eichwerte() -> (u64, u64) {
    (
        EICH_GEFORDERT.load(Ordering::Relaxed),
        EICH_GEMESSEN.load(Ordering::Relaxed),
    )
}

// ----------------------------------------------------------------------------------------------
// DIE PROBE — die Falle selbst, einmal richtig und einmal falsch
// ----------------------------------------------------------------------------------------------

/// Ein Posten „Arbeit": so lang, dass **eine einzige** unter der Sperre gehaltene Runde die
/// Schwelle reisst (1,5 Ticks). Das ist der Punkt: die Gegenprobe soll das Urteil kippen, nicht
/// nur eine groessere Zahl erzeugen.
fn arbeitsdauer() -> u64 {
    schwelle() / 2 * 3
}

/// Den naechsten Posten entnehmen — **und die Sperre dabei sicher wieder loslassen**.
///
/// Die Funktionsgrenze ist hier der Mechanismus, nicht die Formatierung: sie beendet die
/// Lebensdauer des Guards *garantiert* vor der Arbeit. Woertlich dieselbe Abhilfe wie
/// `verifizierer::naechster`.
fn naechster() -> Option<u64> {
    PROBE.lock().entnehmen()
}

/// **Die Probe: dieselbe Schleife, einmal mit und einmal ohne die Falle.**
///
/// Beide Zweige tun *exakt* dasselbe — dieselbe Schlange, dieselbe Sperre, dieselbe Arbeit,
/// dieselbe Anzahl Durchlaeufe. Der **einzige** Unterschied ist, ob der Guard des Scrutinees
/// ueber den Rumpf lebt. Damit isoliert die Gegenprobe genau eine Eigenschaft; eine Mutation,
/// die zwei Dinge zugleich kaputtmacht, beweist nichts ueber die gemeinte.
pub fn probe() {
    if schwelle() == 0 || !sperrwacht::MESSUNG_VORHANDEN {
        return; // ohne Messung ist die Probe eine 45-ms-Warteschleife ohne Aussage
    }
    let dauer = arbeitsdauer();
    PROBE.lock().fuellen();

    #[cfg(not(feature = "sperrmark-gegenprobe"))]
    {
        // RICHTIG: der Guard stirbt an der Funktionsgrenze, die Arbeit laeuft ohne Sperre.
        while let Some(_x) = naechster() {
            warten(dauer);
        }
    }
    #[cfg(feature = "sperrmark-gegenprobe")]
    {
        // FALSCH — und zwar woertlich der C8-Befund: der Guard des Scrutinees lebt bis zum Ende
        // des Rumpfes, die Arbeit laeuft also mit maskierten Interrupts.
        while let Some(_x) = PROBE.lock().entnehmen() {
            warten(dauer);
        }
    }
}

// ----------------------------------------------------------------------------------------------
// DER AUFSCHLAG — was die Messung den heissesten Pfad kostet
// ----------------------------------------------------------------------------------------------
//
// **Warum das gemessen und nicht geschaetzt wird.** `SpinLock::lock` laeuft in jedem Syscall. Eine
// Marke, die dort teuer ist, verfaelscht genau das System, ueber das sie eine Aussage macht --
// und „ein bisschen" ist keine Zahl. Gemessen wird auf einer PRIVATEN, unbestrittenen Sperre, weil
// die Frage „was kostet die Buchfuehrung" ist und nicht „wie lange wartet man auf andere".
//
// Gemessen wird zweimal:
//   * `RUNDEN` mal `lock()`+`drop` -- mit Marke, wenn der Bau sie enthaelt, und ohne, wenn nicht.
//     **Das ist die Groesse des A/B**, und deshalb laeuft diese Messung in BEIDEN Bauten.
//   * `RUNDEN` mal ein Zeitstempelpaar allein -- eine UNTERGRENZE fuer den Aufschlag.
//
// **Ergebnis des A/B** (x86 unter KVM, 20 000 Runden je Seite, unbestrittene Sperre):
//
// | Bau | Zyklen je `lock()`+`drop` |
// |---|---|
// | ohne `caprock-sync/sperrwacht` | **39,3** |
// | mit  `caprock-sync/sperrwacht` | **162,3** |
//
// Also **+123 Zyklen, Faktor 4,1**. Das ist deutlich mehr als das Stempelpaar allein (48,4) --
// dazwischen liegen der `fetch_add` auf einer geteilten Zeile, zwei Wasserstandsvergleiche, der
// um zwei Worte breitere Guard und die verlorene Inlining-Freiheit durch `#[track_caller]`.
// **Die kleinere Zahl waere die bequeme gewesen**; sie ist eine Untergrenze und nicht der
// Aufschlag, und der Unterschied ist Faktor 2,5.
//
// Bei diesem Preis gehoert die Marke nicht in den Vorgabebau. Das ist die im Auftrag ausdruecklich
// zugelassene ehrliche Antwort -- und die Zeile sagt es selbst, statt still nicht zu messen.

/// Runden der Aufschlagsmessung. Gross genug, dass die Aufloesung des Zeitgebers verschwindet,
/// klein genug, dass die Messung selbst keine Sperrhaltung von Belang erzeugt.
const AUFSCHLAG_RUNDEN: u64 = 20_000;

/// Zyklen je `lock()`+`drop` (Zehntel), und Zyklen je Marken-Buchfuehrung (Zehntel).
static AUFSCHLAG: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Die private Sperre der Aufschlagsmessung — unbestritten, damit nur die Buchfuehrung zaehlt.
static AUFSCHLAG_LOCK: SpinLock<u64> = SpinLock::new(0);

/// **Den Aufschlag messen.** Beim Hochlauf, vor SMP (unbestrittene Sperre).
///
/// **Die aeussere Uhr ist bewusst `hal::timer::cycles()` und nicht [`sperrwacht::zyklen`]:** nur
/// so laeuft diese Messung auch in einem Bau **ohne** das Feature, und erst damit gibt es
/// ueberhaupt ein A/B. Eine Aufschlagsmessung, die nur im gemessenen Zustand laeuft, kann den
/// Aufschlag nicht kennen. Ihre Kosten (zwei Aufrufe je Messreihe) verteilen sich auf
/// [`AUFSCHLAG_RUNDEN`] Runden und sind damit belanglos -- anders als im heissen Pfad, wo genau
/// diese serialisierende Fassung zu teuer waere.
pub fn aufschlag_messen() {
    // (a) Die ganze Sperrhaltung, so wie der Kernel sie wirklich faehrt -- mit Marke, wenn der
    //     Bau sie enthaelt, und ohne, wenn nicht. DAS ist die Groesse des A/B.
    let t0 = caprock_hal::timer::cycles();
    for _ in 0..AUFSCHLAG_RUNDEN {
        let mut g = AUFSCHLAG_LOCK.lock();
        *g = g.wrapping_add(1);
    }
    let ganz = caprock_hal::timer::cycles().saturating_sub(t0);

    // (b) Nur die Buchfuehrung: zwei Zeitstempel, ohne Sperre. Das ist genau das, was
    //     `beginn()`/`ende()` dem Pfad hinzufuegen -- ohne Feature ist es folgerichtig ~0.
    let t1 = caprock_hal::timer::cycles();
    let mut senke = 0u64;
    for _ in 0..AUFSCHLAG_RUNDEN {
        let a = sperrwacht::zyklen();
        let b = sperrwacht::zyklen();
        senke = senke.wrapping_add(b.wrapping_sub(a));
    }
    let nur_marke = caprock_hal::timer::cycles().saturating_sub(t1);
    // `senke` muss benutzt werden, sonst faellt die Schleife der Optimierung zum Opfer.
    core::hint::black_box(senke);

    AUFSCHLAG[0].store(ganz * 10 / AUFSCHLAG_RUNDEN, Ordering::Relaxed);
    AUFSCHLAG[1].store(nur_marke * 10 / AUFSCHLAG_RUNDEN, Ordering::Relaxed);
}

/// (Zyklen je Sperrhaltung, Zyklen je Marken-Buchfuehrung) — jeweils in **Zehnteln**.
pub fn aufschlag() -> (u64, u64) {
    (
        AUFSCHLAG[0].load(Ordering::Relaxed),
        AUFSCHLAG[1].load(Ordering::Relaxed),
    )
}

// ----------------------------------------------------------------------------------------------
// DAS URTEIL — an EINER Stelle, damit `all_done` und der Bericht dieselbe Wirklichkeit lesen
// ----------------------------------------------------------------------------------------------

/// Der Hoechststand des **bereinigten** Kanals ueber Hochlauf und Live.
///
/// Beide gehen ins Urteil. Nur den Live-Kanal zu pruefen hiesse, dem Hochlauf eine Freikarte zu
/// geben — und der Hochlauf ist der Abschnitt mit den laengsten Sperren (Nullen, Tabellenbau).
///
/// **Der Hochlauf-Wert ist NICHT bereinigt** (der Schnitt liegt vor der Anmeldung der Liste, s.
/// [`eichung`] — er wird dort als erstes gesetzt, aber der Hochlauf war da schon vorbei). Das ist
/// hier unschaedlich und ausgesprochen: `run_prime_probe` laeuft im Selbsttest, also **nach** dem
/// Hochlauf-Schnitt, und faellt damit in den Live-Kanal.
pub fn hoechststand_bereinigt() -> u64 {
    let s = sperrwacht::stand();
    s.max_bereinigt.max(s.max_hochlauf)
}

/// Der Hoechststand ueber **alles**, einschliesslich der erklaerten Schuldner.
pub fn hoechststand() -> u64 {
    let s = sperrwacht::stand();
    s.max.max(s.max_hochlauf)
}

/// Das Urteil der `sperre`-Zeile. Acht Konjunkte, jedes einzeln falsifizierbar:
///
/// 1. **Die Architektur misst ueberhaupt** — sonst ist `0` kein Messwert, sondern Schweigen.
/// 2. **Die Eichung traegt vollstaendig** — sonst misst hier eine Funktion, die immer dasselbe
///    sagt, oder eine, die die Stelle nicht kennt.
/// 3. **Die Schwelle ist bestimmbar** (`> 0`) — ein einseitiger Vergleich gegen `0` waere gruen,
///    sobald die Kalibrierung ausfaellt (die `NOSEL_TEXT`-Falle).
/// 4. **Genau ein Hochlauf-Abschluss** — Ratsche gegen ein zweites „Marke leerraeumen".
/// 5. **Genau eine Eich-Verwerfung** — dieselbe Ratsche fuer den zweiten Weg.
/// 6. **Der Zeitgeber lief nie rueckwaerts** — sonst ist jede Differenz fragwuerdig.
/// 7. **Es wurde ueberhaupt gemessen** (Sprechprobe).
/// 8. **Die Aussage, und zwar zweigeteilt:** jede NICHT erklaerte Stelle bleibt unter einem Tick,
///    und die erklaerten Schuldner bleiben unter ihrem Deckel. Ohne den zweiten Halbsatz waere die
///    Schuld ein Freibrief nach oben; ohne den ersten verdeckte sie alles darunter.
pub fn urteil() -> bool {
    let s = sperrwacht::stand();
    let schw = schwelle();
    sperrwacht::MESSUNG_VORHANDEN
        && eichstand() == EICH_ALLE
        && schw > 0
        && s.hochlauf_abschluesse == 1
        && s.eich_verwerfungen == 1
        && s.rueckwaerts == 0
        && s.gesehen >= MIND_MESSUNGEN
        && hoechststand_bereinigt() < schw
        && schulden_unter_deckel()
}

/// Promille eines Ticks — damit eine Verschlechterung lesbar ist, **bevor** sie die Schwelle
/// reisst. Eine Zeile, die nur „unter der Schwelle" sagt, ist zwischen 0,1 % und 99 % stumm.
pub fn promille_eines_ticks(zyklen: u64) -> u64 {
    let schw = schwelle();
    if schw == 0 {
        return 0;
    }
    zyklen.saturating_mul(1000) / schw
}

/// **Die Berichtszeile.** Aus beiden Hochlaufwegen zu rufen.
pub fn bericht() {
    let s = sperrwacht::stand();
    let schw = schwelle();
    let (eich_soll, eich_ist) = eichwerte();
    let hoch = hoechststand();

    // **Was die Messung selbst kostet** -- gemessen, nicht geschaetzt, und in BEIDEN Bauten. Eine
    // Marke im heissesten Pfad ohne diese Zahl waere eine Behauptung ueber ihre eigene Unschuld.
    // Steht vor dem Ausstieg unten, damit der Bau OHNE Feature seine Vergleichszahl liefert.
    let (ganz, nur) = aufschlag();
    println!(
        "sperre  : Aufschlag auf dem heissen Pfad: {}.{} Zyklen je lock()+drop (Marke {}), \
         Zeitstempelpaar allein {}.{} Zyklen -- gemessen ueber {} unbestrittene Runden. \
         **Das A/B ueber zwei Bauten ist die belastbare Zahl: 39,3 -> 162,3 Zyklen, also +123 \
         (Faktor 4,1)**; das Stempelpaar ist nur eine UNTERGRENZE, weil im echten Pfad noch der \
         Zaehler, zwei Wasserstandsvergleiche und der breitere Guard dazukommen. Deshalb haengt \
         die Marke an `selftest` und nicht an `default`. Die aeussere Uhr ist hier \
         `hal::timer::cycles()`, damit die Zeile auch OHNE das Feature eine Zahl liefert -- eine \
         Aufschlagsmessung, die nur im gemessenen Zustand laeuft, kann den Aufschlag nicht kennen. \
         Im heissen Pfad steht dagegen `rdtsc` OHNE Merkmalserkennung: `cpuid` waere unter KVM ein \
         bedingungsloser VM-Exit",
        ganz / 10,
        ganz % 10,
        if sperrwacht::MESSUNG_VORHANDEN { "AN" } else { "AUS" },
        nur / 10,
        nur % 10,
        AUFSCHLAG_RUNDEN,
    );

    if !sperrwacht::MESSUNG_VORHANDEN {
        // **Ein Vorgabebau darf nicht wie ein gemessener aussehen.** Ohne `sperrwacht` gibt es
        // keine Zahl -- und `0` waere von „alles kurz" nicht zu unterscheiden.
        println!(
            "sperre  : FAILURES -- NICHT GEMESSEN (Feature `caprock-sync/sperrwacht` nicht \
             gesetzt oder Architektur ohne Zyklenzaehler). Die Marke kostet gemessen 39,3 -> \
             162,3 Zyklen je Sperrhaltung (Faktor 4,1) und haengt deshalb an `selftest`; ein \
             Hoechststand von 0 heisst hier 'nicht gemessen', NICHT 'alles kurz'"
        );
        return;
    }

    let ber = hoechststand_bereinigt();
    println!(
        "sperre  : laengste IRQ-maskierte Sperrhaltung: {} Zyklen @ {}:{} ({} Promille eines \
         Ticks) · Hochlauf (vor der Eichung) {} Zyklen @ {}:{} · gemessene Sperrhaltungen {} \
         (gesamt {}) · rueckwaerts {} (muss 0 sein)",
        s.max,
        if s.datei.is_empty() { "-" } else { s.datei },
        s.zeile,
        promille_eines_ticks(s.max),
        s.max_hochlauf,
        if s.datei_hochlauf.is_empty() { "-" } else { s.datei_hochlauf },
        s.zeile_hochlauf,
        s.gesehen,
        s.gesehen_gesamt,
        s.rueckwaerts,
    );
    // **Die Zahl, gegen die die Schwelle laeuft** -- und die Liste derer, die sie nicht enthaelt.
    // Ohne diese zweite Zeile waere die Marke ab dem ersten erklaerten Langhalter blind fuer alles
    // darunter: ein Maximum verdeckt, was kleiner ist.
    println!(
        "sperre  : ohne die erklaerten Langhalter: {} Zyklen @ {}:{} ({} Promille eines Ticks) -- \
         GEGEN DIESE ZAHL laeuft die Schwelle. Ohne die Trennung waere die Marke blind fuer alles \
         unterhalb des groessten Schuldners",
        s.max_bereinigt,
        if s.datei_bereinigt.is_empty() { "-" } else { s.datei_bereinigt },
        s.zeile_bereinigt,
        promille_eines_ticks(s.max_bereinigt),
    );
    // Jeder Posten mit eigener Zahl und eigenem Deckel. Eine Sammelzeile waere eine Zahl fuer
    // mehrere Tatsachen -- der grosszuegigste Deckel deckte alle anderen mit ab.
    for (i, p) in schuldner().iter().enumerate() {
        let ist = sperrwacht::schuld_hoechststand(i);
        println!(
            "sperre  : erklaerter Langhalter {}/{}: {} Zyklen ({} Promille eines Ticks) gegen \
             Deckel {} -- {} · {} [RATSCHE: darf nur fallen; behoben faellt der Eintrag \
             ersatzlos weg]",
            i + 1,
            schuldner().len(),
            ist,
            promille_eines_ticks(ist),
            p.deckel,
            if ist <= p.deckel { "unter dem Deckel" } else { "UEBER DEM DECKEL" },
            p.grund,
        );
    }
    if !SCHULDEN_OK.load(Ordering::Acquire) {
        println!(
            "sperre  : FAILURES -- die Schuldliste hat mehr Posten ({}) als es Messplaetze gibt \
             ({}); die ueberzaehligen waeren ausgenommen, OHNE geprueft zu werden",
            schuldner().len(),
            sperrwacht::SCHULD_PLAETZE,
        );
    }
    println!(
        "sperre  : {} (C9: Schwelle {} Zyklen = EIN Timer-Tick bei {} Hz -- laenger maskiert \
         heisst nachweisbar verlorene Praemption; bereinigter Hoechststand {} Zyklen = {} \
         Promille davon, ungefiltert {} Zyklen · alle {} erklaerten Schuldner unter ihrem Deckel: \
         {}. Eichung {:#08b}/{:#08b}: \
         gefordert {} gemessen {} Zyklen. Mindestens {} Messungen. Gemessen wird die MASKIERTE \
         Zeit einschliesslich des Wartens aufs Ticket -- nicht gesehen wird Maskierung OHNE \
         Sperre, also `local_irq_disable` von Hand und der Trap-Kontext)",
        if urteil() { "ALL PASS" } else { "FAILURES" },
        schw,
        tickrate(),
        ber,
        promille_eines_ticks(ber),
        hoch,
        schuldner().len(),
        schulden_unter_deckel(),
        eichstand(),
        EICH_ALLE,
        eich_soll,
        eich_ist,
        MIND_MESSUNGEN,
    );
}
