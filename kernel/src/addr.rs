//! **Zwei Adressachsen, zwei Typen** (ext-36, Schritt a).
//!
//! Ein DMA-Puffer hat zwei Adressen, die heute noch denselben Zahlenwert tragen und deshalb
//! ununterscheidbar sind:
//!
//! * die **physische** ([`Pa`]) — was die CPU sieht. Cache-Wartung läuft darüber, der
//!   Allokator kennt nur sie, und die Lebendigkeitsprüfung des DMA-Audits vergleicht gegen sie.
//! * die **IOVA** ([`Iova`]) — was das *Gerät* sieht. Die Stage-1-Tabelle der IOMMU bildet sie
//!   ab, die Bounds-Prüfung des Treibers arbeitet darauf, und sie steht in den Deskriptoren.
//!
//! Solange beide gleich sind, ist jede Verwechslung folgenlos — und genau deshalb unsichtbar.
//! Sobald sie auseinanderlaufen (Schritt b: IOVA-Fenster ≠ 0), wird aus jeder Verwechslung
//! entweder eine Cache-Wartung auf einer Adresse, unter der nichts liegt, oder eine
//! Bounds-Prüfung gegen die falsche Achse — **beides still**, nicht als Absturz.
//!
//! Deshalb sind es getrennte Typen, und zwar **bevor** die Werte auseinanderlaufen: der Compiler
//! zählt die Stellen auf, die heute beides vermischen. Jede davon wird zu einer Entscheidung
//! („hier gehört die PA hin" / „hier die IOVA") statt zu einer Zeile, die man beim Durchlesen
//! übersieht. Der Umbau ist dabei **verhaltensneutral** — beide Architekturen müssen unverändert
//! grün bleiben, weil sich nur die Typen ändern, nicht die Werte.
//!
//! Die Umwandlung nach `u64` ist bewusst eine sichtbare Handlung ([`Pa::raw`]/[`Iova::raw`]):
//! An der HAL-Grenze reden die Funktionen weiterhin in `u64`, und **jede** dieser Stellen ist
//! damit eine markierte Entscheidung.

/// Eine **physische** Adresse (CPU-Sicht).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pa(u64);

/// Eine **I/O-virtuelle** Adresse (Gerätesicht, von der IOMMU übersetzt).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Iova(u64);

impl Pa {
    pub const fn new(v: u64) -> Self {
        Pa(v)
    }
    /// Rohwert — nur an der HAL-Grenze und in Diagnoseausgaben.
    pub const fn raw(self) -> u64 {
        self.0
    }
    pub const fn offset(self, n: u64) -> Self {
        Pa(self.0 + n)
    }
}

impl Iova {
    pub const fn new(v: u64) -> Self {
        Iova(v)
    }
    /// Rohwert — nur an der HAL-Grenze, in Deskriptoren und in Diagnoseausgaben.
    pub const fn raw(self) -> u64 {
        self.0
    }
    pub const fn offset(self, n: u64) -> Self {
        Iova(self.0 + n)
    }
}

/// Eine DMA-Region in **beiden** Achsen.
///
/// Beide Werte werden gebraucht und dürfen nicht auseinander abgeleitet werden: die
/// Lebendigkeitsprüfung (`dma_audit` Code 4) vergleicht die **PA** gegen die Freiliste des
/// Allokators, die Bounds-Prüfung des Treibers die **IOVA** gegen den Kontextinhalt. Würde einer
/// der beiden aus dem anderen berechnet, wäre die Trennung wieder aufgehoben.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaRegion {
    pub iova: Iova,
    pub pa: Pa,
    pub len: u64,
}

impl DmaRegion {
    pub const EMPTY: DmaRegion = DmaRegion {
        iova: Iova::new(0),
        pa: Pa::new(0),
        len: 0,
    };

    // `identity(pa, len)` gab es bis ext-36 Schritt a — es setzte IOVA = PA. Der Konstruktor ist
    // **bewusst entfernt** und nicht nur ungenutzt: bliebe er als bequemer Einstieg stehen,
    // griffe die nächste Architektur (x86-Zuteilung) genau danach, und die Annahme wäre wieder in
    // den Übersetzungstabellen. Eine IOVA entsteht jetzt ausschließlich aus dem Fenster eines
    // Übersetzungskontexts (`ctx_alloc_iova`) — der Typ bietet keinen Weg zurück zur Identität.

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}
