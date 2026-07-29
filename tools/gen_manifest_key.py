#!/usr/bin/env python3
"""SEL4Lake — Manifest-Root-Schluessel erzeugen + Kernel-Key-DB generieren (A-1.3).

Das System-Manifest ist ein **anderes** Autoritaetsdokument als ein TrustedSAS-Zertifikat
(ADR 0014): das Zertifikat bezeugt, wie ein Binary gebaut wurde, das Manifest, wer beim Start
welche Autoritaet bekommt. Zwei Aussagen, zwei Schluesselmengen -- wer Binaries zertifizieren darf,
darf deshalb noch lange nicht die Zuteilung der ganzen Maschine festlegen.

Der **private** Schluessel bleibt unter `keys/` (gitignored, NIE im Repo). Der **oeffentliche** +
seine Key-ID (= SHA-256(pubkey)[..16]) landen in `kernel/src/manifest_keys.rs` und damit im
Kernel-Image: aenderbar nur per Neubau, nicht per Syscall.

Aufruf:
  tools/gen_manifest_key.py [--name manifest-test]   # Paar erzeugen (falls fehlend) + DB schreiben
  tools/gen_manifest_key.py --regen-db               # nur die DB aus keys/*.manifest.pub neu schreiben
  tools/gen_manifest_key.py --ensure                 # wie oben, aber still, wenn schon alles da ist
"""
import argparse
import glob
import hashlib
import os
import sys

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives import serialization
except ImportError:  # pragma: no cover - Umgebungsfrage, keine Logik
    sys.stderr.write(
        "gen_manifest_key: das Python-Paket 'cryptography' fehlt.\n"
        "  pip3 install --user cryptography   (oder --break-system-packages auf Debian)\n"
    )
    raise SystemExit(3)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KEYS_DIR = os.path.join(ROOT, "keys")
DB_FILE = os.path.join(ROOT, "kernel", "src", "manifest_keys.rs")


def raw_priv(sk):
    return sk.private_bytes(
        serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()
    )


def raw_pub(sk):
    return sk.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


def key_id(pub: bytes) -> bytes:
    return hashlib.sha256(pub).digest()[:16]


def rust_bytes(b: bytes) -> str:
    return "[" + ", ".join(f"0x{x:02x}" for x in b) + "]"


def write_db():
    """`manifest_keys.rs` aus allen `keys/*.manifest.pub` neu schreiben."""
    entries = []
    for pubfile in sorted(glob.glob(os.path.join(KEYS_DIR, "*.manifest.pub"))):
        name = os.path.basename(pubfile)[: -len(".manifest.pub")]
        with open(pubfile, "rb") as f:
            pub = f.read()
        if len(pub) != 32:
            raise SystemExit(f"gen_manifest_key: {pubfile} ist kein 32-Byte-Ed25519-PubKey")
        entries.append((name, key_id(pub), pub))
    lines = [
        "//! AUTOGENERIERT von tools/gen_manifest_key.py -- NICHT von Hand editieren (A-1.3).",
        "//!",
        "//! Read-only Root-Schluessel fuer das **System-Manifest**. Ausschliesslich OEFFENTLICHE",
        "//! Schluessel; in den Kernel kompiliert, NICHT per Syscall aenderbar. Bewusst GETRENNT von",
        "//! `trusted_keys.rs`: ein Zertifikat bezeugt die Herkunft eines Binaries, ein Manifest die",
        "//! Zuteilung der ganzen Maschine. Wer das eine darf, darf nicht automatisch das andere.",
        "",
        "use sel4lake_trust::TrustedKey;",
        "",
        "/// Akzeptierte oeffentliche Manifest-Root-Schluessel (per `key_id` referenziert).",
        "pub static MANIFEST_KEYS: &[TrustedKey] = &[",
    ]
    for name, kid, pub in entries:
        lines.append(f"    // {name}")
        lines.append(
            f"    TrustedKey {{ key_id: {rust_bytes(kid)}, pubkey: {rust_bytes(pub)}, revoked: false }},"
        )
    lines += [
        "];",
        "",
        "/// Anti-Downgrade: kleinste akzeptierte `manifest_version`. Firmware-gepflegt; ein Manifest",
        "/// darunter wird abgewiesen, auch wenn seine Signatur stimmt -- sonst waere ein Rueckspielen",
        "/// einer alten, gueltig signierten Zuteilung ein legitimer Weg zurueck zu alter Autoritaet.",
        "pub const MIN_MANIFEST_VERSION: u32 = 1;",
        "",
    ]
    with open(DB_FILE, "w") as f:
        f.write("\n".join(lines))
    return entries


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", default="manifest-test")
    ap.add_argument("--regen-db", action="store_true", help="nur die DB neu schreiben")
    ap.add_argument("--ensure", action="store_true", help="Paar nur erzeugen, wenn es fehlt")
    a = ap.parse_args(argv)

    os.makedirs(KEYS_DIR, exist_ok=True)
    priv = os.path.join(KEYS_DIR, f"{a.name}.manifest.ed25519")
    pub = os.path.join(KEYS_DIR, f"{a.name}.manifest.pub")

    if not a.regen_db:
        if os.path.exists(priv) and a.ensure:
            pass
        elif os.path.exists(priv) and not a.ensure:
            sys.stderr.write(f"gen_manifest_key: {priv} existiert bereits -- nichts erzeugt\n")
        else:
            sk = Ed25519PrivateKey.generate()
            with open(priv, "wb") as f:
                f.write(raw_priv(sk))
            os.chmod(priv, 0o600)
            with open(pub, "wb") as f:
                f.write(raw_pub(sk))
            sys.stderr.write(f"gen_manifest_key: Paar erzeugt -> {priv} (privat!) + {pub}\n")

    entries = write_db()
    sys.stderr.write(
        f"gen_manifest_key: {DB_FILE} geschrieben ({len(entries)} Schluessel: "
        + ", ".join(n for n, _, _ in entries)
        + ")\n"
    )
    if not entries:
        sys.stderr.write(
            "gen_manifest_key: WARNUNG -- leere Key-DB. Der Kernel kann dann KEIN Manifest\n"
            "  annehmen (manifest_audit meldet Code 1). Das ist die sichere Richtung, aber\n"
            "  vermutlich nicht die gewollte.\n"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
