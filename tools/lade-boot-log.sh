#!/usr/bin/env bash
# Einen EINZELNEN Boot der LADE-Konfiguration fahren und das volle Protokoll ablegen.
#
# Gegenstueck zu `tools/boot-x86-log.sh`, das die HAUPT-Konfiguration faehrt (ohne Archiv). Fuer
# C8 gebraucht: die Zahl, um die es geht (`kstack`), entsteht auf dem LADEPFAD -- und den hat nur
# diese Konfiguration. `test-qemu-x86-load.sh` faellt ein Urteil und filtert; fuer eine MESSUNG
# braucht es das Protokoll auch im gruenen Lauf.
#
# Die QEMU-Zeile ist Wort fuer Wort die der Lade-Suite (Beschleunigung, IOMMU, beide virtio-
# Geraete, GPT-Abbild) -- zwei Aufbauten, die dasselbe Geraet verschieden aufsetzen, sind ein Riss.
# Voraussetzung: `test-qemu-x86-load.sh` lief mindestens einmal (Archiv + Zertifikate liegen dann
# in `build/`); das Blockabbild wird hier frisch gebaut, damit der Checkpoint-Sektor leer ist.
set -uo pipefail
cd "$(dirname "$0")/.."

SEK="${SEK:-90}"
RAM="${RAM:-512M}"
CORES="${CORES:-4}"
OUT="${OUT:-build/diag/lade-$RAM-$(date +%H%M%S).log}"
mkdir -p "$(dirname "$OUT")"

KELF=build/target/x86_64-unknown-none/release/caprock-kernel
ARCHIVE="${ARCHIVE:-build/boot-archive-x86.bin}"
[ -f "$ARCHIVE" ] || { echo "FEHLT: $ARCHIVE -- zuerst ./test-qemu-x86-load.sh fahren." >&2; exit 2; }

BLK_IMG="$(mktemp)"
python3 tools/mkgpt.py "$BLK_IMG" --sectors 32768 \
    --part 34:20000 --part 20001:32700 --magic-at 20001 \
    --fat16 34:20000 --file "HELLO.TXT=CAPROCKS-DATEIINHALT" >/dev/null \
    || { echo "FEHLER: GPT-Abbild"; rm -f "$BLK_IMG"; exit 2; }

if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL=(-enable-kvm -cpu host,+invtsc,host-cache-info=on)
else
    ACCEL=(-cpu Skylake-Client)
fi

timeout "$SEK" qemu-system-x86_64 \
    -kernel "$KELF.mb32" -m "$RAM" -smp "$CORES" "${ACCEL[@]}" \
    -machine q35,kernel-irqchip=split -device intel-iommu,caching-mode=on \
    -device virtio-rng-pci,disable-legacy=on,iommu_platform=on \
    -drive if=none,id=blk0,format=raw,file="$BLK_IMG" \
    -device virtio-blk-pci,drive=blk0,disable-legacy=on,iommu_platform=on \
    -device virtio-net-pci,netdev=n0,disable-legacy=on,iommu_platform=on \
    -netdev user,id=n0,restrict=on -initrd "$ARCHIVE" \
    -nographic -serial file:"$OUT" -no-reboot </dev/null >/dev/null 2>&1
RC=$?
rm -f "$BLK_IMG"
echo "lade-boot: rc=$RC ram=$RAM cores=$CORES -> $OUT ($(wc -l <"$OUT") Zeilen)"
