# Verifikation — DMA-Lifetime / Revoke-Reihenfolge (Phase 7)

> **Status:** Kern bewiesen — die **DMA-use-after-free-Sicherheit** (Revoke-Reihenfolge
> `detach → free`) ist als Lebenszyklus-Invariante formal verifiziert (7 verified, CI-gated).
> Eigenständig verständlich (ohne Quellcode).

Bezug: [ADR 0022](../../docs/adr/0022-dma-lifetime-formal-verification.md), ADR 0008 (DMA/SMMU),
Laufzeit-`dma_audit` (Code 4), `docs/invariants.md` §2, `verus/dma_disjoint.rs` (Geometrie).

## 1. Motivation und Ziel

DMA ist die einzige HW-Cap-Kategorie, bei der ein Gerät **direkt** Physikspeicher liest/schreibt —
**vorbei an der CPU-MMU**. Wird eine DMA-Region freigegeben, während ein Gerät noch in sie DMAen darf,
schreibt es in recyceltes RAM (**DMA-use-after-free**) — direkter Isolationsbruch. **Ziel:** die sichere
Revoke-Reihenfolge **beweisen** (statisch, alle Zustände).

## 2. Sicherheitsmodell

- Eine **DMA-Region** trägt zwei lebenszyklus-relevante Flags: `attached` (in einem SMMU-/IOMMU-Kontext
  DMA-aktiviert) und `freed` (an den Allokator zurückgegeben).
- **Invariante** (= `dma_audit` Code 4): eine DMA-aktivierte Region ist **nie** freigegeben
  (`attached ⟹ ¬freed`). Kontrapositiv: eine freigegebene Region ist nie DMA-aktiviert.
- Sichere **Revoke-Reihenfolge:** `detach → (unmap) → free` (`revoke_dma`).
- **Sequentiell:** die SMMU-HW-Durchsetzung selbst + Nebenläufigkeit bleiben **ausserhalb**
  (HAL-/Concurrency-TCB, ADR 0022).

## 3. Zu beweisende Eigenschaften

1. **Invariante erhalten:** `attach`/`detach`/`free` bewahren `attached ⟹ ¬freed`.
2. **Kein DMA nach free:** eine freigegebene Region ist nie attached (und nicht erneut attachbar).
3. **Revoke-Ordnung:** `free` verlangt eine **abgetrennte** Region (`detach` zwingend voraus).
4. **Sichere Teardown-Sequenz:** `detach → free` ist für **jede** invariante Region wohldefiniert + sicher.

## 4. Bezug zu ADRs

ADR 0022 (diese Verifikation) · ADR 0008 (DMA-Mechanismus + SMMU-Enforcement).

## 5. Formale Spezifikation

`DmaRegion { attached, freed }`. `dma_inv(r)` = `r.attached ⟹ ¬r.freed`. `attach(r)` (requires ¬freed),
`detach(r)` (stets), `free(r)` (requires ¬attached) als Zustandsübergänge.

## 6. Verus-Architektur

[`proofs/dma_revoke.rs`](proofs/dma_revoke.rs), per `tools/verus-verify.sh` + Verus-CI-Gate. Abstrakte,
sequentielle Lebenszyklus-Zustandsmaschine (V2, ADR 0022); realer Code unverändert.

## 7. Beweisstrategie

Die Invariante macht die `free`-Vorbedingung (¬attached) und die Use-after-free-Freiheit konsistent;
`safe_teardown` komponiert `detach` (stellt ¬attached her) + `free` und zeigt die universelle Sicherheit
der dokumentierten Reihenfolge.

## 8. Lemmas / 9. Bewiesene Eigenschaften

| Theorem | Aussage | Status |
|---|---|---|
| `attach_preserves_inv` / `detach_preserves_inv` / `free_preserves_inv` | alle Übergänge erhalten `dma_inv` | ✅ |
| `freed_not_attached` | eine freigegebene Region ist nie DMA-aktiviert (kein DMA nach free) | ✅ |
| `free_requires_detached` | `free` verlangt eine abgetrennte Region (Revoke-Ordnung explizit) | ✅ |
| `safe_teardown` | `detach → free` ist für jede invariante Region wohldefiniert + sicher | ✅ |

(7 verified inkl. `main`.)

## 10. Noch offene Eigenschaften

- **Aggregation mehrerer Regionen je SMMU-Kontext** (DmaContext: 1 STE-Gruppe → N Regionen) — die
  Invariante je Region komponiert, der Kontext-Abbau als Ganzes ist die nächste Stufe.
- **Geräte-Adressvalidierung** (jede Geräte-DMA-Adresse liegt in der DmaCap — Level-1-Software-Disziplin)
  als separater Bounds-Beweis (vgl. `verus/dma_disjoint.rs`).

## 11. Bekannte Grenzen

- **Sequentiell:** gleichzeitiges `attach`/`free` über Kerne (Interleavings) liegt **ausserhalb**
  (Loom/TLA+).
- **Lebenszyklus, nicht HW-Durchsetzung:** dass die SMMU-Stage-1 die Isolation **real** durchsetzt,
  ist die HAL-TCB (unter QEMU für emulierte Geräte ohnehin nicht beobachtbar, s. ADR 0008); hier wird
  die **Kernel-seitige Ordnung** bewiesen, die die HW-Durchsetzung erst use-after-free-sicher macht.

## 12. Trusted Computing Base

1. SMMU-HW-Durchsetzung (STE/CD/Stage-1) — HAL-TCB.
2. Modell↔Code-Treue (Flags statt realer DMA_CTX/Free-Liste) — durch `dma_audit` (Code 4) + hwfuzz
   auf dem **echten** Code abgesichert.

## 13. Verbindung zu Runtime-Audits / Kani

- **Laufzeit:** `dma_audit` Code 4 (`dma_ctx_regions_live`: keine gemappte Region überlappt freies RAM,
  via `PhysAllocator::overlaps_free`), hwfuzz (DMA-Churn: attach/detach/revoke + balancierte total_free).
- **Verus (hier):** beweist, dass die Revoke-Ordnung die Use-after-free-Freiheit **für alle Zustände**
  garantiert. Die Ebenen ergänzen sich.

## 14. Verifikationsfortschritt / Nächste Ausbaustufen

- ✅ DMA-Lebenszyklus (Revoke-Ordnung / kein DMA-use-after-free).
- ⏳ Kontext-Aggregation (N Regionen je STE-Gruppe) · Geräte-Adress-Bounds · (später) Nebenläufigkeit.
