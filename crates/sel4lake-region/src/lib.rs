#![no_std]
#![feature(allocator_api)]
//! **Runtime-Speicherabstraktion** (ext-25) für den Single-Address-Space.
//!
//! Im SAS gibt es keine Adressübersetzung — eine [`sel4lake_mem::MemoryCap`] beschreibt einen
//! realen physischen Bereich. Damit safe-Rust-Prozesse darauf **dynamisch** (Heap, DMA-Puffer,
//! Hot-Reload-Zustand) arbeiten können, ohne überall nackte `&mut [u8]` zu reichen, kapselt
//! **diese und nur diese Crate** allen `unsafe`-Speicherzugriff hinter zwei Typen:
//!
//! - [`Region`] — die **cap-besessene** Einheit (ein `MemoryCap` + Audit-Metadaten). Ein Prozess
//!   hält eine **Menge** davon (Regionsliste), nie „einen Heap".
//! - [`RegionView`] — eine geliehene, **begrenzte** Sicht in eine Region (oder einen Teilbereich).
//!   Bietet ausschließlich **sichere** Operationen: typisierte kopierende Zugriffe (`get`/`set`/
//!   `copy_*`/`fill`) und einen **scoped** `with_bytes(closure)`, dessen `&mut [u8]` die Closure
//!   nicht verlassen kann. `split_at`/`subview` sind die Operationen, auf denen der Allokator
//!   arbeitet (s. [`heap`]).
//!
//! Die öffentliche API ist vollständig Safe Rust; die wenigen `unsafe`-Blöcke liegen
//! ausschließlich in den `RegionView`-Accessoren + der Allokator-Glue ([`heap`]) und sind durch
//! die Cap (beweist Besitz + Bounds + Rechte) + Bounds-Checks begründet.

extern crate alloc;

pub mod heap;

use core::marker::PhantomData;
use sel4lake_mem::MemoryCap;

/// Zweck einer Region (Audit/Debug; ermöglicht später getrennte Behandlung von Heap vs. DMA vs.
/// Hot-Reload-Zustand, ohne die Typen zu vermischen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Allgemeiner Heap (Größenklassen-Slabs).
    Heap,
    /// Große/dedizierte Allokation (Bump) im Heap.
    Large,
    /// DMA-Puffer (kontiguierlich, geräte-sichtbar).
    Dma,
    /// Hot-Reload-Zustand (über einen Reload hinweg per Cap übergebbar).
    HotReloadState,
    /// IPC-geteilte Region.
    Shared,
    /// Sonstiges.
    Other,
}

/// Audit-/Debug-Metadaten einer Region (ohne nackte Adressen nach außen).
#[derive(Clone, Copy, Debug)]
pub struct RegionTag {
    pub id: u32,
    pub purpose: Purpose,
}

impl RegionTag {
    pub const fn new(id: u32, purpose: Purpose) -> Self {
        Self { id, purpose }
    }
}

/// Eine **cap-besessene** RAM-Region. Besitzt einen [`MemoryCap`] (linear) — der einzige Weg zu
/// typisiertem Zugriff führt über [`Region::view`] -> [`RegionView`]. Wird die Region aufgelöst
/// ([`Region::into_cap`]), kann der Aufrufer die Cap an den Kernel zurückgeben (Reclaim).
pub struct Region {
    base: u64,
    len: usize,
    cap: MemoryCap,
    tag: RegionTag,
}

impl Region {
    /// Eine Region aus einem [`MemoryCap`] bilden (Runtime-intern). Die Cap *beweist* Besitz,
    /// Bounds und Rechte des Bereichs `[base, base+len)`.
    pub fn from_cap(cap: MemoryCap, tag: RegionTag) -> Self {
        Self {
            base: cap.base(),
            len: cap.len() as usize,
            cap,
            tag,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Die **physische** Basisadresse (geräte-sichtbar; für DMA-Bindung). Reine Zahl, kein Zugriff.
    pub fn phys(&self) -> u64 {
        self.base
    }
    pub fn tag(&self) -> RegionTag {
        self.tag
    }
    pub fn set_tag(&mut self, tag: RegionTag) {
        self.tag = tag;
    }

    /// Die **ganze** Region als (exklusive) Sicht leihen.
    pub fn view(&mut self) -> RegionView<'_> {
        RegionView {
            base: self.base,
            len: self.len,
            _p: PhantomData,
        }
    }

    /// Die Region auflösen und den zugrundeliegenden [`MemoryCap`] zurückgeben (z.B. um die
    /// Region an den Kernel zurückzugeben oder an einen anderen Prozess zu übertragen).
    pub fn into_cap(self) -> MemoryCap {
        self.cap
    }
}

/// Eine geliehene, **begrenzte** Sicht in eine Region (oder einen Teilbereich). Bietet nur
/// **sichere** Operationen; das `&'a mut [u8]` ist nur über [`RegionView::with_bytes`] **scoped**
/// erreichbar und kann die Closure nicht verlassen. Die wenigen `unsafe`-Blöcke hier sind durch
/// die invariante Gültigkeit (`base`/`len` stammen aus einer cap-validierten [`Region`]) + die
/// Bounds-Checks begründet.
pub struct RegionView<'a> {
    base: u64,
    len: usize,
    _p: PhantomData<&'a mut [u8]>,
}

impl<'a> RegionView<'a> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Die physische Adresse des Sichtbeginns (geräte-sichtbar, für DMA). Kein Speicherzugriff.
    pub fn phys_addr(&self) -> u64 {
        self.base
    }

    /// Die Sicht bei `mid` in zwei nicht-überlappende Sichten teilen (Allokator-Primitive).
    /// `None`, wenn `mid > len`.
    pub fn split_at(self, mid: usize) -> Option<(RegionView<'a>, RegionView<'a>)> {
        if mid > self.len {
            return None;
        }
        Some((
            RegionView {
                base: self.base,
                len: mid,
                _p: PhantomData,
            },
            RegionView {
                base: self.base + mid as u64,
                len: self.len - mid,
                _p: PhantomData,
            },
        ))
    }

    /// Einen Teilbereich `[off, off+len)` als (kürzer geliehene) Sicht leihen. `None` bei Out-of-Bounds.
    pub fn subview(&mut self, off: usize, len: usize) -> Option<RegionView<'_>> {
        if off.checked_add(len)? > self.len {
            return None;
        }
        Some(RegionView {
            base: self.base + off as u64,
            len,
            _p: PhantomData,
        })
    }

    /// Einen `T: Pod` an Offset `off` lesen (kopierend, bounds-gecheckt). `None` bei Out-of-Bounds.
    pub fn get<T: Pod>(&self, off: usize) -> Option<T> {
        if off.checked_add(core::mem::size_of::<T>())? > self.len {
            return None;
        }
        // SAFETY: `[off, off+size_of::<T>())` liegt in der cap-validierten Region; `T: Pod` (jedes
        // Bitmuster gültig); unaligned-Read deckt beliebige Offsets ab.
        Some(unsafe { core::ptr::read_unaligned((self.base + off as u64) as *const T) })
    }

    /// Einen `T: Pod` an Offset `off` schreiben (bounds-gecheckt). `false` bei Out-of-Bounds.
    pub fn set<T: Pod>(&mut self, off: usize, val: T) -> bool {
        let Some(end) = off.checked_add(core::mem::size_of::<T>()) else {
            return false;
        };
        if end > self.len {
            return false;
        }
        // SAFETY: Ziel `[off, off+size_of::<T>())` liegt in der cap-validierten, exklusiv
        // geliehenen Region; unaligned-Write.
        unsafe { core::ptr::write_unaligned((self.base + off as u64) as *mut T, val) };
        true
    }

    /// `src` an Offset `off` hineinkopieren. `false` bei Out-of-Bounds.
    pub fn copy_from(&mut self, off: usize, src: &[u8]) -> bool {
        let Some(end) = off.checked_add(src.len()) else {
            return false;
        };
        if end > self.len {
            return false;
        }
        // SAFETY: Ziel liegt in der exklusiv geliehenen Region; `src` ist ein gültiger Slice;
        // die Bereiche überlappen nicht (verschiedene Allokationen).
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), (self.base + off as u64) as *mut u8, src.len());
        }
        true
    }

    /// Ab Offset `off` in `dst` herauskopieren. `false` bei Out-of-Bounds.
    pub fn copy_to(&self, off: usize, dst: &mut [u8]) -> bool {
        let Some(end) = off.checked_add(dst.len()) else {
            return false;
        };
        if end > self.len {
            return false;
        }
        // SAFETY: Quelle liegt in der cap-validierten Region; `dst` gültiger Slice; disjunkt.
        unsafe {
            core::ptr::copy_nonoverlapping((self.base + off as u64) as *const u8, dst.as_mut_ptr(), dst.len());
        }
        true
    }

    /// Die ganze Sicht mit `byte` füllen.
    pub fn fill(&mut self, byte: u8) {
        // SAFETY: exklusiv geliehene, cap-validierte Region der Länge `len`.
        unsafe { core::ptr::write_bytes(self.base as *mut u8, byte, self.len) };
    }

    /// **Scoped-Slice-Zugriff**: leiht der Closure einen `&mut [u8]` über den gesamten View —
    /// dieser kann die Closure **nicht verlassen** (an die Aufruf-Lebensdauer gebunden). So bleibt
    /// in-place-Parsen/memcpy/DMA-Füllen ergonomisch, ohne dass ein nackter Slice nach außen leckt.
    pub fn with_bytes<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        // SAFETY: `[base, base+len)` ist exklusiv geliehen (`&mut self`) und cap-validiert; der
        // erzeugte Slice lebt nur für die Dauer des Closure-Aufrufs.
        let s = unsafe { core::slice::from_raw_parts_mut(self.base as *mut u8, self.len) };
        f(s)
    }

    // --- Runtime-intern (crate-privat): rohe Teile für den Allokator (heap-Modul). ---
    pub(crate) fn raw(&self) -> (u64, usize) {
        (self.base, self.len)
    }
}

/// **Plain-Old-Data**-Markierung: jedes Bitmuster ist ein gültiger Wert (sicher per Kopie les-/
/// schreibbar). `unsafe`, weil es ein Versprechen über das Typ-Layout ist; nur für primitive
/// Werte + deren Arrays implementiert.
///
/// # Safety
/// Implementierende Typen müssen `Copy` sein und für jedes Bitmuster ein gültiges Objekt ergeben
/// (keine Nischen, keine Invarianten, keine Padding-Bedeutung).
pub unsafe trait Pod: Copy {}

macro_rules! impl_pod {
    ($($t:ty),*) => { $( unsafe impl Pod for $t {} )* };
}
impl_pod!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);
// SAFETY: Ein Array von Pod ist Pod (gleiches Argument elementweise, kein Padding zwischen `u8`).
unsafe impl<const N: usize> Pod for [u8; N] {}
