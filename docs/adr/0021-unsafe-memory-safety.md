# ADR 0021 — Speichersicherheit der Software-`unsafe`-Stellen (Kani, schrittweise)

Status: **angenommen** · Datum: 2026-06-27 · ergänzt die funktionale Verifikation (Phasen 1–6).
Bezug: ADR 0015 (Verifikationsansatz), `ARMTest/unsafe-memory-safety-aufwand.md` (Aufwandsanalyse),
`Verification/unsafe-safety/`, `docs/verification.md` (Kani region/sync).

## Kontext

Der safe-Rust-Kern (~5000 LOC) ist by-compiler speichersicher; zu beweisen bleibt nur die
Speichersicherheit der `unsafe`-Blöcke. Die Aufwandsanalyse (`ARMTest/unsafe-memory-safety-aufwand.md`)
teilt die ~200 Stellen scharf in **Kategorie A** (reine RAM-Zeiger/Slice — mit Kani/Verus beweisbar;
`region`/`sync` bereits erledigt) und **Kategorie B** (Hardware-/Maschinenmodell: Pagetables, MMIO,
Kontextwechsel-Assembly — Forschungsklasse, als vertraute HAL-Basis gekapselt). Diese ADR behandelt
**Kategorie A**: die einzelnen, in 1–3 h gut beweisbaren Software-`unsafe`-Stellen werden **nach und
nach** mit Kani memory-safe bewiesen.

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 `#[cfg(kani)]` im Kernel-Crate | Harnesses direkt im Kernel-Code (wie region/sync) | der Kernel-Crate baut nur unter Custom-Target + build-std + HW-Deps -> unter `cargo kani` (Host-Target) nicht baubar |
| V2 Verus-Puffermodell | das Modell statt der echten `core::ptr`-Ops beweisen | beweist die Arithmetik, prüft aber **nicht** die echten unsafe-Ops |
| **V3 eigenständiges Kani-Artefakt (gewählt)** | `Verification/unsafe-safety/`: **getreue Kopien** der Kernel-unsafe-Glue, Kani führt die **echten** `core::ptr`-Ops auf modelliertem Puffer aus | prüft die realen unsafe-Operationen (OOB/Underflow/UB); Kernel unverändert; Treue der Kopie = kleine, auditierbare TCB |

## Entscheidung

**V3** — ein eigenständiges Kani-Verifikationsartefakt (`Verification/unsafe-safety/kani/`): jede
`*_logic`-Funktion ist eine **byte-genaue Kopie** der unsafe-Glue einer Kernel-Stelle (Zeile
referenziert); Kani-Harnesses führen die echten `copy_nonoverlapping`/`write_bytes`/… auf einem
modellierten Puffer aus und beweisen Out-of-Bounds-/Underflow-/UB-Freiheit **unter der dokumentierten
Vorbedingung** — die ihrerseits am Aufrufer als reine Arithmetik bewiesen wird. Realer Kernel-Code
unverändert; via `tools/kani-verify.sh unsafe` + CI-Gate.

**Auswahlkriterium „sinnvoll beweisbar":** eine Stelle wird aufgenommen, wenn sie (a) Kategorie A
(normales RAM) ist und (b) in 1–3 h gut beweisbar ist. Reine Trust-Primitive ohne in-Funktion-Bounds
(`mem::peek/poke` auf beliebige Cap-Adressen, MMIO-`volatile`, fixe RAM-Fenster-Slices) werden **nicht**
„bewiesen", sondern als HW-/Cap-Vertrag dokumentiert (ihre Gültigkeit kommt aus dem bereits
verifizierten Cap-System bzw. der HAL-TCB).

## Konsequenzen

- **Positiv:** die Software-`unsafe`-Stellen (Kategorie A) erhalten einen maschinengeprüften
  Memory-Safety-Beweis mit den echten `core::ptr`-Operationen; ergänzt die bestehenden region/sync-
  Kani-Beweise. Schrittweise erweiterbar (eine Stelle je Iteration).
- **Grenzen:** **Kategorie B** (Pagetables/MMIO/Assembly) bleibt die axiomatisierte, hand-auditierte
  HAL-TCB (vgl. Aufwandsanalyse). Die **Fidelity** der Kopie ↔ Kernel-Stelle ist eine dokumentierte
  Annahme (klein, je Stelle ein Zeilenverweis; bei Kernel-Änderung nachzuziehen).

## Abgedeckte Stellen (wächst je Iteration)

| # | Kernel-Stelle | Eigenschaft | Status |
|---|---|---|---|
| 1 | `kernel/src/system.rs::copy_segment` (einzige unsafe-Stelle des Ladepfads) | Kopie+`.bss`-Nullung bleibt im Ziel-Frame; Vorbedingung `filesz<=total` am Aufrufer etabliert | ✅ |
| 2 | `crates/sel4lake-region/src/heap.rs` (Slab-Free-Liste, `read`/`write` des eingebetteten Nachfolger-Zeigers) | jede Größenklasse fasst einen `usize` + ist usize-ausgerichtet; roher Read/Write des Slot-Zeigers in-bounds + aligned (Round-Trip) | ✅ |
