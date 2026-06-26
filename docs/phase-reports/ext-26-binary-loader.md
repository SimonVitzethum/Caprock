# ext-26 — Generischer Binary-Loader (Phasen L0–L1)

Status: **L0 + L1 fertig** (Boot-Delivery, ELF-Parser, externe Toolchain, EL0-Laden). Suite grün
(51 Checks). Architektur/Format: [ADR 0011](../adr/0011-binary-loader.md). Plan:
[ext-26-binary-loader-plan.md](ext-26-binary-loader-plan.md).

## Ziel

Übergang von **eingebetteten** Prozessen (alles im Kernel-Image, `.user_text`) zu **extern
gebauten, geladenen** Prozessen: ein generischer In-Kernel-Loader (cap-gegatet) lädt statisch
gelinkte EL0-ELF64-Binaries aus einer austauschbaren Quelle in isolierte PDs. Die bestehende
Sicherheitsarchitektur (Capabilities, Domänen, Region-Runtime, Audits, W^X) bleibt erhalten.

## Phasen

- **L0 — Boot-Delivery + Archiv.** `crates/sel4lake-loader` (`#![forbid(unsafe_code)]`, host-
  getestet): bounds-geprüfter Boot-Archiv-Parser. QEMU `-device loader` legt das Archiv in ein
  oben in RAM **reserviertes 16-MiB-Fenster** (`MOD_BASE`, vom `PhysAllocator` ausgenommen); der
  Kernel liest es (`kernel/src/loader.rs`, ein begründetes `unsafe` für den Read-only-Slice).
  Host-Tool `tools/mkarchive.py`.
- **L1a — Minimal-ELF64-Parser.** `sel4lake-loader::elf`: nur `ET_EXEC`/AArch64-Header + `PT_LOAD`-
  Segmente, vollständig bounds-geprüft, panik-frei. 9 Host-Unit-Tests. **Kein** Dynamic-Linking/
  Relokationen.
- **L1b — Externe Programm-Toolchain.** `programs/` = **eigener** Cargo-Workspace (eigene
  Target-Spec + Linker `user.ld`, festgelinkt an VA `0x4100_0000`, getrennte W^X-`PT_LOAD`-
  Segmente). SDK `libsel4lake` (Syscall-Stubs + Panik-Handler). Erstes Programm `hello`.
- **L1c — EL0-isoliertes Laden.** Neues HAL-Primitiv `vspace_map_page_at(va→pa)` (nicht-identity:
  Programme an fester VA gelinkt, an **beliebige** Phys geladen). `system::load_elf`: Segmente in
  RAM-Frames kopieren (die **einzige** `unsafe`-Stelle), **W^X** an die Link-VA mappen, Stack,
  PD in der Manifest-Domäne, Cap-Endowment über `install_cap_checked` (Domänen-Policy bleibt
  gültig), EL0-Spawn. Öffentliche API `loader::load_image(&Program)` (klein, quellen-agnostisch).
  Test `load`: das **extern gebaute** `hello` wird geladen + ausgeführt und signalisiert eine
  endowte Notification (`HELLO_BADGE`) — Beweis, dass Code **außerhalb** des Kernel-Images läuft.

## Nutzer-Review-Verfeinerungen (eingebaut)

1. Loader als eigener Dienst, **kleine** API (`load_image`). 2. ELF-Parse **vollständig in Safe
Rust**, `unsafe` erst beim Segment-Kopieren. 3. **Quelle austauschbar** — die API spricht den
quellen-agnostischen `Program`-Deskriptor, nicht das Archivformat (FS/Flash/Netz später ohne
API-Bruch). 4. Stabile **`program_id`** + `version` je Eintrag.

## Wichtiger Nebenbefund: vorbestehender SMP-Deadlock gefixt

Beim Aufbau der Testumgebung fiel ein **intermittierender Hang** (~27 %/Lauf) auf, der über die
gesamte SMP-/MCS-Ära als „Host-Last-Flakiness" fehlgedeutet war. QEMU-Monitor-Forensik (CPU-Dump
aller Kerne + `addr2line`) + Reproduktion auf ext-25 zeigten: **reentranter Ticket-SpinLock-
Deadlock** — der Timer-Tick (Reschedule) zog ein zweites Ticket auf `SCHEDS`/`NTFNS`, während
Thread-/Idle-Kontext (z. B. `idle→reap_core`) den Lock hielt. Fix: **IRQ-sichere SpinLocks** (DAIF
beim Locken maskieren, beim Drop restaurieren). Verifiziert per `tools/hang-stress.sh`: 30/30
deadlock-frei (vorher 4/15). Siehe `docs/invariants.md` §1a.

## Verifikation

`./test-qemu.sh` → **51/51 ALL PASS** (50 Bestand + neuer `load`). Crate-Parser per Host-`cargo
test` (17 Tests). Stabilität per `tools/hang-stress.sh`.

## Offen (Folgephasen)

L2 (`SYS_LOAD` + `Loader`-Cap + Manifest-getriebenes Cap-Endowment), L3 (HardwareLand + TrustedSAS/
EL1 signatur-gegatet), L4 (Stop/Hot-Reload/Teardown geladener Segmente), L5 (`loader_audit` +
Fuzzer), L6 (Projektstruktur). Danach ext-27: aggressive Testdienste auf dem Loader.
