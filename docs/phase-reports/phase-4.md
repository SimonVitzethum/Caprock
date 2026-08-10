# Phasenbericht 4 — Threads + Scheduler

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Präemptives Multithreading: ein deterministischer Per-Kern-Scheduler, getrieben
vom Timer-Tick, mit echtem Kontextwechsel.

### Kontextwechsel über den Trap-Pfad (`caprock-hal`)

- `handle_exception` gibt jetzt den **wiederherzustellenden** `TrapFrame` zurück;
  der Assembler-Epilog (`__trap_dispatch`) setzt `sp` darauf, bevor er Register
  restauriert und `eret` ausführt. Ein Kontextwechsel ist damit ein reiner
  **SP-Tausch** — die ohnehin vorhandene Trap-Save/Restore-Maschinerie sichert
  und restauriert den vollen Registerkontext.
- `init_thread_frame(stack_top, entry, arg)` legt einen initialen TrapFrame am
  Stack-Top an (ELR=entry, x0=arg, SPSR=EL1h mit IRQs frei) → der erste Switch
  „erettet" sauber in den neuen Thread.
- **Reschedule-Hook:** `set_reschedule_hook` registriert eine Funktion, die der
  Timer-IRQ-Pfad aufruft (entkoppelt die HAL vom Scheduler; saubere Schichtung).
- `gic::handle_irq` liefert die behandelte INTID zurück (Timer-Tick erkennen).

### Scheduler (`crates/caprock-sched`)

- TCB-Tabelle (speichert nur den gesicherten SP) + Per-Kern-Run-Queues
  (FIFO-Ring). Feste Kapazität → allokationsfrei, deterministisch.
- `init_core` (Boot-Kontext wird Idle-Thread), `spawn`, `on_tick`
  (Round-Robin: aktuellen sichern, nächsten wählen, dessen Frame zurückgeben).
- **0 `unsafe`** (der einzige unsafe-Anteil, das Frame-Setup, steckt in der HAL).

### Kernel (`kernel/src/threads.rs`)

- Globaler `SpinLock<Scheduler>`; Reschedule-Hook ruft `on_tick(core, frame)`.
- Thread-Stacks werden aus dem **capability-basierten Allokator** (Phase 2)
  bezogen — die Speicher- und Scheduling-Subsysteme greifen ineinander.
- Demo: 3 Worker-Threads auf core 0 + der Idle-Thread (Boot-Kontext) werden
  preemptiv zeitgescheibt. Jeder Kern registriert seinen Idle-Thread.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**:

```
sched   : core 0..7 ticks=3..4
sched   : worker 0 count=284 (preemptiv neben Idle gelaufen)
sched   : worker 1 count=282 ...
sched   : worker 2 count=261 ...
sched   : ALL PASS
== checks == MMU · memtest · captest · sched · 8 Kerne online: ALL PASS
```

Beweis der Preemption: 3 Worker **und** der Idle-Thread liefen alle auf core 0;
die Worker zählten je ~260–290× hoch, während der Idle-Thread zum Melden kam —
ohne Preemption liefe nur einer von ihnen. Alle 8 Kerne ticken (eigener
Scheduler je Kern). Build ohne Warnungen.

## Getroffene Entscheidungen

- **SP-Tausch im Trap-Pfad** statt separater `switch`-Routine: wiederverwendet die
  Trap-Save/Restore-Logik, einheitlich für Preemption (Timer-IRQ) und künftig
  freiwilliges Yield (SVC). Minimaler neuer Code.
- **Boot-Kontext = Idle-Thread:** kein Spezialfall für den ersten Switch — der
  erste Tick sichert den Boot-Kontext als Idle und wechselt zum ersten Worker.
- **Thread-Stacks aus dem Allokator:** nutzt das Phase-2-Speichermodell statt
  statischer Reservierung.
- **Reschedule-Hook (Funktionszeiger):** hält die HAL scheduler-unabhängig.
- **Round-Robin, eine Priorität (Phase 4):** einfachste korrekte Basis; Bitmap-
  Prioritäten (ADR 0005) sind die nächste Verfeinerung.

## Unsafe-Bilanz

- Neu in `caprock-hal`: 2 Stellen — `init_thread_frame` (Thread-Kontext-Setup)
  und der `transmute` des Reschedule-Hooks (Trap-Dispatch-Plumbing). Beide in
  erlaubten Low-Level-Domänen, kommentiert.
- `caprock-sched`: **0 unsafe**. Kernel-Crate weiterhin **0 `unsafe`-Blöcke**.

## Risiken / offene Punkte

- **Integer-only Kontextwechsel (kein FP/SIMD-Save):** Threads dürfen in Phase 4
  **keinen** FP/SIMD-Zustand über eine Preemption halten. Kernel und Demo-Worker
  sind integer-only, daher korrekt. **Lazy-FP** (CPACR-Trap on first use, ADR
  0005) ist erforderlich, bevor FP-nutzende Threads laufen — vorgemerkt.
- **Ein globaler Scheduler-Lock:** in jedem Timer-IRQ kurz gehalten; auf 8 Kernen
  geringe Contention. Per-Kern-Locks (ADR 0005) sind die Optimierung.
- **Round-Robin/eine Priorität:** Bitmap-Prioritäten + feste Affinität als
  nächste Verfeinerung.
- **Kein Thread-Exit/Join, Stacks werden geleakt:** Worker laufen ewig; ein
  Thread-Lebenszyklus (Exit → Stack-Cap zurückgeben, TCB freigeben) fehlt noch.
- **TCBs noch nicht im Capability-System:** `ObjectKind::Tcb` + Thread-Caps
  (für capability-kontrolliertes Spawn/Kill) folgen mit der IPC/Prozess-Phase.

## Nächste Schritte (Phase 5 — IPC)

1. `ObjectKind::Endpoint`/`Notification`/`Reply`; Endpoint-Warteschlangen.
2. Syscall-Eintritt via `SVC` (synchroner Trap-Pfad ist vorbereitet), ABI
   (`caprock-abi`): Syscall-Nummern, Nachrichten-Layout.
3. Synchrone IPC (Call/ReplyRecv) + Notifications; Block/Unblock über den
   Scheduler (Threads blockieren an Endpoints).
4. Tests: IPC zwischen zwei Threads (Round-Trip), Notification-Signale.
