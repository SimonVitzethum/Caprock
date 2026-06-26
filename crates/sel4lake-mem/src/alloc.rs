//! Capability-basierter physischer Allokator.
//!
//! Verwaltet freies RAM als sortierte, koaleszierende Freiliste (feste Kapazität,
//! daher allokationsfrei und deterministisch) und prägt Wurzel-[`MemoryCap`]s.
//! `alloc` carvt eine seitenausgerichtete Region (First-Fit nach Adresse);
//! `free` gibt sie zurück und verschmilzt benachbarte Bereiche.

use crate::cap::MemoryCap;
use crate::region::{PhysRegion, Rights};
use crate::PAGE;

/// Maximale Anzahl freier Fragmente (genügt für statisches Bring-up; wächst mit
/// einem späteren dynamischen Backing-Store).
const MAX_FRAGMENTS: usize = 64;

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

        for i in 0..self.len {
            let r = self.regions[i];
            let start = align_up(r.base, align);
            let end = start.checked_add(size)?;
            if start >= r.base && end <= r.end() {
                self.remove(i);
                self.insert(PhysRegion::new(r.base, start - r.base)); // Präfix (evtl. leer)
                self.insert(PhysRegion::new(end, r.end() - end)); //       Suffix (evtl. leer)
                return Some(MemoryCap::new(PhysRegion::new(start, size), Rights::RW));
            }
        }
        None
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
