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
| **0** | Boot (Multiboot→Long Mode) + 16550-Serial (COM1) | ✅ QEMU-verifiziert |
| **1** | 4-Level-Paging (PML4..PT) + W^X-Identity-Map + `CR0.WP` | ✅ QEMU-verifiziert |
| **2** | IDT/Exceptions (256 Stubs) + LAPIC + periodischer Timer (gegen PIT kalibriert) | ✅ QEMU-verifiziert |
| **3** | GDT/TSS + Ring 3 + `syscall`/`sysret` + Context-Switch | ✅ QEMU-verifiziert |
| **4** | **Kernel-Kern läuft**: HAL architekturselektiv, Selbsttests + Scheduler + cap-gesicherte IPC | ✅ QEMU-verifiziert |
| 5 | SMP, isolierte Adressräume (PCID), Ring-3-PDs, Loader, IOMMU | offen (s. `todo.md` C6) |

### Stufe 4 (ext-31): der eigentliche Microkernel

Bis Stufe 3 war der x86-Zweig eine Kette von **Hardware-Demos** — der Kernel-Kern selbst war
auf diesem Branch gar nicht einkompiliert (`sel4lake-*` waren aarch64-only). Seit ext-31 ist
`sel4lake-hal` **architekturselektiv** (`src/aarch64/` und `src/x86_64/` hinter derselben API),
und damit läuft auf x86 derselbe Kern wie auf ARM:

```
memtest : ALL PASS      <- identische arch-neutrale Selbsttests
zerotest: ALL PASS
captest : ALL PASS
budget  : ALL PASS
sched   : 1 Kern, 256 Thread-Slots (512 hostbar), Tabellen 208 KiB aus dem RAM
sched   : Worker-Runden [3, 3, 3]  -> ALL PASS   (LAPIC-Timer verdraengt praeemptiv)
ipc     : CALL(21) ueber Endpoint-Cap -> 42      -> ALL PASS
audit   : sched_audit=0 cdt_audit=0              -> ALL PASS
```

**Der Kernel-Kern enthält kein einziges `cfg(target_arch)`.** Möglich ist das, weil beide
Architekturen dasselbe **Trap-Modell** benutzen: Registersatz in einen `TrapFrame` auf dem
Stack, Handler bekommt dessen Adresse, Rückgabewert ist der wiederherzustellende Frame —
der Kontextwechsel ist auf beiden Seiten ein reiner Stackzeiger-Tausch (`eret` bzw. `iretq`).

Was auf x86 **noch fehlt** (ehrlich als „nicht unterstützt" gemeldet, nicht halb umgesetzt):

- **SMP**: `power::cpu_on` meldet `NOT_SUPPORTED` (INIT-SIPI-SIPI + Realmode-Trampolin fehlen).
- **Isolierte Adressräume**: die `vspace_*`-Funktionen melden `false`; es fehlen PCID-Verwaltung
  und ein per-VSpace-Tabellenpool. Damit gibt es auf x86 (noch) keine Ring-3-PDs.
- **IOMMU**: statt SMMUv3 greift der `NullIommuEnforcer` — er setzt **keine** Hardware-Isolation
  durch und sagt das auch (`is_active() == false`). Die softwareseitigen DMA-Garantien
  (Ownership, Bounds, Revoke-Reihenfolge, Audits) sind davon unberührt.
- **Boot-Archiv/Loader**: `SYS_LOAD` schlägt sauber fehl (das ARM-Fenster hat auf x86 kein
  Gegenstück; die Entsprechung wären Multiboot-Module).
- **RAM-Größe**: fest 512 MiB statt aus der Multiboot-Info (das Trampolin reicht `EBX` nicht durch).

## Geänderte/neue Dateien (ggü. `master`)

- `kernel/src/arch/x86_64/mod.rs` — Boot-Trampolin + 16550-Serial + `x86_rust_entry` (Stufe 0).
- `kernel/x86_64-link.ld` — Multiboot1-Linker (@ 1 MiB, `.boot.bss` außerhalb der genullten `.bss`).
- `build-x86.sh`, `test-qemu-x86.sh` — x86-Build (+ELF32-Cast) & QEMU-Test.
- `.cargo/config.toml` — `[target.x86_64-unknown-none]` (Linker-Skript, static).
- `kernel/Cargo.toml` — `sel4lake-*`-Deps aarch64-only (x86 Stufe 0 = nur Boot+Serial).
- `kernel/src/{main,panic}.rs`, `kernel/src/arch/mod.rs` — `cfg(target_arch)`-Trennung.
