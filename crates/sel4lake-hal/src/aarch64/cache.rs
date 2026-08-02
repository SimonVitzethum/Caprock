//! **Cache-Geometrie (aarch64)** — Grundlage der Seitenfarben (Cache-Coloring, todo A1).
//!
//! Spiegelbild zu `../x86_64/cache.rs`: dieselbe öffentliche API, andere Registerquelle.
//! `CLIDR_EL1` nennt die vorhandenen Ebenen, `CSSELR_EL1` wählt eine aus, `CCSIDR_EL1`
//! liefert Zeilenlänge, Assoziativität und Setzahl.
//!
//! Meldet die Plattform keinen Daten-/Unified-Cache, gibt [`llc`] `None` und [`page_colors`]
//! `1` zurück. **`1` ist kein Erfolgswert**, sondern die Aussage „keine Farbunterscheidung
//! möglich" — der Aufrufer muss das als Fehlen der Eigenschaft behandeln.

use core::arch::asm;

/// Seitengröße, gegen die die Farben gerechnet werden (4 KiB).
pub const PAGE: u64 = 4096;

/// Gemessene Geometrie des Last-Level-Cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LlcGeometry {
    /// Cache-Ebene (1..=7), wie von `CLIDR_EL1` gemeldet.
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

/// `CLIDR_EL1` lesen (Ctype-Felder je Ebene + LoC).
fn clidr() -> u64 {
    let v: u64;
    // SAFETY: reines Lesen eines Identifikationsregisters, keine Speicherwirkung.
    unsafe { asm!("mrs {}, CLIDR_EL1", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// `ID_AA64MMFR2_EL1.CCIDX` — erweitert das `CCSIDR_EL1`-Feldlayout (FEAT_CCIDX).
///
/// Ohne diese Abfrage läse man auf CCIDX-Hardware die Felder an der falschen Stelle und
/// bekäme eine plausible, aber falsche Setzahl — und damit eine falsche Farbanzahl.
fn ccidx() -> bool {
    let v: u64;
    // SAFETY: reines Lesen eines Identifikationsregisters.
    unsafe { asm!("mrs {}, ID_AA64MMFR2_EL1", out(reg) v, options(nomem, nostack, preserves_flags)) };
    (v >> 20) & 0xF != 0
}

/// `CCSIDR_EL1` für Ebene `level` (1-basiert), Daten-/Unified-Cache.
///
/// # Safety
/// Schreibt `CSSELR_EL1`. Der Aufrufer muss sicherstellen, dass die Ebene laut `CLIDR_EL1`
/// existiert (sonst ist `CCSIDR_EL1` `UNKNOWN`).
unsafe fn ccsidr_for(level: u8) -> u64 {
    let sel = ((level as u64 - 1) << 1) as u64; // InD = 0 -> Daten/Unified
    let v: u64;
    unsafe {
        asm!(
            "msr CSSELR_EL1, {sel}",
            "isb",
            "mrs {out}, CCSIDR_EL1",
            sel = in(reg) sel,
            out = out(reg) v,
            options(nostack, preserves_flags)
        )
    };
    v
}

/// Last-Level-Cache aus `CLIDR_EL1`/`CCSIDR_EL1` lesen.
///
/// Genommen wird die **höchste** Ebene mit einem Daten- oder Unified-Cache; reine
/// Instruktions-Caches sind für die Datenseitenkanäle irrelevant.
pub fn llc() -> Option<LlcGeometry> {
    let mut best: Option<LlcGeometry> = None;
    for_each_level(|g| {
        if best.map_or(true, |b: LlcGeometry| g.level > b.level) {
            best = Some(g);
        }
    });
    best
}

/// **Die grösste Daten-Cache-Ebene UNTERHALB des LLC** — Spiegelbild zur x86-Fassung, dort steht
/// die ausführliche Begründung. Kurz: Färbung partitioniert nur den LLC, also muss ein
/// Prime+Probe-Opfer grösser sein als die darunterliegende, *nicht* partitionierte Ebene —
/// sonst misst der Test diese Ebene statt der Partitionierung (B-4.5).
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

/// Jede von `CLIDR_EL1` gemeldete Daten-/Unified-Ebene einmal an `f` geben — eine Stelle, an der
/// `CCSIDR_EL1` zerlegt wird.
fn for_each_level(mut f: impl FnMut(LlcGeometry)) {
    let clidr = clidr();
    let ccidx = ccidx();
    for level in 1..=7u8 {
        let ctype = ((clidr >> (3 * (level as u32 - 1))) & 0x7) as u32;
        // 0 = kein Cache, 1 = nur Instruktion. 2 = nur Daten, 3 = getrennt (Datenteil),
        // 4 = unified — nur diese drei tragen Datenzugriffe.
        if ctype < 2 {
            continue;
        }
        // SAFETY: `ctype != 0` heißt, die Ebene existiert laut CLIDR_EL1.
        let c = unsafe { ccsidr_for(level) };
        // Die Zerlegung liegt in `crate::cache_decode` — arch-neutral und **auf dem Host
        // geprueft**, inklusive des CCIDX-Zweigs, den keine QEMU-CPU anbietet. Hier steht
        // deshalb nur noch das Lesen des Registers; die Arithmetik ist dieselbe, die die Tests
        // pruefen. Eine zweite Fassung an dieser Stelle waere genau der Fehler, den die
        // Auslagerung verhindern soll.
        let d = crate::cache_decode::decode_ccsidr(c, ccidx);
        f(LlcGeometry {
            level,
            size_bytes: d.size_bytes(),
            ways: d.ways,
            line_bytes: d.line_bytes,
            sets: d.sets,
        });
    }
}

/// Anzahl unterscheidbarer **Seitenfarben** im LLC.
///
/// Farbe = die Set-Indexbits, die *oberhalb* des Seitenoffsets liegen:
/// `colors = sets * line / PAGE`. Ergebnis ist stets eine Zweierpotenz, mindestens `1`.
pub fn page_colors() -> u32 {
    let Some(g) = llc() else {
        return 1;
    };
    colors_from(g.sets as u64, g.line_bytes as u64)
}

/// Farbanzahl aus Sets und Zeilenlänge — dieselbe geprüfte Funktion wie in
/// [`crate::cache_decode`], nicht eine zweite Fassung.
pub(crate) fn colors_from(sets: u64, line: u64) -> u32 {
    crate::cache_decode::colors_from(sets, line, PAGE)
}
