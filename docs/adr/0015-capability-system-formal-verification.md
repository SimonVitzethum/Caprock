# ADR 0015 — Formale funktionale Verifikation des Capability-Systems (Verus)

Status: **angenommen** · Datum: 2026-06-27 · Phase: funktionale Verifikation, **Phase 1**
(Capability-System — die erste vollständig formal verifizierte Kernel-Komponente).

Bezug: [ADR 0001 (Capabilities)], `docs/verification.md` (Tier-1/2-Pipeline),
`ARMTest/formale-verifikation-aufwand.md` (Stufenmodell), `Verification/capability-system/`
(komponentenlokale Doku). Die Verus-Pilotdateien (`verus/cap_cdt_*.rs`) sind die Vorarbeit.

## Kontext

Tier 1 (Kani) sichert Speichersicherheit/Panik-Freiheit der klar abgegrenzten Komponenten; ein
Verus-Pilot hat bereits **Teile** der `cap_audit_cdt`-Invariante bewiesen — aber in **vier getrennten
Modellen** (Refcount, Sibling-Liste, Struktur, Azyklizität), jeweils gegen **einen** Invarianten-
Aspekt. Phase 1 hebt das auf **funktionale Korrektheit der gesamten Komponente**: jede Capability-
Operation erhält die **vollständige** `cap_audit_cdt`-Invariante.

**Randbedingungen (Nutzer-Vorgabe):**
- Die bestehende Architektur wird **nicht verändert**; Runtime-Audits, Kani, Fuzzer werden **ergänzt**,
  nicht ersetzt → **mehrschichtige** Verifikation.
- Die **HAL** (MMIO, Kontextwechsel, Seitentabellen, Hardwarezugriffe) bleibt außerhalb der
  funktionalen Verifikation — dokumentierte `// SAFETY:`-Verträge + kleine TCB.
- Methodik je Phase: Analyse → Variantenvergleich → ADR → Spezifikation → Implementierung → Beweise →
  Doku → Integration → Build → Validierung → Commit.

## Analyse (Ist-Stand)

Reales System (`crates/sel4lake-cap/src/space.rs`): `CapSpace` = Slot-Tabelle (`Slot{used, object, mdb}`
mit `mdb = {parent, first_child, next_sibling, prev_sibling}`) + Objekt-Tabelle (`Object{used, refcount,
gen, …}`). Operationen: `install_*` (Wurzel-Cap auf neues Objekt), `copy`/`mint` (Kind ableiten,
refcount++), `move`, `delete`/`delete_leaf`, `revoke` (Teilbaum löschen). Laufzeit-Oracle
`cap_audit_cdt` (Codes 1–7) prüft an Quiescenz-Punkten:
1–3 Refcount-Integrität · 4 parent gültig + teilt Objekt + Kinderlisten-Mitgliedschaft · 5 Sibling-
Inverse · 6 first_child-Konsistenz · 7 keine Eltern-Zyklen.

Bereits bewiesen (Pilot, getrennte Modelle): Refcount install/copy/delete; Sibling insert/unlink;
Struktur (4-lokal+5+6) derive; Azyklizität (rank-Zertifikat) derive + allgemein `not_own_ancestor`.

## Variantenvergleich

**Frage 1 — Modell-Granularität:**

| Variante | Beschreibung | Pro | Contra |
|---|---|---|---|
| V1 getrennte Modelle | wie Pilot: je Aspekt ein Datentyp + Invariante | einfachste Einzelbeweise | beweist **nicht**, dass *eine* Operation **alle** Invarianten zugleich erhält (Interaktionen entgehen) |
| **V2 vereintes Modell, volle Invariante (gewählt)** | **ein** `CapSpace`-Datentyp, Invariante = **Konjunktion** aller Klauseln (Refcount + Struktur + Azyklizität); jede Operation gegen die **volle** Invariante | erfasst Operationen-Interaktionen; entspricht exakt `cap_audit_cdt`; „erste **vollständig** verifizierte Komponente" | aufwändiger (revoke über Teilbäume gegen die Konjunktion) |
| V3 reale-Code-Annotation | `sel4lake-cap` direkt mit Verus annotieren | kein Modell↔Code-Gap | **verändert die Architektur** (Nutzer-Verbot); Verus auf den realen Fixed-Arrays + generischem Code ist deutlich aufwändiger |

**Frage 2 — Modell vs. realer Code:** Da die Architektur unverändert bleiben soll, wird ein
**faithful abstraktes Modell** des `CapSpace` gebaut (mirror der realen Datenstruktur + Operationen),
die Invarianten am Modell bewiesen, und die **Modell↔Code-Korrespondenz dokumentiert**. Die
**Treue** des Modells ist eine Vertrauensannahme (TCB) — **abgesichert** durch die parallel laufenden
Runtime-Audits + Fuzzer auf dem **echten** Code (mehrschichtig: Verus beweist das Modell, Audit+Fuzzer
prüfen die Implementierung gegen dasselbe Invariant).

## Entscheidung

1. **V2 — ein vereintes `CapSpace`-Modell** mit der **vollständigen** `cap_audit_cdt`-Invariante als
   Konjunktion (Refcount 1–3 + Struktur 4–6 + Azyklizität 7), inkremental aufgebaut.
2. **Alle sechs Operationen** (`install`, `copy`, `mint`, `move`, `delete`, `revoke`) werden gegen die
   **volle** Invariante bewiesen — jede einzeln, committbar.
3. **Abstraktes Modell**, faithful zum realen Code; Korrespondenz + Treue-Annahme dokumentiert; durch
   Audit+Fuzzer auf dem echten Code abgesichert. **Realer Kernel-Code unverändert.**
4. Azyklizität via **Wohlfundiertheits-Maß** (`rank`, entlang `parent` strikt fallend) — etablierte
   Technik aus dem Pilot.
5. Komponenten-Doku unter `Verification/capability-system/` (eigenständig verständlich ohne Quellcode);
   Integration in `tools/verus-verify.sh` + das Verus-CI-Gate.

## Konsequenzen

- **Positiv:** die erste Kernel-Komponente mit maschinell bewiesener funktionaler Korrektheit ihrer
  Sicherheits-Invariante; `cap_audit_cdt` wird von „geprüft" zu „bewiesen"; mehrschichtige Absicherung
  (Audit+Fuzzer+Kani+Verus). Methode skaliert auf Loader/Region/IPC (Phasen 2–4).
- **Kosten/Grenzen:** der Modell↔Code-Gap bleibt (TCB-Annahme „Modell ist faithful"), bewusst durch
  Runtime-Audits/Fuzzer abgefedert; die **Reachability**-Teilklausel von Code 4 (s ∈ Kinderliste von p
  über Listen-Traversierung) ist die anspruchsvollste und wird ggf. als letzte Ausbaustufe der Phase
  geführt. Concurrency/SMP/HAL bleiben außerhalb (Hardware-Vertrauensgrenze).

## Plan (Phase 1, committbare Schritte)

- **A (dieser Commit):** Analyse · Variantenvergleich · ADR · Plan · `Verification/capability-system/`-
  Scaffold mit der vollständigen Doku-Struktur.
- **B:** vereintes `CapSpace`-Modell + volle Invariante (`cap_inv`) als eine `spec fn`; die Pilot-Lemmas
  (refs_to push/update/member/fresh; rank/ancestor) konsolidiert.
- **C:** Operationen gegen die volle Invariante beweisen — `install`, `copy`, `mint` (= copy + Rechte/
  Badge), `move`, `delete` (Leaf), `revoke` (Teilbaum). Jede Operation einzeln committet.
- **D:** Doku vervollständigen (alle Lemmas/Eigenschaften/offene Punkte/TCB) · in Runner+CI integrieren ·
  Build · Validierung (`verus-verify.sh` grün, test-qemu unverändert grün) · Abschluss-Commit.
