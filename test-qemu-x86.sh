#!/usr/bin/env bash
# Automatisierter QEMU-Boot-Test fuer den x86_64-Port (Branch arch/x86_64).
# Baut den Kernel, bootet ihn als Multiboot-Image unter qemu-system-x86_64, erfasst die serielle
# Ausgabe (COM1) und prueft die erwarteten Marker. Stufe 0: Boot + Long Mode + Serial.
set -uo pipefail
cd "$(dirname "$0")"

SECONDS_RUN="${1:-10}"
ELF="build/target/x86_64-unknown-none/release/sel4lake-kernel.mb32"

echo "== build (x86_64-unknown-none) =="
./build-x86.sh >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }

echo "== boot ($SECONDS_RUN s) =="
OUT="$(timeout --signal=KILL "$SECONDS_RUN" qemu-system-x86_64 \
    -kernel "$ELF" -m 512M -smp 1 \
    -nographic -serial mon:stdio -no-reboot -no-shutdown \
    </dev/null 2>/dev/null)"
echo "$OUT"

echo "== checks =="
fail=0
check() { if echo "$OUT" | grep -q "$1"; then echo "  PASS: $2"; else echo "  FAIL: $2"; fail=1; fi; }
check "x86_64 first light" "Long Mode erreicht + Serial-Banner (COM1)"
check "x86_64 first light: ALL PASS" "Stufe 0: Boot + Long Mode + 16550-Serial"
if [ "$fail" = 0 ]; then echo "== ALL PASS =="; else echo "== FAILURES =="; fi
exit "$fail"
