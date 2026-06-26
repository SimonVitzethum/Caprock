//! Binary-Loader-Parser (ext-26) — die **reine**, bounds-geprüfte Lese-Logik des Loaders.
//!
//! Hier liegt **kein** `unsafe` und keine Kernel-/Hardware-Abhängigkeit: der Code arbeitet
//! ausschließlich auf `&[u8]`-Slices. Damit ist er per Host-`cargo test` vollständig verifizier-
//! und fuzzbar (siehe [ADR 0011](../../docs/adr/0011-binary-loader.md)). Der **privilegierte** Teil
//! (Segmente in Regionen kopieren, W^X mappen, VSpace/PD anlegen, Caps endowen, spawnen) lebt
//! getrennt im Kernel-Glue (`kernel/src/loader.rs`).
//!
//! ## Boot-Archiv (L0)
//! Ein einzelnes, von QEMU `-device loader` in ein reserviertes RAM-Fenster gelegtes Archiv:
//! ```text
//! Header (32 B):  magic:u32 version:u32 count:u32 total_len:u32 reserved:[u32;4]
//! Entry  (96 B):  name:[u8;16] blob_off:u32 blob_len:u32 manifest_off:u32 manifest_len:u32
//!                 domain:u32 flags:u32 hash:[u8;32] reserved:[u32;6]
//! ... danach die Blobs + Manifeste (innerhalb total_len) ...
//! ```
//! Alle Offsets sind relativ zum Archiv-Anfang; alle Zugriffe sind bounds-geprüft. Ein fehlendes/
//! beschädigtes Archiv (falsche Magic) ergibt einen `Err` — der Kernel behandelt das als „0 Module".

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Archiv-Magic ("SLKA" ~ SeL4Lake-Archiv), Little-Endian im Header-Wort.
pub const MAGIC: u32 = 0x534C_4B41;
/// Aktuelle Archiv-Version.
pub const VERSION: u32 = 1;

const HEADER_LEN: usize = 32;
const ENTRY_LEN: usize = 96;

/// Domäne eines geladenen Programms (Manifest/Archiv-Feld). Bewusst kernel-agnostisch (u32);
/// der Kernel-Glue bildet das auf `microkit::Domain` ab.
pub const DOMAIN_TRUSTED: u32 = 0;
pub const DOMAIN_HARDWARE: u32 = 1;
pub const DOMAIN_USERLAND: u32 = 2;

/// Parse-Fehler. Jeder Pfad, der eine fehlerhafte Eingabe erkennt, endet hier — **nie** in einem
/// Out-of-Bounds-Zugriff oder Panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaderError {
    /// Datenpuffer kürzer als der Header / ein Eintrag.
    TooSmall,
    /// Falsche Magic (kein/kaputtes Archiv).
    BadMagic,
    /// Nicht unterstützte Archiv-Version.
    BadVersion,
    /// Ein Offset/Länge liegt außerhalb des Archivs (oder `total_len` > Puffer).
    OutOfBounds,
    /// Unplausible Eintragszahl (Eintragstabelle passt nicht in `total_len`).
    BadCount,
}

fn rd_u32(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Ein geparster Archiv-Eintrag: Metadaten + **Sub-Slices** auf Blob + Manifest (bereits
/// bounds-validiert, also gefahrlos lesbar).
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    name: &'a [u8],
    /// Domäne (`DOMAIN_*`).
    pub domain: u32,
    /// Frei nutzbare Flags (z.B. EL1-Marker), bisher 0.
    pub flags: u32,
    /// Das Programm-Image (ELF64).
    pub blob: &'a [u8],
    /// Das Manifest (Cap-Endowment etc.; ab L2 interpretiert).
    pub manifest: &'a [u8],
    /// Reserviert für die spätere Signatur-/Hash-Verifikation (ADR 0011 §7).
    pub hash: [u8; 32],
}

impl<'a> Entry<'a> {
    /// Der Name als `&str` (bis zum ersten NUL bzw. Ende), nicht-UTF8 → `"?"`.
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|&c| c == 0).unwrap_or(self.name.len());
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

/// Ein geparstes, bounds-validiertes Boot-Archiv. `entry(i)` liefert gefahrlose Sub-Slices.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    data: &'a [u8],
    count: usize,
}

impl<'a> Archive<'a> {
    /// Das Archiv parsen + **vollständig** validieren (Header, Version, alle Eintrags-Bounds).
    pub fn parse(data: &'a [u8]) -> Result<Archive<'a>, LoaderError> {
        if data.len() < HEADER_LEN {
            return Err(LoaderError::TooSmall);
        }
        if rd_u32(data, 0).ok_or(LoaderError::TooSmall)? != MAGIC {
            return Err(LoaderError::BadMagic);
        }
        if rd_u32(data, 4).ok_or(LoaderError::TooSmall)? != VERSION {
            return Err(LoaderError::BadVersion);
        }
        let count = rd_u32(data, 8).ok_or(LoaderError::TooSmall)? as usize;
        let total_len = rd_u32(data, 12).ok_or(LoaderError::TooSmall)? as usize;
        if total_len > data.len() {
            return Err(LoaderError::OutOfBounds);
        }
        // Eintragstabelle muss vollständig in total_len liegen (kein Overflow durch checked_*).
        let table_end = count
            .checked_mul(ENTRY_LEN)
            .and_then(|t| t.checked_add(HEADER_LEN))
            .ok_or(LoaderError::BadCount)?;
        if table_end > total_len {
            return Err(LoaderError::BadCount);
        }
        let a = Archive { data: &data[..total_len], count };
        // Alle Einträge eifrig validieren, damit `entry()` später garantiert gültige Slices liefert.
        for i in 0..count {
            a.parse_entry(i)?;
        }
        Ok(a)
    }

    /// Anzahl der Einträge.
    pub fn count(&self) -> usize {
        self.count
    }

    fn parse_entry(&self, i: usize) -> Result<Entry<'a>, LoaderError> {
        if i >= self.count {
            return Err(LoaderError::OutOfBounds);
        }
        let base = HEADER_LEN + i * ENTRY_LEN;
        let e = self.data.get(base..base + ENTRY_LEN).ok_or(LoaderError::OutOfBounds)?;
        let name = &e[0..16];
        let blob_off = u32::from_le_bytes([e[16], e[17], e[18], e[19]]) as usize;
        let blob_len = u32::from_le_bytes([e[20], e[21], e[22], e[23]]) as usize;
        let man_off = u32::from_le_bytes([e[24], e[25], e[26], e[27]]) as usize;
        let man_len = u32::from_le_bytes([e[28], e[29], e[30], e[31]]) as usize;
        let domain = u32::from_le_bytes([e[32], e[33], e[34], e[35]]);
        let flags = u32::from_le_bytes([e[36], e[37], e[38], e[39]]);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&e[40..72]);
        let blob = slice_within(self.data, blob_off, blob_len)?;
        let manifest = slice_within(self.data, man_off, man_len)?;
        Ok(Entry { name, domain, flags, blob, manifest, hash })
    }

    /// Eintrag `i` (bereits bei `parse` validiert). `None` nur bei `i >= count`.
    pub fn entry(&self, i: usize) -> Option<Entry<'a>> {
        self.parse_entry(i).ok()
    }

    /// Über alle Einträge iterieren.
    pub fn iter(&self) -> impl Iterator<Item = Entry<'a>> + '_ {
        (0..self.count).filter_map(move |i| self.entry(i))
    }
}

/// Ein `[off, off+len)`-Sub-Slice von `data`, bounds-geprüft (Overflow-sicher). `len==0` → leerer
/// Slice am Anfang (gültig).
fn slice_within(data: &[u8], off: usize, len: usize) -> Result<&[u8], LoaderError> {
    let end = off.checked_add(len).ok_or(LoaderError::OutOfBounds)?;
    data.get(off..end).ok_or(LoaderError::OutOfBounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein gültiges Archiv von Hand bauen: `entries` = (name, domain, blob, manifest).
    fn build(entries: &[(&str, u32, &[u8], &[u8])]) -> Vec<u8> {
        let count = entries.len();
        let table_end = HEADER_LEN + count * ENTRY_LEN;
        // Blobs/Manifeste hinter der Tabelle anordnen.
        let mut payload = Vec::new();
        let mut spans = Vec::new();
        for (_, _, blob, man) in entries {
            let bo = table_end + payload.len();
            payload.extend_from_slice(blob);
            let mo = table_end + payload.len();
            payload.extend_from_slice(man);
            spans.push((bo, blob.len(), mo, man.len()));
        }
        let total = table_end + payload.len();
        let mut v = vec![0u8; table_end];
        v[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        v[4..8].copy_from_slice(&VERSION.to_le_bytes());
        v[8..12].copy_from_slice(&(count as u32).to_le_bytes());
        v[12..16].copy_from_slice(&(total as u32).to_le_bytes());
        for (i, ((name, dom, _, _), (bo, bl, mo, ml))) in entries.iter().zip(spans).enumerate() {
            let base = HEADER_LEN + i * ENTRY_LEN;
            let nb = name.as_bytes();
            v[base..base + nb.len().min(16)].copy_from_slice(&nb[..nb.len().min(16)]);
            v[base + 16..base + 20].copy_from_slice(&(bo as u32).to_le_bytes());
            v[base + 20..base + 24].copy_from_slice(&(bl as u32).to_le_bytes());
            v[base + 24..base + 28].copy_from_slice(&(mo as u32).to_le_bytes());
            v[base + 28..base + 32].copy_from_slice(&(ml as u32).to_le_bytes());
            v[base + 32..base + 36].copy_from_slice(&dom.to_le_bytes());
        }
        v.extend_from_slice(&payload);
        v
    }

    #[test]
    fn empty_archive_ok() {
        let raw = build(&[]);
        let a = Archive::parse(&raw).unwrap();
        assert_eq!(a.count(), 0);
        assert!(a.entry(0).is_none());
    }

    #[test]
    fn two_entries_roundtrip() {
        let raw = build(&[
            ("hello", DOMAIN_USERLAND, b"\x7fELFblob1", b"manifest1"),
            ("drv", DOMAIN_HARDWARE, b"BLOB2", b""),
        ]);
        let a = Archive::parse(&raw).unwrap();
        assert_eq!(a.count(), 2);
        let e0 = a.entry(0).unwrap();
        assert_eq!(e0.name(), "hello");
        assert_eq!(e0.domain, DOMAIN_USERLAND);
        assert_eq!(e0.blob, b"\x7fELFblob1");
        assert_eq!(e0.manifest, b"manifest1");
        let e1 = a.entry(1).unwrap();
        assert_eq!(e1.name(), "drv");
        assert_eq!(e1.domain, DOMAIN_HARDWARE);
        assert_eq!(e1.blob, b"BLOB2");
        assert_eq!(e1.manifest, b"");
        assert_eq!(a.iter().count(), 2);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut raw = build(&[("x", DOMAIN_TRUSTED, b"z", b"")]);
        raw[0] ^= 0xFF;
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::BadMagic);
    }

    #[test]
    fn bad_version_rejected() {
        let mut raw = build(&[]);
        raw[4] = 0xEE;
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::BadVersion);
    }

    #[test]
    fn too_small_rejected() {
        assert_eq!(Archive::parse(&[0u8; 8]).unwrap_err(), LoaderError::TooSmall);
        assert_eq!(Archive::parse(&[]).unwrap_err(), LoaderError::TooSmall);
    }

    #[test]
    fn total_len_beyond_buffer_rejected() {
        let mut raw = build(&[]);
        raw[12..16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::OutOfBounds);
    }

    #[test]
    fn bogus_count_rejected() {
        let mut raw = build(&[]);
        raw[8..12].copy_from_slice(&1000u32.to_le_bytes()); // 1000 Einträge, aber winziges total_len
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::BadCount);
    }

    #[test]
    fn entry_blob_offset_out_of_bounds_rejected() {
        let mut raw = build(&[("x", DOMAIN_TRUSTED, b"abcd", b"")]);
        // blob_off (Eintrag 0, @ HEADER+16) auf einen riesigen Wert setzen.
        let base = HEADER_LEN + 16;
        raw[base..base + 4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::OutOfBounds);
    }
}
