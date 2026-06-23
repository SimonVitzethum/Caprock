# Phasenbericht 2 — Physisches Speichermodell (+ W^X-Härtung)

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Das erste echte Capability-Subsystem: ein capability-basiertes physisches
Speichermodell, plus die vorgezogene W^X-Härtung der MMU.

### W^X-Härtung (vorgezogen)

Die Identity-Map (ADR 0002) ist jetzt mehrstufig (L1→L2→L3) und setzt
Seiten-Rechte für den Kernelbereich:

- `.text` → **R-X**, `.rodata` → **R--**, Daten/BSS/Stacks/freies RAM → **RW + XN**.
- MMIO (1. GiB) → Device, XN. Freies RAM (Blöcke) → Normal, RW + XN.
- Linker-Script liefert 4-KiB-ausgerichtete Sektionsgrenzen; der Kernelbereich
  (erste 2 MiB) wird seitengenau gemappt, der Rest per 2-MiB-/1-GiB-Blöcke.

### Capability-basiertes Speichermodell — `crates/sel4lake-mem`

Reine, **unsafe-freie** Arithmetik über physische Regionen (ADR 0003):

- `PhysRegion` / `Rights` (R/W/X) — Region + Rechteset.
- **`MemoryCap`** — *lineare* (move-only) Capability über `[base, len)`. Besitz
  des Rust-Wertes **ist** die Capability: Transfer = Move, Ableitung = `split`,
  Einschränkung = `restrict` (mint-artig), Rückgabe = `free`. Rusts Ownership
  verhindert Double-Free intralingual.
- **`PhysAllocator`** — verwaltet freies RAM als sortierte, koaleszierende
  Freiliste (feste Kapazität → allokationsfrei, deterministisch); prägt
  Wurzel-Caps via `alloc` (First-Fit, seitenausgerichtet) und nimmt sie via
  `free` zurück.

### Integration + Test

- Kernel registriert freies RAM `[__kernel_end, 4 GiB)` in einem globalen
  `SpinLock<PhysAllocator>`.
- `kernel/src/memtest.rs` exerziert alloc/Ausrichtung/split/restrict/Transfer/
  free+Coalescing als Boot-Selbsttest (`memtest : PASS …`).
- `test-qemu.sh` prüft jetzt zusätzlich `memtest : ALL PASS`.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**:

```
mmu     : identity-map, M=1 C=1 I=1 (caches an)     (jetzt W^X)
mem     : freies RAM [0x4011b000, 0x140000000)
memtest : freies RAM = 4094 MiB (1 Fragmente)
memtest : PASS  alloc 1 Seite
memtest : PASS  alloc 4 Seiten, 8-KiB-aligned
memtest : PASS  disjunkte Allokationen
memtest : PASS  freies RAM um 5 Seiten reduziert
memtest : PASS  split 4 -> 1 + 3 Seiten
memtest : PASS  split-Adressen zusammenhaengend
memtest : PASS  restrict RW -> R
memtest : PASS  transfer per Move erhaelt Cap
memtest : PASS  free + Coalescing stellt RAM wieder her
memtest : ALL PASS
== checks ==  PASS: MMU+Caches · PASS: Speichermodell · PASS: 8 Kerne online · PASS: 8 Kerne ticken
```

W^X + alle 8 Kerne booten weiterhin; Timer-Ticks auf allen Kernen.

## Getroffene Entscheidungen

- **MemoryCap als lineares Rust-Objekt** statt Slot-Index (für Phase 2): nutzt
  Rusts Move-Semantik direkt als Capability-Besitz — kein `unsafe`, kein
  Double-Free, Transfer = Move. Die volle CNode/CDT-Slot-Maschinerie (für
  Delegation über cspaces, Revocation-Bäume) folgt in Phase 3 (ADR 0003).
- **Bug gefunden & behoben:** Die W^X-Tabellen-Schleifen wurden von LLVM mit
  NEON vektorisiert; FP/SIMD ist bei EL1 per `CPACR_EL1` standardmäßig getrappt
  (EC 0x07). Fix: FP/SIMD im Boot-Trampolin freigeben (CPU-Init-Domäne), für
  Primär- und Sekundärkern. (In Phase 1 trat kein SIMD-Code auf.)
- **Freiliste mit fester Kapazität (64 Fragmente):** deterministisch und
  allokationsfrei; ein dynamischer Backing-Store kommt, falls nötig.

## Unsafe-Bilanz

- `sel4lake-mem`: **0 unsafe** (reine Region-Arithmetik).
- Neuer `unsafe`: nur 2 Zeilen Boot-Assembler (`CPACR_EL1`-Freigabe, CPU-Init).
- Kernel-Crate weiterhin **0 `unsafe`-Blöcke** (außer Boot-`global_asm!`).

## Risiken / offene Punkte

- **Globaler Allokator liegt noch in `memtest.rs`:** als Demonstrationsträger.
  Wird in Phase 3 in ein sauberes Kernel-`mm`-Modul gehoben und an das
  Capability-System (CNodes/CDT) angebunden.
- **Keine CDT-Revocation bisher:** `split` erzeugt Kinder, aber ohne
  Ableitungsbaum gibt es noch keine rekursive `revoke`. Das ist Phase 3.
- **`alloc` bei voller Freiliste:** Carve könnte im Extremfall ein Fragment
  verlieren, wenn die Liste exakt voll ist (Präfix+Suffix > Kapazität). Bei 64
  Fragmenten und Bring-up-Lasten unkritisch; dokumentiert.
- **RAM-Layout fest verdrahtet** (QEMU `virt`, 4 GiB). DTB-Parsing weiterhin offen.

## Nächste Schritte (Phase 3 — Capability-System-Kern)

1. CNodes (typsichere Slot-Tabellen) + Capability-Derivation-Tree (Arena mit
   Generations-Handles) — ADR 0003.
2. Generische Capability (`enum`: Memory, Endpoint, … ) mit copy/mint/move/
   delete/**revoke** über den CDT.
3. Globalen Allokator in ein Kernel-`mm`-Modul heben und an cspaces anbinden.
4. Tests: Delegation + rekursive Revocation.
