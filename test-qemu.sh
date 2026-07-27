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
# Der Kernel fährt nach bestandenem Selbsttest QEMU per PSCI SYSTEM_OFF herunter
# (`== SELFTEST COMPLETE ==`), daher endet ein erfolgreicher Lauf, sobald er fertig
# ist — auf einem ruhigen Host in wenigen Sekunden. Das Timeout ist nur eine
# Obergrenze für den Fehlerfall (Kernel bleibt dann im Idle). Großzügig gewählt:
# zwei Fuzzer (Ressourcen + IPC) + SMP + isolierte VSpaces teilen unter
# single-threaded QEMU-TCG eine Host-CPU; unter schwerer Host-Last emuliert alles
# deutlich länger (kein Kernel-Hang — der Manager druckt sonst eine `DBG pending`-Zeile).
SECONDS_RUN="${1:-360}"
ELF="build/target/aarch64-sel4lake/release/sel4lake-kernel.elf"

# In-Kernel-Fuzzer (ADR 0013) sind ein optionales Feature. Default: RELEASE-Build OHNE Fuzzer
# (genau die Konfiguration des Langzeittests/Produktivkernels) -> die vier Fuzzer-Checks entfallen.
# Mit `KERNEL_FUZZ=1 ./test-qemu.sh` wird `--features kernel-fuzz` gebaut und die Fuzzer mitgeprueft.
FEAT="${KERNEL_FUZZ:+--features kernel-fuzz}"
echo "== build ${FEAT:-(release, ohne Fuzzer)} =="
./build.sh $FEAT >/dev/null 2>&1 || { echo "BUILD FAILED"; exit 1; }

# ext-26 (L1): die EXTERNEN Programme bauen (eigener Workspace, eigene Target/Linker) + ins
# Boot-Archiv legen (Host-Tool tools/mkarchive.py, KEIN Kernelcode). `hello` ist ein echtes,
# extern gebautes EL0-Programm, das der Kernel laedt + ausfuehrt; `probe` ist ein Platzhalter,
# der den Multi-Modul-/Listen-Pfad zeigt.
mkdir -p build
( cd programs && rustup run nightly cargo build --release ) >/dev/null 2>&1 \
    || { echo "PROGRAMS BUILD FAILED"; exit 1; }
# ext-27: die ADVERSARIALEN Testdienste (eigener tests/-Workspace) bauen — geladen wie Drittsoftware,
# greifen den Kernel + sich gegenseitig an (ADR 0012). NICHT Teil des Kernel-Images.
( cd tests && rustup run nightly cargo build --release ) >/dev/null 2>&1 \
    || { echo "TESTS BUILD FAILED"; exit 1; }
HELLO="programs/build/target/aarch64-sel4lake-user/release/hello.elf"
SVCDEMO="programs/build/target/aarch64-sel4lake-user/release/svc-demo.elf"
TBIN="tests/build/target/aarch64-sel4lake-user/release"
printf 'PLACEHOLDER' > build/_probe.bin
# ext-28 (ADR 0014): TrustedSAS-Binaries signieren. tools/sign_trusted.py fuehrt zuerst den
# Unsafe-Audit (Allowlist {libsel4lake}) durch -> KEIN Zertifikat bei Verletzung, dann SHA-256-
# Bindung an genau dies ELF + Ed25519-Signatur ueber die volle Nachricht. program_id/version MUESSEN
# zum Archiv-Eintrag passen (Identitaets-Bindung). Schluessel: keys/trusted-test (privat, gitignored).
mkdir -p certs
sign() { python3 tools/sign_trusted.py --key keys/trusted-test.ed25519 "$@" >/dev/null 2>&1; }
sign --crate programs/trusted/svc-demo --elf "$SVCDEMO" --program-id 12 --version 1 \
     --out certs/trusted-x.cert || { echo "SIGN trusted-x FAILED"; exit 1; }
sign --crate tests/services/trusted/aggressor --elf "$TBIN/aggressor-t.elf" --program-id 24 --version 1 \
     --out certs/aggressor-t.cert || { echo "SIGN aggressor-t FAILED"; exit 1; }
# hello=UserLand(2), hwhello=HardwareLand(1) (gleiches ELF, L3); trusted-x=TrustedSAS(0): das saubere,
# zertifizierte svc-demo (laedt GELADEN als EL0-isolierte PD). probe=Platzhalter (Multi-Modul-Liste).
# ext-27 Testdienste je Domaene (2 je Domaene): aggressor/intruder -u=UserLand(2), -h=HardwareLand(1),
# -t=TrustedSAS(0). ext-28: TrustedSAS-Module tragen ein Zertifikat (7. Feld); aggressor-t ist
# zertifiziert (laedt + attackiert), intruder-t bewusst OHNE Zertifikat -> verify_image weist es ab.
python3 tools/mkarchive.py build/boot-archive.bin \
    10:hello:2:1:"$HELLO" 11:hwhello:1:1:"$HELLO" 12:trusted-x:0:1:"$SVCDEMO"::certs/trusted-x.cert 2:probe:2:1:build/_probe.bin \
    20:aggressor-u:2:1:"$TBIN/aggressor-u.elf" 21:intruder-u:2:1:"$TBIN/intruder-u.elf" \
    22:aggressor-h:1:1:"$TBIN/aggressor-h.elf" 23:intruder-h:1:1:"$TBIN/intruder-h.elf" \
    24:aggressor-t:0:1:"$TBIN/aggressor-t.elf"::certs/aggressor-t.cert 25:intruder-t:0:1:"$TBIN/intruder-t.elf" \
    >/dev/null 2>&1 || { echo "ARCHIVE BUILD FAILED"; exit 1; }

echo "== boot ($SECONDS_RUN s) =="
# ext-23: SMMUv3 (IOMMU) + virtio-rng-pci HINTER einem pcie-root-port (StreamID = PCI-RID).
# QEMUs SMMUv3 uebersetzt nur Endpunkte hinter einem Root-Port (integrierte Bus-0-Endpunkte
# umgehen die SMMU) -> das DMA-Beweisgeraet haengt am Root-Port. Der Kernel ignoriert beide,
# solange die ext-23-Treiber nicht aktiv sind (Rueckwaertskompatibilitaet).
OUT="$(timeout --signal=KILL "$SECONDS_RUN" qemu-system-aarch64 \
    -machine virt,iommu=smmuv3 -cpu cortex-a72 -smp "$CORES" -m 4G \
    -nographic -serial mon:stdio -no-reboot \
    -net none -device pcie-root-port,id=rp0,chassis=1 -device virtio-rng-pci,bus=rp0,iommu_platform=on \
    -device loader,file=build/boot-archive.bin,addr=0x13F000000 \
    -kernel "$ELF" </dev/null 2>/dev/null)"

echo "$OUT"
echo "== checks =="
fail=0
check() { if echo "$OUT" | grep -q "$1"; then echo "  PASS: $2"; else echo "  FAIL: $2"; fail=1; fi; }
# Fuzzer-Check: nur im `KERNEL_FUZZ=1`-Lauf relevant (im Release-Build ohne Fuzzer entfaellt die
# Zeile -> nicht als Fehler werten, sondern als bewusst uebersprungen melden).
fcheck() { if [ -n "${KERNEL_FUZZ:-}" ]; then check "$1" "$2"; else echo "  SKIP (release ohne Fuzzer): $2"; fi; }

check "M=1 C=1 I=1" "MMU + Caches aktiv"
check "dtb     : ALL PASS" "DTB-Parsing (RAM-Größe aus dem Device Tree)"
check "archive : 10 Modul" "ext-26 L0: Boot-Archiv extern geladen + vom Kernel-Parser gelesen (reserviertes RAM-Fenster, sel4lake-loader)"
check "memtest : ALL PASS" "Speichermodell-Selbsttest (alloc/split/transfer/free)"
check "zerotest: ALL PASS" "Datenremanenz (ext-29): frische UND wiederverwendete Allokationen sind genullt -- kein Restdatenleck zwischen Subjekten"
check "captest : ALL PASS" "Capability-Selbsttest (copy/mint/move/delete/revoke)"
check "dmaalign: ALL PASS" "DMA-Granularitaet (ext-35): unausgerichteter Anfang/angebrochene Laenge werden an der Cap-Praegung abgewiesen (Cache-Wartung wuerde sonst fremde Daten in der Randzeile verwerfen)"
check "budget  : ALL PASS" "Cap-Budget je PD (ext-29): Installationen ueber CAP_BUDGET_PER_PD hinaus abgewiesen (keine Monopolisierung der geteilten Cap-Tabelle), Ersetzen bleibt erlaubt"
check "sched   : ALL PASS" "Scheduler (Preemption auf core 0 + alle Kerne ticken)"
check "fp      : ALL PASS" "FP/SIMD-Kontext bleibt über Preemption erhalten"
check "prio    : ALL PASS" "Bitmap-Prioritäten (höhere Priorität läuft zuerst)"
check "life    : ALL PASS" "Thread-Lebenszyklus (cap-KILL + EXIT + Stack-Rückgewinnung)"
check "notif   : ALL PASS" "Notifications (asynchrone Badge-Signale)"
check "xfer    : ALL PASS" "Capability-Transfer in IPC (Broker delegiert Service-Cap)"
check "grantlk : ALL PASS" "Grant-Leak-Regression (ext-29): 65 Grants in denselben Empfangs-Slot -> verdraengte Cap wird freigegeben, genau 1 lebende Ableitung, kein Leck in der geteilten Cap-Tabelle (Cross-PD-DoS)"
check "ipc     : ALL PASS" "Cap-gesicherte IPC (Server v1, PD<->PD)"
check "reload  : ALL PASS" "Hot-Reload (Server v2 ersetzt v1, gleicher Endpoint, kein Reboot)"
check "ckpt    : ALL PASS" "Stateful Hot-Reload (Zustand bleibt über v1->v2 erhalten)"
check "el0     : ALL PASS" "EL0-Userland (echter User-Thread ruft per Syscall)"
check "el0iso  : ALL PASS" "EL0-Isolation (Kernel-Zugriff faultet, Thread beendet, Kernel überlebt)"
check "scale   : ALL PASS" "Skalierung (ext-30): 1024 Threads GLEICHZEITIG (Kapazitaet zur Boot-Zeit aus dem RAM statt .bss-Konstante), alle auffindbar, danach vollstaendig abgebaut -> Slots + RAM zurueck auf Baseline"
check "migrate : ALL PASS" "Thread-Migration (ext-30): Thread wechselt zur Laufzeit den Kern -- gleiche ThreadId/Tcb-Cap, laeuft auf dem Zielkern weiter, balance_once() verschiebt automatisch, cross-core-KILL, Scheduler-Audit 0"
check "smp     : ALL PASS" "Per-Kern-paralleler Scheduler (Worker je Kern + Cross-Core-IPI-Wake)"
check "xipc    : ALL PASS" "Kern-übergreifende synchrone IPC (Client core0 <-> Server core2)"
check "reclaim : ALL PASS" "EL0-Kernel-Stack-Reclaim (>Pool-viele transiente EL0-Threads)"
check "balance : ALL PASS" "Lastausgleich (lastbewusste Thread-Platzierung über die Kerne)"
check "vspace  : ALL PASS" "Per-Prozess-VSpace (isolierte PD faultet bei Fremdzugriff, SAS-PD darf)"
check "vmm     : ALL PASS" "Allgemeiner VMM (Frame-Caps + map/unmap-Syscalls + VSpace-Teardown)"
check "shm     : ALL PASS" "Shared-Memory-IPC (ein Frame in zwei isolierte VSpaces, cap-gewährt)"
check "native  : ALL PASS" "Natives Code-Laden je isolierter VSpace (privates EL0-RX, W^X)"
check "pages4k : ALL PASS" "4-KiB-Seiten (gemischte RW/RO-Rechte + Guard Pages + L3)"
check "churn   : ALL PASS" "Ressourcen-/Teardown-Invarianten (tausende spawn/destroy ohne Leak)"
check "mcs     : ALL PASS" "MCS Scheduling Contexts (Budget per Cap, Verbrauch/Erschoepfung/Refill, Drosselung)"
check "stale   : ALL PASS" "Audit-Regression: IPC paniert nicht bei gekilltem, in Endpoint-Queue blockiertem Thread"
check "strand  : ALL PASS" "Audit-Regression: erneutes Budget-Bind strandet keinen erschoepften Thread"
check "rgone   : ALL PASS" "Reply-Liveness: toter Reply-Owner entblockt den CALL-Aufrufer (ERR_SERVER_GONE)"
check "ddon    : ALL PASS" "Budget-Donation: intra-core CALL belastet Server-Arbeit gegen das Aufrufer-Budget"
check "rcap    : ALL PASS" "First-class Reply-Cap (ObjectKind::Reply): Revocation bricht den ausstehenden Call ab"
check "rmig    : ALL PASS" "Reply-Cap-Server-Migration: ausstehender Call ueberlebt einen Hot-Reload (v2 schliesst ihn ab)"
fcheck "fuzz    : ALL PASS" "Generativer Kernel-Fuzzer (zufaellige Op-Sequenzen + Baseline-Oracle + SMP-Kontention)"
fcheck "ipcfuzz : ALL PASS" "IPC-State-Machine-Fuzzer (nebenlaeufige Aktoren, KILL/Reload/MCS waehrend IPC, Queue-Oracle)"
check "caplk   : ALL PASS" "CAPS-Reader-Writer-Lock: parallele Cap-Lookups (zwei Kerne halten gleichzeitig den Read-Lock)"
check "domain  : ALL PASS" "Sicherheitsdomaenen: Domaenen-Policy-Oracle (Cap-Typen je Domaene + untrusted Domaenen isoliert)"
check "pdctl   : ALL PASS" "UserLand-Management: cap-gated SYS_PDCTL (PAUSE/RESUME/STOP, nur TrustedSas->UserLand)"
check "chan    : ALL PASS" "Paarweiser Treiber<->Backend-Kanal: unveraenderliche Bindung bei Backend-Erzeugung, 1:N, nur Partner"
check "rtc     : ALL PASS" "RTC-HardwareLand-Backend: generisches MMIO-Cap + vspace_map_device, echtes PL031 RTC_DR-Read ueber Kanal"
check "irq     : ALL PASS" "RTC-IRQ: IRQ-Cap + GIC-SPI-Routing + Deferred-IRQ-Zustellung als Notification an HardwareLand"
check "dma     : ALL PASS" "DMA-Capability: DmaCap hinter DmaEnforcer-Abstraktion, kernel-ausgeschnittene Region, Normal-NC-Mapping, EL0-Round-Trip + Kohaerenz, dma_audit"
check "pcie    : ALL PASS" "PCIe-ECAM-Enumeration: virtio-rng-pci gefunden, BAR-Zuweisung + Bus-Master-Enable, RID == SMMU-StreamID"
check "smmu    : ALL PASS" "SMMUv3-Bring-up hinter DmaEnforcer: Command-/Event-Queue + Stream-Tabelle, Default-Abort, CR0-Enable, CMD_SYNC-Round-Trip"
check "smmubind: ALL PASS" "SMMU-Bindung: enable_dma/disable_dma installiert STE->CD->Stage-1 (nur die DMA-Region), Revoke gibt Tabellen frei (balanciert)"
check "virtiorng: ALL PASS" "virtio-rng-DMA: Geraet DMAt echte Zufallsbytes in die DmaCap-Region; zweistufig: Level-1-Software-Bounds weist Out-of-Window demonstrierbar ab, Level-2-SMMU als HW-Backstop (QEMU emuliert-Geraet-Bypass)"
check "dmagen  : ALL PASS" "Generische DMA-Infra (ext-24): Richtung/Kohaerenz als DmaCap-Attribute (richtungsminimales SMMU-AP), Multi-Region-Kontext, Stream-Gruppen, Scatter-Gather-Validierung, disjunkte Sub-Puffer (SG-Pfad)"
check "sasheap : ALL PASS" "Prozess-Heap (ext-25): echter Box/Vec/BTreeMap-Heap auf realen Physadressen; Hybrid-Allokator (Slabs+Bump) ueber Regionsliste + grow/shrink; Testcode 100% safe, unsafe nur in der Region-Runtime"
check "load    : ALL PASS" "Binary-Loader (ext-26): extern gebautes EL0-Programm aus dem Boot-Archiv geladen + ausgefuehrt (ELF64-Parse in Safe Rust, Segment-Kopie W^X an Link-VA, cap-gegatete isolierte PD + Endowment) -- Prozess NICHT im Kernel-Image"
check "sysload : ALL PASS" "Binary-Loader L2 (ext-26): Laden zur LAUFZEIT via SYS_LOAD-Syscall, cap-gegatet ueber Loader-Cap; Caller delegiert eigene Notification-Cap in die neue PD; ohne Loader-Cap -> ERR_BADCAP"
check "loadhw  : ALL PASS" "Binary-Loader L3 (ext-26) + ext-28: HardwareLand-Programm in vor-erstellte Backend-PD geladen, signalisiert Kanal; TrustedSAS (zertifiziertes svc-demo) laeuft GELADEN EL0-isoliert -- nur mit gueltigem Zertifikat + trust_audit==0"
check "loadstop: ALL PASS" "Binary-Loader L4 (ext-26): geladenen Prozess vollstaendig abgebaut (Thread+VSpace+geladene Segmente+Kstack+PD) -> Ressourcen-Baseline wiederhergestellt, kein Leck"
fcheck "loaderfuzz: ALL PASS" "Binary-Loader L5 (ext-26): Loader-Fuzzer -- fehlerhafte ELF-Varianten durch load_image alle abgelehnt (kein Crash, Parser forbid(unsafe_code)), Baseline unveraendert, loader_audit==0"
fcheck "certfuzz: ALL PASS" "TrustedSAS-Zertifikats-Fuzzer (ext-28, ADR 0014): mutierte/zufaellige Zertifikate + Identitaets-/Binary-Transplantation durch das verify_image-Gate alle abgelehnt (echtes Cert akzeptiert, kein Crash/OOB, no-alloc Krypto), trust_audit+loader_audit==0"
check "aggru   : ALL PASS" "Adversariale Testdienste (ext-27 T0): extern geladener UserLand-Aggressor -- Cap-Confusion (leerer Slot/falscher Typ/falsche Rechte) + Autoritaets-Eskalation (PDCTL/LOAD/KILL ohne Cap) alle als BADCAP/RIGHTS/BADSYS abgewiesen; Dienst signalisiert SUCCESS nur bei voller Abweisung; Audits==0"
check "intru   : ALL PASS" "Adversariale Testdienste (ext-27 T1): extern geladener UserLand-Intruder -- liest Kernel-RAM aus EL0 -> Translation-Fault (nicht in der isolierten VSpace gemappt) -> Kernel terminiert den Angreifer + laeuft weiter; PRE-Badge + el0_fault_count++ + Audits==0 (Hardware-Isolation)"
check "aggrh   : ALL PASS" "Adversariale Testdienste (ext-27 T2): extern geladenes HardwareLand-Backend als Aggressor -- KEINE Management-Autoritaet (PDCTL/LOAD/KILL -> BADCAP), nichts ausserhalb des eigenen Kanals; Cap-Confusion abgewiesen; meldet SUCCESS ueber den Kanal; Audits==0"
check "intrh   : ALL PASS" "Adversariale Testdienste (ext-27 T2): extern geladenes HardwareLand-Backend als Intruder -- Kernel-RAM-Zugriff aus EL0 faultet ebenso (Speicher-Isolation domaenen-unabhaengig); Kernel ueberlebt; PRE + el0_fault_count++ + Audits==0"
check "aggrt   : ALL PASS" "Adversariale Testdienste (ext-27 T3): extern geladener TrustedSAS-Aggressor (EL0-isoliert) -- Trust != Privileg: ohne tatsaechliche PdControl/Loader-Cap PDCTL/LOAD/KILL = BADCAP; Cap-Confusion abgewiesen; SUCCESS nur bei voller Abweisung; Audits==0"
check "intrt   : ALL PASS" "TrustedSAS-Zertifikats-Gate (ext-28, ADR 0014): UNZERTIFIZIERTES TrustedSAS wird abgewiesen -- intruder-t traegt absichtlich unsafe (nicht zertifizierbar) + liegt OHNE Zertifikat im Archiv -> verify_image lehnt das Laden mit Unverified ab; KEIN Thread/keine PD; Audits==0"
check "cross   : ALL PASS" "Adversariale Testdienste (ext-27 T4): Cross-Service-Matrix -- 3 extern geladene Angreifer DREIER Domaenen NEBENLAEUFIG (aggressor-u + aggressor-t melden unabhaengig SUCCESS, intruder-h faultet); gleichzeitige cross-domain Angreifer stoeren einander nicht; kernel-geschuetztes Canary unberuehrt; Audits==0"
fcheck "hwfuzz  : ALL PASS" "Domaenen/HW-Fuzzer: HW-/Management-Cap-Churn gegen Domaenen-Policy + CDT/VSpace-Oracle + Ressourcen-Baseline"
online=$(echo "$OUT" | grep -c "online")
[ "$online" -eq "$CORES" ] && echo "  PASS: alle $CORES Kerne online" || { echo "  FAIL: nur $online/$CORES Kerne online"; fail=1; }

echo "== $([ $fail -eq 0 ] && echo 'ALL PASS' || echo 'FAILURES') =="
exit $fail
