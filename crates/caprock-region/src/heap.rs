//! **Hybrid-Prozess-Heap** (ext-25, R1+R2): Größenklassen-Slabs + Bump-Arenen über einer
//! Regionsliste. Implementiert [`core::alloc::Allocator`] und arbeitet ausschließlich auf
//! [`RegionView`](crate::RegionView)s. Alles `unsafe` (rohe Zeiger, intrusive Free-Listen) liegt
//! hier; nach außen ist nur Safe Rust sichtbar (`Vec::new_in(&heap)`, `Box::new_in(x, &heap)`).
//!
//! - **Klein** (≤ größte Klasse): segregierte Größenklassen. Je Klasse eine **intrusive** Free-
//!   Liste freigegebener Slots; neue Slots werden aus einer **Bump-Region** geschnitten (beschränkte
//!   Fragmentierung, O(1)). Ist die Bump-Region voll, wird über die [`RegionSource`] eine weitere
//!   angefordert (grow).
//! - **Groß** (> größte Klasse): eine **dedizierte** Region je Allokation (kontiguierlich, DMA-/
//!   Hot-Reload-tauglich). Bei `dealloc` sofort an die Source zurückgegeben (shrink).

use crate::{Purpose, Region, RegionTag};
use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;
use caprock_sync::SpinLock;

/// Quelle für Regionen (grow/shrink). Der Allokator fordert hierüber neue Regionen an bzw. gibt
/// ungenutzte zurück. Kernel-seitig vom physischen Allokator bedient; ein EL0-Prozess würde es per
/// Syscall marshallen — die Schnittstelle bleibt identisch.
pub trait RegionSource {
    /// Eine Region mit **mindestens** `min_len` Bytes anfordern (page-ausgerichtet). `None` bei
    /// Erschöpfung.
    fn request(&self, min_len: usize, purpose: Purpose) -> Option<Region>;
    /// Eine (ungenutzte) Region an den Kernel zurückgeben.
    fn release(&self, region: Region);
}

/// Größenklassen (Bytes, Zweierpotenzen). Allokationen werden auf die kleinste passende Klasse
/// aufgerundet; alles Größere geht den Large-Pfad.
const CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];
const NUM_CLASSES: usize = CLASS_SIZES.len();
#[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
const MAX_CLASS: usize = CLASS_SIZES[NUM_CLASSES - 1];

const MAX_SLAB_REGIONS: usize = 12; // Bump-Regionen für Slabs
const MAX_LARGE: usize = 8; //         gleichzeitige Large-Allokationen
/// Default-Größe einer neu angeforderten Bump-Region (Slabs).
const SLAB_REGION_BYTES: usize = 64 * 1024;

/// Eine Bump-Region für Slab-Carving.
struct SlabRegion {
    region: Region,
    bump: usize, // bereits geschnittene Bytes
}

struct HeapInner {
    slabs: [Option<SlabRegion>; MAX_SLAB_REGIONS],
    large: [Option<Region>; MAX_LARGE],
    /// Kopf der intrusiven Free-Liste je Größenklasse (Adresse des nächsten freien Slots; 0 = leer).
    free_heads: [usize; NUM_CLASSES],
}

/// Prozess-Heap. Hält eine **Menge** von Regionen (Regionsliste) über die [`RegionSource`].
pub struct Heap<S: RegionSource> {
    source: S,
    inner: SpinLock<HeapInner>,
}

const NONE_SLAB: Option<SlabRegion> = None;
const NONE_REGION: Option<Region> = None;

/// Kleinste Größenklasse für `effective` Bytes; `None`, wenn > größte Klasse (Large-Pfad).
fn class_for(effective: usize) -> Option<usize> {
    CLASS_SIZES.iter().position(|&c| c >= effective)
}

#[inline]
fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

impl<S: RegionSource> Heap<S> {
    /// Einen leeren Heap anlegen; Regionen werden **lazy** bei der ersten Allokation angefordert.
    pub const fn new(source: S) -> Self {
        Self {
            source,
            inner: SpinLock::new(HeapInner {
                slabs: [NONE_SLAB; MAX_SLAB_REGIONS],
                large: [NONE_REGION; MAX_LARGE],
                free_heads: [0; NUM_CLASSES],
            }),
        }
    }

    /// Belegte Bytes grob (Slab-Bump-Summe + Large) — Telemetrie/Audit für Tests.
    pub fn allocated_bytes(&self) -> usize {
        let g = self.inner.lock();
        let slab: usize = g.slabs.iter().flatten().map(|s| s.bump).sum();
        let large: usize = g.large.iter().flatten().map(|r| r.len()).sum();
        slab + large
    }
    /// Anzahl gehaltener Regionen (Slab + Large) — Telemetrie.
    pub fn region_count(&self) -> usize {
        let g = self.inner.lock();
        g.slabs.iter().flatten().count() + g.large.iter().flatten().count()
    }

    /// Einen Slot der Klasse `ci` beschaffen (Free-Liste oder Bump; ggf. neue Region anfordern).
    fn slab_alloc(&self, g: &mut HeapInner, ci: usize) -> Option<usize> {
        let size = CLASS_SIZES[ci];
        // 1. Freigegebenen Slot wiederverwenden.
        let head = g.free_heads[ci];
        if head != 0 {
            // SAFETY: `head` ist die Adresse eines zuvor freigegebenen Slots dieser Klasse in einer
            // gehaltenen Region; die ersten 8 Bytes tragen den Nachfolger-Zeiger.
            let next = unsafe { core::ptr::read(head as *const usize) };
            g.free_heads[ci] = next;
            return Some(head);
        }
        // 2. Aus einer Bump-Region schneiden (size-ausgerichtet).
        for slot in g.slabs.iter_mut().flatten() {
            let off = align_up(slot.bump, size);
            if off + size <= slot.region.len() {
                slot.bump = off + size;
                return Some(slot.region.phys() as usize + off);
            }
        }
        // 3. Keine Bump-Region hat Platz -> eine neue anfordern.
        let region = self.source.request(SLAB_REGION_BYTES.max(size), Purpose::Heap)?;
        let free = g.slabs.iter().position(|s| s.is_none())?;
        let base = region.phys() as usize;
        g.slabs[free] = Some(SlabRegion { region, bump: size });
        Some(base) // erster Slot bei Offset 0 (page-aligned >= size)
    }

    /// Eine dedizierte Large-Region anfordern; Basis = Allokationsadresse.
    fn large_alloc(&self, g: &mut HeapInner, size: usize, align: usize) -> Option<usize> {
        let region = self.source.request(size.max(align), Purpose::Large)?;
        let base = region.phys() as usize;
        let slot = g.large.iter().position(|r| r.is_none())?;
        g.large[slot] = Some(region);
        Some(base)
    }
}

unsafe impl<S: RegionSource> Allocator for Heap<S> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let size = layout.size();
        let align = layout.align();
        if size == 0 {
            // ZST: gültiger, ausgerichteter, nicht-dereferenzierbarer Zeiger.
            let p = NonNull::new(align as *mut u8).ok_or(AllocError)?;
            return Ok(NonNull::slice_from_raw_parts(p, 0));
        }
        let effective = size.max(align);
        let mut g = self.inner.lock();
        let addr = match class_for(effective) {
            Some(ci) => self.slab_alloc(&mut g, ci),
            None => self.large_alloc(&mut g, size, align),
        }
        .ok_or(AllocError)?;
        let actual = match class_for(effective) {
            Some(ci) => CLASS_SIZES[ci],
            None => size,
        };
        let p = NonNull::new(addr as *mut u8).ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(p, actual))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let size = layout.size();
        if size == 0 {
            return;
        }
        let align = layout.align();
        let effective = size.max(align);
        let addr = ptr.as_ptr() as usize;
        let mut g = self.inner.lock();
        match class_for(effective) {
            Some(ci) => {
                // Slot in die Free-Liste der Klasse einhängen (Nachfolger im Slot speichern).
                // SAFETY: `addr` ist ein zuvor von dieser Klasse vergebener Slot (>= 16 Bytes,
                // trägt den Zeiger); er gehört zu einer gehaltenen Region.
                let old = g.free_heads[ci];
                core::ptr::write(addr as *mut usize, old);
                g.free_heads[ci] = addr;
            }
            None => {
                // Large: zugehörige Region finden + an die Source zurückgeben (shrink).
                if let Some(slot) = g
                    .large
                    .iter()
                    .position(|r| r.as_ref().is_some_and(|rg| rg.phys() as usize == addr))
                {
                    if let Some(region) = g.large[slot].take() {
                        drop(g); // Lock vor dem Source-Call freigeben
                        self.source.release(region);
                    }
                }
            }
        }
    }
}

impl<S: RegionSource> Drop for Heap<S> {
    fn drop(&mut self) {
        let mut g = self.inner.lock();
        for s in g.slabs.iter_mut() {
            if let Some(sr) = s.take() {
                self.source.release(sr.region);
            }
        }
        for r in g.large.iter_mut() {
            if let Some(rg) = r.take() {
                self.source.release(rg);
            }
        }
    }
}

/// Hilfs-Tag für eine vom Allokator angeforderte Heap-Region (Audit).
pub fn heap_region_tag(id: u32) -> RegionTag {
    RegionTag::new(id, Purpose::Heap)
}
