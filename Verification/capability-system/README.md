# Verifikation — Capability-System (Phase 1)

> **Status:** in Arbeit · Schritt A (Analyse/ADR/Plan/Scaffold) abgeschlossen · Schritte B–D folgen.
> Dieses Dokument ist **eigenständig verständlich**: es erklärt die formale Verifikation des
> Capability-Systems vollständig, **ohne dass der Quellcode gelesen werden muss**.

Bezug: [ADR 0015](../../docs/adr/0015-capability-system-formal-verification.md) (Architekturentscheidung
der Verifikation), ADR 0001 (Capabilities), `docs/verification.md` (Gesamtpipeline),
`docs/invariants.md` (Systeminvarianten), Laufzeit-Oracle `cap_audit_cdt`
(`crates/sel4lake-cap/src/space.rs`).

## 1. Motivation und Ziel

Capabilities sind in SEL4Lake die **einzige Autoritätsquelle**: ein Subjekt darf genau das, wofür es
eine Capability hält. Die Integrität der Capability-Tabelle + des Capability-Derivation-Tree (CDT) ist
damit die **Wurzel der gesamten Sicherheit**. Bisher wird sie zur Laufzeit vom Audit `cap_audit_cdt`
(an Quiescenz-Punkten) + von Fuzzern geprüft. **Ziel von Phase 1:** diese Invariante **statisch +
für alle Zustände beweisen** — das Capability-System wird die **erste vollständig formal verifizierte
Kernel-Komponente**. Jede Capability-Operation wird bewiesen, die **vollständige** CDT-Invariante zu
erhalten.

## 2. Sicherheitsmodell

- **Subjekte** halten Capabilities in **Slots** ihres Capability-Space.
- Eine Capability verweist auf ein **Objekt** (Endpoint, Memory, Tcb, …) mit einem **Referenzzähler**.
- Capabilities werden **abgeleitet** (`copy`/`mint`/`install`): die Ableitung bildet einen **Baum**
  (CDT) — Wurzeln (frische Objekte) und Kinder (abgeleitete Caps auf dasselbe Objekt). Der CDT trägt
  `parent`/`first_child`/`next_sibling`/`prev_sibling`.
- **Sicherheitsgarantie:** keine Autorität entsteht aus dem Nichts oder verschwindet unbemerkt; die
  Buchhaltung (Refcounts, Baumstruktur) ist stets konsistent — sonst wären Use-after-free, doppelte
  Freigabe oder Autoritäts-Leck möglich.
- **Vertrauensgrenze:** die **HAL** (Speicher, Hardware) ist außerhalb — das Modell arbeitet auf der
  logischen Tabellen-/Baum-Ebene; die physische Speichersicherheit der Tabellen trägt die HAL-TCB +
  die Kani-Beweise (`sel4lake-region`/`-sync`).

## 3. Zu beweisende Invarianten (= `cap_audit_cdt`, Codes 1–7)

| # | Invariante | Bedeutung |
|---|---|---|
| 1 | jeder belegte Slot zeigt auf ein gültiges, belegtes Objekt | keine baumelnden Referenzen |
| 2 | `refcount(o) == ` Anzahl belegter Slots, die auf `o` zeigen | exakte Buchhaltung |
| 3 | Objekt belegt **⟺** `refcount(o) > 0` | keine verlorenen/geisterhaften Objekte |
| 4l | `parent==Some(p)` ⟹ `p` belegt + `object[p]==object[s]` | Ableitung **teilt das Objekt** |
| 4r | `parent==Some(p)` ⟹ `s` ist in `p`s Kinderliste | Eltern↔Kinderliste konsistent (Reachability) |
| 5 | `next`/`prev` gegenseitige Inverse, gültig+belegt | Sibling-Liste konsistent |
| 6 | `first_child==Some(c)` ⟹ `c` belegt, `parent[c]==Some(s)`, `prev[c]==None` | Kopf der Kinderliste |
| 7 | keine Zyklen in der Eltern-Kette | CDT ist ein **Baum** |

**Abgeleitete Sicherheits-Eigenschaften** (Ziel-Aussagen): „keine Capability entsteht aus dem Nichts",
„keine Capability verschwindet unbeabsichtigt", „Refcounts bleiben korrekt", „Parent/Child + Sibling
konsistent", „keine Zyklen", „jede Ableitung referenziert dasselbe Objekt".

## 4. Bezug zu ADRs

- **ADR 0015** — diese Verifikation (Variantenvergleich, Entscheidung V2: vereintes Modell + volle
  Invariante; abstraktes Modell, realer Code unverändert).
- **ADR 0001** — das Capability-System selbst (CDT, Refcounts, Rechte).
- `docs/adr/0007-security-domains.md` (Domänen-Policy, separate Komponente).

## 5. Formale Spezifikation

Das Modell spiegelt den realen `CapSpace`:

```text
Object  = { used: bool, refcount: nat }
Slot    = { used: bool, object: nat,
            parent, first_child, next_sibling, prev_sibling: Option<nat>,
            rank: nat }          // rank = Wohlfundiertheits-Mass (Ghost) fuer die Azyklizitaet
CapSpace = { objects: Seq<Object>, slots: Seq<Slot> }
```

Die **Gesamtinvariante** `cap_inv(cs)` ist die **Konjunktion** der Klauseln 1–7 (s. §3), formuliert als
`spec fn` über `objects`/`slots`. Hilfs-Spezifikation: `refs_to(slots, o)` = Anzahl belegter Slots, die
auf `o` zeigen (rekursiv); `ancestor(cs, s, k)` = `k`-ter Vorfahre entlang `parent`.

> **Schritt B (umgesetzt):** die Spezifikation ist als **eine** vereinte `cap_inv` in
> [`proofs/cap_space.rs`](proofs/cap_space.rs) implementiert (Konjunktion aller Klauseln 1–7);
> `install` ist gegen die **volle** `cap_inv` bewiesen. Schritt C ergänzt copy/mint/move/delete/revoke.

## 6. Verus-Architektur

- Eigenständige `.rs`-Dateien (Verus `verus!{}`-Blöcke), verifiziert per `tools/verus-verify.sh`
  (umgeht den build-std-Zwang via Standalone-Aufruf; Verus-Binary aus dem gepinnten Release).
- **Keine** Änderung am realen Kernel-Code — reines Modell + Beweis.
- Integration: das Verus-CI-Gate (`.gitea/workflows/verus.yml`) führt alle Beweise bei jedem Push aus.
- Ablage der Phase-1-Beweise: `Verification/capability-system/proofs/` (Ziel; aktuell noch unter
  `verus/cap_cdt_*.rs`, wird in Schritt B hierher konsolidiert).

## 7. Beweisstrategie

- **Invarianten-Erhaltung:** für jede Operation `op`: `requires cap_inv(cs) ∧ <Vorbedingungen>` ⟹
  `ensures cap_inv(op(cs))`. Verus (SMT) schließt die Klauseln; per `assert forall … by { lemma … }`
  werden die Zähl-/Struktur-Argumente geführt.
- **Zählen (Refcount):** Induktions-Lemmas über `refs_to` (Effekt von push/update eines Slots).
- **Struktur (Sibling/Parent/Child):** lokale Konsistenz + die Kopf-Kopplung (`first_child` ⟹
  `prev==None`), die das Einfügen/Entfernen beweisbar macht.
- **Azyklizität:** **Wohlfundiertheits-Maß** `rank` (Eltern strikt kleiner) ⟹ keine Eltern-Kette
  kehrt zurück; Induktion über die Kettenlänge liefert die allgemeine Aussage.

## 8. Beschreibung sämtlicher Lemmas

*(Stand Schritt A — aus dem Pilot; wird in Schritt B konsolidiert/erweitert.)*

- `lemma_refs_push(slots, sl, o)` — Anhängen eines Slots ändert `refs_to(o)` um `contrib(sl, o)`.
- `lemma_refs_update(slots, i, sl, o)` — Ersetzen von Slot `i` verschiebt `refs_to(o)` um die
  Beitragsdifferenz (Induktion).
- `lemma_refs_member(slots, s, o)` — zeigt ein belegter Slot auf `o`, so `refs_to(o) >= 1` (Induktion).
- `lemma_refs_fresh(slots, len, o)` — ein frisches Objekt (`o >= len`) hat `refs_to == 0` (Induktion).
- `ancestor_rank_decreases(cs, s, k)` — jeder echte `k`-Vorfahre hat strikt kleineren Rang (Induktion).
- `not_own_ancestor / no_self_parent / no_2cycle` — Azyklizitäts-Korollare aus der Rang-Monotonie.

## 9. Bewiesene Eigenschaften (Verifikationsfortschritt)

| Aspekt | Operationen bewiesen | Datei (aktuell) | Status |
|---|---|---|---|
| Refcount (1–3) | install, copy, delete | `verus/cap_cdt_refcount.rs` | ✅ bewiesen (9 verified) |
| Sibling (5) | insert_before, unlink | `verus/cap_cdt_tree.rs` | ✅ bewiesen (3 verified) |
| Struktur (4l+5+6) | derive | `verus/cap_cdt_structure.rs` | ✅ bewiesen (2 verified) |
| Azyklizität (7) | derive (+ allg. Korollare) | `verus/cap_cdt_acyclic.rs` | ✅ bewiesen (7 verified) |
| **Vereint (1–7), volle `cap_inv`** | **install ✅ · copy ✅ · mint ✅** · move, delete, revoke ⏳ | [`proofs/cap_space.rs`](proofs/cap_space.rs) | ⏳ B fertig · C laufend (install/copy/mint, 9 verified) |

## 10. Noch offene Eigenschaften

- **Vereinte Invariante:** dass *eine* Operation **alle** Klauseln **zugleich** erhält (Schritt B/C) —
  die Pilot-Beweise sind je Aspekt getrennt.
- **Operationen** `mint`, `move`, `revoke` (Schritt C).
- **Code 4r (Kinderlisten-Reachability):** `s ∈ p`s Kinderliste über Listen-Traversierung — die
  anspruchsvollste Klausel (Reachability); ggf. letzte Ausbaustufe der Phase.

## 11. Bekannte Grenzen der aktuellen Beweise

- **Abstraktes Modell, nicht der reale Code:** die Beweise gelten am Modell; die Treue zum echten
  `sel4lake-cap` ist eine dokumentierte Annahme (s. §12), abgesichert durch Audit+Fuzzer.
- **Sequenziell:** Verus modelliert **keine** Nebenläufigkeit; gleichzeitige Mehrkern-Operationen sind
  außerhalb (durch den `CAPS`-RwLock serialisiert — dessen Korrektheit ist HAL/Concurrency-TCB).
- **Beschränkte Datentypen:** `refcount`/Indizes als `nat` (kein Überlauf im Modell); der reale Code
  nutzt `u32` mit dokumentierter Generations-/Überlauf-Behandlung.

## 12. Vertrauensannahmen (Trusted Computing Base)

1. **Modell-Treue:** das Verus-Modell bildet die reale `CapSpace`-Struktur + Operationen korrekt ab.
   *Absicherung:* dieselbe Invariante wird vom Laufzeit-`cap_audit_cdt` + den Fuzzern auf dem **echten**
   Code geprüft (mehrschichtig).
2. **HAL/Speicher:** die physische Integrität der Tabellen (kein fremder Schreibzugriff) trägt die
   HAL-TCB + die Kani-Beweise (`region`/`sync` speichersicher).
3. **Serialisierung:** Operationen laufen unter dem `CAPS`-RwLock (keine Daten-Races) — Concurrency-TCB.

## 13. Verbindung zu Runtime-Audits und Kani

**Mehrschichtige Verifikation** (die Ebenen sichern sich gegenseitig):
- **Runtime-Audit** `cap_audit_cdt` — prüft die Invariante an Quiescenz-Punkten auf dem echten Code.
- **Fuzzer** (`fuzz`/`ipcfuzz`) — randomisierte Op-Sequenzen + Audit nach jeder Epoche.
- **Kani** — Speichersicherheit der Tabellen-tragenden `unsafe`-Schicht (`region`/`sync`).
- **Verus (hier)** — beweist, dass die Operationen die Invariante **immer** erhalten (nicht nur an
  Audit-Punkten). Verus ersetzt **nichts** — es ergänzt die obersten drei Ebenen um einen Beweis.

## 14. Nächste Ausbaustufen

- **Schritt B:** vereintes `CapSpace`-Modell + volle `cap_inv`.
- **Schritt C:** `install`/`copy`/`mint`/`move`/`delete`/`revoke` gegen die volle Invariante.
- **Schritt D:** Doku-Vervollständigung + CI-Integration + Validierung + Abschluss-Commit.
- **Später (Phase-übergreifend):** Code 4r-Reachability; Modell↔Code-Bindung (Richtung reale
  Implementierung); danach Phase 2 (Loader).
