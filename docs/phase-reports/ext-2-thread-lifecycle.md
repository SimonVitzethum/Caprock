# Ausbaustufe 2 — Thread-Lebenszyklus + TCBs als Capabilities

**Datum:** 2026-06-23 · **Status:** umgesetzt & in QEMU verifiziert.

Behebt das Phase-4-Manko „beendete Threads werden nur geparkt (Stack/TCB
geleakt)" und bringt Threads ins Capability-System.

## 1. Thread-Lebenszyklus (umgesetzt) ✅

- **`SYS_EXIT`:** ein Thread beendet sich selbst. Der Scheduler merkt seinen
  Stack als *Zombie* vor und wechselt zum nächsten Thread (der TCB-Slot wird
  sofort freigegeben — er liegt im Scheduler-Array, nicht auf dem Stack; der
  **Stack** wird erst später freigegeben, da der Thread noch darauf läuft).
- **Reaping:** der Idle-Thread ruft `system::reap()` aus sicherem Kontext (eigener
  Stack) auf; das gibt die Zombie-Stacks an den Allokator zurück. Damit ist die
  Rückgewinnung lecksicher.
- Der TCB speichert nun seine Stack-Region (`stack_base`/`stack_len`).

## 2. TCBs als Capabilities + cap-kontrolliertes `KILL` (umgesetzt) ✅

- **`ObjectKind::Tcb(thread_raw)`** im einen globalen Capability-System
  (`thread_raw` = gepacktes `ThreadId`). `install_tcb` prägt eine Thread-Cap.
- **`SYS_KILL`:** beendet den über eine **Tcb-Cap** bezeichneten (nicht
  laufenden) Thread. Cap-gesichert: der `microkit::dispatch` löst die Cap im
  PD-Cspace auf, prüft Objekttyp (Tcb) + Recht (WRITE) und ruft `Scheduler::kill`.
  Ohne Cap/Recht → `ERR_BADCAP`/`ERR_RIGHTS`.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**. Lebenszyklus-Demo (Killer-PD mit Tcb-Cap tötet
ein Victim, beendet sich dann selbst):

```
life    : victim-count nach KILL=3003, später=3003 (eingefroren: true)
life    : KILL ohne Cap -> result=1 (verweigert: true)
life    : 2 Threads eingesammelt, 64 KiB Stack zurueckgewonnen
life    : ALL PASS
```

- **KILL über die Tcb-Cap** stoppt das Victim (Zähler eingefroren).
- **Cap-Gating:** KILL über einen Slot ohne Cap wird verweigert.
- **Rückgewinnung:** Victim (gekillt) und Killer (Selbst-EXIT) werden eingesammelt;
  freies RAM steigt netto (der Hot-Reload-v2-Stack wurde aus genau diesem
  zurückgegebenen Speicher bedient — Nebenbeweis der Wiederverwendung).

Alle übrigen Tests (MMU, memtest, captest, sched, fp, prio, ipc, reload, 8 Kerne)
weiterhin grün. Kernel-Crate **0 `unsafe`-Blöcke**.

## Getroffene Entscheidungen

- **Deferred Reaping für EXIT:** Den Stack eines Threads, der gerade darauf läuft,
  kann man nicht sofort freigeben. Daher: TCB sofort frei, Stack als Zombie
  vorgemerkt, Freigabe durch den Idle-Thread (sicherer Kontext). `KILL` zielt auf
  einen nicht-laufenden (blockierten/bereiten) Thread — dessen Stack könnte sofort
  frei werden; aus Einheitlichkeit läuft auch er über das Reaping.
- **TCBs im einen Cap-System** (kein zweites): `ObjectKind::Tcb`; `ThreadId` wird
  als `u64` gepackt, damit `sel4lake-cap` nicht von `sel4lake-sched` abhängen muss.
- **`KILL` braucht WRITE:** Thread-Zerstörung ist eine schreibende Autorität.

## Risiken / offene Punkte

- **`kill` nur für nicht-laufende Threads** (auf demselben Kern). Einen auf einem
  *anderen* Kern laufenden Thread zu beenden bräuchte einen IPI (Cross-Core-Stopp)
  — vorgemerkt.
- **Tcb-Cap-Finalisierung** gibt den Stack nicht selbst frei (das macht `reap`);
  Löschen der Tcb-Cap und Beenden des Threads sind derzeit getrennte Schritte.
- **Join/Rückgabewerte** (auf Thread-Ende warten, Ergebnis abholen) fehlen noch.
