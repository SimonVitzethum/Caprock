//! Minimal-ELF64-Parser (ext-26, L1) — **nur** das für statisch gelinkte `ET_EXEC`-Rust-Binaries
//! Nötige: ELF64-Header validieren + `PT_LOAD`-Segmente liefern. **Kein** Dynamic-Linking, **keine**
//! Relokationen, **kein** Symbol-/Section-Parsing (ADR 0011 §2).
//!
//! Vollständig in Safe Rust (`#![forbid(unsafe_code)]` auf Crate-Ebene): jede Header-/Offset-/
//! Größenprüfung passiert hier, bounds-geprüft + panik-frei. Das `unsafe` (validierte Segmente in
//! den Zielspeicher kopieren) liegt erst im Kernel-Glue (`kernel/src/loader.rs`).

use crate::LoaderError;

// --- ELF64-Konstanten (nur die benötigten) ---
const EI_NIDENT: usize = 16;
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2; // e_ident[EI_CLASS]
const ELFDATA2LSB: u8 = 1; // e_ident[EI_DATA] (little-endian)
const ET_EXEC: u16 = 2; // e_type
/// `e_machine` = AArch64.
pub const EM_AARCH64: u16 = 0xB7;
/// `e_machine` = x86-64.
pub const EM_X86_64: u16 = 0x3E;

/// Die **einzige** hier ladbare Maschine — die des laufenden Kernels.
///
/// Das ist kein Formalismus: ein ELF für die andere Architektur würde sonst geparst, seine
/// Segmente kopiert und gemappt, und der Thread stürbe erst beim ersten Befehl an einer Stelle,
/// die mit dem Loader nichts mehr zu tun hat. Die Ablehnung gehört an die Kante.
///
/// Bewusst über `cfg(target_arch)` und nicht als Parameter: diese Crate wird in den Kernel
/// kompiliert, und dessen Architektur ist zur Übersetzungszeit bekannt. Ein Laufzeitparameter
/// wäre eine Entscheidung, die jemand falsch treffen kann.
#[cfg(target_arch = "aarch64")]
pub const EXPECTED_MACHINE: u16 = EM_AARCH64;
#[cfg(target_arch = "x86_64")]
pub const EXPECTED_MACHINE: u16 = EM_X86_64;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("sel4lake-loader kennt nur aarch64 und x86_64 als Ziel-/Hostarchitektur");

const PT_LOAD: u32 = 1; // p_type
const EHDR_LEN: usize = 64; // ELF64-Header
const PHDR_LEN: usize = 56; // ELF64-Program-Header

/// Segment-Rechte (`p_flags`): bit 0 = X, bit 1 = W, bit 2 = R (ELF-Standard).
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

/// Ein zu ladendes `PT_LOAD`-Segment (bereits gegen den Image-Puffer bounds-validiert).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Ziel-virtuelle Adresse (in der VSpace des Prozesses; bei TrustedSAS == phys).
    pub vaddr: u64,
    /// Byte-Offset des Segment-Inhalts im ELF-Image.
    pub offset: usize,
    /// Im Image vorhandene Bytes (zu kopieren).
    pub filesz: usize,
    /// Größe im Speicher (>= `filesz`; Differenz ist `.bss` und wird genullt).
    pub memsz: usize,
    /// Rechte (`PF_R`/`PF_W`/`PF_X`) → der Loader mappt W^X (X→RX, W→RW, sonst RO).
    pub flags: u32,
}

fn rd_u16(d: &[u8], off: usize) -> Option<u16> {
    let b = d.get(off..off + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}
fn rd_u64(d: &[u8], off: usize) -> Option<u64> {
    let b = d.get(off..off + 8)?;
    Some(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// Ein validiertes ELF64-Image: Entry-Point + Program-Header-Tabelle (für die `PT_LOAD`-Iteration).
#[derive(Clone, Copy, Debug)]
pub struct ElfImage<'a> {
    data: &'a [u8],
    entry: u64,
    phoff: usize,
    phnum: usize,
}

impl<'a> ElfImage<'a> {
    /// Den ELF64-Header validieren (Magic, 64-bit, LE, `ET_EXEC`, AArch64) + die
    /// Program-Header-Tabelle bounds-prüfen. **Kopiert nichts** — reine Validierung.
    pub fn parse(data: &'a [u8]) -> Result<ElfImage<'a>, LoaderError> {
        if data.len() < EHDR_LEN {
            return Err(LoaderError::TooSmall);
        }
        let ident = &data[..EI_NIDENT];
        if ident[0..4] != ELF_MAGIC {
            return Err(LoaderError::BadElf);
        }
        if ident[4] != ELFCLASS64 || ident[5] != ELFDATA2LSB {
            return Err(LoaderError::BadElf);
        }
        let e_type = rd_u16(data, 16).ok_or(LoaderError::TooSmall)?;
        let e_machine = rd_u16(data, 18).ok_or(LoaderError::TooSmall)?;
        if e_type != ET_EXEC || e_machine != EXPECTED_MACHINE {
            return Err(LoaderError::BadElf);
        }
        let entry = rd_u64(data, 24).ok_or(LoaderError::TooSmall)?;
        let phoff = rd_u64(data, 32).ok_or(LoaderError::TooSmall)? as usize;
        let phentsize = rd_u16(data, 54).ok_or(LoaderError::TooSmall)? as usize;
        let phnum = rd_u16(data, 56).ok_or(LoaderError::TooSmall)? as usize;
        if phentsize != PHDR_LEN {
            return Err(LoaderError::BadElf);
        }
        // Program-Header-Tabelle muss vollständig im Puffer liegen (Overflow-sicher).
        let table_end = phnum
            .checked_mul(PHDR_LEN)
            .and_then(|t| t.checked_add(phoff))
            .ok_or(LoaderError::OutOfBounds)?;
        if table_end > data.len() {
            return Err(LoaderError::OutOfBounds);
        }
        let img = ElfImage { data, entry, phoff, phnum };
        // Alle PT_LOAD-Segmente eifrig validieren (Bounds + memsz>=filesz), damit die spätere
        // Iteration garantiert gültige Segmente liefert.
        for i in 0..phnum {
            img.parse_phdr(i)?;
        }
        Ok(img)
    }

    /// Entry-Point (virtuelle Adresse).
    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// Ein Program-Header validieren; gibt `Some(Segment)` nur für `PT_LOAD`, sonst `None`
    /// (innerhalb von `Ok`). `Err` bei Out-of-Bounds / `memsz < filesz`.
    fn parse_phdr(&self, i: usize) -> Result<Option<Segment>, LoaderError> {
        let base = self.phoff + i * PHDR_LEN;
        let p = self.data.get(base..base + PHDR_LEN).ok_or(LoaderError::OutOfBounds)?;
        let p_type = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        if p_type != PT_LOAD {
            return Ok(None);
        }
        let flags = u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
        let offset = rd_u64(p, 8).ok_or(LoaderError::OutOfBounds)? as usize;
        let vaddr = rd_u64(p, 16).ok_or(LoaderError::OutOfBounds)?;
        let filesz = rd_u64(p, 32).ok_or(LoaderError::OutOfBounds)? as usize;
        let memsz = rd_u64(p, 40).ok_or(LoaderError::OutOfBounds)? as usize;
        if memsz < filesz {
            return Err(LoaderError::BadElf);
        }
        // Der Segment-Inhalt `[offset, offset+filesz)` muss im Image liegen (Overflow-sicher).
        let end = offset.checked_add(filesz).ok_or(LoaderError::OutOfBounds)?;
        if end > self.data.len() {
            return Err(LoaderError::OutOfBounds);
        }
        Ok(Some(Segment { vaddr, offset, filesz, memsz, flags }))
    }

    /// Über alle `PT_LOAD`-Segmente iterieren (bereits bei `parse` validiert).
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        (0..self.phnum).filter_map(move |i| self.parse_phdr(i).ok().flatten())
    }

    /// Die `filesz`-Bytes eines Segments aus dem Image (gefahrlos, da bounds-validiert).
    pub fn segment_bytes(&self, seg: &Segment) -> &'a [u8] {
        // Bei `parse` validiert; im Fehlerfall leerer Slice (statt Panik).
        self.data.get(seg.offset..seg.offset + seg.filesz).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein minimales gültiges ELF64-`ET_EXEC` (AArch64) mit `segs` = (vaddr, flags, data, bss_extra).
    fn build_elf(entry: u64, segs: &[(u64, u32, &[u8], usize)]) -> Vec<u8> {
        let phnum = segs.len();
        let phoff = EHDR_LEN;
        let table_end = phoff + phnum * PHDR_LEN;
        // Segment-Inhalte hinter der Program-Header-Tabelle anordnen.
        let mut payload = Vec::new();
        let mut offsets = Vec::new();
        for (_, _, data, _) in segs {
            offsets.push(table_end + payload.len());
            payload.extend_from_slice(data);
        }
        let mut v = vec![0u8; table_end];
        // ELF-Header
        v[0..4].copy_from_slice(&ELF_MAGIC);
        v[4] = ELFCLASS64;
        v[5] = ELFDATA2LSB;
        v[6] = 1; // EI_VERSION
        v[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        v[18..20].copy_from_slice(&EXPECTED_MACHINE.to_le_bytes());
        v[24..32].copy_from_slice(&entry.to_le_bytes()); // e_entry
        v[32..40].copy_from_slice(&(phoff as u64).to_le_bytes()); // e_phoff
        v[54..56].copy_from_slice(&(PHDR_LEN as u16).to_le_bytes()); // e_phentsize
        v[56..58].copy_from_slice(&(phnum as u16).to_le_bytes()); // e_phnum
        // Program-Header
        for (i, ((vaddr, flags, data, bss), off)) in segs.iter().zip(&offsets).enumerate() {
            let base = phoff + i * PHDR_LEN;
            v[base..base + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
            v[base + 4..base + 8].copy_from_slice(&flags.to_le_bytes());
            v[base + 8..base + 16].copy_from_slice(&(*off as u64).to_le_bytes()); // p_offset
            v[base + 16..base + 24].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
            v[base + 24..base + 32].copy_from_slice(&vaddr.to_le_bytes()); // p_paddr
            v[base + 32..base + 40].copy_from_slice(&(data.len() as u64).to_le_bytes()); // p_filesz
            v[base + 40..base + 48].copy_from_slice(&((data.len() + bss) as u64).to_le_bytes()); // p_memsz
            v[base + 48..base + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
        }
        v.extend_from_slice(&payload);
        v
    }

    #[test]
    fn two_segments_roundtrip() {
        let raw = build_elf(
            0x10_0000,
            &[
                (0x10_0000, PF_R | PF_X, b"\x00\x01\x02\x03code", 0),
                (0x11_0000, PF_R | PF_W, b"data", 0x100), // 0x100 Bytes .bss
            ],
        );
        let e = ElfImage::parse(&raw).unwrap();
        assert_eq!(e.entry(), 0x10_0000);
        let segs: Vec<_> = e.segments().collect();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].vaddr, 0x10_0000);
        assert_eq!(segs[0].flags, PF_R | PF_X);
        assert_eq!(segs[0].filesz, 8);
        assert_eq!(segs[0].memsz, 8);
        assert_eq!(e.segment_bytes(&segs[0]), b"\x00\x01\x02\x03code");
        assert_eq!(segs[1].vaddr, 0x11_0000);
        assert_eq!(segs[1].flags, PF_R | PF_W);
        assert_eq!(segs[1].filesz, 4);
        assert_eq!(segs[1].memsz, 4 + 0x100);
    }

    #[test]
    fn non_load_segment_skipped() {
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R | PF_X, b"abcd", 0)]);
        // p_type des einzigen Headers auf etwas != PT_LOAD setzen (z. B. PT_NOTE=4).
        raw[EHDR_LEN..EHDR_LEN + 4].copy_from_slice(&4u32.to_le_bytes());
        let e = ElfImage::parse(&raw).unwrap();
        assert_eq!(e.segments().count(), 0);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R | PF_X, b"abcd", 0)]);
        raw[1] = b'X';
        assert_eq!(ElfImage::parse(&raw).unwrap_err(), LoaderError::BadElf);
    }

    #[test]
    fn wrong_class_rejected() {
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R, b"a", 0)]);
        raw[4] = 1; // ELFCLASS32
        assert_eq!(ElfImage::parse(&raw).unwrap_err(), LoaderError::BadElf);
    }

    #[test]
    fn wrong_machine_rejected() {
        // Die JEWEILS ANDERE Architektur. Ein ELF fuer eine fremde Maschine muss an der Kante
        // scheitern, nicht erst beim ersten ausgefuehrten Befehl.
        let foreign = if EXPECTED_MACHINE == EM_AARCH64 { EM_X86_64 } else { EM_AARCH64 };
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R, b"a", 0)]);
        raw[18..20].copy_from_slice(&foreign.to_le_bytes());
        assert_eq!(ElfImage::parse(&raw).unwrap_err(), LoaderError::BadElf);
    }

    #[test]
    fn wrong_type_rejected() {
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R, b"a", 0)]);
        raw[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        assert_eq!(ElfImage::parse(&raw).unwrap_err(), LoaderError::BadElf);
    }

    #[test]
    fn memsz_less_than_filesz_rejected() {
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R | PF_W, b"abcdefgh", 0)]);
        // p_memsz (Header 0 @ phoff+40) auf < filesz(8) setzen.
        let base = EHDR_LEN + 40;
        raw[base..base + 8].copy_from_slice(&4u64.to_le_bytes());
        assert_eq!(ElfImage::parse(&raw).unwrap_err(), LoaderError::BadElf);
    }

    #[test]
    fn segment_offset_out_of_bounds_rejected() {
        let mut raw = build_elf(0x1000, &[(0x1000, PF_R, b"abcd", 0)]);
        // p_offset (Header 0 @ phoff+8) auf einen riesigen Wert setzen.
        let base = EHDR_LEN + 8;
        raw[base..base + 8].copy_from_slice(&0x7FFF_FFFFu64.to_le_bytes());
        assert_eq!(ElfImage::parse(&raw).unwrap_err(), LoaderError::OutOfBounds);
    }

    #[test]
    fn truncated_header_rejected() {
        assert_eq!(ElfImage::parse(&[0u8; 16]).unwrap_err(), LoaderError::TooSmall);
    }
}

// Formale Verifikation (Tier 1, Kani — bounded Model Checking). Nur unter `cargo kani` kompiliert,
// im Normal-Build inert. Der ELF-Parser verarbeitet extern gebaute Binaries (Boot-Archiv/SYS_LOAD) →
// Crash-/Overflow-Freiheit auf beliebiger Eingabe ist sicherheitskritisch. Kani prüft Panics,
// Out-of-Bounds UND Integer-Überläufe (Default-Checks) gemeinsam.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    // 200 reicht für jeden Pfad: `parse` betritt die Program-Header-Schleife nur, wenn
    // `phnum*56 + phoff <= len <= MAXLEN`; mit minimalem phoff=0 also phnum ≤ 3 (alle größeren
    // phnum/phoff lösen vorher OutOfBounds aus). 64 B Header + bis zu 3 Program-Header + Payload
    // passen hinein; unwind(5) deckt die ≤3 Schleifeniterationen vollständig.
    const MAXLEN: usize = 200;

    /// **BEWEIS:** `ElfImage::parse` paniert/OOBt/überläuft **nie** — für beliebige Bytes + Länge
    /// (≤ MAXLEN). Ein verstümmeltes/bösartiges ELF kann den Loader nicht zum Absturz bringen.
    #[kani::proof]
    #[kani::unwind(5)]
    fn parse_never_panics() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        let _ = ElfImage::parse(&data[..len]);
    }

    /// **BEWEIS:** Nach erfolgreichem `parse` ist **jedes** gelieferte Segment in sich konsistent:
    /// `memsz >= filesz` (.bss-/W^X-Vertrag), `segment_bytes()` liefert **exakt** `filesz` Bytes und
    /// liegt **vollständig** im Image (`offset+filesz <= len`, kein Panik/OOB/Overflow beim Iterieren).
    #[kani::proof]
    #[kani::unwind(5)]
    fn segments_are_sound() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        if let Ok(img) = ElfImage::parse(&data[..len]) {
            for seg in img.segments() {
                assert!(seg.memsz >= seg.filesz);
                assert!(img.segment_bytes(&seg).len() == seg.filesz);
                assert!(seg.offset + seg.filesz <= len);
            }
            let _ = img.entry();
        }
    }
}
