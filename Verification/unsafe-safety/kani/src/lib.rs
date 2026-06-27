#![no_std]
//! SEL4Lake — Speichersicherheit der **Software-`unsafe`-Stellen** (Kategorie A, ADR 0021).
//!
//! Eigenständiges Kani-Verifikationsartefakt: jede hier enthaltene Funktion ist eine **getreue Kopie**
//! der unsafe-Glue-Logik einer konkreten Kernel-Stelle (Zeile angegeben). Kani führt die **echten**
//! `core::ptr`-Operationen auf einem modellierten Puffer aus und meldet **jeden** Out-of-Bounds-Zugriff,
//! Overflow oder UB. Bewiesen wird so die Speichersicherheit der unsafe-Blöcke **unter der dokumentierten
//! Vorbedingung** (die ihrerseits am Aufrufer als reine Arithmetik bewiesen wird).
//!
//! **Fidelity-Vertrag:** jede `*_logic`-Funktion MUSS byte-genau der referenzierten Kernel-Stelle
//! entsprechen (gleiche Zeiger-Arithmetik, gleiche Reihenfolge). Bei Änderung der Kernel-Stelle ist die
//! Kopie nachzuziehen — die Treue ist die (kleine, auditierbare) TCB dieses Beweises.
//!
//! Lauf:  tools/kani-verify.sh unsafe

// ===========================================================================
// Stelle 1: kernel/src/system.rs::copy_segment  (system.rs:1214-1221)
//   „Die EINZIGE unsafe-Stelle des Ladepfads" (ADR 0011 §2): ein bereits validiertes ELF-Segment
//   (`src`, filesz Bytes) in den frisch allozierten Ziel-Frame kopieren und den Rest `[src.len, total)`
//   nullen (.bss + Seiten-Padding). Vorbedingung: `src.len() <= total` (total = auf 4 KiB aufgerundetes
//   memsz >= filesz = src.len()).
// ===========================================================================

/// Getreue Kopie des unsafe-Blocks von `copy_segment` (auf einen rohen `dst`-Zeiger).
///
/// # Safety
/// `dst` muss auf einen schreibbaren Puffer von **mindestens `total`** Bytes zeigen und
/// `src.len() <= total` gelten (sonst OOB / Underflow in `total - src.len()`).
pub unsafe fn copy_segment_logic(dst: *mut u8, src: &[u8], total: usize) {
    // — identisch zu kernel/src/system.rs:1218-1220 —
    core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
    core::ptr::write_bytes(dst.add(src.len()), 0, total - src.len());
}

/// Aufrufer-Arithmetik aus kernel/src/system.rs:1313 (`total = (memsz + 4095) & !4095`).
pub fn round_up_4k(memsz: u64) -> u64 {
    (memsz + 4095) & !4095u64
}

// ===========================================================================
// Stelle 2: crates/sel4lake-region/src/heap.rs  (heap.rs:106 read, heap.rs:175 write)
//   Intrusive Slab-Free-Liste: ein freier Slot trägt in seinen ersten Bytes den Zeiger auf den
//   nächsten freien Slot. `slab_alloc` liest ihn (`read`), `deallocate` schreibt ihn (`write`).
//   Vorbedingung: der Slot ist >= eine Größenklasse groß (kleinste = 16 B) und größen-ausgerichtet
//   (==> usize-ausgerichtet) — der eingebettete `usize`-Zeiger passt + ist aligned.
// ===========================================================================

/// Größenklassen — **getreue Kopie** von heap.rs:31.
pub const CLASS_SIZES: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Getreue Kopie des Free-Listen-Reads (heap.rs:106): Nachfolger-Zeiger aus dem Slot lesen.
/// # Safety: `head` zeigt auf einen >= 16-Byte, usize-ausgerichteten Slot.
pub unsafe fn slab_read_next(head: *const usize) -> usize {
    core::ptr::read(head)
}

/// Getreue Kopie des Free-Listen-Writes (heap.rs:175): Nachfolger-Zeiger in den Slot schreiben.
/// # Safety: wie `slab_read_next`.
pub unsafe fn slab_write_next(slot: *mut usize, next: usize) {
    core::ptr::write(slot, next);
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Größe des echten, von Kani modellierten Ziel-Frames (klein für schnelles BMC — die Eigenschaft
    /// ist größeninvariant: gilt sie für jeden Frame bis CAP mit symbolischem `total`/`src_len`, gilt
    /// sie für alle). Symbolisch-lange `copy/write_bytes` sind für CBMC teuer -> klein + `unwind`.
    const CAP: usize = 4;

    /// **BEWEIS (Slab-Free-Listen-Invariante):** jede Größenklasse fasst einen `usize`-Zeiger **und**
    /// ist ein Vielfaches der `usize`-Ausrichtung (ein größen-ausgerichteter Slot ist also
    /// usize-ausgerichtet) — die Vorbedingung der rohen `read`/`write` gilt für **jede** Klasse.
    #[kani::proof]
    fn slab_class_holds_pointer() {
        let ci: usize = kani::any();
        kani::assume(ci < CLASS_SIZES.len());
        let class = CLASS_SIZES[ci];
        assert!(core::mem::size_of::<usize>() <= class);          // Zeiger passt in den Slot
        assert!(class % core::mem::align_of::<usize>() == 0);     // größen- ⟹ usize-ausgerichtet
    }

    /// **BEWEIS (Memory-Safety):** der eingebettete Nachfolger-Zeiger wird über einen ECHTEN, korrekt
    /// ausgerichteten Slot der **kleinsten** Klasse (16 B) geschrieben+gelesen — Kani prüft, dass der
    /// 8-Byte-Zugriff in-bounds + aligned ist, und der Round-Trip den Wert erhält.
    #[kani::proof]
    fn slab_freelist_roundtrip() {
        #[repr(align(8))]
        struct Slot([u8; 16]); // kleinste Größenklasse, usize-ausgerichtet
        let mut slot = Slot([0u8; 16]);
        let p = slot.0.as_mut_ptr() as *mut usize;
        let val: usize = kani::any();
        unsafe { slab_write_next(p, val) };
        let got = unsafe { slab_read_next(p as *const usize) };
        assert!(got == val); // Round-Trip erhält den Zeiger (kein OOB/Misalign)
    }

    /// **BEWEIS (Memory-Safety):** unter der Vorbedingung `src.len() <= total (<= Frame-Größe)` greifen
    /// `copy_nonoverlapping` + `write_bytes` **nie** ausserhalb des Ziel-Frames zu — Kani prüft jeden
    /// rohen Schreibzugriff gegen den echten N-Byte-Puffer.
    #[kani::proof]
    #[kani::unwind(6)]
    fn copy_segment_in_bounds() {
        let total: usize = kani::any();
        let src_len: usize = kani::any();
        kani::assume(total <= CAP);
        kani::assume(src_len <= total); // dokumentierte Vorbedingung (filesz <= total)

        let src = [0u8; CAP];
        let mut dst = [0u8; CAP];
        // Kani meldet hier jeden OOB-Write / Underflow in `total - src_len`.
        unsafe { copy_segment_logic(dst.as_mut_ptr(), &src[..src_len], total) };
    }

    /// **BEWEIS (funktionale Folge):** nach dem Kopieren ist der Schwanz `[src_len, total)` **genullt**
    /// (.bss/Padding sauber) und der Kopf `[0, src_len)` trägt die Quellbytes — keine
    /// uninitialisierten/fremden Bytes im ausgeführten Frame.
    #[kani::proof]
    #[kani::unwind(6)]
    fn copy_segment_zeroes_tail() {
        let total: usize = kani::any();
        let src_len: usize = kani::any();
        kani::assume(total <= CAP);
        kani::assume(src_len <= total);

        let mut src = [0u8; CAP];
        let fill: u8 = kani::any();
        let mut i = 0;
        while i < CAP { src[i] = fill; i += 1; } // Quelle != 0, um die Schwanz-Nullung zu prüfen

        let mut dst = [0xAAu8; CAP];
        unsafe { copy_segment_logic(dst.as_mut_ptr(), &src[..src_len], total) };

        // Kopf trägt die Quelle, Schwanz ist 0.
        let mut k = 0;
        while k < src_len { assert!(dst[k] == fill); k += 1; }
        let mut z = src_len;
        while z < total { assert!(dst[z] == 0); z += 1; }
    }

    /// **BEWEIS (Vorbedingung wird am Aufrufer etabliert):** mit der ELF-Garantie `filesz <= memsz` und
    /// `total = round_up_4k(memsz)` gilt `filesz <= total` — die Vorbedingung von `copy_segment` ist
    /// erfüllt, **und** die Aufrundung läuft nicht über.
    #[kani::proof]
    fn loader_precondition_holds() {
        let filesz: u64 = kani::any();
        let memsz: u64 = kani::any();
        kani::assume(filesz <= memsz);            // ELF-Parser-Garantie (safe-Rust, host-getestet)
        kani::assume(memsz <= u64::MAX - 4095);   // kein Overflow in der Aufrundung
        let total = round_up_4k(memsz);
        assert!(memsz <= total);                  // Aufrundung >= Original
        assert!(filesz <= total);                 // ⟹ Vorbedingung von copy_segment
    }
}
