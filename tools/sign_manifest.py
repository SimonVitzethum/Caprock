#!/usr/bin/env python3
"""SEL4Lake — System-Manifest bauen + signieren (A-1.2 bis A-1.4).

Das Manifest legt die **gesamte Anfangsverteilung von Autoritaet** fest: welche Komponente mit
welchem erwarteten Hash in welche Domaene geladen wird, welche Anfangs-Caps sie bekommt und unter
welcher Politik sie laeuft. Wer die Datei tauschen kann, besitzt sonst die Maschine -- deshalb wird
sie signiert, und die Signatur ist an das Kernel-Image gebunden (`--kernel`).

Format: crates/sel4lake-loader/src/manifest.rs (eingefroren, feste Feldbreiten, Little-Endian).
Signiert wird die **gesamte** Nachricht `[0..msg_len)`, also Kopf UND alle Eintraege.

Aufruf:
  tools/sign_manifest.py --kernel <kernel.elf> --key keys/manifest-test.manifest.ed25519 \\
      --manifest-version 1 --out build/system.manifest \\
      --entry ID:NAME:DOMAIN:IFACE:BLOB[:CAPS[:POLICY[:PRIO[:NUMA[:AFFINITY[:BUDGET_US]]]]]] ...

  DOMAIN   0=TrustedSAS 1=HardwareLand 2=UserLand
  BLOB     Pfad zum Modul -- daraus wird der erwartete SHA-256 berechnet
  CAPS     Komma-Liste aus: loader,pdctl,ntfn,ep,mmio,irq,dma   (leer = keine)
  POLICY   Komma-Liste aus: stripe,root,pinned,nohotreload      (leer = keine)
  AFFINITY Kern-Nummer oder 'any'
"""
import argparse
import hashlib
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from kernel_hash import kernel_code_hash  # noqa: E402

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
except ImportError:  # pragma: no cover
    sys.stderr.write(
        "sign_manifest: das Python-Paket 'cryptography' fehlt.\n"
        "  pip3 install --user cryptography   (oder --break-system-packages auf Debian)\n"
    )
    raise SystemExit(3)

# Muss EXAKT zu crates/sel4lake-loader/src/manifest.rs passen (eingefrorenes Format).
MAGIC = 0x534C_4B4D
FORMAT_VERSION = 1
SIG_ALG_ED25519 = 1
HEADER_LEN = 80
ENTRY_LEN = 96
MAX_ENTRIES = 64
ANY_CORE = 0xFFFF_FFFF

CAPS = {
    "loader": 1 << 0,
    "pdctl": 1 << 1,
    "ntfn": 1 << 2,
    "ep": 1 << 3,
    "mmio": 1 << 4,
    "irq": 1 << 5,
    "dma": 1 << 6,
}
POLICIES = {
    "stripe": 1 << 0,
    "root": 1 << 1,
    "pinned": 1 << 2,
    "nohotreload": 1 << 3,
}


def bits(spec: str, table: dict, what: str) -> int:
    v = 0
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if part not in table:
            raise SystemExit(
                f"sign_manifest: unbekanntes {what} '{part}' (bekannt: {', '.join(sorted(table))})"
            )
        v |= table[part]
    return v


def parse_entry(spec: str):
    f = spec.split(":")
    if len(f) < 5:
        raise SystemExit(
            f"sign_manifest: ungueltiger --entry '{spec}' "
            "(erwartet ID:NAME:DOMAIN:IFACE:BLOB[:CAPS[:POLICY[:PRIO[:NUMA[:AFFINITY[:BUDGET_US]]]]]])"
        )

    def opt(i, default):
        return f[i] if len(f) > i and f[i] != "" else default

    with open(f[4], "rb") as fh:
        blob = fh.read()
    affinity = opt(9, "any")
    return {
        "program_id": int(f[0]),
        "name": f[1].encode()[:16],
        "domain": int(f[2]),
        "iface": int(f[3]),
        "sha256": hashlib.sha256(blob).digest(),
        "caps": bits(opt(5, ""), CAPS, "Cap"),
        "policy": bits(opt(6, ""), POLICIES, "Politik-Flag"),
        "prio": int(opt(7, "1")),
        "numa": int(opt(8, "0")),
        "affinity": ANY_CORE if affinity == "any" else int(affinity),
        "budget": int(opt(10, "0")),
    }


def build_message(kernel_hash: bytes, key_id: bytes, manifest_version: int, entries) -> bytes:
    if len(entries) > MAX_ENTRIES:
        raise SystemExit(f"sign_manifest: mehr als {MAX_ENTRIES} Eintraege")
    roots = [e for e in entries if e["policy"] & POLICIES["root"]]
    if len(roots) != 1:
        # Frueh und laut: der Kernel wuerde es ebenfalls abweisen (manifest_audit Code 5), aber
        # erst beim Booten -- und ein Fehler, der erst in QEMU auffaellt, kostet eine Runde mehr.
        raise SystemExit(
            f"sign_manifest: genau EIN Eintrag muss 'root' tragen, gefunden: {len(roots)}"
        )
    buf = bytearray(HEADER_LEN + len(entries) * ENTRY_LEN)
    struct.pack_into("<IHHIII", buf, 0, MAGIC, FORMAT_VERSION, SIG_ALG_ED25519, 0,
                     manifest_version, len(entries))
    struct.pack_into("<I", buf, 20, ENTRY_LEN)
    buf[24:56] = kernel_hash
    buf[56:72] = key_id
    for i, e in enumerate(entries):
        b = HEADER_LEN + i * ENTRY_LEN
        buf[b:b + len(e["name"])] = e["name"]
        struct.pack_into("<IIII", buf, b + 16, e["program_id"], e["domain"], e["iface"], e["caps"])
        buf[b + 32:b + 64] = e["sha256"]
        struct.pack_into("<IIIII", buf, b + 64, e["policy"], e["numa"], e["affinity"],
                         e["prio"], e["budget"])
    return bytes(buf)


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--kernel", required=True, help="Kernel-ELF, an das gebunden wird")
    ap.add_argument("--key", required=True, help="privater Ed25519-Schluessel (32 B raw)")
    ap.add_argument("--manifest-version", type=int, default=1)
    ap.add_argument("--entry", action="append", default=[], help="s. Moduldoku")
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)

    with open(a.key, "rb") as f:
        raw = f.read()
    if len(raw) != 32:
        raise SystemExit(f"sign_manifest: {a.key} ist kein 32-Byte-Ed25519-Privatschluessel")
    sk = Ed25519PrivateKey.from_private_bytes(raw)
    pub = sk.public_key().public_bytes_raw()
    kid = hashlib.sha256(pub).digest()[:16]

    khash = kernel_code_hash(a.kernel)
    entries = [parse_entry(s) for s in a.entry]
    msg = build_message(khash, kid, a.manifest_version, entries)
    sig = sk.sign(msg)

    with open(a.out, "wb") as f:
        f.write(msg + sig)
    sys.stderr.write(
        f"sign_manifest: {a.out} ({len(entries)} Eintrag/Eintraege, {len(msg) + len(sig)} Bytes)\n"
        f"  Kernel-Code-Hash {khash.hex()[:16]}...  Key-ID {kid.hex()}\n"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
