# Ausbaustufe 5 — Lazy-FP (umgesetzt)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert.

Löst das in [ext-3](ext-3-lazy-fp-locks.md) als „architektonisch unvereinbar"
zurückgerollte Lazy-FP — jetzt möglich, weil die **EL0/EL1-Trennung**
([ext-4](ext-4-el0-userland.md)) existiert. Damit entfällt das eager
q0..q31-Save/Restore (512 Byte) bei **jedem** Trap.

## Warum es jetzt geht (ext-3-Befund aufgelöst)

ext-3 scheiterte, weil `CPACR_EL1.FPEN` mit `0b00` FP **auch an EL1** trappte und
der Kernel-eigene, von LLVM emittierte NEON-Code (Handler, Hooks) sich damit selbst
trappte → verschachtelter Trap → Hang. Zwei Änderungen lösen das:

1. **`FPEN = 0b01`** — FP/SIMD trappt **nur an EL0**, nie an EL1. Der Kernel kann
   nicht mehr von seinem eigenen FP-Trap getroffen werden.
2. **Soft-float Microkernel** — das Target (`targets/aarch64-caprock.json`) setzt
   `"rustc-abi": "softfloat"` + `"features": "+v8a,+strict-align,-neon"`. Der
   Kernel (EL1) emittiert **gar kein** FP/SIMD mehr. Damit gehören die FP-Register
   ausschließlich den EL0-User-Threads, und kein Kernel-Code kann den Lazy-Owner
   versehentlich überschreiben (sonst ginge sein noch nicht gesicherter Zustand
   verloren). Das ist exakt das seL4-Modell (Kernel `-mgeneral-regs-only`).

## Mechanismus

- **`hal::fp`** — `FpState` (q0..q31 + FPSR/FPCR), `save`/`restore` (eigener
  `global_asm!`-Block mit `.arch armv8-a`, damit q-Register trotz `-neon`
  assemblieren), `set_el0_trap(bool)` schaltet `FPEN` zwischen `0b01` (trappen)
  und `0b11` (frei).
- **Pro Kern ein FP-Owner.** `FP_OWNER[core]` (Atomic, je Kern nur vom eigenen Kern
  beschrieben) hält die `ThreadId` des Threads, dessen FP-Register gerade live sind.
  `FP_STATES[slot]` ist der per-Thread-Sicherungspuffer (Zugriff unter `SCHED`).
- **Trap-Pfad integer-only.** Der `TrapFrame` schrumpft von 800 auf **272 Byte**;
  `__trap_dispatch` sichert **kein** FP mehr.
- **FP-Trap (EC 0x07) aus EL0** → `fp_trap`: Register des alten Owners sichern,
  die des Trappers laden, ihn zum Owner machen, `FPEN` freigeben; der `eret`
  wiederholt die getrappte Instruktion. Beim Kontextwechsel setzt
  `sync_fp_trap` `FPEN` passend zum neuen aktuellen Thread (trappen, falls er nicht
  der Owner ist → sein erster FP-Zugriff löst den Lazy-Wechsel aus).
- **Slot-Wiederverwendung:** `spawn` nullt `FP_STATES[slot]` und löscht veraltete
  `FP_OWNER`-Referenzen auf den Slot (unter `SCHED`, bevor der Thread laufen kann).

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (16 Checks). Der `fp`-Check ist nun ein **echter
EL0-Lazy-FP-Test**: zwei EL0-User-Threads (`user_fp_entry`, `.user_text`) halten je
ein eindeutiges 64-Bit-Muster in d0..d3 und geben per `YIELD` strikt gegenseitig
ab (höchste Demo-Priorität → deterministisches Ping-Pong). Jede Abgabe erzwingt
einen FP-Owner-Wechsel; nur korrektes Lazy-Save/Restore lässt das Muster überleben.
Bei Erfolg meldet jeder Thread per `SIGNAL` (Notification-Badge) an einen
EL1-Kollektor.

Beobachtet: `fp : EL0-FP-Threads-OK=0b11/0b11, Lazy-FP-Owner-Wechsel=399`
(beide Threads ohne Korruption über 399 Owner-Wechsel). **Kein Hang** — der
ext-3-Befund ist aufgelöst.

## Auswirkungen / Offene Punkte

- **Performance:** Threads, die zwischen zwei Einplanungen kein FP nutzen, kosten
  keinerlei FP-Traffic; der heiße Trap-Pfad spart 512 Byte Save/Restore.
- **Determinismus:** FP-Kosten fallen nur beim ersten FP-Zugriff nach einem
  Owner-Wechsel an (ein Trap), nicht pauschal pro Trap.
- Migriert ein Thread den Kern (heute nicht — Threads sind kern-gebunden), müsste
  sein FP-Owner-Eintrag auf dem alten Kern invalidiert werden. Für die geplanten
  per-Kern-Scheduler-Instanzen mitzudenken.
