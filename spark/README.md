# SPARK-Experimente: Cap-Space (S1) und Scheduler (S2)

Zwei Portierungen, zwei getrennte Bilanzen, ein Gatter: `tools/spark-beweis.sh`
(Ratschen + Gegenproben, faellt bei Mutation durch).

| | Frage | Antwort |
|---|---|---|
| **S1** `caprock_cap` | Findet GNATprove am **Cap-Space** etwas, das das Verus-Modell nicht sieht? | **Ja, 15 Stellen** — die schaerfste steckt im Pruefer selbst |
| **S2** `caprock_sched` | **Wieviel des Scheduler-Kerns** kommt unter `SPARK_Mode => On`? | **65 von 68 Unterprogrammen analysiert — und die 3 uebersprungenen sind Ruempfe, die in dieser Quelle gar nicht stehen** (zwei HAL-Importe, ein Ada-Freigeber). Kein Stueck Scheduler-Logik unter `Off` |

**Ein Datenpunkt ist keine Kurve, zwei sind eine Gerade.** Was die beiden zusammen sagen,
steht unten unter „Was S1 und S2 gemeinsam ergeben".

---

# S1 — Cap-Space

**Die Frage:** findet GNATprove auf Silver Level (Abwesenheit von Laufzeitfehlern) am
Cap-Space etwas, das das vorhandene Verus-Modell **nicht** sieht?

**Die Antwort: ja, 15 Stellen — und die schaerfste steckt im Pruefer selbst.**

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

---

# S2 — der Scheduler

**Die Frage:** wieviel des Scheduler-Kerns (`crates/caprock-sched/src/lib.rs`, 2163 Zeilen)
kommt unter `SPARK_Mode => On` — und was findet GNATprove dort?

**Beide Ausgaenge waeren ein Ergebnis gewesen.** Der gemessene ist der guenstige:

> **Kein einziges Unterprogramm der portierten Scheduler-Logik steht unter `SPARK_Mode => Off`.**
> Uebersprungen werden genau **drei Ruempfe, und alle drei existieren in dieser Quelle gar
> nicht**: die zwei importierten HAL-Aufrufe und der generische Ada-Freigeber.

Damit ist die Antwort auf „gibt es einen reinen SPARK-Kern?" nach zwei Datenpunkten:
**an der Sprache liegt es nicht.** Was draussen bleibt, ist in Rust ebenfalls draussen —
hinter `unsafe` und in einer anderen Crate.

## Kennzahl 1 — Abdeckung

### Was GNATprove selbst meldet

```
in unit caprock_sched, 65 subprograms and packages out of 68 analyzed
  Caprock_Sched.Free_Parkedgp4873.Free_Parked  skipped; body is SPARK_Mode => Off
  Caprock_Sched.Init_Thread_Frame              skipped; body is SPARK_Mode => Off
  Caprock_Sched.Stapeladresse                  skipped; body is SPARK_Mode => Off
```

Und die Begruendung **je einzelnem** `Off` — das ist die Diagnose, nicht die Zahl:

| Uebersprungen | Warum | Gegenstueck in Rust |
|---|---|---|
| `Init_Thread_Frame` | **importiert, kein Rumpf in dieser Quelle.** Schreibt einen Trap-Frame an das obere Stack-Ende | `caprock_hal::exception::init_thread_frame` — Fremdcrate, arch-spezifisch, in beiden Welten ausserhalb |
| `Stapeladresse` | dito | `fn stapeladresse()` mit `core::hint::black_box` — liest den eigenen Stack-Rahmen; in **keiner** Sprache beweisbar |
| `Free_Parked` | Instanz von `Ada.Unchecked_Deallocation`; der Rumpf liegt in der Ada-Laufzeit | **hat kein Gegenstueck** — Rusts `Parked` ist ein Wert, kein Haldenobjekt. Der Zeiger ist der Preis dafuer, dass SPARK die Linearitaet PRUEFT (s. u.) |

**Das Wesentliche:** die Liste enthaelt kein einziges Stueck Scheduler-Logik. Ready-Queue,
Grund-Menge, Budget/Refill, Zombie-Buchfuehrung, Migration, Directory, Audit — alles unter `On`.
Der Waechter haelt diese **drei Namen** gegen die Bilanz; eine Ratsche ueber der ZAHL 3 waere ein
Loch, weil sie Austausch nicht von Gleichstand unterscheidet.

### Und der ehrliche zweite Nenner

**Die Zahl oben zaehlt ADA-Unterprogramme, nicht Rust-Funktionen** — sie beantwortet „steht der
portierte Code unter `On`?" und nicht „wieviel von `lib.rs` ist portiert?". Die zweite Frage
braucht ihren eigenen Nenner, sonst liest sich die erste groesser, als sie ist.

Portiert sind **55 von 95** Funktionen aus `lib.rs` (die 25 Methodendeklarationen des
`SchedOps`-Traits sind keine Ruempfe und zaehlen nicht mit). Dass die Ada-Seite 68 statt 55
Unterprogramme hat, liegt an vier Freilisten-Helfern und einer Handvoll ausgeschriebener
Ausdruecke, die in Rust Methodenaufrufe fremder Crates sind.

Wer die 40 uebrigen als „geht nicht in SPARK" liest, liest falsch:

| nicht portiert | Anzahl | Grund |
|---|---|---|
| Rohspeicher (`attach_storage`, `*_bytes`, `*_align`) | 5 | **der einzige Grund, der SPARK betrifft** — und in Rust ist es dasselbe: `unsafe fn` mit `# Safety`-Vertrag, Rumpf in `caprock-slab` |
| `cycles.rs` (Zyklenabrechnung) | 5 | eigene Datei, eigener Host-Test, ausserhalb des benannten Umfangs |
| `redirect.rs` (Handler-Bindung) | 3 | eigene Datei, 1143 Zeilen. Der **Mechanismus** (`block_for_handler`/`mark_handler_wait`/`handler_reply`) ist portiert, nur die Bindung ist ein `Boolean` |
| Varianten portierter Formen | 6 | `spawn` = `spawn_parked` + `admit`; `block_current`/`block_for_handler`/`block_for_load` = `block_current_mit` mit anderem Grund |
| reine Ableser / Telemetrie | 19 | die Form `self.resolve(tid).map(..)` — ein Feld lesen, sonst nichts |
| Ada-Vorgabewerte statt `new`/`default` | 2 | Sprachidiom |

Die 19 Ableser sind **nicht vermutet, sondern belegt**: fuenf Vertreter genau dieser Form
(`Priority_Of`, `Reasons_Of`, `Frame_Of`, `Admitted_Of`, `Load`) sind portiert, und alle fuenf
sind bewiesen.

## Kennzahl 2 — Bilanz

`gnatprove --level=3 --timeout=30`, eigene Projektdatei `caprock_sched.gpr` (getrennte Bilanz —
eine Summe ueber beide Module waere eine Zahl, die kippt, sobald jemand am ANDEREN Modul etwas
aendert).

| | gesamt | bewiesen | offen |
|---|---|---|---|
| **Laufzeitpruefungen** (Silver Level) | **131** | 81 | **50** |
| Zusicherungen | 12 | 4 | **8** |
| Terminierung | 64 | 63 | **1** |
| Datenabhaengigkeiten | 63 | 63 | 0 |
| Initialisierung | 118 | 118 | 0 |
| Funktionale Vertraege | 7 | 7 | 0 |
| **Total** | **396** | 337 | **59 (15 %)** |

Zum Vergleich S1 (Cap-Space): 99 Laufzeitpruefungen, 15 offen. Der Scheduler hat bei einem
Drittel mehr Pruefungen **mehr als dreimal so viele offene** — **38 % gegen 15 %**.

**Jede** der 59 offenen Pruefungen ist einer benannten Fundstelle `[S2-Fn]`/`[S2-T1]` im
Quelltext zugeordnet; es bleibt kein unerklaerter Rest. Die 8 Zusicherungen sind vollstaendig
die `debug_assert_eq!(core, self.core)` (s. u.).

## Kennzahl 3 — Gegenpruefung am Rust-Original

Die Klassen, nicht die Einzelzeilen — die Zeilennummern stehen als `[S2-Fn]` im Rumpf.

### Klasse A — nichts im Code stuetzt sie

| | Ort (Rust, `lib.rs`) | Pruefung |
|---|---|---|
| **F1, F4, F8** | `queues[p]` in `enqueue_ready`:1878/1883/1887, `remove_from_ready`:1901/1906/1913, `dequeue_highest`:1997 | **`priority: u8` (0..255) indiziert `[ListHead; 8]`** |
| F2, F5, F6 | `self.tcbs[tail as usize]`:1885, `[prev as usize]`:1903, `[next as usize]`:1908 | rohe Indizes aus der intrusiven Verkettung |
| **F7** | `self.queues[p].count -= 1`:1913 | **Unterlauf, ohne jede Bedingung** |
| F3 | `self.queues[p].count += 1`:1888 | u32-Ueberlauf |
| F9, F10 | `budget_blocked_count`:1857/1859 | Ueber-/Unterlauf |
| **F13, F24, F31, F33** | `depleted_count -= 1`:1961/1517/1585/1356 | **Unterlauf — und dieser Zaehler hat KEINE Nachzaehlung, s. u.** |
| F11, F15, F35, F38 | `self.used += 1`:2035 / `-= 1`:1971/1368 | Ueber-/Unterlauf |
| F12, F22, F23, F27, F32 | `self.tcbs[acct]` / `[a]` aus `Option<usize>`:1946/957/999/1471/1703 | Spendenkette, roh indiziert |
| F17, F18 | `self.zombies[self.ztail]`:1985, `[self.zhead]`:860 | rohe `usize`-Indizes |
| F19 | `init_thread_frame(stack_base + stack_len, ..)`:718 | usize-Ueberlauf in der Adressrechnung |
| F16 | `hier < base + len`:1978 | dito |
| F26, F28, F30, F37 | `self.now += 1`:1446, `self.now + period as u64`:1478/1582/1385 | u64-Ueberlauf |
| F20, F21 | `self.tcbs[cur]` aus `self.current: Option<usize>`:830/914 | s. Klasse B |

### Klasse B — sicher, aber nur ueber ein Argument, das nirgends steht

`self.current` wird ausschliesslich aus `dequeue_highest()` oder aus `resolve()` gesetzt; beide
liefern strukturell einen gueltigen Slot. Das ist ein Argument ueber **alle** Zuweisungsstellen
und steht in keiner Zeile — dieselbe Form wie F14 in S1.

### Klasse C — Portierung, kein Fund (in der ersten Fassung drin, jetzt weg)

* `ZOMBIE_GESAMT.fetch_add(1, ..)` — `AtomicU64::fetch_add` laeuft in Rust **per Definition**
  um, in beiden Profilen und ohne Panic. Ein Range-Typ erzeugte hier zwei Funde, die es am
  Original nicht gibt. Jetzt modular.
* `1u32 << p` als Ada-Exponentiation — der Beweiser scheiterte an `2**n`, nicht an der Sache.
  Jetzt eine Konstantentabelle; die **Indexfrage** bleibt und ist der Fund.
* Vier `S.Tcbs (Nxt)` an den Aufrufstellen von `Dequeue_Highest` — Folgen von F8, keine eigenen
  Funde. Eine **beweisbare** Nachbedingung an `Dequeue_Highest` zaehlt sie jetzt einmal.

### Der Massstab, der alle Ueberlauf-Funde traegt

`[profile.release]` in `Cargo.toml` setzt **`overflow-checks` nicht** (nur `panic = "abort"`).
Ein `count -= 1` bei 0 ist dort kein Panic, sondern ein stiller Umlauf auf `0xFFFF_FFFF` — bei
`queues[p].count` heisst das: `count == 0` wird nie wahr, das Bitmap-Bit bleibt stehen, und
`dequeue_highest` waehlt fuer immer eine leere Liste als „hoechste Prioritaet". Genau die Form
von F6 in S1 und von D11: ein Faden haengt, und jeder Pruefer meldet Ordnung.

### F1/F8 im Klartext — mit ausfuehrbarer Gegenprobe

`Tcb::priority` ist ein `u8`, `Scheduler::queues` ist ein `[ListHead; NPRIO]` mit NPRIO = 8, und
`enqueue_ready` indiziert roh. Geklemmt wird an **genau einer** Aufrufstelle
(`kernel/src/loader.rs:1254`, der Manifestpfad — und dort korrekt, **vor** dem `as u8`).
Gemessen im Baum: **`NPRIO` kommt ausserhalb von `caprock-sched` an genau dieser einen Stelle
vor**; jeder andere `spawn*`-Pfad reicht die Zahl durch, und der Scheduler selbst prueft nichts.

Ausfuehrbare Gegenprobe (`demo/s2_prio_gegenprobe.adb`), drei Ausgaenge, gefahren:

```
1 Positivkontrolle (prio 0): zugelassen=TRUE
2 Kontrolle        (prio 7): zugelassen=TRUE
3 Fund             (prio 8): CONSTRAINT_ERROR (Indexpruefung) -- kein Einreihen
```

Zwischen 2 und 3 wandert **nur** die Prioritaet; damit isoliert die Probe den Wertebereich von
allem anderen. In Rust ist Ausgang 3 ein `panic!("index out of bounds")`, und `panic = "abort"`
steht in **beiden** Profilen — der Knoten stirbt.

Dass es heute kein Aufrufer tut, ist kein Schutz, sondern ein Zufall der Aufrufliste. Das ist
woertlich die Klasse aus `CLAUDE.md`: *„Eine Gefahr, die an einer Stelle per Hand abgewehrt wird
und an 52 nicht, ist ein fehlender Mechanismus, keine Sorgfaltsfrage."*

## Die drei Dinge, die der Cap-Space nicht hatte

Der eigentliche Erkenntnisgewinn: **wo bricht die Portierung?**

### (a) Nebenlaeufigkeit — traegt, und zwar besser als erwartet

`DIRECTORY` ist eine `Abstract_State` mit `External => (Async_Writers => True,
Async_Readers => True)`: GNATprove darf ueber **zwei Lesevorgaenge desselben Eintrags nichts
annehmen** — genau die Wirklichkeit, denn der Thread kann dazwischen migrieren.

**Ergebnis: 63 von 63 Datenabhaengigkeiten bewiesen, 0 offen.** Der Rust-Code liest das
Directory an jeder Stelle **genau einmal** in eine lokale Kopie (`dir_load`) und entscheidet auf
dieser Kopie — `resolve`, `owner_core`, `audit` und `slot_in_use` alle. Das ist nicht
selbstverstaendlich, und es ist jetzt **bewiesen statt gelesen**.

Was NICHT geht, und das ist die ehrliche Haelfte: **SPARK hat keine Ausdrucksform fuer „der
Aufrufer haelt den Spinlock".** `GID_FREE` und die `Scheduler`-Instanz selbst sind hier
gewoehnlicher Zustand; das unterstellt wechselseitigen Ausschluss. Ravenscar/Jorvik boeten
geschuetzte Objekte an — aber ein geschuetztes Objekt ist eine andere Laufzeit, kein
Kernel-Spinlock. Diese eine Zusicherung bleibt in **beiden** Sprachen ein Kommentar.

### (b) Zeit — die Ueberlauffrage existiert, ist aber die schwaechste

`now`, `next_refill`, `remaining`, `budget`, `period` sind Range-Typen; `now += 1` und
`now + period` sind damit Ueberlaufpruefungen (F26/F28/F30/F37). Formal offen — praktisch bei
100 Hz nach rund 5,8 Milliarden Jahren. **Das ist die schwaechste Fundklasse des Experiments**,
und sie gehoert benannt, damit sie nicht die starken verdeckt.

Ein Nebenbefund der Portierung: `range 0 .. 2**64 - 1` ist auf dieser Maschine ein
**128-Bit-Typ**. Die erste Fassung der ausfuehrbaren Gegenprobe rief die HAL ueber
`Convention => C` mit `uint64_t` — und meldete in **allen drei** Ausgaengen `CONSTRAINT_ERROR`,
also auch in der Positivkontrolle. Daran ist sie aufgefallen; eine Probe ohne Positivkontrolle
haette den Fund „bestaetigt".

### (c) `Parked` — **ja, SPARK kann es besser, und der Unterschied ist Fehler gegen Warnung**

Rusts `Parked` (`kernel/src/system.rs:8555`) traegt drei Eigenschaften. Die dritte prueft rustc
(privates Feld). Die ersten beiden nicht:

| | Rust | SPARK |
|---|---|---|
| kein Weg an die `ThreadId` | privates Feld — **rustc prueft es** | privates Feld / Besitzzeiger — ebenso |
| Weiterreichen erzwingen | `#[must_use]` — eine **Warnung**, mit `#[allow]` abschaltbar, und `let _ = p;` schweigt ohnehin | **Beweispflicht**: ein fallengelassener Besitzzeiger ist ein Check, der FEHLSCHLAEGT |
| kein `Drop` | ein weggeworfener `Parked` ist **still** | „resource or memory leak might occur at end of scope" |

Gemessen in `gegenprobe/s2_parked_probe.adb` — zwei Unterprogramme, die sich in **genau einer
Zeile** unterscheiden:

```
Richtig         (mit Admit)   : info: absence of resource or memory leak at end of scope proved
Fallengelassen  (ohne Admit)  : medium: resource or memory leak might occur at end of scope
```

Die Positivkontrolle ist der Punkt: ohne sie waere „genau ein Leck" auch dann wahr, wenn die
Eigentumspruefung gar nicht liefe. Der Waechter verlangt **beide** Zeilen.

**Der Preis, und er gehoert dazu:** SPARKs Linearitaet haengt an Besitz*zeigern*, also an einer
Allokation. Rusts `Parked` ist ein Wert auf dem Stack. Fuer einen Kernel ohne Halde ist das ein
echter Einwand — die Zusage waere in SPARK **staerker** und in derselben Bewegung **teurer**.
Was in SPARK ohne Zeiger bleibt, ist die WIRKUNG statt der Pflicht: dass `spawn_parked` keine
Ready-Queue anfasst, ist als `Post` ausdrueckbar und beweisbar. Das ist genau die halbe Zusage —
sie faengt den Umbau, der versehentlich einreiht, aber nicht den Aufrufer, der `admit` vergisst.

Nebenbei: derselbe Einwand gilt fuer `Migrant`. Er ist in Rust ein undurchsichtiges Token
(`detach_for_migration` erzeugt, `attach_migrated` nimmt) und hier ein einfacher `out`-Parameter
— die Bindung ist **nicht** portiert. Sie waere es mit demselben Mittel und demselben Preis.

## Drei Funde, die nicht im Auftrag standen

### 1. `migration_candidate` laeuft die Kette OHNE Schrittgrenze — `audit` daneben nicht

Einzige unbewiesene Terminierung (`[S2-T1]`, 1 von 64):

```rust
// lib.rs:1416 -- migration_candidate
for p in 0..NPRIO {
    let mut i = self.queues[p].head;
    while i != NIL { .. i = t.qnext; }        // keine Schranke
}
// lib.rs:1743 -- audit, DIESELBE Kette
while i != NIL { .. n += 1; if n > q.count { return 5; } }
```

Zwei Funktionen in derselben Datei laufen dieselbe Verkettung ab; **eine** haelt einen Zyklus
fuer denkbar genug, um ihn zu melden (Code 5), die andere nicht. `migration_candidate` laeuft im
Lastausgleich **unter dem Kern-Lock** — ein Zyklus dort ist kein Fehlerbericht, sondern ein
stehender Kern. `enqueue_ready` wehrt Doppeleintraege ab, ein Zyklus braucht also einen Fehler;
aber genau das gilt fuer `audit` auch, und dort steht die Schranke.

Dieselbe Form wie B-5.5 — *begrenzt war der Pruefer, nicht der Pfad* — nur diesmal andersherum.

### 2. `depleted_count` hat keine Nachzaehlung — und es ist der Zaehler, der schon einmal gelogen hat

`audit` fuehrt seit D10 Code **10**: die unabhaengige Nachzaehlung von `budget_blocked_count`.
Fuer `depleted_count` gibt es **keinen Code 11** (gemessen: null Vorkommen von `depleted_count`
in `audit`). Dabei steht die Begruendung fuer Code 10 woertlich im Quelltext daneben:

> *„Genau das fehlte `depleted_count`: dort standen Erhoehung und Senkung an verschiedenen
> Stellen, die Erhoehung lief mehrfach, und der Zaehler kam nie auf 0 zurueck (D8/M5)."*

`depleted_count` entscheidet in `on_tick`, ob der Refill-Scan ueberhaupt laeuft. Luegt er nach
oben, laeuft ein voller Tabellendurchlauf in **jedem** Tick; luegt er nach unten, bleibt ein
erschoepftes Konto liegen. Er wird an **vier** Stellen gesenkt (`record_zombie`:1961,
`refill_depleted`:1517, `set_budget`:1585, `detach_for_migration`:1356) und an **zwei** erhoeht
(`attach_migrated`:1386, `on_tick`:1479) — alle **sechs** ohne Helfer, anders als
`budget_blocked_count`, das seit D10 genau **eine** Schreibstelle hat. GNATprove meldet alle vier Senkungen als moeglichen Unterlauf.

**Der Befund ist nicht der Unterlauf, sondern die Asymmetrie:** derselbe Kommentar, der D8/M5
erklaert, steht ueber dem Zaehler, der die Behebung bekommen hat — nicht ueber dem, der den
Fehler hatte.

### 3. `debug_assert_eq!(core, self.core)` — 15-mal im Code, 0-mal im Release

Alle 8 unbewiesenen Zusicherungen sind diese eine Form. In Rust steht sie **15-mal** in
`lib.rs`; `debug_assert!` wird im Release-Profil **wegkompiliert**. Damit ist die Bindung
zwischen dem Parameter `core` und der Instanz `self.core` im ausgelieferten Kernel durch
**nichts** gedeckt — nicht durch den Typ, nicht durch eine Pruefung, nicht durch einen Beweis.
Sie haelt, weil der Kernel `SCHEDS[core].lock().on_tick(core, ..)` schreibt: **eine Zahl, zweimal
hingeschrieben.** Dieselbe Klasse wie *„zwei Zahlen aus derselben Hand sind keine zwei Quellen"*.

Behebbar waere sie ohne jeden Beweiser: `SCHEDS[core]` gibt einen Zugriff, der die Kern-Nummer
schon **traegt**, statt sie ein zweites Mal zu verlangen.

## Was das Verus-Modell dazu NICHT sagt

Gemessen an `Verification/scheduler/proofs/runqueue.rs` (413 Zeilen, 12 Beweise):

| Groesse | im Verus-Modell |
|---|---|
| Arithmetik | **ausschliesslich `nat`/`int`** — 40 `int`, 15 `nat`, **0** Vorkommen von `u8`/`u32`/`u64`/`usize` |
| Zustand je Thread | **7 Felder** (`used`, `blocked`, `depleted`, `in_ready`, `prio`, `budget`, `remaining`) gegen **23** im echten `Tcb` |
| Ready-Queue | ein **Mitgliedschafts-Flag** `in_ready: bool`. Der Kommentar sagt es selbst: „Kein Duplikat — Code 3 — ist durch die Modellierung als Mitgliedschafts-*Flag* strukturell ausgeschlossen" |
| `prio` | **`nat`, unbeschraenkt**. `NPRIO` kommt genau einmal vor: in einem Kommentar |
| Directory / Atomics / Kern | **kommen nicht vor** |
| `admitted` (D0), `sc_donor`/`sc_donee` (D9/H-b), `park_wake` (Z22), `handler` (Z26) | **kommen nicht vor** |
| die Zaehler (`used`, `depleted_count`, `budget_blocked_count`, `zcount`, `count`) | **kommen nicht vor** |
| `blocked` | **EIN `bool`** — „blockiert (IPC-Wait/pause)" |

Daraus folgen drei strukturelle Luecken, und die dritte ist die unangenehmste:

1. **`nat` gegen Maschinenwort.** Ein `nat` laeuft weder ueber noch unter. Ueber `count -= 1`,
   `depleted_count -= 1`, `now += 1` kann das Modell **nichts** aussagen — nicht „es ist sicher",
   sondern „die Frage existiert dort nicht". Wortgleich zu S1.
2. **Flag gegen Verkettung.** `in_ready: bool` schliesst Listenkorruption **per Modellierung**
   aus. Genau dort liegen F2/F5/F6 und die fehlende Schrittgrenze `[S2-T1]` — das Modell kann
   sie nicht einmal formulieren.
3. **Das Modell fuehrt `blocked: bool` — den Zustand, den Z24 ABGESCHAFFT hat.** Der Kommentar
   dort lautet „blockiert (IPC-Wait/pause)": ein Bit fuer zwei Lagen, also woertlich der Fehler,
   gegen den die Grund-Menge gebaut wurde. Die sechs Gruende (`IPC`/`BUDGET`/`PAUSE`/`PARK`/
   `HANDLER`/`LOAD`) und der zweite Halbsatz („eingereiht wird nur bei leerer Menge") kommen im
   Modell nicht vor. **Was Verus hier beweist, ist der Vorgaengerzustand.**

Fair bleibt: das Modell **benennt** seine Luecken selbst (Befunde B4 und B5 im Kopf der Datei
nennen fuenf nicht modellierte Uebergangsklassen). Was es nicht benennt, ist Punkt 3 — dass sein
Zustandsraum dem Code seit Z24 nicht mehr entspricht.

## Portierungsregeln (damit die Zahlen etwas wiegen)

Ausgeschrieben in `src/caprock_sched.ads`, zehn Stueck. Die vier, die S1 nicht hatte:

* **`priority` ist ein `U8` (0..255), die Queue-Tabelle hat 8 Faecher.** Das ist keine
  Modellierungsfreiheit, sondern der Rust-Typ.
* **Zaehler sind Range-Typen, nicht modular** — sonst stellte sich die Unterlauffrage nicht.
  Ausnahme: was in Rust ueber `fetch_add` laeuft, ist auch hier modular.
* **Das Directory ist externer Zustand mit `Async_Writers`** — ein zweites Lesen darf einen
  anderen Wert liefern.
* **`GID_FREE` und die Instanz sind gewoehnlicher Zustand.** Das unterstellt den Spinlock, und
  die Unterstellung steht ausgeschrieben da, statt stillschweigend zu gelten.

## Was S1 und S2 gemeinsam ergeben

| | S1 Cap-Space | S2 Scheduler |
|---|---|---|
| unter `SPARK_Mode => On` | 34 von 34, **0 `Off`** | 65 von 68, **0 `Off` in der Logik** (3 Ruempfe existieren in der Quelle nicht) |
| Laufzeitpruefungen | 99, davon 15 offen (**15 %**) | 131, davon 50 offen (**38 %**) |
| Fundklassen | Index + Refcount-Unterlauf | Index + **Zaehler-Unterlauf** + **Terminierung** + **weggekompilierte Zusicherung** |
| was Verus dazu sagt | nichts (`nat`, `Seq`, keine Bruecke zum Code) | nichts (`nat`/`int`, 7 von 23 Feldern) — **und sein Zustand ist seit Z24 ueberholt** |

**Die Gerade durch zwei Punkte:** an der SPRACHE scheitert es an beiden Stellen nicht. Was
draussen bleibt, ist an beiden Stellen dasselbe — Rohspeicher, den Rust ebenfalls hinter
`unsafe` einzaeunt. Die Rate offener Pruefungen steigt aber von 15 % auf 38 %, und der Grund ist
kein Sprachproblem: der Scheduler ist aus **rohen Maschinenindizes** gebaut (intrusive Listen,
`Option<usize>` als Slot, `u8` als Tabellenindex), wo der Cap-Space Tabellen mit Schranken hat.
Ein Umbau nach SPARK waere damit kein Uebersetzungsvorgang, sondern ein **Entwurfsvorgang** —
und die 50 offenen Pruefungen sind seine Aufgabenliste, unabhaengig davon, ob je eine Zeile Ada
im Kernel landet.

**Was die zwei Punkte NICHT hergeben:** eine Hochrechnung auf den ganzen Kernel. Beide Module
sind reine Datenstrukturlogik ohne Hardwarezugriff. `kernel/src/system.rs` (Cap-Aufloesung,
IOVA-Fenster, Teardown), die HAL und die Ladepfade sind nicht vermessen, und dort liegt das
`unsafe`. Zwei Punkte auf einer Geraden sagen etwas ueber die Gerade, nichts ueber die Punkte,
die nicht darauf liegen.
