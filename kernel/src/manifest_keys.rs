//! AUTOGENERIERT von tools/gen_manifest_key.py -- NICHT von Hand editieren (A-1.3).
//!
//! Read-only Root-Schluessel fuer das **System-Manifest**. Ausschliesslich OEFFENTLICHE
//! Schluessel; in den Kernel kompiliert, NICHT per Syscall aenderbar. Bewusst GETRENNT von
//! `trusted_keys.rs`: ein Zertifikat bezeugt die Herkunft eines Binaries, ein Manifest die
//! Zuteilung der ganzen Maschine. Wer das eine darf, darf nicht automatisch das andere.

use sel4lake_trust::TrustedKey;

/// Akzeptierte oeffentliche Manifest-Root-Schluessel (per `key_id` referenziert).
pub static MANIFEST_KEYS: &[TrustedKey] = &[
    // manifest-test
    TrustedKey { key_id: [0x20, 0x9d, 0x7d, 0x3a, 0xb9, 0x5a, 0xc8, 0xcb, 0xe8, 0x40, 0xdd, 0x98, 0xe6, 0x01, 0xf9, 0xfb], pubkey: [0x62, 0x98, 0xda, 0xbf, 0x56, 0x06, 0x09, 0xd2, 0x1e, 0xa4, 0x12, 0xeb, 0x3c, 0x4e, 0x70, 0x39, 0x8b, 0xae, 0x39, 0x6c, 0xb1, 0x14, 0x34, 0x9e, 0x60, 0xa3, 0x79, 0xae, 0x85, 0x63, 0x77, 0x2e], revoked: false },
];

/// Anti-Downgrade: kleinste akzeptierte `manifest_version`. Firmware-gepflegt; ein Manifest
/// darunter wird abgewiesen, auch wenn seine Signatur stimmt -- sonst waere ein Rueckspielen
/// einer alten, gueltig signierten Zuteilung ein legitimer Weg zurueck zu alter Autoritaet.
pub const MIN_MANIFEST_VERSION: u32 = 1;
