# Fuzzer hinter optionalem Feature `kernel-fuzz`

Status: **fertig**. Entscheidung/Architektur: [ADR 0013](../adr/0013-fuzzers-behind-feature.md).
Vorbereitung des Langzeittests: der Release-Kernel enthält **keinerlei** Fuzzer-Code mehr; die
Fuzzer sind ein optionales Verifikationsmodul. Die **Audits bleiben** Kernel-Bestandteil.

## Analyse (Ausgangslage)

Vier In-Kernel-Fuzzer, **alle** in `kernel/src/threads.rs`:

| Fuzzer | Zweck | Kette (Vorgänger → Nachfolger) |
|---|---|---|
| `loaderfuzz` | fehlerhafte ELFs durch den Loader-Parser | loadstop → (aggru) |
| `hwfuzz` | HW-/Management-Cap-Churn gegen die Policy | cross → (fuzz) |
| `fuzz` | generative Cap/Thread/VSpace-Op-Sequenzen | stale/strand/hwfuzz → (ipcfuzz) |
| `ipcfuzz` | nebenläufige IPC-State-Machine | fuzz → (caplk) |

**Kein** `system.rs`-Helfer ist fuzzer-exklusiv: sämtliche Cap-/PD-/DMA-Operationen und **alle**
Audit-Orakel sind geteilte Infrastruktur und bleiben unverändert. Genau **zwei** Nicht-Fuzzer-Tests
gaten auf ein Fuzzer-`DONE`-Flag: `aggru` (ext-27 T0) auf `loaderfuzz`, `caplk` auf `fuzz && ipcfuzz`.

## Umsetzung

1. **Feature** `kernel-fuzz` in `kernel/Cargo.toml` (nicht in `default`).
2. **Modul-Split:** `threads.rs` → `threads/mod.rs`; alle Fuzzer-Funktionen, -Statics, -Konstanten,
   die generischen Fuzzer-Helfer (`frand`/`frand_rights`/`pick_live`/`free_slot`) und die Idle-
   Manager-Treiberschritte wandern in das Kindmodul **`threads/fuzz.rs`** (`#[cfg(feature =
   "kernel-fuzz")] mod fuzz;`). Über `use super::*` erreicht es den gesamten Harness, **ohne**
   Kernel-Sichtbarkeiten zu ändern.
3. **Stub:** `#[cfg(not(feature = "kernel-fuzz"))] mod fuzz { … }` mit identischer API als No-Op;
   die zwei Ketten-Gates (`loaderfuzz_gate`/`fuzzers_gate`) geben im Stub ihren **Vorgänger** durch
   → die Idle-Manager-Kette fließt ohne die Fuzzer transparent durch.
4. **Harness-Integration** (`threads/mod.rs`): ein `fuzz::drive()` je Idle-Tick statt der vier
   Inline-Schritte; `fuzz::report()`/`fuzz::all_passed()`/`fuzz::dbg_*()` für Report/`all_done`/DBG.
5. **API-Erhalt:** vier nur vom Fuzzer aufgerufene `system.rs`-Funktionen (`cap_used_slots`,
   `cap_used_objects`, `unmap_into_thread`, `free_dma_region`) bleiben als Kernel-API erhalten;
   im Release-Build wird ihre Dead-Code-Warnung per `#[cfg_attr(not(feature="kernel-fuzz"),
   allow(dead_code))]` unterdrückt (kein API-Bruch).
6. **Skripte:** `test-qemu.sh`/`tools/hang-stress.sh` bauen per Default **ohne** Feature (Release/
   Langzeit-Konfig); `KERNEL_FUZZ=1` baut `--features kernel-fuzz`. Die vier Fuzzer-Checks laufen
   nur im Feature-Lauf (`fcheck`), sonst `SKIP`.

## Validierung

| Konfiguration | Build | Selbsttest | Deadlock-Stress |
|---|---|---|---|
| `--features kernel-fuzz` | grün, 17 Warnungen (Bestand) | **62/62 ALL PASS** (alle 4 Fuzzer) | `KERNEL_FUZZ=1 hang-stress` |
| Release (ohne Feature) | grün, 17 Warnungen (Bestand) | **58/58 ALL PASS** + 4 SKIP; **alle Audits grün** | `hang-stress` (Default) |

Beide Builds erzeugen **identische** Warnungszahlen ohne fuzzer-spezifische Warnungen — der
Release-Kernel ist nachweislich fuzzer-frei, der Feature-Build verhält sich exakt wie zuvor.

## Folge

Der Langzeittest läuft auf dem **Release-Kernel ohne Fuzzer** (`./test-qemu.sh` / `tools/hang-stress.sh`
ohne `KERNEL_FUZZ`) — genau die produktiv eingesetzte Kernel-Konfiguration.
