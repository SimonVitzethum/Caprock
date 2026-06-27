# SEL4Lake — Formale Verifikation (Tier 1: Kani, Tier 2: Verus-Pilot)

Dieses Dokument beschreibt die **dauerhafte Verifikationspipeline** von SEL4Lake. Strategie-Analyse
(Stufenmodell Tier 1–3, seL4-Vergleich, Tool-Landschaft): `ARMTest/formale-verifikation-aufwand.md`.

## Stufenmodell (Kurzfassung)

- **Tier 0 (vorhanden):** safe-Rust-Kern (by-compiler speichersicher), `forbid(unsafe_code)`-Parser,
  10 Laufzeit-Audits, Fuzzer, `docs/invariants.md`.
- **Tier 1 (HIER, aktiv):** **Kani** (bounded Model Checking, CBMC) beweist Panik-/OOB-/Overflow-
  Freiheit + Struktur-Invarianten der klar abgegrenzten, sicherheitskritischen Komponenten.
- **Tier 2 (Pilot aktiv):** **Verus** — deduktive funktionale Korrektheit (Audits werden zur Spec).
  Erster kleiner Pilot umgesetzt (s. u.).
- **Tier 3 (Forschung):** volle Korrektheit + Isolation/Info-Flow + HW-Modell.

## Tier 1 — was bewiesen ist

| Komponente | Harness | Aussage |
|---|---|---|
| `sel4lake-loader/cert.rs` | `parse_never_panics` | `TrustedCert::parse` paniert/OOBt/überläuft **nie** (beliebige Eingabe) |
| | `parse_partitions_input` | bei Erfolg: `message()+signature()` partitionieren die Eingabe exakt, Sig nicht leer, `len==152+build_info` |
| `sel4lake-loader/archive.rs` | `parse_never_panics` | `Archive::parse` (+ `program(i)`) panik-/OOB-frei |
| `sel4lake-loader/elf.rs` | `parse_never_panics` | `ElfImage::parse` panik-/OOB-/overflow-frei |
| | `segments_are_sound` | bei Erfolg: jedes Segment `memsz>=filesz`, `segment_bytes().len()==filesz`, `offset+filesz<=len` |
| `sel4lake-region/lib.rs` | `split_at_partitions` | `split_at` partitioniert exakt + lückenlos + nicht-überlappend, overflow-/underflow-frei |
| | `subview_within_parent` | `subview` liegt vollständig in der Eltern-Region (`off+sublen<=len`, kein Escape) |
| | `get_set_never_oob` | rohe `get`/`set` greifen (über echten Puffer) nie ausserhalb der Region zu |
| | `copy_fill_never_oob` | rohe `copy_from`/`copy_to`/`fill` bounds-respektierend (kein OOB) |
| `sel4lake-sync/lib.rs` | `spinlock_roundtrip` | `SpinLock`: Guard-Deref memory-safe, Lock/Unlock-Round-Trip (`next==serving`), Daten-Persistenz |
| | `rwlock_write_then_read` | `RwSpinLock`: write→read sieht den Wert; nach allen Drops Zustand `0` |
| | `rwlock_state_arithmetic` | Reader-Count + Writer-Bit over-/underflow-frei; Writer-Drop löscht **nur** das WRITER-Bit |

> **Reichweite bei `sel4lake-sync`:** Kani ist ein **single-threaded** Modellprüfer, **kein**
> Nebenläufigkeits-Checker. Bewiesen sind Memory-Safety der Guards, Single-Thread-Round-Trips +
> Daten-Persistenz und die Zähler-**Arithmetik**. Der **gegenseitige Ausschluss unter gleichzeitigem
> Mehrkern-Zugriff** (Interleavings) liegt **außerhalb** Kanis Reichweite — dafür wäre ein
> Concurrency-Modellprüfer (Loom/TLA+) nötig (mögliche spätere Ergänzung).

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

Zwei Workflows führen bei **jedem Push/PR** aus:
- `.gitea/workflows/kani.yml`: Host-Tests der Loader-Crate + Inert-Check + **alle Kani-Harnesses**.
- `.gitea/workflows/verus.yml`: installiert Verus (gepinntes Release + Z3) + **alle Verus-Beweise**
  (`tools/verus-verify.sh`).

Schlägt ein Beweis fehl (eine spätere Änderung verletzt eine bewiesene Eigenschaft), **schlägt die CI
fehl**. Damit können bereits bewiesene Eigenschaften — Kani **und** Verus — nicht unbeabsichtigt
verloren gehen; die Verifikation ist fester Bestandteil des Entwicklungsprozesses.

## Tier 2 (Pilot) — Verus: dokumentierte Invariante als formale Spezifikation

Der Unterschied zu Kani: Tier 1 beweist **Abwesenheit von Fehlern** (Panic/OOB/Overflow) bounded;
Tier 2 beweist **funktionale Korrektheit** deduktiv — dass eine Operation eine **Invariante erhält**,
für **alle** Zustände (unbeschränkt). Methode (vom Nutzer vorgegeben): **keinen** komplexen Bereich
(Scheduler/IPC), sondern eine **bereits dokumentierte Kernel-Invariante** als erste formale Spec.

Mehrere Pilotdateien spezifizieren **dokumentierte Kernel-Audits** und beweisen, dass die jeweiligen
Operationen die Invariante **erhalten** — statisch + für **alle** Zustände, nicht nur an den
Audit-Quiescenz-Punkten wie zur Laufzeit (`tools/verus-verify.sh`, gesamt **32 verified, 0 errors**
über 9 Dateien, über **sechs** der dokumentierten Audits — `cap_audit_cdt`, `domain_audit`,
`vspace_audit`, `dma_audit`, `loader_audit`, `trust_audit`; sechs verschiedene Invariantentypen:
Zählen, verkettete Struktur, Klassifikation, Permission, Geometrie, Hash-Konsistenz):

**(A) Refcount-Invariante** ([`verus/cap_cdt_refcount.rs`](../verus/cap_cdt_refcount.rs), Codes 1–3):
1. jeder belegte Slot zeigt auf ein gültiges, belegtes Objekt;
2. `refcount(o) == ` Anzahl belegter Slots, die auf `o` zeigen;
3. Objekt belegt **⟺** `refcount(o) > 0`.
Bewiesen für **`install`** (neues Objekt, `refcount=1`), **`copy`** (Slot + `refcount++`) und
**`delete`** (Slot löschen + `refcount--`, Objekt bei 0 freigeben). Kern: vier per Induktion bewiesene
Lemmas über die `refs_to`-Zählfunktion (push/update/member/fresh).

**(B) CDT-Sibling-Konsistenz** ([`verus/cap_cdt_tree.rs`](../verus/cap_cdt_tree.rs), Code 5): die
Geschwisterliste der Derivation-Tree ist eine **doppelt-verkettete Liste**, deren `next`/`prev`
**gegenseitige Inverse** sind (und nur auf gültige, belegte Knoten zeigen). Bewiesen für
**`insert_before`** (am Listenkopf einfügen) und **`unlink`** (Knoten entfernen, Nachbarn umhängen).

**(C) CDT-Strukturinvariante (vereint)** ([`verus/cap_cdt_structure.rs`](../verus/cap_cdt_structure.rs),
Codes 4-lokal + 5 + 6): ein Knotenmodell mit Objekt + allen vier Verkettungen (parent/first_child/
next/prev). Invariante: Ableitung **teilt das Objekt** (`object[parent]==object[s]`, 4-lokal),
Sibling-Inverse (5), `first_child` zeigt zurück + ist Listenkopf (`prev==None`, 6). Bewiesen für
**`derive`** (eine Capability ableiten = neues Kind am Kopf der Kinderliste). Kernidee: der bisherige
Kopf hat `prev==None`, wird also von keinem `next` referenziert → das Einhängen davor bricht nichts.

**(D) Domänen-Policy** ([`verus/domain_policy.rs`](../verus/domain_policy.rs), `domain_audit` Codes
1+2): Hardware-Caps (MMIO/IRQ/DMA) dürfen **nur** HardwareLand-PDs halten, Autoritäts-Caps
(PdControl/Loader) **nur** TrustedSas. Bewiesen, dass das Gate **`install_cap_checked`** (installiert
nur policy-konform) die Klassifikations-Invariante erhält — eine HW-Cap landet nie in einer
Nicht-HardwareLand-PD.

**(E) W^X** ([`verus/wx_invariant.rs`](../verus/wx_invariant.rs), `vspace_audit`): **keine** gemappte
EL0-Seite ist zugleich schreib- **und** ausführbar (Code-Integrität). Bewiesen für **`map_page`**
(mappt nur W^X-konform) und **`make_writable`** (W nur auf nicht-ausführbare Seiten).

**(F) DMA-Disjunktheit** ([`verus/dma_disjoint.rs`](../verus/dma_disjoint.rs), `dma_audit` Inv. 1):
kernel-ausgeschnittene DMA-Regionen sind **paarweise disjunkt** + disjunkt von der Kernel-Region
(Grundlage der DMA-Isolation). Bewiesen für **`alloc_region`** (hängt nur disjunkte Regionen ein).

**(G) CDT-Azyklizität** ([`verus/cap_cdt_acyclic.rs`](../verus/cap_cdt_acyclic.rs), `cap_audit_cdt`
Code 7 — die **schwierigste**): die Eltern-Kette hat **keinen Zyklus irgendeiner Länge**. Bewiesen
über ein **Wohlfundiertheits-Maß** (ein `rank`, der entlang `parent` strikt fällt → jede Kette
terminiert). `derive` erhält die Rang-Monotonie; das Lemma `ancestor_rank_decreases` (Induktion über
die Kettenlänge) liefert die **allgemeine** Aussage `not_own_ancestor`: kein Knoten ist sein eigener
`k`-ter Vorfahre, für **beliebiges** `k`.

**(H) Loader-Use-after-free-Schutz** ([`verus/loader_disjoint.rs`](../verus/loader_disjoint.rs),
`loader_audit`): kein geladenes Segment überlappt eine **freie** RAM-Region. Bewiesen für **`free_ram`**
(gibt RAM nur zurück, wenn disjunkt von allen Segmenten).

**(I) TrustedSAS-Key-DB-Konsistenz** ([`verus/trust_keydb.rs`](../verus/trust_keydb.rs), `trust_audit`):
die read-only Key-DB ist **selbst-zertifizierend** (`key_id == fingerprint(pubkey)`) + die `key_id`s
sind **eindeutig**. Bewiesen für **`add_key`** (`key_id := fingerprint(pubkey)`, nur bei freier key_id).
`fingerprint` ist uninterpretiert — der Beweis hängt nur von ihrer Funktionseigenschaft ab.

So werden `cap_audit_cdt`, `domain_audit`, `vspace_audit`, `dma_audit`, `loader_audit` + `trust_audit`
von zur Laufzeit **geprüften** zu **bewiesenen** Invarianten — sechs der dokumentierten Audits.

Lokal ausführen: `tools/verus-verify.sh` (Verus + Z3 aus dem Release nach `~/.verus`; geforderte
rustc-Toolchain via `rustup toolchain install`). Strategie/Stufenmodell: `ARMTest/formale-verifikation-aufwand.md`.

**Nächste Verus-Schritte (offen, schwieriger):** `delete_leaf` auf der vereinten Struktur (mehr
Fallunterscheidung als `derive`); die Kinderlisten-**Erreichbarkeit** aus Code 4 (Listen-Reachability);
die **Azyklizität** der Eltern-Kette (Code 7, braucht ein Wohlfundiertheits-Maß); dann die übrigen
Audits als eigene Modelle (`domain_audit`, W^X/`vspace_audit`, `dma_audit`); danach schrittweise
Richtung Scheduler/IPC.

> **Realistische Einordnung:** Dies ist eine **dauerhaft wachsende** Verifikation der dokumentierten
> Kernel-Invarianten, kein Einmal-Ziel. Volle funktionale Korrektheit + Isolation/Info-Flow + ein
> Hardware-Modell + die **echte SMP-Nebenläufigkeit** (von Verus single-threaded **nicht** erfasst)
> sind Forschungsklasse (Tier 3, mehrjährig — s. `ARMTest/formale-verifikation-aufwand.md`). Jede
> hier bewiesene Invariante ist ein abgeschlossener, CI-fähiger Baustein auf diesem Weg.

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
- [x] Region-Runtime (`sel4lake-region`: RegionView — Bounds-/Slice-/Overflow-Verträge um die `unsafe`-Blöcke)
- [x] Synchronisationsprimitive (`sel4lake-sync`: Memory-Safety/Round-Trip/Arithmetik, single-thread)
- [ ] Concurrency-Modellprüfung der Locks (Loom/TLA+ — außerhalb Kani)
- [ ] kernweite Overflow-/Arithmetik-Checks
