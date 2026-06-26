#!/usr/bin/env bash
# Deadlock-/Hang-Regressionstest (Bugfix: reentranter Ticket-SpinLock-Deadlock).
#
# Bootet den Kernel N-mal unter QEMU und zaehlt, wie oft der Selbsttest NICHT bis
# `== SELFTEST COMPLETE ==` kommt (= Hang). Vor dem Fix der IRQ-sicheren SpinLocks
# haengte der Kernel ~27 % der Laeufe im el0iso-/reclaim-/native-/Fuzzer-Abschnitt
# (Timer-Tick reentrant gegen SCHEDS/NTFNS in Thread-/Idle-Kontext). Nach dem Fix: 0.
#
# Aufruf:  tools/hang-stress.sh [N=30] [TIMEOUT_S=40]
# Exit 0, wenn 0 Hangs; sonst 1 (mit der letzten Kernel-Zeile je Hang).
set -uo pipefail
cd "$(dirname "$0")/.."

N="${1:-30}"
TMO="${2:-40}"
ELF="build/target/aarch64-sel4lake/release/sel4lake-kernel.elf"

echo "== build =="
./build.sh >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }
# Boot-Archiv (ext-26) wie in test-qemu.sh bereitstellen.
mkdir -p build
printf 'PROBE-A-BLOB' > build/_proba.bin
printf 'PROBE-B-BLOB' > build/_probb.bin
python3 tools/mkarchive.py build/boot-archive.bin \
    probe-a:2:build/_proba.bin probe-b:1:build/_probb.bin >/dev/null 2>&1 || { echo "ARCHIVE FAILED"; exit 1; }

echo "== stress: $N Laeufe (Timeout ${TMO}s je Lauf) =="
ok=0; hang=0
for i in $(seq 1 "$N"); do
    out=$(timeout --signal=KILL "$TMO" qemu-system-aarch64 \
        -machine virt,iommu=smmuv3 -cpu cortex-a72 -smp 8 -m 4G \
        -nographic -serial mon:stdio -no-reboot -net none \
        -device pcie-root-port,id=rp0,chassis=1 -device virtio-rng-pci,bus=rp0 \
        -device loader,file=build/boot-archive.bin,addr=0x13F000000 \
        -kernel "$ELF" </dev/null 2>/dev/null)
    if echo "$out" | grep -q "SELFTEST COMPLETE"; then
        ok=$((ok + 1))
    else
        hang=$((hang + 1))
        echo "  run $i: HANG  last=[$(echo "$out" | grep -v '^$' | tail -1)]"
    fi
done
echo "== OK=$ok HANG=$hang / $N =="
[ "$hang" -eq 0 ]
