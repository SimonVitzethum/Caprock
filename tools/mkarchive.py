#!/usr/bin/env python3
# Boot-Archiv-Assembler (ext-26, Host-Tool — KEIN Kernelcode).
#
# Baut `boot-archive.bin` aus extern gebauten Programm-Blobs + Manifesten. Das Archiv wird per
# QEMU `-device loader,file=...,addr=MOD_BASE` in das reservierte RAM-Fenster geladen; der Kernel
# liest es mit dem `sel4lake-loader`-Parser. Format: crates/sel4lake-loader/src/archive.rs.
#
# Das Boot-Archiv ist nur EINE Quelle (ADR 0011, Verfeinerung 3); jeder Eintrag traegt eine stabile
# numerische program_id + version (Verfeinerung 4).
#
# Aufruf:
#   mkarchive.py OUT ID:NAME:DOMAIN:VERSION:BLOB[:MANIFEST[:CERT]] [ ... ]
#   DOMAIN: 0=TrustedSAS 1=HardwareLand 2=UserLand
#   CERT (ext-28): TrustedSAS-Zertifikat (tools/sign_trusted.py). Leeres Feld = kein Zertifikat;
#   nur TrustedSAS-Module brauchen eines (das verify_image-Gate lehnt unzertifiziertes TrustedSAS ab).
# Beispiel:
#   mkarchive.py build/boot-archive.bin 1:hello:2:1:build/hello.elf:build/hello.manifest
#   mkarchive.py build/boot-archive.bin 12:svc:0:1:build/svc.elf::certs/svc.cert   # Cert, kein Manifest
import struct
import sys

MAGIC = 0x534C4B41
VERSION = 2  # ext-28: Entry-reserved[0..1] -> cert_off/cert_len (0 = kein Zertifikat)
HEADER_LEN = 32
ENTRY_LEN = 96


def main(argv):
    if len(argv) < 2:
        sys.stderr.write("usage: mkarchive.py OUT ID:NAME:DOMAIN:VERSION:BLOB[:MANIFEST] ...\n")
        return 2
    out = argv[1]
    specs = argv[2:]
    entries = []  # (program_id, name, version, domain, blob, manifest, cert)
    for s in specs:
        parts = s.split(":")
        if len(parts) < 5:
            sys.stderr.write(f"mkarchive: ungueltige Spec '{s}' (erwartet ID:NAME:DOMAIN:VERSION:BLOB[:MANIFEST[:CERT]])\n")
            return 2
        pid, name, domain, ver, blobpath = int(parts[0]), parts[1], int(parts[2]), int(parts[3]), parts[4]
        manpath = parts[5] if len(parts) > 5 and parts[5] else None
        certpath = parts[6] if len(parts) > 6 and parts[6] else None
        with open(blobpath, "rb") as f:
            blob = f.read()
        man = b""
        if manpath:
            with open(manpath, "rb") as f:
                man = f.read()
        cert = b""
        if certpath:
            with open(certpath, "rb") as f:
                cert = f.read()
        entries.append((pid, name, ver, domain, blob, man, cert))

    count = len(entries)
    table_end = HEADER_LEN + count * ENTRY_LEN
    payload = bytearray()
    spans = []  # (blob_off, blob_len, man_off, man_len, cert_off, cert_len)
    for (_, _, _, _, blob, man, cert) in entries:
        bo = table_end + len(payload)
        payload += blob
        mo = table_end + len(payload)
        payload += man
        co = table_end + len(payload)
        payload += cert
        spans.append((bo, len(blob), mo, len(man), co, len(cert)))
    total = table_end + len(payload)

    buf = bytearray(table_end)
    struct.pack_into("<IIII", buf, 0, MAGIC, VERSION, count, total)  # reserved[4] bleibt 0
    for i, ((pid, name, ver, domain, _, _, _), (bo, bl, mo, ml, co, cl)) in enumerate(zip(entries, spans)):
        base = HEADER_LEN + i * ENTRY_LEN
        nb = name.encode()[:16]
        buf[base:base + len(nb)] = nb
        # program_id, version, domain, flags(0)
        struct.pack_into("<IIII", buf, base + 16, pid, ver, domain, 0)
        # blob_off, blob_len, manifest_off, manifest_len
        struct.pack_into("<IIII", buf, base + 32, bo, bl, mo, ml)
        # hash[32] (base+48..80) bleibt 0; cert_off/cert_len (80..88, ext-28); reserved[2] (88..96) = 0
        struct.pack_into("<II", buf, base + 80, co, cl)
    buf += payload

    with open(out, "wb") as f:
        f.write(buf)
    sys.stderr.write(f"mkarchive: {out} ({count} Modul(e), {total} Bytes)\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
