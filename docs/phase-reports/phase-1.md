# Phasenbericht 1 — HAL-Fundament

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Ein vollständiges, multicorefähiges Hardware-Fundament für aarch64. Der Kernel
bringt jetzt alle 8 Kerne hoch, mit MMU/Caches, Exception-Handling, GIC und
periodischem Timer pro Kern.

Neue/erweiterte Bausteine:

- **`crates/sel4lake-sync`** — fairer **Ticket-Spinlock** (`SpinLock<T>`), `no_std`.
- **`crates/sel4lake-hal`** — aarch64-HAL, gegliedert in Module:
  - `cpu` — Registerzugriffe (CurrentEL, MPIDR, IRQ-Maske, Barrieren, wfi/halt).
  - `console` — PL011-Debug-UART; lock-freier (`emit_raw`/`emit_fmt`, Pre-MMU/Panic)
    und SMP-serialisierter Pfad (`println!`).
  - `exception` — 2-KiB-Vektortabelle (16 Einträge), voller Kontext-Save in einen
    `TrapFrame`, zentraler `handle_exception`, `eret`-Rückkehr.
  - `mmu` — **eine** Identity-Map (39-bit VA, 1-GiB-Blöcke): L1[0]=Device (MMIO),
    L1[1..=8]=Normal-WB-cacheable; aktiviert MMU + D-/I-Cache (ADR 0002).
  - `gic` — GICv2-Treiber (Distributor + CPU-Interface, IAR/EOI-Pfad).
  - `timer` — Generic Timer (EL1-PPI 30), 10-Hz-Tick, Per-Kern-Tick-Zähler.
  - `psci` — `CPU_ON` via **HVC** (Conduit laut QEMU-DTB) zum Sekundärkern-Start.
- **`kernel`** — Boot-Trampolin um `_start_secondary` erweitert; Init-Reihenfolge
  (MMU → Vektoren → GIC → Timer), PSCI-Bring-up der Kerne 1–7, Idle-Schleife.
- **`test-qemu.sh`** — automatisierter Boot-/SMP-/Timer-Test mit PASS/FAIL.

## Verifiziertes Ergebnis

`./test-qemu.sh` (QEMU `virt`, `-cpu cortex-a72 -smp 8 -m 4G`):

```
mmu     : identity-map, M=1 C=1 I=1 (caches an)
core 0  : online (vectors, gic, timer @ 10 Hz)
timer   : CNTFRQ=62500000 Hz, PPI 30
smp     : starte Kerne 1..7 via PSCI CPU_ON (hvc) ...
core 1..7 : online (EL1, MMU on)
timer   : core 0..7 tick 1/2/3
== checks ==
  PASS: MMU + Caches aktiv
  PASS: alle 8 Kerne online
  PASS: alle 8 Kerne ticken
== ALL PASS ==
```

- **Boottest:** ok. **Multicore:** alle 8 Kerne online. **Timer/IRQ:** alle 8
  Kerne erzeugen unabhängige Ticks (Exception-Vektor + GIC + `eret` korrekt).
- **SMP-Konsole:** Ticket-Spinlock serialisiert die Ausgabe über alle Kerne
  (keine zerhackten Zeilen).
- Build **ohne Warnungen**.

## Getroffene Entscheidungen

- **Reihenfolge MMU-zuerst:** Atomare LDXR/STXR (Grundlage von `core::atomic`)
  sind auf aarch64 nur mit aktiver MMU + cacheable Memory wohldefiniert. Daher:
  erst MMU/Caches, dann Spinlock/SMP. Pre-MMU-Ausgabe ist bewusst lock-frei.
- **Identity-Map mit 1-GiB-Blöcken, eine L1-Tabelle (4 KiB):** minimaler,
  robuster Page-Table-Aufbau; deckt MMIO (Device) und 4 GiB RAM (Normal) ab.
- **GICv2** (von QEMU `virt` bereitgestellt), **EL1-Physical-Timer (PPI 30)**.
- **PSCI via HVC**: aus dem QEMU-DTB ermittelt (`method = "hvc"`, CPU_ON 0xC4000003).
- **Stacks der Sekundärkerne im Linker-Script** reserviert; die Stack-Spitze wird
  via PSCI-`context_id` an den jeweiligen Kern übergeben (kein `static mut`).
- **Feste Kern-Affinität:** MPIDR Aff0 = Kernindex (QEMU `virt`, ein Cluster).

## Unsafe-Bilanz

28 `unsafe`-Stellen insgesamt, vollständig kommentiert. Verteilung:

- **HAL** (erlaubte Domänen — Registerzugriffe, MMU-/Interrupt-Init, Asm):
  `cpu` (9), `mmu` (4), `exception` (3), `gic` (3), `timer` (3), `psci` (1),
  `console` (1).
- **sync** (4): die unvermeidlichen Lock-Primitive (`unsafe impl Send/Sync` +
  zwei `UnsafeCell`-Derefs) — für jede `no_std`-Lock-Implementierung notwendig.
- **kernel-Crate:** **0 `unsafe`-Blöcke** (außer dem Boot-Assembler via
  `global_asm!`). Ziel „Unsafe gegen Null außerhalb der Low-Level-Domänen“ erfüllt.

## Risiken / offene Punkte

- **Kein W^X bisher:** RAM ist als ausführbar+RW (1-GiB-Blöcke) gemappt. Echtes
  `W^X` (`.text` = RX, Rest = RW+XN) erfordert feinere Granularität (2-MiB-/4-KiB-
  Mapping des Kernelbereichs) und 2-MiB-Section-Alignment. **Als Härtungsschritt
  vorgemerkt** (vor/parallel zu Phase 2). MMIO ist bereits XN.
- **DTB weiterhin 0:** Plattforminfos sind aktuell für QEMU `virt` fest verdrahtet
  (RAM-Layout, GIC-/UART-/Timer-Parameter). DTB-Parsing folgt, wenn dynamische
  Plattforminfos nötig werden.
- **Per-CPU-Daten minimal:** bisher nur Per-Kern-Tick-Zähler (Array, MPIDR-Index).
  Vollständige Per-CPU-Struktur (über `TPIDR_EL1`) kommt mit dem Scheduler (Phase 4).
- **GIC-Gruppenkonfiguration:** minimaler GICv2-Setup (genügt für QEMU). Auf realer
  HW ggf. Group0/1- und SMPEN-/Coherency-Feinheiten nötig (dokumentiert).

## Nächste Schritte (Phase 2 — physisches Speichermodell)

1. **Memory-Capabilities** + **capability-basierter physischer Allokator**
   (Untyped-artiges Splitten realer RAM-Regionen) — ADR 0003.
2. RAM-Inventar bestimmen (zunächst statisch für QEMU `virt`: freier Bereich
   oberhalb des Kernelimages bis 4 GiB).
3. Besitz/Transfer/Rückgabe von RAM über Caps; Tests (alloc/free/transfer).
4. Optional vorgezogen: **W^X-Härtung** der Identity-Map.
