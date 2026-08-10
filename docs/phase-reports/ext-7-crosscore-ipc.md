# Ausbaustufe 7 — Kern-übergreifende synchrone IPC

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Schließt die in [ext-6](ext-6-percore-scheduler.md) als offen genannte Lücke:
synchrones **Call/Recv/Reply über Kerngrenzen**. Ein Client auf einem Kern kann
jetzt einen Server auf einem **anderen** Kern aufrufen — Nachricht und Antwort
queren die Kerngrenze, der Partner wird per Reschedule-IPI geweckt.

## Problem

Bisher hielt der Syscall-Pfad `SCHEDS[core]` (die Scheduler-Instanz des eigenen
Kerns) über den gesamten Dispatch und konnte daher keinen IPC-Partner auf einer
**anderen** Instanz erreichen. Zwei Instanzen gleichzeitig zu sperren wäre zudem
deadlock-gefährdet (Kern A will B, Kern B will A).

## Lösung: `SchedOps`-Facade, je Operation genau ein Lock

- **`caprock-sched::SchedOps`** abstrahiert die Scheduler-Operationen, die der
  IPC-/Dispatch-Pfad braucht (`current_id`, `frame_of`, `block_current`,
  `switch_to`, `unblock`, `on_tick`, `exit_current`, `kill`). `frame_of`/`unblock`
  dürfen einen Thread auf **irgendeinem** Kern betreffen.
- **`ipc`/`microkit`** nehmen nun `&mut dyn SchedOps` statt `&mut Scheduler`. In
  `call` verzweigt der Code: liegt der Empfänger auf demselben Kern →
  Rendezvous-Fastpath (`switch_to`); auf einem anderen Kern → Nachricht in seinen
  (blockierten) Frame übertragen, ihn per `unblock` (+IPI) wecken, der Aufrufer
  blockiert auf seinem Kern.
- **Kernel-Facade `KernelSched`** (system.rs) implementiert `SchedOps` über das
  `SCHEDS`-Array: **jede** Operation sperrt **genau eine** Instanz und gibt sie
  sofort frei — nie zwei gleichzeitig; `unblock` auf einen Thread eines anderen
  Kerns schickt zusätzlich einen Reschedule-IPI.

### Warum deadlockfrei

`RES` (ein globaler Lock) serialisiert IPC → es ist immer nur **ein** Dispatch
gleichzeitig aktiv, und nur dieser nimmt `SCHEDS`-Locks zusätzlich zu `RES`. Der
Reschedule-Pfad (Timer/IPI) nimmt **nur** `SCHEDS[core]`, nie `RES`. Da der Dispatch
nie zwei `SCHEDS` gleichzeitig hält und der Reschedule nie auf einen weiteren Lock
wartet, gibt es keinen Wartezyklus. Im Trap sind IRQs maskiert → der Kern wird beim
Lock-Halten nicht preemptiert.

### Korrektheit des Cross-Core-Transfers

Der Partner ist beim Transfer **blockiert**; sein Frame (auf seinem Kernel-Stack im
SAS) ist eine stabile Speicheradresse. Kern A schreibt den Frame, dann `unblock`
(Lock auf `SCHEDS[B]` freigeben). Kern B übernimmt den Thread beim Reschedule (Lock
auf `SCHEDS[B]`) — die Lock-Freigabe/-Übernahme stellt die Sichtbarkeit der
Frame-Schreibzugriffe sicher (Acquire/Release).

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (18 Checks). Neuer `xipc`-Check: ein Client auf
core 0 ruft per `CALL` einen Server auf **core 2**, der mit `7*x` antwortet:
`core0->core2 call(3)->21, call(5)->35, call(7)->49`. CALL (core0→core2) und REPLY
(core2→core0) queren je die Kerngrenze und wecken den Partner per IPI. 6/6
back-to-back stabil. Alle bisherigen 17 Checks bleiben grün (Intra-Kern-IPC nutzt
weiterhin den `switch_to`-Fastpath).

## Offene Punkte

- Endpoint-/Notification-Tabellen liegen unter dem **einen** `RES`-Lock; IPC ist
  damit global serialisiert. Feinere Granularität (per-Endpoint-Locks) wäre eine
  Skalierungs-Optimierung.
- Thread-Migration/Lastausgleich weiterhin bewusst nicht umgesetzt (Threads fest
  kern-gebunden, deterministisch).
