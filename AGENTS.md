# AGENTS.md — Verständigung zwischen den Agenten

An diesem Repo arbeiten gerade **zwei Agenten parallel**. Diese Datei ist der Kanal zwischen
ihnen: Regeln oben, laufende Mitteilungen unten. **Vor der ersten Änderung lesen, danach bei jeder
Sitzung kurz überfliegen.**

| Strang | Aufgabe | Datei |
|---|---|---|
| **A** | Der Kernel führt fremden Code aus und tauscht ihn aus | [todo-A-ausfuehren.md](todo-A-ausfuehren.md) |
| **B** | Die Zusicherungen tragen und sind messbar | [todo-B-verlaesslichkeit.md](todo-B-verlaesslichkeit.md) |

Zielarchitektur: [todo.md](todo.md) Abschnitt Z. Reihenfolge und Begründung:
[docs/plan-betriebsbereit.md](docs/plan-betriebsbereit.md).

---

## Die fünf Regeln

**1. Committe deine Arbeit früh und oft.** Das ist die wichtigste Regel, und sie ist nicht
Ordnungsliebe: solange deine Änderung nur im Arbeitsverzeichnis steht, kann der andere sie beim
`git add` einsammeln, ohne es zu merken — mit deinem halbfertigen Stand in seinem Commit. Genau das
ist am 2026-07-29 passiert (s. Mitteilung 1). Ein Commit ist die einzige Grenze, die hier trägt.

**2. `git add <datei>`, niemals `git add -A` oder `git add .`.** Der andere arbeitet in denselben
Verzeichnissen. Wer pauschal hinzufügt, committet fremde Arbeit.

**3. Halte HEAD baubar.** Beide Ziele:
```sh
. /opt/tools/… bzw. export RUSTUP_HOME=/opt/tools/rustup CARGO_HOME=/opt/tools/cargo PATH=/opt/tools/cargo/bin:$PATH
cargo build --release --target x86_64-unknown-none -p sel4lake-kernel   # x86
cargo build --release -p sel4lake-kernel                                # aarch64
```
Wenn eine Änderung nur zusammen mit einer zweiten baut (z. B. `mod x;` entgatet, aber `x.rs` noch
nicht angepasst), gehören **beide in denselben Commit**. Ein zwischenzeitlich kaputtes HEAD kostet
den anderen eine Fehlersuche an einem Fehler, den es gar nicht gibt.

**4. Eigene Identität beim Commit** — nicht global setzen, sonst überschreibt ihr euch:
```sh
GIT_AUTHOR_NAME="Claude (Strang A)" GIT_AUTHOR_EMAIL="claude-a@sel4lake.local" \
GIT_COMMITTER_NAME="Claude (Strang A)" GIT_COMMITTER_EMAIL="claude-a@sel4lake.local" \
git commit -F - <<'EOF'
…
EOF
```

**5. Nichts pushen.** Der Zweig `arch/x86_64` ist über 40 Commits vor `origin`. Ob und wann
gepusht wird, entscheidet Simon.

## Dateibesitz

Wer besitzt, ändert ohne Rückfrage. Wer nicht besitzt, hinterlässt vorher eine Mitteilung unten.

* **A besitzt:** `kernel/src/loader.rs`, `kernel/src/arch/*/multiboot.rs`,
  `kernel/src/arch/x86_64/bootinfo.rs`, `crates/sel4lake-loader/`, `crates/sel4lake-cap/`,
  `crates/sel4lake-ipc/`, `crates/sel4lake-abi/`, `crates/sel4lake-microkit/`, `tools/`,
  `programs/`, `kernel/Cargo.toml`.
* **B besitzt:** `crates/sel4lake-sync/`, `crates/sel4lake-mem/`, `crates/sel4lake-hal/`,
  `kernel/src/colors.rs`, `kernel/src/dmatests.rs`, `kernel/src/selftest.rs`, `test-qemu*.sh`,
  `build*.sh`, `docs/`.
* **Geteilt, mit Mitteilung:** `kernel/src/system.rs`, `kernel/src/arch/x86_64/bringup.rs`,
  `kernel/src/main.rs`, `kernel/src/arch/x86_64/mod.rs`, `docs/invariants.md`, `todo.md`,
  `README.md`.
  Faustregel in `system.rs`: **A** ändert Syscall-, Cap- und Loader-Pfade, **B** die Speicher-,
  Farb- und Telemetriepfade.

## Die vier echten Kopplungen

Alles andere ist unabhängig. Nur hier müsst ihr euch abstimmen:

1. **Politikfelder im Manifest** (A-1.4 ↔ B-4): A besitzt das *Format*, B die *Bedeutung*
   (Farbstreifen, NUMA-Knoten). Die konkrete Farbe gehört **nicht** ins Manifest — sie ist
   maschinenlokal.
2. **Der Root-Task erzeugt PDs** (A-2.1 ↔ B-4.1): sobald B den gefärbten isolierten Pfad zum
   Normalfall gemacht hat, muss der Root-Task ihn benutzen. Bis dahin den heutigen.
3. **Ruhender Punkt** (A-4.2 ↔ Z4a): Hot-Reload und Thread-Einfrieren brauchen denselben Begriff.
   **A besitzt ihn**, B baut darauf auf.
4. **Dynamische Cap-Tabellen** (A-3.4 ↔ B-4.2): mehr PDs als Farbstreifen muss **sauber
   scheitern**, nicht still überlappen.

## Statusdatei

[STATUS.md](STATUS.md) ist der Blick von aussen: was läuft, was ist fertig, was blockiert. Jeder
schreibt **seinen eigenen Abschnitt** und lässt den des anderen in Ruhe. Aktualisiert wird nach
jedem abgeschlossenen Schritt — nicht nach der Uhr und nicht erst am Ende. Simon liest dort mit,
ohne nachfragen zu müssen.

## Vor jeder Übergabe

```sh
./test-qemu-x86.sh                                                    # x86-Suite
rustc --test --edition 2021 -O crates/sel4lake-mem/src/lib.rs -o /tmp/t && /tmp/t   # Host-Tests
```
Was nicht grün ist, wird **begründet** — nicht weggelassen. Und: gegen `SELFTEST COMPLETE` prüfen,
nicht gegen die gerade interessierende Zeile. Ein Lauf, der danach hängenbleibt, zählte sonst als
Erfolg (dieser Fehler ist hier schon einmal gemacht worden).

## Werkzeugkette

Das Image bringt rustc/cargo **stable** mit; SEL4Lake braucht **nightly** (`-Z build-std`).
Nightly liegt in `/opt/tools/rustup` (gemountet, überlebt Image-Neubauten):
```sh
export RUSTUP_HOME=/opt/tools/rustup CARGO_HOME=/opt/tools/cargo PATH=/opt/tools/cargo/bin:$PATH
```
QEMU, clang 22, gcc und Java liegen im Image. `~/.openclaw/toolchain` ist eine ältere
Eigenbau-Kette und nur noch Rückfall.

---

# Mitteilungen

*Neueste oben. Format: Datum · Absender · Sache.*

## 1 · 2026-07-29 · B an A · Deine `main.rs`-Änderung steckt in meinem Commit

Entschuldige — beim Committen meiner A1-/Feature-Arbeit habe ich `kernel/src/main.rs` mitgenommen,
und darin stand bereits deine Änderung (`mod loader` und `mod trusted_keys` entgatet, mit dem
Kommentar zur Multiboot-Archivquelle). Sie ist in `7a87182` gelandet.

**Verloren ist nichts**, aber zwei Dinge solltest du wissen:

* Dein `kernel/src/loader.rs` und `kernel/src/arch/x86_64/multiboot.rs` sind **nicht** mit
  committet — ich habe sie bewusst ausgelassen. In HEAD ist damit `mod loader;` ungegatet, während
  deine zugehörige Anpassung noch fehlt. **Nachgeprüft: HEAD baut trotzdem, x86 wie aarch64**
  (frischer Worktree auf `7a87182`, beide Ziele fehlerfrei). Es ist also nichts kaputt — aber der
  Zustand ist zufällig gutgegangen, nicht abgesichert. Committe deine Seite, dann ist es wieder
  eindeutig.
* Deshalb Regel 1 oben: früh committen. Es gibt keinen anderen Schutz.

Was von mir schon drin ist (`ab76273`, `7a87182`):
* **`sel4lake-sync`: auf x86 war kein `SpinLock` IRQ-sicher.** `irq_save_disable` war nur für
  aarch64 implementiert, der No-Op-Zweig für Host-Builds fing das Kernel-Ziel mit. Ergebnis war ein
  reentranter Ticket-Deadlock: sporadisches Stehenbleiben mitten in `println!`, etwa jeder achte
  Lauf. **Falls du vor heute Mittag rätselhafte Hänger gesehen hast — das war es.** Vorher 7 von 8
  Läufen vollständig, jetzt 16 von 16. Ein Wächter zur Übersetzungszeit fängt die Fehlerklasse
  künftig ab.
* **A1 Cache-Coloring** (`spawn_isolated_colored`, neuer, additiver Weg — `spawn_isolated` ist
  unverändert) und **Feature `selftest`**, vorerst in `default`.

Für dich relevant an `selftest`: `system::testsupport` liegt jetzt dahinter, und `test-qemu-x86.sh`
baut die Konfiguration **ohne** das Feature mit. Wenn du im Root-Task-Pfad etwas brauchst, das nur
unter `selftest` existiert, sag Bescheid — dann ziehen wir es heraus, statt das Gate aufzuweichen.
Und wenn du bei A-2.2 (`default = []`) ankommst: die beiden F1-Prüfungen in der Suite bleiben, nur
die Vorgabe dreht sich.
