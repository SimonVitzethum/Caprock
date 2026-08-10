# ADR 0001 — Toolchain, Target und Build

**Status:** akzeptiert (Phase 0) · **Datum:** 2026-06-23

## Motivation

Ein Bare-Metal-aarch64-Kernel in Rust braucht: eine `no_std`-Umgebung, eine
Cross-Compilation-Target-Definition, eine reproduzierbare Toolchain und einen
Boot-Pfad, der ohne Bootloader-SDK direkt unter QEMU lädt.

## Analysierte Alternativen

### A) Eigener Kernel via `qemu -kernel` (gewählt)
Eigene Boot-Assembly, eigenes Linker-Script, Laden des ELF direkt durch QEMU.

- **+** Voll unabhängig von der seL4-/Microkit-SDK; Caprock ist ein
  eigenständiger Kernel, kein PD auf fremdem Kernel.
- **+** Minimale bewegliche Teile, schneller Boot, einfache Tests.
- **−** Wir müssen Trap-Vektoren, Timer, GIC, MMU selbst aufsetzen (ohnehin Ziel).

### B) Microkit-Loader wiederverwenden (wie `microkit_rust`)
Die vorhandene `microkit_rust`-Implementierung baut Userland-PDs *auf* dem
seL4-Kernel mittels Microkit-SDK und `loader.img`.

- **+** Vorhandene Build-Mechanik (build-std, custom Target) ist erprobt.
- **−** Setzt einen darunterliegenden seL4-Kernel voraus — widerspricht dem Ziel
  eines eigenständigen Kernels. **Verworfen** als Kernelbasis (aber wertvolle
  Referenz für `no_std`/IPC/Build-Konventionen).

### C) U-Boot / EFI-Boot
- **−** Unnötige Komplexität für die QEMU-`virt`-Zielumgebung. Verworfen.

## Entscheidung

- **Toolchain:** rustup `nightly` (hier 1.98.0), gepinnt via `rust-toolchain.toml`
  mit den Komponenten `rust-src`, `rustfmt`, `llvm-tools`. Nightly ist nötig für
  `-Z build-std` (es gibt kein vorkompiliertes `core`/`alloc` für ein
  freistehendes Target). `/usr/bin/cargo` ist auf diesem System ein
  rustup-Proxy, daher greift das Pinning automatisch.
- **Target:** custom JSON-Spec `targets/aarch64-caprock.json`
  (`llvm-target: aarch64-unknown-none`, `panic-strategy: abort`,
  `relocation-model: static`, `+strict-align`, `rust-lld` als Linker). Neuere
  Nightlies verlangen zusätzlich `-Z json-target-spec` (in `.cargo/config.toml`).
- **build-std:** `core, alloc, compiler_builtins` mit
  `compiler-builtins-mem` (liefert `memcpy`/`memset` etc. ohne libc).
- **Linker-Script:** `kernel/linker.ld`, Ladeadresse `0x4008_0000` (QEMU-`virt`-
  Konvention; RAM ab `0x4000_0000`). Drei LOAD-Segmente (RX/R/RW), `.bss` als
  `NOLOAD`, 64 KiB Boot-Stack im `.bss`-Bereich.
- **FP/NEON:** bleiben aktiviert. rustc koppelt `fp-armv8` an `neon` an die ABI;
  das Deaktivieren wird ausgemustert. Der Kernel verwendet selbst kein
  Fließkomma; FP/SIMD-Zustand wird erst beim Kontextwechsel (lazy) behandelt.

## Sicherheitsauswirkungen

`panic = abort` (kein Unwinding → keine Landing-Pads, kleinere Angriffsfläche).
`relocation-model = static` (keine GOT/PLT-Indirektion zur Laufzeit).
`+strict-align` ist nötig, solange die MMU aus ist (alle Zugriffe sind
Device-typisiert und müssen ausgerichtet sein) — siehe [ADR 0002](0002-no-virtual-memory-sas.md).

## Performanceauswirkungen

`build-std` erlaubt später, `core` mit denselben CPU-Features/Opt-Level wie den
Kernel zu bauen (z. B. spezifische `target-cpu`). LTO ist als `thin` aktiviert.
Solange die MMU aus ist (nur Phase 0), läuft alles uncached und damit langsam —
in Phase 1 wird eine Identity-Map mit aktivierten Caches eingeführt.

## Bauen & Starten

```sh
./build.sh        # rustup run nightly cargo build --release
./run-qemu.sh     # qemu-system-aarch64 -machine virt -cpu cortex-a72 -smp 8 -m 4G ...
```

Artefakt: `build/target/aarch64-caprock/release/caprock-kernel.elf`.
