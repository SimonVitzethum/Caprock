# Die Grenz-Crates — was permissiv werden muss, und warum genau diese

**Stand: 2026-08-09. UMGESETZT** — alle sieben Crates tragen `MIT OR Apache-2.0`. Der Kern bleibt
`AGPL-3.0-or-later`.

Caprock ist `AGPL-3.0-or-later`. Alles, was ein **Programm ausserhalb des Kerns** linkt, ist damit
eine Lizenzgrenze: eine PD, die gegen eine GPLv3-Crate baut, ist mit GPLv3-Code gelinkt — und dann
hilft weder die Prozesstrennung noch die ABI-Ausnahme, denn beide handeln von der *Schnittstelle*,
nicht vom *Linken*.

Das ist dieselbe Grenze, für die Linux die **UAPI-Header-Ausnahme** hat.

## Gemessen, nicht geschätzt: was Programme heute linken

| Programm | linkt |
|---|---|
| `init`, `hello`, `svc-demo` | `libcaprock` |
| `wasmhost` | `libcaprock`, `wasmi` |
| `fs` | `libcaprock`, `caprock-part`, `caprock-fat` |
| `virtio-blk` | `libcaprock`, `caprock-virtio`, `caprock-part`, `caprock-dma` |
| `virtio-net` | `libcaprock`, `caprock-virtio` |

## Muss permissiv werden (`MIT OR Apache-2.0`)

| Crate | warum |
|---|---|
| `programs/libcaprock` | **das SDK. Jedes Programm linkt es** — ohne diese Zeile ist alles Weitere gegenstandslos |
| `crates/caprock-abi` | Syscall-Nummern, Ergebniscodes, Registerbelegung. **Die ABI selbst** |
| `crates/caprock-dma` | `DmaPool`/`DmaBuf` — jede Treiber-PD rechnet damit |
| `crates/caprock-wait` | der `Park`-Trait, Mutex/Completion — jede PD mit mehreren Threads |
| `crates/caprock-virtio` | `Region`, Deskriptor-Typestate, Transport — jede virtio-PD |
| `crates/caprock-part` | GPT-Parser, in Dienst-PDs |
| `crates/caprock-fat` | FAT16-Parser, ebenso |

**Sieben.** Alle sieben sind bereits abhängigkeitsfrei oder hängen nur untereinander — die
Umstellung ist eine Zeile je `Cargo.toml` plus ein Lizenzhinweis, kein Umbau.

## Bleibt GPLv3 (Kern und Kernnahes)

`kernel`, `caprock-sched`, `caprock-ipc`, `caprock-microkit`, `caprock-hal`, `caprock-cap`,
`caprock-mem`, `caprock-slab`, `caprock-loader`, `caprock-trust`, `caprock-region`,
`caprock-sync`, `caprock-dtb`.

Kein Programm ausserhalb des Kerns linkt eines davon — das ist die Probe darauf, dass die Grenze
an der richtigen Stelle liegt, und `tools/kernel-grenze.sh` hält sie ohnehin schon gegen den
Quelltext.

## Der Zeitpunkt ist jetzt

Umlizenzieren braucht die Zustimmung **aller** Urheber. Solange das eine Person ist, ist es ein
Commit; nach dem ersten gemergten fremden Beitrag ist es ein Einsammelprozess — und dann gilt
dasselbe für jede künftige ABI-Ausnahme (s. `LICENSE-EXCEPTION.md`).

**Umgesetzt am 2026-08-09.** Unter AGPL ist das nicht mehr nur sauber, sondern tragend: §13 knüpft
die Copyleft-Pflicht an den **Betrieb**, und stünden die Grenz-Crates unter AGPL, wäre jede PD mit
AGPL-Code gelinkt — die ABI-Ausnahme liefe leer, und jeder Kunden-Workload stünde unter §13-Verdacht.
