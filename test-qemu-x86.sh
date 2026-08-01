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

# **Wiederholungen** (todo B-1.3). Ein einzelner Durchlauf kann einen Nichtdeterminismus
# grundsaetzlich nicht finden -- er kann ihn nur zufaellig treffen. Genau daran ist der
# IRQ-Deadlock von 2026-07-29 monatelang vorbeigelaufen: die Suite lief einmal, und einmal reicht
# bei einer Ausfallquote von einem Achtel eben meistens.
#
# Vorgabe 1, damit der uebliche Aufruf schnell bleibt; im CI/nach Aenderungen an Locks, Scheduler
# oder Bring-up mit `RUNS=8 ./test-qemu-x86.sh` fahren. Gemeldet wird die QUOTE, und eine Quote
# unter 100 % ist ein FAIL -- nicht "meistens gruen".
RUNS="${RUNS:-1}"

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

# **Der gebootete Bau verlangt `selftest` AUSDRUECKLICH** -- nicht, weil es heute noetig waere
# (das Feature steht in `default`), sondern weil es das nach A-2.2 nicht mehr tut. Ohne diese
# Angabe boetete die Suite nach dem Dreh einen Kernel ohne Selbsttests und wartete auf ein
# `SELFTEST COMPLETE`, das nie kommt: kein FAIL, sondern Stille -- und Stille ist die schlechteste
# Art zu scheitern, weil sie wie Erfolg aussieht, bis jemand das Zeitlimit bemerkt.
echo "== build (x86_64-unknown-none, --features selftest) =="
./build-x86.sh --features selftest >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }

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
# Rueckbau MIT Feature -- das ist das Image, das gleich gebootet wird.
./build-x86.sh --features selftest >/dev/null 2>&1 || { echo "BUILD FAILED (Rueckbau)"; exit 1; }
SEL_TEXT=$(readelf -S build/target/x86_64-unknown-none/release/sel4lake-kernel 2>/dev/null \
    | grep -A1 " .text " | tail -1 | tr -s ' ' | cut -d' ' -f2)

# Zuverlaessiger Capture ueber eine Datei (Pipe + SIGKILL verliert sonst QEMUs stdout-Puffer).
LOG="$(mktemp)"
# Zaehlt Laeufe, die das Zeitlimit rissen -- s. die Begruendung in boot_once().
BOOT_TIMEOUTS=0

boot_once() {
    # KEIN `-no-shutdown`. Der Schalter haelt QEMU nach dem ACPI-Poweroff des Gastes am Leben;
    # der Lauf kostete dadurch IMMER das volle Zeitlimit, egal wie schnell der Kernel fertig
    # war. Gemessen am 2026-08-01 mit identischem Kernel und identischer Ausgabe (139 Zeilen,
    # `SELFTEST COMPLETE` in beiden):
    #
    #     mit  -no-shutdown : 130038 ms, rc=124 (vom Zeitlimit erschlagen)
    #     ohne -no-shutdown :    857 ms, rc=0   (sauber beendet)
    #
    # Faktor 152 -- aber der Zeitgewinn ist nicht der wichtigste Teil. Mit `-no-shutdown` lief
    # JEDER Lauf ins Zeitlimit, `rc` war also immer 124 und trug keine Information. Deshalb
    # musste ein Haenger bisher aus dem Logtext erschlossen werden (`bringup : WATCHDOG`).
    # Ohne den Schalter bedeutet rc=124 genau das, was es soll: der Gast hat sich nicht
    # heruntergefahren. Ein zweiter, unabhaengiger Melder fuer dieselbe Sache -- und einer,
    # den kein Kernelfehler stillstellen kann, weil er ausserhalb liegt.
    #
    # aarch64 macht es seit jeher so (test-qemu.sh: nur `-no-reboot`, PSCI SYSTEM_OFF).
    timeout "$SECONDS_RUN" qemu-system-x86_64 \
        -kernel "$ELF" -m "$RAM" -smp 4 "${ACCEL[@]}" \
        -machine q35,kernel-irqchip=split -device intel-iommu,caching-mode=on,intremap=on \
        -device virtio-rng-pci \
        -nographic -serial file:"$LOG" -no-reboot \
        </dev/null >/dev/null 2>&1
    local rc=$?
    [ "$rc" -eq 124 ] && BOOT_TIMEOUTS=$((BOOT_TIMEOUTS + 1))
    return 0
}

echo "== boot ($SECONDS_RUN s, $RUNS Lauf/Laeufe) =="
boot_once
OUT="$(grep -vE "SeaBIOS|iPXE|Press Ctrl|Booting from|C900|PMM|PnP" "$LOG" 2>/dev/null)"

# Wiederholungen (B-1.3, verschaerft durch B-1.2c).
#
# **Frueher wurde hier NUR `SELFTEST COMPLETE` gezaehlt.** Das war zweimal zu wenig:
#  1. Bis B-1.8 druckte der Kernel diesen Marker auch nach einem Watchdog-Abbruch -- ein
#     abgebrochener Lauf zaehlte also als Erfolg. Das ist behoben, aber es bleibt:
#  2. Ein Lauf kann den Marker erreichen und trotzdem eine Pruefung reissen. Genau das ist am
#     2026-07-29 passiert (`iso` mal 2x, mal 1x, mal 0x Faults; `color` einmal FAILURES) -- und
#     die Wiederholungsmessung haette geschwiegen, weil der Marker jedes Mal stand.
#
# Verglichen wird deshalb die **Ergebnissignatur**: alle Ergebniszeilen des Kernels
# (`xxx : ALL PASS|FAILURES|SKIP`) plus der Abschlussmarker, sortiert. Die Prueffunktionen unten
# sind reine greps auf genau diese Zeilen -- gleiche Signatur heisst also gleiches Testergebnis.
# **Ein Lauf, dessen Signatur abweicht, ist ein FAIL**, auch wenn er "auch gruen" aussieht: bei
# einer sporadischen Messung weiss man nicht, welcher der beiden Laeufe die Wahrheit sagt.
run_signature() {
    printf '%s\n' "$1" \
        | grep -oE '^[a-z0-9_]+ +: (ALL PASS|FAILURES|SKIP)|^== SELFTEST [A-Z]+( \(watchdog\))?' \
        | sort
}
REPEAT_OK=1
REPEAT_DONE=0
if [ "$RUNS" -gt 1 ]; then
    SIG0="$(mktemp)"; SIGN="$(mktemp)"
    run_signature "$OUT" > "$SIG0"
    REPEAT_DONE=1
    for n in $(seq 2 "$RUNS"); do
        boot_once
        OUTN="$(grep -vE "SeaBIOS|iPXE|Press Ctrl|Booting from|C900|PMM|PnP" "$LOG" 2>/dev/null)"
        run_signature "$OUTN" > "$SIGN"
        if cmp -s "$SIG0" "$SIGN"; then
            REPEAT_DONE=$((REPEAT_DONE + 1))
        else
            echo "== Lauf $n weicht vom ersten ab (< Lauf 1, > Lauf $n): =="
            diff "$SIG0" "$SIGN" | grep -E '^[<>]' | sed 's/^/     /'
        fi
    done
    rm -f "$SIG0" "$SIGN"
    [ "$REPEAT_DONE" = "$RUNS" ] || REPEAT_OK=0
    echo "== Wiederholungen: $REPEAT_DONE von $RUNS mit IDENTISCHER Ergebnissignatur =="
fi
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
check "ir      : ALL PASS" "B-3.2: Interrupt Remapping aktiv UND Compatibility-Format-Interrupts abgeschaltet -- ohne IR kann ein durchgereichtes Geraet beliebige Interrupt-Nachrichten erzeugen (MSI ist eine DMA-Schreibung, die die Uebersetzung nicht ansieht); mit IR, aber erlaubtem CFI bleibt die Tabelle umgehbar"
check "qi      : ALL PASS" "B-3.1: Queued Invalidation aktiv und der Interrupt-Entry-Cache invalidierbar -- fuer den gibt es KEINEN Registerpfad, er ist damit die Vorbedingung fuer Interrupt Remapping (B-3.2)"
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
# A-3.4: die Thread-Kapazitaet ist eine ZUSAGE, keine Eigenschaft des Testaufbaus. Geprueft wird,
# dass sie erreicht wird -- nicht bloss, dass irgendeine Zahl gemeldet wird.
NTHREADS=$(echo "$OUT" | grep -m1 -oE '^sched   : [0-9]+ Kern, [0-9]+ Thread-Slots' | grep -oE '[0-9]+ Thread-Slots' | grep -oE '^[0-9]+')
if [ -n "${NTHREADS:-}" ] && [ "$NTHREADS" -ge 10000 ]; then
    echo "  PASS: A-3.4: $NTHREADS Thread-Slots (Ziel 10000) -- die Kapazitaet haengt an der Zusage, nicht an der Kernzahl des Testaufbaus"
else
    echo "  FAIL: A-3.4: nur ${NTHREADS:-?} Thread-Slots, Ziel 10000"; fail=1
fi
# A-3.4 Teil 3: die PD-Kapazitaet ist eine Zusage, keine .bss-Konstante.
NPD=$(echo "$OUT" | grep -m1 -oE '^cap     : [0-9]+ Slots / [0-9]+ Objekte / [0-9]+ PDs' | grep -oE '[0-9]+ PDs' | grep -oE '^[0-9]+')
if [ -n "${NPD:-}" ] && [ "$NPD" -ge 10000 ]; then
    echo "  PASS: A-3.4: $NPD PD-Slots -- 10000 Threads koennen jetzt 10000 EIGENE Adressraeume haben, nicht nur geteilte"
else
    echo "  FAIL: A-3.4: nur ${NPD:-?} PD-Slots, Ziel 10000"; fail=1
fi
# A-3.4 Teil 4: dieselbe Zusage fuer die Kommunikation. Threads, Caps und Adressraeume waren
# gedreht -- eine PD ohne Endpoint ist aber kein Tenant, sondern ein Prozess, mit dem niemand
# reden kann. Geprueft wird, dass jede PD mindestens einen Endpoint UND eine Notification haben
# kann, nicht bloss, dass eine Zahl gemeldet wird.
NEP=$(echo "$OUT" | grep -m1 -oE '^ipc     : [0-9]+ Endpoints / [0-9]+ Notifications' | grep -oE '^ipc     : [0-9]+' | grep -oE '[0-9]+$')
NNT=$(echo "$OUT" | grep -m1 -oE '^ipc     : [0-9]+ Endpoints / [0-9]+ Notifications' | grep -oE '/ [0-9]+ Notifications' | grep -oE '[0-9]+')
if [ -n "${NEP:-}" ] && [ "$NEP" -ge 10000 ] && [ -n "${NNT:-}" ] && [ "$NNT" -ge 10000 ]; then
    echo "  PASS: A-3.4: $NEP Endpoints / $NNT Notifications -- jede der 10000 PDs kann Server sein; vorher waren es 32, ab der 33. PD gab es keinen Endpoint mehr"
else
    echo "  FAIL: A-3.4: nur ${NEP:-?} Endpoints / ${NNT:-?} Notifications, Ziel je 10000"; fail=1
fi
check "capsz   : ALL PASS" "A-3.4: der globale Cap-Space wurde nicht erschoepft -- gemessen am HOECHSTSTAND gleichzeitig belegter Slots, nicht am Endstand (ein Lauf, der zwischendurch an die Grenze stiess und danach aufraeumte, sieht am Ende harmlos aus)"
check "capsum  : ALL PASS" "A-3.4 Abschluss: die SUMME wird geprueft, nicht nur das Budget je PD -- die Slots ausserhalb aller PD-Budgets (Wurzelcaps des Kernels) bleiben in der Reserve; sonst bekaeme eine PD INNERHALB ihres Budgets kein Slot mehr"
# Die Summenpruefung darf nicht still ausfallen: eine zu kleine Zaehlflaeche ist ein eigener
# Befund, kein bestandener Test (dieselbe Trennung wie Code 8 im CDT-Audit).
if echo "$OUT" | grep -q "capsum  : Summenpruefung KONNTE NICHT LAUFEN"; then
    echo "  FAIL: A-3.4: die Summenpruefung konnte nicht laufen (Zaehlflaeche zu klein) -- das ist kein Bestehen"; fail=1
fi
check "iface   : ALL PASS" "A-4.4: die Versionssperre des Laders weist eine GEAENDERTE Schnittstellenversion ab und laesst die gleiche durch -- beide Ausgaenge belegt; eine andere program_id bleibt unberuehrt"
check "quiesce : ALL PASS" "A-4.2: der ruhende Punkt -- ein stillgelegter Endpoint weist NEUE Transaktionen ab (ERR_QUIESCING, nicht ERR_BADCAP: 'kommt gleich wieder' ist fuer den Client eine andere Lage als 'gibt es nicht'), laufende duerfen abschliessen; ein ZWEITER Austausch am selben Endpoint wird abgewiesen"
check "rebind  : ALL PASS" "A-4.1: atomares Umbinden -- Pruefung und Tausch unter EINEM Lock; OHNE Stilllegung wird abgewiesen (der Befund waere sonst eine Momentaufnahme), ein fremder Empfaenger blockiert, und im ueberlappenden Fall hat der Endpoint zu KEINEM Zeitpunkt null Empfaenger"
check "state   : ALL PASS" "A-4.3: Zustandsuebergabe ueber eine Region mit VERSIONIERTEM Kopf -- ein abweichendes state_version-Layout und eine fremde program_id werden ABGEWIESEN, statt die Bytes der alten Fassung im eigenen Sinn zu lesen (das waere kein Datenverlust, sondern ein fehlinterpretierter Zustand); eine frische Region meldet NoState statt 'Version 0'; der Uebernahmezaehler zaehlt weiter und wird von Abweisungen nicht erhoeht"
# Anmerkung: der ERNSTFALL (v2 uebernimmt den Zaehler von v1, Marker `ckpt`) laeuft NICHT auf x86 --
# der zustandsbehaftete Hot-Reload haengt an der arch-neutralen Thread-Demo, die hier nicht startet.
# Er wird von test-qemu.sh (aarch64) geprueft. Auf x86 belegt `state` die Torlogik, nicht den Lauf.
check "stripe  : ALL PASS" "B-4.2: erschoepfte Farbpartitionierung scheitert SAUBER -- der 5. Streifenversuch wird abgewiesen, statt den Satz der ersten PD still ein zweites Mal auszugeben; nach Freigabe wieder vergebbar (kein Leck)"
check "audit   : ALL PASS"            "Stufe 4: Scheduler- + CDT-Audit sauber"
# B-1.5: die erwartete Abwesenheit AUSSPRECHEN, statt sie durchlaufen zu lassen.
# Diese Suite bootet den Kernel nackt -- sie baut KEIN Boot-Archiv (keine `programs`, kein
# `mkarchive`, kein Manifest; das tut `test-qemu-x86-load.sh`). Also kann kein Root-Task laufen,
# und der Kernel sagt genau das: `archive : kein gueltiges Boot-Archiv` und `root : FAILURES
# (NoArchive)`. Sein Sammelbericht am Ende wiederholt es dann als nacktes `root : FAILURES` /
# `cdelete : FAILURES` -- ohne den Grund. In einem gruenen Lauf stehen damit zwei Zeilen, die nach
# Fehler aussehen und keiner sind, und wer das Log liest, kann erwartete von echter Meldung nicht
# trennen. Das ist "Stille sieht wie Erfolg aus" mit umgekehrtem Vorzeichen.
# Der Ausweg ist ein Check, KEIN Filter: geprueft wird, dass die Abwesenheit eintritt und begruendet
# gemeldet wird. Baut diese Suite eines Tages ein Archiv mit -- oder hoert der Kernel auf, den Grund
# zu nennen --, schlagen diese Zeilen an und zwingen zu einer Entscheidung. Ein Filter, der die
# FAILURES-Zeilen versteckt, wuerde in genau dem Fall schweigen.
check "archive : kein gueltiges Boot-Archiv" "B-1.5: ERWARTET -- diese Suite bootet ohne Boot-Archiv (das prueft die Lade-Suite)"
check "root    : FAILURES (NoArchive)" "B-1.5: ERWARTET -- ohne Archiv nennt der Kernel den Grund beim Namen, statt still zu idlen; die spaeteren nackten 'root/cdelete : FAILURES' im Sammelbericht folgen daraus"
# B-1.8: kam der Bericht aus `all_done()` oder aus der Notbremse? Bis 2026-07-29 war das am Log
# NICHT unterscheidbar -- `report_and_off()` druckte `SELFTEST COMPLETE` auch nach dem Watchdog,
# beide Zeilen standen im selben Lauf untereinander. Ausgerechnet dieser Marker traegt aber die
# Wiederholungsmessung (B-1.2/B-1.3 zaehlen ihn). Jetzt trennt der Kernel die Ausgaenge; hier wird
# die Trennung abgenommen. Ein Watchdog-Lauf ist ein FAIL, kein "meistens grün".
if echo "$OUT" | grep -q "bringup : WATCHDOG"; then
    echo "  FAIL: B-1.8: der Bericht kam aus der NOTBREMSE, nicht aus all_done() -- die Aussagen darunter sind zu einem Zeitpunkt abgelesen, nicht nach ihrem Beleg"; fail=1
else
    echo "  PASS: B-1.8: der Bericht kam aus all_done() (kein Watchdog) -- die Aussagen sind belegt, nicht abgelesen"
fi
# Der Rueckgabewert von QEMU als zweiter, unabhaengiger Melder (s. boot_once()).
#
# Seit `-no-shutdown` weg ist, heisst rc=124: der Gast hat sich NICHT heruntergefahren. Das ist
# eine Aussage von aussen -- sie haengt an keiner Zeile, die der Kernel selbst drucken muss.
# Genau darin liegt ihr Wert: die Watchdog-Erkennung (B-1.8) liest das Log, und ein Kernel, der
# in der falschen Lage schweigt, koennte sie taeuschen. Ein Zeitlimit kann er nicht taeuschen.
if [ "$BOOT_TIMEOUTS" -gt 0 ]; then
    echo "  FAIL: $BOOT_TIMEOUTS Lauf/Laeufe rissen das Zeitlimit von ${SECONDS_RUN}s -- der Gast"
    echo "        hat sich nicht heruntergefahren. Das ist ein Haenger, unabhaengig davon, was im"
    echo "        Log steht (s. todo D0)."
    fail=1
else
    echo "  PASS: kein Lauf riss das Zeitlimit -- jeder Gast fuhr selbst herunter (rc=0)"
fi
# B-1.2c-Waechter: ein Ergebniswort, das die Signatur NICHT kennt, ist unsichtbar.
#
# Am 2026-08-01 gemessen: `color` druckte als einzige Stelle im Kernel `FAIL` statt `FAILURES`
# (68 andere Tests schreiben `FAILURES`). Damit fiel ein durchgefallener color-Test aus der
# Ergebnissignatur HERAUS, statt als Abweichung aufzutauchen -- Lauf 4 von 8 zeigte als einzigen
# Unterschied eine FEHLENDE Zeile. Wer den Diff las, sah "eine Zeile weniger" und nicht "ein
# Test ist durchgefallen". Der Kommentar weiter oben in dieser Datei belegt, wie lange das
# unbemerkt blieb: dort steht "`color` einmal FAILURES" -- das Wort stand dort nie.
#
# Geprueft wird deshalb die KLASSE, nicht der Einzelfall: jede Zusammenfassungszeile muss eines
# der drei bekannten Woerter tragen. Zusammenfassungen haben Leerzeichen vor dem Doppelpunkt
# (`color   : ...`), Einzelzusagen nicht (`memtest: PASS  alloc 1 Seite`) -- deshalb das ` +`
# im Muster, sonst schluege der Waechter bei jeder Einzelzeile an.
UNBEKANNT="$(printf '%s\n' "$OUT" | grep -oE '^[a-z0-9_]+ +: (FAIL|PASS|OK|NOK|ERROR|ERRORS|FAILED|SUCCESS)\b' || true)"
if [ -n "$UNBEKANNT" ]; then
    echo "  FAIL: B-1.2c: Ergebniszeile(n) mit einem Wort, das die Signatur nicht kennt --"
    echo "        sie sind fuer die Wiederholungsmessung UNSICHTBAR (erlaubt: ALL PASS, FAILURES, SKIP):"
    printf '%s\n' "$UNBEKANNT" | sed 's/^/          /'
    fail=1
else
    echo "  PASS: B-1.2c: jede Zusammenfassungszeile nutzt ALL PASS|FAILURES|SKIP -- keine faellt aus der Signatur"
fi
check "SELFTEST COMPLETE"             "Stufe 4: sauberes system_off (ACPI) statt Timeout"
# todo F1: Gating der Pruefinfrastruktur.
if [ "$NOSEL_OK" = 1 ]; then
    echo "  PASS: F1: der Kernel baut auch OHNE Feature 'selftest' (die schlanke Konfiguration verrottet nicht)"
else
    echo "  FAIL: F1: --no-default-features baut nicht mehr"; fail=1
fi
# Der Vergleich ist bewusst zwischen den ZWEI BENANNTEN Bauten formuliert (mit Feature gegen
# ohne), nicht zwischen "Default" und "Nicht-Default". So bleibt er richtig, egal ob `selftest`
# in `default` steht oder nicht -- A-2.2 dreht genau das um, und eine Pruefung, die sich beim
# Drehen einer Vorgabe mitdrehen muss, ist eine Pruefung, die man dabei vergisst.
if [ -n "$SEL_TEXT" ] && [ -n "$NOSEL_TEXT" ] && [ $((0x$NOSEL_TEXT)) -lt $((0x$SEL_TEXT)) ]; then
    echo "  PASS: F1: .text ohne 'selftest' (0x$NOSEL_TEXT) < mit 'selftest' (0x$SEL_TEXT) -- das Gating wirkt wirklich"
else
    echo "  FAIL: F1: .text schrumpft nicht (mit: 0x$SEL_TEXT, ohne: 0x$NOSEL_TEXT) -- Testcode liegt ausserhalb des Features"; fail=1
fi
if [ "$RUNS" -gt 1 ]; then
    if [ "$REPEAT_OK" = 1 ]; then
        echo "  PASS: B-1.3/B-1.2c: $RUNS von $RUNS Laeufen mit IDENTISCHER Ergebnissignatur -- reproduzierbar,"
        echo "        und zwar im Ergebnis, nicht bloss im Durchlaufen"
    else
        echo "  FAIL: B-1.3/B-1.2c: nur $REPEAT_DONE von $RUNS Laeufen mit identischer Ergebnissignatur."
        echo "        Eine Quote unter 100 % ist KEIN 'meistens gruen', sondern ein Nichtdeterminismus --"
        echo "        und bei abweichenden Signaturen weiss niemand, welcher Lauf die Wahrheit sagt."
        echo "        Die abweichenden Zeilen stehen oben. Verdaechtig sind Sperren, IRQ-Maskierung,"
        echo "        der SMP-Hochlauf (s. todo D0) und zeitkritische Messungen im Bericht."
        fail=1
    fi
fi
if [ "$fail" = 0 ]; then echo "== ALL PASS =="; else echo "== FAILURES =="; fi
exit "$fail"
