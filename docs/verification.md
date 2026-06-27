# SEL4Lake — Formale Verifikation (Tier 1: Kani)

Dieses Dokument beschreibt die **dauerhafte Verifikationspipeline** von SEL4Lake. Strategie-Analyse
(Stufenmodell Tier 1–3, seL4-Vergleich, Tool-Landschaft): `ARMTest/formale-verifikation-aufwand.md`.

## Stufenmodell (Kurzfassung)

- **Tier 0 (vorhanden):** safe-Rust-Kern (by-compiler speichersicher), `forbid(unsafe_code)`-Parser,
  10 Laufzeit-Audits, Fuzzer, `docs/invariants.md`.
- **Tier 1 (HIER, aktiv):** **Kani** (bounded Model Checking, CBMC) beweist Panik-/OOB-/Overflow-
  Freiheit + Struktur-Invarianten der klar abgegrenzten, sicherheitskritischen Komponenten.
- **Tier 2 (später):** **Verus** — funktionale Korrektheit (Audits werden zur Spec).
- **Tier 3 (Forschung):** volle Korrektheit + Isolation/Info-Flow + HW-Modell.

## Tier 1 — was bewiesen ist

| Komponente | Harness | Aussage |
|---|---|---|
| `sel4lake-loader/cert.rs` | `parse_never_panics` | `TrustedCert::parse` paniert/OOBt/überläuft **nie** (beliebige Eingabe) |
| | `parse_partitions_input` | bei Erfolg: `message()+signature()` partitionieren die Eingabe exakt, Sig nicht leer, `len==152+build_info` |
| `sel4lake-loader/archive.rs` | `parse_never_panics` | `Archive::parse` (+ `program(i)`) panik-/OOB-frei |
| `sel4lake-loader/elf.rs` | `parse_never_panics` | `ElfImage::parse` panik-/OOB-/overflow-frei |
| | `segments_are_sound` | bei Erfolg: jedes Segment `memsz>=filesz`, `segment_bytes().len()==filesz`, `offset+filesz<=len` |

Die Harnesses sind `#[cfg(kani)]`-Module direkt in den jeweiligen Quelldateien — **im Normal-Build
vollständig inert** (keine Auswirkung auf Kernel/Tests). Die Eingabe-Obergrenzen (z. B. 200–260 B)
sind **pfad-vollständig** gewählt: jeder Code-Pfad, der weiter läuft, verlangt strukturell eine
kleinere Eingabe; größere Felder lösen immer den frühen Reject aus. Der Beweis ist damit für diese
Parser effektiv vollständig, nicht bloß „getestet bis N".

## Lokal ausführen

```sh
# Einmalig:
cargo install --locked kani-verifier && cargo kani setup
# Alle Loader-Beweise:
tools/kani-verify.sh
# Ein einzelner Harness:
tools/kani-verify.sh --harness cert::kani_proofs::parse_never_panics
```

`tools/kani-verify.sh` kopiert die (abhängigkeitsfreie) Crate nach `$TMPDIR` und ruft dort
`cargo kani` — das umgeht den `build-std`-Zwang aus dem Workspace-`.cargo/config.toml` (Custom-Target
für den bare-metal Kernel), der `cargo kani` sonst brechen würde.

## CI-Gate

`.gitea/workflows/kani.yml` führt bei **jedem Push/PR** aus:
1. Host-Tests der Loader-Crate + Inert-Check (cfg(kani) bricht den Normal-Build nicht),
2. **alle Kani-Harnesses** (cert/archive/elf).

Schlägt ein Beweis fehl (eine spätere Änderung verletzt eine bewiesene Eigenschaft), **schlägt die CI
fehl**. Damit können bereits bewiesene Eigenschaften nicht unbeabsichtigt verloren gehen — die
Verifikation ist fester Bestandteil des Entwicklungsprozesses.

## Neue Komponenten aufnehmen

1. `#[cfg(kani)] mod kani_proofs { use super::*; … }` an die Quelldatei anhängen.
2. Harnesses mit `#[kani::proof]` (+ `#[kani::unwind(N)]` bei Schleifen) schreiben; Eingaben via
   `kani::any()` + `kani::assume(len <= MAXLEN)`.
3. Eine **pfad-vollständige** Eingabeschranke wählen + im Kommentar begründen.
4. Lokal `tools/kani-verify.sh` grün → committen (das CI-Gate prüft es dann dauerhaft).

## Roadmap Tier 1

- [x] Cert-Parser (`cert.rs`)
- [x] Boot-Archiv-Parser (`archive.rs`)
- [x] ELF-Parser (`elf.rs`)
- [x] CI-Gate (Gitea Actions)
- [ ] Region-Runtime (`sel4lake-region`: RegionView/Pod — Bounds-/Slice-/Lifetime-Verträge um die `unsafe`-Blöcke)
- [ ] Synchronisationsprimitive (`sel4lake-sync`: Lock-Invarianten/Zustandsübergänge, soweit modellierbar)
- [ ] kernweite Overflow-/Arithmetik-Checks
