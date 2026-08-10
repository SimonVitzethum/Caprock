# libcaprock — Userland-SDK (ext-26)

Minimal-SDK für extern geladene Caprock-EL0-Programme: die **Syscall-Stubs** + ein **Panik-
Handler**. Bewusst **ohne** Abhängigkeit vom Kernel-Workspace (die ABI-Konstanten sind dupliziert) —
ein extern gebautes Programm hängt nur von diesem SDK ab.

## Syscall-ABI

`svc #0`, Register: `x0`=Nr, `x1`=lokaler Cap-Index, `x2..x5`=Nachricht, `x6`=Tag. Rückgabe:
`x0`=Ergebnis, `x1`=Badge, `x2..x5`=Antwort, `x6`=Tag. Der Kernel restauriert beim `eret` alle
Register außer `x0..x6`.

## API

| Funktion | Syscall | Zweck |
|---|---|---|
| `invoke(nr, cap, msg, tag) -> Ret` | — | Roh-Syscall |
| `signal(cap, badge)` | `SIGNAL` | Notification signalisieren (Badge = **Cap-Badge**, nicht das Argument) |
| `wait(cap) -> u64` | `WAIT` | auf Notification warten (gibt akkumuliertes Badge) |
| `call(cap, msg) -> Ret` | `CALL` | synchroner RPC (senden + auf Antwort warten) |
| `recv(cap) -> Ret` | `RECV` | auf Aufruf warten (Server) |
| `reply(cap, msg)` | `REPLY` | letzten Aufrufer beantworten |
| `yield_now()` | `YIELD` | freiwilliger Zeitscheibenabtritt |
| `park() -> !` | `PARK` | sich dauerhaft blockieren |
| `exit() -> !` | `EXIT` | sich beenden (Stack/TCB/Pool-Slot zurück) |

Weitere Syscall-Nummern (`MAP`/`UNMAP`/`PDCTL`/`LOAD`/`KILL`) sind in `sys::*` definiert; höhere
Wrapper folgen nach Bedarf. Ein geladener Prozess kann nur Caps benutzen, die ihm der Loader (per
Manifest/Delegation, cap-gated) endowt hat.

## Entry

Jedes Programm definiert `#[no_mangle] extern "C" fn _start(arg: usize) -> !`. Der Kernel startet
es mit gesetztem SP (eigener Stack in der isolierten VSpace) und übergibt eine Boot-Info in `x0`.
Der Panik-Handler dieses SDK parkt still (kein Heap/Console im EL0-Programm).
