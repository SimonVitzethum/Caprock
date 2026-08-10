# Ausbaustufe 6 — Per-Kern-parallele Scheduler-Instanzen + Cross-Core-IPI

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Löst die in [ext-3](ext-3-lazy-fp-locks.md) (Lock-Granularität, Teil 1) angekündigte
Folgearbeit: **echte per-Kern-parallele Einplanung**. Der eine globale Scheduler-Lock
ist weg; jeder Kern hat eine eigene Scheduler-Instanz hinter eigenem Lock und plant
**gleichzeitig** zu den anderen ein.

## 1. Per-Kern-Scheduler-Instanzen + TCB-Partitionierung ✅

- `caprock-sched::Scheduler` ist jetzt eine **einzelne Kern-Instanz**: eigene
  TCB-Partition, Run-Queues (eine je Priorität), `current`, Zombies. Der Kernel hält
  `SCHEDS: [SpinLock<Scheduler>; NUM_CORES]` — eine je Kern.
- **TCB-Partitionierung:** Der globale Slot-Raum (`NTHREADS = NUM_CORES*PER_CORE`,
  `PER_CORE=32`) ist statisch aufgeteilt — Kern `c` besitzt `[c*PER_CORE,
  (c+1)*PER_CORE)`. Eine `ThreadId` trägt den **globalen** Slot; `tid.core()` ist
  daraus ableitbar, **ohne** eine fremde Instanz zu sperren. Eine Instanz berührt
  ausschließlich ihre eigenen Threads (Rust-`&mut` erzwingt das).

## 2. Lock-Architektur: paralleler heißer Pfad, deadlockfrei ✅

- Der **Timer-/IPI-Reschedule-Pfad** sperrt nur `SCHEDS[core]` des eigenen Kerns →
  Kerne planen parallel ein, ohne sich zu blockieren (der frühere globale Lock
  serialisierte jeden Tick über alle Kerne).
- **Lock-Ordnung** `RES` → `SCHEDS[*]` → `FP_STATES`. Der Reschedule-Pfad nimmt nur
  `SCHEDS[core]` (+ atomares `FP_OWNER`), wartet also nie auf einen anderen Lock und
  kann an keinem Deadlock-Zyklus teilnehmen. IPC sperrt `RES` dann `SCHEDS[core]`
  (Endpoint-Teilnehmer sind kern-lokal). Kein Pfad nimmt zwei verschiedene
  `SCHEDS[*]` gleichzeitig.

## 3. Cross-Core-Aufwecken via IPI ✅

- GICv2-**SGI/IPI**: `hal::gic::send_sgi(target_core, intid)` schreibt `GICD_SGIR`;
  `IPI_RESCHED_INTID = 0`. `handle_irq` behandelt diesen IPI wie den Timer-Tick →
  Reschedule auf dem Zielkern.
- `system::wake_remote(tid)`: die **Ziel**instanz (`SCHEDS[tid.core()]`) sperren, den
  Thread bereit machen und — bei fremdem Kern — einen Reschedule-IPI schicken, damit
  der Zielkern ihn zeitnah einplant.
- `Scheduler::unblock` ist **idempotent** (wirkt nur auf wirklich blockierte Threads)
  → race-frei, falls ein Wake einen sich gerade blockierenden Thread trifft; der
  nächste Wake holt ihn dann. `wake_remote` wird bis zum Erfolg wiederholt.

## 4. `spawn_on_core` — Arbeit über Kerne verteilen ✅

`system::spawn_on_core(core, …)` plant einen Thread auf einem bestimmten Kern ein
(Bootkern kann auf noch nicht gestartete Kerne vorab einplanen; `bind_cores()` bindet
alle Instanzen vorab an ihre Kern-ID). Sekundärkerne picken die Threads beim Boot auf.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (17 Checks). Neuer `smp`-Check:

- Auf **jedem** Sekundärkern (1..7) läuft ein Worker und macht Fortschritt, während
  core 0 seine komplette Demo abarbeitet — `smp : core c Worker-Fortschritt=5/5` für
  alle c, Beleg paralleler, voneinander unabhängiger Einplanung.
- Ein Parker auf core 1 blockiert sich; core 0 weckt ihn **kern-übergreifend per IPI**
  (`wake_remote`) → `Cross-Core-IPI-Wake … erfolgreich=true`.

Alle 16 vorherigen Checks bleiben grün (IPC bleibt kern-lokal — Endpoint-Teilnehmer
auf demselben Kern; der Rendezvous-Fastpath `switch_to` ist intra-Kern).

## Offene Punkte

- **Kern-übergreifende synchrone IPC** (Call/Recv/Reply mit Partnern auf
  verschiedenen Kernen): Der Rendezvous-Fastpath ist intra-Kern; kern-übergreifend
  bräuchte es Nachrichtentransfer auf der Zielseite (Mailbox + IPI) statt
  `switch_to`. Heute sind Endpoint-Teilnehmer per Konvention ko-lokalisiert.
- **Lastausgleich/Migration:** Threads sind fest kern-gebunden (deterministisch);
  automatische Migration ist bewusst nicht umgesetzt.
- Unter single-threaded QEMU-TCG teilen sich alle emulierten Kerne eine Host-CPU; der
  Parallelitäts-**Gewinn** ist dort nicht als Wall-Clock messbar, die Architektur
  (per-Kern-Locks, paralleler Reschedule-Pfad) und Korrektheit sind es.
