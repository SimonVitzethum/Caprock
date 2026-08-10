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
ELF="build/target/aarch64-caprock/release/caprock-kernel.elf"

# Default: RELEASE-Build OHNE Fuzzer (Langzeittest-/Produktivkonfiguration, ADR 0013). Mit
# `KERNEL_FUZZ=1` wird `--features kernel-fuzz` gebaut (Deadlock-Regression inkl. Fuzzer).
FEAT="${KERNEL_FUZZ:+--features kernel-fuzz}"
echo "== build ${FEAT:-(release, ohne Fuzzer)} =="
./build.sh $FEAT >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }
# Boot-Archiv (ext-26/ext-27) wie in test-qemu.sh bereitstellen (externe Programme + Testdienste).
mkdir -p build
( cd programs && rustup run nightly cargo build --release ) >/dev/null 2>&1 \
    || { echo "PROGRAMS BUILD FAILED"; exit 1; }
# ext-27: die adversarialen Testdienste (eigener tests/-Workspace) — sonst scheitert das Setup der
# ext-27-Tests (Dienst nicht im Archiv) und der Selbsttest erreicht NIE SELFTEST COMPLETE (= "Hang").
( cd tests && rustup run nightly cargo build --release ) >/dev/null 2>&1 \
    || { echo "TESTS BUILD FAILED"; exit 1; }
HELLO="programs/build/target/aarch64-caprock-user/release/hello.elf"
TBIN="tests/build/target/aarch64-caprock-user/release"
printf 'PLACEHOLDER' > build/_probe.bin
python3 tools/mkarchive.py build/boot-archive.bin \
    10:hello:2:1:"$HELLO" 11:hwhello:1:1:"$HELLO" 12:trusted-x:0:1:"$HELLO" 2:probe:2:1:build/_probe.bin \
    20:aggressor-u:2:1:"$TBIN/aggressor-u.elf" 21:intruder-u:2:1:"$TBIN/intruder-u.elf" \
    22:aggressor-h:1:1:"$TBIN/aggressor-h.elf" 23:intruder-h:1:1:"$TBIN/intruder-h.elf" \
    24:aggressor-t:0:1:"$TBIN/aggressor-t.elf" 25:intruder-t:0:1:"$TBIN/intruder-t.elf" \
    >/dev/null 2>&1 || { echo "ARCHIVE FAILED"; exit 1; }

echo "== stress: $N Laeufe (Timeout ${TMO}s je Lauf) =="
ok=0; hang=0
for i in $(seq 1 "$N"); do
    out=$(timeout --signal=KILL "$TMO" qemu-system-aarch64 \
        -machine virt,iommu=smmuv3 -cpu cortex-a72 -smp 8 -m 4G \
        -nographic -serial mon:stdio -no-reboot -net none \
        -device pcie-root-port,id=rp0,chassis=1 -device virtio-rng-pci,bus=rp0 \
        -device loader,file=build/boot-archive.bin,addr=0x13F000000 \
        -kernel "$ELF" </dev/null 2>/dev/null)
    if grep -q "SELFTEST COMPLETE" <<<"$out"; then
        ok=$((ok + 1))
    else
        hang=$((hang + 1))
        echo "  run $i: HANG  last=[$(echo "$out" | grep -v '^$' | tail -1)]"
    fi
done
echo "== OK=$ok HANG=$hang / $N =="
[ "$hang" -eq 0 ]
