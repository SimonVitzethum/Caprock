#!/usr/bin/env python3
"""Caprock — Treiber-Eintrag für LXPD-Images bauen + prüfen (Boot-Modul oder Platte).

Der Eintrag bindet EIN Treiber-Image an seine Herkunft, seinen Bytes-Hash und einen
Manifest-Root-Schlüssel. Geprüft wird er von `crates/caprock-lxpd/src/driver.rs` (reine
Byte-Arithmetik, host-testbar); dieses Werkzeug ist die Bau-Seite dazu — klein, nur
Standardbibliothek, wie `mkarchive.py`.

Herkunft (`--source`, genau eine):

  boot:INDEX          N-tes Multiboot-Modul des Bootloaders
  guid:HEX            GPT-Unique-GUID der Partition (32 Hex-Zeichen, Bindestriche ok)
  range:START:LEN     Blockbereich [START, START+LEN), LEN > 0, ohne Überlauf

Bildbindung: `--image` → SHA-256 der exakten Bytes (FNV wäre gegen einen Angreifer, der
die Platte beschreiben darf, eine Attrappe — s. driver.rs-Moduldoku).

Schlüsselbindung: `--key` ist der rohe 32-Byte-Pubkey (wie `keys/*.manifest.pub` aus
`tools/gen_manifest_key.py`). `key_id = SHA-256(Pubkey)[..16]` — exakt `key_id()` dort und
`kid` in `tools/sign_manifest.py`. Der Zeuge ist `FNV-1a-64-Hex(Roh-Pubkey || Kanonik)`,
Kanonik = Objekt ohne `signature`, Schlüssel sortiert, kompakt (`json.dumps(sort_keys=True,
separators=(",", ":"))`); der Rust-Prüfer stellt dieselbe Kanonik aus jeder Formatierung
wieder her.

Aufruf (bauen):

  tools/lx_driver_manifest.py --driver e1000e --api X1 --source boot:0 \\
      --image build/drv.lxpd --key keys/manifest-test.manifest.pub --out build/drv.entry

Aufruf (prüfen — Zeuge + Bild, Austritt 0/1):

  tools/lx_driver_manifest.py --check build/drv.entry --image build/drv.lxpd \\
      --key keys/manifest-test.manifest.pub
"""
import argparse
import hashlib
import json
import sys

FNV_OFFSET = 0xCBF29CE4842225C5
FNV_PRIME = 0x100000001B3
SCHEMA_VERSION = 1
U64MAX = 0xFFFF_FFFF_FFFF_FFFF


def fnv1a64(data: bytes) -> int:
    h = FNV_OFFSET
    for b in data:
        h ^= b
        h = (h * FNV_PRIME) & 0xFFFF_FFFF_FFFF_FFFF
    return h


def parse_source(spec: str) -> dict:
    """Die Herkunftsangabe lesen — genau eine Form, sonst Abbruch mit Grund."""
    if spec.startswith("boot:"):
        try:
            index = int(spec[len("boot:"):], 10)
        except ValueError:
            raise SystemExit(f"lx_driver_manifest: Modul-Index in '{spec}' ist keine Zahl")
        if not 0 <= index <= 0xFFFF_FFFF:
            raise SystemExit(f"lx_driver_manifest: Modul-Index {index} ausserhalb u32")
        return {"kind": "boot", "module": index}
    if spec.startswith("guid:"):
        raw = spec[len("guid:"):].replace("-", "").lower()
        if len(raw) != 32 or any(c not in "0123456789abcdef" for c in raw):
            raise SystemExit(
                f"lx_driver_manifest: GUID in '{spec}' ist kein 32-Zeichen-Hex"
            )
        if raw == "0" * 32:
            raise SystemExit(
                "lx_driver_manifest: Null-GUID = unbenutzter GPT-Eintrag, keine Herkunft"
            )
        return {"kind": "disk", "part_guid": raw}
    if spec.startswith("range:"):
        parts = spec[len("range:"):].split(":")
        if len(parts) != 2:
            raise SystemExit(
                f"lx_driver_manifest: Bereich '{spec}' — erwartet range:START:LEN"
            )
        try:
            start, length = int(parts[0], 10), int(parts[1], 10)
        except ValueError:
            raise SystemExit(f"lx_driver_manifest: Bereich '{spec}' enthaelt keine Zahlen")
        if length <= 0:
            raise SystemExit("lx_driver_manifest: Bereichslaenge muss > 0 sein")
        if not 0 <= start <= U64MAX or not length <= U64MAX:
            raise SystemExit(f"lx_driver_manifest: Bereich '{spec}' ausserhalb u64")
        if start + length > U64MAX + 1:
            # Halboffen [start, start+len): das Ende darf genau 2^64 sein, nicht mehr.
            raise SystemExit(f"lx_driver_manifest: Bereich '{spec}' laeuft ueber")
        return {"kind": "disk", "sectors": length, "start_lba": start}
    raise SystemExit(
        f"lx_driver_manifest: unbekannte Herkunft '{spec}' "
        "(erwartet boot:INDEX, guid:HEX oder range:START:LEN)"
    )


def read_pubkey(path: str) -> bytes:
    with open(path, "rb") as f:
        pub = f.read()
    if len(pub) != 32:
        raise SystemExit(f"lx_driver_manifest: {path} ist kein 32-Byte-Ed25519-PubKey")
    return pub


def build(args) -> int:
    if not args.driver or not args.api:
        raise SystemExit("lx_driver_manifest: --driver und --api duerfen nicht leer sein")
    source = parse_source(args.source)
    with open(args.image, "rb") as f:
        image = f.read()
    pub = read_pubkey(args.key)
    kid = hashlib.sha256(pub).digest()[:16].hex()
    body = {
        "api_version": args.api,
        "driver": args.driver,
        "image_hash": hashlib.sha256(image).hexdigest(),
        "key_id": kid,
        "schema_version": SCHEMA_VERSION,
        "source": source,
    }
    canonical = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["signature"] = format(fnv1a64(pub + canonical), "016x")
    with open(args.out, "w") as f:
        f.write(json.dumps(body, sort_keys=True, separators=(",", ":")) + "\n")
    sys.stderr.write(
        f"lx_driver_manifest: {args.out} (Treiber '{args.driver}', "
        f"Herkunft {args.source}, Bild {len(image)} B, Key-ID {kid})\n"
    )
    return 0


def check(args) -> int:
    """Eintrag gegen Zeuge + Bild prüfen — dieselben Vergleiche wie driver.rs."""
    with open(args.check, "r") as f:
        try:
            body = json.load(f)
        except json.JSONDecodeError as e:
            sys.stderr.write(f"lx_driver_manifest: PRUEFUNG FEHLGESCHLAGEN — kein JSON ({e})\n")
            return 1
    with open(args.image, "rb") as f:
        image = f.read()
    pub = read_pubkey(args.key)
    if hashlib.sha256(pub).digest()[:16].hex() != body.get("key_id"):
        sys.stderr.write(
            "lx_driver_manifest: PRUEFUNG FEHLGESCHLAGEN — key_id passt nicht zum Schlüssel\n"
        )
        return 1
    sig = body.get("signature")
    body_nosig = {k: v for k, v in body.items() if k != "signature"}
    canonical = json.dumps(body_nosig, sort_keys=True, separators=(",", ":")).encode()
    if sig != format(fnv1a64(pub + canonical), "016x"):
        sys.stderr.write(
            "lx_driver_manifest: PRUEFUNG FEHLGESCHLAGEN — Zeuge stimmt nicht\n"
        )
        return 1
    if hashlib.sha256(image).hexdigest() != body.get("image_hash"):
        sys.stderr.write(
            "lx_driver_manifest: PRUEFUNG FEHLGESCHLAGEN — Bild-Hash stimmt nicht\n"
        )
        return 1
    sys.stderr.write("lx_driver_manifest: PRUEFUNG BESTANDEN\n")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--driver", default="")
    ap.add_argument("--api", default="")
    ap.add_argument("--source", default="")
    ap.add_argument("--image", default="")
    ap.add_argument("--key", default="")
    ap.add_argument("--out", default="")
    ap.add_argument("--check", default="", help="Eintragsdatei prüfen statt bauen")
    a = ap.parse_args(argv)
    if a.check:
        if not (a.image and a.key):
            raise SystemExit("lx_driver_manifest: --check braucht --image und --key")
        return check(a)
    if not (a.driver and a.api and a.source and a.image and a.key and a.out):
        raise SystemExit(
            "lx_driver_manifest: bauen braucht --driver --api --source --image --key --out"
        )
    return build(a)


if __name__ == "__main__":
    sys.exit(main())
