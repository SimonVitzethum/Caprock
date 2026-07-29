//! Boot-Archiv-Parser (ext-26) — **eine** Quelle für [`Program`]-Deskriptoren.
//!
//! Das Archiv wird von QEMU `-device loader` in ein reserviertes RAM-Fenster gelegt; der Kernel
//! liest es hier (bounds-geprüft, panik-frei). Die quellen-agnostische Loader-API hängt **nicht**
//! von diesem Format ab (ADR 0011, Verfeinerung 3) — der Parser produziert nur [`Program`]s.
//!
//! ## Format
//! ```text
//! Header (32 B):  magic:u32  version:u32  count:u32  total_len:u32
//!                 sysman_off:u32  sysman_len:u32  reserved:[u32;2]
//! Entry  (96 B):  name:[u8;16]
//!                 program_id:u32  version:u32  domain:u32  flags:u32
//!                 blob_off:u32  blob_len:u32  manifest_off:u32  manifest_len:u32
//!                 hash:[u8;32]  cert_off:u32  cert_len:u32  reserved:[u32;2]
//! ... danach die Blobs + Manifeste + Zertifikate + das System-Manifest (innerhalb total_len) ...
//! ```
//! Alle Offsets sind relativ zum Archiv-Anfang; alle Zugriffe sind bounds-geprüft. `cert_len==0`
//! (ext-28) bedeutet „kein Zertifikat" (nur für TrustedSAS erforderlich).
//!
//! ## Das System-Manifest (A-1, Format v3)
//!
//! `sysman_off`/`sysman_len` zeigen auf das **eine** Autoritätsdokument des Boot-Images
//! ([`crate::manifest`]). Es liegt bewusst *im* Archiv, nicht daneben: das Boot-Image soll aus
//! genau zwei Dingen bestehen (Kernel + eine Datei), und eine zweite Datei wäre eine zweite
//! Stelle, an der jemand etwas austauschen kann. `sysman_len == 0` heißt „kein Manifest" — dann
//! lädt der Kernel nichts von sich aus (er weiß dann nicht, wer welche Autorität bekäme).

use crate::{slice_within, LoaderError, Program};

/// Archiv-Magic ("SLKA" ~ SeL4Lake-Archiv), Little-Endian im Header-Wort.
pub const MAGIC: u32 = 0x534C_4B41;
/// Aktuelle Archiv-Format-Version (v3: `reserved[0..1]` → `sysman_off`/`sysman_len`).
pub const VERSION: u32 = 3;

const HEADER_LEN: usize = 32;
const ENTRY_LEN: usize = 96;

fn rd_u32(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Ein geparstes, bounds-validiertes Boot-Archiv. `program(i)` liefert gefahrlose [`Program`]s.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    data: &'a [u8],
    count: usize,
    sysman: &'a [u8],
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
        // Eintragstabelle muss vollständig in total_len liegen (Overflow-sicher via checked_*).
        let table_end = count
            .checked_mul(ENTRY_LEN)
            .and_then(|t| t.checked_add(HEADER_LEN))
            .ok_or(LoaderError::BadCount)?;
        if table_end > total_len {
            return Err(LoaderError::BadCount);
        }
        // Das System-Manifest liegt wie jede andere Nutzlast im Archiv und wird genauso
        // bounds-geprüft — es ist an dieser Stelle nur ein Byte-Bereich, kein Autoritätsdokument.
        let body = &data[..total_len];
        let sysman_off = rd_u32(data, 16).ok_or(LoaderError::TooSmall)? as usize;
        let sysman_len = rd_u32(data, 20).ok_or(LoaderError::TooSmall)? as usize;
        let sysman = slice_within(body, sysman_off, sysman_len)?;
        let a = Archive { data: body, count, sysman };
        // Alle Einträge eifrig validieren, damit `program()` später garantiert gültige Slices liefert.
        for i in 0..count {
            a.parse_program(i)?;
        }
        Ok(a)
    }

    /// Anzahl der Programme.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Die Roh-Bytes des System-Manifests (leer = keines). **Ungeprüft** — Parsen und
    /// Signaturprüfung macht [`crate::manifest`] bzw. der Kernel-Verifier.
    pub fn system_manifest(&self) -> &'a [u8] {
        self.sysman
    }

    fn parse_program(&self, i: usize) -> Result<Program<'a>, LoaderError> {
        if i >= self.count {
            return Err(LoaderError::OutOfBounds);
        }
        let base = HEADER_LEN + i * ENTRY_LEN;
        let e = self.data.get(base..base + ENTRY_LEN).ok_or(LoaderError::OutOfBounds)?;
        // SAFETY-frei: feste Offsets in einem garantiert ENTRY_LEN langen Slice.
        let name = &e[0..16];
        let program_id = u32::from_le_bytes([e[16], e[17], e[18], e[19]]);
        let version = u32::from_le_bytes([e[20], e[21], e[22], e[23]]);
        let domain = u32::from_le_bytes([e[24], e[25], e[26], e[27]]);
        let _flags = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
        let blob_off = u32::from_le_bytes([e[32], e[33], e[34], e[35]]) as usize;
        let blob_len = u32::from_le_bytes([e[36], e[37], e[38], e[39]]) as usize;
        let man_off = u32::from_le_bytes([e[40], e[41], e[42], e[43]]) as usize;
        let man_len = u32::from_le_bytes([e[44], e[45], e[46], e[47]]) as usize;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&e[48..80]);
        // ext-28: cert_off/cert_len in den frueher reservierten Feldern (Format v2). cert_len==0 ->
        // leerer Slice (kein Zertifikat).
        let cert_off = u32::from_le_bytes([e[80], e[81], e[82], e[83]]) as usize;
        let cert_len = u32::from_le_bytes([e[84], e[85], e[86], e[87]]) as usize;
        let elf = slice_within(self.data, blob_off, blob_len)?;
        let manifest = slice_within(self.data, man_off, man_len)?;
        let cert = slice_within(self.data, cert_off, cert_len)?;
        Ok(Program::new(program_id, name, version, domain, hash, elf, manifest, cert))
    }

    /// Programm `i` (bereits bei `parse` validiert). `None` nur bei `i >= count`.
    pub fn program(&self, i: usize) -> Option<Program<'a>> {
        self.parse_program(i).ok()
    }

    /// Über alle Programme iterieren.
    pub fn iter(&self) -> impl Iterator<Item = Program<'a>> + '_ {
        (0..self.count).filter_map(move |i| self.program(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DOMAIN_HARDWARE, DOMAIN_TRUSTED, DOMAIN_USERLAND};

    /// Ein gültiges Archiv von Hand bauen: `entries` = (program_id, name, version, domain, blob, manifest).
    fn build(entries: &[(u32, &str, u32, u32, &[u8], &[u8])]) -> Vec<u8> {
        let count = entries.len();
        let table_end = HEADER_LEN + count * ENTRY_LEN;
        let mut payload = Vec::new();
        let mut spans = Vec::new();
        for (_, _, _, _, blob, man) in entries {
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
        for (i, ((id, name, ver, dom, _, _), (bo, bl, mo, ml))) in
            entries.iter().zip(spans).enumerate()
        {
            let base = HEADER_LEN + i * ENTRY_LEN;
            let nb = name.as_bytes();
            v[base..base + nb.len().min(16)].copy_from_slice(&nb[..nb.len().min(16)]);
            v[base + 16..base + 20].copy_from_slice(&id.to_le_bytes());
            v[base + 20..base + 24].copy_from_slice(&ver.to_le_bytes());
            v[base + 24..base + 28].copy_from_slice(&dom.to_le_bytes());
            v[base + 32..base + 36].copy_from_slice(&(bo as u32).to_le_bytes());
            v[base + 36..base + 40].copy_from_slice(&(bl as u32).to_le_bytes());
            v[base + 40..base + 44].copy_from_slice(&(mo as u32).to_le_bytes());
            v[base + 44..base + 48].copy_from_slice(&(ml as u32).to_le_bytes());
        }
        v.extend_from_slice(&payload);
        v
    }

    #[test]
    fn empty_archive_ok() {
        let raw = build(&[]);
        let a = Archive::parse(&raw).unwrap();
        assert_eq!(a.count(), 0);
        assert!(a.program(0).is_none());
    }

    #[test]
    fn two_programs_roundtrip() {
        let raw = build(&[
            (7, "hello", 1, DOMAIN_USERLAND, b"\x7fELFblob1", b"manifest1"),
            (42, "drv", 3, DOMAIN_HARDWARE, b"BLOB2", b""),
        ]);
        let a = Archive::parse(&raw).unwrap();
        assert_eq!(a.count(), 2);
        let p0 = a.program(0).unwrap();
        assert_eq!(p0.program_id, 7);
        assert_eq!(p0.name(), "hello");
        assert_eq!(p0.version, 1);
        assert_eq!(p0.domain, DOMAIN_USERLAND);
        assert_eq!(p0.elf, b"\x7fELFblob1");
        assert_eq!(p0.manifest, b"manifest1");
        let p1 = a.program(1).unwrap();
        assert_eq!(p1.program_id, 42);
        assert_eq!(p1.name(), "drv");
        assert_eq!(p1.version, 3);
        assert_eq!(p1.domain, DOMAIN_HARDWARE);
        assert_eq!(p1.elf, b"BLOB2");
        assert_eq!(p1.manifest, b"");
        assert_eq!(a.iter().count(), 2);
    }

    #[test]
    fn system_manifest_absent_by_default() {
        let raw = build(&[(1, "x", 1, DOMAIN_TRUSTED, b"z", b"")]);
        assert!(Archive::parse(&raw).unwrap().system_manifest().is_empty());
    }

    #[test]
    fn system_manifest_span_is_bounds_checked() {
        let raw = build(&[(1, "x", 1, DOMAIN_TRUSTED, b"abcd", b"")]);
        // Auf den Blob zeigen lassen: `blob_off` steht bei HEADER+32 des ersten Eintrags.
        let bo = u32::from_le_bytes(raw[HEADER_LEN + 32..HEADER_LEN + 36].try_into().unwrap());
        let mut ok = raw.clone();
        ok[16..20].copy_from_slice(&bo.to_le_bytes());
        ok[20..24].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(Archive::parse(&ok).unwrap().system_manifest(), b"abcd");
        // Ausserhalb -> das ganze Archiv wird abgelehnt, nicht bloss das Manifest ignoriert.
        let mut bad = raw.clone();
        bad[16..20].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        bad[20..24].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(Archive::parse(&bad).unwrap_err(), LoaderError::OutOfBounds);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut raw = build(&[(1, "x", 1, DOMAIN_TRUSTED, b"z", b"")]);
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
        raw[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::BadCount);
    }

    #[test]
    fn blob_offset_out_of_bounds_rejected() {
        let mut raw = build(&[(1, "x", 1, DOMAIN_TRUSTED, b"abcd", b"")]);
        // blob_off (Eintrag 0) liegt bei HEADER+32.
        let base = HEADER_LEN + 32;
        raw[base..base + 4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        assert_eq!(Archive::parse(&raw).unwrap_err(), LoaderError::OutOfBounds);
    }
}

// Formale Verifikation (Tier 1, Kani — bounded Model Checking). Nur unter `cargo kani` kompiliert,
// im Normal-Build inert. Das Boot-Archiv ist die EINZIGE externe Datenquelle des Loaders (aus dem
// reservierten RAM-Fenster gelesen) → Crash-Freiheit auf beliebiger Eingabe ist sicherheitskritisch.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    // 260 reicht, um jeden Pfad zu erreichen: `parse` betritt die Eintragsschleife nur, wenn
    // `count*96 + 32 <= total_len <= data.len() <= MAXLEN`, also count ≤ 2 (alle größeren count/
    // total_len lösen vorher BadCount/OutOfBounds aus). Damit deckt unwind(3) die Schleife vollständig.
    const MAXLEN: usize = 260;

    /// **BEWEIS:** `Archive::parse` paniert/OOBt **nie** — für beliebige Bytes + Länge (≤ MAXLEN).
    /// Ein verstümmeltes/bösartiges Boot-Archiv kann den Kernel-Loader nicht zum Absturz bringen.
    #[kani::proof]
    #[kani::unwind(3)]
    fn parse_never_panics() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        if let Ok(a) = Archive::parse(&data[..len]) {
            // Nach erfolgreichem parse liefert jeder Index < count ein gültiges Program (kein Panik).
            let n = a.count();
            kani::assume(n <= 2);
            let mut i = 0;
            while i < n {
                let _ = a.program(i);
                i += 1;
            }
        }
    }
}
