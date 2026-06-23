# SEL4Lake

Ein eigenständiger, capability-basierter Microkernel in **Rust**, inspiriert von
**seL4** und der **seL4-Microkit**-Laufzeit — mit einem bewusst anderen
Speichermodell: **Single-Address-Space, keine per-Prozess-MMU-Isolation**.
Speichersicherheit entsteht aus Rust (intralingual) + Capabilities (Autorität).

Ziele: hochsicher · hochperformant · capability-basiert · deterministisch ·
vollständig modular.

## Schnellstart

```sh
./build.sh      # baut das Kernelimage (aarch64, no_std, build-std)
./run-qemu.sh   # bootet in QEMU: ARM virt, 8 Kerne, 4 GiB (Beenden: Ctrl-A X)
./test-qemu.sh  # automatisierter Boot-/SMP-/Timer-Test (PASS/FAIL)
```

Voraussetzungen: `rustup` mit `nightly` (+ `rust-src`), `qemu-system-aarch64`.
Die Toolchain ist über `rust-toolchain.toml` gepinnt.

## Status

**Phasen 0–7 abgeschlossen** (geplante Roadmap komplett, `./test-qemu.sh` = ALL
PASS). Zuletzt **Phase 7 (Hot-Reload)**: eine Server-Komponente wird im laufenden
System ersetzt (v1 verdoppelt → v2 verdreifacht) über *dieselbe* Endpoint-Cap,
ohne Kernel-Neustart und transparent für den Client. Darunter: 8 Kerne mit
MMU/W^X, capability-basierter Allokator, Capability-System mit Derivation-Tree,
präemptiver Scheduler, cap-gesicherte IPC, Protection Domains. **Ausbaustufen darüber hinaus:** FP/SIMD-Kontextsicherung,
Bitmap-Prioritäten ([Ausbaustufe 1](docs/phase-reports/ext-1-scheduler-context.md))
sowie Thread-Lebenszyklus (EXIT/KILL, Stack-Rückgewinnung) und TCBs als
Capabilities ([Ausbaustufe 2](docs/phase-reports/ext-2-thread-lifecycle.md)).
Test deckt MMU, memtest, captest, sched, **fp**, **prio**, **life**, ipc,
**reload** und 8/8 Kerne ab. Berichte:
[7](docs/phase-reports/phase-7.md) · [6](docs/phase-reports/phase-6.md) ·
[5](docs/phase-reports/phase-5.md) · [4](docs/phase-reports/phase-4.md) ·
[3](docs/phase-reports/phase-3.md) · [2](docs/phase-reports/phase-2.md) ·
[1](docs/phase-reports/phase-1.md) · [0](docs/phase-reports/phase-0.md).

## Dokumentation

- [`docs/00-overview.md`](docs/00-overview.md) — Gesamtüberblick, Crate-Layout,
  Phasen-Roadmap, Vertrauensmodell.
- [`docs/adr/`](docs/adr/) — Architekturentscheidungen (Toolchain, SAS/kein-VM,
  Capabilities, IPC, Scheduler, Hot-Reload).
- [`docs/seL4-architecture-map.md`](docs/seL4-architecture-map.md) — Analyse des
  seL4-Quellcodes als Referenz.

## Projektgrenzen

Im Kernelimage liegen nur: Microkernel, Capability-System, Scheduler, IPC und die
Microkit-Runtime. Treiber, Netzwerkstack, Dateisysteme, Dienste usw. laufen als
**hot-reloadbare** Userland-Komponenten außerhalb des Kernels.
