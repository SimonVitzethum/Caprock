# Kani lokal fahren — Anleitung und Befund vom 2026-07-31

Diese Datei beschreibt, **wie** man die Kani-Beweise fährt, **was ein Lauf aussagt** und **was am
2026-07-31 dabei herauskam**. Sie existiert, weil ein Beweiser zwei Arten von „nicht grün" kennt,
die man nicht verwechseln darf — und weil der Lauf an diesem Tag beide Fallen gestellt hat.

Gegenstück: [verification.md](verification.md) (Übersicht aller Verifikationsstufen).
Skript: [`tools/kani-verify.sh`](../tools/kani-verify.sh).

---

## Der Aufruf

```sh
export RUSTUP_HOME=/opt/tools/rustup CARGO_HOME=/opt/tools/cargo PATH=/opt/tools/cargo/bin:$PATH

bash tools/kani-verify.sh                 # ALLE vier Ziele
bash tools/kani-verify.sh loader          # nur eines
bash tools/kani-verify.sh loader --harness elf::kani_proofs::segments_are_sound
```

**`bash`, nicht `sh`.** Das Skript beginnt mit `#!/usr/bin/env bash` und benutzt `local` sowie
Arrays. Unter `dash` lief am 2026-07-31 nur **ein** Ziel statt vier — und der Rückgabewert war
trotzdem `0`. Ein Lauf, der drei Viertel der Arbeit auslässt und Erfolg meldet, ist die
gefährlichste Sorte grün. Wer den Aufruf ändert, prüft danach, dass vier `== Kani: … ==`-Zeilen im
Log stehen.

**Laufzeit und Speicher.** Der volle Lauf dauerte rund fünfzig Minuten. CBMC ist der
Speicherfresser, nicht Kani: der schwerste Harness kam auf **3 752 561 Variablen und 24 504 554
Klauseln**. 15 GiB RAM haben dafür nicht gereicht (s. u.).

### Warum das Skript Crates kopiert

Der Workspace erzwingt über `.cargo/config.toml` ein Custom-Target mit `-Z build-std` (für den
bare-metal-Kernel). Kani braucht das **Host**-Target. Das Skript kopiert deshalb die Zielcrate samt
ihrer workspace-lokalen Pfad-Abhängigkeiten nach `$TMPDIR` und schreibt dort ein Minimal-`Cargo.toml`
ohne `.cargo/config`. Das ist kein Umweg, sondern die Bedingung dafür, dass Kani überhaupt läuft.

---

## Was ein grüner Kani-Lauf aussagt — und was nicht

Bewiesen wird **Panik-, Overflow- und Zeigerfreiheit** über *symbolischen* Eingaben, dazu die im
jeweiligen Harness formulierten Struktur-Invarianten. Das ist stark: es deckt alle Eingaben
innerhalb der gesetzten Schranken ab, nicht nur die, an die jemand gedacht hat.

**Kani modelliert keine Nebenläufigkeit.** Das steht im Werkzeug-Output selbst und gilt besonders
für `sel4lake-sync`:

```
warning: Kani currently does not support concurrency. The following constructs will be treated as
sequential operations: atomic_xadd (5), atomic_and (5)
```

Für einen Ticket-Spinlock heißt das: bewiesen ist, dass der Code für sich genommen nicht paniert,
nicht überläuft und keinen ungültigen Zeiger dereferenziert — **nicht**, dass er unter echtem
Wettlauf korrekt ist. Dafür steht Loom daneben (B-7.2), und auch Loom hat seine Grenze: es
modelliert eine *Kopie* des Algorithmus, ein Fehler in der `cfg`-**Auswahl** ist für beide
unsichtbar. Genau dort lag B-1.1 (der x86-IRQ-Deadlock) — von Kani nicht findbar, von Loom nicht
findbar, und trotzdem real.

**Ein `foreign function`-Hinweis ist ein Vorbehalt, keine Nebensache:** erreicht die Verifikation
eine solche Stelle, schlägt sie fehl. Im Lauf vom 2026-07-31 war die eine gemeldete Stelle nicht
erreichbar.

---

## Die zwei Arten von „nicht grün"

| Ausgang | Bedeutung | Wie er aussieht |
|---|---|---|
| **Widerlegt** | Kani hat ein Gegenbeispiel. Der Code ist falsch. | `Status: FAILURE` mit `Description:` und `Location:`, dazu eine Trace |
| **Unentschieden** | Der Beweiser kam nicht zum Ende. Über den Code ist **nichts** gesagt. | `CBMC failed`, meist mit `CBMC appears to have run out of memory` |

**Beides erscheint als `VERIFICATION:- FAILED`.** Wer nur darauf schaut, hält einen
Ressourcenabbruch für einen Fehler im Kernel — oder, schlimmer, gewöhnt sich daran und übersieht
später einen echten. Die Zeile darüber entscheidet, welcher Fall vorliegt.

„Nicht bewiesen" ist nicht „widerlegt". Und **keines von beiden** darf als grün durchgehen.

---

## Befund vom 2026-07-31

Voller Lauf, alle vier Ziele, `bash tools/kani-verify.sh`:

| Ziel | Harness | Ergebnis |
|---|---|---|
| loader | `archive::parse_never_panics` | bewiesen |
| loader | `cert::parse_partitions_input` | bewiesen |
| loader | `cert::parse_never_panics` | bewiesen |
| loader | `elf::segments_are_sound` | **unentschieden** (CBMC out of memory) |
| loader | `elf::parse_never_panics` | bewiesen |
| loader | `manifest::parse_partitions_and_entries_are_safe` | bewiesen |
| loader | `manifest::parse_never_panics` | bewiesen |
| sync | drei Harnesses, 105 Prüfungen | bewiesen, 0 Fehler |

Gesamt: **6 von 7 Harnesses des Loaders bewiesen, einer unentschieden**; `sync` vollständig.

### Der unentschiedene: `elf::kani_proofs::segments_are_sound`

```
Solving with CaDiCaL 2.0.0
3752561 variables, 24504554 clauses
CBMC failed
CBMC appears to have run out of memory. You may want to rerun your proof in an environment with
additional memory or use stubbing to reduce the size of the code the verifier reasons about.
```

Der Beweis ist damit **offen** — und er war es vorher auch, ohne dass es jemand bemerkt hat, weil
Kani ausschliesslich im CI-Gate lief (das ist der Kern von B-7.1). Die Panik-Freiheit des
ELF-Parsers ist unabhängig davon bewiesen (`elf::parse_never_panics`); offen ist die
Segment-Soundness.

**Drei Wege, in der Reihenfolge, in der ich sie für richtig halte:**

1. **Schranken enger fassen** — kleinere symbolische Eingabe, engeres `kani::unwind`. Der Beweis
   wird *schwächer*, aber er wird zu einem **Beweis**. Das ist der einzige Weg, an dessen Ende eine
   wahre, überprüfbare Aussage steht.
2. **Mehr Speicher** — verschiebt die Grenze, löst nichts. Als Messung trotzdem nützlich: wenn er
   bei 64 GiB durchgeht, weiss man, wo man steht.
3. **Stubbing** — versteckt am meisten. Nur mit ausdrücklich notierter Annahme, sonst ist der
   Beweis eine Aussage über einen Code, der so nicht existiert.

Wer Weg 1 geht: die neue Schranke **gehört neben den Harness geschrieben**, samt dem, was sie
ausschliesst. Ein Beweis mit unbenannter Schranke ist eine Zusage, deren Reichweite niemand kennt.

---

## Befund vom 2026-08-01: `sync` ist grün — und sagt über ext-29 nichts

D1 stand als „Kani lokal nicht ausführbar (nur CI-Gate) — die ext-29-Änderung an
`sel4lake-sync` ist dort **nicht** gegengeprüft worden". Die erste Hälfte ist erledigt: Kani
0.67.0 läuft lokal, `bash tools/kani-verify.sh sync` liefert

```
Complete - 3 successfully verified harnesses, 0 failures, 3 total.
```

in zusammen 0,3 Sekunden. Die zweite Hälfte ist damit aber **nicht** erledigt, sondern zum
ersten Mal belegt — und der Beleg steht im Lauf selbst.

### Was der grüne Lauf ausschließt

Kani baut für das **Host**-Ziel; im Log steht 287-mal `x86_64-unknown-linux-gnu`. Damit greift
in `sel4lake-sync` der dritte `cfg`-Zweig:

```rust
#[cfg(not(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "none"))))]
const IRQ_MASKING_IMPLEMENTED: bool = false;      // und irq_save_disable() = No-Op
```

Der Lauf prüft also eine Fassung, in der die Interrupt-Maskierung **nichts tut** — genau die
Eigenschaft, die ext-29 geändert hat und deren Fehlen als B-1.1 den x86-IRQ-Deadlock
verursachte.

Der Wächter, der so etwas fangen soll, kann hier nicht greifen:

```rust
#[cfg(target_os = "none")]
const _: () = assert!(IRQ_MASKING_IMPLEMENTED, "Bare-Metal-Ziel ohne Interrupt-Maskierung …");
```

Er ist selbst an `target_os = "none"` gebunden — richtig so, denn ein Host-Bau hat legitim keine
Maskierung. Die Folge ist trotzdem, dass im Kani-Bau **niemand** die Zusage prüft.

**Das Werkzeug hat es sogar gesagt**, und niemand hat es als Reichweitenaussage gelesen:

```
warning: constant `IRQ_MASKING_IMPLEMENTED` is never used
   --> src/lib.rs:109:7
```

Eine unbenutzte Konstante ist hier keine Unordnung, sondern der Nachweis, dass der Wächter im
geprüften Bau nicht existiert.

### Was tatsächlich bewiesen ist

Die drei Harnesses tragen ihre Grenze im Namen — sie sind ausdrücklich als **single-thread**
dokumentiert:

| Harness | Aussage |
|---|---|
| `spinlock_roundtrip` | Guard-Deref speichersicher, Ticket-Zustand nach `Drop` wieder frei, Daten persistieren |
| `rwlock_write_then_read` | Schreiben dann Lesen konsistent, Zustand danach exakt `0` |
| `rwlock_state_arithmetic` | Leserzahl ohne Über-/Unterlauf, `fetch_and` löscht nur das WRITER-Bit |

Das ist Speichersicherheit und Arithmetik der Datenstruktur. Es ist **keine** Aussage über
Wettläufe (Kani modelliert keine Nebenläufigkeit) und **keine** über IRQ-Sicherheit (im
geprüften Bau nicht vorhanden).

### Warum das die bekannte Lücke schärft

`verification.md` sagt bereits: Loom modelliert eine *Kopie* des Algorithmus, ein Fehler in der
`cfg`-**Auswahl** ist für Loom wie für Kani unsichtbar. Der Lauf vom 2026-08-01 macht daraus
eine Messung: es ist nicht nur so, dass Kani den Fehler nicht *fände* — die geprüfte Fassung
**enthält die Eigenschaft gar nicht**. Ein Prüfer, der über die Abwesenheit eines Fehlers
entscheidet, muss belegen können, dass er sprechfähig ist. Hier ist er es nachweislich nicht.

### Was daraus folgt (offen, s. `todo.md` D1)

Ein Kani-Lauf gegen `sel4lake-sync` wird die ext-29-Eigenschaft **nie** abdecken, solange er
auf dem Host-Ziel baut. Zwei Wege, beide noch nicht beschritten:

* Einen Harness bauen, der `irq_save_disable`/`irq_restore` als *Modell* mitführt statt sie
  wegzu-`cfg`-en, und die Reentranz aus dem IRQ-Pfad als Eigenschaft formuliert.
* Oder: ausdrücklich festhalten, dass diese Eigenschaft **nicht** von Kani getragen wird,
  sondern allein vom Wächter zur Übersetzungszeit plus dem Lauftest (B-1.2/B-1.4) — und im
  CI-Gate danebenschreiben, damit ein grünes Kani nicht mehr verspricht, als es prüft.

---

## Prüfliste nach jedem Lauf

* Stehen **vier** `== Kani: … ==`-Zeilen im Log? (Sonst wurden Ziele übersprungen.)
* Steht am Ende `Complete - N successfully verified harnesses, 0 failures`?
* Bei `VERIFICATION:- FAILED`: **erst die Zeile darüber lesen.** `Status: FAILURE` = echter Befund,
  `CBMC failed` = unentschieden.
* Gab es `unsupported constructs`-Warnungen, und war die Stelle erreichbar?
