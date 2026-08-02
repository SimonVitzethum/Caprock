#!/usr/bin/env python3
# Ein Plattenabbild **unabhaengig** nachlesen (A-6.4, Host-Tool — KEIN Kernelcode).
#
# Warum es das gibt: der Kernel meldet, dass die Dateisystem-PD geschrieben und zurueckgelesen hat.
# Das ist eine Aussage des Codes ueber sich selbst. Dieses Werkzeug liest dasselbe Abbild mit einer
# **zweiten, unabhaengigen** Implementierung (Python, andere Sprache, andere Rechnung) und sagt, ob
# wirklich auf der Platte steht, was behauptet wird.
#
# Das ist derselbe Gedanke wie ueberall in diesem Projekt: eine Quittung ist keine Datenlage. Ein
# Schreiber, der sein eigenes Ergebnis bestaetigt, bestaetigt nichts.
#
# Aufruf:
#   checkfat.py ABBILD --part-lba N --file NAME --expect-size N --expect-pattern
import argparse
import struct
import sys

SECTOR = 512


def muster(i):
    """Dasselbe deterministische Muster, das `programs/trusted/fs` schreibt.

    Es steht hier **noch einmal** und wird nicht importiert — genau darin liegt der Wert. Zwei
    Implementierungen derselben Regel stimmen nur ueberein, wenn die Regel eingehalten wurde; eine
    geteilte Funktion wuerde auch dann uebereinstimmen, wenn beide falsch sind.
    """
    return (i * 7 + 13) % 251


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    ap.add_argument("--part-lba", type=int, required=True)
    ap.add_argument("--file", required=True)
    ap.add_argument("--expect-size", type=int, required=True)
    a = ap.parse_args()

    d = open(a.image, "rb").read()
    base = a.part_lba * SECTOR
    b = d[base : base + SECTOR]
    if b[510] != 0x55 or b[511] != 0xAA:
        sys.exit("kein Bootsektor an der Partition")
    bps = struct.unpack_from("<H", b, 11)[0]
    spc = b[13]
    reserved = struct.unpack_from("<H", b, 14)[0]
    nfats = b[16]
    root_entries = struct.unpack_from("<H", b, 17)[0]
    fat_sectors = struct.unpack_from("<H", b, 22)[0]
    if bps != SECTOR:
        sys.exit(f"Sektorgroesse {bps} != 512")
    fat_lba = reserved
    root_lba = fat_lba + nfats * fat_sectors
    root_sectors = root_entries * 32 // SECTOR
    data_lba = root_lba + root_sectors

    def sek(lba):
        o = base + lba * SECTOR
        return d[o : o + SECTOR]

    # Den Eintrag suchen.
    stamm, _, endung = a.file.partition(".")
    ziel = (stamm.upper().ljust(8) + endung.upper().ljust(3)).encode()
    eintrag = None
    for s in range(root_sectors):
        buf = sek(root_lba + s)
        for i in range(SECTOR // 32):
            e = buf[i * 32 : (i + 1) * 32]
            if e[0] == 0:
                break
            if e[:11] == ziel:
                eintrag = e
                break
        if eintrag:
            break
    if not eintrag:
        sys.exit(f"{a.file} nicht im Wurzelverzeichnis")

    start = struct.unpack_from("<H", eintrag, 26)[0]
    size = struct.unpack_from("<I", eintrag, 28)[0]
    if size != a.expect_size:
        sys.exit(f"Groesse {size} != erwartet {a.expect_size}")

    # **Beide** FAT-Kopien pruefen. Ein Schreiber, der nur die erste fortschreibt, hinterlaesst ein
    # Dateisystem, das ein Leser der zweiten Kopie anders sieht -- und genau das faellt sonst
    # niemandem auf, bis ein Pruefwerkzeug es meldet.
    ketten = []
    for kopie in range(nfats):
        c, kette = start, []
        for _ in range(1000):
            kette.append(c)
            o = (fat_lba + kopie * fat_sectors) * SECTOR + c * 2
            nxt = struct.unpack_from("<H", d, base + o)[0]
            if nxt >= 0xFFF8:
                break
            if nxt < 2:
                sys.exit(f"Kette (Kopie {kopie}) zeigt auf Cluster {nxt}")
            c = nxt
        ketten.append(kette)
    if len(set(map(tuple, ketten))) != 1:
        sys.exit(f"die {nfats} FAT-Kopien sind NICHT gleich: {ketten}")
    kette = ketten[0]

    # Inhalt zusammensetzen und gegen das Muster halten.
    inhalt = b""
    for c in kette:
        lba = data_lba + (c - 2) * spc
        inhalt += sek(lba)
    inhalt = inhalt[:size]
    for i, ch in enumerate(inhalt):
        if ch != muster(i):
            sys.exit(f"Byte {i}: {ch} != erwartet {muster(i)}")

    print(f"OK: {a.file}, {size} Byte, {len(kette)} Cluster, {nfats} FAT-Kopien gleich")


if __name__ == "__main__":
    main()
