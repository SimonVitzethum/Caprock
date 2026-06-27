//! AUTOGENERIERT von tools/gen_trusted_key.py -- NICHT von Hand editieren (ext-28, ADR 0014).
//!
//! Read-only TrustedSAS-Root-Key-DB: ausschliesslich OEFFENTLICHE Schluessel; in den Kernel
//! kompiliert, NICHT per Syscall aenderbar. Erweiterung/Rotation/Revocation nur durch erneutes
//! Generieren + Neukompilieren (Firmware-/Kernel-Update). Private Schluessel liegen NIE hier.

use sel4lake_trust::TrustedKey;

/// Akzeptierte oeffentliche Root-Schluessel (per `key_id` referenziert).
pub static TRUSTED_KEYS: &[TrustedKey] = &[
    // trusted-test
    TrustedKey { key_id: [0x60, 0xed, 0x71, 0xb6, 0x24, 0xda, 0x3f, 0x4d, 0x3b, 0x3a, 0x23, 0x0e, 0x96, 0x63, 0xee, 0x9d], pubkey: [0xcb, 0x61, 0xd4, 0x14, 0xed, 0xf0, 0x8e, 0xaf, 0x7b, 0xfb, 0x9e, 0xb7, 0x8b, 0x0a, 0x19, 0xf7, 0x0f, 0x73, 0x6b, 0x98, 0x58, 0xf8, 0x1d, 0x1d, 0x23, 0x76, 0x6f, 0xe4, 0x10, 0xfc, 0xab, 0xd4], revoked: false },
];

/// Anti-Downgrade: minimal akzeptierte Version je `program_id` (firmware-gepflegt).
/// Leer = keine Untergrenze (jede gueltig signierte Version wird akzeptiert).
pub static MIN_VERSION: &[(u32, u32)] = &[];
