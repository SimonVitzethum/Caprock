//! Krypto-Primitive der TrustedSAS-Zertifikatsprüfung (ext-28, [ADR 0014]).
//!
//! Etablierte Krypto (**keine** Eigenentwicklung): **Ed25519**-Verifikation (`ed25519-dalek`) +
//! **SHA-256** (`sha2`) + 128-bit-PubKey-Fingerprint. `no_std` + **no-alloc** (kein globaler Heap
//! im Kernel). **Kein** Signieren (das macht host-seitig das Build-/Signier-Tool) → **kein** RNG
//! nötig. Dieses Crate liefert nur die Primitive + die Key-Typen; die **Policy** (Key-DB-Lookup,
//! Hash-Bindung, Anti-Downgrade, Revocation) liegt im Kernel-Glue (`loader::verify_image`), die
//! read-only Key-DB im Kernel. Die Krypto-Deps liegen damit **ausschließlich kernel-seitig** — die
//! TrustedSAS-**Programm**-Trust-Basis (unsafe-frei) bleibt unberührt.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Ein öffentlicher TrustedSAS-Root-Schlüssel in der **read-only** Kernel-Key-DB (ADR 0014 §4).
/// Privatschlüssel liegen **nie** im Kernel; diese Tabelle ist in den Kernel kompiliert und nur per
/// Firmware-/Kernel-Update änderbar (nicht per Syscall).
#[derive(Clone, Copy)]
pub struct TrustedKey {
    /// 128-bit-Fingerprint = [`fingerprint`]`(pubkey)`.
    pub key_id: [u8; 16],
    /// Ed25519-Public-Key (32 B).
    pub pubkey: [u8; 32],
    /// Zurückgezogen (kompromittiert/rotiert) → alle Zertifikate dieser Key-ID werden abgelehnt.
    pub revoked: bool,
}

/// SHA-256 über `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// 128-bit-Fingerprint eines Public-Keys = `SHA-256(pubkey)[..16]` (ADR 0014: Key-ID,
/// **selbst-zertifizierend** — der Kernel kann beim DB-Laden `key_id == fingerprint(pubkey)` prüfen).
pub fn fingerprint(pubkey: &[u8; 32]) -> [u8; 16] {
    let full = sha256(pubkey);
    let mut id = [0u8; 16];
    id.copy_from_slice(&full[..16]);
    id
}

/// Ed25519-Signatur über `msg` mit `pubkey` prüfen. Rein verifizierend (kein Heap, kein RNG).
/// `false` bei ungültigem Schlüssel **oder** ungültiger Signatur.
///
/// Nutzt **`verify_strict`** (nicht `verify`): lehnt Small-Order-`R`-Komponenten und
/// nicht-kanonische Kodierungen ab → keine Signatur-Malleability an der Vertrauensgrenze (zu einer
/// gegebenen Nachricht existiert keine zweite akzeptierte Signatur).
pub fn verify_sig(pubkey: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let signature = Signature::from_bytes(sig);
    vk.verify_strict(msg, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    fn arr32(s: &str) -> [u8; 32] {
        unhex(s).try_into().unwrap()
    }
    fn arr64(s: &str) -> [u8; 64] {
        unhex(s).try_into().unwrap()
    }

    // RFC 8032, Ed25519 Test 1 (leere Nachricht).
    const PUB: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const SIG: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    #[test]
    fn verify_known_good_vector() {
        let pk = arr32(PUB);
        let sig = arr64(SIG);
        assert!(verify_sig(&pk, b"", &sig), "RFC-8032-Vektor muss verifizieren");
    }

    #[test]
    fn verify_rejects_tampered_sig() {
        let pk = arr32(PUB);
        let mut sig = arr64(SIG);
        sig[10] ^= 0x01; // ein geflipptes Bit
        assert!(!verify_sig(&pk, b"", &sig));
    }

    #[test]
    fn verify_rejects_wrong_message() {
        let pk = arr32(PUB);
        let sig = arr64(SIG);
        assert!(!verify_sig(&pk, b"x", &sig)); // Nachricht != signierte (leere)
    }

    #[test]
    fn verify_rejects_bad_pubkey() {
        // Nicht-kanonischer/ungueltiger Punkt -> from_bytes scheitert -> false.
        let sig = arr64(SIG);
        assert!(!verify_sig(&[0xFFu8; 32], b"", &sig));
    }

    #[test]
    fn fingerprint_is_sha256_prefix() {
        let pk = arr32(PUB);
        let fp = fingerprint(&pk);
        assert_eq!(&fp[..], &sha256(&pk)[..16]);
        assert_eq!(fp.len(), 16);
    }
}
