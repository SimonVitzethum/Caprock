# Ausbaustufe 17 — MCS Scheduling Contexts (Härtung Ziel 3)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Führt **budget-basiertes Scheduling** nach dem Vorbild von seL4-MCS ein: CPU-Zeit wird
nicht mehr nur über Prioritäten/Round-Robin, sondern über **Scheduling Contexts**
(Budget je Periode) zugeteilt — und die Autorität dafür wird **ausschließlich über eine
Capability** vergeben.

## Scheduling-Context-Objekt (Scheduler)

Jeder `Tcb` (in `caprock-sched`) trägt MCS-Felder: `budget` (Ticks je Periode, `0` =
unbeschränkt/Round-Robin), `period`, `remaining`, `next_refill`, `depleted`. Der
Scheduler-`now`-Zähler taktet die Perioden; `depletions`/`refills` sind Telemetrie.

`Scheduler::on_tick(core, frame, tick: bool)` wurde MCS-fähig:

- **`tick = true`** (echter Timer-Tick, aus dem Reschedule-Hook): zuerst **Refill-Scan**
  — erschöpfte MCS-Threads, deren Periode abgelaufen ist (`now >= next_refill`), werden
  auf `budget` aufgefüllt, `depleted = false` und wieder bereit gemacht (`refills += 1`).
  Dann **Budgetverbrauch**: der laufende Thread (sofern `budget > 0`) verliert eine
  Zeitscheibe (`remaining -= 1`); bei `remaining == 0` wird er **erschöpft** markiert
  (`depleted = true`, `next_refill = now + period`, `depletions += 1`) und **nicht** mehr
  eingeplant — er blockiert auf sein Budget, bis der Refill ihn zurückbringt.
- **`tick = false`** (freiwilliges `YIELD`): verbraucht **kein** Budget — nur eine
  reine Neuplanung.

`set_budget(tid, budget, period)` konfiguriert die Felder; `budget_stats()` liefert
`(depletions, refills)`.

## Scheduling-Context-Capability (Autorität nur über Caps)

Neue Objektart `ObjectKind::SchedContext { budget, period }` im `caprock-cap`-CapSpace,
eingebracht über `CapSpace::install_sched_context(budget, period, rights)`. Die Cap
**ist** die Autorität, einem Thread Budget zuzuweisen — analog zu seL4
`SchedContext_Bind`:

- `system::install_sched_context_cap(budget, period, rights)` prägt die Cap.
- `system::bind_sched_context(sc, core, tid)` löst die Cap auf, prüft, dass es ein
  `SchedContext` mit `WRITE`-Recht ist, **liest Budget/Periode aus dem Cap-Objekt**
  (nicht aus einem freien Argument) und setzt sie über den Scheduler von `core`. Ohne
  gültige Cap → `false` (keine Budget-Autorität). Lock-Ordnung: `CAPS` vor `SCHEDS`.

Damit ist die Erweiterung vollständig in die bestehende Capability-Architektur
integriert: keine neue Autorität ohne Capability.

## Demo & Verifikation

Auf einem eigenen Kern (`MCS_CORE = 5`, erst aktiv, sobald Churn-Test und alle
Sekundärkern-Worker fertig sind) konkurrieren zwei **gleichprioritäre** EL1-Threads:

- **Budgetiert**: `budget = 2` Ticks je `period = 16` Ticks, per SchedContext-Cap
  gebunden.
- **Greedy**: unbeschränkt (`budget = 0`).

Beide zählen identische Arbeits-Chunks. Reiner CPU-Spin (kein `YIELD`) → nur der
Timer-Tick preemptet und belastet das Budget. Nach genügend Ticks + mehreren Zyklen
werden beide gestoppt (parken, keine Zombies) und ausgewertet.

`./test-qemu.sh` → **ALL PASS** (27 Checks, neuer `mcs`-Check), 3/3 gespacete Läufe
stabil:

```
mcs : Budget per Cap gebunden=true (budget=2/Periode=16); Erschoepfungen=8, Refills=7
mcs : Fortschritt budgetiert=15196 vs. greedy=128464 (budgetiert gedrosselt + garantiert > 0)
mcs : ALL PASS
```

Belegt alle geforderten Eigenschaften:

- **Budget wird pro Tick verbraucht** und der Thread **bei Budgetende gestoppt**
  (`Erschoepfungen > 0`, budgetierter Fortschritt stark begrenzt).
- **Budget wird nach der Periode aufgefüllt** (`Refills > 0`).
- **Garantierte CPU-Zeit**: der budgetierte Thread macht trotz CPU-Hog jede Periode
  garantierten Fortschritt (`budgetiert > 0`), liegt aber ~8× unter dem Greedy-Thread
  (`budgetiert * 3 < greedy`) → das Budget begrenzt **und** garantiert die Zuteilung.
- **Cap-vergeben**: das Budget wurde nur durch Vorlage der SchedContext-Cap wirksam
  (`gebunden = true`).

## Constraint-Konformität

- Neue Autorität (CPU-Budget) **ausschließlich über Capabilities** vergeben
  (`SchedContext`-Cap + `bind_sched_context`).
- **Alle bestehenden Tests laufen weiterhin vollständig** (26 → 27 Checks, ALL PASS).
- `unsafe` nicht neu eingeführt; nur bestehende erlaubte Low-Level-Pfade.

## Optional / offen

Budget-**Donation über IPC** (Helping/Inheritance bei `CALL`) und ein vollständiger
**Sporadic Server** (mehrere Refill-Slots statt eines einzigen) sind die nächsten
optionalen MCS-Schritte; der hier umgesetzte Kern (periodisches Budget + Erschöpfung +
Refill, cap-kontrolliert) ist die Grundlage dafür.
