# Ausbaustufe 19 — Generativer Kernel-Fuzzer (Audit Bereich H)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert (Oracle-sensitiv).

Setzt **Bereich H** des Sicherheits-Audits ([ext-18](ext-18-security-audit.md)) um: ein
**deterministischer, generativer In-Kernel-Fuzzer**, der zufällige Operationssequenzen
über alle ressourcen-tragenden Subsysteme fährt und nach jeder Epoche **Invarianten**
prüft. Fester Seed → jeder Fehlschlag ist reproduzierbar.

## Aufbau

Ein dedizierter Treiber-Thread (`fuzz_driver`, EL1, core 0, prio 2 → dominiert core 0
bis fertig, parkt dann). Deterministischer PRNG (xorshift64, fester `FUZZ_SEED`). Alle
Operationen sind **nicht-blockierend** (kernelinterne `system::`-Aufrufe), daher kein
Sequenzierungsproblem mit blockierender IPC. Gegate auf MCS/stale/strand fertig.

### Phase 1 — Epochen mit Baseline-Oracle (geprüfte Ops)

Verfolgte Objekt-Pools (Caps, Threads, Mappings). Pro Epoche **50 zufällige Ops** aus:

- **Cap-CDT:** Memory-Cap installieren, `copy`/`mint` (zufällige Rechte, Kind ⊆ Eltern),
  `move`, `delete` (blatt-only), `revoke` (Nachfahren) — baut zufällige Derivation-Trees.
- **Speicher/VSpace:** Frame in eine isolierte VSpace `map` (zufällige RW/RO/RX-Rechte),
  `unmap` (per-Seite-Pfad).
- **Threads:** plain- und isolierten-EL0-Thread `spawn`, `kill`/`destroy_isolated`.
- **MCS:** SchedContext-Cap prägen + an einen Thread `bind`.

Nach den Ops: **vollständiger Teardown** (erst Threads → VSpace-Teardown entfernt
Mappings, dann Caps revoken+löschen → Frames frei; Reihenfolge verhindert dangling
Mappings). Dann das **ORACLE**: alle sechs Ressourcenstände müssen **exakt zur
Baseline** zurückkehren:

1. `MEM.total_free()` · 2. belegte TCBs (core 0) · 3. freie VSpaces/ASIDs ·
4. freie kstack-Pool-Slots · 5. belegte **Cap-Slots** · 6. belegte **Cap-Objekte**.

Jeder Bruch (Leak, Zombie, dangling Ref, **CDT-/Refcount-Verletzung** = Objekt ohne
lebende Cap) liefert einen Bruch-Code 1..6 und stoppt den Fuzzer mit `FAILURES`.
**24 Epochen × 50 = 1200 oracle-geprüfte Ops.**

### Phase 2 — SMP-Kontention (alle 8 Kerne)

Noise-Worker auf cores 1..7 + Treiber-Churn auf core 0 fahren **balancierte**
Cap/MEM-Iterationen (Frame→Cap→Copy→Revoke→Delete, netto 0) **gleichzeitig** → echte
Mehrkern-Last auf den globalen `CAPS`-/`MEM`-Locks + interleavte CDT-Mutationen.
Baseline **nach** dem Worker-Spawn (fester Footns), `GO`, Treiber-Churn, `STOP`, auf
Quiesce aller Worker warten (ACK), dann Baseline-Check (alle Churn balanciert →
zurück zur Baseline). Pro Lauf **~9 000–29 000 Churn-Iterationen** (≈ 4× so viele
Cap-Ops), timing-abhängig.

## Verifikation

`./test-qemu.sh` → **ALL PASS** (30 Checks, neuer `fuzz`-Check), 3/3 gespacete Läufe
stabil:

```
fuzz : Phase1 24/24 Epochen, 1200 gepruefte Ops, Oracle-Bruch-Code=0 (0=keiner);
       Phase2 ~9k-29k SMP-Churn-Iter (8 Kerne) ok=true; gesamt ~10k-30k Ops
fuzz : ALL PASS
```

Die **schwankende SMP-Iterationszahl** (8 535 / 11 220 / 18 455 / 29 314 über Läufe)
belegt **echte timing-abhängige Nebenläufigkeit** — und das Oracle besteht trotz
nichtdeterministischer Interleavings jedes Mal (Baseline exakt wiederhergestellt).

### Oracle-Sensitivität (Negativtest)

Bewiesen, dass das Oracle Lecks wirklich fängt: mit absichtlich ausgelassener
Cap-Löschung im Teardown (gepflanzter Cap-/Frame-Leak) meldet der Fuzzer
**`FAILURES`** statt `ALL PASS`. Danach zurückgerollt.

## Abdeckung & Grenzen

- **Abgedeckt:** Cap-CDT (copy/mint/move/delete/revoke), Frame-Map/Unmap + L3-Lebens-
  zyklus, plain/isolierter Thread-Spawn/Kill/Destroy + VSpace/ASID/kstack-Teardown,
  SchedContext-Bind — leckfrei über zufällige Op-Ordnungen; SMP-Lock-Kontention.
- **Bewusst ausgelassen:** blockierende IPC-Rendezvous (`CALL`/`RECV`/`WAIT`) lassen
  sich von einem Einzeltreiber nicht ohne Gegenpart fahren — dafür die gezielten
  Tests `ipc`/`xipc`/`shm`/`stale`; Endpoint/Notification-Objekte haben keinen
  Destroy-Pfad (s. ext-16) und sind daher nicht Teil des balancierten Fuzzings.

**Test-Support (minimal, kein Feature):** `CapSpace::used_slots/used_objects` +
`system::cap_used_slots/cap_used_objects` (Oracle), `system::unmap_into_thread`
(per-Seite-Unmap-Pfad). Alle bestehenden Tests laufen unverändert (29 → 30 Checks).
