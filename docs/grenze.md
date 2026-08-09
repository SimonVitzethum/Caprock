# Die Grenz-Crates — was permissiv werden muss, und warum genau diese

**Stand: 2026-08-09. Noch NICHT umgesetzt** — dieses Dokument ist die Liste, nicht der Vollzug.

SEL4Lake ist `GPL-3.0-or-later`. Alles, was ein **Programm ausserhalb des Kerns** linkt, ist damit
eine Lizenzgrenze: eine PD, die gegen eine GPLv3-Crate baut, ist mit GPLv3-Code gelinkt — und dann
hilft weder die Prozesstrennung noch die ABI-Ausnahme, denn beide handeln von der *Schnittstelle*,
nicht vom *Linken*.

Das ist dieselbe Grenze, für die Linux die **UAPI-Header-Ausnahme** hat.

## Gemessen, nicht geschätzt: was Programme heute linken

| Programm | linkt |
|---|---|
| `init`, `hello`, `svc-demo` | `libsel4lake` |
| `wasmhost` | `libsel4lake`, `wasmi` |
| `fs` | `libsel4lake`, `sel4lake-part`, `sel4lake-fat` |
| `virtio-blk` | `libsel4lake`, `sel4lake-virtio`, `sel4lake-part`, `sel4lake-dma` |
| `virtio-net` | `libsel4lake`, `sel4lake-virtio` |

## Muss permissiv werden (`MIT OR Apache-2.0`)

| Crate | warum |
|---|---|
| `programs/libsel4lake` | **das SDK. Jedes Programm linkt es** — ohne diese Zeile ist alles Weitere gegenstandslos |
| `crates/sel4lake-abi` | Syscall-Nummern, Ergebniscodes, Registerbelegung. **Die ABI selbst** |
| `crates/sel4lake-dma` | `DmaPool`/`DmaBuf` — jede Treiber-PD rechnet damit |
| `crates/sel4lake-wait` | der `Park`-Trait, Mutex/Completion — jede PD mit mehreren Threads |
| `crates/sel4lake-virtio` | `Region`, Deskriptor-Typestate, Transport — jede virtio-PD |
| `crates/sel4lake-part` | GPT-Parser, in Dienst-PDs |
| `crates/sel4lake-fat` | FAT16-Parser, ebenso |

**Sieben.** Alle sieben sind bereits abhängigkeitsfrei oder hängen nur untereinander — die
Umstellung ist eine Zeile je `Cargo.toml` plus ein Lizenzhinweis, kein Umbau.

## Bleibt GPLv3 (Kern und Kernnahes)

`kernel`, `sel4lake-sched`, `sel4lake-ipc`, `sel4lake-microkit`, `sel4lake-hal`, `sel4lake-cap`,
`sel4lake-mem`, `sel4lake-slab`, `sel4lake-loader`, `sel4lake-trust`, `sel4lake-region`,
`sel4lake-sync`, `sel4lake-dtb`.

Kein Programm ausserhalb des Kerns linkt eines davon — das ist die Probe darauf, dass die Grenze
an der richtigen Stelle liegt, und `tools/kernel-grenze.sh` hält sie ohnehin schon gegen den
Quelltext.

## Der Zeitpunkt ist jetzt

Umlizenzieren braucht die Zustimmung **aller** Urheber. Solange das eine Person ist, ist es ein
Commit; nach dem ersten gemergten fremden Beitrag ist es ein Einsammelprozess — und dann gilt
dasselbe für jede künftige ABI-Ausnahme (s. `LICENSE-EXCEPTION.md`).

**Nicht umgesetzt, weil es eine Entscheidung über fremdes Werk wäre.** Die Liste steht; der Vollzug
gehört dem Urheber.
