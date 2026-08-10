# Phasenbericht 0 — Bring-up

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert

## Was umgesetzt wurde

Ein bootbares Kernel-Skelett für aarch64, das auf QEMU `virt` startet und über
die PL011-Konsole einen Banner ausgibt.

- **Workspace + Build** (ADR 0001): Cargo-Workspace, `rust-toolchain.toml`
  (nightly + `rust-src`/`llvm-tools`), custom Bare-Metal-Target
  `targets/aarch64-caprock.json`, `-Z build-std`, `.cargo/config.toml`,
  `build.sh`, `run-qemu.sh`.
- **Linker-Script** `kernel/linker.ld`: Ladeadresse `0x4008_0000`, Segmente
  RX/R/RW, `.bss` (NOLOAD) + 64 KiB Boot-Stack.
- **Boot-Trampolin** `kernel/src/arch/aarch64/boot.rs` (`global_asm!`):
  Exceptions maskieren → Sekundärkerne parken → Stack setzen → `.bss` nullen →
  `kernel_main(dtb)`.
- **Arch-Layer** `arch/aarch64/mod.rs`: `current_el()`, `halt()`.
- **Debug-Konsole** `console.rs`: PL011 @ `0x0900_0000`, `core::fmt::Write`,
  `print!`/`println!`.
- **Panic-Handler** `panic.rs`: Meldung ausgeben, Kern anhalten.
- **Dokumentation:** Übersicht + ADRs 0001–0006 + seL4-Referenzkarte.

## Build- und Boot-Ergebnis (verifiziert)

Build (`./build.sh`): erfolgreich, **keine Warnungen**. ELF-Entry `0x40080000`,
drei LOAD-Segmente (RX/R/RW). Boot in QEMU (`-machine virt -cpu cortex-a72
-smp 8 -m 4G -nographic`):

```
========================================
 Caprock — capability microkernel
 phase 0: bare-metal bring-up
========================================
arch    : aarch64 (running at EL1)
dtb     : 0x0000000000000000
console : PL011 @ 0x09000000
status  : boot OK — primary core alive, secondaries parked
[idle] halting primary core (wfe)
```

Nur **eine** Banner-Ausgabe trotz `-smp 8` → Sekundärkerne werden korrekt
zurückgehalten (PSCI hält sie bis `CPU_ON`; das Park-Loop ist zusätzlich
defensiv). Kernel läuft wie geplant in **EL1** (QEMU ohne `virtualization=on`).

## Getroffene Entscheidungen

- **Eigenständiger Kernel statt Microkit-PD** (ADR 0001-B verworfen):
  `microkit_rust` baut Userland-PDs *auf* seL4 — Caprock ist ein eigener
  Kernel, den QEMU direkt via `-kernel` lädt. `microkit_rust` bleibt Referenz
  für `no_std`/IPC/Build, nicht Kernelbasis.
- **SAS statt VM** (ADR 0002): „kein virtueller RAM“ wird als Single-Address-Space
  mit *einer* Identity-Map + Caches umgesetzt (nicht „MMU komplett aus“, da das
  alles uncached und damit nicht *hochperformant* machte). In Phase 0 läuft die
  MMU noch nicht; das kommt in Phase 1.
- **FP/NEON aktiviert lassen**: rustc koppelt `fp-armv8` an `neon` an die ABI;
  Deaktivieren wird zum harten Fehler. Kernel nutzt kein FP; FP-Kontext wird
  lazy beim Context-Switch behandelt.

## Unsafe-Bilanz

`unsafe` ausschließlich in erlaubten Domänen, jeweils kommentiert:
1. `boot.rs` — Boot-Assembly (`global_asm!`).
2. `arch/aarch64/mod.rs` — `mrs CurrentEL`, `wfe` (Registerzugriff/Low-Level-Insn).
3. `console.rs` — volatiler MMIO-Zugriff auf PL011-Register.

Kein `unsafe` außerhalb dieser Bereiche.

## Risiken / offene Punkte

- **DTB-Pointer = 0:** QEMU übergibt für unser rohes ELF keinen DTB-Zeiger in
  `x0`. Phase 1 muss die Plattforminfos (RAM-Größe, GIC-/UART-Basen) entweder
  aus dem DTB an der konventionellen RAM-Basis lesen oder für QEMU `virt`
  zunächst fest verdrahten. **Niedriges Risiko**, bekannt.
- **MMU noch aus:** bis Phase 1 läuft alles uncached (langsam) und mit
  Strict-Alignment-Zwang. Kein `W^X`/NX bis zur Identity-Map.
- **Bedrohungsmodell (SAS):** ohne MMU-Isolation müssen alle Komponenten
  speichersicher (Rust) sein. Native untrusted Binaries sind nicht isolierbar —
  bewusst außerhalb des Kern-Bedrohungsmodells (ADR 0002), muss prominent bleiben.
- **Konsole nicht SMP-sicher:** akzeptabel für Phase 0 (single-core); Lock kommt
  mit dem Scheduler/Per-CPU-Daten (Phase 1/4).

## Nächste Schritte (Phase 1 — HAL-Fundament)

1. **Exception-Vektoren** (`VBAR_EL1`) + Trap-Dispatch (sync/IRQ/FIQ/SError).
2. **MMU: eine Identity-Map** (Block-Mappings für 4 GiB RAM, MMIO als Device),
   Caches + `W^X`/`XN`/`RO`-Attribute aktivieren (ADR 0002).
3. **GIC** (v2/v3) initialisieren, **Generic Timer** als Tick-Quelle.
4. **Per-CPU-Daten** + **Ticket-Spinlock** (`crates/caprock-sync`), SMP-Bring-up
   der Sekundärkerne via **PSCI `CPU_ON`**.
5. Tests: Boot, Multicore-Boot (alle 8 Kerne melden sich), Timer-Tick.
