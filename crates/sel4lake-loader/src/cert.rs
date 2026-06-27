//! TrustedSAS-Zertifikat — **reiner**, bounds-geprüfter Parser (ext-28, [ADR 0014]).
//!
//! Hier liegt **keine** Krypto und **kein** `unsafe` (`#![forbid(unsafe_code)]` der Crate): nur das
//! Nachrichtenlayout wird validiert + zerlegt. Die Ed25519-Verifikation der **gesamten** Nachricht +
//! die Policy (Hash-Bindung, Anti-Downgrade, Unsafe-Status-Pflicht) liegen im Kernel-Glue
//! (`loader::verify_image`) bzw. in `sel4lake-trust`; der Kernel hält die read-only Key-DB.
//!
//! ## Format (Little-Endian) — die **gesamte** Nachricht `[0..msg_len)` wird signiert
//! ```text
//! 0    magic:u32              = 0x5453_4331 ("TSC1")
//! 4    format_version:u16      = 1
//! 6    flags:u16               (Eigenschaften)
//! 8    program_id:u32
//! 12   version:u32
//! 16   binary_hash:[u8;32]     (SHA-256 des ELF)
//! 48   manifest_hash:[u8;32]   (SHA-256 des Manifests)
//! 80   key_id:[u8;16]          (128-bit-Fingerprint = SHA-256(pubkey)[..16])
//! 96   unsafe_status:u32       (Bitflags: PROGRAM_FORBID|PROJECT_CLEAN|ALLOWLIST_OK; ADR 0014)
//! 100  unsafe_audit_hash:[u8;32] (SHA-256 des vollstaendigen Unsafe-Audit-Berichts -> bindet ihn)
//! 132  build_info_len:u16
//! 134  build_info:[build_info_len B]  (UTF-8: rustc/toolchain/target/profil/zeitstempel)
//! --- signierte Nachricht endet (msg_len = 134 + build_info_len) ---
//! msg_len  signature:[u8;64]   (Ed25519 ueber [0..msg_len))
//! ```
//! Damit ist **jedes** Feld (Version, Hashes, Flags, Unsafe-Status, Build-Infos) kryptographisch
//! geschuetzt — keines kann nachtraeglich geaendert/ausgetauscht werden.

use crate::LoaderError;

/// Magic ("TSC1").
pub const CERT_MAGIC: u32 = 0x5453_4331;
/// Aktuell unterstützte Zertifikat-Formatversion.
pub const CERT_FORMAT_VERSION: u16 = 1;
/// Länge des festen Kopfteils (bis einschließlich `build_info_len`).
pub const CERT_HEADER_LEN: usize = 134;
/// Länge der Ed25519-Signatur.
pub const CERT_SIG_LEN: usize = 64;
/// Kleinste gültige Zertifikatslänge (leere `build_info`).
pub const CERT_MIN_LEN: usize = CERT_HEADER_LEN + CERT_SIG_LEN;

// Unsafe-Prüfstatus-Bitflags (ADR 0014): vom Build-/Signier-Tool gesetzt, signiert, vom Kernel
// erzwungen (TrustedSAS verlangt [`UNSAFE_ALL_PASS`]).
/// Programm-Crate: `#![forbid(unsafe_code)]` + 0 `unsafe`.
pub const UNSAFE_PROGRAM_FORBID: u32 = 1 << 0;
/// Alle **projektinternen** Crates: 0 `unsafe`.
pub const UNSAFE_PROJECT_CLEAN: u32 = 1 << 1;
/// Dependency-Allowlist erfüllt (`unsafe` nur in der erlaubten ABI-Crate, z. B. `libsel4lake`).
pub const UNSAFE_ALLOWLIST_OK: u32 = 1 << 2;
/// Alle Unsafe-Regeln bestanden (Pflicht für TrustedSAS).
pub const UNSAFE_ALL_PASS: u32 = UNSAFE_PROGRAM_FORBID | UNSAFE_PROJECT_CLEAN | UNSAFE_ALLOWLIST_OK;

/// Ein geparstes, bounds-validiertes TrustedSAS-Zertifikat. Die Krypto-Prüfung (Signatur über
/// [`Self::message`] mit dem über [`Self::key_id`] referenzierten PubKey) + die Policy erfolgen
/// **außerhalb** (Kernel/`sel4lake-trust`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedCert<'a> {
    pub format_version: u16,
    pub flags: u16,
    pub program_id: u32,
    pub version: u32,
    pub binary_hash: [u8; 32],
    pub manifest_hash: [u8; 32],
    pub key_id: [u8; 16],
    pub unsafe_status: u32,
    pub unsafe_audit_hash: [u8; 32],
    build_info: &'a [u8],
    message: &'a [u8],
    signature: &'a [u8],
}

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}

impl<'a> TrustedCert<'a> {
    /// Ein Zertifikat strikt parsen: korrektes Magic + Formatversion, exakte Gesamtlänge passend zu
    /// `build_info_len`. **Nie** ein Out-of-Bounds/Panic — fehlerhafte Eingabe → [`LoaderError::BadCert`].
    pub fn parse(data: &'a [u8]) -> Result<TrustedCert<'a>, LoaderError> {
        if data.len() < CERT_MIN_LEN {
            return Err(LoaderError::BadCert);
        }
        if rd_u32(data, 0) != CERT_MAGIC {
            return Err(LoaderError::BadCert);
        }
        let format_version = rd_u16(data, 4);
        if format_version != CERT_FORMAT_VERSION {
            return Err(LoaderError::BadCert);
        }
        let build_info_len = rd_u16(data, 132) as usize;
        // msg_len overflow-sicher (build_info_len <= u16::MAX); exakte Gesamtlänge erzwingen.
        let msg_len = CERT_HEADER_LEN + build_info_len;
        if data.len() != msg_len + CERT_SIG_LEN {
            return Err(LoaderError::BadCert);
        }
        let mut binary_hash = [0u8; 32];
        binary_hash.copy_from_slice(&data[16..48]);
        let mut manifest_hash = [0u8; 32];
        manifest_hash.copy_from_slice(&data[48..80]);
        let mut key_id = [0u8; 16];
        key_id.copy_from_slice(&data[80..96]);
        let mut unsafe_audit_hash = [0u8; 32];
        unsafe_audit_hash.copy_from_slice(&data[100..132]);
        Ok(TrustedCert {
            format_version,
            flags: rd_u16(data, 6),
            program_id: rd_u32(data, 8),
            version: rd_u32(data, 12),
            binary_hash,
            manifest_hash,
            key_id,
            unsafe_status: rd_u32(data, 96),
            unsafe_audit_hash,
            build_info: &data[CERT_HEADER_LEN..msg_len],
            message: &data[..msg_len],
            signature: &data[msg_len..],
        })
    }

    /// Die **gesamte** signierte Nachricht `[0..msg_len)` (Eingabe der Ed25519-Verifikation).
    pub fn message(&self) -> &'a [u8] {
        self.message
    }
    /// Die Ed25519-Signatur, exakt [`CERT_SIG_LEN`].
    pub fn signature(&self) -> &'a [u8] {
        self.signature
    }
    /// Die (signierten) Build-/Compiler-Informationen (UTF-8).
    pub fn build_info(&self) -> &'a [u8] {
        self.build_info
    }
    /// Hat der Unsafe-Audit **alle** TrustedSAS-Regeln bestanden? (Pflicht für TrustedSAS.)
    pub fn unsafe_all_pass(&self) -> bool {
        self.unsafe_status & UNSAFE_ALL_PASS == UNSAFE_ALL_PASS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein gültiges Zertifikat mit `build_info` der Länge `bil` bauen (Signatur ist Dummy — der
    /// Parser prüft keine Krypto).
    fn good(bil: usize) -> Vec<u8> {
        let msg_len = CERT_HEADER_LEN + bil;
        let mut c = vec![0u8; msg_len + CERT_SIG_LEN];
        c[0..4].copy_from_slice(&CERT_MAGIC.to_le_bytes());
        c[4..6].copy_from_slice(&CERT_FORMAT_VERSION.to_le_bytes());
        c[6..8].copy_from_slice(&0u16.to_le_bytes());
        c[8..12].copy_from_slice(&7u32.to_le_bytes());
        c[12..16].copy_from_slice(&3u32.to_le_bytes());
        for i in 16..48 {
            c[i] = i as u8;
        }
        for i in 48..80 {
            c[i] = (i + 1) as u8;
        }
        for i in 80..96 {
            c[i] = (i + 2) as u8;
        }
        c[96..100].copy_from_slice(&UNSAFE_ALL_PASS.to_le_bytes());
        for i in 100..132 {
            c[i] = (i + 3) as u8;
        }
        c[132..134].copy_from_slice(&(bil as u16).to_le_bytes());
        for i in 0..bil {
            c[CERT_HEADER_LEN + i] = b'B';
        }
        for i in 0..CERT_SIG_LEN {
            c[msg_len + i] = (i + 5) as u8;
        }
        c
    }

    #[test]
    fn parse_roundtrip_with_build_info() {
        let c = good(20);
        let t = TrustedCert::parse(&c).unwrap();
        assert_eq!(t.format_version, 1);
        assert_eq!(t.program_id, 7);
        assert_eq!(t.version, 3);
        assert_eq!(t.unsafe_status, UNSAFE_ALL_PASS);
        assert!(t.unsafe_all_pass());
        assert_eq!(t.build_info(), &[b'B'; 20]);
        assert_eq!(t.message().len(), CERT_HEADER_LEN + 20);
        assert_eq!(t.signature().len(), CERT_SIG_LEN);
        assert_eq!(t.message(), &c[..CERT_HEADER_LEN + 20]);
        assert_eq!(t.signature(), &c[CERT_HEADER_LEN + 20..]);
    }

    #[test]
    fn parse_empty_build_info_ok() {
        let c = good(0);
        let t = TrustedCert::parse(&c).unwrap();
        assert_eq!(t.build_info().len(), 0);
        assert_eq!(c.len(), CERT_MIN_LEN);
    }

    #[test]
    fn unsafe_status_partial_is_not_all_pass() {
        let mut c = good(0);
        c[96..100].copy_from_slice(&(UNSAFE_PROGRAM_FORBID | UNSAFE_PROJECT_CLEAN).to_le_bytes());
        let t = TrustedCert::parse(&c).unwrap();
        assert!(!t.unsafe_all_pass()); // ALLOWLIST_OK fehlt
    }

    #[test]
    fn wrong_total_length_rejected() {
        let mut c = good(20);
        c.pop(); // ein Byte zu kurz
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
        let mut c = good(20);
        c.push(0); // ein Byte zu lang
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn build_info_len_mismatch_rejected() {
        let mut c = good(20);
        c[132..134].copy_from_slice(&21u16.to_le_bytes()); // behauptet 21, Daten passen zu 20
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn too_short_rejected() {
        assert_eq!(TrustedCert::parse(&[]).unwrap_err(), LoaderError::BadCert);
        assert_eq!(
            TrustedCert::parse(&[0u8; CERT_MIN_LEN - 1]).unwrap_err(),
            LoaderError::BadCert
        );
    }

    #[test]
    fn bad_magic_rejected() {
        let mut c = good(0);
        c[0] ^= 0xFF;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn bad_format_version_rejected() {
        let mut c = good(0);
        c[4] = 0xEE;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }
}
