# SPARK-Experiment am Cap-Space

**Die Frage:** findet GNATprove auf Silver Level (Abwesenheit von Laufzeitfehlern) am
Cap-Space etwas, das das vorhandene Verus-Modell **nicht** sieht?

**Die Antwort: ja, 15 Stellen — und die schaerfste steckt im Pruefer selbst.**

Lauf: `tools/spark-beweis.sh` (Ratsche + Gegenprobe, faellt bei Mutation durch).

## Werkzeugkette

Ohne root, vollstaendig benutzerlokal:

```
alr install gnatprove     # ~/.alire/bin/gnatprove   FSF 16.1.0, Why3 1.8.2+git
alr install gnat_native   # ~/.alire/bin/gnatmake    16.1.0
export PATH="$HOME/.alire/bin:$PATH"
```
Beweiser im Bundle: alt-ergo 2.6.1, cvc5 1.3.2, z3 4.15.4.

## Die zwei Kennzahlen

| | |
|---|---|
| **SPARK-Abdeckung** | **34 von 34** Unterprogrammen unter `SPARK_Mode => On`. **Kein einziges `SPARK_Mode => Off`.** Der Cap-Space ist reine Datenstruktur — es gibt nichts, was der Beweiser nicht ansehen kann |
| **Bilanz** | 294 Pruefungen gesamt, **279 bewiesen, 15 unbewiesen (5 %)**. Davon Laufzeitpruefungen: **99 gesamt, 84 bewiesen, 15 unbewiesen** |

Die 15 sind der Rest **nach** dem Aufraeumen: der erste Lauf hatte 39. Die 24 Differenz waren
fehlende Schleifeninvarianten und fehlende Nachbedingungen an internen Helfern — also Befunde
ueber die Portierung, nicht ueber Caprock. Sie sind einzeln beseitigt, und zwar durch
`Loop_Invariant`/`Post`, die GNATprove **selbst nachweist**; keine davon ist eine Annahme.

## Warum das Verus-Modell keine einzige dieser 15 sieht

Gemessen, nicht behauptet (Vorkommenszaehlung ueber `verus/cap_cdt_*.rs`):

| Groesse | in Verus |
|---|---|
| `refcount` | nur in `cap_cdt_refcount.rs` (21x) — als **`nat`**, unbeschraenkt |
| `parent`/Verkettung | nur in `cap_cdt_structure.rs` (9x) + `cap_cdt_acyclic.rs` (24x) |
| **beides im selben Zustand** | **nirgends** — `cap_cdt_refcount.rs` hat 0 Vorkommen von `parent`, die Strukturdateien 0 von `refcount` |
| `gen` (Generation) | **kommt nicht vor** |
| `Finalized` | **kommt nicht vor** |
| `rights` / `badge` | **kommt nicht vor** |
| `move_cap` | **kommt nicht vor** |

Daraus folgen drei strukturelle Luecken:

1. **`nat` gegen `u32`.** Ein `nat` laeuft weder ueber noch unter. Ueber
   `refcount -= 1` und `refcount += 1` kann das Modell deshalb **nichts** aussagen — nicht
   „es ist sicher", sondern „die Frage existiert dort nicht".
2. **`Seq` gegen Tabelle mit Schranke.** Verus setzt `live(c,i) := i < len && used` an und
   fuehrt es als Invariante mit; der **Code** dagegen indiziert roh. Die Bruecke zwischen
   „das Modell haelt die Invariante" und „diese Zeile indiziert innerhalb der Tabelle"
   zieht niemand — auch der Modelltreue-Waechter nicht, der Namen abgleicht, keine Indizes.
3. **`delete_leaf` hat kein Gegenstueck.** Es senkt den Refcount **und** haengt aus. Verus'
   `delete` (Refcount-Datei) beruehrt keine Verkettung, Verus' `unlink` (Listen-Datei)
   beruehrt keinen Refcount. Die **zusammengesetzte** Operation ist nirgends bewiesen.

## Die 15 Funde

Alle sind am Rust-Original gegengeprueft; die Rust-Zeile steht jeweils als Kommentar `[Fn]`
in `src/caprock_cap.adb`.

### Klasse A — nichts im Code stuetzt sie

| | Ort (Rust) | Pruefung |
|---|---|---|
| F1 | `link_child`: `self.slots[f]` | Index, `f` aus `first_child` |
| F2–F4 | `unlink`: `self.slots[p]` / `[par]` / `[n]` | Index, alle drei aus dem `Mdb` |
| F5 | `delete_leaf`: `self.objects[obj]` | Index, `obj` roher `usize` aus dem Slot |
| **F6** | `delete_leaf`: `refcount -= 1` | **Unterlauf, ohne jede Bedingung** |
| F7 | `copy`: `self.objects[obj]` | Index |
| F8 | `copy`: `refcount += 1` | Ueberlauf |
| F9–F12 | `move_cap`: vier rohe Kettenindizes | Index |
| **F13** | `audit_cdt`: `self.slots[ci]` in der Kinderliste | **s. unten** |
| F15 | `inspect`: `self.objects[obj]` | Index |

### Klasse B — sicher, aber nur ueber ein Argument, das nirgends steht

| | Ort | warum sicher |
|---|---|---|
| F14 | `audit_cdt`: `self.slots[pi]` in der Eltern-Kette | die vorige Schleife hat fuer **jeden** belegten Slot `parent < nslots` und `slots[parent].used` geprueft; per Induktion ist die ganze Kette gueltig. Das ist ein Argument ueber **zwei** Schleifen und steht in keiner Zeile |

### F6 im Klartext

`[profile.release]` in `Cargo.toml` setzt **`overflow-checks` nicht**. `refcount -= 1` bei 0
ist dort kein Panic, sondern ein **stiller Umlauf auf 0xFFFF_FFFF** — das Objekt wird nie
finalisiert, die `Memory`-Region nie freigegeben, die `Reply`-Cap nie abgebrochen. Also genau
die Form von D11: ein Faden haengt, und jeder Pruefer meldet Ordnung.

Nach dem Massstab dieses Projekts gehoert dorthin dasselbe wie an `Finalized::overflowed` und
`cdt_walk_overruns`: **„kann nicht vorkommen" muss pruefbar sein statt behauptet.** An diesen
beiden Stellen ist es das; an `refcount -= 1` nicht.

### F13 — der Pruefer kann an der Eingabe sterben, fuer die er gebaut ist

`audit_cdt` validiert den `parent` eines Slots, **bevor** es ihn dereferenziert. Aber dann liest
es `self.slots[p].mdb.first_child` und laeuft die Geschwisterkette ab — **ohne** diesen Index zu
pruefen. Die Pruefung dafuer (Code 6) steht in der Iteration des Slots `p` selbst. Ist `p > s`,
hat sie noch nicht stattgefunden.

Ausfuehrbare Gegenprobe (`demo/f13_gegenprobe.adb`), drei Ausgaenge, gefahren:

```
1 Positivkontrolle (sauber)      : audit_cdt =  0
2 Kontrolle (kaputt bei Slot 0)  : audit_cdt =  6
3 Fund      (kaputt bei Slot 1)  : CONSTRAINT_ERROR (Indexpruefung) -- kein Urteil
```

Zwischen 2 und 3 wandert **nur die Position** des tragenden Slots; der kaputte Wert ist
derselbe (`first_child = 999`). Damit ist die Ursache die **Reihenfolge der Pruefungen** und
nicht die Verfaelschung — die Gegenprobe isoliert.

In Rust ist das `Slab::index` → `.expect("Slab-Index ausserhalb der Kapazitaet")` → Panic, und
`panic = "abort"` steht in **beiden** Profilen. Der Pruefer, der die Anomalie melden soll,
nimmt den Knoten statt dessen mit.

`CLAUDE.md` haelt zu B-5.5 fest: *„Dass ausgerechnet der Pruefer gegen einen zyklischen CDT
geschuetzt war und `revoke` nicht, war die eigentliche Schieflage."* Das stimmt fuer **Zyklen**
(die Schrittgrenze traegt). Gegen einen **Index ausserhalb der Tabelle** ist der Pruefer nicht
geschuetzt, und genau den soll Code 6 melden.

## Die Invarianten, die beim Portieren ausgesprochen werden mussten

| | Zusicherung | in Verus |
|---|---|---|
| I1 | jeder `Some(i)` in `Mdb` erfuellt `i < slots.len()` | als Modellinvariante ja — aber ohne Bruecke zum indizierenden Code, und **ohne `move_cap`** |
| I2 | `slots[s].object < objects.len()` fuer belegte `s` | ja (Refcount-Klausel 1), ebenfalls ohne Bruecke |
| I3 | `refcount(o) == #{belegte Slots auf o}` | ja — **ueber `nat`**, also ohne Maschinenfolge |
| I4 | an jeder `delete_leaf`-Stelle gilt `refcount > 0` | im Modell bewiesen (`lemma_refs_member`); im Code steht es nirgends |
| I5 | die Geschwisterkette ist kuerzer als `slots.len()` | **nein** — Verus hat fuer die Geschwisterkette gar keine Laengen- oder Terminierungsaussage; nur die Schrittgrenze im Code, und die ist ein Fehlerpfad, kein Beweis |
| I6 | `Finalized`: `n <= items.len()`, `dn <= dma.len()` | **nein** — `Finalized` kommt nicht vor. Musste hier als Vor- **und** Nachbedingung von `Push`/`Push_Dma`/`Delete_Leaf`/`Revoke` ausgeschrieben werden, sonst ist `Items(N)` unbeweisbar |
| I7 | Generationen laufen **absichtlich** um (`wrapping_add`); nach 2^32 Wiederverwendungen eines Slots kollidiert eine alte `CapPtr` mit einer neuen | **nein** — Generationen kommen nicht vor. Dabei ist `resolve` und damit die **gesamte** Handle-Sicherheit darauf gebaut |
| I8 | die Schrittgrenze erfuellt `limit < usize::MAX` | nein (formal; in Rust unerreichbar) |
| I9 | `install` rollt bei `NoSlot` zurueck mit `used = false`, laesst aber `refcount = 1` stehen — ein Fenster, in dem I3 verletzt ist und **`audit_cdt` es nicht meldet** (Code 3 vergleicht fuer unbelegte Objekte nur `refs`, nicht `refcount`) | nein. Harmlos, weil `alloc_object_inner` den ganzen Satz ueberschreibt — aber das ist eine unausgesprochene Abhaengigkeit, kein Entwurf |

I5, I6, I7 und I9 sind der eigentliche Ertrag: vier Zusicherungen, die das Cap-System traegt und
die heute **nirgends** stehen — weder als Beweis noch als Vertrag noch als Laufzeitpruefung.

## Portierungsregeln (damit die Zahlen etwas wiegen)

Ausgeschrieben in `src/caprock_cap.ads`. Der Kern: **keine Vorbedingung, die der Rust-Code nicht
auch erzwingt** — wer `Delete_Leaf` ein `Pre => Refcount > 0` gibt, hat den Fund wegdefiniert.
Verkettungsindizes sind vom Typ her **breiter** als die Tabelle, sonst waere der Fall, gegen den
`descend_to_leaf` gebaut ist, wegmodelliert. `refcount` ist ein Range-Typ, kein modularer, sonst
stellte sich die Unterlauf-Frage nicht.

Bekannte Vereinfachung: `ObjectKind` ist **flach** statt als Variantensatz. Ein Ada-Variantensatz
erzeugte Diskriminanten-Pruefungen an `out`-Parametern, die in Rust kein Gegenstueck haben — das
waeren zwei erfundene Befunde gewesen (sie standen im ersten Lauf drin).
