//! **Boot-Informationen für x86_64** — eine Struktur, zwei Herkünfte.
//!
//! Der Kernel startet auf x86 künftig auf zwei Wegen:
//!
//! * **Multiboot**: der Bootloader lädt ihn, das Trampolin baut Long Mode auf, der Kernel liest
//!   den Speicherplan aus der Multiboot-Info und die CPU-Liste aus der ACPI-MADT.
//! * **Kern-Übergabe**: ein Linux-Kernelmodul nimmt Kerne offline, schickt ihnen INIT-SIPI-SIPI
//!   in ein Trampolin und übergibt diese Struktur — Speicher, Kerne und Konsole kommen dann vom
//!   Wirt, nicht von der Firmware.
//!
//! ## Warum beide Wege sofort zusammenlaufen
//!
//! Es wäre bequemer gewesen, den Übergabepfad als zweiten Einstieg neben `run()` zu bauen. Dann
//! liefe er aber **ausschließlich** in dem Szenario, das noch niemand testet — und dieses Projekt
//! hat inzwischen mehrfach erlebt, was ein Pfad wert ist, den kein Testlauf betritt: die
//! DMAR-Gruppenbildung, der Ausschlusspfad für RMRR, der x2APIC-Zweig unter TCG. Jedes Mal war
//! die Antwort dieselbe: den Pfad so legen, dass der reguläre Lauf ihn abfährt.
//!
//! Deshalb baut **auch** der Multiboot-Weg eine [`HandoverInfo`] und geht durch dieselbe
//! Fortsetzung. Was der Übergabepfad später zusätzlich liefert, ist nur die Herkunft der Daten,
//! nicht ihr Format.

/// Erkennungswort — eine übergebene Struktur, die das nicht trägt, wird nicht benutzt.
pub const MAGIC: u64 = 0x4341_5052_4F43_4B53; // "CAPROCKS"
/// Format-Version. Wird erhöht, sobald sich das Layout ändert; das Modul prüft sie mit.
pub const VERSION: u32 = 1;

/// Obergrenzen. Bewusst klein und fest: die Struktur wird von einem fremden Prozess
/// (Linux-Modul) beschrieben, und eine variable Länge wäre eine Parselänge mehr, die stimmen
/// muss.
pub const MAX_REGIONS: usize = 64;
pub const MAX_CPUS: usize = 64;

/// Ein freier Speicherbereich, wie ihn der Wirt hergibt.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MemRegion {
    pub base: u64,
    pub len: u64,
}

/// Woher der Kernel gestartet wurde — nur für die Ausgabe, aber wichtig genug, um sie nicht zu
/// erraten.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BootSource {
    Multiboot = 0,
    Handover = 1,
}

/// Alles, was der Kernel beim Hochlauf von außen braucht.
#[repr(C)]
pub struct HandoverInfo {
    pub magic: u64,
    pub version: u32,
    pub source: BootSource,
    /// Freie Speicherbereiche. Beim Multiboot-Weg einer (bzw. die künstliche Zerstückelung des
    /// Testlaufs), bei der Übergabe so viele, wie der Wirt am Stück hergeben konnte.
    pub n_regions: u32,
    pub regions: [MemRegion; MAX_REGIONS],
    /// Höchste physische Adresse — **nicht** das Ende des letzten Bereichs. Daran hängt die Wahl
    /// des IOVA-Fensters: oberhalb davon kann eine IOVA nie eine gültige PA sein.
    pub ram_top: u64,
    /// APIC-IDs der Kerne, die dieser Kernel benutzen darf.
    pub n_cpus: u32,
    pub apic_ids: [u32; MAX_CPUS],
    /// Physische Adresse eines Ausgabe-Rings, `0` = serielle Schnittstelle benutzen.
    ///
    /// Bei der Übergabe gehört die serielle Schnittstelle dem Wirt; zwei Schreiber auf demselben
    /// UART ergeben ineinander verschachtelte Zeilen und im schlechteren Fall einen verwirrten
    /// Treiber. Der Ring ist die Alternative, die niemandem etwas wegnimmt.
    pub console_ring: u64,
}

impl HandoverInfo {
    pub const fn empty() -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            source: BootSource::Multiboot,
            n_regions: 0,
            regions: [MemRegion { base: 0, len: 0 }; MAX_REGIONS],
            ram_top: 0,
            n_cpus: 0,
            apic_ids: [0; MAX_CPUS],
            console_ring: 0,
        }
    }

    pub fn push_region(&mut self, base: u64, len: u64) -> bool {
        let n = self.n_regions as usize;
        if n >= MAX_REGIONS || len == 0 {
            return false;
        }
        self.regions[n] = MemRegion { base, len };
        self.n_regions += 1;
        true
    }

    /// Ist die Struktur plausibel? Wird auf dem Übergabeweg geprüft, **bevor** irgendetwas
    /// daraus benutzt wird — sie kommt dort aus einem fremden Adressraum.
    pub fn valid(&self) -> bool {
        self.magic == MAGIC
            && self.version == VERSION
            && (self.n_regions as usize) <= MAX_REGIONS
            && (self.n_cpus as usize) <= MAX_CPUS
            && self.n_regions > 0
            && self.ram_top != 0
            && self.regions[..self.n_regions as usize]
                .iter()
                .all(|r| r.len != 0 && r.base.checked_add(r.len).is_some_and(|e| e <= self.ram_top))
    }

    pub fn mem_regions(&self) -> &[MemRegion] {
        &self.regions[..self.n_regions as usize]
    }
}
