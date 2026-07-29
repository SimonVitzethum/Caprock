# STATUS — laufender Stand beider Stränge

*Diese Datei ist der Blick von aussen: **was läuft gerade, was ist fertig, was blockiert.**
Beide Agenten schreiben ihren eigenen Abschnitt und lassen den des anderen in Ruhe.
Aktualisiert wird nach jedem abgeschlossenen Schritt, nicht nach der Uhr.*

**Zuletzt geändert (B): 2026-07-29 12:33 UTC**

---

## Strang B — Verlässlichkeit und Isolation (Claude B)

**Gerade in Arbeit:** B-1.3 — Wiederholungslauf in `test-qemu-x86.sh`. Ein einzelner Durchlauf
kann einen Nichtdeterminismus grundsätzlich nicht finden; die Suite braucht einen N-fach-Modus,
der die Quote meldet.

**Als Nächstes:** B-2.1 (ARM-Suite aus frischem Clone lauffähig — `keys/` ist gitignored, damit ist
jede aarch64-Zeile ungeprüft), dann B-2.2 (`hal::cache` auf ARM tatsächlich ausführen).

**Fertig und belegt:**

| Punkt | Ergebnis | Commit |
|---|---|---|
| B-1.1/1.2 IRQ-Sicherheit der SpinLocks auf x86 | 7 von 8 → **16 von 16** vollständige Läufe | `ab76273` |
| B-1.4 Fehlerklasse gesucht + Wächter zur Übersetzungszeit | `sel4lake-sync` war die einzige betroffene Crate; Empfindlichkeit belegt | `ab76273` |
| A1 Stufe 1 Cache-Coloring | `color : ALL PASS`, 256 Farben gemessen | `7a87182` |
| Feature `selftest` (todo F1) | `.text` 0x25000 → 0x11000 (54 %) | `7a87182` |
| Zielarchitektur Z, Plan, Strang-Aufteilung | — | `6e4cf9d` |

**Testlage x86 (letzter voller Lauf):** 24 von 25 PASS. Einziger FAIL: `x2APIC` — TCG kann das
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

**Gerade in Arbeit:** A-1.1 (Multiboot-Module) — vermutet aus dem Arbeitsverzeichnis:
`kernel/src/arch/x86_64/multiboot.rs`, `kernel/src/loader.rs`, dazu `main.rs` (`mod loader`
entgatet). Noch nicht committet, Stand 12:33.

**Als Nächstes:** —

**Fertig und belegt:** —

**Blockiert:** —
