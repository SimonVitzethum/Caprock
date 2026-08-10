#!/usr/bin/env bash
# Einen EINZELNEN x86-Boot fahren und das volle Protokoll in eine Datei legen.
#
# Warum ein eigenes Werkzeug: `test-qemu-x86.sh` faellt ein URTEIL und legt bei Abweichung ein
# Protokoll ab -- fuer eine MESSUNG (Fuellstaende, Iterationszahlen, Kurven) braucht es aber das
# Protokoll auch dann, wenn alles gruen ist. Ohne das haette man dieselbe Luecke wie beim
# `tail -1`-Sammellauf: der gruene Lauf laesst nichts zurueck, mit dem man rechnen koennte.
#
# Die QEMU-Zeile ist absichtlich dieselbe wie in der Suite (Beschleunigung, IOMMU, Cache-Info) --
# zwei Aufbauten, die dasselbe Geraet verschieden aufsetzen, sind ein Riss (Fallenliste).
set -uo pipefail
cd "$(dirname "$0")/.."

SEK="${SEK:-90}"
RAM="${RAM:-512M}"
CORES="${CORES:-4}"
OUT="${OUT:-build/diag/boot-$RAM-$(date +%H%M%S).log}"
mkdir -p "$(dirname "$OUT")"

if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL=(-enable-kvm -cpu host,+invtsc,host-cache-info=on)
else
    ACCEL=(-cpu Skylake-Client)
fi

timeout --signal=KILL "$SEK" qemu-system-x86_64 \
    -machine q35,kernel-irqchip=split -device intel-iommu,intremap=on,caching-mode=on \
    "${ACCEL[@]}" -smp "$CORES" -m "$RAM" \
    -kernel build/target/x86_64-unknown-none/release/caprock-kernel.mb32 \
    -serial file:"$OUT" -display none -no-reboot >/dev/null 2>&1
RC=$?
echo "boot: rc=$RC ram=$RAM cores=$CORES -> $OUT ($(wc -l <"$OUT") Zeilen)"
