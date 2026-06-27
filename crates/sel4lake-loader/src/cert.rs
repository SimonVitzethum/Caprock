//! TrustedSAS-Zertifikat — **reiner**, bounds-geprüfter Parser (ext-28, [ADR 0014]).
//!
//! Hier liegt **keine** Krypto und **kein** `unsafe` (`#![forbid(unsafe_code)]` der Crate): nur das
//! Nachrichtenlayout wird validiert + zerlegt. Die Verifikation der **gesamten** Nachricht (Signatur
//! gemäß `signature_algorithm_id`) + die Policy (Hash-Bindung, Anti-Downgrade, Unsafe-Status-Pflicht)
//! liegen im Kernel-Glue (`loader::verify_image`) bzw. in `sel4lake-trust`; der Kernel hält die
//! read-only Key-DB.
//!
//! ## Format (Little-Endian, **eingefroren**) — die **gesamte** Nachricht `[0..msg_len)` wird signiert
//! ```text
//! 0    magic:u32                = 0x5453_4331 ("TSC1")
//! --- Build-Identitaet / Krypto- & Policy-Identifier (alle signiert) ---
//! 4    cert_format_version:u16   = 1   (Zertifikatsformat)
//! 6    sig_format_version:u16    = 1   (Signaturformat-Version)
//! 8    signature_algorithm_id:u16      (Signaturverfahren; Ed25519 = 1)
//! 10   certificate_policy_id:u32       (Zertifizierungspolitik; z. B. interne Test-/Produktion)
//! 14   build_rules_version:u16         (Compiler-/Buildregel)
//! 16   audit_protocol_version:u16      (TrustedSAS-Audit-Protokoll)
//! 18   unsafe_rules_version:u16        (Unsafe-Pruefregeln)
//! 20   allowlist_rules_version:u16     (Allowlist-Regeln)
//! ---
//! 22   flags:u16                 (Eigenschaften)
//! 24   reserved:u16              (=0, signiert)
//! 26   program_id:u32
//! 30   version:u32
//! 34   binary_hash:[u8;32]       (SHA-256 des ELF — bindet das Zertifikat FEST an genau dies Binary)
//! 66   manifest_hash:[u8;32]     (SHA-256 des Manifests)
//! 98   key_id:[u8;16]            (128-bit-Fingerprint = SHA-256(pubkey)[..16])
//! 114  unsafe_status:u32         (Bitflags PROGRAM_FORBID|PROJECT_CLEAN|ALLOWLIST_OK; muss ALL_PASS)
//! 118  unsafe_audit_hash:[u8;32] (SHA-256 des vollstaendigen Unsafe-Audit-Berichts -> bindet ihn)
//! 150  build_info_len:u16
//! 152  build_info:[..]           (UTF-8: rustc/toolchain/target/profil/zeitstempel)
//! --- signierte Nachricht endet (msg_len = 152 + build_info_len) ---
//! msg_len  signature:[..]        (variabel; Algorithmus laut signature_algorithm_id, Ed25519 = 64 B)
//! ```
//! Die **variable** Signaturlänge + `signature_algorithm_id`/`certificate_policy_id` erlauben künftige
//! Krypto-/Policy-Wechsel **ohne Strukturänderung**. Jedes Feld ist kryptographisch geschützt — das
//! Zertifikat bezeugt „nach **genau diesem** Verfahren + dieser Policy + diesem Algorithmus erzeugt".

use crate::LoaderError;

/// Magic ("TSC1").
pub const CERT_MAGIC: u32 = 0x5453_4331;
/// Aktuell unterstützte **Zertifikatsformat**-Version (vom Kernel geprüft).
pub const CERT_FORMAT_VERSION: u16 = 1;
/// Aktuelle **Signaturformat**-Version. Vom Tool gestempelt.
pub const SIG_FORMAT_VERSION: u16 = 1;
/// **Signaturalgorithmus-ID**: Ed25519 (RFC 8032). Künftige Verfahren = weitere IDs.
pub const SIG_ALG_ED25519: u16 = 1;
/// Erwartete Signaturlänge für [`SIG_ALG_ED25519`].
pub const SIG_ED25519_LEN: usize = 64;
/// Aktuelle **Compiler-/Buildregel**-Version. Vom Tool gestempelt.
pub const BUILD_RULES_VERSION: u16 = 1;
/// Aktuelle **TrustedSAS-Audit-Protokoll**-Version. Vom Tool gestempelt.
pub const AUDIT_PROTOCOL_VERSION: u16 = 1;
/// Aktuelle **Unsafe-Prüfregel**-Version. Vom Tool gestempelt.
pub const UNSAFE_RULES_VERSION: u16 = 1;
/// Aktuelle **Allowlist-Regel**-Version. Vom Tool gestempelt.
pub const ALLOWLIST_RULES_VERSION: u16 = 1;

// Bekannte `certificate_policy_id`-Werte (der Kernel erzwingt sie zunächst nicht, sie sind aber
// signiert + können später per Policy geprüft werden).
/// TrustedSAS-Standardpolitik v1.
pub const POLICY_TRUSTEDSAS_V1: u32 = 1;
/// TrustedSAS, zusätzlich formal verifiziert.
pub const POLICY_TRUSTEDSAS_FORMAL: u32 = 2;
/// Interne Testzertifikate (Selbsttest).
pub const POLICY_INTERNAL_TEST: u32 = 3;
/// Produktionszertifikate.
pub const POLICY_PRODUCTION: u32 = 4;

/// Länge des festen Kopfteils (bis einschließlich `build_info_len`).
pub const CERT_HEADER_LEN: usize = 152;
/// Kleinste gültige Zertifikatslänge (leere `build_info`, ≥1 Signaturbyte).
pub const CERT_MIN_LEN: usize = CERT_HEADER_LEN + 1;

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
/// [`Self::message`] mit dem über [`Self::key_id`] referenzierten PubKey, Algorithmus laut
/// [`Self::signature_algorithm_id`]) + die Policy erfolgen **außerhalb** (Kernel/`sel4lake-trust`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedCert<'a> {
    pub cert_format_version: u16,
    pub sig_format_version: u16,
    pub signature_algorithm_id: u16,
    pub certificate_policy_id: u32,
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
    /// Ein Zertifikat strikt parsen: korrektes Magic + Zertifikatsformat-Version, `build_info_len`
    /// passt in die Daten, **nicht-leere** Signatur folgt. **Nie** ein Out-of-Bounds/Panic —
    /// fehlerhafte Eingabe → [`LoaderError::BadCert`]. Algorithmus/Policy/Verfahrens-Versionen werden
    /// geparst + (über die Signatur) geschützt; die **Auswertung** (Algorithmus-Wahl + Signaturlänge)
    /// erfolgt im Kernel-Verifier.
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
        let build_info_len = rd_u16(data, 150) as usize;
        // msg_len overflow-sicher (build_info_len <= u16::MAX). Es muss mind. 1 Signaturbyte folgen.
        let msg_len = CERT_HEADER_LEN + build_info_len;
        if data.len() <= msg_len {
            return Err(LoaderError::BadCert);
        }
        let mut binary_hash = [0u8; 32];
        binary_hash.copy_from_slice(&data[34..66]);
        let mut manifest_hash = [0u8; 32];
        manifest_hash.copy_from_slice(&data[66..98]);
        let mut key_id = [0u8; 16];
        key_id.copy_from_slice(&data[98..114]);
        let mut unsafe_audit_hash = [0u8; 32];
        unsafe_audit_hash.copy_from_slice(&data[118..150]);
        Ok(TrustedCert {
            cert_format_version,
            sig_format_version: rd_u16(data, 6),
            signature_algorithm_id: rd_u16(data, 8),
            certificate_policy_id: rd_u32(data, 10),
            build_rules_version: rd_u16(data, 14),
            audit_protocol_version: rd_u16(data, 16),
            unsafe_rules_version: rd_u16(data, 18),
            allowlist_rules_version: rd_u16(data, 20),
            flags: rd_u16(data, 22),
            program_id: rd_u32(data, 26),
            version: rd_u32(data, 30),
            binary_hash,
            manifest_hash,
            key_id,
            unsafe_status: rd_u32(data, 114),
            unsafe_audit_hash,
            build_info: &data[CERT_HEADER_LEN..msg_len],
            message: &data[..msg_len],
            signature: &data[msg_len..],
        })
    }

    /// Die **gesamte** signierte Nachricht `[0..msg_len)` (Eingabe der Signatur-Verifikation).
    pub fn message(&self) -> &'a [u8] {
        self.message
    }
    /// Die Signatur (variabel lang; für Ed25519 [`SIG_ED25519_LEN`] B). Der Verifier prüft Länge +
    /// Algorithmus.
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

    /// Ein gültiges Zertifikat (Ed25519-Algorithmus, `bil` Bytes `build_info`, `sl` Signaturbytes)
    /// bauen (Signatur ist Dummy — der Parser prüft keine Krypto).
    fn good(bil: usize, sl: usize) -> Vec<u8> {
        let msg_len = CERT_HEADER_LEN + bil;
        let mut c = vec![0u8; msg_len + sl];
        c[0..4].copy_from_slice(&CERT_MAGIC.to_le_bytes());
        c[4..6].copy_from_slice(&CERT_FORMAT_VERSION.to_le_bytes());
        c[6..8].copy_from_slice(&SIG_FORMAT_VERSION.to_le_bytes());
        c[8..10].copy_from_slice(&SIG_ALG_ED25519.to_le_bytes());
        c[10..14].copy_from_slice(&POLICY_INTERNAL_TEST.to_le_bytes());
        c[14..16].copy_from_slice(&BUILD_RULES_VERSION.to_le_bytes());
        c[16..18].copy_from_slice(&AUDIT_PROTOCOL_VERSION.to_le_bytes());
        c[18..20].copy_from_slice(&UNSAFE_RULES_VERSION.to_le_bytes());
        c[20..22].copy_from_slice(&ALLOWLIST_RULES_VERSION.to_le_bytes());
        c[26..30].copy_from_slice(&7u32.to_le_bytes()); // program_id
        c[30..34].copy_from_slice(&3u32.to_le_bytes()); // version
        for i in 34..66 {
            c[i] = i as u8; // binary_hash
        }
        for i in 66..98 {
            c[i] = (i + 1) as u8; // manifest_hash
        }
        for i in 98..114 {
            c[i] = (i + 2) as u8; // key_id
        }
        c[114..118].copy_from_slice(&UNSAFE_ALL_PASS.to_le_bytes());
        for i in 118..150 {
            c[i] = (i + 3) as u8; // unsafe_audit_hash
        }
        c[150..152].copy_from_slice(&(bil as u16).to_le_bytes());
        for i in 0..bil {
            c[CERT_HEADER_LEN + i] = b'B';
        }
        for i in 0..sl {
            c[msg_len + i] = (i + 5) as u8;
        }
        c
    }

    #[test]
    fn parse_roundtrip() {
        let c = good(24, SIG_ED25519_LEN);
        let t = TrustedCert::parse(&c).unwrap();
        assert_eq!(t.cert_format_version, 1);
        assert_eq!(t.signature_algorithm_id, SIG_ALG_ED25519);
        assert_eq!(t.certificate_policy_id, POLICY_INTERNAL_TEST);
        assert_eq!(t.sig_format_version, SIG_FORMAT_VERSION);
        assert_eq!(t.build_rules_version, BUILD_RULES_VERSION);
        assert_eq!(t.audit_protocol_version, AUDIT_PROTOCOL_VERSION);
        assert_eq!(t.unsafe_rules_version, UNSAFE_RULES_VERSION);
        assert_eq!(t.allowlist_rules_version, ALLOWLIST_RULES_VERSION);
        assert_eq!(t.program_id, 7);
        assert_eq!(t.version, 3);
        assert!(t.unsafe_all_pass());
        assert_eq!(t.binary_hash[0], 34);
        assert_eq!(t.build_info(), &[b'B'; 24]);
        assert_eq!(t.message().len(), CERT_HEADER_LEN + 24);
        assert_eq!(t.signature().len(), SIG_ED25519_LEN);
        assert_eq!(t.message(), &c[..CERT_HEADER_LEN + 24]);
        assert_eq!(t.signature(), &c[CERT_HEADER_LEN + 24..]);
    }

    #[test]
    fn parse_empty_build_info_ok() {
        let c = good(0, SIG_ED25519_LEN);
        let t = TrustedCert::parse(&c).unwrap();
        assert_eq!(t.build_info().len(), 0);
    }

    #[test]
    fn variable_signature_length_accepted_by_parser() {
        // Der Parser ist algorithmus-agnostisch: er akzeptiert jede nicht-leere Signaturlänge; die
        // Längenpruefung (64 fuer Ed25519) macht der Kernel-Verifier.
        let c1 = good(0, 1);
        assert_eq!(TrustedCert::parse(&c1).unwrap().signature().len(), 1);
        let c2 = good(8, 96);
        assert_eq!(TrustedCert::parse(&c2).unwrap().signature().len(), 96);
    }

    #[test]
    fn unsafe_status_partial_is_not_all_pass() {
        let mut c = good(0, SIG_ED25519_LEN);
        c[114..118].copy_from_slice(&(UNSAFE_PROGRAM_FORBID | UNSAFE_PROJECT_CLEAN).to_le_bytes());
        assert!(!TrustedCert::parse(&c).unwrap().unsafe_all_pass());
    }

    #[test]
    fn empty_signature_rejected() {
        let c = good(0, 0); // keine Signaturbytes
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn build_info_len_overruns_rejected() {
        let mut c = good(8, SIG_ED25519_LEN);
        // behauptet eine riesige build_info -> msg_len >= data.len() -> abgelehnt
        c[150..152].copy_from_slice(&60000u16.to_le_bytes());
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn too_short_rejected() {
        assert_eq!(TrustedCert::parse(&[]).unwrap_err(), LoaderError::BadCert);
        assert_eq!(
            TrustedCert::parse(&[0u8; CERT_HEADER_LEN]).unwrap_err(),
            LoaderError::BadCert
        );
    }

    #[test]
    fn bad_magic_rejected() {
        let mut c = good(0, SIG_ED25519_LEN);
        c[0] ^= 0xFF;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }

    #[test]
    fn bad_cert_format_version_rejected() {
        let mut c = good(0, SIG_ED25519_LEN);
        c[4] = 0xEE;
        assert_eq!(TrustedCert::parse(&c).unwrap_err(), LoaderError::BadCert);
    }
}

// Formale Verifikation (Tier 1, ADR/Analyse `ARMTest/formale-verifikation-aufwand.md`): bounded
// Model Checking mit **Kani**. Nur unter `cargo kani` (`cfg(kani)`) kompiliert — im Normal-Build
// vollständig inert (kein Einfluss auf Kernel/Tests). Hebt die bisher nur **gefuzzte** Aussage
// „panik-frei, bounds-geprüft" auf einen **Beweis** für JEDE Eingabe bis `MAXLEN`.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    // Obergrenze der symbolischen Eingabe. WICHTIG: 260 reicht, um JEDEN Code-Pfad zu erreichen — ein
    // Cert, das über den `build_info_len`-Check hinaus geparst wird, verlangt `msg_len < len`, also
    // `build_info_len < len-152 <= 108`; größere `build_info_len` (bis u16::MAX) lösen IMMER den
    // frühen `BadCert`-Rücksprung aus (kein Panik-Pfad braucht len > 260). Der Beweis ist damit für
    // diesen Parser effektiv vollständig, nicht bloß „bis 260".
    const MAXLEN: usize = 260;

    /// **BEWEIS:** `TrustedCert::parse` paniert/OOBt **nie** — für beliebige Bytes + beliebige Länge
    /// (≤ MAXLEN). Adversariale/verstümmelte Zertifikate können den Parser nicht zum Absturz bringen.
    #[kani::proof]
    #[kani::unwind(4)]
    fn parse_never_panics() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        let _ = TrustedCert::parse(&data[..len]);
    }

    /// **BEWEIS:** Bei Erfolg partitionieren `message()` + `signature()` die Eingabe **exakt**
    /// (`msg_len + sig_len == len`), die Signatur ist **nicht leer**, und
    /// `message().len() == CERT_HEADER_LEN + build_info().len()` — die strukturelle Korrektheit, auf
    /// die sich der Kernel-Verifier (`verify_image`) verlässt.
    #[kani::proof]
    #[kani::unwind(4)]
    fn parse_partitions_input() {
        let data: [u8; MAXLEN] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= MAXLEN);
        if let Ok(c) = TrustedCert::parse(&data[..len]) {
            assert!(c.message().len() + c.signature().len() == len);
            assert!(!c.signature().is_empty());
            assert!(c.message().len() == CERT_HEADER_LEN + c.build_info().len());
            assert!(c.message().len() >= CERT_HEADER_LEN);
        }
    }
}
