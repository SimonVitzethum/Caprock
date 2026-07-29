//! Capability-basierter physischer Allokator.
//!
//! Verwaltet freies RAM als sortierte, koaleszierende Freiliste (feste Kapazität,
//! daher allokationsfrei und deterministisch) und prägt Wurzel-[`MemoryCap`]s.
//! `alloc` carvt eine seitenausgerichtete Region (First-Fit nach Adresse);
//! `free` gibt sie zurück und verschmilzt benachbarte Bereiche.

use crate::cap::MemoryCap;
use crate::color::{color_of, ColorMask, MASK_BITS};
use crate::region::{PhysRegion, Rights};
use crate::PAGE;

/// Tragen **alle** `npages` Seiten ab `start` eine Farbe aus `mask`?
///
/// Bewusst eine eigene Funktion mit früher Rückgabe bei der ersten falschen Farbe: die
/// Bedingung ist ein „für alle", und ein „für alle" das aus Bequemlichkeit nur die erste Seite
/// prüft, hält die Zusicherung nicht.
fn run_is_colored(start: u64, npages: u64, colors: u32, mask: ColorMask) -> bool {
    for j in 0..npages {
        if !mask.contains(color_of(start + j * PAGE, colors)) {
            return false;
        }
    }
    true
}

/// Maximale Anzahl freier Fragmente. 64 genügte QEMU, wird aber auf realer HW mit vielen
/// GLEICHZEITIG lebenden isolierten PDs überschritten -> `alloc` gibt `None` trotz freiem RAM
/// (Split-Rest passt nicht in die Liste). 1024 * 16 B = 16 KiB BSS; alloc/free scannen linear (O(n)).
const MAX_FRAGMENTS: usize = 1024;

/// Physischer Speicher-Allokator über eine feste Freiliste.
pub struct PhysAllocator {
    regions: [PhysRegion; MAX_FRAGMENTS],
    len: usize,
}

const fn align_up(value: u64, align: u64) -> u64 {
    (value + (align - 1)) & !(align - 1)
}

impl Default for PhysAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysAllocator {
    pub const fn new() -> Self {
        Self {
            regions: [PhysRegion::new(0, 0); MAX_FRAGMENTS],
            len: 0,
        }
    }

    /// Freies RAM registrieren (wird in die Freiliste eingefügt + koalesziert).
    /// Gibt `false` zurück, wenn die Freiliste voll ist.
    pub fn add_region(&mut self, base: u64, len: u64) -> bool {
        self.insert(PhysRegion::new(base, len))
    }

    /// `size` Bytes mit Ausrichtung `align` (mind. [`PAGE`]) allozieren.
    /// Liefert eine RW-Wurzel-Cap oder `None`, wenn kein Platz passt.
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<MemoryCap> {
        let align = align.max(PAGE);
        let size = align_up(size.max(1), PAGE);

        // BEST-FIT: das Fragment mit dem KLEINSTEN Rest (`r.len - size`) wählen, das die Allokation
        // (nach Alignment) fasst. So landen kleine Allocs in kleinen Löchern und GROSSE Löcher
        // bleiben für große/ausgerichtete Allocs erhalten (z. B. 2-MiB-aligned isolierte Stacks) —
        // vermeidet First-Fit-Fragmentierung (4-KiB-Tabellen knabbern das erste große Fragment an).
        // Semantik sonst unverändert (Split/Coalesce/Accounting) -> memtest (1 Fragment) unberührt.
        let mut best: Option<usize> = None;
        for i in 0..self.len {
            let r = self.regions[i];
            let start = align_up(r.base, align);
            // Overflow beim Ausrichten/Addieren -> dieses Fragment überspringen.
            let Some(end) = start.checked_add(size) else {
                continue;
            };
            if start >= r.base && end <= r.end() {
                // Beidseitiger Verschnitt erhöht die Fragmentzahl um 1; ist die Liste dann voll,
                // ginge das Suffix beim `insert` verloren (Leck) -> dieses Fragment überspringen.
                let two_sided = start > r.base && end < r.end();
                if two_sided && self.len >= MAX_FRAGMENTS {
                    continue;
                }
                if best.map_or(true, |b| r.len < self.regions[b].len) {
                    best = Some(i);
                }
            }
        }
        let i = best?;
        let r = self.regions[i];
        let start = align_up(r.base, align);
        let end = start + size;
        self.remove(i);
        self.insert(PhysRegion::new(r.base, start - r.base)); // Präfix (evtl. leer)
        self.insert(PhysRegion::new(end, r.end() - end)); //       Suffix (evtl. leer)
        Some(MemoryCap::new(PhysRegion::new(start, size), Rights::RW))
    }

    /// Wie [`alloc`](Self::alloc), aber **jede Seite** der gelieferten Region trägt eine Farbe
    /// aus `mask` (Cache-Partitionierung, `todo.md` A1).
    ///
    /// Aufeinanderfolgende Seiten tragen aufeinanderfolgende Farben. Eine zusammenhängende
    /// Region über `n` Seiten überstreicht also `n` aufeinanderfolgende Farben — **das ist die
    /// eigentliche Grenze des Verfahrens**, nicht eine Eigenschaft dieser Funktion: wer eine
    /// Region will, die größer ist als sein Farbanteil, kann sie nicht bekommen, ohne fremde
    /// Farben mitzunehmen. Die Funktion gibt dann `None` zurück, statt die Zusicherung still
    /// aufzuweichen.
    ///
    /// `None` auch bei leerer Maske. `colors <= 1` heißt „keine Farbunterscheidung möglich":
    /// dann ist jede Region einfarbig, und die Funktion verhält sich wie [`alloc`](Self::alloc)
    /// — der **Aufrufer** muss entscheiden, ob er das akzeptiert (s. `colors::usable` im Kernel),
    /// denn eine erfüllte Farbbedingung über einer einzigen Farbe sagt nichts aus.
    pub fn alloc_colored(
        &mut self,
        size: u64,
        align: u64,
        colors: u32,
        mask: ColorMask,
    ) -> Option<MemoryCap> {
        if mask.is_empty() {
            return None;
        }
        if colors <= 1 || mask == ColorMask::ALL {
            return self.alloc(size, align);
        }
        let align = align.max(PAGE);
        let size = align_up(size.max(1), PAGE);
        let npages = size / PAGE;
        // Mehr Seiten als Maskenbits ginge nur mit lückenloser Maske — die ist oben schon
        // abgefangen. Ohne diese Zeile liefe die Suche über alle Fragmente ins Leere.
        if npages > MASK_BITS as u64 {
            return None;
        }
        let step = align;

        let mut best: Option<(usize, u64)> = None;
        for i in 0..self.len {
            let r = self.regions[i];
            let mut start = align_up(r.base, align);
            // Das Farbmuster wiederholt sich spätestens nach `colors` Schritten; mehr zu
            // probieren kann nichts Neues finden.
            let mut tries = 0u32;
            while tries <= colors {
                let Some(end) = start.checked_add(size) else {
                    break;
                };
                if end > r.end() {
                    break;
                }
                if run_is_colored(start, npages, colors, mask) {
                    let two_sided = start > r.base && end < r.end();
                    if !(two_sided && self.len >= MAX_FRAGMENTS) {
                        // Best-Fit wie in `alloc`: kleinstes taugliches Fragment zuerst.
                        if best.map_or(true, |(b, _)| r.len < self.regions[b].len) {
                            best = Some((i, start));
                        }
                    }
                    break;
                }
                let Some(next) = start.checked_add(step) else {
                    break;
                };
                start = next;
                tries += 1;
            }
        }
        let (i, start) = best?;
        let r = self.regions[i];
        let end = start + size;
        self.remove(i);
        self.insert(PhysRegion::new(r.base, start - r.base));
        self.insert(PhysRegion::new(end, r.end() - end));
        Some(MemoryCap::new(PhysRegion::new(start, size), Rights::RW))
    }

    /// Eine Capability zurückgeben (konsumiert sie) und ihre Region freigeben.
    pub fn free(&mut self, cap: MemoryCap) -> bool {
        self.insert(cap.region())
    }

    /// Eine rohe Region freigeben (für die Finalisierung im Capability-System,
    /// wenn die letzte Cap auf ein Memory-Objekt gelöscht wird; die Korrektheit
    /// — genau einmalige Freigabe — sichert dort der Derivation-Tree).
    pub fn free_region(&mut self, region: PhysRegion) -> bool {
        self.insert(region)
    }

    /// Ist `[base, base+len)` **vollständig** frei?
    ///
    /// Gegenstück zu [`overlaps_free`](Self::overlaps_free), die schon bei teilweiser
    /// Überlappung `true` sagt. Für eine Leckprüfung braucht es die strenge Form: „die Region
    /// dieser PD ist zurück" ist etwas anderes als „ein Teil davon ist zurück".
    ///
    /// Warum das die richtige Leckprüfung ist und nicht ein Vorher/Nachher-Vergleich von
    /// [`total_free`](Self::total_free): der Summenzähler ist **global**. Läuft nebenher ein
    /// anderer Kern, der einen Thread-Stack einsammelt, ändert er die Summe, ohne dass die
    /// geprüfte Operation etwas damit zu tun hätte — der Test wird dann aus fremdem Grund rot
    /// (auf dem x86-Testaufbau reproduziert: `bilanz` mal 1, mal 0, gleicher Kernel). Diese
    /// Prüfung fragt stattdessen nach genau der Region, um die es geht.
    pub fn fully_free(&self, base: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        let Some(end) = base.checked_add(len) else {
            return false;
        };
        // Die Liste ist sortiert und koalesziert: eine vollständig freie Region liegt deshalb
        // in **einem** Fragment. Wäre sie über zwei verteilt, wären die beiden benachbart und
        // schon verschmolzen.
        for i in 0..self.len {
            let r = &self.regions[i];
            if r.base <= base && end <= r.end() {
                return true;
            }
        }
        false
    }

    /// Summe der freien Bytes.
    pub fn total_free(&self) -> u64 {
        let mut sum = 0;
        for i in 0..self.len {
            sum += self.regions[i].len;
        }
        sum
    }

    /// Anzahl freier (koaleszierter) Fragmente.
    pub fn fragments(&self) -> usize {
        self.len
    }

    /// Überlappt `[base, base+len)` ein aktuell **freies** Fragment? Trägt die DMA-Revoke-
    /// Ordnung-Invariante (`docs/invariants.md` §2): eine noch in einem SMMU-Kontext gemappte
    /// Region darf NIE freigegeben sein — andernfalls läge sie in der Free-Liste und überlappte
    /// hier (DMA-use-after-free). `len == 0` → nie eine Überlappung.
    pub fn overlaps_free(&self, base: u64, len: u64) -> bool {
        if len == 0 {
            return false;
        }
        let end = base.saturating_add(len);
        for i in 0..self.len {
            let r = &self.regions[i];
            if base < r.base + r.len && r.base < end {
                return true;
            }
        }
        false
    }

    // --- intern ---

    /// Sortiert (nach `base`) einfügen und anschließend koaleszieren.
    fn insert(&mut self, r: PhysRegion) -> bool {
        if r.is_empty() {
            return true;
        }
        if self.len >= MAX_FRAGMENTS {
            return false;
        }
        let mut i = 0;
        while i < self.len && self.regions[i].base < r.base {
            i += 1;
        }
        let mut j = self.len;
        while j > i {
            self.regions[j] = self.regions[j - 1];
            j -= 1;
        }
        self.regions[i] = r;
        self.len += 1;
        self.coalesce();
        true
    }

    fn remove(&mut self, idx: usize) {
        let mut k = idx;
        while k + 1 < self.len {
            self.regions[k] = self.regions[k + 1];
            k += 1;
        }
        self.len -= 1;
    }

    /// Benachbarte Regionen verschmelzen (Liste ist sortiert + überlappungsfrei).
    #[allow(clippy::needless_range_loop)]
    fn coalesce(&mut self) {
        let mut i = 0;
        while i + 1 < self.len {
            if self.regions[i].end() == self.regions[i + 1].base {
                self.regions[i].len += self.regions[i + 1].len;
                let mut k = i + 1;
                while k + 1 < self.len {
                    self.regions[k] = self.regions[k + 1];
                    k += 1;
                }
                self.len -= 1;
            } else {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::stripe;

    /// 256 Farben — die auf dem x86-Testaufbau **gemessene** Zahl (QEMU q35,
    /// `-cpu Skylake-Client`: LLC 16 MiB, 16-fach, 64 B/Zeile). Keine Wunschzahl.
    const COLORS: u32 = 256;

    /// Allokator mit einem zusammenhaengenden RAM-Fenster ab `base`.
    fn with_ram(base: u64, len: u64) -> PhysAllocator {
        let mut a = PhysAllocator::new();
        assert!(a.add_region(base, len));
        a
    }

    /// Traegt jede Seite der Region eine Farbe aus `mask`?
    fn all_pages_in(cap: &MemoryCap, mask: ColorMask) -> bool {
        (0..cap.len() / PAGE).all(|j| mask.contains(color_of(cap.base() + j * PAGE, COLORS)))
    }

    /// Die Farbmenge, die eine Region tatsaechlich belegt.
    fn colors_used(cap: &MemoryCap) -> std::collections::BTreeSet<u32> {
        (0..cap.len() / PAGE).map(|j| color_of(cap.base() + j * PAGE, COLORS)).collect()
    }

    #[test]
    fn farbige_allokation_liefert_nur_erlaubte_farben() {
        let mut a = with_ram(0x4000_0000, 64 * 1024 * 1024);
        let m = stripe(1, 4).unwrap();
        let cap = a.alloc_colored(4 * PAGE, PAGE, COLORS, m).expect("Platz vorhanden");
        assert!(all_pages_in(&cap, m), "Seite mit unerlaubter Farbe vergeben");
    }

    /// **Die A1-Eigenschaft im Allokator**: zwei PDs mit disjunkten Streifen bekommen
    /// Seiten, die sich keine einzige Cache-Farbe teilen.
    #[test]
    fn disjunkte_streifen_teilen_keine_farbe() {
        let mut a = with_ram(0x4000_0000, 64 * 1024 * 1024);
        let (m0, m1) = (stripe(0, 2).unwrap(), stripe(1, 2).unwrap());
        let c0 = a.alloc_colored(8 * PAGE, PAGE, COLORS, m0).expect("PD 0");
        let c1 = a.alloc_colored(8 * PAGE, PAGE, COLORS, m1).expect("PD 1");
        assert!(all_pages_in(&c0, m0));
        assert!(all_pages_in(&c1, m1));
        let (s0, s1) = (colors_used(&c0), colors_used(&c1));
        assert!(s0.is_disjoint(&s1), "gemeinsame Farben: {:?}", &s0 & &s1);
        // und die Regionen ueberlappen auch physisch nicht
        assert!(c0.base() + c0.len() <= c1.base() || c1.base() + c1.len() <= c0.base());
    }

    /// **Sensitivitaet**: ohne Farbbedingung liefert derselbe Allokator sehr wohl Seiten
    /// ausserhalb des Streifens. Ohne diesen Test koennte `disjunkte_streifen_teilen_keine_farbe`
    /// gruen sein, weil die Anordnung es zufaellig hergibt — nicht, weil die Maske wirkt.
    #[test]
    fn ohne_maske_faellt_die_eigenschaft_um() {
        let mut a = with_ram(0x4000_0000, 64 * 1024 * 1024);
        let m = stripe(1, 4).unwrap(); // Farben 16..31 (mod 64)
        let cap = a.alloc(4 * PAGE, PAGE).expect("Platz vorhanden");
        assert!(
            !all_pages_in(&cap, m),
            "ungefaerbte Allokation landete zufaellig im Streifen -> der Farbtest waere leer"
        );
    }

    /// Eine Region, die groesser ist als der Farbanteil, wird **abgewiesen** statt
    /// stillschweigend fremde Farben mitzunehmen. Das ist die harte Grenze des Verfahrens.
    #[test]
    fn zu_grosse_region_wird_abgewiesen() {
        let mut a = with_ram(0x4000_0000, 64 * 1024 * 1024);
        let m = stripe(0, 4).unwrap(); // 16 von 64 Bits
        assert!(a.alloc_colored(16 * PAGE, PAGE, COLORS, m).is_some(), "16 Seiten passen");
        assert!(
            a.alloc_colored(17 * PAGE, PAGE, COLORS, m).is_none(),
            "17 Seiten passen nicht in 16 aufeinanderfolgende Farben"
        );
        // Und der Klassiker: eine 2-MiB-Region (512 Seiten) ist mit KEINER echten
        // Teilmaske erfuellbar -- genau der Grund, warum der 2-MiB-Blockdeskriptor
        // der isolierten PD nicht gefaerbt werden kann (s. todo.md A1).
        assert!(a.alloc_colored(512 * PAGE, 512 * PAGE, COLORS, m).is_none());
    }

    /// `fully_free` unterscheidet „ganz zurueck" von „teilweise zurueck" — genau die
    /// Unterscheidung, an der ein Vorher/Nachher-Summenvergleich vorbeigeht.
    #[test]
    fn fully_free_ist_streng() {
        let mut a = with_ram(0x4000_0000, 16 * 1024 * 1024);
        let cap = a.alloc(8 * PAGE, PAGE).unwrap();
        let (b, l) = (cap.base(), cap.len());
        assert!(!a.fully_free(b, l), "belegte Region gilt als frei");
        assert!(a.free(cap));
        assert!(a.fully_free(b, l), "zurueckgegebene Region gilt nicht als frei");
        // Haelfte wieder wegnehmen: `overlaps_free` sagt weiter ja, `fully_free` muss nein sagen.
        let half = a.alloc(4 * PAGE, PAGE).unwrap();
        assert!(a.overlaps_free(b, l), "Rest ist noch frei -> Ueberlappung besteht");
        assert!(!a.fully_free(b, l), "teilweise belegte Region gilt als vollstaendig frei");
        assert!(a.free(half));
    }

    #[test]
    fn leere_maske_wird_abgewiesen() {
        let mut a = with_ram(0x4000_0000, 16 * 1024 * 1024);
        assert!(a.alloc_colored(PAGE, PAGE, COLORS, ColorMask::EMPTY).is_none());
    }

    /// `colors <= 1` heisst „keine Farbunterscheidung" -> Verhalten wie `alloc`.
    /// Der Aufrufer, nicht der Allokator, muss das als fehlende Eigenschaft werten.
    #[test]
    fn ohne_farben_wie_alloc() {
        let mut a = with_ram(0x4000_0000, 16 * 1024 * 1024);
        let cap = a.alloc_colored(4 * PAGE, PAGE, 1, stripe(0, 4).unwrap());
        assert!(cap.is_some());
    }

    /// Farbige Allokationen lecken nichts: nach dem Zurueckgeben steht die Bilanz wieder.
    #[test]
    fn farbige_allokation_ist_bilanzneutral() {
        let mut a = with_ram(0x4000_0000, 64 * 1024 * 1024);
        let free0 = a.total_free();
        let caps: std::vec::Vec<_> = (0..4)
            .map(|i| a.alloc_colored(4 * PAGE, PAGE, COLORS, stripe(i, 4).unwrap()).unwrap())
            .collect();
        assert!(a.total_free() < free0);
        for c in caps {
            assert!(a.free(c));
        }
        assert_eq!(a.total_free(), free0, "Speicher nach Rueckgabe nicht vollstaendig zurueck");
        assert_eq!(a.fragments(), 1, "Freiliste nicht wieder koalesziert");
    }
}
