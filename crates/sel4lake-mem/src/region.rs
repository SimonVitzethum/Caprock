//! Physische Region und Zugriffsrechte.

/// Ein zusammenhängender physischer Speicherbereich `[base, base+len)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PhysRegion {
    pub base: u64,
    pub len: u64,
}

impl PhysRegion {
    pub const fn new(base: u64, len: u64) -> Self {
        Self { base, len }
    }

    /// Erste Adresse hinter der Region.
    pub const fn end(&self) -> u64 {
        self.base + self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Enthält die Region die Adresse `addr`?
    pub const fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }

    /// Liegt `other` vollständig in dieser Region?
    pub const fn contains_region(&self, other: &PhysRegion) -> bool {
        other.base >= self.base && other.end() <= self.end()
    }
}

/// Zugriffsrechte einer Capability (Lesen / Schreiben / Ausführen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rights(u8);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const READ: Rights = Rights(1);
    pub const WRITE: Rights = Rights(2);
    pub const EXEC: Rights = Rights(4);
    /// Lesen + Schreiben (Standard für Datenspeicher).
    pub const RW: Rights = Rights(0b011);
    /// Lesen + Schreiben + Ausführen.
    pub const RWX: Rights = Rights(0b111);

    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Enthält dieses Rechteset alle Rechte aus `other`?
    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    /// Schnittmenge der Rechte (zum Einschränken/`restrict`).
    pub const fn intersect(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }

    /// Vereinigung der Rechte.
    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }
}
