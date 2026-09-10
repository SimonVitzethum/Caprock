#!/usr/bin/env bash
# Fahrt 4 — GRUB-ISO mit ZWEI Multiboot-Modulen (LXPD-Treiberfahrt).
#
# VORBILD: tools/mkgrubiso.sh (wird NICHT angefasst, nur gelesen). Gleiche Form:
# `grub-file --is-x86-multiboot`-Pruefung des Kernels, `grub.cfg` mit
# `timeout=0` und einem Eintrag, Bau via `grub-mkrescue`.
#
# Unterschied: FEST zwei `module`-Zeilen — Modul 0 = Boot-Archiv, Modul 1 =
# bind_elf-Treiber-Image. Modul 1 landet als Bootloader-Spanne im Gast
# (`set_lxpd_module_span`); `boot_lxpd_treiber` (kernel/src/loader.rs) findet
# sie per `sha256 == Manifest-Eintrag`. `mkgrubiso.sh` kennt nur EIN Modul,
# deshalb dieses eigene Skript statt eines Eingriffs dort.
#
# Aufruf:
#   bash tools/lx_fahrt4_iso.sh --kernel K.mb32 --archive A.bin --driver D.img --out ISO

if [ -z "${BASH_VERSION:-}" ]; then
    echo "FEHLER: dieses Skript braucht bash, nicht sh/dash." >&2
    exit 2
fi
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

KERNEL=""; ARCHIVE=""; DRIVER=""; OUT=""

while [ $# -gt 0 ]; do
    case "$1" in
        --kernel)  KERNEL="$2";  shift 2 ;;
        --archive) ARCHIVE="$2"; shift 2 ;;
        --driver)  DRIVER="$2";  shift 2 ;;
        --out)     OUT="$2";     shift 2 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unbekannte Option '$1'" >&2; exit 2 ;;
    esac
done

[ -n "$KERNEL" ]  || { echo "FEHLER: --kernel fehlt" >&2; exit 2; }
[ -n "$ARCHIVE" ] || { echo "FEHLER: --archive fehlt" >&2; exit 2; }
[ -n "$DRIVER" ]  || { echo "FEHLER: --driver fehlt" >&2; exit 2; }
[ -n "$OUT" ]     || { echo "FEHLER: --out fehlt" >&2; exit 2; }

for w in grub-mkrescue grub-file xorriso; do
    command -v "$w" >/dev/null || { echo "FEHLER: '$w' fehlt (Paket grub / xorriso)." >&2; exit 1; }
done
[ -f "$KERNEL" ]  || { echo "FEHLER: Kernel '$KERNEL' fehlt" >&2; exit 1; }
[ -f "$ARCHIVE" ] || { echo "FEHLER: Boot-Archiv '$ARCHIVE' fehlt" >&2; exit 1; }
[ -f "$DRIVER" ]  || { echo "FEHLER: Treiber-Bild '$DRIVER' fehlt" >&2; exit 1; }

# Wie mkgrubiso.sh: der Bootloader laedt nur, was er als Multiboot erkennt.
grub-file --is-x86-multiboot "$KERNEL" \
    || { echo "FEHLER: '$KERNEL' ist kein Multiboot1-Image." >&2; exit 1; }

BAUM="$(mktemp -d)"
trap 'rm -rf "$BAUM"' EXIT
mkdir -p "$BAUM/boot/grub"
cp "$KERNEL" "$BAUM/boot/kernel.mb32"
cp "$ARCHIVE" "$BAUM/boot/boot-archive.bin"
cp "$DRIVER" "$BAUM/boot/lxpd-test.img"

{
    echo "set timeout=0"
    echo "set default=0"
    echo ""
    echo 'menuentry "Caprock+LXPD" {'
    echo "    multiboot /boot/kernel.mb32"
    echo "    module /boot/boot-archive.bin boot-archive"
    echo "    module /boot/lxpd-test.img lxpd-test"
    echo "    boot"
    echo "}"
} > "$BAUM/boot/grub/grub.cfg"

grub-mkrescue -o "$OUT" "$BAUM" >/dev/null 2>&1 \
    || { echo "FEHLER: grub-mkrescue fehlgeschlagen." >&2; exit 1; }
[ -s "$OUT" ] || { echo "FEHLER: '$OUT' ist leer." >&2; exit 1; }

echo "== ISO: $OUT ($(du -h "$OUT" | cut -f1)) =="
echo "   Kernel  : $KERNEL"
echo "   Modul 0 : $ARCHIVE  -> /boot/boot-archive.bin"
echo "   Modul 1 : $DRIVER  -> /boot/lxpd-test.img"
