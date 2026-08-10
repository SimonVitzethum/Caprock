//! Memory-Capability — lineare Autorität über eine physische Region.

use crate::region::{PhysRegion, Rights};

/// Capability über einen physischen Speicherbereich.
///
/// **Linear / move-only** (kein `Clone`/`Copy`): Besitz des Wertes *ist* die
/// Capability. Transfer zwischen Eigentümern ist ein gewöhnlicher Rust-Move;
/// `split`/`restrict` leiten Kind-Caps ab; Rückgabe erfolgt über
/// [`PhysAllocator::free`](crate::PhysAllocator::free). Wird eine Cap fallen
/// gelassen, ohne sie zurückzugeben, „verliert“ man nur die Region (kein
/// Sicherheitsproblem) — daher `#[must_use]`.
#[derive(Debug)]
#[must_use = "eine MemoryCap repräsentiert Speicherbesitz; gib sie zurück oder leite sie ab"]
pub struct MemoryCap {
    region: PhysRegion,
    rights: Rights,
}

impl MemoryCap {
    /// Nur innerhalb der Crate prägbar (Allokator bzw. Ableitung).
    pub(crate) const fn new(region: PhysRegion, rights: Rights) -> Self {
        Self { region, rights }
    }

    pub const fn region(&self) -> PhysRegion {
        self.region
    }
    pub const fn base(&self) -> u64 {
        self.region.base
    }
    pub const fn len(&self) -> u64 {
        self.region.len
    }
    pub const fn rights(&self) -> Rights {
        self.rights
    }

    /// Capability ableiten: in zwei nicht-überlappende Kind-Caps zerlegen.
    ///
    /// Die erste erhält `first_len` Bytes, die zweite den Rest; beide erben die
    /// Rechte der Eltern-Cap. Bei ungültiger Länge (`0` oder `>= len`) wird die
    /// unveränderte Cap als `Err` zurückgegeben (sie ist linear, geht also nicht
    /// verloren).
    pub fn split(self, first_len: u64) -> Result<(MemoryCap, MemoryCap), MemoryCap> {
        if first_len == 0 || first_len >= self.region.len {
            return Err(self);
        }
        let base = self.region.base;
        let first = PhysRegion::new(base, first_len);
        let second = PhysRegion::new(base + first_len, self.region.len - first_len);
        Ok((
            MemoryCap::new(first, self.rights),
            MemoryCap::new(second, self.rights),
        ))
    }

    /// Rechte einschränken (mint-artig). Es können nur Rechte *entfernt* werden:
    /// das Ergebnis ist die Schnittmenge mit `rights`.
    pub fn restrict(self, rights: Rights) -> MemoryCap {
        MemoryCap::new(self.region, self.rights.intersect(rights))
    }
}
