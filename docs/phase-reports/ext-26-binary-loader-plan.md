# ext-26 — Generischer Binary-Loader: Implementierungsplan

Status: **Plan** (zur Freigabe). Architektur/Format: siehe [ADR 0011](../adr/0011-binary-loader.md)
(In-Kernel, cap-gegatet; Minimal-ELF64 `PT_LOAD`).

Jede Phase: build → `test-qemu.sh` (alle bestehenden Checks bleiben grün) → Sensitivitätstest →
commit+push. Die bestehende Microkernel-Semantik bleibt unverändert; der Loader *nutzt* sie.

## Architektur-Bausteine

- **`crates/sel4lake-loader` (NEU, 0 `unsafe`):** reine, bounds-geprüfte Parser für (a) das
  **Boot-Archiv** und (b) **Minimal-ELF64** (`ElfImage` = Entry + `PT_LOAD`-Segment-Iterator) +
  **Manifest**. Host-`cargo test` (Unit-Tests gegen gültige + fehlerhafte Eingaben). Keine
  Kernel-/HW-Abhängigkeit → maximal testbar/fuzzbar.
- **Kernel-Glue (`kernel/src/loader.rs`, NEU):** der *privilegierte* Teil — Segmente in
  Region-Runtime-Regionen kopieren, W^X mappen, VSpace/PD anlegen, Caps via `install_cap_checked`
  endowen, Entry-Thread spawnen. Nutzt ausschließlich bestehende `system::`-Primitive.
- **`crates/libsel4lake` (NEU, Userspace-SDK):** `_start`/crt0, Syscall-Stubs (die `invoke`-ABI),
  Panic-Handler, Boot-Info-Zugriff. Gemeinsame Abhängigkeit der externen Programme (keine
  *gegenseitige* Abhängigkeit zwischen Diensten).
- **`tools/mkarchive` (NEU, Host-Tool, kein Kernelcode):** assembliert `boot-archive.bin` aus den
  extern gebauten Programm-ELFs + Manifesten.
- **Externer Target/Linker für Programme:** `targets/aarch64-sel4lake-user.json` +
  `programs/user.ld` (ET_EXEC, fixe Lade-VA, `PT_LOAD`, no_std, panic=abort).

## Phasen

### L0 — Boot-Delivery + Archiv-Leser + RAM-Reservierung
- `MOD_BASE`/`MOD_WINDOW` (oben in RAM); `init_mem` gibt dem Allokator nur `[free_base, MOD_BASE)`.
- `sel4lake-loader`: Archiv-Header/Entry-Parser (bounds-geprüft) + Unit-Tests.
- Kernel liest das Archiv (Telemetrie: Anzahl/Names). `test-qemu.sh`: `-device loader,
  file=build/boot-archive.bin,addr=MOD_BASE` (+ leeres Dummy-Archiv im Build).
- **Test `archive`:** Kernel findet N Einträge, Magic/Version korrekt. **Sensitivität:** Bad-Magic/
  Out-of-Window-Offset → abgelehnt, Kernel läuft weiter. Allokator-Fenster nie vergeben.

### L1 — Minimal-ELF64-Parser + EL0-isoliertes Laden (erstes externes Programm)
- `sel4lake-loader::parse_elf` (Header + `PT_LOAD`, vollständig bounds-geprüft) + Unit-Tests.
- `kernel/src/loader.rs::load_isolated(bytes, domain)`: VSpace anlegen, je Segment Region
  allozieren, `filesz` kopieren, `.bss` nullen, an `p_vaddr` W^X mappen (X→RX/I-Cache-Sync, W→RW,
  sonst RO), EL0-Thread am Entry spawnen.
- `libsel4lake` + `programs/userland/hello` (minimal: SIGNAL + PARK), Build → Archiv.
- **Test `load`:** externes UserLand-Programm aus dem Archiv läuft + signalisiert (Badge beobachtet).
  **Sensitivität:** abgeschnittenes ELF / Memsz<Filesz / Bad-Entry → abgelehnt.

### L2 — `Loader`-Cap + `SYS_LOAD` + Manifest + Cap-Endowment
- `ObjectKind::Loader` + Install; `SYS_LOAD` (Syscall 13) + Dispatch: verlangt `Loader`-Cap,
  lädt Archiv-Eintrag, endowt Caps laut Manifest (Delegation aus Aufrufer-Caps via `copy/mint` +
  `install_cap_checked`), spawnt, gibt `PdControl`-Cap zurück. Fehlercodes.
- **Test `sysload`:** ein TrustedSAS-Setup-Thread mit `Loader`-Cap lädt ein Programm, das über eine
  endowte Endpoint-Cap zurück-IPCt. **Sensitivität:** ohne `Loader`-Cap → `ERR_BADCAP`; Manifest
  fordert Cap, die der Aufrufer nicht hält → nicht endowt; HW-Cap-Manifest in UserLand → von
  `install_cap_checked` abgelehnt (Domänen-Policy).

### L3 — HardwareLand laden + TrustedSAS-EL1 (signatur-gegatet)
- HardwareLand-Programm (kleines MMIO/DMA-Backend) laden; Kanal vom Autorisierer verdrahtet.
- TrustedSAS-EL1: fixe-VA-Slot-Konvention + `verify_image`-Hook (EL1 ohne gültige Signatur →
  abgelehnt; Test-Signatur-Stub erlaubt ein designiertes Image). Trust-Vorbehalt dokumentiert.
- **Tests `loadhw`, `loadtrusted`:** geladenes HardwareLand-Backend bedient seinen Kanal; EL1-Laden
  nur mit verify-Stub. **Sensitivität:** unsigniertes EL1-Image → abgelehnt.

### L4 — Lebenszyklus: Stop / Hot-Reload / mehrere Prozesse
- Mehrere Programme gleichzeitig; eines via `PdControl` stoppen (Balance); eines aus einer
  „v2"-Archiv-Version per `reload_swap` hot-reloaden (Endpoint/Zustand überlebt).
- **Test `loadlife`:** 3 Programme parallel, Stop (Region-Balance), Reload (gleicher Endpoint).

### L5 — Audits + Fuzzer
- `loader_audit()`: geladene Images konsistent (Segmente disjunkt, in PD-Regionen, W^X korrekt,
  Domäne==Manifest, keine Überlappung mit Kernel-Image/anderen PDs). In `ipc_audit` aggregiert
  (Code 50+).
- `loaderfuzz`: fehlerhafte ELF/Archiv-Varianten (abgeschnitten, Bad-Offsets, Memsz<Filesz,
  überlappende/riesige Segmente, Bad-Entry, W^X-Manifest-Verletzung, Bad-Magic) → alle **abgelehnt**,
  kein Panic/OOB, Allokator balanciert. (Crate-Unit-Fuzz + In-Kernel-Epochen.)
- **Tests `loaderaudit`, `loaderfuzz`.**

### L6 — Projektstruktur + SDK + Doku
- `libsel4lake`-SDK finalisieren; `programs/{trusted,hardware,userland}/` Beispiel-Dienste;
  Top-Level-Build-Skript (alle externen Programme → Archiv).
- Doku: ADR 0011 (fertig), dieser Plan, Phasenbericht, per-Programm-READMEs, Memory.

## Danach (separate Ausbaustufe ext-27)
Die **aggressive Blackbox-/Greybox-Testumgebung** (sechs sich gegenseitig angreifende Dienste,
`tests/*-test-N/`) wird **auf** dem Loader aufgebaut — die EL0-Angriffe über die Syscall-ABI, die
Cap-/CDT-Angriffe in der bestehenden In-Kernel-Selftest-Suite (s. ADR-0011-Abgrenzung).

## Kritische Dateien
- NEU: `crates/sel4lake-loader/`, `crates/libsel4lake/`, `kernel/src/loader.rs`, `tools/mkarchive`,
  `targets/aarch64-sel4lake-user.json`, `programs/user.ld`, `programs/**`, `tests/**`.
- GEÄNDERT (Kernel): `kernel/src/main.rs` (init_mem-Fenster, Archiv lesen, Loader-Setup),
  `kernel/src/system.rs` (Loader-Glue-Aufrufe, `SYS_LOAD`-Hook, `loader_audit`-Aggregation),
  `crates/sel4lake-cap/src/object.rs` (`ObjectKind::Loader`), `crates/sel4lake-cap/src/space.rs`
  (`install_loader`), `crates/sel4lake-abi/src/lib.rs` (`SYS_LOAD`), `crates/sel4lake-microkit`
  (Dispatch `SYS_LOAD` + Domänen-Check), `kernel/src/threads.rs` (Tests), `test-qemu.sh` (`-device
  loader` + neue Checks), Workspace-`Cargo.toml` (neue Crates).

## Verifikation
Pro Phase grün (bestehende Checks + neuer `<name>: ALL PASS`), Sensitivitäts-Break/Restore, Commit.
Host-Last-Flakiness bekannt (TCG-Starvation). Der Parser zusätzlich per Host-`cargo test` verifiziert.
