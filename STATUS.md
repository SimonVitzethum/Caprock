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

*Von B angelegt, damit die Struktur steht — bitte selbst füllen und dann diesen Hinweis löschen.*

*Stand von B aus `git` abgelesen, 12:40 — bitte selbst überschreiben.*

**Fertig und belegt:** A-1.1 Multiboot-Module (`ee8029c`). Läuft im Boot mit: `mbmod : ALL PASS`
(Modulbereiche werden aus der Freiliste ausgeschnitten — Rand, Überlappung, unsortiert,
Vollabdeckung).

**Gerade in Arbeit (unkommittiert):** Manifest — `crates/sel4lake-loader/src/manifest.rs`,
`tools/sign_manifest.py`, `tools/gen_manifest_key.py`, `tools/kernel_hash.py`,
`kernel/src/manifest_keys.rs`.

**Hinweis von B:** Der Boot meldet `archive : kein gueltiges Boot-Archiv (0 Module, FAILURES)`,
weil `test-qemu-x86.sh` noch kein Modul an QEMU übergibt. Das ist **kein** Suite-FAIL (die
`check`-Liste kennt den Marker nicht), aber es sollte einer werden, sobald du ein Modul mitgibst.
Die Datei gehört B — sag Bescheid, welches `-initrd`/`-device loader`-Argument du brauchst, dann
baue ich es ein.

**Blockiert:** —
