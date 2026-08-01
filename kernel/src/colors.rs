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
    if let (Some((ta, ba)), Some((tb, bb))) = (a, b) {
        r.spawned = true;

        // Die Kernelseite SOFORT nach dem Erzeugen ablesen, vor jeder anderen Messung.
        //
        // Grund: Der Einsprungpunkt dieser PDs faultet ABSICHTLICH (Isolationstest). Ein
        // anderer Kern sammelt den gefaulteten Thread ein, und dabei werden Kernel-Stack und
        // ASID frei. `kstack_of()` und `vspace_tables_of()` liefern dann 0 -- und zwar alle
        // drei zugleich, weil sie an derselben Belegung haengen.
        //
        // Genau das war am 2026-08-01 in 4 von 400 Laeufen zu sehen: `rueckgelesen=0
        // (kstack=0 l1=0 l2=0)` bei sonst fehlerfreier Zuteilung (`in_mask`, `kernelseite`,
        // `disjunkt`, `bilanz` alle 1). Kein Farbfehler, sondern ein Wettlauf mit dem
        // Einsammler -- dieselbe Ursache, die weiter unten schon fuer `balanced` beschrieben
        // ist ("nebenher sammelt ein anderer Kern den Stack ... ein").
        //
        // Das Ablesen ist ein Atomzugriff auf eine Tabelle, keine teure Operation; es vor die
        // uebrigen Messungen zu ziehen kostet nichts und schliesst das Fenster.
        let kernelseite: [(u64, u64, u64); 2] = core::array::from_fn(|i| {
            let t = if i == 0 { ta } else { tb };
            let ks = crate::system::testsupport::kstack_of(t.slot());
            let (l1, l2) = crate::system::testsupport::vspace_tables_of(
                crate::system::testsupport::asid_of(t.slot()),
            );
            (ks, l1, l2)
        });

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
