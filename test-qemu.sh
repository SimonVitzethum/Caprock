#!/usr/bin/env bash
# Automatisierter QEMU-Boot-/SMP-/Timer-Test für SEL4Lake.
#
# Baut den Kernel, bootet ihn unter QEMU (ARM virt, 8 Kerne, 4 GiB), erfasst die
# serielle Ausgabe für einige Sekunden und prüft auf die erwarteten Marker:
#   * MMU aktiv (M=1 C=1 I=1)
#   * alle 8 Kerne online
#   * jeder Kern erzeugt Timer-Ticks
set -uo pipefail
cd "$(dirname "$0")"

CORES=8
# Default großzügig: 8 Demo-Threads auf den Sekundärkernen + die core-0-Demo
# teilen sich unter single-threaded QEMU-TCG eine Host-CPU -> alles emuliert länger.
SECONDS_RUN="${1:-35}"
ELF="build/target/aarch64-sel4lake/release/sel4lake-kernel.elf"

echo "== build =="
./build.sh >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }

echo "== boot ($SECONDS_RUN s) =="
OUT="$(timeout --signal=KILL "$SECONDS_RUN" qemu-system-aarch64 \
    -machine virt -cpu cortex-a72 -smp "$CORES" -m 4G \
    -nographic -serial mon:stdio -no-reboot \
    -kernel "$ELF" </dev/null 2>/dev/null)"

echo "$OUT"
echo "== checks =="
fail=0
check() { if echo "$OUT" | grep -q "$1"; then echo "  PASS: $2"; else echo "  FAIL: $2"; fail=1; fi; }

check "M=1 C=1 I=1" "MMU + Caches aktiv"
check "dtb     : ALL PASS" "DTB-Parsing (RAM-Größe aus dem Device Tree)"
check "memtest : ALL PASS" "Speichermodell-Selbsttest (alloc/split/transfer/free)"
check "captest : ALL PASS" "Capability-Selbsttest (copy/mint/move/delete/revoke)"
check "sched   : ALL PASS" "Scheduler (Preemption auf core 0 + alle Kerne ticken)"
check "fp      : ALL PASS" "FP/SIMD-Kontext bleibt über Preemption erhalten"
check "prio    : ALL PASS" "Bitmap-Prioritäten (höhere Priorität läuft zuerst)"
check "life    : ALL PASS" "Thread-Lebenszyklus (cap-KILL + EXIT + Stack-Rückgewinnung)"
check "notif   : ALL PASS" "Notifications (asynchrone Badge-Signale)"
check "xfer    : ALL PASS" "Capability-Transfer in IPC (Broker delegiert Service-Cap)"
check "ipc     : ALL PASS" "Cap-gesicherte IPC (Server v1, PD<->PD)"
check "reload  : ALL PASS" "Hot-Reload (Server v2 ersetzt v1, gleicher Endpoint, kein Reboot)"
check "ckpt    : ALL PASS" "Stateful Hot-Reload (Zustand bleibt über v1->v2 erhalten)"
check "el0     : ALL PASS" "EL0-Userland (echter User-Thread ruft per Syscall)"
check "el0iso  : ALL PASS" "EL0-Isolation (Kernel-Zugriff faultet, Thread beendet, Kernel überlebt)"
check "smp     : ALL PASS" "Per-Kern-paralleler Scheduler (Worker je Kern + Cross-Core-IPI-Wake)"
check "xipc    : ALL PASS" "Kern-übergreifende synchrone IPC (Client core0 <-> Server core2)"
check "reclaim : ALL PASS" "EL0-Kernel-Stack-Reclaim (>Pool-viele transiente EL0-Threads)"
check "balance : ALL PASS" "Lastausgleich (lastbewusste Thread-Platzierung über die Kerne)"
online=$(echo "$OUT" | grep -c "online")
[ "$online" -eq "$CORES" ] && echo "  PASS: alle $CORES Kerne online" || { echo "  FAIL: nur $online/$CORES Kerne online"; fail=1; }

echo "== $([ $fail -eq 0 ] && echo 'ALL PASS' || echo 'FAILURES') =="
exit $fail
