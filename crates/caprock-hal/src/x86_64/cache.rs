//! **Cache-Geometrie (x86_64)** — Grundlage der Seitenfarben (Cache-Coloring, todo A1).
//!
//! Der Last-Level-Cache ist **physisch indiziert**. Welche Set-Indexbits eine Seite bestimmt,
//! entscheidet damit die Physadresse — und genau diese Bits sind die „Farbe" einer Seite.
//! Zwei Seiten unterschiedlicher Farbe können sich im LLC nicht verdrängen.
//!
//! Die Geometrie wird **gemessen, nicht angenommen**: `CPUID`-Blatt 4 zählt die Cache-Ebenen
//! auf. Meldet die Plattform kein Blatt 4 (oder keinen Daten-/Unified-Cache), gibt [`llc`]
//! `None` zurück und [`page_colors`] `1` — „eine Farbe" heißt *keine* Partitionierung, und der
//! Aufrufer muss das als Fehlen der Eigenschaft behandeln, nicht als erfüllte Eigenschaft.

use super::cpu::{cpuid, cpuid_count};

/// Seitengröße, gegen die die Farben gerechnet werden (4 KiB).
pub const PAGE: u64 = 4096;

/// Gemessene Geometrie des Last-Level-Cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LlcGeometry {
    /// Cache-Ebene (1..=3), wie von der HW gemeldet.
    pub level: u8,
    /// Gesamtgröße in Bytes.
    pub size_bytes: u64,
    /// Assoziativität (Wege).
    pub ways: u32,
    /// Zeilenlänge in Bytes.
    pub line_bytes: u32,
    /// Anzahl Sets.
    pub sets: u32,
}

/// `CPUID.4:EAX[4:0]` — Cache-Typ.
const TYPE_NULL: u32 = 0;
const TYPE_DATA: u32 = 1;
const TYPE_UNIFIED: u32 = 3;

/// Last-Level-Cache aus `CPUID`-Blatt 4 lesen.
///
/// Genommen wird die **höchste** Ebene mit einem Daten- oder Unified-Cache; reine
/// Instruktions-Caches sind für die Datenseitenkanäle irrelevant. `None`, wenn die CPU
/// Blatt 4 nicht anbietet oder keine passende Ebene meldet.
pub fn llc() -> Option<LlcGeometry> {
    let mut best: Option<LlcGeometry> = None;
    for_each_level(|g| {
        if best.map_or(true, |b: LlcGeometry| g.level > b.level) {
            best = Some(g);
        }
    });
    best
}

/// **Die grösste Daten-Cache-Ebene UNTERHALB des LLC** — auf x86 üblicherweise der L2.
///
/// Warum das jemanden interessiert: Seitenfärbung partitioniert **nur** den LLC. Ein
/// Arbeitssatz, der vollständig in die darunterliegende (nicht partitionierte) Ebene passt, wird
/// von *jedem* Angreifer verdrängt, gleich welcher Farbe — ein Prime+Probe über einen solchen
/// Arbeitssatz misst dann die private Ebene und nicht die Partitionierung.
///
/// `kernel/src/colors.rs` (B-4.5) leitet daraus seine Opfergrösse ab. Bis 2026-08-02 stand dort
/// stattdessen eine feste Zahl (2 MiB) mit dem Kommentar „die Messmaschine hat ~1,5 MiB L2 je
/// Kern" — auf einer Maschine mit **genau 2 MiB L2** war die Bedingung damit gerade nicht mehr
/// erfüllt, und die Positivkontrolle trug nur noch sporadisch. Eine Bedingung, die als Zahl
/// festgeschrieben statt aus der Geometrie abgeleitet wird, gilt genau auf einer Maschine.
///
/// `None`, wenn es keine solche Ebene gibt oder die CPU Blatt 4 nicht anbietet.
pub fn below_llc() -> Option<LlcGeometry> {
    let top = llc()?;
    let mut best: Option<LlcGeometry> = None;
    for_each_level(|g| {
        if g.level < top.level && best.map_or(true, |b: LlcGeometry| g.level > b.level) {
            best = Some(g);
        }
    });
    best
}

/// Jede gemeldete Daten-/Unified-Ebene einmal an `f` geben.
///
/// Eine Stelle, an der `CPUID.4` zerlegt wird — [`llc`] und [`below_llc`] unterscheiden sich nur
/// in der Auswahl. Zwei Fassungen derselben Zerlegung wären genau die Doppelung, an der dieses
/// Projekt schon einmal auseinandergelaufene Farbarithmetik hatte.
fn for_each_level(mut f: impl FnMut(LlcGeometry)) {
    // Blatt 4 existiert nur, wenn das höchste Basisblatt >= 4 ist. Ohne diese Prüfung
    // liefert `cpuid` das höchste unterstützte Blatt zurück — also plausibel aussehenden
    // Müll, aus dem eine Farbanzahl fiele, die nichts mit der HW zu tun hat.
    if cpuid(0).0 < 4 {
        return;
    }
    // 16 Unterblätter sind mehr als jede real gemeldete Cache-Hierarchie; der Abbruch bei
    // TYPE_NULL ist der eigentliche Terminator, die Schranke nur das Netz darunter.
    for sub in 0..16u32 {
        let (eax, ebx, ecx, _) = cpuid_count(4, sub);
        let ctype = eax & 0x1F;
        if ctype == TYPE_NULL {
            break;
        }
        if ctype != TYPE_DATA && ctype != TYPE_UNIFIED {
            continue;
        }
        let level = ((eax >> 5) & 0x7) as u8;
        let line = (ebx & 0xFFF) + 1;
        let partitions = ((ebx >> 12) & 0x3FF) + 1;
        let ways = ((ebx >> 22) & 0x3FF) + 1;
        let sets = ecx + 1;
        let size = line as u64 * partitions as u64 * ways as u64 * sets as u64;
        f(LlcGeometry { level, size_bytes: size, ways, line_bytes: line, sets });
    }
}

/// Anzahl unterscheidbarer **Seitenfarben** im LLC.
///
/// Farbe = die Set-Indexbits, die *oberhalb* des Seitenoffsets liegen:
/// `colors = sets * line / PAGE` (äquivalent `size / (ways * PAGE)`).
/// Ergebnis ist stets eine Zweierpotenz und mindestens `1`.
///
/// **`1` bedeutet: keine Farbunterscheidung möglich.** Das ist kein Erfolgswert.
pub fn page_colors() -> u32 {
    let Some(g) = llc() else {
        return 1;
    };
    colors_from(g.sets as u64, g.line_bytes as u64)
}

/// Farbanzahl aus Sets und Zeilenlänge — als eigene Funktion, damit die Arithmetik ohne
/// Hardware prüfbar ist (s. Tests im `cache`-Modul der Crate).
pub(crate) fn colors_from(sets: u64, line: u64) -> u32 {
    // Dieselbe geprüfte Funktion wie auf ARM (`crate::cache_decode`) — die Farbarithmetik ist
    // nicht architekturabhängig, und zwei Fassungen davon würden auseinanderlaufen.
    crate::cache_decode::colors_from(sets, line, PAGE)
}
