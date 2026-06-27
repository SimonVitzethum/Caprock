//! TrustedSAS-Zertifikat — **reiner**, bounds-geprüfter Parser (ext-28, [ADR 0014]).
//!
//! Hier liegt **keine** Krypto und **kein** `unsafe` (`#![forbid(unsafe_code)]` der Crate): nur das
//! Nachrichtenlayout wird validiert + zerlegt. Die Ed25519-Verifikation der **gesamten** Nachricht +
//! die Policy (Hash-Bindung, Anti-Downgrade, Unsafe-Status-Pflicht) liegen im Kernel-Glue
//! (`loader::verify_image`) bzw. in `sel4lake-trust`; der Kernel hält die read-only Key-DB.
//!
//! ## Format (Little-Endian) — die **gesamte** Nachricht `[0..msg_len)` wird signiert
//! ```text
//! 0    magic:u32                = 0x5453_4331 ("TSC1")
//! --- Build-Identitaet / Zertifizierungsverfahren (alle signiert) ---
//! 4    cert_format_version:u16   = 1   (Zertifikatsformat-Version)
//! 6    sig_format_version:u16    = 1   (Signaturformat: Ed25519 ueber die Nachricht)
//! 8    build_rules_version:u16         (Compiler-/Buildregel-Version)
//! 10   audit_protocol_version:u16      (TrustedSAS-Audit-Protokoll-Version)
//! 12   unsafe_rules_version:u16        (Unsafe-Pruefregel-Version)
//! 14   allowlist_rules_version:u16     (Allowlist-Regel-Version)
//! ---
//! 16   flags:u16                 (Eigenschaften)
//! 18   reserved:u16              (=0, signiert, zukuenftig)
//! 20   program_id:u32
//! 24   version:u32
//! 28   binary_hash:[u8;32]       (SHA-256 des ELF — bindet das Zertifikat FEST an genau dies Binary)
//! 60   manifest_hash:[u8;32]     (SHA-256 des Manifests)
//! 92   key_id:[u8;16]            (128-bit-Fingerprint = SHA-256(pubkey)[..16])
//! 108  unsafe_status:u32         (Bitflags PROGRAM_FORBID|PROJECT_CLEAN|ALLOWLIST_OK; muss ALL_PASS)
//! 112  unsafe_audit_hash:[u8;32] (SHA-256 des vollstaendigen Unsafe-Audit-Berichts -> bindet ihn)
//! 144  build_info_len:u16
//! 146  build_info:[..]           (UTF-8: rustc/toolchain/target/profil/zeitstempel)
//! --- signierte Nachricht endet (msg_len = 146 + build_info_len) ---
//! msg_len  signature:[u8;64]     (Ed25519 ueber [0..msg_len))
//! ```
//! Damit ist **jedes** Feld kryptographisch geschützt — inkl. der **Verfahrens-Versionen**: ein
//! Zertifikat sagt nicht nur „von diesem Schlüssel signiert", sondern „nach **genau diesem**
//! TrustedSAS-Zertifizierungsverfahren erzeugt". Spätere Regeländerungen lassen sich so nicht unter
//! demselben Format vermischen.

use crate::LoaderError;

/// Magic ("TSC1").
pub const CERT_MAGIC: u32 = 0x5453_4331;
/// Aktuell unterstützte **Zertifikatsformat**-Version (vom Kernel geprüft).
pub const CERT_FORMAT_VERSION: u16 = 1;
/// Aktuelle **Signaturformat**-Version (Ed25519 über die Nachricht). Vom Tool gestempelt.
pub const SIG_FORMAT_VERSION: u16 = 1;
/// Aktuelle **Compiler-/Buildregel**-Version. Vom Tool gestempelt.
pub const BUILD_RULES_VERSION: u16 = 1;
/// Aktuelle **TrustedSAS-Audit-Protokoll**-Version. Vom Tool gestempelt.
pub const AUDIT_PROTOCOL_VERSION: u16 = 1;
/// Aktuelle **Unsafe-Prüfregel**-Version. Vom Tool gestempelt.
pub const UNSAFE_RULES_VERSION: u16 = 1;
/// Aktuelle **Allowlist-Regel**-Version. Vom Tool gestempelt.
pub const ALLOWLIST_RULES_VERSION: u16 = 1;

/// Länge des festen Kopfteils (bis einschließlich `build_info_len`).
pub const CERT_HEADER_LEN: usize = 146;
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
    // Build-Identitaet / Zertifizierungsverfahren (signiert).
    pub cert_format_version: u16,
    pub sig_format_version: u16,
    pub build_rules_version: u16,
    pub audit_protocol_version: u16,
    pub unsafe_rules_version: u16,
    pub allowlist_rules_version: u16,
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
    /// Ein Zertifikat strikt parsen: korrektes Magic + Zertifikatsformat-Version, exakte Gesamtlänge
    /// passend zu `build_info_len`. **Nie** ein Out-of-Bounds/Panic — fehlerhafte Eingabe →
    /// [`LoaderError::BadCert`]. Die übrigen Verfahrens-Versionen werden geparst + (über die
    /// Signatur) geschützt, hier aber **nicht** erzwungen (kann der Kernel später tun).
    pub fn parse(data: &'a [u8]) -> Result<TrustedCert<'a>, LoaderError> {
        if data.len() < CERT_MIN_LEN {
            return Err(LoaderError::BadCert);
        }
        if rd_u32(data, 0) != CERT_MAGIC {
            return Err(LoaderError::BadCert);
        }
        let cert_format_version = rd_u16(data, 4);
        if cert_format_version != CERT_FORMAT_VERSION {
            return Err(LoaderError::BadCert);
        }
        let build_info_len = rd_u16(data, 144) as usize;
        // msg_len overflow-sicher (build_info_len <= u16::MAX); exakte Gesamtlänge erzwingen.
        let msg_len = CERT_HEADER_LEN + build_info_len;
        if data.len() != msg_len + CERT_SIG_LEN {
            return Err(LoaderError::BadCert);
        }
        let mut binary_hash = [0u8; 32];
        binary_hash.copy_from_slice(&data[28..60]);
        let mut manifest_hash = [0u8; 32];
        manifest_hash.copy_from_slice(&data[60..92]);
        let mut key_id = [0u8; 16];
        key_id.copy_from_slice(&data[92..108]);
        let mut unsafe_audit_hash = [0u8; 32];
        unsafe_audit_hash.copy_from_slice(&data[112..144]);
        Ok(TrustedCert {
            cert_format_version,
            sig_format_version: rd_u16(data, 6),
            build_rules_version: rd_u16(data, 8),
            audit_protocol_version: rd_u16(data, 10),
            unsafe_rules_version: rd_u16(data, 12),
            allowlist_rules_version: rd_u16(data, 14),
            flags: rd_u16(data, 16),
            program_id: rd_u32(data, 20),
            version: rd_u32(data, 24),
            binary_hash,
            manifest_hash,
            key_id,
            unsafe_status: rd_u32(data, 108),
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
        c[6..8].copy_from_slice(&SIG_FORMAT_VERSION.to_le_bytes());
        c[8..10].copy_from_slice(&BUILD_RULES_VERSION.to_le_bytes());
        c[10..12].copy_from_slice(&AUDIT_PROTOCOL_VERSION.to_le_bytes());
        c[12..14].copy_from_slice(&UNSAFE_RULES_VERSION.to_le_bytes());
        c[14..16].copy_from_slice(&ALLOWLIST_RULES_VERSION.to_le_bytes());
        c[16..18].copy_from_slice(&0u16.to_le_bytes()); // flags
        c[20..24].copy_from_slice(&7u32.to_le_bytes()); // program_id
        c[24..28].copy_from_slice(&3u32.to_le_bytes()); // version
        for i in 28..60 {
            c[i] = i as u8; // binary_hash
        }
        for i in 60..92 {
            c[i] = (i + 1) as u8; // manifest_hash
        }
        for i in 92..108 {
            c[i] = (i + 2) as u8; // key_id
        }
        c[108..112].copy_from_slice(&UNSAFE_ALL_PASS.to_le_bytes());
        for i in 112..144 {
            c[i] = (i + 3) as u8; // unsafe_audit_hash
        }
        c[144..146].copy_from_slice(&(bil as u16).to_le_bytes());
        for i in 0..bil {
            c[CERT_HEADER_LEN + i] = b'B';
        }
        for i in 0..CERT_SIG_LEN {
            c[msg_len + i] = (i + 5) as u8;
        }
        c
    }

    #[test]
    fn parse_roundtrip() {
        let c = good(24);
        let t = TrustedCert::parse(&c).unwrap();
        assert_eq!(t.cert_format_version, 1);
        assert_eq!(t.sig_format_version, SIG_FORMAT_VERSION);
        assert_eq!(t.build_rules_version, BUILD_RULES_VERSION);
        assert_eq!(t.audit_protocol_version, AUDIT_PROTOCOL_VERSION);
        assert_eq!(t.unsafe_rules_version, UNSAFE_RULES_VERSION);
        assert_eq!(t.allowlist_rules_version, ALLOWLIST_RULES_VERSION);
        assert_eq!(t.program_id, 7);
        assert_eq!(t.version, 3);
        assert!(t.unsafe_all_pass());
        assert_eq!(t.binary_hash[0], 28);
        assert_eq!(t.build_info(), &[b'B'; 24]);
        assert_eq!(t.message().len(), CERT_HEADER_LEN + 24);
        assert_eq!(t.message(), &c[..CERT_HEADER_LEN + 24]);
        assert_eq!(t.signature(), &c[CERT_HEADER_LEN + 24..]);
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
        c[108..112].copy_from_slice(&(UNSAFE_PROGRAM_FORBID | UNSAFE_PROJECT_CLEAN).to_le_bytes());
        let t = TrustedCert::parse(&c).unwrap();
        assert!(!t.unsafe_all_pass()); // ALLOWLIST_OK fehlt
    }

    #[test]
    fn wrong_total_length_rejected() {
        let mut c = good(24);
        c.pop();
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
        let mut c = good(24);
        c.push(0);
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn build_info_len_mismatch_rejected() {
        let mut c = good(24);
        c[144..146].copy_from_slice(&25u16.to_le_bytes()); // behauptet 25, Daten passen zu 24
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
    fn bad_cert_format_version_rejected() {
        let mut c = good(0);
        c[4] = 0xEE;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }
}
