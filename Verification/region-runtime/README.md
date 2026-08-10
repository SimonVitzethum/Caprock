# Verifikation — Region-Runtime (Phase 3)

> **Status:** Kern bewiesen — die **Ressourcen-Bilanz** des Allokators (Konservierung, Ownership,
> Balance) ist formal verifiziert (7 verified, CI-gated); die **Speichersicherheit** der RegionViews
> ist bereits mit Kani bewiesen. Eigenständig verständlich (ohne Quellcode).

Bezug: [ADR 0017](../../docs/adr/0017-region-runtime-formal-verification.md), ADR 0010 (Region-Runtime),
`docs/verification.md` (Kani-Speichersicherheit von `caprock-region`).

## 1. Motivation und Ziel

Die Region-Runtime ist die **einzige** Crate mit Speicher-`unsafe` im Userland-Pfad. Kani beweist
bereits, dass die rohen Zugriffe **nie ausserhalb der Region** liegen (Speichersicherheit). **Ziel von
Phase 3:** die **funktionale** Korrektheit der Ressourcenverwaltung — **kein Leak, keine
Doppel-Freigabe, exakte Balance** — beweisen (statisch, alle Zustände). Das hebt die Laufzeit-
`total_free`-Balance-Checks (`churn`/`sasheap`) von „geprüft" auf „bewiesen".

## 2. Sicherheitsmodell

- Ein **Pool** verwaltet `total` Bytes Kapazität, davon `free` aktuell frei, der Rest in **Regionen**
  gebunden (jede mit Größe + `live`-Flag = lineare Ownership).
- **Konservierung:** kein Byte entsteht/verschwindet — `free + Σ(lebende Regionen) == total`.
- **Ownership:** eine Region wird **genau einmal** freigegeben (Freigabe verlangt `live`).

## 3. Zu beweisende Eigenschaften

1. **Konservierung erhalten:** `alloc`/`free_region` bewahren `free + Σ(lebende) == total`.
2. **Keine Doppel-Freigabe:** `free_region` verlangt eine **lebende** Region (lineare Ownership).
3. **Balance / kein Leak:** `alloc(n)` + Freigabe stellt `free` **exakt** wieder her.

## 4. Bezug zu ADRs

ADR 0017 (diese Verifikation) · ADR 0010 (Region-Runtime + Prozess-Heap).

## 5. Formale Spezifikation

`Pool { total, free, regions: Seq<Region> }`, `Region { size, live }`. `live_bytes(regions)` = Summe
der Größen lebender Regionen (rekursiv). `pool_inv(p)` = `free + live_bytes == total`. `alloc`/
`free_region` als Spec-Funktionen; Eigenschaften via Erhaltungs-Lemmas.

## 6. Verus-Architektur

[`proofs/conservation.rs`](proofs/conservation.rs), per `tools/verus-verify.sh` + Verus-CI-Gate.
Abstraktes Bilanz-Modell (V2, ADR 0017); die Speichersicherheit der RegionViews liefert **Kani**
(`caprock-region`, `docs/verification.md`). Realer Code unverändert.

## 7. Beweisstrategie

Induktions-Lemmas über `live_bytes` (`lemma_live_push`/`lemma_live_update`, analog zu `refs_to` im
Capability-System) tragen die Konservierung durch alloc/free; die Balance komponiert beide.

## 8. Lemmas / 9. Bewiesene Eigenschaften

| Theorem | Aussage | Status |
|---|---|---|
| `lemma_live_push` / `lemma_live_update` | `live_bytes`-Effekt von Anhängen/Ersetzen | ✅ |
| `alloc_preserves` | `alloc(n)` (n ≤ free) erhält Konservierung; `free -= n` | ✅ |
| `free_preserves` | Freigabe einer **lebenden** Region erhält Konservierung; `free += size` | ✅ |
| `alloc_free_balance` | `alloc(n)` + Freigabe stellt `free` **exakt** wieder her (kein Leak) | ✅ |

(7 verified inkl. `main`.)

## 10. Noch offene Eigenschaften

- **RegionSource grow/shrink** (Pool-Wachstum/-Schrumpfung über die Kernel-Quelle) — baut auf der
  Konservierung auf, nächste Stufe.
- **Hybrid-Allocator-Geometrie** (Slab/Bump-Disjunktheit) — Speichersicherheit (Kani) + Intervall-
  Disjunktheit (vgl. `verus/dma_disjoint.rs`).
- **Zero-Copy-Invarianten** (ein Frame in zwei isolierten VSpaces, cap-gewährt) + **Hot-Reload-
  Zustandsübergabe** (Zustand überlebt Komponententausch).

## 11. Bekannte Grenzen

- **Abstraktes Bilanz-Modell:** abgesichert durch die Laufzeit-`total_free`-Checks (`churn`/`sasheap`)
  + Fuzzer auf dem **echten** Allokator.
- **Bytes-Bilanz, nicht Adress-Geometrie:** die geometrische Disjunktheit (kein Überlapp) ist separat
  (Kani-Speichersicherheit + Intervall-Disjunktheit).

## 12. Trusted Computing Base

1. RegionView-**Speichersicherheit** — mit Kani bewiesen.
2. Modell↔Code-Treue der Bilanz — durch `churn`/`sasheap` + Fuzzer auf dem echten Code abgesichert.
3. Der physische Allokator (`PhysAllocator`) als Byte-Quelle — Kernel-Kern (separat geprüft).

## 13. Verbindung zu Runtime-Audits / Kani

- **Kani:** RegionView (`get`/`set`/`copy`/`fill`/`split_at`/`subview`) nie OOB — Speichersicherheit.
- **Laufzeit:** `churn` (2000 Zyklen → `total_free`-Baseline), `sasheap` (Box/Vec/BTreeMap → Balance).
- **Verus (hier):** beweist, dass die Bilanz **immer** stimmt. Die Ebenen ergänzen sich.

## 14. Verifikationsfortschritt / Nächste Ausbaustufen

- ✅ Konservierung + Ownership + Balance.
- ⏳ RegionSource grow/shrink · Hybrid-Geometrie · Zero-Copy · Hot-Reload-Zustand.
