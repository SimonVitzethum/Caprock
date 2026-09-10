# Mem-Server Stufe 2: SPEZIFIKATION (Entscheidung, Schnittstelle, Stufenplan)

Stand: 2026-09-09. Stufe 1 ist gebaut und host-geprüft (`src/lib.rs`, `src/protokoll.rs`):
eine PD hält eine 2-MiB-Arena, vergibt Grants per IPC, lebende Grants überlappen nie, ein
fremder Schein löst nicht auf. Was Stufe 1 bewusst nicht kann, steht in der `lib.rs`-Doku:
kein echter Cap-Transfer ohne `CSUB` (der Schein ist ein Versprechen, keine Cap), kein
Demand-Paging, kein COW, Entzug nur kooperativ. Diese Datei ändert daran nichts — sie
entscheidet, was Stufe 2 darauf baut. **Kein Engine-Bau.**

## Die Entscheidung: WASM zuerst, musl bedingt, Kompat-Lib punktuell

Drei Wege (todo.md Z14 Stufe 2 / Z15, hier nur angewandt, nicht neu bewertet):

| Weg | TCB | Reichweite | Preis |
|---|---|---|---|
| WASM in einer PD | null (Engine ist Userland) | alles, was zu WASM/WASI kompiliert | keine nativen Binaries; `no_std`-Engine muss rein |
| musl gegen `libcaprock` neu gelinkt | null | fast alles aus Quelltext | je ein Dienst für FDs, Pfade, Threads, Signale |
| eigene Kompat-Bibliothek | null | was man selbst baut | kleinste Reichweite |

**Entschieden: WASM zuerst.** Drei Gründe, alle aus dem Bestand:

1. **Die Sandbox liegt in der PD, nicht im Kern** — TCB-Wirkung null bei geschlossenem
   WASI-Satz. Der Linux-Satz ist per Definition nicht geschlossen (gVisor: ~260 Syscalls in
   ~100 000 Zeilen Go mit Lücken; WSL1→WSL2: Microsoft gab die Persönlichkeit auf — das
   stärkste verfügbare Datum für „ABI-Treue einholen kostet Jahre").
2. **Der Speicherbedarf ist beim Start bekannt** (WASM-Linearspeicher) — er passt auf das
   heutige Grant-Modell ohne `memory.grow`: eine Anfrage, eine Länge, ein Zweck.
3. **musl braucht erst Stufe 1 UND einen Pfad-Dienst.** Heute: FAT16 über eine feste Datei
   (A-6.3). Datei-Deskriptoren, Pfade, Threads, Signale sind je ein eigener Dienst — der
   musl-Weg wird sinnvoll, wenn sie stehen, nicht vorher.

Kompat-Lib bleibt punktuell richtig (wo WASM nicht reicht), ersetzt aber keinen der Wege.

## Schnittstelle: WASM-Linearspeicher über Grants (Draht unverändert)

Der Draht ist `src/protokoll.rs` (`[u64; 4]`, vier Register — mehr trägt ein REPLY nicht):

| ART | Richtung | Worte | Antwort |
|---|---|---|---|
| ANFORDERN (1) | Client → Server | Länge, Zweck-Kennziffer | KODE, Handle |
| AUFLOESEN (2) | Client → Server | Handle | KODE, CPU, DEV, Länge |
| ABBILDEN (3) | Client → Server | Handle („per SYS_MAP abgebildet") | KODE |
| AUSBLENDEN (4) | Client → Server | Handle („per SYS_UNMAP entfernt") | KODE |
| FREIGEBEN (5) | Client → Server | Handle | KODE |
| ENTZUG (6) | Server → Client | Handle („bilde aus, der Schein ist tot") | — |

WASM-Abbildung: `ANFORDERN(Speicher, Zweck=Allgemein/Kennziffer 0)` → `AUFLOESEN` → `SYS_MAP`
→ `ABBILDEN`; Rückgabe `AUSBLENDEN` → `FREIGEBEN`. Keine neue Nachrichtenart, kein neues
Wort — der Linearspeicher ist ein Grant mit Zweck, weiter nichts.

## Stufenplan (jede Stufe einzeln abgenommen, keine baut die Engine)

- **2a — WASM ohne `memory.grow` (statisch).** Modul mit festem Speicher startet in einer PD,
  fordert beim Start genau einmal an, gibt beim Ende zurück. Abnahme: zwei WASM-PDs mit
  disjunktem Speicher, „zweite sieht nichts" wie Stufe 1, nur über den Draht.
- **2b — `memory.grow` via Server.** Wachsen heißt weiteres `ANFORDERN` (neuer Grant, kein
  Anhängen — Grants überlappen nie, also wächst der Speicher als Liste, nicht als Block).
  Abnahme: Modul wächst über die Startgröße, alter Inhalt bleibt, zweite PD sieht nichts.
- **2c — musl-Re-Link (bedingt).** Erst wenn Pfad-Dienst + Stufe 1 stehen: statisches
  `musl`-Binary gegen `libcaprock`, Abnahme `write`/`exit`/`brk`/`mmap` ohne Dateien, dann
  mit. Fällt die Bedingung, fällt die Stufe — 2a/2b hängen nicht daran.

**Explizit nicht hier:** Engine-Auswahl und -Bau (Kandidaten nur benannt: Engine mit
`no_std`-Pfad, z. B. wasmi-Klasse; Prüffrage ist der Speicherzugriff pro Lade/Speicher —
Trap bei Zugriff über den Linearspeicher hinaus). Erst die Schnittstelle, dann die Engine.

## Simon-Entscheidung 2026-09-10: WASM-Go

Go für den WASM-Weg. Nächster Schritt ist Stufe 2a ohne `memory.grow` (statisch, eine
Anfrage beim Start); 2b/2c hängen nicht daran und warten auf ihre Bedingung.
