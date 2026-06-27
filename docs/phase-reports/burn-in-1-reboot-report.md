# Burn-in #1 — Reboot-/Power-Cycle-Burn-in: Abschlussbericht + Analyse

Status: **abgeschlossen** · Methode: Power-Cycle-Burn-in des **unveränderten** Release-Kernels
(ohne `kernel-fuzz`), `tools/burn-in.py`. Komplementärer Test #2 (Continuous-Soak): siehe
[burn-in-2-soak-plan.md](burn-in-2-soak-plan.md).

## 1. Eckdaten

- **Laufzeit:** 8,00 h (28 797 s QEMU-Wall-Clock) · **Iterationen (Reboots):** 4384
- **Ergebnis je Lauf:** CLEAN = **4379** · FAILURE = 0 · PANIC = 0 · HANG = **5** → **99,886 % sauber**
- **Boot-Zeit:** Ø 6,6 s · min 6,3 s · max 120,1 s (= die 5 Hang-Backstops). Kein Drift:
  erste 20 Läufe Ø 6,5 s ↔ letzte 20 Ø 6,4 s.

## 2. Aggregierte Aktivität (über alle Läufe)

| Größe | Wert |
|---|---|
| Gestartete Prozesse (Churn-Spawn/Destroy-Zyklen) | **8 758 000** |
| Transiente EL0-Threads (Reclaim) | 70 064 |
| Geladene externe Dienst-Instanzen | 56 927 |
| **Hot-Reloads** (reload/ckpt/rmig) | **13 137** |
| Verifizierte synchrone IPC-Runden (CALL/REPLY, untere Schranke) | 35 032 |
| **DMA** (Bus-Master in DmaCap-Region, virtio-rng) | 280 256 Bytes + je Lauf EL0-Round-Trip/SG/Multi-Region |
| **Loader-Aufrufe** (load/sysload/loadhw/loadstop + ext-27) | 56 927 |
| EL0-Faults abgefangen (erwartet, Intruder/Isolationstests) | 35 072 |

## 3. Konsistenz-/Balance-Bilanz

Über **alle 4379** sauberen Läufe **bit-identisch**:

- Freies RAM (memtest): **konstant 4077 MiB**, 1 Fragment ✓
- Region-/Churn-Balance (2000 Zyklen → Baseline): **konstant `true`** ✓
- `domain_audit`: **konstant 0** ✓ · el0iso-Faults: konstant 8 ✓ · ALL-PASS-Checks: konstant 55 ✓
- **Memory-Bilanz Start↔Ende:** jeder Lauf startet identisch und stellt nach dem auditierten
  Selbsttest (churn/loadstop/sasheap `Baseline: true`) die Ressourcen-Baseline wieder her.

Einzige Abweichung: die **5 Hang-Läufe** (ALL-PASS=3 statt 55, da `report()` nie lief).

## 4. Fehler / reproduzierbare Auffälligkeiten — **5 Hangs (0,114 %)**

| iter | offener Test | Quelle |
|---|---|---|
| 1026, 2048, 2835 | `churn=false` (3×) | SMP-Messrace |
| 1870, 3470 | `virtiorng=false` (2×) | Geräte-Poll-/DMA-Timing (TCG) |

**Verifizierte Diagnose (aus den Logs, kein Core-Dump nötig):**
1. In **jeder** Hang-Anomalie ist `dmagen=true`. `dmagen` ist auf `VRNG_DONE` gegatet → `virtiorng`
   (und das früher laufende `churn`) **lief vollständig durch**. Der Hang ist „Test fertig, aber
   `OK=false`" — **kein** stehengebliebener/deadlockter Test.
2. In jeder Anomalie wurde `DBG pending` gedruckt → core 0 lebte (Idle-Loop, Timer aktiv).
→ **Kein Kernel-Deadlock, kein Kernel-Kern-Bug.** Der Kernel blieb lebendig; die Balancen/Audits
hielten in allen 4379 sauberen Läufen.

**Eigentlicher Defekt (Primärbefund):** Der Selbsttest-Harness hat **keinen Completion-Timeout**.
`all_done()` verlangt von **jedem** Test `DONE && OK`. Wird ein synchroner Test selten `OK=false`,
wird `all_done()` **nie** true → der Kernel spinnt den Idle-Loop ewig → eine seltene Per-Test-
Flakiness wird zum **Hang** statt zu einem gemeldeten FAILURE + `system_off`.

**Zwei zugrundeliegende Flakiness-Quellen (Test-/Treiber-Ebene, kein Kernel-Kern):**
- **churn** (`threads/mod.rs`): prüft mit IRQs-aus auf core 0, ob **globale** Zähler (`total_free`/
  `free_vspaces`/`user_kstack_free_count`) nach 2000 Zyklen zur Baseline zurückkehren. IRQs-aus
  stoppt nur core 0 — die unmittelbar davor gespawnten `reclaim`-/`balance`-Threads auf den anderen
  Kernen perturbieren die globalen Zähler im Messfenster → Schein-Leak → `OK=false`.
- **virtiorng** (`system::virtio_rng_dma_demo`): bounded Geräte-Poll des virtio-rng-Modells; unter
  TCG-Timing-Jitter wird `used_adv`/eine Sensitivitäts-DMA selten nicht rechtzeitig fertig → `OK=false`.

## 5. Stabilitäts-Aussagekraft — was #1 belegt und was **nicht**

| Dimension | #1 | Begründung |
|---|---|---|
| **Kaltstart-/Boot-Robustheit** | **stark ✓** | 4384 identische Kaltstarts (Reset→MMU/Caches→SMP-Bring-up 8 Kerne→Init). |
| **Ressourcenbilanz über viele Reboots** | **per Lauf stark ✓ · kumulativ N/A** | Jeder Lauf stellt die Baseline her, bit-identisch über 4379 Läufe. Cross-Boot-Drift strukturell ausgeschlossen (RAM-Reset je Boot). |
| **Deterministische Teststabilität** | **stark ✓ (mit Caveat)** | Konstante Metriken über tausende Läufe — **aber** 0,114 % der Läufe zeigten flaky `OK=false` (churn/virtiorng). |
| **Langzeit-Speicherkonsistenz** | **nur ~6 s/Lauf · langfristig ✗** | Kein Leak über die kurze Selbsttest-Aktivität; **nicht** belegt über Stunden einer Instanz. |
| **Dauerbetrieb einer Instanz** | **ausdrücklich ✗** | Jede Instanz lebt ~6 s, dann SYSTEM_OFF. → Ziel #2. |
| **Zufällige Ereignisfolgen** | **✗** | Release-Selbsttest deterministisch; Randomisierung = Fuzzer (`kernel-fuzz`, ausgeschlossen). |
| **Hohe Parallelität** | **partiell ✓ (fix, beschränkt)** | 8 Kerne, Cross-Core-IPC, `caplk`, ext-27 `cross` — aber moderate, fixe Last, keine anhaltende Hochlast. |
| **Reale Hardware statt TCG** | **gar nicht ✗** | Alles QEMU-TCG (emulierte MMU/SMMU, starkes Speichermodell). Gilt für #1 **und** #2. |

## 6. Empfehlungen vor einer formalen Verifikation

1. **Harness-Watchdog** (Bugfix): `all_done()`-Deadline → offene Tests drucken, FAILURE markieren,
   `system_off`. Macht jede künftige Per-Test-Flakiness selbst-diagnostizierend statt zum Hang.
2. **churn SMP-robust** (Bugfix): erst nach `reclaim`+`balance` vollständig gereapt/quiesziert messen
   (stabile Doppelmessung), bzw. nur core-lokale Zähler.
3. **virtiorng robuster Poll** (Bugfix): ausreichender/adaptiver Geräte-Poll gegen TCG-Jitter.
4. **Continuous-Soak (#2)** durchführen — schließt Dauerbetrieb + Langzeit-Speicherkonsistenz.
5. **Reale-HW-Validierung** (z. B. STM32MP25) als eigener Schritt — Weak-Memory-Ordnung, Cache/TLB,
   SMMU-Durchsetzung sind unter TCG nicht prüfbar.

**Gesamturteil:** Der Release-Kernel ist bzgl. **Kaltstart-Robustheit** und **deterministischer
Per-Lauf-Korrektheit/Balance** sehr reif (99,886 % über 4384 Reboots, alle Audits/Balancen
bit-identisch, **kein Kernel-Kern-Defekt**). Die 5 Hangs sind **Test-/Harness-Defekte** (flaky
Messung + fehlender Watchdog), keine Kernel-Bugs. Offene Reife-Dimensionen (Dauerbetrieb,
Langzeit-Speicher, Zufallsfolgen, reale HW) adressieren #2 + ein HW-Schritt.
