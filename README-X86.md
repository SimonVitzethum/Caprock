# SEL4Lake — x86_64-Port (Branch `arch/x86_64`)

Portierung des aarch64-Microkernels auf **x86_64**, gestaffelt mit häufigen QEMU-Läufen. Ziel:
**ein Branch pro Architektur** — `master` = aarch64 (QEMU `virt`), `arch/x86_64` = x86_64
(QEMU `pc`/`q35`). Der Code ist über `cfg(target_arch = …)` getrennt, sodass **beide Architekturen
auf diesem Branch baubar bleiben** (Regressionssicherheit): `./build.sh` baut aarch64, `./build-x86.sh`
baut x86_64.

## Bauen + Testen

```sh
./build-x86.sh                 # -> build/target/x86_64-unknown-none/release/sel4lake-kernel(.mb32)
./test-qemu-x86.sh             # baut + bootet unter qemu-system-x86_64 + prueft Marker
./build.sh && ./test-qemu.sh   # aarch64 weiterhin unveraendert gruen (Regression)
```

## Boot-Weg (x86_64)

Eingebauter Rust-Target **`x86_64-unknown-none`** (bare-metal, soft-float, kein SSE, static). Der
Kernel ist ein **Multiboot1**-Image (Header in `.multiboot`, Linker `kernel/x86_64-link.ld`, geladen
@ 1 MiB). QEMUs Multiboot1-Loader akzeptiert nur **ELF32**, der Rust-Target erzeugt aber ELF64 →
`build-x86.sh` castet den ELF-**Container** per `objcopy -O elf32-i386` auf ELF32 (`*.mb32`); alle
Lade-/Entry-Adressen sind < 4 GiB, der Code bleibt 64-bit. QEMU tritt am 32-bit-`_start` ein:

```
_start (.code32): PML4/PDPT nullen -> 1 GiB Identity-Map (2-MiB-Seiten) -> CR3 ->
                  CR4.PAE -> EFER.LME -> CR0.PG -> lgdt(64-bit) -> retf CS=0x08 ->
long_mode (.code64): Datensegmente, Stack, .bss nullen -> x86_rust_entry()
```
(`retf`-Far-Return statt `ljmp` — robuster in LLVM-Intel-Syntax. `.boot.bss` mit den Boot-Seiten-
tabellen liegt im Linker AUSSERHALB von `[__bss_start,__bss_end)`, damit die `.bss`-Nullung im Long
Mode die aktive Identity-Map nicht löscht; PML4/PDPT werden im 32-bit-Trampolin genullt.)

## Stand

| Stufe | Inhalt | Status |
|---|---|---|
| **0** | Boot (Multiboot→Long Mode) + 16550-Serial (COM1) + Banner | ✅ **QEMU-verifiziert** (`== ALL PASS ==`) |
| 1 | 4-Level-Paging (PML4..PT) + W^X-Identity-Map (`mmu`-API) | offen |
| 2 | IDT/Exceptions + LAPIC/IOAPIC + LAPIC-Timer (`intc`/`timer`) | offen |
| 3 | Syscall (`syscall`/`sysret`) + ring3-User-Mode + Context-Switch (x86-Register) | offen |
| 4 | SMP (APIC INIT-SIPI-SIPI) + `system_off` + Kern-Module für x86 aktivieren | offen |
| 5 | volle Selbsttest-Suite unter `qemu-system-x86_64` grün | offen |

Der **Kernel-Kern** (Caps/Sched/IPC/Loader/Cert/Audits) ist arch-agnostisch; in Stufe 0 ist er auf
diesem Branch noch **nicht** aktiv (die `sel4lake-*`-Crates + die aarch64-HAL sind in `kernel/Cargo.toml`
aarch64-only). Stufe für Stufe werden die arch-agnostischen Crates auf eine gemeinsame
`[dependencies]`-Sektion gehoben, sobald sie für x86_64 bauen, und eine x86_64-HAL (Paging, APIC,
Timer, Syscall, Context-Switch, SMP) implementiert — bei jeder Stufe bleibt aarch64 baubar und die
x86-QEMU-Simulation wird neu durchlaufen.

## Geänderte/neue Dateien (ggü. `master`)

- `kernel/src/arch/x86_64/mod.rs` — Boot-Trampolin + 16550-Serial + `x86_rust_entry` (Stufe 0).
- `kernel/x86_64-link.ld` — Multiboot1-Linker (@ 1 MiB, `.boot.bss` außerhalb der genullten `.bss`).
- `build-x86.sh`, `test-qemu-x86.sh` — x86-Build (+ELF32-Cast) & QEMU-Test.
- `.cargo/config.toml` — `[target.x86_64-unknown-none]` (Linker-Skript, static).
- `kernel/Cargo.toml` — `sel4lake-*`-Deps aarch64-only (x86 Stufe 0 = nur Boot+Serial).
- `kernel/src/{main,panic}.rs`, `kernel/src/arch/mod.rs` — `cfg(target_arch)`-Trennung.
