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

/// Farbsatz der PD mit laufender Nummer `i` (rundläufig über [`PARTITIONS`]).
///
/// `None`, wenn die Aufteilung nicht aufgeht — dann gibt es keinen disjunkten Satz, und der
/// Aufrufer darf **nicht** ersatzweise „alle Farben" nehmen: das wäre die Zusicherung ohne
/// die Eigenschaft.
pub fn mask_for(i: u32) -> Option<ColorMask> {
    sel4lake_mem::stripe(i % PARTITIONS, PARTITIONS)
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
        r.in_mask = region_in_mask(ba, sz, r.colors, m0) && region_in_mask(bb, sz, r.colors, m1);
        r.disjoint = !regions_share_color((ba, sz), (bb, sz), r.colors);
        // Kernel-Seite: Stack (16 KiB) und die beiden obersten Tabellen (je 4 KiB) jeder PD.
        r.kernel_side_in_mask = [(ta, m0), (tb, m1)].iter().all(|&(t, m)| {
            let ks = crate::system::testsupport::kstack_of(t.slot());
            let (l1, l2) = crate::system::testsupport::vspace_tables_of(
                crate::system::testsupport::asid_of(t.slot()),
            );
            ks != 0
                && l1 != 0
                && l2 != 0
                && region_in_mask(ks, crate::system::USER_KSTACK_SIZE as u64, r.colors, m)
                && region_in_mask(l1, PAGE, r.colors, m)
                && region_in_mask(l2, PAGE, r.colors, m)
        });
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
