# Anbindung: welcher RSP-Befehl wird welchen DEBUG_*-Gedanken bedienen

Gelesen, nicht erinnert: `docs/plan-debugger.md` (§3a, §9, §10b–d), `crates/caprock-abi/src/lib.rs`
(`sys::DEBUG_* = 21..25`, `debug::RIGHT_*`, `debug::READ_MAX`, `ERR_DEBUG_BUSY = 19`,
`ERR_NOT_DEBUGGABLE = 20`). v1 ist gebaut; diese Kiste (`programs/rspd`) bedient noch
nichts — jede Antwort ist `$#00`. Die Tabelle unten sagt, was spaeter wohin greift, damit
die Implementierungsstufe keinen Befehl an den falschen Gedanken haengt.

## Befehl → Gedanke (gelesen aus Plan §3a und ABI-Doku)

| RSP | Heute in `rspd` | Spaeter bedient durch | Wo |
|---|---|---|---|
| `qSupported...` | benannt, leer | kein Syscall — Protokollebene; antwortet nur, was wirklich geht (heute: nichts) | diese PD |
| `?` (Anhaltegrund) | benannt, leer | kein Syscall — Sidecar read-only lesen, `KOPF_GEN` vorher/nachher (Plan §4b); nur gehalten aussagekraeftig (Plan §2d) | diese PD + Sidecar-Mapping |
| `g` (Register lesen) | benannt, leer | kein Syscall — Sidecar-Mapping read-only (Plan §4/§4a: ein Thread je Seite, sonst defeated das Layout die Cap-Pruefung) | Kernel-Mapping + diese PD |
| `G...` (Register schreiben) | benannt, leer | `DEBUG_WRITE_REGS = 25`, Maske GPR + PC + SP + maskierte Flags; `cs`/`ss`/EL-Bits nie (Plan §3b) | v1-Syscall, v2-Maske |
| `mADDR,LEN` (Speicher lesen) | benannt, leer | `DEBUG_READ_MEM = 24`, in der PD als Schleife ueber `debug::READ_MAX` (512); Ziel-Seitentabellen, User-zugaenglich geprueft (Plan §3a, `dbgmem`-Zeile) | v1-Syscall |
| `MADDR,LEN:...` (Speicher schreiben) | benannt, leer | **fehlt**: `DEBUG_WRITE_MEM = 30` (s. Patch) — Spiegel von `DEBUG_READ_MEM` mit `debug::WRITE_MAX` | ABI-Patch + Kernel |
| `c[ADDR]` (fortsetzen) | benannt, leer | `DEBUG_CONTINUE = 23` — loescht `DEBUG` und nur das (Plan §5) | v1-Syscall |
| `s[ADDR]` (Einzelschritt) | benannt, leer | **fehlt**: `DEBUG_SINGLE_STEP = 31` (s. Patch) + `hal::debug` auf beiden Architekturen (Plan §10b: `TF` / `MDSCR_EL1.SS` + `PSTATE.SS`, Debugvektor auf den Stopp-Pfad) | ABI-Patch + HAL + Kernel |
| `H...` (Threadwahl) | `Unbekannt`, leer | offen — Mehrthread-Entscheidung, kein v1/v2-Gegenstand; wird benannt, sobald entschieden | spaeter |
| `T...` (Thread lebendig?) | `Unbekannt`, leer | wie `H` | spaeter |
| `Z0...` (Software-Breakpoint) | `Unbekannt`, leer | **abgewiesen per Design** (Plan §10d, Route 1): `int3`-Patchen ist Schreiben nach Programmtext gegen W^X + Verus-Beweis | nie (bleibt leer) |
| `Z1...`/`z1...` (HW-Breakpoint) | `Unbekannt`, leer | **fehlt**: `DEBUG_HWBREAK = 32` (s. Patch) + pro-Thread-Sichern der Debugregister im Kontextwechsel (Plan §10b — sonst feuert As Watchpoint in B, PD-Grenze!) | ABI-Patch + HAL + Kernel |
| `D` (Detach) | `Unbekannt`, leer | kein neuer Syscall — Freigabe laeuft ueber Cap-Revoke/`cap_delete` + `release_finalized_debug` (Plan §6/§6a); der Stopp hat genau einen Eigentuemer (`ERR_DEBUG_BUSY`, Plan §8a) | v1-Mechanik |
| `!` (erweiterter Modus) | `Unbekannt`, leer | kein Gegenstand — eine PD, ein Ziel (Plan §2a) | nie |

## Warum `M`/`s`/`Z` Patches brauchen (und `G` nicht)

- `G` schreibt Register: dafuer gibt es `DEBUG_WRITE_REGS` bereits — was fehlt, ist nur die
  v2-Maske (PC/SP/Flags, Plan §3b), kein neuer Syscall.
- `M` schreibt **Speicher**: dafuer gibt es keinen Syscall. `DEBUG_READ_MEM` zu
  ueberladen ("Richtung als Flag") waere *ein Parameter mit zwei Bedeutungen* (Plan §15
  warnt vor genau dieser Kopplung) — also ein eigener Syscall mit eigener Kapazitaet.
- `s` braucht CPU-Zustand (TF/MDSCR), den es im HAL noch gar nicht gibt (Plan §1a:
  null Treffer) — Syscall ohne HAL waere ein Versprechen ohne Mechanik.
- `Z1` braucht zusaetzlich Kontextwechsel-Disziplin (FP_OWNER-Klasse) — vier Register je
  Kern sind eine Kapazitaet, deren Ueberlauf benannt werden muss (`ERR_DEBUG_BUSY`-Form).

## Stufe 2 (RSP-WIRE, 2026-09-10): verdrahtet was geht, benannt der Rest

`bediene(nutzlast, backend, aus)` in `src/lib.rs` (`Backend`-Trait: `read_mem`,
`write_mem`, `read_regs` — injizierbar, host-testbar; der PD-Eintritt implementiert es
gegen die echten Syscalls). `beantworte` bleibt die kernellose Stufe-1-Antwort
(immer `$#00`); Framing/Prüfsummen/Parser sind Byte-identisch.

| RSP | Stufe 2 | Mechanik |
|---|---|---|
| `qSupported...` | beantwortet: `PacketSize=2048` | kein Syscall — nur was wirklich geht (kein `QStartNoAckMode+`, kein `multiprocess+`) |
| `?` | beantwortet: `S05` (SIGTRAP) | kein Syscall — kanonischer Stoppgrund, keine erfundene Thread-Id |
| `g` | bedient über `Backend::read_regs` | produktiv Sidecar read-only — bewusst KEIN Syscall 24 (ABI: "Frame reading has no syscall"); wer `g` an 24 hinge, läse Zielspeicher statt Registerframe |
| `mADDR,LEN` | bedient über `Backend::read_mem` | Syscall 24 (`DEBUG_READ_MEM`), PD schleift in `READ_MAX` (512); Teillänge = Lücke (kurze Antwort), Absage = `E01` |
| `MADDR,LEN:HEX` | bedient über `Backend::write_mem` | Syscall 33 (`DEBUG_WRITE_MEM`, `debug_write_mem`, `WRITE_MAX`-Deckel), Erfolg = `OK`; Längenwiderspruch → leer (kein Kürzen), Lücke → `E01` (kein Teil-`OK`) |
| `c[ADDR]` | benannt unbedient (`$#00`) | `DEBUG_CONTINUE = 23` EXISTIERT, trägt `c` aber bewusst noch nicht: Weiterlaufen braucht die Halt-Disziplin des Ziels (gehalten? wessen Stopp?), die gehört dem PD-Eintritt, nicht dem Protokoll |
| `s[ADDR]` | benannt unbedient (`$#00`) | fehlt: `DEBUG_SINGLE_STEP = 34` ist fail-closed (`ERR_BADSYS`) — kein `hal::debug` (x86: TF, aarch64: `MDSCR_EL1.SS`/`PSTATE.SS`, `#DB`-Pfad) |
| `Z0...` | benannt unbedient (`$#00`) | nie per Design (Plan §10d, Route 1): `int3`-Patchen ist Schreiben nach Programmtext gegen W^X — der Debugger schreibt es selbst via `M`/Syscall 33 |
| `Z1...`/`z1...` | benannt unbedient (`$#00`) | fehlt: `DEBUG_HWBREAK = 35` ist fail-closed (`ERR_BADSYS`) — kein `hal::debug`, kein pro-Thread-Sichern der Debugregister im Kontextwechsel |
| `G...` | benannt unbedient (`$#00`) | `DEBUG_WRITE_REGS = 25` existiert, `G` wartet auf die v2-Maske (PC/SP/Flags, Plan §3b) |

Korrektur zum Auftrag ("`g` an `DEBUG_READ_MEM`"): das verwechselt `g` und `m`.
`m` (Speicher lesen) geht an 24 — verdrahtet. `g` (Register lesen) geht ans Sidecar —
verdrahtet über `read_regs`. Die Form ist in beiden Fällen ein gedeckelter
Blocktransfer; die Nummer 24 steht nur bei `m`.

Mini-Wrapper statt `libcaprock`-Dep (Form wie `load`/`csub` gelesen, nicht importiert):
`forbid(unsafe_code)` verträgt kein SVC-asm, die Standalone-Isolation (`/tmp`-Tests)
verträgt keinen Workspace-Dep, und der echte SVC steht ohnehin erst im PD-Eintritt —
hier Form (Nummern, Deckel) + Trait. Einzige Wahrheit der Nummern: `caprock_abi::sys`.

## Was diese Stufe beweist (und was nicht)

- Bewiesen (host-testbar): Rahmung, Pruefsummen, Escapes, Neusynchronisation,
  benannte Nicht-Bedienung mit korrekter Fehlantwort.
- Nicht bewiesen: alles mit Kernel- oder Zielberuehrung — dafuer gibt es die `dbg`-Zeile
  (Plan §11), nicht diese Kiste.
