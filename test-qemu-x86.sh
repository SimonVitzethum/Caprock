#!/usr/bin/env bash
# Automatisierter QEMU-Boot-Test fuer den x86_64-Port (Branch arch/x86_64).
# Baut den Kernel, bootet ihn als Multiboot-Image unter qemu-system-x86_64, erfasst die serielle
# Ausgabe (COM1) und prueft die erwarteten Marker. Stufe 0: Boot + Long Mode + Serial.
set -uo pipefail
cd "$(dirname "$0")"

SECONDS_RUN="${1:-60}"
ELF="build/target/x86_64-unknown-none/release/sel4lake-kernel.mb32"

echo "== build (x86_64-unknown-none) =="
./build-x86.sh >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }

# Zuverlaessiger Capture ueber eine Datei (Pipe + SIGKILL verliert sonst QEMUs stdout-Puffer).
LOG="$(mktemp)"
echo "== boot ($SECONDS_RUN s) =="
timeout "$SECONDS_RUN" qemu-system-x86_64 \
    -kernel "$ELF" -m 512M -smp 4 \
    -nographic -serial file:"$LOG" -no-reboot -no-shutdown \
    </dev/null >/dev/null 2>&1 || true
OUT="$(grep -vE "SeaBIOS|iPXE|Press Ctrl|Booting from|C900|PMM|PnP" "$LOG" 2>/dev/null)"
rm -f "$LOG"
echo "$OUT"

echo "== checks =="
fail=0
check() { if echo "$OUT" | grep -q "$1"; then echo "  PASS: $2"; else echo "  FAIL: $2"; fail=1; fi; }
check "acpi    : 4 CPU(s) laut MADT"  "ACPI-MADT: CPU-Liste gelesen (x86-Gegenstueck zum DTB)"
check "mbi     : Speicherplan gelesen" "Multiboot-Speicherplan (RAM-Groesse gelesen statt fest verdrahtet)"
check "smp     : 4 von 4 Kern(en) online" "SMP: alle Sekundaerkerne per INIT-SIPI-SIPI gestartet (16-bit-Trampolin -> Long Mode)"
check "sched   : core 3 ticks="  "SMP: jeder Kern hat einen eigenen LAPIC-Timer + Scheduler-Instanz"
check "mmu     : identity-map, paging=1 caches=1 CR0.WP=1" "Stufe 1: 4-Level-Paging + W^X (CR0.WP)"
check "timer   : LAPIC-Timer 100 Hz"  "Stufe 2: LAPIC-Timer (gegen PIT kalibriert)"
check "memtest : ALL PASS"            "Kernel-Kern: Speichermodell-Selbsttest (arch-neutral, identisch zu aarch64)"
check "zerotest: ALL PASS"            "Kernel-Kern: Datenremanenz (genullte Allokationen)"
check "captest : ALL PASS"            "Kernel-Kern: Capability-Selbsttest (CDT/Refcounts/Revoke)"
check "budget  : ALL PASS"            "Kernel-Kern: Cap-Budget je PD"
check "sched   : ALL PASS"            "Stufe 4: praeemptiver Scheduler (LAPIC-Timer verdraengt Threads ueber den Trap-Frame-Tausch)"
check "ipc     : ALL PASS"            "Stufe 4: cap-gesicherte IPC (CALL/RECV/REPLY zwischen zwei PDs)"
check "ring3   : ALL PASS"            "Stufe 4c: Ring-3-Threads (Syscall aus Ring 3; Zugriff auf Kernel-Speicher faultet -> Thread beendet, Kernel laeuft weiter)"
check "audit   : ALL PASS"            "Stufe 4: Scheduler- + CDT-Audit sauber"
check "SELFTEST COMPLETE"             "Stufe 4: sauberes system_off (ACPI) statt Timeout"
if [ "$fail" = 0 ]; then echo "== ALL PASS =="; else echo "== FAILURES =="; fi
exit "$fail"
