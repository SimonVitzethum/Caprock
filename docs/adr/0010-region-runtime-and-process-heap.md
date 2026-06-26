# ADR 0010 — Region-Runtime + prozess-lokaler Hybrid-Heap (Safe-Rust-API, gekapseltes `unsafe`)

Status: **Akzeptiert** (ext-25, umgesetzt R0–R4)

## Kontext

ADR 0002 (SAS) verlangt: jede Adresse ist real-physisch, keine MMU-Isolation, Speichersicherheit
ausschließlich über Capabilities + Safe Rust. Bis ext-24 hatten Trusted-SAS-Prozesse **keinen
dynamischen RAM** (`#![no_std]`, kein `#[global_allocator]`, kein `Box`/`Vec`); cap-gewährte
Regionen waren nur über rohes `peek_u64`/`poke_u64` erreichbar. Ziel von ext-25: **echter
dynamischer Heap** (Box/Vec/BTreeMap) für safe-Rust-SAS-Prozesse auf realen Physadressen — ohne
`unsafe` im App-Code, ohne einen allgemeinen `MemoryCap -> &mut [u8]`-Wrapper nach außen.

## Entscheidung

### 1. Runtime-Schicht `sel4lake-region` — die EINZIGE Stelle mit Speicher-`unsafe`
Zwei Typen kapseln allen rohen Zugriff; die öffentliche API ist vollständig Safe Rust:

- **`Region`** — die **cap-besessene** Einheit: hält einen `MemoryCap` (lineares Eigentum) +
  Audit-Metadaten (`RegionTag { id, Purpose }`). `phys()`/`len()`/`into_cap()`/`view()`. Ein
  Prozess hält eine **Menge** davon — nie „einen Heap".
- **`RegionView<'a>`** — eine geliehene, **begrenzte** Sicht. Bietet nur sichere Operationen:
  - typisierte **kopierende** Zugriffe `get<T: Pod>`/`set<T: Pod>`/`copy_from`/`copy_to`/`fill`,
  - **scoped** `with_bytes(|s: &mut [u8]| …)` — der Slice ist an die Closure gebunden und kann sie
    **nicht verlassen** (ergonomisch fürs Parsen/memcpy/DMA-Füllen, ohne dass ein nackter Slice
    nach außen leckt),
  - `split_at`/`subview` — die Operationen, auf denen der Allokator arbeitet.

Die wenigen `unsafe`-Blöcke (in den `RegionView`-Accessoren + der Allokator-Glue) sind durch die
**cap-validierte** Region (Besitz + Bounds + Rechte) + die Bounds-Checks begründet und auditierbar.

(Verworfen: ein öffentlicher `region_as_slice() -> &mut [u8]`-Wrapper — er würde nackte Slices
überall verteilen und Heap/DMA/Hot-Reload/Audit-Metadaten nicht trennen.)

### 2. Multi-Region von Anfang an + `RegionSource` (grow/shrink)
Ein Prozess-Heap besitzt eine **Regionsliste**, nicht eine Region. Eine `RegionSource`-Trait
abstrahiert grow/shrink:

```
trait RegionSource { fn request(min_len, Purpose) -> Option<Region>; fn release(Region); }
```

Kernel-/Trusted-SAS-seitig bedient `KernelRegionSource` sie direkt aus dem physischen Allokator
(`MEM`); ein EL0-Prozess würde dasselbe per Syscall marshallen — die Schnittstelle bleibt identisch.

### 3. Hybrid-Allokator (gewählt nach Vergleich) — `Heap<S: RegionSource>` ⊳ `core::alloc::Allocator`
- **Klein** (≤ größte Klasse): **Größenklassen-Slabs** (16…2048 B). Je Klasse eine **intrusive**
  Free-Liste freigegebener Slots; neue Slots werden aus einer **Bump-Region** geschnitten
  (beschränkte Fragmentierung, O(1)). Ist eine Bump-Region voll → neue über `RegionSource` (grow).
- **Groß** (> größte Klasse): eine **dedizierte** Region je Allokation (kontiguierlich, DMA-/Hot-
  Reload-tauglich); bei `dealloc` sofort an die Source zurück (shrink). `Heap::drop` gibt **alle**
  Regionen zurück.

Verglichene Alternativen (für den SAS, **ohne** Kompaktierung):
- *Bump/Arena-only:* optimal in 5/6 Kriterien, aber kein mixed-lifetime-Heap → nur als Large-/
  Arena-Pfad genutzt.
- *Region-Free-List (klassischer malloc):* im SAS dauerhafte Fragmentierung (keine Kompaktierung)
  + schwerste Verifikationslast (Coalescing-Invarianten) → **verworfen**.
- *Reine Größenklassen:* gut, aber DMA/Zustand nicht sauber getrennt → zum Hybrid erweitert.

Begründung: der Hybrid trifft Fragmentierung / Performance / Hot-Reload / Zero-Copy / DMA /
Verifikation **gleichzeitig** und hält das `unsafe` minimal (Bump trivial + Slab simpel, **kein**
Coalescing-Malloc).

### 4. Kein impliziter globaler Heap
Das SAS-Modell verlangt **prozess-lokale** Heap-Instanzen (`Heap::new(source)` + `Box::new_in`/
`Vec::new_in`/`BTreeMap::new_in`). Der `#[global_allocator]` ist ein **Wächter** (`NoGlobalHeap`),
der versehentliches `Box::new`/`Vec::new` paniert — ein impliziter, prozess-übergreifender Heap
existiert bewusst nicht (welche Regionen welches Prozesses?).

## Vertrauens-/Sicherheitsmodell

Mit *no unsafe im App-Code + buglosem Compiler* (ADR 0002): ein safe-Rust-Prozess kann keinen
Zeiger fälschen und nur Speicher berühren, der von seinen legitimen Referenzen erreichbar ist —
Stack, statische Daten, **sein Heap** (= seine `Region`-Menge). Die `sel4lake-region`-Runtime ist
die **kleine, klar auditierbare** Schicht, in der das gesamte Speicher-`unsafe` konzentriert ist;
ihre Korrektheit (Bounds + cap-Besitz) trägt die Isolation, **ohne** MMU. Capabilities regeln,
*welche* Regionen der Kernel überhaupt gewährt (Least Privilege).

## Konsequenzen

- Trusted-SAS-Prozesse können nun `Box`/`Vec`/`BTreeMap` auf realen Physadressen nutzen; der
  Heap wächst/schrumpft über `RegionSource`.
- **Zukunft ohne API-Bruch:** DMA-Regionen sind dieselbe `Region`/`RegionView` (Purpose::Dma; der
  ext-24-`DmaPool` ist ein Spezialfall); Hot-Reload-Zustand lebt in einer `Region`, die per Cap an
  v2 übergeben wird (identity-stabile Adresse); Audit-/Debug-Info hängt an `RegionTag`.
- **SAS-Preise (bewusst):** Heap = nicht-zusammenhängende Regionen; **keine** Lebend-Kompaktierung
  (Referenzen dürfen nicht verschoben werden); Fragmentierung sichtbar → mit Größenklassen begegnet.
- **Aufgeschoben:** Slab-Region-Reclaim bei vollständig freier Region (erfordert Free-Listen-
  Filterung), EL0-Syscall-`RegionSource` (kernel-seitig direkt), Slab-Batching, `realloc`-Fastpath
  (derzeit über den `Allocator`-Default grow = alloc+copy+dealloc).
