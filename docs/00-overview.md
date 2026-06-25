# SEL4Lake — Architekturübersicht

SEL4Lake ist ein eigenständiger, capability-basierter Microkernel in Rust,
inspiriert von seL4 und der seL4-Microkit-Laufzeit, aber mit einem bewusst
anderen Speichermodell. Ziel ist ein Betriebssystem, das zugleich
**hochsicher, hochperformant, capability-basiert, deterministisch und vollständig
modular** ist.

Dieses Dokument gibt den Gesamtüberblick. Begründete Einzelentscheidungen liegen
als ADRs (Architecture Decision Records) unter [`adr/`](adr/). Die Analyse des
seL4-Quellcodes (als Referenz, *nicht* als Fork-Basis) liegt in
[`seL4-architecture-map.md`](seL4-architecture-map.md).

---

## 1. Leitidee

Drei existierende Systeme bilden die geistige Grundlage:

| System | Was wir übernehmen | Was wir anders machen |
|--------|--------------------|------------------------|
| **seL4** | Capability-Modell, Endpoint/Notification-IPC, Untyped-Speicher, Fastpath, deterministische Kernelstruktur | Kein per-Prozess-VSpace/MMU-Isolation; Rust statt C; kein bewiesener, aber unsicherer C-Kern |
| **seL4 Microkit** | Protection-Domain-Modell, Channels, statisches Systemlayout, schlanke Runtime | Microkit-Runtime wird Teil des Kernelimages; Komponenten sind hot-reloadbar |
| **Theseus OS** | Single-Address-Space (SAS), Speichersicherheit *intralingual* durch Rust, Live-Austausch von Modulen | Zusätzlich explizites seL4-Capability-Modell als Autoritätsschicht |

Der zentrale, definierende Unterschied zu seL4: **SEL4Lake nutzt keine
per-Prozess-Adressräume und keine MMU-basierte Speicherisolation**. Jede Adresse
ist eine echte physische Adresse. Speichersicherheit entsteht aus zwei sich
ergänzenden Mechanismen — der Rust-Typsicherheit (verhindert Speicherfehler
*innerhalb* einer Komponente) und Capabilities (regeln Autorität *zwischen*
Komponenten). Details und Konsequenzen: [ADR 0002](adr/0002-no-virtual-memory-sas.md).

---

## 2. Was im Kernelimage liegt — und was nicht

**Im Kernelimage (Trusted Computing Base):**

- Microkernel-Kern (Boot, Traps, Scheduling-Schleife)
- Capability-System
- Scheduler
- IPC-System (Endpoints, Notifications, Reply)
- Microkit-Runtime (PD-/Channel-Modell)

**Nicht im Kernelimage** (laufen als Userland-Komponenten, hot-reloadbar):

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

- Treiber, Netzwerkstack, Dateisysteme, USB, GUI
- Systemdienste, Device-Manager, Shell, Logging

Die einzige Geräteberührung im Kernel ist eine **Debug-Konsole** (PL011-UART)
für die Bring-up-Phase — analog zu seL4s Debug-`printf`. Produktives Logging ist
ein Userland-Dienst. Siehe [ADR 0006](adr/0006-hot-reload-architecture.md).

---

## 3. Geplante Crate-Struktur

Subsysteme werden erst als eigene Crate ausgegliedert, sobald sie echten Inhalt
haben (keine leeren Abstraktionen auf Vorrat). Vorhanden nach Phase 6:
`kernel`, `crates/sel4lake-{sync, abi, hal, mem, cap, sched, ipc, microkit}`.
Zielstruktur:

```
SEL4Lake/
  kernel/              # bootbares Image: Boot, Arch-Glue, verdrahtet Subsysteme
  crates/
    sel4lake-abi/      # geteilte Kernel<->User-ABI: Syscall-Nrn, Nachrichten-Layout, Cap-Rechte
    sel4lake-sync/     # no_std-Synchronisation: Ticket-Spinlock, Per-CPU-Daten
    sel4lake-hal/      # aarch64-HAL: Kontextwechsel, Traps, GIC, Timer, Identity-MMU, SMP
    sel4lake-mem/      # capability-basierter physischer Allokator (Untyped-artig)
    sel4lake-cap/      # Capability-Typen, CNode, Derivation-Tree (CDT)
    sel4lake-sched/    # deterministischer Multicore-Scheduler
    sel4lake-ipc/      # Endpoints, Notifications, Reply, Transfer, Fastpath
  microkit/
    sel4lake-microkit/ # In-Image-Microkit-Runtime (PD-/Channel-Modell)
  user/                # Out-of-Image-Komponenten (Treiber, Dienste) — hot-reloadbar
  targets/  docs/  build.sh  run-qemu.sh
```

**Abhängigkeitsrichtung** (azyklisch, von unten nach oben):
`abi` → `sync` → `hal` → `mem` → `cap` → {`sched`, `ipc`} → `microkit` → `kernel`.

Tiefe, schmale Interfaces (wenige, ausdrucksstarke APIs), keine parallelen
Systeme mit gleicher Funktion, keine versteckten globalen Zustände.

---

## 4. Phasen-Roadmap

| Phase | Inhalt | Tests in QEMU |
|------:|--------|---------------|
| **0** ✅ | Bring-up: Boot, Sekundärkern-Parken, Stack/BSS, Debug-Konsole | Boottest |
| **1** ✅ | HAL: Exception-Vektoren/Traps, GICv2, Generic Timer, **MMU als einzelne Identity-Map mit Caches an**, Ticket-Spinlock, SMP-Bring-up (PSCI/HVC) | Boot + Multicore-Boot (8/8) + Timer-Tick ✓ |
| **2** ✅ | Physisches Speichermodell: lineare Memory-Capabilities, capability-basierter Allokator (Coalescing), Besitz/Transfer/Rückgabe/`split`/`restrict`; **W^X-Härtung** vorgezogen | Cap-Test (alloc/split/transfer/free) ✓ |
| **3** ✅ | Capability-System-Kern: `CapSpace`, CDT, Objekt-Refcount + Finalisierung, Generations-Handles, copy/mint/move/delete/revoke | Cap-Test (Delegation/Revocation/Finalisierung) ✓ |
| **4** ✅ | Threads + Scheduler: TCB, Kontextwechsel (SP-Tausch im Trap-Pfad), präemptiver Per-Kern-Round-Robin, Idle, Thread-Stacks aus dem Allokator | Scheduler-Test (Preemption, 3 Worker + Idle) ✓ |
| **5** ✅ | IPC: synchrone Endpoints (Call/Recv/Reply), SVC-Syscall-Dispatch + ABI, Block/Unblock + Rendezvous-Direkt-Switch | IPC-Test (Call/Reply-Round-Trip) ✓ |
| **6** ✅ | Microkit-Runtime im Image: Protection Domains mit eigenem Cap-Space, **cap-gesicherte IPC** (Endpoints als Caps, Rechte-Prüfung) | PD↔PD-IPC + Zugriff-verweigert-Test ✓ |
| **7** ✅ | Hot-Reload: Server-Komponente im laufenden System ersetzen (Quiesce → Swap über stabile Endpoint-Cap), ohne Kernel-Neustart, transparent für Clients | Hot-Reload-Test (v1→v2, gleicher Endpoint) ✓ |

Nach jeder Phase: Build → Boottest → IPC-Test → Scheduler-Test → Capability-Test
(soweit für die Phase anwendbar) und ein Phasenbericht unter
[`phase-reports/`](phase-reports/).

---

## 5. Vertrauensmodell (wichtige Konsequenz des SAS-Designs)

Ohne MMU-Isolation kann der Kernel nativen, beliebigen Maschinencode **nicht**
hardwareseitig einsperren: ein Prozess mit rohen Zeigern könnte ohne MMU überall
hinschreiben. Speichersicherheit *zwischen* Komponenten beruht deshalb darauf,
dass **alle im System laufenden Komponenten speichersicher sind** — d. h. in
sicherem Rust geschrieben und durch eine vertrauenswürdige Toolchain übersetzt
(intralinguale Sicherheit, wie bei Theseus). Capabilities sind die
Autoritätsschicht darüber: sie entscheiden, *welche* Speicher-/IPC-/Geräte-Rechte
eine Komponente überhaupt besitzt (Least Privilege).

Daraus folgt eine harte Projektregel: `unsafe` ist außerhalb der erlaubten
Low-Level-Domänen (Boot, Kontextwechsel, Registerzugriffe, MMU-/Interrupt-Init)
zu vermeiden, denn dort — und nur dort — kann die intralinguale Sicherheitsgarantie
gebrochen werden. Vollständige Begründung und Alternativen in
[ADR 0002](adr/0002-no-virtual-memory-sas.md).

---

## 6. Zielhardware

ARM (aarch64), 8 Kerne, 4 GiB RAM. Entwicklungs- und Testplattform: QEMU
`virt`. Build und Boot sind in [ADR 0001](adr/0001-toolchain-and-build.md)
beschrieben; gebaut wird mit `./build.sh`, gestartet mit `./run-qemu.sh`.
