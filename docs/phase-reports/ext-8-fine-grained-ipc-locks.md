# Ausbaustufe 8 — Feinkörniges IPC-Locking (per-Endpoint-Locks)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Ersetzt den **einen** globalen Ressourcen-Lock (`RES`), der alle Syscalls global
serialisierte, durch mehrere unabhängige Locks. IPC auf verschiedenen Endpoints
läuft nun nebenläufig; Speicher-, Cap- und IPC-Verwaltung blockieren sich nicht
mehr gegenseitig.

## Lock-Aufteilung

`RES{phys, cspace, eps, ntfns, pds}` →
- **`CAPS`** — `SpinLock<Caps{cspace, pds}>`: Cap-Auflösung + -Verwaltung + PDs.
- **`MEM`** — `SpinLock<PhysAllocator>`: physischer Speicher.
- **`EPS[i]`** — ein `SpinLock<Endpoint>` **je Endpoint** (32 unabhängige Locks).
- **`NTFNS[i]`** — ein `SpinLock<Notification>` je Notification.

## Sperrordnung (deadlockfrei, vorab adversarisch geprüft)

Totaler Rang: **`CAPS < {EPS[i], NTFNS[i], MEM} < SCHEDS[core] < FP_STATES`**. Jede
Hold-then-acquire-Kante steigt streng im Rang → kein Zyklus. Wichtige Pfade:

- **Dispatch:** sperrt `CAPS` für die *kurze* Cap-Auflösung und gibt ihn frei →
  danach läuft das IPC unter dem **per-Endpoint**-Lock parallel zu anderen
  Endpoints. Nur `REPLY` mit Cap-Transfer hält `CAPS` über das Sperren des
  Endpoints (`CAPS → EPS[i]`, der grant läuft vor dem Wecken des Aufrufers), gibt
  `CAPS` aber vor dem Rendezvous frei.
- **`spawn`:** `MEM` (Stack) → `SCHEDS` → `FP_STATES`.
- **`reap`:** sammelt Zombies unter `SCHEDS`, **gibt frei**, dann `MEM` — nie beide
  zusammen (sonst Inversion zu `spawn`s `MEM → SCHEDS`).
- **`cap_delete/revoke`:** `CAPS → MEM` (Finalisierung gibt Speicher zurück).
- **Reschedule (Timer/IPI):** nur `SCHEDS[core]`, wartet nie auf einen anderen Lock
  → kann an keinem Zyklus teilnehmen.

Der **Dispatch besorgt das Locking selbst** (microkit): er bekommt `&SpinLock<Caps>`
und die per-Objekt-Lock-Arrays und sperrt in der korrekten Reihenfolge; die
`SchedOps`-Facade sperrt je Operation genau eine `SCHEDS`-Instanz. Im Trap sind IRQs
maskiert → kein Preempt beim Lock-Halten.

## Korrektheit

Ein Subagent hat den Lock-Graphen adversarisch gegen alle Pfade geprüft: **kein
Deadlock-Zyklus**. Zwei Implementierungs-Invarianten bestätigt: (1) `SchedOps` sperrt
je Aufruf genau **eine** `SCHEDS`-Instanz und gibt sie frei (Cross-Core-IPC =
`unblock` dann `block_current` als getrennte Aufrufe, nie verschachtelt); (2) der
Ticket-`SpinLock` nutzt Acquire/Release → Cross-Core-Frame-Transfers sind korrekt
sichtbar.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (18 Checks, unverändert). Die IPC-, Hot-Reload-,
Cap-Transfer- und Cross-Core-IPC-Tests laufen unverändert grün, jetzt aber über
getrennte Locks. **8/8 back-to-back stabil** (kein Deadlock/Hang).

## Offene Punkte

- Cap-Auflösung serialisiert weiterhin kurz auf `CAPS`. Lock-freie/Read-parallele
  Auflösung (z. B. RwLock oder seqlock auf cspace) wäre die nächste Stufe.
- Die `create`-Pfade (Endpoint/Notification-Slot-Reservierung) scannen die Arrays;
  unkritisch, da nur beim Setup.
