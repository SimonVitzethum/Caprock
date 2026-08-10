# Ausbaustufe 4 — Lazy-FP (Befund) + Lock-Granularität

**Datum:** 2026-06-24 · **Status:** Lazy-FP als unvereinbar erkannt + zurückgerollt;
Lock-Trennung (Scheduler vs. Ressourcen) umgesetzt & verifiziert.

## 1. Lazy-FP — versucht, als architektonisch unvereinbar verworfen

**Idee:** FP/SIMD-Zustand nur bei Bedarf sichern (Trap-on-Use via `CPACR_EL1.FPEN`):
beim Threadwechsel FP trappen; bei der ersten FP-Nutzung den FP-Kontext lazy
umladen. Implementiert wurde der vollständige Mechanismus (CPACR-Steuerung,
per-TCB-FP-Bereich, per-Kern-`fp_owner`, FP-Trap-Hook, `save_fp`/`restore_fp`).

**Befund (in QEMU):** **Hang.** Ursache ist grundlegend: In Caprock laufen alle
Threads **auf EL1** (noch kein EL0-Userland). `CPACR_EL1.FPEN` trappt FP bei EL1
aber **auch für den Kernel selbst** — und rustc/LLVM emittieren NEON im
Kernel-Code (Exception-Handler, Hooks). Sobald FP getrappt war, löste der erste
NEON-Befehl *innerhalb* eines Handlers einen verschachtelten FP-Trap aus →
Endlosschleife/Hang.

**Schlussfolgerung:** Lazy-FP setzt eine **EL0/EL1-Trennung** voraus (EL0-FP
trappen, Kernel läuft auf EL1 mit FP dauerhaft an). Bis es echtes EL0-Userland
gibt, ist **eager FP** (vollständiges q0..q31-Save/Restore im Trap-Pfad,
Ausbaustufe 1) das korrekte Verfahren. Lazy-FP wurde sauber zurückgerollt; der
FP-Korrektheitstest (zwei Threads → 75 000) besteht weiterhin (eager).

→ Lazy-FP ist damit an die spätere **EL0-Userland-Phase** geknüpft (ADR 0002).

> **Nachtrag (2026-06-24):** Mit der EL0/EL1-Trennung (ext-4) ist Lazy-FP nun
> umgesetzt — `FPEN=0b01` (nur EL0 trappt) + soft-float Microkernel. Siehe
> [ext-5](ext-5-lazy-fp.md). Der eager-FP-Pfad ist entfernt; der `fp`-Check ist
> jetzt ein echter EL0-Lazy-FP-Test.

## 2. Lock-Granularität — Scheduler-Lock vom Ressourcen-Lock getrennt

**Umgesetzt (`kernel/src/system.rs`):** Der bisher einzelne `SYSTEM`-Lock wurde in
zwei Locks aufgeteilt:

- **`SCHED`** — der Scheduler (heißer Pfad).
- **`RES`** — Allokator, Capability-Space, Endpoints, Notifications,
  Protection Domains.

Der **Timer-Reschedule-Pfad** (jeder Tick, jeder Kern) sperrt **nur `SCHED`** und
blockiert damit keine reinen Ressourcen-Operationen (Cap-/Speicher-/PD-Verwaltung)
mehr. Operationen, die beides brauchen (IPC-Dispatch, `spawn`, `reap`), sperren in
**fester Reihenfolge `RES` vor `SCHED`**; kein Pfad sperrt `SCHED` vor `RES` →
**deadlockfrei**. Verifiziert: `./test-qemu.sh` weiterhin **ALL PASS** (14 Checks).

**Noch offen — echte per-Kern-*parallele* Scheduler:** Mehrere Kerne können den
Scheduler weiterhin nicht *gleichzeitig* bearbeiten (ein globaler `SCHED`-Lock).
Dafür bräuchte es **per-Kern-Scheduler-Instanzen** (eigene TCB-Partition je Kern)
und für Cross-Core-Operationen (Unblock/Kill eines Threads auf einem anderen Kern)
einen **IPI-Pfad**. Da Threads aktuell **feste Same-Core-Affinität** haben und IPC
nur same-core stattfindet, ist der globale `SCHED`-Lock noch kein Engpass; die
Aufteilung in per-Kern-Instanzen ist als gezielter Folge-Refactor vorgemerkt
(Korrektheit vor Mikrooptimierung, ADR 0005).

## Ergebnis

`./test-qemu.sh` → **ALL PASS** (MMU, dtb, memtest, captest, sched, fp, prio,
life, notif, xfer, ipc, reload, ckpt, 8 Kerne). Eager FP bleibt aktiv; Scheduler-
und Ressourcen-Lock sind getrennt. Kein neuer `unsafe`-Code im Kernel-Crate.
