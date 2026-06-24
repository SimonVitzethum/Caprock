# Ausbaustufe 9 — Reclaim der EL0-Kernel-Stack-Pool-Slots

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Behebt das in [ext-4](ext-4-el0-userland.md) dokumentierte Leck: der EL1-only
Kernel-Stack-Pool für EL0-Threads (feste 8×16 KiB im Kernel-Image) war ein
Bump-Allokator und gab Slots beim Thread-Ende nie zurück. Jetzt: **Free-List** —
beim Thread-Ende kommt der Slot zurück.

## Warum ein Pool (kein MEM-Allokator)

Der Kernel-Stack eines EL0-Threads MUSS **EL1-only** sein — läge er im
EL0-zugänglichen RAM (alles oberhalb `__kernel_end` ist `UserRw`), könnte der
EL0-Thread seinen eigenen Kernel-Stack lesen/schreiben (Privileg-Bruch). Daher der
feste Pool im Kernel-Image, getrennt vom allgemeinen `MEM`-Allokator.

## Mechanismus

`KstackPool` (hinter eigenem `KSTACKS`-Lock):
- `free: [bool; 8]` — verfügbare Slots.
- `slot_of: [u16; MAX_THREADS]` — Pool-Slot je globalem Thread-Slot (`KSTACK_NONE`
  = keiner; EL1-Threads haben keinen).

- `spawn_user`: `claim_user_kstack()` reserviert einen freien Slot; nach
  erfolgreichem `sched.spawn_user` `record_user_kstack(tid.slot(), idx)`. Fehlerpfade
  geben Slot **und** User-Stack zurück.
- **Reclaim beim Thread-Ende** über die `KernelSched`-Facade / den Fault-Hook:
  - `exit_current` (EXIT-Syscall): Slot des sich beendenden Threads zurückgeben.
  - `el0_fault` (EL0-Thread faultet): Slot des isolierten Threads zurückgeben.
  - `kill` (cap-`KILL` eines EL0-Threads): bei Erfolg Slot zurückgeben.
  No-Op für EL1-Threads (`slot_of == KSTACK_NONE`).

`KSTACKS` wird **stets allein** gehalten (nie mit `SCHEDS`/`MEM` verschachtelt — der
Reclaim liest die Thread-ID unter `SCHEDS`, gibt den Lock frei und sperrt dann
`KSTACKS`) → keine Sperrordnungs-Beschränkung, kein Deadlock. Im Trap sind IRQs
maskiert, daher ist der „aktuelle Thread“ über die kurzen Einzelsperren stabil.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (19 Checks). Neuer `reclaim`-Check: der
Idle-Manager erzeugt fortlaufend transiente EL0-„Exiter" (jeder macht sofort
`EXIT`), bis **16** erzeugt sind — doppelt so viele wie der 8er-Pool gleichzeitig
fasst. Das gelingt nur, weil jeder Exiter beim Ende seinen Pool-Slot zurückgibt:
`reclaim : 16 transiente EL0-Threads erzeugt (Pool=8), jetzt N Slots frei; ALL PASS`.
Ohne Reclaim bliebe der Pool nach wenigen Spawns leer und der Test scheiterte.
6/6 Läufe stabil.

## Offene Punkte

- Pool weiterhin fest dimensioniert (8 Slots) → max. 8 **gleichzeitig** lebende
  EL0-Threads. Eine größere/dynamische EL1-only Region wäre eine spätere Erweiterung.
