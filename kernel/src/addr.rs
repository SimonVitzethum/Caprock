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

// ================================================================================================
// Die DRITTE Achse: was ein Subjekt sieht (2026-08-04)
// ================================================================================================
//
// `Pa`/`Iova` trennen CPU- und Gerätesicht. Es gibt eine dritte, und sie hat dieselbe Falle: die
// **virtuelle** Adresse, unter der eine PD ihre Region sieht. Auch hier waren beide Werte lange
// gleich — und deshalb war jede Verwechslung folgenlos und unsichtbar.
//
// Zwei Fehler dieses Projekts hingen daran, beide erst sichtbar, als die Werte auseinanderliefen:
// `Scheduler::spawn_user` nahm EINEN Wert für den EL0-Stackzeiger und die Reap-Region (Ergebnis:
// `#PF cr2=0x80_0000_0000` im Kernel), und `spawn_isolated_native` nahm die Physadresse des
// Code-Frames als **Einsprungadresse**.
//
// **Warum ein Typ und nicht nur ein Prüfskript.** Der erste Anlauf war ein Wächter, der die
// Aufrufstellen gegen eine Liste hält. Er hat prompt zwei Löcher gezeigt: `vspace_map_dma` stand
// gar nicht in seiner Funktionsliste, und ein Grundtext war schlicht falsch. Ein Wächter über
// einer Textfläche prüft, was jemand aufgeschrieben hat; ein Typ prüft, was der Compiler sieht.
// Solange es einen öffentlichen Weg `Va::from(pa.raw())` gäbe, wäre die Liste eine Bitte.
//
// Deshalb: [`Va`] hat **keinen** Konstruktor aus `u64` und **keinen** aus [`Pa`] — außer
// [`Va::identity`], und die verlangt eine Variante von [`IdentityReason`]. Das Enum ist
// geschlossen; eine neue identische Abbildung braucht eine neue Variante, und die schreibt man
// nicht versehentlich. **Die Liste IST der Quelltext.**

/// Eine **virtuelle** Adresse — was ein Subjekt (eine PD) sieht.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Va(u64);

/// **Warum an dieser Stelle VA == PA gelten darf.**
///
/// Jede Variante ist eine Entscheidung mit Begründung, kein Etikett. Die Frage, die sie
/// beantworten muss, lautet: *warum gilt die Identität hier, und was wäre die Folge, wenn sie
/// fällt?* Wo eine Variante zusätzlich **falsifizierbar** ist, steht der Falsifikator dabei —
/// ein Grund, den niemand widerlegen kann, überlebt seinen Autor auch dann, wenn er falsch ist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdentityReason {
    /// **`SYS_MAP`/`SYS_UNMAP`.** Der Aufrufer nennt eine **Cap**, keine Adresse — die ABI trägt
    /// kein Adressargument (`sel4lake_abi::sys::MAP` liest `x1` als Cap-Index; die Basis kommt
    /// aus `ObjectKind::Memory(r).base`, also aus der Cap-Auflösung **im Kernel**).
    ///
    /// **Damit liegt die Identität NICHT in der ABI, sondern in einer Entscheidung des Kernels**
    /// — er nimmt die PA des Frames als VA. Das ist behebbar, ohne die ABI anzufassen: der
    /// Rückgabewert nennt dem Aufrufer die Adresse ohnehin (`reg::MSG0`), er muss sie also nicht
    /// vorher kennen. Bis zum 2026-08-04 stand hier „das ist die ABI" — eine falsche Ursache,
    /// die den Punkt als unbehebbar erscheinen liess.
    ///
    /// **Falsifikator:** `tools/identitaet.sh` prüft, dass der `SYS_MAP`-Zweig **keine** Adresse
    /// aus dem Frame liest. Träte eine auf, wäre dieser Grund widerlegt.
    SyscallMapByCap,
    /// **Kernel-seitiges Einblenden für Tests/Demos** (`map_into_thread`). Der Kernel hält die
    /// PA bereits in der Hand und blendet sie einem Thread ein; ein VA-Fenster wäre möglich, hier
    /// aber ohne Nutzen, weil beide Seiten derselbe Code sind. Kein Subjekt nennt die Adresse.
    KernelSetupMapping,
    /// **MMIO-Registerfenster eines Geräts.** Hier ist die Identität die **Zusicherung selbst**:
    /// ein Treiber rechnet mit Adressen aus der PCI-Enumeration, und die sind physisch (CPU-Sicht,
    /// [`Pa`]). Gäbe man ihm eine andere VA, müsste er sie erst erfahren — und die BAR-Werte, die
    /// er im Konfigurationsraum liest, wären falsch.
    DeviceMmioWindow,
    /// **DMA-Fenster einer Treiber-PD.** Getrennt von [`Self::DeviceMmioWindow`], weil hier
    /// **zwei** Achsen im Spiel sind: die PD sieht die Region unter einer VA, das **Gerät** unter
    /// einer [`Iova`]. Dass die CPU-seitige Abbildung identisch ist, ist eine Eigenschaft der
    /// **Abbildung**; dass die Region tief liegen muss, wäre eine der **Allokation** (32-Bit-
    /// Geräte) — s. `todo.md` E-Rest 3e. Die beiden in einen Eintrag zu falten war der Fehler der
    /// ersten Fassung dieser Liste.
    DeviceDmaWindow,
    /// **Globale Kernel-Abbildung eines Gerätefensters** (ECAM, BAR-Fenster beim Hochlauf). Kein
    /// Subjekt beteiligt: der Kernel bildet sich selbst ein Registerfenster ein, um zu
    /// enumerieren. „Identisch" heisst hier nur „in der Identitätskarte des Kernels".
    KernelGlobalDeviceWindow,
}

impl Va {
    /// **Die einzige Umwandlung `Pa -> Va`.**
    ///
    /// Sie verlangt einen Grund, und der Grund ist ein Wert eines geschlossenen Enums — also
    /// etwas, das im Quelltext steht und nicht in einer Liste daneben. Wer eine neue identische
    /// Abbildung braucht, braucht eine neue Variante; das ist die Stelle, an der jemand
    /// nachdenkt.
    pub const fn identity(_reason: IdentityReason, pa: Pa) -> Va {
        Va(pa.raw())
    }

    /// Eine VA aus dem **privaten Fenster** einer isolierten PD (E-Rest 3d). Stammt aus der
    /// Abbildung, nicht aus einer PA — deshalb ein eigener Weg.
    pub const fn window(v: u64) -> Va {
        Va(v)
    }

    /// Eine VA aus einer **Link-Adresse** (ELF `p_vaddr`, fester Stack-VA des Ladepfads). Sie
    /// kommt aus dem Programm, nicht aus dem Speicher — und schon gar nicht aus einer PA.
    pub const fn link(v: u64) -> Va {
        Va(v)
    }

    /// Rohwert — nur an der HAL-Grenze und in Diagnoseausgaben.
    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn offset(self, n: u64) -> Self {
        Va(self.0 + n)
    }
}

// Bewusst NICHT vorhanden: `From<u64> for Va`, `From<Pa> for Va`, `Va::new`. Genau diese drei
// wären der bequeme Weg zurück in die Identität — dieselbe Überlegung wie bei
// `DmaRegion::identity` (s. u.), eine Achse weiter.

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
