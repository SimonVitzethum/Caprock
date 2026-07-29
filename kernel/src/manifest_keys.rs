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
    TrustedKey { key_id: [0x44, 0xff, 0x9e, 0xd6, 0xe1, 0x3c, 0x10, 0xb7, 0x54, 0x3f, 0x74, 0x16, 0x90, 0x09, 0x6a, 0xa2], pubkey: [0x84, 0xf5, 0x49, 0xeb, 0x17, 0x1f, 0xaf, 0x99, 0x8c, 0x02, 0xf7, 0x3d, 0x81, 0xcc, 0x44, 0x6d, 0xdd, 0xb3, 0x40, 0xd2, 0x23, 0x59, 0xac, 0x2a, 0x2c, 0x7b, 0xc3, 0x8e, 0x6c, 0xe5, 0x85, 0x8e], revoked: false },
];

/// Anti-Downgrade: kleinste akzeptierte `manifest_version`. Firmware-gepflegt; ein Manifest
/// darunter wird abgewiesen, auch wenn seine Signatur stimmt -- sonst waere ein Rueckspielen
/// einer alten, gueltig signierten Zuteilung ein legitimer Weg zurueck zu alter Autoritaet.
pub const MIN_MANIFEST_VERSION: u32 = 1;
