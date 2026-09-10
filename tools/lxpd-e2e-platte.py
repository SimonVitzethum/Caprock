#!/usr/bin/env python3
"""Caprock LXPD-E2E — GPT-Typ-Patch + LXIMG2-Verzeichnis auf eine Lade-Suite-Platte.

Erweitert eine mit `tools/mkgpt.py` gebaute Platte (2 Partitionen, FAT + Magie — die
Lade-Suite sieht davon kein Byte anders), ohne ihre Zusagen zu brechen:

1. Partition 2 bekommt den LXPD-Typ (`"LXPD-CAPROCK-DRV"`, Dienst-Politik aus
   `programs/lxpd-runtime`). Lage und Groesse bleiben — der SCAN des Blockdienstes
   zaehlt weiter 2 belegte Eintraege (`part : ALL PASS` haengt daran), Magie (20001),
   Probe-Sektor (20002) und Checkpoint (32710) liegen weiter, wo die Suite sie sucht.
   Beide GPT-Kopien werden neu versiegelt (Eintrags-CRC + Kopf-CRC).
2. Ab `--lba` liegt ein `LXIMG2`-Verzeichnis (Eintrag + Bild + Manifest, je auf Sektoren
   aufgerundet) — das, was `LadeDienst::{suchen,bild_lesen,pruefen}` liest.

Dokumente (Werte wie in den Dienst-Tests, damit jede Pruefung greift):
- Eintrag: driver `e1000e`, api `X1`, Herkunft `DiskGuid` (Unique-GUID von Partition 2),
  `image_hash` = SHA-256 des `--image`, `key_id` + Zeuge gegen PUBKEY (32 x 0x42).
- Manifest: die Test-Kanonik (Coverage 100, Grants, IRQ 7, 2 Stubnamen), signiert mit
  lxpd_runtime-Schluessel `deadbeef` (FNV-1a-64 ueber Key-Bytes + Kanonik, wie `lx-bind`).

Aufruf:
  tools/lxpd-e2e-platte.py --disk build/lxpd-e2e.img --image <treiber.elf> [--lba 21000]

Prueft nach dem Schreiben alles selbst nach (GPT-Signatur/Revision/CRCs beider Koepfe,
Verzeichnis-Magic/Laengen, SHA-256 des Bildes gegen den Eintrag, Zeuge + Manifest-Signatur
per Nachrechnen) und meldet `LXPD-E2E-PLATTE: ALL PASS` — oder bricht mit Grund ab.
"""
import argparse
import hashlib
import json
import struct
import sys
import zlib

SEKTOR = 512
SIG = b"EFI PART"
LXPD_TYP = b"LXPD-CAPROCK-DRV"
assert len(LXPD_TYP) == 16

VERZEICHNIS_MAGIC = b"LXIMG2\0\0"

PUBKEY = bytes([0x42] * 32)
MANIFEST_KEY = "deadbeef"  # wie `b"deadbeef"` im Dienst (Hex sniffend dekodiert)

# Suite-Sektoren, die unberuehrt bleiben muessen (Magie, blkdev-Probe, Checkpoint).
FREI_HALTEN = {20001, 20002, 32710}

# Test-Kanonik aus `programs/lxpd-runtime` (CANON dort) — byte-identisch, sonst faellt die
# Manifest-Signatur im Dienst (der die Kanonik selbst wiederherstellt).
CANON = b'{"api_version":"X1","arch":"x86-64","class_b_objects":[],"coverage_pct":100.0,"dma_window":{"base":0,"size":65536,"bits":64},"driver":"e1000e","gpl_affected":false,"grants_bar":[{"index":0,"base":4096,"size":8192,"flags":"RW"}],"irq_vector":7,"schema_version":1,"trampolines":["dma_map_single->caprock_dma_map","spin_lock->caprock_spin_lock"]}'

FNV_OFFSET = 0xCBF29CE484222325
FNV_PRIME = 0x100000001B3


def fnv1a64(data: bytes) -> int:
    h = FNV_OFFSET
    for b in data:
        h ^= b
        h = (h * FNV_PRIME) & 0xFFFF_FFFF_FFFF_FFFF
    return h


def key_bytes(key: str) -> bytes:
    s = key.strip().encode()
    if len(s) % 2 == 0 and all(c in b"0123456789abcdefABCDEF" for c in s):
        return bytes(int(s[i:i + 2], 16) for i in range(0, len(s), 2))
    return key.encode()


def parse_header(sektor: bytes):
    assert sektor[0:8] == SIG, "keine GPT-Signatur"
    assert struct.unpack_from("<I", sektor, 8)[0] == 0x00010000, "fremde Revision"
    (hsize,) = struct.unpack_from("<I", sektor, 12)
    want, = struct.unpack_from("<I", sektor, 16)
    assert zlib.crc32(sektor[:16] + b"\0\0\0\0" + sektor[20:hsize]) & 0xFFFFFFFF == want, \
        "Kopf-CRC kaputt"
    my, alt, first, last = struct.unpack_from("<QQQQ", sektor, 24)
    entry_lba, = struct.unpack_from("<Q", sektor, 72)
    num, esz = struct.unpack_from("<II", sektor, 80)
    ecrc, = struct.unpack_from("<I", sektor, 88)
    return {"hsize": hsize, "my": my, "alt": alt, "entry_lba": entry_lba,
            "num": num, "esz": esz, "ecrc": ecrc}


def read_entries(f, h):
    f.seek(h["entry_lba"] * SEKTOR)
    return bytearray(f.read(h["num"] * h["esz"]))


def haupt(args) -> int:
    bild = open(args.image, "rb").read()
    if not 1 <= len(bild) <= 65536:
        sys.exit(f"lxpd-e2e-platte: Bild {len(bild)} B ausserhalb 1..=65536")
    if len(bild) > 8192:
        sys.exit(f"lxpd-e2e-platte: Bild {len(bild)} B passt nicht ins 8-KiB-Shared-Fenster")
    pub = PUBKEY
    kid = hashlib.sha256(pub).digest()[:16].hex()
    img_hash = hashlib.sha256(bild).hexdigest()

    with open(args.disk, "r+b") as f:
        f.seek(SEKTOR)
        h = parse_header(f.read(SEKTOR))
        entries = read_entries(f, h)
        assert h["num"] >= 2 and h["esz"] == 128, \
            f"unerwartete Eintragsliste (num={h['num']}, esz={h['esz']})"
        # Partition 2 (Index 1): belegt? Typ patchen, Unique-GUID lesen.
        o = 1 * h["esz"]
        typ = bytes(entries[o:o + 16])
        assert typ != bytes(16), "Partition 2 unbenutzt — nichts zu patchen"
        first, last = struct.unpack_from("<QQ", entries, o + 32)
        assert (first, last) == (20001, 32700), \
            f"Partition 2 unerwartet ({first}..{last}) — Lade-Suite-Layout?"
        guid = bytes(entries[o + 16:o + 32])
        assert guid != bytes(16), "Partition 2 ohne Unique-GUID"
        entries[o:o + 16] = LXPD_TYP
        # Neu versiegeln: Eintrags-CRC in den Kopf, Kopf-CRC ueber die ersten hsize Bytes.
        ecrc = zlib.crc32(bytes(entries)) & 0xFFFFFFFF
        f.seek(h["entry_lba"] * SEKTOR)
        f.write(entries)
        f.seek(SEKTOR)
        kopf = bytearray(f.read(SEKTOR))
        struct.pack_into("<I", kopf, 88, ecrc)
        struct.pack_into("<I", kopf, 16, 0)
        crc = zlib.crc32(bytes(kopf[:h["hsize"]])) & 0xFFFFFFFF
        struct.pack_into("<I", kopf, 16, crc)
        f.seek(SEKTOR)
        f.write(kopf)
        # Sicherungskopie: Eintraege + Kopf am Plattenende (gleicher Inhalt, eigene Lage).
        n_sec = h["num"] * h["esz"] // SEKTOR
        f.seek(h["alt"] * SEKTOR - n_sec * SEKTOR)
        # Die Lage aus dem Sicherungskopf lesen statt raten (mkgpt legt sie ans Ende).
        f.seek(h["alt"] * SEKTOR)
        bkopf = bytearray(f.read(SEKTOR))
        bent_lba, = struct.unpack_from("<Q", bkopf, 72)
        f.seek(bent_lba * SEKTOR)
        f.write(entries)
        struct.pack_into("<I", bkopf, 88, ecrc)
        struct.pack_into("<I", bkopf, 16, 0)
        bcrc = zlib.crc32(bytes(bkopf[:h["hsize"]])) & 0xFFFFFFFF
        struct.pack_into("<I", bkopf, 16, bcrc)
        f.seek(h["alt"] * SEKTOR)
        f.write(bkopf)

        # Dokumente bauen.
        src = {"kind": "disk", "part_guid": guid.hex()}
        canon_entry = ('{"api_version":"X1","driver":"e1000e","image_hash":"%s",'
                       '"key_id":"%s","schema_version":1,"source":{"kind":"disk",'
                       '"part_guid":"%s"}}') % (img_hash, kid, guid.hex())
        sig_entry = format(fnv1a64(pub + canon_entry.encode()), "016x")
        eintrag = ('{\n  "signature" : "%s",\n  "source" : {"kind":"disk",'
                   '"part_guid":"%s"},\n  "driver" : "e1000e",\n  "image_hash" : "%s",'
                   '\n  "key_id" : "%s",\n  "schema_version" : 1,\n  "api_version" : "X1"\n}'
                   ) % (sig_entry, guid.hex(), img_hash, kid)
        eintrag_b = eintrag.encode()
        sig_man = format(fnv1a64(key_bytes(MANIFEST_KEY) + CANON), "016x")
        man_obj = json.loads(CANON.decode())
        man_obj["signature"] = sig_man
        manifest_b = (json.dumps(man_obj, sort_keys=True, indent=2) + "\n").encode()
        assert 1 <= len(eintrag_b) <= 4096, f"Eintrag {len(eintrag_b)} B"
        assert 1 <= len(manifest_b) <= 8192, f"Manifest {len(manifest_b)} B"
        lba = args.lba
        e_sek = -(-len(eintrag_b) // SEKTOR)
        b_sek = -(-len(bild) // SEKTOR)
        m_sek = -(-len(manifest_b) // SEKTOR)
        braucht = 1 + e_sek + b_sek + m_sek
        belegung = set(range(lba, lba + braucht))
        assert not (belegung & FREI_HALTEN), \
            f"LXIMG2 {lba}..{lba + braucht - 1} trifft Suite-Sektor {sorted(belegung & FREI_HALTEN)}"
        assert lba >= first and lba + braucht - 1 <= last, \
            "LXIMG2 ragt aus Partition 2 heraus"
        verzeichnis = bytearray(SEKTOR)
        verzeichnis[0:8] = VERZEICHNIS_MAGIC
        struct.pack_into("<III", verzeichnis, 8, len(eintrag_b), len(bild), len(manifest_b))
        struct.pack_into("<I", verzeichnis, 20, 0)
        strom = bytearray()
        for dok in (eintrag_b, bild, manifest_b):
            strom += dok
            while len(strom) % SEKTOR:
                strom += b"\0"
        f.seek(lba * SEKTOR)
        f.write(verzeichnis)
        f.write(strom)

        # Nachpruefen: GPT beider Koepfe + Verzeichnis + Dokumente + Krypto.
        f.seek(SEKTOR)
        h1 = parse_header(f.read(SEKTOR))
        e1 = read_entries(f, h1)
        assert zlib.crc32(bytes(e1)) & 0xFFFFFFFF == h1["ecrc"], "Eintrags-CRC (primaer)"
        assert bytes(e1[o:o + 16]) == LXPD_TYP, "Typ-Patch weg?"
        f.seek(h1["alt"] * SEKTOR)
        hb = parse_header(f.read(SEKTOR))
        # Sicherungskopf-Eintraege liegen an seiner entry_lba:
        f.seek(hb["entry_lba"] * SEKTOR)
        eb = f.read(hb["num"] * hb["esz"])
        assert zlib.crc32(eb) & 0xFFFFFFFF == hb["ecrc"], "Eintrags-CRC (Backup)"
        assert eb[o:o + 16] == LXPD_TYP, "Typ-Patch im Backup weg?"
        f.seek(lba * SEKTOR)
        v = f.read(SEKTOR)
        assert v[0:8] == VERZEICHNIS_MAGIC, "Verzeichnis-Magic weg?"
        el, bl, ml, fl = struct.unpack_from("<IIII", v, 8)
        assert (el, bl, ml, fl) == (len(eintrag_b), len(bild), len(manifest_b), 0)
        rest = f.read((e_sek + b_sek + m_sek) * SEKTOR)
        e2 = rest[:el]
        b2 = rest[e_sek * SEKTOR:e_sek * SEKTOR + bl]
        m2 = rest[(e_sek + b_sek) * SEKTOR:(e_sek + b_sek) * SEKTOR + ml]
        assert hashlib.sha256(b2).hexdigest() == img_hash, "Bild-Hash weicht ab"
        assert hashlib.sha256(b2).hexdigest() == json.loads(e2.decode())["image_hash"]
        # Zeuge + Manifest-Signatur nachrechnen (dieselben Formeln wie oben).
        ej = json.loads(e2.decode())
        assert ej["key_id"] == kid, "key_id passt nicht zum Pubkey"
        nosig = {k: v for k, v in ej.items() if k != "signature"}
        canon2 = json.dumps(nosig, sort_keys=True, separators=(",", ":")).encode()
        assert ej["signature"] == format(fnv1a64(pub + canon2), "016x"), "Eintrags-Zeuge falsch"
        mj = json.loads(m2.decode())
        assert mj["signature"] == format(fnv1a64(key_bytes(MANIFEST_KEY) + CANON), "016x"), \
            "Manifest-Signatur falsch"

    n = len(bild)
    print(f"lxpd-e2e-platte: P2-Typ=({LXPD_TYP.decode()}) guid={guid.hex()} "
          f"LXIMG2@{lba} ({braucht} Sektoren: 1+{e_sek}+{b_sek}+{m_sek}) "
          f"Bild {n} B sha256={img_hash[:16]}… key_id={kid[:16]}…", file=sys.stderr)
    print("LXPD-E2E-PLATTE: ALL PASS")
    return 0


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--disk", required=True)
    ap.add_argument("--image", required=True)
    ap.add_argument("--lba", type=int, default=21000)
    sys.exit(haupt(ap.parse_args()))
