//! TrustedSAS-Zertifikat — **reiner**, bounds-geprüfter Parser (ext-28, [ADR 0014]).
//!
//! Hier liegt **keine** Krypto und **kein** `unsafe` (`#![forbid(unsafe_code)]` der Crate): nur das
//! feste 160-Byte-Layout wird validiert + zerlegt. Die eigentliche Ed25519-Verifikation + die
//! Hash-Bindung liegen im Krypto-Crate `sel4lake-trust` bzw. im Kernel-Glue (`loader::verify_image`);
//! der Kernel hält die read-only Key-Datenbank. So bleibt der Parser host-test- und fuzzbar.
//!
//! ## Format (160 B, Little-Endian)
//! ```text
//! 0   magic:u32          = 0x5453_4331 ("TSC1")
//! 4   format_version:u16  = 1
//! 6   flags:u16
//! 8   program_id:u32
//! 12  version:u32
//! 16  binary_hash:[u8;32]    (SHA-256 des ELF)
//! 48  manifest_hash:[u8;32]  (SHA-256 des Manifests)
//! 80  key_id:[u8;16]         (128-bit-Fingerprint = SHA-256(pubkey)[..16])
//! --- signierter Payload (96 B) ---
//! 96  signature:[u8;64]      (Ed25519 ueber Bytes [0..96))
//! ```

use crate::LoaderError;

/// Gesamtlänge eines Zertifikats (Payload + Signatur).
pub const CERT_LEN: usize = 160;
/// Länge des **signierten** Payloads (alles vor der Signatur).
pub const CERT_PAYLOAD_LEN: usize = 96;
/// Länge der Ed25519-Signatur.
pub const CERT_SIG_LEN: usize = 64;
/// Magic im Header-Wort ("TSC1").
pub const CERT_MAGIC: u32 = 0x5453_4331;
/// Aktuell unterstützte Zertifikat-Formatversion.
pub const CERT_FORMAT_VERSION: u16 = 1;

/// Ein geparstes, bounds-validiertes TrustedSAS-Zertifikat. Die Krypto-Prüfung (Signatur über
/// [`Self::payload`] mit dem über [`Self::key_id`] referenzierten PubKey + Hash-Bindung) erfolgt
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
    /// Die signierten Bytes `[0..96)` (Eingabe der Ed25519-Verifikation).
    payload: &'a [u8],
    /// Die Signatur `[96..160)` (64 B).
    signature: &'a [u8],
}

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}

impl<'a> TrustedCert<'a> {
    /// Ein Zertifikat strikt parsen: exakt [`CERT_LEN`] Bytes, korrektes Magic + Formatversion.
    /// **Nie** ein Out-of-Bounds/Panic — jede fehlerhafte Eingabe endet in [`LoaderError::BadCert`].
    pub fn parse(data: &'a [u8]) -> Result<TrustedCert<'a>, LoaderError> {
        if data.len() != CERT_LEN {
            return Err(LoaderError::BadCert);
        }
        if rd_u32(data, 0) != CERT_MAGIC {
            return Err(LoaderError::BadCert);
        }
        let format_version = rd_u16(data, 4);
        if format_version != CERT_FORMAT_VERSION {
            return Err(LoaderError::BadCert);
        }
        let mut binary_hash = [0u8; 32];
        binary_hash.copy_from_slice(&data[16..48]);
        let mut manifest_hash = [0u8; 32];
        manifest_hash.copy_from_slice(&data[48..80]);
        let mut key_id = [0u8; 16];
        key_id.copy_from_slice(&data[80..96]);
        Ok(TrustedCert {
            format_version,
            flags: rd_u16(data, 6),
            program_id: rd_u32(data, 8),
            version: rd_u32(data, 12),
            binary_hash,
            manifest_hash,
            key_id,
            payload: &data[..CERT_PAYLOAD_LEN],
            signature: &data[CERT_PAYLOAD_LEN..CERT_LEN],
        })
    }

    /// Die signierten Bytes (Eingabe der Ed25519-Verifikation), exakt [`CERT_PAYLOAD_LEN`].
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }
    /// Die Ed25519-Signatur, exakt [`CERT_SIG_LEN`].
    pub fn signature(&self) -> &'a [u8] {
        self.signature
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> [u8; CERT_LEN] {
        let mut c = [0u8; CERT_LEN];
        c[0..4].copy_from_slice(&CERT_MAGIC.to_le_bytes());
        c[4..6].copy_from_slice(&CERT_FORMAT_VERSION.to_le_bytes());
        c[6..8].copy_from_slice(&0u16.to_le_bytes()); // flags
        c[8..12].copy_from_slice(&7u32.to_le_bytes()); // program_id
        c[12..16].copy_from_slice(&3u32.to_le_bytes()); // version
        for i in 16..48 {
            c[i] = i as u8; // binary_hash
        }
        for i in 48..80 {
            c[i] = (i + 1) as u8; // manifest_hash
        }
        for i in 80..96 {
            c[i] = (i + 2) as u8; // key_id
        }
        for i in 96..160 {
            c[i] = (i + 3) as u8; // signature
        }
        c
    }

    #[test]
    fn parse_roundtrip() {
        let c = good();
        let t = TrustedCert::parse(&c).unwrap();
        assert_eq!(t.format_version, 1);
        assert_eq!(t.program_id, 7);
        assert_eq!(t.version, 3);
        assert_eq!(t.binary_hash[0], 16);
        assert_eq!(t.manifest_hash[0], 49);
        assert_eq!(t.key_id[0], 82);
        assert_eq!(t.payload().len(), CERT_PAYLOAD_LEN);
        assert_eq!(t.signature().len(), CERT_SIG_LEN);
        assert_eq!(t.payload(), &c[..96]);
        assert_eq!(t.signature(), &c[96..]);
    }

    #[test]
    fn wrong_length_rejected() {
        assert_eq!(TrustedCert::parse(&[]).unwrap_err(), LoaderError::BadCert);
        assert_eq!(TrustedCert::parse(&[0u8; 159]).unwrap_err(), LoaderError::BadCert);
        assert_eq!(TrustedCert::parse(&[0u8; 161]).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut c = good();
        c[0] ^= 0xFF;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn bad_format_version_rejected() {
        let mut c = good();
        c[4] = 0xEE;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }
}
