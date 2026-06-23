# Phasenbericht 5 — IPC

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Synchrone Inter-Thread-Kommunikation über Endpoints — Syscall-Eintritt, Rendezvous,
Nachrichtentransfer und Blockieren/Entblocken im Scheduler.

### ABI (`crates/sel4lake-abi`)

Geteilte, abhängigkeitsfreie Schnittstelle: Syscall-Nummern (`YIELD/CALL/RECV/
REPLY`), Register-Layout (`x0`=Nr/Ergebnis, `x1`=EP/Badge, `x2..x5`=Nachricht,
`x6`=Tag) und Ergebniscodes. Register-basierte IPC für niedrige Latenz.

### HAL-Erweiterung (`crates/sel4lake-hal`)

- **SVC-Dispatch:** `handle_exception` erkennt `ESR.EC=0x15` (SVC) und routet an
  einen registrierten **Syscall-Hook** (analog zum Reschedule-Hook). Der Hook
  liefert — wie bei Preemption — den fortzusetzenden TrapFrame zurück; eine
  blockierende IPC gibt einfach den Frame eines *anderen* Threads zurück.
- **`syscall::invoke`:** Thread-seitiger Stub (`svc #0` mit Register-Marshalling).
- **`frame_reg`/`frame_set_reg`:** gekapselter Zugriff auf Register eines
  (gesicherten) TrapFrames — Grundlage des Nachrichtentransfers.

### Scheduler-Erweiterung (`crates/sel4lake-sched`)

`block_current` (blockieren + nächsten bereiten Thread wählen), `switch_to`
(blockieren + **direkt** zum IPC-Partner wechseln — Rendezvous-Fastpath),
`unblock`, `frame_of`, `current_id`. TCB trägt nun Kern-Affinität + Blockiert-Flag.

### IPC (`crates/sel4lake-ipc`)

Endpoint-Tabelle mit Sender-/Empfänger-Warteschlangen und einem „aktuellen
Aufrufer". `call`/`recv`/`reply` realisieren das RPC-Muster: bei einem wartenden
Partner Rendezvous (Transfer + Direkt-Switch), sonst Blockieren. **Kein eigenes
`unsafe`** (Frame-Zugriff in der HAL gekapselt).

### Kernel-Integration (`kernel/src/threads.rs`)

Scheduler + Endpoints liegen hinter **einem** gemeinsamen `SYSTEM`-Lock → keine
Lock-Ordering-Probleme zwischen Timer- und Syscall-Pfad. Reschedule- und
Syscall-Hook locken nur diesen Zustand.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**. IPC-Demo (Client ruft Server, der verdoppelt):

```
ipc     : call(10) -> 20 (erwartet 20)
ipc     : call(20) -> 40 (erwartet 40)
ipc     : call(30) -> 60 (erwartet 60)
ipc     : call(40) -> 80 (erwartet 80)
ipc     : ALL PASS
```

Plus weiterhin: MMU, memtest, captest, sched (3 Worker preemptiv + Idle),
8/8 Kerne online. Build ohne Warnungen.

## Getroffene Entscheidungen

- **Syscall als Sonderfall des Trap-Pfads:** `handle_exception` gibt den
  fortzusetzenden Frame zurück; Block/Switch ist exakt derselbe Mechanismus wie
  bei der Timer-Preemption (Phase 4). Minimaler, einheitlicher Code.
- **Direkt-Switch beim Rendezvous (`switch_to`):** bei `call` mit wartendem
  Empfänger wird ohne Umweg über die Ready-Queue zum Server gewechselt — niedrige
  Latenz (seL4-Fastpath-Idee).
- **Ein gemeinsamer `SYSTEM`-Lock** (Scheduler + Endpoints): umgeht
  Lock-Ordering vollständig; Per-Kern-Aufteilung ist die spätere Optimierung.
- **Register-basierte Nachrichten** (4 Wörter + Tag): schnell; größere Transfers
  später per Zero-Copy-Memory-Cap (ADR 0004) ohne Kopieren.
- **Threads als EL1-Kernel-Threads** nutzen den Syscall-Pfad (noch kein EL0):
  exerziert den vollständigen IPC-Weg end-to-end vor der Userland-Phase.

## Unsafe-Bilanz

- Neu in `sel4lake-hal`: SVC-Dispatch (nutzt vorhandenes ESR-Lesen), Syscall-Hook
  (`transmute` wie Reschedule), `frame_reg`/`frame_set_reg` (Kontextzugriff),
  `syscall::invoke` (`svc`-Asm). Alle in erlaubten Low-Level-Domänen, kommentiert.
- `sel4lake-abi`, `sel4lake-ipc`, `sel4lake-sched`: **0 unsafe**.
- Kernel-Crate weiterhin **0 `unsafe`-Blöcke**.

## Risiken / offene Punkte

- **Endpoints noch nicht cap-gesichert:** IPC adressiert Endpoints per ID; der
  Zugriff ist noch nicht über per-Thread-cspaces (Capability-Besitz) gated. Echte
  Zugriffskontrolle braucht per-Thread-Cap-Spaces (Phase 6) — bis dahin kann jeder
  Thread jede Endpoint-ID nennen. **Wichtigste offene Sicherheitslücke**, dokumentiert.
- **Nur Call/Recv/Reply, ein Endpoint-Typ:** Notifications (async Badges),
  nicht-blockierendes `Send`, und ein echtes `ReplyRecv` (in einem Syscall) folgen.
- **Kein Capability-Transfer in Nachrichten:** Rechte-/Memory-Cap-Übergabe per IPC
  (ADR 0004) ist noch offen.
- **Integer-only Kontext (Phase 4):** gilt weiter — Threads dürfen kein FP über
  Block/Preemption halten (Lazy-FP TODO).
- **Ein globaler `SYSTEM`-Lock:** Contention auf 8 Kernen bei hoher IPC-/Tick-Rate;
  Per-Kern-Strukturen sind die Optimierung (ADR 0004/0005).

## Nächste Schritte (Phase 6 — Microkit-Runtime im Image)

1. Per-Thread/-PD-Capability-Spaces; Endpoint-Zugriff über Endpoint-**Caps**
   (`ObjectKind::Endpoint` ins Cap-System integrieren) → echte IPC-Zugriffskontrolle.
2. Protection-Domain-Modell + Channels (Microkit) auf Endpoints/Notifications abbilden.
3. Notifications + Capability-Transfer in Nachrichten.
4. Tests: PD↔PD-Kommunikation über Channels, cap-gesicherte IPC.
