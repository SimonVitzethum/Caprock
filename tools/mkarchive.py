#!/usr/bin/env python3
# Boot-Archiv-Assembler (ext-26, Host-Tool — KEIN Kernelcode).
#
# Baut `boot-archive.bin` aus extern gebauten Programm-Blobs + Manifesten. Das Archiv wird per
# QEMU `-device loader,file=...,addr=MOD_BASE` in das reservierte RAM-Fenster geladen; der Kernel
# liest es mit dem `sel4lake-loader`-Parser. Format siehe crates/sel4lake-loader/src/lib.rs.
#
# Aufruf:
#   mkarchive.py OUT NAME:DOMAIN:BLOB[:MANIFEST] [NAME:DOMAIN:BLOB[:MANIFEST] ...]
#   DOMAIN: 0=TrustedSAS 1=HardwareLand 2=UserLand
# Beispiel:
#   mkarchive.py build/boot-archive.bin hello:2:build/hello.elf:build/hello.manifest
import struct
import sys

MAGIC = 0x534C4B41
VERSION = 1
HEADER_LEN = 32
ENTRY_LEN = 96


def main(argv):
    if len(argv) < 2:
        sys.stderr.write(__doc__ or "usage: mkarchive.py OUT [NAME:DOMAIN:BLOB[:MANIFEST] ...]\n")
        return 2
    out = argv[1]
    specs = argv[2:]
    entries = []  # (name, domain, blob_bytes, manifest_bytes)
    for s in specs:
        parts = s.split(":")
        if len(parts) < 3:
            sys.stderr.write(f"mkarchive: ungueltige Spec '{s}'\n")
            return 2
        name, domain, blobpath = parts[0], int(parts[1]), parts[2]
        manpath = parts[3] if len(parts) > 3 else None
        with open(blobpath, "rb") as f:
            blob = f.read()
        man = b""
        if manpath:
            with open(manpath, "rb") as f:
                man = f.read()
        entries.append((name, domain, blob, man))

    count = len(entries)
    table_end = HEADER_LEN + count * ENTRY_LEN
    payload = bytearray()
    spans = []  # (blob_off, blob_len, man_off, man_len)
    for (_, _, blob, man) in entries:
        bo = table_end + len(payload)
        payload += blob
        mo = table_end + len(payload)
        payload += man
        spans.append((bo, len(blob), mo, len(man)))
    total = table_end + len(payload)

    buf = bytearray(table_end)
    struct.pack_into("<IIII", buf, 0, MAGIC, VERSION, count, total)
    # reserved[4] bleibt 0
    for i, ((name, domain, _, _), (bo, bl, mo, ml)) in enumerate(zip(entries, spans)):
        base = HEADER_LEN + i * ENTRY_LEN
        nb = name.encode()[:16]
        buf[base:base + len(nb)] = nb
        struct.pack_into("<IIIIII", buf, base + 16, bo, bl, mo, ml, domain, 0)
        # hash[32] (base+40..72) + reserved[6] (72..96) bleiben 0
    buf += payload

    with open(out, "wb") as f:
        f.write(buf)
    sys.stderr.write(f"mkarchive: {out} ({count} Modul(e), {total} Bytes)\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
