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
| **4b** | **SMP** (INIT-SIPI-SIPI), ACPI-MADT/MCFG, Multiboot-Speicherplan | ✅ QEMU-verifiziert (4 Kerne) |
| **4c** | **Ring 3**: User-Threads mit Syscall + Fault-Isolation (SAS-Modell) | ✅ QEMU-verifiziert |
| **5** | **Per-Prozess-Adressräume**: isolierte PDs mit eigenem Adressraum | ✅ QEMU-verifiziert |
| 6 | PCI-Enumeration, IOMMU (VT-d), PCID-Optimierung | offen (s. `todo.md` C6) |

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

### Stufe 4b (ext-32): SMP + Plattformbeschreibung

```
acpi    : 4 CPU(s) laut MADT
acpi    : PCI-ECAM @ 0xb0000000, Busse 0..255
mbi     : Speicherplan gelesen -> RAM bis 0x1ffe0000 (511 MiB)
smp     : 4 von 4 Kern(en) online
sched   : core 0..3 ticks=24/24/23/22   -> jeder Kern hat LAPIC-Timer + Scheduler-Instanz
```

Drei Dinge, die auf ARM anders (und einfacher) sind:

- **Kernstart.** PSCI startet einen Kern direkt in 64 Bit an beliebiger Adresse. Auf x86 gibt es
  keine solche Firmware-Schnittstelle: der AP startet nach `INIT`-`SIPI`-`SIPI` im
  **16-bit-Real-Mode unterhalb von 1 MiB**. Das Trampolin (`hal::x86_64::power`) macht
  16 → 32 → 64 Bit und springt dann in den Rust-Einstieg.
- **Wo das Trampolin liegt.** Laufadresse ist 0x8000 (damit alle Labels darin absolut sind),
  **Ladeadresse** aber im Kernel-Image: ein eigenes Ladesegment unter 1 MiB sortiert die
  ELF-Segmente nach Adresse und schiebt den Multiboot-Header aus den ersten 8 KiB der Datei —
  dann bootet QEMU kommentarlos gar nicht. Der BSP kopiert den Block einmalig.
- **W^X über den Moduswechsel.** Der AP führt auf 0x8000 weiter, **nachdem** er `CR0.PG`
  gesetzt hat; wäre die Seite dann NX, gäbe es genau in diesem Moment einen #PF ohne IDT und
  damit einen Triple Fault. Die Seite ist beim Kopieren RW und wird danach per
  `mmu::protect_page` auf R-X gestellt; die Parameter liegen auf einer **eigenen** RW-Seite,
  damit nie eine Seite zugleich schreibbar und ausführbar ist.

**Plattformbeschreibung** kommt jetzt von der Plattform statt aus dem Code: CPU-Liste aus der
**ACPI-MADT** (Gegenstück zu den `/cpus`-Knoten des DTB), PCI-ECAM-Fenster aus der **MCFG**,
RAM-Größe aus dem **Multiboot-Speicherplan**.

### Stufe 4c (ext-32): Ring 3

```
ring3   : Ring-3-Thread machte 5 Syscalls; abgefangene Ring-3-Faults: 1
el0-trap: User-Thread faultete (FAR=0x100000) -> beendet, Kernel laeuft weiter
```

Ein Ring-3-Thread arbeitet per `int 0x80` mit dem Kernel; ein zweiter liest **Kernel**-Speicher
und wird dafür beendet, ohne den Kernel mitzureißen — das x86-Gegenstück zum `el0iso`-Test.
Drei Dinge waren dafür nötig:

- **`US` auf allen vier Ebenen.** Auf x86 ist die effektive Berechtigung die UND-Verknüpfung
  über PML4E/PDPTE/PDE/PTE. Ohne `US` in den Zwischenebenen verweigert die CPU jeden
  Ring-3-Zugriff, egal was im Blatt steht. Die Zwischenebenen sind deshalb permissiv, die
  Entscheidung fällt am Blatt — genau wie `AP[1]` auf aarch64.
- **`TSS.RSP0` je Thread.** Auf ARM hat jede Ausnahmestufe ihr eigenes Stackregister; auf x86
  schaltet die CPU beim Trap aus Ring 3 auf `TSS.RSP0` um. Der wird jetzt bei jeder Rückkehr
  nach Ring 3 auf den Kernel-Stack **genau dieses** Threads gesetzt.
- **Ring-3-Code + -Daten in eigenen Sektionen** (`.user_text`/`.user_data`), weil der
  Kernel-`.text` supervisor-only bleibt. Ein Ring-3-Thread kann deshalb keine Kernel-Funktion
  aufrufen — sein Syscall ist direkt eingebettet, wie bei den EL0-Demos auf aarch64.

### Stufe 5 (ext-33): Per-Prozess-Adressräume

```
iso     : SAS-Thread las 0x5e141a4e0bedc0de; isolierter Thread faultete an DERSELBEN Adresse
```

Aufbau eines isolierten Adressraums — dasselbe Modell wie auf ARM (**der Kernel sieht alles,
der User nichts**, und in diese Grundfläche werden die Frames der PD „hineingestanzt"), nur mit
einer Ebene mehr:

```
  PML4 (l1) ──[0]──> PDPT ──[0]──> PD (l2)   GiB 0: Kernel-Image (GETEILTE PTs, wie die
                          │                         Kernel-L3 auf ARM) + RAM supervisor-only
                          │                         + User-Blöcke dieser PD (US)
                          └─[1..3]─> ISO_PD_HIGH    GiB 1..3 supervisor-only (statisch, geteilt)
```

x86 hat gegenüber ARM (39-Bit-VA, drei Ebenen) eine Tabellenebene mehr; `vspace_create_base`
bekommt deshalb einen `alloc`-Rückkanal für die PDPT (auf ARM ungenutzt). Die statischen
`ISO_PD_HIGH`-Tabellen für GiB 1..3 sparen drei Frames **je** Adressraum: sie enthalten nichts
PD-Spezifisches, nur „RAM, aber nur für den Kernel".

Was auf x86 **noch fehlt** (ehrlich als „nicht unterstützt" gemeldet, nicht halb umgesetzt):

- **Per-Prozess-Adressräume**: die `vspace_*`-Funktionen melden weiterhin `false` — es fehlen
  PCID-Verwaltung und ein per-VSpace-Tabellenpool. Ring-3-Threads laufen deshalb im **SAS-Modell**
  (gemeinsamer Adressraum, Isolation gegen den Kernel per `US`-Bit) — dasselbe, was auf aarch64
  für *trusted* PDs gilt. Isolierte PDs (jede mit eigenem Adressraum) gibt es auf x86 noch nicht.
- **IOMMU**: statt SMMUv3 greift der `NullIommuEnforcer` — er setzt **keine** Hardware-Isolation
  durch und sagt das auch (`is_active() == false`). Die softwareseitigen DMA-Garantien
  (Ownership, Bounds, Revoke-Reihenfolge, Audits) sind davon unberührt.
- **Boot-Archiv/Loader**: `SYS_LOAD` schlägt sauber fehl (das ARM-Fenster hat auf x86 kein
  Gegenstück; die Entsprechung wären Multiboot-Module).
- **PCI-Enumeration**: das ECAM-Fenster wird gefunden und gemeldet, aber noch nicht durchsucht
  (der Kernel-PCIe-Pfad ist derzeit an das ARM-`virt`-Board gebunden).

## Geänderte/neue Dateien (ggü. `master`)

- `kernel/src/arch/x86_64/mod.rs` — Boot-Trampolin + 16550-Serial + `x86_rust_entry` (Stufe 0).
- `kernel/x86_64-link.ld` — Multiboot1-Linker (@ 1 MiB, `.boot.bss` außerhalb der genullten `.bss`).
- `build-x86.sh`, `test-qemu-x86.sh` — x86-Build (+ELF32-Cast) & QEMU-Test.
- `.cargo/config.toml` — `[target.x86_64-unknown-none]` (Linker-Skript, static).
- `kernel/Cargo.toml` — `sel4lake-*`-Deps aarch64-only (x86 Stufe 0 = nur Boot+Serial).
- `kernel/src/{main,panic}.rs`, `kernel/src/arch/mod.rs` — `cfg(target_arch)`-Trennung.
