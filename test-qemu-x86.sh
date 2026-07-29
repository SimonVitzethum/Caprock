#!/usr/bin/env bash
# Automatisierter QEMU-Boot-Test fuer den x86_64-Port (Branch arch/x86_64).
# Baut den Kernel, bootet ihn als Multiboot-Image unter qemu-system-x86_64, erfasst die serielle
# Ausgabe (COM1) und prueft die erwarteten Marker. Stufe 0: Boot + Long Mode + Serial.
set -uo pipefail
cd "$(dirname "$0")"

SECONDS_RUN="${1:-120}"
# RAM-Groesse ist ein **Testparameter**, kein Detail: alles, was aus `RAM_TOP` abgeleitet wird --
# die IOVA-Fensterbasis voran -- prueft auf einer 512-MiB-Maschine andere Zweige als auf einer
# grossen. Der 32-Bit-Ausschluss in `dmawin` war nur der auffaelligste Fall: dort lag das Fenster
# unter 4 GiB, und der Test waere gruen gewesen, ohne die Eigenschaft zu pruefen.
RAM="${2:-512M}"

# **KVM, wenn verfuegbar.** Unter TCG kostet ein serialisierter Zeitstempel ~14 000 Zyklen --
# damit ist alles unter ~5 us Sektionslaenge nicht aufloesbar, und `invtsc` kann TCG gar nicht
# zusagen ("TCG doesn't support requested feature: CPUID[80000007h].EDX.invtsc"). Beides kommt
# mit `-enable-kvm -cpu host` zurueck: Invariant TSC wird durchgereicht, die Aufloesung faellt
# auf die Groessenordnung eines echten `rdtscp`. Der emulierte `intel-iommu` bleibt unter KVM
# nutzbar (`kernel-irqchip=split` ist ohnehin gesetzt), `caching-mode=on` ebenso.
# Fallback auf TCG, damit der Lauf ohne /dev/kvm nicht scheitert -- dann aber mit den obigen
# Einschraenkungen, und die Zeile `cycles :` sagt das auch.
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    # `+invtsc` MUSS explizit angefordert werden: QEMU laesst es auch bei `-cpu host` weg, weil es
    # die Live-Migration blockiert. Unter KVM wird es dann durchgereicht, unter TCG abgelehnt.
    ACCEL=(-enable-kvm -cpu host,+invtsc)
    echo "== Beschleunigung: KVM (-cpu host) =="
else
    # TCG-Rueckfall: **nicht** `qemu64`. Dieses Modell ist synthetisch und meldet weder
    # CPUID-Blatt 4 (Cache-Geometrie -> der `color`-Test kann nur SKIPpen) noch x2APIC
    # (-> `apic`-Pruefung faellt durch, ohne dass am Kernel etwas fehlt). Beides sind
    # Luecken des MODELLS, keine des Kernels, und beide verschwinden mit einem echten
    # Modell. `Skylake-Client` meldet LLC 16 MiB/16-fach/64 B -> 256 Seitenfarben und
    # x2APIC; was TCG davon nicht kann, sagt QEMU beim Start selbst an.
    ACCEL=(-cpu Skylake-Client)
    echo "== Beschleunigung: TCG (kein /dev/kvm) -- Zyklenwerte sind dann indikativ =="
fi
ELF="build/target/x86_64-unknown-none/release/sel4lake-kernel.mb32"

echo "== build (x86_64-unknown-none) =="
./build-x86.sh >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }

# todo F1: die Konfiguration OHNE Pruefinfrastruktur wird HIER MITGEBAUT.
#
# Das ist der wichtigere Teil des Gatings. Ein `--no-default-features`-Build, den niemand baut,
# verrottet still -- und genau diese Fehlerform hat in diesem Projekt schon mehrfach zugeschlagen
# (leere Event-Queue, nie ausgefuehrter x86-Testpfad, DMAR-Ausschlusspfad). Gebootet wird er
# nicht: ohne `selftest` hat der Kernel derzeit keine Aufgabe (todo F2), er wuerde nur idlen.
# Geprueft wird also genau das, was pruefbar ist -- dass er uebersetzt und linkt.
echo "== build (--no-default-features: ohne Pruefinfrastruktur) =="
NOSEL_OK=1
rustup run nightly cargo build --release --no-default-features \
    --target x86_64-unknown-none -p sel4lake-kernel >/dev/null 2>&1 || NOSEL_OK=0
# Der Vergleich gehoert dazu: schrumpft das Image NICHT, ist das Gating wirkungslos geworden
# (jemand hat Testcode ausserhalb des Features abgelegt), und der Build allein wuerde das nicht zeigen.
NOSEL_TEXT=$(readelf -S build/target/x86_64-unknown-none/release/sel4lake-kernel 2>/dev/null \
    | grep -A1 " .text " | tail -1 | tr -s ' ' | cut -d' ' -f2)
./build-x86.sh >/dev/null 2>&1 || { echo "BUILD FAILED (Rueckbau der Default-Konfiguration)"; exit 1; }
SEL_TEXT=$(readelf -S build/target/x86_64-unknown-none/release/sel4lake-kernel 2>/dev/null \
    | grep -A1 " .text " | tail -1 | tr -s ' ' | cut -d' ' -f2)

# Zuverlaessiger Capture ueber eine Datei (Pipe + SIGKILL verliert sonst QEMUs stdout-Puffer).
LOG="$(mktemp)"
echo "== boot ($SECONDS_RUN s) =="
timeout "$SECONDS_RUN" qemu-system-x86_64 \
    -kernel "$ELF" -m "$RAM" -smp 4 "${ACCEL[@]}" \
    -machine q35,kernel-irqchip=split -device intel-iommu,caching-mode=on \
    -device virtio-rng-pci \
    -nographic -serial file:"$LOG" -no-reboot -no-shutdown \
    </dev/null >/dev/null 2>&1 || true
OUT="$(grep -vE "SeaBIOS|iPXE|Press Ctrl|Booting from|C900|PMM|PnP" "$LOG" 2>/dev/null)"
# Ein leerer Lauf sieht in der Auswertung aus wie "alle Pruefungen fehlgeschlagen" -- eine
# Fehldiagnose, die schlimmer ist als gar keine. Also unterscheiden: kam nichts an, ist das ein
# Problem des Aufbaus (QEMU, KVM, Zeitlimit), nicht des Kernels.
if [ -z "$OUT" ]; then
    echo "== KEIN OUTPUT: der Lauf hat nichts geliefert (Logdatei $(wc -c < "$LOG" 2>/dev/null || echo 0) Byte) =="
    echo "== Das ist KEIN Testergebnis -- Aufbau pruefen (KVM verfuegbar? Zeitlimit zu knapp?) =="
    rm -f "$LOG"
    exit 2
fi
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
check "pci     : ALL PASS"            "PCI-Enumeration ueber das ECAM-Fenster aus der ACPI-MCFG (virtio-rng gefunden, Bus-Master an)"
check "vtdcaps : ALL PASS" "VT-d-Faehigkeiten (Schritt 1): SAGAW/MGAW/ND/CM/RWBF/ECAP.C/QI/IR/SC/ScalableMode einmal gelesen und protokolliert; jede spaetere Bit-Entscheidung leitet sich daraus ab"
check "vtdgrp  : ALL PASS" "DMAR-Auswertung + Gruppenbildung (Schritt 2) gegen eine EINGESPEISTE Tabelle/Topologie: Catch-all zuletzt, Scope-Typ 2 als Subhierarchie, ACS-Gruppen, RID-Alias-Mengen, RMRR-Ausschluss, Firmware-Muell abgefangen, Vollstaendigkeits-Oracle"
check "apic    : x2APIC" "x2APIC aktiv (MSR-Pfad statt MMIO): schnellere IPIs, 64-Bit-ICR in einem Zugriff, und 32-Bit-APIC-IDs -- xAPIC kann nur 255 Kerne adressieren"
check "cycles  : ALL PASS" "Zyklenzaehler (Stufe 1): serialisierender Zeitstempel, invariant-TSC geprueft statt angenommen, gegen den PIT kalibriert"
check "dmawin  : ALL PASS" "IOVA-Fenstergrenzen (ext-36b) -- DIESELBE Funktion wie im ARM-Lauf, nicht nachgebaut"
check "dmatok  : ALL PASS" "Teardown-Token (ext-37) -- dieselbe Funktion wie im ARM-Lauf; VtdEnforcer::attach liefert jetzt Some"
check "iommu   : ALL PASS"            "IOMMU (VT-d): Bring-up aus der ACPI-DMAR, Root-Tabelle mit Default-Block, Uebersetzung aktiv, Invalidierung quittiert"
check "iso     : ALL PASS"            "Stufe 5: per-Prozess-Adressraeume (isolierte PD sieht fremdes RAM NICHT, SAS-PD schon)"
# Cache-Partitionierung (todo A1). SKIP ist hier ein EIGENES Ergebnis, kein PASS: meldet die
# Plattform keine Cache-Geometrie, gibt es genau eine Seitenfarbe, und "die Farbsaetze zweier PDs
# sind disjunkt" waere dann wahr, ohne geprueft zu sein. Genau diese Verwechslung -- Abwesenheit
# als Erfuellung zu lesen -- hat dieses Projekt bei der SMMU-Event-Queue schon einmal bezahlt.
if echo "$OUT" | grep -q "color   : SKIP"; then
    echo "  SKIP (Plattform meldet keine Cache-Geometrie -> 1 Farbe): Cache-Partitionierung zwischen PDs"
else
    check "color   : ALL PASS" "A1: zwei isolierte PDs teilen sich KEINE Cache-Farbe -- Region, Kernel-Stack und Seitentabellen jeder PD stammen aus disjunkten Farbsaetzen; eine Region jenseits der Streifenbreite wird abgewiesen statt fremde Farben mitzunehmen"
fi
check "audit   : ALL PASS"            "Stufe 4: Scheduler- + CDT-Audit sauber"
check "SELFTEST COMPLETE"             "Stufe 4: sauberes system_off (ACPI) statt Timeout"
# todo F1: Gating der Pruefinfrastruktur.
if [ "$NOSEL_OK" = 1 ]; then
    echo "  PASS: F1: der Kernel baut auch OHNE Feature 'selftest' (die schlanke Konfiguration verrottet nicht)"
else
    echo "  FAIL: F1: --no-default-features baut nicht mehr"; fail=1
fi
if [ -n "$SEL_TEXT" ] && [ -n "$NOSEL_TEXT" ] && [ $((0x$NOSEL_TEXT)) -lt $((0x$SEL_TEXT)) ]; then
    echo "  PASS: F1: .text schrumpft ohne 'selftest' von 0x$SEL_TEXT auf 0x$NOSEL_TEXT Bytes -- das Gating wirkt wirklich"
else
    echo "  FAIL: F1: .text schrumpft nicht (0x$SEL_TEXT -> 0x$NOSEL_TEXT) -- Testcode liegt ausserhalb des Features"; fail=1
fi
if [ "$fail" = 0 ]; then echo "== ALL PASS =="; else echo "== FAILURES =="; fi
exit "$fail"
