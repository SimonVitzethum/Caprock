//! **Seitenfarben** — die Arithmetik hinter der Cache-Partitionierung (`todo.md` A1).
//!
//! Der Last-Level-Cache ist physisch indiziert. Die Set-Indexbits, die *oberhalb* des
//! Seitenoffsets liegen, hängen damit an der Physadresse und sind konstant für die gesamte
//! Seite — das ist ihre **Farbe**. Zwei Seiten verschiedener Farbe können einander im LLC
//! nicht verdrängen; ein Farbsatz ist deshalb ein Cache-Anteil.
//!
//! Dieses Modul ist **reine Arithmetik ohne Hardware** — genau deshalb ist es auf dem Host
//! prüfbar (`cargo test -p sel4lake-mem`). Die Geometrie kommt von `hal::cache`, die Politik
//! (welche PD welchen Satz bekommt) vom Kernel; hier steht nur, was eine Farbe *ist* und was
//! eine Maske bedeutet.

/// Seitengröße (Farbgranularität).
use crate::PAGE;

/// Ein Farbsatz: Bitmaske über bis zu 64 Farben.
///
/// 64 ist keine Verlegenheitsgrenze, sondern die Zahl, die ohne Allokation in ein Wort passt.
/// Reale LLCs haben mehr Farben (gemessen auf dem x86-Testaufbau: 256). Deshalb ist die Maske
/// **nicht** „Farbe i = Bit i", sondern ein Muster, das sich über den Farbraum wiederholt:
/// Farbe `c` gehört zur Maske, wenn Bit `c % 64` gesetzt ist. Bei 256 Farben und 64 Bits fasst
/// ein Maskenbit also 4 Farben zusammen — die Partitionierung wird gröber, bleibt aber
/// **disjunkt**, und das ist die Eigenschaft, auf die es ankommt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorMask(pub u64);

/// Anzahl Bits in einer [`ColorMask`].
pub const MASK_BITS: u32 = 64;

impl ColorMask {
    /// Leere Maske — akzeptiert keine Farbe. Eine Allokation dagegen MUSS fehlschlagen.
    pub const EMPTY: ColorMask = ColorMask(0);
    /// Alle Farben (das Verhalten vor A1: keine Partitionierung).
    pub const ALL: ColorMask = ColorMask(u64::MAX);

    /// Enthält die Maske Farbe `c`?
    #[inline]
    pub const fn contains(self, c: u32) -> bool {
        self.0 & (1u64 << (c % MASK_BITS)) != 0
    }

    /// Ist die Maske leer?
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Überschneiden sich zwei Farbsätze? **Das ist die A1-Eigenschaft**: für zwei PDs
    /// muss das `false` sein, sonst teilen sie sich Cache-Sets.
    #[inline]
    pub const fn intersects(self, other: ColorMask) -> bool {
        self.0 & other.0 != 0
    }

    /// Anzahl gesetzter Bits.
    #[inline]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }
}

/// Farbe einer Physadresse bei `colors` Farben.
///
/// `colors` muss eine Zweierpotenz sein; andernfalls entspräche die Rechnung nicht den
/// Set-Indexbits, und die „Farbe" wäre eine Zahl ohne Bezug zur Hardware.
#[inline]
pub fn color_of(pa: u64, colors: u32) -> u32 {
    if colors <= 1 {
        return 0;
    }
    ((pa / PAGE) as u32) & (colors - 1)
}

/// **Streifen** `i` von `n` disjunkten Farbsätzen.
///
/// Die Aufteilung ist bewusst *streifenweise* (zusammenhängende Farbblöcke) und nicht
/// verschränkt: Farben eines Streifens liegen im RAM als zusammenhängende Seitenläufe
/// nebeneinander, was den Allokator zusammenhängende Blöcke schneiden lässt statt Einzelseiten
/// aus der Mitte zu stanzen (Fragmentierung). Bei verschränkter Vergabe wäre jede zweite Seite
/// ein eigenes Fragment.
///
/// `None`, wenn `n` keine Zweierpotenz ist, `n > MASK_BITS`, oder `i >= n` — die Aufteilung
/// muss aufgehen, sonst wären die Sätze nicht disjunkt und die Zusicherung eine Behauptung.
pub fn stripe(i: u32, n: u32) -> Option<ColorMask> {
    if n == 0 || i >= n || n > MASK_BITS || !n.is_power_of_two() {
        return None;
    }
    let per = MASK_BITS / n;
    let lo = i * per;
    // `per == MASK_BITS` (n == 1) würde bei `1<<64` überlaufen -> Sonderfall über ALL.
    let bits = if per == MASK_BITS { u64::MAX } else { ((1u64 << per) - 1) << lo };
    Some(ColorMask(bits))
}

/// **Niedrigster freier Streifen** unter `n`, oder `None`, wenn alle vergeben sind (B-4.2).
///
/// `taken` ist eine Bitmenge: Bit `i` gesetzt heißt Streifen `i` ist vergeben. Die Funktion ist
/// bewusst rein und liegt hier statt im Kernel — dieselbe Technik wie bei `cache_decode`: die
/// Arithmetik gegen eingespeiste Werte prüfen, nicht gegen den Zustand einer laufenden Maschine.
///
/// **Der Rückgabewert `None` ist der eigentliche Zweck.** Vorher vergab der Kernel Farbsätze mit
/// `i % n` — die (n+1)-te PD bekam wieder den Satz der ersten, und niemand merkte es. Eine
/// erschöpfte Partitionierung muss ein *Fehlschlag* sein, keine stille Überschneidung.
pub const fn pick_free(taken: u32, n: u32) -> Option<u32> {
    if n == 0 || n > 32 {
        return None;
    }
    let mut i = 0;
    while i < n {
        if taken & (1u32 << i) == 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leere Belegung -> Streifen 0; danach der jeweils niedrigste freie.
    #[test]
    fn pick_free_nimmt_den_niedrigsten_freien() {
        assert_eq!(pick_free(0b0000, 4), Some(0));
        assert_eq!(pick_free(0b0001, 4), Some(1));
        assert_eq!(pick_free(0b0011, 4), Some(2));
        assert_eq!(pick_free(0b0111, 4), Some(3));
        // Luecke in der Mitte: freigegebene Streifen werden wiederverwendet, nicht uebersprungen.
        assert_eq!(pick_free(0b1101, 4), Some(1));
    }

    /// **Der Punkt der Uebung:** ist alles vergeben, gibt es KEINEN Ersatz.
    #[test]
    fn pick_free_erschoepft_ist_ein_fehlschlag_keine_wiederholung() {
        assert_eq!(pick_free(0b1111, 4), None);
        // Und nicht etwa Streifen 0 erneut -- genau das war der Fehler von `i % n`.
        assert_ne!(pick_free(0b1111, 4), Some(0));
    }

    /// Bits oberhalb von `n` gehen die Auswahl nichts an.
    #[test]
    fn pick_free_ignoriert_bits_jenseits_von_n() {
        assert_eq!(pick_free(0b1111_0000, 4), Some(0));
        assert_eq!(pick_free(0b1111_0001, 4), Some(1));
    }

    /// Unsinnige Streifenzahlen ergeben keinen Streifen (statt Panik oder Ueberlauf).
    #[test]
    fn pick_free_weist_unsinnige_streifenzahl_ab() {
        assert_eq!(pick_free(0, 0), None);
        assert_eq!(pick_free(0, 33), None);
        // Genau 32 ist die Grenze und muss noch gehen.
        assert_eq!(pick_free(0, 32), Some(0));
        assert_eq!(pick_free(u32::MAX, 32), None);
    }

    /// Zusammenspiel mit `stripe`: jeder von `pick_free` gelieferte Index ergibt einen gueltigen
    /// Satz, zwei verschiedene ueberlappen nicht, und nach `n` Vergaben ist Schluss.
    ///
    /// Das ist die Aussage, auf der B-4.2 steht -- „sauberer Fehlschlag statt stiller
    /// Ueberschneidung" ist genau die Konjunktion dieser beiden Haelften.
    #[test]
    fn gelieferte_indizes_ergeben_disjunkte_saetze() {
        let mut taken = 0u32;
        let mut masks = [0u64; 4];
        let mut used = 0usize;
        while let Some(i) = pick_free(taken, 4) {
            let bits = stripe(i, 4).expect("Aufteilung geht auf").0;
            assert_ne!(bits, 0, "ein leerer Satz waere keine Trennung");
            for m in masks.iter().take(used) {
                assert_eq!(bits & m, 0, "Streifen {i} ueberlappt einen frueheren");
            }
            masks[used] = bits;
            used += 1;
            taken |= 1 << i;
        }
        assert_eq!(used, 4, "genau vier disjunkte Saetze, dann Schluss");
        assert_eq!(pick_free(taken, 4), None, "erschoepft heisst erschoepft");
        // Gegenprobe: die vier Saetze zusammen decken den ganzen Farbraum ab, keiner faellt weg.
        assert_eq!(masks.iter().fold(0u64, |a, m| a | m), u64::MAX);
    }

    /// Farbe ist seitenkonstant und läuft mit der Seitennummer um.
    #[test]
    fn farbe_ist_seitenkonstant_und_zyklisch() {
        let colors = 256;
        assert_eq!(color_of(0, colors), 0);
        assert_eq!(color_of(PAGE - 1, colors), 0, "innerhalb der Seite konstant");
        assert_eq!(color_of(PAGE, colors), 1);
        assert_eq!(color_of(PAGE * 255, colors), 255);
        assert_eq!(color_of(PAGE * 256, colors), 0, "Umlauf nach `colors` Seiten");
    }

    /// Ohne Farbunterscheidung ist alles Farbe 0 — und das darf nicht wie Erfolg aussehen.
    #[test]
    fn eine_farbe_heisst_keine_farbe() {
        assert_eq!(color_of(PAGE * 7, 1), 0);
        assert_eq!(color_of(PAGE * 8, 1), 0);
        assert_eq!(stripe(0, 1).unwrap(), ColorMask::ALL);
    }

    /// **Die A1-Eigenschaft**: n Streifen sind paarweise disjunkt und decken zusammen alles ab.
    #[test]
    fn streifen_sind_paarweise_disjunkt_und_vollstaendig() {
        for n in [1u32, 2, 4, 8, 16, 32, 64] {
            let masks: alloc_vec::Vec = (0..n).map(|i| stripe(i, n).unwrap()).collect();
            let mut union = 0u64;
            for (a, ma) in masks.iter().enumerate() {
                assert!(!ma.is_empty(), "n={n}: Streifen {a} ist leer");
                for (b, mb) in masks.iter().enumerate() {
                    if a != b {
                        assert!(!ma.intersects(*mb), "n={n}: Streifen {a} und {b} ueberlappen");
                    }
                }
                union |= ma.0;
            }
            assert_eq!(union, u64::MAX, "n={n}: Streifen decken nicht alle Farben ab");
        }
    }

    /// Eine Aufteilung, die nicht aufgeht, wird abgewiesen statt stillschweigend gerundet.
    #[test]
    fn ungueltige_aufteilung_wird_abgewiesen() {
        assert!(stripe(0, 0).is_none(), "n=0");
        assert!(stripe(2, 2).is_none(), "i >= n");
        assert!(stripe(0, 3).is_none(), "keine Zweierpotenz");
        assert!(stripe(0, 128).is_none(), "mehr Streifen als Maskenbits");
    }

    /// Die Maske wiederholt sich über den Farbraum — bei 256 Farben trägt ein Bit 4 Farben,
    /// und zwei disjunkte Streifen bleiben auch dann disjunkt.
    #[test]
    fn maske_wiederholt_sich_ueber_den_farbraum() {
        let (a, b) = (stripe(0, 2).unwrap(), stripe(1, 2).unwrap());
        let colors = 256;
        for page in 0..(colors as u64 * 2) {
            let c = color_of(page * PAGE, colors);
            assert!(a.contains(c) ^ b.contains(c), "Farbe {c} in beiden oder keinem Streifen");
        }
    }

    /// Winziger Vec-Ersatz: die Crate ist `no_std`, die Tests laufen aber auf dem Host.
    mod alloc_vec {
        pub type Vec = std::vec::Vec<super::super::ColorMask>;
    }
}
