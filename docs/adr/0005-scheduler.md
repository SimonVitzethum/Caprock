# ADR 0005 — Scheduler

**Status:** vorgeschlagen (Phase 4) · **Datum:** 2026-06-23

## Motivation

Der Scheduler soll **deterministisch, multicorefähig, ARM-optimiert** und
**latenzarm** sein. Zielkonfiguration: 8 Kerne, 4 GiB RAM.

## Referenz: seL4

Klassisch: pro Domäne/Priorität Ready-Queues mit L1/L2-Bitmap für O(1)-Auswahl,
Round-Robin-Domänen-Scheduling (Determinismus/Isolation). MCS-Variante: explizite
**Scheduling-Contexts** (Budget + Sporadic-Server-Refill), Budget-Donation bei
Reply. SMP: pro-Kern-Scheduler-Zustand, IPIs zum Reschedule. Details:
[`../seL4-architecture-map.md`](../seL4-architecture-map.md).

## Analysierte Lösungsansätze

### A) Globale Run-Queue
- **+** Einfaches Lastausgleichsverhalten.
- **−** Globale Sperre → Contention und Nichtdeterminismus auf 8 Kernen. Verworfen.

### B) Pro-Kern-Ready-Queues + Bitmap-Prioritäten, feste Affinität (gewählt)
Jeder Kern hat eigene Ready-Queues mit L1/L2-Prioritäts-Bitmap (O(1)-Auswahl).
Threads haben eine **feste Kern-Affinität** (explizit, nicht heuristisch
migrierend). Migration nur durch expliziten Capability-Aufruf.

- **+** Keine globale Sperre → Determinismus + Skalierung.
- **+** O(1)-Auswahl, vorhersagbare Latenz.
- **+** Feste Affinität = reproduzierbares Timing (gut für *deterministisch*).
- **−** Lastausgleich ist explizit/manuell statt automatisch (bewusst: Vorhersagbarkeit
  vor Heuristik).

### C) MCS-Scheduling-Contexts für Budgetierung (übernommen als Schicht)
Budget + Periode pro Scheduling-Context, Sporadic-Server-Refill, Donation bei
Reply — als optionale Schicht über B.

- **+** Zeitliche Isolation/Garantien, deterministische Budgets, ideal für
  Echtzeit und für faire Behandlung hot-reloadbarer Dienste.
- **−** Mehr Zustand/Komplexität; daher schrittweise nach dem Basis-Scheduler.

## Entscheidung

**B als Basis, C als Schicht.** Pro-Kern-Bitmap-Scheduler mit fester Affinität
und Prioritäten; darüber optionale Scheduling-Contexts (Budget/Refill/Donation)
nach MCS-Vorbild. Cross-Core-Reschedule über gezielte IPIs (ADR 0004), nicht über
globale Sperren. Idle-Thread pro Kern (`wfi`/`wfe`).

ARM-Spezifika: Zeitbasis ist der **Generic Timer** (`CNTPCT`/`CNTV`), Interrupts
über die **GIC** (Phase 1). Kontextwechsel speichert GP-Register immer, FP/SIMD
**lazy** (nur wenn der Thread sie nutzt) — spart Zyklen im häufigen Fall.

## Sicherheitsauswirkungen

Feste Affinität + pro-Kern-Zustand reduzieren Cross-Core-Seitenkanäle und
Timing-Variabilität. Scheduling-Contexts verhindern, dass eine kompromittierte
Komponente CPU-Zeit monopolisiert (Verfügbarkeits-Schutz).

## Performanceauswirkungen

O(1)-Auswahl, keine globale Contention, kein TTBR-Wechsel beim Switch (ADR 0002)
→ niedrige, konstante Kontextwechsel-Latenz. Lazy-FP spart im typischen Fall den
SIMD-Save/Restore.
