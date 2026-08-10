# Ausbaustufe 4 — EL0-Userland + Privileg-Isolation

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Bringt eine **hardware-erzwungene Privileg-Grenze** in den Single-Address-Space:
User-Threads laufen auf **EL0**, der Kernel auf **EL1**. Das schließt die Lücke
aus ADR 0002 („kein Schutz gegen bösartigen nativen Code") für die wichtigste
Grenze — Kernel ↔ User — ohne den SAS aufzugeben (es bleibt **eine** Identity-Map,
kein virtueller Speicher, keine per-Prozess-VSpaces). Außerdem entsperrt es den
zuvor verworfenen Lazy-FP-Pfad (ext-3).

## 1. MMU: Privileg-Grenze über AP-Bits ✅

Die *eine* Identity-Map mappt nun pro Seite differenziert (`caprock-hal::mmu`):

| Region                              | EL0 | EL1 | XN-Bits     |
|-------------------------------------|-----|-----|-------------|
| Kernel `.text`                      |  –  | RX  | PXN aus     |
| Kernel `.rodata`                    |  –  | RO  | PXN/UXN     |
| Kernel `.data`/`.bss`/Kernel-Stacks |  –  | RW  | PXN/UXN     |
| `.user_text`                        | RX  | RO  | PXN (an EL1)|
| User-RAM (> `__kernel_end`)         | RW  | RW  | PXN/UXN     |

`AP[2:1]`: `0b01` = RW EL0+EL1, `0b11` = RO EL0+EL1, `0b10` = RO EL1, `0b00` =
RW EL1. So ist das Kernel-Image für EL0 **unsichtbar** und User-Code an EL1
**nicht ausführbar** (PXN) — beide Richtungen der Grenze hardware-erzwungen.

## 2. EL0-Thread-Pfad ✅

- **TrapFrame** trägt jetzt `SP_EL0` (Offset 264). Der Trap-Entry sichert `SP_EL0`,
  der Exit restauriert ihn — so hat jeder EL0-Thread seinen eigenen User-Stack,
  während der Trap auf dem EL1-Kernel-Stack (`SP_EL1`) abläuft.
- **`init_thread_frame(…, el0, user_sp)`** setzt `SPSR=EL0t` (Modusbits `0b0000`)
  und `SP_EL0=user_sp`; der `eret` springt damit nach EL0. `frame_from_el0` liest
  die Modusbits zur Laufzeit.
- **`spawn_user`**: Der initiale Frame liegt auf einem **EL1-only Kernel-Stack**
  aus einem festen Linker-Pool (`__user_kstacks`, 4×16 KiB); der Thread läuft auf
  einem **User-Stack** aus dem EL0-zugänglichen RAM (`phys.alloc`). Reaping gibt
  den dynamischen *User*-Stack an den Allokator zurück; der Kernel-Pool-Slot wird
  in Phase 1 geleakt (klein, fest — Reclaim ist ein Folge-Schritt).
- Der Kontextwechsel (SP-Tausch im Trap-Pfad) funktioniert unverändert für EL0:
  nach dem `eret` bleibt `SP_EL1` auf dem Kernel-Stack-Top des Threads stehen.

## 3. Isolation: EL0-Fault tötet den Thread, nicht den Kernel ✅

`handle_exception` routet einen **synchronen Fault aus EL0** an einen Fault-Hook,
statt den Kernel anzuhalten. Der Hook (`system::el0_fault`) beendet **nur den
fehlerhaften Thread** (`exit_current`) und wechselt zum nächsten lauffähigen
Thread. Faults aus **EL1** (echter Kernel-Bug) halten weiterhin an (fail-stop).
So kann nicht vertrauenswürdiger User-Code den Kernel hardware-seitig nicht mehr
kompromittieren.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (16 Checks). Zwei neue:

- **`el0`** — ein echter EL0-Thread (`user_entry`, reines Inline-`svc` in
  `.user_text`) ruft per `CALL`-Syscall einen EL1-Server und übergibt `0xC0DE`;
  der Kernel registriert den EL0-Aufrufer (`EL0-Syscall gesehen: true`).
- **`el0iso`** — ein bösartiger EL0-Thread (`bad_user`) liest `0x4008_0000`
  (Kernel-`.text`). Beobachtet: `el0-trap: … faultete (EC=0x24 FAR=0x40080000)
  -> beendet, Kernel laeuft weiter`. EC `0x24` = Data Abort aus niedrigerem EL;
  FAR = exakt die verbotene Kernel-Adresse. Dass der Abschlussbericht überhaupt
  erscheint, ist der Beweis, dass der Kernel den User-Fault überlebt hat.

## Offene Punkte

- Reclaim der EL1-only Kernel-Stack-Pool-Slots (aktuell fester 4er-Pool, geleakt).
- Keine Adressraum-Isolation *zwischen* User-Komponenten (SAS-Eigenschaft, ADR
  0002) — EL0 schützt nur die Kernel-Grenze, nicht User ↔ User.
- Lazy-FP ist nun möglich (FP-Trap an EL0 statt EL1) — noch nicht umgesetzt.
