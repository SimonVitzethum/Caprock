# ADR 0013 — Fuzzer hinter optionalem Cargo-Feature `kernel-fuzz`

Status: **angenommen** · Datum: 2026-06-27 · Kontext: Vorbereitung des Langzeittests

## Kontext

Der Selbsttest-Harness (`kernel/src/threads.rs`) enthält vier **In-Kernel-Fuzzer** — aktive
Testfall-Generatoren, die zur Verifikation, **nicht** für den Produktivbetrieb dienen:

- `fuzz` — generativer Kernel-Fuzzer (zufällige Cap/Thread/VSpace-Op-Sequenzen, Phase 1 + SMP).
- `ipcfuzz` — IPC-State-Machine-Fuzzer (nebenläufige Aktoren + KILL/Reload/MCS während IPC).
- `hwfuzz` — Domänen-/HW-/Management-Cap-Churn gegen die Policy.
- `loaderfuzz` — fehlerhafte ELF-Varianten durch den Binary-Loader-Parser.

Vor dem Langzeittest soll der **Release-Kernel keinerlei Fuzzer-Code** mehr enthalten: die Fuzzer
sind Entwicklungs-/Verifikationswerkzeuge und werden als **optionale Build-Module** behandelt. Der
Langzeittest läuft bewusst auf **derselben** Konfiguration, die produktiv eingesetzt wird (ohne
Fuzzer).

**Maßgebliche Befunde (Analyse):** Alle vier Fuzzer liegen vollständig in `threads.rs`; ihre
Statics werden **nur** dort referenziert. **Kein** `system.rs`-Helfer ist fuzzer-exklusiv — sämtliche
Cap-/PD-/DMA-Operationen und **alle Audit-Orakel** (`domain_audit`, `cap_audit_cdt`, `vspace_audit`,
`dma_audit`, `loader_audit`, `ipc_audit`) sind **geteilte Infrastruktur** und bleiben unverändert
im Kernel. Es gibt genau **zwei** Stellen, an denen ein **Nicht-Fuzzer**-Test auf das `DONE`-Flag
eines Fuzzers gatet (Ketten-Reconnect-Punkte).

## Entscheidung

### 1. Cap-Feature statt Code-Entfernung
Neues Cargo-Feature `kernel-fuzz` in `kernel/Cargo.toml`, **nicht** in `default`. Der normale
Release-Build (`cargo build --release`) kompiliert **ohne** das Feature → **kein** Fuzzer-Code im
Binary. Der Verifikations-Build (`--features kernel-fuzz`) enthält die Fuzzer mit **exakt** der
bisherigen Funktionalität.

### 2. Fuzzer in ein eigenes, feature-gegatetes Modul
`threads.rs` → `threads/mod.rs`; die vier Fuzzer (Treiber-Funktionen, Statics, Konstanten,
Hilfs-Structs, die Idle-Manager-Treiberschritte) wandern in das **Kindmodul**
`kernel/src/threads/fuzz.rs`, deklariert als `#[cfg(feature = "kernel-fuzz")] mod fuzz;`. Das
Kindmodul nutzt `use super::*` und erreicht damit **alle** (auch privaten) Harness-Items —
**ohne** die Sichtbarkeit (`pub`) von Kernel-Internals zu ändern (Vorgabe „Kernel-APIs möglichst
unverändert"). Ein separater Krate würde umfangreiche `pub`-Öffnungen erzwingen und wurde deshalb
verworfen; das Kindmodul ist die kleinst-invasive „gleichwertige Struktur".

### 3. Kleine Modul-API + Stub für den Release-Build
`fuzz` exportiert (nur `pub(super)`): `drive()` (führt je Idle-Tick den fälligen Fuzzer-Schritt
aus), `all_passed()` (für `all_done()`), `report()` (Druckzeilen), die zwei Ketten-Gates
`loaderfuzz_gate(pred)` / `fuzzers_gate(pred)` und vier `dbg_*()`-Bools. Für den Release-Build
(`#[cfg(not(feature = "kernel-fuzz"))]`) stellt ein **Stub-Modul** dieselbe API als No-Op bereit:
`drive()`/`report()` = leer, `all_passed()` = `true`, die Gates geben ihren **Vorgänger** durch
(`loaderfuzz_gate(pred)=pred` etc.), `dbg_*()` = `true`. So fließt die Idle-Manager-Kette
transparent durch, wenn die Fuzzer fehlen — **ohne** Sonderfälle im Harness.

### 4. Audits bleiben Kernel-Bestandteil
Ausdrückliche Trennung: **Fuzzer** (aktive Testfall-Erzeugung) → optionales Modul. **Audits**
(Prüfung der Kernel-Invarianten) → bleiben **immer** im Kernel, feature-unabhängig. Die Fuzzer
**rufen** Audits; die Audit-Funktionen selbst werden nie gegatet.

## Validierung

- `cargo build --release --features kernel-fuzz` + `KERNEL_FUZZ=1 ./test-qemu.sh` → **alle** Fuzzer
  laufen wie bisher (`fuzz`/`ipcfuzz`/`hwfuzz`/`loaderfuzz : ALL PASS`), 62/62.
- `cargo build --release` (ohne Feature) + `./test-qemu.sh` → **kein** Fuzzer-Code; die Nicht-Fuzzer-
  Tests + **alle Audits** bleiben grün (58/58); die vier Fuzzer-Checks entfallen.
- `hang-stress.sh` deadlock-frei in beiden Konfigurationen (Default = Release ohne Fuzzer).

## Konsequenzen

- Der Release-Kernel ist schlanker und entspricht exakt der Langzeittest-/Produktiv-Konfiguration.
- Fuzzer bleiben als Entwicklungswerkzeug mit unveränderter Funktionalität erhalten (Feature-Build).
- Kein Kernel-API-Bruch; die Audits sind klar von den Fuzzern getrennt.
- Muster für künftige reine Verifikationswerkzeuge: optionales feature-gegatetes Modul + Stub.
