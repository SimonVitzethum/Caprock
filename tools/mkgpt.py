#!/usr/bin/env python3
# GPT-Plattenabbild bauen (A-6.2, Host-Tool — KEIN Kernelcode).
#
# Warum selbst gebaut und nicht `sgdisk`/`parted` aufgerufen:
#
#  * **Verfuegbarkeit.** Eine Suite, die an einem Fremdwerkzeug haengt, faellt auf einem Rechner
#    ohne dieses Werkzeug aus -- und zwar als "Test rot", nicht als "Aufbau unvollstaendig". Genau
#    diese Verwechslung hat das Projekt schon mehrfach bezahlt.
#  * **Kontrolle.** Der Parser soll auch gegen KAPUTTE Tabellen geprueft werden. Ein Werkzeug, das
#    nur gueltige schreibt, kann den Negativfall nicht herstellen; `--break` unten schon.
#
# Aufruf:
#   mkgpt.py OUT --sectors N [--part FIRST:LAST] ... [--magic-at LBA] [--break WAS]
#   --break: signature | header-crc | entries-crc   (fuer die Gegenprobe)
import argparse
import struct
import sys
import zlib

SECTOR = 512
SIG = b"EFI PART"
REVISION = 0x00010000
HEADER_SIZE = 92
NUM_ENTRIES = 128
ENTRY_SIZE = 128

# Irgendein Typ-GUID != 0 (Linux filesystem data). Der Wert ist gleichgueltig; dass er NICHT null
# ist, ist die Aussage -- lauter Nullen heisst "unbenutzter Eintrag".
TYPE_GUID = bytes.fromhex("af3dc60f8384720e3d69d8477de4")[:14] + b"\x00\x02"


def protective_mbr(disk_sectors):
    """LBA 0: schuetzender MBR. Er sagt einem MBR-Leser 'die Platte ist voll belegt' und
    verhindert damit, dass ein altes Werkzeug die GPT fuer freien Platz haelt."""
    mbr = bytearray(SECTOR)
    e = 446
    mbr[e + 0] = 0x00           # nicht bootbar
    mbr[e + 1 : e + 4] = b"\x00\x02\x00"  # CHS-Anfang (egal, LBA gilt)
    mbr[e + 4] = 0xEE           # Typ: GPT protective
    mbr[e + 5 : e + 8] = b"\xff\xff\xff"  # CHS-Ende
    struct.pack_into("<I", mbr, e + 8, 1)
    struct.pack_into("<I", mbr, e + 12, min(disk_sectors - 1, 0xFFFFFFFF))
    mbr[510] = 0x55
    mbr[511] = 0xAA
    return bytes(mbr)


def build_entries(parts):
    entries = bytearray(NUM_ENTRIES * ENTRY_SIZE)
    for i, (first, last) in enumerate(parts):
        o = i * ENTRY_SIZE
        entries[o : o + 16] = TYPE_GUID
        entries[o + 16 : o + 32] = bytes((i + 1,)) * 16   # eindeutige GUID, Inhalt gleichgueltig
        struct.pack_into("<Q", entries, o + 32, first)
        struct.pack_into("<Q", entries, o + 40, last)
        name = f"sel4lake{i}".encode("utf-16-le")
        entries[o + 56 : o + 56 + len(name)] = name
    return bytes(entries)


def build_header(my_lba, alt_lba, first_usable, last_usable, entry_lba, entries_crc, broken):
    h = bytearray(HEADER_SIZE)
    h[0:8] = SIG if broken != "signature" else b"XFI PART"
    struct.pack_into("<I", h, 8, REVISION)
    struct.pack_into("<I", h, 12, HEADER_SIZE)
    # 16..20 = CRC, kommt zuletzt und muss beim Rechnen NULL sein.
    struct.pack_into("<Q", h, 24, my_lba)
    struct.pack_into("<Q", h, 32, alt_lba)
    struct.pack_into("<Q", h, 40, first_usable)
    struct.pack_into("<Q", h, 48, last_usable)
    h[56:72] = b"\x11" * 16  # Platten-GUID
    struct.pack_into("<Q", h, 72, entry_lba)
    struct.pack_into("<I", h, 80, NUM_ENTRIES)
    struct.pack_into("<I", h, 84, ENTRY_SIZE)
    struct.pack_into("<I", h, 88, entries_crc)
    crc = zlib.crc32(bytes(h)) & 0xFFFFFFFF
    if broken == "header-crc":
        crc ^= 0xFFFFFFFF
    struct.pack_into("<I", h, 16, crc)
    return bytes(h).ljust(SECTOR, b"\x00")


def build_fat16(total, dateien):
    """Ein **lesbares FAT16** in `total` Sektoren bauen (A-6.3).

    Selbst gebaut, aus demselben Grund wie die GPT: `mkfs.vfat` ist ein Fremdwerkzeug, und eine
    Suite, die daran haengt, faellt ohne es als "Test rot" aus statt als "Aufbau unvollstaendig".

    **Die Clusterzahl entscheidet ueber den Typ.** Unter 4085 Clustern ist es FAT12, nicht FAT16 --
    und als FAT16 gelesen liefert eine FAT12-Tabelle lauter falsche Ketten, ohne dass irgendetwas
    kaputt aussieht. Die Groesse der Partition ist damit kein Detail, sondern eine Bedingung; sie
    wird hier geprueft statt gehofft.
    """
    spc = 1
    reserved = 1
    num_fats = 2
    root_entries = 512
    root_sectors = root_entries * 32 // SECTOR
    fat_sectors = 1
    for _ in range(8):
        data = total - reserved - num_fats * fat_sectors - root_sectors
        fat_sectors = max(1, -(-(data // spc * 2) // SECTOR))
    fat_lba = reserved
    root_lba = fat_lba + num_fats * fat_sectors
    data_lba = root_lba + root_sectors
    clusters = (total - data_lba) // spc
    if not (4085 <= clusters <= 65524):
        sys.exit(f"{clusters} Cluster -> kein FAT16 (noetig 4085..65524); Partition anders waehlen")

    img = bytearray(total * SECTOR)
    b = img
    struct.pack_into("<H", b, 11, SECTOR)
    b[13] = spc
    struct.pack_into("<H", b, 14, reserved)
    b[16] = num_fats
    struct.pack_into("<H", b, 17, root_entries)
    struct.pack_into("<H", b, 19, total if total < 0x10000 else 0)
    struct.pack_into("<H", b, 22, fat_sectors)
    if total >= 0x10000:
        struct.pack_into("<I", b, 32, total)
    b[510] = 0x55
    b[511] = 0xAA

    naechster = 2
    for i, (name, inhalt) in enumerate(dateien):
        n = max(1, -(-len(inhalt) // (spc * SECTOR)))
        start = naechster
        for k in range(n):
            c = start + k
            nxt = 0xFFFF if k + 1 == n else c + 1
            for f in range(num_fats):
                o = (fat_lba + f * fat_sectors) * SECTOR + c * 2
                struct.pack_into("<H", img, o, nxt)
            lba = data_lba + (c - 2) * spc
            teil = inhalt[k * spc * SECTOR : (k + 1) * spc * SECTOR]
            img[lba * SECTOR : lba * SECTOR + len(teil)] = teil
        stamm, _, endung = name.partition(".")
        eintrag = (stamm.upper().ljust(8) + endung.upper().ljust(3)).encode()
        o = root_lba * SECTOR + i * 32
        img[o : o + 11] = eintrag
        img[o + 11] = 0x20
        struct.pack_into("<H", img, o + 26, start)
        struct.pack_into("<I", img, o + 28, len(inhalt))
        naechster = start + n
    return bytes(img)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--sectors", type=int, required=True)
    ap.add_argument("--part", action="append", default=[], metavar="FIRST:LAST")
    ap.add_argument("--magic-at", type=int, default=None,
                    help="LBA, auf die 'SEL4LAKE' geschrieben wird")
    ap.add_argument("--fat16", metavar="FIRST:LAST",
                    help="in diese Partition ein lesbares FAT16 legen (A-6.3)")
    ap.add_argument("--file", action="append", default=[], metavar="NAME=INHALT",
                    help="Datei im FAT16 (nur mit --fat16)")
    ap.add_argument("--break", dest="broken", default=None,
                    choices=["signature", "header-crc", "entries-crc"])
    a = ap.parse_args()

    n = a.sectors
    entry_sectors = NUM_ENTRIES * ENTRY_SIZE // SECTOR   # 32
    first_usable = 2 + entry_sectors                      # 34
    last_usable = n - 2 - entry_sectors
    parts = [tuple(int(x) for x in p.split(":")) for p in a.part]
    for first, last in parts:
        if first < first_usable or last > last_usable:
            sys.exit(f"Partition {first}:{last} liegt ausserhalb {first_usable}..{last_usable}")

    entries = build_entries(parts)
    entries_crc = zlib.crc32(entries) & 0xFFFFFFFF
    if a.broken == "entries-crc":
        entries_crc ^= 0xFFFFFFFF

    disk = bytearray(n * SECTOR)
    disk[0:SECTOR] = protective_mbr(n)
    disk[SECTOR : 2 * SECTOR] = build_header(1, n - 1, first_usable, last_usable, 2,
                                             entries_crc, a.broken)
    disk[2 * SECTOR : 2 * SECTOR + len(entries)] = entries
    # Sicherungskopie am Ende: Eintraege direkt vor dem letzten Sektor, Kopf im letzten.
    backup_entry_lba = n - 1 - entry_sectors
    disk[backup_entry_lba * SECTOR : backup_entry_lba * SECTOR + len(entries)] = entries
    disk[(n - 1) * SECTOR : n * SECTOR] = build_header(n - 1, 1, first_usable, last_usable,
                                                       backup_entry_lba, entries_crc, a.broken)
    if a.fat16:
        first, last = (int(x) for x in a.fat16.split(":"))
        dateien = []
        for spec in a.file:
            name, _, inhalt = spec.partition("=")
            dateien.append((name, inhalt.encode()))
        fat = build_fat16(last - first + 1, dateien)
        disk[first * SECTOR : first * SECTOR + len(fat)] = fat

    if a.magic_at is not None:
        o = a.magic_at * SECTOR
        disk[o : o + 8] = b"SEL4LAKE"

    with open(a.out, "wb") as f:
        f.write(disk)


if __name__ == "__main__":
    main()
