# Burn-in #2 — Continuous-Soak-Test (Plan) + Stabilitäts-Aussagekraft-Analyse

Status: **IMPLEMENTIERT** (nach ext-28). Feature `soak` (`kernel/src/threads/soak.rs`, Kernel-Kern
byte-identisch) + Host-Orchestrator `tools/soak.py`. Aufruf: `tools/soak.py --hours H`. Der laufende
Bericht entsteht unter `build/soak/report.md`. Validierung (Heartbeat temporär 3 s): >10 000 Epochen,
freies RAM byte-identisch konstant, `cap_obj`/`cap_slots` konstant, `loads==dmas==Epochen`, alle
Audits 0, 0 Anomalien. Dieses Dokument (a) bewertet ehrlich, welche Stabilitätsaussagen Burn-in #1
erlaubt und
welche **nicht**, und (b) leitet daraus den komplementären Continuous-Soak-Test ab. Ziel: zwei
komplementäre Stabilitätsnachweise — (1) Reboot/Power-Cycle, (2) Dauerbetrieb **einer** Instanz.

## A. Was Burn-in #1 (Reboot/Power-Cycle, Release ohne `kernel-fuzz`) wirklich beweist

Methode: der **unveränderte** Release-Kernel wird tausendfach kalt gebootet; jeder Lauf ist ein
vollständig auditierter, balance-geprüfter End-to-End-Durchlauf (Selbsttest) und endet per PSCI
SYSTEM_OFF. Bewertung je Dimension:

| Dimension | Aussagekraft #1 | Begründung |
|---|---|---|
| **Kaltstart-/Boot-Robustheit** | **STARK ✓** (Kernaussage) | Jede Iteration durchläuft Reset → MMU/Caches → SMP-Bring-up (8 Kerne) → Subsystem-Init. Tausende identische Kaltstarts ohne Abweichung = harter Beleg. |
| **Vollständige Ressourcenbilanz über viele Reboots** | **PER LAUF stark ✓ · kumulativ N/A** | Jeder Lauf stellt seine Baseline wieder her (churn 2000 Zyklen→Baseline, loadstop/sasheap-Balance, freies RAM konstant, cap/vspace/kstack zurück, alle Audits 0) — und das **identisch** über alle Läufe. Eine *kumulative* Drift über Reboots gibt es strukturell nicht (QEMU setzt RAM je Boot zurück). |
| **Deterministische Teststabilität** | **STARK ✓** (Kernaussage) | Dasselbe komplexe Szenario (konstante Metriken: freeMiB, el0-trap, domain_audit, ALL-PASS-Zahl) gelingt tausendfach **ohne Flakiness** — kein seltener Fehlerpfad. |
| **Langzeit-Speicherkonsistenz** | **NUR kurzfristig (~6 s/Lauf) · NICHT langfristig ✗** | Belegt: kein Leak über die ~6 s Selbsttest-Aktivität (inkl. 2000 Churn-Zyklen) je Lauf. **Nicht** belegt: Drift/Fragmentierung/Allokator-Degradation einer Instanz über **Stunden** — keine Instanz lebt länger als einen Lauf. |
| **Dauerbetrieb einer einzelnen Kernelinstanz** | **AUSDRÜCKLICH NICHT ✗** | Jede Instanz lebt ~6 s, dann SYSTEM_OFF. Über das Verhalten einer Instanz über Stunden sagt der Test **nichts**. → Genau das Ziel von Burn-in #2. |
| **Verhalten unter zufälligen Ereignisfolgen** | **NICHT ✗** | Der Release-Selbsttest ist deterministisch; randomisierte Op-Sequenzen sind Aufgabe der Fuzzer (`kernel-fuzz`, bewusst ausgeschlossen). Einzige Nichtdeterminismus-Quelle: SMP-/TCG-Scheduling-Jitter — das ist **keine** zufällige Ereignisfolge. |
| **Verhalten unter hoher Parallelität** | **PARTIELL ✓ (beschränkt)** | Jeder Lauf nutzt 8 Kerne (SMP-Scheduler, Cross-Core-IPC/IPI, per-Kern-Worker, `caplk` nebenläufige CAPS-Leser, ext-27 `cross` = 3 gleichzeitige cross-domain Angreifer). Das ist **moderate, fixe** Parallelität — **keine** anhaltende Hochlast-Kontention über Zeit. |
| **Verhalten auf realer Hardware statt TCG** | **GAR NICHT ✗** | Alles läuft unter QEMU-TCG (cortex-a72, software-emulierte MMU/SMMU, weitgehend starkes Speichermodell). Reale Schwach-Speicher-Ordnung, Cache-/TLB-Timing, echte IRQ-/Geräte-Timings, echte SMMU-Durchsetzung (QEMU setzt emulierte Geräte-DMA nicht durch) sind **nicht** abgedeckt. Gilt für #1 **und** #2. |

**Fazit #1:** Stärken sind **Kaltstart-Robustheit**, **deterministische Teststabilität** und
**Per-Lauf-Ressourcenbilanz** — alles mit sehr hoher Wiederholungszahl. Ausdrückliche Lücken:
**Dauerbetrieb einer Instanz**, **Langzeit-Speicherkonsistenz über Stunden**, **zufällige
Ereignisfolgen**, **anhaltende Hochlast**, **reale Hardware**.

## B. Burn-in #2 — Continuous-Soak einer einzelnen Kernelinstanz

**Ziel:** genau die Lücken schließen, die #1 offenlässt — vor allem **Dauerbetrieb einer Instanz**
und **Langzeit-Speicherkonsistenz**. Eine **einzige** Kernelinstanz läuft viele Stunden **ohne
Neustart** und arbeitet kontinuierlich.

### Grundsatz: Kernel unverändert, nur Harness/Testdienste erweitert
Neues Cargo-Feature **`soak`** (nicht in `default`, analog `kernel-fuzz`). Nur der Selbsttest-
**Harness** (`kernel/src/threads/…`, Testcode) erhält einen Soak-Treiber; der **Kernel-Kern**
(System-Calls, Audits, Speicher, Caps, Scheduler, IPC, Loader, DMA) bleibt **byte-identisch** zum
Release. Der Default-Release-Build ist damit unverändert; der Soak ist ein separater Feature-Build.

### Soak-Treiber (Harness, `#[cfg(feature = "soak")]`)
Nach dem regulären Selbsttest fährt der Idle-Manager **nicht** SYSTEM_OFF, sondern eine
**Endlos-Soak-Schleife** in Epochen. Jede Epoche rotiert über die **bereits vorhandenen**
Operationen (keine neue Kernel-Funktionalität, nur deren Daueraufruf):
1. externe Dienste laden → laufen lassen → vollständig abbauen (`destroy_loaded`) — alle 3 Domänen
   inkl. ext-27-Angreifer (Loader + Isolation + IPC + DMA-Kanal kontinuierlich);
2. Hot-Reload-Zyklen (Server v1↔v2 wiederholt);
3. Region-/Frame-Churn (alloc/free, sasheap-artig);
4. DMA-Round-Trips (virtio-rng + DmaCap-Region);
5. nebenläufige cross-domain Angreifer (ext-27 `cross`) wiederholt;
6. Ressourcen-Erschöpfung (Reclaim-Pool wiederholt ausreizen).
**Leak-Neutralität:** jede Epoche baut vollständig ab. Am Epochenende: **alle** Audits
(`domain/cap_cdt/vspace/dma/loader/ipc_audit`) + Ressourcen-Baseline **gegen den Soak-START**
prüfen → jede Drift = echter Befund. Optional seed-randomisierte Epochen-Reihenfolge (Testlogik,
kein Kernel-Feature) → deckt teilweise „zufällige Ereignisfolgen" ab; kombinierbar mit
`--features soak,kernel-fuzz` für volle Randomisierung im Dauerbetrieb.
**Heartbeat:** alle N Epochen (~1×/Minute) eine `SOAK` -Zeile auf die serielle Konsole mit: Epoche,
Uptime (Ticks), freies RAM, Region-Balance, Cap-Anzahl, aktive Prozesse, Loader-/DMA-/IPC-Zähler,
Fault-Zähler, Audit-Ergebnisse.

### Host-Orchestrator `tools/soak.py`
Baut `--features soak`, bootet **eine** lange QEMU-Instanz (**kein** SYSTEM_OFF — läuft die Zieldauer
durch; der Host beendet QEMU nach Ablauf bzw. bei Stillstand), liest den seriellen Strom
**kontinuierlich**, parst die Heartbeats und verfolgt **Kurven über die Zeit**:
- **Speicher-Kurve** (freies RAM Start→Verlauf→Ende) — der zentrale Langzeit-Konsistenz-Indikator;
- Cap-Anzahl-, Region-Balance-, VSpace-/Kstack-Kurven (monotone Drift = Leak);
- Audit-Zeitreihe (bei jedem Heartbeat 0?);
- Fault-/Panic-Erkennung + **Heartbeat-Lücken** (= Hang/Stillstand);
- kumulative Op-Zähler (Prozesse, Hot-Reloads, IPCs, DMA, Loader-Aufrufe).
Erzeugt den Soak-Bericht (Speicher-Kurve Start↔Ende, Drift-Analyse, Audit-Zeitreihe, Anomalien,
Uptime, Empfehlungen).

### Erfolgskriterien (Soak)
- Freies RAM / Cap-Anzahl / Region-/VSpace-/Kstack-Balance über die **gesamte** Laufzeit **stabil**
  (keine monotone Drift; Rückkehr zur Start-Baseline je Epoche);
- **alle** Audits == 0 bei **jedem** Heartbeat;
- keine Panic/Assertion, kein unerwarteter Fault, **keine Heartbeat-Lücke** (kein Hang);
- Ziel-Uptime (viele Stunden) erreicht.

### Was Soak #2 beweist (Komplement zu #1)
Dauerbetrieb einer Instanz ✓ · Langzeit-Speicherkonsistenz ✓ · anhaltende Last über Zeit ✓ ·
(mit `+kernel-fuzz`) zufällige Ereignisfolgen im Dauerbetrieb ✓. **Weiterhin offen für beide:**
reale Hardware statt TCG — separater Validierungsschritt vor einer formalen Verifikation
empfohlen (echte Schwach-Speicher-Ordnung, Cache/TLB, SMMU-Durchsetzung auf z. B. STM32MP25).

## C. Reihenfolge

1. Burn-in #1 (Reboot) vollständig zu Ende laufen lassen → Abschlussbericht **+ Analyse A** (oben).
2. `soak`-Feature + Soak-Treiber (Harness) + `tools/soak.py` implementieren (Kernel-Kern unverändert).
3. Soak #2 als **eine** lange Instanz fahren → Soak-Bericht.
4. Erst wenn **beide** Nachweise sauber sind → nächste größere Ausbaustufe.
