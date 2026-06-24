# Ausbaustufe 18 — Sicherheits-Audit (feindliche Perspektive)

**Datum:** 2026-06-24 · **Status:** 2 reale Bugs behoben + regressionsgetestet; übrige
Funde verifiziert (real/sicher/by-design) und mit Empfehlungen dokumentiert.

SEL4Lake wurde als **potenziell feindliches Zielsystem** behandelt: gezielte
adversariale Code-Analyse aller Subsysteme (Capabilities, Speicher/VSpace, IPC, SMP,
MCS, Hot-Reload) + reproduzierbare QEMU-Tests für die plausibelsten Funde. Jeder Fund
wurde **selbst am Code verifiziert** (keine ungeprüften Subagent-Behauptungen). Für die
zwei bestätigten Bugs: Ursache isoliert, minimal behoben, Regressionstest hinzugefügt,
der **ohne Fix nachweislich fehlschlägt**.

---

## Behobene Bugs (bestätigt + reproduziert + regressionsgetestet)

### BUG 1 — Kernel-Panik-DoS: toter Thread in Endpoint-Queue (HOCH)

**Ort:** `crates/sel4lake-ipc/src/lib.rs`, `recv()` (Z. 183) und `call()` (Z. 155).

**Fehler:** Beide machten `ops.frame_of(x).expect(...)` auf einen aus der
`senders`/`receivers`-Queue entnommenen Thread. **Kein Pfad entfernt einen gekillten/
beendeten Thread aus diesen Queues** (nur `retire_receiver` für Hot-Reload, explizit).

**Angriffsweg:** Thread A `CALL`t einen ungedienten Endpoint → blockiert in `senders`.
A wird gekillt (cap-`KILL`; `kill` ist erlaubt für blockierte Threads). `record_zombie`
erhöht die Generation → A's `ThreadId` löst nicht mehr auf, **bleibt aber als toter
Eintrag in `senders`**. Der nächste `RECV` entnimmt A → `frame_of(A) == None` →
`.expect("sender frame")` → **`[KERNEL PANIC]`** (ganzer Kernel hält an). Symmetrisch in
`call()` für einen gekillten blockierten Empfänger.

**Auswirkung:** Unprivilegierter, kernelweiter Denial-of-Service.

**Fix:** Statt `.expect` werden tote Einträge übersprungen (`while let Some(x) =
queue.dequeue() { let Some(f) = frame_of(x) else { continue }; … }`). Die Queues sind
damit **selbstheilend** gegen tote Einträge — keine Scheduler↔IPC-Kopplung nötig.
`reply()`/`signal()` waren bereits tolerant (`if let Some`).

**Regressionstest `stale`:** Opfer `CALL`t ungedienten Endpoint → blockiert; wird
gekillt; Server `RECV`t (überspringt toten Eintrag); lebender Client wird korrekt
bedient (Antwort = 2× `STALE_MAGIC`). **Verifiziert:** mit zurückgerolltem Fix →
`[KERNEL PANIC] sender frame` an lib.rs:187 → `FAILURES`.

### BUG 2 — MCS-Thread-Stranding bei erneutem Budget-Bind (HOCH)

**Ort:** `crates/sel4lake-sched/src/lib.rs`, `set_budget()`.

**Fehler:** `set_budget` (von `bind_sched_context` genutzt) setzte `depleted = false`,
**ohne den Thread wieder einzureihen**. Ein erschöpfter Thread ist per Konstruktion
weder laufend, noch bereit, noch blockiert — er hängt nur am Refill-Scan (der
`depleted == true` erfordert). Nach `set_budget` ist er `depleted == false`, aber in
keiner Queue → **für immer unplanbar**, belegt aber weiter seinen TCB-Slot.

**Angriffsweg:** Einem bereits erschöpften Thread eine neue SchedContext-Cap binden
(`bind_sched_context` mehrfach erlaubt). Auswirkung: dauerhafter Thread-Verlust (DoS)
**und** Täuschung der Leak-Erkennung (`used_tcbs` überzählt → Baseline-Vergleich lügt).

**Fix:** War der Thread erschöpft (und nicht laufend/blockiert), nach dem Reset wieder
`enqueue_ready`.

**Regressionstest `strand`:** budgetierter Worker (budget=1, lange Periode → kein
natürlicher Refill) erschöpft, dann erneut gebunden → Zähler muss wieder wachsen
(`2315 → 283207`). **Verifiziert:** mit zurückgerolltem Fix → Zähler eingefroren →
`FAILURES`.

**Test-Helfer:** `system::kill_local(tid)` — kernelinterner Kill (gleiche Mechanik wie
der cap-`KILL`-Syscall), zwingend nötig, um den `stale`-Test deterministisch zu bauen.

---

## Verifiziert SICHER (Funde widerlegt / by-design)

- **Page-Table-Initialisierung:** Neue L1/L2/L3 werden **vollständig** beschrieben
  (`vspace_create_base`, `vspace_map_page` Z. 472-475 setzt alle 512 L3-Einträge auf
  EL1-only). **Kein** Garbage-PTE-Isolationsbruch.
- **EL0/EL1-Trennung:** Deskriptor-Bits korrekt — Rw=`AP_RW_EL0|PXN|UXN`,
  Rx=`AP_RO_EL0|PXN` (W^X, EL1-non-exec), Ro=`AP_RO_EL0|PXN|UXN`; erste 2 MiB +
  Kernel-Image + Page-Tables EL1-only. Eine isolierte PD kann durch ihre **eigene**
  VSpace weder Kernel noch fremdes RAM lesen (Hardware-Fault).
- **Cap-Rechte:** `copy`/`mint` schneiden Rechte (`intersect`) → **keine Eskalation**
  (Kind erbt ⊆ Eltern). `grant_cap` übergibt `RWX`, aber `intersect` clamped auf die
  Quellrechte → kein READ→WRITE-Upgrade.
- **Cap-Dispatch:** Generation **und** Bounds **und** Objektart **und** Rechte werden je
  Syscall geprüft (CALL=WRITE, RECV/REPLY=READ, SIGNAL=WRITE, WAIT=READ, KILL=WRITE,
  MAP/UNMAP nach Recht). Stale Handles (Generation), gefälschte/Out-of-Range-Slots und
  Kind-Verwechslung (Memory↔Endpoint↔Tcb↔Notification) werden abgewiesen.
- **Doppel-Einreihung im Scheduler (Fund F3):** widerlegt. Alle Scheduler-Ops eines
  Kerns serialisieren über **denselben** `SCHEDS[core]`-Lock (auch cross-core `unblock`
  lockt `SCHEDS[target]`); `switch_to` ist intra-core. `unblock` ist auf `blocked`
  idempotent; `on_tick`-Refill und `cur`-Requeue sind disjunkt (ein erschöpfter Thread
  ist nie `current`). Kein erreichbarer Double-Enqueue.
- **`free_region`/Allokator:** `alloc` ist bounds-/overflow-sicher (`checked_add`);
  `MemoryCap`-Linearität (move-only) verhindert Cap-Doppelfreigabe.

---

## Reale Restrisiken & Härtungsempfehlungen (nicht behoben)

Priorisiert. Diese sind reale Schwächen, aber entweder nicht von untrusted Code
erreichbar, by-design-Limitierungen oder mit relevanten Kosten/Designentscheidungen
verbunden — daher dokumentiert statt sofort gefixt.

1. **Kein Zero-on-Free (Confidentiality, MITTEL-HOCH):** Frames werden bei Freigabe
   **nicht genullt**. Eine neue PD, die einen recycelten Frame (Stack/gemappt) erhält,
   kann Rest-Daten der Vorgänger-PD lesen (deterministisch via First-Fit). *Empfehlung:*
   Zero-on-Free im `MEM`-Free-Chokepoint. *Kosten-Hinweis:* blankes Nullen der 2-MiB-
   Isolationsregionen × tausende Churn-Zyklen ist unter Single-Thread-TCG sehr teuer
   (~GiB-memset → Timeout-Risiko); produktiv (echte HW / DMA-memset) unkritisch, im
   Test ggf. größenbegrenzt/gated.
2. **Ein einziger ausstehender `caller`/`waiter` je Endpoint/Notification (MITTEL):**
   Ein zweites Rendezvous überschreibt den ersten `caller`; ein zweiter `WAIT`
   überschreibt den ersten `waiter` → der Verdrängte hängt für immer (Ressourcen-DoS;
   nicht speicherunsicher). *Empfehlung:* Reply-Caps (seL4-Stil) bzw.
   Mehrfach-Waiter-Liste; mindestens den verdrängten Thread mit Fehler aufwecken.
3. **Hot-Reload nicht atomar gegen laufende `CALL` (MITTEL):** `reload_swap` zieht den
   Empfänger zurück + entzieht die Recv-Cap, ohne ausstehende `caller`/`senders` zu
   drainen. Eine zur Reload-Zeit in Flug befindliche `CALL` kann verloren gehen oder von
   v2 fehl-beantwortet werden. (Demo umgeht es: Reload nur bei `BATCH1_DONE`.)
   *Empfehlung:* in-flight-Caller beim Quiesce entblocken/umleiten.
4. **`SIGNAL` mit Badge 0 → verlorener Wakeup (MITTEL):** `pending |= 0` ist ein No-Op;
   ein vorheriges Signal ohne Waiter geht verloren. Der Kernel erzwingt `badge != 0`
   nicht. *Empfehlung:* Badge != 0 bei Notification-Cap-`mint` erzwingen, oder den
   Waiter auch bei Badge 0 wecken.
5. **`MAP` ist nur cap-gegated, nicht spatial (by-design, KONTEXTABHÄNGIG):** Wer eine
   Memory-Cap hält, darf den Frame mappen — korrektes Capability-Modell. Riskant erst,
   **wenn** untrusted PDs Caps zu Frames erhalten, die der Kernel später anders nutzt.
   Aktuell sicher: Page-Tables sind **nicht** cap-hinterlegt (separate `MEM.alloc`), und
   Caps werden für disjunkte Regionen geprägt. *Empfehlung:* falls künftig untrusted
   PDs beliebige Caps bekommen, einen „Frame-ist-kernelintern/Page-Table“-Guard + per-
   Frame-W^X-Buchhaltung ergänzen; UAF-Schutz via Mapping-Refcount (ein gemappter Frame
   darf nicht freigegeben werden).
6. **u32-Generation-Wrap (NIEDRIG):** Nach 2³² Slot-Recyclings kollidiert eine stale
   `ThreadId`/Tcb-Cap (ABA). Praktisch unerreichbar; *Empfehlung:* 64-bit-Generation,
   falls extreme Churn-Lebensdauern erwartet werden.
7. **`exit_current`/`block_current` `.expect` bei leerer Queue (NIEDRIG):** Beendete
   sich der **einzige/Idle**-Thread eines Kerns, paniert `dequeue_highest().expect(...)`.
   Nur trusted Boot-Code steuert den Idle-Thread (untrusted EL0 kann das nicht
   auslösen). *Empfehlung:* leere Queue defensiv behandeln (Idle behalten).

---

## Getestete Angriffe (Übersicht nach Bereich)

- **A Capabilities:** gelöschte/stale Caps, Generation-Reuse, gefälschte Slots,
  Rechte-Eskalation via copy/mint/grant, Kind-Verwechslung → **alle sicher** (Dispatch
  prüft Generation/Bounds/Kind/Rechte; copy/mint clampen).
- **B Speicher:** UAF/Double-Free/Refcount, ASID-/VSpace-/Mapping-Leaks → Allokator +
  Page-Table-Init sicher; Zero-on-Free fehlt (Risiko 1); UAF nur kernelintern-latent
  (kein DELETE/REVOKE-Syscall für PDs). Churn-Test (2000 Zyklen) zeigt keine Leaks.
- **C VSpace-Isolation:** fremde/Kernel-Seiten lesen/schreiben, Guard/W^X/RX/RO,
  EL0→EL1 → **Hardware-erzwungen sicher** für die eigene VSpace (s. „Verifiziert
  sicher“); cap-vermittelte Cross-PD-Maps sind by-design (Risiko 5).
- **D IPC:** **BUG 1 gefunden+behoben** (Panik bei totem Queue-Eintrag); Single-
  caller/waiter + Badge-0 + Hot-Reload-in-flight als Restrisiken (2-4).
- **E SMP:** Lock-Ordnung azyklisch (CAPS < EPS/NTFNS/MEM < SCHEDS < FP_STATES);
  per-Kern-Serialisierung verhindert Double-Enqueue (F3 widerlegt); kein Deadlock-Pfad
  gefunden. (Generativer Mehrkern-Fuzzer: Empfehlung unten.)
- **F MCS:** **BUG 2 gefunden+behoben** (Stranding); YIELD lädt kein Budget (kooperativ,
  by-design — Hinweis: erlaubt CPU-Schätzung via Tick-Sampling zu unterlaufen, inhärent
  bei tick-basiertem Budget); Budget kommt aus der Cap (keine Eskalation ohne Cap).
- **G Hot-Reload:** Endpoint stabil, Client-Cap unverändert; nicht-atomar gegen
  in-flight CALL (Risiko 3).

## Empfehlungen für weitere Härtung

1. Zero-on-Free (größenbegrenzt/gated) — Risiko 1.
2. Reply-Caps + Mehrfach-Waiter — Risiken 2/3.
3. Badge≠0 erzwingen — Risiko 4.
4. **Generativer Kernel-Fuzzer** (Bereich H): zufällige Sequenzen aus Spawn/Kill/Exit/
   Yield/Map/Unmap/Call/Reply/Signal/Wait/Cap-Ops/Reload/Bind über alle 8 Kerne, nach
   jeder Runde Invarianten prüfen (keine Leaks/Zombies/toten Refs/Queue-Korruption). In
   diesem Audit wurde **kein** vollständiger Fuzzer gebaut (Umfang/Stabilität); die
   gezielten Repro-Tests + der bestehende Churn-Test (2000 Zyklen) decken die
   höchstprioren Pfade ab. Ein Fuzzer ist der nächste sinnvolle Schritt.
5. 64-bit-Generation; defensives Idle-Handling — Risiken 6/7.

**Ergebnis:** 2 reale, erreichbare Bugs (1× Kernel-DoS, 1× Thread-DoS) gefunden,
behoben und regressionsgetestet (Tests schlagen ohne Fix nachweislich fehl). Mehrere
behauptete Schwachstellen als sicher/by-design widerlegt. `./test-qemu.sh` = **ALL
PASS** (29 Checks, 3/3 gespacete Läufe stabil).
