# Ausbaustufe 14 — Natives Code-Laden je isolierter VSpace

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Schließt die Weg-C-Reihe ab: eine isolierte PD führt ihren **eigenen, privat
geladenen** Code aus einer **privaten EL0-RX-Region** aus — nicht die geteilte
`.user_text`. Damit kann untrusted/nativer Code, der nicht Teil des Kernelimages ist,
in einer eigenen VSpace laufen, vollständig vom restlichen System getrennt.

## Mechanismus (`system::spawn_isolated_native`)

1. **Zwei** private Frames allozieren: ein Code-Frame C und ein Stack-Frame S
   (je 2 MiB, GiB 1).
2. Den Code `[code, code+code_len)` in C **kopieren** (in der globalen Map ist C
   EL0+EL1-RW → der Kernel schreibt ihn).
3. **I-Cache kohärent machen** (`cpu::sync_code_range`: `dc cvau` bis PoU +
   `ic ivau` + Barrieren) — ohne das holt der Kern evtl. veraltete/leere I-Cache-
   Zeilen für den frisch geladenen Code.
4. Eine frische VSpace anlegen; C als **EL0-RX** mappen (`vspace_map_code_block`,
   `AP_RO_EL0|PXN`, also W^X: ausführbar, nicht schreibbar), S als **EL0-RW**.
5. Thread starten mit **Entry = Anfang von C** (privater Code) und Stack in S.

Da die Adressierung identity ist und der Entry auf C zeigt, läuft der Thread
nachweislich aus seiner **privaten** Region: wäre C nicht EL0-RX in seiner VSpace
gemappt, würde schon der Instruktions-Fetch faulten.

## W^X für geladenen Code

C wird vom Kernel über die **globale** Map (RW) beschrieben, aber in der **eigenen**
VSpace der PD nur **RX** gemappt — die PD kann ihren eigenen Code also nicht
modifizieren (W^X). Andere isolierte PDs mappen C nicht (privat).

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (24 Checks). Neuer `native`-Check: die Bytes von
`native_template` (eine `.user_text`-Vorlage: `SIGNAL` + `PARK`, positionsunabhängig)
werden in einen privaten Code-Frame kopiert; die isolierte PD führt **die Kopie** aus
und signalisiert Badge NATIVE. `native : privat geladener Code lief in eigener VSpace
(nicht geteilte .user_text)=true; ALL PASS`. 6/6 gespacete Läufe stabil.

## Stand der Weg-C-Reihe (ext-11..14)

- **ext-11**: per-Prozess-VSpace (Hybrid): isolierte PD faultet bei Fremdzugriff,
  SAS-PD darf — echte User↔User-Trennung.
- **ext-12**: allgemeiner VMM (Frame-Caps + `map`/`unmap` + Teardown).
- **ext-13**: Shared-Memory-IPC (ein Frame in zwei isolierte VSpaces, cap-gewährt).
- **ext-14**: natives Code-Laden je isolierter VSpace (privates EL0-RX, W^X).

Eine isolierte PD hat damit: privaten Code (RX) + privaten Stack/Daten (RW) +
optional cap-gewährte gemeinsame Frames; sie erreicht **kein** fremdes RAM und
kommuniziert ausschließlich über IPC + geteilte Frames. Das ist die in der
Sicherheitsanalyse geforderte „perfekte Prozesstrennung, nur Kommunikation über IPC"
— für untrusted/native Prozesse, neben der schnellen SAS-Spur für vertrauenswürdige
Rust-Komponenten.

## Offene Punkte

- Granularität 2 MiB (vom VMM geerbt); 4-KiB-Frames + echtes Binärformat (Segmente,
  Relocations) wären die nächste Stufe.
- EXIT-isolierter PDs: Teardown läuft über den Fault-Pfad; reiner `EXIT` könnte
  Tabellen/Code-Frames leaken (Demo parkt/faultet stattdessen).
