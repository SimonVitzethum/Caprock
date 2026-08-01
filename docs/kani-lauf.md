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

## Prüfliste nach jedem Lauf

* Stehen **vier** `== Kani: … ==`-Zeilen im Log? (Sonst wurden Ziele übersprungen.)
* Steht am Ende `Complete - N successfully verified harnesses, 0 failures`?
* Bei `VERIFICATION:- FAILED`: **erst die Zeile darüber lesen.** `Status: FAILURE` = echter Befund,
  `CBMC failed` = unentschieden.
* Gab es `unsupported constructs`-Warnungen, und war die Stelle erreichbar?
