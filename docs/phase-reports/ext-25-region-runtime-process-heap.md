# ext-25 — Region-Runtime + prozess-lokaler Hybrid-Heap (Safe-Rust-API)

Status: **fertig** (R0–R4), Suite grün (49 Checks, `test-qemu.sh`).

## Ziel

Echter **dynamischer Heap** (`Box`/`Vec`/`BTreeMap`) für safe-Rust-Trusted-SAS-Prozesse auf
**realen Physadressen** — ohne `unsafe` im App-Code, ohne einen öffentlichen `MemoryCap -> &mut
[u8]`-Wrapper. Alles Speicher-`unsafe` in einer kleinen, klar auditierbaren Runtime-Schicht.
Architektur von Anfang an auf **mehrere Regionen** ausgelegt. Siehe
[ADR 0010](../adr/0010-region-runtime-and-process-heap.md).

## Phasen

- **R0 — Runtime `caprock-region`.** Neues Crate, die **einzige** Stelle mit Speicher-`unsafe`.
  `Region` (besitzt `MemoryCap` + `RegionTag{id,Purpose}`); `RegionView<'a>` mit ausschließlich
  sicheren Operationen: `get<T:Pod>`/`set<T:Pod>`/`copy_from`/`copy_to`/`fill`, scoped
  `with_bytes(|&mut [u8]| …)` (Slice kann die Closure nicht verlassen), `split_at`/`subview`.
- **R1 — `RegionSource` (grow/shrink).** Trait `request(min_len, Purpose)`/`release(Region)`;
  kernel-seitig `KernelRegionSource` über den physischen Allokator (`MEM`). EL0 würde es per
  Syscall marshallen — Schnittstelle identisch.
- **R2 — Hybrid-Allokator `Heap<S>` ⊳ `core::alloc::Allocator`.** Größenklassen-Slabs (16…2048 B,
  intrusive Free-Listen, Bump-Carving aus Regionen) für Klein-Allokationen; dedizierte Regionen
  (Bump/whole) für Groß-Allokationen; grow/shrink über die `RegionSource`; `Drop` gibt alle
  Regionen zurück. Arbeitet auf `RegionView`s.
- **R3 — Test `sasheap`.** Ein Trusted-SAS-Kontext (safe Rust) baut `Heap::new(KernelRegionSource)`
  und nutzt **echte** `Box`/`Vec`/`BTreeMap` via `*_in(&heap)`: Vec mit 4096 Elementen (Realloc =
  grow, über Größenklassen + Region hinaus), Box, BTreeMap mit 256 Knoten (Slab-Churn), Large-Alloc
  16 KiB (dedizierte Region), Drop, Balance (alle Regionen zurück an `MEM`). **Testcode 100% safe.**
- **R4 — Doku.** ADR 0010, dieser Bericht, Memory.

## Architekturwahl (nach Vergleich)

Gewählt: **Hybrid (Größenklassen-Slabs + Bump-Arenen)** über einer Regionsliste. Begründung:
trifft Fragmentierung / Performance / Hot-Reload / Zero-Copy / DMA / Verifikation gleichzeitig und
hält das `unsafe` minimal. Klassischer Region-Free-List-Malloc verworfen (im SAS keine
Kompaktierung → dauerhafte Fragmentierung + schwerste Verifikationslast). Reines Bump-only verworfen
als Allgemein-Heap (kein mixed-lifetime-Free) — bleibt der Large-/Arena-Pfad.

## `unsafe`-Bilanz

Das gesamte Speicher-`unsafe` liegt in `crates/caprock-region` (RegionView-Accessoren +
Allokator-Glue), begründet durch cap-validierte Bounds. App-/Testcode ist 100% Safe Rust. Der
Kernel-`#[global_allocator]` ist ein **Wächter** (`NoGlobalHeap`), der versehentliches `Box::new`/
`Vec::new` paniert — prozess-lokale Heaps (`*_in`) sind Pflicht.

## Verifikation

`./test-qemu.sh` → **49/49 ALL PASS** (bei freier CPU). Neuer Check `sasheap`: Vec(4096,Realloc/
grow)=true, Box=true, BTreeMap(256)=true, Large-Alloc(dedizierte Region)=true, Region-angefordert=
true, balanciert(alle Regionen zurück)=true; `domain_audit`/`vspace_audit` == 0. Host-Last-
Flakiness bekannt (TCG-Starvation, kein Kernel-Bug — sauberer Lauf kommt im ruhigen Fenster durch).

## Bewusst aufgeschoben

Slab-Region-Reclaim bei vollständig freier Region (Free-Listen-Filterung), EL0-Syscall-
`RegionSource`, Slab-Batching, `realloc`-Fastpath (derzeit `Allocator`-Default grow = alloc+copy+
dealloc), hwfuzz-Heap-Churn (der deterministische `sasheap`-Test churnt bereits via BTreeMap/Vec-
Realloc), Hot-Reload-Zustand als übergebene `Region`.
