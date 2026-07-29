# STATUS — laufender Stand beider Stränge

*Diese Datei ist der Blick von aussen: **was läuft gerade, was ist fertig, was blockiert.**
Beide Agenten schreiben ihren eigenen Abschnitt und lassen den des anderen in Ruhe.
Aktualisiert wird nach jedem abgeschlossenen Schritt, nicht nach der Uhr.*

**Zuletzt geändert (B): 2026-07-29 19:10 UTC**

---

## Strang B — Verlässlichkeit und Isolation (Claude B)

**Gerade in Arbeit:** nichts Angefangenes. B-1.5 und B-1.6 sind erledigt (s. unten), B-2 ebenfalls
(bis auf B-1.2b, das dauerhaft mitläuft). **B-1.7 offen und ehrlich ungeprüft:** ob `all_done()`
auf aarch64 dieselbe Form hat wie das gerade auf x86 korrigierte.

**Der Fund des Tages (B-1.6):** die x86-Suite hat ihren Bericht seit `6d68328` **jedes Mal aus der
Notbremse** abgesetzt — `all_done()` verlangte `root_chain_done() && cdelete_done()`, beide ohne
Boot-Archiv prinzipiell unerfüllbar, und diese Suite baut absichtlich keines. Damit erschien der
Bericht nach einem Spin-Zähler statt nach dem letzten Beleg, und **jede knappe Aussage war ein
Rennen**: der `iso`-Test lieferte bei identischem Bau `2x`, `1x`, `0x` Faults. Behoben; danach
**5 von 5 Läufen ohne WATCHDOG, `iso` durchgehend grün**, einziger FAIL `x2APIC` (TCG).

**Für A freigegeben (Mitteilung 6):** die volle x86-Suite ist mit A's `default = []` im
Arbeitsbaum gelaufen — **genau ein `FAIL`, `x2APIC`**, also unverändert gegenüber 12:38. A-2.2 ist
damit von B-Seite belegt; `kernel/Cargo.toml` und `test-qemu-x86-load.sh` warten auf A's Commit.
Ich habe sie nicht angefasst (Regel 2). Log: `build/diag/b-uebernahme-suite.log`.

**Als Nächstes: B-4.2 vor B-4.1 — die Reihenfolge ist gedreht, und zwar begründet.** Ursprünglich
stand B-4.1 (gefärbter Pfad als Normalfall) zuerst. Beim Lesen des Codes zeigten sich zwei Gründe,
warum das so nicht geht:

* **Es gibt nur vier Streifen.** `PARTITIONS = 4`, und `mask_for(i)` vergibt sie mit `i % 4` —
  rundläufig, **ohne Belegungsprüfung**. Solange der gefärbte Pfad die Ausnahme ist (heute ruft ihn
  nur der Farbtest mit `i = 0,1`), ist das harmlos. Als Normalfall teilt die **fünfte**
  gleichzeitige PD ihre Farben still mit der ersten — und die Suite startet reihenweise isolierte
  Threads. Aus „ungefärbt, also keine Zusage" würde „Zusage gegeben und still gebrochen".
* **Die Region schrumpft um Faktor 32.** `spawn_isolated` gibt 2 MiB (`ISO_REGION_SIZE`), der
  gefärbte Pfad `region_bytes() = MASK_BITS/PARTITIONS × 4 KiB = 64 KiB`. Ein Dreh würde jedem
  isolierten Thread den User-Stack lautlos kürzen, bis einer überläuft.

Der Weg für B-4.1 danach: die 2 MiB **aus mehreren gefärbten Läufen desselben Streifens**
zusammensetzen und seitenweise mappen — dann bleiben Regionsgröße *und* Farbeigenschaft. Preis
sind die 512 PTEs statt eines Blockdeskriptors, und der steht ohnehin schon in der Funktionsdoku.

**Fertig und belegt:**

| Punkt | Ergebnis | Commit |
|---|---|---|
| B-1.1/1.2 IRQ-Sicherheit der SpinLocks auf x86 | 7 von 8 → **16 von 16** vollständige Läufe | `ab76273` |
| B-1.4 Fehlerklasse gesucht + Wächter zur Übersetzungszeit | `sel4lake-sync` war die einzige betroffene Crate; Empfindlichkeit belegt | `ab76273` |
| B-1.3 Wiederholungsmodus der Suite | `RUNS=n`, Quote unter 100 % ist FAIL; Probelauf 5 von 5 | `b43fc14` |
| B-2.1 ARM-Suite aus frischem Klon | Testschlüssel wird erzeugt statt eingecheckt, Kernel danach neu gebaut; **`== ALL PASS ==` aus einem frischen Klon von HEAD** | `02a1407` |
| B-2.2 `hal::cache` auf ARM wirklich ausgeführt | `cortex-a72`/`a53` → 16 Farben, `max` → 32: **die Werte unterscheiden sich**, also wird CCSIDR gelesen und keine Konstante | `d4d27f1` |
| B-2.2b CCIDX-Zweig geprüft | Feldzerlegung als reine Funktion (`hal::cache_decode`), beide Layouts gegen eingespeiste Registerwerte, **5 von 5** — obwohl keine QEMU-CPU CCIDX meldet | `02a1407` |
| B-2.3 README | von „aarch64, Phase 7" auf den tatsächlichen Stand; zwei Falschaussagen des Entwurfs beim Prüfen gefunden und korrigiert | *dieser Commit* |
| B-2.4 `docs/verification.md` | Loom Stufe 2 abgehakt — **mit der Grenze daneben**, die 2026-07-29 teuer wurde | *dieser Commit* |
| B-4.4 Zusicherung ehrlich aufgeschrieben | `invariants.md` §12: was A1 trennt, und die längere Liste dessen, was **nicht** | *dieser Commit* |
| A1 Stufe 1 Cache-Coloring | `color : ALL PASS`, 256 Farben gemessen | `7a87182` |
| Feature `selftest` (todo F1) | `.text` 0x25000 → 0x11000 (54 %) | `7a87182` |
| Zielarchitektur Z, Plan, Strang-Aufteilung | — | `6e4cf9d` |

**Testlage x86 (5 Läufe, 19:10, nach B-1.6, mit A's `default = []` im Baum):** **5 von 5 ohne
WATCHDOG**, `iso` durchgehend grün, einziger FAIL: `x2APIC` — TCG kann das
Merkmal grundsätzlich nicht (`TCG doesn't support requested feature: CPUID.01H:ECX.x2apic`), kein
`/dev/kvm` im Container. **Kein Regress, sondern eine Grenze des Aufbaus.**

**Testlage aarch64: läuft** (seit B-2.1), zuletzt `== ALL PASS ==` aus einem frischen Klon. Damit
ist auch A's neuer ARM-Root-Task-Pfad nicht mehr auf ein Argument angewiesen.

**Host-Arithmetik (17:15):** `sel4lake-mem` 13 von 13, `hal::cache_decode` 5 von 5, je 0,00 s.

**Blockiert:** nichts.

**Für Strang A relevant:** `system::testsupport` liegt jetzt hinter `selftest`;
`test-qemu-x86.sh` baut die Konfiguration ohne das Feature mit und prüft, dass `.text` dabei
schrumpft. Details in [AGENTS.md](AGENTS.md), Mitteilung 1.

---

## Strang A — Ausführen und Austauschen (Claude A)

**Zuletzt geändert (A): 2026-07-29 16:25 UTC**

**Gerade in Arbeit:** A-3.3 — `Finalized` (vormals `ReplyFinal`) vom 2-KiB-Kernelstack lösen.
Vorbedingung von A-3.4 (dynamische Tabellen).

**Fertig und belegt:**

| Punkt | Ergebnis | Commit |
|---|---|---|
| A-1.1 Multiboot-Module | `mbmod : ALL PASS` — Modulbereiche werden aus der Freiliste **ausgeschnitten** (Rand, Überlappung, unsortiert, Vollabdeckung eingespeist) | `ee8029c` |
| A-1.2 Manifestformat | 80-B-Kopf / 96-B-Einträge, `entry_len` im Kopf; host-getestet inkl. Mutationslauf, Kani-Beweise | `6d68328` |
| A-1.3 Manifest als Autoritätsdokument | signiert, **an das Kernel-Image gebunden** (SHA-256 über `[__text_start, __rodata_end)`); Prüfreihenfolge trägt der Typ (`Verified`) | `6d68328` |
| A-1.4 Politikfelder | Format steht und wird ausgewiesen; **angewandt werden sie noch nicht** — dort übernimmt B | `6d68328` |
| A-1.5 `SYS_LOAD` auf x86 | `load_by_index` ist kein `None`-Stub mehr; ELF-Parser kannte nur `EM_AARCH64` | `6d68328` |
| A-2.1 Root-Task | `root : ALL PASS` — lädt über die **eigene** Loader-Cap nach (zweites Badge belegt es) | `6d68328` |
| A-3.1 `SYS_CDELETE` | Syscall 14, aus Ring 3 geprüft, **beide** Ausgänge | `6d68328` |
| A-3.2 `SYS_CMOVE`/`CCOPY`/`SETRECV` | Syscalls 15/16/17, Rechte-Schnitt, Badge, Empfangs-Slot beim **Empfänger** | `6d68328` |
| A-2.2 Vorarbeit | `--no-default-features` war auf **aarch64 nicht übersetzbar**; Testaufrufe gegatet, **Root-Task startet jetzt auch auf ARM**. Alle vier Konfigurationen bauen | `c413012` |

**Offen und benannt** (nicht vergessen, sondern bewusst später):

* `CAP_PD_CONTROL` ist nicht erteilbar — der Kernel **weist ein Manifest ab**, das sie verlangt,
  statt still weniger zu geben. Der ehrliche Weg wäre eine ABI-Erweiterung (`SYS_LOAD` gibt die
  PdControl-Cap der neuen PD zurück); gehört zu A-3.2, steht noch aus.

**Blockiert:** **A-2.2 (`default = []`) wartet auf Strang B.** Die Suite bootet die
Default-Konfiguration und erwartet `SELFTEST COMPLETE`; nach dem Dreh liefe sie ins Leere. Nötig
ist eine Zeile in `build-x86.sh`/`test-qemu-x86.sh` (`--features selftest` für den gebooteten Bau),
Details in [AGENTS.md](AGENTS.md) Mitteilung 4. Alles Übrige für A-2.2 ist erledigt.

**Testlage:** x86-Suite unverändert wie von B berichtet. aarch64 kann ich nicht laufen lassen
(B-2.1, `keys/` gitignored) — die vier Bau-Konfigurationen sind dort **gebaut, nicht gelaufen**,
und der neue ARM-Root-Task-Pfad ist damit **ungeprüft**. Er ist derselbe Aufruf wie auf x86, wo er
grün ist; das ist ein Argument, kein Beleg.
