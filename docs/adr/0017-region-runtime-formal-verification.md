# ADR 0017 — Formale funktionale Verifikation der Region-Runtime (Verus, Phase 3)

Status: **angenommen** · Datum: 2026-06-27 · Phase 3 der funktionalen Verifikation.
Bezug: ADR 0010 (Region-Runtime + Prozess-Heap), ADR 0015 (Verifikationsansatz), `Verification/region-runtime/`.

## Kontext

Die Region-Runtime (`sel4lake-region`) ist die **einzige** Crate mit Speicher-`unsafe` im Userland-Pfad;
ihre **Speichersicherheit** (RegionView-Zugriffe nie OOB) ist bereits mit **Kani** bewiesen
(`docs/verification.md`). Phase 3 ergaenzt die **funktionale** Schicht: die **Ressourcen-Bilanz** des
Allokators — Konservierung, Ownership, Balance —, die zur Laufzeit von `churn`/`sasheap`
(`total_free`-Baseline) geprueft wird.

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 voller Hybrid-Allocator (Slabs+Bump) | reale Allokator-Datenstrukturen modellieren | gross; die Slab-/Bump-Geometrie ist Speichersicherheit (Kani) + Disjunktheit (vgl. dma_disjoint), nicht die Kern-**Bilanz** |
| **V2 Bilanz-Modell (gewählt)** | Pool als `total`/`free` + Regionen-Seq; **Konservierung** `free + Σ(lebende) == total`, alloc/free dagegen | erfasst genau die Funktional-Eigenschaft (kein Leak, keine Doppel-Freigabe, Balance); klein + faithful |

## Entscheidung

**V2** — die **Konservierungs-Invariante** `free + Σ(lebende Regionen) == total` modellieren und
beweisen, dass `alloc`/`free_region` sie **erhalten**, dass `free_region` eine **lebende** Region
verlangt (keine Doppel-Freigabe = lineare Ownership) und dass `alloc(n)`+`free` `free` **exakt**
wiederherstellt (**Balance**, kein Leak). Abstraktes Modell; realer Code unverändert; die
Speichersicherheit der RegionViews liefert Kani.

## Konsequenzen

- **Positiv:** die Ressourcen-Bilanz (Laufzeit-`total_free`-Checks) wird von „geprueft" zu „bewiesen";
  Ownership/no-double-free formalisiert. Schichtung: Kani (Speichersicherheit) + Verus (Bilanz).
- **Grenzen/offen:** Hybrid-Allocator-**Geometrie** (Slab/Bump-Disjunktheit), Zero-Copy-Invarianten
  (ein Frame in zwei VSpaces, cap-gewaehrt), Hot-Reload-Zustandsuebergabe — spaetere Stufen; die
  RegionSource-grow/shrink ist die naechste (baut auf der Konservierung auf).
