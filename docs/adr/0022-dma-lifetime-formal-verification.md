# ADR 0022 — Formale Verifikation des DMA-Lebenszyklus (Verus, Phase 7)

Status: **angenommen** · Datum: 2026-06-27 · Phase 7 der funktionalen Verifikation (weitere Komponente).
Bezug: ADR 0015 (Verifikationsansatz), ADR 0008 (DMA/SMMU), `Verification/dma-lifetime/`,
Laufzeit-`dma_audit` (Code 4), `docs/invariants.md` §2.

## Kontext

DMA ist die einzige HW-Cap-Kategorie, bei der ein bus-masterndes Gerät **direkt** Physikspeicher liest/
schreibt — **vorbei an der CPU-MMU**. Wird eine DMA-Region freigegeben, während ein Gerät noch in sie
DMAen darf (Durchsetzung nicht entzogen), schreibt das Gerät in bereits recyceltes RAM (**DMA-use-
after-free**) — ein direkter Bruch der Isolation. Die sichere Revoke-Reihenfolge ist
`detach → (unmap) → free` (`revoke_dma`); zur Laufzeit prüft `dma_audit` Code 4 („eine noch in einem
SMMU-Kontext gemappte Region überlappt nie freies RAM"). Phase 7 **beweist** die zugrunde liegende
Lebenszyklus-Invariante. Die **SMMU-Hardware-Durchsetzung selbst** + **Nebenläufigkeit** bleiben
**ausserhalb** (HAL-/Concurrency-TCB).

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 geometrisches Disjunktheits-Modell | Adressintervalle + `overlaps_free` bitgenau | Disjunktheit ist bereits separat (`verus/dma_disjoint.rs`); hier geht es um die **zeitliche** Ordnung, nicht die Geometrie |
| **V2 Lebenszyklus-Zustandsmaschine (gewählt)** | Region als `{attached, freed}`-Flags; `attach`/`detach`/`free` als Übergänge | erfasst **genau** die Use-after-free-Eigenschaft (Code 4) als Ordnungs-Invariante; klein + faithful zur `revoke_dma`-Realität |

## Entscheidung

**V2** — den DMA-Lebenszyklus als Zustandsmaschine modellieren und beweisen: **Invariante**
`attached ⟹ ¬freed` (eine DMA-aktivierte Region ist nie freigegeben — = `dma_audit` Code 4). Übergänge:
`attach` (nur auf lebender Region), `detach` (stets, idempotent), `free` (verlangt **abgetrennte**
Region). Bewiesen: alle Übergänge erhalten die Invariante; **kein DMA nach free** (freigegebene Region
nie attached/attachbar); `free` erzwingt die **Revoke-Ordnung** (`detach` zwingend voraus); die
kanonische `detach → free`-Sequenz ist **immer** sicher. Abstraktes Modell; realer Code unverändert.

## Konsequenzen

- **Positiv:** die DMA-use-after-free-Sicherheit (Revoke-Reihenfolge) ist als Ordnungs-Invariante
  bewiesen, ergänzt `dma_audit` (Code 4) + den hwfuzz. Zusammen mit `verus/dma_disjoint.rs` (Geometrie)
  ist der DMA-Sicherheitskern (Bounds-Disjunktheit **und** Lebenszyklus) formal abgedeckt.
- **Grenzen/offen:** die **SMMU-Hardware-Durchsetzung** (STE/CD/Stage-1 setzt die Isolation real durch)
  ist die HAL-TCB; **Nebenläufigkeit** (gleichzeitiges attach/free über Kerne) + die **Aggregation
  mehrerer Regionen je Kontext** bleiben ausdrücklich ausserhalb (Folgestufe bzw. HW-/Concurrency-TCB).
