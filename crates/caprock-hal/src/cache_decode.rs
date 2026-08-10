//! **Feldzerlegung von `CCSIDR_EL1`** — arch-neutral und ohne Hardware, damit sie prüfbar ist.
//!
//! Warum das hier steht und nicht in `aarch64/cache.rs`: das Registerlayout hat **zwei Fassungen**,
//! und welche gilt, entscheidet `ID_AA64MMFR2_EL1.CCIDX`. Ohne CCIDX liegen Assoziativität in
//! `[12:3]` und Setzahl in `[27:13]`; mit CCIDX in `[23:3]` und `[55:32]`. Wer die falsche Fassung
//! liest, bekommt keine Fehlermeldung, sondern eine **plausible falsche Zahl** — und daraus eine
//! falsche Farbanzahl, die stillschweigend die ganze Cache-Partitionierung entwertet.
//!
//! Genau diese Sorte Fehler lässt sich auf QEMU nicht finden: keines der angebotenen CPU-Modelle
//! meldet CCIDX (geprüft: `cortex-a72`, `cortex-a53`, `max`). Der CCIDX-Zweig wäre damit
//! geschrieben, übersetzt und **nie ausgeführt** — dieselbe Fehlerform, die dieses Projekt bereits
//! dreimal bezahlt hat (leere SMMU-Event-Queue, nie ausgeführter x86-Testpfad,
//! DMAR-Ausschlusspfad) und die zuletzt am 2026-07-29 als IRQ-Deadlock zuschlug.
//!
//! Die Antwort ist dieselbe wie bei `hal::dmar`: **eine reine Funktion über eingespeiste Daten.**
//! Der reale Pfad füttert sie aus dem Systemregister, der Test aus Literalen — und dann ist der
//! CCIDX-Zweig geprüft, ohne dass irgendeine Hardware ihn anbieten muss.

/// Geometrie einer Cache-Ebene, wie sie in `CCSIDR_EL1` steht.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ccsidr {
    /// Zeilenlänge in Bytes.
    pub line_bytes: u32,
    /// Assoziativität (Wege).
    pub ways: u32,
    /// Anzahl Sets.
    pub sets: u32,
}

impl Ccsidr {
    /// Gesamtgröße in Bytes.
    pub fn size_bytes(self) -> u64 {
        self.line_bytes as u64 * self.ways as u64 * self.sets as u64
    }
}

/// `CCSIDR_EL1` zerlegen. `ccidx` = `ID_AA64MMFR2_EL1.CCIDX != 0`.
///
/// Die Felder sind in beiden Fassungen **um eins vermindert** kodiert (`Associativity` = Wege − 1),
/// und `LineSize` ist `log2(Bytes) − 4`. Beides wird hier zurückgerechnet, damit der Aufrufer mit
/// echten Größen arbeitet statt mit Registerkodierungen.
pub const fn decode_ccsidr(ccsidr: u64, ccidx: bool) -> Ccsidr {
    let line_bytes = 1u32 << ((ccsidr & 0x7) as u32 + 4);
    let (ways, sets) = if ccidx {
        // FEAT_CCIDX: Associativity [23:3] (21 Bit), NumSets [55:32] (24 Bit).
        ((((ccsidr >> 3) & 0x1F_FFFF) + 1) as u32, (((ccsidr >> 32) & 0xFF_FFFF) + 1) as u32)
    } else {
        // Ohne CCIDX: Associativity [12:3] (10 Bit), NumSets [27:13] (15 Bit).
        ((((ccsidr >> 3) & 0x3FF) + 1) as u32, (((ccsidr >> 13) & 0x7FFF) + 1) as u32)
    };
    Ccsidr { line_bytes, ways, sets }
}

/// Anzahl unterscheidbarer **Seitenfarben** aus Sets und Zeilenlänge.
///
/// Farbe = die Set-Indexbits oberhalb des Seitenoffsets: `sets * line / page`. Ergebnis ist stets
/// eine Zweierpotenz und mindestens `1`; auf die nächstkleinere Zweierpotenz wird **abgerundet**,
/// denn nur echte Indexbits sind Farbbits — ein nicht-2er-potenter Set-Zähler (manche LLC-Slices)
/// darf keine Farben vortäuschen.
///
/// **`1` ist kein Erfolgswert**, sondern die Aussage „keine Farbunterscheidung möglich".
pub const fn colors_from(sets: u64, line: u64, page: u64) -> u32 {
    let span = sets * line;
    if span <= page {
        return 1;
    }
    let n = span / page;
    let bits = 63 - n.leading_zeros() as u64;
    (1u64 << bits) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Der reale Messwert vom ARM-Testaufbau (QEMU `virt`, `-cpu cortex-a72`): L2 1 MiB,
    /// 16-fach, 64 B/Zeile, 1024 Sets. Ohne CCIDX kodiert.
    #[test]
    fn ohne_ccidx_wie_auf_dem_testaufbau() {
        // LineSize=2 (2^(2+4)=64), Associativity=15 (->16), NumSets=1023 (->1024)
        let raw = 2u64 | (15 << 3) | (1023 << 13);
        let g = decode_ccsidr(raw, false);
        assert_eq!(g, Ccsidr { line_bytes: 64, ways: 16, sets: 1024 });
        assert_eq!(g.size_bytes(), 1024 * 1024);
        assert_eq!(colors_from(g.sets as u64, g.line_bytes as u64, 4096), 16);
    }

    /// **Der Zweig, den keine QEMU-CPU anbietet.** Dieselbe Geometrie, CCIDX-kodiert: die Felder
    /// stehen an anderen Bitpositionen, das Ergebnis muss identisch sein.
    #[test]
    fn mit_ccidx_dieselbe_geometrie_andere_bitpositionen() {
        let raw = 2u64 | (15 << 3) | (1023 << 32);
        let g = decode_ccsidr(raw, true);
        assert_eq!(g, Ccsidr { line_bytes: 64, ways: 16, sets: 1024 });
        assert_eq!(g.size_bytes(), 1024 * 1024);
    }

    /// **Sensitivität.** Wer das falsche Layout wählt, bekommt keine Fehlermeldung, sondern
    /// stillschweigend Unsinn. Ohne diesen Test könnten beide Zweige denselben Code enthalten und
    /// die Tests darüber wären trotzdem grün.
    #[test]
    fn falsches_layout_liefert_stillschweigend_unsinn() {
        let ccidx_raw = 2u64 | (15 << 3) | (1023 << 32);
        let falsch = decode_ccsidr(ccidx_raw, false); // CCIDX-Wert, aber alte Zerlegung
        assert_ne!(falsch.sets, 1024, "die Zerlegungen sind nicht unterscheidbar");
        assert_eq!(falsch.sets, 1, "NumSets liegt bei CCIDX oberhalb von Bit 27 -> alte Sicht: 0");

        let alt_raw = 2u64 | (15 << 3) | (1023 << 13);
        let falsch2 = decode_ccsidr(alt_raw, true); // alter Wert, aber CCIDX-Zerlegung
        assert_ne!(falsch2.ways, 16, "Associativity ist bei CCIDX 21 Bit breit");
    }

    /// Ein grosser LLC mit CCIDX — der Fall, fuer den das Feld ueberhaupt erweitert wurde.
    /// 64 MiB, 16-fach, 64 B/Zeile -> 65536 Sets, passt NICHT in die alten 15 Bit.
    #[test]
    fn grosser_llc_braucht_ccidx() {
        let sets = 65536u64;
        assert!(sets - 1 > 0x7FFF, "sonst waere CCIDX gar nicht noetig");
        let raw = 2u64 | (15 << 3) | ((sets - 1) << 32);
        let g = decode_ccsidr(raw, true);
        assert_eq!(g.sets, 65536);
        assert_eq!(g.size_bytes(), 64 * 1024 * 1024);
        assert_eq!(colors_from(g.sets as u64, g.line_bytes as u64, 4096), 1024);
    }

    /// Farbanzahl: Zweierpotenz, abgerundet, mindestens 1.
    #[test]
    fn farbanzahl_rundet_ab_und_ist_mindestens_eins() {
        assert_eq!(colors_from(64, 64, 4096), 1, "Spannweite = Seitengroesse -> eine Farbe");
        assert_eq!(colors_from(32, 64, 4096), 1, "kleiner als eine Seite -> eine Farbe");
        assert_eq!(colors_from(1024, 64, 4096), 16);
        assert_eq!(colors_from(1536, 64, 4096), 16, "24 -> abgerundet auf 16, nicht 24");
    }
}
