//! **Seitenfarben / Cache-Partitionierung** (todo A1) — arch-neutral.
//!
//! Der Last-Level-Cache ist physisch indiziert: welche Cache-Sets eine Seite belegen kann,
//! entscheidet ihre **Physadresse**. Die Bits des Set-Index, die oberhalb des Seitenoffsets
//! liegen, sind die *Farbe* der Seite. Zwei Seiten verschiedener Farbe können einander im LLC
//! nicht verdrängen — und genau das ist der Hebel gegen den Zeitseitenkanal zwischen zwei PDs
//! (`todo.md` A1): bekommt jede PD einen disjunkten Farbsatz, beobachtet keine mehr die
//! Verdrängungen der anderen.
//!
//! ## Was hier gemessen und was entschieden wird
//!
//! Gemessen wird nur die Geometrie (`hal::cache`). Entschieden wird hier: wie viele Farben es
//! gibt, wie sie auf PDs verteilt werden, und **ob die Partitionierung überhaupt trägt**.
//!
//! Der letzte Punkt ist der wichtige. `hal::cache::page_colors()` liefert `1`, wenn die
//! Plattform keine Cache-Geometrie meldet — „eine Farbe" heißt *keine* Partitionierung.
//! Ein Prüfer, der aus `1` ableitete „alle PDs haben disjunkte Farbsätze", würde eine
//! Abwesenheit als Erfüllung lesen. [`usable`] trennt das deshalb explizit: unter zwei Farben
//! ist die Eigenschaft **nicht vorhanden**, nicht etwa trivial erfüllt.

use sel4lake_hal::{self as hal, println};
use sel4lake_mem::{color_of, ColorMask};

/// Seitengröße, gegen die Farben gerechnet werden.
pub const PAGE: u64 = 4096;

// `color_of` kommt aus `sel4lake-mem` — **dieselbe** Funktion, die auch der Allokator benutzt.
// Eine zweite Fassung hier wäre der klassische Fehler: der Test rechnete die Farbe anders als
// die Zuteilung und bestätigte am Ende nur seine eigene Arithmetik.

/// Anzahl Farben dieser Maschine (Zweierpotenz, mindestens 1).
pub fn count() -> u32 {
    hal::cache::page_colors()
}

/// Trägt die Farbpartitionierung auf dieser Maschine überhaupt?
///
/// **Nur** ab zwei Farben. Unter zwei Farben gibt es nichts zu trennen, und jede Aussage der
/// Form „die Farbsätze zweier PDs sind disjunkt" wäre dann strukturell wahr, ohne geprüft zu
/// haben — genau die leere Beobachtung, die dieses Projekt an anderer Stelle schon einmal
/// teuer bezahlt hat (leere SMMU-Event-Queue).
pub fn usable() -> bool {
    count() >= 2
}

/// In wie viele disjunkte Farbsätze der Farbraum geteilt wird.
///
/// Zweierpotenz und höchstens [`sel4lake_mem::MASK_BITS`]. Der Wert ist eine **Politik**, keine
/// Hardwaregröße: mehr Partitionen heißt bessere Trennung und weniger Cache je PD — und, weil
/// ein zusammenhängender Seitenlauf nur so lange in einem Streifen bleibt wie der Streifen
/// breit ist, auch eine kleinere größtmögliche zusammenhängende Region (s. [`region_bytes`]).
pub const PARTITIONS: u32 = 4;

/// Farbsatz des Streifens mit der Nummer `i` (rundläufig über [`PARTITIONS`]).
///
/// `None`, wenn die Aufteilung nicht aufgeht — dann gibt es keinen disjunkten Satz, und der
/// Aufrufer darf **nicht** ersatzweise „alle Farben" nehmen: das wäre die Zusicherung ohne
/// die Eigenschaft.
///
/// **Diese Funktion führt KEINE Belegung** — `i % PARTITIONS` heißt: die fünfte Nummer bekommt
/// wieder den Satz der ersten. Für einen Selbsttest, der zwei feste Nummern vergleicht, ist das
/// richtig; für die Zuteilung an PDs ist es die stille Farbüberschneidung aus B-4.2. Wer eine PD
/// bedient, nimmt [`claim_stripe`].
pub fn mask_for(i: u32) -> Option<ColorMask> {
    sel4lake_mem::stripe(i % PARTITIONS, PARTITIONS)
}

// --- Streifenvergabe mit Belegung (B-4.2) ---------------------------------------------------
//
// Bis hierher wurden Farbsätze rundläufig vergeben. Solange der gefärbte Pfad die Ausnahme war
// (nur der Selbsttest mit zwei festen Nummern), fiel das nicht auf. Als Normalfall — und dorthin
// soll er, B-4.1 — bedeutet es: ab der fünften gleichzeitigen PD teilen sich zwei PDs ihre
// Farben, **ohne dass es jemand merkt**. Das wäre schlimmer als der heutige Zustand: heute ist
// der reguläre Pfad ungefärbt und verspricht nichts; dann wäre er gefärbt und bräche das
// Versprechen still.
//
// Deshalb eine geführte Belegung, und der Fehlschlag ist **sauber**: ist kein Streifen frei,
// gibt es `None` und die PD entsteht gar nicht erst. Kein Ersatzsatz, keine Aufweichung, keine
// „alle Farben"-Rückfallebene — eine Zusicherung, die unter Last leise schwächer wird, ist
// keine.

/// Bit `i` gesetzt = Streifen `i` ist vergeben. Höchstens [`PARTITIONS`] Bits in Gebrauch.
static STRIPES_TAKEN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

// Die Auswahl selbst (`sel4lake_mem::pick_free`) liegt bei `stripe`, nicht hier — aus demselben
// Grund wie `color_of` oben: eine zweite Fassung derselben Arithmetik im Kernel würde am Ende nur
// sich selbst bestätigen. Dort ist sie ohne Hardware host-getestet (Technik wie `cache_decode`).

/// Einen freien Farbstreifen belegen. Gibt `(Streifennummer, Farbsatz)`.
///
/// `None` heißt **kein freier Streifen** (oder die Aufteilung geht nicht auf) — und dann darf der
/// Aufrufer die PD nicht ungefärbt anlegen und so tun, als sei sie getrennt. Freigabe über
/// [`release_stripe`], gebunden an die Lebensdauer der VSpace (`vspace_teardown`).
pub fn claim_stripe() -> Option<(u32, ColorMask)> {
    use core::sync::atomic::Ordering;
    loop {
        let cur = STRIPES_TAKEN.load(Ordering::Acquire);
        let i = sel4lake_mem::pick_free(cur, PARTITIONS)?;
        // Der Farbsatz muss VOR dem Belegen feststehen: geht die Aufteilung nicht auf, wäre ein
        // belegter Streifen ohne Maske ein Leck, das niemand je freigibt.
        let mask = sel4lake_mem::stripe(i, PARTITIONS)?;
        if STRIPES_TAKEN
            .compare_exchange_weak(cur, cur | (1u32 << i), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some((i, mask));
        }
        // Verloren: ein anderer Kern war schneller. Neu lesen, nicht blind weiterzählen.
    }
}

/// Einen Streifen wieder freigeben. Doppelte Freigabe ist wirkungslos, nicht schädlich.
pub fn release_stripe(i: u32) {
    if i < PARTITIONS {
        STRIPES_TAKEN.fetch_and(!(1u32 << i), core::sync::atomic::Ordering::AcqRel);
    }
}

/// Wie viele Streifen sind gerade vergeben? (Telemetrie/Selbsttest.)
pub fn stripes_in_use() -> u32 {
    STRIPES_TAKEN
        .load(core::sync::atomic::Ordering::Acquire)
        .count_ones()
}

/// **B-4.2 auf der laufenden Maschine belegen:** die Streifen erschöpfen und prüfen, dass der
/// Versuch danach *scheitert*, statt einen Satz ein zweites Mal auszugeben.
///
/// Die Arithmetik dahinter ist host-getestet (`sel4lake_mem::pick_free`); hier geht es um die
/// Zustandsführung im Kernel — dass die Belegung wirklich atomar geführt und die Freigabe wirklich
/// wirksam ist. Der Test stellt den Ausgangszustand danach wieder her; er läuft vor dem Anlegen
/// gefärbter PDs, belegt also nichts, was jemandem gehört.
#[cfg(feature = "selftest")]
pub fn run_stripe_alloc() -> bool {
    let vorher = stripes_in_use();
    if vorher != 0 {
        // Nicht „durchgefallen", sondern nicht durchführbar: es hält schon jemand Streifen.
        println!("stripe  : SKIP ({vorher} Streifen bereits vergeben -- Test braucht den Ruhezustand)");
        return true;
    }
    let mut held: [Option<u32>; PARTITIONS as usize] = [None; PARTITIONS as usize];
    let mut n = 0usize;
    while n < PARTITIONS as usize {
        match claim_stripe() {
            Some((i, m)) => {
                // Jeder Satz muss nichtleer sein und darf keinen früheren überlappen.
                for prev in held.iter().take(n).flatten() {
                    if let Some(pm) = sel4lake_mem::stripe(*prev, PARTITIONS) {
                        if pm.0 & m.0 != 0 {
                            println!("stripe  : FAILURES (Streifen {i} ueberlappt {prev})");
                            return false;
                        }
                    }
                }
                held[n] = Some(i);
                n += 1;
            }
            None => {
                println!("stripe  : FAILURES (nur {n} von {PARTITIONS} Streifen vergebbar)");
                for h in held.iter().flatten() {
                    release_stripe(*h);
                }
                return false;
            }
        }
    }
    // **Der eigentliche Punkt:** jetzt ist alles vergeben, und der nächste Versuch muss scheitern.
    let erschoepft = claim_stripe().is_none();
    if !erschoepft {
        println!("stripe  : FAILURES (der {}. Versuch bekam einen Satz -- stille Ueberschneidung)", PARTITIONS + 1);
    }
    for h in held.iter().flatten() {
        release_stripe(*h);
    }
    // Und nach der Freigabe muss wieder etwas gehen, sonst leckt die Belegung.
    let wieder_frei = stripes_in_use() == 0 && claim_stripe().is_some();
    if wieder_frei {
        release_stripe(0);
    }
    let ok = erschoepft && wieder_frei && stripes_in_use() == 0;
    println!(
        "stripe  : {} Streifen vergeben, {}. Versuch abgewiesen: {}; nach Freigabe wieder vergebbar: {}",
        PARTITIONS,
        PARTITIONS + 1,
        erschoepft,
        wieder_frei
    );
    println!(
        "stripe  : {} (B-4.2: erschoepfte Farbpartitionierung scheitert SAUBER, statt einen Satz still ein zweites Mal auszugeben)",
        if ok { "ALL PASS" } else { "FAILURES" }
    );
    ok
}

/// Größte **zusammenhängende** Region, die vollständig in einen Streifen passt.
///
/// Aufeinanderfolgende Seiten tragen aufeinanderfolgende Farben; ein Lauf verlässt den Streifen
/// nach `MASK_BITS / PARTITIONS` Seiten. **Das ist die harte Grenze des Verfahrens** und der
/// Grund, warum die 2-MiB-Region von [`crate::system::spawn_isolated`] nicht gefärbt werden
/// kann: 512 Seiten überstreichen jeden Streifen mehrfach, also alle Farben.
pub const fn region_bytes() -> u64 {
    (sel4lake_mem::MASK_BITS / PARTITIONS) as u64 * PAGE
}

/// Ergebnis des Farb-Selbsttests.
#[cfg(feature = "selftest")]
#[derive(Default)]
pub struct ColorTest {
    pub ok: bool,
    /// Gemessene Farbanzahl der Maschine.
    pub colors: u32,
    /// **Trägt die Eigenschaft überhaupt?** Unter zwei Farben gibt es nichts zu trennen; alles
    /// Weitere wäre dann strukturell wahr, ohne geprüft zu sein. Ist das `false`, ist der Test
    /// *nicht durchgefallen*, sondern **nicht durchführbar** — und muss so gemeldet werden.
    pub usable: bool,
    /// Beide PDs bekamen überhaupt eine Region.
    pub spawned: bool,
    /// Jede Seite jeder PD-Region liegt im Farbsatz ihrer PD.
    pub in_mask: bool,
    /// Auch Kernel-Stack und die obersten Seitentabellen der PD liegen in ihrem Farbsatz.
    /// Getrennt gefuehrt, weil es eine eigene Zusicherung ist: der Kernel arbeitet dort **im
    /// Namen** des Subjekts, und ein Seitenlauf der MMU hinterlaesst dieselben Spuren.
    pub kernel_side_in_mask: bool,
    /// Aufschlüsselung von [`Self::kernel_side_in_mask`]: Kernel-Stack, oberste und zweitoberste
    /// Tabelle — jeweils über **beide** PDs. Ein einzelnes `false` sagt, *welche* der drei
    /// Zusicherungen gebrochen ist; ohne das ist der Sammelwert nur ein Alarm ohne Adresse.
    pub ks_in_mask: bool,
    pub l1_in_mask: bool,
    pub l2_in_mask: bool,
    /// **Konnte die Kernel-Seite überhaupt zurückgelesen werden?** Getrennt von
    /// [`Self::kernel_side_in_mask`], weil beides ganz verschiedene Befunde sind: eine gebrochene
    /// Färbung ist ein Isolationsfehler, eine `0` aus dem Rückkanal ein Fehler des Tests. Vorher
    /// fielen beide in dasselbe Bit (`l1 != 0 && in_mask(l1)`) — ein `kernelseite=0` liess sich
    /// dann nicht deuten, ohne zu raten. Dieselbe Trennung wie bei `audit_cdt` (Code 8:
    /// „konnte nicht laufen" statt still „konsistent").
    pub kernel_side_readable: bool,
    pub ks_read: bool,
    pub l1_read: bool,
    pub l2_read: bool,
    /// **Die A1-Eigenschaft**: die Farbmengen beider PDs sind disjunkt.
    pub disjoint: bool,
    /// Eine Region jenseits der Streifenbreite wird abgewiesen statt fremde Farben mitzunehmen.
    pub oversize_refused: bool,
    /// Nach dem Abbau steht die Speicherbilanz wieder.
    pub balanced: bool,
}

/// Belegt eine Region ausschließlich Farben aus `mask`?
#[cfg(feature = "selftest")]
fn region_in_mask(base: u64, len: u64, colors: u32, mask: ColorMask) -> bool {
    let mut p = base;
    while p < base + len {
        if !mask.contains(color_of(p, colors)) {
            return false;
        }
        p += PAGE;
    }
    true
}

/// Überschneiden sich die tatsächlich belegten Farbmengen zweier Regionen?
///
/// Bewusst über die **belegten** Farben und nicht über die Masken: dass zwei Masken disjunkt
/// sind, hat der Host-Test der Crate bereits gezeigt. Hier ist die Frage, ob der Allokator sich
/// auch daran gehalten hat.
#[cfg(feature = "selftest")]
fn regions_share_color(a: (u64, u64), b: (u64, u64), colors: u32) -> bool {
    let mut p = a.0;
    while p < a.0 + a.1 {
        let ca = color_of(p, colors);
        let mut q = b.0;
        while q < b.0 + b.1 {
            if ca == color_of(q, colors) {
                return true;
            }
            q += PAGE;
        }
        p += PAGE;
    }
    false
}

/// **Farb-Selbsttest** (todo A1): zwei isolierte PDs mit disjunkten Farbsätzen erzeugen und
/// nachweisen, dass sie sich keine Cache-Farbe teilen.
///
/// `entry` ist der Einsprungpunkt für die beiden Test-PDs (arch-spezifisch, kommt vom
/// Hochlaufweg). Die PDs werden unmittelbar wieder abgebaut; geprüft wird die Zuteilung, nicht
/// ihr Programm.
#[cfg(feature = "selftest")]
pub fn run_color(entry: usize, prio: u8) -> ColorTest {
    let mut r = ColorTest { colors: count(), ..Default::default() };
    r.usable = usable();
    if !r.usable {
        return r; // nicht durchgefallen — nicht durchfuehrbar. Siehe Feld-Doku.
    }
    let (Some(m0), Some(m1)) = (mask_for(0), mask_for(1)) else {
        return r;
    };

    let sz = region_bytes();

    // Eine Region jenseits der Streifenbreite MUSS abgewiesen werden. Zuerst, damit ein
    // Fehlschlag hier nicht von den beiden PDs verdeckt wird.
    r.oversize_refused = crate::system::alloc_colored(sz * 2, PAGE, m0).is_none();

    let a = crate::system::spawn_isolated_colored(entry, 0, prio, m0);
    let b = crate::system::spawn_isolated_colored(entry, 1, prio, m1);
    if let (Some((ta, ba, ksa)), Some((tb, bb, ksb))) = (a, b) {
        r.spawned = true;

        // Die Kernelseite wird **nicht mehr zurueckgelesen**, sondern vom Spawn geliefert.
        //
        // Warum das noetig war: Der Einsprungpunkt dieser PDs faultet ABSICHTLICH. Wird der
        // Thread eingesammelt, geben `kstack_of()`/`vspace_tables_of()` 0 zurueck -- alle drei
        // zugleich, weil sie an derselben Belegung haengen. Genau das zeigten am 2026-08-01
        // mehrere Laeufe: `rueckgelesen=0 (kstack=0 l1=0 l2=0)` bei sonst fehlerfreier Zuteilung,
        // und im Protokoll steht die Ursache woertlich in der Zeile davor:
        // `el0-trap: User-Thread 0x8 faultete ... -> beendet, Kernel laeuft weiter`.
        //
        // Zwei Anlaeufe halfen NICHT, und beide sind lehrreich:
        //  * frueher lesen (4/400 -> 3/500): das Fenster beginnt, sobald `spawn` zurueckkehrt --
        //    frueher als "sofort danach" geht nicht.
        //  * die Buchfuehrung im Kernel unter den SCHEDS-Lock ziehen (-> 5/500): das war ein
        //    ECHTER Fehler (ein Kernel-Stack-Leck, s. `system::record_user_kstack`) und ist
        //    behoben -- aber es war nicht DIESER.
        //
        // Ein Wert, der von der Lebendigkeit eines Threads abhaengt, der sterben darf, ist als
        // Messgroesse untauglich, egal wie schnell man liest. Deshalb kommt er jetzt von dort,
        // wo er stabil ist: aus dem Spawn selbst, vor der Existenz des Threads.
        let kernelseite: [(u64, u64, u64); 2] = [ksa, ksb];

        r.in_mask = region_in_mask(ba, sz, r.colors, m0) && region_in_mask(bb, sz, r.colors, m1);
        r.disjoint = !regions_share_color((ba, sz), (bb, sz), r.colors);
        // Kernel-Seite: Stack (16 KiB) und die beiden obersten Tabellen (je 4 KiB) jeder PD.
        r.ks_in_mask = true;
        r.l1_in_mask = true;
        r.l2_in_mask = true;
        r.ks_read = true;
        r.l1_read = true;
        r.l2_read = true;
        for (i, &(_t, m)) in [(ta, m0), (tb, m1)].iter().enumerate() {
            let (ks, l1, l2) = kernelseite[i];
            // Lesbarkeit und Färbung getrennt: eine `0` heisst „nicht zurueckgelesen", nicht
            // „falsch gefaerbt". Beides faellt weiterhin durch (`ok` fordert beide Sammelwerte),
            // aber die Meldung sagt jetzt, welcher der beiden Faelle vorliegt.
            r.ks_read &= ks != 0;
            r.l1_read &= l1 != 0;
            r.l2_read &= l2 != 0;
            r.ks_in_mask &=
                ks == 0 || region_in_mask(ks, crate::system::USER_KSTACK_SIZE as u64, r.colors, m);
            r.l1_in_mask &= l1 == 0 || region_in_mask(l1, PAGE, r.colors, m);
            r.l2_in_mask &= l2 == 0 || region_in_mask(l2, PAGE, r.colors, m);
        }
        r.kernel_side_readable = r.ks_read && r.l1_read && r.l2_read;
        r.kernel_side_in_mask = r.ks_in_mask && r.l1_in_mask && r.l2_in_mask;
        crate::system::destroy_isolated(ta);
        crate::system::destroy_isolated(tb);
        // Leckprüfung an GENAU den beiden Regionen, nicht an der globalen Summe. Der
        // Summenvergleich war hier nachweislich unbrauchbar: nebenher sammelt ein anderer Kern
        // den Stack des gerade gefaulteten `iso_probe`-Threads ein, und je nachdem, ob dieser
        // Rückgang ins Messfenster fällt, wurde derselbe Kernel mal grün und mal rot gemeldet
        // (auf dem x86-Testaufbau beobachtet). Ein Test, der aus fremdem Grund fehlschlägt, ist
        // so wenig wert wie einer, der nicht fehlschlagen kann.
        r.balanced =
            crate::system::region_fully_free(ba, sz) && crate::system::region_fully_free(bb, sz);
    }
    r.ok = r.usable
        && r.spawned
        && r.in_mask
        && r.kernel_side_readable
        && r.kernel_side_in_mask
        && r.disjoint
        && r.oversize_refused
        && r.balanced;
    r
}

/// Die gemessene Geometrie in den Boot-Bericht schreiben.
///
/// Kein Testbericht, sondern eine Bring-up-Meldung (todo F3): sie sagt, was die *Hardware*
/// hergibt, unabhängig davon, ob ein Test läuft.
/// Das Ergebnis von [`run_color`] melden — **die einzige Druckstelle, von beiden Hochlaufwegen
/// gerufen.**
///
/// Sie stand bis 2026-08-01 in `arch/x86_64/bringup.rs`, also auf genau einem Zweig. Das hatte zwei
/// Folgen, und beide sind eingetreten: der Test lief auf aarch64 gar nicht (todo A1 „nur auf x86
/// gemessen"), und die Textfassung driftete — `color` schrieb als **einzige** von 69 Stellen `FAIL`
/// statt `FAILURES`, womit ein durchgefallener Test aus der Ergebnissignatur der Suite (B-1.2c)
/// HERAUSfiel, statt aufzufallen. Zwei Fassungen desselben Urteils driften; eine kann es nicht.
///
/// Hinter demselben Gate wie [`ColorTest`] — ohne das baut die schlanke Konfiguration nicht (F1).
#[cfg(feature = "selftest")]
pub fn report_color(c: &ColorTest) {
    if !c.usable {
        println!(
            "color   : SKIP -- {} Farbe(n) gemessen, unter 2 gibt es nichts zu trennen (QEMU meldet \
             ohne echtes CPU-Modell keine Cache-Geometrie; mit -cpu Skylake-Client sind es 256)",
            c.colors
        );
        return;
    }
    println!(
        "color   : {} Farben, {} Partitionen, Region {} KiB · in_mask={} \
         kernelseite={} (kstack={} l1={} l2={}) rueckgelesen={} (kstack={} l1={} l2={}) \
         disjunkt={} \
         uebergross_abgewiesen={} bilanz={}",
        c.colors,
        PARTITIONS,
        region_bytes() / 1024,
        c.in_mask as u8,
        c.kernel_side_in_mask as u8,
        c.ks_in_mask as u8,
        c.l1_in_mask as u8,
        c.l2_in_mask as u8,
        c.kernel_side_readable as u8,
        c.ks_read as u8,
        c.l1_read as u8,
        c.l2_read as u8,
        c.disjoint as u8,
        c.oversize_refused as u8,
        c.balanced as u8
    );
    println!(
        // `FAILURES`, nicht `FAIL` — s. den Kommentar am Kopf dieser Funktion.
        "color   : {} (zwei isolierte PDs teilen sich keine Cache-Farbe)",
        if c.ok { "ALL PASS" } else { "FAILURES" }
    );
}

pub fn report() {
    match hal::cache::llc() {
        Some(g) => {
            let colors = count();
            println!(
                "cache   : LLC L{} {} KiB, {}-fach, {} B/Zeile, {} Sets -> {} Seitenfarbe(n){}",
                g.level,
                g.size_bytes / 1024,
                g.ways,
                g.line_bytes,
                g.sets,
                colors,
                if colors >= 2 { "" } else { " (keine Partitionierung moeglich)" }
            );
        }
        None => println!(
            "cache   : keine Cache-Geometrie gemeldet -> 1 Seitenfarbe (keine Partitionierung moeglich)"
        ),
    }
}

// --- B-4.5: Wirkung statt nur Zuteilung (Prime+Probe) --------------------------------------
//
// Bis hierher belegt `run_color`, dass die Farbsätze zweier PDs **disjunkt sind**. Das ist eine
// Aussage über die Zuteilung, nicht über ihre Wirkung. Die eigentliche Behauptung von A1 lautet
// aber: *weil* sie disjunkt sind, verdrängen sich die PDs im LLC nicht gegenseitig. Diese Zeile
// stand bis 2026-08-01 ungeprüft in `docs/invariants.md` §12.
//
// ## Warum das eine Positivkontrolle braucht
//
// Ein Prime+Probe misst, wie lange ein erneuter Durchlauf über den eigenen Datensatz dauert,
// nachdem jemand anderes den Cache benutzt hat. Bleibt die Zeit niedrig, war nichts verdrängt.
// Genau hier lauert der Fehler, gegen den dieses Projekt seine Regel hat: **auf einer Maschine
// ohne echten Cache bleibt die Zeit IMMER niedrig.** Unter TCG hätte der Test also „keine
// Verdrängung" gemeldet — und damit A1 scheinbar bewiesen, ohne je etwas gemessen zu haben.
//
// Deshalb misst er drei Werte statt einem:
//
//   base     — Opfer erneut lesen, ohne dass jemand dazwischen war
//   shared   — nachdem ein Angreifer mit dem GLEICHEN Farbsatz gelaufen ist
//   disjoint — nachdem ein Angreifer mit einem DISJUNKTEN Farbsatz gelaufen ist
//
// `shared` ist die Positivkontrolle: liegt sie nicht deutlich über `base`, kann diese Maschine
// Verdrängung nicht zeigen, und dann ist über `disjoint` **nichts** auszusagen -> SKIP, nicht
// PASS. Das erledigt den TCG-Fall nebenbei und ohne den Hypervisor zu erkennen: ein Emulator
// ohne Cache fällt durch die Positivkontrolle, weil er es nicht anders kann.
//
// ## Warum der Angreifer aus vielen Regionen besteht
//
// Eine gefärbte Region ist auf [`region_bytes`] begrenzt (64 KiB bei 4 Partitionen) — das ist
// die harte Grenze des Verfahrens, nicht eine Einstellung. Eine einzelne solche Region kann eine
// andere gar nicht aus dem LLC verdrängen: sie ist um Größenordnungen kleiner als der Anteil des
// LLC, der auf ihren Streifen entfällt. Der Angreifer besteht daher aus so vielen Regionen
// desselben Streifens, dass er diesen Anteil zweimal überschreibt — dimensioniert an der
// **gemessenen** LLC-Größe, nicht an einer Konstanten.

/// Obergrenze für die Angreifer-Regionen je Seite (Speicher: 2 × MAX × [`region_bytes`]).
#[cfg(feature = "selftest")]
const MAX_ATTACKER: usize = 96;

/// Wiederholungen je Messpunkt; gewertet wird das **Minimum**. Ein Minimum ist gegen Störungen
/// robust, wie sie ein Timer-Tick oder ein anderer Kern erzeugt: Störungen können eine Messung
/// nur verlängern, nie verkürzen.
#[cfg(feature = "selftest")]
const ROUNDS: usize = 8;

/// Ergebnis des Prime+Probe (B-4.5).
#[cfg(feature = "selftest")]
#[derive(Default)]
pub struct PrimeProbe {
    /// Urteil. Nur aussagekräftig, wenn [`Self::sensitive`] gilt.
    pub ok: bool,
    /// Konnte der Aufbau überhaupt hergestellt werden (Farben, Geometrie, Speicher)?
    pub ran: bool,
    /// **Positivkontrolle**: hat der gleichfarbige Angreifer messbar verdrängt? Ohne das ist
    /// über den disjunkten nichts auszusagen.
    pub sensitive: bool,
    /// Zyklen für einen Durchlauf über das Opfer — ungestört.
    pub base: u64,
    /// … nachdem ein Angreifer mit dem **gleichen** Farbsatz lief.
    pub shared: u64,
    /// … nachdem ein Angreifer mit einem **disjunkten** Farbsatz lief.
    pub disjoint: u64,
    /// Größe **eines** Angreifer-Datensatzes in KiB (beide sind gleich groß).
    pub attacker_kib: u64,
    /// Aller Speicher zurückgegeben?
    pub balanced: bool,
}

/// Einen Datensatz einmal vollständig durchlaufen, eine Leseoperation je Cache-Zeile.
///
/// `read_volatile` ist hier nicht Vorsicht, sondern Bedingung: ein gewöhnliches Lesen, dessen
/// Ergebnis niemand benutzt, darf der Compiler entfernen — und dann misst der Test eine leere
/// Schleife.
#[cfg(feature = "selftest")]
fn walk(regions: &[(u64, u64)], line: u64) {
    for &(base, len) in regions {
        let mut off = 0;
        while off < len {
            // SAFETY: `base..base+len` stammt aus `alloc_colored`, gehört exklusiv dem Test,
            // liegt im identity-gemappten Normal-RAM und wird nirgends gleichzeitig benutzt.
            unsafe { core::ptr::read_volatile((base + off) as *const u8) };
            off += line;
        }
    }
}

/// Einen Durchlauf über das Opfer messen (Zyklen).
#[cfg(feature = "selftest")]
fn probe(victim: &[(u64, u64)], line: u64) -> u64 {
    let t0 = hal::timer::cycles();
    walk(victim, line);
    hal::timer::cycles().wrapping_sub(t0)
}

/// **Prime+Probe: verdrängen disjunkte Farbsätze einander messbar weniger?** (B-4.5)
///
/// Gibt `ran == false`, wenn der Aufbau nicht herstellbar war (eine Farbe, keine Geometrie, zu
/// wenig Speicher), und `sensitive == false`, wenn die Maschine Verdrängung nicht zeigen kann.
/// Beides ist **kein Fehlschlag**, sondern die Feststellung, dass hier nichts zu messen ist.
#[cfg(feature = "selftest")]
pub fn run_prime_probe() -> PrimeProbe {
    let mut r = PrimeProbe::default();
    if !usable() {
        return r;
    }
    let Some(g) = hal::cache::llc() else {
        return r;
    };
    let (Some(m0), Some(m1)) = (mask_for(0), mask_for(1)) else {
        return r;
    };
    let line = u64::from(g.line_bytes).max(16);
    let sz = region_bytes();
    let free0 = crate::system::total_free();

    // So viele Regionen, dass der auf EINEN Streifen entfallende LLC-Anteil zweimal
    // überschrieben wird. Aus der gemessenen Geometrie, nicht geraten.
    let je_streifen = u64::from(g.size_bytes) / u64::from(PARTITIONS);
    let n = ((2 * je_streifen).div_ceil(sz) as usize).clamp(8, MAX_ATTACKER);

    // Belegen. Bei jedem Fehlschlag alles bisher Belegte zurueckgeben -- ein Test, der unter
    // Speichermangel leckt, macht den naechsten Test unbrauchbar.
    let mut caps: [Option<sel4lake_mem::MemoryCap>; 1 + 2 * MAX_ATTACKER] =
        core::array::from_fn(|_| None);
    let mut nutz = 0usize;
    let mut belegen = |maske: ColorMask,
                       caps: &mut [Option<sel4lake_mem::MemoryCap>],
                       nutz: &mut usize|
     -> Option<(u64, u64)> {
        let c = crate::system::alloc_colored(sz, PAGE, maske)?;
        let (b, l) = (c.base(), c.len());
        caps[*nutz] = Some(c);
        *nutz += 1;
        Some((b, l))
    };

    let mut victim = [(0u64, 0u64); 1];
    let mut ang_gleich = [(0u64, 0u64); MAX_ATTACKER];
    let mut ang_disjunkt = [(0u64, 0u64); MAX_ATTACKER];

    let mut aufbau = || -> Option<()> {
        victim[0] = belegen(m0, &mut caps, &mut nutz)?;
        for e in ang_disjunkt.iter_mut().take(n) {
            *e = belegen(m1, &mut caps, &mut nutz)?;
        }
        for e in ang_gleich.iter_mut().take(n) {
            *e = belegen(m0, &mut caps, &mut nutz)?;
        }
        Some(())
    };
    let aufgebaut = aufbau().is_some();

    if aufgebaut {
        r.ran = true;
        r.attacker_kib = n as u64 * sz / 1024;
        let (gleich, disjunkt) = (&ang_gleich[..n], &ang_disjunkt[..n]);

        // IRQs aus: ein Timer-Tick mitten in einer Messung verlaengert sie und verschiebt
        // ausgerechnet das Minimum, auf das gewertet wird.
        let daif = hal::cpu::local_irq_save();
        let (mut b, mut s, mut d) = (u64::MAX, u64::MAX, u64::MAX);
        for _ in 0..ROUNDS {
            // Ungestoert. Zweimal laufen, damit die erste (kalte) Runde nicht gewertet wird.
            walk(&victim, line);
            b = b.min(probe(&victim, line));

            // Disjunkter Angreifer.
            walk(&victim, line);
            walk(disjunkt, line);
            d = d.min(probe(&victim, line));

            // Gleichfarbiger Angreifer -- die Positivkontrolle.
            walk(&victim, line);
            walk(gleich, line);
            s = s.min(probe(&victim, line));
        }
        hal::cpu::local_irq_restore(daif);
        r.base = b;
        r.shared = s;
        r.disjoint = d;

        // Positivkontrolle: der gleichfarbige Angreifer muss den Durchlauf um mindestens die
        // Haelfte verlaengert haben. Darunter ist die Messung zu stumpf, um ueber den
        // disjunkten Fall zu urteilen -- und dann sagt der Test das, statt PASS zu melden.
        r.sensitive = s >= b.saturating_add(b / 2);
        // Die eigentliche Behauptung: mit disjunkten Farben liegt die Zeit naeher am
        // ungestoerten Fall als am verdraengten.
        r.ok = r.sensitive && d.saturating_mul(2) <= b.saturating_add(s);
    }

    for c in caps.iter_mut().take(nutz) {
        if let Some(cap) = c.take() {
            crate::system::free(cap);
        }
    }
    r.balanced = crate::system::total_free() == free0;
    r.ok = r.ok && r.balanced;
    r
}

/// Das Ergebnis von [`run_prime_probe`] melden — wie [`report_color`] die **einzige** Druckstelle.
#[cfg(feature = "selftest")]
pub fn report_prime_probe(p: &PrimeProbe) {
    if !p.ran {
        println!(
            "pprobe  : SKIP -- kein Aufbau moeglich ({} Farbe(n), LLC-Geometrie/Speicher)",
            count()
        );
        return;
    }
    println!(
        "pprobe  : Opfer {} KiB, Angreifer {} KiB je Farbsatz · ungestoert={} Zyklen \
         disjunkt={} gleichfarbig={} · balanciert={}",
        region_bytes() / 1024,
        p.attacker_kib,
        p.base,
        p.disjoint,
        p.shared,
        p.balanced as u8
    );
    if !p.sensitive {
        println!(
            "pprobe  : SKIP -- die Positivkontrolle traegt nicht: der GLEICHFARBIGE Angreifer hat \
             nicht messbar verdraengt (gleichfarbig={} vs ungestoert={}). Auf einer Maschine, die \
             Verdraengung nicht zeigen kann (z. B. TCG: kein echter Cache), ist ueber den disjunkten \
             Fall nichts auszusagen -- 'kein Unterschied' waere hier kein Beleg, sondern ein Artefakt",
            p.shared, p.base
        );
        return;
    }
    println!(
        "pprobe  : {} (B-4.5: disjunkte Farbsaetze verdraengen einander messbar weniger -- die \
         WIRKUNG von A1, nicht nur die Zuteilung; Positivkontrolle traegt)",
        if p.ok { "ALL PASS" } else { "FAILURES" }
    );
}
