# Verifikation — Capability-System (Phase 1)

> **Status (2026-08-03, gemessen):** die **vollständige `cap_inv`** (Klauseln 1–7) + **vier** der
> sechs Operationen (`install`/`copy`/`mint`/`delete`) sind gegen sie bewiesen —
> `18 verified, 0 errors` in 3,0 s (Verus 0.2026.07.27.31579f0). Dazu der Erreichbarkeitssatz
> `unreachable_after_delete` (§10a). Verbleibend: `move` (allg.)/`revoke` über die
> **Reachability-Ausbaustufe** (§14). Dieses Dokument ist **eigenständig verständlich** — es
> erklärt die Verifikation vollständig **ohne Quellcode**.
>
> **Korrektur, die hier stehen bleibt.** Vom 2026-06-27 (`3384abb`, Commit-Titel „delete (Leaf)
> gegen die VOLLE cap_inv bewiesen") bis zum 2026-08-03 stand an dieser Stelle „10 verified" —
> und der Beweis war in Wahrheit **rot**: `9 verified, 1 ERRORS`, ein leerer `by {}`-Rumpf für die
> Eltern-Klausel plus `rlimit exceeded` für den Funktionsrumpf. Nachgemessen **auch gegen die
> damals in der CI gepinnte Verus-Fassung 0.2026.06.20.911e4e7** — identisch rot. Es war also
> kein Beweiser-Versionsartefakt: der Beweis war nie grün, 37 Tage lang, mit einem CI-Gate
> darüber. Was fehlte, war nicht der Beweis, sondern **jemand, der das Ergebnis liest**.

Bezug: [ADR 0015](../../docs/adr/0015-capability-system-formal-verification.md) (Architekturentscheidung
der Verifikation), ADR 0001 (Capabilities), `docs/verification.md` (Gesamtpipeline),
`docs/invariants.md` (Systeminvarianten), Laufzeit-Oracle `cap_audit_cdt`
(`crates/caprock-cap/src/space.rs`).

## 1. Motivation und Ziel

Capabilities sind in Caprock die **einzige Autoritätsquelle**: ein Subjekt darf genau das, wofür es
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
  die Kani-Beweise (`caprock-region`/`-sync`).

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
| **Vereint (1–7), volle `cap_inv`** | **install ✅ · copy ✅ · mint ✅ · delete ✅** · move/revoke ⏳ | [`proofs/cap_space.rs`](proofs/cap_space.rs) | C1 (additive) + C2b delete + Erreichbarkeit (**18 verified, 0 errors**, 3,0 s) · move/revoke laufend |

**`cap_inv` (Schritt B/C1):** die Konjunktion der Klauseln **1–3** (Refcount), **4-lokal** (Ableitung
teilt Objekt), **4-sib** (Geschwister teilen Elternknoten — beim Lösch-Beweis als notwendige, wahre
Klausel ergänzt), **5** (Sibling-Inverse), **6** (first_child = Listenkopf), **7** (Azyklizität via
`rank`). Bewiesen, dass **install** (Wurzel-Cap), **copy** (Kind ableiten + `refcount++`) und **mint**
(= copy mit Rechten/Badge, ausserhalb des Invariant-Modells) die **gesamte** `cap_inv` zugleich
erhalten — Refcount, Struktur **und** Azyklizität in je einem Beweis.

## 10. `delete` (Leaf) — was der Beweis wirklich brauchte

**Bewiesen ✅ (C2b, 2026-08-03).** Die frühere Fassung hatte die richtige *Idee* (Dekomposition in
per-Klausel-Asserts), aber die Dekomposition fand im **selben Funktionsrumpf** statt — also in
**einer** SMT-Query. Das ist keine Dekomposition, sondern eine Gliederung: die rlimit-Wand steht
nicht vor einer schwierigen Klausel, sondern vor der *Summe* aus vier Struktur-Klauseln und der
Refcount-Rechnung in einem Kontext. Die Klauseln liegen jetzt in **eigenen `proof fn`** —
`lemma_del_parent`/`_next`/`_prev`/`_first_child` —, jede mit ihrer eigenen Query und ihrem eigenen
Budget. Ergebnis: von *rlimit exceeded* auf **3,0 s** für die ganze Datei, ohne `#[verifier::rlimit]`.

**Der leere `by {}` war kein Beweisproblem, sondern eine fehlende Kette.** Die Eltern-Klausel gilt,
aber ihr Nachweis braucht fünf Schritte, die Z3 nicht selbst findet: (i) der gelöschte Slot ist tot,
also ist jeder lebende Slot `s` ein anderer; (ii) `s` lebte deshalb auch vorher; (iii) sein `parent`
ist unverändert (Rahmen-Lemma); (iv) das Ziel `p` ist **nicht** der gelöschte Slot — das ist der
einzige Punkt, an dem `no_children` gebraucht wird; (v) `object`/`rank` von `p` stehen still. Ohne
(iv) ist die Klausel schlicht falsch.

**Der Eingriff steht jetzt als Spezifikation da, nicht als Zuweisungsfolge im Rumpf:**
`unlink1`/`unlink2`/`unlink_slots` bilden `CapSpace::unlink` Schritt für Schritt ab — gleiche
Verzweigung, gleiche Reihenfolge, gleiche Feldzuweisung (früher wich das Modell an zwei Stellen ab,
s. §12).

### 10a. Kinderlisten-Erreichbarkeit — `unreachable_after_delete`

Ein eigener Satz, mit **zwei** Hälften, weil die erste ohne die zweite wertlos ist:

1. **kein lebender Slot zeigt noch auf den gelöschten** — über **keine** der vier Kanten
   (`parent`, `next`, `prev`, `first_child`); und der Slot selbst ist tot.
2. **der Elternknoten jedes anderen Slots ist unverändert.**

Ohne (2) bestünde ein Eingriff, der sauber aushängt und nebenbei ein fremdes Kind umhängt, die
Prüfung (1) mühelos. Gemessen: eine Mutation, die genau das tut (`unlink` löscht nebenbei den
`parent` des Nachfolgers), lässt den Beweis fallen.

## 10b. Noch offene Eigenschaften

- **`move`** (Index-Relokation: `dst` erbt den Inhalt, alle Verweise auf `src` werden auf `dst`
  umgebogen, `src` geleert; `refcount` unverändert). **Befund:** der **Leaf-Fall** ist wie `delete`
  dekomponierbar (Erhaltungs-Hilfsfakt + per-Klausel-Asserts + `no_children`), aber die strukturellen
  Klauseln brauchen für die **Relokation** (`dst` übernimmt `src`s Verkettung) zusätzliche
  Fall-Hinweise (dst/pv/nx); **allgemeines** `move` (Cap **mit** Kindern) erfordert — wie `revoke` —
  die unbeschränkte Kind-`parent`-Umbiegung, also die Reachability-Iteration.
- **`revoke`** (Teilbaum löschen) — die anspruchsvollste; braucht die **Reachability-Iteration** über
  den Teilbaum (rekursive Spec + Erhaltung).
- **Code 4r (Kinderlisten-Reachability):** `s ∈ p`s Kinderliste über Listen-Traversierung — bisher als
  **Vorbedingung** geführt (s. o.); ihr expliziter Nachweis (rekursive Spec + Erhaltung durch alle
  Operationen) ist die **letzte Ausbaustufe** der Phase und Voraussetzung für `revoke`.

## 10c. Empfindlichkeit — gemessen, nicht behauptet

Ein Beweis, durch den eine Mutation durchgeht, hat dort eine Lücke. Neun Mutationen, **acht
gefallen**:

| # | Mutation | Ausgang |
|---|---|---|
| M1 | `next[pv]` wird nicht fortgeschrieben | **gefallen** |
| M2 | `first_child[par]` wird nicht nachgezogen | **gefallen** |
| M3 | `prev[nx]` wird nicht fortgeschrieben | **gefallen** |
| M4 | Blatt-Vorbedingung `first_child is None` entfernt | *durchgegangen* — s. u. |
| M5 | `no_children` entfernt (Blatt-Vorbedingung bleibt) | **gefallen** (Nachbedingung + Assert) |
| M6 | `cap_inv` (6) ohne `prev[first_child] is None` | **gefallen** |
| M7 | `cap_inv` (4-sib) ohne geteilten Elternknoten | **gefallen** |
| M8 | `unlink` hängt den Nachfolger nebenbei um (`parent := None`) | **gefallen** |
| M9 | Slot wird zuerst geleert statt zuletzt | **gefallen** |

**M4 ist keine Lücke, sondern eine Redundanz — und sie ist bewiesen.** `first_child is None` folgt
aus `no_children` + Klausel 6: hätte das Blatt ein `first_child`, so hätte dieses Kind `parent ==
Some(i)`, was `no_children` verbietet. Der Beweis dafür steht als `lemma_leaf_from_no_children` in
derselben Datei. Die Vorbedingung bleibt trotzdem stehen, weil sie das ist, was der reale Code an
dieser Stelle prüft. Das Paar M4/M5 zeigt die Ordnung der beiden Vorbedingungen: `no_children`
allein trägt, die Blatt-Eigenschaft allein trägt **nicht**.

## 11. Bekannte Grenzen der aktuellen Beweise

- **Abstraktes Modell, nicht der reale Code:** die Beweise gelten am Modell. Die Treue zum echten
  `caprock-cap` war bis 2026-08-03 eine **dokumentierte Annahme**; seither hält sie
  `tools/verus-modelltreue.sh` (s. §12) — ein normalisierter Strukturvergleich mit Selbsttest.
  Was er **nicht** leistet: er vergleicht Verzweigung und Feldzuweisung, nicht die Bedeutung.
  Ein Umbau, der beide Seiten gleichartig verfälscht, käme durch.
- **Sequenziell:** Verus modelliert **keine** Nebenläufigkeit; gleichzeitige Mehrkern-Operationen sind
  außerhalb (durch den `CAPS`-RwLock serialisiert — dessen Korrektheit ist HAL/Concurrency-TCB).
- **Beschränkte Datentypen:** `refcount`/Indizes als `nat` (kein Überlauf im Modell); der reale Code
  nutzt `u32` mit dokumentierter Generations-/Überlauf-Behandlung.

## 12. Vertrauensannahmen (Trusted Computing Base)

1. **Modell-Treue:** das Verus-Modell bildet die reale `CapSpace`-Struktur + Operationen korrekt ab.
   *Absicherung, seit 2026-08-03 nicht mehr nur Prosa:* **`tools/verus-modelltreue.sh`** reduziert
   `crates/caprock-cap/src/space.rs::unlink`/`delete_leaf` **und** `unlink1`/`unlink2`/
   `unlink_slots` auf dieselbe normalisierte Ereignisfolge (Verzweigung + Feldzuweisung) und
   verlangt Gleichheit — heute 12 Ereignisse, deckungsgleich. Ein `match Option {Some/None}` und
   ein `if … is Some { } else { }` fallen dabei auf dieselbe Form; der Dialektunterschied
   verschwindet, die Struktur bleibt stehen. Der Wächter hat einen **Selbsttest** (8 Fälle): sieben
   Mutationen — vier am echten Code, drei am Modell — müssen ihn auslösen, und eine **kosmetische
   Änderung auf beiden Seiten** (Binder umbenannt, Kommentare, Leerzeilen) darf ihn **nicht**
   auslösen. Ein Wächter, der immer schreit, wird abgeschaltet; einer, der nie schreit, ist eine
   Kopie mit Zertifikat.
   *Zusätzlich:* dieselbe Invariante wird vom Laufzeit-`cap_audit_cdt` + den Fuzzern auf dem
   **echten** Code geprüft (mehrschichtig).

   **Zwei Abweichungen, die der Wächter beim ersten Lauf gefunden hat** (beide jetzt behoben, indem
   das *Modell* dem Code angeglichen wurde):
   - Das Modell zog `first_child[par]` nach, wenn `first_child[par] == Some(i)` galt. Der Code prüft
     das **nicht**: er schreibt, sobald `prev is None && parent is Some`. Unter `cap_inv` **allein**
     ist das nicht dasselbe — Klausel 6 sagt nur die Gegenrichtung; erst die Reachability-Klausel
     4r macht beides gleich. `cap_inv` bleibt in beiden Fassungen erhalten; der Beweis führt den
     Fall jetzt mit.
   - Das Modell leerte den Slot **zuerst**, der Code **zuletzt**. Bei einem Selbst-Geschwister
     (`next[i] == Some(i)`, von `cap_inv` nicht ausgeschlossen) sind das verschiedene Endzustände.
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

**Und eine Ebene, die vorher fehlte:** ein grüner Beweis kann per Konstruktion **nicht** bemerken,
dass sich der Code unter ihm bewegt hat — das Modell ändert sich ja nicht mit. Genau deshalb ist
`tools/verus-modelltreue.sh` kein Beiwerk, sondern die Ebene, auf der die anderen ruhen. Sie läuft
am Ende von `tools/verus-verify.sh` und damit im CI-Gate mit.

## 14. Nächste Ausbaustufen

**Phase-1-Kern abgeschlossen:** die vollständige `cap_inv` (Klauseln 1–7) + die vier Operationen
`install`/`copy`/`mint`/`delete` sind gegen sie bewiesen. Verbleibend ist die **Reachability-
Ausbaustufe**, die `move` (allgemein) + `revoke` erst beweisbar macht:

### Reachability (Code 4r) — Roadmap der finalen Ausbaustufe

Die Zeiger-CDT braucht für `move`/`revoke` zusätzliche **Wohlgeformtheits-Klauseln**, die `cap_inv`
(lokal) bewusst noch **nicht** erzwingt (sie wurden bei den Lösch-/Relokations-Versuchen als fehlend
identifiziert):
- **keine Selbst-Geschwister** (`next[s] != Some(s)`, `prev[s] != Some(s)`),
- **endliche/azyklische Geschwisterlisten** (eine Sibling-Kette kehrt nicht zurück),
- **Mitgliedschaft (4r):** jeder Knoten mit `parent==Some(p)` ist von `p.first_child` über
  `next`-Schritte **erreichbar**.

**Lösungsansatz (analog zum `rank`-Trick für die Azyklizität):** ein Ghost-Feld **`sib_pos: nat`** je
Slot mit der Invariante `next[s]==Some(n) ⟹ sib_pos[n] == sib_pos[s]+1` und `first_child[p]==Some(c) ⟹
sib_pos[c]==0`. Das macht Geschwisterlisten **wohlfundiert** (Position steigt strikt → endlich, kein
Zyklus, kein Selbst-Geschwister) und trägt die Mitgliedschaft. Damit werden:
- **`move` (allgemein)** — die Kind-`parent`-Umbiegung über die (nun endliche) Kinderliste,
- **`revoke`** — die Teilbaum-Iteration (rekursiv über `first_child`/`next`, terminiert dank `sib_pos`)

beweisbar. Aufwand: eigener mehrstufiger Block (die anspruchsvollste CDT-Verifikationsstufe, vgl.
seL4s MDB-Beweise).

### Phasenübergreifend

- Modell↔Code-Bindung (Richtung reale `caprock-cap`-Implementierung).
- Danach **Phase 2 (Loader)**, **Phase 3 (Region-Runtime)**, … (s. `Verification/README.md`).
