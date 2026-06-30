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

# Zuverlaessiger Capture ueber eine Datei (Pipe + SIGKILL verliert sonst QEMUs stdout-Puffer).
LOG="$(mktemp)"
echo "== boot ($SECONDS_RUN s) =="
timeout "$SECONDS_RUN" qemu-system-x86_64 \
    -kernel "$ELF" -m 512M -smp 1 \
    -nographic -serial file:"$LOG" -no-reboot -no-shutdown \
    </dev/null >/dev/null 2>&1 || true
OUT="$(grep -vE "SeaBIOS|iPXE|Press Ctrl|Booting from|C900|PMM|PnP" "$LOG" 2>/dev/null)"
rm -f "$LOG"
echo "$OUT"

echo "== checks =="
fail=0
check() { if echo "$OUT" | grep -q "$1"; then echo "  PASS: $2"; else echo "  FAIL: $2"; fail=1; fi; }
check "x86_64 first light: ALL PASS"   "Stufe 0: Boot + Long Mode + 16550-Serial (COM1)"
check "idt     : int3 behandelt"       "Stufe 2(IDT): Exception-Dispatch (int3 gefangen + iretq)"
check "PML4 W\^X-Identity aktiv"        "Stufe 1: 4-Level-Paging aktiv (CR3 + CR0.WP)"
check "W\^X-Bits korrekt"              "Stufe 1: W^X-Bits (.text=R-X, .rodata=R--/NX)"
if [ "$fail" = 0 ]; then echo "== ALL PASS =="; else echo "== FAILURES =="; fi
exit "$fail"
