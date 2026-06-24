# Ausbaustufe 10 — Lastausgleich (lastbewusste Thread-Platzierung)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Verteilt neue Threads über die Kerne, statt sie alle auf dem erzeugenden Kern zu
häufen — **Platzierung zur Spawn-Zeit** auf dem am wenigsten belasteten Kern.

## Mechanismus

- `Scheduler::load()` — Lastmaß eines Kerns: Anzahl belegter TCB-Slots (laufend +
  bereit + blockiert/geparkt).
- `system::least_loaded_core()` — sperrt jede Scheduler-Instanz einzeln/kurz (nie
  zwei gleichzeitig), liefert den Kern mit der geringsten Last.
- `system::spawn_balanced(entry, arg, prio)` — platziert den Thread auf diesem Kern
  (über `spawn_on_core`). Best-effort (die Last kann sich zwischen Auswahl und
  Spawn ändern; für die deterministische Verteilung genügt es, da die Last mit
  jedem Spawn steigt und der nächste Spawn den dann leichtesten Kern wählt).

## Warum keine Laufzeit-Migration

Echte präemptive **Migration** (einen laufenden Thread auf einen anderen Kern
verschieben) kollidiert mit dem TCB-Partitionsmodell (ext-6): Die [`ThreadId`]
kodiert den besitzenden Kern im globalen Slot (`core*PER_CORE + local`). Eine
Migration änderte den Slot → die `ThreadId` → und bräche damit **alle** Referenzen
(Tcb-Caps im `CapSpace`, PD-Bindungen, Endpoint-/Notification-Wartelisten,
FP-Owner). Korrekte Migration bräuchte entweder kern-stabile Thread-IDs (Entkopplung
von ID und Partition) oder ein konsistentes Ummappen aller Referenzen — eine
größere Architekturänderung. Bewusst nicht umgesetzt; Threads bleiben nach der
Platzierung kern-gebunden (das bewahrt die deterministische, lock-arme Per-Kern-
Einplanung).

## Begleitende Korrektur: per-Kern-Reaping

Bisher sammelte nur der Idle-Manager auf core 0 beendete Threads ein. Jetzt reapt
**jeder Kern in seiner Idle-Schleife seine eigenen** Zombies — sonst lecken Threads
Speicher, die auf einem Sekundärkern enden. Der `life`-Test misst die
Stack-Rückgewinnung nun über einen **monotonen Reaped-Bytes-Zähler**
(`system::reaped_bytes`) statt eines absoluten `total_free`-Vergleichs — robust
gegen die Speicher-Churn der Reclaim-/Balance-Tests.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (20 Checks). Neuer `balance`-Check: der Manager
platziert 16 Worker lastbewusst; beobachtete Verteilung z. B.
`[0, 2, 2, 3, 3, 2, 2, 2]` — der ausgelastete Bootkern (core 0) bekam **0**, die
Last verteilte sich gleichmäßig über die Kerne 1–7 (7 Kerne genutzt, max 3/Kern).
Ohne Lastausgleich landeten alle 16 auf core 0 (`[16,0,…]`). 6/6 Läufe stabil.

## Offene Punkte

- Laufzeit-Migration/Work-Stealing (s. o.) — größere Architekturänderung.
- Lastmaß ist die TCB-Anzahl; ein gewichtetes Maß (Priorität, Laufzeit) wäre
  feiner.
