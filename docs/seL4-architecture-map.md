# seL4-Architektur-Referenz (Quellanalyse)

Strukturierte Karte des seL4-C-Kernels unter
`/home/simon/Dokumente/SEL4Lake/seL4/src`. **Referenz, kein Fork-Vorbild zum
Kopieren** — sie dient dazu, bewährte Muster für die Rust-Neuimplementierung zu
verstehen. Pfade sind relativ zu `seL4/src` (Header teils unter `seL4/include`).

## Top-Level-Subsysteme

| Verzeichnis | Rolle | Umfang |
|-------------|-------|--------|
| `api/` | Syscall-Eintritt, Fault-Behandlung | ~36K |
| `kernel/` | Kernlogik: Boot, Threads, Scheduling, cspace | ~104K |
| `object/` | Kernel-Objekte (TCB, Endpoint, CNode, …) | ~232K |
| `fastpath/` | Optimierter IPC-Hot-Path | ~36K |
| `machine/` | HW-Abstraktion: IO, FPU, Profiling | ~56K |
| `arch/` | Architekturspezifisch (ARM/x86/RISC-V) | ~1.7M |
| `plat/` | Plattform-Treiber/-Config (40+ Plattformen) | ~716K |
| `drivers/` | Timer, UART, IRQ, SMMU | ~136K |
| `smp/` | SMP: IPI, Inter-Core-Locking | ~16K |
| `model/` | Scheduler-Datenstrukturen/-Zustand | ~16K |

Kernbeobachtung: Der Kern (`api/`, `kernel/`, `object/`) ist relativ
architekturunabhängig; `arch/`+`plat/` (~2.4M) tragen die Hardware-Spezifik.

## Capability-System

- `include/object/cap.h`, `include/object/structures.h`,
  `include/object/cnode.h`; Implementierung in `kernel/cspace.c`, `object/cnode.c`.
- **CTE** = `cap_t` + `mdb_node_t` (Cap + Ableitungs-Metadaten).
- **CNodes**: dynamisch dimensionierte CTE-Tabellen, radix-/guard-adressiert
  (mehrstufiger Lookup wie ein Page-Table-Walk).
- **CDT/MDB**: Eltern-Kind-Beziehungen der Ableitungen → Basis für Revocation.
- Operationen: `invokeCNodeInsert/Move/Revoke/Delete`, `cteRevoke/Delete/Move`,
  `isMDBParentOf`, `isFinalCapability`.

## IPC

- `include/object/endpoint.h|notification.h|reply.h`; Impl. in
  `object/endpoint.c`, `object/notification.c`, `fastpath/fastpath.c`.
- Endpoint-Zustände: `Idle/Send/Recv`. Notification: `Idle/Waiting/Active(Badge)`.
- Syscall-Pfad: `api/syscall.c` → `handleSyscall` → `handleInvocation` →
  `decodeInvocation` (`object/objecttype.c`) → objekt-spezifischer Handler.
- Fastpath (`fastpath/fastpath.c`): bei erfüllten Bedingungen (kein Fault,
  Send-Recht, Empfänger bereit, Prio passend) direkter Übergang, sonst `slowpath`.
- Syscall-Typen: `Call, ReplyRecv, NBSendRecv, Recv, NBRecv, Send, Yield, Interrupt`.

## Scheduler

- `model/statedata.h`, `kernel/thread.h`, `kernel/thread.c`, `object/tcb.c`.
- Klassisch: Ready-Queues `[domain][priority]` + L1/L2-Bitmap (O(1)-Auswahl),
  Round-Robin-Domänen (`ksDomSchedule`, `ksCurDomain`, `ksDomainTime`).
- MCS: `object/schedcontext.c`, `kernel/sporadic.h` — Scheduling-Contexts mit
  Budget + Sporadic-Server-Refill (`scRefillHead/Tail`), Budget-Donation bei Reply.
- SMP: pro-Kern-Zustand via `NODE_STATE(...)`, Reschedule per IPI (`smp/ipi.c`).
- Kernfunktionen: `schedule`, `scheduleChooseNewThread`,
  `tcbSchedEnqueue/Append/Dequeue`, `rescheduleRequired`.

## Untyped-Speichermodell

- `include/object/untyped.h`, `object/untyped.c`, `object/objecttype.c`,
  `kernel/boot.c`.
- Alle Kernel-Objekte entstehen aus **Untyped** via `invokeUntyped_Retype`:
  validiert leere Ziel-Slots, teilt die Region, erzeugt Caps, trackt
  `capFreeIndex`. Objekte haben feste oder variable Größen (Bits).
- Finalisierung (`finaliseCap`): letzte Cap gelöscht → Aufräumen (Endpoint-IPC
  abbrechen, TCB entbinden); **Zombie-Caps** für lange, unterbrechbare Revokes.

## aarch64-Spezifika

- `arch/arm/64/head.S` (Boot-Entry), `traps.S` (Vektoren/Handler),
  `c_traps.c` (C-Dispatch), `kernel/thread.c` (Kontextwechsel),
  `kernel/vspace.c` (MMU/Page-Tables/ASID — in SEL4Lake **entfällt** der
  per-Prozess-Teil, ADR 0002), `machine/registerset.c`, `machine/fpu.c`.
- Boot: MMU-aus-Entry → Boot-Page-Tables → MMU an → `init_kernel` → Rootserver.

## Konsequenzen für SEL4Lake

Übernommen werden die *Konzepte* (Caps/CDT, Endpoint/Notification/Reply,
Bitmap-Scheduler, Retype-Idee), neu sind: Rust statt C, **kein per-Prozess-VSpace**
(ADR 0002), Zero-Copy-IPC im SAS (ADR 0004), Index-Arena-CDT statt Zeiger-MDB
(ADR 0003), Hot-Reload über stabile Cap-Identität (ADR 0006).
