# Caprock — erledigte Punkte

Ausgelagert aus [todo.md](todo.md), damit dort nur steht, was noch zu tun ist.

Die Begründungen bleiben erhalten, und das ist der eigentliche Zweck dieser Datei: bei mehreren
Einträgen ist das Wertvolle nicht, **dass** etwas gelöst wurde, sondern **welche Annahme sich
dabei als falsch erwiesen hat**. Wer später dieselbe Abkürzung erwägt, findet hier, warum sie
schon einmal nicht getragen hat.

---

## C8. Der VERIFIZIERERTHREAD — die Krypto ist vom Aufrufer-Stack herunter (2026-08-11)

**Klasse:** Kapazität / Sicherheit · **Stand:** (a) und (b) **gebaut und gemessen**; (c) bleibt
offen und steht weiter in [todo.md](todo.md).

### Die Zahl, um die es ging — vorher und nachher

Gemessen mit dem vorhandenen `kstackmark` (Muster beim Anlegen, Abzählen beim Tod des Threads und
beim Schlussfegen), in **beiden** Suiten, `-m 512M`, 4 vCPU:

| Messung | vorher | nachher |
|---|---|---|
| **Lade-Suite**, EL0-Kstack | **11 992 / 16 384 B (73,1 %)**, Reserve 4392 B | **1 312 / 16 384 B (8,0 %)**, Reserve 15 072 B |
| Hauptsuite, EL0-Kstack | 824 / 16 384 B (5,0 %) | 824 / 16 384 B (5,0 %) — unverändert |
| **Verifiziererstack** (64 KiB), Lade-Suite | — | **12 328 / 65 536 B (18,8 %)**, Reserve 53 208 B |
| Verifiziererstack, Hauptsuite | — | 2040 / 65 536 B (3,1 %) |

Die Hauptsuite bleibt gleich, und **das ist die Kontrolle**: sie hat kein Boot-Archiv, also lief
über ihren `SYS_LOAD` nie echte Krypto. Wäre ihre Zahl mitgefallen, hätte der Umzug etwas anderes
verschoben als das Gemeinte.

Die 12 328 B des Verifizierers gegen die 11 992 B von vorher sind die **erwartete** Differenz: es
ist derselbe Pfad plus zwei Rahmen (Schleifenrumpf des Threads, das Verdichten der Endowment-Liste).
Der tiefste Kernelpfad ist damit nicht billiger geworden — er steht nur nicht mehr auf einem Stack,
den jeder Thread bezahlt.

**Die Rechnung, um die es geht:** 10 000 EL0-Threads × 16 KiB = 160 MiB. Der Restpfad braucht
1312 B; 4 KiB je Thread wären damit **nicht mehr geraten, sondern gemessen** — 40 MiB statt 160.
Gesenkt ist die Konstante hier **noch nicht** (das ist Folgeposten 3 in [C4](todo.md)); erbracht ist
die Bedingung dafür.

### Die `kstack`-Zeile misst seither etwas anderes — und sagt das selbst

Das ist die Randbedingung (b), und sie ist der Grund, warum die Zeile jetzt einen Absatz über sich
selbst druckt: **vorher war sie eine Messung des Ladepfads, jetzt ist sie eine des Restpfads.** Wer
1312 gegen 11 992 hält, vergleicht über einen Bedeutungswechsel hinweg. Genau daran ist in diesem
Projekt schon einmal eine Zahl wertlos geworden (die 399er-Lehre), deshalb steht die Umdefinition
in der Berichtszeile, in beiden Suiten-Skripten und hier.

### Der Bau

* **Ein neuer Grund in der Grund-Menge**, nicht ein viertes Bit: `BlockReasons::LOAD` (Z24, sechste
  Instanz derselben Klasse). Wecker ist **ausschliesslich** `Scheduler::load_reply` — `resume`
  entfernt `PAUSE`, `unpark` `PARK`, `unblock` `IPC`, `handler_reply` `HANDLER`, und eingereiht
  wird nur bei **leerer** Menge. Der Aufrufer eines `SYS_LOAD` kann damit nicht von einem fremden
  Wecker losgelassen werden, bevor sein Ergebnisregister geschrieben ist.
* **`SYS_LOAD` ist eine ÜBERGABE geworden.** Der Dispatch-Callback lädt nicht mehr, er reicht den
  Auftrag weiter und blockiert; sein Rückgabetyp ist `LadeUebergabe` mit drei unterscheidbaren
  Ausgängen (`Uebergeben(sp)`, `Ausgelastet`, `KeinVerifizierer`).
* **Die Serialisierung ist ein benannter DoS-Kanal.** `AUFTRAEGE_MAX = 4`, Überlauf →
  **`ERR_LOAD_BUSY = 13`**, und der Überläufer wird **gar nicht erst blockiert**. Das ist D11
  wörtlich: wer eine Kapazität einführt und den Überlauf nicht benennt, hat ein Loch gebaut.
  `KeinVerifizierer` bekommt einen **anderen** Code (`ERR_SERVER_GONE`) — „gerade voll" und „gibt es
  nicht" verlangen vom Aufrufer verschiedene Antworten.

### Die Reihenfolge, an der alles hängt — und warum es hier KEINE Weckmarke gibt

Der Aufrufer wird blockiert, **bevor** sein Auftrag sichtbar wird; beides unter *einer* Sperrung der
Auftragsschlange. Andersherum könnte der Verifizierer auf einem anderen Kern fertig sein, bevor der
Aufrufer blockiert ist — `load_reply` entfernte einen Grund, den es noch nicht gibt, und der
Aufrufer setzte ihn danach für immer. Für `PARK` fängt eine Weckmarke genau das ab; hier tut es die
Sperrung, und sie ist billiger als ein zweiter Zustand. Sperrordnung `SCHLANGE (R1.5) → SCHEDS (R2)`,
eingetragen in `docs/invariants.md` §1.

### Die Absage ist GEFAHREN, nicht behauptet

Prüfzeile `verif`, gattert in `all_done()`, läuft in **beiden** Suiten:

```
verif   : Absage gefahren -- 5 Sonden gegen eine Schranke von 4: abgewiesen=1 bedient=4 ·
          beim Beobachten wegen LOAD blockiert=4 · Ueberlaeufer NICHT blockiert=true und
          danach WEITERGELAUFEN=true · Fuellstand erreichte 4/4
```

Der Verifizierer wird pausiert (`PAUSE` steht **neben** seinem `PARK` in der Menge und stört es
nicht — genau die Eigenschaft, die Z24 hergestellt hat), dann melden sich fünf Aufrufer gegen vier
Plätze. Der Archivindex ist absichtlich **ungültig**: gemessen wird die Schlange, nicht das Laden,
und so läuft die Sonde auch in der Hauptsuite, die kein Archiv hat. Die beiden Ausgänge sind dadurch
trennscharf — `ERR_BADCAP` heisst „war beim Verifizierer und ist dort gescheitert", `ERR_LOAD_BUSY`
heisst „kam nie hin".

Dass der Überläufer **weiterläuft**, wird an einem Rundenzähler gemessen und nicht an einem
Zustandsbit: ob ein Thread läuft, ist an keinem Bit ablesbar, an einem Fortschritt schon (dieselbe
Unterscheidung wie bei der FP- und der Park-Sonde). Und `Fuellstand erreichte 4/4` ist die
Sprechprobe: ohne sie wäre jede Aussage über den Überlauf eine Aussage über einen Fall, der nie
eingetreten ist.

### Zwei Gegenproben, beide isolierend

| Mutation | Wirkung | offen laut Watchdog |
|---|---|---|
| `load_reply` entfernt `LOAD` **nicht** | `bedient=0` (die vier Bedienten hängen), `abgewiesen=1`, `blockiert=4`, alles andere unverändert | `verif` — **genau eins** |
| `platz_nehmen` ohne `else`-Zweig (die D11-Form: still verwerfen, trotzdem blockieren) | `abgewiesen=0`, `blockiert=5` (der Überläufer hängt mit), `angenommen=5` gegen `bearbeitet=4` — ein Auftrag lautlos weg | `verif` — **genau eins** |

Die zweite Gegenprobe reproduziert das D11-Bild in einem Satz: ein Thread wartet für immer, die
Schlange ist leer, kein Zähler klagt.

### Drei Befunde, die nicht im Auftrag standen

1. **Die Platzierungspolitik wäre stillschweigend mitgewandert.** `load_into_pd_mit` legt einen
   Thread ohne Manifest-Affinität auf `hal::cpu::core_id()` — bis C8 war das der **Aufrufer**,
   danach wäre es der Verifizierer gewesen. Jedes ohne Affinität geladene Programm hätte seinen Kern
   gewechselt, als Nebenwirkung einer Stack-Verschiebung. Der Auftrag trägt den Heimatkern deshalb
   mit (`load_by_index(.., heimatkern)`, `ladepolitik_auf`).
2. **Die abgeleiteten Endowment-Caps lecken auf den neuen Abweispfaden.** Auf dem angenommenen Weg
   räumt `load_by_index` sie bei Misserfolg auf; bei `Ausgelastet`/`KeinVerifizierer` kommt der
   Loader gar nicht erst dran. Ohne die Löschung im Dispatch blieben sie als verwaiste CDT-Kinder
   liegen und blockierten sogar das `delete` des Eltern-Caps — dieselbe Falle, die der Abweispfad im
   Loader schon einmal bezahlt hat, an einer Stelle, die es vor C8 nicht gab.
3. **Eine Marke auf Bit 63 kollidiert mit dem obersten Zahlenfeld.** Die erste Fassung packte
   „gemessen" auf Bit 63 des Ergebniswortes; dort liegt `verloren` (Bits 56..64). Die Marke las sich
   beim Auspacken als `verloren = 128`, das Urteil fiel durch, und der Lauf ging in den Watchdog —
   bei **jedem einzelnen grünen Feld** in der Zeile darüber. Ein Bild, das wie ein echter Befund
   aussieht und eine Bitmaske ist.
4. **Der eigene Entwurf war eine Zeile lang das, was er verwerfen wollte.** Der Thread-Rumpf hiess
   zuerst `while let Some(a) = SCHLANGE.lock().entnehmen() { … laden(&a) … }`. Der Guard eines
   `while let`-Scrutinees lebt bis zum **Ende des Rumpfes**, und `SpinLock::lock` maskiert die IRQs
   des eigenen Kerns — die gesamte Ed25519-/SHA-2-Prüfung wäre also **mit gesperrten Interrupts**
   gelaufen, also genau die Fassung „Präemption für die Dauer aus", die C8 in seiner Tabelle
   ausdrücklich verwirft. Hereingeholt nicht durch eine Entscheidung, sondern durch eine
   Temporaries-Lebensdauer, und von **keiner** Prüfzeile zu sehen: die Suite war grün, die Messwerte
   stimmten, das Latenzloch war unsichtbar. Belegt mit einem `Drop`-Zeugen statt erschlossen (im
   `while let` fällt der Guard nach dem Rumpf, in `let v = { … };` davor). Behoben durch eine
   **Funktionsgrenze** (`naechster()`), nicht durch einen Kommentar. Dieselbe Wurzel wie der
   `match lock() { … None => lock() }`-Selbst-Deadlock aus der Fallenliste — anderer Schaden, gleiche
   Ursache.

---

## D14. Eine BERICHTSZEILE MEHR kippt die Z4f-Pruefung — **BEHOBEN, und es war nicht die Wanduhr**

**Klasse:** Prueferform · **Stand:** **Ursache benannt und behoben (2026-08-11, abends).** Der
Fehler lag nicht im Kernel und nicht in der Zeit, sondern in der Nachpruefung selbst.

### Der Mechanismus

Die Nachpruefung „der Sektor blieb unveraendert" lautete in `test-qemu-x86-load.sh`:

```python
d[0:8] == b'SL4KCKPT' and d[16] != 0x00
```

`d[16]` ist nach `crates/caprock-cap/src/checkpoint.rs` (`OFF_BODY = 16`, dort beginnen 32 Byte
`kernel_hash`) das **erste Byte des KERNEL-HASHES** — und der Negativfall unmittelbar darueber
kippt genau dieses Byte mit `s[16] ^= 0xFF`. Damit hing das Urteil an einer Groesse, die mit dem
Schreiben des Sektors nichts zu tun hat: **am SHA-256 des gerade gebauten Kernels.**

**Gemessen, nicht ueberlegt** (`d[16]` gegen alle 256 moeglichen Hash-Bytes, an einem echten
Abbild; die Zuordnung `d[16] == kernel_code_hash(ELF)[0]` unabhaengig aus dem ELF nachgerechnet:
`0xd0 == 0xd0`):

| Richtung | Zahl |
|---|---|
| **Falsch-Alarm**: `kernel_code_hash[0] == 0xFF` → gekippt `0x00` → „ueberschrieben", obwohl NICHTS geschrieben wurde | **1 von 256 Bauten** |
| **Blinder Fleck**: der Kernel ueberschreibt wirklich (schreibt SEINEN Hash hin) → Zeile meldet „unveraendert" | **255 von 256** |

Das erklaert **jede** Beobachtung des alten Eintrags, und zwar besser als die Zeitthese:

* *„je Bau reproduzierbar (2 von 2 Laeufen)"* — der Hash ist je Binary konstant. Eine
  Wanduhr-Ursache waere geflattert.
* *„nicht ihr Inhalt, ihre blosse Existenz"* — jede Aenderung am Kernel-Binaerinhalt wuerfelt den
  Hash neu, gleich welchen Text die Zeile traegt.
* *„beim Zuruecknehmen wieder gruen"* — das alte Binary hat wieder sein altes Hash-Byte.

Die Zeitthese war eine naheliegende Verwechslung: eine `println!`-Zeile aendert **zwei** Dinge,
die Ausgabelaenge und das Binary. Nur das zweite ist deterministisch, und nur das zweite wurde
gelesen. **Dieselbe Klasse wie `rx_used` gegen „Daten sind angekommen": zwei Groessen bewegen sich
zusammen, und man liest die falsche.**

### Was daraus geworden ist

* Geprueft werden jetzt die **512 Byte selbst**, vor und nach dem Boot (SHA-256 ueber den Sektor).
  Ein Praedikat ueber EIN Byte kann „unveraendert" nicht ausdruecken — es hat den Fall, fuer den
  es gebaut war, in 255 von 256 Faellen nicht gesehen.
* Damit ist der Punkt „die naechste Zeile, die jemand hinzufuegt, kippt sie wieder" weg — die
  Ausweichbewegung (NMI-Zahlen in die vorhandene `ist`-Zeile) darf bleiben oder zurueckgenommen
  werden, sie traegt keine Zaehne mehr.
* **Die Bilanz muss nachgerechnet werden, nicht nur der Ausfall beklagt:** die Zeile hat in
  ihrer ganzen Lebensdauer **keine** Ueberschreibung belegen koennen. Jedes „PASS" von ihr war
  eine Aussage ueber ein Hash-Byte. Ob der Kernel den Sektor je ueberschrieben hat, ist damit
  fuer die Vergangenheit **unbeantwortet** — nicht „nein". (Der Kernelpfad sieht richtig aus:
  `CKPT_REJECTED` setzen und sofort zurueckkehren. Belegt ist es seit heute, vorher war es
  gegengelesen.)

### Die Lehre

**Ein Praedikat, das die gepruefte Groesse nur STELLVERTRETEND liest, prueft den Stellvertreter.**
`d[16] != 0` sollte „512 Byte unveraendert" heissen und hiess „ein bestimmtes Hash-Byte ist nicht
null". Solche Stellvertreter entstehen, weil der direkte Vergleich einen Wert VOR dem Lauf
festhalten muss — genau die Zeile, die man sich sparen will.

---

<details>
<summary>Der Eintrag, wie er bis zur Behebung dastand (2026-08-11 vormittags)</summary>

Beim Einbau der NMI-Reentranz-Zahlen fiel die Lade-Suite aus:

```
FAIL: der abgewiesene Checkpoint wurde ueberschrieben
```

**Zugeordnet, nicht vermutet.** Die beiden geaenderten Dateien einzeln zurueckgesetzt:

| Stand | Lade-Suite |
|---|---|
| beide Aenderungen | **FAILURES** (2 von 2 Laeufen) |
| nur `exception.rs` (NMI-Buchhaltung) | ALL PASS |
| nur der `urteil()`-Konjunkt, **ohne** die neue Berichtszeile | ALL PASS |
| mit der neuen Berichtszeile | **FAILURES** |

**Es ist also EINE zusaetzliche `println!`-Zeile im Abschlussbericht** — rund 450 Zeichen ueber
eine byteweise Serielle. Nicht ihr Inhalt, nicht das Urteil, das sie traegt: ihre blosse Existenz.

**Was das ueber die Pruefung sagt.** Der Z4f-Negativfall verlangt, dass ein Kernel einen
strukturell heilen, aber **fremd gebundenen** Checkpoint abweist und den Sektor **unveraendert**
laesst. Der Kernelpfad dafuer sieht richtig aus: `CKPT_REJECTED` setzen und **sofort
zurueckkehren**, ohne zu schreiben. Trotzdem haengt der Ausgang an der Ausgabelaenge — also an
Wanduhrzeit, und damit an genau der Groesse, die in D13 schon einmal vier Abweichungen erzeugt hat.

**Umgangen, nicht behoben:** die NMI-Zahlen stehen jetzt in der **vorhandenen** `ist`-Zeile statt
in einer eigenen. Das ist eine Ausweichbewegung und keine Erklaerung — und sie hat eine Zaehnezahl:
**die naechste Zeile, die jemand hinzufuegt, kippt sie wieder**, und dann sucht er dort, wo er
gerade gearbeitet hat, statt hier.

- [x] **Den Mechanismus benennen.** ~~Wer schreibt den Sektor, wenn der Kernel laenger braucht?~~
      **Niemand.** Der Sektor wurde nie ueberschrieben; die Pruefung las das falsche Byte, s. o.
      Alle drei Kandidaten (`drv_service_step`, `reap`, eine Notschranke) waren daneben.
- [x] **Die Pruefung von der Ausgabelaenge entkoppeln.** Sie hing nie an ihr — sie hing am
      Kernel-Hash. Jetzt haengt sie an den 512 Byte, um die es geht.

</details>

---

## A1 + Z11c — der reguläre Weg ist gefärbt, und die Politik steht im Manifest (2026-08-07)

### 1. Was offen war

A1 galt nur für `spawn_isolated_colored` — eine PD, die der Kernel selbst erzeugt. Der **reguläre**
Weg, ein Programm auf diesen Knoten zu bringen, ist der Lader, und der war ungefärbt. So stand es
im Eintrag: *„solange `spawn_isolated` der Normalfall ist, ist A1 im Normalbetrieb nicht wirksam."*

Z11c sagte, die Politik gehöre ins Manifest. Das Format hatte die Felder seit A-1.4
(`policy_flags`, `numa_node`, `core_affinity`, `priority`, `budget_us`) — der Kernel **druckte**
sie und hielt nur `POLICY_ROOT_TASK` ein. `POLICY_EXCLUSIVE_STRIPE` wurde ausdrücklich
**abgewiesen**, was fail-closed und richtig war; die übrigen fünf wurden **still ignoriert**.

Beide Punkte sind dieselbe Frage: *wer entscheidet, ob eine PD einen Farbstreifen bekommt?* Die
Antwort ist das Autoritätsdokument, nicht der Code.

### 2. Die Begründung, warum gefärbtes Laden nicht ginge, war zur Hälfte falsch

`policy_gate` schrieb: Segmente und Stack kämen „zusammenhängend aus `mem_alloc`", ein Farbstreifen
trage aber nur `region_bytes()`. Der zweite Teil stimmt. Der erste beschrieb die damalige
**Allokation**, nicht eine Notwendigkeit — gemappt wurde längst **seitenweise**
(`vspace_map_page_at` in einer 4-KiB-Schleife). Physische Zusammenhängung wird gar nicht gebraucht.

Damit war die Aufgabe nicht „stückweises Mapping bauen", sondern nur: **stückweise allozieren und
jedes Stück einzeln buchen.** Stückgröße ist `colors::region_bytes()` — die größte am Stück
gleichfarbige Menge (x86 512 KiB, aarch64 16 KiB). Ungefärbt bleibt es bei einem Stück je Segment,
der bisherige Pfad verschiebt sich also nicht.

**Ein Leck, das dabei sichtbar wurde.** `loaded_register` nahm `segs.iter().take(MAX_IMG_SEGS)` —
was darüber lag, wurde stillschweigend weggelassen und beim Teardown nie freigegeben. Das ist
wörtlich die Form von D11 (`if cap { .. }` ohne `else`). Solange die Segmentzahl vorher gegen
dieselbe Schranke geprüft wurde, war es unerreichbar; mit der stückweisen Allokation nicht mehr.
Jetzt `#[must_use] -> bool`, und der Aufrufer räumt auf.

**Ein benannter Unterschied, kein Versehen:** der Stack einer gefärbten PD wandert in die
Teardown-Liste statt in die **Reap-Region** des Threads. Eine Reap-Region ist *eine*
zusammenhängende Region; dem Scheduler eine Liste zu geben wäre ein Umbau des Thread-Todes für
einen Randfall. Folge: der Stack einer gefärbt geladenen PD überlebt den Tod ihres Threads bis zum
Abbau der PD — genau wie ihre Segmente es heute schon tun.

### 3. Der Nachweis — und warum er nicht aus dem Ladepfad kommt

    pdcolor : Farben=512 Streifen=16 Farben | gefaerbte-PD: 5 Seiten in 5 Farbe(n),
              alle-im-Streifen=true | ungefaerbte-PD: 2 Seiten,
              zufaellig-auch-im-Streifen=false, disjunkt=true
    pdcolor : ALL PASS

Gemessen wird die **Teardown-Buchhaltung** (`system::loaded_frames_of`), nicht der Ladepfad: der
hat die Frames selbst aus dem gefärbten Allokator geholt, ihn zu fragen wäre ein Schreiber, der
sein eigenes Ergebnis bestätigt. Dieselbe Überlegung wie bei `tools/checkfat.py`.

**Zwei eigene Fehler im Prüfer, beide gemessen statt vermutet:**

1. **Die erste Gegenprobe war nicht erfüllbar.** Sie verlangte, die ungefärbte Vergleichs-PD müsse
   *mehr* Farben belegen, als ein Streifen fasst. Die geladenen Programme sind aber klein (2 und 5
   Seiten), und 2 Seiten können nie mehr als 16 Farben belegen. Das Kriterium fiel durch,
   **unabhängig davon, ob die Färbung trägt** — ein Prüfer, der nicht bestehen *kann*, misst nichts.
   Jetzt trägt `in_maske` selbst die Aussage, und ihre Kraft steht in der Zeile: 5 Seiten in 16 von
   512 Farben. Nicht entscheidbar ist es bei <2 Farben, bei einem Streifen über alle Farben, oder
   wenn die PD weniger als zwei Seiten bzw. Farben hat — dann SKIP.
2. **Die Messung stand zuerst in `all_done()`.** Die wird *gepollt*; eine druckende Messung gehört
   dorthin so wenig wie ein Urteil, das erst im Bericht entsteht. Die Suite lief in den Watchdog.
   Jetzt einmal gemessen, Ergebnis in einem Atomic, `all_done()` liest.

**Und ein dritter, den die Zeile selbst gefangen hat.** Ich hatte nur `load_program_into_pd`
umgestellt; `hello` kommt aber über `load_image` → `load_elf`. Die Zeile meldete daraufhin
**SKIP** („kein Programm mit EXCLUSIVE_STRIPE geladen") statt ALL PASS — und genau deshalb ist es
aufgefallen. Ein Prüfer, der Abwesenheit als Erfolg gebucht hätte, hätte hier grün gemeldet.

### 4. Z11c: die Politik wird angewandt, nicht nur gelesen

    ladepol : Thread mit ABWEICHENDER Politik: Manifest verlangte prio=2 kern=None,
              Scheduler gab prio=Some(2) kern=Some(0)   -> ALL PASS

| Feld | Stand |
|---|---|
| `POLICY_EXCLUSIVE_STRIPE` | eingehalten |
| `priority`, `core_affinity` | eingehalten |
| `numa_node != 0` | **abgewiesen** — der Allokator hat keine Knoten (Z8) |
| `POLICY_PINNED` | **abgewiesen** — „Lastausgleich ist per Vorgabe aus" ist keine Zusicherung |
| `budget_us != 0` | **abgewiesen** — eine MCS-Reservierung braucht Budget *und* Periode |

Der Grundsatz ist derselbe wie bei `EXCLUSIVE_STRIPE` vorher: **was der Kernel nicht einhalten
kann, wird abgewiesen, nicht ignoriert.** Bei `budget_us` heißt das ausdrücklich: aus einer Zahl
eine Reservierung zu machen hieße, die Periode zu **erfinden** — sie stünde dann in keinem
Dokument, und ein Manifest bekäme eine Garantie, die es nie verlangt hat.

**Zwei eigene Fehler auch hier:**

1. Die Zeile verglich zuerst den **zuletzt** geladenen Thread. Der verlangt `prio=1`, und 1 ist die
   Vorgabe — ein Ladepfad, der die Angabe komplett ignoriert, hätte dieselbe Zeile gedruckt.
   Gefragt ist der Fall, in dem sich verlangt und bekommen **unterscheiden**.
2. Danach las sie die Priorität **im Bericht** zurück und bekam `None`: `hello` läuft kurz und
   beendet sich. Wörtlich die Falle aus dem Farbtest — *ein Wert, der an der Lebendigkeit eines
   Threads hängt, taugt nicht als Messgröße*. Jetzt beim Laden erfasst, wo der Thread noch nicht
   einmal zugelassen ist.

### 5. Der Befund, der größer ist als der Eintrag

Die Prioritäten standen seit jeher im Test-Manifest (3/1/2/2/2) und wurden **nie eingelöst** — es
waren Platzhalter. Eingehalten *reißt* dieselbe Zuteilung die Lade-Suite: ein **pollender** Treiber
(B-3.2, kein IRQ, s. `todo.md`) auf einer höheren Priorität als sein Client lässt den Client
verhungern. Richtig zugeteilt und trotzdem unbrauchbar.

**Ein Feld, das nie eingelöst wird, sammelt Werte an, die niemand geprüft hat — und der Tag, an dem
es eingelöst wird, ist der Tag, an dem sie alle falsch sind.** Genau deshalb braucht ein pollender
Treiber ein Budget, und genau deshalb ist `budget_us` als Einzahl-Feld nicht einhaltbar.

### 6. Way-Partitionierung (CAT/MPAM): bewertet, nicht gebaut — und der Grund ist eine Messung

Auf dem Entwicklungsrechner gibt es sie nicht: 13th-Gen-Core-i7, keine `cat_l3`/`rdt_a`-Flag in
`/proc/cpuinfo`, kein `resctrl`. Sie ließe sich hier bauen, aber **nicht prüfen** — und eine
Zusicherung ohne Messung ist hier kein Fortschritt.

Der Entwurfspunkt gilt unabhängig davon: Färbung schränkt ein, welche *Sets* eine PD belegen kann,
CAT, welche *Ways*. **Beide gleichzeitig ohne gemeinsame Politik ist schlechter als eine** —
dieselbe Falle wie Farbe gegen NUMA. Der Vorteil von CAT wäre genau das, was der Färbung fehlt:
keine Bindung an Physadressen, also **auch als Gast wirksam** (§12 misst, dass Färbung unter KVM
gar nicht trägt).

### 7. Drei Einträge, die offen standen und erledigt waren

Beim Nachlesen gefunden, nicht beim Bauen: **Z11b** (Manifest signiert und an das Kernel-Image
gebunden — steht seit A-1.2/A-1.3), **Z11e** (`iface_version` wird durchgesetzt — A-4.4) und
**Z11f** (die Negativliste in `docs/invariants.md` §13). Alle drei sind aus `todo.md` heraus.

---

## D0 — der Thread lief, bevor er seine PD hatte (gefangen und behoben 2026-08-07)

**Wie lange das offen war.** Seit dem 2026-07-29. Vier Messreihen, drei Erklärungsversuche, eine
Umbenennung des Fehlerbilds — und am Ende war es keine der drei Hypothesen aus dem Eintrag
(SMP-Hochlauf, Konsolensperre, Idle-Schleife).

### 1. Warum er sich so lange nicht fangen ließ

Die Rate ist **0,0180 %** — einer je 5556 Läufen. Jede bisherige Messreihe war zu klein:

| Reihe | Läufe | Erwartete Treffer | Ergebnis |
|---|---|---|---|
| 2026-08-01 | 200 | 0,04 | 1 (Zufall — das war ein *anderes* Bild) |
| 2026-08-03 | 2300 | 0,41 | 0 → „ausgeschlossen" |
| 2026-08-07 | **50 000** | **9,0** | **9** |

Die 2300 sauberen Läufe vom 2026-08-03 wurden damals als „die alte Quote von 0,5 % ist
ausgeschlossen" gelesen. Das stimmte — und war trotzdem irreführend: bei der **wahren** Rate war
`0,99982²³⁰⁰ ≈ 66 %`, ein Nullbefund also der *wahrscheinlichste* Ausgang. **Eine Messung, deren
wahrscheinlichstes Ergebnis „nichts" ist, belegt nichts.** Die Zahl, die dazugehört, ist nicht die
Stichprobengröße, sondern die erwartete Trefferzahl.

### 2. Der Fehler

    let (srv, cli) = (spawn(ipc_server, ..), spawn(ipc_client, ..));   // ab hier LAUFFAEHIG
    bind_pd(srv_pd, srv); bind_pd(cli_pd, cli);                        // Autoritaet erst hier

Zwischen den beiden Zeilen liegt der gesamte Aufbau des Clients: eine Stackbelegung unter der
MEM-Sperre, ein `sched.spawn`, ein `CAPS.write()`. Fällt der Server in dieses Fenster, macht er
sein erstes `RECV` mit **leerem Cspace** — dann ist nicht *eine* Cap unsichtbar, sondern jede. Er
bekommt `ERR_NOPD`, verlässt seine Schleife und dreht für immer in `spin_loop()`. Der Client
wartet 61 s auf eine Antwort von einem Empfänger, den es nicht mehr gibt.

**Es sah deshalb nie nach einem Deadlock aus.** Der Knoten lief die vollen 61 s durch
(`ticks=6104` gegen 52 in der Referenz, Worker-Runden 4182 gegen 27) und bestand jede andere
Prüfung. Nur zwei Zeilen wichen ab — und die zweite (`freeze : IPC-Rolle-abgewiesen=false`) war
die, die es verriet: sie prüft `freeze_thread(IPC_SERVER_TID)` und erwartet `Busy`, weil ein
Thread in `RECV` eine offene IPC-Beziehung hat. `false` heißt: **der Server ist in keiner
IPC-Rolle mehr.**

**Der Riss steckt auch im Produktionspfad.** `load_into_pd` macht den Thread lauffähig, bindet
danach die PD und installiert **danach** das Endowment. Dort deckt ein `local_irq_save` die
Lücke — aber nur unter zwei Bedingungen, die nirgends festgeschrieben sind: die Ready-Queue ist
streng kernlokal, und der Lastausgleich ist aus (Vorgabe). Wer den Schalter umlegt, öffnet ein
Fenster, in dem eine geladene Treiber-PD **ohne jede Cap** anläuft. Dieses Projekt hat den
Formfehler schon einmal bezahlt: *„Eine lokale IRQ-Sperre ist kein Fenster über eine geteilte
Größe."* Hier hält sie zufällig — das ist kein Grund, sie halten zu lassen.

**Und das Wissen war da.** An genau **einer** von 53 Stellen steht seit jeher
`hal::cpu::local_irq_disable()` mit dem Kommentar *„atomar gegen Preempt, damit es nicht vor dem
Bind läuft"*. Eine Gefahr, die an einer Stelle per Hand abgewehrt wird und an 52 nicht, ist keine
Sorgfaltsfrage — sie ist ein **fehlender Mechanismus**.

### 3. Die Behebung

Es gibt keine Reihenfolge, die trägt: `bind_pd` braucht die `tid`, die `spawn` erst liefert. Die
Lücke lässt sich verkleinern, nicht schließen. Also wird **erzeugen** von **zulassen** getrennt:

| | |
|---|---|
| `Scheduler::spawn_parked` / `spawn_user_at_parked` | legt an, reiht **nicht** ein |
| `Scheduler::admit` | reiht ein — ab hier darf er laufen |
| `system::spawn_*_parked` (7 Erzeuger) | dieselbe Trennung eine Ebene höher |
| `system::admit_in_pd(pd, tid)` | binden, dann zulassen |
| `system::spawn_in_pd(pd, ..)` | alles drei in einem Aufruf |

Kein `park: bool`. Ein Schalter wäre wieder ein **wählbarer Grund**, und die bequeme Belegung wäre
die falsche — derselbe Befund wie bei `IdentityReason`. Zwei Namen sind nicht zu verwechseln.

Umgestellt: **58 Aufrufstellen** (46 mechanisch, 12 von Hand), dazu `load_into_pd` und der
IPC-Aufbau. Eine der zwölf war ein eigener Fehler derselben Art: die `page_probe`-Sonde bekam ihre
drei Mappings **nach** dem Binden. Ein `admit_in_pd` an dieser Stelle hätte sie loslaufen lassen,
bevor die Seiten standen — sie hätte auf P statt auf P+8KiB gefaultet, und der Test hätte etwas
anderes gemessen, als er sagt. **Die naheliegende Fassung einer Behebung kann den Fehler
mitnehmen.**

**Und die PD ist nicht die ganze Autorität.** Ein systematisches Gegenlesen — nicht nach `bind_pd`,
sondern nach *allem*, was nach der Zulassung noch Autorität vergibt — fand drei weitere Stellen
(`killer`, `producer`, `consumer`): dort stand `install_pd_cap` **hinter** dem `admit_in_pd`. Für
den Thread sind ein leerer Cspace und ein Cspace ohne die eine gebrauchte Cap **dasselbe** — er
bekommt `ERR_BADCAP` statt `ERR_NOPD`, und beim Killer-Test hätte das eine Verweigerung gemeldet,
die keine ist. Sie lauten jetzt `bind_pd` → `install_pd_cap` → `admit`.

Das Gegenlesen selbst ist der Punkt: gesucht wurde nach `map_into_thread`, `install_pd_cap` und
`set_vspace_of` **nach** einer Zulassung auf demselben Ziel — nicht nach dem Muster, das den
Befund ausgelöst hatte. Ein Audit, der nur die gefundene Form sucht, findet nur sie wieder.

### 4. Der Wächter zählt die GELEGENHEIT, nicht den Treffer

Bei 0,018 % wäre ein Melder, der nur beim Unglück spricht, in 5555 von 5556 Läufen stumm. Die
**Reihenfolge** dagegen ist in jedem Lauf prüfbar: `bind_pd` fragt vorher `is_admitted(tid)` und
zählt `LATE_PD_BIND`. Drei Dinge gehören dazu:

* **Auf dem Kern des Threads**, nicht des Aufrufers — `is_admitted` löst gegen `self.core` auf und
  wäre sonst ausgerechnet bei `spawn_on_core(1, ..)` blind, also dort, wo das Rennen am ehesten
  trifft.
* `unklar` **getrennt** gezählt: „nicht auflösbar" ist kein „rechtzeitig".
* **Sprechprobe**: die Zeile fällt durch, wenn in diesem Lauf gar keine Bindung beobachtet wurde.
  Ein Zähler, der auf Null steht, weil nichts passierte, ist kein Testergebnis.

Erklärte Spätbindungen tragen einen **Namen** (`SpaetbindungsGrund`, heute genau einer: Z4 Stufe 2
bindet dem Checkpoint-Subjekt seinen Umfang nachträglich an, und der Worker *benutzt* keine Cap).
Kein `if tid == WORKER_TID0` — eine Ausnahme ohne Namen wächst unsichtbar.

### 5. Was der Modell-Treue-Wächter dazu gesagt hat

Er hat die Änderung **von selbst** beanstandet, und zwar dreifach richtig: `Tcb.admitted` sei
keinem Modellfeld zugeordnet; `admit` schreibe Scheduler-Zustand, ohne benannt zu sein; und
`spawn`/`spawn_user_at` seien *„als zustandsschreibend benannt, schreiben aber nichts (mehr)"* —
ein **veralteter Registereintrag**, den ohne ihn niemand bemerkt hätte.

`admitted` ist bewusst **nicht** auf `in_ready` abgebildet: die beiden bedeuten Verschiedenes.
`in_ready` fällt zurück, sobald der Thread blockiert oder läuft; `admitted` bleibt wahr. Ein Feld
auf ein Modellfeld abzubilden, das etwas anderes heißt, wäre schlimmer, als es außerhalb zu
führen — der Beweis zeigte dann eine Aussage über `in_ready`, und man **läse** sie als Aussage
über `admitted`.

### 6. Die Abnahme — und was sie NICHT sagt

| | vorher | nachher |
|---|---|---|
| Läufe | 50 000 | 50 000 |
| D0-Treffer | **9** | **0** |

**Was gedeckt ist:** Ursache identifiziert, 5 von 5 Treffern zeichengleich (`ERR_NOPD`, null
bediente Anfragen), strukturell umgebaut, 50 000 Läufe ohne Treffer.

**Was NICHT gedeckt ist, und das gehört danebengeschrieben:**

1. **Die Bedingungen der beiden Reihen sind nicht dieselben.** Zwischen ihnen wurde der
   Speicherregler berichtigt — vorher maß er den RSS der Subshell statt QEMUs Prozessbaum, fiel
   unter die Untergrenze und rechnete mit 192 statt 361 MiB je Lauf. Bei einem **Startrennen** ist
   die Parallelität genau die Größe, die die Trefferrate erzeugt. `P(0 | unveränderte Rate) ≈
   1,2·10⁻⁴` rechnet damit gegen die Rate der *alten* Bedingungen und misst unter *neuen*: die
   Zahl steht für „behoben **oder** weniger Druck", und die beiden sind nicht getrennt.
2. **Schlimmer: die Bedingung der Fundmessung ist nicht mehr feststellbar.** Ihr Protokoll ist in
   der Mitte abgeschnitten; die Zeile „Regler steht bei N Arbeitern" fehlt. Ich kann nicht sagen,
   unter welcher Parallelität die 9 Treffer entstanden sind. Deshalb nimmt `tools/d0-messen.sh`
   jetzt `ARBEITER_FEST=N` und **nennt die Bedingung in der Bilanz** — eine Messbedingung, die
   nicht im Ergebnis steht, ist beim nächsten Vergleich verloren.
3. **Die x86-Reihe prüft den Umbau fast nicht.** `pdbind` zählt auf x86 **3** Bindungen, auf
   aarch64 **70** — `kernel/src/threads/mod.rs` ist `#[cfg(target_arch = "aarch64")]`. 50 000
   x86-Läufe decken also drei Zulassungsstellen ab und lassen siebzig am unbeobachteten Ende.
   Der Messstand fährt seit 2026-08-07 deshalb auch `ARCH=arm`.

Was die Nullmessung trotzdem wert macht: derselbe Aufbau hat den Fehler nachweislich **gesehen** —
9-mal in Reihe 1, 5-mal in einer Zwischenreihe über 6895 Läufe. Eine Nullmessung ohne diesen Beleg
wäre gar nichts.

### 6a. Der aarch64-Hänger: gemessen, nicht zugeordnet

Beim Umbau meldete ich „die aarch64-Suite hängt jetzt: irgendein Thread wird geparkt und nie
zugelassen", fand danach 6 von 6 grün und ordnete es dem bekannten Sporadikum zu. **Das war eine
Entlastung durch Erinnerung** — bei einer Rate um 5 % liefert 6/6 grün nichts (die eigene Rechnung
aus D12: 71 % Chance auf null Treffer bei identischer Rate), und die Zuordnung zu einem bekannten
Sporadikum ist genau die Struktur von „trat auch vorher auf".

Nachgeholt als Messung, 32 Läufe bei 16-facher Parallelität, **9 Abweichungen**:

* **9 von 9: `bringup : offen: color`** — dieselbe Aussage, jedes Mal.
* Die Farbzeilen sind **byte-identisch zur Referenz** (`color : ALL PASS`, `stripe : ALL PASS`,
  `pprobe : SKIP`). Es fällt also nichts durch.
* Der Watchdog feuert **zwischen** dem Druck der Farbsuite (Zeile 102) und dem `COLOR_DONE`-Store
  (Zeile 104). Die Frist sind 6000 **Ticks** — Wanduhrzeit — und die Farbsuite ist auf `cross`,
  `strand` und `loadstop` gegatet, läuft also als letzte. Unter Überbuchung laufen Ticks weiter,
  die Gastausführung nicht.

Also **D13, nicht D0 und nicht D6** — und kein geparkter Thread. Die Frage war überhaupt erst
beantwortbar, weil der aarch64-Watchdog seit 2026-08-07 **nennt**, was offen war: bis dahin
versprach die Kopfzeile „offene Tests:" und druckte den vollen Bericht, in dem eine nie gesetzte
Aussage von einer bestandenen nicht zu unterscheiden ist. x86 nennt sie seit jeher, und genau
diese Zeile hat D0 eingegrenzt.

### 6b. Was der Zähler NICHT sehen kann — und der Typ, der es schließt

`spaet == 0` zählt **späte Bindungen**, nicht **ausbleibende Zulassungen**. Eine 62. Aufrufstelle,
die `spawn_parked` ruft und `admit` vergisst, ist daran nicht zu sehen. Und die vier Stellen mit
Autorität *nach* der Zulassung fand ein Gegenlesen — Auffindbarkeit, nicht Unmöglichkeit.

Seit 2026-08-07 gibt `spawn_*_parked` deshalb ein **`Parked`** zurück: `#[must_use]`, kein
`Drop`-Impl (sonst ließe sich das Feld in `admit` nicht herausbewegen), kein öffentlicher Weg an
die `ThreadId`. Wer sie braucht, ruft `admit`, und das verbraucht den Zeugen. Alles, was vorher
geschehen muss — PD binden, Caps setzen, Seiten mappen — läuft über `&Parked`.

**Der Typ hat sofort eine fünfte Stelle gefunden, die das Gegenlesen übersehen hatte:**
`map_region_into_thread` an drei Geräte-Backends (RTC, IRQ, DMA) lief **nach** der Zulassung. Mein
Scan suchte nach `map_into_thread` und `install_pd_cap`; diese Variante kam darin nicht vor. Der
Kommentar an einer der Stellen sagt selbst, was dann passiert: *„ohne dieses Mapping faultet das
Backend beim RTC-Read"*.

**Und die naheliegende Umstellung nahm den Fehler mit:** das mechanische Rebinding machte `b`
wieder zu einer `ThreadId`, also übersetzte `map_region_into_thread` weiter — das Rennen blieb.
Erst von Hand ist daraus binden → mappen → zulassen geworden.

Bewacht wird der Zeuge von `tools/zulassung.sh` (kein öffentliches Feld, kein `Copy`, kein
öffentlicher Ausgang, `admit` nimmt per Wert), mit Selbsttest in beide Richtungen — 7 von 7. Dort
liegt auch der **Ankertest** für `ERLAUBTE_SPAETBINDUNGEN`: Menge statt Zahl, und jeder Name muss
eine echte Variante bezeichnen, in beide Richtungen.

### 6bb. Der Umbau hat eine Regression erzeugt — gefunden vom Audit des Kernels selbst

Die aarch64-Reihe (600 Läufe, feste Parallelität 6) meldete **23 Abweichungen**. 22 davon sind
D13 (`offen: color`). Die dreiundzwanzigste ist etwas anderes, und sie ist der eigentliche Ertrag
der Reihe:

    scale : 1024 von 1024 Threads GLEICHZEITIG erzeugt (alle auffindbar=true,
            Slot-Buchhaltung=true), danach abgebaut -> Baseline=true, sched_audit=7
    scale : FAILURES

Der Lauf ist **vollständig durchgelaufen** (`SELFTEST COMPLETE`, kein Watchdog), jedes andere Feld
ist identisch zur Referenz. Nur `sched_audit` steht auf 7 statt 0.

**Code 7 ist wörtlich der Zustand eines geparkten Threads:**

    if !t.blocked && !t.depleted && self.current != Some(local) && t.queued == NOT_QUEUED {
        return 7;    // „lauffaehig und in keiner Liste"
    }

Vor der D0-Behebung konnte es diesen Zustand nicht geben — `spawn` reihte sofort ein. Seit
`spawn_parked` gibt es ihn, und er ist **richtig**: der Thread wartet darauf, dass sein Erzeuger
ihm PD, Caps und Mappings gibt. Der Audit kannte das Parken nicht und meldete zu Recht, was er
sah. Behoben durch `t.admitted` in der Bedingung — für alles, wofür Code 7 gebaut wurde (D8: ein
erschöpfter Thread, der über `unblock` lauffähig wird), gilt `admitted == true`, die Schärfe bleibt.

**Drei Dinge, die daran hängen:**

1. **Die x86-Messung konnte das nicht finden.** `pdbind` zählt auf x86 3 Bindungen, auf aarch64 70
   — das Fenster zwischen `spawn_parked` und `admit` gibt es auf x86 dreimal je Lauf, auf aarch64
   siebzigmal. In 56 895 x86-Läufen trat das Bild nie auf; in 600 aarch64-Läufen einmal. Eine
   aarch64-Reihe ist hier mehr wert als weitere x86-Läufe, und das ist jetzt gemessen statt
   argumentiert.
2. **Der Modell-Treue-Wächter hat die Berichtigung sofort beanstandet** und die Bedingung
   vorher/nachher gegenübergestellt. Dabei fiel ein Eintrag auf, den ich am selben Tag geschrieben
   hatte: `admitted` stand als „von keiner Einplanungsentscheidung gelesen" — seit der Audit es
   liest, stimmt das nicht mehr. Eine Einplanungs*entscheidung* ist es weiterhin nicht, aber ein
   **Urteil** hängt daran, und das ist mehr als Beobachtung.
3. **Das ist die Klasse, vor der gewarnt war.** Ein Umbau, der einen neuen Zustand einführt, muss
   jede Stelle mitnehmen, die über Zustände urteilt — nicht nur die, die sie erzeugen. Gefunden
   hat es kein Gegenlesen und kein Typ, sondern eine **Messung unter Last auf der Architektur, wo
   der neue Zustand oft vorkommt**.

### 6c. Die Gegenprobe fand, dass der Wächter nichts gattert

`pdbind` stand im Bericht — und in keiner Abschlussbedingung. Eine Mutation, die `spawn_in_pd`
zuerst zulassen und dann binden ließ, ergab `pdbind : FAILURES`, **und die Suite meldete
`== ALL PASS ==`**.

Die Ursache war größer als die Zeile: x86s `all_done()` baute eine Liste **für den Bericht** und
gab eine **getrennte `&&`-Kette** zurück. Zwei Wirklichkeiten aus derselben Hand — die Kette hatte
21 Glieder, die Liste 24; `pdcolor`, `ladepol` und `pdbind` standen im Bericht und gatterten
nichts. Seit 2026-08-07 ist die Liste **das Urteil** (`flags.iter().all(..)`), auf beiden
Architekturen. Danach zeigt die Mutation `bringup : offen waren: pdbind` → `== FAILURES ==`.

### 6d. Was Verus dazu NICHT sagt

Das IPC-Modell kennt den Begriff „Thread ohne PD" **nicht** — null Vorkommen von PD-Bindung oder
`ERR_NOPD` in `Verification/ipc/proofs/`. „16 Beweisdateien, 0 errors" heißt hier also nur, dass
die vorhandenen Beweise weiter halten; über die neue Eigenschaft sagt es **nichts**. Eine
Invariante, die `RECV` an eine gebundene PD knüpft, gibt es nicht, und bloße Repräsentierbarkeit
des Zustands wäre auch keine. Das steht als offener Punkt in `todo.md`.

### 7. Die vier Abweichungen, die übrig blieben — und warum sie kein Kernelbefund sind

Die zweite Reihe meldete 4 Abweichungen in 50 000 Läufen, **keine davon D0** (`Schleife
verlassen: nein` in allen vieren). Zwei Bilder:

| Bild | Zahl | Messwert |
|---|---|---|
| `cycles : FAILURES` | 2 | 1-ms-Fenster = 11 689 334 bzw. 11 720 490 Zyklen statt 2 803 578 — **Faktor 4,2** |
| `freeze : FAILURES` | 2 | ein ~30-ms-Fenster sah **0** Worker-Runden statt 3 |

**Die Entscheidung fällt nicht an der Zahl, sondern an der FORM.** Bei einem der beiden
`freeze`-Fehlschläge fiel `laeuft-vorher=false (49->49)` durch — das ist die **Positivkontrolle**,
gemessen *bevor* überhaupt eingefroren wird. Ein Fehler im Auftaupfad kann sie strukturell nicht
verursachen. Damit ist die naheliegende Lesart („die Umstellung hat `thaw` beschädigt") widerlegt,
ohne dass man den Auftaupfad überhaupt ansehen muss.

Was beide Bilder gemeinsam haben: ein Fenster, das in **Wanduhrzeit** definiert ist. Der Messstand
fährt 16 Gäste zu je 4 vCPU auf 20 Kernen — **3,2-fache Überbuchung**. Wird eine vCPU vom Wirt
verdrängt, laufen Ticks und TSC weiter, die Gastausführung nicht. Der Faktor 4,2 im
`cycles`-Fenster ist genau das, gemessen.

Das ist ein Befund über den **Messstand**, nicht über den Kernel — steht als eigener Punkt in
`todo.md`. Belege: `docs/befunde/d0/lastartefakt-{freeze,cycles}-2026-08-07.log`.

### 8. Der Melder, ohne den nichts davon messbar gewesen wäre

Der Server-Rumpf lautete `if m.result != result::OK { break; }` — der **Grund** des Ausstiegs fiel
auf den Boden. Eine Zeile (`IPC_SERVER_EXIT.store(m.result, ..)` plus eine Berichtszeile) machte
aus einer Hypothese eine Messung: **4 von 4** Treffern `ERR_NOPD`, `bediente 0 Anfrage(n)`. Ohne
sie hätte das Vollprotokoll nur gesagt, *dass* der Server weg ist, nicht *warum*.

---

## D11 — der Ueberlauf einer Endpoint-Warteschlange ist benannt (2026-08-04)

**Der Fehler.** `TidQueue::enqueue` war ein `if self.count < QCAP { .. }` **ohne `else`**. Bei
`QUEUE_CAP = 32` landeten von 33 CALLs 32 in der Queue. Der 33.: `block_current` lief trotzdem,
der Sentinel `0xDEADBEEF` blieb unberuehrt im Frame (kein Ergebniscode), er stand in **keiner**
Struktur des Endpoints, 33 nachfolgende RECVs bedienten 32 — und `quiescence_of(..).is_quiescent()`
meldete ihn als **ruhig**, `audit` `(false,false)`, `purge_thread` `false`. Ein Faden haengt
dauerhaft, und JEDER Pruefer meldet Ordnung. Schlimmer als die leere Ereigniswarteschlange ohne
`CD.R`, weil die Ruhemeldung zusaetzlich einen Hot-Reload (A-4.2) freigegeben haette.

Dieselbe Zeile traf vier Wege: den 33. RECV ebenso; `bind_receiver` meldete `true`, obwohl
verworfen; `migrate_owner` meldete `true`, **loeschte die Antwortpflicht** und verlor den Aufrufer
— ein Client, der auf eine Antwort wartet, die niemand mehr schuldet.

**Eine fuenfte Fundstelle, die der Befund nicht nannte.** `Notification::wait` hat dieselbe Form
bei Kapazitaet 1: ein zweiter `WAIT` **ueberschrieb** `waiter`. Der Ueberschriebene blieb
blockiert und war danach fuer `purge_thread`/`audit`/`quiescence_of` unsichtbar. Dass die
Kapazitaet hier 1 statt 32 ist, aendert an der Struktur des Fehlers nichts.

**Die Behebung.** `enqueue -> bool`, `#[must_use]`, und **jede** Aufrufstelle wertet aus. Die
beiden, an denen `false` strukturell unmoeglich ist (`TidQueue::remove`, `rebind_server`), sagen
dort **warum** — nicht „das wird schon". `call`/`recv` weisen mit `ERR_EP_FULL` ab und blockieren
**nicht**; `bind_receiver` meldet Misserfolg; `migrate_owner` prueft `is_full()` **vor** dem
`take()` und ist im vollen Fall ein reines No-Op — fail-closed heisst hier: die Antwortpflicht
bleibt beim alten Besitzer, und der Aufrufer laeuft ueber den vorhandenen Weg (`owner_died` ->
`ERR_SERVER_GONE`) auf. Eine begonnene Transaktion, die ehrlich scheitert, statt einer, die
lautlos verschwindet.

**Warum ein DRITTER Code (`ERR_EP_FULL = 9`) und nicht `ERR_QUIESCING`.** Das war die Frage, die
der Befund offenliess. Die drei Lagen verlangen vom Client verschiedene Antworten: `ERR_BADCAP`
heisst „gibt es nicht" (nie wieder versuchen), `ERR_QUIESCING` „kommt gleich wieder" (die
Wartezeit ist durch den Austausch begrenzt und haengt nicht an fremdem Verhalten), `ERR_EP_FULL`
„gerade kein Platz" — eine **Lastaussage**: sie haengt an den anderen 32 Wartenden, kann sofort
wieder gelten, und wer stumpf wiederholt, verschaerft sie. Die beiden zusammenzuwerfen naehme dem
Client genau die Unterscheidung, die A-4.2 gerade eingefuehrt hat.

**Das Modell ist dem Code gefolgt, nicht umgekehrt.** `Verification/ipc/proofs/endpoint.rs`:
`dropped_*` -> `rejected_*` (ein Verlust ist unbeobachtbar, eine Abweisung ist eine Antwort),
neue `send_gate`/`recv_gate` mit Code 3, `core_eq` fuer „die Operation hat den Endpoint nicht
angefasst". **25 -> 30 Beweise, 0 errors.** Der Beweis, um den es geht, ist
`send_never_strands`/`recv_never_strands`: unter offenem Tor gibt es nur zwei Ausgaenge —
zugestellt/eingereiht oder abgewiesen-mit-Code. Einen dritten gibt es nicht mehr; genau der war
D11. Dazu `gate_three_reasons_distinct`, `bind_receiver_full_is_noop` und
`migrate_owner_full_keeps_token`.

Bemerkenswert am alten Stand: `send_drops_above_cap` **bewies den Verlust** — der Beweis war
richtig, der Code war falsch. Ein Beweis, der dem Code treu ist, kann das Falsche beweisen.

**Belegt statt behauptet.** `tools/verus-modelltreue-ipc.sh` faehrt den echten Quelltext gegen das
uebersetzte Modell: **93 -> 99 Faelle**, **28 -> 35 Selbsttestfaelle**. Neu ist das **Hauptbuch
der Gestrandeten** (`Welt::gestrandete`): jeder Faden, der nach einer Operation blockiert ist und
in KEINER Struktur steht, wird vermerkt; nach einem Lauf ueber alle vier Ueberlaufwege muss die
Liste leer sein. Die Positivkontrolle dazu ist keine Zeile im Lauf, sondern **fuenf Mutationen**,
die D11 einzeln wiederherstellen (auch die Wurzel: „`enqueue` meldet beim Ueberlauf Erfolg") —
jede wird erkannt. Eine leere Liste ist nur dann eine Aussage, wenn sie sich fuellen kann.

Die `befund`-Mechanik des Waechters hat dabei genau das getan, wofuer sie gebaut wurde: als die
Behebung stand, schlug sie an („der Befund trifft nicht mehr zu") und verlangte, dass B2/B2b/B2c/B2d
aus ihrem Kopf heraus und hierher wandern.

**Im Kernel sichtbar:** Pruefzeile `epfull` (`system::run_epfull`, lokales Objekt wie `run_quiesce`
und `run_rebind`). Sie prueft den Weg, der ohne Scheduler auskommt — `bind_receiver` —, und die
Positivkontrolle steckt in der Anlage: die ersten 32 muessen gelingen **und auffindbar sein**,
sonst waere die Zeile von „bind_receiver geht nie" nicht zu unterscheiden. Gegenprobe gefahren:
die alte Fassung wieder eingesetzt -> `epfull : FAILURES` und `bringup : offen waren: epfull`.

Die drei blockierenden Wege (`call`/`recv`/`migrate_owner`) misst der Host-Waechter gegen
denselben Quelltext. Das steht so in der Pruefzeile, damit die Suite nicht mehr behauptet, als sie
prueft.

---

## Ein Ausstieg, der nicht ausschliesst -- und Zeugen, die nicht entkommen (2026-08-06)

**Der D12-Ausstieg schloss seltene Kernelfehler per Definition aus.** Die erste Fassung verlangte
eine **reproduzierbare** Pruefzeile. Die Ausfaelle liegen bei grob 1 in 32 -- die Klasse, um die
es geht, ist genau die, die nicht reproduziert: ein verpasstes `WFE`-Wakeup, ein Timer/IPI-Rennen.
Beide haetten das Kriterium nie erfuellt und waeren auf ewig unter „Geruest" gefallen. Damit haette
das Prior eine Tuer bekommen, durch die es nicht widerlegt werden kann -- **dieselbe Bequemlichkeit
wie „trat auch vorher auf", nur mit umgekehrtem Vorzeichen.**

Das Kriterium haengt jetzt an der **Form des Artefakts**, und ein **einzelner** Lauf kann es
erfuellen: vollstaendige Ausgabe bis zum Ende, Exit-Code passend zum Urteil, und eine Pruefzeile,
die **inhaltlich** abweicht (Werte, Zaehler, Reihenfolge) statt bloss zu fehlen. Wiederholbarkeit
gehoert in die **Priorisierung**, nicht in die Klassifikation: ein einmaliger Kernel-Befund ist
einer, auch wenn er schwerer zu jagen ist.

**Die Zeugen konnten entkommen.** `MmioWindowWitness(())` ist ausserhalb nicht herstellbar --
innerhalb des Engstellen-Moduls aber beliebig oft, und nichts hinderte dort ein
`pub fn witness() -> MmioWindowWitness`, ein `#[derive(Clone, Copy)]` oder ein oeffentliches Feld.
**rustc prueft Herstellbarkeit, nicht Nicht-Weitergabe** -- und die Namenstabelle, die das frueher
gemerkt haette, war mit dem Zeugen-Umbau entfallen. Der Waechter prueft jetzt genau diese Luecke:
kein Zeuge leitet `Clone`/`Copy`/`Default` ab, keiner steht in Rueckgabeposition einer
oeffentlichen Funktion, keiner als oeffentliches Feld. Alle drei Wege als Gegenprobe gefahren.

Der dritte fiel dabei zuerst durch: mein Muster war zeilenanfangs verankert und uebersah ein
`pub struct T { pub w: MmioWindowWitness }` in EINER Zeile. **Ein Muster, das nur die uebliche
Formatierung trifft, prueft den Stil und nicht die Eigenschaft.**

**Die Reichweiten-Nachrechnung deckte nur den neuen Sammler.** Nachgeholt fuer alle Messreihen
dieser Sitzung:

| Bau | Gruen-Kriterium | fehlsicher? |
|---|---|---|
| `sammellauf.sh` (1 Lauf, vor der Behebung) | leere Zeile galt als Erfolg | **nein** -- `load 6G`, Wiederholung dann echt gruen |
| `armlast.sh` (aarch64-Bisect) | exakter Zeichenvergleich | ja |
| `armfang.sh` | `grep -q "ALL PASS"` auf der Schlusszeile | ja |
| die `for M in …`-Schleifen | letzte Zeile gedruckt, von mir gelesen | ja, mit menschlicher Einschraenkung |

Der Grund steht im Quelltext der Suiten und wurde nachgesehen: `== ALL PASS ==` wird
**ausschliesslich** im `fail = 0`-Zweig als letzte Ausgabe vor `exit "$fail"` geschrieben. Ein
abgebrochener oder durchgefallener Lauf kann sie nicht als letzte tragen; Text und Exit-Code
koennen in der gruenen Richtung nicht auseinanderlaufen. **Damit steht die Gruen-Bilanz dieser
Sitzung**, mit der einen benannten Ausnahme.

**`SAMMELLAUF_BEHALTEN=0` ist weg -- Rotation statt Abschalter.** So ein Schalter wird irgendwann
aus Platzgruenden gesetzt und nie zurueckgenommen; danach ist die Rueckhaltung weg, und niemand
merkt es. Jetzt hoechstens `SAMMELLAUF_MAX` (200) Protokolle, aelteste fallen weg. Dieselbe
Ueberlegung wie beim Loeschen der Logs vor der Pruefung, eine Ebene hoeher.

---

## Erfundene Erfolge, und warum der Sammler nur `$?` lesen darf (2026-08-05)

**Der schwerere Teil des Sammler-Fehlers war nicht der verlorene Fehlschlag.** Ich hatte die zwei
Loecher als „Fehlerbilder weg" verbucht. Das zweite -- eine **leere** Schlusszeile galt als
Erfolg -- hat aber kein Bild verloren, sondern einen **Erfolg erfunden**: ein Lauf, der mitten in
der Ausgabe endete, wurde gruen gebucht. Und weil bei Erfolg geloescht wurde, liess sich nicht
nachzaehlen, wie oft. **Ein Zaehler, der hochzaehlt, wenn nichts geschieht, beschaedigt die
GRUEN-Bilanz, nicht nur die rote.**

**Reichweite, so genau wie sie geht** -- und das ist der Unterschied zwischen „Bilanz kaputt" und
„Bilanz nachgerechnet": die Datei existierte fuer **einen** Sammellauf, bevor der Fehler auffiel;
dort zeigte `load 6G` die leere Schlusszeile (der Wiederholungslauf war dann echt gruen). Die
aarch64-Bisect-Reihe lief **nicht** ueber diese Datei, sondern ueber ein Skript mit exaktem
Zeichenvergleich (`[ "$r" = "== ALL PASS ==" ]`) -- eine abgeschnittene Zeile waere dort NICHT als
gruen gezaehlt worden. Die 6/6 sind unberuehrt.

**(A) Der Ausgang entscheidet sich am EXIT-CODE.** Die erste Fassung verglich Schlusszeilen --
erst gegen `ALL PASS`, dann zusaetzlich gegen die Formel der Waechter, morgen gegen die der
naechsten Suite. Das ist derselbe Befund wie beim ersten Identitaets-Waechter: ein Pruefer, dessen
Schluessel nicht die des Registers sind. Der Schluessel ist `$?`. Gibt eine Suite bei Fehlschlag
`0` zurueck, ist **das** der Fehler -- einer in der Suite; der Sammler meldet den Widerspruch als
Befund UEBER die Suite, richtet sein Urteil aber nicht danach. So waechst keine Formelliste, und
diese Fehlerklasse ist strukturell weg.

**(B) Protokolle werden vorerst auch bei ERFOLG behalten**, bis die Datei ein paar Dutzend Laeufe
getragen hat. Solange ist „gruen" eine Aussage des Sammlers ueber sich selbst. Speicher ist
billiger als eine zweite Runde dieser Erkenntnis.

Fuenf Faelle als Gegenprobe gefahren: Erfolg, Waechter-Formel mit eigener Schlussformel, leere
Schlusszeile, `exit 0` mit `FAILURES`, `exit 1`.

## Der Zeuge: die Bindung Stelle<->Grund haelt jetzt rustc (E-Rest 3g, 2026-08-05)

Mein Grund fuer die Vertagung war falsch. „`pub(in path)` verlangt einen Vorfahren, und `Va` liegt
in `crate::addr`" stimmt -- geht aber am Punkt vorbei: **der Zeuge braucht keinen Vorfahren.**

```rust
// im Modul der Engstelle:
pub struct MmioWindowWitness(());        // Feld privat -> nur hier herstellbar
// in crate::addr:
pub fn for_mmio_window(_w: crate::system::MmioWindowWitness, pa: Pa) -> Va { .. }
```

Der Typ ist ausserhalb **nennbar**, aber nicht **herstellbar**. Eine Zeile je Engstelle, kein
Umzug. Ich hatte es als „Entwurfsarbeit" eingestuft -- dieselbe Fehleinstufung, die `tail -1`
neben Entwurfsarbeit geparkt hat.

**Der Beleg ist der Bau selbst:** `bringup.rs` konnte den Zeugen fuer das globale Geraetefenster
nicht herstellen und scheiterte mit „argument #1 of type `KernelGlobalWindowWitness` is missing".
Statt den Zeugen oeffentlich konstruierbar zu machen (was ihn wertlos machte), wandert der Aufruf
hinter `system::map_device_window_global`. Nebenertrag: die Schichtung stimmt danach besser --
`bringup` sagt WAS, `system` entscheidet unter welcher Achse.

**Die Tabelle Konstruktor->aufrufende Funktion im Waechter ist ersatzlos entfallen.** Er prueft
nur noch, dass die Zeugen so gebaut sind (privates Feld, einer je Konstruktor) und dass jeder
Konstruktor seinen verlangt -- also das, was ein Typ nicht ueber sich selbst aussagen kann.

**Dazu der fehlende Ankertest der Mengenpruefung.** `IDENTITY_DEBTS` wurde gegen die Schuldnamen
aus `class()` verglichen -- aber niemand prueft, ob diese Namen ueberhaupt noch Varianten sind.
Eine Umbenennung haette einen „Austausch" gemeldet, waehrend in Wahrheit der Anker weg ist. Beim
`SyscallMapByCap`-Falsifikator stand dieser Test seit dem ersten Tag; hier fehlte er. Gegenprobe
gefahren.

---

## Kardinalzahl statt Menge: derselbe Fehler in zwei neuen Waechtern (2026-08-05)

Zwei am selben Tag gebaute Waechter trugen denselben Defekt -- und es ist der, gegen den der
ganze VA==PA-Umbau geht: **eine Zahl steht, wo eine Menge gemeint ist.**

**(1) Die Stelligkeitspruefung zaehlte Aufrufe, statt sie zu binden.** Sie verlangte „ein- oder
zweimal". Ein Konstruktor je Stelle bindet aber nur den **Namen**, nicht den **Ort**:
`Va::for_mmio_window` ist eine oeffentliche Methode, der DMA-Pfad *koennte* sie rufen. Zwei
Aufrufe aus dem FALSCHEN Paar sind von Abbilden+Gegenstueck nicht zu unterscheiden, solange nur
gezaehlt wird -- die Lockerung von „hoechstens einmal" auf „zwei" hat genau das Loch aufgemacht,
das sie schliessen sollte. Der Waechter haelt jetzt eine Tabelle Konstruktor -> **aufrufende
Funktionen** und vergleicht Mengen.

Dass es ein Skript tut und nicht rustc, hat einen Grund und steht als E-Rest 3g: `pub(in path)`
verlangt einen **Vorfahren** des Elements, und `Va` liegt in `crate::addr` -- ein Modul, das nicht
darueber liegt, ist nicht ausdrueckbar. Der strukturelle Weg waere, die Engstellen in ein privates
Untermodul mit eigenen Zeugen-Typen zu ziehen.

**(2) `IDENTITY_DEBTS` war ein `usize`.** Eine Ratsche ueber einer Zahl greift nur gegen
**Zuwachs**, nicht gegen **Austausch** -- und Austausch ist der wahrscheinlichere Vorgang, weil er
sich beim Umbauen wie Fortschritt anfuehlt. Jetzt eine Liste von Namen und Mengengleichheit.
Gegenprobe gefahren: eine Schuld gegen eine andere getauscht (Zahl bleibt 3) -> der Waechter
schlaegt an und sagt dazu, dass eine Zahl das durchgelassen haette.

**(3) `tail -1` -- dreimal ein Fehlerbild gekostet, und es war eine Zeile.** Es stand als Punkt
neben Entwurfsarbeit in `todo.md`; das war die falsche Behandlung. `tools/sammellauf.sh` haelt
jetzt die **vollstaendige stdout** je Lauf fest, loescht sie nur bei Erfolg und zeigt bei Ausfall
die durchgefallenen Pruefzeilen gleich mit.

Er hat dabei binnen einer Stunde **zwei eigene Loecher** gezeigt, beide von der Sorte, die dieses
Projekt sammelt:
* Erfolg war als „`ALL PASS` in der Schlusszeile" definiert -- und legte damit die Protokolle der
  gruenen WAECHTER ab, die anders schliessen (`== Kerngrenze eingehalten ==`). Ein Sammler, der
  Erfolge als Ausfaelle ablegt, macht sein eigenes Verzeichnis unlesbar. Geprueft wird jetzt auf
  das **Fehlerwort**, nicht auf ein bestimmtes Erfolgswort.
* eine **leere** Schlusszeile galt als Erfolg. Ein Lauf, der mitten in der Ausgabe endet
  (SIGKILL, Zeitlimit, abgeschnittene Pipe), hat keine -- und ein Praedikat, das nur auf das
  Fehlerwort prueft, liest daraus „kein Fehler". **Schweigen als Erfolg**, im eigenen Werkzeug,
  eine Stunde nach dem Bau.

**(4) Keine Punktschaetzung aus n=1.** „Die Rate liegt bei 1/32" waere derselbe Fehler wie der
aarch64-Bisect, eine Ebene hoeher: ein Ausfall in 32 Laeufen gibt ein 95-%-Intervall von grob
0,5 % bis 16 %. Eine als Baseline notierte Zahl macht jede spaetere Messung unfalsifizierbar.
Festgehalten ist: **ein Ausfall in 32, Intervall breit, Rate unbestimmt.**

**(5) Die drei seltenen Ausfaelle gehoeren wahrscheinlich zusammen -- und das Prior liegt beim
GERUEST.** Der aussagekraeftigste Befund ist der `RUNS=8`-Ausfall mit einem Protokoll, dessen
**Signatur mit der eines gruenen Laufs identisch** ist: wenn alle Ergebniszeilen stimmen und der
Lauf trotzdem durchfaellt, bricht eine Pruefung, die nichts mit der gepruefeten Eigenschaft zu tun
hat. In dieser Sitzung wurde der Messaufbau **fuenfmal** als schuldig ueberfuehrt (`tail -1`;
drei Suiten ohne Protokoll; Rueckhalteblock hinter der Loeschung; und die zwei Loecher im Sammler
selbst). Nach fuenf Treffern ist „drei seltene Kernelfehler" nicht mehr die naheliegende
Hypothese.

---

## VA == PA, dritter Anlauf: die BINDUNG, nicht nur die Liste (2026-08-05)

Der zweite Anlauf schloss die *Liste* (ein geschlossenes Enum), aber nicht die *Bindung*: `reason`
war ein **freies Argument** von `Va::identity(reason, pa)`. Nichts hinderte einen Aufrufer an
`Va::identity(Mmio, dma_pa)` -- der Waechter haette einen gueltigen Grund gesehen und geschwiegen.
**Solange der Grund waehlbar ist, ist die bequemste Variante wieder die falsche.**

**Behoben durch einen Konstruktor JE STELLE:** `Va::for_syscall_map`, `for_kernel_setup`,
`for_mmio_window`, `for_dma_window`, `for_kernel_global_window`. Kein Grund-Argument mehr, das man
verwechseln kann -- der Konstruktor traegt seine Stelle im Namen. Die beiden Engstellen
(`vspace_map_masked`, `vspace_unmap`) nehmen den **Konstruktor als Funktionswert** statt eines
Grundes; die Zuordnung Stelle<->Grund ist damit typgeprueft statt disziplingeprueft.

Der Waechter prueft dazu die **Stelligkeit**: jeder Konstruktor wird ein- **oder zweimal**
benutzt. Zwei ist der Normalfall und richtig -- Abbilden und Gegenstueck muessen dieselbe Achse
nehmen, und genau das war der `unmap_dma_from_thread`-Befund. Die erste Fassung dieser Pruefung
verlangte „hoechstens einmal" und schlug prompt bei allen fuenf an; sie hatte das Gegenstueck
nicht mitgedacht.

**Invariante und Schuld sind jetzt getrennt.** `IdentityReason` faltete zwei Dinge in einen
Begriff: „die Identitaet IST hier die Zusicherung, sie faellt nie" (MMIO, globale Kernelfenster)
und „die Identitaet ist eine Entscheidung, behebbar ohne ABI-Bruch" (`SYS_MAP`, Kernel-Setup,
DMA-Fenster). Dieselbe Faltung, gegen die der ganze Umbau geht -- und die Folge waere gewesen,
dass die **Schuld unsichtbar** wird: wer die Liste in einem halben Jahr liest, saehe ueberall
einen Grund und schloesse, alles sei nach Absicht.

`IdentityClass::{Invariant, Debt}` trennt sie, und `IDENTITY_DEBTS = 3` ist eine **Ratsche**: der
Waechter vergleicht gegen die Zahl, und sie darf nur fallen. Eine neue Schuld schlaegt an; eine
behobene verlangt, dass die Zahl nachgezogen wird (sie faellt nicht von selbst).

**Die Protokolle waren nicht weggeworfen -- es gab sie nie.** Ich hatte geschrieben, die Suiten
legten bei Abweichung selbst ein Log ab und nur meine Sammelschleife habe es verworfen.
Nachgesehen: das stimmt fuer **keine** der drei. Die Lade-Suite loeschte ihr `mktemp` am Ende
bedingungslos, die x86-Suite ihres direkt nach dem Einlesen (lange vor den Pruefungen), die
ARM-Suite hob nur bei abweichender Signatur eines Wiederholungslaufs etwas auf.

Alle drei legen jetzt bei `fail != 0` das volle Protokoll unter `build/diag/` ab. **Der erste
Anlauf war dabei selbst ein stummer Pruefer**: der Block stand am Dateiende, das Log war zu dem
Zeitpunkt aber schon geloescht -- er konnte nie feuern. Gegenprobe mit erzwungenem Fehlschlag
gefahren: 176 bzw. 193 Zeilen abgelegt.

Und die Rueckhaltung hat sofort geliefert: der naechste `RUNS=8`-Ausfall hinterliess ein
Protokoll, dessen **Signatur mit der eines gruenen Laufs identisch** ist. Damit ist bekannt, was
es NICHT war (Signaturabweichung, Wiederholungsvergleich) -- und dass die durchgefallene Pruefung
in der **stdout der Suite** steht, die meine Sammelschleife weiterhin nur als `tail -1` festhielt.
Dieselbe Luecke, eine Ebene hoeher. Steht als D12.

---

## VA == PA: die Annahme steht jetzt im TYP, nicht in einer Liste daneben (2026-08-04)

**Die erste Fassung war disziplinarisch und hat sich binnen Stunden selbst widerlegt.** Ein
Skript hielt die Aufrufstellen gegen eine Liste im Skriptkopf. Zwei Loecher, beide von aussen
angestossen:

* `hal::mmu::vspace_map_dma` stand in seiner Funktionsliste **gar nicht**. Der DMA-Pfad einer
  Treiber-PD bildet identisch ab, und der Waechter sah ihn nie -- eine Textflaeche ueber einem
  Loch.
* der Grundtext zu `SYS_MAP` war **falsch**. Er sagte „das ist die ABI". `caprock_abi::sys::MAP`
  traegt aber **kein Adressargument**: der Aufrufer nennt eine Cap, und die Basis kommt aus
  `ObjectKind::Memory(r).base` -- aus der Cap-Aufloesung IM KERNEL. Die Identitaet liegt damit in
  einer Entscheidung des Kernels und ist behebbar, **ohne die ABI anzufassen** (der Rueckgabewert
  nennt dem Aufrufer die Adresse ohnehin, `reg::MSG0`). Der Punkt war also nie strukturell
  unbehebbar, wie der Grund glauben machte.

Der zweite ist der lehrreichere: **ein Waechter prueft die Existenz eines Grundes, nie seine
Wahrheit.** Ein falscher Grund ist damit unsterblich.

**Die strukturelle Fassung.** `kernel/src/addr.rs` bekommt die **dritte Achse**: `Va` -- was ein
Subjekt sieht. Sie hat **keinen** Konstruktor aus `u64` und **keinen** aus `Pa`. Der einzige Weg
ist `Va::identity(reason, pa)`, und `reason` ist eine Variante des geschlossenen Enums
`IdentityReason`. Dazu zwei Wege, die gar nicht aus einer PA kommen: `Va::window` (das private
Fenster) und `Va::link` (ELF-Link-Adressen). **Die Liste IST damit der Quelltext**; eine neue
identische Abbildung braucht eine neue Variante, und die schreibt man nicht versehentlich.

Dieselbe Ueberlegung wie bei `DmaRegion::identity`, das ext-36 **bewusst entfernt** hat: bliebe
der bequeme Einstieg stehen, griffe der naechste danach.

**Was der Waechter jetzt noch tut** -- nur das, was ein Typ nicht kann: (1) es gibt keinen
zweiten Konstruktor, (2) jede Enum-Variante traegt einen Grund, (3) die identisch abbildenden
HAL-Funktionen werden nur aus den benannten Engstellen gerufen -- **die Funktionsliste liest er
aus der HAL selbst**, damit das erste Loch nicht wiederkommt, (4) **Falsifikatoren**.

**Der Falsifikator zu `SyscallMapByCap`** ist der erste seiner Art: der Grund behauptet, die ABI
trage kein Adressargument. Das Skript versucht ihn zu widerlegen -- es prueft, dass im
`SYS_MAP`-Zweig die Basis aus der aufgeloesten Cap stammt und **nicht** aus einem Frame-Register.
Und es prueft, dass der Anker ueberhaupt da ist: ein Falsifikator, der ins Leere liest, ist
keiner. Nicht jeder Grund laesst das zu -- aber die, die es zulassen, sollten nicht Prosa
bleiben.

**Der neue Waechter hat sofort geliefert.** `unmap_dma_from_thread` rief `vspace_unmap_page`
direkt mit einer Physadresse -- ausserhalb jeder Engstelle, ohne dass irgendwo stand warum. Die
alte Fassung hatte die Stelle nicht gesehen. Sie traegt jetzt denselben Grund wie der Mapping-Weg
(`DeviceDmaWindow`), und das ist keine Formsache: raeumte der Abbau auf einer anderen Achse ab
als das Mappen, bliebe eine Abbildung stehen -- genau das, was `dma_audit` Code 8 meldet.

**MMIO und DMA sind zwei Gruende, nicht einer.** Die erste Liste hatte sie in einem Eintrag. Bei
MMIO **ist** die Identitaet die Zusicherung (ein Treiber rechnet mit BAR-Adressen aus der
PCI-Enumeration, und die sind physisch -- CPU-Sicht, `Pa`). Bei DMA ist sie nur die **CPU-seitige
Haelfte**; was das Geraet sieht, ist eine `Iova` aus dem Fenster des Uebersetzungskontexts. Beides
zusammenzufalten waere dieselbe Vermengung eine Achse weiter gewesen.

**Was vorher schon richtig war und bleibt** (aus der ersten Fassung uebernommen): der
Spawn-Pfad. `spawn_isolated_native` bildete Code und Stack identisch ab und nahm die Physadresse
des Code-Frames als **Einsprungadresse** -- beides geht jetzt ins Fenster, und die Entry-VA ist
der **Rueckgabewert** der Abbildung (`ISO_USER_VA + slot * TWO_MIB`), also konstruktiv aus dem
Platz abgeleitet und keine zweite Zahl, die zufaellig passen muss.
`Scheduler::spawn_user` (ein Wert fuer EL0-SP UND Reap-Region) ist **geloescht**, nicht
repariert; es gibt nur noch `spawn_user_at`.

---

## VA == PA, erste Fassung: die Annahme als Liste (2026-08-04, ueberholt am selben Tag)

Nach dem Fenster-Umbau war die Frage nicht mehr „geht das?", sondern **„wo steckt dieselbe
Annahme noch?"**. Der Kernel bildet an manchen Stellen identisch ab -- die VA, die ein Subjekt
sieht, IST die PA. Jede solche Stelle traegt eine stillschweigende Annahme, und die Annahmen sehen
einander alle gleich. Zwei Fehler dieses Projekts hatten dieselbe Form, und beide waren
unsichtbar, **solange die zwei Zahlen zufaellig gleich waren**.

**Die Bestandsaufnahme.** Die Flaeche ist klein: neun Aufrufstellen in acht identisch abbildenden
HAL-Funktionen. Sie zerfallen in drei Klassen, und die Klasse entscheidet, was zu tun war.

**(1) Entfernt -- die Identitaet war eine Altlast.** `spawn_isolated_native` bildete Code- und
Stack-Frame identisch ab **und nahm die Physadresse des Code-Frames als Einsprungadresse**. Beide
gehen jetzt ins private Fenster (Plaetze `SLOT_CODE`/`SLOT_DATA`), der Entry ist eine VA. Damit
standen `vspace_map_region` und `vspace_map_code_region` ohne Aufrufer da -- **geloescht**, statt
als toter Pfad liegenzubleiben, den der naechste wieder benutzt.

**(2) Unmoeglich gemacht -- der Fehler, der schon zugeschlagen hatte.** `Scheduler::spawn_user`
nahm EINEN Wert fuer zwei Dinge: den EL0-Stackzeiger und die Reap-Region, die beim Thread-Tod an
den Allokator zurueckgeht. Beim Heben des GiB-0-Deckels wurde daraus ein `#PF
cr2=0x80_0000_0000` im **Kernel**. Die Funktion ist **geloescht**, nicht repariert: es gibt nur
noch `spawn_user_at`, das beide Werte verlangt. Der letzte Aufrufer -- ein SAS-Thread, bei dem die
Zahlen wirklich gleich sind, weil er sich die Identitaetskarte des Kernels teilt -- schreibt sie
jetzt zweimal hin. **Die Gleichheit ist dort ein Zufall der Umgebung und keine Eigenschaft des
Aufrufs**, und genau das soll der Quelltext sagen.

Reparieren haette hier nicht gereicht. Eine Funktion, die zwei Bedeutungen in einen Parameter
faltet, ist auch mit Warnschild noch die bequemere von zweien.

**(3) Benannt statt still -- die Identitaet IST die Zusicherung.** Neun Stellen bleiben:
`SYS_MAP`/`SYS_UNMAP` (der Aufrufer nennt eine Memory-Cap, also eine PA, und bekommt sie unter
derselben Zahl -- das ist die ABI, und sie zu aendern hiesse, ein Subjekt muesste eine VA nennen,
die es nicht kennt), die Geraetefenster (ein Treiber rechnet mit Adressen aus der
PCI-Enumeration, und die sind physisch) und zwei globale Kernel-Abbildungen ohne Subjekt.

**Der eigentliche Ertrag ist der Waechter.** `tools/identitaet.sh` haelt die Liste gegen den
Quelltext: jede identisch abbildende Stelle braucht einen Eintrag **mit Grund** -- und der Grund
beantwortet „warum gilt die Identitaet hier, und was waere die Folge, wenn sie faellt?". Kommt
eine Stelle dazu, schlaegt er an. Selbsttest in **beide** Richtungen: eine untergeschobene Stelle
wird erkannt, ohne sie schweigt er wieder.

Dabei prompt hereingefallen: der erste Anlauf suchte im Kopiebaum mit **absoluten** Pfaden, die
auf keinen Listeneintrag passten -- also meldete der Selbsttest seine eigene Mechanik als Befund.
Er hat damit funktioniert (er schlug an, wo nichts war), aber die Aussage war eine andere als
gemeint. Ein Waechter, dessen Schluessel nicht die des Registers sind, prueft eine zweite
Wirklichkeit -- dieselbe Falle wie `iova_window_clear_of_msi`, das die Fensterlage nachrechnete
statt sie zu lesen.

**Was der Waechter NICHT kann, und das steht in seinem Kopf:** er sieht Aufrufe, keine Absichten.
Ob eine erlaubte Stelle ihre Identitaet weiterhin zu Recht annimmt, prueft er nicht. Dafuer stehen
die Gruende in der Liste, und `isohigh` misst in beiden Suiten den Fall, der frueher strukturell
unmoeglich war.

---

## E-Rest 3d (Haelfte 2) — der GiB-0-Deckel fuer isolierte PDs ist weg (2026-08-04)

**Der Deckel war eine Zahl, nicht ein Gefuehl: 504.** GiB 0 abzueglich der ersten 16 MiB, je
2 MiB private Region — so viele isolierte PDs passten gleichzeitig, und das band frueher als
`MAX_VSPACES` (4096). Der Host-Test `gib0_deckel_ist_eine_zahl` rechnet es auf dem Speicherplan
von `-m 6G` nach und ist zugleich die Positivkontrolle des Umbaus.

**Die Ursache war die IDENTITAET, nicht „eine PD sieht es".** `vspace_map_block` leitet den
Tabellenindex aus der **Phys**adresse ab (VA == PA), und eine isolierte VSpace hat ihr eigenes
Seitenverzeichnis nur fuer GiB 0. Also musste die Region dorthin.

**Die Behebung: ein VA-Fenster ausserhalb der Identitaetskarte.** Es MUSS dort liegen, und das ist
der Punkt, an dem die naheliegende Loesung scheitert: der Kernel laeuft beim Syscall im Adressraum
*dieser* PD und greift dort ueber die Identitaetskarte auf beliebiges physisches RAM zu. Eine
User-VA irgendwo in GiB 0, die auf eine andere PA zeigt, **verdeckt** genau diese Sicht — der
Kernel laese an der Stelle den Speicher der PD statt den eigenen. Gewaehlt sind deshalb Bereiche,
die der Kernel nie identisch belegt: auf x86 `PML4[1]` (512 GiB; die Identitaetskarte benutzt
ausschliesslich `PML4[0]`), auf aarch64 `L1[9]` (die Eintraege 0..=8 sind belegt, 9..511 leer).
`vspace_map_user_window` legt die Tabellen an, `vspace_collect_user_window` gibt sie beim Abbau
zurueck.

**Belegt:** `isohigh : ALL PASS` bei 3G/4G/6G — die Regionen zweier isolierter PDs liegen bei
`0x1_02b0_0000` (4,04 GiB), und die **Farbtrennung haelt unveraendert**: die Farbbedingung ist
eine Aussage ueber die Physadresse und von der virtuellen Lage vollstaendig unberuehrt. Bei
512M/2560M meldet die Zeile `SKIP` — dort gibt es keinen Speicher oberhalb 4 GiB, die Frage ist
**nicht entscheidbar**, und das ist kein bestandener Test. Gegenprobe gefahren: die alte
Zuteilung wieder eingesetzt -> `isohigh : FAILURES` und die Suite rot.

**Zwei Annahmen der eigenen Notiz waren falsch — beide zugunsten der Sache.**

* „Der Preis ist der Verlust des 2-MiB-Block-Fastpaths." **Nein.** Der Fastpath hing nie an der
  Identitaet, sondern nur an der **Ausrichtung der VA**. Ein 2-MiB-ausgerichteter Block bleibt
  ein Blockdeskriptor; nur der Index kommt jetzt aus der VA statt aus der PA.
* „Gehoert mit B-4.1 zusammen entschieden." **Nein.** A1 ist gar nicht betroffen. Der Preis sind
  zwei bis drei 4-KiB-Rahmen je isolierter PD, und die duerfen selbst oben liegen.

**Der Fehler, der es teuer gemacht haette.** `Scheduler::spawn_user` nimmt EINEN Wert fuer zwei
Dinge: den EL0-Stackzeiger und die **Reap-Region**, die beim Thread-Tod an den Allokator
zurueckgeht. Solange VA == PA galt, war das dieselbe Zahl. Nach dem Umbau nicht mehr — gemessen
als `#PF cr2=0x0000008000000000` im **Kernel**: der Reap-Pfad gab eine virtuelle Adresse als
Physadresse frei. Behoben ueber das laengst vorhandene `spawn_user_at`, das der Ladepfad seit
A-2 benutzt (dort war VA != PA schon immer der Normalfall). **Ein Parameter, der zwei Bedeutungen
traegt, ist so lange harmlos, wie die beiden zufaellig gleich sind** — dieselbe Form wie das
`blocked`-Bit im Scheduler (D9) und wie „unten zuerst" als Zufall der Groessenrelation (E-Rest 3b).

**Gemessen:** RAM-Reihe 512M · 2560M · 3G · 4G · 6G (Hauptsuite) und 512M · 3G · 6G (Lade-Suite),
alle `== ALL PASS ==`; x86 `RUNS=8` und aarch64 `RUNS=4` mit identischer Signatur; Host-Tests,
Kerngrenze.

**Offen bleibt E-Rest 3e:** DMA-Regionen haengen weiterhin an GiB 0 — aus zwei Gruenden, die
auseinandergehoeren: sie werden identisch in die Treiber-PD abgebildet (dasselbe Fenster wuerde
es loesen), und sie sind **geraetesichtbar** — ob ein Geraet oberhalb 4 GiB adressiert, ist eine
Eigenschaft des Geraets, und die Angebotsliste fuehrt sie nicht.

---

## E-Rest 3d (Haelfte 1) — die Stellen sind aufgezaehlt, und der Speicher oben traegt (2026-08-04)

3b hatte den Zonenwunsch eingefuehrt, aber mit einer Vorsichtsmassnahme bezahlt: `mem_alloc` gab
„unten zuerst" vor, **weil** unbekannt war, wer alles darauf baut. Damit war der gesamte Speicher
oberhalb 4 GiB praktisch Reserve -- der Ausweichzaehler meldete ehrlich `0x`, also einen Pfad, der
nie lief.

**Die Aufzaehlung steht jetzt an EINER Stelle** (`enum Zone` in `kernel/src/system.rs`), nicht
verstreut in Kommentaren:

* `KernelOnly` (bevorzugt **oben**): Thread-/Cap-/IPC-Tabellen, Kernel-Thread-Stacks, die
  Segment- und Stack-Frames **geladener Programme**, alle L3-Seitentabellen, AP- und
  Sekundaerstacks. Gemessen bei 3G und 6G: **alle 28** dieser Allokationen liegen oberhalb
  4 GiB, Haupt- und Lade-Suite gruen.
* `PdMappable` (muss **tief**): alles, was identisch abgebildet wird (VA == PA). Das ist
  strukturell, nicht empirisch: `vspace_map_page_at` weist `va >= GIB1_END` ab, `pd_block_index`
  ebenso.
* harte Bedingung (`gib0_zone`, `None` statt einer unbrauchbaren Adresse): die private Region
  einer isolierten PD, `spawn_isolated_native` und `alloc_dma_region`. Der mittlere Fall kam
  hier dazu -- er stand noch auf „einmal fragen, danach pruefen, bei Verfehlung aufgeben".

**Die Gegenprobe ist der eigentliche Beleg.** Stellt man `system::alloc` auf `KernelOnly`, faellt
die **Lade-Suite** bei `-m 3G` aus (`drv`/`blkdev`/`dmaiso` -- die Treiber-PD wird nie bereit),
waehrend die **Hauptsuite gruen bleibt**. Eine Klassifikation, die nur gegen die Hauptsuite
geprueft worden waere, haette den Fehler durchgelassen. Das ist dieselbe Form wie D8/D9/D11: die
Suite loest den Fall nicht aus, den sie zu decken scheint.

**Zwei eigene Vermutungen widerlegt, beide gemessen statt geglaubt:**

* „Geladene Programmsegmente brauchen GiB 0." **Falsch.** `load_into_pd` bildet ueber
  `vspace_map_page_at` ab, und das nimmt VA und PA **getrennt** -- die Physadresse ist frei.
  Diese Vermutung hatte ich am selben Tag als Befund notiert; sie stand auf einer Messung, die
  durch einen anderen Fehler (den Selbst-Deadlock aus 3b) verfaelscht war. Eine Messung an einem
  kaputten Aufbau ist keine Messung.
* „Das Geraet erreicht nur GiB 0." **Falsch.** Mit der virtio-Region oberhalb 4 GiB liest
  `virtio-blk` den Sektor korrekt (`Geraet-DMA=1`, Magie stimmt). Die Region bleibt trotzdem
  konservativ tief -- aber aus einem anderen Grund: die 32-Bit-Faehigkeit ist eine Eigenschaft
  des Geraets, und die Angebotsliste enthaelt sie nicht (`Geraete-ohne-deklarierte-Adressbreite=1`).

**Der Zaehler ist zweiteilig und sagt jetzt etwas.** „PD-abbildbar musste nach oben ausweichen"
waere ein Befund (GiB 0 ist voll, und die Region liegt dort, wo eine PD sie nicht sieht); „reiner
Kernel-Speicher musste nach unten" ist harmlos. Gemessen: 512M und 2560M -> 0 / **28** (es gibt
oben nichts, der Pfad ist also GEFAHREN), 3G/4G/6G -> 0 / 0.

**Gemessen:** RAM-Reihe 512M · 2560M · 3G · 4G · 6G (Hauptsuite) und 512M (3x) · 3G · 6G
(Lade-Suite), alle `== ALL PASS ==`; x86 `RUNS=8` und aarch64 `RUNS=4` mit identischer Signatur;
sechs neue Host-Tests fuer das Zonen-**Intervall** (`alloc_in`), darunter eine Positivkontrolle,
die beim ersten Anlauf zu Recht durchfiel: sie stand auf dem 3G-Speicherplan, wo Best-Fit von
sich aus oben waehlt -- eine Positivkontrolle muss zu dem Aufbau passen, in dem sie steht.

Nebenbefund beim Bauen: die Suche kannte die Untergrenze, der **Zuschnitt** danach nicht (er
rechnete `start` aus `r.base` neu). Gefunden wurde in der Zone, herausgeschnitten darunter --
drei Host-Tests haben es sofort gezeigt.

**Offen bleibt** (s. `todo.md`): der Rest der Aufzaehlung (EL0-Kernel-Stacks, EL0-User-Stack,
IOMMU-Tabellen, Sentinel-Page -- alle konservativ, keine geprueft) und der 1-GiB-Deckel selbst,
der im VSpace-Layout liegt und nur mit einer nicht-identischen Abbildung faellt.

---

## E-Rest 3b — die Freiliste kennt den Zonenwunsch (2026-08-04)

**Der Befund war groesser als der Eintrag.** Notiert war: `alloc_dma_region` und isolierte
PD-Regionen muessen in GiB 0 liegen, fragen aber nach „irgendeiner" Region und geben bei
Verfehlung auf, ohne ein zweites Mal zu fragen. Richtig — aber die eigentliche Abhaengigkeit lag
eine Ebene tiefer: **„unten zuerst" war ueberhaupt ein Zufall der Groessenrelation.** Best-Fit
nimmt das kleinste passende Fragment; solange der Bereich oberhalb 4 GiB zufaellig groesser war
(`-m 4G`: 2048 gegen 2032 MiB; `-m 6G`: 4096 gegen 2032), landete alles Unbenannte unten. Bei
`-m 3G` (1024 gegen 2032) kehrt sich das um.

Gemessen, nachdem der alte Behelf entfernt war: nicht nur `dmawin`/`dmatok`/`iso` fielen aus,
sondern der ganze Ladepfad — `drv`, `blkdev`, `fs`, `part` reihenweise rot, die Treiber-PD kam gar
nicht hoch.

**Die Behebung, zweiteilig.** (1) `caprock_mem::alloc_below`/`alloc_colored_below` nehmen eine
Obergrenze; gesucht wird nur unter Fragmenten, die sie erfuellen koennen, und ein Fragment ueber
die Grenze hinweg wird an ihr **beschnitten** statt verworfen. Best-Fit vergleicht dabei den
**nutzbaren** Teil, nicht die Fragmentlaenge — sonst gewaenne ein riesiges Fragment, von dem nur
ein Zipfel unter der Grenze liegt, gegen ein kleines, das ganz hineinpasst. Farbe und Zone werden
in EINER Entscheidung getroffen (dasselbe Argument wie Z8 fuer NUMA: zwei nacheinander laufende
Politiken kaempfen gegeneinander). (2) „Unten zuerst" ist jetzt eine **ausgesprochene Politik** in
`mem_alloc`/`alloc_colored`, mit hohem Speicher als Ueberlauf. `claim_user_kstack` griff als
einzige Stelle am Wrapper vorbei und geht jetzt darueber — eine Vorgabe, an der eine einzige
Stelle vorbeigreift, ist keine Vorgabe.

Der Behelf im Speicherplan ist damit weg: hoher Speicher geht **vollstaendig** in die Freiliste
(bei `-m 3G` vorher 0 von 1024 MiB vergeben, jetzt 1024 von 1024).

**Zwei eigene Fehler unterwegs, beide gemessen und beide lehrreich.**

* `match MEM.lock() { .. None => MEM.lock() }` haelt den Guard bis zum Ende des `match`. Der
  Ausweichpfad war damit ein **Selbst-Deadlock** auf einem Spinlock — und zwar genau der Pfad,
  der selten laeuft. Die Lade-Suite blieb stehen, sobald der untere Bereich einmal nicht reichte.
  Ein Fehler im seltenen Zweig sieht aus wie ein Haenger und nicht wie ein Fehler.
* Der Zaehler fuer den Ausweich zaehlte zuerst **Versuche** statt **Wirkung** und meldete `1x` auf
  einer 512-MiB-Maschine, auf der es oberhalb 4 GiB gar keinen Speicher gibt. Gezaehlt hatte er
  eine absichtlich uebergrosse Anforderung aus dem Farbtest, die NIRGENDS passte. „Unten war kein
  Platz" und „es wurde oben genommen" sind zwei verschiedene Aussagen — dieselbe Verwechslung wie
  `rx_used` gegen „Daten sind angekommen".

**Gemessen.** RAM-Reihe 512M · 2560M · 3G · 4G · 6G auf der Hauptsuite, 512M · 3G · 6G auf der
Lade-Suite, alle `== ALL PASS ==`. Host-Tests mit **Positivkontrolle**:
`ohne_zone_waehlt_best_fit_den_oberen_bereich` belegt, dass Best-Fit ohne Zonenwunsch wirklich
oben landet — ohne diese Zeile sagte der Test darunter nichts, er koennte auch gruen sein, wenn
die Zone gar nichts bewirkt.

**Was NICHT behoben ist**, steht als E-Rest 3d in `todo.md`: welche Stellen GiB 0 wirklich
brauchen, ist nicht aufgezaehlt — „unten zuerst" ist eine Vorsichtsmassnahme, keine Zusicherung.
Und der 1-GiB-Deckel fuer Regionen mit PD-eigener Abbildung bleibt: `vspace_map_block` bildet
**identisch** ab (VA == PA), das ist eine Eigenschaft des VSpace-Layouts und mit Allokatorarbeit
nicht zu heben. Der Ausweichzaehler in der `mem`-Zeile meldet ehrlich `0x` — der Ueberlaufpfad ist
in QEMU **ungefahren**, seine Wirkung nur host-getestet.

---

## A1. Cache-Partitionierung zwischen PDs — **Stufe 1 erledigt** (x86)

Rest in [todo.md](todo.md#a1-cache--timing-seitenkanäle-zwischen-pds); hier steht, was trägt und
welche Annahmen dabei umgefallen sind.

**Gemessen, nicht angenommen.** `hal::cache` liest die LLC-Geometrie aus der Hardware — x86 über
`CPUID`-Blatt 4 (Unterblätter durchzählen, höchste Ebene mit Daten-/Unified-Cache), aarch64 über
`CLIDR_EL1`/`CCSIDR_EL1` inklusive der FEAT_CCIDX-Feldverschiebung. Farbe einer Seite =
`sets * line / PAGE`, immer auf die nächstkleinere Zweierpotenz abgerundet: nur echte Indexbits
sind Farbbits. Auf dem Testaufbau (QEMU q35, `-cpu Skylake-Client`): LLC L3 16 MiB, 16-fach,
64 B/Zeile, 16384 Sets → **256 Seitenfarben**.

**`1` ist kein Erfolgswert.** Meldet die Plattform keine Geometrie, gibt es eine Farbe — und
„die Farbsätze zweier PDs sind disjunkt" wäre dann strukturell wahr, ohne geprüft zu sein.
`colors::usable()` trennt das, der Test meldet **SKIP statt PASS**, und die Suite hat dafür einen
eigenen Zweig. Dieselbe Falle wie bei der SMMU-Event-Queue ohne `CD.R`, nur eine Ebene höher.
`qemu64` ist genau dieser Fall: das synthetische Modell meldet weder Blatt 4 noch x2APIC. Der
TCG-Rückfall der Suite steht deshalb jetzt auf `Skylake-Client` — die Lücken waren die des
**Modells**, nicht die des Kernels.

**Die Annahme, die umfiel: Färbung und der 2-MiB-Blockdeskriptor schließen einander aus.**
Aufeinanderfolgende Seiten tragen aufeinanderfolgende Farben, also überstreicht eine
zusammenhängende Region über `n` Seiten `n` aufeinanderfolgende Farben. Die 2-MiB-Region von
`spawn_isolated` sind 512 Seiten — bei 256 Farben also jede Farbe zweimal. Kein Zuteilungstrick
ändert das; es ist Arithmetik. Färbung gibt es deshalb nur mit einer Region, die **höchstens so
breit ist wie der Farbstreifen** (`MASK_BITS / PARTITIONS` Seiten, bei 4 Partitionen 64 KiB), und
mit seitenweisem Mapping statt eines Block-PTE. Der Preis steht im Code, nicht in einer Fußnote:
16 PTEs plus eine L3-Tabelle statt eines Deskriptors. `alloc_colored` weist eine zu große
Anforderung **ab**, statt still fremde Farben mitzunehmen.

**Auch die Kernel-Seite gehört dazu.** Kernel-Stack (16 KiB) und die obersten Seitentabellen einer
PD werden vom Kernel benutzt, aber *im Namen dieses Subjekts*; ihre Cache-Zeilen tragen dessen
Zugriffsmuster, und ein MMU-Walk hinterlässt dieselben Spuren. Beide kommen jetzt aus demselben
Streifen, und der Test weist das getrennt nach (`kernel_side_in_mask`) — sonst wäre es eine
Behauptung im Kommentar.

**Die zweite Annahme, die umfiel: `total_free()` ist als Leck-Orakel unbrauchbar, sobald andere
Kerne laufen.** Der erste Bilanz-Test verglich die globale Summe vor und nach dem Abbau und war
nichtdeterministisch — gleicher Kernel, mal grün, mal rot. Ursache: nebenher sammelt ein anderer
Kern den Stack des planmäßig gefaulteten `iso_probe`-Threads ein; fiel dieser Rückgang ins
Messfenster, sah eine bilanzneutrale Operation aus wie ein Gewinn. Ersetzt durch
`PhysAllocator::fully_free(base, len)`: die Frage lautet jetzt „ist **genau diese** Region wieder
frei", unabhängig von fremder Nebenläufigkeit. Ein Test, der aus fremdem Grund fehlschlägt, ist so
wenig wert wie einer, der nicht fehlschlagen kann.

**Wo die Arithmetik geprüft wird.** Farb- und Allokatorlogik liegen in `caprock-mem` und sind
reine Rechnung ohne Hardware — 13 Host-Unit-Tests (`rustc --test crates/caprock-mem/src/lib.rs`,
Laufzeit 0,00 s; der Umweg über `cargo` würde wegen `build-std` die halbe Standardbibliothek
übersetzen). Darunter ein **Sensitivitätstest**: ohne Maske muss die Eigenschaft umfallen. Ohne
ihn könnte der Disjunktheitstest grün sein, weil die Anordnung es zufällig hergibt, statt weil die
Maske wirkt. Der Kernel-Test rechnet die Farbe mit **derselben** `color_of` wie der Allokator —
eine zweite Fassung im Testmodul hätte am Ende nur die eigene Arithmetik bestätigt.

## B. Thread-Migration — **erledigt** (ext-30)

- [x] **B1** Kern-Zuordnung aus der ThreadId gelöst — globales **Thread-Directory**
      `gid -> (used, gen, core, local)`, lock-frei lesbar.

- [x] **B2** `migrate_to(tid, dst)` unter beiden Scheduler-Locks (aufsteigende Kern-Ordnung),
      **Push-Modell** (nur der abgebende Kern kann den Lazy-FP-Kontext sichern).

- [x] **B3** Kernel-Glue auf „lock-frei lesen → sperren → **erneut prüfen** → ggf. wiederholen"
      umgestellt (`system::with_owner`); `ThreadId::core()` existiert nicht mehr.

- [x] **B5** Test `migrate`: Thread wechselt unter Last den Kern, läuft dort nachweislich weiter,
      dieselbe Tcb-Cap bezeichnet ihn weiterhin, cross-core-KILL, Scheduler-Audit 0.

## C. Flexible Kapazitäten — **weitgehend erledigt** (ext-30)

- [x] **C1** Neue Crate `caprock-slab` (`Slab`/`AtomicTable`/`FreeList`); Thread-Directory,
      per-Kern-TCB-Tabellen, FP-Kontexte, `VSPACE_OF`, Kernel-Stack-Zuordnung **und** die
      Sekundär-Stacks kommen beim Boot aus dem RAM (`system::configure`).

- [x] **C2** Kernzahl aus dem Device Tree (`Dtb::cpu_count`); `MAX_CORES = 256` dimensioniert nur
      noch Arrays von *Locks*/Atomics.

### C4. Effizienz bei tausenden Threads (lineare Scans beseitigen)

- [x] `Scheduler::alloc_tcb` — **Freiliste** (O(1)).

- [x] Ready-Queues — **intrusive doppelt verkettete Listen** durch die TCBs: Einreihen/Ausklinken
      O(1) und **kein** Queue-Speicher mehr (vorher `[usize; PER_CORE]` je Priorität).

- [x] `system::least_loaded_core` — **lock-freier** Lastzähler je Kern.

- [x] MCS-Refill-Scan je Tick — entfällt vollständig, solange kein Budget erschöpft ist.

- [x] `thread_alive` / IPC-Liveness-Audit — lock-frei über das Directory statt Zielkern sperren.

### C6. x86-64-Port — **Stufe 4 erreicht** (ext-31, Branch `arch/x86_64`)

- [x] Boot (Multiboot→Long Mode), Serial, 4-Level-Paging + W^X + `CR0.WP`

- [x] IDT/Exceptions (256 Stubs, einheitliches Frame-Layout), LAPIC, Timer (PIT-kalibriert)

- [x] GDT/TSS, Ring 3, `syscall`/`sysret`, Context-Switch

- [x] HAL architekturselektiv; alle 13 Crates bauen für beide Architekturen

- [x] Kernel-Kern auf x86: Allokator, Capability-System, Scheduler (präemptiv), IPC, Audits

- [x] **SMP**: INIT-SIPI-SIPI + 16-bit-Trampolin (16→32→64 Bit), 4 Kerne in QEMU verifiziert —
      jeder Kern mit eigenem LAPIC-Timer und eigener Scheduler-Instanz

- [x] **Ring 3**: User-Threads laufen (Syscall per `int 0x80`, Fault auf Kernel-Speicher beendet
      den Thread) — im **SAS-Modell**, wie trusted PDs auf aarch64. Nötig dafür: `US` auf allen
      vier Paging-Ebenen, `TSS.RSP0` je Thread, eigene `.user_text`/`.user_data`-Sektionen

- [x] **Per-Prozess-Adressräume**: `vspace_*` vollständig implementiert (PML4→PDPT→PD, geteilte
      Kernel-PTs, supervisor-only Grundfläche + „hineingestanzte" User-Blöcke). Isolierte PDs
      laufen; Test `iso` zeigt: dieselbe Adresse ist für die SAS-PD lesbar, für die isolierte nicht

- [x] **PCI-ECAM + Enumeration**: Fenster aus der ACPI-**MCFG**, Geräte werden aufgezählt,
      virtio-rng gefunden, Bus-Master aktiviert. (BARs vergibt auf dem PC die Firmware — anders
      als auf `virt`, wo der Kernel das selbst tut.)

- [x] **RAM-Plan** aus dem Multiboot-Speicherplan (BSP-Trampolin reicht `EBX` durch); CPU-Liste
      aus der ACPI-**MADT** statt fester Kernzahl

## E. DMA-Härtung (aus dem Design-Review, ext-35)

- [x] **§2 sagt, was der Code leistet.** Die alte Formulierung („danach kann kein Gerät mehr in die
      Region DMAen") galt für künftige Übersetzungen und war für bereits übersetzte, in-flight
      Posted Writes falsch. Jetzt mit expliziter Arbeitsteilung Quiesce ↔ STE-Entfernung.

- [x] **BME-Clear + Flush-Read vor dem Unmap** (`pcie::quiesce_by_rid`). Schließt die Lücke, die
      die ehrliche Formulierung sichtbar macht, ohne gerätespezifisches Wissen (kein FLR).
      Nebeneffekt: die Reihenfolge stimmt jetzt (vorher lief das Gerät während des Unmaps weiter →
      Translation Faults statt Korruption, aber ein Fault-Sturm verdeckt echte Fehler).

- [x] **Granularitätsprüfung an der Cap-Prägung** (`CapError::Unaligned`, Test `dmaalign`).

- [x] **ATS-Entscheidung** mit den drei Bedingungen, unter denen sie revidiert werden dürfte.

- [x] **IOVA ≠ PA** — in zwei Schritten umgesetzt (ext-36). (a) Achsen als eigene Typen
      (`addr::Pa`/`addr::Iova`), `dma_prepare`/`dma_complete` auf die PA-Achse,
      `dma_addr_in_region` auf die IOVA-Achse — verhaltensneutral, weil die Werte noch gleich
      waren. (b) Fensterbasis oberhalb `RAM_TOP` weggedreht: Bump-Allokator je Kontext (1 GiB
      Fenster), 2-MiB-Schutzbänder (= größte Stage-1-Blockgranularität, ein 4-KiB-Band wäre von
      einem Block-Mapping überspannbar), IOVA 0 unabgebildet, keine IOVA-Wiederverwendung, keine
      arithmetische Beziehung zur PA, `DmaRegion::identity` **entfernt**, `DmaHandle.pa`
      kernelprivat. `stage1_map_region` indiziert mit der IOVA und trägt die PA ins Blatt.
      Der Negativtest in `virtiorng` hat die drei geforderten Teile (Queue vorher leeren,
      Eintrag prüfen statt zählen, Positivkontrolle im selben Lauf) und **kippt** in Schritt b.
      Nebenbefunde, die erst durch echte Übersetzung sichtbar wurden: der Treiber verlangt jetzt
      verbindlich `VIRTIO_F_ACCESS_PLATFORM` (sonst umgeht das emulierte Gerät die SMMU),
      `STE.S1STALLD` wird nur noch bei `IDR0.STALL_MODEL == 0b10` gesetzt (sonst `C_BAD_STE`),
      und im CD fehlten `A` (Terminate) und `R` (Fault aufzeichnen) — ohne `R` bliebe die
      Event-Queue per Konfiguration leer und der Negativtest prüfte nichts.
      Offen geblieben: nur der aarch64-Enforcer vergibt Fenster; `VtdEnforcer::attach` meldet
      weiterhin `None` (x86 hat keine per-Gerät-Zuteilung, s. C).

- [x] **Nacharbeit zu ext-36b — Belastbarkeit des Belegs.**
      * **Queue-Liveness**: „Event-Queue leer" zählt nur, wenn im selben Lauf ein echter
        `F_TRANSLATION` beobachtet wurde. Ein Selbsttest beim Hochlauf ist nicht konstruierbar
        (ein Übersetzungsfehler braucht eine echte Bus-Master-Anforderung; `ATOS` liefert ins
        `PAR`, nicht in die Event-Queue) — der Nachweis im selben Lauf ist die erreichbare Form.
      * **`dma_audit` Code 6**: `C_BAD_STE`/`C_BAD_CD`/… werden beim Leeren der Queue gezählt und
        überleben es. Ein solcher Eintrag heißt „der Stream übersetzt gar nicht" — genau der
        Zustand, in dem jedes Abwesenheits-Oracle bedeutungslos ist.
      * **Sensitivitätskontrolle**: Bypass-STE statt der eingeklappten „schreibt ODER faultet"-
        Fassung. Die Mutation „Fensterbasis 0" leistet es nicht — die Sentinel-Seite faultet auch
        bei IOVA = PA, sie unterscheidet die beiden Welten nicht.
      * **Fenstergrenzen** (`dmawin`): Fenster am Slot statt an der Erzeugung (die schärfere,
        nirgends notierte Grenze waren ~500 Kontext-*Erzeugungen*, nicht die ~256 Attaches je
        Kontext), Bump je Slot überlebt den Kontextabbau, drei getrennte Fehlerursachen
        (Fensterende / Eingangsbreite / **Geräte-Adressbreite**), alle geprüft.
      **Nicht-Wiederverwendung ist als dauerhafte Politik entschieden**, nicht als offener Punkt:
      eine IOVA, die nie zurückkommt, kann keine veraltete Übersetzung tragen. Die verbleibende
      Lebenszeit-Obergrenze je Slot ist ein geprüfter sauberer Fehlschlag, kein Betriebszustand.
      Falls sie je erreicht wird: Slot-Recycling beim Kontextabbau, nicht IOVA-Recycling im
      Kontext. Damit muss der Teardown-Token nur den **PA-Free** absichern — der Unmap bleibt
      zwingend (stehende Übersetzung auf freigegebene PA = der UAF), aber es gibt nichts
      zurückzugeben. Ein Zustand weniger.
      Ebenfalls erledigt: undeklarierte Geräte-Adressbreiten werden geführt und protokolliert
      statt stillschweigend als 64 Bit angenommen.

- [x] **Teardown-Token** (ext-37): `CapSpace::delete_leaf` gibt `ObjectKind::Dma` **nicht** mehr
      frei, sondern meldet die Region über `Finalized` (vormals `ReplyFinal`) zurück; der Kernel
      legt still, unmappt, synchronisiert und gibt erst gegen einen `DmaTeardownToken` frei. Der
      Token trägt eine `DmaRegion` (beide Achsen), nicht eine `PhysRegion`.
      Die vier Entscheidungen: **synchron** (ein Zwischenzustand, den es meistens nicht gibt, ist
      schwerer richtig zu halten als einer, den es nie gibt); **gebündelt** für `revoke`
      (`DmaEnforcer::finalize` nimmt den ganzen Stapel: alle entwaffnen, alle spülen, alle
      unmappen, ein `TLBI`+`SYNC`); **`KILL` darf Pending erzeugen** (`KillScope` in
      `destroy_pd`), überall sonst zählt es zusätzlich als Anomalie; **Audit-Code 7** prüft, dass
      eine Pending-Region weder in der Freiliste noch in einer Übersetzungstabelle steht.
      Die IOVA-Rückgabe entfällt, weil Nicht-Wiederverwendung Politik ist — ein Zustand weniger.
      Test `dmatok`: `cap_delete` ohne `dma_detach` baut ab und gibt frei (Basislinie exakt
      wiederhergestellt); eine StreamID ohne Gerät (Konfigurations-Read `0xFFFF`) landet
      deterministisch im Pending-Zustand, die Region fehlt genau um ihre Größe, der Unmap ist
      trotzdem erfolgt, Code 7 hält.

## D. Verifikation

- [x] **D5** Tier-1-Roadmap in `docs/verification.md` war stale — sie führte die
      Concurrency-Modellprüfung der Locks noch als offen, obwohl Loom Stufe 2 seit `c2116ac`
      steht. Abgehakt in `529bc35` (B-2.4), **mit der Grenze daneben**: Loom modelliert eine
      *Kopie* des Algorithmus, ein Fehler in der `cfg`-Auswahl bleibt für Loom wie für Kani
      unsichtbar. Genau dort lag B-1.1, der x86-IRQ-Deadlock.
      Der Punkt stand danach noch in `todo.md` — der Eintrag beschrieb also einen Zustand, den
      es seit `529bc35` nicht mehr gab. Eine Liste, die Erledigtes als offen führt, ist
      derselbe Fehler wie ein grüner Testlauf ohne Testergebnis: sie sagt etwas aus, wofür sie
      keinen Beleg hat.

---

## A-5.2. virtio auf x86 — Transport, Blockgerät, Netzkarte

**Erledigt 2026-08-01.** Der Rest von Strang A-5 (die Treiber-**PD**) steht als A-5.1 weiter offen;
was hier fertig wurde, ist die Treiberlogik samt Beleg, dass sie trägt.

### Die Annahme, die umfiel: „der RNG belegt den Transport"

Sie war zur Hälfte richtig. `virtio-rng` hat **eine** Deskriptorzelle mit Write-Flag, das Gerät
füllt sie — damit steht fest, dass Bus-Master-DMA **in** unseren Speicher ankommt. Was nicht
feststand: ob das Gerät unseren Speicher auch **liest**. Der RNG liest nie etwas von uns; die
Richtung kam in seinem Testfall strukturell nicht vor.

Das ist dieselbe Fehlerform wie die leere SMMU-Event-Queue ohne `CD.R` — eine Aussage, die wahr
aussieht, weil der Fall, der sie widerlegen könnte, gar nicht ausgeführt wird. Und sie trug bis in
den **Negativtest**: dass die VT-d-Root-Tabelle im Default-Block auch Lesezugriffe sperrt, war eine
Annahme über Hardwaresemantik. Genau solche Annahmen haben in diesem Projekt schon zweimal
danebengelegen (`STE.S1STALLD`, `GCMD` als Read-Modify-Write).

`blk` schließt beides in einer Transaktion:

| Glied der Kette | Richtung | Inhalt |
|---|---|---|
| 0 | Gerät **liest** | Anfragekopf: Typ, Sektornummer |
| 1 | Gerät schreibt | 512 Byte Sektordaten |
| 2 | Gerät schreibt | Statusbyte |

Kommt Glied 0 nicht an, weiß das Gerät nicht einmal, welchen Sektor es liefern soll.

### Gemessen (x86-Suite, 20 von 20 Läufen mit identischer Ergebnissignatur)

    virtio  : Transport (vor VT-d): Caps=1 Geraet-DMA=1 (64 Byte)
    vblk    : Lesen (vor VT-d): Kapazitaet=2048 Sektor(en) Status=0x00 geschrieben=513 Byte
              Magie=0x454b414c344c4553 (erwartet 0x454b414c344c4553)
    vnet    : MAC 52:54:00:12:34:56; Sendepuffer abgeholt=1 Rahmen empfangen=1 (76 Byte)
              ARP-Antwort von 10.0.2.2=1
    virtio  : Sperre (nach VT-d): Geraet-DMA=0 (0 Byte)  VT-d-Faults 0 -> 1
    vblk    : Sperre (nach VT-d): abgeschlossen=0 Status=0xff (vom Geraet unberuehrt)

Das Statusbyte wird **vor** der Anfrage auf `0xff` gesetzt, nicht auf 0. Auf 0 vorbelegt wäre die
Statusprüfung wertlos gewesen: `VIRTIO_BLK_S_OK` ist 0, der Test wäre also auch dann grün, wenn das
Gerät nie geantwortet hat. Nach dem VT-d-Aufbau steht dort weiterhin `0xff` — damit ist belegt, dass
das Gerät den Anfragekopf **nicht einmal gelesen** hat, und nicht bloß, dass keine Antwort kam.

### Warum der Inhalt geprüft wird und nicht die Länge

Ein Puffer voller Nullen ist von einem nie beschriebenen Puffer nicht zu unterscheiden — und ein
frisches Plattenabbild besteht genau daraus. Die Suite legt deshalb eine Magie („CAPROCK") in
Sektor 0. Die **Kapazität** ist die zweite, unabhängige Aussage: sie kommt aus dem
gerätespezifischen Konfigurationsraum (`VIRTIO_PCI_CAP_DEVICE_CFG`), den der RNG nicht hat und der
deshalb bis hierher nirgends aufgelöst wurde. Steht dort Müll, ist die Capability falsch
lokalisiert — ein Fehler, den die Magie allein nicht fände, weil der Datenpfad davon unberührt ist.
Die erwartete Zahl steht in der **Suite**, wo das Abbild entsteht, nicht im Kernel: dessen Aufgabe
ist, die Kapazität zu melden, nicht sie zu kennen.

Bei `net` dieselbe Frage, andere Antwort: geprüft wird nicht, ob der Empfangspuffer sich füllt,
sondern ob darin die **ARP-Antwort auf die eigene Anfrage** steht (Opcode 2, Absender-IP == die
angefragte). ARP ist dafür gewählt, weil es zustandslos ist, keinen Handshake und keine Zeitgeber
braucht — und weil die Antwort etwas trägt, das man nachrechnen kann.

**Beide Gegenproben laufen gelassen, nicht behauptet:** mit falscher Magie im Abbild meldet `vblk`
`FAILURES`, mit einer ARP-Zieladresse, für die niemand antwortet, meldet `vnet` `FAILURES` — und
beide Male läuft der Kernel in den Watchdog statt `SELFTEST COMPLETE` zu drucken. Sie stehen also
wirklich in `all_done()`.

### Was `net` zusätzlich prüft, das `blk` nicht kann

Zwei Queues mit **verschiedenem `queue_notify_off`**. Ein Treiber, der die Notify-Adresse der
ersten Queue für beide benutzt, weckt das Gerät auf der falschen Seite. Bei einem Einqueue-Gerät
kann dieser Fehler strukturell nicht auftreten — er wäre also bis zum ersten Mehrqueue-Gerät
unentdeckt geblieben.

Die Kopfgröße ist die zweite Falle: unter `VIRTIO_F_VERSION_1` sind es **immer 12 Byte**
(`virtio_net_hdr_mrg_rxbuf`), auch ohne ausgehandeltes `VIRTIO_NET_F_MRG_RXBUF`. Die 10-Byte-Fassung
gehört zum Legacy-Layout. Wer sie einsetzt, verschiebt jeden empfangenen Rahmen um zwei Byte und
findet den Ethertype an der falschen Stelle — ein Fehler, der wie „das Gegenüber antwortet nicht"
aussieht.

### Der Umbau, der dafür nötig war

`VirtioRng` war ein Monolith, in dem Handshake und Anfrage ineinander lagen. Bei einem Gerät fällt
das nicht auf; bei dreien wäre die bequeme Wahl gewesen, den Handshake zu **kopieren** — und damit
drei Fassungen derselben Zustandsmaschine zu haben, von denen zwei irgendwann still zurückbleiben.
Der virtio-Handshake ist genau die Sorte Ablauf, bei der eine vergessene Zeile (`FEATURES_OK` nicht
zurückgelesen) nicht auffällt, bis ein Gerät die Features ablehnt.

Herausgelöst sind deshalb `Transport` (Status, Features, Queues, Notify, Konfigurationsraum) und
`Queue` (Split-Ring). Alle drei Geräte sitzen darauf; die Crate hat weiterhin **keine einzige
Abhängigkeit**, `tools/kernel-grenze.sh` führt weiterhin **keine** Ausnahme.

`Queue` trägt bewusst **nur die CPU-Achse**. Die Gerätesicht kennt allein `Transport::queue_setup`,
wo sie in die Adressregister geht. Beide Adressen in einer Struktur zu halten, aus der man sich je
nach Zweck die passende greift, ist genau die Bequemlichkeit, die aus zwei Achsen wieder eine macht.

### Nebenbefund: die Lade-Suite war seit heute früh rot, und zwar aus diesem Grund

`test-qemu-x86-load.sh` startete `-device virtio-rng-pci` **ohne** `disable-legacy=on,iommu_platform=on`.
Das Gerät ist dann transitional und bietet `VIRTIO_F_ACCESS_PLATFORM` nicht an; der Treiber bricht
ab — richtig so. Nur steht `virtio` seit heute in `all_done()`, also wurde der Lauf nie fertig und
lief in den Watchdog. Das sah wie ein Hänger aus und war eine Gerätekonfiguration: die
`virtio`-Prüfung kam am selben Tag dazu, `test-qemu-x86.sh` bekam die Schalter, diese Suite nicht.
**Zwei Suiten, die dasselbe Gerät verschieden aufsetzen, sind ein Riss, durch den genau so etwas
fällt.**

---

## A-5.1. Der erste Treiber außerhalb des Kerns

**Halb erledigt 2026-08-01.** Die Treiber-PD läuft und bedient ihr Gerät; die **Richtungsumkehr**
(Treiber als Dienst, hot-reloadbar) steht weiter in [todo-A-ausfuehren.md](todo-A-ausfuehren.md).

### Gemessen

    devassign: zuteilbar: RID 0x0020, Konfigurationsseite 0xb0020000, BAR 0xfe004000+0x4000
    drv     : Teilergebnis der PD: Features-ok=1 used-fortgeschritten=1 Status=0x00
              geschrieben=513 Kapazitaet=2048
    drv     : Treiber-PD meldete=1 (laeuft=1 Fenster-gemappt=1 Geraet-aufgeloest=1);
              DMA-Region 0x3b51000, Geraetesicht 0x20e00000;
              erste acht Byte des Sektors 0x454b414c344c4553 (erwartet 0x454b414c344c4553)

`programs/hardware/virtio-blk` ist ein geladenes HardwareLand-Programm. Es mappt drei Fenster, löst
sein Gerät **selbst** auf, fährt den virtio-Handshake und liest Sektor 0 per Bus-Master-DMA. Der
Kern führt dabei **keinen** virtio-Schritt aus.

### Der Einwand, der wegfiel: „der Konfigurationsraum ist geräteweit"

Bis hierher galt: ein Treiber darf den PCI-Konfigurationsraum nicht sehen, denn wer ihn liest,
sieht jedes Gerät der Maschine. Deshalb sollte der Kern die virtio-Strukturen auflösen und dem
Treiber das **Ergebnis** reichen. Der Preis dieser Lösung: der Kern müsste virtio kennen.

Der Einwand stimmt für ein globales Adressregister (`0xCF8`/`0xCFC`) und für das ECAM-Fenster als
Ganzes. Er stimmt **nicht für eine Funktion**: ECAM bildet `(bus, dev, func)` auf je 4 KiB ab, also
auf genau eine Seite. Eine Funktion ist mappbar, ohne die Nachbarn mitzugeben.

Damit fällt die Aufteilung anders und besser aus: der Kern behält die **Enumeration** (welches
Gerät gibt es, wer bekommt es), der Treiber macht seinen **Capability-Lauf** auf seiner eigenen
Seite. `caprock_virtio::probe_ecam` ist dafür da — und die HAL ruft **dieselbe** Routine auf, statt
eine zweite Fassung davon zu halten.

### Zwei Aussagen aus zwei Quellen

Der Treiber meldet, dass seine **Transaktion** durchlief (Badge seiner endowten Notification). Der
Kernel prüft die **Bytes** in der Region, die er ausgegeben hat. Keine der beiden genügt allein:
meldet nur der Treiber, glaubt man dem Code, der es behauptet; prüft man nur die Bytes, könnte sie
auch jemand anders geschrieben haben. Die Gegenprobe mit falscher Magie im Abbild zeigt genau das —
`meldete=1`, und trotzdem `FAILURES`.

### Was dafür fehlte (und beim Bauen sichtbar wurde)

**1. `vspace_map_device` war auf x86 ein Stumpf, der `false` lieferte.** Der ganze
HardwareLand-Gerätepfad existierte dort nicht — nicht als Skip, sondern als Abwesenheit. Auf ARM
trägt ihn der RTC-Test seit ext-22; auf x86 hätte niemand gemerkt, dass er fehlt, bevor jemand ihn
brauchte. Genau die Fehlerform, wegen der die DMA-Tests nach `kernel/src/dmatests.rs` gewandert sind.

Die Implementierung hat eine Falle, die zählt: `vspace_create_base` hängt GiB 1..3 **jeder**
isolierten PD an dieselben statischen Tabellen (`ISO_PD_HIGH`) — drei Frames gespart, und richtig,
solange dort nichts PD-Spezifisches steht. Ein Gerätefenster ist genau das. Wer es dort hineinschriebe,
gäbe es **jeder** isolierten PD, und zwar lautlos: die Cap-Prüfung liefe korrekt durch, die Isolation
wäre trotzdem weg. Beim ersten Gerätefenster in einem GiB entsteht deshalb eine **private Kopie** der
Tabelle. `vspace_collect_device_tables` gibt sie beim Abbau zurück und lässt die geteilten in Ruhe —
die freizugeben wäre kein Leck, sondern das Gegenteil: der Allokator bekäme statischen Kernel-Speicher
als freies RAM.

**2. `SYS_MAP` konnte MMIO-/DMA-Caps nicht.** Es nahm nur `Memory`-Caps. Ein Treiber-PD hält aber
genau die anderen beiden. Dazu kam die Frage, **wo** seine Fenster liegen: `map` bildet identity ab,
und die physische Lage steht nirgends im Programm. Der bequeme Weg wäre ein Boot-Info-Block gewesen —
und der wäre „eine Autoritätsquelle neben dem Manifest" (so steht es seit A-2.1 in `loader::boot_arg`,
und es stimmt). `SYS_MAP` **beschreibt** jetzt stattdessen die Cap, die der Aufrufer ohnehin hält:
Basis, Länge und — bei DMA — die **Gerätesicht**. Neue Autorität entsteht dabei keine.

**3. Das Manifest galt nur für den Root-Task.** `SYS_LOAD` endowte ausschließlich, was der Lader
delegierte; die `initial_caps` aller anderen Einträge waren ein Wunschzettel. Jetzt gilt: die
**Loader-Cap** sagt, WER laden darf, das **Manifest** sagt, WAS das Geladene bekommt. Erst dadurch
kann ein Treiber Geräte-Autorität haben, ohne dass der Root-Task sie je besessen hätte — er könnte
sie sonst gar nicht weiterreichen.

**4. HardwareLand war über `SYS_LOAD` nicht ladbar.** `load_image` wies die Domäne ab: ein Backend ist
per Entwurf an einen Partner gebunden (ext-22), eine „bare" HardwareLand-PD bricht `domain_audit`.
Wer lädt, **ist** dieser Partner — eine andere Antwort gibt es nicht. Der Dispatch reicht deshalb die
Aufrufer-PD durch, und der Kanal entsteht **vor** den Anfangs-Caps: seine Notification *ist* die Cap,
die das Manifest mit `ntfn` meint. Eine frisch erzeugte wäre eine fremde gewesen, und die
HardwareLand-Cap-Policy hätte sie zu Recht abgewiesen — der Treiber hätte Geräte-Autorität gehabt und
keinen Weg, etwas zu sagen.

### Der Fehler, der eine Runde kostete

MMIO-Fenster wurden pauschal **schreibgeschützt** gemappt: im Dispatch stand `ro_kind = true` für
`ObjectKind::Mmio`. Der Treiber faultete beim allerersten Registerschreibzugriff
(`FAR=0xfe004014` = `common_cfg + device_status`). Ein Registerfenster, das man nicht beschreiben
kann, steuert nichts — ob ein Fenster schreibbar ist, sagt die **Cap**, nicht seine Art. `ro` bleibt
der DMA-Richtung `DeviceRead` vorbehalten: dort liest das Gerät, und der Puffer ist gegen es
geschützt.

Gefunden wurde er nur, weil die PD **Stufenbadges** meldet (läuft / Fenster gemappt / Gerät
aufgelöst) und ihr Teilergebnis in die eigene DMA-Region schreibt. Ohne das wäre jeder Fehlschlag
dieselbe Zeile gewesen — „meldete=0" —, egal ob das Programm nie startete, sein Fenster nicht mappen
konnte oder das Gerät nicht fand.

### Gegenproben (gefahren, nicht behauptet)

* Falsche Magie im Abbild → `drv : FAILURES`, obwohl der Treiber `meldete=1`.
* Kein Blockgerät → `drv : SKIP`, und der Treiber startet **gar nicht** (`NoDevice`, fail-closed):
  ein Treiber ohne Gerät meldet später einen Fehler, den niemand mit der Zuteilung in Verbindung
  bringt.

### Regression

x86 `RUNS=10` → 10 von 10 identische Signatur, `== ALL PASS ==`; Lade-Suite `== ALL PASS ==`;
aarch64 `== ALL PASS ==` (inkl. `rtc` — der ARM-Gerätepfad — und `virtiorng`, das jetzt durch
dieselbe `probe_ecam`-Routine läuft); `tools/kernel-grenze.sh` ohne Ausnahme.

---

## A-5.1. Der Treiber als Dienst — und sein Austausch

**Erledigt 2026-08-02.** Vorstufe (die PD läuft und liest einmalig einen Sektor) steht weiter oben;
hier steht der Teil, der A-5.1 abschließt: die **Richtungsumkehr** und der **Austausch**.

### Gemessen (Lade-Suite)

    drv : Zuteilung: DMA-Region 0x3b51000, Geraetesicht 0x20e00000
    drv : Anfrage 1 an v1: Status=0 Bytes=0x454b414c344c4553 Kapazitaet=2048 bedient=1
    drv : Austausch: Ergebnis=0 (umgebunden OHNE Empfaengerluecke); v1 bereit=1 v2 bereit=1
    drv : Anfrage 2 an v2: Status=0 Bytes=0x454b414c344c4553 Kapazitaet=2048 bedient=2

### Die Richtungsumkehr

Bis hierher rief der Kernel den Treiber: er selbst fuhr den virtio-Handshake. Jetzt **wartet** der
Treiber (`recv`), ein Client **fragt** (`call`), der Treiber **antwortet** (`reply`). Der Kernel ist
Client, nicht Treiber.

Das ist keine Umbenennung. Es ist die Bedingung dafür, dass „austauschbar" eine Eigenschaft des
**Mechanismus** wird statt dieses einen Treibers: in `loader::reload_driver` kommt das Wort
„virtio" nicht vor. Der Kernel ersetzt einen Empfänger an einem Endpoint; was dahinter steckt, weiß
er nicht. Genau das ist die Abnahmebedingung.

### Der Austausch, und in welcher Reihenfolge

1. Die neue Fassung wird geladen — in eine **zweite** Backend-PD am **selben Kanal**. Der Endpoint
   ist das, was den Austausch überlebt; bekäme v2 einen eigenen, müsste jeder Client umgehängt
   werden. Genau die gerissene IPC-Beziehung, gegen die A-4.1 gebaut ist.
2. Sie bekommt **dieselbe** Gerätezuteilung — neue Caps, dieselben Fenster, dieselbe DMA-Region,
   dieselbe IOVA. Das Gerät wird nicht losgelassen und nicht neu angehängt: die Übersetzung im
   IOMMU-Kontext bleibt stehen. Das ist der Unterschied zwischen einem Austausch und einem Neustart.
3. Erst wenn v2 wirklich am Endpoint steht, wird **stillgelegt** (A-4.2) und **umgebunden** (A-4.1).
   Weil v2 vorher schon Empfänger war, ist der Ausgang `overlapped`: der Endpoint hatte zu **keinem**
   Zeitpunkt null Empfänger.

Dass A-4.1 damit zum ersten Mal an einem **echten** Dienst abgenommen ist, ist ein Nebenertrag: der
`rebind`-Selbsttest lief bis dahin am lokalen `Endpoint`-Objekt, weil der überlappende Erfolgsfall
sonst nicht herstellbar war.

**Zur Überlappung:** zwischen Schritt 2 und dem Abbau von v1 halten kurzzeitig **beide** Fassungen
Caps auf dasselbe Gerät. Das ist nicht dasselbe wie „zwei Treiber": die Stilllegung sorgt dafür,
dass in diesem Fenster keiner von beiden eine Anfrage bekommt. Autorität zu **halten** und sie zu
**benutzen** sind verschiedene Dinge, und nur das zweite wäre hier ein Fehler.

### Woran man sieht, dass es ein Austausch war und kein Neustart

Der **Bedienungszähler** liegt in der DMA-Region, nicht in einer Programmvariablen. Eine Variable
stirbt mit der Fassung, die Region nicht. `bedient=1` → `bedient=2` belegt deshalb, dass v2 dieselbe
Region geerbt hat. Bekäme sie eine frische, stünde dort wieder 1 — und der Test wäre rot.

Dazu kommen zwei **verschieden gebadgte** Bereit-Meldungen (v1 und v2 bekommen beim Endowment
unterschiedliche Badges). Ohne sie wäre „eine Fassung hat sich zweimal gemeldet" von „zwei
Fassungen haben sich gemeldet" nicht zu unterscheiden.

### Der Fehler, den erst die Wiederverwendung sichtbar gemacht hat

`queue_setup` nullte nur den **Treiberteil** der Ringe (`avail`) — `used` gehört schließlich dem
Gerät. Das ist genau falsch, und es fällt nur auf, wenn eine Region wiederverwendet wird:

* das Gerät setzt seinen used-Index beim Reset auf 0 zurück;
* im Speicher steht aber noch der Endstand der vorigen Fassung, hier 1;
* die neue Fassung merkt sich diesen Stand als Ausgangswert und wartet auf eine Änderung — das
  Gerät schreibt nach der ersten Anfrage wieder genau 1.

Der Treiber wartet also auf einen Fortschritt, der bereits eingetreten ist, und läuft in seine
Poll-Schranke. Im Log sah das aus wie ein stummes Gerät (`Status=1`), und die Bytes im Puffer waren
trotzdem richtig — weil sie noch von v1 stammten. Genau die Sorte Befund, die ohne Inhaltsprüfung
als Erfolg durchgegangen wäre.

Die Spec ist eindeutig: **der Treiber** initialisiert den Speicher der Queue, bevor er sie
freigibt, und vor `QUEUE_ENABLE` gehört er ihm. Gegenprobe gefahren: mit wieder entfernter
`used`-Nullung meldet Anfrage 2 `Status=1`, und der Lauf ist rot.

### Was A-5.1 ausdrücklich NICHT einschließt

**`CAP_IRQ`.** Der Treiber pollt. Ein Geräte-Interrupt käme auf x86 per MSI-X, und seit B-3.2 steht
die Interrupt-Remapping-Tabelle auf lauter „not present": ein Gerät ohne IRTE kann keinen Interrupt
auslösen — mit Absicht. Eine IRTE-**Vergabe** gibt es nicht (nachgesehen, nicht vermutet). Der Weg
dorthin ist B-3-Arbeit. `endow_from_manifest` weist `CAP_IRQ` deshalb **ab**, statt eine Autorität
zu erteilen, die niemand einlöst — dieselbe Regel wie bei `CAP_PD_CONTROL` in A-2.1.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=8` → 8 von 8 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; `tools/kernel-grenze.sh` ohne Ausnahme.

---

## A-6.1. Das Dienstprotokoll über dem Treiber

**Erledigt 2026-08-02.** Der erste Schritt von A-6 („über dem Sektor"), und alles davon läuft
**außerhalb des Kerns**.

### Gemessen (Lade-Suite)

    blkdev : INFO Status=0 Kapazitaet=2048 Sektorgroesse=512;
             READ(0)=0 WRITE(100)=0 FLUSH=0;
             Rueckgelesen=0x454b414c344c4553 (erwartet 0x454b414c344c4553);
             READ(jenseits der Platte)=3 (erwartet 3 = Bereich)

### Der Puffer des Treibers ist die Ablage

`READ` füllt ihn, `WRITE` schreibt ihn zurück — der Client nennt Sektoren, keine Adressen. Das ist
kein Notbehelf, sondern das übliche Staging-Modell eines Blockgeräts: der Puffer muss DMA-fähig und
gerätesichtbar sein, und beides kann ein beliebiger Client-Puffer nicht zusagen.

Der Nebeneffekt ist ein **besserer Test**: `READ(0)` holt die Magie von der Platte, `WRITE(100)`
schreibt **genau diese Bytes** woandershin. Kein erfundenes Muster — echte Daten, die von der Platte
kamen. Und der Rückleseschritt ist die eigentliche Aussage: eine quittierte Schreibanfrage ist eine
Quittung, keine Daten.

Eine geteilte Übertragungsfläche kommt erst mit A-6.2, wo es einen Abnehmer dafür gibt. Eine
Schnittstelle vor ihrem ersten Benutzer belegt nur eine Vermutung.

### `ST_RANGE` ist ein eigener Status

„Ich habe nicht gefragt" ist für den Client eine andere Lage als „das Gerät hat nein gesagt". Die
erste ist sein Fehler, die zweite nicht. Die Bereichsprüfung liegt deshalb **vor** dem Gerät: ein
Gerät, das über sein Ende hinaus gefragt wird, darf antworten, wie es will — der Dienst hat vorher
nein zu sagen. Geprüft wird beides: der Sektorbereich gegen die Kapazität **und** die Anzahl gegen
die Puffergröße (sonst schriebe das Gerät hinter das Ende der Region).

### Der Fehler, der eine Runde kostete: die Erwartung hing an der falschen Größe

`completed()` verlangte `Sektoren * 512 + 1` geschriebene Bytes. Beim **Lesen** stimmt das (Daten +
Statusbyte); beim **Schreiben** und beim **Flush** schreibt das Gerät nur das Statusbyte, also genau
eins. Ergebnis: jede Schreibanfrage fiel durch — und zwar mit `Status=1`, was nach einem Gerätefehler
aussieht, **während die Daten längst auf der Platte standen** (der Rückleseschritt fand sie). Ohne
den Rückleseschritt hätte der Befund in die falsche Richtung gezeigt.

Die Erwartung steht jetzt als `expected_written` **im Ergebnis**, gebildet dort, wo die Richtung
bekannt ist. Eine Größe, die von etwas abhängt, das der Aufrufer nicht sieht, gehört nicht in seine
Rechnung.

### Zwei eigene Fehler mit derselben Form: ein Urteil, das seinen eigenen Bericht auslösen soll

Zweimal in Folge stand ein Prüfergebnis in `all_done()` **und** wurde erst im Bericht gebildet. Dann
kann es den Bericht nicht auslösen: der Lauf läuft in den Watchdog und druckt das Ergebnis trotzdem.
Im Log sieht das aus wie „grün, aber gehangen".

Beim zweiten Mal kam eine Variante dazu: die Zustandsmaschine wartete auf einen Treiber-Dienst, den
es ohne Boot-Archiv **nie** geben kann — also blieb das Urteil ungesetzt und die Hauptsuite hing.
`driver_service().is_none()` taugt dort nicht als Kriterium: unmittelbar nach dem Start ist es auch
dann `None`, wenn gleich einer kommt. „Es gibt kein Archiv" ist die Aussage, die von Anfang an
feststeht.

**Regel daraus:** ein Wert, der in `all_done()` steht, muss **außerhalb** des Berichts entstehen.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=8` → 8 von 8 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; `tools/kernel-grenze.sh` ohne Ausnahme.

**Ein Nebenbefund, der nicht von hier stammt:** die aarch64-Suite zeigte in einem von vier Läufen
ein sporadisches `xfer : FAILURES` (Capability-Transfer in IPC), die drei anderen waren grün. Sie
hat keine Wiederholungsmessung wie die x86-Suite (`RUNS`), also fällt so etwas nur zufällig auf.
Gehört zu D0/B-1.3, nicht zu A-6.

---

## A-6.2. Die Partitionstabelle — im Dienst gelesen, nicht im Kern

**Erledigt 2026-08-02.**

### Gemessen

    part : GPT-Scan Status=0 belegte Eintraege=2 (erwartet 2);
           erste Partition LBA 34 ueber 967 Sektoren (erwartet 34 / 967)

Gegenprobe mit drei kaputten Tabellen — **alle drei abgewiesen, mit unterscheidbarem Grund**:

| kaputt gemacht | gemeldet |
|---|---|
| Signatur | `ABGEWIESEN, Grund=2` (Signatur) |
| Kopf-CRC | `ABGEWIESEN, Grund=5` (Kopf-CRC) |
| Eintrags-CRC | `ABGEWIESEN, Grund=8` (Eintrags-CRC) |

Dazu **14 von 14** Host-Tests des Parsers (Sekunden, über `rustc --test`, nicht über den
`build-std`-Pfad).

### Warum der Parser nicht in den Kern gehört

Eine Partitionstabelle sind **fremde Bytes auf einer Platte**, die ein beliebiger Mandant
geschrieben haben kann. Sie mit Kernprivileg zu interpretieren ist genau die Klasse Fehler, die man
dort nicht haben will. `caprock-part` hängt deshalb an nichts, ist `forbid(unsafe_code)`, und wird
vom **Blockdienst** gelinkt — der außerhalb des Kerns läuft (A-5.1).

Dass ein Blockdienst Partitionen meldet, ist dabei nichts Ungewöhnliches: das ist die Aufgabe einer
Blockschicht.

### Zwei Stellen, an denen ein GPT-Parser gern schlampt

**Der Kopf-CRC läuft über den Kopf, in dem das CRC-Feld selbst steht** — und dieses Feld muss dabei
als **Null** gelten. Wer es mitrechnet, bekommt nie eine Übereinstimmung; wer es *überspringt*
statt es zu nullen, verschiebt alle folgenden Bytes. Beides sieht wie „Tabelle kaputt" aus.

Der eigene Test ist beim ersten Anlauf genau da hineingetappt: er baute eine Mutation und rechnete
die Prüfsumme neu, während im CRC-Feld noch die alte stand. Ergebnis war `HeaderCrc` statt des
Fehlers, den der Test auslösen wollte — er hätte also etwas anderes belegt, als er behauptet. Jetzt
gibt es dafür `reseal()`, und die Falle steht in der Modul-Doku.

**Die Eintragsliste kommt in Stücken.** Sie ist 16 KiB groß, eine Anfrage liefert 4 KiB. Die
Prüfsumme muss trotzdem über das **Ganze** gehen — wer je Stück prüft, prüft ein Stück und glaubt an
die Tabelle. `Crc32` ist deshalb fortschreibbar. Und der letzte Happen darf nicht auf die
Sektorgrenze aufgerundet eingerechnet werden: die Prüfsumme gilt für `entries_bytes`, nicht für die
gelesenen Sektoren.

### Jede Bedingung einzeln, mit eigenem Fehler

Ein Parser für fremde Daten ist Angriffsfläche. `PartError` unterscheidet acht Fälle, statt in ein
`Option` zusammenzufallen: der Unterschied zwischen „hier ist gar keine GPT" (Signatur) und „hier
ist eine, die nicht mehr stimmt" (CRC) ist der Unterschied zwischen einer unformatierten Platte und
einem Datenverlust. `num_entries * entry_size` wird **geprüft** multipliziert — eine Längenprüfung
hinter einem übergelaufenen Produkt ist eine Attrappe.

### Die Testabbilder baut ein eigenes Werkzeug

`tools/mkgpt.py`, nicht `sgdisk`/`parted`. Zwei Gründe:

* eine Suite, die an einem Fremdwerkzeug hängt, fällt auf einem Rechner ohne dieses Werkzeug als
  „Test rot" aus statt als „Aufbau unvollständig" — eine Verwechslung, die dieses Projekt schon
  mehrfach bezahlt hat;
* nur ein eigenes Werkzeug kann den **Negativfall** herstellen (`--break signature|header-crc|
  entries-crc`), gegen den der Parser abgenommen wird.

**Beide Suiten benutzen dasselbe Werkzeug.** Zwei Suiten, die dieselbe Platte verschieden aufsetzen,
wären derselbe Riss wie zwei, die dasselbe Gerät verschieden aufsetzen — das stand am 2026-08-01
schon einmal in dieser Datei. Die Magie liegt seither auf LBA 34 (erster Sektor der ersten
Partition); auf LBA 0 steht der schützende MBR.

### Der Watchdog kann jetzt sagen, worauf er gewartet hat

Er druckte „nicht alle Aussagen belegt" und sonst nichts. Damit ist ein Hänger von einem nicht
erfüllten Kriterium nicht zu unterscheiden — man sieht, DASS es nicht fertig wurde, und muss raten.
Beim Umstellen auf GPT hat das eine Runde gekostet: alle Einzelzeilen standen auf `ALL PASS`, der
Lauf lief trotzdem in die Notbremse. Jetzt nennt sie die offenen Punkte beim Namen
(`bringup : offen waren: root vblk blkdev part`), und der Befund war in einer Zeile sichtbar: der
Kernel-Test aus A-5.2 las noch LBA 0, wo jetzt der MBR steht.

### Nebenbefund: die aarch64-Suite ist nicht deterministisch

Über gut zehn Läufe an diesem Tag: **drei** sporadische Ausfälle an **drei verschiedenen**
Prüfungen (`xfer`, `dtb`, einer weiteren), dazwischen 4 von 4 grün am Stück. Die ARM-Suite hat
keine Wiederholungsmessung wie `RUNS` auf x86 — so etwas fällt dort nur zufällig auf, und ein
einzelner roter Lauf ist von einem echten Befund nicht zu unterscheiden. Gehört zu D0/B-1.3.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=6` → 6 von 6 identische Signatur, `== ALL PASS ==`;
aarch64 4 von 4 `== ALL PASS ==`; Parser-Host-Tests 14/14; `tools/kernel-grenze.sh` ohne Ausnahme.

---

## A-6.3. Ein lesendes Dateisystem als eigene PD

**Erledigt 2026-08-02.** Damit ist A-6 vollständig: Blockdienst, Partitionstabelle, Dateisystem —
und **kein Schritt davon liegt im Kern**.

### Gemessen

    fs : Status=0 (0=Datei gelesen, 2=nicht gefunden, 3=Kette defekt, 4=Kette zu kurz)
         Groesse=20 erste acht Byte=0x454b414c344c4553 Cluster=1

Der Weg dahin: `INFO` → `SCAN` (GPT, beide Prüfsummen) → Bootsektor der Partition → `parse_boot` →
Wurzelverzeichnis durchsuchen → Clusterkette folgen. Jeder Sektor kommt über den Blockdienst per
IPC; die PD selbst fasst kein Register an.

Dazu **16 von 16** Host-Tests des FAT-Parsers (Sekunden, über `rustc --test`).

### Warum das eine eigene PD ist und der GPT-Scan nicht

Den GPT-Scan macht der Blockdienst selbst (A-6.2) — das ist bei einer Blockschicht üblich und war
die kleinere Änderung. Ein **Dateisystem** dort unterzubringen wäre etwas anderes: es interpretiert
beliebig viel fremde Struktur, und je mehr davon in der PD liegt, die das Gerät steuert, desto
weniger bedeutet „Fehlereindämmung". Ein Treiber hat mit Verzeichniseinträgen nichts zu tun.

### Der Fund, der den Entwurf verbessert hat: das Zertifikats-Gate hat Nein gesagt

Die FS-PD ist TrustedSAS und braucht deshalb ein Zertifikat (ADR 0014). Das Gate hat sie
**abgewiesen**:

    fs: 2 unsafe [PROGRAMM] forbid_unsafe_code=NEIN
    UNSAFE-AUDIT FEHLGESCHLAGEN fuer fs -> KEIN Zertifikat.

Die zwei `unsafe` waren die rohen Zugriffe auf das gemappte Fenster. Der bequeme Ausweg wäre
gewesen, die PD nach UserLand zu verschieben (kein Zertifikat nötig) — und damit eine Regel zu
umgehen, statt ihr zu folgen.

Der richtige Ausweg war, das `unsafe` **dorthin zu legen, wo es hingehört**: ins auditierte SDK,
das ohnehin auf der Allowlist steht. `libcaprock::Window` ist die Kapsel — er entsteht **nur** aus
`map_window` (also aus Basis und Länge, die der Kernel gerade selbst gemappt hat), seine Felder sind
privat, und **jeder** Zugriff wird gegen die Länge geprüft, mit geprüfter Addition. Dieselbe Bauart
wie `Verified` im Manifest-Parser: die Bedingung trägt der Typ, nicht die Disziplin des Aufrufers.

Danach ist die PD `forbid(unsafe_code)` und das Gate lässt sie durch. **Ein Prüfer, der Nein sagt,
hat den Entwurf verbessert** — das ist der Zweck.

### Warum eine eigene Übertragungsfläche und nicht die DMA-Region

Die DMA-Region des Treibers ist **non-coherent** gemappt (Normal-NC), damit das Gerät
hineinschreiben kann, ohne dass jemand Cache-Wartung fährt. Eine zweite, **gecachte** Abbildung
derselben Seiten in der Client-PD wäre auf x86 ein Attribut-Alias — laut SDM undefiniert, in der
Praxis „geht meistens". Also eine eigene Region aus normalem RAM, in beiden PDs mit denselben
Attributen, und der Treiber **kopiert**. Genau das tut ein echter Treiber ohnehin, sobald der
Puffer des Clients nicht DMA-fähig ist.

### Drei Rollen, drei Badges — und was passiert, wenn man das übersieht

Root-Task, Treiber und Client melden sich alle über eine endowte Notification. Beim ersten Anlauf
teilten sich Treiber und Client eine Ablage: die zuletzt geladene PD überschrieb sie, und der
Kernel wartete auf ein Signal am **falschen Objekt**. Der Austausch meldete daraufhin `NotReady` —
was nach einem Zeitproblem aussieht und eine vertauschte Ablage war.

Zweiter Fund derselben Sorte: beim Hot-Reload fehlte der neuen Fassung **Slot 6** (die
Übertragungsfläche). Sie brach korrekt ab — der Treiber verlangt sie —, und der Austausch meldete
wieder `NotReady`. **Wer eine Fassung ersetzt, muss ihr alles geben, was die alte hatte**; eine
Endowment-Liste, die beim Ersetzen von der beim Erstladen abweicht, ist ein Riss.

Und: zwei Clients desselben Dienstes brauchen eine **Reihenfolge**. Liefen sie gleichzeitig,
mischten sich ihre Anfragen, und der Austausch fiele mitten in ein fremdes Gespräch. Der
Bedienungszähler wird deshalb **relativ** geprüft (`r2 == r1 + 1`) statt absolut — ein fester
Startwert wäre eine Aussage über die Reihenfolge aller Clients statt über den Austausch.

### Das Testabbild

`tools/mkgpt.py` baut jetzt auch ein **lesbares FAT16** (`--fat16`, `--file`) — selbst gebaut, aus
demselben Grund wie die GPT: `mkfs.vfat` ist ein Fremdwerkzeug, und eine Suite, die daran hängt,
fällt ohne es als „Test rot" aus statt als „Aufbau unvollständig".

Die Platte hat **zwei** Partitionen: die erste trägt das FAT16, die zweite ist roh und trägt die
Magie der älteren Tests. Getrennt, weil die Magie sonst auf dem FAT-Bootsektor läge — zwei Tests,
die sich dieselbe Fläche teilen, sind ein Riss, durch den beide fallen können.

**Die Größe der Partition ist eine Bedingung, kein Detail:** unter 4085 Clustern ist es FAT12, nicht
FAT16, und als FAT16 gelesen liefert eine FAT12-Tabelle lauter falsche Ketten, ohne dass irgendetwas
kaputt aussieht. `mkgpt.py` prüft das und bricht ab, statt ein Abbild zu schreiben, das der Parser
zu Recht ablehnt.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=6` → 6 von 6 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; Host-Tests 14/14 (GPT) und 16/16 (FAT); `tools/kernel-grenze.sh` ohne
Ausnahme.

---

## A-6.4. Schreiben — und der zweite Melder

**Erledigt 2026-08-02.**

### Gemessen

    fs   : Schreiben Status=0; Rueckgelesen Status=0 Groesse=700 (erwartet 700) geprueft=700 Byte
    PASS : A-6.4: OK: HELLO.TXT, 700 Byte, 2 Cluster, 2 FAT-Kopien gleich
           -- unabhaengig vom Kernel am Abbild nachgelesen

Die Datei wächst von 20 auf 700 Byte, also **über einen zweiten Cluster hinaus**. Eine
Schreibprobe, die in den vorhandenen Cluster passt, prüft die Kettenverlängerung nicht — und die
ist der Teil, an dem ein Schreiber schiefgeht.

### Der Fund, der den Aufwand rechtfertigt

`tools/checkfat.py` liest dasselbe Abbild mit einer **zweiten, unabhängigen** Implementierung
(Python, andere Sprache, andere Rechnung) — auch das Muster ist dort **noch einmal** hingeschrieben
und nicht importiert: zwei Implementierungen derselben Regel stimmen nur überein, wenn die Regel
eingehalten wurde; eine geteilte Funktion stimmte auch dann, wenn beide falsch sind.

Die Gegenprobe hat den Wert sofort gezeigt. Schreibt die PD absichtlich nur **eine** FAT-Kopie:

    fs   : ALL PASS      <- der Kernel meldet Erfolg
    FAIL : A-6.4: die 2 FAT-Kopien sind NICHT gleich

Der Kernel-seitige Befund bleibt grün, **weil die PD über Kopie 0 zurückliest** — sie bestätigt
ihr eigenes Ergebnis mit derselben Sicht, mit der sie es geschrieben hat. Nur der unabhängige Leser
sieht, dass ein anderer Leser (der Kopie 1 benutzt) etwas anderes sähe. Genau dafür ist die zweite
Quelle da.

### Der häufigste Schreibfehler überhaupt

Ein FAT-Dateisystem hat üblicherweise **zwei** Kopien der Tabelle. Wer nur die erste fortschreibt,
hinterlässt ein Dateisystem, das jedes Prüfwerkzeug als beschädigt meldet — und das ein Leser der
zweiten Kopie anders sieht als der Schreiber. Die Zahl der Kopien steht im Bootsektor; sie zu
ignorieren ist der Fehler, den `Fat16::fat_copy_lba` unmöglich machen soll, und der Host-Test
`schreiben_setzt_beide_fat_kopien` fängt ihn.

### Und der Flush

Ohne ihn ist „geschrieben" eine Aussage über einen Puffer, nicht über die Platte. Das ist der
Unterschied zwischen einem Dateisystem, das einen Stromausfall überlebt, und einem, das es meistens
tut. Der Statuscode des Flush wird **weitergereicht**, nicht verworfen — ohne ausgehandeltes
`VIRTIO_BLK_F_FLUSH` darf das Gerät ihn ablehnen, und dann gilt die Dauerhaftigkeitszusage nicht.

### Was nicht dabei ist

Anlegen und Löschen von Dateien, Unterverzeichnisse, lange Namen. Die Suche nach einem freien
Cluster geht nur über den **ersten** FAT-Sektor — für die Probe reicht das, für einen
Produktivtreiber nicht, und das steht im Code an der Stelle statt in einer Fußnote.

### Regression

Lade-Suite `== ALL PASS ==`; x86 `RUNS=6` → 6 von 6 identische Signatur, `== ALL PASS ==`;
aarch64 `== ALL PASS ==`; Host-Tests 20/20 (FAT) und 14/14 (GPT); `tools/kernel-grenze.sh` ohne
Ausnahme.

---

## B-3.4. Der Interrupt-Nachrichtenbereich als IOVA

**Erledigt 2026-08-02.**

Auf x86 behandelt VT-d eine DMA-Schreibung nach `0xFEE0_0000..0xFEF0_0000` als
**Interrupt-Nachricht** und befragt die Übersetzung gar nicht. Eine IOVA dort ist damit unbenutzbar
— und zwar auf die unangenehmste Art, die es gibt: die Seitentabellen sähen völlig richtig aus, das
Gerät erzeugte trotzdem Interrupts statt Speicherzugriffen. **Kein Fault, kein Eintrag in der
Fehlerwarteschlange, nur Daten, die nirgends ankommen.**

### Strukturell statt geprüft

Die Fensterbasis liegt jetzt oberhalb des Bereichs. Die Alternative wäre gewesen, bei jeder Vergabe
zu prüfen — und eine Bedingung, die an jeder Vergabestelle richtig geprüft werden muss, vergisst die
nächste Vergabestelle. Der Preis sind ein paar GiB ungenutzter IOVA-Raum von 39 Bit. Das ist kein
Preis.

Der Bereich kommt aus der HAL (`iommu::interrupt_message_window`) statt aus einer `cfg`-Verzweigung
beim Aufrufer. Auf aarch64 liefert sie `None` — und das ist eine **Zusage** („jede IOVA wird
übersetzt"), keine Unkenntnis. Sollte je eine Plattform dazukommen, deren MSI-Doorbell außerhalb der
Übersetzung liegt, gehört sie dorthin und nicht in eine Sonderbehandlung.

### Es war ein echtes Loch

Die Gegenprobe ohne das Überspringen: `dmawin : ... kein Kontext-Fenster im
Interrupt-Nachrichtenbereich=0`, `dmawin : FAILURES`. Das Fenster lag vorher **wirklich** darin —
mit 512 MiB RAM beginnt es bei ~`0x2000_0000` und spannt über 39 Bit, `0xFEE0_0000` liegt mitten
drin.

Der Nachweis steht als eigenes Feld (`msi_clear`) und eigene Zeile, nicht als stille Konjunktion:
die anderen Fenstergrenzen führen zu einem sauberen Fehlschlag, dieser Bereich zu **gar keinem**.
Genau deshalb muss die Aussage einzeln dastehen.

---

## A1-Rest. `caprock_mem::stripe` rechnete über 64 Bit statt über die Farben

**Erledigt 2026-08-02** (parallel bearbeitet, Ergebnis hier integriert).

CLAUDE.md führte das seit dem 2026-08-01 als offen: „`caprock_mem::stripe` hat denselben Fehler".
Es stimmte — und der Fehler war schlimmer als die Notiz vermuten ließ.

### Gemessen, nicht argumentiert

`stripe(i, n)` rechnete `per = MASK_BITS / n`, also Blöcke über alle 64 Bit. `ColorMask::contains(c)`
fragt aber Bit `c % 64`; bei 16 Farben werden die Bits 16..63 **nie** befragt. Gegen den
unveränderten Code, `n = 4`:

    colors=16   Streifen 0: 16 Farben, erwartet 4    <- der ganze Farbraum
    colors=16   Streifen 1..3: je 0 Farben           <- keine einzige
    colors=256  Streifen 0..3: je 64 Farben          <- zufällig richtig

**Die schlimmere Hälfte:** `is_empty()` meldete für diese Sätze `false`, und `intersects` meldete
„disjunkt" — leere Mengen schneiden sich nicht. Der Selbsttest `run_stripe_alloc` wäre auf aarch64
**grün** gewesen, ohne dass irgendetwas getrennt war. Das ist exakt die Fehlerform, gegen die das
Entwurfsprinzip dieses Projekts geschrieben ist: ein Prüfer, der über Abwesenheit entscheidet, ohne
sprechfähig zu sein.

### Die Signatur musste sich ändern

`stripe(i, n)` → `stripe(i, n, colors)`. Ein festes 64-Bit-Muster kann nicht gleichzeitig bei 16
und bei 256 Farben richtig **und** zusammenhängend sein: bei 16 verlangt Korrektheit Streifen `i` =
Farben `4i..4i+3`, bei 256 einen 16-Bit-Block. Das legt unvereinbare Muster fest. Die einzige
parameterfreie Alternative wäre Verschränkung (Lauflänge 1) — die das Modul ausdrücklich verwirft,
weil `region_bytes()` auf dem Zusammenhang steht.

Bei 256 Farben sind die Masken **bit-identisch** zu vorher; x86 verhält sich unverändert, und die
Suite bestätigt das (`color : ALL PASS`, `stripe : ALL PASS`, Werte unverändert).

### Sensitivitätsmessung statt Behauptung

Alte Rechnung hinter der neuen Signatur wiederhergestellt: **17 passed, 5 failed**. Danach:
**22 passed, 0 failed** (vorher 18). Die vier neuen Tests laufen ausdrücklich über beide
Farbanzahlen — ein Test nur bei 256 hätte den Fehler nie gesehen.

### Zwei Nicht-Änderungen, bewusst

* **`pick_free` hat den Fehler nicht.** Seine Schranke `n > 32` ist die Breite des `taken: u32`,
  eine echte Eigenschaft des Typs, keine Farbanzahl.
* **`alloc.rs` `npages > MASK_BITS` bleibt.** Sieht nach derselben Familie aus, ist keine: eine
  Verschärfung auf `colors.min(MASK_BITS)` wäre ein Falsch-Negativ.

---

## B-3.3. Mehr-Einheiten-Aggregation — die Hälfte, die schlimmer war

**Erledigt 2026-08-02** (parallel bearbeitet, Ergebnis integriert; nur `x86_64/vtd.rs` und
`x86_64/dmar.rs` übernommen).

Die Todo-Notiz sagte: „`VtdCaps` ist noch eine Einheit". Das stimmte **nicht mehr** — `caps_common()`
aggregierte längst, ebenso die Fault-Zähler und `discover`. Was die Notiz **nicht** sagte, war der
schlimmere Teil:

| Stelle | was wirklich passierte |
|---|---|
| `init()` | stellte **nur Einheit 0** scharf. Einheiten 1..n blieben mit `TE = 0` — und das heißt nicht „blockiert", sondern **keine Übersetzung**. Das ist kein blindes Oracle, das ist kein Schutz. |
| `invalidate_context_cache()` | lief nur auf Einheit 0. Nach `context_clear` blieb das Gerät an den übrigen Einheiten aus dem Cache übersetzt. |
| `flush_entry` / `slpt_map` | lasen die Fähigkeiten von **Einheit 0** und trafen damit **Politik**: `ECAP.C` entscheidet über `clflush`, `ECAP.SC` über `SNP`. Fehlt einer anderen Einheit `SC`, ist `SNP` dort ein **reserviertes** Bit — wörtlich der `STE.S1STALLD`-Mechanismus, den dieses Projekt schon einmal bezahlt hat. |
| Sprechfähigkeit | fehlte ganz. `ru32` liefert bei fehlender Basis `0`; eine deklarierte, stumme Einheit trug damit genauso zu `faults_empty() == true` bei wie eine fehlerfreie. |

### Was daraus folgt

Bei Fähigkeiten ist das **Minimum** die einzige sichere Richtung — wer das Maximum nähme, verspräche
etwas, das eine der Einheiten nicht kann. Bei Fehlern umgekehrt die **Summe**: ein Fault an Einheit 1,
den niemand zählt, ist ein Isolationsbruch, den das Oracle nicht sieht.

Stumme und abgeschnittene Einheiten zählen jetzt in `CFG_ERRORS` und werden über den bestehenden
Audit-Code 6 sichtbar — je einmal gelatcht, weil beides Zustände sind und keine Ereignisse; sonst
wäre der Zähler eine Funktion der Aufrufhäufigkeit.

### Ein Befund, der nur gemeldet werden konnte — und jetzt behoben ist

`VtdEnforcer::detach` hatte `let Some(caps) = caps_common() else { return }`. Solange `caps_common()`
praktisch immer `Some` war, fiel das nicht auf. Seit eine stumme Einheit erkannt wird, kann es `None`
werden — und dann bliebe eine **Übersetzung stehen, während ihre Region freigegeben wird**. Genau die
Lage, gegen die das Teardown-Token existiert, nur ohne jede Meldung.

Behoben: der Ausfall wird gezählt und über einen neuen `dma_audit`-**Code 8** sichtbar. Ein stilles
`return` wäre die schlimmere Variante desselben Fehlers gewesen — kein Schutz **und** kein Hinweis
darauf.

### Grenze der Aussage

Die Mehr-Einheiten-Pfade sind auf QEMU q35 nicht erreichbar (genau **eine** DRHD). Sie sind also
durch keinen Lauf gedeckt, sondern nur durch Codeprüfung und dadurch, dass der Ein-Einheit-Fall
bitgleich bleibt. Das steht hier, damit niemand die grüne Suite für einen Beleg der Aggregation hält.

---

## D6. Die aarch64-Suite maß gar nicht — und das sah aus wie ein Kernelfehler

**Teilweise erledigt 2026-08-02.**

Über rund 13 Läufe fielen **vier verschiedene** Prüfungen aus (`xfer`, `dtb`, `prio`, `loadstop`),
scheinbar zufällig, rund 30 %. Das sah nach Nichtdeterminismus im Kernel aus. Es war zur Hauptsache
die **Testmechanik**, und zwar in zwei Schichten:

1. **Die Ausgabe hing an einer Pipe**, beendet wurde mit `--signal=KILL`. Ein per SIGKILL
   erschlagenes QEMU flusht seinen stdout-Puffer nicht. Dieselbe Falle hatte die x86-Suite am
   2026-08-01 schon einmal bezahlt — sie schreibt seither in eine Datei.
2. Nach dem Umbau auf eine Datei teilten sich **alle Läufe eine** Datei. Gelegentlich fehlten dann
   **frühe** Bootzeilen in der ausgewerteten Ausgabe, während alle Ergebniszeilen da waren. Die
   Signatur blieb deshalb identisch, und trotzdem fiel je nach Lauf eine **andere** Prüfung durch —
   genau die, deren Zeile nicht Teil der Signatur ist.

Jeder Lauf bekommt jetzt seine eigene Datei. Ergebnis: `RUNS=16` → **16 von 16** mit identischer
Signatur, `ALL PASS`.

### Was das über Messen sagt

Die ARM-Suite lief bis heute **genau einmal**. Ein roter Lauf war dort von einem echten Befund nicht
zu unterscheiden, ein grüner sagte nichts über den nächsten. Die x86-Suite vergleicht seit B-1.3 die
Ergebnissignatur über `RUNS` Läufe — dieselbe Mechanik ist jetzt auch hier, und zwar **dieselbe** und
keine zweite Fassung davon.

### Was NICHT erledigt ist

In einer Messung dazwischen (gemeinsame Datei) standen 14 von 16, und die beiden Abweichungen waren
**echte** Watchdog-Läufe — diese Zeile kommt vom Kernel, nicht von der Mechanik. 16 saubere Läufe
schließen eine Rate von 12,5 % nicht aus (P ≈ 12 %). Der Hänger bleibt offen; er ist jetzt nur
erstmals messbar, und bei Abweichung liegt das volle Log in `build/diag/`.

Im ARM-Hänger sind **alle Ergebniszeilen grün** und der Bericht kommt trotzdem aus der Notbremse —
dieselbe Form wie D0 auf x86. Der nächste Schritt ist deshalb derselbe wie dort: die Notbremse muss
sagen, **worauf** sie gewartet hat.

---

## B-7.2. Loom prüfte eine Kopie — und die Kopie war nicht das Problem

**Erledigt 2026-08-02** (parallel bearbeitet, Ergebnis integriert und selbst nachgemessen).

Die Notiz sagte: „Loom modelliert eine *Kopie* des Lock-Algorithmus." Das stimmte —
`Verification/concurrency/loom/src/lib.rs` trug in Zeile 2 wörtlich „**GETREUE Kopie** der
Lock-Logik". Zeile für Zeile geprüft waren die Ordnungen **heute** sogar identisch. Das ist aber
kein Trost, sondern der Punkt: **nichts hielt sie zusammen**. Eine geänderte Ordnung im echten Lock
hätte den Beweis nicht gestreift.

Jetzt übernimmt `tools/loom-verify.sh` `crates/caprock-sync/src/lib.rs` **unverändert**; die
Beweise stehen in derselben Datei (`#[cfg(all(loom, test))] mod loom_proofs`), wie die
Kani-Beweise auch. Vier cfg-Schalter genügen: `no_std` nur ohne Loom, Atomics und `UnsafeCell` aus
`loom`, und ein `spin_hint()`, das unter Loom `yield_now()` ist (Looms Threads laufen kooperativ —
eine Schleife ohne Abgabepunkt liefe endlos, statt zu explorieren).

### Der eigentliche Fund war ein anderer

Die Kopie war nicht der Grund, warum der Beweis wenig bewies. Der Grund war `core::cell::UnsafeCell`:

| Mutation im **echten** Lock | mit `core`-Zelle | mit `loom`-Zelle |
|---|---|---|
| `fetch_and(!RW_WRITER)` → `store(0)` | 3 von 6 fallen | 3 von 6 fallen |
| Ticket-Release `Release` → `Relaxed` | **0 — unbemerkt** | **2 fallen** |

Mit `core::cell` prüfte Loom nur das **Atomic-Protokoll**. Eine abgeschwächte Ordnung, die die
Nutzlast nicht mehr veröffentlicht, lief durch alle Beweise. Erst weil `data` unter Loom aus
`loom::cell` kommt, liegt der Zugriff im verfolgten Fenster und wird gegen die Atomics geordnet.

**Selbst nachgemessen**, nicht übernommen: `Ordering::Release` → `Relaxed` in Zeile 317 des echten
Locks → `test result: FAILED. 8 passed; 2 failed`. Danach zurückgesetzt und wieder `10 passed`.

### Was der Beweis weiterhin NICHT abdeckt

* **Die IRQ-Maskierung.** Loom modelliert Threads, keine Unterbrechungen; ein Interrupt mitten im
  kritischen Abschnitt **desselben** Kerns ist kein Thread-Interleaving. Unter Loom läuft das
  Host-Ziel, dort ist `IRQ_MASKING_IMPLEMENTED == false`. Der reentrante Ticket-Deadlock (der
  x86-Fehler vom 2026-07-29) wird allein von der Übersetzungszeit-Zusicherung `target_os = "none"`
  gehalten.
* **`Deref`/`DerefMut` sind zweigeteilt** — der Kernel dereferenziert einen `*mut T`, Loom einen
  verfolgten Griff. Das *Protokoll* ist geprüft, die zwei Zeilen Zeigerarithmetik nicht. Kleiner
  Rest derselben Fehlerform, aber benannt.
* **Looms Speichermodell ist C11**, nicht aarch64/x86.

### Die tote Kopie ist gelöscht

`Verification/concurrency/loom/src/{lib,ticket}.rs` sind weg. Bleiben sie liegen, steht genau die
Kopie weiter im Baum, die dieser Punkt beseitigt hat — und sieht dabei autoritativ aus. In
`docs/adr/0023` steht jetzt ein Vorspann, der den überholten Zustand als überholt markiert, statt
ihn stillschweigend falsch werden zu lassen. `hierarchy.rs`/`crosscore.rs` bleiben: die modellieren
**andere** Gegenstände (globale Sperrordnung, Cross-Core-IPC), keine Nachbildungen.

---

## D6, Teil 2. Die ARM-Notbremse zeigte den Stand von vor 35 Sekunden

**Erledigt 2026-08-02.**

Die `DBG pending`-Zeile lief genau **einmal**, nach ~25 s. Im beobachteten Hänger ist das zu früh:
dort sind am Ende **alle** Ergebniszeilen grün, der Bericht kommt trotzdem aus der Notbremse, und
der 25-s-Schnappschuss zeigt lauter Tests, die danach noch fertig wurden. Damit war nicht zu sagen,
welche Bedingung bei Ablauf offen war.

Jetzt läuft sie ein zweites Mal, kurz **vor** der Deadline. Das ist das ARM-Gegenstück zu dem, was
die x86-Notbremse seit heute tut (`bringup : offen waren: …`) — und dort war der Befund damit in
einer Zeile sichtbar.

---

## B-7.1. Die Notiz war überholt — die Lücke lag woanders

**Erledigt 2026-08-02** (parallel bearbeitet, integriert und selbst nachgemessen).

Die Notiz sagte: „Kani läuft nur im CI-Gate; die ext-29-Änderung an `caprock-sync` ist dort nicht
abgedeckt." Gemessen stimmte das **nicht**: `tools/kani-verify.sh` führt `sync` seit Längerem als
Ziel, und die CI ruft das Skript **ohne Argumente**, also mit allen vier Zielen.

**Woher die Notiz kam, ist trotzdem sichtbar** — und das ist der lehrreiche Teil: der CI-Job hieß
*„Kani — Tier-1-Beweise (Loader-Parser)"*, sein Kopfkommentar nannte nur `cert/archive/elf`. Wer die
CI liest statt das Skript, muss schließen, `sync` sei ungegatet. Dieselbe Fehlerform wie B-7.2, eine
Ebene höher: **eine Beschreibung, die neben der Sache herläuft** — und die dann als „Befund" in eine
Aufgabenliste wandert und dort Arbeit erzeugt, die es nicht braucht. Die Beschriftung ist korrigiert.

### Die echte Lücke: konkrete Werte statt eines Zustandsraums

Die drei vorhandenen Beweise liefen mit **konkreten** Werten — ein Leser, zwei Leser, ein Schreiber.
Das ist dieselbe Größenordnung, die Loom seit B-7.2 über Interleavings abdeckt. Über den
Zustandsraum, in dem die Zusicherungen des Zustandsworts tatsächlich leben, sagte **keines** der
beiden Werkzeuge etwas:

| neuer Beweis | Eigenschaft, die nur Kani sieht |
|---|---|
| Release bewahrt beliebige Leserzahl | genau die Zusicherung, für die dort `fetch_and(!RW_WRITER)` steht und nicht `store(0)` |
| Leser setzt nie das Writer-Bit | für **jede** Leserzahl unter der Grenze (2^31 Zustände) |
| Leserzahl-Grenze ist scharf | die Grenze ist **erreichbar** — damit die Annahme des vorigen nicht leer ist |
| Ticketüberlauf ist unschädlich | von **beliebigem** Ticketstand, auch am `u32`-Wrap |
| `writers_waiting` bleibt ausgeglichen | eine Drift um eins wäre eine dauerhafte Lesersperre, sichtbar erst weit weg von der Ursache |

`sync` steht damit bei **8 statt 3** Beweisen; alle vier Ziele zusammen: 7 + 6 + 8 + 7, 0 failures.

### Zwei Werkzeuge, dieselbe Zeile, verschiedene Richtungen

`store(0)` statt `fetch_and(!RW_WRITER)` kostet unter **Loom** 3 von 6 Beweisen, unter **Kani** 1 von
8. Das ist kein Widerspruch, sondern der Ertrag: Loom zeigt, dass *ein* Interleaving die Zahl 2
verfehlt; Kani zeigt, dass die Leserzahl für **jeden** der 2^31 möglichen Werte verlorengeht.
Selbst nachgemessen: **7 verified, 1 failure**, danach wieder 8/0.

### Der Inert-Check deckte nur eine Crate ab

Er soll belegen, dass `cfg(kani)` den Normalbau nicht bricht — und kopierte dafür ausschließlich
`caprock-loader`. `caprock-sync` trägt seit B-7.1/B-7.2 **zwei** externe cfgs (`kani` *und*
`loom`). Der Kernel-Build fängt das mit ab, aber nicht als benannte Zusicherung, und ein Schutz, den
niemand ausspricht, fällt beim nächsten Umbau unbemerkt weg. Jetzt ist er dabei; lokal
nachgestellt: `Finished`.

### Was weiterhin NICHT abgedeckt ist

* **Die IRQ-Maskierung — auch von Kani nicht.** Kani läuft auf dem Host-Target, dort ist
  `IRQ_MASKING_IMPLEMENTED == false` und die Maskierung ein No-Op. Der x86-Fehler vom 2026-07-29 war
  ein **`cfg`-Auswahlfehler**; weder Kani noch Loom kann ihn sehen, weil beide ein Ziel bauen, auf
  dem der No-Op-Zweig richtig ist. Gehalten wird das allein von der Übersetzungszeit-Zusicherung
  `#[cfg(target_os = "none")] const _: () = assert!(IRQ_MASKING_IMPLEMENTED, …)` — ein
  *Compile*-Gate, kein Beweis-Gate. Genau so steht es jetzt auch im Code.
* **Die Leserzahl-Grenze ist eine Grenze, keine Unmöglichkeit:** bei `RW_WRITER - 1` gleichzeitigen
  Lesern kippt die Eigenschaft (bewiesen). Praktisch unerreichbar, aber jetzt benannt statt
  stillschweigend vorausgesetzt.
* Kani bleibt **single-threaded**; echte Gleichzeitigkeit deckt Loom ab.

---

## B-5.5. Der Prüfer war begrenzt, `revoke` nicht

**Erledigt 2026-08-02** (parallel bearbeitet, integriert; Kernel-Seite hier ergänzt).

### Der Befund stand genau verkehrt herum

| Stelle | begrenzt? |
|---|---|
| `audit_cdt`, Geschwister- und Elternkette | **ja** (`steps > nslots`) |
| `revoke`, Abstieg zum Blatt | **nein** |
| `revoke`, äußere Löschschleife | **nein** |
| `move_cap`, Kinder umhängen | **nein** |
| `child_count` | **nein** |

Ausgerechnet der **Prüfer** war gegen einen zyklischen CDT geschützt. Er läuft auf Anforderung.
`revoke` läuft auf **Mandantenwunsch** — und unter der CAPS-Sperre. Aus einer Datenstrukturanomalie
wäre damit kein Latenzproblem geworden, sondern ein stehender Knoten.

Dazu: ein Kindindex außerhalb der Tabelle war ein Kernel-Panic aus Mandantendaten (`slots[i]` statt
`slots.get(i)`).

### Die Schranke ist hergeleitet, nicht gegriffen

Ein azyklischer Lauf besucht keinen Slot zweimal — also `slots.len()`. Das ist dieselbe Zahl, die
`audit_cdt` seit jeher nimmt; sie steht jetzt als **eine** benannte Quelle da statt zweimal
eingestreut.

Ein Überlauf **bricht ab und wird gezählt**, statt stillschweigend abzuschneiden: ein unvollständiges
`revoke` lässt Abkömmlinge am Leben, die weg sein sollten. Das ist kein Latenzbefund, sondern ein
Autoritätsbefund — deshalb prüft der Kernel ihn jetzt als `cdt_audit`-**Code 9**. Ohne diese Prüfung
wäre die Zählung vorhanden und die Prüfung nicht; genau der Zustand, den dieselbe Datei bei A-3.3
schon einmal hatte.

### Als Operationszahl, nicht als Zeit

    cdtlen : Abstieg 1/80256 Schritte, Revoke 4/80256 Loeschungen

Erst der **Abstand zur Schranke** macht „begrenzte kritische Sektion" zu einer Aussage. Zeit wäre
das falsche Maß: sie hängt an Taktrate und Emulation und sagt auf Blech, unter KVM und unter TCG
jeweils etwas anderes.

### Sensitivität — und warum ein Test „terminiert" heißt

| Mutation an der echten Schranke | Ergebnis |
|---|---|
| `steps > limit` → `steps > limit * 4` | 5 passed, 1 failed |
| Prüfung **entfernt** (Zustand vor B-5.5) | der Test „terminiert auf einem Zyklus" **hängt**, nach 60 s abgebrochen |
| unverändert | 6 passed, 0 failed |

Der zweite Fall ist der eigentliche Beleg. **Dass der Test überhaupt zurückkehrt, ist das Ergebnis** —
ohne Schranke läuft dieselbe Schleife endlos, im Kernel unter der CAPS-Sperre.

### Nebenertrag: sechs Tests, die nirgends liefen

`caprock-cap` hatte **keinen** Host-Test-Pfad. `cargo test -p caprock-cap` scheitert am
erzwungenen Custom-Target (`build-std`), und kein Skript baute die Crate außerhalb des Workspace.
Die sechs neuen Tests wären also entstanden und nie gelaufen.

`tools/host-tests.sh` sammelt jetzt alle reinen Crates an einem Ort — abhängigkeitsfreie über
`rustc --test` (Sekunden), solche mit Abhängigkeiten über ein Wegwerf-Projekt im TMPDIR:

    mem 22 · part 14 · fat 20 · cap 6   →  == HOST-TESTS: ALL PASS ==

Ein Test, der nirgends läuft, ist kein Test, sondern eine Absichtserklärung.

### Offen gelassen, benannt

`revoke` gibt bei Überlauf weiterhin `Ok(())`. Ein eigener `CapError` wäre die stärkere Antwort und
wäre machbar (der Kernel matcht nirgends erschöpfend auf `CapError`) — aber das ändert eine
öffentliche Aufzählung und ist eine Entscheidung für den Kernel, nicht für eine Crate-Änderung.
Bis dahin trägt Code 9 die Aussage.

---

## B-5.1. Die Abrechnung hing am Tick

**Der Zustand vorher war schlimmer als die Notiz vermutete.** `grep cycles` in
`crates/caprock-sched/`: kein Treffer. Belastet wurde ausschliesslich in `on_tick`, und auch dort
nur bei `tick == true`; `block_current`, `switch_to` und der YIELD-Pfad rechneten **gar nichts** ab.
Die Verzerrung war damit nicht „bis zu 10 ms je Umplanung", sondern **vollstaendig**: ein Thread,
der 9,9 ms rechnet und dann blockiert, zahlte **null**. Wer das systematisch tut, rechnet dauerhaft
umsonst — und der Nachbar, der zufaellig beim Tick lief, zahlte dessen Anteil mit. Fuer eine Cloud,
die CPU-Zeit verkauft, ist das kein Rundungsfehler, sondern ein Abrechnungsfehler mit Methode.

**Ein schnellerer Tick waere die falsche Antwort** — er erhoeht Aufloesung *und* Overhead. Ein
Stempel beim Ein- und Auslasten erhoeht nur die Aufloesung.

### Der Schnitt: die Uhr gehoert dem Kernel, die Rechnung nicht

Die Arithmetik liegt **abhaengigkeitsfrei** in `crates/caprock-sched/src/cycles.rs` und wird als
Datei auf dem Host geprueft (`tools/host-tests.sh cycles`, 9 Tests). `caprock-sched` als Ganzes
haengt an `caprock-hal` (arch-Asm) und wird auf dem Host nie bauen; laese das Modul die Uhr selbst,
waere jede Falle nur auf einer bestimmten Maschine ausloesbar statt mit einem Literal.

### Die drei Fallen

* **Rueckwaertssprung ist ein Fehler, kein Wrap.** `wrapping_sub` waere die naheliegende Zeile und
  die teuerste: ein Zaehler, der um 100 Zyklen zurueckspringt, ergaebe eine Differenz von rund
  `2^64` — das Konto ist sofort und dauerhaft erschoepft, aus 20 ns Messfehler wird ein Thread, der
  nie wieder laeuft. Aus **einem** Stempelpaar laesst sich Wrap nicht von Ruecksprung trennen; man
  muss sich entscheiden, und die Entscheidung ist eindeutig: 64 Bit bei 5 GHz wrappen nach ~117
  Jahren, ein nicht-invarianter Zaehler springt beim ersten Frequenzwechsel.
* **Plausibilitaetsgrenze** `2^40` (~3 min bei 5 GHz), bewusst grosszuegig: sie soll kaputte Proben
  fangen, nicht lange. Zu eng gezogen verfaelschte sie die Abrechnung nach unten — derselbe Fehler
  wie die Ticks, nur andersherum.
* **Migration.** Der Kern wird **mitgestempelt** und **vor** der Zahl geprueft. Ein Zyklenzaehler ist
  nur innerhalb eines Kerns eine Zeitachse; ueber einen Kernwechsel ist die Differenz keine Dauer,
  sondern eine Zufallszahl.

`Source::Untrusted` ist die Vorgabe: ohne ausdrueckliche Invarianz-Zusage wird **nichts**
abgerechnet, und `CycleStats::measurable()` trennt „hat nicht gerechnet" von „konnte nicht messen".
`consumed == 0` ohne gueltige Probe ist eine Leerstelle, keine Null — dieselbe Form wie die
Sprechprobe der IOMMU-Einheiten.

### Im Kernel: ein Zaehlerstand, nicht zwei

`system::charged()` klammert jede Umplanung (`on_tick` beide Pfade, `block_current`, `switch_to`,
`exit_current`). Es liest die Uhr **einmal**. Zwei Lesungen waeren die naheliegende Fassung und
hinterliessen ein Loch: die Zyklen der Umplanung selbst laegen zwischen den Stempeln und gehoerten
niemandem, die Summe aller Konten waere systematisch kleiner als die verstrichene Zeit — also genau
der Fehler, gegen den B-5.1 antritt, nur kleiner. Mit einem Stempel ist die Achse **lueckenlos**
aufgeteilt, und das ist nachpruefbar. Der Preis ist benannt: die Kosten der Umplanung traegt der
ankommende Thread. Das ist eine Zuordnungsentscheidung, keine Messungenauigkeit.

### Die Pruefung — und ein Loch im ersten Entwurf

```
cycacct : ALL PASS -- B-5.1: 63 Zyklenproben gegen 39 Ticks (1127396473 Zyklen verbucht;
          verworfen: rueckwaerts=0 unplausibel=0 Kernwechsel=0 Quelle-nicht-zugesichert=0;
          Maschine invariant=true)
```

`Proben > Ticks` **ist** die Aussage: die Tick-Rechnung kann hoechstens einmal je Tick belasten,
jede weitere Probe ist Rechenzeit, die vorher niemand zahlte.

Der erste Entwurf waehlte den Zweig danach, ob `rejected_source == 0` war. Damit haette ein
**vergessenes `set_cycle_source`** den Test bestanden: alles landete im Ablehnungszaehler, der
Untrusted-Zweig war erfuellt, und der Bericht meldete gruen fuer einen Kernel, der gar nicht
abrechnet. Die Sollgroesse muss von aussen kommen — jetzt entscheidet `hal::timer::invariant_tsc()`,
was die Maschine kann, und der Kernel muss sich daran messen lassen.

### Sensitivitaet (echte Mutationen, x86-Suite)

| Mutation | Ergebnis |
|---|---|
| M1: `set_cycle_source` entfernt | **FAILURES** — 0 Proben, `Quelle-nicht-zugesichert=86`, Maschine invariant=true |
| M2: Klammerung nur noch am Tick (`block_current`/`switch_to`/YIELD roh) | **FAILURES** — **39 Proben gegen 47 Ticks** |
| M3: `stamp_current` entfernt | **FAILURES** — 0 Proben, 0 Zyklen |
| unveraendert | ALL PASS |

M2 ist der aussagekraeftigste: weniger Abrechnungsereignisse als Ticks — genau der Zustand vorher.

### Offen

`consumed_cycles(tid)` liefert die Zahl, aber es gibt noch **keine Monitoring-Cap**, ueber die ein
Mandant sie abfragen kann; das gehoert zu B-6.1. Und `next_refill` bleibt tickbasiert: stellt B-5.2
die Verdraengung auf Zyklen um, muss es mitwandern, sonst gibt es zwei Zeitachsen fuer dasselbe
Budget.

**Nebenbefund am Werkzeug:** `tools/host-tests.sh` meldete ein **fehlendes** Ziel im selben Wortlaut
wie ein **kaputtes** („liess sich nicht uebersetzen"). In einem unvollstaendigen Baum las sich das
wie ein kaputtes Projekt, und umgekehrt haette sich ein wirklich kaputtes Ziel als „ist halt nicht
da" abtun lassen. Ein fehlendes Ziel ist jetzt ein eigener Ausgang — es faellt durch, sagt aber,
warum.

---

## D5. Eine rote Zeile, die niemand ansah

Der aarch64-Kernel meldete `root : FAILURES (NoManifest)`, und `test-qemu.sh` prueft diese Zeile
**gar nicht**. Ein Urteil, das niemand ansieht, unterscheidet nicht mehr zwischen „wie immer" und
„gerade gebrochen" — derselbe Fehler wie `-no-shutdown` (rc war immer 124), nur mit umgekehrtem
Vorzeichen.

**Behoben:** `test-qemu.sh` erzeugt den Manifest-Schluessel **vor** dem Build (die oeffentliche
Haelfte wird in `kernel/src/manifest_keys.rs` einkompiliert), signiert `init` fuer aarch64
(`certs/init-arm.cert` — das x86-Zertifikat traegt hier nicht, anderer Hash), baut ein Manifest mit
genau einem `root`-Eintrag und legt `init` als **erstes** Archivmodul ab. `main.rs` ruft
`manifest_report()` wie die x86-Seite. Geprueft werden jetzt `archive : 11 Modul`,
`manifest : ALL PASS` und `root : ALL PASS`.

### Fund 1: die Startmenge stand im falschen Dokument

`boot_arg` gab dem Root-Task `archive.count()`. Das Archiv ist ein **Behaelter**, das Manifest ist
das **Autoritaetsdokument** — und auf x86 fiel der Unterschied nie auf, weil dasselbe Skript beide
erzeugte. Zwei Zahlen, die uebereinstimmen, weil sie aus derselben Hand kommen.

Auf aarch64 liegen zehn Testdienste im Archiv, die nicht zur Startmenge gehoeren. `init` haette
`count = 11` bekommen und sie als Startmenge geladen: die adversarialen Dienste ein zweites Mal,
und `probe`, das gar kein ELF ist.

Die Zahl kommt jetzt aus dem Manifest. Die dadurch noetige Zusage — der Root-Task spricht die
uebrige Startmenge ueber einen **Archivindex** an (`SYS_LOAD`), bekommt ihre **Groesse** aber aus
dem Manifest, also muessen die Manifest-Eintraege `0..n` auf den Archivpositionen `0..n` liegen —
ist eine **gepruefte Regel** und keine Gewohnheit: `RootTaskError::StartSetNotPrefix`, fail-closed.

Negativkontrolle: `init` an Archivposition 1 statt 0 →
`root : FAILURES (StartSetNotPrefix)`.

### Fund 2: `loadstop` mass eine globale Baseline mit einer lokalen Sperre

Der L4-Test vergleicht globale Zaehler (freier Speicher, freie VSpaces, Kernel-Stacks) vor und nach
laden+abbauen — abgesichert mit `local_irq_disable()`, das aber nur **einen** Kern stillstellt.
Sieben andere und der Einsammler laufen weiter.

Solange dort sonst nichts passierte, ging das gut. Mit dem Root-Task passiert etwas: `init` beendet
sich, und seine Freigabe faellt gelegentlich mitten in die Messung. Gemessen:

```
DBG loadstop: loaded=true free 4204597248->4204613632 vsp 4085->4085 kstack 9989->9989
```

**16 KiB mehr** hinterher — kein Leck, ein fremder `free`. Die Zeile meldete trotzdem FAILURES, und
weil `all_done()` auf dem Bestehen dieses Tests besteht, sah es aus wie ein Haenger.

Der Punkt ist allgemeiner als der Root-Task: eine Gleichheitsaussage ueber eine geteilte Groesse
braucht ein Fenster, in dem niemand sonst daran schreibt. Die lokale IRQ-Sperre war nie dieses
Fenster — sie sah nur so aus. Jetzt wird erst **Ruhe festgestellt** (zwei gleiche Proben in Folge)
und dann gemessen; ein Lauf, der nie ruhig wird, faellt **durch** und sagt das auch so („nicht
messbar" ist kein bestandener Test). Ein echtes Leck faellt weiterhin durch — es steht in jeder
Wiederholung in den Zahlen.

### Nachgemessen, nicht angenommen

`todo.md` hatte ausdruecklich gewarnt: ein laufender Root-Task ist ein zusaetzlicher Thread.
`RUNS=6 ./test-qemu.sh` → **6 von 6** mit identischer Ergebnissignatur, `== ALL PASS ==`.
x86-Suite und x86-Lade-Suite ebenfalls `== ALL PASS ==` (beide fahren denselben geaenderten
`loader.rs`-Pfad).

---

## A-5.3. Die Zuteilung stand im Enumerator, nicht im Manifest

Bis A-5.2 sagte das Manifest ueber Geraete-Autoritaet nur `mmio,dma` -- „diese Komponente darf ein
Registerfenster und eine DMA-Region halten". **Welches** Geraet das ist, entschied der Kernel, und
zwar nach Fundreihenfolge. Der Kommentar an `DRIVER_DEVICE` sagte das offen und zog die richtige
Konsequenz: es wurde **genau eines** angeboten, weil jede Auswahl unter mehreren eine im Kernel
versteckte Politik gewesen waere.

A-5.3 nimmt dieser Begruendung die Grundlage, statt sie zu umgehen.

### Der Selektor

Der Manifest-Eintrag traegt jetzt `vendor`/`device`/`class` (je einzeln „beliebig") in **8 der 12
reservierten Bytes**. `ENTRY_LEN` bleibt 96 -- das Format ist eingefroren, und jedes bestehende
signierte Manifest bleibt gueltig.

**Die Rueckwaertsfalle wurde ausdruecklich gestellt und entschaerft.** Ein vor A-5.3 erzeugtes
Manifest hat dort Nullen. Waeren die als `vendor=0 device=0 class=0` gelesen worden, passte der
Selektor auf **kein** Geraet -- die Formaterweiterung haette jedem alten Dokument still die
Geraete-Zuteilung weggenommen, und zwar mit derselben Meldung wie ein echter Fehlgriff. Null heisst
deshalb „nicht gesetzt" und wird auf `ANY` abgebildet; ein Host-Test haelt das fest.

**Keine Instanz.** Kein Bus, kein Geraet, keine Funktion. Ein Manifest, das `00:04.0` nennt, ist an
die Topologie EINER Maschine gebunden und auf der naechsten stillschweigend falsch -- es zeigte
dann auf ein anderes Geraet, nicht auf keines. Der Selektor benennt eine **Art**; welche Instanz
das ist, sieht nur der Kernel, weil nur er enumeriert.

**Fail-closed.** Passt kein Geraet, gibt es keines. Der Rueckfall „nichts passt → nimm irgendeins"
waere die bequeme Zeile und genau die versteckte Politik, gegen die der Selektor antritt, nur eine
Ebene tiefer.

### Die Pruefung -- und warum die naheliegende nichts taugt

„Der Treiber hat ein Geraet bekommen" ist wertlos: die Aussage ist auch dann wahr, wenn er das
falsche bekam. Geprueft wird deshalb dreierlei, und der erste Punkt ist der wichtigste:

1. **Es gab ueberhaupt etwas zu entscheiden.** Bei nur einem angebotenen Geraet trifft jeder
   Selektor dieselbe Wahl -- weniger als zwei Angebote sind ein **SKIP mit Begruendung**, kein PASS.
   Die Lade-Suite startet dafuer jetzt zusaetzlich eine `virtio-net-pci`.
2. Die Zuteilung passt auf den Selektor **des Manifest-Eintrags** (gelesen aus dem Dokument, nicht
   aus dem Kernel-Zustand).
3. **Das nicht gewaehlte Geraet ist noch frei.** Erst das macht aus „es passte" ein „es wurde
   ausgewaehlt".

```
devsel  : ALL PASS (A-5.3: das Manifest verlangt 1af4:1042, zugeteilt wurde RID 0x0018 1af4:1042;
          2 angeboten, 1 vergeben, 1 liegen geblieben)
```

### Zwei Negativfaelle in der Suite, zwei Mutationen im Code

| Fall | Ergebnis |
|---|---|
| Selektor `vendor=dead,device=beef` (unerfuellbar) | **keine Zuteilung**, `0 vergeben` -- kein Ersatzgeraet |
| Selektor `vendor=1af4,device=1041` (die **Netzkarte**) | `zugeteilt wurde RID 0x0020 1af4:1041` -- obwohl das Blockgeraet in der Angebotsliste DAVOR steht |
| Mutation: „nichts passt → nimm irgendeins" | Negativfall 4 **faellt** |
| Mutation: Selektor ignorieren (`ANY` statt `e.device`) | Negativfall 4 **und** 5 fallen |

Negativfall 5 ist der aussagekraeftigste: er trennt „es passte" von „es war ohnehin das erste".

### Ein Fehler unterwegs, der sich lohnte

Der erste Entwurf berichtete direkt nach `start_root_task_reported()` -- und sah `0 Zuteilungen`.
Der Grund: die Funktion **laedt** den Root-Task, sie fuehrt ihn nicht aus. Die Treiber-PD entsteht
erst, wenn `init` selbst laeuft und `SYS_LOAD` ruft. Ein Bericht an dieser Stelle haette einen
funktionierenden Kernel fuer kaputt erklaert.

### Nebenertrag am Werkzeug

`caprock-loader` hat **49** `#[test]`s -- und lief in keinem Skript. Es gibt kein
`.github/workflows/` in diesem Baum, und Kani prueft Beweise, keine `#[test]`s; das ist nicht
dasselbe. Jetzt in `tools/host-tests.sh` (Gesamtstand: mem 22 · part 14 · fat 20 · cycles 9 ·
loader 49 · cap 6).

### Was NICHT belegt ist

Der Kernel findet die *Kandidaten* weiterhin ueber `hal::pcie::find(VIRTIO_VENDOR, …)`, weil die
BAR-Bestimmung durch den virtio-Faehigkeitslauf geht. Die **Auswahl** steht sauber im Manifest, das
**Angebot** noch nicht.

Und: es laeuft immer nur **eine** Treiber-PD. Dass zwei zugeteilte Geraete **voneinander** isoliert
sind, ist damit nicht belegt -- der Fall, der es widerlegen koennte, kommt gar nicht vor. Dieselbe
Form wie `virtio-rng` vor A-5.2. Steht als **A-5.4** in `todo-A-ausfuehren.md`.

---

## B-4.5 (Teil 2). Der Prime+Probe war nicht reproduzierbar — und die Ursache war NICHT der Allokator

`todo-B-verlaesslichkeit.md` notierte als Nebenbedingung: der Aufbau brauche „~50 MiB in 800
gefärbten Einzelregionen" und trage damit „an die Fragmentgrenze des Allokators"; dort sei er
nicht reproduzierbar (54–179 Zyklen bei identischem Aufbau, `balanciert` kippte). **Nachgemessen
am 2026-08-02 stimmt der Befund, die Ursache aber nicht.**

### Was der Aufbau tatsächlich tat (8 Aufbauten in einem Boot, x86 unter KVM)

Der Aufbau war **byte-identisch reproduzierbar**: in allen Läufen dieselben Physadressen
(`vbase=0x3b40000 dbase=0x3b50000 gbase=0x4540000`), dieselbe Fragmentzahl davor (4) und danach
(4), dieselbe freie Summe. Der Fragment-Höchststand lag bei **419 von `MAX_FRAGMENTS = 1024`**,
`balanciert` war in **8 von 8** wahr, und jede der 800 Regionen war nach dem Abbau nachweislich
frei (`region_fully_free`, 800 von 800 geprüft).

Die Zahlen streuten trotzdem: `disjunkt` = 147, 141, 141, **45**, 134, 135, 189, 140. Ein
identischer Aufbau kann keine Streuung erzeugen — **die Ursache lag also in der Messung.**
Gefunden wurden drei, alle drei derselben Sorte wie die zwei bereits im Code dokumentierten
Fallen: eine Messung, die nicht messen *kann*, was sie zu messen behauptet.

### Ursache 1: Der Angreifer lief linear — und ein linearer Strom verdrängt nichts

Falle 1 („ein linearer Durchlauf misst den Vorauslader") war nur für das **Opfer** behoben. Für
den Angreifer stand im Code ausdrücklich: „hier genügt ein linearer Lauf: er soll den Cache
*füllen*, nicht ihn messen." Das ist auf jedem Cache mit verdrängungsresistenter Einlagerung
falsch — ein rein sequenzieller Strom ist genau das Muster, das als „kein Wiedergebrauch"
erkannt und an der LRU-Position eingelagert wird. Gemessen, 10 Aufbauten mit identischer
Zuteilung, Angreifer 24 MiB gegen 16 MiB LLC:

| Angreifer | Positivkontrolle trug in |
|---|---|
| linear (bis 2026-08-02) | **3 von 10** |
| bit-umgekehrt | **10 von 10** |

Im nicht tragenden Fall lag `gleichfarbig` bei 43 gegen `ungestoert` 38 Zyklen: der Angreifer war
schlicht wirkungslos. Der Angreifer läuft jetzt in derselben bit-umgekehrten Reihenfolge wie die
Zeigerkette des Opfers — gleich viele Speicherzugriffe, andere Reihenfolge.

### Ursache 2: Das Minimum ist für zwei von drei Messpunkten der falsche Schätzer

Im Code stand: „gewertet wird das Minimum. Störungen können eine Messung nur verlängern, nie
verkürzen." Für `base` stimmt das (reine Latenz). Für `shared` und `disjoint` ist die gesuchte
Größe die **Verdrängung**, und jede Störung, die den Angreifer bremst oder den Arbeitssatz
teilweise stehen lässt, macht die Messung *kürzer*. Das Minimum greift also systematisch in genau
die Richtung, die die Positivkontrolle zerstört. Gemessen (dieselben 10 Aufbauten, 16 Proben):

| Schätzer | Positivkontrolle trug in |
|---|---|
| Minimum | **4 von 10** |
| Median | **10 von 10** |

Gewertet wird jetzt der Median; gemeldet werden **Minimum/Median/Maximum** für alle drei
Messpunkte. Eine Messung, die ihre eigene Streuung nicht kennt, taugt für eine Blech-Aussage
nicht. Dazu kommt ein **Auflösungs-Gate**: überlappen die Verteilungen von `ungestoert` und
`gleichfarbig` (`q3 >= q1`), ist der Medianvergleich eine Zahl ohne Aussage → SKIP mit den
Quartilen, kein Urteil.

**Ehrlich dazu:** die Mutation „wieder Minimum statt Median" fiel nach den beiden anderen
Korrekturen in 6 von 6 Läufen **nicht** mehr durch — mit richtig dimensioniertem Opfer und
gestreutem Angreifer ist auch das Minimum weit über der Schwelle. Der Median bleibt, weil seine
Verzerrung nachgewiesen und einseitig ist, nicht weil er hier noch etwas rettet.

### Ursache 3: „Das Opfer überschreitet den L2" war eine feste Zahl, kein abgeleiteter Wert

Falle 2 war 2026-08-01 mit `VICTIM_BYTES = 2 MiB` und der Begründung „die Messmaschine hat
~1,5 MiB L2 je Kern" behoben — also mit einer Konstanten, die auf genau einer Maschine gilt. Auf
der heutigen Messmaschine meldet die Ebene unter dem LLC **4 MiB**; das 2-MiB-Opfer lag damit
wieder vollständig privat (`ungestoert` = 38–40 Zyklen, L2-Latenz), und der *farblich disjunkte*
Angreifer räumte es von dort heraus — was er darf, denn Färbung partitioniert nur den LLC.
`effect` wäre selbst bei perfekt wirkender Färbung durchgefallen.

Die Opfergröße wird jetzt aus der Geometrie abgeleitet, und zwar gegen **beide** Schranken:

```text
    Größe der nicht partitionierten Ebene   <   Opfer   ≤   LLC / PARTITIONS
```

Dafür ist `hal::cache::below_llc()` neu (x86 aus `CPUID.4`, aarch64 aus `CLIDR_EL1`/`CCSIDR_EL1`,
beide über dieselbe Zerlegung wie `llc()` — eine zweite Fassung wäre die Doppelung, an der die
Farbarithmetik hier schon einmal auseinandergelaufen ist).

**Das Fenster kann leer sein, und auf dieser Maschine ist es das:** 4 MiB private Ebene gegen
16 MiB / 4 Partitionen = 4 MiB Farbanteil. Untergrenze = Obergrenze, kein gültiges Opfer. Das ist
keine Schwäche des Tests, sondern eine **Aussage über die Maschine**, und sie gehört neben A1:
wo eine nicht partitionierte Cache-Ebene so groß ist wie ein ganzer Farbanteil des LLC, kann
Färbung mit dieser Streifenzahl nichts schützen, was nicht ohnehin privat liegt. Der Test meldet
das mit beiden Zahlen, statt ein Urteil zu erfinden.

**Für den noch offenen Blech-Lauf ist das die konkrete Anforderung an die Maschine:**
`L2 < LLC / PARTITIONS`. Auf einem Server-Xeon (1–2 MiB L2, 32+ MiB LLC) ist sie erfüllt; auf
einem hybriden Notebook-Kern mit 4 MiB E-Core-L2 nicht.

### Der Aufbau kommt nicht mehr aus 800 `alloc_colored`-Aufrufen

Die Fragmentierung war **nicht** die Ursache der Streuung — aber die Bauart trug drei stille
Risiken, und sie sind mit demselben Schnitt weg:

* Der Fragment-Höchststand (419) skaliert mit `1/region_bytes`: auf einer Maschine mit 16 statt
  256 Farben wären es viermal so viele, und `MAX_FRAGMENTS` ist 1024.
* `MAX_ATTACKER = 384` war auf der Messmaschine **exakt erreicht**. Jede Maschine mit größerem
  LLC hätte einen stillschweigend zu kleinen Angreifer bekommen — also eine stillschweigend
  geschwächte Positivkontrolle. Der Bericht nennt die Angreifergröße jetzt in Prozent des LLC.
* Die drei Regionslisten lagen als lokale Arrays auf dem Kernel-Stack: **12,8 KiB Rahmen bei
  16 KiB Stack**. Dieselbe Falle, die `ReplyFinal` schon einmal gestellt hat.

Jetzt kommt der Speicher aus wenigen großen, **ungefärbten** Blöcken (8 MiB, auf der Messmaschine
14 Stück), und die farbreinen Läufe werden darin **gesucht** — mit `color_of` und derselben Maske,
die auch der Allokator befragt. Das ist die stärkere Aussage: die Farbe jeder benutzten Seite wird
an der Verwendungsstelle nachgerechnet, statt einem `alloc_colored` geglaubt zu werden. Neu ist
auch eine ausdrückliche Prüfung `farbtreu`: Opfer und gleichfarbiger Angreifer müssen Farben
**teilen**, der disjunkte Angreifer mit keinem von beiden eine — sonst messen die drei Messpunkte
nicht, was ihre Namen sagen.

### Zwei Fehler, die erst die Sensitivitätsprüfung gefunden hat

**Der Bilanzprüfer prüfte nur, was er selbst getan hatte.** Erster Entwurf: `region_fully_free`
innerhalb der Freigabeschleife. Gegenprobe (Schleife um einen Block verkürzt): `bilanz=1`, Lauf
grün, 8 MiB weg. Ein Prüfer, der nur das prüft, was er selbst angefasst hat, kann ein **Vergessen**
nicht sehen. Die geholten Spannen werden jetzt getrennt von den Caps geführt und in einer eigenen
Schleife geprüft; dieselbe Mutation meldet danach
`pprobe : FAILURES (Speicher NICHT vollstaendig zurueck ...)`.

**Ein halbe Sekunde IRQ-Sperre kippt andere Tests.** `SpinLock::lock()` maskiert Interrupts für
die gesamte Lebensdauer des Guards; der Arena-Guard lebt über Aufbau, Messung und Abbau. Mit einem
zwischenzeitlich 8 MiB großen Opfer waren das rund 1,5 s ohne Timer-Tick auf dem Bootkern — der
Lauf endete im `bringup : WATCHDOG` mit `ipc : FAILURES`. Der Prime+Probe hatte einen Test
gekippt, der mit ihm nichts zu tun hat; genau deshalb ist der Farbtest auf aarch64 aus
`spawn_demo` ausgehängt. Zwischen zwei Proben werden die Interrupts jetzt kurz aufgemacht — hier
zulässig, weil `PP_ARENA` von genau einer Stelle genommen wird und kein Interrupt-Pfad sie zieht.

### Und: die Zeile wurde von keiner Suite gelesen

`pprobe` stand seit 2026-08-01 in jedem x86-Log und kam in **keinem** Testskript vor. Seit
2026-08-02 prüft `test-qemu-x86.sh` alle drei Ausgänge — `FAILURES` ist ein FAIL, das **Fehlen**
der Zeile ebenfalls (dann ist der Hochlauf vorher stehengeblieben), und im SKIP-Fall wird
zusätzlich verlangt, dass `farbtreu=1 bilanz=1` gilt: ein SKIP, der über den Aufbau nichts sagt,
wäre eine Aussage über gar nichts.

### Sensitivität (Ausgaben im Wortlaut in der Sitzungsnotiz)

Fünf Mutationen am eigenen Code: vergessene Freigabe → `FAILURES (Speicher NICHT vollstaendig
zurueck)`; „disjunkter" Angreifer aus dem Opferstreifen → `FAILURES (Farbwahl gebrochen)`;
Urteilspfad erzwungen → `FAILURES` (unter KVM schützt die Farbe nicht — richtig so); Urteilspfad
erzwungen **und** disjunkter Angreifer stillgelegt → `ALL PASS` (der Zweig kann beide Ausgänge);
linearer Angreifer → `SKIP -- die Positivkontrolle traegt nicht` in 2 von 6 Läufen, mit wechselndem
Grund von Lauf zu Lauf. Die fünfte (Minimum statt Median) fiel **nicht** durch, s. o.

### Zahlen nachher

10 Boots, x86 unter KVM, identischer Aufbau:
`ungestoert` Median 79–88, `disjunkt` Median 174–271, `gleichfarbig` Median 183–247;
Positivkontrolle in 10 von 10, `farbtreu`/`bilanz` in 10 von 10, **10 von 10 mit identischer
Ergebnissignatur**. Acht Aufbauten in einem Boot: `disjunkt` Median 189–215 (±7 %) gegen vorher
45–189 (Faktor 4,2). `RUNS=5 ./test-qemu-x86.sh` → `== ALL PASS ==`, 5 von 5 identische Signatur.
Bootzeit 0,9 s → 1,3–1,6 s (KVM) bzw. 2,8 s (TCG): der Prime+Probe läuft jetzt bei **jedem** Start
statt nur am Blech-Tag.

**Nachtrag beim Zusammenfuehren (2026-08-02):** die `pprobe`-Zeile lief bis dahin gegen eine
**erfundene** Cache-Geometrie. QEMU meldet unter `-cpu host` ohne `host-cache-info=on` seine
Legacy-Deskriptoren (L3 16 MiB/16-fach, L2 4 MiB) statt der der Maschine. Der SKIP-Grund war
deshalb ein Aufbau-Artefakt ("das Opfer ueberschreitet die private Ebene nicht" -- 4096 gegen
4096 KiB), nicht die Sache. Mit dem Schalter in **beiden** x86-Suiten:

```
cache   : LLC L3 24576 KiB, 12-fach, 64 B/Zeile, 32768 Sets -> 512 Seitenfarbe(n)
color   : ALL PASS (zwei isolierte PDs teilen sich keine Cache-Farbe)
pprobe  : Opfer 4096 KiB (privat 2048, ueber-privat=ja), Angreifer 32768 KiB (133% des LLC)
          ungestoert=79/86/89 disjunkt=242/275/…
pprobe  : SKIP -- unter einem Hypervisor NICHT ENTSCHEIDBAR (CPUID.1:ECX[31]). Die
          Positivkontrolle traegt (gleichfarbig=281 vs ungestoert=86), der disjunkte Farbsatz
          schuetzt NICHT (disjunkt=275).
```

Der Zugewinn ist nicht bloss Genauigkeit: **512 ist keine 256.** Genau daran hing der
A1-Rest-Fehler -- `stripe` rechnete mit `MASK_BITS` statt mit der Farbanzahl und war bei 256
zufaellig richtig. Eine Suite, die nie etwas anderes als 256 Farben sieht, kann diese Klasse von
Fehlern grundsaetzlich nicht finden. `RUNS=3` → 3 von 3 identische Signatur, Lade-Suite ebenfalls
`== ALL PASS ==`.

---

## A1 / B-4.2 / B-4.5 auf aarch64. Drei Tests, die nicht liefen — und die man nicht vermissen konnte

Bis 2026-08-02 setzte `threads::spawn_demo` auf aarch64 `COLOR_OK`, `STRIPE_ALLOC_OK` und
`PPROBE_OK` **hart auf `true`**. Der Grund im Kommentar war echt: an jener Stelle, ganz am Anfang
von `spawn_demo`, belegen und geben die Tests Speicher frei, *bevor* die baseline-empfindlichen
Prüfungen ihre Ausgangswerte nehmen; danach fiel mal `captest`, mal `sched` durch. Der Ausweg war
falsch.

**Warum falsch: es gab keine Zeile.** Kein `color`, kein `stripe`, kein `pprobe` im ARM-Protokoll,
kein Check in `test-qemu.sh` — und drei dauerhaft wahre Konjunkte in `all_done()`. Ein Ausfall der
Färbung auf ARM hätte `== ALL PASS ==` gemeldet. Das ist nicht „ein Test fehlt", das ist „ein Test
besteht immer", und die zweite Form ist die gefährlichere: sie sieht aus wie Erfolg. Ein
weggelassener Test hinterlässt eine Lücke, die jemand bemerken kann; ein hart gesetztes `true`
hinterlässt eine Zusicherung, an die man sich gewöhnt.

### Die Lösung ist der Platz, nicht das Weglassen

Die drei Tests hängen jetzt als **letztes Glied der Testkette** in `demo_report_then_idle`
(`run_color_suite`), gegatet auf `cross` (letzter adversarialer Dienst), `strand` (letzte
Scheduler-Messung) und `loadstop` (Ressourcen-Bilanz). Danach nimmt keine Prüfung mehr eine
Baseline; was der Farbtest an der Freiliste anrichtet, kann niemanden mehr kippen. Der
Gate-Ausdruck steht als Zusicherung im Code: wer eine baseline-empfindliche Prüfung dahinter
einhängt, muss ihn ergänzen.

**Die Ruhephase aus D5 (`loadstop_quiet`) wird gemessen, aber NICHT als Tor benutzt** — und das
ist der interessantere Teil. `run_loadstop` braucht sie, weil es eine Gleichheitsaussage über
**globale** Zähler trifft; dort entscheidet ein fremder `free` im Messfenster über PASS oder FAIL.
Die Farbtests treffen diese Aussage nicht: sie prüfen ihre Bilanz **regionsgenau**
(`region_fully_free` je geholter Region), seit A1 diese Falle einmal bezahlt hat. Damit ist die
Ruhephase hier eine Diagnose: findet sich nach der gesamten Testkette *kein* ruhiges Fenster,
werkelt dort noch etwas, obwohl alles fertig gemeldet hat — das gehört ins Protokoll. Ein Tor
daraus zu machen wäre verkehrt: der Test schwiege dann ausgerechnet dort, wo etwas nicht stimmt.

### Was dabei herauskam — A1 gilt auf aarch64

    cache   : LLC L2 1024 KiB, 16-fach, 64 B/Zeile, 1024 Sets -> 16 Seitenfarbe(n)
    color   : 16 Farben, 4 Partitionen, Region 16 KiB · in_mask=1 kernelseite=1 … disjunkt=1
              uebergross_abgewiesen=1 bilanz=1
    color   : ALL PASS
    stripe  : ALL PASS

Das ist **die 16-Farben-Aufteilung** — genau der Fall, den die `MASK_BITS`-Verwechslung falsch
machte (Streifen 0 bekam alle 16 Farben, die Streifen 1–3 keine, und weil leere Mengen sich nicht
schneiden, meldete der Selbsttest „disjunkt"). Der Test, der den Fehler gefunden hat, belegt jetzt
seine Behebung auf derselben Maschine.

### Zwei Messfehler, die erst der ARM-Lauf sichtbar gemacht hat

**1. Die Uhr war zu grob, und der Test hat es nicht gemerkt.** `chase` meldete Zyklen **je
Kettenglied** (`dt / n`). Auf aarch64 ist `cycles()` `CNTPCT_EL0` — ein architektonischer Zähler
mit grober Granularität. Bei 4096 Gliedern ergab die Ganzzahldivision **0**:

    pprobe  : … ungestoert=0/0/0 disjunkt=0/0/1 gleichfarbig=0/0/1
    pprobe  : SKIP -- nicht aufloesbar: die Verteilungen … ueberlappen (q3=0 >= q1=0)

Das Auflösungs-Gate hat den Fall gefangen — mit der falschen Begründung. Gemessen wurde die
Granularität des Zählers, nicht der Cache. `chase` meldet jetzt die **Gesamtdauer**; alle
Vergleiche des Tests sind Verhältnisse und gelten dafür unverändert, nur mit `n`-mal mehr
Auflösung. Die vertraute Zahl „Zyklen je Glied" steht weiter im Bericht, mit einer Nachkommastelle
von Hand (`je_glied`) — `0` wäre dort keine Messgröße, sondern eine verlorene Aussage. Dazu ein
eigener Ausgang `clock_ok`: tickt der Zähler während eines ungestörten Durchlaufs überhaupt nicht,
sagt der Bericht das als **Befund über die Uhr**, statt es unter „nicht auflösbar" zu verbuchen.

**2. `run_stripe_alloc` wäre auf einer Maschine mit einer Farbe durchgefallen.** Seit `stripe` die
Farbanzahl kennt, liefert es unter zwei Farben korrekt `None` — der Test las das als „nur 0 von 4
Streifen vergebbar" und meldete FAILURES. Das wäre ein Fehlschlag über die *Maschine* gewesen, nicht
über die Belegungsführung, die dort geprüft wird. Jetzt SKIP mit Grund, dieselbe Trennung wie in
`run_color`: nicht durchführbar ist nicht durchgefallen.

### `pprobe` auf aarch64: wohlgestellt, und trotzdem SKIP

    pprobe  : Opfer 256 KiB (privat 32, ueber-privat=ja), Angreifer 1536 KiB je Farbsatz (150% des
              LLC), 1 Bloecke, Kette 4096 Glieder, 9 Proben · Zyklen je Durchlauf (min/median/max):
              ungestoert=2562/2611/5140 disjunkt=3447/3602/4845 gleichfarbig=2903/3032/5413
              · je Glied (Median): 0.6/0.8/0.7 · farbtreu=1 bilanz=1
    pprobe  : SKIP -- die Positivkontrolle traegt nicht (Median gleichfarbig=3032 vs
              ungestoert=2611)

Das Opfergrößen-Fenster ist auf ARM **nicht leer** (32 KiB private Ebene gegen 256 KiB Farbanteil
des 1-MiB-LLC) — anders als unter QEMU/x86, wo QEMUs synthetische Cache-Deskriptoren es zuklappen.
Der Test ist hier also wohlgestellt und scheitert an der einzigen Stelle, an der er scheitern
darf: TCG hat keinen echten Cache, die Positivkontrolle kann nicht tragen, und über den disjunkten
Fall ist damit nichts auszusagen. Aufbau, Farbwahl (`farbtreu=1`) und Bilanz (`bilanz=1`) sind
trotzdem geprüft — und `test-qemu.sh` verlangt genau das auch im SKIP-Fall: ein SKIP, der über den
Aufbau nichts sagt, wäre eine Aussage über gar nichts.

### Gemessen, nicht angenommen

8 Läufe `./test-qemu.sh` (der committete Stand kennt kein `RUNS=`, deshalb einzeln gefahren und
die Signaturen verglichen): **8 von 8 `== ALL PASS ==` mit identischer Ergebnissignatur**,
`color : ALL PASS`, `stripe : ALL PASS`, `pprobe : SKIP` in jedem Lauf — und **`captest` und
`sched` in allen acht Läufen unverändert grün**. Das war die eigentliche Bedingung: ein Test, der
andere Tests kippt, macht das gesamte Ergebnis unbrauchbar, und das wiegt schwerer als drei
zusätzliche grüne Zeilen.

---

---

## B-6.2: die Fehlerdomaene — und ein Panic, der nicht den Knoten reisst, sondern verschluckt wird

**Erledigt 2026-08-02.** Ergebnis: [docs/fehlerdomaene.md](docs/fehlerdomaene.md) (betreiberseitig),
Invariante §14 in [docs/invariants.md](docs/invariants.md).

Die **Festlegung** ist die billige Variante aus Z9, bewusst: **der Knoten ist die Fehlerdomäne,
Redundanz wird über Knoten gebaut.** Dazu die zweite Hälfte, die im Eintrag nicht stand: **alle PDs
im globalen SAS-Adressraum (`VSPACE_OF == 0`) bilden untereinander EINE Domäne.** Ihre Trennung
ruht auf intralingualer Sicherheit (Zertifikats-Gate ADR 0014) und nicht auf der Hardware —
gemessen ist das der Kontrast, den `iso`/`vspace` ohnehin schon prüften: der SAS-Faden **liest** die
fremde Probe-Adresse erfolgreich, die isolierte PD faultet an derselben. Damit ist „kein Kundencode
in TrustedSAS" keine Vorsichtsmaßnahme mehr, sondern die Konsequenz einer benannten Fehlerdomäne.

**Beim Aufschreiben fiel eine Ungenauigkeit auf, die man leicht übernimmt:** „TrustedSAS teilt
einen Adressraum" stimmt so nicht. `Domain::TrustedSas` ist eine Vertrauens*stufe* und darf global
**oder** isoliert laufen (`domain_audit` verlangt Isolation nur für `HardwareLand`/`UserLand`,
`crates/caprock-microkit/src/lib.rs:246–250`); **extern geladene** TrustedSAS-PDs bekommen heute
sogar immer eine eigene VSpace (`kernel/src/loader.rs:719–722`). Die Fehlerdomäne hängt also am
**Adressraum**, nicht am Etikett — und eine Zusicherung, die das Etikett nennt, wäre in beide
Richtungen falsch: zu streng für geladene Trusted-Dienste, zu lasch, falls je etwas anderes global
laufen dürfte.

### Die Prämisse des Todo-Eintrags war falsch — und die Wahrheit ist unangenehmer

Der Eintrag sagte: *„Ein Panic reißt heute den Knoten mit."* Gemessen (QEMU, x86_64 `-smp 4` unter
KVM, aarch64 `-smp 8`, je ein absichtlich ausgelöster Panic an definierter Stelle) stimmt das für
die **häufigsten** Fälle nicht:

* **Panic in einem Kernelfaden (EL1/Ring 0), Bootkern.** Der Lauf läuft **61 s weiter**, alle vier
  Kerne ticken (6102/6088/6084/6080), die gesunden Worker erreichen ~4900 Runden, der gepanickte
  steht bei 2 — `sched : Worker-Runden [4908, 4910, 2]`. `ipc`, `ring3`, `iommu`, `dmatok`,
  `quiesce`, `rebind`, `state`, `capsz` im selben Lauf: **ALL PASS**. **Auf aarch64 dasselbe**
  (8 Kerne, 6004–7006 Ticks, `worker 0 count=106773`, `worker 1 count=107023`, `worker 2 count=2`).
* **Panic auf einem Sekundärkern.** Prüfsignatur **Zeile für Zeile identisch** zum sauberen Lauf
  (31 Ergebniszeilen, `diff` leer), `== SELFTEST COMPLETE ==`, `rc=0`. Der gepanickte Kern tickt
  danach weiter (`sched : core 1 ticks=11`). **Ohne die Konsolenzeile wäre der Panic durch nichts
  nachweisbar.**

Die Ursache steht in zwei Zeilen: `panic.rs` ruft ein `halt()`, das die Interrupts **nicht
maskiert** — x86 `kernel/src/arch/x86_64/mod.rs:294` ist `loop { hlt }` **ohne `cli`** (die
HAL-Fassung `crates/caprock-hal/src/x86_64/cpu.rs:257` würde maskieren, der Panic-Pfad benutzt sie
nicht), aarch64 `loop { wfe }` mit unverändertem DAIF. Der nächste Timer-Tick holt den Kern in den
Scheduler zurück.

**Und für die anderen beiden Fälle stimmt der Satz — dort aber still:**

* **Panic unter gehaltener `MEM`-Sperre**: das Log endet mitten im Hochlauf, kein `smp : ... online`,
  **keine Watchdog-Zeile**, `rc=124`. Der Ticket-Lock (`crates/caprock-sync/src/lib.rs:170–178`)
  hat keine Schranke; `now_serving` steht für immer, und weil es ein *Ticket*-Lock ist, blockiert
  nicht nur der nächste Zieher, sondern jeder.
* **Panic im Steuerfaden des Bootkerns**: der Knoten läuft weiter, meldet aber nie wieder etwas —
  Notbremse und Abschlussbericht hängen an genau diesem Faden (`bringup.rs:1065–1067` sagt das
  selbst). **Von außen nicht von einem Deadlock zu unterscheiden.** Wer einen D0-Hänger untersucht,
  muss zuerst ausschließen, dass er einen Panic vor sich hat.

### Die eigentliche Lücke

Nicht „ein Panic reißt den Knoten", sondern: **derselbe Fehler hat vier verschiedene Ausgänge, und
welcher eintritt, hängt davon ab, WO er auftrat — nicht, wie schlimm er war.** Für ein
Sicherheitsprodukt ist der verschluckte Panic der schlechtere: ein Panic heißt, eine Invariante ist
nachweislich verletzt, und danach vergibt derselbe Kernel weiter Capabilities, teilt Speicher zu
und schaltet Adressräume um. Ein Absturz wäre ein *definierter* Zustand; „läuft weiter mit
verletzter Invariante" ist keiner.

Dieselbe Form wie die leere Event-Queue ohne `CD.R` und wie `virtio-rng`, das nur schreibt: eine
Aussage sah wahr aus, weil der Fall, der sie widerlegt, nie gelaufen war. Hier war der Fall trivial
herstellbar — ein `panic!()` und ein Bootvorgang.

### Doppelfehler: es gibt keinen Wächter

Ein Panic im Panic-Handler rekursiert unbegrenzt. Gemessen **362 Ebenen** auf einem 64-KiB-AP-Stack,
**ohne** `#DF` (Vektor 8 ist in `crates/caprock-hal/src/x86_64/exception.rs:494` nur *benannt*, es
gibt keinen IST-Stack dafür), **ohne** Schutzseite am Kernel-Stack. Der Lauf endete erst, als der
Bootkern die Maschine abschaltete — was jenseits des Stackendes passiert wäre, ist damit **nicht**
gemessen und steht in der Nicht-gemessen-Liste. Sichtbarer Nebeneffekt: die Ausgabe des sterbenden
Kerns verschränkt sich zeichenweise mit der der gesunden, weil `emit_raw` im Panic-Pfad absichtlich
sperrfrei ist.

### Was nicht gebaut wurde, und warum

Billig wären: IRQs im Panic-Pfad maskieren (Minuten), Rekursionswächter im Panic-Handler (Minuten),
`panic` → `system_off` (~1 h; beide Abschaltwege existieren und werden heute **nur** aus
Testabschlusspfaden gerufen, nie aus `panic.rs`). Die drei zusammen machen die Festlegung wahr,
statt sie zu behaupten. Sie sind aber eine **Entscheidung**, keine Reparatur: sie tauschen
Verfügbarkeit gegen Ehrlichkeit, und heute ist der Knoten in den Fällen C/D nachweislich
*verfügbarer* als die Zusicherung verlangt. Diese Entscheidung gehört nicht in einen Nebensatz.
Aufwände und die weiteren Punkte (Schranke im Ticket-Lock — zieht Loom/Kani nach sich; `#DF` mit
IST; Schutzseiten; Panic-IPI, der ohne NMI genau den Fall verfehlt, den er treffen soll) in
`docs/fehlerdomaene.md` §6.

### Der ehrliche VM-Vergleich steht jetzt da, wo ein Betreiber ihn liest

Ein Wirt mit 100 VMs verliert bei einem **Gastkern**-Panic einen Gast; ein Caprock-Knoten hat gar
keine Gastkernschicht — was ein Gastkern täte, tut der geteilte Kern. Der Handel in einem Satz: eine
VM-Plattform hat *viele große* Fehlerdomänen (Größenordnung 10⁷ LOC je Mandant), Caprock hat *eine
kleine* (~18 kLOC `kernel/src`, ~36 kLOC mit `crates/`). Weniger Code kann ausfallen — aber wenn er
ausfällt, fällt alles aus. Daraus zwei betriebliche Sätze, die vorher nirgends standen:
**ein Knoten ist keine Redundanzeinheit** (zwei Repliken auf einem Knoten sind eine), und
**Wartung ist knotengranular**, solange es keine Live-Migration einzelner PDs gibt (Z3/Z4).

### Nicht geprüft (steht auch im Dokument)

Blech (alles lief unter QEMU); der Stacküberlauf jenseits Ebene 362; Panic unter `CAPS` (gemessen
wurde `MEM`); Panic in einem Interrupt-Handler; die aarch64-Fassung der Fälle E/F/G (nur C wurde
dort gegengeprüft); ob ein Panic die IOMMU-Tabellen in einem Zwischenzustand hinterlässt. Eine
Fehlerdomäne auf ungeprüften Annahmen ist schlimmer als keine — deshalb ist die Liste Teil der
Zusicherung, nicht ihr Anhang.

---

## A-5.4 (Teil 1). Zwei Treiber-PDs — und vier versteckte Politiken, die dabei herausfielen

**Was fehlte.** A-5.3 belegt, dass *eine* Treiber-PD das *benannte* Geraet bekommt. Nicht belegt
war, dass zwei zugeteilte Geraete **voneinander** getrennt sind — denn es lief immer nur eine.
Dieselbe Form wie `virtio-rng` vor A-5.2: eine Aussage sieht wahr aus, weil der Gegenbeweis nie
laeuft.

**Der zweite Treiber** (`programs/hardware/virtio-net`) ist bewusst klein. Er faehrt kein Netzwerk;
er holt sein Geraet, faehrt EINE echte ARP-Transaktion und meldet, was er dabei gesehen hat. Die
Transaktion ist die **Positivkontrolle** fuer den noch offenen Teil 2 — ohne sie hiesse "der
Fremdzugriff kam nicht an" nur, dass ueberhaupt nichts lief.

### Vier Stellen, die "der eine Treiber" meinten

Der Umbau war kein Aufwand nebenbei, sondern der eigentliche Ertrag. Bei EINEM Treiber war jede
dieser Stellen richtig; beim zweiten wurde jede zu einer **stillen Fehlwahl** — mit gueltigen Caps
und ohne eine einzige Fehlermeldung:

| Stelle | war | ist |
|---|---|---|
| `DriverAssign`-Suche | „die erste benutzte" (4×) | Schluessel `program_id` aus dem Manifest |
| `DRIVER_SERVICE` | ein Platz, „der zuletzt geladene" | Liste, `driver_service_of(id)` |
| `DRIVER_NTFN` | ein Register | die Notification **dieses** Dienstes |
| geteilte Uebertragungsflaeche | „die eine" | die des **benannten** Dienstes |

Der Reihe nach aufgetreten, und jede war im Log als etwas anderes sichtbar: der Blockdienst
antwortete nicht (`Status=18446744073709551615`), der Austausch meldete `kein Dienst`, `v2 meldete
bereit=0` bei einem Austausch, der sauber gelaufen war, und die Dateisystem-PD verschwand
ersatzlos aus dem Bericht.

**Die Regel, die daraus wurde:** wo eine Wahl mehrdeutig waere, wird **abgewiesen statt geraten**.
`driver_service()` liefert bei zwei Diensten `None` — genau daran ist der Kernel-Testclient beim
ersten Lauf aufgelaufen, was richtig war.

### Der Client benennt seinen Dienst

Die letzten 4 reservierten Bytes des Manifest-Eintrags tragen jetzt `service_id` (A-5.4). Damit ist
die in A-5.3 offen benannte Luecke zu: „gib mir einen Endpoint" hiess bei zwei Diensten „gib mir
irgendeinen", und welchen, entschied die Ladereihenfolge. `0` bleibt zulaessig und heisst „es gibt
ohnehin nur einen"; gibt es mehrere, wird abgewiesen.

### Gemessen

```
devsel  : Eintrag 3 verlangt 1af4:1042, bekam RID 0x0018 1af4:1042
devsel  : Eintrag 5 verlangt 1af4:1041, bekam RID 0x0020 1af4:1041
devsel  : ALL PASS (2 angeboten, 2 vergeben, 0 frei. JEDE Zuteilung wurde gegen den Selektor
          IHRES Manifest-Eintrags geprueft)
```

Beide Negativfaelle sind dadurch **schaerfer** geworden: ein unerfuellbarer Selektor trifft genau
den Eintrag, der ihn traegt, und der zweite Treiber bleibt unbeeintraechtigt; und zeigt der
Selektor der BLOCK-PD auf die Netzkarte, bekommt sie die Netzkarte — obwohl das Blockgeraet in der
Angebotsliste davor steht.

### Offen (Teil 2)

Der eigentliche Nachweis: Geraet A darf nicht in die DMA-Region von PD B schreiben. Der Knopf dafuer
steht (`VirtioNet::arp_probe_rx_at` verschiebt **genau eine** Adresse, damit das Geraet weiter
laufen kann), das Programm kennt die Anfragen `OP_SELF`/`OP_FOREIGN` — der Kernel-Testclient, der
beides ausloest und gegen die VT-d-Faults haelt, fehlt noch.

---

## A-5.4 (Teil 2). Das Geraet des einen Treibers erreicht die Region des anderen nicht

Die Mandantenaussage, auf die A-5 hinauslaeuft — und bis hierher war sie **unbelegt**, weil immer
nur eine Treiber-PD lief.

### Der Aufbau, und warum genau EINE Adresse wandert

`VirtioNet::arp_probe_rx_at` verschiebt allein die Adresse des **Empfangspuffers**. Virtqueues und
Sendepuffer bleiben in der eigenen Region, denn das Geraet muss **laufen** koennen: wuerden die
Ringe mitwandern, faende es nicht einmal die Deskriptoren, der Versuch scheiterte an der falschen
Stelle, und „nichts kam an" bewiese nur, dass nichts lief.

Dass die Netz-PD die fremde IOVA genannt bekommt, ist kein Loch, sondern der Kern der Aussage:
eine Adresse zu **kennen** hilft nicht, wenn der Uebersetzungskontext sie nicht aufloest. Ein
Angreifer, der raten muesste, bewiese nur, dass Raten schwer ist.

### Vier Zahlen, und keine reicht allein

```
dmaiso  : Positivkontrolle features=1 tx=1 rx=1 arp=1 · Fremdversuch features=1 tx=1 rx=1 arp=0
          · Opfer 0x00000000ffe00800 -> 0x00000000ffe00800 · VT-d-Faults=1
dmaiso  : ALL PASS
```

1. **Positivkontrolle** — derselbe Treiber, dasselbe Geraet, dieselbe Kette.
2. **Keine Daten** beim Fremdversuch.
3. **Das Opfer unberuehrt**, vom KERNEL nachgeprueft. Dass der Angreifer „nichts bekommen" meldet,
   ist eine Aussage ueber ihn.
4. **Mindestens ein VT-d-Fault** — der *aktive* Beleg, dass geblockt wurde. Ohne ihn waere „keine
   Antwort" auch mit einem stummen Gegenueber vereinbar.

### Zwei Fehler im eigenen Entwurf, beide gemessen

**Der Fremdversuch meldete zuerst `arp_reply = true`** — obwohl VT-d nachweislich blockiert hatte
(ein Fault gezaehlt). Grund: `arp_probe` nullte vom Empfangspuffer nur **acht Byte**; `ethertype`
steht bei +12, `oper` bei +20, die Absender-IP bei +28. Die zweite Probe las die Antwort der
**ersten**. Solange nur eine Probe je Lauf lief, konnte das nicht auffallen. Genau die Fehlerform,
die dieses Projekt schon zweimal bezahlt hat: ein Puffer mit den richtigen Bytes darin ist von
einem beschriebenen Puffer nicht zu unterscheiden, solange niemand vorher aufraeumt.

**`rx_used` gehoerte nicht ins Kriterium.** Das Bit kommt aus dem used-Ring und sagt, dass das
Geraet den Deskriptor *abgearbeitet* hat — es ist auch dann 1, wenn die Schreibung an der IOMMU
scheiterte (QEMU legt den Puffer trotzdem zurueck). „Das Geraet hat gehandelt" und „die Daten sind
angekommen" sind zwei Aussagen, und nur die zweite gehoert hierher.

Und ein dritter, der bei der Ablage passierte: `DMAISO_OK` entstand zuerst **im Bericht** und steht
in `all_done()` — was der Bericht setzt, kann den Bericht nicht ausloesen. Das Urteil faellt jetzt
in `drv_service_step`.

### Sensitivitaet

| Mutation | Ergebnis |
|---|---|
| M1: dem Angreifer die **eigene** Region nennen | `Fremdversuch arp=1`, `VT-d-Faults=0` → **FAILURES** |
| M2: Gegenueber antwortet nicht (`net=192.0.2.0/24`) | `Positivkontrolle rx=0 arp=0` → **SKIP mit Begruendung**, kein Urteil |
| unveraendert | ALL PASS |

M1 belegt, dass der Test einen **gelungenen** Fremdzugriff erkennt — ohne ihn hiesse „kein Zugriff"
nur, dass der Pfad ueberhaupt nichts tut. M2 belegt den dritten Ausgang: nicht messbar ist weder
bestanden noch durchgefallen.

**Gemessen:** x86 `ALL PASS`, Lade-Suite `ALL PASS`, aarch64 3/3 identische Signatur, Host-Tests
121, Kerngrenze sauber.

---

## Z4a + Z4b. Der Haltepunkt, und was auf keinen Fall mitwandert

Z4 ist ausdruecklich mehrstufig, und die Reihenfolge ist bindend. Z4e haengt an einem Netzstack
(Z10, gibt es nicht) und an Attestierung (Z7); gebaut sind deshalb die beiden Stufen, die **heute**
pruefbar sind — und Z4b ist die, die die Notiz selbst „die gefaehrlichste Stelle des ganzen
Vorhabens" nennt.

### Z4a: „benennbar" heisst zwei pruefbare Dinge

`system::freeze_thread` haelt einen Thread genau dann an, wenn beides gilt:

1. **Nicht im Kernel.** Ein Thread auf einem Kern kann mitten in einem Syscall stehen; einer auf
   **keinem** Kern ist per Konstruktion an einer Trap-Grenze stehengeblieben. Das wird ueber
   `current_id` je Kern **gefragt**, nicht geglaubt.
2. **Keine offene IPC-Beziehung** (`thread_quiescence`) — sonst bliebe ein Partner zurueck. Das ist
   Z4d Stufe 1: Migration nur ohne offene Transaktionen.

Die drei Ausgaenge sind unterscheidbar, und der Unterschied traegt: `StillRunning` ist
**voruebergehend** (der Reschedule-IPI ist unterwegs, wiederholen), `Busy` ist eine **Absage** (wer
darauf wartet, wartet ewig).

**Gewartet wird nicht hier.** Z4a verlangt ein `SYS_FREEZE`, das wartet — aber diese Funktion nimmt
die Scheduler-Sperre jedes Kerns, und in einer Schleife darauf zu warten, dass ein anderer Kern
voranschreitet, waehrend man seine Sperre haelt, ist die Bauanleitung fuer einen Deadlock. Der
Aufruf ist idempotent; das Warten gehoert dem Aufrufer.

### Die Pruefung misst die WIRKUNG, nicht den Rueckgabewert

```
freeze  : laeuft-vorher=true (35->38) eingefroren=true steht=true (38->38)
          aufgetaut=true laeuft-wieder=true (38->41) IPC-Rolle-abgewiesen=true
freeze  : ALL PASS
```

Der Rundenzaehler eines Workers muss sich **vorher** bewegen (sonst belegt „er steht" nichts,
sondern beschreibt einen Thread, der nie lief), waehrend des Einfrierens **stehen**, und nach dem
Auftauen **wieder laufen** — ohne den dritten Schritt waere ein `freeze`, das den Thread
kaputtmacht, von einem korrekten nicht zu unterscheiden. Dazu die vierte Aussage: der IPC-Server in
`RECV` wird **abgewiesen**. „Blockiert" und „ruhend" sehen von aussen gleich aus und sind es nicht.

| Mutation | Ergebnis |
|---|---|
| M1: `freeze` deplant nicht | `steht=false (37->44)` → **FAILURES**, und die Suite wird rot |
| M2: IPC-Rolle wird nicht geprueft | `IPC-Rolle-abgewiesen=false` → **FAILURES** |

### Drei Fehler im eigenen Entwurf, alle gemessen

1. **`MAX_CORES` statt `num_cores()`.** Die Schleife lief bis 8, konfiguriert waren 4 —
   `current_id` auf einer Scheduler-Instanz ohne Tabellen ist ein Programmfehler, und sie sagt das
   auch so (`kein laufender Thread (gefragter Kern 4, Instanz-Kern 0, TCB-Kapazitaet 0)`).
   **Nebenbefund:** der Panic hat den Knoten **nicht** gestoppt — der Lauf lief bis in die
   Pruefungen weiter. Genau das, was B-6.2 am selben Tag gemessen hat, hier zufaellig noch einmal.
2. **Das Beobachtungsfenster war kuerzer als ein Tick.** 400 000 Leerdurchlaeufe sind unter KVM
   knapp eine Millisekunde; die Verdraengung kommt nach 10 ms. Die Positivkontrolle meldete
   `laeuft-vorher=false` bei einem voellig gesunden Worker — gemessen wurde die Fensterlaenge.
   Gezaehlt wird jetzt in **Ticks**.
3. **`FREEZE_OK` stand in `all_done()`**, wird aber im Bericht gesetzt — was der Bericht setzt, kann
   den Bericht nicht ausloesen. Jetzt kein Konjunkt mehr; gelesen wird die Zeile von der **Suite**,
   und dass sie gelesen wird, ist die Lehre aus D5. Die Mutation belegt es: M1 macht die Suite rot.

### Z4b: was auf keinen Fall mitwandert

`crates/caprock-cap/src/checkpoint.rs`, abhaengigkeitsfrei und host-getestet (7 Tests). Die Regel
in einem Satz: **was auf der Zielmaschine nicht dasselbe bezeichnen kann, wandert nicht mit — es
wird verweigert, nicht ersetzt.** Der bequeme Weg waere, eine MMIO-Cap „auf das entsprechende
Geraet drueben" abzubilden; es gibt kein entsprechendes Geraet, es gibt ein anderes.

**Die Entscheidung braucht einen Umfang.** Bei `Mmio`/`Irq`/`Dma` ist sie fuer sich klar; bei einem
**Endpoint** nicht: wandert der Partner mit, ist die Cap sinnvoll uebertragbar, bleibt er zurueck,
zeigt sie ins Leere. Dieselbe Cap ist mal uebertragbar und mal nicht, und was zutrifft, haengt am
`Scope`, nicht an der Cap. Eine Klassifikation ohne ihn waere kuerzer und in der Haelfte der Faelle
falsch.

**Fail-closed:** was nicht ausdruecklich als uebertragbar gefuehrt ist, wird verweigert — ein neuer
Objekttyp, den niemand betrachtet hat, wandert damit nicht mit, statt durchzurutschen, weil er in
keiner Ablehnungsliste stand.

Drei Entwurfsentscheidungen, die die Tests festhalten:

* **Speicher wandert als Inhalt, nicht als Adresse.** Zwei Regionen gleicher Laenge an
  verschiedenen Physadressen sind extern **gleich** — das ist der Beleg. Die Zielmaschine faerbt
  neu; Farbe ist maschinenlokal (ein Argument dafuer, sie nie in die ABI zu heben).
* **Beziehungen werden ueber den Platz im Checkpoint bezeichnet**, nicht ueber eine ID — sonst
  waere die maschinenlokale Zahl doch mitgewandert, nur getarnt.
* **Ein Budget traegt eine Vorbedingung** (`SameTickSemantics`). Die Zahlen sind Ticks, und ein Tick
  bedeutet auf einer Maschine ohne invarianten Zaehler etwas anderes (Z4f, B-5.1). Sie zu
  verschweigen hiesse, die Abrechnung still falsch werden zu lassen.

Und die Ablehnungsgruende sind **einzeln** und nicht ein Sammel-`false`: „das Geraet gibt es dort
nicht" und „der Partner bleibt zurueck" sind verschiedene Befunde, und nur der zweite laesst sich
durch einen groesseren Umfang beheben.

### Was NICHT gebaut ist

Z4c (Dirty-Tracking), Z4d ueber Stufe 1 hinaus (Endpoint-Proxys), Z4e (Transport — braucht Netz und
Attestierung), Z4f (Maschinenvergleich). Und es gibt **keinen** vollstaendigen Checkpoint: die
externe Darstellung existiert als Typ und als Regel, ein Serialisierer und ein Wiederhersteller
nicht. Was hier steht, ist die Grundlage, auf der beides gebaut werden kann — und die Absage, ohne
die beides gefaehrlich waere.

> **Nachtrag 2026-08-03:** Serialisierer und Wiederhersteller gibt es jetzt, s. den naechsten
> Abschnitt. Der Satz „ein Checkpoint existiert nicht" gilt damit nicht mehr.

## Z4 Stufe 2. Ein Thread ueber die Bootgrenze

Z4a haelt einen Thread an, Z4b sagt, was mitwandern darf. Was fehlte, war die **Grenze**. Hier ist
sie die Maschinengrenze in der Zeit: derselbe Rechner, drei Laeufe, dazwischen je ein Neustart.
Alles, was der Kernel im RAM haelt, ist weg; was bleibt, ist ein Sektor.

**Die Aussage in einem Satz:** der Fortschrittszaehler eines Threads startet im naechsten Lauf bei
dem Wert aus dem vorigen, nicht bei null — und der Checkpoint, aus dem er kommt, wird abgewiesen,
wenn er zu einem anderen Kernel-Image gehoert.

### Der Entwurf: derselbe Kernel, kein Flag

Der Kernel liest `CKPT_SECTOR` und entscheidet daraufhin, was dieser Lauf ist. Vier Ausgaenge:

| Befund | Lauf |
|---|---|
| kein Checkpoint (leerer Sektor) | Kaltstart → speichern, Epoche 1 |
| passt alles | wiederherstellen → weiterarbeiten → wieder speichern, Epoche + 1 |
| Struktur kaputt (Version/Laenge/Pruefsumme) | abweisen, mit Code |
| Magie ja, **Kernel-Hash nein** | **abweisen** — Z4f in klein |

Ein Schalter waere bequemer gewesen und waere zugleich der Fehler: ein Kernel, dem man sagen muss,
ob er wiederherstellen soll, kann nicht merken, dass der Checkpoint gar nicht zu ihm gehoert.

**Der Transport ist der, den es schon gibt.** `programs/hardware/virtio-blk` nimmt `OP_WRITE` und
kopiert die geteilte Uebertragungsflaeche auf die Platte; der Kernel legt seine 512 Byte hinein und
ruft wie jeder andere Client (`ckpt_client` in `bringup.rs`). Ein eigener Speicherpfad im Kern waere
ein zweiter Treiber gewesen — genau das, was A-5.1 abgeschafft hat. `programs/hardware/virtio-blk`
ist **unveraendert**.

### Das Format

`crates/caprock-cap/src/checkpoint.rs`, neben der Regel, die entscheidet, was hineindarf.
Abhaengigkeitsfrei, ohne `unsafe`, host-getestet — fremde Bytes werden nirgends mit Kernprivileg
interpretiert, dieselbe Linie wie `caprock-part`/`caprock-fat`.

| Offset | Breite | Feld |
|---|---|---|
| 0 | 8 | Magie `SL4KCKPT` |
| 8 | 4 | Formatversion (1) |
| 12 | 4 | Rumpflaenge |
| 16 | 32 | **`kernel_code_hash`** |
| 48 | 8 | Fortschritt (`WORKER_ROUNDS[0]`) |
| 56 | 8 | Nonce (Zyklenzaehler beim Einfrieren) |
| 64 | 8 | Epoche |
| 72 | 4 | Zahl der Caps |
| 76 | 4 | Vorbedingungen (Bit 0 = `SameTickSemantics`) |
| 80 | 16·n | je Cap: Typ-Tag + Wert (`ExternKind`) |
| … | 4 | CRC-32 |

Feste Breiten, Little-Endian, wie das Manifest. `decode` prueft in dieser Reihenfolge: Magie →
Version → **beide** Laengenfelder gegeneinander und gegen die Bytefolge → Pruefsumme → Kernel-Hash.
Die Reihenfolge ist die Aussage: erst wenn 1–4 stehen, ist ein Fehlschlag bei 5 wirklich „ein
Checkpoint eines ANDEREN Kernels" und nicht bloss „kaputte Bytes".

`Image::build` ist die Stelle, an der Z4b wirkt: es ruft `classify_all`, und **eine einzige nicht
uebertragbare Cap verhindert den Checkpoint** — mit Slot und Grund, vor dem Schreiben.

Die Pruefsumme ist bewusst das uebliche CRC-32 (`zlib.crc32`): nur so kann ein *unabhaengiger*
Leser in einer anderen Sprache einen Checkpoint herstellen oder nachpruefen. Genau das braucht der
Negativfall unten — dieselbe Form wie `tools/checkfat.py`.

### Der Fehler im eigenen Entwurf, der die ganze Stufe umgebaut hat

Der erste Aufbau war: Lauf 1 speichert den Zaehler, Lauf 2 liest ihn und setzt ihn. Die Suite
verglich beide Zahlen, alles gruen. **Die Mutation hat den Aufbau widerlegt**: ein Kernel, der den
gefundenen Wert LIEST und MELDET, ihn aber nicht in den Thread schreibt, kam durch —

```
bootckpt: wiederhergestellt ... Fortschritt=151 ... Zaehler-danach=151     (M1, nichts gesetzt)
```

Der Grund: unter KVM ist der Hochlauf reproduzierbar. Gemessen an derselben Stelle des Hochlaufs:
**133, 142, 146, 148, 150, 151, 155 Runden** — Streuung rund 20. Der gespeicherte Wert und der
selbst gezaehlte fallen zusammen. Die Vorgabe „der Zaehler steht bei jedem Lauf woanders" ist auf
dieser Maschine **falsch**, und ein reproduzierbarer Wert kann nicht belegen, dass er geerbt wurde.

Die Nonce fing das nicht auf: sie belegt die **Herkunft der Bytes**, nicht die **Wirkung** des
Wiederherstellens. Zwei verschiedene Fragen — dieselbe Unterscheidung wie zwischen „das Geraet hat
gehandelt" (`rx_used`) und „die Daten sind angekommen" (A-5.4).

**Die Loesung ist eine wachsende Kette.** Jeder Lauf, der durchkommt, laesst den wiederhergestellten
Thread noch **mindestens 100 Runden** arbeiten (`CKPT_MIN_DELTA`, rund das Fuenffache der gemessenen
Streuung) und speichert erst dann, mit Epoche + 1. Ab dem zweiten Glied liegt der gespeicherte Wert
strukturell ausserhalb dessen, was ein Lauf ohne Wiederherstellung erreicht. Gemessen:

```
Lauf 1 (Kaltstart):     gespeichert Fortschritt=244  Epoche=1
Lauf 2:  wiederhergestellt Fortschritt=244 Zaehler-vorher=147 Zaehler-danach=244  Epoche=1
         gespeichert        Fortschritt=345                                        Epoche=2
Lauf 3:  wiederhergestellt Fortschritt=345 Zaehler-vorher=151 Zaehler-danach=345  Epoche=2
         gespeichert        Fortschritt=446                                        Epoche=3
```

`Zaehler-vorher` ist der Stand, den dieser Lauf **ohne jede** Wiederherstellung erreicht haette.
151 gegen 345 — die beiden Zahlen sind trennbar, und erst dadurch ist „geerbt oder selbst gezaehlt?"
ueberhaupt eine Messung. Die Suite prueft das ausdruecklich als **Positivkontrolle**: sind sie
gleich, ist der Lauf *nicht messbar* und faellt durch, statt gruen zu melden.

Nebenbefund derselben Runde: der Kaltstart fror den Thread ein und wartete dann auf einen Zuwachs,
den ein eingefrorener Thread nicht erreichen kann (`Zuwachs-erreicht=0`, 20 s Notschranke).
Einfrieren gehoert zum **Anfassen** des Zustands, nicht zum Warten darauf.

### Ein zweiter Fehler, den der Umbau freigelegt hat — und der aelter ist als Z4

`drv : ALL PASS` las den Datenpuffer der Treiber-PD **im Bericht** und verglich ihn mit der Magie
des Abbilds. Solange nur `drv_client` diesen Puffer benutzte, ging das gut: sein letzter Schritt war
ausdruecklich so gelegt, dass am Ende die Magie dort steht. Mit einem **zweiten** Client stand dort
dessen Sektor, und die Zeile meldete `FAILURES` fuer einen Treiber, der alles richtig gemacht hatte:

```
drv : Zuteilung: ... Sektor 0x54504b434b344c53 (erwartet 0x454b414c344c4553)
                          ^-- "SL4KCKPT", little-endian
```

Der Puffer eines Treibers gehoert dem **letzten Client**, nicht der Aussage. Der Wert wird jetzt
**erfasst, wenn die Aussage gilt** (Ende der Dienstfolge) und im Bericht nur noch gelesen; der
Momentanwert steht daneben als Diagnose. Das ist kein Umgehen des Befunds, sondern seine Behebung:
die Zeile misst seither, was sie behauptet.

### Was geprueft wird — und was ohne Negativfall nichts belegen wuerde

| Pruefung | Aussage |
|---|---|
| Kette 1 → 2 → 3, Epochen 1 → 2 → 3 | der Zustand **waechst** ueber Bootgrenzen, statt in jedem Lauf neu zu entstehen |
| `Zaehler-danach == Fortschritt` | der geerbte Wert steht **im Thread**, nicht bloss im Bericht |
| `Zaehler-vorher != Fortschritt` | **Positivkontrolle**: es war ueberhaupt etwas zu messen |
| Nonce des Erzeugers | die Bytes stammen aus dem vorigen Lauf, sind kein Rest im Puffer |
| `eingefroren=1` | angefasst wurde ein **stehender** Thread (Z4a) |
| Verweigerung, Grund 1/2/3 | Z4b am echten Gegenstand, s. u. |
| fremder Kernel-Hash → Lesecode 7 | Z4f in klein, s. u. |

**Negativfall 1 — eine nicht uebertragbare Cap.** In **jedem** Lauf wird zusaetzlich die
**Treiber-PD** klassifiziert; sie haelt echte Geraete-Autoritaet. Ihr Kanal (Endpoint +
Notification) liegt dabei ausdruecklich **im Umfang** — damit als Grund nur uebrigbleibt, was durch
keinen groesseren Umfang behebbar ist. Gemessen: `Slot 3, Grund 1` (Geraetefenster). Grund 5
(„Partner nicht im Umfang") waere behebbar gewesen und haette die Regel nicht belegt; die Suite
laesst nur 1/2/3 gelten.

**Negativfall 2 — ein Checkpoint eines fremden Kernel-Images.** Hergestellt wird er so, wie ein
echter aussaehe: der Sektor bleibt **strukturell heil**, nur das Hashfeld traegt einen anderen Wert,
und die Pruefsumme wird mit `zlib.crc32` **neu gerechnet**. Ein blosses Bytekippen waere der
schwaechere Fall — der faellt schon an der Pruefsumme durch, und der Test haette „kaputte Bytes"
gemessen statt „falsches Image". Ergebnis:

```
bootckpt: ABGEWIESEN Sektor=32710 Lesecode=7 (... 7=FREMDES KERNEL-IMAGE)
          -- der Sektor bleibt unveraendert, es wurde weder geladen noch ueberschrieben
```

Zwei Nachpruefungen dazu, beide noetig: es wurde **nicht** wiederhergestellt, und es wurde
**nicht** gespeichert. Ein Kernel, der nach der Abweisung seinen eigenen Checkpoint schriebe,
machte aus einem fremden Zustand lautlos einen eigenen.

### Sensitivitaet — zwei Mutationen, im Wortlaut

| Mutation | Wirkung |
|---|---|
| **M1** `WORKER_ROUNDS[0].store(…)` beim Wiederherstellen entfernt (Wert wird gelesen und gemeldet, nicht gesetzt) | `FAIL: … Lauf 2 fand den Zustand aus Lauf 1 nicht (erwartet … Zaehler-danach=241; bekam 241 / … / 156)`, dazu drei weitere FAIL-Zeilen; Kette bricht (241 → 256 statt 241 → 341); `== FAILURES ==` |
| **M2** Kernel-Hash-Vergleich in `Image::decode` entfernt | Host-Test `fremdes_kernel_image_wird_abgewiesen … FAILED` (21/22); Suite: der manipulierte Checkpoint wird **geladen** (`wiederhergestellt … Epoche=3`) statt abgewiesen → zwei FAIL-Zeilen, `== FAILURES ==` |

M1 ist die wichtigere: sie ist genau die Mutation, die den **ersten** Aufbau nicht rot machte.

### Was serialisiert wird — und was ausdruecklich nicht

**Drin:** Magie, Formatversion, `kernel_code_hash`, Fortschritt, Nonce, Epoche, das Ergebnis der
Cap-Klassifikation (je Cap ein `ExternKind`) und die Vorbedingung `SameTickSemantics`. 116 Byte bei
zwei Caps.

**Nicht drin — und der wichtigste Punkt dieses Abschnitts:**

* **Kein Trap-Frame, keine Register.** Das waere die naechste Stufe gewesen, und sie ist ohne Z4c
  **wertlos oder schlimmer**: ein Trap-Frame traegt `RIP` und `RSP`, und `RSP` zeigt in einen Stack,
  dessen Inhalt nach dem Neustart frischer Speicher ist. Wiederhergestellt ergaebe das einen Thread,
  der an der richtigen Adresse mit einem falschen Stack weiterlaeuft — ein Fehler, der spaeter und
  woanders auftritt. Registersatz ohne Speicher ist kein Checkpoint, sondern ein Zeiger ins Leere.
  Der Registersatz ist ohnehin der leichte Teil; der schwere ist Z4c.
* **Kein Speicher** (Z4c). Die Cap wandert als `Region { len }` — als **Laenge**, nicht als Inhalt.
  Der Inhalt ist auf der Zielseite null.
* **Keine offenen IPC-Beziehungen** (Z4d Stufe 1). `Scope::EMPTY`, und `freeze_thread` weist einen
  Thread mit offener Transaktion ohnehin ab.
* **Keine Authentifizierung** (Z4e). Die Pruefsumme sagt „strukturell heil", nicht „von wem". Ein
  Checkpoint ist derzeit nur so vertrauenswuerdig wie das Medium, auf dem er liegt. Ueber ein Netz
  waere das offen; ueber einen lokalen Sektor ist es die vorhandene Vertrauensgrenze.
* **Kein Maschinenvergleich** (Z4f). Geprueft wird das **Kernel-Image**, nicht die Maschine. Zwei
  Rechner mit demselben Image und verschiedener Cache-/NUMA-Klasse oder ohne `invtsc` waeren nicht
  unterscheidbar — die Vorbedingung `SameTickSemantics` steht im Checkpoint, aber **niemand prueft
  sie**. Das ist die naechste offene Stelle.
* **Nicht gemessen:** ob die Folge auch dann traegt, wenn der Blockdienst mitten im Schreiben
  ausgetauscht wird. Der Austausch (A-5.1) laeuft im selben Lauf, aber **vor** dem Checkpoint.

### Ort im Ablauf, und warum

Die Checkpoint-Folge laeuft **nach** der Dienst-/Blockdienst-Folge und **vor** dem Kreuz-DMA-Nachweis
(A-5.4). Nicht danach: A-5.4 liest das erste Wort der fremden DMA-Region vor und nach dem
Fremdversuch, und eine Platten-E/A dazwischen ginge durch genau diese Region — der „unabhaengige
Zeuge" saehe eine Aenderung, die der Angreifer nicht gemacht hat.

Der Sektor liegt auf **32710**, ausserhalb beider Partitionen (34..20000, 20001..32700) und
ausserhalb der GPT-Sicherungskopie (Eintraege ab 32735, Kopf auf 32767). Frei bleibt 32701..32734.
Die A-6-Pruefungen (GPT, FAT16, `tools/checkfat.py`) sind unberuehrt.

`bootckpt` steht als **Fertig-Merker** in `all_done()`, damit der Bericht nicht vor der Platten-E/A
kommt — das **Urteil** steht dort ausdruecklich nicht, es entsteht im Bericht und wird von der Suite
gelesen. Dieselbe Aufteilung wie bei `freeze` (Z4a).

**Gemessen (2026-08-03):** Lade-Suite `== ALL PASS ==` (39 Pruefungen, davon 12 neu), Host-Tests
`mem 22 · part 14 · fat 20 · cycles 9 · loader 50 · cap 22` → `ALL PASS`, Kerngrenze sauber.

## B-7.3. Der `delete`-Beweis war 37 Tage rot — und das Gate darüber grün gemeldet

**Ausgangslage, gemessen statt erinnert.** `tools/verus-verify.sh` fuhr 16 Beweisdateien; eine
davon, `Verification/capability-system/proofs/cap_space.rs`, meldete `9 verified, 1 ERRORS`.
Zwei Fehler in `proof fn delete`: ein `assert forall … by {}` mit **leerem Rumpf** für die
Eltern-Klausel („assertion failed"), und `function body check: Resource limit (rlimit) exceeded`
für die Funktion selbst.

**Der Befund, der schwerer wiegt als der Beweis.** Der Commit, der das eingebracht hat, heißt
`3384abb` „Phase 1 (Capability-System) Schritt C2a: delete (Leaf) gegen die VOLLE cap_inv
**bewiesen**" (2026-06-27). Die README der Komponente führte seither `10 verified ✅`. Das
CI-Gate `.gitea/workflows/verus.yml` stammt vom **selben Tag** und läuft `on: [push, pull_request]`;
beide Commits sind auf `origin/arch/x86_64` **und** `origin/master`.

Der naheliegende Ausweg wäre gewesen, das auf die Verus-Version zu schieben: die CI pinnte
`0.2026.06.20.911e4e7`, lokal lief `0.2026.07.27.31579f0`. **Nachgemessen — der Ausweg trägt
nicht.** Die alte Datei fällt unter der gepinnten Fassung mit **denselben zwei Fehlern**. Der
Beweis war nie grün. Damit bleiben genau zwei Möglichkeiten, und beide sind ein eigener Befund:
das Gate lief nie (kein Runner), oder es lief rot und niemand hat das Ergebnis gelesen. Ein Gate,
dessen Ausgang niemand liest, ist kein Gate — es ist eine Beschriftung. Das ist dieselbe Form wie
„ein Test, der nirgends läuft, ist kein Test", nur eine Ebene höher.

*(Die Version bleibt trotzdem eine Falle: zwei verschiedene Beweiser für dieselbe Aussage, ohne
eine Stelle, an der das auffiele. Der Pin steht jetzt auf der Fassung, gegen die entwickelt wird;
beide sind mit dem neuen Stand nachgemessen, je `18 verified, 0 errors`.)*

### Was der leere `by {}` wirklich brauchte

Die Klausel **gilt** — sie war kein Beweisproblem, sondern eine fehlende Kette aus fünf Schritten,
die Z3 nicht selbst findet:

1. der gelöschte Slot ist tot ⟹ jeder in `cs2` lebende Slot `s` ist ein **anderer**;
2. also lebte `s` schon in `cs`;
3. sein `parent` ist unverändert (Rahmen-Lemma);
4. das Ziel `p` ist **nicht** der gelöschte Slot — der **einzige** Punkt, an dem `no_children`
   gebraucht wird;
5. `object`/`rank` von `p` stehen still.

Ohne (4) ist die Klausel schlicht falsch. Der leere Rumpf war also nicht „zu wenig Rechenzeit",
sondern eine Behauptung ohne Argument.

### Die rlimit-Wand stand nicht vor einer schweren Klausel, sondern vor ihrer Summe

Die frühere Fassung hatte die richtige Idee (per-Klausel-Asserts), aber sie standen **alle im
selben Funktionsrumpf** — also in **einer** SMT-Query, zusammen mit der Refcount-Rechnung. Das ist
Gliederung, nicht Dekomposition. Die vier Struktur-Klauseln liegen jetzt in eigenen `proof fn`
(`lemma_del_parent`/`_next`/`_prev`/`_first_child`), jede mit eigener Query und eigenem Budget.

Dazu wurde der Eingriff selbst aus dem Rumpf herausgezogen und als **Spezifikation** hingeschrieben
(`unlink1`/`unlink2`/`unlink_slots` als explizite `Seq::update`-Folge), statt als `let`-Kette im
Beweis zu leben. Ergebnis: **von `rlimit exceeded` (24,3 s bis zum Abbruch) auf 3,0 s**, ganz ohne
`#[verifier::rlimit]` — das alte `#[verifier::rlimit(50)]` ist ersatzlos weg. Datei jetzt
`18 verified, 0 errors`; die ganze Suite `98 verified` über 16 Dateien in 14,0 s.

### Kinderlisten-Erreichbarkeit — mit der Hälfte, ohne die die andere wertlos ist

`unreachable_after_delete` sagt **beides**:

1. **kein lebender Slot zeigt noch auf den gelöschten** — über **keine** der vier Kanten
   (`parent`, `next`, `prev`, `first_child`), und der Slot selbst ist tot;
2. **der Elternknoten jedes anderen Slots ist unverändert.**

Ohne (2) bestünde ein Eingriff, der sauber aushängt und nebenbei ein fremdes Kind umhängt, die
Prüfung (1) mühelos. Gemessen: genau diese Mutation (M8) lässt den Beweis fallen.

Nebenbefund aus dem Beweis: **`cap_inv` schließt Selbst-Geschwister nicht aus.** `next[i]==Some(i)`
zusammen mit `prev[i]==Some(i)` erfüllt alle sieben Klauseln. Das ist bekannt (README §14 führt es
als Reachability-Ausbaustufe), aber es ist an mehreren Stellen des Löschbeweises der Fall, den man
einzeln ausschließen muss — und es ist der Grund, warum die Reihenfolge der Schreibzugriffe
überhaupt eine Rolle spielt.

### Empfindlichkeit: 8 von 9 Mutationen fallen — und die neunte ist eine bewiesene Redundanz

| # | Mutation | Ausgang |
|---|---|---|
| M1 | `next[pv]` nicht fortgeschrieben | **gefallen** |
| M2 | `first_child[par]` nicht nachgezogen | **gefallen** |
| M3 | `prev[nx]` nicht fortgeschrieben | **gefallen** |
| M4 | Blatt-Vorbedingung `first_child is None` entfernt | *durchgegangen* |
| M5 | `no_children` entfernt (Blatt-Vorbedingung bleibt) | **gefallen** |
| M6 | `cap_inv` (6) ohne `prev[first_child] is None` | **gefallen** |
| M7 | `cap_inv` (4-sib) ohne geteilten Elternknoten | **gefallen** |
| M8 | `unlink` hängt den Nachfolger um (`parent := None`) | **gefallen** |
| M9 | Slot zuerst geleert statt zuletzt | **gefallen** |

**M4 ist kein Loch.** `first_child is None` **folgt** aus `no_children` + Klausel 6: hätte das
Blatt ein `first_child`, so hätte dieses Kind `parent == Some(i)` — was `no_children` verbietet.
Statt das nur zu behaupten, steht es als `lemma_leaf_from_no_children` in derselben Datei und wird
mitverifiziert. Die Vorbedingung bleibt trotzdem stehen, weil sie das ist, was der reale Code an
dieser Stelle prüft. Das **Paar** M4/M5 ist die eigentliche Aussage: die beiden Vorbedingungen sind
nicht unabhängig, sondern geordnet — `no_children` trägt allein, die Blatt-Eigenschaft nicht.

### Die Grenze zum echten Quelltext: `tools/verus-modelltreue.sh`

**Ein grüner Beweis kann per Konstruktion nicht bemerken, dass sich der Code unter ihm bewegt
hat** — das Modell ändert sich ja nicht mit. Bis hierher war die Modell-Treue eine *dokumentierte
Annahme* (README §11/§12). Das ist die Form, die B-7.2 teuer bezahlt hat.

Der Wächter reduziert beide Seiten auf **dieselbe normalisierte Ereignisfolge** aus Verzweigung und
Feldzuweisung und verlangt Gleichheit — heute 12 Ereignisse, deckungsgleich:

```
BRANCH prev · ARM some prev · WRITE prev.next := next
             · ARM none prev · BRANCH parent · ARM some parent · WRITE parent.first_child := next
BRANCH next · ARM some next · WRITE next.prev := prev
WRITE self.* := EMPTY · DELETE_LEAF unlink release_slot refcount--
```

`match Option { Some/None }` und `if … is Some { } else { }` fallen dabei auf dieselbe Form; der
Dialektunterschied verschwindet, die Struktur bleibt stehen. **Selbsttest, 8 Fälle:** vier
Mutationen am echten Code (Feldwert vertauscht, `first_child`-Zweig gelöscht, Leerung vorgezogen,
`release_slot` vor `unlink`), drei am Modell (Feldwert vertauscht, Zweig gelöscht, `unlink1`
umbenannt → *der Wächter liest ins Leere*) müssen ihn auslösen — **und eine kosmetische Änderung
auf beiden Seiten** (Binder umbenannt, Kommentare, Leerzeilen) darf ihn **nicht** auslösen. Beide
Hälften sind nötig: ein Wächter, der immer schreit, wird abgeschaltet; einer, der nie schreit, ist
eine Kopie mit Zertifikat.

**Der erste Lauf fand sofort zwei Abweichungen** — beide im Modell, beide behoben, indem das Modell
dem Code angeglichen wurde (nicht umgekehrt):

* **Die `first_child`-Fortschreibung stand unter der falschen Bedingung.** Das Modell zog nach,
  wenn `first_child[par] == Some(i)` galt; der Code schreibt, sobald `prev is None && parent is
  Some` — **ohne** zu prüfen, ob `i` überhaupt der Kopf war. Unter `cap_inv` **allein** ist das
  nicht dasselbe: Klausel 6 sagt nur die Gegenrichtung („der Kopf hat `prev == None`"), nicht
  „jedes Kind mit `prev == None` ist der Kopf". Erst die Reachability-Klausel 4r macht beides
  gleich. Der Code ist damit nicht falsch (`cap_inv` bleibt in beiden Fassungen erhalten), aber er
  **verlässt sich auf 4r** — und 4r ist genau die Klausel, die es noch nicht gibt. Der Beweis führt
  den Fall jetzt mit; die Abhängigkeit steht im Quelltext des Beweises.
* **Die Reihenfolge war umgedreht.** Das Modell leerte den Slot **zuerst**, der Code **zuletzt**.
  Bei einem Selbst-Geschwister sind das verschiedene Endzustände (s. o.).

Beides hätte kein Beweis der Welt gefunden: der alte Beweis wäre mit dem alten Modell grün gewesen.

### Und der Einsammler selbst war die dritte Lücke

`tools/verus-verify.sh` sammelte `verus/*.rs` + `Verification/*/proofs/*.rs`. Eine Beweisdatei
irgendwo sonst — ein Unterverzeichnis unter `proofs/`, eine neue Ablage, eine Datei eine Ebene
höher — lief damit **nirgends**, und zwar lautlos: das Skript meldete weiter `0 errors`, weil es
sie gar nicht kannte. Jetzt entscheidet der **Inhalt** (jede `.rs` mit einem `verus!`-Block, die
Wegwerf-Worktrees unter `.claude/` ausgenommen — dort liegen **alte Kopien derselben Dateien**),
ein leerer Fund ist ein Fehler statt eines Erfolgs, und die verwendete Verus-Version steht in der
Ausgabe. `--selftest` schiebt eine nachweislich falsche Beweisdatei unter, **an einer Stelle, die
die alten zwei Globs nicht getroffen hätten**, und verlangt, dass der Lauf daran scheitert; das
Gate fährt diesen Selbsttest jetzt vor den Beweisen.

*(Ein Fehler im eigenen Entwurf, unterwegs gefunden: der Selbsttest prüfte die Fundliste mit
`beweisdateien | grep -q`. `grep -q` schließt die Pipe früh, der Erzeuger stirbt an SIGPIPE, und
`set -o pipefail` macht daraus einen Fehlschlag der ganzen Pipeline — der Selbsttest meldete „wird
nicht eingesammelt", obwohl sie eingesammelt wurde. Ein Prüfer, der an seiner eigenen Mechanik
scheitert und das als Befund ausgibt, ist derselbe Fehler wie die Pipe in der ARM-Suite, nur in
klein.)*


## Aus `todo.md` herübergeräumt am 2026-08-11

`todo.md` ist die Quelle für **was ist noch offen**. Erledigtes darin macht genau diese
Frage unbeantwortbar — dieselbe Form, die der Modell-Treue-Wächter einmal an sich selbst
gemeldet hat. Was hier steht, ist deshalb **nicht** neu geschrieben, sondern wörtlich
verschoben: die Begründungen sind der Wert, nicht die Häkchen.


### Ganze Einträge, die nur noch Erledigtes trugen

## ~~SPERRT DIE INTEGRATION~~: das nullgrosse LOAD-Duplikat — **URSACHE GEFUNDEN, BEHOBEN** (2026-08-10, abends)

**Die Ursache lag NICHT im Linkerskript, sondern in der BAUUMGEBUNG — und deshalb hat kein
einziger der drei Versuche daran etwas ändern können.**

Gemessen, mit einer Zeile:

```
$ cargo -Zunstable-options config get target.x86_64-unknown-none.rustflags
["-C","link-arg=-Tkernel/x86_64-link.ld","-C","relocation-model=static",
 "-C","link-arg=-Tkernel/x86_64-link.ld","-C","relocation-model=static"]
```

**Cargo liest `.cargo/config.toml` aus JEDEM Vorfahrenverzeichnis und hängt Array-Werte
aneinander.** Ein Agenten-Worktree liegt unter `<repo>/.claude/worktrees/<id>`, also **innerhalb**
des Hauptbaums — Cargo findet die Konfiguration deshalb zweimal, und `-Tkernel/x86_64-link.ld`
steht doppelt auf der Linkerzeile. `lld` wertet den `SECTIONS`-Block dann **zweimal** aus. Alles
Weitere folgt daraus:

* **Die sieben leeren Doppel-Sektionen** sind genau die Ausgabesektionen, die ausser
  Eingabe-Beschreibungen noch etwas enthalten (`. = ALIGN(..)`, Symbolzuweisung, feste Adresse).
  `.boot`, `.data`, `.boot_bss` bestehen nur aus `*(...)` — leere Ausgabesektionen dieser Art
  verwirft lld, deshalb fehlen genau diese drei in der Duplikatliste. Das Muster, an dem der
  Befund hing, war die ganze Zeit die Antwort.
* **Alle Linkersymbole trugen die Werte des ZWEITEN Durchlaufs** (`. ` fängt wieder bei 1M an):
  `__text_start = 0x100000`, `__bss_start = 0x101000`, `__aptramp_lma = 0x100000`. Der gesunde
  Hauptbaum hat `0x101000 / 0x18e000 / 0x18d000`. **Das ist der eigentliche Schaden** — das
  nullgrosse LOAD-Segment und der verschobene Multiboot-Header sind nur seine sichtbarste Folge.

**Warum der Gegenversuch aus dem alten Eintrag ebenfalls fehlerhaft baute** („A3-Code im selben
Worktree zurückgenommen"): die Ursache war der **Ort** des Worktrees, nicht sein Inhalt. Die
Vermutung „der Worktree-Zustand ist selbst verdächtig" war richtig, nur eine Ebene zu vage.

**Behoben** in `build-x86.sh` und `build.sh`: `tools/rustflags-entdoppeln.py` liest die
**effektive** Flagliste, entdoppelt sie und setzt sie als `RUSTFLAGS` — und meldet **laut**, dass
es das getan hat (eine stille Reparatur wäre dieselbe Krankheit wie das stille Mischen).
`RUSTFLAGS` und **nicht** `CARGO_TARGET_<T>_RUSTFLAGS`: gemessen wird die zielspezifische
Variable von Cargo mit der Konfiguration **mitgemischt** — danach standen die Flags dreifach da
und das Abbild war schlechter statt besser. `RUSTFLAGS` ersetzt.

**Belegt:** 22 Sektionen statt 29, sieben LOAD-Segmente ohne nullgrosses, letztes Segment
`filesz=0x58` statt `0xf7000`, Multiboot-Header bei Dateioffset 4096, `./test-qemu-x86.sh` →
`== ALL PASS ==`. **Das Linkerskript ist dabei unverändert geblieben** (`git diff` leer) — die
drei Versuche (a)/(b)/(b2) haben ein Symptom bearbeitet und können aus dem Register.

**Zwei Dinge, die daraus folgen und breiter gelten:**

* **Die literale Abnahmezeile `cargo build --release -p caprock-kernel --target
  x86_64-unknown-none --features selftest` ist in einem Worktree SELBST betroffen** — sie umgeht
  `build-x86.sh` und erzeugt das kaputte Abbild. Wer sie zur Abnahme benutzt, misst ein Artefakt,
  das nie gebootet hätte.
* **Dasselbe Loch stand im F1-Gate der Suite.** `test-qemu-x86.sh` baute die
  `--no-default-features`-Konfiguration mit direktem `cargo`, las die `.text`-Grösse mit
  `grep -A1 " .text " | tail -1` — und traf damit die **leere Doppel-Sektion**. `NOSEL_TEXT` stand
  auf **0**, und die Zeile meldete `PASS` („0 < 0x62000") für eine Zahl, die kein Messwert war.
  Behoben; die Grösse ist jetzt wieder eine gemessene (`0x362a5`).

- [ ] **Offen bleibt die strukturelle Frage:** ein Worktree im Repo erbt jede
      Vorfahren-Konfiguration doppelt — das betrifft auch `programs/.cargo/config.toml` (dort
      steht `-Tuser.ld` **neben** dem geerbten `-Tkernel/...`) und damit die Lade-Suite. Geprüft
      ist das nicht; der Entdoppler deckt heute nur die beiden Kernel-Bauwege ab.

## Das nullgrosse LOAD-Duplikat — der Stand VOR der Ursachenfindung (zum Nachlesen)

**Klasse:** Bauwerkzeug · **Stand:** überholt, s. o.

Ein halber Tag ging an einen Blocker, den es als Codeproblem **nie gab**. Der Ablauf, weil er die
Lehre trägt:

1. Zwei Agenten meldeten unabhängig, die x86-Suite laufe „in diesem Baum nicht"
   (`Error loading uncompressed kernel without PVH ELF Note`, Logdatei 0 Byte), und schrieben es
   der **Grundlinie** zu.
2. Nachgemessen: der Hauptbaum war gesund (`.boot` bei Offset 4096, warm **und kalt** gebaut,
   beide Suiten grün), die Änderungszweige nicht. Daraus wurde „die Änderungen kippen die
   Sektionslage" — **auch das war falsch**.
3. Drei Behebungsversuche am Linkerskript, jeder gemessen, jeder zurückgenommen.
4. **Die Ursache war, dass Cargo Linkerskripte nicht als Bau-Eingabe kannte.** Ohne
   `rerun-if-changed` löst eine `.ld`-Änderung **kein Neu-Linken** aus; in den Agenten-Worktrees
   war der Linkerschritt damit gegen einen Stand gelaufen, den es so nicht mehr gab.
5. Gegenprobe: ein **frischer** Integrationszweig im Hauptbaum, mit den `rerun-if-changed`-Haken
   von Anfang an, mit derselben A3-Arbeit gemerged → **keine leeren Duplikatsektionen**, Header
   bei 4096, `== ALL PASS ==` in **beiden** Suiten, alle Wächter grün.

**ZWEI BERICHTIGUNGEN, in dieser Reihenfolge gezogen — und die zweite hat die erste überholt.**

*Erste (vormittags):* „Der Blocker existierte als Codeproblem nie" war eine Hypothese im
Ergebniskostüm; belegt war nur „**reproduziert auf frischem Zweig nicht**". Richtig gezogen — aber
das benannte **Residualrisiko („ein reihenfolgeabhängiges Layout")** zeigte in die **falsche
Richtung**. Die Reihenfolge war nie das Thema.

*Zweite (abends, s. o.):* die Ursache ist ein **benennbarer, jederzeit auslösbarer Mechanismus** —
Cargo mischt Vorfahren-Konfigurationen und hängt Arrays an. „Aufgelöst" war damit ebenfalls zu
früh: der Mechanismus ist nicht weg, er **verschonte den Hauptbaum nur zufällig** (dort gibt es
genau eine Konfiguration) und trifft jeden, der in einem verschachtelten Arbeitsbaum baut.

Der Unterschied ist nicht akademisch. Die drei Versuchszeilen sind als *Messwerte* entwertet, die
*Beobachtungen* aber waren echte `readelf`-Ausgaben — nur eben von einem Abbild, dessen
Linkerskript zweimal ausgewertet worden war. **Zusammen mit dem wasm-Eintrag ist das der Grund,
warum „aufgelöst" ein Messwert sein muss und kein Gefühl:** dort sagte das Muster die falsche
Hypothese exakt voraus, hier hat die zweite Grabung die bequeme erste Erklärung („Bau-Artefakt,
weg damit") durch die unbequeme richtige ersetzt.

**Deshalb ist die Eigenschaft jetzt dauerhaft geprüft statt behauptet:** der Bauzeit-Wächter
verlangt zusätzlich, dass **kein LOAD-Segment Dateiinhalt trägt, wo nur NOBITS-Sektionen liegen** —
genau die Form, an der sich der Fall zeigte, nicht die vermutete Ursache. Dieselbe Bewegung, die
aus dem wasm-Fall die Vollzähligkeits-Zeile gemacht hat.
Zwei eigene Fehler dabei, beide gemessen: die erste Fassung **rechnete die Zuordnung
Sektion → Segment nach** statt sie zu lesen und ordnete dem Segment bei `0x9000` prompt `.boot`,
`.text` und `.rodata` zu (dessen `memsz` überspannt den ganzen Bildbereich) — die
`iova_window_clear_of_msi`-Falle, im eigenen Wächter. Jetzt wird die Zuordnung aus `readelf`
gelesen. Und die Sprechprobe hat das gefangen, nicht das Gegenlesen: mit `all` → `any` mutiert
nennt der Wächter jetzt genau `.aptramp_data, .bss, .boot_bss`.

**Die Lehre ist nicht „Linkerskripte sind heikel", sondern: eine Messung muss wissen, welches
Artefakt sie gemessen hat.** Der Binary-Fingerprint der Suiten schliesst „veralteter Build" als
Erklärung für einen *Suitenlauf* aus — für den *Linkerschritt* war dieselbe Tür offen, und drei
Versuchszeilen einer Tabelle beschrieben einen Stand, den es nie gab. Eine Versuchstabelle, deren
Spalte „bootet" nicht misst, was sie behauptet, ist schlimmer als keine.

**Was bleibt und sich gelohnt hat:**

- [x] `kernel/build.rs` + `programs/libcaprock/build.rs` melden die vier `.ld`-Dateien als
      `rerun-if-changed`. Gemessen: ein `touch` aufs Skript allein löst `Compiling caprock-kernel`
      aus. Eine Datei statt sieben bei den Programmen, weil jedes Programm `libcaprock` linkt.
- [x] **Der Bauzeit-Wächter in `build-x86.sh`**: der Multiboot-Header muss in den ersten 8192
      Dateibytes liegen, sonst `BUILD FAILED` mit dem gemessenen Offset **und** der LOAD-Tabelle.
      Die Bedingung stand seit jeher im Linkerskript und wurde von nichts durchgesetzt. Sie ist
      **grenzwertig**, nicht stabil — ein Bauwerkzeug, das ein unbootbares Abbild ausliefert, ist
      die Bauzeit-Fassung von „Schweigen als Erfolg".
- [ ] **Messregel, ab sofort:** `KEIN OUTPUT` ist als Befund **unterbestimmt**. „Lädt nicht" und
      „läuft, aber die Konsole hängt an einer verschobenen Adresse" sind darunter
      ununterscheidbar. `-d int` (kommen überhaupt Faults?) und `info registers` (wo steht das
      System?) gehören dazu, bevor ein `KEIN OUTPUT` als Zeile ins Register geht.
- [ ] **Und für Agenten in Worktrees:** `git stash` nicht benutzen — `refs/stash` ist über alle
      Worktrees geteilt, und zwei Agenten haben sich damit gegenseitig den Stand gepoppt (beide
      über `git fsck` zurückgelegt, nichts verloren).

## NULL IST EIN BEFUND, KEIN MESSWERT — der Durchgang durch die einseitigen Vergleiche

**Klasse:** Prüferform · **Stand:** durchgegangen 2026-08-10, **eine** lebende Fundstelle, behoben

Der F1-Fund trägt eine Regel, die über F1 hinausgeht. `0 < 0x62000 ⇒ PASS` ist ein Prüfer, der
**bei Totalausfall der Messung grün wird** — dieselbe Form wie ein nie gesetztes Bit, das als
„kein Fehler" gelesen wird, nur als Schwellenvergleich statt als Flagge. Die Regel:

> Jede gemessene Grösse, die in einen Vergleich mit **nur einer** Schranke geht, braucht eine
> **Plausibilitätsuntergrenze** — oder der Wert Null muss ausdrücklich als **„nicht gemessen"**
> ausscheiden. „Nicht messbar" ist kein bestandener Test.

**Der Durchgang, mit Zahlen — damit „ich habe nachgesehen" nicht wieder ein Nullbefund ohne
Grösse ist** (dieselbe Falle wie „habe ich noch nie gesehen"):

| gesucht | gefunden |
|---|---|
| numerische Vergleiche in den drei QEMU-Suiten und allen `tools/*.sh` (`-lt/-gt/-le/-ge`) | **41** |
| davon ohne vorherige `-n`/`-z`-Absicherung oder `> 0`-Wächter | **1** — die F1-Zeile |
| Urteilszeilen im Kernel der Form `wert </<= KONSTANTE` | **0** (alle Treffer sind Schleifenwächter, keine Urteile) |
| Urteilszeilen im Kernel der Form `wert > 0 && …` | durchgehend, das ist die gesunde Form |

Behoben ist die eine: F1 hat jetzt `F1_MIN = 0x10000` **und** einen eigenen Zweig für „nicht
messbar" (leerer Wert ⇒ `FAIL`, nicht stillschweigend `else`). Die Untergrenze ist begründet und
nicht gegriffen: ein Kernel ohne Prüfinfrastruktur hat weiterhin Scheduler, IPC,
Speicherverwaltung und HAL; unter 64 KiB `.text` ist das kein kleineres Abbild, sondern ein
kaputter Bau.

**Was der Durchgang NICHT abdeckt und offen bleibt:** Prüfer, die eine Grösse gar nicht erst
erheben (die Klasse „ein Test, der nirgends läuft"), und Vergleiche innerhalb der Verus-Modelle.
Ein Durchgang, der seine eigene Reichweite nicht nennt, ist die nächste Nullaussage.

### Z25. Eager-FP auf x86 — der Dreier-Commit, 2026-08-09 — **ZU**
**Klasse:** Sicherheit · **Stand:** alles fertig und gemessen; `fp : ALL PASS` und die Zeile
**gattert** (`all_done`, 27 Flags). Zwei Gegenproben mit unterscheidbarer Signatur, s. unten.

- [x] **Der Auslöser liegt im WECHSEL, nicht im ersten Zugriff.** `CR0.TS` bleibt nach `enable_sse`
      dauerhaft aus; `sync_fp_trap` sichert/lädt beim Wechsel. Grund ist **CVE-2018-3665
      (LazyFP)**: mit gesetztem `TS` wird der `FXRSTOR` aufgeschoben, und spekulative Ausführung
      kann die Register des **vorigen** Besitzers lesen, bevor das `#NM` zugestellt ist. Seit SSE
      für Userland scharf ist, kann dort Schlüsselmaterial liegen.
      Billig bleibt es trotzdem: wechselt der laufende Thread nicht (jeder Syscall ohne
      Umplanung), passiert nichts.

- [x] **`#NM` ist eine laute Invariante — mit RIP, Kern-ID und `CR0` —, und der Zähler läuft JE
      KERN.** Global summiert ginge ein einzelner AP, dessen `enable_sse` in einem Refactor aus
      der Reihenfolge rutscht, im Rauschen der übrigen unter — und das ist die wahrscheinlichste
      künftige Regression. **Gemessen: `k0=0 k1=0 k2=0 k3=0`.**
      Nicht angehalten wird: ein Panic reisst den Knoten nachweislich **nicht** mit (§14), er
      verschlechterte nur die Diagnose. Der Zähler macht den Lauf rot, das genügt.

- [x] **`ts_loeschen` meldet, statt still zu reparieren.** `ts_vorgefunden()` zählt, wie oft
      `CR0.TS` gesetzt **vorgefunden** wurde — unter eager muss das 0 sein. Ein `debug_assert!`
      wäre im Release-Bau weg, und dort läuft die Suite. **Gemessen: 0.**

- [x] **Das Vektor-Inventar** — ein Zähler je CPU-Ausnahme (0..31), von der Suite gedruckt.
      Vektor 7 ist die Zeile, um die es geht; die übrigen stehen dabei, **damit sichtbar ist, dass
      überhaupt gezählt wird** (ein Melder, der nur beim Unglück spricht, ist in jedem gesunden
      Lauf stumm). Der heisse Syscall-/Timer-Pfad ist bewusst **nicht** dabei.
      **Gemessen: `14(#PF)=2`, sonst nichts — Vektor 7 kommt gar nicht vor.**

- [x] **Die aarch64-Divergenz ist begründet, in BEIDEN HAL-Verträgen.** `CPACR_EL1.FPEN` trappt
      präzise und **nur EL0**; eine LazyFP-Entsprechung ist nicht veröffentlicht. Die
      Trap-Reichweiten sind verschieden, und die Exponierung ist es auch. Beide Dateien nennen die
      jeweils andere Seite — die vorige Fassung sah auf einer Architektur eager und auf der anderen
      lazy aus, **ohne dass irgendwo stand warum**, und war damals tatsächlich ein Versehen.

- [x] **Die rote FP-Sonde war ein Fehler im KRITERIUM, nicht im Kernel — und das Kriterium war
      unerreichbar.** Verlangt war „das Muster hat **alle 64** Abgaben überstanden". Gemessen kam
      die Sonde bis **3/64** — bei grüner Sofortprüfung (1000/1000, also beim ersten Versuch) und
      **null** gemeldeten Korruptionen. Sie war nie korrumpiert, sie war **langsamer als der
      Bericht**: eine Iteration je Rundlauf-Runde, und eine Runde ist durch den **Tick** begrenzt,
      nicht durch das `YIELD`. Die 64 war eine Zahl ohne Bezug zur Rundenlänge.
      **Ein Kriterium, das die geprüfte Sache nicht erreichen kann, ist kein strenges Kriterium,
      sondern gar keins:** grün ist unmöglich, also sagt rot nichts. Es hat keine Trennschärfe —
      dieselbe Form wie ein Prüfer, der die falsche Größe liest, nur von der anderen Seite.
- [x] **Die Sprechprobe ist die EIGENE Verdrängungszahl der Sonde, nicht die globale.**
      `fp_switch_count()` sagt, dass irgendwo gewechselt wurde. Lägen beide Sonden auf
      verschiedenen Kernen und verdrängten einander nie, wäre die Zahl hoch und die
      Musterprüfung **gegenstandslos** — sie prüfte Register, die zwischen ihren Abgaben niemand
      angefasst hat. Dieselbe Form wie `rx_used` gegen „Daten sind angekommen".
      Neu: `fp_watch`/`fp_watch_restores` zählen die Restores **dieser** Threads, eingetragen nach
      der Zulassung (der Zähler ist damit eine **Untergrenze** — er verliert höchstens die ersten
      Restores, und in der Richtung, die den Test strenger macht).
      **Gemessen: Verdrängungen 4/4 bei Fortschritt 3/3** — also ≈ eine Verdrängung je Iteration.
      Die Sonden kontrahieren wirklich; das war vorher nicht belegt, sondern angenommen.
- [x] **Gefordert wird `>= 2`, nicht `>= 1`** — der erste Restore lädt einen frisch genullten Slot,
      also *bevor* die Sonde ihr Muster geschrieben hat. Erst der zweite belegt, dass ein
      **geschriebenes** Muster eine Verdrängung überstanden hat.
- [x] **Das Urteil steht an EINER Stelle (`fp_urteil()`), und `fp` gattert jetzt** (`all_done`,
      26 → 27 Flags). Vorher war das Draussenbleiben richtig (ein unerreichbares Kriterium hätte
      die Suite dauerhaft rot gefärbt); mit einem erreichbaren Kriterium wäre es das Gegenteil —
      eine grüne Zeile, die nichts gattert, also genau der `pdbind`-Fehler.
      Der Eintrag in `BEKANNT_ROT` ist **ausgetragen**, mit dem Grund an der Stelle.
- [x] **Zwei Gegenproben, und sie sind UNTERSCHEIDBAR rot** — der gefundene Wert benennt die
      Ursache, statt nur „nicht meins" zu sagen:
      * **kein Save des vorigen Besitzers** → `Sonde0=0x8000000000000000` = Markerbit 63 plus
        lauter Nullen, also „ein frisch genullter Slot wurde restauriert". Verdrängungen blieben
        bei 2/2 — die Sprechprobe spricht also **weiter**, während die Eigenschaft fällt, und das
        ist ihre Aufgabe.
      * **kein Restore** → `Sonde0=0xa5a55a5a3c3cc3c3`, wörtlich das **Muster des Partners**:
        Identitätsvertauschung, der FP-Zustand folgt der CPU statt dem Thread.
      Beide `fp : FAILURES`, danach wieder `== ALL PASS ==`.
      Ehrlich dazu: die Isolation ist hier **nicht** vollständig — mit der Korruption fällt
      zwangsläufig auch der Fortschritt, weil die Sonde bei Erkennung parkt. Das sind nicht zwei
      Fehler, sondern eine Wirkung mit zwei sichtbaren Folgen; die Konjunkte sind kausal verkettet,
      nicht unabhängig.
- [x] **Was die alte Diagnose kostete, bleibt als Lehre stehen.** Der Berichtstext nannte als
      Ursache „`CR4.OSFXSR` wird nirgends gesetzt" — seit A4 überholt. Die Zeile blieb rot, die
      Erklärung stimmte nicht mehr, und nachdiagnostiziert hat es niemand. **Ein Grund, der nicht
      mehr stimmt, macht den Punkt unbehebbar** — dieselbe Form wie der falsche `SYS_MAP`-Grund im
      Identitäts-Wächter.

### Z24. Der Blockadegrund ist eine MENGE, keine Bit-Sammlung — **GEBAUT 2026-08-10**

**Stand:** umgesetzt und gemessen. x86 Haupt-Suite `== ALL PASS ==`, Lade-Suite unverändert
(nur der bekannte `wasm`-Rest), **aarch64 `== ALL PASS ==`**, Host-Tests, alle drei
Modelltreue-Wächter, Kerngrenze/Zulassung/Identität grün. Zwei Gegenproben, beide isolierend.

- [x] **`BlockReasons` steht, und die tragende Aussage steht in EINER Zeile:** eingereiht wird
      **nur bei leerer Menge** (`wecke_falls_lauffaehig`). Ohne diesen Halbsatz wäre die Menge
      bloss eine andere Schreibweise für dieselben Bits.
      `blocked`/`budget_blocked`/`parked` sind als Felder **verschwunden**; `park_wake` bleibt
      eigenes Feld — es ist eine Marke, kein Grund.

- [x] **Zwei Gegenproben, jede kippt GENAU EIN Konjunkt.**
      * **M1** — `unpark` entfernt *alle* Gründe statt `PARK`: `lief-trotz-pause=true`, alle
        sechs übrigen Aussagen bleiben grün.
      * **M2** — eingereiht wird *ohne* die leere Menge zu verlangen: dasselbe eine Konjunkt kippt.
      Beide `park : FAILURES`, danach wieder `== ALL PASS ==`. Damit ist belegt, dass **beide**
      Hälften der Regel tragen, nicht nur die griffigere.

- [x] **Neu: die Aussage „verlorenes Pausieren" — die es bis dahin nicht gab.** Gemessen an der
      **Wirkung** (ein Rundenzähler der Park-Sonde), nicht an einem Bit: einfrieren, dann das
      `UNPARK` eines Geschwisters — der Zähler darf sich **nicht** bewegen. Mit Sprechprobe
      (`laeuft-nach-thaw`), denn sonst wäre „bewegt sich nicht" von „darf sich nicht bewegen"
      nicht zu unterscheiden.

- [x] **`H-b` ist WEGGEFALLEN, nicht umgeschrieben.** `pause` löschte bis dahin den Budget-Grund
      („PAUSE ÜBERNIMMT die Blockade") — es **musste** einen fremden Grund löschen, um den eigenen
      durchzusetzen, weil ein einziges Bit keine zwei Gründe trägt. Der Preis stand im Kommentar
      daneben: ein pausierter und wieder fortgesetzter Thread lief auf **leerem Konto** weiter.
      Mit der Menge verschwindet der Griff ersatzlos. Dieselbe Auflösung wie bei der D9-Aussage.

- [x] **Ein Wecker muss ab jetzt seinen Grund NENNEN — und das hat drei Stellen aufgedeckt:**
      * `SYS_PDCTL` RESUME/START rief `unblock`, also „hebe irgendeine Blockade auf". Neu:
        `resume` (entfernt `PAUSE`), durch alle drei Schichten (`SchedOps`, Kernel, microkit).
      * `thaw_thread` rief `unblock`, obwohl `freeze_thread` über `pause` einfriert — **genau die
        Naht `thaw × park`, um derentwillen der Umbau geplant wurde.** Sie stand offen im Code.
      * **Der aarch64-Cross-Core-Test weckte einen GEPARKTEN Thread mit `wake_remote`** (dem
        Wecker für IPC). Das ging, solange ein Bit beide Gründe trug.

- [x] **Der Fastpath war die vierte Instanz — und er steckte NICHT in `unblock`.** `switch_to`
      schrieb `blocked = false` am Ziel und liess es **unmittelbar** laufen: ein Thread, der in
      `RECV` steht und pausiert wurde, lief beim nächsten `send` los, und die Pausen-Entscheidung
      war still weg. Jetzt ist der Fastpath **bedingt**: die Nachricht ist zugestellt (der
      IPC-Grund fällt), aber gewechselt wird nur zu einem wirklich lauffähigen Ziel.

- [x] **Gefunden durch MESSUNG, nicht durch Gegenlesen: aarch64 `offen: smp`, 4 von 4 Läufen.**
      x86 blieb dabei grün — den Pfad gibt es dort nicht. Die Grundlinie (`00c8e73`) wurde
      nachgemessen und war grün, bevor die Ursache gesucht wurde; „vermutlich vorbestehend" wäre
      hier falsch gewesen. Dieselbe Lehre wie beim Audit-Code 7 nach dem D0-Umbau: **ein Umbau,
      der einen neuen Zustand einführt, muss jede Stelle mitnehmen, die über Zustände URTEILT —
      und gefunden wird das unter Last auf der Architektur, wo der Zustand oft vorkommt.**

- [x] **Der Modelltreue-Wächter hat den Umbau von selbst beanstandet** — und dabei **zwei
      Funktionen aufgedeckt, die er nie gesehen hatte**: `unpark` und `set_budget_blocked`
      schrieben Zustand über `parked` bzw. `budget_blocked`, und **beide Felder standen nicht in
      seinem Schreibmuster**. Er hielt zwei Funktionen für stumm, die es nie waren. Seit die Menge
      EIN Feld ist, fällt das nicht mehr durch.
      Die Abbildung ist jetzt eine **Projektion** (`blocked <-> reasons != {}`), keine
      Gleichsetzung; was das Modell damit *nicht* sagt, steht dort ausdrücklich.
      Fünf Selbsttest-Mutationen zielten auf Konstrukte, die der Umbau beseitigt hat — vier sind
      **umgeschrieben** (die Gefahr hat eine neue Gestalt), eine ist **zurückgezogen** mit Grund:
      ein Selbsttest für ein Verhalten, das es nicht mehr geben darf, wäre eine Ratsche in die
      falsche Richtung.

- [x] **Nebenbefund, vorgefunden:** `test-qemu.sh` prüfte `$LOG1` und kopierte `$LOG` — eine
      Variable, die es dort nicht gibt. Unter `set -u` brach der Block ab, und zwar **genau im
      Fehlerfall**: der Code, der geschrieben wurde, damit keine Fehlschlagsprotokolle mehr
      verlorengehen, verlor sie selbst.

---

**Die ursprüngliche Planung, zum Nachlesen:**

### Z24 (Plan vom 2026-08-09)
**Klasse:** Struktur · **Ersetzt** den ursprünglich als „Spurious-Wake-Vertrag" geplanten Schritt;
der Vertrag bleibt, aber als Beigabe, nicht als Kern.

**Warum das kein weiterer Waechter ist, sondern ein Umbau: es ist die DRITTE Instanz derselben
Klasse.**

| | |
|---|---|
| D9 | `blocked` trug drei Bedeutungen → `budget_blocked` abgespalten |
| Z22 P4 | `parked` als weiteres Bit dazu |
| 2026-08-09 | die Naht `thaw × park` reisst — verlorenes Wecken **und** verlorenes Pausieren |

Jede Abspaltung repariert die letzte Kollision und **stellt die nächste auf**. Der Checkpoint,
`thread_quiescence` und jeder künftige Grund (Signalzustellung aus Z16 steht schon auf der Liste)
müssen jeweils **alle** Bits kennen, und jede Stelle, die eines vergisst, ist ein neuer stiller
Pfad. Beim dritten Mal ist das Muster kein Zufall.

- [ ] **`blocked_reasons: BitSet`** (IPC · Budget · Pause · Park · …). Lauffähig **genau dann,
      wenn die Menge leer ist**. `unblock(grund)` entfernt **einen** Grund und plant **nur bei
      leerer Menge** ein.
      Damit ist `thaw` **per Konstruktion unfähig**, einen geparkten Thread zu wecken: es entfernt
      *Pause*, *Park* bleibt, die Menge ist nicht leer. Der gefundene Fehler wird nicht behoben,
      sondern **unformulierbar** — und die vierte Instanz kann nicht entstehen.
      Der Umbau ist klein: die Bits existieren, sie werden zur Menge zusammengezogen.

- [ ] **Die D9-Aussage wird vom Sonderfall zur Instanz einer Regel.** „`unpark` weckt keinen
      IPC-Wartenden" ist dann kein eigener Wächter mehr, sondern folgt aus „`unpark` entfernt
      *Park*, sonst nichts".

- [ ] **Verlorenes PAUSIEREN braucht die Menge — der Vertrag fängt es NICHT.** `while` statt `if`
      macht Warteplätze robust gegen überzählige Wecks. Aber „`PDCTL PAUSE` wird durch den `unpark`
      eines Geschwisters still aufgehoben" ist **kein** spurious wake, sondern der Verlust einer
      **Autoritätsentscheidung** — ein Debugger oder der Gruppenschnitt aus Z23 pausiert und sieht
      den Thread trotzdem laufen. Keine Zusicherung auf der Warteseite deckt das.
      Ohne die Menge braucht dieser Fall eine **eigene** Gegenprobe.

- [ ] **Der Spurious-Wake-Vertrag bleibt — mit gedrehter Begründung.** Nicht „das System erzeugt
      heute spurious wakeups, also legitimieren wir sie", sondern: der Vertrag ist
      **Verteidigungstiefe**, während die Grund-Menge dafür sorgt, dass der Kernel sie **nicht mehr
      systematisch erzeugt**. Ein Kernel, der Wecks gratis verteilt und sich auf die Schleifen
      seiner Nutzer verlässt, hat die Beweislast nur verschoben.
      Als Satz in `caprock-wait` (`while`, nie `if`) **und** als injizierte Gegenprobe: ein
      überzähliger Weckruf darf keine der elf Aussagen kippen.

- [ ] **Die Umrechnungstabelle — Stelle für Stelle, damit der Umbau eine ABSCHRIFT wird.**
      Aufgenommen am 2026-08-09 aus `crates/caprock-sched/src/lib.rs` (Zeilennummern vom Stand
      `3a5fd5e`; sie verschieben sich, die **Funktionen** nicht).
      **Der Punkt dieser Tabelle:** an 19 Stellen ist jeweils die *richtige* Begründung zu wählen.
      Eine mechanische Ersetzung `blocked -> reasons != 0` wäre genau der Fehler, den der Umbau
      beseitigen soll — sie schriebe die Mehrdeutigkeit in die neue Struktur hinein.

      | Zeile | Funktion | Wirkung | Grund |
      |---|---|---|---|
      | 743 | `block_current` | setzt | **vom Aufrufer**: der Weg wird von IPC *und* von `park_current` benutzt. Deshalb `block_current_mit(core, frame, grund)`; `block_current` bleibt als IPC-Fassung |
      | 757 | `switch_to` | setzt | `IPC` (der Aufrufer blockiert für das Rendezvous) |
      | 759 | `switch_to` | löscht | `IPC` am **Ziel** |
      | 783 | `unblock` | liest | `reasons != 0` |
      | 798 | `unblock` | löscht | `IPC` — **und nur einreihen, wenn die Menge danach LEER ist** |
      | 895 | `is_blocked` | liest | `reasons != 0` |
      | 909/910 | `pause` | setzt | `PAUSE` |
      | 1090 | `on_tick` | liest | `reasons == 0` (requeue) |
      | 1103/1107 | `on_tick` | setzt | `BUDGET` (Konto erschöpft) |
      | 1161 · 1218 · 1557 | `refill_depleted` | löscht | `BUDGET` |
      | 1179 · 1223 | Auswahl | liest | `reasons == 0` |
      | 1370 · 1438 | `audit` | liest | `reasons != 0` |

      Dazu: `budget_blocked` (46 Erwähnungen) wird zu `reasons & BUDGET`, `parked` zu
      `reasons & PARK`. **`park_wake` bleibt ein eigenes Bit** — es ist eine *Marke*, kein
      Blockadegrund, und es in die Menge zu ziehen wäre dieselbe Verwechslung noch einmal.
      `Z23` fügt später genau **einen** Wert hinzu: `FREEZE`.

      **Die Aussage, die den ganzen Umbau trägt**, steht in Zeile 798: eingereiht wird **nur bei
      leerer Menge**. Ohne sie ist die Menge bloss eine andere Schreibweise für dieselben Bits.

- [ ] **Der Scheduler-Modelltreue-Wächter prüft den Umbau mit** — `parked`/`park_wake` sind dort
      schon als „ausserhalb des Modells" eingetragen und müssen auf die Menge umgeschrieben werden.


### Einzelne erledigte Punkte, nach ihrem Herkunfts-Eintrag


#### aus: Z11. Boot-Image = Kernel + **eine** Manifestdatei; alles andere außerhalb und austauschbar

- [x] **Z11b. Das Manifest ist ein Autoritätsdokument — erledigt** (A-1.2/A-1.3, hier bis
      2026-08-07 nur nicht nachgetragen). Es ist Ed25519-signiert über die **gesamte** Nachricht
      und über `kernel_hash` an **dieses** Kernel-Image gebunden; die Prüfreihenfolge trägt der
      Typ (`SystemManifest::parse` liefert keine Einträge, die gibt es nur über `Verified`).
      Anti-Downgrade über `manifest_version`. Belegt als `manifest: ALL PASS` mit Negativfällen
      (manipulierte Kopie, Manifest für einen anderen Kernel).

- [x] **Z11e. Protokollversionen werden geprüft — erledigt** (A-4.4, hier nur nicht
      nachgetragen). `iface_gate` hält die `iface_version` beim ersten Laden einer `program_id`
      fest und weist jeden weiteren Ladevorgang mit anderer Version ab. Der Abweisungszweig ist
      über das Manifest allein **nicht erreichbar** (pro Boot gibt es genau ein Manifest) —
      deshalb füttert der Selbsttest `iface_record_or_check` direkt, statt einen ungeprüften Zweig
      stehen zu lassen.

- [x] **Z11f. Die Negativliste steht — erledigt** (`docs/invariants.md` §13, normativ). Sie nennt
      den Kernel selbst (das Manifest ist an sein Image gebunden — ein getauschter Kernel entwertet
      jede Signatur), IOMMU-Kontexte, aktive DMA-Regionen und gebundene IRQ-Zustellung; dazu die
      Grenze, die beim Schreiben dazukam: **eine gefärbte PD ist nicht hot-reloadbar, wenn alle
      Streifen vergeben sind** (Hot-Reload erzeugt die neue Instanz, bevor die alte verschwindet —
      beide brauchen gleichzeitig einen eigenen Streifen).


#### aus: Z14. Fremde Software ohne Gastschicht — bewertet 2026-08-09

- [x] **Gemessen am 2026-08-09, bevor geplant wurde.** `wasmi 0.31`, `no_std`, `opt-level="z"`,
      LTO, für `x86_64-unknown-none` gebaut und gelinkt:

      | | |
      |---|---|
      | Engine-Code | **`.text` 216 KiB + `.rodata` 17 KiB** |
      | Heap, triviales Modul (`main() -> i32`) | 5,1 KiB |
      | Heap, realistisches Rust-Modul (24 KB `.wasm`, Vec + sort) | **1 233 KiB** |
      | private Region einer isolierten PD (heute) | 2 MiB |
      | TCB des Kerns | 212 KiB `.text` |

      **Zwei Befunde daraus.** Erstens: die Engine ist **so groß wie der ganze Mikrokern** — aber
      sie liegt in einer PD, für andere Mandanten wächst die TCB um null. Zweitens: **es passt
      heute schon**, 233 KiB Code + 1,2 MiB Heap in 2 MiB — mit rund 0,5 MiB Luft. Der Preis der
      Engine ist ihr **Code**, nicht ihr Speicher; der Speicher gehört dem Gast (Linearspeicher).

      Folge für die Reihenfolge: Stufe 1 (Speicher-Server) ist **nicht** Vorbedingung für den
      ersten WASM-Schritt — wohl aber für `memory.grow` und für mehr als einen Gast.


#### aus: A3 — DAS KERNEL-PRIMITIV IST GEBAUT (2026-08-10). Was steht, was offen ist, und die Schwelle

- [x] **Eigene Cap-ARTEN, kein umgewidmeter Endpoint.** `ObjectKind::SyscallHandler { ep, pd,
      sidecar, len }` und `FaultHandler { .. }`. Ein gewöhnlicher Endpoint wird von
      `SYS_SETHANDLER` **abgewiesen** (`ERR_BADCAP`), ein `SyscallHandler` im Fault-Slot ebenso.
      `CALL` auf einer Handler-Cap ist abgewiesen (sonst gäbe ein Handler sich als Gast aus);
      `RECV`/`REPLY` sind erlaubt, und **`REPLY` hat dort eine Wirkung, die ein Endpoint nicht
      haben kann**: es lässt zusätzlich den Blockadegrund `HANDLER` fallen. Genau das ist der
      Unterschied, um dessentwillen es eigene Arten sind.
      Beide Arten sind über `domain_allows_kind` auf **TrustedSas** beschränkt — dieselbe Klasse
      wie `PdControl`/`Loader`, weil Z26 die Autorität ehrlich zusammenrechnet: die
      Persönlichkeits-PD **ist der Kernel des Gastes**.

- [x] **Der Aufruf braucht ZWEI Autoritäten von ZWEI Seiten.**
      `SYS_SETHANDLER(tcb_cap, syshandler_cap, faulthandler_cap)` (Nr. 19), beide im Cspace des
      **Aufrufers**, Tcb-Cap mit `WRITE` (nicht `READ` — die Bindung ändert, wer den Thread
      ausführt, das ist `KILL`-Klasse). Wer umschaltet, ist damit weder Gast noch Handler.

- [x] **Die Gast-PD hält nichts, und ihre Autorität wird verringert.** Die Weiche steht **ganz
      oben** im Dispatch, vor `YIELD`/`EXIT`/`SETHANDLER`. Ein gebundener Thread erreicht den
      Caprock-Kernel gar nicht mehr und kann sich insbesondere **nicht selbst entbinden**.
      Entbinden darf, wer die Tcb-Cap hält — und Entbinden braucht keine Handler-Cap („Autorität
      abzugeben darf nie an einer Erlaubnis hängen", dieselbe Regel wie `CDELETE`).

- [x] **Fail-closed, und benannt.** Handler weg → der Gast **faultet** mit `ERR_HANDLER_GONE`
      (11), er fällt **nicht** auf die native ABI zurück. Das ist als Kreuzprodukt-Test
      formuliert (`bindung_vorhanden_heisst_niemals_kernel`) und mit einer Mutation belegt, die
      genau diesen Rückfall wieder einbaut.
      **Und die Asymmetrie ist gebaut, nicht übersehen:** bei einem Syscall ist „der Kernel macht
      es" eine **Beförderung**, bei einem Fault eine **Herabstufung**. Deshalb zwei Funktionen
      (`weiche_syscall`/`weiche_fault`) und nicht ein Parameter mit zwei Bedeutungen.

- [x] **Nachtrag 3, BAUPFLICHT: das Zyklusverbot steht IM KERNEL.** `pruefe_bindung` geht die
      Handler-Kette vom Handler aufwärts; erreicht sie den Gast, wird abgewiesen
      (`ERR_HANDLER_CYCLE` = 10). **Gebaut ist die allgemeine Azyklizität, nicht die billige
      Absage aus Z26** („Threads einer PD mit Handler-Bindung dürfen selbst nicht gebunden
      werden") — die verböte auch **gestapelte Persönlichkeiten**, und die sind legitim
      (`gestapelte_persoenlichkeiten_bleiben_erlaubt`). Der Gang braucht keinen Hilfsspeicher und
      keine Besuchsmarken, weil der Graph **funktional** ist: eine PD hat höchstens einen Kernel.
      Sechs unterscheidbare Absagen, drei ABI-Codes, ein Zählregister (`HANDLER_URTEILE`) — denn
      `KetteZuLang` heisst „es gibt bereits einen Kreis ohne den Gast", also **Kernelfehler**, und
      der darf im Audit nicht mit einem Aufruferfehler verschmelzen.

- [x] **Nachtrag 3, zweite Hälfte: der Wartegrund ist von Tag eins in der Grund-Menge (Z24).**
      `BlockReasons::HANDLER` (Bit 4), **ein** Wecker (`handler_reply`), und
      `tools/redirect-negativ.sh` Q1 hält per Quelltext-Wächter fest, dass es genau **eine**
      Stelle im Baum gibt, die ihn entfernt — mit Sprechprobe (mit einer eingebauten zweiten
      Stelle findet der Wächter 2). Damit ist die von Nachtrag 3 vorhergesagte fünfte Instanz
      **unformulierbar** statt bewacht.

- [x] **Ein Rennen gefunden und geschlossen, das der Entwurf nicht genannt hatte.** Die Zustellung
      läuft über den vorhandenen Endpoint-Transport; im **kernübergreifenden** Zweig ruft
      `Endpoint::call` erst `unblock(server)` **mit IPI** und dann `block_current`. Der Handler
      kann auf seinem Kern losgelaufen sein und geantwortet haben, bevor der Aufruf zurückkommt —
      `handler_reply` liefe dann **vor** `mark_handler_wait`, entfernte einen Grund, den es noch
      nicht gibt, und der Gast hinge für immer, mit jedem Prüfer auf grün. Wörtlich das D11-Bild.
      Der Grund wird deshalb **vor** dem `call` gesetzt.


#### aus: Z23. Prozess-Freeze — geplant 2026-08-09, NICHT begonnen

- [x] **S1 — Zwei-Phasen-Stilllegung: GEBAUT und gemessen** (2026-08-10, `qgate : ALL PASS`,
      gattert in `all_done`).

      `Pd::quiescing` je PD, geprüft im Syscall-Pfad: `CALL`/`RECV` **aus** der PD heraus →
      `ERR_QUIESCING`, `REPLY` bleibt erlaubt. Kernel-API `pd_quiesce`/`pd_is_quiescing`.
      **TCB-Kosten wie geplant: ein Bit je PD und eine Prüfung.**

      **Gemessen wird die WIRKUNG, nicht ein Bit** — und das ist der Kniff, der die Zeile
      aussagekräftig macht: eine Ring-3-Sonde in einer PD mit geschlossenen Toren bekommt auf
      `RECV` **sofort** `ERR_QUIESCING`; nach dem Öffnen **blockiert derselbe Aufruf**, weil es
      keinen Sender gibt. Derselbe Syscall, anderer Ausgang. Ablesbar an der **Grund-Menge aus
      Z24** — der Umbau vom selben Tag liefert Z23 sein Messinstrument.
      Sechs Aussagen, darunter „das Öffnen hat wirklich etwas geändert" (ein Tor, das schon offen
      war, belegt nichts) und der **rohe Ergebniscode** (»abgewiesen« und »mit DIESEM Grund
      abgewiesen« sind zwei Aussagen).

      **Die Tore werden geschlossen, BEVOR der Thread zugelassen wird** — andersherum gäbe es ein
      Fenster, in dem das `RECV` noch durchginge, und der Test misste die Reihenfolge zweier
      Ereignisse statt der Eigenschaft. Dieselbe Lehre wie D0, eine Ebene höher.

      **Zwei Gegenproben:**
      * **M1** — Tor entfernt: rot. **Nicht isoliert** (drei Felder kippen), und das ist
        strukturell: ohne Tor blockiert die Sonde im ersten `RECV` und kann gar nichts
        aufschreiben. Eine Wirkung mit drei sichtbaren Folgen, keine drei Fehler.
      * **M2** — abgewiesen, aber mit `ERR_BADCAP` statt `ERR_QUIESCING`: **perfekt isoliert**,
        nur der Ergebniscode kippt (1 statt 8), die fünf übrigen Aussagen bleiben grün. Damit ist
        belegt, dass die Zeile den **Grund** liest und nicht bloss „abgewiesen".

      **~~Was NICHT gemessen ist~~ — seit 2026-08-10 GEMESSEN, in `pdthrd`.** Mit zwei Threads
      derselben PD (Z22 P2) gibt es die offene Transaktion: der Client hängt in seinem `CALL`,
      der Server hält den Reply-Token, **und in diesem Zustand** werden die Tore geschlossen.
      Gemessen: zweites `RECV` → **8** (`ERR_QUIESCING`), `CALL` → **8**, `REPLY` → **0** (`OK`),
      Client bekommt **42**. Die Tore gehen hier ausdrücklich **nach** dem Rendezvous zu (anders
      als bei `qgate`): vorher geschlossen gäbe es die offene Transaktion gar nicht, und der Test
      belegte wieder nur, dass ein Tor schliesst.

      **Gegenprobe (M2): auch `REPLY` gegattert** → `REPLY=8` statt 0, und der Client **hängt für
      immer** (`Client bekam u64::MAX, fertig=false`, sein Rundenzähler `0→0`), während
      `zweites-RECV=8` und `CALL=8` grün bleiben. Drei Konjunkte kippen aus **einer** Änderung,
      und diese Kausalkette **ist** die Zusicherung: genau der Deadlock, den der Entwurfssatz
      vorhersagt („sonst könnte ein Server seine offene Antwort nicht loswerden").

      **Gegenprobe (M1), nicht isoliert und aus einem strukturellen Grund:** das Tor nur für
      `CALL` gelten zu lassen macht das zweite `RECV` **blockierend** statt abweisend — der
      Server kommt nie zu `CALL`/`REPLY`, und alles danach fällt mit aus. Eine Sonde, die eine
      **Folge** von Syscalls abarbeitet, kann an einem blockierenden Glied nicht weiterzählen;
      dieselbe Kopplung wie bei der M1-Gegenprobe von `qgate`.


#### aus: Z22. Die vier harten Stellen aus Z21 — gebaut

- [x] **P4 — `wait_event`/`wake_up` ohne Kernelobjekt** (2026-08-09, `park : ALL PASS`).
      `SYS_PARK` legt schlafen, `SYS_UNPARK` weckt. Die **Warteschlange liegt in der PD** — eine
      gewöhnliche Liste; der Kernel kennt nur „schlafe" und „wecke Thread T".
      **Warum nicht Notifications:** ein Linux-Treiber schläft an vielen Stellen (jedes
      `wait_queue_head_t`, jede Completion, der bestrittene Zweig jedes Mutex). Je Warteschlange
      ein Kernelobjekt **und** ein Cap-Slot wäre die TCB-Rechnung genau falsch herum — und
      `Notification` fasst ohnehin nur **einen** Wartenden (dieselbe Kapazitätsform wie D11).
      **Die Weckmarke ist nicht optional.** Ohne sie ist „Bedingung prüfen (falsch)" → „parken"
      unterbrechbar, und ein dazwischen eintreffendes Wecken verpufft. Marke setzen und Blockade
      prüfen stehen deshalb **unter demselben Kern-Lock**.
      **Zwei Bits, jedes mit genau einer Bedeutung** (`parked`, `park_wake`) — die D9-Lehre:
      `unpark` weckt **nur**, wer wegen `PARK` blockiert ist; ein IPC-Wartender bleibt liegen.
      **Kosten:** ein Syscall, zwei Bits je TCB. Der unbelastete Weg („Bedingung ist schon wahr")
      ist **null Syscalls** — er fasst den Kernel gar nicht an.
      **Gemessen, sechs Aussagen** mit Positivkontrolle (das zweite `PARK` **muss** blockieren,
      sonst bestünde ein `PARK`, das nichts tut, die erste Aussage mit Bestnote). Zwei
      Gegenproben: Marke entfernt → `marke-wirkt=false`; Grundprüfung entfernt →
      **nur** `ipc-bleibt-liegen=false`.
      **Zwei eigene Fehler dabei, beide in der Fallenliste.**

- [x] **P3 — der DMA-Pool liegt in der PD** (2026-08-09, `crates/caprock-dma`, 13 Host-Tests).
      Der Kernel mappt die Region **einmal** und vergibt das IOVA-Fenster; alles danach ist
      Arithmetik **innerhalb eines bereits gewährten Fensters** und fügt keine Autorität hinzu —
      also gehört es nicht in die TCB. Der heisse Pfad ist **null Syscalls**.
      **Das Loch, das der Typ schliesst:** die Treiber-PD reichte zwei lose `u64` durch
      (`dma_cpu`, `dma_dev`), die per **Konvention** zusammengehörten — während der Kernel genau
      diese beiden Achsen seit jeher im Typ trennt. Ein `DmaBuf` trägt beide und lässt sich nicht
      falsch herum auspacken.
      **Die wichtigste Absage:** `map` (= `dma_map_single`) gibt für jede Adresse **ausserhalb**
      des Pools `None`. Einen Stapelpuffer für DMA anzumelden ist in Linux-Treibern ein
      verbreiteter Fehler; ohne diese Prüfung entstünde daraus eine IOVA auf fremden Speicher.
      **Befund unterwegs:** `caprock_virtio::Region::from_raw` prüft **nichts** — eine Region mit
      `dev == cpu` (Identität!) oder `dev == 0` liesse sich bauen und liefe scheinbar. Der Pool ist
      jetzt das **Tor** davor, fail-closed in vier Richtungen. Und die Kopie in den Datenbereich
      war gegen `shared_len` begrenzt — die Länge der **Quelle** als Schranke für das **Ziel**.
      **Latent**, weil `count` anderswo begrenzt war; die Schranke stand trotzdem an der falschen
      Grösse.

- [x] **`drv`/`blkdev`/`part`: gefunden, isoliert, behoben — und die Ursache war eine ZWEITE
      Client-PD.** (2026-08-10)

      **Der Weg dorthin, weil er die halbe Aussage ist.** Erst gehämmert (`tools/lade-haemmern.sh`):
      12 Läufe, **ein** Binary (Fingerprint `7aae3ed5531e`), 12× rot, **verhaltensgleich** — der
      einzige Unterschied zwischen zwei Protokollen war eine Thread-Nummer in einer PASS-Zeile.
      Damit war „Flattern **oder** veralteter Build" endgültig erledigt und die Suite als
      Bisect-Orakel brauchbar. Ehrlich zur Schranke: 12 Läufe schliessen eine Grünquote von 20 %
      mit p < 0,07 aus, 5 % nicht — „flattert nicht" heisst hier „nicht in einer Grössenordnung,
      die ein Bisect verdirbt", nicht „nie".
      Das Orakel urteilt über **die drei Zeilen**, nicht über die Schlusszeile: ältere Stände haben
      andere Prüfzeilen, und wer auf `== ALL PASS ==` bisectet, bisectet die Geschichte der Suite.

      **Erster schlechter Commit: `a159b6b`** („Z15/W1: wasmhost baut und LÄUFT"), `28cc05d` grün.
      Der Commit ändert **zwei** Dinge — 69 Zeilen Bring-up **und** einen sechsten Archiveintrag.
      Ohne Isolation wäre nur der Commit bekannt, nicht die Ursache; das Weglassen genau dieses
      einen Eintrags macht die drei Zeilen grün.

      **Die Ursache: `CLIENT_NTFN` war EIN Slot für eine ROLLE.** Die Behebung von A-6.3 lautete
      „drei Rollen, drei Badges, drei Ablagen" — und *Client* ist eine **Rolle**, keine Instanz.
      Mit `wasmhost` als zweitem Client zeigte dieselbe Zelle auf dessen Objekt; der `drv`-Ablauf
      wartete auf das Badge der **Dateisystem**-PD, das dort nie ankommt, blieb auf `DRV_STEP=0`
      stehen, und drei Prüfzeilen fielen aus — **ohne dass am Treiber irgendetwas kaputt war**.
      Der Kommentar an der Stelle beschreibt den Fehler wörtlich („die zuletzt geladene PD
      überschriebe die Ablage der früheren … Genau das ist beim Bau von A-6.3 passiert") und
      verhindert ihn nicht: die Behebung war eine Ebene zu flach.
      Behoben mit einer Ablage **je `program_id`** (dieselbe Lösung wie bei den vier versteckten
      Politiken aus A-5.4), Schranke = Höchstzahl der Manifest-Einträge (**hergeleitet**, also
      Überlauf strukturell unerreichbar), Überlauf trotzdem **gezählt und gegattert**
      (`clientntfn`, D11-Lehre). `client_notification()` ohne Argument gibt es **nicht mehr** —
      „die Client-Notification" war der Name einer Mehrdeutigkeit.
      Gemessen: `clientn : 3 Client-PD(s) mit EIGENER Ablage, 0 verloren`. **Drei** teilten sich
      bis dahin eine Zelle.

- [x] **Der zweite Befund war grösser: der PRÜFER meldete FAIL für Zeilen, die im Protokoll
      STANDEN.** (2026-08-10) Nach dem Fix blieben 9 rote Prüfungen, deren Zeilen nachweislich da
      waren. Ursache: `echo "$OUT" | grep -q MUSTER`. `grep -q` steigt beim **ersten Treffer** aus,
      `echo` bekommt SIGPIPE, und `set -o pipefail` (Zeile 5 jeder Suite) macht daraus rc=141 —
      also „nicht gefunden".
      **Das kippt erst oberhalb des Pipe-Puffers: gemessen zwischen 66 und 70 KiB Ausgabe.** Damit
      hing das Urteil der Suite an der **Grösse ihrer eigenen Ausgabe**; solange das Protokoll klein
      blieb, war das Grün Glück, und der sechste Archiveintrag hat es über die Kante geschoben.
      Betroffen waren **71 Stellen in drei QEMU-Suiten** und `tools/hang-stress.sh` — alle auf
      Here-Strings umgestellt (keine Pipeline ⇒ kein `pipefail`, kein SIGPIPE). Die sieben
      verbliebenen Pipelines haben **keinen** frühen Ausstieg (`grep -E`/`-c`/`-v` lesen bis EOF)
      und können die Form nicht auslösen.
      **Bewacht durch eine Sprechprobe des Prüfers selbst**, an bewusst **256 KiB** Eingabe und in
      beide Richtungen: vorhanden → PASS, abwesend → FAIL. An einer kleinen Eingabe wäre sie
      während des ganzen Fehlers grün gewesen. Gegenprobe gefahren: mit der alten Fassung meldet
      sie `PRUEFER DEFEKT` und bricht mit `exit 2` ab („kein Testergebnis, sondern ein
      Aufbauproblem").
      **Die Richtung ist die schlimmere Hälfte:** erfundene **Misserfolge**. Sie kosten kein
      Fehlerbild, sie **ertränken** es — neun falsche FAILs neben einem echten, und der echte war
      nicht mehr zu sehen. Bilanz: 30 → 1 Prüfung rot, und der Rest ist echte offene Arbeit
      (`wasm : SKIP`, s. Z15/W1).
      Dazu die eigene Falle beim Umbau: mein Ersetzer hielt ein `|` **innerhalb** eines Regex für
      ein Pipe-Zeichen und zerlegte `Grund (1|2|3)`. `bash -n` fand das **nicht** — die kaputte
      Zeile war syntaktisch gültig. Gefunden hat es erst eine Prüfung jeder geänderten Zeile auf
      „Here-String steht am Ende der grep-Invocation".

- [x] **`wasm`: ENTSCHIEDEN — und beide Hypothesen sind widerlegt, meine wie die älteste offene.**
      (2026-08-10)

      **Die Antwort:** `loader  : SYS_LOAD fehlgeschlagen -- Index 5, program_id 6, Grund
      NoResources`. `wasmhost` wird **nie geladen**. Es gibt keine PD, keinen Thread, keine Cap —
      und damit nichts, was eine Domänen-Politik beim Installieren degradieren könnte.

      **Widerlegt 1 (die Spur aus W1, seit Wochen offen): die Domänen-Policy in
      `install_cap_checked`.** Sie sagte für das gemessene Muster genau das Richtige voraus
      (TrustedSas meldet sich, UserLand nicht) — und ist trotzdem falsch. `a159b6b` fasst den
      Cap-Code gar nicht an (gemessen an der Dateiliste), und der Thread existiert nie. **Ein
      Muster, das zu einer Hypothese passt, ist kein Beleg für sie** — es ist der Anlass, sie zu
      prüfen. Die Spur gehört aus der Offen-Liste.

      **Widerlegt 2 (meine, vom selben Tag): „der Root-Task schweigt seit `a159b6b`".** Er hat nie
      geschwiegen. `a159b6b` schob ein `let b = client_notification()…` **zwischen**
      `let b = root_badge()` und die `root`-Zeile — seither druckte sie das Badge der
      **Client**-Notification. Daraus wurde ein „Root-Task lief: false" (während `pdcolor` und
      `ladepol` das Gegenteil belegten), ein vermeintlicher Kippunkt im Bisect und eine Hypothese
      über Cap-Fehlbindungen. Nach dem Aufheben der Verdeckung: `0x748454c4f`, `root-Badge
      angekommen: true`, `hello-Badge angekommen: true`.
      **Und das Badge trug die Antwort die ganze Zeit**: `init` setzt bei einem Ladefehler Bit
      `i+1`, für Index 5 also `0x40` — es steht in `0x748454c4f`. Die Diagnose lag einen
      Variablennamen entfernt.

      **Was den Fall entschieden hat, war die cap-freie Auskunft.** `SIGNAL` **ist** eine
      Cap-Invokation — die Formulierung „ohne jede Cap-Operation" war falsch und hätte den Cap-Pfad
      fälschlich entlastet. Wirklich cap-frei ist nur der **Scheduler**: existiert der Thread, ist
      er zugelassen, worin blockiert er? Antwort: er existiert nicht, und das Thread-Register hat
      5 Einträge statt 6. Das trennt „läuft nie an" von „läuft, und das Signal versandet" in einem
      Blick — und es hat drei geplante Sonden-Umbauten erspart.

      **Vier Prüfer waren an diesem einen Fall beteiligt und alle vier waren kaputt:**
      1. `wasm` schloss von Schweigen auf Abwesenheit → entscheidet jetzt an der Endowment-Tabelle.
      2. `root` las die falsche Variable **und** hiess falsch → `root-Badge angekommen`, eigenes
         Badge. „Lief" hat es nie gemessen.
      3. Das abgelegte Fehlerprotokoll war das des letzten (Negativfall-)Boots → der **Hauptboot**
         wird jetzt eigens abgelegt.
      4. `SYS_LOAD` verlor den Grund im `.ok()` → er wird **genannt** (`Grund NoResources`).

- [x] **BEHOBEN, und die Ursache war KEINE Ressource.** (2026-08-10) `wasmhost` hatte ein
      PT_LOAD-Segment auf einer **nicht seitenausgerichteten** VA (`0x2004_6700`).
      `vspace_map_page_at` weist eine krumme VA beim **allerersten** Aufruf ab — der Allokator
      wurde nie gefragt.

      **Der Bauweg:** `.bss : ALIGN(8)` in `programs/user.ld` und `user-x86.ld`. `wasmhost` ist das
      einzige Programm mit einem **schreibbaren** Segment und hat keine `.data`; lld verwirft die
      leere Ausgabesektion, das RW-PT_LOAD beginnt also bei `.bss` mit dessen 8-Byte-Ausrichtung.
      Auf **beiden** Architekturen — das Image war nirgends ladbar.
      Behoben mit `ALIGN(4096)`. **Nicht** kernelseitig die VA abrunden: `.rodata` reicht in
      dieselbe Seite, sie wäre erst RO und dann RW gemappt — ein W^X-Loch als „Behebung".

      **Alle vier Hypothesen sind widerlegt, auch die führende** („Seitentabellen kommen aus einem
      eigenen festen Vorrat, `total_free()` liest den falschen Topf"). Sie erklärte beide Zahlen
      zugleich und war trotzdem falsch: die Fragmentschranke greift bei `align == 4096` nie, die
      Farbmaske ist `None` (Politik 0), und der Zonen-Ausweich läuft bedingungslos. **Die Prämisse
      der ganzen Frage war falsch** — `mem_alloc_masked_anywhere` hat nie `None` gegeben, es wurde
      nie gerufen.

      **Und der Grund dafür war meine eigene, frisch „sprechfähig" gemachte Fehlerzeile.** Sie
      schrieb den Fehlschlag als „Speicher für eine Seitentabelle, **4096 Byte**" fest — und diese
      4096 war ein **Literal im Quelltext**, kein Messwert. Eine Diagnose, die eine Ursache
      **nennt, die sie nicht gemessen hat**, ist dieselbe Krankheit, die `NoResources` eine Ebene
      höher gerade erst behoben hatte. Sie hat den Fall ein zweites Mal in die falsche Richtung
      geschickt.
      Behoben strukturell: **nur der Allokator darf behaupten, es sei der Allokator gewesen** —
      `a3` markiert sich selbst, wenn es `None` gibt; sonst meldet der Ausgang
      `MANGEL_MAPPING_ABGEWIESEN` („KEINE Ressource, das Abbilden wurde abgewiesen; der Allokator
      wurde dabei NICHT gefragt"). Zwei Nachrechnungen derselben Größe wären die
      `iova_window_clear_of_msi`-Falle gewesen.

      **Gemessen, in beide Richtungen:**
      * `tools/segment-ausrichtung.sh` — zählt PT_LOAD mit `p_vaddr % 4096 != 0` über alle
        Programm-ELFs, **ohne QEMU**. Vorher 1 je Architektur, danach **0**; mit Sprechprobe.
      * Die sechs übrigen ELFs sind nach dem Eingriff **bit-identisch** (md5 verglichen) — das
        Risiko ist gemessen, nicht behauptet.
      * Gegenprobe: Ausrichtung zurückgedreht → dieselbe Stelle meldet jetzt **Code 10** statt
        Code 6. Zwei Lagen, zwei Zeilen.
      * `wasm : ALL PASS` mit allen vier Aussagen, `vollzahl: 6 von 6`, **Lade-Suite
        `== ALL PASS ==`** — zum ersten Mal seit dem 2026-08-03.

      **Was der Lader weiterhin NICHT prüft:** `ElfImage::parse_phdr` liest `p_vaddr` ohne
      Ausrichtungsprüfung. Er nimmt also weiter ein Image an, das er nie abbilden kann — die
      Absage fällt erst tief im Ladepfad. Eigener offener Punkt.

- [x] **P2 — mehrere Threads je PD: GEBAUT und gemessen** (2026-08-10, `pdthrd : ALL PASS`,
      gattert in `all_done`, eigene Prüfzeile in der Suite).

      **Die Ursache war ein Feld, kein fehlender Mechanismus.** `Pd::thread` trug **einen**
      Thread, `pd_of` war ein linearer Scan darüber. Eine zweite Bindung **überschrieb** die
      erste — der erste Thread verlor damit lautlos seinen ganzen Cspace und bekam bei jedem
      Syscall `ERR_NOPD`. Dieselbe Form wie „eine Ablage je ROLLE" bei `CLIENT_NTFN`: eine Zelle
      für etwas, das es mehrfach gibt.

      Ersetzt durch einen **Rückwärts-Index Thread-Slot → PD** (`PdTable::owner`, die vierte
      per-Thread-Tabelle neben `FpState`/`VSPACE_OF`/`KSTACKS`). Der Eintrag trägt die **volle**
      `ThreadId` (Slot **und** Generation) und die **Belegungs-Generation der PD** — ohne die
      zweite hätte der schnelle Weg eine Lücke, die der lineare Scan nicht hatte: eine PD wird
      frei, ihr Index sofort neu vergeben, und ein noch lebender Thread der alten PD zeigte auf
      die **neue**. Eine Beschleunigung, die eine Fremd-PD-Zuordnung erfindet, wäre schlimmer als
      der Scan.

      **Gemessen wird die WIRKUNG, an drei verschiedenen Grössen** (`pdthrd`, 16 Aussagen):
      * *Derselbe Cspace* — Server und Client sind zwei Threads DERSELBEN PD und reden über
        **denselben lokalen Cap-Slot** miteinander: `pd(server)=Some(1) pd(client)=Some(1)`,
        `gebundene-Threads=2`, erstes `RECV` → `OK` (bei nur einer Bindung wäre es `ERR_NOPD`).
      * *Getrennte Grund-Mengen (Z24)* — der Server parkt, sein Rundenzähler steht
        (`1078337 → 1078337`), **während** der des Clients läuft (`1120791 → 5560605`); nach
        `UNPARK` läuft er wieder. Ohne die zweite Hälfte wäre „steht" von „ist tot" nicht zu
        unterscheiden.
      * *Z23 S1* — s. den Eintrag dort.

      **Der Weg über das Manifest war gar nicht nötig.** Der alte Eintrag nannte ihn als
      „billigsten Weg" und den vollen 96-Byte-Eintrag als Blocker (`entry_len`-Bump auf der
      **signierten** Fläche). Beides entfällt: `admit_in_pd(pd, ..)` zweimal auf dieselbe PD
      genügt, sobald die Zuordnung nicht mehr in einem Feld der PD steht. Das Format bleibt
      unangetastet.

      **Eine Stelle, die das Gegenlesen fast übersehen hätte:** `domain_audit` prüfte die
      Isolationsregel (Code 3) über `thread_of(pd)`, also über den **ersten** Thread. Ein
      zweiter, global laufender Thread einer isolierten PD wäre damit unsichtbar geworden —
      genau die D0-Lehre („ein Umbau, der einen neuen Zustand einführt, muss jede Stelle
      mitnehmen, die über Zustände URTEILT"). Ersetzt durch `any_thread(pd, ..)` über den
      Rückwärts-Index, O(Threads) statt O(PDs × Threads).

      **Gegenprobe (M3): `attach_owner` weggelassen** → `pd(server)=Some(1)`,
      **`pd(client)=None`**, `gebundene-Threads=0`, `Bindungen ohne Rueckwaerts-Tabelle=6`,
      `pd_of` wieder linear (**40 019** Scan-Iterationen statt 0). Das ist wörtlich das alte
      Verhalten, und es reisst `pdthrd` **und** `vorrat`. Nicht isoliert, und die Kopplung ist
      strukturell: derselbe Index trägt beide Eigenschaften.

- [x] **Der teuerste O(n)-Pfad des Systems fiel dabei mit ab — und er stand nicht in C4.**
      `pd_of` löst bei jedem **cap-auflösenden** Syscall die aufrufende PD auf (CALL/RECV/REPLY/
      SIGNAL/WAIT/MAP; YIELD und PARK kehren im Dispatch vorher zurück) und tat das linear über
      alle `NPDS = 10 000` PDs. Die drei Stellen, die C4 nennt, laufen je Cap-Allokation bzw. je
      Thread-Tod — diese je Syscall. Jetzt O(1); gemessen `0` Scan-Iterationen bei 15 Aufrufen
      (Sprechprobe: eine Null allein wäre von „nie gefragt" nicht zu unterscheiden).
      **Nebenbefund:** der Rückfallpfad wurde von `UNPARK 0xDEAD_BEEF` ausgelöst — einer
      **EL0-erreichbaren** Eingabe. Ein Thread-Slot jenseits der Tabelle beantwortet sich selbst
      (`None`); ihn linear zu suchen hiess, den langsamsten Weg ausgerechnet für die
      Angriffseingabe zu nehmen.


#### aus: A1. Cache-/Timing-Seitenkanäle zwischen PDs

- [x] **Der reguläre Weg ist gefärbt — und die Entscheidung steht im Manifest, nicht im Code**
      (2026-08-07). Ein Programm mit `POLICY_EXCLUSIVE_STRIPE` wird stückweise aus **einem**
      Streifen geladen: Segmente, Stack, Seitentabellen, EL0-Kernel-Stack. Belegt als
      `pdcolor : ALL PASS` (5 Seiten in 16 von 512 Farben, gemessen an der Teardown-Buchhaltung).
      Details in [done.md](done.md). `spawn_isolated` bleibt ungefärbt und ist kernel-intern;
      der Produktpfad ist der Lader.

- [x] **Die Farbanzahl begrenzt die Anzahl gleichzeitig getrennter PDs** — erledigt mit B-4.2,
      hier bis 2026-08-01 nur nicht nachgetragen. `claim_stripe`/`release_stripe`
      (`kernel/src/colors.rs`) führen Belegung über `STRIPES_TAKEN`; ist kein Streifen frei, gibt es
      `None`, und die PD entsteht **gar nicht erst** — kein Ersatzsatz, keine „alle Farben"-Rückfallebene.
      `mask_for` (rundläufig, ungeprüft) steht nur noch im Test selbst; der Spawn-Pfad
      (`system.rs:2069`) nimmt `claim_stripe`. Der Test `stripe` deckt beide Ausgänge ab.

- [x] **Nur auf x86 gemessen** — behoben am 2026-08-01. Der fehlende Schlüssel war nur die
      *äußere* Hürde; B-2.1 erzeugt ihn inzwischen selbst. Die *innere* saß tiefer: `run_color` und
      `run_stripe_alloc` hatten ihren **einzigen Aufrufer in `arch/x86_64/bringup.rs`**. Die ARM-Suite
      hätte also auch mit Schlüssel nichts gemessen — sie rief nur `colors::report()` (Geometrie),
      nie den Test. Genau die Fehlerform, die dieses Projekt dreimal bezahlt hat, ein viertes Mal.

      Behandlung wie bei `dmatests.rs`: der Test liegt arch-neutral in `kernel/src/colors.rs` und
      wird von **beiden** Hochlaufwegen gefahren (aarch64 aus `threads::spawn_demo`, ganz am Anfang —
      `run_stripe_alloc` braucht den Ruhezustand). Die Druckstelle liegt **einmal** in
      `colors::report_color`; die vorherige Verdopplung war die Ursache des `FAIL`-statt-`FAILURES`-
      Fehlers. `color`/`stripe` stehen jetzt in der ARM-Abschlussbedingung und in `test-qemu.sh`.

      **Nicht** neu ist die Geometriemessung — die steht seit B-2.2 (2026-07-29) und wurde dort
      gegen drei CPU-Modelle geprüft, die *verschiedene* Werte liefern (`cortex-a72`/`a53` → 16,
      `max` → 32). Genau das belegt, dass `CCSIDR_EL1` gelesen und nicht eine Konstante
      zurückgegeben wird. Neu ist, dass die **Zuteilung** dort geprüft wird: bis heute lief auf
      aarch64 die Meldung, nicht der Test.


      **Der Umzug hat sofort zwei echte Fehler gefunden — beide nur auf aarch64 sichtbar, beide
      inzwischen BEHOBEN:**

      1. `region_bytes()` rechnete mit `caprock_mem::MASK_BITS` (64) statt mit der tatsächlichen
         Farbanzahl. Auf x86 (256 Farben) zufällig richtig; auf aarch64 (16 Farben) umfasst ein
         Streifen nur 4 Farben, eine 64-KiB-Region aber 16 aufeinanderfolgende Seiten — also jede
         Farbe, mehrfach. **Behoben** (Laufzeitrechnung `min(count(), MASK_BITS) / PARTITIONS`).
      2. `caprock_mem::stripe` teilte ebenfalls `MASK_BITS` auf statt `count()`. Bei 16 Farben
         umfasste Streifen 0 damit *alle* Farben und die Streifen 1–3 keine — und weil leere Mengen
         sich nicht schneiden, meldete der Selbsttest trotzdem „disjunkt". **Behoben am
         2026-08-02**: `stripe(i, n, colors)` nimmt die Farbanzahl als Parameter, mit Host-Tests
         gegen 16 **und** 256 Farben (ein Test nur gegen 256 wäre grün gewesen und hätte nichts
         belegt).

      **Drittens — der Test war auf aarch64 AUSGEHÄNGT, und das ist seit 2026-08-02 behoben.**
      An seinem alten Platz ganz am Anfang von `threads::spawn_demo` belegte und gab er Speicher
      frei, *bevor* die baseline-empfindlichen Tests ihre Ausgangswerte nehmen; danach fiel mal
      `captest`, mal `sched` durch. Der Ausweg war damals, ihn auszuhängen — und dabei
      `COLOR_OK`/`STRIPE_ALLOC_OK`/`PPROBE_OK` **hart auf `true`** zu setzen. Das war die
      schlechtere Hälfte: drei dauerhaft wahre Konjunkte in `all_done()`, keine Berichtszeile,
      kein Check in `test-qemu.sh` — die Abwesenheit war damit nicht bloß unbelegt, sie war
      **unsichtbar**.

      Die Lösung ist der **Platz**, nicht das Weglassen: die drei Tests hängen jetzt als letztes
      Glied der Testkette in `demo_report_then_idle` (`run_color_suite`), hinter `cross`, `strand`
      und `loadstop`. Danach nimmt keine Prüfung mehr eine Baseline. Gemessen (8 Läufe,
      `test-qemu.sh`): `color : ALL PASS`, `stripe : ALL PASS`, `pprobe : SKIP` (unter TCG kann die
      Positivkontrolle nicht tragen — kein echter Cache), `captest` und `sched` in allen acht
      Läufen unverändert grün, identische Ergebnissignatur. Damit ist A1 **zum ersten Mal auf
      aarch64 belegt** — und zwar auf der 16-Farben-Aufteilung, also genau dem Fall, den (2) falsch
      machte. Details in [done.md](done.md).


#### aus: C4. Effizienz bei tausenden Threads (lineare Scans beseitigen)

- [x] **`pd_of` — der teuerste O(n)-Pfad, und er stand in dieser Liste NICHT.** Behoben, s.
      [Z22 P2](#z22-die-vier-harten-stellen-aus-z21--gebaut). Er lief je **cap-auflösendem
      Syscall** über alle 10 000 PDs; die drei Stellen unten laufen je Cap-Allokation bzw. je
      Thread-Tod. Vorher/Nachher gemessen: **40 019 → 0** Scan-Iterationen (Gegenprobe: den
      Rückwärts-Index nicht anhängen).

- [x] **Registerkorrektur: „Kernel-Stacks 64 KiB je Thread" stimmt — für KERNEL-Threads.**
      Es sind **zwei** Grössen, und beide sind real: `STACK_SIZE = 64 KiB` (Kernel-Thread) und
      `USER_KSTACK_SIZE = 16 KiB` (EL1-Stack eines EL0-Threads). Die Korrektur „es sind 16, nicht
      64" ist damit nur zur Hälfte richtig — welche gilt, hängt an der **Art** des Threads, nicht
      am Datum des Eintrags. Gemessen an der Kurve: eine SAS-PD mit Kernel-Thread kostet
      **64 KiB**, und bei `-m 512M` ist genau das die Schranke (7212 Prozesse, freies RAM auf 0).

- [x] **Der UNTERBAU unter der Guard-Page steht (2026-08-10, x86): per-Kern-TSS + IST-Stacks.**
      Ohne ihn machte die Guard-Page das Bild *schlechter*: sie verwandelt den Überlauf in einen
      `#PF`, dessen Handler auf denselben kaputten Stack pusht -> `#DF` -> ohne eigenen Stack
      **Triple Fault ohne jede Ausgabe**. Gebaut: jeder Kern hat eine eigene TSS und drei eigene
      IST-Stacks (`#DF`/`NMI`/`#MC`, je 4 KiB); `#PF` bekommt ausdrücklich **keinen** (mit IST wäre
      er nicht mehr wiedereintrittsfähig). Belegt als `ist : ALL PASS` in beiden Suiten (die
      Vektoren werden ausgelöst und die Frame-Adresse zurückgelesen) und durch `tools/df-sonde.sh`
      — ein **echter** `#DF`, mit Gegenprobe ohne IST (dann stumm). `IST_STACK_BYTES` ist gemessen:
      **816 von 4096 B**. Details in `crates/caprock-hal/src/x86_64/gdt.rs`.


#### aus: C7. Die Kapazitätskurve — wo es WIRKLICH bricht (gemessen 2026-08-10)

- [x] **Der benannte Mangel gilt jetzt auch auf den `spawn_*`-Pfaden** (2026-08-10, nachmittags).
      Statt `keiner (der Fehlschlag lag NICHT an einer Ressource)` steht am Kurvenende die
      gemessene Ursache: SAS bei `-m 512M` **„Speicher fuer den Stack eines KERNEL-Threads
      (64 KiB), angefordert 65536 Byte, frei waren 40960"**; isoliert bei 3 GiB **„Speicher fuer
      die private Region einer isolierten PD, angefordert 2097152 Byte, frei waren 4706304"**.
      Zwei neue Codes (`MANGEL_KERNEL_THREAD_STACK`, `MANGEL_PRIVATREGION`),
      `create_vspace_masked` unterscheidet jetzt **ASID-Platz** von **Seitentabellen-Speicher**
      (zwei Töpfe in einer Funktion), und `vspace_map_user_region` trennt „Allokator sagte nein"
      von `MANGEL_MAPPING_ABGEWIESEN`.
      **Die Menge steht im TYP, nicht daneben:** `benannt_alloc(code, size, f)` reicht `size` an
      den Allokator weiter *und* meldet sie — die Zahl kommt genau einmal vor. Damit ist die
      Falle vom Vormittag (`mangel(MANGEL_SEITENTABELLE, 4096)` als Literal) strukturell zu.
      Bewacht als Prüfzeile `mangel` (gattert): eine **provozierte, wirklich abgewiesene**
      Anforderung auf `spawn_isolated_colored`, vorher **vergiftet** (`MANGEL_VERGIFTET = 255`),
      damit Schweigen ein eigener Ausgang ist. Gegenprobe gefahren: das Literal statt der Messung
      kippt **genau ein** Konjunkt (`gemeldet 4096 Byte (angefordert 16384 Byte)`), die Suite
      läuft in den Watchdog (`offen waren: mangel`).
      **Zwei Zahlen, die der Melder nebenbei sichtbar gemacht hat:** die isolierte Kurve endet
      mit **4,7 MiB freiem RAM** bei einer 2-MiB-Anforderung — die Schranke ist dort
      **Fragmentierung**, nicht Erschöpfung. Und `spawn_on_core_parked` **verlor bei jedem
      Fehlschlag des Thread-Slots seine 64 KiB Stack** (`?` ohne Rückgabe) — genau an der
      Kapazitätsgrenze, wo dieser Zweig läuft. Beides behoben bzw. benannt.

- [x] **Der Mangel-Sweep: aus „gegengelesen" ist „kann nicht schweigen" geworden** (2026-08-10,
      abends). Die `mangel`-Zeile belegte **eine** Meldestelle; die übrigen standen im Quelltext
      als „gegengelesen, nicht gemessen" — ehrlich und trotzdem ein Nullbefund.
      **Zuerst gezählt, denn „rund zwanzig" war falsch: es sind 31** (`system::MELDESTELLEN`,
      nachgezählt von `tools/mangel-stellen.sh` mit Selbsttest in beide Richtungen) — 17
      handgeschriebene `mangel(..)`-Aufrufe, die **schweigen können**, und 14 über
      `benannt_alloc`/`benannt_slot`, die es strukturell nicht können.
      **Provoziert wird mit einer Sperre im Allokator**, nicht mit einer Mutation je Stelle:
      `sperre_scharf(k)` lässt `k` Anforderungen durch und weist ab der `k+1`-ten jede ab; über
      wachsendes `k` wandert der Fehlschlag den Pfad entlang. Der Allokator sagt nein, den Weg
      danach geht der echte Code — die Sperre schreibt keinen Mangel-Code. Sie merkt sich die
      **abgewiesene Menge**, und die gemeldete Zahl wird gegen *diese* geprüft, nicht gegen eine
      Konstante im Prüfer, die mit einem Literal gemeinsam falsch sein könnte.
      Gemessen: **23 provozierte Abweisungen auf 6 spawn-Pfaden**, 5 von 6 bis zum Ende gefahren,
      `geschwiegen=0 · keiner=0 · Menge-nicht-aus-dem-Aufruf=0`. Abdeckung **11 von 31**; die
      Summanden gehen auf: 11 provoziert + 9 Platz-Töpfe + 10 Ladepfad + 1 (2917).
      **Drei Gegenproben, jede isoliert genau ein Konjunkt:** eine stumm gemachte Meldestelle →
      `geschwiegen=5`; eine Menge aus einem Literal → `Menge NICHT aus dem Aufruf=4`; und die
      dritte ist der eigentliche Befund (s. u.).
      **Der Befund, der größer ist als der Eintrag: die vergiftete Marke war seit ihrer Einführung
      tot.** Jeder `spawn_*`-Pfad ruft `mangel_zuruecksetzen()` als **erste** Anweisung — also
      zwischen dem Vergiften und der ersten Anforderung. „Der Pfad hat geschwiegen" war damit
      strukturell unerreichbar, während die Berichtszeile ihn wörtlich versprach. Gemessen mit
      derselben stumm gemachten Stelle: **mit** der Behebung `geschwiegen=5`, **ohne** sie
      `keiner=5` — und `keiner` heißt „lag an keiner Ressource", genau das, wovon die Marke
      trennen sollte. Seit heute überlebt die Marke das Zurücksetzen; `mangel_entgiften()` nimmt
      sie hinterher weg.
      **Nebenbefund, der eine Stelle als solche einordnet:** die drei
      `MANGEL_MAPPING_ABGEWIESEN`-Stellen (2917/4057/4120) können gegen einen leeren Allokator
      **nie** feuern — sie melden den Fall „der Allokator wurde NICHT gefragt". Wer sie prüfen
      will, braucht eine krumme VA/PA, keinen leeren Topf. Das ist kein Loch, sondern die
      Bedeutung der Stelle; sie steht deshalb als eigener Summand in der Bilanz.

- [x] **Der Seitentabellen-Topf hat eine Kurve — und die Zahl ist 5 Rahmen (20 KiB) je isolierter
      PD** (2026-08-10, nachmittags). Gemessen **an der Quelle** (`pt_rahmen` an allen acht
      Allokationsstellen, `pt_zurueck` an den fünf Freigabestellen), nicht als Differenz des
      freien RAM — eine Differenz misst Stacks, private Regionen und Segmente mit. Steht in der
      `vorrat`-Zeile und in jedem Kurvenpunkt.
      Gemessen bei `-m 3G`, isoliert: n=200 → 1000 Rahmen, n=400 → 2000, … n=1400 → 7000,
      Ende bei n=1477 → 7385 Rahmen = 29,5 MiB. **Streng linear, 20 480 Byte je Prozess.**
      Eine geladene PD (Lade-Suite) kostet **7 Rahmen = 28 KiB**; die SAS-Reihe fragt den Topf
      strukturell nie (0 Rahmen bei n=7206), und **das steht jetzt in der Zeile**.
      Bewacht als Prüfzeile `ptab` (gattert) mit vier benannten Konjunkten; der schärfste ist die
      **Bilanz** `raus >= zurueck` — es kann nichts zurückkommen, was nie herausgegeben wurde.
      Gegenprobe gefahren: eine einzige nicht mehr buchende Allokationsstelle ergibt
      `9 raus / 17 zurueck`, kippt **genau diesen** Konjunkt, und der Lauf endet im Watchdog
      (`offen waren: ptab`).
      Nebenertrag: die Hauptsuite schließt den Topf auf **17 raus / 17 zurueck** — die erste
      Leckprüfung, die dieser Topf je hatte.

- [x] **Die isolierte Reihe der Tabelle oben maß etwas anderes, als ihr Name sagt.** Der
      Kurvenarbeiter lag in `.text`; eine isolierte PD bekommt einen **EL0**-Thread, also
      faultete **jeder einzelne** an seiner eigenen Einsprungadresse. Gemessen: **228
      `el0-trap`-Zeilen in einem Lauf mit 224 isolierten PDs**, am Ende **0 belegte VSpaces** und
      ein Seitentabellen-Topf, der auf 15 Rahmen zurückgefallen war. „3040 isolierte Prozesse"
      hieß in Wahrheit „3040 mal eine PD angelegt, deren Thread sofort starb". Mit
      `kurven_arbeiter_el0` in `.user_text`: 220 statt 224 bei 512 MiB, **220 belegte VSpaces**,
      1100 gehaltene Rahmen, 3 Faults im ganzen Lauf.
      Das ist die Falle aus `CLAUDE.md` wörtlich — nur hat sie hier keine Prüfzeile rot gefärbt,
      sondern eine **Kapazitätszahl** erzeugt. **Die Zahlen der Tabelle oben (224/1504/3040) sind
      damit neu zu messen**; die Ersatzwerte lauten bisher 220 (512M) und 1477 (3G).


#### aus: E. DMA-Härtung — Rest

- [x] **x86-Fensterwahl — im Kern ERLEDIGT und RAM-UNABHAENGIG (gemessen 2026-08-03).** Die
      Befuerchtung im Eintrag („auf einer kleineren Maschine nicht") trifft **nicht** zu, und
      zwar aus einem Grund, den der Eintrag nicht nannte: das Fenster kommt als **feste Zusage**
      aus der HAL (`crates/caprock-hal/src/x86_64/iommu.rs:32`), nicht aus `RAM_TOP`. Es gibt
      nur zwei Faelle, beide enden oberhalb des Sperrbereichs — bis ~4 GiB springt die Basis auf
      `0xFF00_0000`, darueber liegt sie ohnehin hoeher.
      Gemessen ueber **fuenf RAM-Groessen** (256M / 512M / 1G / 2G / 2560M), alle `msi_clear=1`,
      `dmawin : ALL PASS`, rc=0. Von den drei Nebenbedingungen: **IR** beruecksichtigt
      (`GSTS.IRES=1`, `CFIS=0`), **ACS** beruecksichtigt (echtes `acs_enabled` je Funktion, die
      Gruppen-Aliasmenge geht vollstaendig in `DmaCtx::sids`; q35: 8 Geraete, 6 Gruppen),
      **RMRR nur teilweise**.
      Was bleibt, steht als **E-Rest 1/2/3** im Abschnitt D — der luegende Pruefer im schwachen
      Zweig, die RMRR-Faerbung, und der nicht ausfuehrbare 4-GiB-Zweig.
      Nebenbefund: **Punkt 6.4 unten fuehrt IR noch als offen**, waehrend `bringup.rs:2621`
      sagt „steht seit B-3.2". Eine Beschriftung, die neben der Sache herlaeuft — nachpruefen.

- [x] **x86-Fensterwahl (Herleitung)**: `0xFEE0_0000–0xFEEF_FFFF` ist als
      IOVA **unbenutzbar**. VT-d behandelt DMA-Requests dorthin als Interrupt-Nachrichten und
      schickt sie durch das Interrupt-Remapping statt durch die Second-Level-Tabellen — eine IOVA
      in diesem Fenster wird also *nicht übersetzt*, egal was in der Tabelle steht. Auf einer
      Maschine mit RAM oberhalb 4 GiB liegt die aus `RAM_TOP` abgeleitete Basis ohnehin darüber,
      auf einer kleineren nicht. Gehört als Bedingung an die Fensterwahl, zusammen mit IR, ACS
      und RMRR.

- [x] **Descriptor-Typestate ERLEDIGT (2026-08-03).** `crates/caprock-virtio/src/owned.rs`:
      `Owned<Driver>`/`Owned<Device>` mit unbewohnten Markern, `Region::carve` (monoton → keine
      ueberlappenden Puffer), `Completion` als Abschlussbeleg. `Queue::set_desc` ist **privat**;
      der einzige Weg ist `Queue::arm`, das den Puffer **by value** nimmt. Zurueck nur ueber
      `reclaim(buf, &Completion)` oder das benannte `reclaim_unproven` (das `blk` braucht, um
      nach einem Timeout die `0xff` im Statusbyte lesen zu duerfen). Alle drei Treiber migriert.
      Die Crate bleibt **abhaengigkeitsfrei** — nachgeprueft.
      **Belegt statt behauptet:** `tools/typestate-negativ.sh` — Positivkontrolle plus drei
      Negativfaelle, jeder mit **erwartetem Fehlercode** (E0382/E0599/E0624), damit ein
      Tippfehler kein Beleg ist; drei Mutationen kippen je genau ihren Fall.
      **Nebenbefund, im Code vermerkt:** `#[derive(Clone, Copy)]` auf `Owned` ist ein **No-op**
      (das Derive erzeugt die Schranke `S: Copy`, und die Marker sind unbewohnt). Die erste
      Mutation war dadurch wirkungslos und meldete faelschlich gruen.

- [x] **Descriptor-Typestate (Herleitung)** (`Owned<Driver>`/`Owned<Device>`) treiberseitig. Ausdrücklich
      **Ergonomie, nicht TCB**: eine Compile-Zeit-Disziplin innerhalb der Treiber-PD trägt an der
      Vertrauensgrenze nichts — sie fängt Fehler des Treiberautors, nicht das Verhalten eines
      kompromittierten Treibers. Lohnt trotzdem, weil „Puffer steht armiert in der Queue, ist im
      sicheren Code aber wieder adressierbar" real und häufig ist.


#### aus: D15. Der Kernel springt nach Adresse 0 — 2 von 600 aarch64-Läufen (2026-08-08)

- [x] **ENTSCHIEDEN (2026-08-08): D15 ist NICHT durch den D0-Umbau entstanden — er war vorher
      da.** Die Vorher-Reihe auf `2ef9ddb`, gleiche Bedingung, gleiche Größe:

      | Stand | Läufe | D15 | Rate |
      |---|---|---|---|
      | **vor** dem D0-Umbau (`2ef9ddb`) | 2000 | **3** | 0,15 % |
      | **nach** dem Umbau | 2000 | **6** | 0,30 % |
      | gepoolt | 4000 | 9 | **0,225 %** |

      `P(≥6 von 9 Treffern in einer Reihe, wenn kein Unterschied)` = **0,254** einseitig, ≈ 0,51
      zweiseitig. **Kein Hinweis auf einen Unterschied.**

      **Damit ist meine eigene Hypothese widerlegt** — „der neue Zustand *geparkt* trifft auf den
      Reap-Pfad" kann nicht stimmen, wenn das Bild ohne diesen Zustand genauso oft auftritt.

- [x] **Belegt, warum die aarch64-Reihe mehr wert ist — sie hat eine Regression gefunden, die
      56 895 x86-Läufe nicht sahen** (2026-08-07). `scale : FAILURES`, `sched_audit=7`: der
      Audit-Code „lauffähig und in keiner Liste" ist wörtlich der Zustand eines geparkten Threads.
      1 von 600 aarch64-Läufen. Behoben (`t.admitted` in der Bedingung), Details in `done.md`.


#### aus: D. Verifikation

- [x] **D5 erledigt (2026-08-02): der aarch64-Kernel hat einen Root-Task — und der Weg dorthin fand
      zwei Fehler, die nichts mit dem Manifest zu tun hatten.**
      Details in [done.md](done.md#d5-eine-rote-zeile-die-niemand-ansah).

      Der Anlass war die Zeile `root : FAILURES (NoManifest)`, die die Suite **gar nicht prüfte**.
      Behoben: `test-qemu.sh` baut ein signiertes Manifest mit genau einem `root`-Eintrag, `init`
      bekommt ein aarch64-Zertifikat (`certs/init-arm.cert`), `manifest_report()` wird auf ARM
      gerufen wie auf x86, und die Suite prüft `root`, `manifest` und die Modulzahl.

      **Was dabei herauskam, war wertvoller als der Anlass:**

      1. **Die Startmenge stand im falschen Dokument.** `boot_arg` gab dem Root-Task
         `archive.count()` — die Größe des *Behälters*, nicht der *Startmenge*. Auf x86 stimmte
         beides überein, weil dasselbe Skript Archiv und Manifest erzeugte. Auf aarch64 liegen zehn
         Testdienste im Archiv, die nicht zur Startmenge gehören; `init` hätte sie als Startmenge
         geladen — darunter `probe`, das gar kein ELF ist. Jetzt kommt die Zahl aus dem Manifest,
         und die dadurch nötige Zusage (Manifest-Einträge `0..n` liegen auf Archivpositionen
         `0..n`) ist eine **geprüfte Regel**: `RootTaskError::StartSetNotPrefix`, fail-closed.
         Negativkontrolle gefahren.
      2. **`loadstop` maß eine globale Baseline mit einer lokalen Sperre.** `local_irq_disable()`
         stellt einen Kern still; sieben andere und der Einsammler laufen weiter. Gemessen wurde
         `free 4204597248 -> 4204613632` — **16 KiB mehr** hinterher, ein fremder `free`, kein Leck.
         Die Prüfung stellt jetzt erst Ruhe fest und misst dann; ein Lauf, der nie ruhig wird,
         fällt **durch** („nicht messbar" ist kein bestandener Test).

      Nachgemessen wie gefordert: `RUNS=6` → 6/6 mit identischer Signatur, `== ALL PASS ==`.

- [x] **D8 BEHOBEN am 2026-08-03 (gemessen davor und danach): ein erschöpfter Thread kam über
      `unblock` zurück in die Ready-Liste und lief auf leerem Konto — ohne jede Cap.**
      Behebung in drei Teilen: Wächter **innerhalb** des `unblock`-Rumpfes, `!blocked`-Wächter in
      `refill_depleted`, neuer Audit-Code **9**. Nachgemessen: **jede** Wirkung (M1/M2/M3a/M5)
      auf 0, Positivkontrolle weiter bestanden, `M7.ticks_mit_budget = 6` — **kein Verhungern**.
      x86-Suite, Lade-Suite, Host-Tests, Verus + drei Wächter: alle grün. **500 Läufe** (5 Ströme
      à 100) mit **derselben Signatur wie vor der Behebung** (`e419003d625f`) — keine Regression,
      und zugleich der Beleg, dass die Suite diesen Fehler nie ausgelöst hat. Passt zu
      `audit() == 0`: niemand konnte ihn sehen.

      Der Rest des Eintrags bleibt als Herleitung stehen. **Zwei Punkte sind weiter offen** und
      stehen am Ende: der Donee-Zweig in `refill_depleted` und die Erreichbarkeit im laufenden
      Kernel.

      Werkzeug: `tools/sched-erschoepfung-messen.sh` (~3 s, 96 Messwerte, `--nur-echt` für den
      Einzellauf). Der **echte** `crates/caprock-sched/src/lib.rs` wird gelinkt — genau eine
      Zeile unterscheidet den Harness vom Original (`#![no_std]`), Stellvertreter ist nur
      `init_thread_frame`. Keine Zweitfassung des Schedulers.

      **Die Kette, und sie braucht kein Privileg.** Der PDCTL-Weg (PAUSE → RESUME) verlangt eine
      `PdControl`-Cap. Der zweite nicht:
      `switch_to` setzt beim IPC-CALL `blocked = true` am **Aufrufer** und spendet dem Server
      dessen Konto (`sc_donor`) → `on_tick` belastet über `acct = sc_donor.unwrap_or(cur)` und
      setzt `depleted = true` **am blockierten Aufrufer** → `reply` ruft `ops.unblock(caller)`
      (`caprock-ipc:653`) → `unblock` (`sched:658`) prüft `depleted` nicht →
      `enqueue_ready` (`sched:1099`) auch nicht.

      | Gemessen (M1 / M2) | Wert |
      |---|---|
      | in der Ready-Liste, **Liste gelaufen** statt `queued` gelesen | 1 |
      | dabei `depleted` / `remaining` | 1 / 0 |
      | `audit()` | **0** |
      | Alternative im Augenblick der Wahl bereit | 1 |
      | wird `current`, verbraucht eine volle Zeitscheibe | 1 |

      **Warum die Aussage das Paar braucht.** „Läuft auf leerem Budget" allein kann denselben
      Wert aus einem anderen Grund annehmen: ist **kein anderer Thread lauffähig** (M6), lässt
      `dequeue_highest` `current` stehen, und `depleted_count` wächst auch ohne jedes `unblock`.
      Deshalb trägt erst **„wird `current`" ∧ „eine Alternative stand im selben Augenblick
      bereit"**. Dieselbe Unterscheidung wie bei Z4 („der Wert stimmt" gegen „der Wert wurde
      geerbt").

      **Korrektur an der ersten Herleitung.** „`depleted_count` wächst bei jedem Tick" stimmt
      **nicht**: nach dem erneuten Erschöpfen setzt `on_tick` `requeue = false`, der Thread ist
      wieder off-queue und läuft nicht von selbst weiter. Der Zuwachs ist **+1 je
      PAUSE/RESUME-Paar** (M3a: 6 Runden → `depleted_count` 7 bei **einem** echt erschöpften
      Konto), `next_refill` verschiebt sich um genau die eine dabei verbrauchte Tick.
      Kontrolle M3b (dieselben Ticks ohne PAUSE/RESUME): Zähler bleibt 1, keine Drift.

      **Die Folge, die bleibt (M5).** Nach vollständigem Refill steht `depleted_count = 3` bei
      **0** wirklich erschöpften Konten. Der Zähler kehrt nie auf 0 zurück ⇒
      `if self.depleted_count > 0 { self.refill_depleted(); }` ist **dauerhaft wahr**, der
      O(n)-Scan läuft ab da in jedem Tick. Die Zusicherung im Kommentar darüber — „der Tick
      kostet nichts, unabhängig von der Tabellengröße" — fällt damit.

      **Gegenprobe.** Mit dem Wächter verschwindet **jede** M1/M2/M3a/M5-Wirkung; die
      M4/M4b/M6-Werte bleiben. Der Test misst also genau diese Ursache und trennt sie sauber von
      der anderen.

      **Zweiter, unabhängiger Fehler (M4): PAUSE hält nicht.** `refill_depleted` reiht **ohne
      `!blocked`-Prüfung** wieder ein. Gemessen: erschöpfen → PAUSE → 100 Ticks → der
      **pausierte** Thread (`blocked = 1`) wird `current` und verbraucht eine Zeitscheibe,
      `audit()` = 0. Dafür braucht es kein `unblock`. Steht ein höher priorisierter Läufer
      daneben (M4b), bleibt er mit `blocked = 1` in der Ready-Liste und `audit()` = **2** — das
      belegt zugleich, dass `audit()` sprechfähig ist und in M1 **geschwiegen** hat.

      **Behebung — drei Teile, und die naheliegende Fassung ist die falsche.**
      1. `if blocked && !depleted { … }` ist **schädlich** (gemessen als G-a): der Rumpf wird
         übersprungen, `blocked` bleibt `true`, das RESUME wird verschluckt. Zusammen mit der
         Behebung von M4 (G-d) misst man `ticks_mit_budget = 0`, `ende.blocked = 1` —
         **vollständiges Verhungern.** Die halbe Behebung, die schlimmer ist als der Befund.
      2. Richtig und modellgleich (G-b):
         ```rust
         if self.tcbs[s].blocked {
             self.tcbs[s].blocked = false;
             if !self.tcbs[s].depleted { self.enqueue_ready(s); }
         }
         ```
         Wer ihn danach einreiht, ist gemessen: `refill_depleted` (M7: 2 Refills, 6 Ticks mit
         echtem Budget, am Ende nicht blockiert). Kein Verhungern.
      3. Dazu G-c in `refill_depleted`, sonst bleibt M4 stehen:
         `_ => { if !self.tcbs[slot].blocked && self.current != Some(slot) { self.enqueue_ready(slot); } }`
      4. Und **B2 schließen**: `audit()` braucht im Queue-Lauf ein `if t.depleted { return <neuer
         Code> }`. Ohne das bleibt genau der Zustand unbeobachtbar, der die Aussage widerlegt —
         dieselbe Form wie die leere Event-Queue ohne `CD.R`.

      **Der Donee-Zweig ist am 2026-08-03 nachgemessen worden** — er trägt nicht, aber anders
      als vermutet: nicht „er weckt zu viel", sondern **er weckt den Falschen und lässt den
      Richtigen liegen**. Eigener Eintrag: **D9**.

      **Auch das noch nicht gemessen:** dass die Kette in einem *laufenden* Kernel eintritt.
      Gemessen ist die Zustandsmaschine am echten `Scheduler`; die Erreichbarkeit aus dem
      Syscall ist aus dem Quelltext argumentiert (`system.rs:6588`, `system.rs:2743`,
      `caprock-ipc:653`), nicht end-to-end ausgelöst.

- [x] **D11 BEHOBEN am 2026-08-04: der Überlauf einer Endpoint-Warteschlange ist BENANNT.**
      Neuer ABI-Code `ERR_EP_FULL = 9`; `TidQueue::enqueue` gibt `bool` und ist `#[must_use]`;
      alle sechs Aufrufstellen werten ihn aus. `call`/`recv` weisen ab **ohne zu blockieren**,
      `bind_receiver` und `migrate_owner` melden Misserfolg statt Erfolg — und `migrate_owner`
      prüft **vor** dem `take()`, sodass die Antwortpflicht beim alten Besitzer stehenbleibt
      statt gelöscht zu werden.

      **Warum ein dritter Code und nicht `ERR_QUIESCING`** (die Frage, die dieser Eintrag
      offenließ): „gibt es nicht" (nie wieder), „kommt gleich wieder" (nach dem Austausch) und
      „gerade kein Platz" verlangen verschiedene Reaktionen. Der dritte ist eine **Lastaussage**
      — er hängt an den anderen 32 Wartenden, kann sofort wieder gelten, und wer stumpf
      wiederholt, verschärft ihn.

      **Eine zweite Fundstelle, die dieser Eintrag nicht nannte:** `Notification::wait` hatte
      dieselbe Form bei Kapazität 1 — ein zweiter `WAIT` **überschrieb** den Wartenden, und der
      Überschriebene war danach in keiner Struktur mehr. Ebenfalls `ERR_EP_FULL`.

      **Belegt, nicht behauptet.** Das Verus-Modell ist mitgezogen (`dropped_*` → `rejected_*`,
      `send_gate`/`recv_gate` mit Code 3, **25 → 30 Beweise**), darunter `send_never_strands`:
      unter offenem Tor gibt es nur noch zwei Ausgänge, zugestellt/eingereiht **oder**
      abgewiesen-mit-Code. Der Modelltreue-Wächter fährt den echten Quelltext (93 → **99
      Fälle**, 28 → **35 Selbsttestfälle**) und führt ein **Hauptbuch der Gestrandeten**: nach
      einem Lauf über alle vier Überlaufwege muss es leer sein. Die Positivkontrolle sind fünf
      Mutationen, die D11 einzeln wiederherstellen — jede wird erkannt. Dazu die Prüfzeile
      `epfull` in der x86-Suite; eine Mutation macht sie rot und die Notbremse nennt sie
      (`bringup : offen waren: epfull`).

      Der Rest des Eintrags bleibt als Herleitung stehen.

- [x] **E-Rest 3b BEHOBEN am 2026-08-04: die Freiliste kennt den Zonenwunsch, statt ihn zu
      erraten.** `caprock_mem::alloc_below`/`alloc_colored_below` nehmen eine Obergrenze; Farbe
      **und** Zone werden dabei in EINER Entscheidung getroffen (dasselbe Argument wie Z8 für
      NUMA). Die drei Stellen mit einer *benannten* GiB-0-Bedingung (`alloc_dma_region`,
      `spawn_isolated`, `spawn_isolated_colored`) nennen sie jetzt und **suchen** statt einmal zu
      fragen und aufzugeben. Der Behelf im Speicherplan ist weg: hoher Speicher geht
      **vollständig** in die Freiliste (bei `-m 3G` vorher 0 von 1024 MiB, jetzt 1024 von 1024).

      **Der eigentliche Befund war ein anderer, als der Eintrag annahm.** Nicht nur die drei
      benannten Stellen hingen an der Belegungsordnung — „unten zuerst" war überhaupt ein
      **Zufall der Größenrelation**: Best-Fit nimmt das kleinste passende Fragment, und solange
      der obere Bereich zufällig größer war (4G, 6G), landete alles Unbenannte unten. Sobald das
      nicht mehr gilt, fällt der Ladepfad aus (gemessen: `drv`/`blkdev`/`fs`/`part` reihenweise
      rot bei 3G). Deshalb ist „unten zuerst" jetzt eine **ausgesprochene Politik** in
      `mem_alloc`/`alloc_colored` mit hohem Speicher als Überlauf — sie reproduziert das
      gemessene Verhalten, statt eine unbelegte Freiheit zu behaupten. `claim_user_kstack` griff
      als einzige Stelle am Wrapper vorbei und geht jetzt ebenfalls darüber.

      **Zwei eigene Fehler dabei, beide gemessen.** (1) `match MEM.lock() { … None => MEM.lock() }`
      hält den Guard bis zum Ende des `match` — der Ausweichpfad war ein **Selbst-Deadlock** auf
      einem Spinlock, und zwar genau der Pfad, der selten läuft (Lade-Suite blieb stehen).
      (2) Der Zähler für den Ausweich zählte zuerst *Versuche* statt *Wirkung* und meldete `1x`
      auf einer 512-MiB-Maschine, auf der es oberhalb 4 GiB gar keinen Speicher gibt — gezählt
      war in Wahrheit eine absichtlich übergroße Anforderung aus dem Farbtest. Dieselbe
      Verwechslung wie `rx_used` gegen „Daten angekommen".

      **Gemessen:** RAM-Reihe 512M · 2560M · 3G · 4G · 6G, Haupt- **und** Lade-Suite, alle
      `== ALL PASS ==`; Host-Tests mit **Positivkontrolle** (`ohne_zone_waehlt_best_fit_den_oberen_bereich`
      belegt, dass Best-Fit ohne Zone wirklich oben landet — sonst sagte der Test darunter nichts).

      **Was NICHT behoben ist und jetzt benannt gehört:** der 1-GiB-Deckel für Regionen mit
      **PD-eigener** Abbildung bleibt, und er liegt nicht im Allokator. `vspace_map_block` bildet
      **identisch** ab (VA == PA) und GiB 1..3 jeder isolierten PD hängen an geteilten statischen
      Tabellen — eine DMA-Region oder eine isolierte PD *kann* deshalb nur in GiB 0 liegen. Das
      ist eine Eigenschaft des VSpace-Layouts; es zu heben ist eigene Arbeit (s. E-Rest 3d).

- [x] **E-Rest 3d (Rest) BEHOBEN am 2026-08-04: der 1-GiB-Deckel für isolierte PDs ist weg.**
      Die private Region einer isolierten PD wird nicht mehr **identisch** abgebildet, sondern in
      ein **VA-Fenster ausserhalb der Identitätskarte** (`hal::mmu::ISO_USER_VA`; x86 `PML4[1]`
      = 512 GiB, aarch64 `L1[9]` = 9 GiB — beides Bereiche, in denen der Kernel nie identisch
      zugreift). Damit ist die Physadresse frei.

      **Der gemessene Deckel war 504** (Host-Test `gib0_deckel_ist_eine_zahl`: GiB 0 abzüglich
      der ersten 16 MiB, je 2 MiB) — nicht `MAX_VSPACES` (4096). Belegt, dass er fällt:
      `isohigh : ALL PASS` bei 3G/4G/6G, Regionen bei `0x1_02b0_0000` (4,04 GiB), und die
      Farbtrennung hält unverändert — sie ist eine Aussage über die **Phys**adresse und von der
      virtuellen Lage unberührt. Bei 512M/2560M meldet die Zeile `SKIP`, weil es dort keinen
      Speicher oberhalb 4 GiB gibt und die Frage **nicht entscheidbar** ist. Gegenprobe gefahren:
      die alte Zuteilung wieder eingesetzt → `isohigh : FAILURES`.

      **Zwei Annahmen dieses Eintrags waren falsch.** (a) „Der Preis ist der Verlust des
      2-MiB-Block-Fastpaths" — nein: der Fastpath hing nie an der Identität, sondern nur an der
      **Ausrichtung der VA**. Ein 2-MiB-Block bleibt ein Blockdeskriptor. (b) „Gehört mit B-4.1
      zusammen entschieden" — nein: A1 ist davon gar nicht betroffen, die Farbbedingung liegt auf
      der Physadresse. Der Preis sind zwei bis drei 4-KiB-Rahmen je isolierter PD für die
      Fenstertabellen, und die dürfen selbst oben liegen.

      Der Fehler, der das teuer gemacht hätte: `spawn_user` nimmt **einen** Wert für den EL0-SP
      **und** die Reap-Region, die beim Thread-Tod an den Allokator zurückgeht. Solange VA == PA
      galt, war das dieselbe Zahl; jetzt sind es zwei. Gemessen als `#PF cr2=0x80_0000_0000` im
      **Kernel** — der Reap-Pfad gab eine virtuelle Adresse als Physadresse frei. Behoben über
      das bereits vorhandene `spawn_user_at` (Ladepfad benutzt es seit A-2).

- [x] **VA==PA systematisch aufgeräumt am 2026-08-04.** Nach dem Fenster-Umbau war die Frage
      nicht mehr „geht das?", sondern „wo steckt dieselbe Annahme noch?". Ergebnis der
      Bestandsaufnahme — die Fläche ist klein und jetzt **aufgezählt**:

      * **Entfernt (die Identität war eine Altlast):** `spawn_isolated_native` bildete Code- und
        Stack-Frame identisch ab **und nahm die Physadresse des Code-Frames als
        Einsprungadresse**. Beide gehen jetzt ins Fenster (Plätze `SLOT_CODE`/`SLOT_DATA`), der
        Entry ist eine VA. Damit sind `vspace_map_region`/`vspace_map_code_region` **ohne
        Aufrufer** und gelöscht — kein toter Pfad, der später wieder benutzt wird.
      * **Unmöglich gemacht:** `Scheduler::spawn_user` nahm EINEN Wert für den EL0-Stackzeiger
        **und** die Reap-Region. Es ist **gelöscht**, nicht repariert; es gibt nur noch
        `spawn_user_at`, das beide verlangt. Der letzte Aufrufer (SAS-Thread, wo die Zahlen
        wirklich gleich sind) schreibt sie jetzt zweimal hin — die Gleichheit ist dort ein Zufall
        der Umgebung, keine Eigenschaft des Aufrufs.
      * **Benannt statt still (die Identität ist die Zusicherung):** neun Aufrufstellen bleiben,
        alle mit Grund in `tools/identitaet.sh` — `SYS_MAP`/`SYS_UNMAP` (der Aufrufer nennt eine
        Memory-Cap, also eine PA, und das ist die ABI), die Gerätefenster (ein Treiber rechnet mit
        Adressen aus der PCI-Enumeration, und die sind physisch) und zwei globale
        Kernel-Abbildungen ohne Subjekt.

      **Der Wächter ist der eigentliche Ertrag:** `tools/identitaet.sh` hält die Liste gegen den
      Quelltext. Eine neue identisch abbildende Stelle schlägt an und muss ihren Grund
      hinschreiben, bevor sie durchgeht. Mit Selbsttest in **beide** Richtungen: eine
      untergeschobene Stelle wird erkannt, ohne sie schweigt er wieder. Dabei prompt
      hereingefallen — der erste Anlauf suchte mit absoluten Pfaden, die auf keinen Listeneintrag
      passten, und meldete seine eigene Mechanik als Befund.

      **Was er NICHT kann, und das steht in seinem Kopf:** er sieht Aufrufe, keine Absichten. Ob
      eine erlaubte Stelle ihre Identität weiterhin zu Recht annimmt, prüft er nicht.

- [x] **E-Rest 3g BEHOBEN am 2026-08-05: die Bindung Stelle↔Grund hält rustc.**
      Mein Grund für die Vertagung („`pub(in path)` verlangt einen Vorfahren, `Va` liegt in
      `crate::addr`") ging am Punkt vorbei: der **Zeuge** braucht keinen Vorfahren. Jeder
      `Va::for_*` verlangt jetzt einen Typ mit privatem Feld aus dem Modul seiner Engstelle
      (`crate::system::*Witness`) — nennbar, aber nur dort herstellbar. Eine Zeile je Engstelle.

      **Der Beleg ist der Bau selbst:** `bringup.rs` konnte den Zeugen für das globale
      Gerätefenster nicht herstellen und scheiterte mit „argument #1 of type
      `KernelGlobalWindowWitness` is missing". Statt den Zeugen öffentlich konstruierbar zu machen
      (was ihn wertlos machte), wandert der Aufruf hinter `system::map_device_window_global` —
      Nebenertrag: die Schichtung stimmt danach besser.

      Die Tabelle Konstruktor→aufrufende Funktion im Wächter ist **ersatzlos entfallen**; er prüft
      nur noch, dass die Zeugen so gebaut sind (privates Feld, einer je Konstruktor) und dass jeder
      Konstruktor seinen verlangt. Ich hatte das als „Entwurfsarbeit" eingestuft — dieselbe
      Fehleinstufung, die `tail -1` neben Entwurfsarbeit geparkt hat.

- [x] **E-Rest 1 BEHOBEN am 2026-08-04: `iova_window_clear_of_msi` gibt im schwachen Zweig „in Ordnung" zurück, ohne
      urteilen zu können.** (2026-08-03, gemessen) `kernel/src/system.rs:3900`:

          let Some(first) = strong_window_base() else {
              return true; // kein starkes Fenster -> es gibt nichts zu ueberlappen
          };

      Der Kommentar stimmt nur, wenn es gar kein Fenster gibt. Tatsächlich ist es dann
      `[0, 512 GiB)` und **enthält** den Sperrbereich `0xFEE0_0000–0xFEEF_FFFF`. Sichtbar
      gemacht über eine HAL-Mutation: Suite `== ALL PASS ==`, `msi_clear = 1`, bei nachweislich
      weggefallener Trennung. Erreichbar ab ~512 GiB RAM.

      Genau das, was `docs/invariants.md` und CLAUDE.md als Entwurfsprinzip ausschließen: **ein
      Prüfer, der über Abwesenheit entscheidet, muss belegen können, dass er sprechfähig ist.**
      Hier gibt er Schweigen als Erfolg aus. Richtig wäre: im schwachen Zweig prüfen, ob das
      Fenster den Bereich enthält, und sonst „nicht entscheidbar" melden — nicht `true`.

      Dazu: **`DmaCtx::strong_window` wird geschrieben und nirgends gelesen** (`system.rs:3944`
      und `:3952` setzen es, `:4217` deklariert es, kein Lesezugriff). Die Unterscheidung
      „starkes Fenster ja/nein" ist erfasst und wird nicht verwendet — kein Bericht, kein
      Audit-Code.

- [x] **E-Rest 2 BEHOBEN am 2026-08-04: RMRR färbt die Gruppe.** (2026-08-03, gemessen)
      `GroupSpansUnits` färbt die ganze ACS-Gruppe, `Rmrr` nur die einzelne Funktion. Gemessen
      mit einem Sonderharness gegen den unveränderten `dmar.rs`: zwei Funktionen ohne ACS in
      einer Gruppe, RMRR auf 05.1 → `excluded[0] = None`, Aliasmenge `[0x28, 0x29]`,
      `audit() = 0`. Wird 05.0 zugeteilt, bekommt die RID des RMRR-Geräts einen Kontexteintrag
      in dessen Domäne. **Auf q35 unsichtbar (0 RMRRs), auf echter Hardware der Normalfall** —
      also genau die Sorte Lücke, die eine Emulation nie zeigt.

- [x] **E-Rest 3 BEHOBEN am 2026-08-04: der Kernel bootet mit RAM über 4 GiB (3G/4G/6G gemessen), und der Zweig der Fensterwahl trägt.** (2026-08-03)
      Ab `-m 3G` stirbt der Boot mit `#PF`, `cr2 = 0x0000_0070_0000_0014`, `rip` →
      `Transport::status` (`0x14` = `DEVICE_STATUS`). Sobald QEMU Speicher oberhalb 4 GiB
      anlegt, legt SeaBIOS die virtio-BARs bei `0x70_0000_0000` ab — und `mmu.rs:99` hat
      `MAPPED_GIB = 4`. Solange das steht, kann die Fensterwahl in diesem Bereich nicht
      gemessen werden.

- [x] **D10 BEHOBEN am 2026-08-03: der Refill-Weckelauf läuft nur noch, wenn jemand darauf
      wartet.** Gemessen in **Iterationen** (nicht Zeit — eine Iterationszahl ist eine
      Eigenschaft des Programms), Sprechprobe Tabellengröße 32 gegen 10 000.

      | | vorher | nachher |
      |---|---|---|
      | Ruhe (nichts erschöpft) | 0 | 0 |
      | 100 Refills in **einem** Tick, kein Donee | 1 000 000 | **0** |
      | dito, ein Donee | 1 000 000 | **10 000** |
      | dito über **Migration** | 1 000 000 | **0** |
      | `set_budget` | 10 000 | **0** |
      | je Konto ein eigener Donee | 1 000 000 | **1 000 000** |

      **Zwei Korrekturen an meiner eigenen Notiz.** Die `on_tick`-Zusicherung war nur **halb**
      kaputt: der Normalfall kostet wirklich 0. Kaputt war der Fall „irgendein Konto erschöpft",
      und die 10 000 kommen aus der **äußeren** Schleife, die schon vor der Donation da war.
      Und der Einzelfall wird durch den Zähler nicht billiger.

      **O(n²) ist erreichbar** — mein Einwand („pro Tick erschöpft nur eins") stimmt und reicht
      nicht, weil die **Periode** die zweite Hälfte der Summe ist. Weg 1: Konto *i* erschöpft im
      Tick `m+i`, Periode `Z−m−i` → alle Refills fallen auf Tick `Z`; gemessen 100 Refills in
      *einem* Timer-Interrupt. Weg 2 braucht gar keine Periodenwahl: `attach_migrated` setzt
      `next_refill = now + period`, mehrere erschöpfte Threads, die im selben Tick ankommen,
      refillen gemeinsam — die Periode muss nur **geteilt** sein. Ausgelöst vom **Lastausgleich**,
      nicht vom Mandanten. Weg 1 ist cap-vergittert, Weg 2 nicht.

      **Der Zähler hat, was `depleted_count` heute Morgen fehlte:** eine **unabhängige
      Nachzählung** in `audit()` → **Audit-Code 10**, wenn er von der Tabelle abweicht. Genau
      diese Nachzählung fehlte damals, und deshalb log er. Bewegt wird er ausschließlich über
      `set_budget_blocked(local, an)` (2 Erhöhungen, 4 Senkungen, 3 Bulk-Nachführungen, alle im
      Kommentar aufgezählt). Die Lügen-Mutation „Senken weg" — die D8/M5-Form — schlägt an
      allen vier Auflösungswegen an, die Positivkontrolle fällt durch.

      **Was bleibt:** trägt jedes Konto einen eigenen blockierten Donee, ist k·n unverändert.
      Ein Zähler kann „wer zeigt auf mich?" nicht beantworten; dafür bräuchte es eine
      Donee-**Liste** je Konto.

- [x] **D10 (Herleitung) Der Refill kostet seit H-b einen VOLLEN Tabellendurchlauf je
      aufgefülltem Konto — gelesen, nicht gemessen.** (2026-08-03)

      Die neue Schleife (`refill_depleted`, `for d in 0..self.tcbs.len()`) liegt **innerhalb** der
      bestehenden Schleife über die Thread-Tabelle. `Slab::len()` ist die **Tabellengröße**, nicht
      die Belegung — laut A-3.4 sind das **10 000** Slots.

      | | |
      |---|---|
      | **sicher** (steht im Code) | ein Refill kostet ab jetzt 10 000 Iterationen statt einer konstanten Zahl |
      | **nicht belegt** | ob „viele Konten refillen im selben Tick" (→ O(n²), 10⁸ Iterationen in einem Timer-Interrupt) erreichbar ist |

      Dagegen spricht, dass pro Kern und Tick höchstens **ein** Konto erschöpft und `next_refill`
      an die Erschöpfungszeit gekoppelt ist — die Refills verteilen sich von selbst. Gemessen ist
      das nicht.

      **Beschädigt ist eine ausgesprochene Zusicherung.** Der Kommentar in `on_tick` sagt:
      „Refill-Scan NUR, wenn überhaupt ein Konto erschöpft ist (Normalfall: keins → der Tick
      kostet nichts, **unabhängig von der Tabellengröße**)." Das gilt so nicht mehr.

      **Die naheliegende Abkürzung funktioniert nicht:** „nur laufen, wenn `sc_donee.is_some()`"
      reißt D5 sofort wieder auf — dort ist `sc_donee` gerade `None`, während Donees warten. Das
      ist der ganze Punkt von H-b. Es braucht einen eigenen Zähler `budget_blocked_count`.

      **Bewusst NICHT sofort gebaut.** Zähler haben am selben Tag schon einmal gelogen
      (`depleted_count`, D8/M5), und ungemessenen Code nachzuschieben wäre genau der Fehler, den
      D8 und D9 vermieden haben. Reihenfolge: erst ein Messfall mit großer Tabelle, der die Kosten
      **zeigt**, dann der Zähler, dann die Gegenprobe.

      Zwei kleinere Stellen derselben Änderung, ebenfalls neu O(n) statt O(1), aber nicht im
      Tick-Pfad: `record_zombie` (je Thread-Tod) und `set_budget` (nur im `was_depleted`-Zweig).

- [x] **D9 BEHOBEN am 2026-08-03 (H-b): der DONEE-Zweig — fünf Befunde, alle gemessen und alle
      behoben.** Der Kern der Behebung ist ein neues TCB-Bit `budget_blocked`, das den **Grund**
      einer Blockade trägt: bis dahin teilten sich „pausiert", „wartet in IPC" und „wartet auf
      Konto-Refill" ein einziges `blocked`, und daran hingen D1, D4, D5 und D7. Dazu wird die
      Spende als **Stapel** behandelt — `refill_depleted` weckt alle mit
      `sc_donor == slot && budget_blocked` statt des einen `sc_donee`, den der zweite CALL
      überschreibt und der innere REPLY löscht (das ist D5).

      | Messgröße | vorher | nachher |
      |---|---|---|
      | `D1.pausierter_ist_current` (PAUSE hält) | 1 | **0** |
      | `D4.ticks_donee_lief` (Konto stirbt) | 0 | **299** |
      | `D5.ticks_mid_lief` (verschachtelte Spende) | 0 | **6** |
      | `D6.wird_current` / `zaehler_luegt_um` | 1 / 1 | **0 / 0** |
      | `D7.ticks_donee_lief` | 0 | **9** |
      | `M1`/`M4`/`M5` (D8 — keine Regression) | 0 | **0** |
      | `M7.ticks_mit_budget` (kein Verhungern) | 6 | **6** |
      | Positivkontrolle P2 | bestanden | **bestanden** |

      x86-Suite, Lade-Suite, Host-Tests, Verus + drei Wächter: alle grün. **500 Läufe** (5 × 100)
      mit derselben Signatur `e419003d625f` wie vor D8 und vor H-b.

      **Was H-b NICHT ist: ein Verus-Beweis.** `budget_blocked` ist als **außerhalb** eingetragen,
      weil es vollständig zum Spenden-Mechanismus gehört und Donation laut ADR 0019 außerhalb des
      Modells liegt. Die Kehrseite gehört benannt: `roundrobin_no_starve` und `no_lost_thread`
      gelten damit für eine Welt **ohne** Spende — und genau in der Spende lagen D4, D5 und D7.
      Ein gestrandeter Donee ist `depleted == false`, in keiner Liste, `audit() == 0`; er fällt
      durch **jede** dieser Zusicherungen hindurch, weil sie über ihn gar nicht sprechen. Der
      Beleg für H-b ist `sched-erschoepfung-messen.sh`, nicht Verus. Wer das ändern will, braucht
      ein Modell **mit** Donation.

      Neuer Aufwand daraus: **D10** (der Refill kostet jetzt einen vollen Tabellendurchlauf).

      Die Herleitung und alle Zahlen der fünf Befunde stehen unverändert darunter.

- [x] **D9 (Herleitung — kein offener Punkt, sondern das Protokoll zur Behebung darüber.)
      Der DONEE-Zweig in `refill_depleted`, gemessen am 2026-08-03, fünf Befunde.** Werkzeug: `tools/sched-erschoepfung-messen.sh` (jetzt **208 Messwerte**, die
      D-Reihe kam dazu). Der echte `crates/caprock-sched/src/lib.rs` wird gelinkt; Mutationen
      nur auf Kopien.

      **Positivkontrolle zuerst (P2):** der gesunde Fall trägt — Konto erschöpft, während der
      Donee läuft; der Donee ist *nachweislich* auf das Konto-Budget geblockt (`blocked = 1`,
      **nicht** in der Liste, `audit() = 0`); nach 100 Ticks Refill wird er `current`, während
      eine Alternative bereitsteht, und der nächste Tick belastet ein Konto **mit** Budget.
      Ohne diese Zeile wäre keine Zahl darunter etwas wert.

      | Befund | gemessen |
      |---|---|
      | **D1 (F1) PAUSE hält nicht** — gesetzt, während das Konto noch Budget hat (`konto_depleted = 0`, `depleted_count = 0`), also nachweislich die PAUSE und nicht das Budget | nach dem Refill `blocked = 0`, **wird `current`**, Alternative stand bereit, verbraucht Budget, `audit() = 0` |
      | **D5 verschachtelte Spende** (fs → Blockdienst → Treiber ist genau diese Form) — der zweite CALL überschreibt `sc_donee`, der innere REPLY (`end_donation`) löscht es; der äußere Server behält seinen `sc_donor` | beim Erschöpfen ist `sc_donee = None` → `_`-Zweig → **niemand weckt ihn**: 0 Ticks in 3 Perioden, `blocked = 1`, `audit() = 0`. Client **und** Server dauerhaft fest. **Kein Privileg nötig: zwei CALLs und ein REPLY.** |
      | **D4 das Konto stirbt**, während der Donee auf sein Budget geblockt ist | `record_zombie` löst `sc_donor`, lässt `blocked = 1` — mit dem Konto verschwindet der einzige Wecker: 0 Ticks in 3 Perioden, `audit() = 0` |
      | **D7 `set_budget`/`bind_sched_context` auf das Konto** räumt `depleted` weg — und damit den Anlass des Refills | 0 Ticks in 3 Perioden. Die vorhandene Prüfzeile `strand` deckt genau diese Strandung ab — aber nur für **einen einzelnen** budgetierten Thread, nicht für seinen Donee |
      | **D6 `unblock` prüft das falsche Konto** — der D8-Wächter fragt `self.tcbs[s].depleted`, belastet wird aber `sc_donor.unwrap_or(s)` | RESUME am Donee → in der Liste, wird `current` mit bereitstehender Alternative, **ein voller Tick auf leerem Konto**, `depletions +1`, `depleted_count` driftet +1 (M5-Form) |

      **F2 (D2): an der Scheduler-Schnittstelle auslösbar, im Kernel nicht.** Ein Donee mit
      **eigenem** erschöpftem Konto wird vom Zweig eingereiht — und **Audit-Code 9 meldet es**
      (`audit() = 9`), der Zustand ist also nicht unbeobachtbar. Über `caprock-ipc` ist er
      derzeit nicht herstellbar: Donee wird man nur über `switch_to` aus `call`, und das Ziel
      kommt aus der Empfängerliste — dort landet nur, wer `recv` ausgeführt hat, also gelaufen
      ist, also nicht erschöpft war. **Offene Entwurfsfrage:** ob Code 9 einen Thread ausnehmen
      muss, der auf einem **fremden** Konto läuft.

      **F3: nicht auslösbar, und der Grund ist benannt.** Ein veralteter `sc_donee` entsteht
      nicht — Tod löscht ihn (`record_zombie`), Migration wird für Donee *und* Konto verweigert
      (`detach_for_migration`). Sprechprobe dazu: ein Thread **ohne** Spende lässt sich sehr wohl
      herauslösen. Die Gegenrichtung ist der Befund: `sc_donee` wird **zu früh** gelöscht (D5).

      **Behebungsvorschlag, gemessen (H-b im Werkzeug): der GRUND der Blockade gehört
      mitgeschrieben.** Ein Bit `budget_blocked` im TCB; `on_tick` setzt es nur, wenn der Donee
      nicht schon aus einem anderen Grund blockiert war; `refill_depleted` weckt **alle**
      Threads mit `sc_donor == slot && budget_blocked` (die Spende ist ein **Stapel**, `sc_donee`
      nur seine Spitze); `pause` übernimmt die Blockade; `unblock` lässt eine Budget-Blockade
      stehen (gefahrlos, weil der Wecker **benannt** ist) und schiebt eine Blockade auf ein
      leeres fremdes Konto in denselben Zustand; `record_zombie`/`set_budget` lassen ihre Donees
      frei. Gemessen: D1/D4/D5/D6/D7 auf 0 bzw. „läuft wieder" (D4 299 Ticks, D5 6, D7 9),
      Positivkontrolle bestanden, **M1…M7 unverändert** (keine D8-Regression).

      **Und die Frage aus D8 noch einmal gestellt:** der naheliegende Wächter ist wieder der
      falsche. `if !blocked && current != Some(d)` — wörtlich aus dem `_`-Zweig übertragen —
      **fällt in der Positivkontrolle durch**: der Donee ist an dieser Stelle *immer* blockiert
      (`on_tick` hat ihn gerade blockiert), also weckt ihn niemand mehr. Verhungerungsprobe zu
      H-b: nach RESUME 4 Ticks **mit** Budget (echt: 3).

      **Nebenbefund am Werkzeug selbst:** die Gegenproben von D8 waren nach ihrer eigenen
      Behebung stumm abgebrochen („der Anker passt nicht mehr") — der Lauf am echten Quelltext
      blieb grün und meldete das nicht als Fehler. Die Fassungen heißen jetzt `V0` (Stand vor
      D8, als Sprechprobe der Mechanik: die alten Befunde tauchen dort wieder auf), `H-a`, `H-b`.

      **Nicht gemessen:** dass D1/D5 in einem *laufenden* Kernel eintreten. Die Aufruffolgen
      sind die von `caprock-ipc` (`call` → `switch_to`, `reply` → `end_donation` + `unblock`,
      `recv` → `block_current`) und `system::freeze_thread` (das **vor** der Quiescence-Prüfung
      `pause` absetzt und die PAUSE bei `Busy` stehen lässt), aber nicht end-to-end ausgelöst.

- [x] **D7 ERLEDIGT am 2026-08-03: das IPC-Modell sagt jetzt, was gilt.** Nicht „mehr Beweise",
      sondern ehrlicher: 3 Felder → **9**, 4 Operationen → **13**, 5 Beweise → **24**
      (**25 verified, 0 errors**).
      * `send_no_loss` traegt die Kapazitaetsschranke als **Vorbedingung**, und der Verlust
        darueber ist **bewiesen** statt weggelassen.
      * `ep_inv` ist auf das abgeschwaecht, was haelt (`quiescing || ep_inv_strong`); die
        Aufrufdisziplin steht als Vorbedingung **im Modell** statt in einem Kommentar, und der
        Bruch der starken Fassung ist bewiesen.
      * neu: `reply` (Token-Konsum, kein Doppel-Reply, `token_inv`), das Tor
        (`gate_distinguishes`, `gate_rejects_are_noops`, `reply_not_gated_by_quiescing`),
        `queue_cap()`.
      **Bewusst NICHT bewiesen, weil falsch:** `ep_inv_strong` als erhalten, unbedingte
      Verlustfreiheit, Token-Erhalt — ihr **Gegenteil** ist bewiesen. Liveness ebenfalls nicht:
      das ist eine Scheduler-Eigenschaft und wird gemessen, nicht bewiesen.
      Waechter: **93 Prueffaelle** (32), **28 Selbsttestfaelle** (12), darunter 14 Sprechproben
      **am Modell allein** — ohne die bliebe ein auf `true` aufgeweichtes `ep_inv`/`token_inv`
      unbemerkt. Ein veralteter Anker ist jetzt ein **harter Fehler**; im Baseline-Lauf ging
      „die kosmetische Mutation hat nichts geaendert" vorher direkt in „still" ueber — die
      Negativkontrolle bestand, **weil** nichts geaendert wurde.
      **Was der Beweis jetzt traegt:** das Protokoll eines Endpoints unter der Aufrufdisziplin
      des Kernels — 8 von ~15 Operationen, alle 6 Zustandsfelder. **Nicht** getragen: Tod,
      `rebind_server` (ausgerechnet *die* A-4.1-Operation), Nebenlaeufigkeit, Liveness. Die drei
      dabei gefundenen Fehler (**D11**) liegen alle genau dort, wo Modell und Wirklichkeit sich
      nur am Rand beruehren.

- [x] **D7 (Herleitung) Das IPC-Modell trägt für den echten Endpoint nur einen Ausschnitt — gemessen, nicht
      geschätzt** (2026-08-03, `tools/verus-modelltreue-ipc.sh`, 32 Fälle · 12 Selbsttestfälle).

      Der Wächter fährt den **echten** `caprock-ipc`-Quelltext gegen ein aus der Beweisdatei
      **übersetztes** Modell (nicht abgeschrieben) und misst die Entsprechung unter einer
      hingeschriebenen Abbildung. Ergebnis: `call`/`recv` entsprechen `send`/`recv` in beiden
      Zweigen, über beide Kern-Pfade, FIFO-treu, über Ketten hinweg. Und drei Löcher, jedes
      einzeln nachgemessen:

      **(a) `ep_inv` gilt am echten Endpoint NICHT.** Über die *öffentliche* Schnittstelle sind
      Zustände mit wartenden Sendern UND geparkten Empfängern erreichbar: `bind_receiver` sieht
      die Sender-Queue nicht an, und `migrate_owner` reiht den Aufrufer wieder als Sender ein,
      während ein Empfänger geparkt sein darf. Beides ist gewollt (A-4.1/A-4.3) — aber es heißt,
      die Rendezvous-Invariante hält der **Typ** nicht, sondern die Aufrufdisziplin des Kernels,
      und über die sagt der Beweis nichts. Dass `threads::mod` heute erst migriert und dann v2
      erzeugt, ist eine Reihenfolge, keine Zusicherung.

      **(b) `send_no_loss` gilt am echten Endpoint NICHT.** `Seq::push` ist unbeschränkt,
      `TidQueue::enqueue` verwirft ab `QUEUE_CAP` **still**. Gemessen am 33. Sender an *einem*
      Endpoint: `msgs_total` bleibt auf 32. Im Quelltext benannt, nirgends gemessen — bis jetzt.
      (Das ist zugleich die alte Baustelle „der 33. Sender wird still verworfen".)

      **(c)** `!used` (`ERR_BADCAP`), `quiescing` (`ERR_QUIESCING`) und der Leichen-Zweig
      (`frame_of == None`) haben im Modell **kein** Gegenstück.

      Offen ist nicht der Wächter, sondern das Modell. Damit es weiter trägt, bräuchte es
      mindestens: `used`/`quiescing` als Bits mit Abweisung; eine **Schranke** auf beiden
      Warteschlangen (dann wäre `send_no_loss` nur unter `len < QUEUE_CAP` beweisbar — was der
      Wahrheit entspricht); `caller`/`reply_owner` samt `reply` als dritter Operation; und
      `bind_receiver`/`migrate_owner` als Operationen, unter denen `ep_inv` dann nachweislich
      *nicht* erhalten bleibt — die Invariante müsste zu „kein Rendezvous ist fällig, außer
      während eines laufenden Austauschs" abgeschwächt werden. Kernel-Quelltext ist deshalb
      **nicht** geändert worden; die Befunde stehen im Kopf von
      `Verification/ipc/proofs/endpoint.rs`.


#### aus: F. Debug-/Testcode aus dem Release-Build nehmen

- [x] **F1/F2 erledigt (2026-07-30, A-2.2).** `feature = "selftest"` steht, `default = []` ist
      gedreht, und **beide** Konfigurationen werden gebaut: `test-qemu-x86.sh` baut den
      `--no-default-features`-Bau mit und vergleicht die `.text`-Größe — ein Gating, das nichts
      schrumpfen lässt, ist wirkungslos geworden, und der Build allein zeigte das nicht.
      Vorbedingung war der Root-Task (A-2.1): vorher wäre das Gating kein schlankerer Kernel
      gewesen, sondern ein leerer.

#### aus: Z22. Die vier harten Stellen aus Z21 — gebaut

- [x] **BEANTWORTET (2026-08-11) — und die Frage war die falsche.** Gemessen wurde damals
      „`fs` meldet sich, `hello` und `wasmhost` nicht", und daraus wurde eine Frage über
      **Badges und Isolation**. Die Ursache lag im Ladepfad: `wasmhost` hatte ein `PT_LOAD` an
      einer krummen VA (`.bss : ALIGN(8)` in den Programm-Linkerskripten), der Lader wies es
      korrekt ab, und das Programm lief nie. Heute laden alle sechs, `clientn` meldet **3
      Client-PDs mit eigener Ablage, 0 verloren**, und die Objekt-Ids sind sämtlich verschieden.
      Ein Programm, das gar nicht erst geladen wird, sieht von aussen aus wie eines, dessen
      Signal nicht ankommt.

#### aus: C9. Sperrhaltedauer — die Marke steht, die drei Befunde sind offen (2026-08-12) (2026-08-13)
* [x] **C9b — BEHOBEN (2026-08-13).** 69 100 174 Zyklen (2,46 Ticks) → **431 580–1 154 404
  (15–41 Promille)**, Faktor rund 100, und die Haltung hängt nicht mehr an der Zeilenlänge.
  **Beide naheliegenden Wege waren falsch, mit Zahlen ausgeschlossen:** je Byte kostet die
  Formatierung 1,2 Zyklen, ein ausgegebenes Byte **40 754** (zwei VM-Exits) — „Formatierung aus
  der Sperre heben" hätte 0,003 % entfernt; „Sperre je Zeile" war schon der Ist-Zustand. Und es
  ist keine QEMU-Eigenschaft: auf Blech kostet dieselbe 1623-Zeichen-Zeile bei 115 200 Baud
  141 ms = **14 Ticks**. Gebaut ist die Trennung der zwei Eigenschaften (`crates/caprock-hal/src/konsole.rs`):
  **Unteilbarkeit** über ein Besitzrecht **ohne** Maskierung, **maskiert** nur noch ein einzelnes
  `outb`, das `THRE`-Warten davor. Der Panikpfad blieb sperr- und atomicfrei.
  **Die erste Fassung erzeugte genau die Falle, gegen die der Auftrag gewarnt hatte:** roh aus dem
  Trap-Kontext gedruckt → in 8 Läufen **zwei zerrissene Zeilen, eine traf das Ergebniswort**
  (`isohigh : ` ohne `SKIP`), die Zeile fiel aus der Signatur. Gefunden hat es ein **Zähler**
  (`risse`, gattert), nicht das Gegenlesen.
  Der Schuldposten für `konsole.rs` bleibt — mit **anderem Grund**: Blockgrössen 16/4/1 ergaben
  denselben Ausreisser, die Zahl misst also einen **gestallten `outb`** (D13-Klasse) und nicht die
  Sperrhaltung. *Eine Zahl, die sich durch die Änderung, die sie erzeugen müsste, nicht bewegt,
  misst nicht die Sache.*

