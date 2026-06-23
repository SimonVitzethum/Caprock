# Ausbaustufe 1 — Scheduler- & Kontext-Härtung

**Datum:** 2026-06-23 · **Status:** FP + Prioritäten umgesetzt & verifiziert;
Per-Kern-Locks begründet zurückgestellt.

Über die geplante Roadmap (Phasen 0–7) hinaus. Drei Teile:

## 1. FP/SIMD-Kontextsicherung (umgesetzt) ✅

**Problem:** Bis Phase 7 war der Kontextwechsel *integer-only* — Threads durften
keinen FP/SIMD-Zustand über eine Preemption halten.

**Lösung:** Der `TrapFrame` (`sel4lake-hal::exception`) enthält jetzt den vollen
FP-Zustand (`q0..q31`, `FPSR`, `FPCR`); er wird in `__trap_dispatch` **eager**
gesichert/wiederhergestellt (Frame jetzt 800 statt 272 Byte). GP-`stp` erreicht
nur Offset ±504, daher wird die Adresse für `FPSR/FPCR` (@784) per `add`
gebildet.

**Test (`fp`):** zwei Threads summieren je 50 000×`1.5` und preempten sich
gegenseitig; beide liefern korrekt **75 000** → FP-Zustand bleibt erhalten.

*Lazy-FP* (Sichern erst bei Bedarf via CPACR-Trap) bleibt eine spätere
Optimierung; eager ist einfacher und sicherer.

## 2. Bitmap-Prioritäten (umgesetzt) ✅

**Lösung (`sel4lake-sched`, ADR 0005):** Pro Kern eine Ready-Queue je Priorität
(`NPRIO = 8`) + ein **L1-Bitmap** für O(1)-Auswahl der höchsten nichtleeren
Priorität (`leading_zeros`). Round-Robin innerhalb einer Priorität. `spawn`/
`init_core` nehmen eine Priorität; `block_current`/`unblock`/`on_tick`/`switch_to`
respektieren sie. Neuer Syscall **`PARK`** (Selbst-Block, kein Cap nötig), damit
fertige Threads die CPU dauerhaft an niedrigere Prioritäten abgeben.

**Test (`prio`):** drei Threads mit Prioritäten 4 > 3 > 2 führen Arbeit aus und
parken; der höher priorisierte wird zuerst fertig (Reihenfolge #0, #1, #2). Die
übrige Demo (Idle + Worker + IPC + Reload) läuft auf der Standardpriorität 1
(Round-Robin wie zuvor).

## 3. Per-Kern-Locks (begründet zurückgestellt)

**Entscheidung:** **nicht** umgesetzt — bewusst.

- Reine **Performance**-Optimierung, keine Korrektheit. Aktuell keine messbare
  Contention (µs-Kritische-Abschnitte, 100-Hz-Ticks).
- Konflikt mit dem in **Phase 6** bewusst gewählten **Einzel-`SYSTEM`-Lock**
  (sched + eps + cspace + pds): Der cap-gesicherte IPC-Dispatch braucht alle vier
  gemeinsam. Per-Kern-Scheduler-Locks führen wieder Lock-Ordering zwischen
  Scheduler- und IPC-/Cap-Lock ein (Cross-Core-Unblock, Deadlock-Risiko) — genau
  das, was das Einzel-Lock vermeidet.
- Gemäß Projektregel „Sicherheit/Korrektheit vor Mikrooptimierung" zurückgestellt.

**Sauberer Folgeschritt (wenn nötig):** Per-Kern-Scheduler-Zustand mit eigenem
Lock; Cross-Core-Operationen (`unblock` auf fremdem Kern) über IPI + Remote-Lock;
IPC/Cap behalten ihren eigenen Lock mit fester Ordnung (Sched-Lock vor IPC-Lock).

## Verifiziertes Gesamtergebnis

`./test-qemu.sh` → **ALL PASS**: MMU, memtest, captest, sched, **fp**, **prio**,
ipc, reload, 8/8 Kerne. Kernel-Crate weiterhin **0 `unsafe`-Blöcke**; neuer
`unsafe` nur im erweiterten FP-Save/Restore-Assembler (`sel4lake-hal`).
