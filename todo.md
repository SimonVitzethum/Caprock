# Caprock — offene Punkte

Nur **noch nicht Erledigtes**. Was fertig ist, steht mitsamt Begründung in [done.md](done.md).
Reihenfolge innerhalb eines Abschnitts = Priorität. `[~]` = teilweise erledigt, Rest benannt.

**Aufgeräumt am 2026-08-11:** 6 vollständig erledigte Einträge und 54 einzelne `[x]`-Punkte sind
**wörtlich** nach [done.md](done.md) gewandert (Abschnitt „Aus `todo.md` herübergeräumt"), samt
ihrer Begründungen — die sind der Wert, nicht die Häkchen. Danach steht hier **kein einziges
`[x]`** mehr; die Datei beantwortet die Frage „was ist offen" wieder, ohne dass jemand Erledigtes
überlesen muss. Stand danach: **138 offene** und **16 teilweise erledigte** Punkte.

---

## C7b. Die 2 MiB je isolierter PD sind gemessen und gesenkt — und der Hebel war der falsche

**Klasse:** Kapazität / Produktziel · **Stand:** die Messung steht, die Grösse ist gesenkt
(2 MiB → **64 KiB**), die Kurve ist **nachgemessen** — und sie widerlegt den „Faktor 22".
Offen ist der Vorrat, der jetzt bindet (s. C7c).

Die Mandantendichte hängt an dieser Zahl (`RAM / Regionsgrösse`, s. C7), und sie war bisher eine
Summe ohne Summanden. Gelesen im Quelltext, nicht geschätzt:

| Posten | vorher | **jetzt** | was es ist |
|---|---|---|---|
| **private Region** | **2048 KiB** | **64 KiB** | **der EL0-Stack EINES Threads**, `mem_alloc_anywhere`, mit `zero_phys` genullt, abgebildet nach `SLOT_DATA` |
| Seitentabellen | 20 KiB | 24 KiB | +1 Rahmen: ohne den 2-MiB-Block braucht das User-Fenster eine Tabelle |
| EL1-Kernel-Stack | 4 KiB | 4 KiB | seit dem 2026-08-12; vorher 16 |
| Guard-Page | 4 KiB | 4 KiB | seit dem 2026-08-10 |
| | **≈ 2076 KiB** | **≈ 96 KiB** | **gemessen** in der Kurve („Je Prozess kostete es 96 KiB"), nicht gerechnet |

**Der Befund war richtig: die 2 MiB waren GRANULARITÄT, nicht Bedarf.** `vspace_map_user_window`
hat zwei Zweige — `len == TWO_MIB && phys % TWO_MIB == 0` bildet **einen 2-MiB-Block** ab (kein
Seitentabellenrahmen), alles andere seitenweise **mit** einer Tabelle.

**Die Messung, die gefehlt hat** (2026-08-13, x86, Prüfzeile `ustack`, gattert):

| Suite | tiefster gemessener EL0-Pfad | Region |
|---|---|---|
| `test-qemu-x86.sh` | 6168 B — und das ist die **Tiefensonde selbst**; die Arbeiter dieser Suite fassen ihren Stack nicht an (15 von 17 Messungen: Tiefe 0) | 2 MiB |
| `test-qemu-x86-load.sh` | **7400 B von 16384 B (45,1 %)** — ein echtes geladenes Programm | 16 KiB |

Summenbedingung, wie bei C4 und nicht am Höchststand: `7400 B + 8192 B Reserve = 15 592 B ≤
65 536 B`, Abstand Faktor 4,2. **Der zweite Summand ist auf EL0 nachweislich NULL** — ein Trap aus
Ring 3 wechselt immer den Stack (x86 `RSP0` aus der TSS, aarch64 `SP_EL1`); das Gegenstück zum
Interrupt-Handler auf demselben EL1-Stack existiert hier nicht, und Signal-Handler auf dem
User-Stack gibt es in diesem System nicht.

**UND JETZT DAS ERGEBNIS, DAS DEN EINTRAG UMDREHT.** Die Kurve wurde nachgemessen — beide Stände
mit **demselben** Kernel, nur die Konstante getauscht:

| RAM | vorher (2 MiB) | Grund | **nachher (64 KiB)** | Grund |
|---|---|---|---|---|
| 512M | 220 | **RAM** (1 MiB frei) | **321** (+46 %) | **Guard-Tabellen-Topf**, 419 MiB frei |
| 3G | 1119 | **Guard-Tabellen-Topf**, 739 MiB frei | **979** (−12,5 %) | **Guard-Tabellen-Topf**, 2918 MiB frei |

**„Faktor 22 auf die Mandantendichte" war falsch, und zwar aus einem Grund, den die Rechnung nicht
sehen konnte:** sie hat `RAM / Regionsgrösse` gerechnet und damit vorausgesetzt, dass RAM bindet.
Bei 3 GiB tat er das **schon vorher nicht** — dort war bereits mit 2-MiB-Regionen der feste
Vorrat aufgeteilter Guard-Seitentabellen die Schranke (1119 PDs bei 739 MiB freiem RAM). Die
**Kosten** je PD sind wirklich um Faktor 21,7 gefallen (2076 → 96 KiB, gemessen); die **Dichte**
ist ihnen nicht gefolgt, weil der nächste Topf fest ist. Der Zugewinn steht dort, wo RAM wirklich
band: bei 512 MiB, +46 %.

Dass die Zahl bei 3G sogar **fällt** (1119 → 979), ist kein Widerspruch, sondern dieselbe Ursache:
der Topf bindet an der Zahl **verschiedener 2-MiB-Blöcke**, über die die Kernel-Stacks streuen,
und die hängt an der Schrittweite je PD. Wer die Regionsgrösse ändert, ändert die Streuung.

- [x] **Die Messung: wie tief ist der EL0-Stack wirklich?** Steht als `kernel/src/userstackmark.rs`
      (Nullung statt Füllmuster — die Region wird ohnehin genullt, das höchste von Null
      verschiedene Byte IST der Wasserstand), mit Eichung (drei Fälle) **und** einer Tiefensonde
      auf dem echten Pfad. Die Sonde prüft, was die Eichung strukturell nicht kann: dass die
      Buchführung Thread-Slot → Region auf die **richtige** Region zeigt. Ein Eintrag, der auf
      irgendeine andere genullte Region zeigt, meldet „viel Luft" und bestünde jede Eichung.
- [x] **Die Grösse senken**, mit der Summenbedingung. `hal::mmu::PRIV_REGION_SIZE = 64 KiB`.
- [ ] **Der zweite Weg bleibt daneben stehen und ist jetzt der EINZIGE, der noch etwas bewegt:**
      die VA bei 2 MiB lassen und **lazy** unterlegen (Speicher-Server, [Z16](#z16) Stufe 1).
      Dann kostet eine PD nur, was sie anfasst. **Aber:** solange C7c bindet, ändert auch das die
      Dichte nicht — die Reihenfolge ist damit umgedreht, C7c kommt zuerst.
- [ ] **Was die Messung NICHT sieht, und es gehört in jede Folgeentscheidung:** ein mit **Null**
      beschriebenes Stackwort ist unsichtbar (das Muster-Verfahren von `kstackmark` erkennt jeden
      Schreibzugriff, das Null-Verfahren nur solche mit Wert ≠ 0). Der Messwert ist eine
      **Untergrenze**; der Abstand Faktor 4,2 zur Regionsgrösse ist die Antwort darauf. Wer die
      Grösse weiter senken will, braucht zuerst ein Verfahren ohne diesen blinden Fleck — etwa
      den gesicherten User-`RSP` im Trapframe als zweiten, inhaltsunabhängigen Schätzer.
- [ ] **Der `ustack`-Bericht steht nur im x86-Hochlaufweg**, die Zähler laufen arch-neutral mit.
      Dieselbe Einordnung (und dieselbe Lücke) wie bei `kstack` und `sperre` — s. C9d.

## C7c. Der Vorrat, der JETZT bindet: 16 aufgeteilte Guard-Blöcke (gemessen 2026-08-13)

**Klasse:** Kapazität / Produktziel · **Stand:** gemessen und benannt, nicht behoben.
**Er ist seit C7b die Schranke der Mandantendichte** — auf jeder RAM-Grösse, die geprüft wurde.

`hal::mmu::GUARD_BLOCKS = 16`. Ein Kernel-Stack liegt oberhalb 16 MiB, wo die Identitätskarte in
2-MiB-Blöcken steht; eine 4-KiB-Wache verlangt, den Block in eine Seitentabelle aufzuteilen, und
die HAL hat keinen Allokator (Kerngrenze) — der Vorrat ist statisch. Reicht er nicht, wird die
Stack-Anforderung **benannt abgewiesen** (`MANGEL_GUARD_TABELLE`) statt unbewacht bedient. Das ist
richtig so; die Schranke ist fail-closed und keine Korrektheitslücke.

**Die Herleitung der 16 stimmt nicht mehr — und sie hat nie das Richtige gerechnet.** Im Quelltext
steht: „ein 16-KiB-Stack mit Wache belegt 20 KiB, in einen 2-MiB-Block passen also gut 100 davon,
16 Blöcke tragen rund 1600 bewachte Stacks — mehr als die 224..1477 einer isolierten
Kapazitätskurve". Zwei Fehler darin:

1. Der EL1-Stack ist **seit dem 2026-08-12 4 KiB**, mit Wache also 8 KiB — 256 je Block, 4096
   insgesamt. Die Zahl im Kommentar ist überholt.
2. Wichtiger: die Rechnung misst die **Packungsdichte innerhalb** eines Blocks. Gebunden ist aber
   die Zahl **verschiedener** Blöcke, über die die Stacks streuen — und die hängt daran, was
   zwischen zwei Stacks sonst alloziert wird. Gemessen bei 512M/64 KiB: **321 bewachte Stacks über
   16 Blöcke = rund 20 je Block**, also **7,8 % der 4096, die hineinpassten**.

**Die strukturelle Antwort steht schon im Quelltext und ist jetzt bezifferbar:** eine
**Stack-Arena** — ein zusammenhängender Bereich, dessen Blöcke einmal aufgeteilt werden. Sie hebt
die Dichte um den Faktor, der oben als Verschnitt steht (rund 12), ohne eine einzige statische
Tabelle mehr.

- [ ] **Stack-Arena statt gestreuter Kernel-Stacks.** Umbau der Stack-Zuteilung, nicht eine
      grössere Konstante. `GUARD_BLOCKS` zu erhöhen kauft lineare Zahlen mit linearem BSS und
      lässt den Verschnitt von 92 % stehen.
- [ ] **Bis dahin gehört die Zahl in die Produktaussage**, nicht die Rechnung `RAM / 96 KiB`:
      gemessen sind **321 (512M)** und **979 (3G)** isolierte, **lebende** Mandanten.

## C9. Sperrhaltedauer — die Marke steht, die drei Befunde sind offen (2026-08-12)

Die **Sperrhaltedauer-Marke** misst seit dem 2026-08-12, wie lange der Kern am Stück mit
maskierten Interrupts läuft (`crates/caprock-sync`, `kernel/src/sperrmark.rs`, Prüfzeile
`sperre`). Die Schwelle ist hergeleitet und nicht gefühlt: **ein Timer-Tick** — länger maskiert
heisst nachweisbar verlorene Präemption. Gegenprobe: `tools/sperr-gegenprobe.sh`.

**Der erste Lauf hat drei Stellen gefunden, die vorher nirgends eine Zahl waren.** Alle drei
stehen als *benannte, gedeckelte* Schuld in `sperrmark::SCHULDEN` (Ratsche je Posten, darf nur
fallen) — sie sind damit sichtbar, aber nicht behoben. Die Reihenfolge ist der Punkt: **jede
wurde erst sichtbar, als die davor benannt war** — ein Maximum verdeckt, was kleiner ist.

* [ ] **C9a — `kernel/src/colors.rs:982`: 1,19 Mrd. Zyklen (rund 420 ms, 42 Ticks).**
  `run_prime_probe` (B-4.5) nimmt `PP_ARENA.lock()` und gibt es 162 Zeilen später frei;
  dazwischen liegt die *ganze* Messung — `system::alloc` (also die MEM-Sperre **unter** der
  Arena-Sperre), das Suchen farbreiner Läufe, Tausende Zeigerkettenläufe, die Rückgabe. Dieselbe
  Form wie der C8-Befund, nur ohne `while let`: nicht die Sperre ist zu lang, die **Arbeit liegt
  darunter**. Prüfcode, kein Produktivpfad — deshalb (a) und nicht (c).
  *Weg:* die Arena braucht Exklusivität, nicht IRQ-Maskierung. Entweder die Messung aus dem
  kritischen Abschnitt heben oder ein Primitiv ohne Maskierung. Braucht eine eigene Gegenprobe.
* [ ] **C9c — `kernel/src/system.rs:9198`: 20,4 Mio. Zyklen (7,3 ms, 0,73 Ticks).**
  `purge_ipc_for_thread` hält `IPC_ORPHANS` als **äusseren** Lock über einen Sweep von
  O(Endpoints + Notifications) = 20 128 Einzelsperrungen — **je Thread-Tod**. Die
  *Iterationszahl* stand dort seit C4/D10 im Kommentar; was nirgends stand, ist die daraus
  folgende **Latenz**. Reiner Produktivpfad, und schon heute bei drei Vierteln der Schwelle
  (gemessen 494…748 Promille über fünf Läufe). Deckel deshalb **enger als die Schwelle**
  (856 statt 1000 Promille) — er ist eine Fessel, kein Freibrief.

* [ ] **C9d — die Marke gattert nur auf x86.** Die Eichung läuft auf beiden Architekturen (und
  besteht dort auch, `0b111111`), die **Berichtszeile und das Gatter** stehen nur im
  x86-Hochlaufweg — dieselbe Einordnung wie `kstackmark`. Zwei Gründe, beide benennbar: die
  Schuldliste enthält mit `console.rs` einen **x86-Pfad** (aarch64 hat einen anderen Konsolen-
  treiber, dessen Zahl niemand gemessen hat). Der **zweite** Grund ist seit dem 2026-08-13
  weggefallen: die aarch64-Suite ist wieder grün (s. C9e). Ein Gatter dort ist damit abnehmbar,
  und was noch fehlt, ist nur die aarch64-Konsolenzahl.

* [~] **C9e — BEHOBEN am 2026-08-13; was offen bleibt, ist die Klasse, nicht der Fall.**
  Das Bild war `color : FAILURES` mit **lauter Nullen** (`in_mask=0 kernelseite=0 disjunkt=0
  bilanz=0`, dabei `uebergross_abgewiesen=1`), danach Watchdog mit `offen: color` — 3 von 3
  Läufen zeichengleich, also deterministisch und nicht D13.

  **Die Ursache war eine Größenrelation, keine Nebenläufigkeit.** aarch64 hat 16 Farben, bei
  `PARTITIONS = 4` ist ein Streifen also **4** Farben breit; ein EL0-Kernel-Stack ist dort
  16 KiB = **4** Seiten und passte damit *genau* — die Eigenschaft „der Stack passt in seinen
  Streifen" folgte aus `Stackseiten == Streifenbreite` und nicht aus der Struktur. Seit der
  Wachseite (2026-08-10, `USER_KSTACK_ALLOC = USER_KSTACK_SIZE + PAGE`) fordert
  `claim_user_kstack_masked` **5** aufeinanderfolgende Seiten an. Fünf aufeinanderfolgende
  Seiten tragen fünf **verschiedene** Farben; in einen 4-Farben-Streifen passen sie an keiner
  Adresse und bei keinem Füllstand. `alloc_colored` gab also **immer** `None`,
  `spawn_isolated_colored` ebenso, und `run_color` fiel vor dem `if let` heraus — daher genau
  die Felder null, die hinter dem `if let` entstehen, und `uebergross_abgewiesen` als einziges
  auf 1.

  Zwei Umstände, die es unsichtbar hielten: auf **x86** ist ein Streifen 16 lebendige Maskenbits
  breit und der Stack seit C4 zwei Seiten — dort ist reichlich Luft. Und auf **aarch64 gibt es
  die Wache gar nicht** (`hal::mmu::guard_unterstuetzt() == false`, `guard_unmap` ist ein
  Zähler): die Seite, die jede gefärbte Stack-Anforderung unmöglich machte, tut auf dieser
  Architektur nachweislich **nichts**.

  **Behoben** durch `Allocator::alloc_colored_vorspann_in`: die ersten `n` Seiten einer
  gefärbten Anforderung stehen unter keiner Farbbedingung. Das ist keine Aufweichung — eine
  Wachseite trägt keine Daten und belegt kein Cache-Set; die Farbzusicherung sagt über sie
  nichts aus. Dazu die Ausrichtung von `al` (16 KiB) auf `PAGE` gesenkt: mit 16-KiB-Ausrichtung
  des **Blocks** wäre der Stack bei `roh + PAGE` gerade *nicht* streifenausgerichtet gewesen.
  Belegt ohne QEMU (`caprock-mem`, `wachseite_sprengt_den_streifen_bei_16_farben`: 4 Seiten
  passen, 5 nie, mit Vorspann wieder — und jede Stackseite im Streifen), Gegenprobe im Kernel
  isoliert auf **einen** Parameter (`vorspann` 1 → 0 stellt das Bild zeichengleich wieder her,
  jede andere Zeile bleibt grün).

  **Was offen bleibt:**
  - [ ] Eine gefärbte Anforderung, die **breiter ist als der Streifen**, ist strukturell
        unerfüllbar und kommt trotzdem als `None` zurück — ununterscheidbar von „Speicher
        alle". Genau daran hat diese Suche gehangen: das Fehlerbild sah nach Mangel aus.
        Gefragt ist ein eigener Grund (`MANGEL_*` reicht nicht: es ist kein Mangel).
  - [ ] `colors::region_bytes()` kennt die Schranke bereits (`Streifenbreite * PAGE`) —
        `USER_KSTACK_ALLOC` prüft sie nirgends gegen. Zwei Größen, die zueinander passen
        müssen, und keine Zeile, die es sagt.

---

## LESEHILFE: was welche Kennung bedeutet — und der Stand der vier Z26-Nachträge

**Eine Kennung, zwei Bedeutungen** (bemerkt 2026-08-10, bevor sie zur Fussnote wurde):

| Kennung | in **Z19** (Substrat der Sprachlaufzeiten) | in **Z26** (GPU/CUDA) |
|---|---|---|
| `A1` | der Prozessstart-Stack (`argc`, `auxv`) | NVIDIA-**Kernelmodul** portieren |
| `A2` | TLS | die `/dev/nvidia*`-**ioctl-Fläche** als PD-Protokoll |
| `A3` | **Stack und Guard-Page** | die **binäre Linux-ABI** (Loader für fremde ELFs, Signale, `futex`, `mmap`) |
| `A4` | SSE freischalten (erledigt) | 84 MB **GSP-Firmware** ausliefern + Attestierung |

Wer „A3" ohne Strang nennt, hat zwei verschiedene Arbeiten gemeint. **Ab hier immer mit Strang:
`Z19/A3` oder `Z26/A3`.** Dieselbe Sorte Drift wie „seL4 → SEL4 → Sel4lake", nur bei Kennungen
statt bei Namen — und aus demselben Grund teuer: in drei Monaten rekonstruiert es niemand mehr.

**Was am 2026-08-10 gebaut wurde, ist `Z26/A3` — genauer: nur dessen Kernstück**, das
Kernel-Primitiv für umgeleitete Syscalls. Nicht der Loader für fremde ELFs, nicht die
glibc-Symbolfläche, nicht Signale/`futex`/`mmap`.

**Stand der vier Nachträge, die vor dem Bau zu klären waren:**

| | Nachtrag | Stand |
|---|---|---|
| 1 | Die Umkehr der protokollierten Entscheidung **Z16** muss als neue Entscheidung benannt sein | **erledigt** — steht als Eintrag; die Forderung der alten Analyse (WSL1 scheiterte an der **Verhaltenstreue**) ist mit übernommen |
| 2 | Die **Autoritätsliste** war registergross: Frame-Schreibumfang, Gast-**Speicherzugriff**, **Vspace**-Recht | **offen** — das Primitiv vergibt nach eigener Messung **1,5 von 4**. Ohne Gast-Speicherzugriff ist *jeder Syscall mit Zeiger* nicht implementierbar (`read`, `write`, `ioctl` — für CUDA die ganze Fläche); ohne Vspace-Manipulation kein `mmap` |
| 3 | Die **Blockiernaht** (Zyklusverbot **im Kernel**, Wartegrund in der Grund-Menge) | **erledigt, beide Hälften** — `pruefe_bindung` geht den Graphen (nicht nur den direkten Partner), Wartegrund von Tag eins in `BlockReasons` |
| 4 | **Messmatrix mit vorab fixierter Schwelle** | **halb** — die Schwelle steht (umgeleiteter `getpid` ≤ 2000 Zyklen, verankert an Z18 (2)), **gemessen ist nichts**. Für `read` 4/64 KiB ist keine Schwelle festlegbar, weil der Pfad nicht existiert (Gast-Speicher-Autorität fehlt, s. Nachtrag 2) |

**Die grösste Lücke des Primitivs ist am 2026-08-13 geschlossen:** die **Nutzlast** steht. Der
Frame wird ins Sidecar kopiert und das Ergebnis zurückgeschrieben, ein Pfad prägt Handler-Caps, und
`SYS_SETHANDLER` ist erfolgreich gewesen — belegt als `redirect : ALL PASS` an einem echten Gast,
der einen umgeleiteten Syscall macht und **das Ergebnis sieht**. Details, Gegenproben und die
**neuen** offenen Punkte (isolierter Handler / EL0-Gast, `rip`/`rsp` im Rückschrieb) unten bei
[A3](#a3--das-kernel-primitiv-ist-gebaut-2026-08-10-was-steht-was-offen-ist-und-die-schwelle).

## Was der Durchgang durch die einseitigen Vergleiche NICHT abgedeckt hat

**Klasse:** Prüferform · **Stand:** der Durchgang selbst ist erledigt (s. [done.md](done.md)),
diese Reste sind es nicht.

Die Regel steht: *jede gemessene Grösse, die in einen Vergleich mit **nur einer** Schranke geht,
braucht eine Plausibilitätsuntergrenze — oder der Wert Null muss ausdrücklich als „nicht gemessen"
ausscheiden.* Gefunden und behoben wurde eine lebende Fundstelle (F1). Zwei Klassen hat der
Durchgang **nicht** erreicht, und ein Durchgang, der seine Reichweite verschweigt, ist die nächste
Nullaussage:

- [ ] **Prüfer, die eine Grösse gar nicht erst erheben.** Der Durchgang suchte nach Vergleichen;
      er kann nichts über Zeilen sagen, die nie eine Zahl bilden. Das ist die Klasse „ein Test,
      der nirgends läuft", nur eine Ebene tiefer.
- [ ] **Vergleiche innerhalb der Verus-Modelle.** Dort gelten dieselben Formen, aber die
      Suchmuster des Durchgangs (Shell-Vergleiche, Rust-Urteilszeilen) greifen nicht.

## S2. Der zweite SPARK-Datenpunkt: der Scheduler — und es bricht NICHT

**Klasse:** Verifikation / Sicherheit · **Stand:** Frage beantwortet, **fünf Befunde im Rust-Code
offen**

Die Entscheidungsfrage war: *wieviel Prozent des Scheduler-Kerns kommen unter `SPARK_Mode => On`?*
Der Cap-Space kam mit 34/34. Der Scheduler hat, was jener nicht hatte: **Nebenläufigkeit, Zeit und
den `Parked`-Typ**.

**Antwort: 65 von 68 Unterprogrammen analysiert. Übersprungen genau drei — und alle drei sind
Rümpfe, die in dieser Quelle gar nicht stehen** (importierte Fremdcrate-Funktion, eine
`black_box`-Stackadresse, die in *keiner* Sprache beweisbar ist, und ein Laufzeit-Rumpf).
**Kein Stück Scheduler-Logik unter `Off`** — Ready-Queue, Grund-Menge, Budget/Refill,
`spawn_parked`/`admit`, Zombie-Buchführung, Migration, Audit: alles drin.

**Es bricht also nicht bei 60 %, und an der Sprache liegt es nicht.**

**Die Beweisquote ist trotzdem deutlich schlechter als beim Cap-Space: 38 % offen gegen 15 %.**
Und der Grund ist Entwurf, nicht Sprache — **rohe Maschinenindizes an jeder Naht**. Das ist die
eigentliche Aussage dieses Datenpunkts.

### Die drei Fragen

**Nebenläufigkeit trägt** — 63/63 Datenabhängigkeiten bewiesen: der Rust-Code liest überall genau
einmal in eine Kopie, und das ist jetzt **bewiesen statt gelesen**. Was *nicht* geht: „der Aufrufer
hält den Spinlock" hat in SPARK **keine Ausdrucksform** — bleibt in beiden Sprachen ein Kommentar.

**NACHGEMESSEN 2026-08-13: in VERUS hat er eine.** Nachgebaut an `kernel/src/system.rs:310`
(`record_user_kstack`, heute ein Kommentar): ein `tracked struct SchedsGuard` mit privatem Feld,
`lock()` als einzige Quelle, `requires` an der Engstelle. Drei Aufrufer, drei **verschiedene**
Ausgänge — richtiger Kern `verified`; **fremder** Kern `precondition not satisfied`; selbstgebauter
Beleg `constructor for an opaque datatype`. Unter `#![no_std]`, und `tracked`/`ghost` wird vor der
Codeerzeugung gelöscht: **kein Byte, keine Halde.** Kosten: 9 Zeilen Zeugentyp, 8 Zeilen
Sperrverträge, 1 `requires`, 1 Argument je Aufrufstelle.

**Und `Parked`: JEIN.** `tracked` ist **affin, nicht linear** — eine Funktion, die den Zeugen
fallenlässt, geht durch (2 verified, 0 errors). Eine automatische Leckprüfung wie SPARKs „leak
proved" gibt es **nicht**. Mit einer Ghost-Bilanz geht es doch und ohne Halde (`spawn_parked` erhöht
`offen`, `admit` senkt sie; `vergessen()` → `postcondition not satisfied`, `ordentlich()` →
verified) — aber das ist eine Pflicht, die man **hinschreiben** muss, durch jede Signatur.
**Das bleibt SPARKs einziger echter Vorsprung**, und er kostet in Verus eine Bilanz statt eines
Schalters.

**Zeit ist die schwächste Klasse** — `now += 1` formal offen (bei 100 Hz nach 5,8 Mrd. Jahren).

**`Parked`: SPARK kann es besser — Fehler statt Warnung.** Gemessen an zwei Unterprogrammen, die
sich in *einer* Zeile unterscheiden: mit `Admit` → „absence of resource or memory leak **proved**",
ohne → „leak might occur". Rusts `#[must_use]` ist eine **abschaltbare Warnung**.
**Der Preis, und er wiegt hier:** SPARKs Linearität hängt an einer **Allokation**, Rusts `Parked`
ist ein **Stackwert** — für einen Kernel ohne Halde ein echter Einwand.

### Fünf Befunde im Rust-Code

- [ ] **S2a — `migration_candidate` läuft die Ready-Kette OHNE Schrittgrenze.** Nachgeprüft
      (`crates/caprock-sched/src/lib.rs`): `while i != NIL { … i = t.qnext … }`, während `audit`
      über **derselben** Kette `if n > q.count { return 5 }` führt. Läuft im **Lastausgleich unter
      dem Kern-Lock**: ein Zyklus dort ist ein stehender Kern. Dieselbe Form wie B-5.5, nur
      umgekehrt — dort war der *Prüfer* begrenzt und der Pfad nicht.
- [ ] **S2b — `depleted_count` hat keine Audit-Nachzählung.** Code 10 gibt es nur für
      `budget_blocked_count`. Und es ist **genau der Zähler, der in D8/M5 gelogen hat** — vier
      Senkungen, zwei Erhöhungen, alle ohne Helfer.
- [ ] **S2c — `debug_assert_eq!(core, self.core)` steht 15× in `lib.rs` und 0× im Release.**
      Alle acht offenen Zusicherungen sind diese Form: eine Zusage, die im ausgelieferten Kernel
      nicht existiert.
- [ ] **S2d — `priority: u8` indiziert `[ListHead; 8]` roh**, und `NPRIO` kommt ausserhalb der
      Crate an **genau einer** Stelle vor. Ausführbare Gegenprobe: prio 0 ✓, prio 7 ✓,
      **prio 8 → `CONSTRAINT_ERROR`**. In Rust ein Panic mit `panic = "abort"`.
- [ ] **S2e — das Verus-Scheduler-Modell führt `blocked: bool`, den Zustand, den [Z24](#z24)
      ABGESCHAFFT hat.** 7 modellierte Felder gegen 23, Arithmetik nur `nat`/`int`. Der
      Modell-Treue-Wächter prüft die *Abbildung*, nicht die *Reichweite* — dieselbe Lücke wie bei
      S1.

### Eine Falle am eigenen Werkzeug

**gnatprove sammelt je Objektverzeichnis auf.** Nach dem Projekt-Split meldete der S1-Lauf
**232 statt 99** Prüfungen. Die Ratsche riss — **in der Gegenrichtung hätte sie geschwiegen**.
Behoben (Leeren + Einheitsprüfung); als Falle in `CLAUDE.md`.

## S1. Das SPARK-Experiment hat zwei echte Fehler gefunden (2026-08-13, Zweig `spark-experiment`)

**Klasse:** Verifikation / Sicherheit · **Stand:** Experiment beantwortet, **zwei Befunde im
Rust-Code offen**

Die Frage lautete: *findet GNATprove auf Silver Level am Cap-Space etwas, das das Verus-Modell
nicht sieht?* **Antwort: ja, 15 Stellen.** Die Kennzahl, die darüber entscheidet, ob das etwas
wiegt: **34 von 34 Unterprogrammen unter `SPARK_Mode => On`, kein einziges `Off`** — 294
Prüfungen, 279 bewiesen, 15 offen (5 %). Der erste Lauf hatte 39; die 24 Differenz waren fehlende
Schleifeninvarianten meiner Portierung, beseitigt durch Zusicherungen, die GNATprove **selbst
nachweist**.

- [ ] **S1a — `audit_cdt` kann den Knoten mitnehmen, statt die Anomalie zu melden.**
      NACHGEPRÜFT im echten Quelltext (`crates/caprock-cap/src/space.rs`, Geschwisterlauf): `p`
      wird gegen `nslots` geprüft, **`first_child` und `next_sibling` nicht** —
      `self.slots[ci]` indiziert ungeprüft. Die Schrittgrenze (B-5.5) schützt gegen **Zyklen**,
      nicht gegen einen Index **ausserhalb der Tabelle**.
      In Rust ist das ein Panic, und `panic = "abort"` steht in **beiden** Profilen: **der Prüfer,
      der die Anomalie melden soll, reisst den Knoten mit.** Dieselbe Form wie B-6.2, nur im
      Audit-Pfad.
      Die Gegenprobe isoliert die **Position**, nicht die Verfälschung — derselbe kaputte Wert,
      nur der Slot wandert: sauber → `audit_cdt = 0`; kaputt bei Slot 0 → `6`; kaputt bei Slot 1 →
      **`CONSTRAINT_ERROR`, kein Urteil**. Die Prüfung für `p` steht in dessen *eigener* Iteration;
      ist `p > s`, hat sie noch nicht stattgefunden.

- [ ] **S1b — `refcount -= 1` in `delete_leaf` ohne Bedingung, und `overflow-checks` ist im
      Release NICHT gesetzt.** Also kein Panic, sondern **stiller Umlauf auf `0xFFFF_FFFF`**:
      Objekt nie finalisiert, Region nie freigegeben, Reply nie abgebrochen. Dieselbe Form wie
      D11 — eine Schranke, die niemand benennt, ist ein Loch statt eines Schutzes.

### Warum Verus keine einzige davon sehen kann — gezählt, nicht behauptet

`refcount` steht nur in `cap_cdt_refcount.rs` (21×, als **`nat`** — über `refcount -= 1` kann ein
`nat`-Modell nicht einmal die Frage stellen), `parent` nur in den Strukturdateien. **Keine Datei
führt beides im selben Zustand.** `gen`, `Finalized`, `rights`, `badge`, `move_cap` kommen im
ganzen Modell **gar nicht** vor. `delete_leaf` senkt den Refcount **und** hängt aus — diese
zusammengesetzte Operation ist nirgends bewiesen.

**Das reiht sich in die Registerkorrekturen ein:** „16 Beweisdateien, 0 errors" sagt, dass die
vorhandenen Beweise halten — über die **Reichweite** sagt es nichts, und hier ist sie gemessen.

### Vier ungeschriebene Zusicherungen, die die Portierung ans Licht zwang

**I5** Länge der Geschwisterkette (Verus hat dafür *keine* Längen- oder Terminierungsaussage) ·
**I6** die Kapazitätszusage von `Finalized` · **I7** der **absichtliche** Umlauf der Generationen —
darauf ruht `resolve` und damit die **gesamte** Handle-Sicherheit, und Generationen kommen im
Modell nicht vor · **I9** ein Fenster im `install`-Rollback (`used = false` bei `refcount = 1`),
das `audit_cdt` strukturell **nicht** meldet: Code 3 vergleicht für unbelegte Objekte nur `refs`,
nicht `refcount`. Harmlos, weil `alloc_object_inner` überschreibt — **Zufall der Reihenfolge,
kein Entwurf.**

### Was das über SPARK sagt — und was nicht

Der Gewinn kam **nicht** aus „Ada ist sicherer", sondern daraus, dass GNATprove **jede**
Indizierung und **jede** Arithmetik als Beweispflicht behandelt, während Verus nur beweist, was
jemand modelliert hat. Das ist ein Argument für **mehr Beweisfläche**, nicht zwingend für einen
Sprachwechsel.

### NACHGEMESSEN 2026-08-13: Verus findet S1a UND S1b am echten Code — die Vermutung hält

Die Gegenthese ist **bestätigt: es war die VOREINSTELLUNG, nicht die Sprache.** Mit
`#[verifier::verify]` auf der `mod`-Zeile (bzw. `--no-external-by-default`) ist jede Indizierung und
jede Arithmetik in gewöhnlichem Rust eine Beweispflicht, wie bei GNATprove.

| | Meldung |
|---|---|
| **S1b** `space.rs:1067  refcount -= 1` | `possible arithmetic underflow/overflow` **und** `precondition not satisfied` — Rumpf unverändert |
| **S1a** `space.rs:844  self.slots[ci]` | `precondition not satisfied`; das `self.slots[p]` eine Zeile darüber wird **entlastet** — der Melder ist trennscharf, nicht pauschal |

**Die Zahl, an der der Vergleich hängt, sind zwei verschiedene Grössen** — die Verwechslung wäre der
Fehler:

* **Pflichten AUFWERFEN: 0 Zeilen je Zeile Code.** Ein Schalter. Delta zum Produktionstext über die
  ganze Datei: **24 Zeilen, davon 21 Attributzeilen und 3 `derive` — kein einziger Funktionsrumpf.**
* **Pflichten ENTLASTEN, an `delete_leaf` gemessen: 12 Zeilen Spezifikation auf 19 Zeilen Code
  (0,63).** Danach bleibt **genau eine** Pflicht offen — S1b. Untergrenze: zwei Aufrufer-Verträge
  sind `external_body`, also angenommen statt bewiesen.

Lauf A (ganze Datei ohne `audit_cdt`): 48 von 67 Funktionen ohne offene Pflicht, 60 offene Pflichten
(54 Indexgrenzen, 6 Arithmetik). Lauf B (+ `audit_cdt`): 90 — die 33 Zusatzzeilen dort sind
**ausschliesslich Schleifenform** (`for` → `while`), keine Bedingung und kein Index geändert.

- [ ] **S1c — das Überlauf-Gegenstück zu S1b, das SPARK NICHT nannte:**
      `crates/caprock-cap/src/space.rs:543  self.objects[obj].refcount += 1;` in `copy`. Dieselbe
      fehlende Schranke in der anderen Richtung. Praktisch gedeckt, weil der Refcount durch die
      boot-dimensionierte Slot-Tabelle weit unter 2³² bleibt — **aber S1b war auch „praktisch"
      gedeckt, bis es das nicht war.** Eine Schranke, die aus einer Grössenrelation folgt statt aus
      der Struktur, verschwindet beim nächsten Messwert.
- [ ] **`space.rs:170  &self.dma[..self.dn]`** in `Finalized::dma_regions` — hält, weil `push_dma`
      `dn <= len` mitführt. **Das steht nirgends.** Eine Zusicherung, die niemand aufschreibt, ist
      beim nächsten Umbau weg.
- [ ] **I5 ist eine Lücke des MODELLS, nicht des Werkzeugs.** Verus verlangt `decreases` für **jede**
      Schleife — strikt stärker als SPARK Silver, das Terminierung gar nicht fordert. Die
      Terminierungsaussage über die Geschwisterkette fehlte also nicht, weil Verus sie nicht kann,
      sondern weil das Modell sie nicht benutzt hat. Dieselbe Form wie „16 Beweisdateien, 0 errors"
      sagt nichts über die neue Eigenschaft.
- [ ] **Was der Umstellung im Weg steht, ist WERKZEUGREIFE, nicht Ausdruckskraft** — und das ist
      der Posten, der zu beziffern wäre, bevor jemand `space.rs` dauerhaft unter Verus stellt:
      vier reproduzierte Abstürze in Verus `0.2026.06.20` (der schwerste: `for i in 0..n { c[i] }`
      **ausserhalb** eines `verus!`-Blocks, also genau auf dem Weg, der die GNATprove-Haltung gibt);
      `IndexSetTrustedSpec` in vstd versiegelt, weshalb ein **eigener** Container `x[i] = v` keine
      Vorbedingung geben kann (umgangen über einen `Deref`-Neutyp auf `Vec`); keine
      vstd-Spezifikationen für Iterator-Adapter (`position`, `filter`, `count`, `copied`,
      `enumerate`) — allein deswegen sind **6** Funktionen von `space.rs` unerreichbar;
      `#[derive(Debug)]` unverifizierbar und `#[derive(PartialEq)]` **fällt durch**.

**Werkzeugkette:** `alr install gnatprove` + `gnat_native`, vollständig unter `~/.alire`, ohne
root. `tools/spark-beweis.sh` ist ein **Gatter** (Ratsche in beide Richtungen, Abdeckungsprüfung
gegen heimliches `SPARK_Mode => Off`, zu wenige Prüfungen gelten als **ausgefallener** Lauf) und
hat beim ersten Lauf einen eigenen Fehler gefangen.

## Z28. Syscall-ABIs als PLUGGABLE Module — entschieden 2026-08-13

**Klasse:** Architektur · **Stand:** entschieden, nicht begonnen. Setzt die Nutzlast von
[Z26/A3](#a3--das-kernel-primitiv-ist-gebaut-2026-08-10-was-steht-was-offen-ist-und-die-schwelle)
voraus.

Der Kernel kennt nur `(Gast, Slot, Anlass)` und **keine Semantik** — das ist gebaut. Daraus folgt
eine Architektur, die bisher nur implizit da war: **das Syscall-ABI ist ein Userland-Begriff.**
Prozess A → Linux, B → WASI, C → nativ ist ein Tabellenblick, kein Kernel-Umbau.

### Die Gruppen — der Schnitt folgt dem GETEILTEN ZUSTAND, nicht der Syscall-Nummer

Eine Zerlegung „je Syscall" wäre nominell: 350 Handler in **einer** PD teilen einen Adressraum,
also erreicht jeder den Speicher jedes anderen — die Cap regelt, wer *gerufen* wird, nicht, wer
*lesen* kann. Der tragende Schnitt ist der entlang der geteilten Datenstruktur:

| Gruppe | teilt | Grösse |
|---|---|---|
| Prozess/Thread | TCB-Zustand, Scheduling | klein |
| Zeit | Uhr, Timer | winzig |
| Zufall | Entropiequelle | winzig |
| Identität | uid/gid/Credentials | winzig |
| Synchronisation | `futex`, Warteschlangen | klein |
| Speicher | Speicherkarte | mittel |
| fd/Dateien | fd-Tabelle, VFS | **gross** |
| Netz | Sockets | **gross** |
| Geräte | Geräte-Caps, DMA-Fenster | mittel |
| Native Dienste | Caprock-Caps direkt | klein |

**Die Verteilung der Grössen ist der eigentliche Befund: sechs sind winzig, zwei sind gross.**
Genau das macht die Beweisstufe unten möglich — *man verifiziert, was klein ist, und **isoliert**,
was gross ist.*

**Die Gruppen tragen ABGESTUFTE AUTORITÄT, und das ist ihr Zweck.** Der Kernel lernt damit keine
Syscall-Semantik, sondern **Autoritätsklassen**: eine Cap der Klasse „fs" darf Gast-Speicher
berühren, eine der Klasse „Netz" keinen Vspace anfassen. Fünf bis zehn benannte Klassen sind
**auditierbar**; eine freie Autoritätsmaske ist es nicht — dieselbe Entscheidung wie bei
`IdentityReason` (geschlossenes Enum statt wählbarem Argument).

- [ ] **Keine Auffanggruppe.** „Passt in keine Gruppe" braucht eine **benannte Absage**. Ein
      „Sonstiges" würde zur Halde und erbte die **Vereinigung aller Autoritäten** — dann ist die
      Klassifikation weg, und niemand merkt es.
- [ ] **Routing je BEREICH, nicht nur je Gast.** Heute: ein Handler je Gast. Eine gemischte
      Laufzeit braucht „0…350 an Linux, 400…750 an nativ". Der Schritt ist klein, solange die
      Tabelle neu ist.
- [ ] **Kein implizites Weiterreichen.** „Handler kennt N nicht → fällt an Linux durch" gäbe einem
      Handler Autorität über einen Gast, der ihn **nie gewählt hat**. Nicht abgedeckt heisst
      **faulten**; wer eine Rückfallebene will, trägt sie ausdrücklich ein.
- [ ] **Gruppenübergreifende Syscalls kosten mehrere Umläufe.** `mmap` einer Datei berührt Speicher
      *und* fs. Die Gruppen partitionieren den **Einstieg**, nicht die Arbeit. Wie teuer das ist,
      hängt an der Zahl, die es im ganzen Baum nicht gibt (s. unten).

### DMA erreicht den Kernel nicht — und das steht schon

Die Forderung ist **strukturell erfüllt**, nicht durch Sorgfalt: `addr::Pa` und `addr::Iova` sind
getrennte Typen, `DmaRegion::identity` wurde **absichtlich gelöscht**, IOVA-Fenster liegen
**oberhalb `RAM_TOP`** mit Schutzbändern und ohne Wiederverwendung — oberhalb `RAM_TOP` kann eine
IOVA nie zufällig eine gültige PA sein. Durchgesetzt wird es von der IOMMU, nicht vom Treiber.
Ein Handler der Geräte-Gruppe **kann** also keine IOVA konstruieren, die Kernelspeicher benennt.

- [ ] **Als Eigenschaft der GRUPPE festschreiben, nicht ihres Codes:** die Geräte-Gruppe ist die
      einzige, die DMA-fähige Caps weitergibt. Heute folgt das aus dem Code; es gehört in die
      Autoritätsklasse.

### Die Beweisstufe: EINGESCHLOSSENHEIT statt Korrektheit

**Die Eintrittskarte für ein Modul ist nicht „bewiesen richtig", sondern „kann nicht ausbrechen".**
Das ist um Grössenordnungen billiger und im Projekt bereits das Muster: `caprock-part` und
`caprock-fat` sind abhängigkeitsfrei und `forbid(unsafe_code)`, mit genau dieser Begründung —
*fremde Plattenbytes werden nirgends mit Kernprivileg interpretiert.* Ein falsch gelesener
FAT-Eintrag liefert Unsinn; er wird nicht zu Codeausführung.

Die Eigenschaft zerfällt in vier, drei davon fast gratis:

| Teil | wie |
|---|---|
| **keine fremde Logik ausführen** | in sicherem Rust gibt es keinen Weg von Bytes zu einem Funktionszeiger — dafür braucht es `transmute`. **`forbid(unsafe_code)` deckt es mechanisch ab** |
| **keine Autorität ausserhalb der Gruppe** | die Cap — also ohnehin der Entwurf |
| **Speichersicherheit** | fällt mit `forbid(unsafe_code)` ab |
| **Termination** | **NICHT gratis.** Sicheres Rust darf ewig schleifen, und ein hängender Handler ist ein DoS für seinen Gast |

- [ ] **Termination kernelseitig lösen, nicht durch Beweis:** eine **Frist** je umgeleitetem
      Syscall, danach faultet der Gast mit benanntem Grund. Das ist die vorhandene
      Fail-closed-Regel mit einer Uhr — und sie deckt zusätzlich den Fall ab, den kein Beweis
      abdeckt: ein Handler, der auf etwas *anderes* wartet.

> **Der Wächter STEHT** (`tools/eingeschlossenheit.py`, 2026-08-13, in `tools/abnahme.sh`, 0,49 s):
> ein Modul mit `[package.metadata.caprock] einschluss = "streng"` trägt `forbid(unsafe_code)`,
> hat kein Bauskript und hängt nur von benannten Crates ab; 9 Module deklariert, 15 Sprechproben
> in beide Richtungen, 0 Kandidaten = Rückgabecode 3. Details und die fünf Befunde, die er dabei
> aufgedeckt hat, in [done.md](done.md) — drei davon im **signierten** Unsafe-Audit von ADR 0014
> und deshalb als eigener offener Punkt weiter unten. **Diese Zeile steht hier, weil die folgenden
> Punkte an ihm hängen** — nicht als Erfolgsregister.

- [ ] **Was der Wächter NICHT weiß: welche Module das Primitiv registriert.** Er prüft, was sich
      *deklariert*; eine Registertabelle gibt es nicht, also ist die Frage „ist jedes registrierte
      Modul auch deklariert?" heute strukturell unbeantwortbar. Sobald die Tabelle aus der
      Nutzlast von [Z26/A3](#a3--das-kernel-primitiv-ist-gebaut-2026-08-10-was-steht-was-offen-ist-und-die-schwelle)
      existiert, gehört sie als **zweite Quelle** in den Wächter — sonst ist die Zugehörigkeit
      genau so lange freiwillig, wie es niemandem auffällt.

- [ ] **Die Marshalling-Naht braucht echte Aufmerksamkeit — sie ist die eine Tür.** Dort treffen
      gastkontrollierte Bytes auf Handlercode. `forbid(unsafe_code)` **und** Prüfung des
      Versionskopfs statt Interpretation. Ist sie nicht sauber, nützt die Sauberkeit aller 350
      Module nichts, weil alle durch dieselbe Tür kommen.

**Was bleibt und benannt gehört:** ein eingeschlossenes Modul kann weiterhin **falsch** sein. Das
ist genau dann tragbar, wenn die Zerlegung hält — **ein Gast, eine Gruppe** —, denn dann ist ein
falscher Handler das Problem *seines* Gastes und nicht des Systems. Diese Aussage ist der ganze
Gewinn, und sie hängt an der Cap-Grenze, nicht am Code.

### Volle Verifikation: wo sie trägt, und wo sie eine Wunschform bewiese

- [ ] **Für ein NATIVES Caprock-ABI ist der Beweis von Ende zu Ende sinnvoll** — die Spezifikation
      ist unsere. Hier zuerst anfangen, mit der kleinsten Gruppe (Zeit oder Identität).
- [ ] **Für Linux nicht.** „Was Linux tut" existiert **nirgends formal**; man verifiziert gegen
      *seine eigene* Spezifikation, und ob die Linux entspricht, ist unbewiesen — genau dort
      wohnen die Kompatibilitätsfehler. Das ist wörtlich die Falle „ein Beweis, der die Wunschform
      beweist, ist schlechter als keiner" (`send_no_loss` galt am echten Endpoint nicht), und es
      deckt sich mit dem Registerbefund zu [Z16](#z16): **WSL1 scheiterte an der Verhaltenstreue**,
      nicht an Speicherfehlern. Verifikation kauft Speichersicherheit und innere Konsistenz —
      Verhaltenstreue kauft sie nicht. Ein verifizierter Linux-Handler ist deshalb **„bewiesen
      gegen unser Modell"** zu beschriften, nicht „bewiesen".
- [ ] **Modell-Treue-Wächter von Anfang an**, nicht nachträglich. Ohne ihn läuft das Modell vom
      Code weg; dieses Projekt hat drei davon gebaut, weil genau das dreimal passiert ist.
- [ ] **Beweisstufe als Zahl führen:** wie viele Module mit Beweis registriert sind und wie viele
      ohne — als Ratsche, die nur fallen darf.

### Die Zahl, an der das alles hängt

**Es gibt im ganzen Baum keine Zyklenmessung eines IPC-Umlaufs.** Sie entscheidet, wie fein die
Zerlegung sein darf (jede Gruppengrenze in einer Kette kostet einen weiteren Umlauf), ob ein
Handler-Thread je Gast oder ein migrierender Thread nötig ist ([B](#b-thread-migration--rest)) —
und ob der Kernel unter Druck doch ABI-Wissen bekommt, was die ganze Pluggability beendete.
Die Schwelle steht seit dem 2026-08-10 fest: **umgeleiteter `getpid` ≤ 2000 Zyklen Median**.

- [ ] **Messen, sobald die Nutzlast steht und die Maschine ruhig ist** — Handler in isolierter PD
      **gegen** Handler in `TrustedSas`; die Differenz *ist* der Preis der Isolation. Dazu der
      nackte IPC-Umlauf als Sockel. Nicht unter paralleler Last: dieser Messstand hat das Projekt
      schon einmal mit 3,2-facher Überbuchung in die Irre geführt (D13).

## Der Unsafe-Audit von ADR 0014 hat drei Löcher — gefunden beim Bau des Z28-Wächters (2026-08-13)

**Klasse:** Prüferform / Vertrauenskette · **Stand:** offen. Keins der drei ist heute **wirksam**;
alle drei sind gemessen, nicht vermutet.

Das Zertifikat einer TrustedSAS-PD trägt `unsafe_status`, der Kernel erzwingt beim Laden
`== UNSAFE_ALL_PASS` (ADR 0014 §6, `crates/caprock-loader/src/cert.rs`). Die Bits berechnet
`tools/sign_trusted.py`. Die Aussage ist also **signiert** — und damit ist ein Loch in ihrer
Berechnung schlimmer als eins in einem gewöhnlichen Prüfer: es wird kryptographisch beglaubigt.

- [ ] **`has_forbid` nimmt das Attribut aus einem KOMMENTAR an.** Freies `re.search` über den
      rohen Dateiinhalt; die bloße Erwähnung `` `#![forbid(unsafe_code)]` `` in einem
      Doku-Kommentar genügt. In `crates/caprock-loader/src/lib.rs` Zeile 3 steht eine solche
      Erwähnung (dort zusätzlich zum echten Attribut, also ohne Wirkung). `UNSAFE_PROGRAM_FORBID`
      lässt sich damit **mit Prosa verdienen**. Abhilfe steht daneben:
      `tools/eingeschlossenheit.py` entfernt Kommentare und verankert zeilenanfangs.
- [ ] **`unsafe{` ohne Leerzeichen zählt nicht mit.** `RX_UNSAFE` verlangt `\s+`; `unsafe{ … }`
      ist gültiges Rust. **Gemessen: 0 Vorkommen im ganzen Baum** — das Loch ist unbenutzt, nicht
      abwesend. Der Einzeiler ist `\bunsafe\b\s*`; die Verschärfung kann nur mehr finden, nie
      weniger.
- [ ] **Die einzige Crate der Allowlist hat ein Bauskript.** `programs/libcaprock/build.rs` läuft
      zur Bauzeit auf dem **Wirt** mit voller Autorität; `forbid(unsafe_code)` berührt es nicht,
      und der Audit sieht nur `src/`. Heute harmlos (RUSTFLAGS-Prüfung), aber es sitzt in der
      Vertrauenskette **jedes** TrustedSAS-Programms. Entweder in den Audit aufnehmen oder
      ausdrücklich als Schuld führen — stillschweigend außerhalb ist die dritte Möglichkeit, die
      es nicht geben darf.
- [ ] **Zwei Parser für dieselbe Bedingung wären der nächste Riss.** Der Z28-Wächter hält seine
      Menge heute gegen `ALLOWLIST` in `sign_trusted.py` (mit Ankertest). Richtiger wäre **eine**
      Zählung mit zwei Verbrauchern — dasselbe Muster wie `tools/mangel-zaehlen.py`, das
      `kernel/build.rs` und `tools/mangel-stellen.sh` gemeinsam benutzen.

## Zwei Fremdbyte-Parser sind eingeschlossen in der SACHE, nicht im TYP (2026-08-13)

**Klasse:** Härtung · **Stand:** offen, gemessen mit `tools/eingeschlossenheit.py --liste`.

`caprock-dtb` (Device-Tree-Parser) und `caprock-abi` haben **0 `unsafe` und keine
Abhängigkeiten**, tragen aber kein `#![forbid(unsafe_code)]`. Bei `caprock-dtb` ist das genau die
Klasse von `caprock-part`/`caprock-fat`: **fremde Bytes**, die im Hochlauf gelesen werden. „Heute
kein `unsafe`" ist eine Momentaufnahme; `forbid` ist eine Bedingung.

- [ ] Beiden `#![forbid(unsafe_code)]` geben und sie deklarieren — oder in
      `AUSDRUECKLICH_NICHT` absagen, mit Grund. Zwei Zeilen je Crate; der Wächter zieht die
      Konsequenz von selbst nach (er prüft dann transitiv mit).

## Z. Zielarchitektur (Stand 2026-07-29) — woran alles andere zu messen ist

> **Reihenfolge und Begründung:** [docs/plan-betriebsbereit.md](docs/plan-betriebsbereit.md).
> Hier steht *was* offen ist, dort *in welcher Reihenfolge* und *warum*.
>
> **Zur parallelen Bearbeitung geteilt in zwei unabhängige Stränge:**
> [todo-A-ausfuehren.md](todo-A-ausfuehren.md) — der Kernel führt fremden Code aus und tauscht ihn
> aus (Manifest, Root-Task, Caps, Hot-Reload, Treiber). **Kritischer Pfad.**
> [todo-B-verlaesslichkeit.md](todo-B-verlaesslichkeit.md) — die Zusicherungen tragen und sind
> messbar (Determinismus, ARM-Zweig, IOMMU, Färbung/SMT, Zeit/NUMA, Attestierung).
> Dateibesitz und die vier echten Kopplungen stehen am Ende von Strang A.

Der Mikrokern ist **das Basissystem eines Cloud-Servers**, nicht ein Hypervisor darunter und
keine VM darin. Es gibt keine Gastschicht: Isolation, Zeit- und Ressourcenverwaltung macht der
Kern selbst. Daraus folgen drei Anforderungen, die keine reine Fleißarbeit sind, sondern den
Entwurf mitbestimmen — und an denen die Abschnitte A–G im Folgenden hängen.

**Die Lastform (Simon, 2026-08-01): Vercel-artige Dienste.** Kurzlebige Funktionsaufrufe, hohe
Fluktuation, **viele Mandanten je Maschine**, fremder Code. Das ist keine Randnotiz, sondern legt
drei Größen fest, die sonst frei wählbar aussähen:

* **Spawn/Teardown ist der heiße Pfad**, nicht der Startpfad. Genau dort saßen die D0-Rennen.
* **Die Zahl gleichzeitig getrennter Mandanten ist heute 4.** `PARTITIONS = 4`, und `claim_stripe`
  scheitert ab dem fünften sauber (B-4.2) — richtig so, aber vier cache-getrennte Mandanten je
  Maschine sind für diese Lastform um Größenordnungen zu wenig. Und Erhöhen ist nicht gratis:
  `region_bytes()` ist `min(Farben, 64) / PARTITIONS` Seiten, bei `PARTITIONS = 64` also **eine
  Seite** je zusammenhängender Region. Diese Spannung ist der eigentliche Inhalt von B-4.1.
* **Die Maschine muss Blech sein.** Neu belegt am 2026-08-01 (B-4.5): läuft Caprock selbst als
  Gast, schreibt der Wirt die Farbbits um, und die Cache-Trennung ist wirkungslos — gemessen, nicht
  vermutet. Wer „Isolation ohne VMs" verkauft und dafür gemietete VMs benutzt, verkauft nichts.

**Z1. Kein VM-Overhead, Isolation vollständig durch den Kern.** Der isolierte Pfad (eigene VSpace
je PD, gefärbt) muss der **Normalfall** werden, nicht die Ausnahme. Heute ist `spawn_isolated` der
reguläre Weg und ungefärbt; `spawn_isolated_colored` ist der Sonderweg (s. [A1](#a1-cache--timing-seitenkanäle-zwischen-pds)).

**Gesetzte Regel (Simon, 2026-07-29): TrustedSAS ist ausschließlich für eigenen Code. Kein
Kundencode läuft jemals in einer TrustedSAS-PD.** Das ist keine Vorsichtsmaßnahme, sondern folgt
aus dem Modell: TrustedSAS-PDs teilen sich einen Adressraum, und ihre Isolation ruht auf
*intralingualer* Sicherheit — safe Rust kann keinen Zeiger auf fremden Speicher erzeugen. Diese
Voraussetzung lässt sich nur an eigenem Quellcode prüfen (das Zertifikats-Gate ADR 0014 tut genau
das: `unsafe_status == ALL_PASS`), niemals an einem fremden Binary.

Drei Folgerungen, die den Entwurf binden:

* **Der isolierte Pfad ist der Produktpfad.** Seine Leistung und seine Skalierbarkeit *sind* das
  Produkt — nicht die des SAS-Pfads. Optimierungsarbeit gehört dorthin.
* **Das Zertifikats-Gate ist ein Betreiberwerkzeug, keine Kundenschnittstelle.** Kunden brauchen
  kein Zertifikat und bekommen keins; ein Weg, auf dem ein Kunde ein TrustedSAS-Zertifikat
  erlangen könnte, wäre ein Entwurfsfehler. Umgekehrt heißt das: [Z7](#z7-attestierung-und-messbarer-boot)
  betrifft die **Maschine**, nicht das Kundenbinary — der Kunde will wissen, *worauf* er läuft,
  nicht beweisen, *was* er mitbringt.
* **Zwei Klassen, zwei Zusicherungen.** Eigene Dienste (Treiber, Netzstack, Kontrollebene) dürfen
  TrustedSAS sein und zahlen dafür keinen Adressraumwechsel. Alles Fremde ist isoliert, gefärbt
  und wird so abgerechnet. Das gehört in `docs/invariants.md`, damit die Grenze nicht später aus
  Bequemlichkeit verwischt.

**Z2. Abrechnung feiner als der Zeittakt — „nur zahlen, was man braucht".** 100 Hz heißt 10 ms
Granularität; das ist zugleich die Untergrenze der Abrechnung *und* eine Jitterquelle, die HPC
gerade abschaltet. Ziel ist eine Abrechnung, die **nicht** an der Tick-Rate hängt: Verbrauch wird
per TSC/CNTVCT beim Ein- und Auswechseln gestempelt (Zyklen, nicht Ticks), und der Tick selbst
wird für Rechenkerne abgeschaltet statt beschleunigt. Ein schnellerer Tick wäre die falsche
Antwort — er erhöht die Auflösung *und* den Overhead; ein Zyklenstempel erhöht nur die Auflösung.
Siehe [Z5](#z5-tickless-für-rechenkerne) und [D](#d-verifikation) (per-TCB-`consumed_cycles`).

**Z3. Threads einfrieren, versetzen, weiterlaufen lassen (Server-übergreifend).** Das ist die
Eigenschaft, die VMs heute liefern (Live-Migration) und die ein VM-freier Entwurf ersetzen muss.
Sie ist die **anspruchsvollste** Anforderung im ganzen Dokument, weil sie quer zu fast allem liegt:
ein Thread ist nicht nur sein Registersatz, sondern sein Speicher, seine Caps, seine
IPC-Beziehungen und seine Position in den Kerneltabellen. Zerlegung in [Z4](#z4-checkpointrestore-eines-threads).

### Z11. Boot-Image = Kernel + **eine** Manifestdatei; alles andere außerhalb und austauschbar
**Klasse:** Aufbau/Betrieb · **Aufwand:** groß, betrifft Loader, Cap-System und IPC

**Gesetzte Regel (Simon, 2026-07-29):** Im Boot-Image liegen genau zwei Dinge — der Kernel und
eine Datei, die festlegt, *was* geladen wird. Alles Übrige (Treiber, Netzstack, Dienste,
Kontrollebene) liegt außerhalb des Images und ist **zur Laufzeit austauschbar, ohne dass IPC-
Beziehungen verlorengehen**.

Das ist die schärfere Fassung der bestehenden Projektgrenze („nur Mikrokern, Caps, Scheduler, IPC
und Microkit-Runtime im Image") und zieht Konsequenzen nach sich, die über sie hinausgehen.

- [ ] **Z11a. Das Henne-Ei-Problem benennen und lösen.** *(Bootloader-Weg belegt 2026-08-01:
      `tools/mkgrubiso.sh` baut ein GRUB-ISO aus Kernel + Boot-Archiv; ein echter Bootloader
      liefert nachweislich dasselbe wie QEMUs `-kernel`/`-initrd` — Speicherplan, ein Modul,
      `mbmod : ALL PASS`, `archive : 2 Modul(e) -> ALL PASS`, Root-Task läuft,
      `SELFTEST COMPLETE`. Einziger Unterschied: die Ladeadresse des Moduls, wie erwartet.
      Die Sorge wegen `Flags = 0` im Multiboot-Header war unbegründet — GRUB liefert
      Speicherplan und Module auch ungefragt. **Offen bleibt die zweite Hälfte:** das
      Nachladen zur Laufzeit. Der Plattentreiber dafür ist da und läuft seit A-5.1 (2026-08-01)
      als **geladenes Userland-Programm** — es bedient Leseanfragen über seinen Kanal, inhaltlich
      geprüft. Was noch fehlt, ist alles darüber: eine Partitionstabelle und ein Dateisystem.
      Ein Sektor ist noch kein Archiv.)*
      Einen Plattentreiber kann man nicht von
      der Platte laden. Der einzige ehrliche Ausweg: **der Bootloader liefert die Startmodule**
      (Multiboot-Module bei GRUB) — die liegen damit außerhalb des Kernel-Images, kommen aber
      trotzdem zum Boot an. Das Manifest nennt sie; der Kernel prüft, dass genau das ankam, was
      dort steht. Nachladen über Netz/Platte gibt es erst, wenn die zugehörigen Treiber laufen.
      Ohne diese Trennung („Startmenge vom Bootloader" vs. „Nachladen zur Laufzeit") wird das
      Manifest ein Wunschzettel, den niemand einlösen kann.
- [~] **Z11c. Die Politik steht im Manifest und wird ANGEWANDT** (2026-08-07). Bis dahin las der
      Kernel die Felder und **druckte** sie; eingehalten wurde nur `POLICY_ROOT_TASK`. Ein
      Politikfeld, das nur gedruckt wird, ist schlechter als keins — es sieht konfiguriert aus.

      | Feld | Stand |
      |---|---|
      | `POLICY_EXCLUSIVE_STRIPE` | **eingehalten** (A1, `pdcolor : ALL PASS`) |
      | `priority`, `core_affinity` | **eingehalten** (`ladepol : ALL PASS` — prio 2 verlangt, prio 2 bekommen) |
      | `POLICY_ROOT_TASK`, `POLICY_NO_HOTRELOAD`, `iface_version` | eingehalten (A-2.1 / A-4.5 / A-4.4) |
      | `numa_node != 0` | **abgewiesen** — der Allokator hat keine Knoten (Z8) |
      | `POLICY_PINNED` | **abgewiesen** — „Lastausgleich ist per Vorgabe aus" ist keine Zusicherung |
      | `budget_us != 0` | **abgewiesen**, s. den offenen Punkt darunter |

      **Offen: `budget_us` ist als Format nicht einhaltbar.** Eine MCS-Reservierung braucht Budget
      **und** Periode; das Manifest hat eine Zahl. Aus einer Zahl eine Reservierung zu machen hieße,
      die Periode zu erfinden — sie stünde dann in keinem Dokument. Dazu die Auflösung: `set_budget`
      rechnet in Ticks (100 Hz → 10 000 µs), alles darunter wäre 0 oder aufgerundet. Das ist eine
      **Formatfrage**: entweder ein `period_us`-Feld (die reservierten Bytes sind weg, also eine
      neue `entry_len` und damit eine Formatversion), oder `budget_us` fällt.

      **Offen: `priority = 0` ist nicht von „nichts gesagt" zu unterscheiden.** 0 ist im Scheduler
      die niedrigste gültige Priorität; heute bekommt ein Eintrag mit 0 die Vorgabe. Wer wirklich 0
      will, kann es nicht sagen. Dieselbe Formatfrage wie oben.

      **ERLEDIGT, und es ist die Vorbedingung für beide: die Absage bei unbekanntem Format ist
      LAUT.** (2026-08-10) Die Regel, ausgesprochen bevor irgendjemand die Formatversion anfasst:

      > Ein Lader, der auf eine `format_version` oder `entry_len` trifft, die er nicht kennt,
      > **weist ab** — und die Absage sagt, dass es ein **Versionsunterschied** ist, nicht ein
      > Formfehler. Er liest **niemals** die ihm bekannten Felder heraus und überspringt den Rest.

      Beide Hälften sind nötig, und die erste ist die schärfere: **signiert wird `[0..msg_len)`,
      also die ganze Nachricht.** Ein v2-Manifest, aus dem ein v1-Lader 96 von 104 Byte je Eintrag
      liest, wäre **authentisch und unverstanden zugleich** — eine gültige Signatur belegt
      Echtheit, **nicht Verständnis**, und der Byteversatz verschöbe ausgerechnet `initial_caps`,
      `policy_flags` und `priority`: eine **Autoritätszuteilung**, still verrutscht.

      Abgewiesen hat der Parser schon vorher — aber **stumm**: sechs strukturell verschiedene
      Gründe endeten in einem `BadManifest`, und sieben Host-Tests schrieben diese
      Ununterscheidbarkeit fest. Zwei davon heißen „neuer als dieser Kernel", vier „kaputte
      Bytes", und sie führen zu **entgegengesetzten** Handlungen. Im Kernel endete es als
      `im Archiv liegen N B, die nicht parsen` — drei Zeilen unter einem Kommentar, der für den
      **Kernel-Hash** genau diese Lehre schon zieht („die häufigste echte Ursache ist *neuer
      Kernel, altes Manifest*, deshalb steht hier, WORAN gebunden wurde — sonst rät man").
      Für den Hash gezogen, für die Formatversion nicht. Und eine Datei weiter unterscheidet
      `Archive::parse` längst **fünf** benannte Gründe — ausgerechnet der sicherheitskritischere
      Parser tat es nicht.

      Neu: `LoaderError::UnsupportedManifestFormat { format_version, entry_len }` — **mit den
      Zahlen**, denn eine Absage ohne sie ist dieselbe Rateübung wie vorher. Kernel-Berichtszeile
      mit beiden Werten *und* dem, was dieser Kernel kann. Host-Tests: die vier Formfälle bleiben
      `BadManifest`, zwei neue Fälle ergeben die neue Variante mit den richtigen Zahlen, dazu eine
      **Sprechprobe der Unterscheidung selbst** (`assert_ne!`) — ohne die wären beide Tests auch
      dann grün, wenn die Varianten denselben Wert hätten. System-Negativfall 3b in der Lade-Suite:
      `entry_len=104` → benannte Absage, und die Zahl 104 erscheint nur, wenn der Kernel wirklich
      diesen Zweig genommen hat.

      **Der Nachtrag, der zur ehrlichen Fassung gehört:** die Absage fällt **vor** der
      Signaturprüfung — sie muss, man kann nicht verifizieren, was man nicht parsen kann. Die
      Diagnose ist damit **unauthentifiziert** und durch ein gekipptes Byte provozierbar. Was der
      signierte Kopf trägt, ist die **andere** Richtung: ein *angenommenes* Manifest kann keine
      untergeschobene Formatversion haben. Beide Hälften stehen im Code; nur die angenehme zu
      nennen machte daraus eine Zusicherung, die nicht gilt.

      Damit ist der Weg zu Formatversion 2 (`period_us`, `priority`-Sagbarkeit) frei: ein
      v2-Manifest auf einem v1-Kernel ist ab jetzt **von Korruption unterscheidbar**. Vorher wäre
      der Formatsprung ein Feldeinsatz mit einer Fehlermeldung gewesen, die in die falsche
      Richtung zeigt.

      **Ein Befund nebenbei, der größer ist als der Eintrag:** die Prioritäten standen seit jeher
      im Test-Manifest (3/1/2/2/2) und wurden nie eingelöst — es waren Platzhalter. Eingehalten
      **reißt** dieselbe Zuteilung die Lade-Suite: ein *pollender* Treiber (B-3.2, kein IRQ) auf
      einer höheren Priorität als sein Client lässt den Client verhungern. Richtig zugeteilt und
      trotzdem unbrauchbar. Ein Feld, das nie eingelöst wird, sammelt Werte an, die niemand geprüft
      hat — und der Tag, an dem es eingelöst wird, ist der Tag, an dem sie alle falsch sind.
      (Genau deshalb braucht ein pollender Treiber ein **Budget**, s. oben.)

- [ ] **Z11d. Hot-Reload ohne IPC-Verlust — was dafür wirklich nötig ist.** Auf ARM existiert der
      Fall bereits (Phase 7: eine Server-PD wird über *dieselbe* Endpoint-Cap ersetzt). Für einen
      Betriebsanspruch reicht das aber nicht; drei Dinge fehlen:
      1. **Endpoint-Identität unabhängig vom Thread.** Clients halten Caps auf den *Endpoint*,
         nicht auf den Server-Thread — das trägt heute schon. Der Austausch ist dann ein
         Umbinden, wer auf dem Endpoint empfängt. Das muss **atomar** sein: zwischen „alter
         Server weg" und „neuer Server empfangsbereit" darf kein Zustand liegen, in dem ein
         `CALL` mit `NoEndpoint` scheitert.
      2. **Schwebende Transaktionen.** Ein Client, der in `CALL` blockiert, hält eine Reply-Cap
         auf den alten Server. Entweder der neue Server erbt die offenen Replys, oder der
         Austausch findet nur an einem *ruhenden* Punkt statt (keine offene Transaktion). Die
         zweite Variante ist ehrlich und für Stufe 1 wahrscheinlich richtig — sie braucht aber
         einen Begriff von „ruhend", den es heute nicht gibt. Verwandt mit [Z4a](#z4-checkpointrestore-eines-threads):
         derselbe Haltepunkt-Begriff trägt beides.
      3. **Zustandsübergabe.** Die neue Fassung braucht den Zustand der alten. Entweder eine
         Speicherregion, die den Austausch überlebt (dann ist ihr Format eine ABI und muss
         versioniert werden), oder ein ausdrückliches Übergabeprotokoll. Ohne Festlegung wird
         Hot-Reload ein Neustart mit Datenverlust und heißt nur anders.
### Z4. Checkpoint/Restore eines Threads
**Klasse:** neues Subsystem · **Aufwand:** groß, mehrstufig

Reihenfolge ist hier keine Geschmacksfrage — jede Stufe ist Vorbedingung der nächsten.

- [~] **Z4a: der Haltepunkt steht** (2026-08-02, `system::freeze_thread`, Prüfzeile `freeze`).
      Details in [done.md](done.md#z4a--z4b-der-haltepunkt-und-was-auf-keinen-fall-mitwandert).
      „Benennbar" heisst zwei prüfbare Dinge: **nicht auf einem Kern** (über `current_id` je Kern
      gefragt, nicht geglaubt) und **keine offene IPC-Beziehung**. Die drei Ausgänge sind
      unterscheidbar, und der Unterschied trägt: `StillRunning` ist vorübergehend, `Busy` ist eine
      **Absage** — wer darauf wartet, wartet ewig. Geprüft wird die **Wirkung**: der Rundenzähler
      muss sich vorher bewegen, dann stehen, dann wieder laufen; zwei Mutationen machen die Suite
      rot.

      **Offen bleibt der Syscall.** `freeze_thread` ist eine Kernel-Funktion, kein `SYS_FREEZE`,
      und sie **wartet nicht** — sie nimmt die Scheduler-Sperre jedes Kerns, und darin auf einen
      anderen Kern zu warten ist die Bauanleitung für einen Deadlock. Der Aufruf ist idempotent;
      das Warten gehört dem Aufrufer. Ein blockierender Syscall darüber ist eine ABI-Frage und
      eine eigene Stufe.
- [~] **Z4b: die Verweigerungsregel steht, der Serialisierer nicht** (2026-08-02,
      `crates/caprock-cap/src/checkpoint.rs`, 7 Host-Tests). Der Registersatz war ohnehin der
      leichte Teil; gebaut ist der schwere: **was auf der Zielmaschine nicht dasselbe bezeichnen
      kann, wandert nicht mit — es wird verweigert, nicht ersetzt.** Verweigert werden `Mmio`,
      `Irq`, `Dma`, `Reply`, `Loader` und jede Beziehungs-Cap, deren Partner nicht im Umfang ist;
      die Gründe sind **einzeln** und kein Sammel-`false`, weil nur manche durch einen grösseren
      Umfang behebbar sind. Fail-closed: ein neuer Objekttyp wandert nicht mit, statt
      durchzurutschen.

      Drei Entscheidungen stecken in den Tests: Speicher wandert als **Inhalt** (zwei Regionen
      gleicher Länge an verschiedenen Physadressen sind extern gleich), Beziehungen über den
      **Platz im Checkpoint** statt über eine ID, und ein Budget trägt eine **Vorbedingung**
      (`SameTickSemantics`, Z4f) statt sie zu verschweigen.

      ~~**Offen:** ein Serialisierer und ein Wiederhersteller.~~ Beide stehen seit 2026-08-03,
      s. Stufe 2 direkt darunter.
- [~] **Z4 Stufe 2: ein Thread über die BOOTGRENZE** (2026-08-03, Prüfzeile `bootckpt`).
      Details in [done.md](done.md#z4-stufe-2-ein-thread-ueber-die-bootgrenze). Derselbe Kernel
      speichert und stellt wieder her und **entscheidet selbst welches** — er liest den Sektor
      (`CKPT_SECTOR = 32710`, ausserhalb beider Partitionen). Transport ist der vorhandene
      Blockdienst; der Kern ist Client, `programs/hardware/virtio-blk` blieb unverändert. Das
      Format liegt in `crates/caprock-cap/src/checkpoint.rs` (feste Breiten, LE, CRC-32,
      host-getestet), und `Image::build` ruft `classify_all`: **eine nicht übertragbare Cap
      verhindert den Checkpoint vor dem Schreiben.**

      **Der Befund, der den Entwurf umgebaut hat, gehört in die Merkliste:** der Fortschrittszähler
      ist unter KVM **reproduzierbar** (gemessen 133…155 an derselben Stelle des Hochlaufs). Eine
      Mutation, die den gefundenen Wert liest und meldet, ohne ihn zu setzen, traf den gespeicherten
      Wert exakt — und die Suite blieb grün. Erst eine **wachsende Kette** (jeder Lauf arbeitet
      +100 Runden weiter und speichert mit Epoche + 1) trennt „geerbt" von „selbst gezählt"; die
      Positivkontrolle dazu (`Zaehler-vorher != Fortschritt`) steht in der Suite.

      **Offen bleibt hier:** kein Trap-Frame und keine Register — das wäre ohne
      [Z4c](#z4-checkpointrestore-eines-threads) wertlos oder schlimmer, weil `RSP` in einen Stack
      zeigt, dessen Inhalt nach dem Neustart frischer Speicher ist. Und der Checkpoint ist
      **nicht authentifiziert** (Z4e): die Prüfsumme sagt „strukturell heil", nicht „von wem".
- [ ] **Z4c. Speicher mitnehmen.** Die private Region der PD ist der Zustand. Bei 64 KiB
      ([A1](#a1-cache--timing-seitenkanäle-zwischen-pds)) ist das billig, bei 2 MiB weniger.
      Offen: Dirty-Tracking, damit nicht alles kopiert werden muss, und die Frage, ob die
      **Farbe** auf der Zielmaschine erhalten bleiben muss (nein — Farbe ist maschinenlokal; die
      Zielmaschine färbt neu; das ist ein Argument dafür, Farbe nie in die ABI zu heben).

      **Seit Stufe 2 ist das die BLOCKIERENDE Stufe, nicht eine nebenläufige.** Der Checkpoint
      trägt eine Speicher-Cap als `Region { len }` — als **Länge**, nicht als Inhalt; drüben ist
      sie null. Und ein Trap-Frame lässt sich erst dann sinnvoll mitnehmen, wenn der Stack
      mitkommt: `RSP` ohne Stackinhalt ist ein Zeiger ins Leere, und der Fehler tritt später und
      woanders auf.
- [ ] **Z4d. IPC-Beziehungen.** Ein wandernder Thread mit offenem `CALL` hat einen wartenden
      Server zurückgelassen. Entweder Migration nur ohne offene Transaktionen (einfach, ehrlich,
      wahrscheinlich richtig für Stufe 1), oder Endpoint-Proxys über das Netz (ein eigenes
      Projekt).
- [ ] **Z4e. Transport + Vertrauen.** Ein Checkpoint ist der vollständige Zustand eines Tenants.
      Er geht verschlüsselt und authentifiziert über das Netz, oder gar nicht. Hängt an
      [Z7](#z7-attestierung-und-messbarer-boot): die Zielmaschine muss nachweisen können, dass sie
      dieselbe Kernelversion mit denselben Zusicherungen fährt — sonst wandert der Zustand in eine
      schwächere Umgebung, und der Tenant merkt es nicht.
- [~] **Z4f. Identische Server als Vorbedingung benennen.** „gleicher Server" muss geprüft
      werden, nicht angenommen: gleiche Architektur, gleiche Kernelversion, gleiche
      Cache-/NUMA-Klasse. Ein Vergleich, der nur die Architektur prüft, lässt einen Thread von
      einer Maschine mit `invtsc` auf eine ohne wandern — und die Abrechnung aus [Z2](#z-zielarchitektur-stand-2026-07-29--woran-alles-andere-zu-messen-ist)
      wird still falsch.

      **Ein Stück davon steht (2026-08-03):** der Checkpoint trägt `kernel_code_hash` und wird
      abgewiesen, wenn er zu einem anderen Kernel-Image gehört (Lesecode 7, Negativfall in
      `test-qemu-x86-load.sh`). Das ist die **Kernelversion**, und nur sie.

      **Nicht geprüft — und das ist genau die Lücke, die dieser Punkt meint:** die MASCHINE. Zwei
      Rechner mit demselben Image, aber verschiedener Cache-/NUMA-Klasse oder einer ohne `invtsc`,
      sind heute nicht unterscheidbar. Die Vorbedingung `SameTickSemantics` steht **im** Checkpoint
      (`precondition_bits`), aber niemand wertet sie aus — sie zu tragen und nicht zu prüfen ist
      der halbe Weg, und der halbe Weg sieht von aussen aus wie der ganze.

#### Z12. Wire-Formate deklarativ beschreiben und erzeugen (Vorschlag Simon, 2026-08-07)
**Klasse:** Werkzeug/Userspace · **Aufwand:** klein für den ersten Schnitt, wächst mit dem Umfang

**Die Idee:** fremde Protokolle (Ethernet, später IPv4/UDP, EDID/Display) stehen als
Beschreibungsdatei im Repo und werden zu Rust übersetzt. Ein neues oder altes Format kostet dann
eine Datei statt eines Parsers.

**Das passt zur Kerngrenze, und zwar genau.** Erzeugte Parser sind reine Funktionen über
Byte-Slices — keine Caps, keine Syscalls, keine Allokation. Sie gehören dorthin, wo `caprock-part`
(502 Zeilen, 0 Abhängigkeiten, `forbid(unsafe_code)`) und `caprock-fat` (703 Zeilen, ebenso)
schon stehen: **Userspace**. Der Generator ist ein **Bauzeit-Werkzeug** auf dem Host; im Kern
ändert sich nichts.

**Wo der Ertrag wirklich liegt.** Nicht in der Bequemlichkeit, sondern in den zwei Fehlern, die
handgeschriebene Parser tatsächlich machen: **Endianness** und **Bereichsprüfung**. Ein Generator
macht beide gleichförmig, und mit `forbid(unsafe_code)` plus stets längengeprüften Zugriffen ist
das ein Sicherheitsgewinn, kein Komfortgewinn.

**Der Umfang entscheidet über den Nutzen — und er ist ungleich verteilt:**

| Format | Anteil, der LAYOUT ist | Eignung |
|---|---|---|
| Ethernet II / 802.3 / VLAN, ARP | fast alles | **sehr gut** |
| IPv4/UDP-Köpfe | groß (Optionen sind der Rest) | gut |
| EDID (128-Byte-Blöcke) | fast alles | gut |
| TCP | der Kopf ist ein Bruchteil | schlecht — der Zustandsautomat ist die Arbeit |
| Display/Grafik allgemein | wenig (Register, Timings, Framebuffer) | schlecht — kein Wire-Format |

**„Nur die Protokolldatei ändern, nichts am Code" gilt fürs LESEN und SCHREIBEN, nicht fürs
VERHALTEN.** Eine Tabelle erzeugt den Kopf; sie erzeugt keine ARP-Auflösung, kein DHCP, keine
Neuübertragung, kein Mode-Setting. Bei Ethernet/ARP ist das Verhältnis günstig (der Rahmen *ist*
fast die ganze Arbeit), bei TCP nicht. Das sollte den Geltungsbereich bestimmen, nicht die
Begeisterung.

**Die Falle, und sie ist die bekannte:** kommen Sender und Parser aus **derselben** Beschreibung,
macht eine falsche Beschreibung beide **gemeinsam falsch** — und jede Prüfzeile bleibt grün.
Wörtlich die Form von D8/D9/D11 („die Suite hat den Fehler nie ausgelöst") und von `send_no_loss`
(„der Beweis war richtig, der Code war falsch"). Das Gegenmittel gibt es hier schon:
`tools/checkfat.py` liest das Abbild in einer **anderen Sprache**, mit noch einmal
hingeschriebenem Muster. Für Rahmen heisst das: ein **Golden-Hexdump aus einem echten Mitschnitt**,
von Hand geprüft und **nicht** aus der Beschreibung erzeugt.

**Zum Format: YAML ist die teuerste der möglichen Wahlen.** `serde_yaml` ist seit März 2024
archiviert, die Nachfolge zersplittert (`serde_yaml2`, `serde_yml`, `yaml-serde`) ohne
offensichtlichen Gewinner. Dazu braucht eine Layout-Beschreibung mehr als eine flache Tabelle
(variable Längen, TLVs, bedingte Felder) — und genau dort wachsen deklarative Formate entweder zu
einer Programmiersprache heran oder reichen nicht. Zwei ehrliche Wege:
* **Kaitai Struct** — reifer DSL mit Compiler, erzeugt bereits Rust, und es gibt fertige
  `.ksy`-Dateien für Ethernet/IPv4/EDID. Preis: ein Java-Compiler im Bauweg und erzeugter Code,
  dessen Form (Allokation, `no_std`) man nicht bestimmt. **Einen Versuch wert, bevor entschieden
  wird** — geerbte Beschreibungen sind eine echte Ersparnis.
* **Eine winzige eigene Beschreibung** (TOML oder zeilenbasiert), Parser abhängigkeitsfrei in
  `tools/`. Passt zum Stil des Hauses und lässt die Form des Erzeugnisses in eigener Hand.

**Vorschlag für den ersten Schnitt — an vorhandenem Code, nicht auf der grünen Wiese:**
1. **Ethernet II + ARP** beschreiben. Beides steht heute handgeschrieben in
   `crates/caprock-virtio/src/net.rs` (`ETHERTYPE_ARP`, `ARP_REQUEST/REPLY`, 42-Byte-Rahmen von
   Hand gebaut) — es gibt also ein Vorher und ein Nachher zu vergleichen.
2. Erzeugnis als **abhängigkeitsfreie, `forbid(unsafe_code)`** Crate, **ins Repo eingecheckt**:
   der Bau läuft ohne Generator, und der Diff ist lesbar.
3. **Wächter nach dem Muster von `tools/identitaet.sh`:** das eingecheckte Erzeugnis muss
   byte-gleich zu dem sein, was der Generator jetzt erzeugt — mit Selbsttest, der belegt, dass er
   fehlschlagen kann.
4. **Unabhängiger Zeuge:** Golden-Hexdump je Rahmen. Dazu trägt der vorhandene
   ARP-Hin-und-Rückweg (`virtio-net`, A-5.2) die Verhaltensaussage — er belegt Senden *und*
   Empfangen inhaltlich.
5. Erst danach über IPv4/UDP und EDID entscheiden.

**Abnahmebedingung:** der handgeschriebene Rahmenbau in `net.rs` verschwindet, und die
ARP-Prüfzeile bleibt grün — gleiches Verhalten, eine Quelle.

**Was es NICHT löst:** Z10 (kein Netzstack). Der Generator ist die kleinere Hälfte; wer ihn für
den Stack hält, verwechselt den Kopf mit dem Protokoll.

**Nebenbefund bei der Untersuchung, eigener Punkt:** dasselbe Problem existiert bereits, aber bei
**unseren eigenen** Dienstprotokollen — s. Z13.

### Z14. Fremde Software ohne Gastschicht — bewertet 2026-08-09
**Klasse:** Produktlücke · **Aufwand:** gestuft, die erste Stufe ist klein · **Randbedingung
(Simon, 2026-08-09): Isolation und die sehr kleine TCB bleiben erhalten.**

**Die Zahl, die den Rahmen setzt:** die ausgelieferte TCB ist `.text` **212 KiB** + `.rodata`
24 KiB (Release ohne `selftest`; mit Testcode 360/52 KiB). Alles, was hier bewertet wird, darf
diese Zahl nicht erhöhen.

- [ ] **Der Ist-Stand, gemessen.** Die ABI hat **17 Syscalls**, alle cap-basiert. Für fremde
      Software fehlen vier Dinge, und drei davon sind keine Kleinigkeit:

      | Fehlt | Befund |
      |---|---|
      | `brk`/`mmap` | **kein Weg, zur Laufzeit Speicher zu bekommen.** `SYS_MAP` bildet ab, was die PD **schon hält**; ihr Speicher steht mit dem Endowment beim Laden fest |
      | Threads in einer PD | kein Erzeuger in der ABI (0 Treffer für `SPAWN`/`CLONE`) — eine PD hat **einen** Thread |
      | Uhr | keine Zeit-ABI (0 Treffer für `CLOCK`/`GETTIME`) |
      | dynamisches Linken | `PT_INTERP` wird nirgends behandelt — nur statische Binaries |

      **Was es dagegen schon gibt und was der Schlüssel ist:** Cap-Transfer über IPC (der
      `xfer`-Test delegiert eine Cap per REPLY). Eine PD kann einer anderen Autorität geben,
      **ohne dass der Kernel etwas Neues lernt**.

- [ ] **Beliebige Linux-Programme: möglich — und meine erste Bewertung war falsch** (berichtigt
      2026-08-09).

      Ich hatte geschrieben, das sei durch die TCB-Randbedingung **ausgeschlossen**, weil der
      Kernel „300+ Linux-Syscalls kennen" müsste. Das ist der falsche Entwurf, und er ist nicht
      der einzige. Der richtige steht seit zwanzig Jahren in seL4: **der Kernel kennt keinen
      einzigen Linux-Syscall — er leitet sie um.**

      **Der Mechanismus.** Eine PD trägt ein Bit „fremde Persönlichkeit" und eine Handler-Cap.
      Ist es gesetzt, wird *jeder* Syscall aus dieser PD nicht dispatcht, sondern als IPC-Nachricht
      an den Handler geschickt — Registerinhalt als Nachrichtenwörter. Der Handler ist eine
      gewöhnliche PD und emuliert dort, was Linux verspricht. Kernelkosten: ein Bit je PD, ein
      Zweig im Syscall-Einsprung, und der vorhandene IPC-Sendepfad. Das ist keine Schätzung von
      300 Syscalls, sondern von **einer Verzweigung**.

      **Was darüber hinaus im Kern fehlt — vier begrenzte Operationen:**

      | Fehlt | wofür | warum begrenzt |
      |---|---|---|
      | Syscall-/Fault-Umleitung an eine PD | jeder Linux-Syscall, jeder Seitenfehler (COW!) | ein Bit + ein Zweig; heute beendet ein Fault den Thread im Kern |
      | Thread in eine **bestehende** PD legen | `clone`, pthreads | heute hat eine PD **einen** Thread (0 Treffer für `SPAWN`/`CLONE` in der ABI) |
      | Mappen/Entmappen in eine **fremde** PD | `mmap`, `mprotect`, `ld.so`, COW | `SYS_MAP` bildet heute nur in die **eigene** VSpace ab |
      | Thread-Kontext einer fremden PD lesen/schreiben | Signale, `ptrace`, COW-Wiederaufnahme | cap-gated wie `PDCTL` |

      Zusammen grob 500–800 Zeilen, also **5–8 % TCB-Wachstum** (212 KiB → ~225 KiB). Nicht
      nichts, aber weit entfernt von „ausgeschlossen". Jede dieser vier ist außerdem für sich
      nützlich — die Fault-Umleitung z. B. wäre der erste Schritt zu einem Pager in Userspace.

- [ ] **Und trotzdem ist es die falsche Wette. Die Begründung ist empirisch, nicht ästhetisch.**

      * **„Beliebig" ist das Problemwort.** Neunzig Prozent der Programme brauchen ~50 Syscalls;
        die letzten zehn brauchen die anderen 250 **plus** `/proc`, `/sys`, netlink, epoll,
        io_uring, Namespaces, cgroups — Schnittstellen, die keine Syscalls sind und deren
        Verhalten nirgends spezifiziert ist außer im Linux-Quelltext.
      * **gVisor** implementiert rund 260 Syscalls in ~100 000 Zeilen Go und hat weiterhin Lücken;
        es ist ein Google-Projekt mit einem Jahrzehnt Arbeit.
      * **WSL1 war genau dieser Entwurf** — eine Linux-Persönlichkeit im NT-Kernel. Microsoft hat
        ihn **aufgegeben und durch eine VM ersetzt** (WSL2), weil ABI-Treue und
        Dateisystemleistung nicht einzuholen waren. Das ist das stärkste verfügbare Datum, und es
        stammt von jemandem mit mehr Mitteln als diesem Projekt.
      * **Der TCB-Vorteil wird für den Linux-Mandanten selbst zunichte.** Für das System und für
        andere Mandanten bleibt die TCB klein — für das Linux-Programm besteht sie aus Kern **plus**
        Persönlichkeitsserver. Wer „kleine TCB" als Produktversprechen führt, muss diesen Satz
        mitliefern, sonst ist er unehrlich.
      * **Leistung:** jeder Syscall wird ein IPC-Umlauf. gVisor zahlt dafür 2–10× bei
        syscall-lastigen Lasten. Der IPC-Fastpath dieses Kerns hilft, hebt es aber nicht auf.

      **Bewertung:** technisch möglich mit begrenztem Kernelwachstum, wirtschaftlich ein
      mehrjähriger Strang mit einem bekannten schlechten Ausgang. WASM liefert den größten Teil des
      Kompatibilitätsnutzens zu einem Bruchteil der Kosten und **stützt** die Produktthese, statt
      sie zu untergraben.

      **Wenn es trotzdem gemacht wird, dann in dieser Reihenfolge:** die vier Kerneloperationen
      zuerst und einzeln abgenommen (jede ist für sich nützlich), dann eine Persönlichkeit für
      **statisch gelinkte, einthreadige** Programme, dann `clone`, dann `fork`. Und die erste
      Messung ist nicht „läuft busybox", sondern: **wie viele verschiedene Syscalls ruft die
      Zielanwendung wirklich?** `strace -c -f` auf dem Zielprogramm ist eine Stunde Arbeit und
      entscheidet den ganzen Strang.

      Was **nicht** ausgeschlossen und viel billiger ist: derselbe Quelltext, **neu gelinkt**.

- [ ] **Stufe 1 (klein, und Vorbedingung für alles Weitere): ein Speicher-Server in Userspace.**
      Eine PD hält eine große Memory-Cap und gibt auf Anfrage abgeleitete Caps per IPC-REPLY
      heraus; der Client mappt sie mit `SYS_MAP`. Das ist `brk`/`mmap` — **ohne eine einzige neue
      Kernelzeile**, weil Cap-Ableitung (`CCOPY`) und Cap-Transfer beide schon stehen.

      Der Kern bleibt unberührt, die Politik („wer bekommt wie viel") wandert dorthin, wo sie
      hingehört: in eine PD und ins Manifest. Das ist derselbe Schritt wie A-5.1 beim Treiber und
      A-6.3 beim Dateisystem.

      **Abnahme:** eine PD, die beim Laden 4 KiB bekommt, fordert zur Laufzeit 1 MiB an, schreibt
      sie voll und gibt sie zurück — und eine zweite PD bekommt dieselbe Region **nicht** zu
      sehen (Positivkontrolle über denselben Server, nur eine Anfrage wandert).

- [ ] **Stufe 2, drei Wege — und sie schließen einander nicht aus.**

      | Weg | TCB-Wirkung | was läuft damit | Preis |
      |---|---|---|---|
      | **WASM in einer PD** | **null** (die Engine ist Userland) | alles, was zu WASM/WASI kompiliert — heute der Großteil dessen, wofür „Vercel-artige Dienste" steht | keine bestehenden nativen Binaries; eine `no_std`-Engine muss rein |
      | **musl gegen `libcaprock` neu gelinkt** | null | fast alles, was aus Quelltext baut | libc-Portierung: Datei-Deskriptoren, Pfade, Threads, Signale — jedes davon ein eigener Dienst |
      | **eigene Kompat-Bibliothek** | null | was man selbst dagegen baut | ehrlichste, aber kleinste Reichweite |

      **Bewertung:** WASM passt zur Produktthese und zur Randbedingung am besten — die Sandbox
      liegt **in** der PD, nicht im Kern, und der WASI-Satz ist klein und geschlossen (im
      Gegensatz zum Linux-Satz, der es per Definition nicht ist). Der Speicherbedarf ist beim
      Start bekannt (WASM-Linearspeicher), also trägt sogar das heutige Modell ohne Stufe 1 —
      allerdings ohne `memory.grow`.

      Der musl-Weg ist der mit der größten Reichweite und dem größten Preis; er wird erst
      sinnvoll, wenn Stufe 1 steht **und** ein Dateisystem-Dienst mit Pfaden existiert (heute:
      FAT16 über eine feste Datei, A-6.3).

### Z27. Binärkompatibilität ist ab jetzt eine LINIE — die Umkehr von Z16, entschieden 2026-08-10
**Klasse:** Richtungsentscheidung · **Anlass:** [Z26](#z26-gpu--der-plan-2026-08-09-gemessen-auf-dieser-maschine)/Nachtrag 1, eingelöst beim Bau von A3.

**Diese Entscheidung KEHRT EINE PROTOKOLLIERTE UM, und deshalb steht sie hier und nicht in einem
Cap-Entwurf.** Ohne diesen Eintrag stünden zwei Stränge mit widersprechenden Prämissen im
Register, und in drei Monaten wüsste niemand, welcher gilt.

**Was Z16 sagte** (2026-08-09, wörtlich): *„Was dieses Ziel STREICHT, und das ist der eigentliche
Ertrag: Binärkompatibilität fällt weg. Damit entfallen alle vier Kerneloperationen aus Z14 —
**keine Syscall-Umleitung**, kein `ld.so` für fremde Binaries, keine ABI-Treue gegenüber Linux,
kein Persönlichkeitsserver."*

**Was ab heute gilt:** die Syscall-Umleitung ist **gebaut** (Z26/A3, s. u.). Damit ist Z16 nicht
falsch und nicht abgelöst — es ist **eine von zwei Linien**:

| | Z16 — übersetzen | Z27 — fahren |
|---|---|---|
| Eingabe | Quelltext | unveränderte Linux-ELFs |
| Anforderung | POSIX-**Semantik** | Linux-**Verhalten** |
| Kernelkosten | eine Kerneloperation (Threads in einer PD) | das Umleitungsprimitiv + drei Autoritäten (s. A3) |
| Präzedenz, die trägt | Redox OS (`relibc`) | Fuchsia/Starnix |
| Präzedenz, die scheiterte | — | WSL1, gVisor |

**Der Grund für die Umkehr** — und er ist nicht „schöner", sondern **unumgänglich**: `libcuda`,
`libnvidia-glcore` und die übrigen NVIDIA-Userland-Bibliotheken gibt es **nicht als Quelltext**.
Z16 kann sie strukturell nicht erreichen, gleich wie gut es wird. Dasselbe gilt für jede
Kunden-Last, die niemand neu übersetzt, und für Go (eigener Runtime, umgeht libc grundsätzlich).
Wer „PaaS mit GPU-Compute" will, hat hier keine Wahl, sondern eine Rechnung.

**Und jetzt die Forderung, die die alte Analyse an die neue stellt — sie ist der eigentliche
Inhalt dieses Eintrags:**

> **WSL1 ist nicht an der Umleitungsmechanik gescheitert, sondern an der VERHALTENSTREUE. Eine Cap
> macht das kein Gramm leichter.**

Das Primitiv aus A3 löst das Problem „wie kommt der Syscall zur Persönlichkeit". Es löst **nichts**
von dem, woran WSL1 gestorben ist:

- [ ] **Die Fläche ist nicht die Zahl der Syscalls, sondern die Zahl der beobachtbaren
      Verhaltensweisen.** Z16 hat 48 verschiedene Syscalls über fünf Programme gemessen. Für
      Z27 ist diese Zahl **die falsche Größe**: `ioctl` ist EIN Syscall und eine unbegrenzte
      Fläche; `/proc` ist gar kein Syscall; `futex` hat Dutzende Untermodi mit
      Zeitverhalten. Bevor irgendjemand Aufwand schätzt, muss **eine Messgröße für Treue**
      festgelegt sein. „48 Syscalls, davon haben wir 30" wäre genau die Sorte Grün, die dieses
      Projekt bei einem Prüfer nicht durchgehen lässt.

- [ ] **Die Abnahme ist eine fremde Testsuite, nicht der eigene Prüfer.** Dieselbe Regel wie in
      Z16 („übersetzt ist nicht funktioniert"), nur schärfer: hier gibt es nicht einmal einen
      Compiler, der widerspricht. Ein Kandidat, der **misst statt zu behaupten**, ist die
      **LTP** (Linux Test Project) — Zahl der bestandenen Fälle je Untermenge, ins Protokoll,
      mit Datum und Stichprobengröße.

- [ ] **Wo die beiden Linien einander widersprechen, gilt der ANWENDUNGSFALL, nicht die
      Reihenfolge im Register.** Z16 bleibt der billigere Weg für alles, was als Quelltext
      vorliegt (und das ist der grössere Teil eines Server-OS). Z27 ist der einzige Weg für
      Blobs. Ein Programm, das über Z27 läuft, obwohl sein Quelltext vorliegt, zahlt die
      Umleitungskosten umsonst.

- [ ] **Diese Umkehr verschiebt Z14 auf den kritischen Pfad** (stand dort als „bewertet,
      zurückgestellt"). Der teuerste Teil des GPU-Vorhabens ist damit **nicht der Treiber**.

### Z26. GPU — der Plan, 2026-08-09 (gemessen auf DIESER Maschine)
**Klasse:** Bewertung · **nicht begonnen** · Anlass: GPU-Code schreibt man am wenigsten selbst,
und NVIDIA-Userland ist proprietär.

## Die zwei Messungen, die alles entscheiden

| gemessen | Wert |
|---|---|
| `modinfo -F license nvidia` | **`Dual MIT/GPL`** |
| `/lib/firmware/nvidia/610.43.03/gsp_ga10x.bin` | **84 MB** |
| GPU | `10de:2d18` GB206M, RTX 5070 Max-Q (Blackwell) |

**Beide drehen die Erwartung.**

**Erstens: der NVIDIA-KERNELTREIBER ist offen und MIT.** Seit 2022 sind das die
`open-gpu-kernel-modules`; auf dieser Maschine läuft genau die Variante. MIT ist mit AGPLv3
**vereinbar** — der Treiber müsste **nicht** durch die GPLv2-Zone. Das ist die günstigste
Lizenzlage aller bisher betrachteten Treiber, günstiger als NVMe (21/21 v2-only).

**Zweitens, und das ist der eigentliche Hebel: 84 MB Firmware.** Seit Turing läuft die
Treiberlogik auf dem **GSP** (einem RISC-V-Kern *auf der GPU*). Das Kernelmodul ist damit
weitgehend **Firmware-Lader + RPC-Transport**, nicht ein Monolith aus Hardwarelogik. Die
Größenordnung, die in Z20 für i915 genannt wurde (1,9 MB Modul), gilt für NVIDIA-Blackwell so
**nicht**.

## Der Bruch liegt woanders: im USERLAND

`libcuda`, `libnvidia-glcore` und die übrigen Userland-Bibliotheken sind **proprietäre Blobs**.
Sie lassen sich **nicht neu übersetzen** — und damit ist der ganze Z16-Weg („Quelltext klonen und
übersetzen") für sie versperrt. Was sie brauchen, ist **binäre Linux-ABI-Kompatibilität**: glibc-
Symbole, `ioctl` auf `/dev/nvidia*`, `mmap`-Semantik, `futex`, Signale. Das ist Z14, der Strang,
der bisher zurückgestellt war.

**Daraus folgen zwei Wege, und sie schliessen einander aus.**

### Weg A — mit Blob: CUDA, aber binäre ABI

| Stufe | Inhalt |
|---|---|
| A1 | Kernelmodul (MIT) portieren: PCI/BARs, **grosse DMA**, MSI-X, GSP-Firmware laden, RPC-Kanal |
| A2 | `/dev/nvidia*`-**ioctl-Fläche** als PD-Protokoll — die uAPI ist die stabile Seite |
| A3 | **Binäre Linux-ABI** für die Blobs: Loader für fremde ELFs, glibc-Symbolfläche, Signale, `futex`, `mmap` |
| A4 | 84 MB Firmware ausliefern und laden → Dateisystem, und die **Attestierungsfrage** (was signiert die 84 MB?) |

**A3 ist das Projekt**, nicht A1. Und es widerspricht der bisherigen Linie (Z16: übersetzen statt
Binaries fahren) — das ist eine Richtungsentscheidung, keine Erweiterung.

### Weg B — ohne Blob: alles offen und übersetzbar, aber kein CUDA

**`nouveau` (Kernel, GPL-2.0) + `NVK` (Mesa, Vulkan-Treiber, offen)** — beides Quelltext, beides
über Z16 übersetzbar, keine binäre ABI nötig. Ergebnis: **Vulkan/OpenGL, kein CUDA.**

## ENTSCHIEDEN: CUDA JA → Weg A (2026-08-09, Simon)

Damit ist die binäre Linux-ABI (A3) **kein optionaler Strang mehr, sondern Voraussetzung** — und
Z16 („Quelltext übersetzen statt Binaries fahren") ist nicht mehr die einzige Linie, sondern eine
von zweien. Die Blobs sind nicht verhandelbar: `libcuda` gibt es nicht als Quelltext, und ROCm oder
oneAPI wären ein anderer Hersteller, kein anderer Weg.

**Was das konkret verschiebt:** Z14 rückt von „bewertet, zurückgestellt" auf den kritischen Pfad,
und der teuerste Teil des GPU-Vorhabens ist **nicht** der Treiber.

## Fuchsia als Vorbild — und es sind ZWEI Bausteine

Die Frage „kann man sich an Fuchsias Schicht orientieren" trifft, und zwar doppelt. Beide Teile
sind **BSD-3-Clause**, also permissiv — anders als `dde_linux` (GPLv2) gäbe es hier **keine**
Lizenzreibung mit AGPL.

### 1. `Starnix` → das Vorbild für A3

Eine **Linux-Binärkompatibilität im Userland**: ein Zircon-Prozess, der die Linux-Syscall-Fläche
implementiert (`/proc`, `/dev`, `futex`, Signale, `mmap`) und unveränderte Linux-ELFs fährt — in
Rust geschrieben, TCB-neutral. Genau die Stellung, die eine PD hier hätte.

**Die wichtigste übertragbare Lehre ist aber nicht der Aufbau, sondern was Fuchsia dafür am KERNEL
ändern musste:** *restricted mode* (`zx_restricted_enter`) — ein Kernel-Primitiv, mit dem ein
Thread fremden User-Code ausführt und dessen Syscalls **zurück in den Userspace-Handler** fallen
statt in den Kernel. Reine Abfangerei aus dem Userland (der gVisor-ptrace-Weg) war zu teuer.

**Das ist die eine Zeile TCB, die Weg A kosten wird**, und sie sollte im Entwurf stehen, bevor
jemand anfängt — nicht als Überraschung im dritten Monat. Für Caprock hiesse das: ein Thread einer
PD führt Fremdcode aus, und sein `syscall`-Eintritt wird an eine **andere PD** zugestellt statt an
den Kernel. Das ist eine echte Erweiterung des Thread-Modells, kein Aufsatz.

### Die vierte Vorbedingung, cap-förmig — und damit VIEL kleiner als `restricted mode`

**Der Vorschlag (Simon, 2026-08-09): über Capabilities BEIDER Prozesse.** Er trägt, und er ist
besser als Fuchsias Form, weil er fast keine neue Mechanik braucht — Caprock hat die Bausteine
schon.

**Der Kern:** der Syscall-Eintritt eines Threads wird nicht *behandelt*, sondern **an einen
Endpoint zugestellt**. Der Kernel marshallt den Trap-Frame in eine IPC-Nachricht, blockiert den
Thread, weckt den Handler; dessen `REPLY` schreibt die Ergebnisregister zurück und setzt fort. Das
ist **wörtlich das Fault-Handler-Muster von seL4** — und `RECV`/`REPLY`, Trap-Frames und der
`switch_to`-Fastpath existieren hier alle bereits.

**TCB-Kosten:** ein Cap-Slot je Thread („Syscalls gehen an E") plus das Marshalling. Kein neuer
Ausführungsmodus, kein zweiter Kontext je Thread.

#### Wer hält was — und die Asymmetrie ist der Punkt

| Seite | hält | bedeutet |
|---|---|---|
| **Handler-PD** | die **Endpoint-Cap** | darf die Syscalls des Gastes empfangen **und seine Ergebnisregister schreiben** — sie kann ihn also belügen. Substanzielle Autorität, deshalb eine Cap und kein globaler Schalter |
| **Gast-PD** | **nichts** | ihre Autorität wird *verringert*: sie erreicht den Caprock-Kernel gar nicht mehr |

**Und die dritte Partei, die der Entwurf nennen muss: wer schaltet um?** Nicht der Handler — sonst
könnte jede PD die Syscalls eines fremden Threads an sich ziehen. Umschalten darf nur, wer
**Autorität über den Thread** hat. Also braucht der Aufruf **beides**:

    SYS_SETHANDLER(tcb_cap, endpoint_cap)

Genau das ist „Capabilities beider Prozesse", präzise gemacht: die Tcb-Cap sagt *wessen* Syscalls,
die Endpoint-Cap sagt *wohin*.

#### Es sind NEUE Cap-Arten, kein umgewidmeter Endpoint (Simon, 2026-08-09)

Der erste Entwurf oben nahm einen gewöhnlichen `Endpoint`. Das ist zu wenig, und zwar aus dem
Grund, den dieses Projekt schon mehrfach bezahlt hat: **ein Wert, der zwei Bedeutungen trägt, ist
so lange harmlos, wie die beiden zufällig gleich sind.**

Die Autorität ist hier eine **andere** als „darf IPC empfangen":

| | gewöhnlicher Endpoint | Syscall-Handler |
|---|---|---|
| empfängt Nachrichten | ja | ja |
| schreibt **fremde Ergebnisregister** | nein — `REPLY` schreibt die Antwort, nicht beliebigen Zustand | **ja** |
| kann den Gast **belügen** | nein | **ja** |
| ist von einem Dienst-Endpoint unterscheidbar | — | **muss es sein** |

Deshalb eigene Arten:

* **`SyscallHandler`** — „darf den Syscall-Strom der an mich gebundenen Threads empfangen und ihre
  Ergebnisregister schreiben". **Nur der an mich gebundenen**: die Bindung steht im Kernel je
  Thread, und ein `REPLY` wirkt ausschliesslich auf den Absender. Ohne diese Einschränkung wäre die
  Cap ein Generalschlüssel auf fremde Registerzustände.
* **`FaultHandler`** — getrennt, weil `mmap`-Semantik verlangt, dass der Handler **Seitenfehler**
  sieht. Ein Kanal mit zwei Bedeutungen ist die Form, die hier dreimal gerissen ist (`blocked`,
  die Park-Naht, `CR0.TS`); die vierte muss nicht sein.

**Was der eigene Typ zusätzlich möglich macht** und ein Endpoint nicht:

* **Im Manifest deklarierbar** (`CAP_SYSHANDLER`) — wer eine Linux-Persönlichkeit sein darf, steht
  dann in der **signierten** Fläche, fail-closed, statt sich aus dem Besitz eines Endpoints zu
  ergeben.
* **Im Audit benennbar** — `cdt_audit` kann die Frage „wer darf fremde Register schreiben"
  beantworten; über einen gewöhnlichen Endpoint kann sie niemand stellen.
* **Verwechslung ausgeschlossen** — eine PD kann nicht versehentlich ihren *Dienst*-Endpoint als
  Syscall-Handler binden. Der Übersetzer weist es ab, nicht ein Kommentar.

**Der Aufruf trägt damit drei Autoritäten, jede von einer anderen Seite:**

    SYS_SETHANDLER(tcb_cap, syshandler_cap, faulthandler_cap)

`tcb_cap` sagt **wessen** Syscalls (und die hält, wer den Thread kontrolliert — nicht der Handler),
die beiden anderen sagen **wohin**.

#### VIER NACHTRÄGE VOR DEM BAU (Einwand vom 2026-08-09, alle vier berechtigt)

**(1) Dieser Strang KEHRT EINE PROTOKOLLIERTE ENTSCHEIDUNG UM — und sagte es nicht.**
**EINGELÖST am 2026-08-10 als [Z27](#z27-binärkompatibilität-ist-ab-jetzt-eine-linie--die-umkehr-von-z16-entschieden-2026-08-10)**, samt der Forderung, die die alte Analyse an
die neue stellt (Verhaltenstreue, Messgröße dafür, LTP als Abnahme). Der Text unten bleibt als
Begründung stehen.
Z16 steht auf dem Satz: *„Quelltext klonen und übersetzen streicht die Binärkompatibilität — und
damit die Syscall-Abfangung, genau das, woran WSL1 und gVisor gescheitert sind."* Ein
`SyscallHandler`, der Gast-Syscalls an eine Persönlichkeits-PD umleitet, **IST** die
Syscall-Abfangung — der WSL1-Bauplan mit besserer Cap-Hygiene.
Die Umkehr ist vermutlich **richtig** (unveränderte Binaries, Go ohne libc, Kunden-Workloads, die
niemand neu übersetzt) — aber sie ist eine **neue Entscheidung gegen eine alte** und gehört mit
ihrem Grund ins Protokoll, nicht implizit in einen Cap-Entwurf. Sonst stehen hier zwei Stränge mit
widersprechenden Prämissen, und in drei Monaten weiss niemand, welcher gilt.
**Und die alte Analyse stellt eine Forderung an die neue:** WSL1 scheiterte **nicht** an der
Umleitungsmechanik, sondern an der **Verhaltenstreue** dahinter. Die wird durch eine Cap kein
Gramm leichter.

**(2) Die Autoritätsliste ist registergross geschrieben und damit falsch.** Drei Löcher,
aufsteigend:
* **Welche Register?** `rax` genügt für `read`. `rt_sigreturn` ersetzt den **kompletten**
  Trap-Frame, `clone` braucht einen **zweiten** Frame auf neuem Stack, Signalzustellung schreibt
  `rsp`/`rip`. Entweder darf `REPLY` den ganzen Frame schreiben — dann ist die Cap faktisch eine
  **Debugger-Cap** und „kann den Gast belügen: ja" untertreibt noch — oder sie darf es nicht, und
  die Persönlichkeit kann Signale und `clone` **nicht** implementieren. **Diese Grenzziehung ist
  die eigentliche Entscheidung und fehlt.**
* **Die Argumente.** Fast jeder Linux-Syscall übergibt **Zeiger**. Der Handler muss Gast-Speicher
  lesen und schreiben (`copy_from_user`-Äquivalent) — Zugriff auf den **gesamten**
  Gast-Adressraum, um Grössenordnungen breiter als ein Ergebnisregister. Woher kommt sie:
  Mapping des Gast-AS in die Handler-PD (**welche Cap trägt das?**) oder ein Kernel-Kopierdienst
  (**TCB-Kosten je Syscall**)?
* **`mmap`.** Der `FaultHandler` **sieht** Seitenfehler; sie zu **beheben** heisst, Mappings im
  **Gast-Vspace** zu installieren — eine dritte Autorität, die in `SYS_SETHANDLER` nicht vorkommt.

**Ehrlich zusammengezählt hält die Persönlichkeits-PD: Frame-Schreibrecht, vollen Speicherzugriff,
Vspace-Manipulation. Sie IST der Kernel des Gastes** und gehört in dessen TCB — das ist in Ordnung,
aber der Eintrag muss es **so nennen**, statt die Autorität klein zu schreiben. Die
Manifest-Deklarierbarkeit wird dadurch **wichtiger**: in der signierten Fläche steht dann, wer
Gast-Kernel sein darf, und das ist genau die Zeile, die ein Auditor sucht.

**(3) Die Blockiernaht ist die FÜNFTE Instanz — diesmal vorhersagbar statt gefunden.**
Ein Gast-Thread, der auf die Handler-Antwort wartet, ist blockiert, mit einem **neuen Grund**. Der
gehört **von Tag eins** in die Grund-Menge ([Z24](#z24)), sonst wiederholt sich die Park-Naht:
`thaw` weckt einen Handler-Wartenden, der `unpark` eines Geschwisters verbraucht die Marke, und der
Gast läuft **mit halbem Syscall** weiter.
Dazu drei Nähte, die heute niemand nennt:
* **Zu Z23:** eine Persönlichkeits-PD einzufrieren macht **alle ihre Gäste** unfrierbar oder
  hängend — die Partner-Nennung in der Freeze-Absage muss den Handler-Wartefall kennen.
* **Die asynchrone Hälfte der Entzugs-Regel:** stirbt die Handler-PD, muss der wartende Gast
  **sofort** faulten, nicht ewig warten. „Faulten statt zurückfallen" deckt bisher nur den
  synchronen Fall.
* **Zyklen:** A behandelt B, B behandelt A ist ein **Deadlock per Konstruktion**. Die billige
  Absage — „Threads einer PD mit `SyscallHandler`-Bindung dürfen selbst nicht gebunden werden" —
  muss **im Kernel** stehen, nicht in der Doku.
  **GEBAUT am 2026-08-10 (s. A3), und zwar NICHT in der billigen Fassung:** die verböte auch
  gestapelte Persönlichkeiten. Gebaut ist die allgemeine Azyklizität als Gang über einen
  funktionalen Graphen. Der Wartegrund steht als `BlockReasons::HANDLER` in der Grund-Menge.

**(4) Die Messmatrix, mit Schwelle VORHER.** Umlauf gegen nativen Syscall ist nur der **Sockel**;
je Syscall kommen die Speicherzugriffe des Handlers auf Gast-Puffer dazu, und die **dominieren**
bei `read`/`write` mit realen Grössen. Also zwei Lasten:
| Last | misst |
|---|---|
| `getpid` (null Argumente) | den Sockel: Umlauf, Marshalling, Rückkehr |
| `read` mit 4 KiB und 64 KiB | den Vollpfad **mit** Gast-Speicherzugriff |
**Die Schwelle, ab der die cap-förmige Fassung fällt, steht VOR der Messung fest** — sonst wird
danach verhandelt, was vorher hätte feststehen müssen.
**Und der Fuchsia-Vergleich hinkt in BEIDE Richtungen:** `switch_to` spart den Scheduler (zu
unseren Gunsten) — aber `restricted mode` spart auch den **Adressraumwechsel**, weil Gast und
Supervisor sich einen teilen, und diese Fassung wechselt **zweimal je Syscall** (zu unseren
Lasten). Das ist genau die TLB-Frage aus **Z18 (3)**, und die PCID/ASID-Zahl von dort ist die
zweite Hälfte der Rechnung.

#### A3 — DAS KERNEL-PRIMITIV IST GEBAUT (2026-08-10). Was steht, was offen ist, und die Schwelle

**Stand (2026-08-13): die NUTZLAST steht — ein Gast macht einen umgeleiteten Syscall und SIEHT
das Ergebnis.** Gemessen in QEMU als Prüfzeile `redirect : ALL PASS`: ein Gast setzt Syscall **39**
ab (`getpid` in der Linux-ABI, eine Nummer, die der Caprock-Kernel **nicht kennt** — nativ wäre das
`ERR_BADCAP`), der Kernel leitet um, ein Handler-Thread in einer **eigenen PD** liest den
Trap-Frame aus dem Sidecar und schreibt die Antwort zurück, der Gast bekommt `0xc0ffef` = sein
eigenes Argument + 1. Die Antwort ist **berechnet**, nicht konstant: eine feste Zahl könnte der
Handler auch melden, ohne den Frame gelesen zu haben. Und ein **Köder** in der Antwortnachricht
belegt, dass der Wert nicht über den IPC-Transport kam (der Gast findet in `x2` seinen eigenen Wert
wieder, nicht den Köder).

Gemessen: `redirect` **35/35** Host-Tests, `tools/redirect-negativ.sh` **11 Mutationen** + 1
Quelltext-Wächter mit Sprechprobe **ALL PASS**, `== HOST-TESTS: ALL PASS ==`,
Scheduler-Modelltreue **29 Selbsttestfälle** grün, Kerngrenze, Zulassung grün,
`tools/abnahme.sh` 13 Punkte grün (beide Architekturen gebaut, RAM-Reihe 512M · 2560M · 3G · 4G ·
6G, Lade-Suite 512M · 3G · 6G, aarch64-Suite).

**Drei Gegenproben, gefahren und protokolliert** (jeweils Mutation → Suite → zurückgenommen):

| Gegenprobe | erwartet | gemessen |
|---|---|---|
| (a) der Frame wird **nicht** ins Sidecar kopiert | der Handler sieht Müll | `handler-sah-frame=false` (alle fünf Bits 0), `x0=0x0`, **`x2=0xbadbad` — der Köder kam durch** |
| (b) das Ergebnis wird **nicht** zurückgeschrieben | der Gast sieht seinen alten Registerwert | **isoliert:** `handler-sah-frame=true` (0b11111) bleibt grün, nur `gast-sieht-ergebnis=false` (`x0=0x0`) und `register-aus-sidecar=false` fallen |
| (c) **Sprechprobe:** es wird gar nicht umgeleitet | darf nicht wie ein bestandener Lauf aussehen | `x0=0x1` = `ERR_BADCAP` (die native Antwort), `bedient=0`, `0 Zustellungen`, und **`kein-rueckfall-auf-die-native-ABI=false`** — die Rechteausweitung wird sichtbar |

Bei (a) fallen zwei Felder, und das ist **keine fehlende Isolation**: die Antwort ist aus dem
gelesenen Argument gerechnet, wer den Frame nicht sieht, kann sie nicht liefern. Ein Konjunkt mit
zwei Beobachtern — dieselbe Form wie M1 in `tools/redirect-negativ.sh`.

**Fail-closed ist mitgemessen**, an derselben Zeile: nach dem Stilllegen der Handler-PD bleibt der
Gast bei seinem nächsten Syscall **blockiert** (`handler-blockiert=true`, Rundenzähler steht), statt
auf die native ABI zurückzufallen — mit Positivkontrolle davor (`laeuft-vorher=true`).

##### Was steht

Die sieben erledigten Punkte stehen wörtlich in [done.md](done.md) (Abschnitt „Aus `todo.md`
herübergeräumt am 2026-08-11"). Hier bleibt nur, was daran offen ist — s. „WAS OFFEN IST" weiter
unten und die Nachtragstabelle in der LESEHILFE ganz oben.

##### Die Entwurfsentscheidung, die Z26 noch nicht hatte: SIDECAR statt Register-Schreib-Cap

Z26 gab dem Handler eine Cap, mit der er **fremde Ergebnisregister schreibt**. Gebaut ist etwas
anderes, und der Grund ist **gemessen**:

| | Wörter | Bytes |
|---|---|---|
| `caprock_abi::MSG_WORDS` | **4** | 32 |
| `TrapFrame` x86_64 (15 GPR + Vektor/Fehler/rip/cs/rflags/rsp/ss) | **22** | 176 |
| `TrapFrame` aarch64 (x0..x30 + ELR + SPSR + SP_EL0) | **34** | 272 |

Vier Wörter können `rt_sigreturn` (ersetzt den **ganzen** Frame) und `clone` (braucht einen
**zweiten**) strukturell nicht tragen — das ist keine Unbequemlichkeit, sondern die Grenze, die
Nachtrag 2 als „die eigentliche Entscheidung, und sie fehlt" benannt hat. Also liegt der Frame in
einem geteilten Fenster, dem **Sidecar**, ein Slot je gebundenem Gast (512 B, hergeleitet aus 272
aufgerundet auf die nächste Zweierpotenz), Slot-Vergabe als **Maske** (nicht als Zähler — ein
Zähler vergäbe nach einer Einzelfreigabe einen Slot zweimal, und dann liest ein Gast den halben
Syscall eines anderen).

**Das ist die Form von Fuchsias `zx_restricted_bind_state`, und sie halbiert die erste der drei
Autoritäten aus Nachtrag 2:** es gibt **keine** Operation „schreibe die Register des Threads T".
Es gibt ein Fenster, in dem die Frames **seiner eigenen** Gäste liegen. Der Unterschied ist
auditierbar (eine Region in der Speicherbuchhaltung statt einer Fähigkeit im Cap-Audit) und die
Einschränkung „nur an mich gebundene Threads", die Z26 als Kernelprüfung fordert, ist damit
**strukturell** statt geprüft.

**Was die Sidecar-Form nicht kann, und es ist genau eine Sache:** sie braucht einen Slot je
gebundenem **Gast-Thread**, nicht je Handler-Thread. Bei Fuchsia gehört das Sidecar dem Thread,
der `restricted mode` betritt — dort ist nichts umzuhängen. Hier hängen mehrere Gäste an einer
Handler-PD; ein Fenster für alle wäre ein Rennen zwischen zwei gleichzeitigen Gast-Syscalls. Preis:
Speicher und eine Schranke (64 Slots je Handler-PD, `ERR_NOSPACE`), nicht Umhängen.

##### Die Autoritätsrechnung, ehrlich: das Primitiv vergibt EINEINHALB von vier

| Autorität (Nachtrag 2) | hier | Folge |
|---|---|---|
| Frame lesen **und schreiben** | **ja**, ganzer Frame über das Sidecar | `rt_sigreturn`, `clone`, Signalzustellung sind damit **möglich** |
| Gast-**Speicher** (Zeigerargumente) | **nein** | **jeder Syscall mit Zeiger ist nicht implementierbar** — `read`, `write`, `openat`, `ioctl`. Für CUDA heisst das: die ganze `ioctl`-Fläche fehlt |
| **Vspace** des Gastes manipulieren | **nein** | **`mmap`/`mprotect`/Demand-Paging sind nicht implementierbar** |
| Faults **sehen** | **ja**, eigene Cap | ein Fault-Handler *sieht*; **beheben** kann er nicht (das ist Zeile 3) |

Die beiden Neins sind **kein Versehen**: es sind eigene Caps mit eigenen Entwurfsfragen (welche
Cap trägt „der ganze Adressraum von G"? ein Kernel-Kopierdienst kostet TCB je Syscall). Sie
mitzunehmen hiesse, drei Entscheidungen in einer Zeile zu treffen.

##### DIE SCHWELLE — festgelegt VOR jeder Messung (Nachtrag 4)

**Der Adressraumwechsel ist die offene Größe, und sie ist eine Entwurfsentscheidung, keine
Nachlässigkeit.** Bei Starnix teilen sich Gast und `starnix_kernel` **einen** Adressraum (untere
Hälfte Gast, obere Hälfte Supervisor); ein Rundlauf kostet dort einen **Moduswechsel**, keinen
Adressraumwechsel. Diese Fassung setzt den Handler in eine **eigene PD** und wechselt **zweimal je
umgeleitetem Syscall**.

Die Starnix-Form wäre hier **nicht billig**: Caprocks isolierte PDs teilen sich die statischen
oberen Tabellen (`ISO_PD_HIGH`, GiB 1..3) — einen Handler dort einzublenden gäbe ihn **jeder**
isolierten PD (die Falle „Geteilte Seitenverzeichnisse vertragen keine PD-spezifischen Einträge").
Sie verlangte private obere Tabellen je Gast und setzte den Kernel des Gastes **in dessen
Adressraum**. Deshalb: eigene PD, zwei Wechsel, Preis benannt.

**Die Schwelle, ab der die cap-förmige Fassung fällt und `restricted mode` gebaut werden muss:**

| Last | Schwelle | verankert an |
|---|---|---|
| umgeleiteter **`getpid`** (Sockel: kein Gast-Speicherzugriff) | **≤ 2000 Zyklen** Median | Z18 (2): ein Linux-Syscall liegt mit Mitigations bei 100–200; „unter 1000" hiess dort, die Dienst-Architektur trägt. Ein Redirect **ist** ein solcher Umlauf plus Marshalling — das Doppelte ist die grosszügige Fassung derselben Zahl |
| **`read` 4 KiB / 64 KiB** | noch **nicht** festlegbar | er braucht die Gast-Speicher-Autorität, und die ist nicht gebaut. Eine Schwelle für einen Pfad, den es nicht gibt, wäre eine Zahl ohne Gegenstand |

Über 2000 Zyklen ist **nicht** „dann optimieren wir": dann ist der Entwurf gefallen, und die
Antwort heisst geteilter Adressraum (private obere Tabellen je Gast) oder PCID (Z18 (3)).
**Die Zahl steht hier, bevor gemessen wurde** — sonst wird danach verhandelt, was vorher hätte
feststehen müssen.

##### WAS OFFEN IST — und das ist mehr, als oben steht

- [ ] **NEU (2026-08-13): die Messung fährt keinen ISOLIERTEN Handler und keinen EL0-Gast.**
      Beide sind Kernel-Threads in eigenen PDs; der Trap-Pfad ist derselbe (`int 0x80` legt
      denselben Frame an, `iretq` stellt ihn her) und die zurückgeschriebenen Wörter sind
      dieselben — aber „ein EL0-Gast wird umgeleitet" und „das Fenster liegt in der VSpace einer
      isolierten Handler-PD" sind damit **erschlossen, nicht gemessen**. Ein erschlossener Fall
      war in diesem Projekt schon dreimal ein ungeprüfter. Für den isolierten Handler ist der Weg
      klar (`spawn_isolated_parked` + `SYS_MAP` auf der Fenster-Cap); er verlangt Handler- und
      Gastcode in `.user_text` und ist deshalb eine eigene Runde.

- [ ] **NEU (2026-08-13): der Rückschrieb umfasst nur die ALLZWECKREGISTER.** `rip`/`rsp`
      (x86_64) bzw. `elr`/`sp_el0` (aarch64) kommen **nicht** zurück — und damit ist
      `rt_sigreturn` weiterhin **nicht** implementierbar, obwohl die Autoritätstabelle oben das
      Gegenteil sagt („Frame lesen **und schreiben** — ja, ganzer Frame → `rt_sigreturn`, `clone`
      sind damit möglich"). Der Grund ist kein Versehen: **Lesen und Schreiben sind nicht dieselbe
      Autorität.** Der Frame enthält `cs`/`ss` bzw. `spsr`, also den **Ring** — ein Handler, der
      sie zurückschreiben dürfte, beförderte seinen Gast, und das wäre die Rechteausweitung, gegen
      die die Weiche steht, nur von der anderen Seite. `rip`/`rsp` sind weniger gefährlich, aber
      jedes von ihnen braucht eine eigene Gültigkeitsprüfung (Kanonizität — ein nicht-kanonischer
      `rip` schlägt beim `iretq` **im Kernel** auf, Ausrichtung). Die Zeile in der Tabelle ist
      seit dem 2026-08-13 in **zwei** geteilt.

- [ ] **`handler_lebt` liest die falsche Größe — benannt, nicht versteckt.** Geprüft wird, ob die
      Handler-**PD** existiert und nicht stillgelegt ist. **Nicht** geprüft: ob die Handler-*Cap*
      noch im Cspace liegt. Eine gelöschte Cap bei lebender PD führt heute dazu, dass der Handler
      nicht mehr `RECV`t — der Gast wartet dann in `BlockReasons::HANDLER`, **statt zu faulten**.
      Damit deckt die Fail-closed-Regel den Fall „Cap entzogen" nur zur Hälfte. Ein Prüfer, der
      die falsche Größe liest, kann den Fehler strukturell nicht sehen; deshalb steht es hier und
      im Quelltext an der Funktion.

- [ ] **Die asynchrone Hälfte der Entzugs-Regel fehlt** (Nachtrag 3): stirbt die Handler-PD,
      während ein Gast wartet, wird der Gast heute **nicht** geweckt. Er hängt. Es braucht einen
      Pfad im PD-Teardown, der alle Gäste dieser Handler-PD mit `ERR_HANDLER_GONE` freigibt —
      und die Gästeliste gibt es noch nicht (nur die Kante `gast -> handler`, nicht ihre
      Umkehrung).

- [ ] **Zu Z23 (Freeze): die Partner-Nennung kennt den Handler-Wartefall nicht.** Eine
      Persönlichkeits-PD einzufrieren macht alle ihre Gäste hängend. `handler_lebt` gibt
      `quiescing`-PDs bereits als tot aus (der Gast faultet also statt zu hängen) — aber die
      **Freeze-Absage** nennt den Handler-Wartefall nicht, und das gehört zusammen entschieden.

- [ ] **NEU (2026-08-13), und es ist der unangenehmste Befund dieser Runde: die ganze
      Sidecar-Arithmetik war TOTER CODE.** `slot_offset`, `slot_gueltig`, `slots_in`,
      `fenster_deckt` und `FRAME_MAX_BYTES` hatten im **ganzen Baum** keinen Aufrufer ausserhalb
      ihres eigenen Testmoduls — gemessen mit `grep`, nicht vermutet. Die Mutationen M5 und M6 in
      `tools/redirect-negativ.sh` belegen seit dem 2026-08-10, dass die **Funktionen** richtig
      sind; dass sie **gerufen** werden, belegte nichts. Das ist die Form „eine grüne Zeile, die
      nichts gattert", eine Ebene tiefer: ein Negativtest, der eine Eigenschaft absichert, die
      niemand benutzt.
      Seit der Nutzlast sind alle fünf tragend (`slot_gueltig` in `sidecarkopie::slot_adresse`,
      `fenster_deckt` beim Prägen der Cap **und** an der Bindung, `slot_offset` im Zusteller).
      **Und dabei fiel eine echte Lücke auf:** `SIDECAR_SLOTS = 64` und das `len` der Cap waren
      **zwei unabhängige Zahlen**. Eine Handler-Cap mit einem 4-KiB-Fenster hätte 64 Slots
      vergeben und der Kernel bis Offset 32 KiB geschrieben — über die Region hinaus, in fremden
      Speicher. Jetzt weist `install_*_handler_cap` das ab (`CapError::ZuKlein`) und
      `SYS_SETHANDLER` noch einmal (`ERR_NOSPACE`).

- [ ] **Nicht gemessen: die Kosten — und seit dem 2026-08-13 ist das keine Frage der Bausteine
      mehr, sondern der Maschine.** Kein Zyklenwert, weder für die Weiche noch für den Umlauf.
      Der Umlauf war bis heute **nicht messbar**, weil die Nutzlast fehlte; jetzt läuft er, und die
      Schwelle (umgeleiteter `getpid` ≤ 2000 Zyklen Median) ist einlösbar. **Gemessen wird sie auf
      einer RUHIGEN Maschine**: dieser Messstand ist mit 3,2-facher vCPU-Überbuchung schon einmal
      in die Irre geführt worden (D13), und eine Zahl von dort stünde danach im Register. Die
      Weiche kostet auf dem unbelasteten Pfad ein
      `Option`-Lesen am laufenden TCB je Syscall (eine Sperrung, ein Vergleich, kein Cap-Lookup) —
      **auch das ist eine Behauptung über den Code, keine Messung.** Sie gehört an
      [Z18](#z18-hohe-leistung-für-übersetzten-fremdcode--vermessen-2026-08-09) (2), das ohnehin
      als „die eigentliche Lücke" offen ist.

#### Die Regel, ohne die es eine Rechteausweitung wäre

**Fällt die Handler-Cap weg (gelöscht, entzogen, Handler tot), muss der Gast FAULTEN — nicht auf
die native ABI zurückfallen.** Ein Rückfall machte aus dem Entzug einer Cap eine **Beförderung**:
der Gast spräche plötzlich direkt mit dem Kernel. Fail-closed, und es ist dieselbe Form wie
`ERR_SERVER_GONE` beim Endpoint-Austausch.

#### Was noch zu entscheiden ist

* **Faults getrennt oder mitgeführt?** `mmap`-Semantik verlangt, dass der Handler auch
  **Seitenfehler** sieht. seL4 trennt Fault-Endpoint von Syscall-Zustellung; hier wäre eine zweite
  Cap sauberer als ein überladener Kanal — ein Kanal mit zwei Bedeutungen ist die Form, die dieses
  Projekt schon dreimal bezahlt hat.
* **Die Messung, die (a) gegen (b) entscheidet:** Kosten eines IPC-Umlaufs gegen einen nativen
  Syscall auf dieser Maschine. Fuchsia ging zu `restricted mode` **wegen der Kosten**; hier ist der
  Vergleich aber ein anderer, weil `switch_to` auf demselben Kern direkt umschaltet (mit
  Budget-Spende) statt über den Scheduler zu laufen. **Vor dem Bau messen, nicht danach** — und
  die Zahl gehört neben die von gVisors ptrace-Weg, damit klar ist, welcher Vergleich gilt.

### 2. `Magma` → das Vorbild für A1/A2

Fuchsias **GPU-Architektur**: geteilt in einen *system driver* (im Treiberprozess) und einen
*application driver* (Herstellercode im Anwendungsprozess, z. B. Mesa), die über Kanäle reden.
Also exakt der Schnitt, den Caprock ohnehin braucht — Kommandopuffer werden in der Anwendung
gebaut und über IPC eingereicht, statt über `ioctl` auf ein Gerät.

**Der Nutzen liegt im Entwurf, nicht im Code.** Zircons Objektmodell (Handles, VMOs, Kanäle) ist
nicht das von Caprock (Caps, PDs, Endpoints); es geht um die **Schnittführung**, und die ist
übertragbar. Fuchsia zu portieren wäre keine Abkürzung, sondern ein zweites Betriebssystem.

**Vor der Übernahme zu prüfen** (aus der Quelle, nicht aus dem Gedächtnis): der genaue Umfang von
`zx_restricted_enter`, wie Starnix `mmap`/`fork` auf VMOs abbildet, und ob Magmas Schnitt die
CUDA-Fläche überhaupt trägt — Magma ist auf Grafik zugeschnitten, und `libcuda` spricht mit
`/dev/nvidia*` und `/dev/nvidia-uvm`, nicht mit einer Grafik-uAPI. **Das ist der Punkt, an dem das
Vorbild enden könnte.**

## Die Frage, die der Entscheidung vorausging

* **PaaS mit GPU-Compute** (Inferenz, Training) → **CUDA ist der Markt**, also Weg A, also binäre
  ABI. Es gibt keine Abkürzung; ROCm/oneAPI wären ein anderer Hersteller, nicht ein anderer Weg.
* **Nur Darstellung/Compositing** → Weg B, und dann ist auch **i915 der billigere Einstieg**
  (728 der 900 Dateien MIT, gemessen).
* **Server ohne Grafik** → gar keine GPU, und der ganze Strang entfällt.

## Was in JEDEM Fall zuerst kommt

Unabhängig vom Weg: **`CAP_IRQ` auf x86** (IRTE-Vergabe), **grosse/zusammenhängende DMA** und
**Firmware-Laden aus einem Dateisystem**. Ohne diese drei ist keine Variante lauffähig — und alle
drei stehen ohnehin auf dem Weg zum Server.

**Weiterhin nicht der nächste Schritt** — aber die Entscheidung ist gefallen, und sie kostet den
Unterschied zwischen „übersetzen" und „fremde Binaries fahren". Die drei Vorbedingungen (`CAP_IRQ`
auf x86, grosse DMA, Firmware aus einem Dateisystem) stehen ohnehin auf dem Weg zum Server; die
vierte — **das Kernel-Primitiv für umgeleitete Syscalls** — steht auf keinem anderen Weg und ist
damit der ehrliche Preis dieser Entscheidung.

### Z23. Prozess-Freeze — geplant 2026-08-09, NICHT begonnen
**Klasse:** Nebenstrang · **Vorbedingung:** mehrere Threads je PD (Z22 P2, offen) — ohne die ist
jeder Nachweis hier nur ein zweites Z4a

**Der Ist-Stand, gemessen:** `freeze_thread` ist die **einzige** Freeze-Funktion im Baum. Es gibt
keinen Prozess-Freeze, und die naheliegende Fassung („alle Threads der Reihe nach") ist nicht bloss
unimplementiert, sondern **strukturell unmöglich**: treiben zwei Threads einer PD miteinander IPC,
gibt `freeze_thread` für **beide** `Busy`, und es existiert keine Reihenfolge, die das auflöst.
Die Bausteine sind da (`endpoint_quiesce`/`ERR_QUIESCING`, `thread_quiescence`, `Freeze`, `Parked`,
`checkpoint::Scope`/`classify`); der Ablauf darüber ist nie gebaut worden.

- [ ] **S1 (ursprünglicher Plan, zum Nachlesen).** Erst die **Tore schliessen**, dann einfrieren.
      Ein `PD_QUIESCING`-Bit je PD, geprüft im Syscall-Pfad: `CALL`/`RECV` **aus** der PD heraus
      scheitern mit `ERR_QUIESCING`, laufende Transaktionen dürfen **abschliessen** (`REPLY` bleibt
      erlaubt) — dieselbe Torlogik wie A-4.2, nur mit der PD als Umfang statt einem Endpoint.
      **Warum nicht einfach alle Endpoints der PD stilllegen:** an einem Endpoint hängen auch
      **fremde** PDs. Ein Endpoint-weiter Riegel fröre Dritte mit ein — die Stilllegung muss am
      **Subjekt** hängen, nicht am Objekt.
      **TCB-Kosten, benannt:** ein Bit je PD und eine Prüfung in `CALL`/`RECV`. Mehr nicht.

- [ ] **S1b — was ein FREMDER Aufrufer erlebt, ist offen — und das exportiert den Deadlock.**
      Das Subjekt-Gating stoppt, was die PD selbst **anfängt**. Ihre Endpoints existieren aber
      weiter: eine dritte PD, die während des Fensters oder nach dem Einfrieren **hineinruft**,
      bekommt ohne definierte Antwort **unbegrenztes Blockieren** — der Freeze reicht genau den
      Deadlock an Unbeteiligte weiter, den die Partner-Nennung auf der eigenen Seite vermeidet.
      Drei ehrliche Optionen, und keine ist gratis:
      * **`ERR_QUIESCING` auch an Aufrufer** — macht den Freeze für Clients sichtbar. Ehrlich,
        aber **jeder** Client braucht Retry-Logik.
      * **Begrenztes Anstellen** mit benannter Schranke — dann ist zu sagen, **wer den Pufferplatz
        bezahlt** (und der Überlauf ist wieder ein D11-Fall).
      * **Blockieren als dokumentierter Vertrag** („ein Call in eine eingefrorene PD wartet bis zum
        Thaw"). Für eine PaaS mit Migration vertretbar — aber dann gehört die Aussage **in die
        Zusicherung des Endpoints**, nicht ins Kleingedruckte.

- [ ] **S2 — die Absage muss den PARTNER nennen.** Ein Thread, der in einem `CALL` an einen
      **fremden** Server hängt, der nie antwortet, macht die PD unfrierbar. Das ist kein
      vorübergehender Zustand und darf kein Hänger sein: `Freeze::BusyOn { tid, partner }` statt
      `Busy(Quiescence)`.
      **An WEN der Name geht, ist Teil der Spezifikation:** an den **Halter der Freeze-Autorität**,
      nicht an die eingefrorene PD. Sonst wird die Absage zum **Orakel**, mit dem eine PD die
      IPC-Topologie fremder PDs ausforschen kann — eine Fehlermeldung mit Namen ist ein Kanal.
      Dazu eine **Frist** mit benanntem Ausgang. „Wir warten, bis es ruhig ist" terminiert nicht
      beweisbar, und eine Stilllegung ohne Frist ist von einem Deadlock nicht zu unterscheiden —
      genau die Ununterscheidbarkeit, die `docs/fehlerdomaene.md` schon einmal gekostet hat.

- [ ] **S3 — der Freeze ist eine MENGE, kein Ablauf.** Entweder steht die ganze PD oder keiner
      ihrer Threads. Ein Teilerfolg, der liegen bleibt, ist schlimmer als ein Fehlschlag: die PD
      wäre halb tot und **jeder Prüfer meldete Ordnung** (D11-Form).
      Also ein Zeuge `FrozenPd` nach dem Vorbild von [`Parked`](#z22): `#[must_use]`, **kein**
      öffentlicher Weg an die ThreadIds, kein `Drop` (sonst liesse sich der Inhalt beim Auftauen
      nicht herausbewegen) — und ein ausdrückliches `abort_freeze`, das die schon eingefrorenen
      wieder auftaut. Bewacht wie `tools/zulassung.sh`, mit Selbsttest in beide Richtungen.
      **`abort_freeze` ist der am wenigsten geübte und gefährlichste Pfad und braucht seine EIGENE
      Gegenprobe:** Teilerfolg → Abbruch → alle Threads wieder lauffähig, **keine Weckmarke
      verloren, kein Grund-Bit hängengeblieben**. Das ist schwerer als der Erfolgsfall, weil es
      jeden Zwischenzustand rückwärts durchläuft — und mit der Grund-Menge aus **Z24** fast
      geschenkt (den Freeze-Grund aus der Menge entfernen, fertig). **Noch ein Grund, Z24 VOR Z23
      zu ziehen.**

- [ ] **Was das NACH AUSSEN heisst, und es gehört in beide Protokolle.** „Der Checkpoint trägt
      keinen Thread" bedeutet: **Resume-Latenz und Live-Migration haben derzeit kein messbares
      Objekt.** Was existiert, ist ein **Anwendungs**-Checkpoint (Fortschrittszähler +
      Cap-Klassifikation); eine Latenzzahl darauf misst etwas anderes, als „Resume" in einer
      PaaS-Zusage bedeutet.
      **Der Zwei-Phasen-Gruppenschnitt muss als Entwurf spezifiziert sein, BEVOR Benchmarks nach
      aussen benannt werden** — sonst entsteht die Zahl zuerst und definiert rückwirkend, was sie
      gemessen haben soll. Der Satz, der dafür fehlt, ist die Reihenfolge **mit ihren
      Fehlerfällen**: was passiert mit einer Transaktion, die nicht ausläuft — Frist, Abbruch, oder
      Vererbung an den Checkpoint? Drei verschiedene Zusagen, und keine davon ist getroffen.
      (Dieselbe Ehrlichkeit wie „*ein Thread überlebt eine Bootgrenze* liest sich stärker, als die
      Sache ist". **Gehört auch ins Velve-Protokoll**, nicht nur hierher.)

- [ ] **S4 — den Zustand aufzählen, der mitwandern MUSS.** Heute wandert **nichts**: `Image` trägt
      `progress`, `nonce`, `epoch`, `caps` — eine Anwendungsgrösse und die Cap-Klassifikation. Kein
      Trap-Frame, keine Register, kein Stack, kein Speicherinhalt.
      Aufzuzählen ist mindestens: je Thread Registerzustand, Stackinhalt, `priority`/`budget`/
      `period`/`remaining`, der **Blockadegrund**, und — als Schuld aus Z22 P4 — **`parked` und
      `park_wake`**. Je PD: Adressraum (welche Seiten, welche Farben) und Cspace. Dazu die
      schwebende IPC-Lage: Reply-Token und die `pending`-Badges der Notifications.
      **Ausdrücklich dazu: der FP-Zustand.** Er liegt nach dem Eager-Umbau im `FP_STATES`-Slot,
      **nicht** im Trap-Frame. Wird er nicht genannt, wandert ein Thread **ohne seine XMM** und
      rechnet nach dem Thaw mit fremden oder genullten Registern weiter — der stille
      Registerverlust, nur über die Bootgrenze.
      **Und die Entscheidung, die JETZT zu treffen ist, nicht später implizit im Migrationscode:
      ein Bild mit Threadzustand trägt GEHEIMNISSE.** Trap-Frame + Stack + Speicherinhalt heisst
      Schlüsselmaterial im Bild — nach dem Eager-Umbau ausdrücklich **auch die XMM-Register**, also
      genau das Material, dessentwegen eager beschlossen wurde. Ein `Image` mit `progress` und
      Cap-Klassen war ein **Metadatum**; eines mit Registern und Speicher ist ein **Datenträger**.
      Wer es lesen darf, wo es liegt, ob es ruhend verschlüsselt ist — bei Migration **verlässt das
      Bild die Maschine**, das ist dieselbe Sorte Entscheidung wie `SVT`/`SID` bei der IRTE:
      Autorität, vorab zu spezifizieren.
      **Regel, ab sofort im Plan:** *Bild enthält Registerzustand ⇒ vertraulich; Ablage- und
      Transportregel steht, bevor S4 gebaut wird.*
      **Die Regel für den Rest steht schon:** was nicht übertragbar ist, wird **benannt abgewiesen** —
      `classify` tut das für Caps. S4 heisst, `Scope`/`classify` von Caps auf **Threadzustand**
      auszudehnen, nicht ein neues Verfahren zu erfinden.

- [ ] **S5 — Gerät und DMA: hier gibt es heute NULL Zeilen.** Eine eingefrorene Treiber-PD, deren
      Gerät gerade in ihre Region schreibt, ist **nicht** eingefroren — der Deskriptorring läuft
      weiter, und mit MSI (Z22 P1) wird das mehr, nicht weniger.
      Drei Wege, und der erste ist die richtige erste Fassung:
      * **(a) fail-closed:** eine PD mit DMA-Cap wird **nicht** eingefroren. Billig, ehrlich,
        sofort. Eine Absage, die stimmt, ist besser als eine Zusage, die nicht hält.
      * **(b) das Gerät stilllegen** — treiberspezifisch, gehört also **in die PD**: ein
        `PD_FREEZE_PREPARE`-Upcall, den der Treiber beantwortet („DMA steht"). Das ist ein
        **Protokoll**, kein Kernelmechanismus, und damit TCB-neutral. Der richtige Endzustand.
      * **(c) den IOMMU-Kontext abhängen** — generisch und kernelseitig, zerstört aber laufende
        Anfragen. Nur als Notbremse.
      **Berichtigt (Einwand vom 2026-08-09): der stärkere Mechanismus existiert schon — die IOMMU
      selbst.** Ein Kanarienwort fängt nur das Gerät, das zufällig **dieses Wort** beschreibt; DMA
      in alle übrigen Seiten bleibt unsichtbar, und die falsche Beruhigung überwiegt. Der Freeze
      dreht stattdessen die **Domäne des Geräts auf non-present**: jede DMA während des
      Eingefroren-Seins wird ein **IOMMU-Fault**, und der Fault-Weg ist bereits gebaut — laut,
      gemessen, **flächendeckend statt wortgross**.
      Das Kanarienwort bleibt trotzdem: als **Tripwire im Selbsttest**, der den **Prüfer** prüft.
      Aber die Zusicherung „kein DMA während Freeze" gehört an die Hardware-Grenze, die sie
      vollständig durchsetzen kann.
      **Damit wird auch die erste Absage präziser:** nicht „eine PD mit DMA-Cap wird nicht
      eingefroren", sondern „wird nicht eingefroren, **solange der Domänen-Schwenk nicht gebaut
      ist**" — der Weg vom fail-closed zum Endzustand läuft über eine Zeile, die es schon gibt.

- [ ] **S6 — Auftauen ist nicht die Umkehrung.** Tore öffnen, wiederherstellen, fortsetzen — und
      zwei Dinge, die heute schon falsch sind (s. den Befund unten): `thaw` muss den Park-Zustand
      kennen, und ein Thread, der **mit** gesetzter Weckmarke eingefroren wurde, muss nach dem
      Auftauen **sofort** weiterlaufen und darf nicht schlafen.

- [ ] **Benannte Auslassung: die ZEIT.** Ein aufgetauter Thread sieht die Uhr **springen**; jede
      Frist, die er vor dem Freeze berechnet hat, ist danach Unsinn. Ob die Antwort „die monotone
      Uhr pausiert mit" oder „Fristen werden beim Thaw neu gestellt" lautet, ist später
      entscheidbar — **benannt** muss sie jetzt sein, sonst findet der erste Timeout-Test sie als
      Heisenbug.

- [ ] **Abnahme — und sie muss fehlschlagen können.** Vier Aussagen, von denen nur die dritte neu
      ist; die ersten beiden sind die Z4a-Form (Positivkontrolle, Beobachtungsfenster **länger als
      ein Tick** — die erste Z4a-Fassung mass mit einem kürzeren und meldete Stillstand für einen
      Thread, der völlig in Ordnung war):
      1. Die PD läuft nachweislich **vorher** (ein Zähler bewegt sich).
      2. Eingefroren steht er über ein Fenster von mehreren Ticks, und **nach** dem Auftauen läuft
         er wieder.
      3. **Der Fall, den die Reihenfolge nicht kann:** zwei Threads derselben PD, die **miteinander
         IPC treiben**, werden gemeinsam eingefroren. Ohne diesen Fall beweist der ganze Strang
         nichts, was Z4a nicht schon zeigt.
      4. Eine PD, die auf einen **fremden** Server wartet, bekommt eine Absage, die den **Partner
         nennt** — kein Hänger, kein `false`.
      5. **Eine DRITTE PD ruft während des Fensters hinein** — und das **definierte** Verhalten
         wird **gemessen**, nicht angenommen (s. S1b). Ohne diesen Fall ist die Zusage über
         Unbeteiligte unbelegt.
      Dazu die Gegenprobe: eine Mutation, die die Tore **nicht** schliesst, muss Fall 3 reissen.

- [ ] **Vorher zu beheben, weil Z23 sonst auf einem kaputten Fundament plant** (Befund vom
      2026-08-09, aus dem Code gelesen): `parked`/`park_wake` werden **ausserhalb des Schedulers
      von nichts gelesen** — nicht von `thread_quiescence`, nicht vom Checkpoint, **und nicht von
      `thaw_thread`/`PDCTL RESUME`**. Daraus folgt heute schon:
      * `thaw_thread` ruft `unblock`, und `unblock` fasst `parked` nicht an → ein aufgetauter
        geparkter Thread läuft mit `parked == true` weiter, und `is_parked` lügt ab da.
      * Der nächste `unpark` sieht dieses veraltete `parked == true`, **verbraucht die Marke
        sofort** und ruft `unblock` auf einen laufenden Thread. Die Weckmarke ist damit weg, ohne
        dass jemand geschlafen hat — **ein verlorenes Wecken**, exakt das, wogegen Z22 P4 gebaut
        ist.
      * Umgekehrt hebt der `unpark` eines Geschwisterthreads ein `PDCTL PAUSE` auf einen geparkten
        Thread **stillschweigend** auf.
      Wieder „ein Bit, zwei Gründe" (D9), diesmal an der Naht zwischen Park und Pause/Resume: in
      `unpark` vermieden, an der anderen Seite stehengelassen. **Und es verschiebt den Status des
      Spurious-Wake-Vertrags:** das System erzeugt schon heute spurious wakeups, der Vertrag ist
      also keine Vorsorge, sondern die einzige Fassung, die stimmt.

### Z22. Die vier harten Stellen aus Z21 — gebaut
**Klasse:** Substrat · **Stand:** 2026-08-09 · Leitlinie: *so viel wie möglich in der PD, TCB so
klein wie möglich, Leistung maximal.*

Die Aufteilung, die daraus folgt:

| Punkt | im Kernel (TCB) | in der PD |
|---|---|---|
| 1 IRQ | IRTE-Vergabe mit SID-Prüfung, Vektor→Notification | Maskieren am Gerät (eigene MSI-X-Tabelle), Handler |
| 2 threaded IRQ | **nichts Neues** | Workqueue, Ausschluss zwischen eigenen Threads |
| 3 DMA | Region **einmal** mappen (steht schon) | IOVA-Allokator über dem Fenster, `dma_map` ohne Syscall |
| 4 Schlafen | ein Syscall + zwei Bits je TCB | Warteschlangen, Completions, `wait_event` |

- [~] **P1 — x86 MSI-X + IRTE: Kodierung UND Vergabe stehen, verdrahtet ist nichts**
      (2026-08-09 Kodierung, Vergabe gemergt 2026-08-10).
      **Neu seit dem Merge:** `vergib`/`ruecknahme` mit `SVT=01`+`SID`, dazu sieben Mutationen in
      `tools/irte-vergabe-negativ.sh`, jede mit dem NAMEN des Tests, der fallen muss.
      **Was weiterhin fehlt, und es ist der ganze Punkt:** kein Kernelpfad ruft sie auf. Die IRT
      steht seit B-3.2 auf lauter „not present", jeder Treiber pollt. Ein Modul, das nur von
      seinen eigenen Tests aufgerufen wird, ist gebaut und nicht in Betrieb — und diese
      Unterscheidung gehört in den Eintrag, sonst liest sich `irte : ALL PASS` wie eine
      Betriebsaussage.
      **Fertig:** `crates/caprock-hal/src/x86_64/irte.rs` — IRTE- und MSI-Adress-Kodierung als
      **reine Funktion**, 10 Host-Tests, als Ziel `irte` eingehängt. Nach dem Vorbild von `dmar.rs`
      eine eigene Datei, und aus demselben Grund: ein Bit an der falschen Stelle äussert sich als
      *„das Gerät unterbricht einfach nicht"* — ohne Fault, ohne Meldung, ohne irgendetwas, das
      nach einem Fehler aussieht. Mit Literalen in Sekunden prüfbar; in QEMU bräuchte es Gerät,
      Treiber und Glück.
      Die Sicherheitsaussage ist `SVT=01`/`SID`: die PD schreibt ihre MSI-X-Tabelle **selbst**
      (sie besitzt das Fenster, das spart einen Syscall je Vektor) — und könnte damit den Handle
      einer **fremden** IRTE eintragen. Die Quellprüfung der Einheit weist das ab. Ohne sie wäre
      das ein Loch, das im Normalbetrieb nie auffällt.
      Drei Absagen statt stiller Kürzung: Vektor < 32 (ein Gerät auf Vektor 14 sähe wie ein
      Seitenfehler aus), APIC-ID > 255 ohne `EIME` (abgeschnitten zeigte sie auf einen **anderen**
      Kern — ein Interrupt, der still am falschen Ort ankommt, sieht aus wie Erfolg), und eine
      Gerätenummer > 31 (liefe in die Busnummer hinein und ordnete den Eintrag einem **fremden**
      Gerät zu).
      **Offen:** die Vergabe (Index-Allokator + Schreiben in die Tabelle + `IEC`-Invalidierung),
      der Weg vom Manifest zur IRTE, ein `SYS_MSI`-artiger Zugang oder eine Vergabe im Lader,
      und die Zustellung bis in eine Treiber-PD. Dazu die drei x86-No-Ops in `intc`.

- [~] **P2 (Stand vom 2026-08-09, überholt) — die Userland-Hälfte steht, die Threads fehlen.**
      **Fertig:** `crates/caprock-wait` — Mutex, `WaitQueue`, `Completion` über einem Trait mit
      **zwei** Methoden (`park`/`unpark` aus P4). 11 Host-Tests gegen einen Stellvertreter. Der
      unbestrittene Weg ist **null Syscalls**; der volle Warteraum ist **benannt** und der Aufrufer
      parkt dann nicht (D11); der Selbst-Deadlock wird gemeldet statt zu hängen; `Completion`
      **zählt**, weil ein `complete()` vor dem Warten in einem Treiber der Normalfall ist.
      **Offen:** mehrere Threads je PD. Billigster Weg ist das **Manifest** (der Lader legt N an,
      kein neuer Syscall, Kernel kostet **nichts**) — der 96-Byte-Eintrag ist aber voll, es
      bräuchte einen `entry_len`-Bump. Das Format ist dafür selbstbeschreibend ausgelegt; es
      berührt aber die **signierte** Fläche und verdient einen eigenen Durchgang.

### Z21. Linux-Treiber als PD-Prozesse — bewertet 2026-08-09
**Klasse:** Kompatibilitätsschicht · **Aufwand:** groß, aber **einmalig statt je Treiber** ·
**TCB:** neutral

- [ ] **Möglich? Ja, und es ist erprobt — kein Neuschreiben, sondern ein BAU.** Genode fährt
      Linux-Treiber in Userspace-Komponenten (`dde_linux`/`lx_emul`), Intel-Grafik eingeschlossen;
      Rump-Kernel machen dasselbe für NetBSD. Entscheidend an dem Ansatz: **die echten
      Linux-Quellen werden übersetzt**, gegen eine Emulationsschicht. Deshalb skaliert er auf viele
      Treiber, statt einen zu ersetzen.

- [ ] **Was die Schicht liefern muss** — die Liste ist lang, aber endlich:
      Speicher (`kmalloc`/`vmalloc`, Seitenallokator, **DMA-API**), Synchronisation (Spinlocks,
      Mutexe, Completions, Wait-Queues, RCU), Zeit (`jiffies`, `ktime`, Timer, Delays),
      Nebenläufigkeit (`kthread`, Workqueues, Tasklets), **Interrupts** (`request_irq`, threaded
      IRQs), PCI (Konfigurationsraum, BARs, MSI/MSI-X), Gerätemodell (`struct device`,
      probe/remove), `request_firmware`, `readl`/`writel`/`ioremap`.

- [ ] **Vier harte Stellen, die spezifisch für eine PD sind — und drei davon sind Arbeit, nicht
      Zweifel:**

      1. **`CAP_IRQ` ist auf x86 wirkungslos** — **berichtigt am 2026-08-09, nachgesehen statt
         erinnert.** Der Eintrag sagte „im Manifestformat definiert, nicht umgesetzt". Das ist
         falsch: der ganze Weg steht (`install_irq_cap`, `bind_irq`, ein **lock-freier** Hook im
         IRQ-Kontext, der maskiert und vermerkt, und `drain_pending_irqs`, das ausserhalb des
         IRQ-Kontexts als Notification-Badge zustellt) — und er **läuft auf aarch64**, mit
         Prüfzeile (`RTC_INTID`, Badge, `irqs_delivered() >= 1`).
         Wirkungslos ist er auf **x86**, und zwar an einer eng umrissenen Stelle: `enable_intid`,
         `mask_intid` und `route_spi` sind dort dokumentierte **No-Ops** („IOAPIC noch nicht
         portiert"). Es fehlt also nicht der Mechanismus, sondern die x86-Zustellung — und für
         PCIe ist der richtige Weg **MSI-X**, nicht der IOAPIC. Dazu die IRTE-Vergabe (B-3.2 hat
         die Tabelle auf „not present" gesetzt, das ist Absicht).
         **Zweite Lücke, die der Eintrag nicht nannte:** `bind_irq` ist **kernelintern**. Eine
         Treiber-PD kann ihren IRQ nicht selbst binden — der Test tut es von Kernelseite.
      2. **„Interrupts sperren" ist in einer PD bedeutungslos.** `spin_lock_irqsave` ist in Linux
         Ausschluss gegen den *eigenen* IRQ-Handler. In einer PD wird daraus Ausschluss zwischen
         **Threads derselben PD** — braucht also Threads-in-PD (Z19) und die Abbildung
         „Handler = Thread", was genau ein *threaded IRQ* ist.
      3. **`virt_to_phys` gegen IOVA.** Linux-Treiber rechnen mit Physadressen. Hier sind `Pa` und
         `Iova` **getrennte Typen** — die Verwechslung, die dieses Projekt absichtlich unmöglich
         gemacht hat, ist in Linux-Treibern der Normalfall. Die DMA-API muss auf die DMA-Cap
         abgebildet werden. Eher ein Vorteil (die Trennung existiert), aber Arbeit an jeder Stelle.
      4. **Treiber schlafen.** `msleep`, `wait_event`. In einer PD heißt das IPC/Notification —
         machbar, bestimmt aber die Struktur der ganzen Schicht.

- [ ] **Der Preis, der nicht in Zeilen steht.**
      * **Wartung:** Linux' *interne* API ändert sich mit jeder Version. Genode pinnt Kernelstände
        und zieht periodisch nach. Das sind **Dauerkosten**, keine Einmalkosten — dieselbe Sorte,
        die bei [Z17](#z17-turso-als-native-datenbank--vorgemessen-2026-08-09-nicht-begonnen) gegen
        einen Fork gesprochen hat, hier aber unvermeidlich ist.
      * **Lizenz:** Linux-Treiber sind GPLv2, dieses Projekt ist **BSD-2-Clause**. Die PD-Trennung
        ist hier ein **Vorteil**: Treibercode in einem eigenen Prozess hinter einer IPC-Grenze ist
        etwas anderes als ins Kernelbinary gelinkt. Die Emulationsschicht selbst wäre abgeleitetes
        Werk und damit GPL — sauber trennbar, so löst Genode es auch. **Keine juristische Aussage,
        aber die Architektur steht auf der günstigeren Seite.**

- [ ] **Mesa: neu übersetzen JA, portieren NEIN — und der Grund stützt den ganzen Entwurf.**

      Mesa läuft **schon** in Userspace. Die Linux-Trennung ist:
      Kern = i915/xe (Modesetting, Speicherverwaltung, Command-Submission) ·
      Userspace = Mesa/`iris` (Shader übersetzen, Command-Buffer bauen) → `ioctl` auf `/dev/dri/cardN`.

      **Die DRM-uAPI ist stabil und dokumentiert; die interne Kernel-API ist es nicht.** Die
      PD-Grenze fällt also genau dorthin, wo die Schnittstelle stabil ist — die instabile Seite
      liegt *innerhalb* der Schicht. Das ist kein Zufall, sondern derselbe Schnitt, den Linux
      selbst zieht.

      Mesa braucht damit:
      * **Neu übersetzen** gegen die Caprock-libc — wie alles in Z16, kein Sonderfall.
      * `ioctl` → **IPC**. Genau die Form, die `virtio-blk` heute schon hat: OP-Codes über einen
        Endpoint (`OP_INFO`/`OP_READ`/`OP_WRITE`/…). Die ABI dieses Kernels kennt kein `ioctl`
        (0 Vorkommen) und braucht auch keines.
      * `mmap` von GEM-Puffern → Speicher-Caps + `SYS_MAP`.
      * Fences/Sync → Notifications.

      **Mesa wird also nicht umgeschrieben**, und dieser Teil veraltet auch nicht mit jeder
      Linux-Version.

- [ ] **Sinnvoll? Bedingt ja — und die Bedingung ist die Reihenfolge.**
      * Für den **Server** ist es **nicht** der nächste Schritt. NVMe (~1500 Zeilen, selbst
        geschrieben) und die RTL8168 sind billiger als eine Linux-Schicht, und ein Server braucht
        keine GPU.
      * Für den **Desktop** ist es der einzige realistische Weg — i915 neu zu schreiben ist keiner.
      * **Der Wendepunkt:** sobald mehr als zwei oder drei nichttriviale Linux-Treiber gebraucht
        werden, ist die Schicht billiger als die Einzelportierungen. Bei WiFi (MT7925, `mac80211`)
        allein wäre sie es vermutlich schon.

      **Empfehlung: nicht jetzt.** Aber **`CAP_IRQ` ist Vorbedingung für beides** — für eigene
      Treiber wie für geliehene — und gehört deshalb vorgezogen, unabhängig davon, wie diese Frage
      entschieden wird.

### Z20. Was bis zu einem nutzbaren SERVER-OS fehlt — und was ein Desktop kosten würde
**Klasse:** Einordnung · **Stand:** 2026-08-09 · Messwerte sind gemessen, Schätzungen sind als
solche markiert.

- [ ] **Server-OS: erreichbar, und der vorhandene Plan deckt den größeren Teil.** Z19 liefert das
      Substrat, Z16 die libc, Z14 den Speicher-Server. **Was darüber hinaus fehlt und heute in
      keinem Punkt steht:**

      | fehlt | Größe (Schätzung) | TCB |
      |---|---|---|
      | **Dateisystem über FAT16 hinaus** — Verzeichnisse, Pfade, VFS; FAT16 mit *einer* Datei ist kein Serverdateisystem | groß, aber abhängigkeitsfrei wie `caprock-part`/`-fat` | neutral |
      | **TCP/IP** — `smoltcp` in einer PD (Rust, `no_std`, erprobt) | Wochen zum Laufen, Monate zur Härtung | neutral |
      | **NVMe-Treiber** — `virtio-blk` läuft nur in VMs | klein: Queue-Paare, Admin- + IO-Queue, PRP-Listen | neutral (PD) |
      | **Realtek RTL8168** — die Netzkarte dieses Laptops, klassisch und dokumentiert | mittel, weit billiger als WiFi | neutral (PD) |
      | **Eigenständiger Boot auf Blech** | GRUB-ISO steht; offen sind ACPI/APIC auf echten Chipsätzen | — |
      | **Dienstverwaltung, Protokollierung, Konfiguration** | mittel | neutral |

      **Der Ist-Stand auf echter Hardware ist besser als „nie".** Die Kern-Übergabe
      (`tools/handover/`, Stufen 0/1a/1b belegt) nimmt fünf E-Cores offline und schickt ihnen
      INIT-SIPI-SIPI — Caprock hat auf diesem Blech schon Befehle ausgeführt, nur nicht
      eigenständig.

      **Schätzung, ausdrücklich als solche:** ein *demonstrierbarer* Server — statisch gebaute
      Rust/C-Binaries, HTTP über `smoltcp`, Daten auf NVMe, eigenständig gebootet — liegt bei
      **6–12 Monaten** fokussierter Arbeit für eine Person. *Nutzbar* im Sinne von „ein Fremder
      betreibt das" eher **1–3 Jahre** mit kleinem Team. Was die Schätzung trägt: jeder Punkt oben
      ist **begrenzt und TCB-neutral**, und genau eine Zeile im ganzen Vorhaben braucht den Kernel.

- [ ] **Desktop mit KDE auf diesem Laptop: zwei bis drei Größenordnungen mehr — und nicht dieselbe
      Art Arbeit.** Das ist der Punkt: die Volumina liegen dort, wo Messen und kleine TCB sie
      **nicht** verkleinern.

      **BERICHTIGUNG (2026-08-09, nach Einwand): der Vergleich „i915 ist 1,9 MB, die TCB 236 KiB"
      war schief — und zwar nach der eigenen Logik dieses Projekts.** Ein Grafiktreiber gehört in
      eine **HardwareLand-PD**, genau wie `virtio-blk` und `virtio-net`, und dann wächst die TCB um
      **null**. Dieselbe Rechnung wie bei der WASM-Engine (216 KiB Code, so groß wie der Kern, aber
      in einer PD). Ein abstürzender Grafiktreiber reißt dann seine PD ab und nicht das System —
      das ist ein **Vorteil** gegenüber Linux, wo ein GPU-Treiberfehler ein Kernelfehler ist, und
      es ist die Produktthese am schwersten möglichen Fall.

      Die Architektur dafür steht zum großen Teil: MMIO-Fenster auf der eigenen
      Konfigurationsraum-Seite, DMA-Region mit eigenem VT-d-Kontext und eigenem IOVA-Fenster,
      Gerätezuteilung aus dem Manifest, und die Trennung zweier Treiber-PDs ist **gemessen**
      (A-5.4). Was fehlt, ist nicht die Isolation, sondern:

      | Baustein | die tatsächliche Schwierigkeit |
      |---|---|
      | **`CAP_IRQ`** | im Manifestformat definiert, **nicht umgesetzt** — jeder Treiber pollt. Für eine GPU nicht gangbar. Braucht IRTE-Vergabe (B-3), und die Remapping-Tabelle steht seit B-3.2 absichtlich auf „not present" |
      | **Die Linux-Treiber-API** | i915 lässt sich nicht allein herausheben: es hängt an DRM-Core, GEM/TTM, dma-buf, Workqueues, PCI-Subsystem. Portieren heißt eine **Schicht** bauen, die Linux-Treibercode ohne Linux ausführt |
      | **Firmware** | GuC/HuC-Blobs laden — braucht ein Dateisystem |
      | **Menge** | ~150k Zeilen für i915 allein, TCB-neutral hin oder her. Neutral heißt nicht billig |

      **Der Präzedenzfall, und er macht die Sache plausibler als ich sie dargestellt habe:**
      **Genode** fährt Linux-Treiber in Userspace-Komponenten (`dde_linux`), Intel-Grafik
      eingeschlossen. Das ist erprobt, nicht hypothetisch.

      **Und es ändert die Wirtschaftlichkeit:** die Schicht ist die Investition, die Treiber sind
      danach vergleichsweise billig — sie schaltet **alle** Linux-Treiber frei, nicht einen: WiFi,
      GPU, Audio, USB. Wer den Desktop will, baut nicht i915, sondern `dde_linux`. Das ist ein
      eigener Strang in der Größenordnung von Z16, und er hat denselben Charakter:
      **Kompatibilitätsschicht in Userland, Kern unberührt.**

      | Baustein | warum der Desktop trotzdem teuer bleibt |
      |---|---|
      | **Ausweg Framebuffer** | EFI-GOP ohne Beschleunigung ist machbar — ohne 3D, ohne Videodekodierung, ohne Moduswechsel, ohne externen Monitor, ohne Hotplug |
      | **Qt6** | ~5 Mio. Zeilen, braucht volles POSIX plus fontconfig, freetype, harfbuzz, ICU, D-Bus, Wayland. Portierbar (QNX zeigt es), aber ein eigenes Projekt |
      | **KDE Frameworks + Plasma** | nochmals Millionen Zeilen darüber |
      | **WiFi MT7925** | 802.11be, sehr neu; braucht Firmware und setzt auf `mac80211` (~200k Zeilen) auf |
      | **Eingabe** | xHCI + USB-HID + I2C-HID (Touchpad) + eine libinput-artige Schicht |
      | **Suspend/Resume, Akku, Thermik, Helligkeit** | berührt **jeden** Treiber — kein Feature, sondern eine Eigenschaft des ganzen Systems |

      **Und es dient der Produktthese nicht.** Die These ist eine Cloud für Vercel-artige Dienste,
      Isolation durch den Kern statt durch VMs. Ein Desktop wäre ein Hobbymeilenstein.

- [ ] **Die interessante Mitte, geteilt mit dem Serverpfad: HEADLESS auf echtem Blech.**
      NVMe + RTL8168 + serielle oder Framebuffer-Konsole, eigenständig gebootet. Belegt „läuft auf
      echter Hardware" — heute nur halb wahr — und **jede Zeile zählt für den Server**.

      Nebenbei fällt dort etwas, das dieses Projekt ausdrücklich offen führt: die Farbtrennung (A1)
      ist **auf Blech** als Wirkung messbar, unter KVM strukturell nicht (§12).

### Z19. Das SUBSTRAT der Sprachlaufzeiten — was C/C++/Rust/Zig brauchen, bevor irgendein Dienst existiert
**Klasse:** Grundlage · **Aufwand:** überschaubar und **vollständig aufzählbar** ·
**Abgrenzung:** ohne Netzstack, ohne Dateisystem — nur, was eine Laufzeit zum *Starten und Rechnen*
braucht.

Nachgesehen am 2026-08-09. Die Liste ist kurz, aber vier ihrer Punkte stehen **vor** allem, was in
[Z16](#z16-quelltext-übersetzen-statt-binaries-laufen-lassen--bewertet-2026-08-09) geplant ist —
ohne sie startet nicht einmal ein `main(){return 0;}`.

- [ ] **A1. Der Prozessstart-Stack fehlt vollständig.** Ein geladenes Programm bekommt heute
      `boot_arg` in **einem Register**. `_start` von musl, `lang_start` von Rust und Zigs `_start`
      erwarten dagegen den **System-V-Prozessstartstack**: `argc`, `argv[]`, `envp[]` und —
      entscheidend — **`auxv`**.

      `auxv` ist keine Formalie: `AT_PHDR`/`AT_PHNUM` sagen der Laufzeit, wo ihre eigenen
      Program-Header liegen (der Entroller und die TLS-Einrichtung brauchen das), `AT_PAGESZ` die
      Seitengröße, `AT_RANDOM` die Bytes für den Stack-Canary, `AT_HWCAP` die CPU-Merkmale.
      Ohne `auxv` fällt musl beim Start um, bevor eine Zeile Nutzcode läuft.

- [ ] **A2. Es gibt keine TLS-Basis — und das trifft C schon ohne Threads.** Weder `IA32_FS_BASE`/
      `wrfsbase` (x86) noch `TPIDR_EL0` (aarch64) werden für Userland gesetzt, und der Trap-Frame
      führt sie nicht mit; ein Kontextwechsel würde sie also auch nicht erhalten.

      **`errno` ist in musl thread-lokal.** Ohne TLS ist damit nicht „Threading kaputt", sondern
      **die C-Bibliothek als solche**. Dasselbe gilt für Rusts `#[thread_local]` und C++'
      `thread_local`.

      Dazu gehört: **der Lader kennt `PT_TLS` nicht** (`elf.rs` liefert ausschließlich `PT_LOAD`).
      Das TLS-Abbild eines Programms wird also gar nicht erst gefunden. Drei Teile: `PT_TLS` lesen,
      den Block je Thread anlegen, die Basis setzen **und über den Kontextwechsel führen**.

- [ ] **A3. Der Stack ist 16 KiB.** `LOADED_STACK_BYTES = 0x4000`. Übliche Vorgabe für den
      Hauptthread ist **8 MiB** (glibc, musl, Rust). Rekursion, große Stackrahmen in C++ oder ein
      Formatierer mit Puffer auf dem Stack laufen darüber. Das ist kein Feintuning, sondern eine
      Größenordnung — und es gehört ins Manifest, nicht in eine Konstante.

- [ ] **A4. SSE ist auf x86 nicht eingeschaltet** — s. [Z18](#z18-hohe-leistung-für-übersetzten-fremdcode--vermessen-2026-08-09) (1).
      `CR4.OSFXSR` wird nirgends gesetzt, die SysV-ABI verlangt XMM für `double`. Gemessen mit der
      neuen `fp`-Prüfzeile.

- [ ] **B. Danach erst wird es interessant — und diese drei sind alles, was ohne Dienste fehlt:**

      | | wofür | Stand |
      |---|---|---|
      | **Speicher** (`mmap`/`brk`) | jedes `malloc`, jedes `Vec`, jedes `new` | fehlt (Z14 Stufe 1) |
      | **Konsole** (`write` auf 1/2) | ohne sie kann kein Programm etwas berichten — kein Dateisystem, ein Dienst | fehlt |
      | **Zeit** (`clock_gettime`) | Rusts `std::time`, C++ `<chrono>` | fehlt (vDSO-Seite, W2) |

- [ ] **C. Was sprachspezifisch dazukommt.**

      | Sprache | zusätzlich | Bemerkung |
      |---|---|---|
      | **Zig** | fast nichts | Allokatoren sind **explizit** (werden durchgereicht), kein verstecktes `malloc`. Ein Zig-Programm mit `FixedBufferAllocator` braucht nur A1–A4. **Die billigste erste Sprache.** |
      | **C** | A1–A4 + B | `errno` macht A2 zur Pflicht |
      | **Rust** | + `std::sys`-Port | `panic = "abort"` steht schon → **kein Entroller nötig**; dafür fällt `catch_unwind` weg |
      | **C++** | + **Entroller** (`.eh_frame`, `libunwind`) | Ausnahmen sind die eine echte Zusatzforderung. Braucht keine Syscalls, aber `AT_PHDR` aus A1 |

- [ ] **Was „alle möglichen Programme" NICHT heißen kann, und das gehört danebengeschrieben.**
      Auch mit A und B und C laufen nicht alle: `fork` ohne `exec`, `dlopen`, `mmap` einer Datei,
      `/proc`, und jedes Programm, das `syscall` direkt schreibt statt über die libc. Die ehrliche
      Formulierung ist **„alles, was CPU, Speicher, Konsole und Zeit braucht"** — das ist sehr viel
      (Übersetzer, Kompression, Kryptografie, Datenstrukturen, Rechenlasten), aber es ist eine
      benennbare Menge und keine Allaussage.

---

## Z19 — der vollständige Plan (2026-08-09)

**Grundsatz, der jede Einzelentscheidung unten bestimmt:** so wenig wie möglich in den Kernel. Für
jeden Punkt steht deshalb dabei, was der Kernel *tun muss* und was Userland selbst erledigt.

### Schritt 0 — die Konsole (vorgezogen)

Jede Abnahme **ab Schritt 2** ist eine Ausgabe („Zig meldet `argc`", „zwei Threads melden
verschiedene TLS-Werte", „gerechnetes Ergebnis"). Ohne Konsole wäre die erste Abnahme, die
*fehlschlagen kann*, nicht von einer zu unterscheiden, die nur **nichts sagen kann** — genau die
Prüfer-Falle aus dem eigenen Protokoll. Der Ringpuffer (TCB-neutral, der Kernel behält seine
eigene Ausgabe) gehört deshalb **vor** Schritt 2.

**Nicht vor Schritt 1:** dessen Abnahme ist die `fp`-Zeile, und die druckt der **Kernel**.
Schritt 1 ist damit selbsttragend.

### Schritt 1 — A4: SSE freischalten (VIER Bits) + die Eager/Lazy-Entscheidung

- **`[~]` Angefangen am 2026-08-09, NICHT gelandet — s. „Stand" unten.**
- **Vier Bits, nicht eines:**

  | Bit | wofür |
  |---|---|
  | `CR0.EM` = **0** | solange gesetzt, faultet jede FP-Instruktion — **unabhängig** von OSFXSR |
  | `CR0.MP` = **1** | lässt `WAIT`/`FWAIT` zusammen mit `TS` richtig trappen |
  | `CR4.OSFXSR` = 1 | schaltet `FXSAVE`/`FXRSTOR` **und** SSE frei |
  | `CR4.OSXMMEXCPT` = 1 | lenkt SIMD-FP-Ausnahmen auf `#XM` statt `#UD` |

- **Auf JEDEM Kern** — `CR0`/`CR4` sind kernlokal. Eine gemeinsame Funktion `hal::fp::enable_sse()`
  für BSP und AP, damit die zwei Stellen nicht auseinanderlaufen können, plus ein Zähler
  „auf wie vielen Kernen freigeschaltet", der `num_cores()` sein muss.
- **Vorher `CPUID.1:EDX` prüfen** (FXSR Bit 24, SSE Bit 25) — sonst `#GP` auf fremder Hardware.
- **Die Abnahme läuft auf einem AP, nicht auf dem BSP.** Eine Sonde nur auf Kern 0 prüft genau die
  eine Stelle, die man am ehesten richtig macht.

- **Die Sicherheitsentscheidung gehört HIERHER: eager statt lazy.**
  Lazy-FP über `CR0.TS` ist auf Intel **LazyFP, CVE-2018-3665** — spekulative Ausführung liest den
  *alten* FP-Registersatz, bevor der `#NM`-Trap den Wechsel nachholt; die XMM-Register des vorigen
  Threads werden über einen Spectre-Kanal auslesbar. Linux, Windows und die BSDs sind deshalb auf
  eager umgestiegen.

  **Bis heute war das folgenlos** — Userland war soft-float, SSE war aus, in XMM stand nichts. Mit
  `enable_sse()` stehen dort echte Daten, und XMM ist genau der Ort, an dem
  AES-NI-Schlüsselmaterial lebt. Für ein System, dessen Verkaufsargument Isolation ist, wäre lazy
  über PD-Grenzen kein Abwägen zwischen Leistung und Sicherheit, sondern **zwischen Leistung und
  der eigenen These**.

  **Die Vorher-Zahl steht schon:** `Lazy-FP-Owner-Wechsel=399` aus der aarch64-Suite. Damit lassen
  sich die Eager-Kosten beziffern, falls jemand zurückdrehen will — ohne sie wäre so ein Rückbau
  eine Meinung.

- **`[x]` A4 GELANDET (2026-08-09, dritter Anlauf) — und der Weg dahin war der Ertrag.**

  **Zuerst zwei eigene Fehlaussagen, die berichtigt gehören:**
  * „Alle vier Bits haben dieselbe Wirkung auf die Fault-Leitung" war **architektonisch falsch**
    und methodisch unbelegt. `CR4.OSXMMEXCPT` verschiebt nichts von `#UD` nach `#NM` — es
    entscheidet nur, ob eine *unmaskierte SIMD-Ausnahme* als `#XM` gemeldet wird, und die setzt
    laufenden SSE-Code voraus. Gemessen hatte ich ausserdem drei **Kombinationen**, nicht vier
    Einzelbits. Eine Verallgemeinerung über Messungen, die es nicht gab.
  * Das Symptom hatte ich zweimal falsch gelesen: es war ein **Hänger** (Zeitlimit), und alle 14
    `FAIL`-Zeilen waren Folgen davon.

  **Die billige Messung hat die teure Analyse ersetzt.** Roter Build unter **TCG** mit
  `-d int -D fault.log` (unter KVM sieht man nichts — der Wirtskern behandelt die Interrupts):

      22: v=07 e=0000 cpl=3 IP=0023:...16a016   <- Ring 3 faultet auf FP (erwartet)
      23: v=07 e=0000 cpl=0 IP=0008:...12791b   <- der KERNEL faultet auf FP -- IM HANDLER

  Zwei Zeilen, und die Ursache steht da. **Kein `#GP` beim CR-Schreiben** — der Schreibpfad ist
  damit entlastet, die Alternativhypothese widerlegt.

  **Die Ursache:** `fp_trap` rief `hal::fp::save`/`restore` **vor** `set_el0_trap(false)`.
  `FXSAVE`/`FXRSTOR` sind selbst FP-Instruktionen — mit gesetztem `CR0.TS` lösen sie genau den
  Trap aus, aus dem heraus sie gerufen werden. Rekursion, Hänger.

  **Warum es nie aufgefallen ist, und das ist die eigentliche Lehre:** `CR0.TS` gilt für **jede**
  Privilegstufe, `CPACR_EL1.FPEN` auf ARM dagegen nur für EL0 — dort darf der Kernel immer
  rechnen, und **derselbe Aufrufercode ist korrekt**. Der x86-Pfad war tot (`CR0.EM = 1` machte SSE
  zu `#UD`, x87 benutzt ein soft-float-Kernel nicht), also feuerte `#NM` nie. **SSE freizuschalten
  macht toten Code lebendig.**

  **Behoben in der HAL, nicht beim Aufrufer:** `save`/`restore` löschen `CR0.TS` selbst. Beim
  Aufrufer wäre es Disziplin; in der HAL kann die nächste Aufrufstelle es nicht vergessen.

  **Gelandet:** `enable_sse()` (vier Bits, CPUID-Prüfung, eine Funktion für BSP und AP),
  aufgerufen auf beiden — auf dem AP **nach** `init_core()`, weil davor kein Scheduler existiert,
  und **vor** der Freigabe, weil `fxsave` XMM nur mit `OSFXSR` zuverlässig sichert. Zähler
  `SSE_CORES` **ohne** `cfg` (ein `cfg` nur am Zähler bei unbedingter Aufrufstelle liess
  `--no-default-features` nicht mehr übersetzen — das war der zweite der beiden vermengten Fehler).
  Abnahme: beide Konfigurationen bauen, x86 `RUNS=5` grün.

- [ ] **A4-Rest, und er folgt aus der Eager-Entscheidung:** im Eager-Modus ist `#NM` **kein zu
      behandelnder Fall mehr, sondern per Definition ein Kernelfehler** — der Trap existiert nur,
      weil `TS` gesetzt war, und eager heisst: `TS` ist nie gesetzt. Den Handler zu *härten* wäre
      also die falsche Konsequenz; richtig ist, ihn durch eine **laute Meldung mit RIP und Kern-ID**
      zu ersetzen und die Invariante **„`TS == 0` nach jedem Wechsel"** in die Prüfzeile zu nehmen.

      Dabei ist auf den **Lazy-Restbestand** zu achten: die alte Maschinerie *setzt* `TS` beim
      Ownerwechsel. Bleibt ein Pfad davon stehen, ist eager deklariert und lazy gebaut — und der
      erste FP-nutzende User-Thread findet es heraus.

      Und: die aarch64-Zeile `Lazy-FP-Owner-Wechsel=399` misst danach **etwas anderes** und muss
      umbenannt oder neu definiert werden, sonst vergleicht das Protokoll Vorher und Nachher über
      einen Bedeutungswechsel hinweg.

- [ ] **Die Verallgemeinerung, die mehr wert ist als der Einzelfall: ein VEKTOR-INVENTAR.**
      „Handler installiert, nie ausgeführt" ist eine **Klasse**, und sie ist jetzt zweimal
      getroffen worden (die fehlende x86-`fp`-Prüfzeile war dieselbe Lücke von der anderen Seite).
      Ein Melder für die erste Ausführung ist zu klein gedacht: **jeder Trap-Vektor bekommt einen
      Zähler**, und die Suite druckt am Ende, welche Vektoren auf welcher Architektur je gefeuert
      haben. Ein Array und eine Druckzeile — und aus „toter Code, den niemand kennt" wird eine
      Inventarliste, die bei jedem Lauf mitkommt. Ein Pfad, der laut Inventar auf aarch64 feuert
      und auf x86 nie, ist dann kein Zufallsfund beim Debuggen, sondern eine offene Zeile im
      Protokoll.

- [ ] **Noch nicht wieder drin** (fielen beim Zurücksetzen weg, sind aus der Beschreibung
      rekonstruierbar): die Ring-3-`fp`-Sonde mit zwei verschiedenen Mustern, ihr Lauf auf einem
      **AP** statt dem BSP, und die `fp`-Zeile in `all_done()`. Sie kommen mit dem Vektor-Inventar
      zusammen, das ihr Zeuge ist.

### Schritt 2 — A1: der Prozessstart-Stack

- **Ein Fund, der vorher zu klären war:** die Program-Header liegen in **keinem** `PT_LOAD`.
  `programs/user-x86.ld` beginnt mit `. = 0x20000000;` ohne `SIZEOF_HEADERS`; `readelf` bestätigt
  es (erstes `LOAD` bei Offset `0x1000`). **`AT_PHDR` zeigte damit ins Leere** — und daran hinge
  sowohl die TLS-Einrichtung als auch der C++-Entroller. Behebung im Linkerskript:
  `. = 0x20000000 + SIZEOF_HEADERS;`, damit das erste Segment ab Offset 0 abbildet. Standard­praxis,
  kostet nichts, muss aber **vor** A1 passieren.
- **Wo:** `load_into_pd_mit` (`system.rs`), unmittelbar vor `spawn_user_at_parked`; und
  `init_thread_frame` (`crates/caprock-hal/src/x86_64/exception.rs:338`) bekommt einen fertigen
  `rsp` statt eines Arguments in `rdi`.
- **Aufbau, von oben nach unten:** Zeichenketten (`argv[0]`, Umgebung) · Auffüllung ·
  `auxv` (mit `AT_NULL` abgeschlossen) · `envp` (NULL) · `argv` (NULL) · `argc`. `rsp` zeigt auf
  `argc`.
- **`auxv`-Mindestmenge:** `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_ENTRY`, `AT_RANDOM`
  (16 Byte), `AT_HWCAP`, `AT_NULL`.
- **Falle, und sie ist im Quelltext schon halb dokumentiert:** SysV verlangt beim **Funktions**-
  eintritt `rsp % 16 == 8` (so, als hätte ein `call` gerade die Rücksprungadresse abgelegt) — beim
  **Prozess**-eintritt dagegen `rsp % 16 == 0`. Der Kommentar an `init_thread_frame` beschreibt
  heute den ersten Fall; für `_start` gilt der zweite. Wer das verwechselt, bekommt einen
  `#GP` bei der ersten 16-Byte-ausgerichteten SSE-Operation — also **erst nach Schritt 1**, und
  dann sieht es wie ein FP-Fehler aus.
- **`AT_RANDOM` braucht echten Zufall, und den gibt es beim Laden noch nicht.** `virtio-rng` ist
  eine Treiber-PD und läuft später. Eine TSC-Mischung ist **kein** Zufall; wer sie einsetzt, muss
  es hinschreiben, sonst ist der Stack-Canary Schmuck. Ehrliche Zwischenlösung: `AT_RANDOM` mit
  einer benannten, als schwach markierten Quelle füllen und den Punkt offen führen.
- **Abnahme:** ein Zig-Programm liest `argc` und `AT_PAGESZ` aus seinem Startstack und meldet die
  **Werte**. Nicht „gestartet" — ein Programm, das startet und Unsinn liest, sähe genauso aus.

### Schritt 3 — A2: TLS

- **Die Erkenntnis, die den Aufwand halbiert:** musl richtet TLS **selbst** ein. `__init_tls`
  findet `PT_TLS` über `AT_PHDR`, legt den Block an und ruft dann `arch_prctl(ARCH_SET_FS)`. Der
  Kernel muss `PT_TLS` also **nicht verstehen** — der Lader bleibt unverändert. Er braucht drei
  Dinge:
  1. `AT_PHDR` (kommt aus Schritt 2),
  2. **einen Syscall „setze die TLS-Basis dieses Threads"**,
  3. die Basis **im TCB** und über den Kontextwechsel geführt.
- **Kernelanteil:** x86 `MSR_FS_BASE` (`0xC000_0100`) schreiben — oder `CR4.FSGSBASE` + `wrfsbase`,
  was schneller ist und einen eigenen Punkt darstellt. aarch64: `TPIDR_EL0`. Dazu ein Feld im TCB
  und ein Schreibzugriff im Wechsel.
- **Sicherheitsnotiz, die dazugehört:** die TLS-Basis ist eine beliebige User-Adresse. Der Kernel
  **dereferenziert sie nie** — `%fs:0` liest ausschließlich Ring 3. Sonst wäre der Syscall ein
  „lies mir eine Adresse meiner Wahl"-Dienst.
- **Der Modelltreue-Wächter wird das beanstanden** (neues TCB-Feld, neue Schreibstelle). Das ist
  erwartet und gehört ins Register — s. `admitted` am 2026-08-07.
- **Abnahme:** zwei Threads mit **verschiedenen** TLS-Werten, wechselseitig über N Wechsel geprüft
  — dieselbe Form wie die `fp`-Sonde. Verschieden ist wieder der Punkt: mit gleichen Werten fiele
  ein fehlendes Sichern nicht auf.

### Schritt 4 — A3: Stack und Guard-Page

- **Nicht raten, messen.** Wie tief geht ein Zig-/C-Hello wirklich? Stack mit einem Muster füllen,
  nach dem Lauf das Wasserzeichen zählen. Erst danach eine Zahl festlegen.
- **Interim:** `LOADED_STACK_BYTES` (heute 16 KiB) auf den gemessenen Bedarf plus Reserve.
- **Richtig:** ins Manifest — und das braucht mehr als `entry_len = 96`, also **Formatversion 2**.
  Diese Version kann `period_us` aus [Z16](#z16-quelltext-übersetzen-statt-binaries-laufen-lassen--bewertet-2026-08-09)
  gleich mitnehmen: **eine** Version für zwei Felder statt zweier Versionen.
- **Eine Guard-Page gehört dazu, sonst ist „größerer Stack" nur „später kaputt".** Ohne sie
  überschreibt ein Überlauf still, was darunter liegt.
  **Abnahme:** eine Rekursion definierter Tiefe läuft; eine tiefere **faultet sauber** — und der
  Fault liegt in der Guard-Page, nicht irgendwo.

### Schritt 5 — B: die drei Dienste

| | Entwurf | Anmerkung |
|---|---|---|
| **Speicher** | Z14 Stufe 1: eine PD hält eine große Memory-Cap und gibt abgeleitete per IPC-REPLY | TCB-neutral, `CCOPY` und Cap-Transfer stehen beide schon |
| **Zeit** | vDSO-Seite (W2): nur-lesbar in jede PD, Tickzähler + Frequenz | kein Syscall; genau das, was Linux selbst tut |
| **Konsole** | s. u. — **hier ist eine Entwurfsentscheidung zu treffen, keine Umsetzung** | |

**Die Konsolenfrage, ausgeschrieben:** der Kernel besitzt heute die serielle Schnittstelle für
seine eigenen Berichte. Drei Wege:

* **(a) Debug-Syscall.** Billig, aber die TCB wächst, und ein „nur zum Debuggen"-Syscall bleibt.
* **(b) Konsolen-PD bekommt das Gerät.** Sauber — aber der Kernel verlöre seine Ausgabe, und die
  ist der Träger jeder Prüfzeile. Für den Selbsttest nicht hinnehmbar.
* **(c) Geteilter Ringpuffer.** Das Programm schreibt in eine Seite, der Kernel leert sie beim
  Bericht. **TCB-neutral**, und es ist dieselbe Form wie Z18(5).

**Empfehlung: (c).** Falls (a) als Zwischenschritt genommen wird, gehört er als *Debug* markiert
und in `docs/invariants.md` ausdrücklich als nicht-produktiv benannt — sonst bleibt er.

### Die Abnahmekette, und warum sie in dieser Reihenfolge trennt

1. nach **A4**: `fp : ALL PASS`, in `all_done()` — SSE **und** Lazy-Save belegt.
2. nach **A1**: Zig meldet `argc` und `AT_PAGESZ` — der Prozessstart trägt, **ohne** Speicherdienst.
3. nach **A2**: zwei Threads, verschiedene TLS-Werte, über Wechsel erhalten — `errno` wäre möglich.
4. nach **A3**: definierte Rekursion läuft, tiefere faultet in der Guard-Page.
5. nach **B**: ein **Zig**-Programm mit `FixedBufferAllocator` rechnet und meldet ein
   **gerechnetes** Ergebnis über die Konsole.

**Zig zuerst, und das ist kein Geschmack:** es ist die einzige der vier Sprachen, die ohne
Speicherdienst auskommt (Allokatoren werden explizit durchgereicht). Damit trennt die Abnahme den
**Prozessstart** von der **Speicherfrage**, statt beides zugleich zu prüfen — und wenn Schritt 5
fehlschlägt, ist bekannt, dass 1–4 stehen.

### Was dieser Plan NICHT löst, und das gehört dazu

* **C++-Ausnahmen** brauchen den Entroller (`.eh_frame` + `AT_PHDR`). A1 liefert die Vorbedingung,
  mehr nicht.
* **Rusts `std`** braucht zusätzlich den `sys`-Port; `panic = "abort"` steht schon, ein Entroller
  ist dort also nicht nötig.
* **Threads** kommen nicht vor. Ein Programm, das `std::thread` *benutzt*, läuft nach diesem Plan
  nicht — nur eines, das sie höchstens linkt.
* **`fork`, `dlopen`, `mmap` einer Datei, `/proc`** bleiben außerhalb, s. Z19-Abgrenzung.

**Aufwandsschätzung, ausdrücklich als Schätzung markiert:** A4 ist Stunden, A1 und A2 je Tage,
A3 Stunden plus die Formatversion, B(Zeit) Stunden, B(Speicher) und B(Konsole) je Tage. Die Zahlen
sind geraten — was sie belastbar machte, wäre die erste Umsetzung, und deshalb steht A4 vorn.

### Z18. Hohe Leistung für übersetzten Fremdcode — vermessen 2026-08-09
**Klasse:** Leistung · **Aufwand:** ein Punkt ist fast umsonst, einer ist Voraussetzung für alles
Weitere · **Randbedingung:** Isolation und kleine TCB bleiben.

Kompatibilität ([Z16](#z16-quelltext-übersetzen-statt-binaries-laufen-lassen--bewertet-2026-08-09))
und **Leistung** sind verschiedene Achsen, und auf der zweiten verlieren Mikrokerne historisch.
Die Gründe sind aber benennbar und einzeln messbar.

- [ ] **(1) Das User-Ziel rechnet Fliesskomma in SOFTWARE — gemessen, nicht vermutet.**
      Dieselbe Funktion (Skalarprodukt über 64 `f64`), einmal für ein normales x86_64-Ziel und
      einmal für `programs/x86_64-caprock-user.json` übersetzt:

      | | Befehle | SSE-FP | Bibliotheksaufrufe |
      |---|---|---|---|
      | normales x86_64-Ziel | 18 | **8** (`mulsd`/`addsd`) | keine |
      | **Projektziel** | 53 | **0** | **4× `__muldf3` + 4× `__adddf3`** |

      Jede Multiplikation und jede Addition wird ein **Aufruf**. Ein `mulsd` kostet ~4 Zyklen;
      `__muldf3` entpackt, multipliziert 64×64→128 Bit, normalisiert, rundet, packt — Größenordnung
      50–150 Zyklen mit Aufrufkosten. Für numerischen Code ist das der Unterschied zwischen
      brauchbar und unbrauchbar.

      **Und der Kern verlangt es nicht.** Er hat vollständiges Lazy-FP: `fxsave64`/`fxrstor64` mit
      `CR0.TS`-Trap auf x86, `CPACR_EL1.FPEN` auf aarch64 — genau damit User-Threads FP benutzen
      *können*. Das `+soft-float` im **User**-Ziel ist aus der Entscheidung „soft-float
      Microkernel" (ext-3/ext-5) mitgewandert; für den Kern ist sie richtig, für Userland nicht.

      **BERICHTIGUNG (2026-08-09, nach Einwand): das ist kein Leistungspunkt, sondern ein
      Z16-BLOCKER auf x86.** Die x86-64-SysV-ABI *setzt SSE2 voraus* — `double` wird in
      XMM-Registern übergeben und zurückgegeben. Nachgemessen an derselben Funktion `f64 -> f64`:

      | | Argumente | Rückgabe |
      |---|---|---|
      | normales x86_64 (SysV) | `%xmm0`, `%xmm1` | `%xmm0` |
      | **Projektziel** | `%rdi`, `%rsi` | `%rax` |

      Das sind **zwei Aufrufkonventionen**. Ein upstream gebautes musl nimmt die erste an; der
      Linker sieht nur gleiche Symbolnamen und kann die Verwechslung nicht bemerken — soft-float-
      Aufrufer gegen hard-float-Bibliothek ist **stille Korruption**, keine Fehlermeldung. Die
      Alternative wäre ein Custom-ABI-musl, also genau die Fork-Pflege, die bei
      [Z17](#z17-turso-als-native-datenbank--vorgemessen-2026-08-09-nicht-begonnen) verworfen
      wurde. Die Entscheidung steht damit **vor** dem musl-Port fest, unabhängig von jeder
      Zyklenzahl.

      **Zwei Bedingungen, bevor das umgestellt wird:**
      * **[x] Die x86-`fp`-Prüfzeile steht seit 2026-08-09 — und sie hat sofort etwas gefunden.**
        Zwei Ring-3-Sonden laden verschiedene Muster in `xmm0..xmm3`, geben per `YIELD` ab und
        prüfen nach jeder Rückkehr. Ergebnis: `gelaufen=true`, Muster erhalten **`0b0`** von
        `0b11`.

        **Ursache gemessen, nicht vermutet: `CR4.OSFXSR` wird nirgends gesetzt.** Ohne dieses Bit
        löst *jede* SSE-Instruktion ein **`#UD`** aus statt eines `#NM` — die Sonde faultet beim
        ersten `movq xmm0` und erreicht den Owner-Wechsel nie. Der `#NM`-Handler existiert
        (`exception.rs:437`), `fxsave64`/`fxrstor64` auch; was fehlt, ist das **Freischalten des
        Befehlssatzes**.

        Damit ist auch klar, dass `+soft-float` im User-Ziel kein Überbleibsel war, sondern zu
        einem Kernel passt, der SSE nie eingeschaltet hat. Die Arbeit ist also: `CR4.OSFXSR` (+
        `OSXMMEXCPT`) setzen, die Prüfzeile grün bekommen, **dann** das Ziel umstellen.

        Die Zeile steht **nicht** in `all_done()`. Das ist eine Entscheidung: eine bekannte Lücke
        dauerhaft rot zu färben verdeckt jede künftige Regression. Sie wird gegattert, sobald
        `CR4.OSFXSR` gesetzt ist — vorher wäre es eine Anforderung, die diese Konfiguration nicht
        erfüllen *kann*, dieselbe Form wie `root` ohne Archiv.

      * ~~Auf x86 gibt es keine `fp`-Prüfzeile~~ — nur aarch64 maß den Lazy-FP-Pfad
        (`fp : EL0-FP-Threads-OK=0b11/0b11, Owner-Wechsel=399`). SSE einzuschalten hiesse, einen
        Pfad scharfzustellen, der auf dieser Architektur **nie ausgeführt** wurde. Dieselbe
        Fehlerform wie „der Farbtest lag im x86-Hochlauf und lief auf aarch64 nie", nur
        gespiegelt. Erst die Prüfzeile, dann das Ziel.
      * **`fxsave` deckt x87/MMX/SSE ab, nicht AVX.** SSE/SSE2 sind mit dem vorhandenen Sicherer
        erlaubt; AVX zu erlauben, ohne auf `xsave` umzustellen, wäre ein stiller
        Registerverlust zwischen zwei Threads — die schlimmste Sorte Fehler.

- [ ] **(2) Die IPC-Kosten sind NICHT GEMESSEN — und das ist die eigentliche Lücke.**
      Es gibt im ganzen Baum keine Zyklenmessung eines IPC-Umlaufs. Auf einem Mikrokern ist das
      die bestimmende Größe: jeder Dienstaufruf (Datei lesen, Speicher holen, futex wecken) ist ein
      Umlauf statt eines Syscalls. **„Hohe Leistung" ohne diese Zahl ist ein Wunsch, keine
      Aussage** — und ohne sie ist auch nicht entscheidbar, ob Punkt (3) und (5) sich lohnen.

      Die Infrastruktur steht: `crates/caprock-sched/src/cycles.rs` (B-5.1), `hal::timer::cycles()`
      mit `rdtscp`+`lfence`, und `cpuid` ist als Falle bereits bekannt (3556 statt 51 Zyklen unter
      KVM).

      **Damit die Messung ENTSCHEIDUNGSFÄHIG wird, braucht sie drei Trennungen und zwei Anker** —
      ein nackter Umlaufwert trägt wenig:

      | Trennung | warum |
      |---|---|
      | Fastpath gegen Slowpath | falls der IPC-Pfad die Unterscheidung überhaupt hat; hat er sie nicht, ist **das** der Befund |
      | mit gegen ohne Adressraumwechsel | dieselbe PD über Notification anpingen gegen Umlauf zwischen zwei PDs |
      | warm gegen kalt | der heiße Messloop misst den besten Fall; ein Umlauf nach künstlicher TLB-Verschmutzung misst den, den ein Dienst unter Last sieht — **und das ist zugleich die Vorher-Zahl für (3)**, sonst ist der PCID-Gewinn später nicht zu beziffern |

      **Die zwei Anker, ohne die die Zahl bedeutungslos ist:** seL4 veröffentlicht Fastpath-IPC in
      der Gegend weniger hundert Zyklen auf x86; ein Linux-Syscall liegt mit Mitigations bei grob
      100–200. **Unter 1000** hieße: die Dienst-Architektur trägt. **Mehrere tausend** hieße:
      (4)/(5) sind keine Optimierungen, sondern Voraussetzungen.

      **Abnahme:** eine Zeile `ipccost` mit Median und Streuung je Fall, plus eine
      Positivkontrolle, die zeigt, dass die Messung einen künstlich verlangsamten Pfad auch sieht.

- [ ] **(3) Jeder Adressraumwechsel leert den GANZEN TLB — und PCID ist KEIN Bit, das man setzt.**
      Ohne `invpcid` muss beim Entmappen über *alle* PCIDs invalidiert oder ein Generationszähler
      geführt werden. **Stale TLB-Einträge über PCID-Grenzen sind die stillste Fehlerklasse, die
      ein Kernel haben kann:** ein Cap-Entzug, dessen Mapping im TLB einer fremden PCID überlebt,
      wäre eine **Sicherheitslücke**, kein Leistungsfehler.

      Auf aarch64 gibt es das Gegenstück dagegen fast geschenkt (ASID im `TTBR`, `tlbi aside1is`).
      **Billige Reihenfolge, falls (2) zeigt, dass der TLB-Verlust dominiert:** ASIDs auf aarch64
      zuerst scharfstellen und den Gewinn dort messen, bevor die PCID-Maschinerie auf x86 entsteht.

      Zum Ist-Stand: die ASID ist heute „eine reine **Software**-Nummer des Kernels" (`mmu.rs`),
      PCID ist nicht benutzt; bei IPC-lastigen Lasten wechselt der Adressraum zweimal je Umlauf.
      Steht als Optimierung schon in [C3](#c3-cap--pd--ipc-tabellen-dynamisch) — mit (2) wird es
      messbar statt plausibel.

- [ ] **(4) Daten nicht durch IPC kopieren, sondern Speicher teilen.** Das Muster steht bereits:
      die fs-PD liest über eine **geteilte Übertragungsfläche**, nicht über Nachrichtenwörter. Der
      nächste Schritt ist, Dateiseiten direkt in die Client-PD zu mappen (`mmap` einer Datei) —
      dann kostet ein Lesezugriff **null** IPC statt einem je Puffer.

- [ ] **(5) Bündeln statt einzeln fragen.** Ein Ring aus Anforderungen und Fertigmeldungen
      zwischen zwei PDs (io_uring-Form) macht aus N Umläufen einen. Das ist genau die Form, die
      [Z17](#z17-turso-als-native-datenbank--vorgemessen-2026-08-09-nicht-begonnen) ohnehin
      erwartet — Tursos I/O-Traits sind darum herum gebaut.

- [ ] **(6) Was schon da ist und nur benannt gehört.** Budget-Spende (ADR 0019, `sc_donor`/
      `sc_donee`): der Server rechnet auf dem Konto des Clients, statt eigene Zeit zu brauchen —
      das ist die klassische Antwort auf „der Dienst wird nicht eingeplant, wenn der Client
      wartet". Der IPC-Fastpath (`switch_to`) existiert ebenfalls.

- [ ] **Reihenfolge.** (2) zuerst — ohne die Zahl ist alles Weitere geraten. Dann (1), weil es
      die größte gemessene Einzelwirkung hat und fast nichts kostet, sobald die x86-`fp`-Prüfzeile
      steht. Danach entscheidet die Messung, ob (3), (4) oder (5) zuerst lohnt.

### Z17. Turso als NATIVE Datenbank — vorgemessen 2026-08-09, nicht begonnen
**Klasse:** Anwendung · **Aufwand:** klein für den Kern der Sache, groß für das Drumherum ·
**Reihenfolge: NACH dem Rust-`std`-Port aus [Z16](#z16-quelltext-übersetzen-statt-binaries-laufen-lassen--bewertet-2026-08-09)**

- [ ] **Erst die Rollenfrage, denn daran hing eine Fehlentscheidung.** `sqlite3` steht in Z16 als
      **Messinstrument**, nicht als Nutzlast: die Abnahme „sqlite3 besteht seine Testsuite" prüft
      nicht, ob es eine Datenbank gibt, sondern ob die POSIX-Schicht `fcntl`-Sperren, `fdatasync`,
      `pread64`/`pwrite64` und die Fehlerpfade richtig macht — unter einer Last, die ein fremdes
      Team über 25 Jahre gehärtet hat.

      **Es durch eine Rust-Neuimplementierung zu ersetzen wäre zirkulär:** die Plattform würde mit
      Software geprüft, die durch den **eigenen** Rust-`std`-Port läuft — also über einen Unterbau,
      den dieses Projekt selbst geschrieben hat. Über die musl-Schicht, die der Meilenstein misst,
      bewiese das nichts. Dazu die Abhängigkeitsinversion: Turso braucht den Rust-`std`-Port, und
      der kommt in Z16 **nach** dem sqlite3-Meilenstein — die Abnahme von Stufe 5 stünde auf
      Stufe 7.

      **Z16 bleibt deshalb unverändert.** `sqlite3` in C über musl.

- [ ] **Und ein Fork wäre die schlechteste der Varianten.** Ein eigener Zweig eines Beta-Projekts
      mit Vollzeit-Team ist ein Dauer-Rebase gegen ein bewegliches Ziel. Entweder from scratch
      oder upstream unverändert — aber keine gepflegte Divergenz.

- [ ] **Wo es richtig gut ist: als natives Backend, das die POSIX-Schicht UMGEHT.** Turso
      abstrahiert seine I/O hinter Traits; io_uring ist **ein** Backend, nicht die Annahme. Ein
      Caprock-Backend bildete diese Traits direkt auf IPC und Caps ab — kein `fcntl`, keine vDSO,
      kein VFS-Umweg — und wäre ein **Upstream-Beitrag statt eines Forks**. Es wäre zugleich die
      erste Anwendung, die den eigentlichen Vorteil dieser Architektur zeigt: native Software
      **braucht** die POSIX-Umgebung nicht, nur die Kompatibilitätsschicht bekommt sie.

- [ ] **Die Vormessung, vor der ersten Zeile** (2026-08-09, `tursodatabase/turso`, flacher Klon):

      | | |
      |---|---|
      | Vorhandene Backends | **7** (`io_uring`, `unix`, `windows`, `win_iocp`, `generic`, `memory`, `vfs`) — die Abstraktion trägt nachweislich |
      | `trait File` | 17 Methoden, davon **7 ohne Vorgabe** |
      | `trait IO` | 15 Methoden, davon **2 ohne Vorgabe** |
      | Pflichtfläche insgesamt | **9 Methoden** |
      | Kleinstes vollständiges Backend (`generic.rs`) | **117 Zeilen** |
      | `supports_shared_wal_coordination` | Vorgabe **`false`** — die ganze `shared_wal_*`-Familie ist freiwillig |

      **Und die Zuordnung auf das, was Caprock schon hat:**

      | Methode | Abbildung | Stand |
      |---|---|---|
      | `pread` / `pwrite` | Blockdienst `OP_READ`/`OP_WRITE` (sektorweise, positionsbehaftet) | **steht** (A-6.1) |
      | `sync` | `OP_FLUSH` | **steht** |
      | `size` | `OP_INFO` bzw. die Dateigröße der fs-PD | **steht** |
      | `open_file` | fs-PD | halb (A-6.3 liest eine Datei; keine Pfade) |
      | `truncate`, `remove_file` | fs-PD, FAT-Ebene | fehlt, gewöhnliche Arbeit |
      | `current_time_*` | die vDSO-Seite aus **W2** | fehlt, billig |
      | `lock_file` / `unlock_file` | **hier ist das Cap-Modell BESSER als POSIX** | s. u. |

      Vier der neun bilden also direkt auf den vorhandenen Blockdienst ab.

- [ ] **Der interessante Punkt sind die Sperren, und er fällt zu unseren Gunsten aus.**
      `generic.rs` macht `lock_file` als **No-Op** (`Ok(())`) — das Backend nimmt an, es sei der
      einzige Schreiber. POSIX-`fcntl`-Sperren sind beratend, prozessweit und berüchtigt
      brüchig; auf einem Cap-System ist „exklusiver Zugriff auf diese Datei" dagegen genau das,
      was eine **Cap** ausdrückt: wer sie hat, hat ihn, und es gibt keinen zweiten Weg.

      Das ist der Satz, der diesen Strang lohnend macht — nicht „läuft auch", sondern „ist hier
      strenger als auf Linux". Er ist allerdings **unbelegt**, solange es das Backend nicht gibt,
      und gehört deshalb nicht in `docs/invariants.md`, bevor er gemessen ist.

- [ ] **Abnahme, wenn es soweit ist: nicht „läuft", sondern „besteht Tursos DST-Suite"** —
      deterministische Simulation und Fuzzing. Mit dem Vorbehalt, der dazugehört: das ist ein
      **Kompatibilitätstest gegen SQLite**, kein eigenständiges Korrektheitskorpus in der
      Größenordnung von TCL plus TH3. Als Messlatte für *das Backend* reicht es; als Messlatte für
      die Dateisemantik einer Plattform wäre es das schwächere Instrument — genau deshalb bleibt
      `sqlite3` in Z16 stehen.

- [ ] **Vorbehalt zum Reifegrad, aus zweiter Hand und nicht selbst gemessen:** die Maintainer
      bezeichnen libSQL als produktionsreif und die Rust-Engine als Beta. Vor einem Produktschritt
      wäre das nachzuprüfen; für einen Backend-Beitrag ist es unerheblich.

### Z16. Quelltext übersetzen statt Binaries laufen lassen — bewertet 2026-08-09
**Klasse:** Produktstrang · **Aufwand:** groß, aber **eine Größenordnung kleiner als
Binärkompatibilität** · **Randbedingung:** Isolation und kleine TCB bleiben.

> **ACHTUNG, 2026-08-10: die Prämisse dieses Eintrags gilt nicht mehr allein.** Der Satz weiter
> unten — „Binärkompatibilität fällt weg, damit entfällt die Syscall-Umleitung" — ist durch
> [Z27](#z27-binärkompatibilität-ist-ab-jetzt-eine-linie--die-umkehr-von-z16-entschieden-2026-08-10)
> **umgekehrt**: die Syscall-Umleitung ist gebaut (Z26/A3). Z16 ist damit **eine von zwei
> Linien**, nicht die Linie. Was hier steht, bleibt richtig für alles, was als Quelltext
> vorliegt; für Blobs (`libcuda`) gilt Z27. Wer nur diesen Eintrag liest, liest die halbe
> Entscheidung.

**Das Ziel (Simon, 2026-08-09):** Quelltext klonen, übersetzen, auf dem Mikrokern laufen lassen —
möglichst viele Linux-Bibliotheken aus C, C++, Zig, Rust, Go.

- [ ] **Was dieses Ziel STREICHT, und das ist der eigentliche Ertrag.** Binärkompatibilität fällt
      weg. Damit entfallen alle vier Kerneloperationen aus [Z14](#z14-fremde-software-ohne-gastschicht--bewertet-2026-08-09):
      keine Syscall-Umleitung, kein `ld.so` für fremde Binaries, keine ABI-Treue gegenüber Linux,
      kein Persönlichkeitsserver. Übrig bleibt **eine libc und die Dienste dahinter** — und die
      dürfen ihre eigene ABI haben, weil alles neu übersetzt wird.

      Das ist auch der Unterschied zu WSL1/gVisor, an denen die Binärkompatibilität gescheitert
      ist: dort musste Linux-**Verhalten** nachgebildet werden. Hier muss nur POSIX-**Semantik**
      stimmen, und wo eine Bibliothek darüber hinausgeht, ist der Quelltext da.

- [ ] **Gemessen am 2026-08-09, `strace -f` über fünf echte Programme:**

      | Programm | verschiedene Syscalls |
      |---|---|
      | C-Hello, statisch gelinkt | **14** |
      | `git --version` | 22 |
      | `python3 -c pass` | 34 |
      | `sqlite3` | 36 |
      | `git clone` (lokal) | 38 |
      | **Vereinigung** | **48** |

      Nebenbefund, der W2 bestätigt: **`clock_gettime` erscheint nicht** — glibc holt die Zeit aus
      der **vDSO**. Die nur-lesbar gemappte Seite mit Tickzähler ist also nicht ein Behelf, sondern
      genau das, was Linux selbst tut.

- [ ] **Die Arbeit ist nicht die libc, sondern was hinter ihr steht.** Die Vereinigung, nach
      Diensten sortiert — und daneben, was Caprock davon heute hat:

      | Dienst | im Messsatz | Stand heute |
      |---|---|---|
      | Speicher (`mmap`/`brk`/`mprotect`) | 4 | **fehlt** — Z14 Stufe 1 (Speicher-Server), TCB-neutral |
      | Dateien (`openat`/`read`/`getdents64`/…) | 10 | halb: fs-PD liest/schreibt FAT16 **eine Datei**; keine Pfade, kein `/dev`, kein `/proc` |
      | Threads (`futex`, `set_tid_address`, …) | 5 | **fehlt** — ein Thread je PD; braucht eine Kerneloperation |
      | Prozesse (`execve`, `clone`, `wait4`) | 5 | halb: `SYS_LOAD` erzeugt PDs; kein `fork` |
      | Signale | 3 | **fehlt** |
      | Zeit | (vDSO) | **fehlt** — s. W2, billig |
      | Netz | 0 im Messsatz, für eine Cloud unverzichtbar | virtio-net-Treiber steht, **kein TCP/IP** |

      **Genau eine dieser Zeilen braucht den Kernel** (Threads in einer PD). Alle anderen sind
      Dienste in PDs — die TCB bleibt bei 236 KiB.

- [ ] **Je Sprache, und die Reihenfolge folgt daraus.**

      | Sprache | Weg | Schwierigkeit |
      |---|---|---|
      | **C** | musl portieren (~90 000 Zeilen, portiert wird nur die Syscall-Schicht) | **Grundlage für alles Weitere** |
      | **Rust** | `std`-Port (eine `sys`-Schicht, wie Redox/Hermit/UEFI sie haben) | mittel — und es ist die Sprache dieses Projekts |
      | **Zig** | bringt musl selbst mit; `std.os` braucht eine Schicht | vermutlich am billigsten nach C |
      | **C++** | libc++ über musl, plus Unwinding für Ausnahmen | mittel, hängt an C |
      | **Go** | eigener Runtime, **umgeht libc grundsätzlich**, braucht Threads, Signale (Präemption), `mmap` | **eigener Strang**, teuerste Zeile — realistisch zuletzt oder über WASI |

- [ ] **Der Präzedenzfall, und er macht Mut statt Angst.** **Redox OS** ist dieselbe Form: ein
      Mikrokern in Rust, eine eigene libc (`relibc`), und darauf portierte Software in Menge — mit
      einem kleinen Team. Das ist die belastbare Referenz für diesen Weg, nicht gVisor oder WSL1;
      die scheiterten an einer Anforderung, die dieses Ziel gar nicht stellt.

- [ ] **Die Falle, die dieser Strang mitbringt: „übersetzt" ist nicht „funktioniert".** Eine
      Bibliothek, die durchbaut, kann in jedem zweiten Aufruf falsch liegen. Die Abnahme ist
      **ihre eigene Testsuite**, nicht der Compiler — und für jede portierte Bibliothek gehört die
      Zahl ins Protokoll (`sqlite3` hat rund 700 Tests, `zlib` eine Handvoll, `openssl` Tausende).
      Sonst entsteht dieselbe Sorte Grün wie bei einem Prüfer, der nicht fehlschlagen kann.

- [ ] **Eine Annahme, die ausgesprochen gehört: übersetzt wird auf einem Linux-Rechner
      (cross), nicht auf dem Kern.** Selbst-Hosting („klonen und kompilieren **auf** Caprock")
      ist eine andere Größenordnung: es braucht `fork`/`exec`, ein volles Dateisystem, viel
      Speicher und einen Compiler als portierte Anwendung. Das ist die Kür, nicht der Einstieg —
      und wenn es doch das Ziel ist, ändert es die Reihenfolge unten.

- [ ] **Gemessen am 2026-08-09 an TESTLASTEN statt an Startaufrufen** — und das verschiebt die
      Prioritäten. `strace -f` über zlib (`example`), sqlite3 (echte Platte, WAL, 20 000 Zeilen,
      `VACUUM`, `integrity_check`), einen 4-Thread-Sperrtest und Git:

      | Last | Syscalls | futex | fork/`clone` | Signal-**Zustellung** |
      |---|---|---|---|---|
      | zlib `example` | 19 | — | — | — |
      | sqlite3 (Platte, WAL, VACUUM) | 33 | — | — | — |
      | `git status` | — | 1 | — | — |
      | 4 Threads, 800 000 Sperrzyklen | — | **5 863** | 4× `clone3` | — |
      | `git clone` | 38 | 903 | 29× `clone3`, 4× `wait4`, 6× `execve` | — |
      | `python3 -c pass` | 34 | 1 | — | — (66× `rt_sigaction`, also nur INSTALLIEREN) |

      **Drei Befunde, die den Plan ändern:**

      1. **Der schwere Teil ist die Dateisemantik, nicht die Threads.** sqlite allein verlangt
         `fcntl` (Sperren), `fdatasync`, `pread64`/`pwrite64`, `ftruncate`, `mremap`, `getcwd`,
         `unlink` — und braucht dabei **weder futex noch fork noch Signalzustellung**.
      2. **Signal-ZUSTELLUNG kommt in keiner gemessenen Last vor.** Nur `rt_sigaction`/
         `rt_sigprocmask`, also das Eintragen von Handlern. `rt_sigreturn` erschien ausschließlich
         über die `sh`-Hülle, nicht aus den Programmen.
      3. **0,7 %.** 800 000 Sperrzyklen ergaben 5 863 futex-Aufrufe — der unkontendierte Pfad
         betritt den Kernel nie. Was ein Userspace-futex kostet, betrifft also 0,7 % der
         Sperroperationen, nicht alle.

- [ ] **DREI Entscheidungen, die VOR den musl-Port gehören — nicht als Überraschung in einen
      späteren Meilenstein.** (Und eine Berichtigung: „genau eine Zeile braucht den Kernel" war zu
      bequem. Es ist **eine sicher**, eine **zweite bedingt**, und eine dritte ist vermeidbar —
      aber das ist eine Entscheidung, keine Tatsache.)

      **(1) futex: Dienst über die VORHANDENE Blockierprimitive, kein neuer Syscall.**
      Die harte Frage ist „worauf blockiert der Aufrufer?", und sie hat hier eine Antwort, die
      nichts kostet: **auf seiner eigenen Notification, über das vorhandene `SYS_WAIT`.** Der
      Kernel hat die Blockierprimitive bereits; der Dienst führt nur die Warteschlange und
      signalisiert gezielt. `Notification::wait` fasst genau **einen** Wartenden (seit D11
      ausdrücklich, mit `ERR_EP_FULL` statt stillem Überschreiben) — das passt, wenn jeder Thread
      seine eigene hat.

      **Kosten, gemessen statt geschätzt:** 2 IPC-Umläufe je *kontendiertem* Warten statt eines
      Syscalls — auf 0,7 % der Sperroperationen. **TCB-Wachstum: null.**

      **Was diese Entscheidung umstoßen würde** (und das gehört dazu, sonst ist sie unwiderlegbar):
      eine Last, bei der die Contention-Rate so hoch ist, dass der Umlauf dominiert. **Und die
      bekannte Alternative gehört mitgenannt, damit die Entscheidung nicht alternativlos dasteht:**
      mit einem Anforderungsring ([Z18](#z18-hohe-leistung-für-übersetzten-fremdcode--vermessen-2026-08-09) (5))
      plus Notification wird das Wecken zum **Eintrag** statt zum Umlauf — die Rechnung „2 Umläufe
      je kontendiertem Warten" gilt dann nicht mehr, und ein Kernel-futex wäre auch dann nicht die
      einzige Antwort. Die Messung
      dafür steht fest: dieselbe Sperrschleife, Rate der futex-Aufrufe je Sperroperation, und die
      Wandzeit gegen eine Fassung mit Kernel-futex. Erst dann ist ein Kernel-futex ein Befund und
      keine Vorliebe.

      **(2) Signale: synchron zuerst, asynchron ist ein KERNEL-Upcall.**
      Stufe A — Handler werden in der libc geführt und an **Syscall-Grenzen** ausgeliefert
      (`EINTR`, `SIGPIPE` als `EPIPE`, `SIGCHLD` beim `wait`). Das deckt **jede gemessene Last**
      ab und kostet den Kernel nichts.
      Stufe B — echte asynchrone Zustellung (SIGALRM in eine Rechenschleife, SIGINT vom Terminal)
      verlangt, einen laufenden Thread zu unterbrechen und ihm einen Handler-Frame aufzubauen.
      **Das kann kein Dienst per IPC**, das ist ein Upcall und damit eine TCB-Änderung. Sie wird
      gebaut, wenn eine Testsuite sie nachweislich verlangt — und die Zeile im Protokoll ist dann
      `rt_sigreturn` aus dem Programm selbst, nicht aus einer Hülle.

      **(3) fork: `posix_spawn` zuerst, echtes `fork()` nur auf Nachweis.**
      Gemessen forkt `git clone` (29× `clone3`, 6× `execve`, 4× `wait4`) — aber als
      **fork+exec**, und genau das deckt `posix_spawn` ab. Es bildet sauber auf den vorhandenen
      Weg ab: PD anlegen, laden, starten (`SYS_LOAD`). Echtes COW-`fork()` bräuchte
      Fault-Umleitung und Mappen in eine fremde PD — beides TCB — und wird erst gebaut, wenn eine
      Testsuite es verlangt (ein `fork` **ohne** folgendes `exec`, das den Kindzustand benutzt).

- [ ] **Reihenfolge, jede Stufe mit einer eigenen Abnahme.**
      1. **Speicher-Server** (Z14 Stufe 1) — ohne `mmap`/`brk` läuft keine libc. TCB-neutral.
      2. **Uhr als vDSO-Seite** (W2) — billig, und ohne sie ist fast jede Bibliothek unbrauchbar.
      3. **musl-Port**, erst gegen ein Ziel ohne Dateien: `write`/`exit`/`brk`/`mmap`.
         **Abnahme:** ein C-Hello, aus Quelltext gebaut, läuft — und seine 14 Syscalls sind
         einzeln belegt, nicht bloß „es lief".
      4. **fd-Tabelle + VFS-Dienst** über die vorhandene fs-PD; Pfade, `getdents64`.
         **Abnahme:** `zlib` baut und besteht seine Testsuite (19 Syscalls, gemessen).
      5. **Der eigentliche Brocken: Dateisemantik.** `fcntl`-Sperren, `fdatasync`,
         `pread64`/`pwrite64`, `ftruncate`, `unlink`, `getcwd`. **TCB-neutral**, aber es ist die
         größte Einzelstufe — gemessen an sqlite, das genau das braucht und sonst nichts.
         **Abnahme:** `sqlite3` besteht seine Testsuite **ohne** Threads, ohne fork, ohne Signale.
      6. **Threads** (die eine Kerneloperation) + `futex` als Dienst über Notifications.
         **Abnahme:** der 4-Thread-Sperrtest liefert 800 000 als Ergebnis, und die gemessene
         Rate der Blockierungen steht im Protokoll.
      7. **Rust-`std`-Port**, danach C++/Zig.
      8. **TCP/IP-Dienst** über den vorhandenen virtio-net-Treiber — ab hier ist es eine Cloud.
      9. Go: eigener Strang, eigene Entscheidung.

### Z15. WASM in einer PD — der Plan (2026-08-09)
**Klasse:** neues Subsystem · **Aufwand:** gestuft, W1 ist klein · **TCB-Wirkung:** null bis W2

Alle Zahlen aus [Z14](#z14-fremde-software-ohne-gastschicht--bewertet-2026-08-09). Jede Stufe hat
eine Abnahme, die **fehlschlagen kann**; eine Stufe ohne Gegenprobe gilt nicht als fertig.

- [~] **W1 — angefangen 2026-08-09, Stand: die PD läuft, der MELDEWEG nicht.**

      **Belegt (nicht vermutet):** `programs/userland/wasmhost` baut gegen `wasmi 0.31` für
      `x86_64-caprock-user` (429 KB ELF, drei PT_LOAD: 257 KB Code, 30 KB rodata, **2 MiB reines
      BSS** als Heap-Arena). Der Lader nimmt es an — das BSS-Segment mit `FileSiz 0` ist genau der
      Fall, den `copy_segment_at` seit dem Farbumbau behandelt. **Die PD läuft**, gemessen mit
      einer cap-freien Sonde (ein Fault an `0xDEAD_0000` erscheint im Protokoll), sowohl mit
      64 KiB als auch mit 2 MiB Arena.

      **Offen:** keins der vier Badges kommt an. Der Meldeweg ist `ccopy` (eigen gebadgte Kopie)
      + `signal`, dasselbe Muster wie `init` bei A-3.1.

      Ausgeschlossen durch Nachsehen im Handler, nicht durch Probieren:
      * **Rechte-Verstärkung** — `ccopy` *schneidet* (`rights_from_bits(mask).intersect(have)`),
        eine zu große Maske ist also harmlos. (Meine erste „Korrektur" von `7` auf `0b010` beruhte
        auf der falschen Annahme und war selbst ein Fehler.)
      * **Slot-Bereich** — `NCAPS = 16`, die Zielslots 6..9 sind gültig.
      * **Belegter Zielslot** — der Lader endowt nach 0..5.
      * **Ladefehler / Segmentgröße** — s. o., die Sonde läuft.

      **Die nächste Spur, und sie ist konkret:** der Handler reicht einen bei
      `install_cap_checked` **abgelehnten** Kopie-Cap heraus, um ihn zu löschen — die
      Domänen-Policy urteilt also über die Kopie. `init` ist TrustedSas, `wasmhost` ist UserLand.
      Zu prüfen ist, ob eine UserLand-PD überhaupt eine selbst gemintete Notification-Kopie
      installieren darf. Zweite Spur: welches Notification-**Objekt** der Kernel liest —
      `CLIENT_NTFN` wird von jeder nicht-Root-, nicht-Treiber-PD überschrieben, und in dieser
      Startmenge sind das `hello`, `fs` und `wasmhost`.

      **Was diese Suche schon gelehrt hat:** ich habe zweimal zwei Dinge gleichzeitig geändert
      (Heapgröße und Meldeweg; Rechte und Quellslot) und mir damit zwei Läufe wertlos gemacht.
      Die cap-freie Sonde war der erste Schritt, der etwas entschieden hat — weil sie **eine**
      Frage stellt und keine Cap braucht.

- [ ] **W1 (Rest) — die Engine läuft in einer PD, ohne eine einzige neue Kernelzeile.** Ein neues Programm
      `programs/userland/wasmhost` linkt `wasmi` gegen `libcaprock`, nimmt ein `.wasm` aus dem
      Boot-Archiv (dritter Weg neben ELF und Manifest), instanziiert es und meldet das Ergebnis
      über seinen Endpoint.

      **Der Heap ist ein Bump-Allokator über die private Region** — das ist genau das Modell, das
      eine PD heute hat (fester Speicher, kein `brk`), und es ist gemessen ausreichend.

      **Abnahme:** `wasm : ALL PASS` mit dem *gerechneten* Ergebnis des Gastmoduls, nicht mit
      „lief durch". Dazu zwei Negativfälle, denn sonst belegt die Zeile nur, dass eine Engine
      startet: ein **mutiertes** Modul (ein Byte im Code-Abschnitt) muss abgewiesen werden, und
      ein Modul, das über seinen Linearspeicher hinausgreift, muss einen WASM-Trap auslösen —
      **ohne** dass die PD faultet. Der zweite Fall ist die eigentliche Aussage: die Sandbox hält
      *innerhalb* der PD, und die PD-Isolation ist die zweite Linie.

- [ ] **W2 — der WASI-Kern, und ein Blocker, der klein ist.** `proc_exit`, `fd_write` (Konsole),
      `random_get`, `clock_time_get`. Die ersten drei gehen über vorhandene Dienste bzw. Caps.

      **`clock_time_get` hat heute keine Grundlage: es gibt keine Zeit-ABI** (0 Treffer für
      `CLOCK`/`GETTIME`). Der billigste Weg, der die TCB fast nicht anfasst, ist **kein Syscall**,
      sondern eine **nur-lesbar in jede PD gemappte Seite mit Tickzähler und Frequenz** — der
      Kernel schreibt sie ohnehin, der Leser braucht keinen Übergang. Das ist der vDSO-Gedanke,
      und er kostet ein Mapping plus einen Schreibzugriff im Timer-Pfad.

      **Abnahme:** die Uhr muss *monoton* und *plausibel* sein — zwei Lesungen mit einer
      bekannten Wartezeit dazwischen, und die Differenz liegt im erwarteten Band. Ein Zähler, der
      steht, ist von einem, der läuft, sonst nicht zu unterscheiden.

- [ ] **W3 — Speicher-Server (Z14 Stufe 1) → `memory.grow` und mehr als ein Gast.** Erst hier
      wird die Sache mehrmandantenfähig. Abnahme wie in Z14 beschrieben, plus: zwei WASM-PDs
      gleichzeitig, und die eine sieht den Linearspeicher der anderen **nicht** (Positivkontrolle
      über denselben Server, nur eine Adresse wandert — dieselbe Form wie A-5.4).

- [ ] **W4 — Dateien über die fs-PD** (A-6.3): `path_open`, `fd_read`, `fd_seek`, `fd_close`.
      Die PD fährt kein Gerät, sie ruft den Blockdienst — der Weg steht seit A-6.3. Abnahme: ein
      Gast liest eine Datei, deren Inhalt ein **unabhängiger** Leser (`tools/checkfat.py`)
      bestätigt.

- [ ] **W5 — ein Gast je PD, und das bleibt so.** Mehrere Gäste in einer Engine wären billiger und
      wären die Aufgabe der Isolationsaussage: die Trennung zweier Mandanten läge dann in der
      Engine statt im Kern. Das ist genau die Schicht, die dieses Projekt nicht haben will.
      **Gehört als Festlegung in `docs/invariants.md`**, nicht in einen Kommentar.

- [ ] **Was NICHT geplant ist und warum.** Ein JIT (Cranelift): er braucht ausführbaren, zur
      Laufzeit beschriebenen Speicher — also W^X aufzuweichen oder eine `mprotect`-ähnliche
      Operation. Beides ist teuer an der Stelle, an der dieses Projekt am wenigsten nachgeben
      will. Der Interpreter kostet Faktor 5–20 an Rechenzeit; das ist der Preis, und er ist
      messbar statt behauptet.

### Z13. Das Blockdienst-Protokoll steht DREIMAL (gemessen 2026-08-07)
**Klasse:** Drift · **Aufwand:** klein

Bei der Untersuchung zu Z12 gemessen:

| Kopie | Opcodes | Statuscodes |
|---|---|---|
| `programs/hardware/virtio-blk` (Server) | 6 | 5 |
| `programs/trusted/fs` (Client) | 5 | 1 |
| `kernel/src/arch/x86_64/bringup.rs` (Client) | 7 | 0 |

Drei Herkünfte für **eine** Aussage, gehalten allein von Disziplin — und sie **weichen bereits
ab**: `OP_STOP` kennt nur der Server, die Statuscodes kennen die Clients fast gar nicht (`fs`
prüft nur gegen `ST_OK`, der Kernel gegen keinen). Das ist heute kein Fehler, weil die Zahlen
übereinstimmen; nichts erzwingt es.

Dasselbe beim **Layout der Übertragungsfläche**: `OFF_ERGEBNIS = 4096` (fs), `OFF_SERVED = 0x600`
(Server), `OFF_DATA` aus `caprock_virtio::blk` — das Dienstprotokoll leiht sich sein Layout aus
der Treiber-Crate.

**Das ist der billigere und näherliegende Schnitt als Z12**, weil es unser eigenes Format ist:
eine Quelle, drei Erzeugnisse, und der vorhandene `iface_version`-Gate aus A-4.4 könnte seine
Version **aus dem Hash der Beschreibung** beziehen — dann kann sich das Drahtformat nicht ändern,
ohne dass ein Hot-Reload abgewiesen wird. Kein neuer Mechanismus, nur eine Kopplung, die heute
fehlt.

## Z5. Tickless für Rechenkerne
**Klasse:** Scheduler/Timer · **Aufwand:** mittel

- [ ] Der 100-Hz-Tick läuft auf **jedem** Kern. Für einen Kern, auf dem genau ein rechenbereiter
      Thread liegt, ist jeder Tick reiner Verlust — und in HPC die klassische „OS-Noise"-Quelle,
      die Kollektivoperationen über tausende Ränge hinweg verschleppt. Nötig: Timer nur armieren,
      wenn es etwas zu verdrängen gibt (>1 lauffähiger Thread oder ein ablaufendes Budget), sonst
      abschalten. Zusammen mit [Z2](#z-zielarchitektur-stand-2026-07-29--woran-alles-andere-zu-messen-ist)
      ergibt das: Abrechnung per Zyklenstempel, Verdrängung per Bedarf — die beiden werden
      entkoppelt, und genau das ist der Punkt.
- [ ] Kern-Isolierung als Politik: Rechenkerne bekommen keine IPIs, keine Balancierung
      (`balance_once` ist per Default aus — hier ist das zufällig schon richtig), keine
      Interrupt-Zustellung außer ihren eigenen.

### Z6. SMT — die Lücke, die Cache-Coloring NICHT schließt
**Klasse:** Seitenkanal · **Aufwand:** Scheduler-Politik, klein bis mittel

- [ ] [A1](#a1-cache--timing-seitenkanäle-zwischen-pds) partitioniert den **LLC**. Zwei
      Hyperthreads auf demselben physischen Kern teilen sich aber L1, L2, TLB, Store-Buffer und
      Branch-Predictor — dagegen hilft keine Farbe, und zwar prinzipiell nicht. Solange SMT an ist
      und der Scheduler Geschwister-Threads beliebig belegt, ist die A1-Zusicherung auf einer
      SMT-Maschine deutlich schwächer, als sie klingt. Zwei gangbare Wege: SMT abschalten (kostet
      Durchsatz, ist ehrlich) oder **Gang-Scheduling der Geschwister** — ein physischer Kern
      gehört zu jedem Zeitpunkt genau einem Tenant. Der zweite Weg braucht die
      Topologie (`CPUID.1F`/`0B` bzw. MPIDR) im Scheduler; die liest heute niemand.
- [ ] Solange das offen ist, gehört es in die Zusicherung: `docs/invariants.md` muss sagen, dass
      A1 den LLC trennt **und die kernlokalen Strukturen nicht**.

### Z7. Attestierung und messbarer Boot
**Klasse:** Vertrauen · **Aufwand:** mittel-groß

- [ ] Das Zertifikats-Gate (ADR 0014) sichert, welche **Binaries** starten dürfen. Ein Tenant
      will aber aus der Ferne nachweisen können, **was auf der Maschine läuft**, bevor er seinen
      Zustand dorthin gibt — Measured Boot, Messkette bis in den Kernel, Signatur über die
      Messung. Vorbedingung von [Z4e](#z4-checkpointrestore-eines-threads); ohne das ist
      Thread-Migration ein Vertrauensvorschuss an eine unbekannte Maschine.

### Z8. NUMA
**Klasse:** Speicher/Skalierung · **Aufwand:** mittel, greift in den Allokator

- [ ] Der `PhysAllocator` hat **eine flache Freiliste ohne Knotenbegriff**. Beim Zielbild
      Dual-EPYC ist das kein Detail: Speicher am falschen Knoten kostet grob Faktor zwei an
      Latenz, und bei CXL wird es schlimmer. Nötig: Knoten aus ACPI SRAT/SLIT lesen, Freilisten je
      Knoten, PD-Zuteilung knotenlokal, und der Scheduler darf einen Thread nicht auf einen Kern
      legen, dessen Knoten seinen Speicher nicht hält.
- [ ] **Verzahnt mit A1**: Farbe *und* Knoten müssen **gemeinsam** vergeben werden. Zwei
      unabhängig entwickelte Politiken kämpfen sonst gegeneinander — der Farbstreifen erzwingt
      eine Physadresse, der Knoten eine andere, und wer zuerst zuteilt, gewinnt. Das gehört in
      **eine** Entscheidung, nicht in zwei Schichten.

### Z9. Fehlerdomäne
**Klasse:** Betrieb · **Aufwand:** Entwurfsentscheidung

- [~] **Die Festlegung steht (B-6.2, 2026-08-02) — und die Prämisse dieses Eintrags war falsch.**
      „Ein Kernel-Panic reißt den Knoten mit" stimmt **nicht**, und das ist die schlechtere
      Nachricht: der Panic-Pfad hält den Kern **ohne IRQ-Maskierung**, der nächste Timer-Tick
      holt ihn zurück in den Scheduler. Gemessen lief der Knoten **61 s weiter** und meldete
      `ipc`, `ring3`, `iommu`, `dmatok` als ALL PASS — mit einer nachweislich verletzten
      Invariante. Vier verschiedene Ausgänge je nachdem **wo** der Panic auftritt, darunter ein
      stiller Totalausfall (unter der MEM-Sperre) und ein Zustand, der von außen nicht von einem
      Deadlock zu unterscheiden ist.

      Festlegung in `docs/fehlerdomaene.md` (betreiberseitig, mit Messtabelle und VM-Vergleich),
      normativ als `docs/invariants.md` §14.

      **Offen ist die Entscheidung, nicht die Beschreibung:** die Festlegung ist heute
      *behauptet*, nicht *hergestellt*. Drei Maßnahmen von zusammen etwa einem halben Tag machen
      sie wahr — IRQs im Panic-Pfad maskieren, ein Rekursionswächter im Panic-Handler,
      `panic → system_off` (beide Abschaltwege existieren und werden aus `panic.rs` **nie**
      gerufen). Das ist ein Abwägen zwischen Verfügbarkeit und Ehrlichkeit und gehört Simon.

### Z10. I/O überhaupt
**Klasse:** Grundlage · **Aufwand:** groß

- [ ] Kein Netzstack, kein Dateisystem. Eine Cloud ohne Netz und Speicher ist keine.
      *(Zwei Schritte erledigt, 2026-08-01: **A-5.2** brachte Treiberlogik für Blockgerät und
      Netzkarte, kernfrei in `crates/caprock-virtio`; **A-5.1** brachte die erste Treiber-PD —
      `programs/hardware/virtio-blk` liest einen Sektor als geladenes Userland-Programm, mit
      eigener Gerätezuteilung aus dem Manifest.)*
      Seit 2026-08-02 ist der Treiber ein **Dienst**: er bedient Anfragen über seinen Kanal und
      ist im Betrieb austauschbar (A-5.1 abgeschlossen).
      Seit 2026-08-02 steht darüber ein **Speicherstapel, vollständig außerhalb des Kerns**:
      Blockdienst (A-6.1), GPT (A-6.2) und ein lesendes **FAT16-Dateisystem als eigene PD**
      (A-6.3). Eine Datei wird über GPT → FAT16 → Blockdienst → Treiber gelesen.
      Seit A-6.4 **schreibt** sie auch (zweiter Cluster, beide FAT-Kopien, Flush, jedes Byte
      zurückgelesen; unabhängig am Abbild nachgeprüft).
      **Was weiterhin fehlt:** Dateien anlegen/löschen, Unterverzeichnisse, lange Namen — und ein
      **Netzstack**. Jeder Treiber **pollt**: eine IRQ-Cap ist nicht erteilbar, solange es keine
      IRTE-Vergabe gibt (B-3).
- [ ] Für durchgereichte Geräte ist **Interrupt Remapping** ([E](#e-dma-härtung--rest), Schritt 4)
      keine Kür: ohne IR kann ein Gerät beliebige Interrupt-Nachrichten erzeugen. Das ist der
      Standardausbruch aus einer Geräte-Zuteilung und muss vor dem ersten Tenant-Gerät stehen.

---

## A. Offen aus dem Sicherheits-Review (ext-29)

### A1. Cache-/Timing-Seitenkanäle zwischen PDs
**Klasse:** Seitenkanal · **Aufwand:** Architekturänderung, kein Patch

`[~]` **Stufe 1 steht und ist auf BEIDEN Architekturen gemessen** (s. [done.md](done.md)):
Cache-Geometrie wird aus der HW gelesen, der Allokator vergibt farbrein, und zwei so erzeugte PDs
teilen sich nachweislich keine Cache-Farbe — Region, Kernel-Stack und Seitentabellen. x86 seit
2026-08-01 (seit 2026-08-02 mit der ECHTEN Geometrie der Maschine, 512 Farben — vorher meldete
QEMU 256 erfundene), aarch64 seit 2026-08-02 (16 Farben — die Aufteilung, an der die
`MASK_BITS`-Verwechslung hing). Offen bleibt das Folgende.

- [ ] **Way-Partitionierung (Intel CAT / AMD L3-QoS / ARM MPAM) — bewertet 2026-08-07, nicht
      gebaut, und der Grund ist eine Messung.** Auf dem Entwicklungsrechner gibt es sie nicht:
      13th-Gen-Core-i7, keine `cat_l3`/`rdt_a`-Flag in `/proc/cpuinfo`, kein `resctrl`. Sie ließe
      sich hier also bauen, aber **nicht prüfen** — und eine Zusicherung ohne Messung ist in diesem
      Projekt kein Fortschritt, sondern eine Zeile in `docs/invariants.md`, die niemand einlösen
      kann. (Auf dem Produktziel Dual-EPYC gibt es L3-CAT; dort wäre es messbar.)

      **Der Entwurfspunkt, der davon unabhängig gilt:** Färbung und Way-Partitionierung lösen
      dasselbe Problem auf verschiedenen Ebenen, und **beide gleichzeitig ohne gemeinsame Politik
      ist schlechter als eine**. Die Farbe schränkt ein, welche *Sets* eine PD belegen kann; CAT
      schränkt ein, welche *Ways* sie belegen darf. Zwei unabhängig entwickelte Zuteiler kämpfen
      gegeneinander — dieselbe Falle wie Farbe gegen NUMA ([Z8](#z8-numa)), wo sie ausdrücklich
      benannt ist. Wer CAT einführt, muss zuerst entscheiden, ob es die Färbung **ersetzt**
      (dann fällt `region_bytes()` weg und PDs dürfen wieder große Blöcke nehmen) oder **ergänzt**.

      Der Vorteil von CAT wäre genau das, was der Färbung fehlt: keine Bindung an Physadressen,
      also **auch als Gast wirksam** — s. §12, wo gemessen ist, dass Färbung unter KVM gar nicht
      trägt (`disjunkt=234` gegen `gleichfarbig=210`).

- [ ] **Way-Partitionierung (Intel CAT / ARM MPAM) nicht betrachtet.** Sie träfe dieselbe
      Eigenschaft über die Hardware statt über den Allokator, käme ohne kleinere Regionen und ohne
      seitenweises Mapping aus — und ließe sich mit der Färbung kombinieren. QEMU emuliert CAT
      nicht, also wäre sie hier **nicht prüfbar**; das ist ein Grund, sie zurückzustellen, kein
      Grund, sie zu vergessen.
- [ ] **A1 trennt den LLC und sonst nichts.** Kernlokale Strukturen (L1, L2, TLB, Store-Buffer,
      Branch-Predictor) bleiben geteilt, sobald zwei Tenants auf Geschwister-Hyperthreads liegen —
      s. [Z6](#z6-smt--die-lücke-die-cache-coloring-nicht-schliesst). Und die Farbvergabe muss mit
      der NUMA-Zuteilung **gemeinsam** entschieden werden, nicht nacheinander, s.
      [Z8](#z8-numa).

- [~] **Der Wirkungsnachweis ist gebaut, aber nicht erbracht** (B-4.5, s.
      [todo-B-verlaesslichkeit.md](todo-B-verlaesslichkeit.md)). Der Prime+Probe läuft
      arch-neutral bei **jedem** Start und wird von beiden Suiten gelesen; Aufbau,
      Positivkontrolle, Farbwahl und Bilanz sind auch dort geprüft, wo kein Urteil möglich ist —
      ein Prüfpfad, der zum ersten Mal am Zieltag läuft, ist am Zieltag kaputt.

      **Die alte Annahme „erst auf Blech ODER unter KVM" war falsch:** unter einem Hypervisor ist
      die Frage nicht schwer zu messen, sondern **prinzipiell nicht entscheidbar** — ein Gast färbt
      gastphysische Adressen, und die zweite Übersetzungsstufe schreibt die Farbbits um. Gemessen
      (Gast): `ungestoert=41 disjunkt=234 gleichfarbig=210` — die Positivkontrolle trägt, der
      disjunkte Farbsatz schützt **nicht**.

      **Offen:** ein Lauf auf echter Hardware — und die Maschine muss eine Bedingung erfüllen:
      `Größe der nicht partitionierten Cache-Ebene < LLC / PARTITIONS`. Sonst gibt es **keine
      gültige Opfergröße**, und das ist eine Eigenschaft der Maschine, nicht des Tests.

### A2. Spectre-v2 auf HW ohne FEAT_CSV2
**Klasse:** Seitenkanal · **Aufwand:** klein, aber HW-abhängig

`array_index_nospec` (v1) und die Barriere beim VSpace-Wechsel sind da. Für v2 auf HW, die **nicht**
`CSV2 >= 1` meldet, wäre Branch-Predictor-Invalidierung beim Adressraumwechsel nötig —
`SMCCC_ARCH_WORKAROUND_1` per SMC. QEMU `virt` bietet den Firmware-Call nicht an → erst auf echter
HW (STM32MP257F-DK) umsetzbar/testbar. Boot-Report `spec :` zeigt bereits, was die HW garantiert.

### A3. Globale Cap-Tabelle bleibt geteilte Ressource fester Größe
**Klasse:** Fairness/Skalierung · **Aufwand:** mittel-groß

`CAP_BUDGET_PER_PD` begrenzt jetzt den **Schaden**, ersetzt aber nicht das seL4-Modell (jede PD
bekommt ihren CNode aus dem **eigenen** Untyped-Budget). Solange die Tabelle geteilt ist, bleibt
Kapazität eine globale Größe.

**Gemessen dazu (2026-08-10, s. [C7](#c7-die-kapazitätskurve--wo-es-wirklich-bricht-gemessen-2026-08-10)):
die geteilte Cap-Tabelle ist heute NICHT der Engpass — bei 9984 gleichzeitigen Prozessen stehen
11 von 80 256 Slots.** Das entkräftet die Dringlichkeit, nicht das Argument: der Höchststand hängt
daran, wie viele Caps eine PD **hält**, und die Kurven-PDs halten keine. Eine Zahl, die aus einem
Lastfall ohne Caps stammt, sagt über einen Lastfall mit Caps nichts — dieselbe Unterscheidung wie
`rx_used` gegen „Daten sind angekommen".
**Nebenbedingung beim Vergrößern: erledigt** (A-3.3). `ReplyFinal`/`Finalized` hielt sein
`[(u32,u64); NOBJECTS]`-Array **auf dem Kernelstack**, und nicht als einziges: `dma_finalize` hielt
eine Kopie derselben Regionen, jede `finalize`-Implementierung der Enforcer nochmals ein
`[usize; NOBJECTS]`. Gemessen am x86-Release war der Rahmen von `cap_delete`/`cap_revoke` deshalb
**6168 Byte** — gross genug, dass der Compiler eine Stack-Probe einbaute. Jetzt leiht die Struktur
ihren Speicher vom Aufrufer (ein statischer Puffer im Kernel), Rahmen 72 bzw. 88 Byte. `NOBJECTS`
hochzuziehen koppelt damit nicht mehr an die Stackgrösse. Siehe auch
[C3](#c3-cap--pd--ipc-tabellen-dynamisch).

*(Die frühere Angabe „aktuell 2 KiB" war veraltet: Kernel-Threads haben 64 KiB, EL0-Threads einen
16-KiB-EL1-Stack. Der Punkt stand trotzdem — 6,2 KiB waren 38 % des kleineren der beiden.)*

### A4. Kein Syscall zum Löschen eigener Caps
**Klasse:** ABI-Lücke · **Aufwand:** klein-mittel

Die ABI kennt kein `CDELETE`/`CMOVE`/`CCOPY` — Cap-Slots einer PD räumt nur der Kernel (Teardown,
PDCTL). Folge: ein langlebiger Dienst, der dynamisch Caps empfängt (IPC-Grant), läuft gegen
`CAP_BUDGET_PER_PD` und kann sich **nicht selbst befreien**. Nötig: mindestens `SYS_CDELETE` (eigener
Slot, cap-gegatet auf den eigenen Cspace), sinnvollerweise auch ein wählbarer Empfangs-Slot für
Grants statt des festen `GRANT_RECV_SLOT`.

---

## B. Thread-Migration — Rest

**Ziel:** Threads sind nicht mehr fest an den Kern gebunden, auf dem sie erzeugt wurden.
**Blocker war:** `ThreadId.slot` kodierte den Kern (`slot / PER_CORE`) — die Identität eines Threads
hing an seiner Kern-Affinität.

- [~] **B4** Policy vorhanden (`balance_once`, im Tick-Pfad hinter einem Intervall), aber per
      Default **AUS** (`system::set_balancing`). **Offen:** standardmäßig einschalten. Blocker sind
      nicht der Mechanismus, sondern die Demo-/Testthreads dieses Images, die feste Affinität
      voraussetzen (Cross-Core-IPC-Test, Budget-Donation, Platzierungs-Telemetrie) — die müssten
      erst affinitätsunabhängig formuliert werden.

**Ebenfalls offen:** Migration eines Threads mit aktiver **Budget-Donation** (die Donation-Links
sind lokale Slots — heute wird die Migration in dem Fall schlicht verweigert, bis der Server
geantwortet hat). Für eine kernübergreifende Donation müssten die Links globale `gid`s werden.

**Bewusst NICHT migriert:** der laufende Thread eines Kerns (nur bereite/blockierte) — sonst müsste
der Trap-Frame kernübergreifend übernommen werden.

---

## C. Flexible Kapazitäten (Zielbild: 256 Kerne, viele tausend Prozesse)

**Zielbild:** Dual-EPYC-Klasse — **256 Kerne**, **viele tausend** Prozesse, effizient.
Die Thread-/Scheduler-Seite ist umgestellt (Kapazität kommt beim Boot aus dem RAM, heiße Pfade
O(1); belegt durch den `scale`-Test mit 1024 gleichzeitigen Threads). Offen bleiben die Cap-/IPC-
Tabellen, die GIC-Skalierung und der x86-Port.

- [ ] **C3** <a id="c3-cap--pd--ipc-tabellen-dynamisch"></a>Cap-/PD-/Endpoint-/Notification-Tabellen
      dynamisch (hing an [A3](#a3-globale-cap-tabelle-bleibt-geteilte-ressource-fester-größe):
      `ReplyFinal` zuerst vom Stack lösen — **das ist seit A-3.3 erledigt**, der Weg ist frei).

### C4. Effizienz bei tausenden Threads (lineare Scans beseitigen)

Kapazität allein reicht nicht — mehrere Pfade sind **O(n)** in der Tabellengröße und werden bei
tausenden Threads zum Engpass:

**Seit 2026-08-10 gibt es dafür eine gemessene Grundlinie statt einer Liste** — die Zeile
`vorrat` druckt Füllstände **und** Iterationszahlen in jedem Lauf (`tools/boot-x86-log.sh`).
Gezählt, nicht gestoppt: eine Iterationszahl ist eine Eigenschaft des Programms, eine
Zeitmessung nicht (D10).

- [ ] `CapSpace::{free_slot_index, alloc_object}` — lineare Scans → Freilisten.
      **Grundlinie:** in der Kapazitätskurve nie der Engpass — bei 9984 Prozessen stehen die
      Cap-Slots bei **11/80256**. Die Tabelle ist für diesen Lastfall um drei Grössenordnungen
      überdimensioniert; der Scan ist trotzdem linear und wird es unter cap-hungrigen PDs.

- [ ] `PdTable::create` — linearer Scan → Freiliste. **Grundlinie: 16 Iterationen** im normalen
      Suitenlauf; in der Kurve wächst er quadratisch mit der PD-Zahl (jede Anlage scannt von
      vorn bis zum ersten freien Slot).

- [ ] `purge_ipc_queues` — iteriert **alle** Endpoints + Notifications je Thread-Tod.
      **Grundlinie gemessen: 6 Aufrufe → 120 768 durchlaufene IPC-Objekte**, also 20 128 je
      Thread-Tod (`NEPS + NNTFNS`), jedes einzeln gesperrt. Bei 10 000 sterbenden Threads sind
      das **201 Millionen** Sperroperationen. Das ist die grösste verbliebene Zahl der Liste.

      **Es ist dieselbe Struktur wie `pd_of` vor seiner Behebung: der Kernel SUCHT, wer auf den
      Toten zeigt, statt es zu WISSEN.** Und die strukturelle Fassung ist dieselbe — eine
      **intrusive Warteliste je Thread**: wer blockiert, trägt sich beim Objekt *und* bei sich
      selbst ein; der Tod läuft die **eigene** Liste ab. Damit wird der Pfad `O(Grad)` statt
      `O(alle Objekte)`, und der Grad ist in der Praxis 1–2.
      **Der Anker dafür existiert schon:** die Grund-Menge aus [Z24](#z24) weiss ohnehin je
      Thread, **worauf** er wartet — heute nur als Grund*klasse* (`IPC`), nicht als Zeiger auf das
      Objekt. Die Erweiterung von „welcher Grund" auf „welches Objekt" ist die natürliche Stelle;
      sie ersetzt die Suche durch die Angabe, die beim Blockieren ohnehin vorlag.
      **Vorsicht bei der naheliegenden Fassung** (D8-Muster): wer die Liste beim Wecken nicht
      genauso sorgfältig austrägt wie beim Eintragen, tauscht eine teure Schleife gegen einen
      Zeiger auf ein totes Objekt.

- [ ] **Kernel-Stacks lazy oder geteilt** — 10 000 EL0-Threads × 16 KiB = **160 MiB**, auf einer
      512-MiB-Maschine **31 % nur für schlafende Threads**; mit Kernel-Threads (64 KiB) wären es
      640 MiB und damit unmöglich.

      **Das Wasserzeichen gibt es seit 2026-08-10 (`kstack`-Zeile, gattert), und es beantwortet
      die Frage GEGEN die Erwartung: 16 KiB sind nicht grosszügig, sie sind knapp.**

      | Suite | Höchststand EL0-Kstack | Reserve |
      |---|---|---|
      | `test-qemu-x86.sh` (ohne Archiv) | 832 / 16384 B (**5,0 %**) | 15552 B |
      | `test-qemu-x86-load.sh` (mit Archiv) | **12008 / 16384 B (73,2 %)** | **4376 B** |

      **Der tiefste Pfad ist `SYS_LOAD`** — `loader::load_by_index` verifiziert eine
      **Ed25519-Signatur und einen SHA-2-Hash im Kernel**, also auf dem 16-KiB-Stack des
      aufrufenden EL0-Threads (grösste Rahmen des Abbilds:
      `vartime_double_scalar_mul_basepoint` 3528 B, `NafLookupTable::from` 1928 B,
      `ed25519 verify` 1384 B, dazu `load_into_pd_mit` 1416 B). Rekordhalter gemessen:
      Thread-Slot 14, **Programm 1 = `init`**, der Root-Task.

      **Damit ist die ursprüngliche Frage umgedreht.** „Kann man 16 KiB senken" hiess bisher
      „wieviel Luft ist da"; die Antwort ist **27 %**, und sie hängt an einer Kryptobibliothek,
      deren Stackbedarf niemand in diesem Projekt festgelegt hat. Drei Folgeposten, in dieser
      Reihenfolge:

      1. **Die Verifikation gehört vom Aufrufer-Stack herunter**, nicht der Stack vergrössert.
         Ein `SYS_LOAD` ist der einzige Syscall, der 12 KiB braucht — alle anderen bleiben unter
         1 KiB. Solange das so bleibt, zahlt **jeder** der 10 000 Threads für **einen** Pfad.
         Wandert die Verifikation auf einen eigenen, kernlokalen Arbeitsstack (einer je Kern
         statt einer je Thread), sind 4 KiB je Thread plausibel — das wären **40 MiB statt
         160 MiB**, und die Rechnung oben fällt um den Faktor 4.
      2. **Eine Guard-Page fehlt.** Der Kstack liegt aus `mem_alloc` ohne unmapped Nachbarn;
         ein Überlauf schreibt **still** in fremden Kernelspeicher. Bei 73 % Auslastung des
         tiefsten Pfads ist das kein theoretischer Posten. Dieselbe Forderung steht in
         [Z19/A4](#schritt-4--a3-stack-und-guard-page) für den **User**-Stack — sie gilt hier
         genauso, und hier trifft ein Überlauf den Kernel.
      3. **Erst danach lazy/geteilt.** Eine kleinere Zahl zu wählen, ohne (1) zu machen, hiesse
         den Ladepfad zu brechen.

      **Was die Messung NICHT erreicht** (steht so auch in der Berichtszeile): den
      **Syscall-Umleitungspfad** (Z26/A3) — in beiden x86-Suiten läuft kein gebundener Gast, die
      `redirect`-Kette ist im Wasserstand also nicht enthalten; **aarch64** (die Zähler laufen
      dort mit, die Berichtszeile steht nur auf x86); und einen **restlos** aufgebrauchten
      64-KiB-Kernel-Thread-Stack (der wird am Muster erkannt, und ohne Muster fällt er aus der
      Erkennung — für die EL0-Klasse gilt das nicht, dort wird der Kstack direkt gemessen).

- [ ] **OFFEN, aus derselben Arbeit: der `#DF`-Bericht kann den Thread-SLOT nicht nennen.**
      Er steht in `SCHEDS`/`KSTACKS`, und beide brauchen einen Spinlock. Ein `#DF` kann aber genau
      den Kontext unterbrochen haben, der die Sperre hält — der Handler bliebe stehen und heraus
      käme **kein Output**, also exakt das Bild, gegen das die Meldung gebaut ist. Identifiziert
      wird deshalb der **Stack** (lock-frei aus `TSS.rsp0`), nicht der Thread. Behebbar mit einer
      lock-freien Auskunft über den laufenden Thread je Kern — `FP_OWNER[core]` (`system.rs`) ist
      bereits ein `AtomicU64` mit genau dieser Information und hat nur keinen öffentlichen Leser.
      **Eine Zeile in `system.rs`, und der Slot steht in der Meldung.**

- [ ] **OFFEN: `MAX_TSS_CORES = 16` ist eine statische Grenze.** Die TSS und ihre IST-Stacks müssen
      stehen, bevor es einen Allokator gibt (`gdt::init()` läuft vor `mmu::init_primary`). Ein Kern
      darüber wird **abgewiesen und angehalten** (fail-closed, `ohne-TSS`-Zähler im Bericht) statt
      ohne `RSP0` zu laufen — auf einer Maschine mit mehr als 16 Kernen kostet das also Kerne, nicht
      Sicherheit. Behebbar, indem die APs ihre IST-Stacks vom Allokator bekommen (nur der BSP
      braucht statische).

### C7. Die Kapazitätskurve — wo es WIRKLICH bricht (gemessen 2026-08-10)

**Klasse:** Messung · **Stand:** **zwei der drei fehlenden Zeilen stehen (2026-08-10,
nachmittags) — C7 und [C4](#c4-die-on-bilanz) bleiben OFFEN.** Die Kurve sagte, *dass* es bis
9984 ging, aber nicht, *welcher Vorrat als nächster reisst*. Seit heute nennt sie die Ressource
(Prüfzeile `mangel`) und zeigt den Seitentabellen-Topf über wachsendes N (Prüfzeile `ptab`) —
beide gattern, beide mit isolierender Gegenprobe. Es fehlt noch die **Stack-Wasserstandsmarke**.
**Und die isolierte Zeile der Tabelle unten ist neu zu messen**: ihre Arbeiter faulteten alle
sofort (s. unten).

**Die Stack-Wasserstandsmarke ist seit 2026-08-10 erbracht** (`kstack`-Zeile, gattert in beiden
x86-Suiten) — und sie hat die Erwartung umgedreht: nicht „16 KiB sind grosszügig", sondern
**12008 von 16384 B (73,2 %) auf dem Ladepfad**. Zahlen, Ursache und die drei Folgeposten stehen
bei [C4](#c4-effizienz-bei-tausenden-threads-lineare-scans-beseitigen).

`tools/kapazitaet-messen.sh` legt PDs **mit je einem Thread** an (eine PD ohne Thread ist kein
Prozess) und druckt den Füllstand über wachsendes N. Ziel parametrisierbar über
`CAPROCK_SCALE_TARGET` (`kernel/build.rs` meldet die Variable als Bau-Eingabe — sonst misst man
den Vorgängerstand). Die Kurve läuft **ganz am Schluss** von `report_and_off`, nach jedem Urteil:
eine Messung, die tausende PDs belegt, kippt sonst jede baseline-empfindliche Zeile des Laufs.

| Aufbau | 512 MiB | 3 GiB | 6 GiB | Schranke |
|---|---|---|---|---|
| SAS-PD, **kernlokaler** Spawn | 4987 | — | **4987** | **Hosting-Kapazität EINES Kerns** = `je_kern × MIGRATION_HEADROOM` = 2500 × 2 = 5000. **Nicht RAM**: bei 6 GiB blieben 5771 MiB frei |
| SAS-PD, **lastverteilter** Spawn | 7212 | **9984** | **9984** | 512M: **RAM** (freies RAM auf 0, 64 KiB je Prozess). 3G/6G: **Thread-Slots** — 9984 + 16 schon lebende = **10 000 = `TARGET_THREADS`**, exakt die Zusage |
| **Isolierte** PD *(überholt, s. u.)* | 224 | 1504 | 3040 | **RAM**, und zwar **2 MiB je Prozess** (private Region). Linear in der RAM-Grösse; freie VSpaces bei n=1000 noch 4087, also **nicht** VSpace/ASID |
| **Isolierte** PD, Arbeiter in `.user_text` *(2026-08-10; die 3G-Zahl hat später nicht mehr gehalten, s. u.)* | **220** | **1477** | — | **RAM**, 2084 KiB je Prozess (2 MiB Region + **20 KiB Seitentabellen**). Diese Zeile misst PDs, deren Thread **lebt** — die darüber tat es nicht |
| **Isolierte** PD, 2-MiB-Region, Stand 2026-08-13 | **220** | **1119** | — | 512M: **RAM**. 3G: **NICHT RAM**, sondern der feste Vorrat aufgeteilter Guard-Seitentabellen (739 MiB frei) — s. [C7c](#c7c-der-vorrat-der-jetzt-bindet-16-aufgeteilte-guard-bloecke-gemessen-2026-08-13) |
| **Isolierte** PD, **64-KiB-Region** (C7b, 2026-08-13) | **321** | **979** | — | **beide Male der Guard-Tabellen-Topf** (419 bzw. 2918 MiB frei), **96 KiB je Prozess**. Und die Threads leben nachweislich: die Kurve zählt die Regionen mit einer **Spur** ihres Threads, nicht die Tabelleneinträge |

**Die drei Aussagen, die daraus folgen:**

1. **10 000 Prozesse sind erreicht — wenn sie sich einen Adressraum teilen und ab 3 GiB RAM.**
   Die Schranke ist dann die statische Thread-Kapazität, und sie ist auf die Zahl genau bemessen.
2. **10 000 isolierte Tenants sind es nicht.** Bei 2 MiB je privater Region bräuchten sie
   **≈ 20 GiB** allein dafür — und das ist der Fall, um den es beim Produktziel geht. Die
   Kapazitätstabelle (`NPDS = 10 000`) beschreibt eine **Tabelle**, keine erreichbare
   Tenant-Zahl.
3. **Wer alle Prozesse aus einem Thread heraus erzeugt, trifft 5000 und hält es für die
   Systemgrenze.** Der Unterschied zwischen den ersten beiden Zeilen ist eine Zeile Code
   (`spawn_parked` gegen `spawn_balanced_parked`) und ein Faktor 2.

**Welche Zeile für das Produktziel zählt — und warum sie die schlechteste ist.** Die zweite Zeile
belegt die **Kernel-Datenstrukturen**; für eine PaaS zählt die **dritte**.

> **Berichtigung der eigenen Aussage vom selben Tag.** Als dieser Absatz zuerst geschrieben wurde,
> stützte er sich auf die Zeile `224 / 1504 / 3040` — und die maß PDs, **deren Thread sofort
> starb** (`kurven_arbeiter` lag in `.text`). Die Schlussfolgerung hat den Wechsel auf lebende
> Threads überstanden (220 statt 224, 1477 statt 1504; die Größenordnung ist dieselbe), **die
> Herkunft der Zahl aber nicht**: sie kam aus einer Reihe, die nicht maß, was ihr Name sagt. Wer
> aus einer entwerteten Zeile eine Produktzusage ableitet, hat Glück gehabt, nicht recht behalten.
> Die Zahlen unten sind die der berichtigten Reihe.

Dort war die Schranke RAM bei **2084 KiB je PD** (2 MiB private Region **+ 20 KiB
Seitentabellen**, getrennt gemessen), also ≈ `RAM / 2 MiB` Mandanten — auf einer 64-GiB-Maschine rund **32 000**,
auf der Messmaschine dreistellig. **„10 000 Prozesse" ist als Zusage nur in der Fassung wahr, die
kein Mandantenmodell ist.** So gehört es gesagt, und nicht weicher.

> **Zweite Berichtigung, 2026-08-13 (C7b).** Der Absatz darüber rechnet `RAM / Regionsgrösse` —
> und setzt damit voraus, dass **RAM** bindet. Nachgemessen gilt das nur bei 512 MiB. Schon bei
> 3 GiB endete die Reihe **vor** dem RAM, nämlich am festen Vorrat aufgeteilter
> Guard-Seitentabellen (1119 PDs bei **739 MiB freiem RAM**). Die Regionsgrösse ist seither
> gesenkt (2 MiB → 64 KiB, **96 KiB je PD gemessen**), und die Dichte ist ihr **nicht** gefolgt:
> 512M 220 → **321**, 3G 1119 → **979**. Wer aus einer Kostensenkung eine Dichtezusage ableitet,
> hat den nächsten Topf nicht gefragt. Die Zahl, die heute gilt, steht in
> [C7c](#c7c-der-vorrat-der-jetzt-bindet-16-aufgeteilte-guard-bloecke-gemessen-2026-08-13).

**Die 2 MiB WAREN der Hebel Nummer eins — sie sind es nicht mehr, und das ist gemessen.** Die
klassischen Antworten (**Lazy-Zuteilung**, **geteilte Nullseite mit COW**) bleiben richtig und
bleiben Territorium des **Speicher-Servers** ([Z16](#z16) Stufe 1); sie bewegen die Dichte aber
erst, wenn [C7c](#c7c-der-vorrat-der-jetzt-bindet-16-aufgeteilte-guard-bloecke-gemessen-2026-08-13)
behoben ist. Die Priorität dreht sich damit um: **C7c vor Z16 Stufe 1.**

- [x] **Woraus bestehen die 2 MiB?** Aufgeschlüsselt und gemessen — s.
      [C7b](#c7b-die-2-mib-je-isolierter-pd-sind-gemessen-und-gesenkt--und-der-hebel-war-der-falsche).
      **Vor** dem Umbau gemessen und **danach** noch einmal, mit derselben Kernelfassung und nur
      getauschter Konstante; ohne die Vorher-Messung am selben Stand wäre die Differenz einem
      halben Dutzend Änderungen seit dem 2026-08-10 zuzuschreiben gewesen.

- [~] **Die nicht provozierten Meldestellen** (Rest des obigen Punktes). **Am 2026-08-11 von 21
      auf 16 gesunken: der Sweep fährt jetzt den echten LADEPFAD.** Stand (die Summanden gehen
      gegen `system::MELDESTELLEN` auf, und das prüft eine `const`-Zusicherung zur **Bauzeit** —
      Prosa hatte kein Gatter, und genau deshalb stand hier vorher 11+9+10+1 = **31** neben einem
      Nenner von 32):
      * **16 provoziert** — 12 in **beiden** Suiten, 4 nur mit Boot-Archiv (`4288` Segmentspeicher,
        `4304`/`4366` Seitentabellen, `4350` User-Stack). Gefahren wird `system::load_into_pd_mit`,
        also dieselbe Funktion wie bei `SYS_LOAD`, mit einer **Halteregel statt eines geratenen
        Deckels**: ein Durchgang nach dem Stack-Topf ist Schluss, denn der nächste würde ein
        fremdes Programm ohne jede Cap STARTEN.
      * **Ein Befund nebenbei:** `map_region_into_thread` (4608) stand als „Ladepfad" gebucht und
        ist keiner — es ist der Weg zum **Gerätefenster** einer Treiber-PD und braucht kein Archiv.
        Es wird seither in **beiden** Suiten provoziert. Ein Grund, den niemand nachprüft,
        überlebt jede Umgebung.
      * **10 Platz-Töpfe** (Thread-Slot, ASID, Farbstreifen, PD-Slot; +1 gegenüber der alten
        Zählung, weil `4171` Farbstreifen fälschlich unter „Ladepfad" lief): die Sperre sitzt im
        Speicher-Allokator und trifft sie nicht. Eine zweite Sperre auf den Platz-Töpfen wäre
        billig — die Frage ist, wohin sie gehört, ohne dass die Kapazitätskurve sie mitzieht.
      * **3 „der Allokator wurde nicht gefragt"** (`MANGEL_MAPPING_ABGEWIESEN`): erreichbar über
        ein Image mit **krummer Segment-VA** — genau so ist der `wasm`-Ladefehler vom 2026-08-10
        entstanden. Ein Bild dieser Art in `tools/mkarchive.py` wäre die nächste billige Stufe.
      * **1 fester HAL-Vorrat** (`MANGEL_GUARD_TABELLE`): die HAL hat keinen Allokator.
      * **2 strukturell nicht auslösbar, und das ist ein Befund über die Stellen selbst**:
        `4207` (`vspace_l2` gibt für eine soeben von `create_vspace_masked` gelieferte ASID
        **immer** `Some` — toter Zweig) und `4269` (fail-closed gegen mehr als `MAX_IMG_SEGS`
        Frame-Stücke: das braucht ein IMAGE, keinen leeren Topf).

- [ ] **Noch ohne Füllstandsanzeige** (Rest des obigen Punktes): der **FP-Slab**, die
      **VSpace-/ASID-Tabelle** (nur als `free_vspaces()` in der Kurve, nicht im Bericht), die
      **IRTE-Tabelle** und der **Finalisierungspuffer**.

- [ ] **Die dritte Zeile fehlt weiterhin: die Stack-Wasserstandsmarke** (s. [C4](#c4-die-on-bilanz),
      „Kernel-Stacks lazy oder geteilt"). Gemessen ist, wie **gross** die Stacks sind — nicht, ob
      sie **reichen**.

### C5. GIC-Skalierung (ARM-Blocker für > 8 Kerne) — **weiterhin offen**

`caprock-hal::gic` ist **GICv2** (8-CPU-Grenze, `GICD_SGIR`-Zielmaske ist 8 Bit). Mehr als 8 Kerne
brauchen auf ARM **GICv3/GICv4** (Redistributoren je Kern, `ICC_SGI1R_EL1`, ITS für MSI). QEMU:
`-machine virt,gic-version=3` (bis 512 vCPUs). **Solange das fehlt, sind >8 Kerne auf ARM nicht
testbar**, unabhängig von den Kapazitäten. Die Kapazitätsseite ist seit ext-30 vorbereitet
(`MAX_CORES = 256`, Tabellen zur Boot-Zeit dimensioniert) — es fehlt die Interrupt-Hardware.

### C6. x86-64-Port — Rest

`caprock-hal` ist jetzt architekturselektiv (`src/aarch64/` + `src/x86_64/` hinter derselben API).
Der **Kernel-Kern läuft auf x86_64**: dieselben Selbsttests, derselbe präemptive Scheduler,
dasselbe cap-gesicherte IPC — ohne ein einziges `cfg(target_arch)` im Kern. Details:
`README-X86.md`.

- [ ] **PCID** als Optimierung: der Adressraumwechsel flusht derzeit den ganzen TLB (die ASID ist
      eine reine Software-Kennung). Mit `CR4.PCIDE` + getaggten Einträgen entfiele das

- [~] **IOMMU (VT-d)**: Schritte 1–3 erledigt (s. [done.md](done.md)) — `attach` liefert `Some`,
      `dmawin`/`dmatok` laufen auf x86 mit **derselben** Funktion wie auf ARM.
      **Offen:** Schritt 4 (Interrupt Remapping inkl. CFI-Abschaltung, setzt Queued Invalidation
      voraus) und die Mehr-Einheiten-Aggregation (AGAW-Minimum, Zähler über alle DRHDs).

- [ ] **Boot-Archiv** über Multiboot-Module (`SYS_LOAD` schlägt derzeit sauber fehl)

- [~] Die ARM-seitigen Demo-/Testdienste (`threads/mod.rs`, 5402 Zeilen) sind stark ARM-gekoppelt
      (EL0-Isolation, MMIO/RTC, DMA). `dmawin`/`dmatok` sind nach `dmatests.rs` herausgezogen und
      laufen auf beiden Architekturen; **offen** sind `dmagen` (braucht ein arch-neutrales
      Leaf-Rücklesen; die Kohärenz-Teilprüfung existiert auf x86 gar nicht — legitimer SKIP) und
      der virtio-Negativtest (hängt am ARM-virtio-Treiber). Hängt mit [F1](#f-debug-testcode-aus-dem-release-build-nehmen)
      zusammen: dasselbe Modul, zwei Gründe es aufzuteilen.

---

## E. DMA-Härtung — Rest

Reihenfolge nach struktureller Wirkung, nicht nach Aufwand.

- [ ] **Kern-Uebergabe an Caprock (Variante B) — Stufe 2.** Stufe 0/1a/1b sind belegt:
      `tools/handover/` nimmt fuenf E-Cores offline, schickt ihnen INIT-SIPI-SIPI in ein
      Trampolin und bringt sie in den Long Mode mit eigener GDT, eigenen Seitentabellen und
      eigenem CR3 — wiederholbar, ohne Reboot, `rmmod` gibt alles zurueck.
      Erledigt auf der Kernel-Seite:
      * **x2APIC** (MSR-Pfad). Entscheidung fuer maximale Leistung *und* weil xAPIC nur 255
        Kerne adressieren kann. Zweistufiger Uebergang aus -> xAPIC -> x2APIC.
      * **Speicher als Bereichsliste** (`init_mem_regions`), im QEMU-Lauf immer zerstueckelt
        gefahren.
      * **`HandoverInfo`** als gemeinsame Struktur beider Startwege, vom Multiboot-Pfad
        mitbenutzt und mitgeprueft.
      Offen, in dieser Reihenfolge:
      1. **Bild-Platzierung ohne Relokation.** Der Kernel ist auf 1 MiB gelinkt, und dort liegt
         Linux. Statt ihn positionsunabhaengig zu machen: das Modul legt das Bild in einen
         4-MiB-Block (`alloc_pages(order=10)` — genau die Buddy-Obergrenze, das Bild ist 3,1 MB)
         und bildet in **seinen** Seitentabellen VA 1 MiB auf diese PA ab. `mmu::init_primary`
         muss die Abbildung dann uebernehmen statt reiner Identitaet — ein Versatz aus der
         `HandoverInfo`, keine Relokation.
      2. **Konsole als Ring** statt UART: die serielle Schnittstelle gehoert dem Wirt, zwei
         Schreiber ergeben verschachtelte Zeilen. Ring in der `HandoverInfo`, vom Modul ueber
         debugfs lesbar.
      3. **Was auf uebergebenen Kernen ausbleiben muss:** VT-d (Linux besitzt die Einheiten fuer
         Interrupt-Remapping — `GCMD.TE` toetet jeden Linux-DMA) und `intc::init_dist()` (legt
         den 8259 still, der ist global).
      4. **ELF-Laden im Modul** und der Sprung ins Bild.
      Wenn ein uebernommener Kern echten Kernel-Code faehrt, ist `release=1` beim Entladen nicht
      mehr selbstverstaendlich — dann `release=0` und Reboot.

- [ ] **VT-d-Zuteilung (Punkt 6)** — Abnahmekriterium **vorab**: nicht „neue x86-Tests grün",
      sondern **`dmaalign`/`dmawin`/`dmagen`/`dmatok`/Audit 4–7/Negativtest hören auf zu
      skippen** — ohne x86-Sonderpfade in den Tests. Ein separater `vtdtest` wäre das
      Warnsignal: er hieße, dass die Eigenschaften auf x86 anders formuliert sind, und dann
      existiert doch ein zweiter Entwurf. Was danach noch skippt, ist die ehrliche Liste dessen,
      was x86 nicht hat.
      Zerlegung:
      1. [x] **`VtdCaps`** einmal beim Hochlauf lesen, protokollieren, jede Bedingung daraus
         ableiten (nicht an der Verwendungsstelle entscheiden). Dazu die Reihenfolge-Falle
         behoben: `GCMD` ist kein RMW-Register, und `TE = 0` heißt **freier DMA**, nicht
         Blockade — jedes Kommando geht jetzt über `gcmd_issue`, das die Zustandsbits aus `GSTS`
         übernimmt. Ein `SRTP`-Schreibzugriff „nur mit dem Kommandobit" hätte die Übersetzung
         für die Dauer des Wechsels abgeschaltet, und der Test hätte es als Erfolg gelesen.
         **Messung auf der Zielplattform (QEMU q35 + intel-iommu):** SAGAW `0x6` (39 **und** 48
         Bit) → 39 Bit / 3 Level gewählt, damit die Fensterarithmetik nicht zweimal existiert;
         MGAW 48 → Eingangsgrenze `0x80_0000_0000`, identisch zur ARM-Seite; ND → 65536 Domains;
         QI und IR vorhanden; Scalable Mode aus; **ein** Fault-Recording-Register.
         Drei Werte, die Arbeit nach sich ziehen:
         * **`CM = false`** — anders als erwartet. QEMUs `intel-iommu` hat `caching-mode` per
           Default **aus**, das heißt: der Aufbau verzeiht hier ein fehlendes „nach dem Anlegen
           invalidieren", und auf `CM = 1`-Hardware bräche es. Die Richtung der Divergenz ist
           also umgekehrt zur Erwartung, die Konsequenz dieselbe: unbedingt invalidieren, `CM`
           nur protokollieren. Für Schritt 3 zusätzlich `caching-mode=on` in den Testaufbau, um
           genau das zu erzwingen.
         * **`ECAP.C = false`** — die Einheit ist **nicht** page-walk-kohärent. Schreibvorgänge
           auf Root-/Context-/Second-Level-Einträge brauchen einen Cache-Clean. Betrifft die
           Tabellen, nicht die Puffer — ein Pfad, den `dma_granule()` gar nicht abdeckt, und den
           QEMU nicht bestraft.
         * **`SC = false`** — keine Snoop Control, No-Snoop ist nicht überstimmbar. „x86 ist
           kohärent" gilt damit nur, solange kein Gerät No-Snoop benutzt. Gehört als Bedingung
           an `dma_granule() == 1`, nicht als Konstante.
      2. [x] DMAR/DRHD inkl. Device-Scope, **Gruppenbildung aus ACS**, RMRR-Ausschlüsse.
         Umgesetzt als **reine Funktion über eingespeiste Daten** (`hal::dmar`, `forbid(unsafe)`):
         `parse` nimmt einen Byte-Slice, `build_groups` eine Topologiebeschreibung. Der reale Pfad
         füllt beides aus ACPI/PCI-Enumeration, der Selbsttest aus Literalen — auf dem
         Standardaufbau (flach, keine RMRR) liefen Ausschlusspfad und Gruppenfälle sonst **nie**.
         Sensitivität belegt: „Catch-all in derselben Schleife" macht `catch_all_last` **und**
         `bridge_scope_subtree` rot, „Typ 2 wie Typ 1" nur letzteres — beide Male genau die
         Zusicherung, die die Mutation verletzt.
         Reale Messung (QEMU q35): 7 Geräte, 1 Einheit, **5 Gruppen**, 0 Ausschlüsse, keine ATSR,
         Segment durchweg 0, Oracle 0.
         Offen aus diesem Schritt: mehrere DRHD-Einheiten sind **geparst**, aber `VtdCaps` ist
         noch eine Einheit — das Minimum über alle Einheiten, die eine zuteilbare Gruppe scopen,
         und die Aggregation von Fault-/Config-Zählern über alle Einheiten stehen aus (bis dahin
         wäre das Oracle für alles blind, was nicht an Einheit 0 hängt). Eine Gruppe, die über
         Einheiten streut, wird bereits **vollständig** ausgeschlossen (`GroupSpansUnits`).
         Alt: Ausgabe:
         Liste zuteilbarer **Gruppen** (nicht Geräte — ohne ACS auf allen Upstream-Bridges ist
         die Isolationsgranularität die Gruppe: Peer-to-Peer hinter einem Switch umgeht die
         IOMMU, Multifunktionsgeräte ohne ACS teilen die RID-Sicht). Die Benennung `dma_group`
         von Anfang an, nicht nachträglich — sonst hängen Tests an der zu starken Aussage.
         RMRR-behaftete Geräte werden **abgewiesen und protokolliert**, nicht mit einer Lücke
         zugeteilt: ein Teil ihres Zugriffs liegt per Konstruktion außerhalb der Kontrolle.
      3. [x] Root-/Context-Tabellen + SLPT, `attach` liefert `Some`. **Meilenstein erreicht.**
         Befund vorweg, der das Kriterium betraf: die DMA-Tests haben auf x86 nicht *geskippt* —
         `threads/mod.rs` ist `#[cfg(target_arch = "aarch64")]`, es gab sie dort **gar nicht**.
         Ein Test, den es auf einer Architektur nicht gibt, kann dort auch nicht grün werden.
         `dmawin`/`dmatok` liegen deshalb jetzt arch-neutral in `kernel/src/dmatests.rs`, und
         beide Bring-up-Pfade rufen **dieselbe Funktion** — nicht eine nachgebaute. Beide auf
         x86 grün, aarch64 unverändert.
         Umgesetzt: SLPT (3 Level, 39 Bit), Kontext-Einträge für **alle** RIDs der Gruppe (sie
         landen in `DmaCtx::sids` und laufen damit durch dieselbe `ctx_quiesce`-Schleife wie auf
         ARM — die Falle aus `13810e9` ist strukturell vermieden), DID ab 1 (bei `CM=1` ist 0
         reserviert), `FPD` bleibt aus, Kontext-Cache **vor** IOTLB, unbedingt auch nach dem
         Anlegen. Richtung fällt heraus statt hinzuzukommen: Präsenz *ist* `R|W`, `ToDevice`
         wird zu „nur R". `SNP` wird ohne `ECAP.SC` nicht gesetzt (reserviert). Jeder
         Tabellenschreibzugriff geht durch `write_entry`, das ohne `ECAP.C` `clflush`+`sfence`
         ausführt und nie ohne Flush zurückkehrt — auch auf den Zwischenebenen.
         Kontext-Tabellen entstehen **einmal beim Bring-up** (256 x 4 KiB), nicht beim ersten
         `attach`: sie sind eine Bus-Struktur, dürfen beim Abbau eines Kontexts nicht freigegeben
         werden, und eine lazy angelegte, nie freigegebene Tabelle sähe in jeder
         Ressourcenbilanz wie ein Leck aus. Symmetrisch zur linearen Stream-Tabelle auf ARM, und
         der DMA-Pfad kommt damit ohne Allokation aus.
         Nebenbefund im Test selbst: die 32-Bit-Annahme in `dmawin` war **maschinen**-, nicht
         architekturabhängig — auf dem x86-Aufbau mit 512 MiB RAM liegt das Fenster unter 4 GiB,
         und der Test wäre grün gewesen, ohne die Eigenschaft zu prüfen. Jetzt 20 Bit.
         Offen aus diesem Schritt: `dmagen` und der virtio-Negativtest hängen noch am
         ARM-Modul (Leaf-Rücklesen bzw. virtio-Treiber); QI bleibt bewusst ungenutzt, der
         Kernel fährt ausschließlich den Registerpfad.
         Alt:
         Dabei: **`FPD`** (Fault Processing Disable) im Kontext-Eintrag ist wörtlich `CD.R` noch
         einmal — gesetzt, würde der Negativtest wieder an einer strukturell leeren Beobachtung
         bestehen. Und `FSTS.PFO` (Overflow der Fault-Recording-Register) muss in denselben
         Zähler wie `config_errors()`, sonst bedeutet „keine weiteren Faults" nach einem Sturm
         nichts. (`fault_overflow()` steht bereits.)
      3b.[ ] **Queued Invalidation ist Vorbedingung von 6.4, nicht Alternative.** Die
         Invalidierung des Interrupt-Entry-Cache existiert nur als QI-Deskriptor, nicht als
         Registeroperation — IR lässt sich ohne QI nicht korrekt betreiben. Der Registerpfad ist
         damit ein **Provisorium mit bekanntem Ablaufdatum**: nichts Weiteres darauf aufbauen,
         und beim Umstieg wirklich umstellen statt beides zu halten.
      4. [ ] Interrupt Remapping (`GCMD.IRE`) inkl. **Abschalten des Compatibility-Format-
         Interrupts** — IR aktiv bei weiter erlaubtem CFI ist eine offene Tür an der Seite.
         Eigene Zeile in `docs/invariants.md` §2, weil es dieselbe Struktur hat wie die
         BME/STE-Arbeitsteilung: zwei Mechanismen, von denen keiner den anderen ersetzt.

---

## D15. Der Kernel springt nach Adresse 0 — 2 von 600 aarch64-Läufen (2026-08-08)
**Klasse:** Fehler · **Aufwand:** offen, Ursache unbekannt · **Fundort:** aarch64-Messreihe nach
der Audit-Berichtigung

- [ ] **Das Bild, zweimal formgleich:**

          el0-trap: User-Thread 0x100000426 faultete (EC=0x20 FAR=0x0) -> beendet, Kernel laeuft weiter

          [EXCEPTION] unerwarteter Trap
            kind=4 (Current EL SPx)
            ESR=0x000000008600000d (EC=0x21)
            ELR=0x0000000000000000
            FAR=0x0000000000000000

      `EC=0x20` beim User-Thread ist ein **Instruction Abort aus einer niedrigeren EL** mit
      `FAR=0` — sein PC stand auf 0. Danach nimmt der **Kernel** `EC=0x21` (Instruction Abort auf
      derselben EL) mit `ELR=0`: er ist selbst nach 0 gesprungen. Der Lauf endet dort; das externe
      Zeitlimit räumt ihn ab (`rc=137`).

      Zum Vergleich: die *absichtlichen* Isolationssonden faulten in derselben Reihe mit
      `EC=0x24 FAR=0x40000000` (Datenzugriff). `EC=0x20 FAR=0` ist ein anderes Tier.

- [ ] **Die Rate: 6 in 2000 Läufen = 0,30 %** (95-%-Intervall rund [0,11 %, 0,65 %]), gemessen am
      2026-08-08 bei fester Parallelität 6. Von 63 Abweichungen derselben Reihe sind 57 D13
      (`offen: color`) und **0** ein Audit-Befund — die Code-7-Berichtigung trägt über 2000 Läufe
      (vorher 1 in 600).

      Die erste Beobachtung (2 in 600) war zu klein für eine Rate; erst diese Reihe gibt eine.

- [ ] **Die Vorher-Reihe — und die Trennschärfe, VOR dem Start gerechnet.** Wenn D15 durch den
      Umbau entstand, ist die Vorher-Rate 0. Wie groß muss die Reihe sein, damit ein Nullbefund
      etwas heißt?

      | Vorher-Reihe | `P(0 Treffer, wenn unverändert)` | `P(alle 6 in der Nachher-Reihe)` |
      |---|---|---|
      | 600 | 0,165 | **0,207** — trägt nicht |
      | 1000 | 0,050 | **0,088** — trägt nicht |
      | **2000** | 0,0025 | **0,0156** — trägt |
      | 3000 | 0,0001 | 0,0041 |

      Also **2000**, nicht 600. Der aarch64-Bisect vom 2026-08-04 hat genau diesen Schritt
      ausgelassen (0/6 gegen 1/18, Fisher p ≈ 1) — und das stand hinterher fest statt vorher.

      Läuft seit dem 2026-08-08 in einem Worktree auf `2ef9ddb` (Stand vor dem D0-Umbau), mit dem
      **neuen** Messstand: der ist Messinfrastruktur, nicht Prüfgegenstand.

- [ ] **Und die Aussage, die dazu geführt hatte, war ein Nullbefund ohne Größe.** Ich schrieb, das
      Bild sei „in **keinem** aarch64-Protokoll vor dem D0-Umbau aufgetaucht". Das stimmte für die
      Protokolle, die ich hatte — eine Handvoll `RUNS=6`-Läufe. Bei 0,15 % ist die erwartete
      Trefferzahl darin **0,02**. „Nie gesehen" war also die wahrscheinlichste Beobachtung, ganz
      gleich ob der Fehler da war.

      Genau derselbe Fehlschluss wie bei D0 am 2026-08-03 (2300 saubere Läufe → „ausgeschlossen",
      tatsächlich 66 % Chance auf einen Nullbefund). Er ist hier ein zweites Mal passiert, in
      derselben Sitzung, in der er als Lehre aufgeschrieben wurde — diesmal in der Form
      **„ich habe es noch nie gesehen"**, die keine Stichprobengröße nennt und deshalb noch
      leichter durchrutscht.

- [ ] **Die Spur, die bleibt: beide Threads liegen auf WIEDERVERWENDETEN Slots.**
      `0x100000424` und `0x100000426` — Generation **1**, Slots 1060 und 1062. Der `scale`-Test
      erzeugt 1024 Threads gleichzeitig und baut sie ab; Generation 1 heißt, der Slot ist schon
      einmal recycelt worden. Ein Slot, der recycelt wird, während noch jemand auf ihn zeigt,
      ergäbe genau ein `sp`/`entry` von 0.

      Das gilt unabhängig vom D0-Umbau weiter und ist der nächste Ansatzpunkt — der Verdacht
      richtet sich jetzt auf `reap`/`record_zombie`/`alloc_tcb` **als solche**, nicht auf ihr
      Zusammenspiel mit dem Parken.

- [ ] **Was als Nächstes zu tun ist, in dieser Reihenfolge:**
      1. ~~Eine Reihe, die eine Rate ergibt~~ — erledigt: 0,225 % gepoolt über 4000 Läufe.
      2. ~~Dieselbe Reihe auf dem Stand VOR dem Umbau~~ — erledigt, s. oben: der Umbau ist es nicht.
      3. **Einen Melder in den Reap-Pfad**, der die *Gelegenheit* zählt statt des Treffers. Bei
         0,225 % ist ein Melder, der nur beim Unglück spricht, in 444 von 445 Läufen stumm.
         Zu zählen wäre: wird ein TCB-Slot recycelt, auf den noch ein Verweis zeigt (Directory,
         Ready-Queue, `sc_donee`, Reply-Token)? Die Generation ist dafür da — ein Zugriff mit
         veralteter Generation ist die Gelegenheit, und sie ist in jedem Lauf zählbar.
      4. **Den Einstiegspunkt festhalten, wo er gilt.** `entry`/`sp` eines Threads beim
         `alloc_tcb` mitschreiben und beim EL0-Fault mit `FAR=0` ausgeben — dann sagt das
         Protokoll, ob der Thread mit `entry=0` erzeugt wurde oder unterwegs dorthin geriet.
         Das ist der Unterschied zwischen „falsch aufgesetzt" und „überschrieben".


**Klasse:** Beleglücke · **Aufwand:** eine Messung, ein Verus-Modell

- [ ] **Die Abnahmemessung ist nicht mit der Fundmessung vergleichbar.** Zwischen beiden wurde der
      Speicherregler berichtigt (RSS an der Subshell statt an QEMUs Prozessbaum, 192 statt
      361 MiB je Lauf) — die Parallelität war also eine andere. Bei einem **Startrennen** ist genau
      die Last die Größe, die die Trefferrate erzeugt: `P(0 | unverändert) ≈ 1,2·10⁻⁴` steht damit
      für „behoben **oder** weniger Druck", und die beiden sind nicht getrennt.

      **Und die Bedingung der Fundmessung ist nicht mehr feststellbar** — ihr Protokoll ist in der
      Mitte abgeschnitten, die Zeile „Regler steht bei N Arbeitern" fehlt.

      **Zu tun:** den behobenen Kernel unter *fester* Arbeiterzahl fahren, mindestens so hoch wie
      die Fundmessung gekonnt hätte (ihr RAM-Budget erlaubte 42, CPU band bei ~16). Wird bei
      **fest 24** nichts getroffen, ist „weniger Druck" ausgeschlossen — die Bedingung ist dann
      monoton in der Richtung, die zählt. `ARBEITER_FEST=24 tools/d0-messen.sh 50000`.
      Die Bilanz nennt die Bedingung seit 2026-08-07 selbst.

- [ ] **Die x86-Reihe prüft den Umbau fast nicht.** `pdbind` zählt auf x86 **3** Bindungen, auf
      aarch64 **70** (`kernel/src/threads/mod.rs` ist `#[cfg(target_arch = "aarch64")]`). 50 000
      x86-Läufe decken drei Zulassungsstellen ab. Der Messstand fährt seit 2026-08-07 `ARCH=arm`;
      eine Reihe in der Größenordnung 2000 ist dort mehr wert als weitere x86-Läufe.

- [ ] **Verus sagt zu D0 nichts.** Das IPC-Modell kennt den Begriff „Thread ohne PD" nicht — null
      Vorkommen von PD-Bindung oder `ERR_NOPD` in `Verification/ipc/proofs/`. „16 Dateien, 0
      errors" heißt hier nur, dass die vorhandenen Beweise weiter halten.

      Was fehlte, ist eine **Invariante**, die `RECV` an eine gebundene PD knüpft — bloße
      Repräsentierbarkeit des Zustands beweist nichts. Das ist kein kleiner Zusatz: das Modell
      kennt heute nur Endpoints und Warteschlangen, keine PDs; ein `pd_bound`-Prädikat einzuführen
      heißt, den Modellzustand zu erweitern und die bestehenden Beweise darüber neu zu führen.

- [ ] **Ein fallengelassener `Parked` ist kein Übersetzungsfehler.** `#[must_use]` macht ihn zur
      Warnung; ein `let _ = spawn_parked(..)` schluckt sie. Der Typ deckt den gefährlicheren Fall
      (eine `ThreadId`, die vor der Zulassung entkommt), nicht diesen. Ein `Drop`-Impl wäre **kein**
      Ausweg — damit ließe sich das Feld in `admit` nicht mehr herausbewegen, und der Typ verlöre
      genau die Eigenschaft, um die es geht.

## C4-Nachtrag: die 40 MiB sind EINGELOEST, nicht mehr prognostiziert (2026-08-12)

`USER_KSTACK_SIZE` steht auf **x86 bei 4 KiB** statt 16. Zweistufig gesenkt, jede Stufe mit beiden
Suiten belegt:

| Grösse | Höchststand (Lade-Suite) | Reserve | Suiten |
|---|---|---|---|
| 16 KiB, **vor** C8 | 11 992 B — **73,1 %** | 4392 B | ALL PASS |
| 16 KiB, nach C8 | 1 312 B — 8,0 % | 15 072 B | ALL PASS |
| 8 KiB | 1 312 B — 16,0 % | 6 880 B | ALL PASS |
| **4 KiB** | **1 312 B — 32,0 %** | **2 784 B** | ALL PASS, Abnahme-Reihe 12/12 |

**10 000 EL0-Threads kosten damit 40 MiB statt 160** — auf einer 512-MiB-Maschine 7,8 % statt 31 %.

**Die Begründung hängt an drei Dingen, die es vorher nicht gab**, und ohne sie wäre die Zahl
geraten: die **Guard-Page** (ein Überlauf ist ein `#PF`, kein stiller Treffer), die **IST-Stacks**
für `#DF`/`NMI`/`#MC` — ein NMI fragt `IF` nicht und läuft **nicht** mehr auf diesem Stack —, und
die **Wasserstandsmarke**, die den Wert bei jedem Lauf nachmisst statt ihn zu glauben.

**Auf aarch64 bleibt es bei 16 KiB.** Dort fehlen alle drei. Eine Zahl, die auf einer Architektur
gemessen und auf der anderen übernommen wird, ist auf der anderen geraten — `MASK_BITS` war auf
x86 zufällig richtig.

- [ ] **aarch64 nachziehen** — Guard-Page (Blockaufteilung), ein IST-Gegenstück und die
      Wasserstandszeile. Erst dann ist die Konstante dort mehr als eine Vorsichtsmassnahme, und
      erst dann zählt `unbewachte_stacks()` nicht mehr jeden Stack.
- [ ] **Was die 4 KiB NICHT abdecken:** der Syscall-Umleitungspfad (Z26/A3) läuft in keiner der
      beiden x86-Suiten; sein Beitrag zum Wasserstand ist ungemessen. Wer ihn verdrahtet, misst
      die Zeile neu, bevor er sie glaubt.

## C8. Der VERIFIZIERERTHREAD — **gebaut** (2026-08-11); offen ist nur noch (c)

**Klasse:** Kapazität / Sicherheit · **Stand:** (a) und (b) sind **gebaut und gemessen**, die
Herleitung samt Gegenproben steht in [done.md](done.md). Hier bleibt, was offen ist.

**Was erledigt ist, in einer Zeile:** `SYS_LOAD` verifiziert nicht mehr auf dem 16-KiB-Kstack des
Aufrufers, sondern auf dem 64-KiB-Stack eines dedizierten Verifiziererthreads. Der Aufrufer
blockiert regulär mit dem eigenen Grund `BlockReasons::LOAD` (Z24); der Überlauf der Auftragsschlange
heisst `ERR_LOAD_BUSY` und lässt den Überläufer **lauffähig**. Gemessen (Lade-Suite, 512M):
EL0-Kstack **11 992 → 1 312 B**, Verifiziererstack **12 328 / 65 536 B**.

- [ ] **(c) Die „Krypto raus aus dem Kernel"-Frage ist ein EIGENER Entwurf, kein Nebensatz.**
      Die Signaturprüfung ist **Wurzelvertrauen**. Sie kann in eine PD wandern — aber dann ist
      *diese PD* TCB, und der Gewinn ist nur echt, wenn der Kernel dem **Ergebnis** über einen
      **schmaleren Kanal** traut als dem Code selbst (etwa: der Kernel prüft nur noch einen Hash
      gegen eine vom Verifizierer signierte Freigabe). Für jetzt reicht der kernlokale Thread;
      die PD-Fassung steht hier als **benannte Option**, damit sie nicht als „haben wir mal
      erwogen" verschwindet.

- [ ] **Aus dem Bau übrig geblieben: `AUFTRAEGE_MAX = 4` ist eine gewählte Zahl, keine
      hergeleitete.** Sie steht an einer Stelle und im Bericht, aber sie ist Politik: sie sagt, wie
      lang der DoS-Kanal werden darf, bevor abgewiesen wird. Was fehlt, ist ein **Budget je PD** —
      heute kann eine einzelne PD alle vier Plätze belegen und fremde Ladevorgänge um genau so viel
      verzögern, wie ein Ladevorgang dauert. Die Absage ist damit benannt, die **Fairness** ist es
      nicht.

- [ ] **Und: `heimatkern` ist eine Angabe, kein Beleg.** Der Auftrag trägt den Kern des Aufrufers
      mit, damit die Platzierung nach dem Umzug dieselbe bleibt — geprüft ist das nur mittelbar
      (`ladepol` liest Affinität aus dem TCB zurück, aber nur für Programme, die eine im Manifest
      *haben*). Ein Programm **ohne** Affinität hat heute keine Zeile, die seinen Kern nachliest.

- [ ] **NEU und ernst: `caller_pd` ist seit C8 eine ZAHL AUS DER VERGANGENHEIT.**
      Vor C8 lief `load_by_index` synchron im Syscall des Aufrufers — die Aufrufer-PD war
      per Konstruktion am Leben. Jetzt liegt zwischen der Übergabe und der Ausführung ein
      Scheduling-Fenster, und `caller_pd` ist ein **Index**. Stirbt der Aufrufer in diesem Fenster
      und wird sein PD-Slot neu vergeben, bindet `create_hardware_backend(caller_pd, ..)` das
      Treiber-Backend an den **falschen Partner** — und die Cap-Prüfung liefe dabei korrekt durch,
      genau wie beim Geräte-Fenster in den geteilten Seitenverzeichnissen.
      **Heute nicht ausgelöst** (der einzige `SYS_LOAD`-Aufrufer der Suiten ist der Root-Task, und
      der stirbt nicht), aber das ist eine Aussage über die Testfälle, nicht über den Code.
      Die strukturelle Fassung ist dieselbe wie bei `ThreadId`: der Auftrag müsste eine
      **generationsbehaftete** PD-Referenz tragen statt eines Index, oder die Ausführung müsste die
      Identität des Aufrufers vor der Benutzung nachprüfen. Ein Halbfix („ist der Aufrufer noch am
      Leben?" unmittelbar vor dem Laden) verschiebt das Fenster nur und sieht wie eine Behebung aus.

- [ ] **Die Sprechprobe `laeuft` heisst „wurde gestartet", nicht „lebt".** Sie liest
      `verifizierer::thread_id().is_some()`, also ein Feld, das `starten()` einmal setzt — nicht das
      Thread-Directory. Heute ist beides deckungsgleich, und zwar aus einem benennbaren Grund: der
      Verifizierer hat **keine PD** und es gibt **keine Tcb-Cap** auf ihn, er ist aus Ring 3 also
      nicht tötbar. Entsteht je ein Weg, ihn zu beenden, wird die Zeile **still falsch** — sie
      meldete „laeuft", während `uebergeben` Aufträge in eine Schlange legt, die niemand mehr leert
      (bis `ERR_LOAD_BUSY`). Die Behebung ist eine Zeile (`caprock_sched::is_live`); sie steht hier
      und nicht im Code, weil sie heute **nicht falsifizierbar** ist — ein Wächter, den keine
      Gegenprobe erreichen kann, ist eine Behauptung.

- [ ] **Kleiner, aber aus derselben Wurzel: ein Ladeergebnis kann ins Leere gehen.** Stirbt der
      Aufrufer, während sein Auftrag wartet, wird die PD trotzdem geladen und ihre Id erfährt
      niemand. Gezählt wird das (`Antwort-ins-Leere` in der `verif`-Zeile, heute 0), aufgeräumt
      nicht. Zu entscheiden ist, ob ein solcher Auftrag beim Tod des Aufrufers **verworfen** gehört
      — dann braucht der Sterbepfad einen Griff in die Auftragsschlange, und der ist die Stelle, an
      der die Sperrordnung noch einmal geprüft werden muss.

## D13. Die Suite hat Prüfungen, die in WANDUHRZEIT messen — und der Messstand ist überbucht
**Klasse:** Messstand · **Aufwand:** klein, aber die Abgrenzung ist die eigentliche Arbeit

- [ ] **Gemessen am 2026-08-07: 4 Abweichungen in 50 000 Läufen, alle vier Lastartefakte.** Der
      D0-Messstand fährt 16 Gäste zu je 4 vCPU auf 20 Kernen — **3,2-fache Überbuchung**. Wird eine
      vCPU vom Wirt verdrängt, laufen Ticks und TSC weiter, die Gastausführung nicht.

      | Bild | Zahl | Messwert |
      |---|---|---|
      | `cycles : FAILURES` | 2 | 1-ms-Fenster = 11 689 334 bzw. 11 720 490 Zyklen statt 2 803 578 — **Faktor 4,2** |
      | `freeze : FAILURES` | 2 | ein ~30-ms-Fenster sah **0** Worker-Runden statt 3 |

      Belege: `docs/befunde/d0/lastartefakt-{freeze,cycles}-2026-08-07.log`.

- [ ] **Wie man es von einem Kernelfehler unterscheidet — an der FORM, nicht an der Zahl.** Bei
      einem der beiden `freeze`-Fehlschläge fiel `laeuft-vorher=false (49->49)` durch: die
      **Positivkontrolle**, gemessen *bevor* eingefroren wird. Ein Fehler im Auftaupfad kann sie
      strukturell nicht verursachen. Die naheliegende Lesart („die D0-Umstellung hat `thaw`
      beschädigt") war damit widerlegt, ohne den Auftaupfad überhaupt anzusehen.

      Das ist die allgemeine Regel für diesen Messstand: **bei einer Abweichung zuerst fragen,
      welche Konjunkte fallen — nicht, wie oft sie fällt.** Ein Zähler allein hätte hier zu einer
      Fehlersuche im Scheduler geführt.

- [ ] **Was zu tun ist.** Die betroffenen Fenster sind in Wanduhrzeit definiert (`hal::timer::ticks`
      bzw. eine 1-ms-Kalibrierung). Zwei Wege, und der erste ist der bessere:

      1. **In der gemessenen Größe zählen statt in der Zeit.** Für `freeze` heißt das: warten, bis
         der Zähler sich um N bewegt hat, mit einer Obergrenze — dann ist „er steht" die Aussage,
         und nicht „er hat sich in 30 ms nicht bewegt". Dieselbe Überlegung wie bei D10, wo eine
         Iterationszahl eine Stoppuhr ersetzt hat: *eine Iterationszahl ist eine Eigenschaft des
         Programms, eine Zeitmessung nicht.*
      2. Das Fenster verlängern. Billiger, aber es verschiebt die Grenze nur — bei 6-facher
         Überbuchung fällt es wieder.

      **Nicht**: die Prüfung unter Last aushängen. Ein Test, der bei Last schweigt, schweigt genau
      dann, wenn er gebraucht wird.

- [ ] **Die Rate auf aarch64: 57 in 2000 Läufen = 2,85 %** bei fester Parallelität 6 (gemessen
      2026-08-08) — mit Abstand die häufigste Abweichung dieser Reihe, und sie ist ein Artefakt
      des Messstands, kein Kernelbefund.

- [ ] **Auf aarch64 ist es dasselbe, und dort ist es die HÄUFIGSTE Ursache** (gemessen
      2026-08-07): 32 Läufe bei 16-facher Parallelität, **9 Abweichungen, alle neun
      `bringup : offen: color`**. Die Farbzeilen sind byte-identisch zur Referenz (`color`/`stripe`
      ALL PASS, `pprobe` SKIP) — es fällt nichts durch. Der Watchdog feuert **zwischen** dem Druck
      der Farbsuite und dem `COLOR_DONE`-Store: die Frist sind 6000 **Ticks**, und die Farbsuite
      ist auf `cross`/`strand`/`loadstop` gegatet, läuft also als letzte.

      Damit ist auch eine Zuordnung berichtigt: der aarch64-Hänger beim D0-Umbau war **nicht** D6
      und **kein geparkter Thread**. Beantwortbar wurde die Frage erst dadurch, dass der
      aarch64-Watchdog seit 2026-08-07 **nennt**, was offen war.

- [ ] **Vorbehalt zur Zahl.** 2 `freeze`-Artefakte in 50 000 gegen 0 in den 56 895 Läufen davor ist
      **nicht** signifikant (Fisher p ≈ 0,2). Es gibt also keinen Beleg, dass die Empfindlichkeit
      neu ist — nur, dass sie existiert.

## D. Verifikation

- [ ] **KEIN CI-RUNNER — alle Gates warten (2026-08-03). Das ist der Rest eines Befunds, dessen
      erste zwei Schichten heute zu sind.** Der Reihe nach, weil jede Schicht die nächste verdeckt
      hat:

      | # | Befund | Stand |
      |---|---|---|
      | 1 | Gates lagen in `.gitea/workflows/`, der Server ist **GitLab** — er liest das nicht | **behoben**: `.gitlab-ci.yml` |
      | 2 | `.gitea/workflows/kani.yml` war seit Anlage **ungültiges YAML** (`: ` im Jobnamen, Z. 63 Sp. 49) | **behoben** + `tools/ci-yaml.sh` mit Selbsttest |
      | 3 | **kein Runner registriert** | **offen — hier** |

      Gemessen, nicht vermutet: Pipeline 5 stand **284 s** in der Warteschlange, Pipeline 6
      **939 s (16 min)** — beide mit allen Jobs `pending`, `runner=None`, `tag_list` leer. Ein
      registrierter Runner greift binnen Sekunden zu; 16 Minuten sind keine Langsamkeit mehr. Die Runner-API (`/projects/2/runners`) braucht ein Token, das hier nicht vorliegt —
      **welcher** Runner fehlt (kein Runner / falsche Tags / `paused`), ist damit noch nicht
      auseinandergehalten.

      Vor dem Push existierten **zwei** Pipelines in der ganzen Projektgeschichte, beide vom
      2026-05-23, beide auf `main`, beide mit GitLabs Auto-DevOps-Vorlage
      (`semgrep-sast`, `container_scanning`, `code_quality`, `build`, `test`) — nie mit unseren
      Gates.

      **Warum das mehr ist als eine Konfigurationsaufgabe.** Der Verus-Beweis in `cap_space.rs`
      war 37 Tage rot, während der zugehörige Commit „delete (Leaf) gegen die VOLLE cap_inv
      bewiesen" hieß und der README „10 verified" behauptete. Die naheliegende Erklärung war
      „gelaufen, aber niemand hat hingesehen". Sie ist falsch: **es gab nichts zu sehen.**
      Ein Gate, das auf keinem Server steht, ist keine schwächere Zusicherung, sondern keine —
      und es ist von einem grünen Gate äußerlich nicht zu unterscheiden. Das ist dieselbe Form
      wie die leere Ereigniswarteschlange ohne `CD.R`.

      **Abnahmebedingung — und sie ist absichtlich nicht „die CI ist grün":** eine Pipeline-ID
      vorzeigen können, deren `verus`-Job **gelaufen** ist. Dazu drei Fälle, die
      auseinandergehalten gehören, weil sie von außen gleich aussehen:
      (a) keine Pipeline → Konfiguration nicht gefunden;
      (b) Pipeline mit `pending`-Jobs → kein Runner (der Fall heute);
      (c) Jobs mit Ergebnis → erst hier sagt grün etwas.

      Nebenpunkt, sobald ein Runner läuft: der `verus`-Job fährt `--selftest` **vor** dem
      eigentlichen Lauf. Wenn der Selbsttest grün ist, aber der Hauptlauf auch — dann erst ist
      belegt, dass das Gate fehlschlagen *kann* und trotzdem nicht fehlschlägt.

      Eigene Falle dabei, für den nächsten, der misst: **jeder Push bricht die vorige Pipeline
      desselben Zweigs ab.** Pipeline 4 und 5 gingen so auf `canceled` — das sah nach „Runner
      hat abgelehnt" aus und war mein eigener nächster Commit.

- [ ] **Zyklenzähler weiterführen** (Stufe 1 teilweise erledigt): `hal::timer::cycles()` /
      `cycles_per_sec()` / `invariant_tsc()` stehen auf beiden Architekturen, serialisierend und
      gegen den PIT kalibriert. Offen: die per-TCB-Abrechnung (`consumed_cycles`, gestempelt beim
      Ein-/Auswechseln) und darauf die Monitoring-Cap.
      **Invarianz ist Telemetrie, keine Bestehensbedingung** — aber die Einschränkung hing am
      Emulator, nicht am Entwurf: unter KVM mit `-cpu host,+invtsc` meldet dieselbe Prüfung
      `true` (`+invtsc` muss **explizit** angefordert werden, QEMU lässt es auch bei `-cpu host`
      weg, weil es die Live-Migration blockiert). Zen/EPYC hat Invariant TSC durchgehend. Der
      TCG-Fallback bleibt und sagt im Log, dass die Werte dort indikativ sind — und dass die
      Zusage anderswo vorhanden ist.

- [ ] **RAM-Größe als Testparameter** (teilweise erledigt): `test-qemu-x86.sh` nimmt `-m` als
      zweiten Parameter (512M/8G geprüft). Für aarch64 steht dasselbe noch aus.

- [ ] **Zählgrenzen + Lock-Sektion als Operationszahl** (nächster Schritt, s. Diskussion):
      Iterationen je Thread-Tod in `purge_ipc_queues`, CDT-Walk-Länge, `revoke`-Teilbaumgröße,
      Tabellenbelegung, Stackbytes je Thread — als **Anzahl**, damit maschinenunabhängig. Die
      Amdahl-Rechnung zerfällt dann in „Operationen unter Lock je Thread-Lebenszyklus" (jetzt
      verfügbar) mal „Kosten je Operation" (auf Blech kalibriert). Mit KVM ist die direkte
      Sektionsmessung zusätzlich möglich (Auflösung 51 Zyklen), also beides.

- [ ] **D1** *(Kernfrage beantwortet, Rest ist eine Entwurfsentscheidung)* Kani deckt die
      ext-29-Eigenschaft von `caprock-sync` **strukturell nicht** ab — belegt 2026-08-01: Kani
      baut für das Host-Ziel, dort greift der dritte `cfg`-Zweig mit
      `IRQ_MASKING_IMPLEMENTED = false` und `irq_save_disable()` als No-Op; der Lauf sagt es
      selbst (`warning: constant IRQ_MASKING_IMPLEMENTED is never used`). Ein grünes `sync`
      beweist Speichersicherheit und Arithmetik, nicht IRQ-Sicherheit.
      **Kompensiert:** die Eigenschaft trägt der Wächter zur Übersetzungszeit, und dass der
      auslösen *kann*, ist jetzt geprüft statt behauptet — `tools/guard-verify.sh` baut
      unverändert (muss übersetzen) und mit zerstörten `cfg`-Zweigen (muss am Wächter
      abbrechen), für beide Bare-Metal-Ziele; im Kani-Gate verdrahtet. Details:
      `docs/kani-lauf.md`.
      **Bleibt offen:** ob zusätzlich ein Kani-Harness gebaut wird, der die Maskierung als
      *Modell* mitführt (statt sie wegzu-`cfg`-en) und Reentranz aus dem IRQ-Pfad als
      Eigenschaft formuliert. Das wäre ein echter Beweis statt eines Wächters — aber er fände
      einen `cfg`-Auswahlfehler weiterhin nicht, denn er liefe auf demselben Host-Ziel.
- [~] **D2 halb erledigt (2026-08-02, B-7.2).** Die **Kopie** ist weg: Loom prüft den echten
      `caprock-sync`-Quelltext (das Skript kopiert ihn unverändert, die Beweise stehen in
      derselben Datei), die alten Nachbauten sind gelöscht. Der Fund dabei: mit
      `core::cell::UnsafeCell` prüft Loom nur das **Atomic-Protokoll** — eine abgeschwächte
      Ordnung im Ticket-Release lief durch ALLE Beweise; erst mit `loom::cell::UnsafeCell` fallen
      2 von 10.

      **Offen bleibt die IRQ-Maskierung**, und sie ist prinzipiell nicht modellierbar (kein DAIF
      in Loom). Deadlockfreiheit gegenüber Preemption bleibt damit Argument, nicht Beweis.

- [ ] **D3** Die ext-29-Invarianten (Grant-Nicht-Leck, Zeroing, Cap-Budget) und die
      ext-30-Invarianten (Directory-Kohärenz, Migrations-Sperrordnung) haben Laufzeittests, aber
      keine Verus-/Kani-Beweise. Für die Migration wäre die Sperrordnung „aufsteigende Kern-ID"
      ein lohnendes Loom-Modell (zwei Kerne migrieren gegeneinander).

- [~] **D6 (2026-08-02): die aarch64-Suite hatte keine Wiederholungsmessung — jetzt hat sie eine.**

      **Was der Befund war:** über rund 13 Läufe fielen **vier verschiedene** Prüfungen aus
      (`xfer`, `dtb`, `prio`, `loadstop`), scheinbar zufällig. Das sah nach Kernel-Nichtdeterminismus
      aus und war zur Hauptsache **die Testmechanik**:

      1. Die Ausgabe hing an einer **Pipe**, beendet wurde mit `--signal=KILL`. Ein per SIGKILL
         erschlagenes QEMU flusht seinen stdout-Puffer nicht.
      2. Nach dem Umbau auf eine Datei teilten sich alle Läufe **eine** Datei. Gelegentlich fehlten
         dann **frühe** Bootzeilen in der ausgewerteten Ausgabe, während alle Ergebniszeilen da
         waren — die Signatur blieb identisch, und trotzdem fiel je nach Lauf eine andere Prüfung
         durch. Jeder Lauf bekommt jetzt seine eigene Datei.

      **Gemessen nach dem Umbau:** `RUNS=16` → 16 von 16 mit identischer Signatur, `ALL PASS`.

      **Was NICHT erledigt ist:** in einer Messung davor (gemeinsame Datei) standen 14 von 16, und
      die beiden Abweichungen waren echte Watchdog-Läufe — die Zeile kommt vom Kernel, nicht von
      der Mechanik. 16 saubere Läufe schließen eine Rate von 12,5 % nicht aus (P ≈ 12 %). Der
      Hänger ist damit **offen**, aber erstmals messbar: die Suite hält jetzt bei Abweichung das
      volle Log (`build/diag/arm-abweichung-lauf-N.log`), und `RUNS` ist der Weg, die Rate zu
      bestimmen.

      **Nächster Schritt:** die aarch64-Notbremse soll sagen, **worauf** sie gewartet hat — auf
      x86 tut sie das seit heute (`bringup : offen waren: …`), und dort war der Befund damit in
      einer Zeile sichtbar. Im ARM-Hänger sind alle Ergebniszeilen grün und der Bericht kommt
      trotzdem aus der Notbremse; ohne diese Zeile ist nicht zu sagen, welche Bedingung fehlte.

- [ ] **D4** Verus laut `docs/verification.md` offen: `delete_leaf` auf der vereinten Struktur,
      Kinderlisten-Erreichbarkeit, danach Scheduler/IPC.

- [ ] **E-Rest 3c: `hiiso` hat noch nie ein Urteil gefällt.** (2026-08-04, gemessen direkt nach
      der E-Rest-3-Behebung.) Die Zeile meldet in **allen vier** RAM-Größen `SKIP` — auch bei 3G
      und 6G, wo die geteilte hohe Gerätetabelle existiert (`=1`). In der **Lade-Suite** kommt
      sie gar nicht vor.

      | Hälfte der Prüfung | Stand |
      |---|---|
      | unzulässige Einträge in den geteilten Tabellen | wird ausgewertet (`=0`), und eine `US`-Mutation lässt sie fehlschlagen — **sprechfähig** |
      | **private Kopie für eine isolierte PD** | `private Kopien = 0` überall — **läuft nirgends** |

      Damit ist die Eigenschaft, vor der CLAUDE.md ausdrücklich warnt („ein Gerätefenster dort
      einzutragen gäbe es JEDER isolierten PD, lautlos, denn die Cap-Prüfung liefe korrekt
      durch"), für den Bereich **oberhalb 4 GiB** behauptet und nicht vorgeführt. Der
      Aggregatwert ist ehrlicherweise `SKIP` und zählt nicht als bestanden — die Lücke steht
      trotzdem.

      **Zu tun:** einen Fall bauen, in dem eine **isolierte PD** ein Gerätefenster oberhalb
      4 GiB bekommt, und zeigen, dass (a) sie es hat, (b) eine zweite isolierte PD es **nicht**
      hat. Ohne (b) ist es kein Isolationsnachweis. Der natürliche Ort ist die Lade-Suite bei
      6G — dort gibt es Treiber-PDs, und die BARs liegen bei `0x70_0000_0000`.

- [ ] **E-Rest 1b: im SCHWACHEN Zweig teilen sich alle vier Kontexte EIN Fenster.** (2026-08-04,
      beim Beheben von E-Rest 1 bemerkt, **nicht** behoben.) `slot_window` gibt dort jedem Slot
      `[0, 512 GiB)`. Damit gilt die Zusicherung „zwei Kontexte vergeben nie dieselbe IOVA" nicht
      — und genau darauf beruht die Bounds-Prüfung eines Treibers gegen den **eigenen** Kontext.
      Der schwache Zweig ist ab ~512 GiB RAM erreichbar. Seit E-Rest 1 wird er wenigstens
      **gemeldet** (`dmawin : FAILURES` statt stillem `msi_clear=1`), aber die Slot-Trennung
      fehlt dort weiterhin.

- [~] **E-Rest 3d ZUR HÄLFTE ERLEDIGT am 2026-08-04: die Stellen sind aufgezählt, der Speicher
      oberhalb 4 GiB trägt gemessen.** Die Klassifikation steht als Aufzählung an **einer** Stelle
      (`enum Zone` in `kernel/src/system.rs`), nicht verstreut:

      | Klasse | wer | Stand |
      |---|---|---|
      | `KernelOnly` — bevorzugt **oben** | Thread-/Cap-/IPC-Tabellen, Kernel-Thread-Stacks, Segment- **und** Stack-Frames geladener Programme, alle L3-Seitentabellen, AP- und Sekundärstacks | gemessen: bei 3G/6G liegen **alle 28** oberhalb 4 GiB, beide Suiten grün |
      | `PdMappable` — muss **tief** | alles identisch Abgebildete (`vspace_map`/`map_frame`/`map_into_thread`) | strukturell: `vspace_map_page_at` weist `va >= GIB1_END` ab; **Gegenprobe gefahren** |
      | harte Bedingung (`gib0_zone`) | isolierte PD-Region, `spawn_isolated_native`, `alloc_dma_region` | bekommen `None` statt einer unbrauchbaren Adresse |

      **Die Gegenprobe ist der eigentliche Beleg:** stellt man `system::alloc` auf `KernelOnly`,
      fällt die **Lade-Suite** bei `-m 3G` aus (`drv`/`blkdev`/`dmaiso` — die Treiber-PD wird nie
      bereit), während die **Hauptsuite grün bleibt**. Eine Klassifikation, die nur die Hauptsuite
      prüft, hätte den Fehler durchgelassen.

      **Zwei eigene Vermutungen dabei widerlegt, beide gemessen statt geglaubt.** (a) „Geladene
      Programmsegmente brauchen GiB 0" — falsch: `vspace_map_page_at` nimmt VA und PA getrennt,
      sie liegen jetzt oben. (b) „Das Gerät erreicht nur GiB 0" — falsch: mit der virtio-Region
      oberhalb 4 GiB liest `virtio-blk` den Sektor korrekt (`Geraet-DMA=1`). Die erste Vermutung
      hatte ich am selben Tag noch als Befund notiert; sie stand auf einer Messung, die durch
      einen anderen Fehler (Selbst-Deadlock) verfälscht war.

      Der Ausweichzähler ist jetzt **zweiteilig** und sagt etwas: bei 512M/2560M weichen 28
      Kernel-Allokationen nach unten aus (es gibt oben nichts) — der Pfad ist also **gefahren**,
      nicht bloß vorhanden; bei 3G/4G/6G ist er 0 in beide Richtungen.

      **Offen bleibt (a) der Rest der Aufzählung:** EL0-Kernel-Stacks (`claim_user_kstack`),
      der EL0-User-Stack von `spawn_user`, die IOMMU-Tabellen (`alloc_zeroed`) und die
      Sentinel-Page des DMA-Tests stehen weiter konservativ auf `PdMappable`. Für keine ist
      gezeigt, dass sie oben liegen **darf** — „konservativ" heisst hier ungeprüft, nicht sicher.
      Die Kernel-Stacks sind dabei der lohnendste Posten: 16 KiB je EL0-Thread, bei tausenden
      Threads die bestimmende Größe in der knappen Zone.

- [ ] **D12: aarch64 fällt unter Host-Überbuchung gelegentlich aus — und meine Entlastung des
      eigenen Umbaus war NICHT gedeckt.** (2026-08-04, gemessen.)

      Ich hatte geschrieben, der Fehlschlag „trat auch vor diesen Änderungen auf". Das war
      **behauptet, nicht gemessen**. Nachgeholt als Bisect über die **Last** statt über den Code:
      derselbe Aufbau (drei parallele QEMU-Suiten: x86 3G, x86 6G, Lade-Suite 6G), einmal auf dem
      Stand **vor** dem VA-Fenster (`986bb40`, eigener Worktree), einmal danach.

      | Stand | Läufe unter dreifacher Parallellast | grün |
      |---|---|---|
      | vor dem Umbau (`986bb40`) | 6 | **6** |
      | nach dem Umbau (HEAD) | 6 + 12 | **17** |

      **Diese Messung konnte die Frage NIE beantworten — und das hätte vor dem Start auffallen
      müssen.** Bei der beobachteten Rate von 1/18 ≈ 5,5 % liefert ein 6-Lauf-Vorher mit rund
      **71 %** Wahrscheinlichkeit null Fehlschläge, *auch wenn die Rate identisch ist*
      (`0,945⁶ ≈ 0,71`). Fisher über 0/6 gegen 1/18 gibt **p ≈ 1**. Ich habe also Wandzeit gegen
      ein Ergebnis getauscht, das unter beiden Hypothesen gleich wahrscheinlich war — eine
      Messung, die sich wie Erkenntnis anfühlt und keine ist. Für eine Unterscheidung bräuchte
      es grob **50+ Vorher-Läufe**.

      **Die Lehre gehört zu „billig falsifizieren" dazu:** vor dem Start prüfen, ob der Aufbau
      bei der *erwarteten Effektgröße* überhaupt trennen kann. Ein Bisect über die Last ist
      billig im Engineering und teuer in Wandzeit — genau dort lohnt die Rechnung vorher.

      **Was bleibt.** Meine Entlastung („trat auch vorher auf") war unbelegt und ist es
      weiterhin; die Messung hat sie weder gestützt noch widerlegt. Der Unterschied zu vorher ist
      nur, dass ich das jetzt **weiss**.

      **Was fehlt, und das ist der eigentliche Mangel:** ich habe **kein Protokoll** eines
      Fehlschlags. `tools/`-Nachbau steht (`armfang.sh` im Scratchpad hält das volle Log bei
      Abweichung), hat aber in 12 Läufen nicht ausgelöst. Ohne die fehlschlagende Zeile ist jede
      Ursachenvermutung Prosa.

      **Die drei Ausfälle gehören wahrscheinlich zusammen — und das Prior liegt beim GERÜST.**
      aarch64 unter Parallellast, x86-Lade-Suite bei 512M sequenziell, x86 `RUNS=8` mit einem
      Protokoll, dessen **Signatur mit der eines grünen Laufs identisch** ist. Der letzte Befund
      ist der aussagekräftigste: wenn alle Ergebniszeilen stimmen und der Lauf trotzdem
      durchfällt, bricht eine Prüfung, die **nichts mit der geprüften Eigenschaft zu tun hat** —
      abgeschnittene Ausgabe, Zeitlimit im Erwartungsabgleich, ein verlorenes letztes Zeichen.

      In dieser Sitzung wurde der Messaufbau **dreimal** als schuldig überführt (`tail -1`; drei
      Suiten ohne Protokoll; ein Rückhalteblock hinter der Löschung). Nach drei Treffern ist die
      naheliegende Hypothese nicht mehr „drei seltene Kernelfehler", sondern **ein Gerüstfehler**.
      Die drei zusammen zu behandeln und zuerst dort zu suchen, ist billiger als drei getrennte
      Jagden.

      **Das Prior braucht einen AUSSTIEG, sonst kippt es.** „Zuerst das Gerüst prüfen" ist nach
      fünf Treffern richtig — aber ohne vorab festgelegtes Kriterium wird daraus in drei Wochen
      „war bestimmt wieder das Gerüst", dieselbe unfalsifizierbare Bequemlichkeit wie „trat auch
      vorher auf", nur mit umgekehrtem Vorzeichen. Deshalb **jetzt** hingeschrieben, solange das
      Prior noch nicht liebgewonnen ist:

      **Das Kriterium hängt an der FORM des Artefakts, nicht an der Wiederholbarkeit.** Die erste
      Fassung verlangte eine *reproduzierbare* Prüfzeile — und hätte damit genau die Klasse
      ausgeschlossen, um die es hier geht: ein verpasstes `WFE`-Wakeup oder ein Timer/IPI-Rennen
      bei 1 in 32 reproduziert nicht und wäre auf ewig unter „Gerüst" gefallen. Das wäre dem
      Prior eine Tür gegeben, durch die es nicht widerlegt werden kann — dieselbe Bequemlichkeit
      wie „trat auch vorher auf", nur mit umgekehrtem Vorzeichen.

      Ein Ausfall ist ein **Kernel-Befund**, wenn **alle drei** Merkmale zutreffen — ein
      **einzelner** Lauf kann das erfüllen:
      1. die Ausgabe ist **vollständig bis zum Ende** (Schlusszeile vorhanden),
      2. der **Exit-Code passt zum Urteil** (kein `exit 0` mit `FAILURES`, kein Abbruch),
      3. eine Prüfzeile **weicht inhaltlich ab** — Werte, Zähler oder Reihenfolge sind anders,
         nicht bloss kürzer oder fehlend.

      Alles andere — abgeschnittene Ausgabe, fehlende Schlusszeile, Zeitlimit im
      Erwartungsabgleich, Exit-Code ohne passendes Urteil — ist bis auf Weiteres ein
      **Gerüst-Befund** und wird dort gesucht.

      **Wiederholbarkeit gehört in die Priorisierung, nicht in die Klassifikation.** Ein
      einmaliger Kernel-Befund ist ein Kernel-Befund; dass er schwerer zu jagen ist, ändert
      nichts daran, was er ist.

      **Nicht als „Umgebung" ablegen.** Ein Fehler, der nur bei Überbuchung des Wirts auftritt,
      ist ein Kandidat für ein verpasstes `WFE`-Wakeup oder ein Timer/IPI-Rennen — zeitabhängige
      Kernelfehler leben genau dort, und die Emulation verschiebt nur die Wahrscheinlichkeit,
      nicht die Ursache. Verwandt mit D0 (x86) und D6 (aarch64), aber mit einem eigenen,
      **reproduzierbaren Auslöser**: Last.

      **Dasselbe Bild auf x86, und derselbe Mangel.** Die **Lade-Suite bei 512M** fiel am
      2026-08-04 zweimal aus — beide Male innerhalb eines längeren Sammellaufs, isoliert
      danach 6 von 6 grün (und davor schon 3 von 3). Auch dafür habe ich **kein Protokoll**.

      **Die Protokolle waren nicht „von meiner Schleife weggeworfen" — es gab sie nie.** Ich
      hatte geschrieben, die Suiten legten bei Abweichung selbst ein Log ab und nur meine
      Sammelschleife habe es verworfen. Nachgesehen (2026-08-05): das stimmt für **keine** der
      drei. Die Lade-Suite benutzte ein `mktemp` und löschte es am Ende **bedingungslos**; die
      x86-Suite löschte ihres direkt nach dem Einlesen, lange vor den Prüfungen; die ARM-Suite
      hob nur bei abweichender **Signatur** eines Wiederholungslaufs etwas auf. In `build/diag/`
      liegt nichts vom 2026-08-04.

      **Behoben, und die Rückhaltung ist gefahren worden.** Alle drei Suiten legen jetzt bei
      `fail != 0` das volle Protokoll unter `build/diag/` ab. Der erste Anlauf war dabei selbst
      ein stummer Prüfer: der Block stand am Dateiende, das Log war zu dem Zeitpunkt aber schon
      gelöscht — er **konnte** nie feuern. Gegenprobe mit erzwungenem Fehlschlag gefahren: 176
      Zeilen (x86-Suite) bzw. 193 Zeilen (Lade-Suite) abgelegt.

      **Die Rückhaltung hat sofort gefeuert — und dabei das nächste Loch gezeigt.** Am
      2026-08-05 fiel `RUNS=8` auf x86 aus; das Kernel-Protokoll lag diesmal vor
      (`build/diag/ABWEICHUNG-…`, 176 Zeilen). Ergebnis der Auswertung:

      * die **Signatur** des abgelegten Laufs ist **identisch** mit der eines grünen Laufs
        (diff leer) — der Fehlschlag war also **keine** Signaturabweichung,
      * es entstand auch kein `abweichung-lauf-N.log`, der Wiederholungsvergleich schlug also
        ebenfalls nicht an,
      * womit die durchgefallene Prüfung eine der **abgeleiteten** sein muss (Zeitlimit,
        `checks`-Abschnitt) — und **deren Zeile steht in der stdout der Suite**, die meine
        Sammelschleife wieder nur als `tail -1` festhielt.

      **Dieselbe Lücke, eine Ebene höher.** Die Suite bewahrt jetzt das *Kernel*-Protokoll; was
      fehlte, war die *Urteilsausgabe*. Drei weitere `RUNS=8`-Durchgänge (24 Läufe) danach waren
      grün.

      **Keine Punktschätzung daraus.** „Die Rate liegt bei 1/32" wäre genau der Fehler von
      oben, eine Ebene höher: ein Ausfall in 32 Läufen gibt ein 95-%-Intervall von grob
      **0,5 % bis 16 %** — eine Zahl, die als Baseline notiert wird, macht jede spätere Messung
      unfalsifizierbar. Festhalten lässt sich: **ein Ausfall in 32, Intervall breit, Rate
      unbestimmt.** Und ohne die Prüfzeile ist nicht einmal bekannt, **welche** Aussage bricht.

      **(1) erledigt am 2026-08-05:** `tools/sammellauf.sh` hält die **vollständige stdout** je
      Lauf fest. Zwei eigene Fehler dieser Datei sind dabei aufgefallen, und der zweite hat die
      Bilanz-Frage gedreht:

      * Erfolg war als „`ALL PASS` in der Schlusszeile" definiert — und legte damit die
        Protokolle der **grünen Wächter** ab, die anders schliessen.
      * Eine **leere** Schlusszeile galt als Erfolg. Das hat kein Fehlerbild *verloren*, es hat
        einen **Erfolg erfunden**: ein Lauf, der mitten in der Ausgabe endete, wurde grün
        verbucht. **Damit ist die Grün-Bilanz beschädigt, nicht nur die rote** — und weil bei
        Erfolg gelöscht wurde, liess sich nicht nachzählen, wie oft.

      **Reichweite, und diesmal für ALLE Messreihen dieser Sitzung nachgesehen — nicht nur für
      den neuen Sammler.**

      | Bau | Grün-Kriterium | fehlsicher? |
      |---|---|---|
      | `sammellauf.sh` (1 Lauf, vor der Behebung) | leere Zeile galt als Erfolg | **nein** — `load 6G` betroffen, Wiederholung dann echt grün |
      | `armlast.sh` (aarch64-Bisect, 6+6 Läufe) | `[ "$r" = "== ALL PASS ==" ]`, exakter Zeichenvergleich | ja |
      | `armfang.sh` (12 Läufe) | `grep -q "ALL PASS"` auf der Schlusszeile | ja |
      | die `for M in …`-Schleifen (RAM-Reihen, `RUNS=8`) | letzte Zeile **gedruckt**, von mir gelesen | ja, mit Einschränkung |

      Der Grund, warum die letzten drei tragen, steht im Quelltext der Suiten und wurde
      nachgesehen: `== ALL PASS ==` wird **ausschliesslich** im `fail = 0`-Zweig als letzte
      Ausgabe vor `exit "$fail"` geschrieben. Ein abgebrochener, abgeschnittener oder
      durchgefallener Lauf kann diese Zeile nicht als letzte tragen, und Text und Exit-Code können
      in der grünen Richtung nicht auseinanderlaufen. Die Einschränkung bei den Schleifen ist
      menschlich (ich habe die Zeilen gelesen), nicht mechanisch — und leere Zeilen sind mir
      beide Male aufgefallen.

      **Damit steht die Grün-Bilanz dieser Sitzung**, mit der einen benannten Ausnahme.

      **Zwei Festlegungen daraus.** (A) Der Ausgang entscheidet sich am **Exit-Code**, nicht am
      Text — sonst wächst mit jeder neuen Suite eine Formel und mit ihr ein Loch. Der Text wird
      nur gegengelesen; ein Widerspruch ist ein Befund **über die Suite**. (B) Protokolle werden
      **vorerst auch bei Erfolg behalten**, bis die Datei ein paar Dutzend Läufe getragen hat.
      Fünf Fälle als Gegenprobe gefahren (Erfolg, Wächter-Formel, leere Zeile, `exit 0` mit
      FAILURES, `exit 1`).

      **Zu tun:** (2) alle Sammelläufe über `sammellauf.sh` führen und laufen lassen, bis eine
      Prüfzeile vorliegt; (3) **zuerst das Gerüst prüfen** (s. o.), nicht den Kernel; (4) beide
      Stände mit **je 50+** Läufen messen — alles darunter kann bei dieser Effektgröße nicht
      trennen.

- [ ] **E-Rest 3f: der Abbau leitet die Adresse NEU HER, statt zu konsumieren, was das Abbilden
      zurückgab.** (2026-08-05.) `unmap_dma_from_thread(tid, phys, len)` nimmt dieselben rohen
      Werte noch einmal entgegen und rechnet die VA erneut aus. Dass er dabei dieselbe Achse
      trifft, hält heute ein **Konstruktorname** (`Va::for_dma_window` an beiden Stellen) und der
      Wächter, der ihn zählt — also Auffindbarkeit, nicht Unmöglichkeit.

      **Die strukturelle Fassung:** das Abbilden gibt ein **Handle** zurück, der Abbau konsumiert
      genau dieses Handle. Dann ist die Adresse im Teardown nie wieder ein freier Wert, und die
      Asymmetrie ist **unkonstruierbar** statt auffindbar. Das passt zu den vorhandenen
      Teardown-Token (ext-37) und zum `Owned<T>`-Typestate der virtio-Crate — dieselbe Bauform,
      eine Ebene tiefer.

      **Das Handle muss an die PD gebunden sein**, sonst entsteht die nächste Faltung: ein Handle
      aus PD A, das in PD B abbaut, wäre wieder derselbe Wert mit zwei Bedeutungen. Die Bindung
      gehört in den Typ (ASID oder PD-Id im Handle, vom Abbau geprüft), nicht in eine
      Aufrufkonvention.

      Reihenfolge: nach E-Rest 3e (a), weil der Fenster-Umbau die Signatur ohnehin anfasst.

- [ ] **E-Rest 3e: DMA-Regionen hängen an GiB 0 — und das sind ZWEI Fragen, keine.**
      (2026-08-04, beim VA==PA-Durchgang neu zerlegt; die erste Fassung dieses Eintrags faltete
      beide zusammen und machte den Punkt dadurch größer, als er ist.)

      **(a) Abbildung — dieselbe Arbeit wie E-Rest 3d.** Die Region wird über
      `map_region_into_thread`/`MappingKind::Dma` **identisch** in die Treiber-PD abgebildet
      (`IdentityReason::DeviceDmaWindow`). Das ist die CPU-Seite, und für sie gilt wörtlich, was
      für die private PD-Region galt: das Fenster löst es. Kein neuer Entwurf nötig.

      **(b) Allokation — eine ganz andere Frage, und die eigentliche Lücke.** Ein 32-Bit-fähiges
      Gerät kann nur in die unteren 4 GiB schreiben. Das ist **keine Eigenschaft der Abbildung**,
      sondern eine Einschränkung der **Zuteilung** — und unter VT-d/SMMU löst sie sich sogar auf:
      die **IOVA** kann niedrig sein, während die PA irgendwo liegt. GiB-0-Pinning ist damit eine
      **Politik des Allokators**, die beim IOMMU-Detect gewählt gehört, und keine Eigenschaft der
      Region.

      **Was wirklich fehlt, ist die Ausdrückbarkeit — und sie gehört an die GERÄTE-Seite.** Eine
      `dma_mask` in der **Memory**-Cap wäre dieselbe Faltung eine Achse weiter: die 32-Bit-Grenze
      ist eine Eigenschaft des **Geräts**, nicht der Speicherregion. Stünde sie in der
      Memory-Cap, entstünde Speicher, der „für 32-Bit-Geräte" ist und für nichts anderes taugt —
      ein Angebot, das die Begrenzung seines Konsumenten trägt. (So stand es bis zum 2026-08-05
      in diesem Eintrag; falsch.)

      Richtig: die Einschränkung gehört an die **Geräte-Cap** bzw. an die DMA-Domäne, und die
      Zuteilung ist dann ein **Join** aus Regionsangebot und Gerätebeschränkung — dieselbe Form
      wie „Farbe UND Zone in einer Entscheidung" (E-Rest 3b) und wie Farbe+NUMA (Z8).

      **Der Fehlerfall des Joins — leere Schnittmenge — gehört an die Cap-ABLEITUNG, nicht an den
      DMA-Zeitpunkt.** Eine Geräte-Cap, für die kein zulässiges Angebot existiert, sollte gar
      nicht erst herstellbar sein. Sonst entsteht eine Cap, die aussieht wie Autorität und beim
      ersten Gebrauch scheitert — dieselbe Form wie ein Manifest-Eintrag, dessen Selektor auf kein
      Gerät passt (A-5.3), und dort ist die Antwort schon „abweisen statt raten".

      Heute meldet die Suite die Angabe als Abwesenheit
      (`dmawin : Geraete-ohne-deklarierte-Adressbreite=1`) — ehrlich, aber ein **Zähler für eine
      fehlende Angabe** und keine Angabe. Solange sie fehlt, ist jede Wahl der Basisadresse ein
      Rateschritt: unten liegen heisst „für alle Geräte sicher", dieselbe Sorte Vorsichtsmaßnahme
      wie „unten zuerst" aus E-Rest 3b — sie kostet die knappe Zone und belegt nichts.

      Reihenfolge: **(b) vor (a)**. (a) ohne (b) wäre ein Vertrauensvorschuss an unbekannte
      Hardware — die Region läge dann oben, weil es geht, nicht weil das Gerät es kann.

---

## F. Debug-/Testcode aus dem Release-Build nehmen

**Befund (gemessen):** rund **8 000 der 28 800 Zeilen** — gut ein Viertel — sind Prüfinfrastruktur,
und sie wird bei jedem Start mitkompiliert und ausgeführt:

| | Zeilen |
|---|---|
| `threads/mod.rs` (Demo-Threads + Testdienste) | 5 402 |
| `threads/fuzz.rs` | 1 299 |
| `selftest.rs` | 363 |
| `arch/x86_64/dmar_selftest.rs` | 249 |
| `dmatests.rs` | 186 |
| `testsupport` in `system.rs` | 558 |

Dazu **364 Telemetrie-Statics** (`static X_OK: AtomicBool`), davon 339 in den Testmodulen selbst,
und 234 `println!`-Aufrufe über 731 Zeilen. Der Fuzzer hängt bereits an `kernel-fuzz`; der Rest
nicht.

**Die Abhängigkeit ist einseitig** — geprüft: weder `system.rs` noch `loader.rs` noch irgendeine
Crate ruft in die Testmodule. Es gibt genau **acht** Aufrufstellen (`main.rs`, `bringup.rs`), und
`testsupport` wird außerhalb der Testmodule nur an **einer** Stelle benutzt (`peek_dma_words` in
der virtio-Demo).

- [ ] **F3** Boot-Meldungen (`mmu:`, `apic:`, `mem:`) **nicht** mitgaten — sie sind kein
      Debug-Code, sondern das einzige Lebenszeichen eines Kernels ohne Konsole. Dass die HAL mit
      8 401 Zeilen nur **sieben** `println!` enthält, zeigt, dass die Trennung dort schon
      eingehalten wird; die Grenze verläuft zwischen Bring-up-Meldung und Testbericht.

---

## G. Bekannte Architekturgrenzen (dokumentiert, kein Bug)

- IPC überträgt **4 Registerwörter**; alles Größere über Shared Memory.
- TrustedSAS-PDs teilen einen Adressraum — gewollte Konsequenz der intralingualen Isolation
  (safe Rust kann keinen Zeiger auf fremden Speicher erzeugen); das Zertifikats-Gate erzwingt die
  Voraussetzung (`unsafe_status == ALL_PASS`).
- Tier 3 der Verifikation (funktionale Vollkorrektheit, Info-Flow, HW-Modell) ist Forschungsklasse.
