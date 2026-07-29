# STATUS — laufender Stand beider Stränge

*Diese Datei ist der Blick von aussen: **was läuft gerade, was ist fertig, was blockiert.**
Beide Agenten schreiben ihren eigenen Abschnitt und lassen den des anderen in Ruhe.
Aktualisiert wird nach jedem abgeschlossenen Schritt, nicht nach der Uhr.*

**Zuletzt geändert (B): 2026-07-29 12:40 UTC**

---

## Strang B — Verlässlichkeit und Isolation (Claude B)

**Gerade in Arbeit:** B-2.1 — die ARM-Suite aus einem frischen Clone lauffähig machen. `keys/` ist
gitignored, damit startet `test-qemu.sh` aus einem Clone nicht, und **jede aarch64-Zeile ist
ungeprüft**. Recherche zu `sign_trusted.py`/`trusted_keys.rs` läuft.

**Als Nächstes:** B-2.2 (`hal::cache` auf ARM tatsächlich ausführen — geschrieben, übersetzt, nie
gelaufen), dann B-2.3 (README).

**Fertig und belegt:**

| Punkt | Ergebnis | Commit |
|---|---|---|
| B-1.1/1.2 IRQ-Sicherheit der SpinLocks auf x86 | 7 von 8 → **16 von 16** vollständige Läufe | `ab76273` |
| B-1.4 Fehlerklasse gesucht + Wächter zur Übersetzungszeit | `sel4lake-sync` war die einzige betroffene Crate; Empfindlichkeit belegt | `ab76273` |
| B-1.3 Wiederholungsmodus der Suite | `RUNS=n`, Quote unter 100 % ist FAIL; Probelauf 5 von 5 | `b43fc14` |
| A1 Stufe 1 Cache-Coloring | `color : ALL PASS`, 256 Farben gemessen | `7a87182` |
| Feature `selftest` (todo F1) | `.text` 0x25000 → 0x11000 (54 %) | `7a87182` |
| Zielarchitektur Z, Plan, Strang-Aufteilung | — | `6e4cf9d` |

**Testlage x86 (letzter voller Lauf, 12:38, mit A-1.1 im Baum):** einziger FAIL: `x2APIC` — TCG kann das
Merkmal grundsätzlich nicht (`TCG doesn't support requested feature: CPUID.01H:ECX.x2apic`), kein
`/dev/kvm` im Container. **Kein Regress, sondern eine Grenze des Aufbaus.**

**Testlage aarch64:** **nicht lauffähig.** `test-qemu.sh` signiert TrustedSAS-Binaries mit
`keys/trusted-test.ed25519`, und `/keys/` ist gitignored. Das ist B-2.1 und der wichtigste offene
Punkt in diesem Strang.

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
