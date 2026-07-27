# Vire auf SEL4Lake — Integration: was nötig wäre, was gut wäre

*Vire ist ein AOT-Compiler (Python-ergonomisch, Rust-schnell, speichersicher via
RC + Zyklen-Collector) mit LLVM/clang-Backend und einer schlanken C-Runtime
(`~/Schreibtisch/FastLLVM`). Dieses Dokument beschreibt aus SEL4Lake-Sicht, was
nötig ist, um Vire-Programme als SEL4Lake-Komponenten laufen zu lassen, und was
darüber hinaus gut wäre. Vire-seitige Analyse: `sprache/SEL4LAKE-PORT-UND-
MICROKERNEL.md` im Vire-Repo.*

## Warum Vire zu SEL4Lake passt (die Kernbeobachtung)
SEL4Lakes Single-Address-Space (ADR 0002) gibt die Performance (Zero-Copy-IPC, keine
TLB-Shootdowns, SP-Tausch-Context-Switch) — **verlangt aber, dass JEDE Komponente
speichersicher ist**, weil es keine per-Prozess-MMU-Isolation gibt. Heute heißt das
„alles in Rust". **Vire erweitert die Menge sicherer Sprachen um eine zweite**:
speichersicher über RC + Collector statt Borrow-Checker, ergonomischer für
Anwendungslogik. Der Kernel liefert die Performance, die Sprache die Sicherheit, die
der Kernel voraussetzt — komplementär. Vire-Komponenten sind AOT (kein JIT/Warmup),
deterministisch, klein (16-B-Objekt-Header + Slab).

## Die Naht ist schon da: die `plat_*`-Schicht
Vires Runtime (`crates/driver/src/runtime.c`) hat unter `FASTLLVM_FREESTANDING`
bereits eine **plattform-abstrakte Schicht** mit dem ausdrücklichen Hinweis
„produktiv ersetzt die Zielumgebung `plat_*` durch ihren eigenen Allokator". Der
ganze Port ist im Kern: **diese wenigen Funktionen auf SEL4Lake abbilden.** Kein
Sprachkern-Umbau, keine Runtime-Neuschreibung.

### Was SEL4Lake bereitstellen muss (die konkrete C-ABI-Naht)
| Vire-Symbol | Semantik | SEL4Lake-Abbildung |
|---|---|---|
| `plat_alloc(size)` → genullter Speicher | Objekt-/Slab-Speicher | aus einer **cap-besessenen `sel4lake-region`** (ADR 0010) bedienen; der Slab instanziiert pro Region |
| `plat_free(ptr)` | Freigabe | in die Region-Freiliste (oder no-op, wenn Region en bloc endet) |
| `plat_write(s, n)` | Ausgabe | **IPC** an einen Konsolen-/Serial-Server (Endpoint-Cap), kein stdio |
| `plat_abort()` / `jrt_platform_halt()` (weak) | Fehler-Halt | Komponente terminieren / Fault an Supervisor |
| `jrt_debug_putchar(c)` (weak) | Debug-Byte | optional Debug-Konsole |

Die Runtime baut `#![no_std]`-artig ohne libc; SEL4Lake stellt diese ~5 Funktionen
als `extern "C"` (Rust) bereit. Das ist der gesamte Pflicht-Teil von Phase A.

## Was nötig ist (Pflicht, Phase A — bootende Komponente)
1. **Target:** Vire baut `vire build --target aarch64-unknown-none <prog>.vr`
   (das `--target`-Flag existiert). Ergebnis: ein natives Objekt/ELF ohne libc.
   (x86 folgt, wenn der SEL4Lake-x86-Branch landet → `x86_64-unknown-none`.)
2. **Laden:** über den generischen **Binary-Loader** (ADR 0011, cap-gegatet) — Vires
   Ausgabe ist ein extern gebautes Objekt, genau die Zielklasse des Loaders.
3. **Speicher:** die `plat_*`-Naht auf `sel4lake-region` legen (s.o.). Der
   16-B-Header + Slab passen ideal zu knappen Regionen.
4. **IO:** `plat_write` → Konsolen-IPC.
5. **Einstieg/Ende:** Vires `main`→`java_main`-Entry aufrufen; **kein `atexit`** (in
   der freestanding-Runtime schon weg) — die Komponente läuft dauerhaft / via
   Supervisor beendet.

## Was gut wäre (Phase B/C — nutzt SEL4Lakes Stärken)
- **Zero-Copy-Objektübergabe (SAS-Bonus):** weil alle Komponenten einen Adressraum
  teilen, kann eine Vire-Objektreferenz **cap-gewährt ohne Kopie** an eine andere
  Komponente gehen — anders als bei seL4. Vires opaker **`Ptr`-Typ = eine Capability**
  (Handle auf Region/Endpoint) ist die natürliche FFI-Grenze.
- **Determinismus für Real-Time-Komponenten:** Vire hat die Hebel schon — **`--no-cycles`**
  (solver-bewiesen azyklisch → reine RC, kein Collector) + **Region-Inferenz** (borgt
  Traversal-Referenzen, kein RC) + **Auto-Arena** (per-Iteration-Bump, en-bloc-frei).
  Damit sind Vire-Komponenten **GC-pausenfrei** und speicher-vorhersagbar — passt zu
  SEL4Lakes Determinismus-Ziel.
- **Nebenläufigkeit:** Vires `--threads`-Pfad (heute pthreads) auf **SEL4Lake-TCBs +
  Notifications/IPC** (Scheduler Phase 4/5) neu implementieren. Monitore → SEL4Lake-
  Primitiven.
- **Hot-Reload (Phase 7):** Vire-Komponenten sind AOT + deterministisch → passen zum
  v1→v2-Austausch über dieselbe Endpoint-Cap. Ein Vire-Server könnte hot-reloadbar
  sein wie die Rust-Server heute.
- **Das GC-Modell im SAS (Designentscheidung):** **nicht** einen geteilten Collector
  über Komponentengrenzen (koppelt alle → pausiert alle). Stattdessen **per-Komponente
  Regionen + eigener Slab/Collector**; über die Grenze nur cap-gewährte `Ptr`-Handles.
  Isolation der GC-Domänen = Isolation der Latenz.

## Integrationsmodell (Empfehlung)
**Die Runtime NICHT nach Rust umschreiben** — sie ist reines, freestanding-fähiges C
(RC/Collector/Slab laufen schon ohne libc). Stattdessen:
1. Ein SEL4Lake-Crate `sel4lake-vire-rt` liefert die `plat_*`/`jrt_*`-Weak-Symbole
   als `extern "C"` (backed by `sel4lake-region` + IPC).
2. Vire baut das Programm-Objekt (`--target aarch64-unknown-none`); die Runtime.c
   wird freestanding mitkompiliert (Vires Treiber embed sie schon via `include_str!`).
3. Linken: Programm-Objekt + Vire-Runtime + `sel4lake-vire-rt` → eine
   Komponente; der Loader (ADR 0011) lädt sie cap-gegatet.

So bleibt die Vertrauenskette intact (Capabilities/Region-Runtime/Loader unverändert),
und die einzige neue vertrauenswürdige Fläche ist die kleine `plat_*`-Naht.

## Verifikations-Hinweis
SEL4Lakes Sicherheit ruht auf „jede Komponente speichersicher". Vires Sicherheit ist
RC + Collector (kein Borrow-Checker-Beweis). Für hochsichere Komponenten: die
`plat_*`-Naht + der Collector wären der Audit-Fokus (analog zu ADR 0021
„unsafe memory safety"). Vires **Heap-Bilanz-Oracle** (0 live objects am Ende, in der
Testsuite geprüft) ist ein starker Runtime-Soundness-Check, den man in die
SEL4Lake-Tests übernehmen könnte.

## Kurzfassung
**Nötig:** die `plat_*`-Naht (~5 Funktionen) auf `sel4lake-region` + Konsolen-IPC,
aarch64-Target, Laden via Loader. **Gut:** Zero-Copy-`Ptr`=Cap, `--no-cycles`/Region/
Arena für deterministische Komponenten, SEL4Lake-Threads, Hot-Reload, per-Komponente
GC-Domänen. **Kein** Sprach- oder Runtime-Neubau — nur ein kleines Plattform-Crate.
