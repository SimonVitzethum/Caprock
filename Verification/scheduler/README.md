# Verifikation — Scheduler (Phase 5)

> **Status:** Kern bewiesen — die **Runqueue-Konsistenz** (kein toter/blockierter/erschoepfter Eintrag,
> kein Duplikat, kein verlorener Thread, laufender Thread nicht zugleich bereit) und die
> **MCS-Budget-Buchhaltung** (Restbudget ≤ Budget, saubere Erschoepfung/Refill, keine Aushungerung)
> sind formal verifiziert (13 verified, CI-gated). Eigenständig verständlich (ohne Quellcode).
> **Phase 5 schliesst die funktionale Kern-Verifikation ab.**

Bezug: [ADR 0019](../../docs/adr/0019-scheduler-formal-verification.md), ADR 0005 (per-Kern-MCS-
Scheduler), Laufzeit-`Scheduler::audit`, `docs/verification.md`.

## 1. Motivation und Ziel

Der Scheduler entscheidet, **welcher** Thread laeuft. Zwei Fehlerklassen sind sicherheitskritisch:
(a) eine **inkonsistente Ready-Queue** (ein toter, blockierter oder doppelt eingeplanter Eintrag, ein
verlorener lauffaehiger Thread) bricht die Verfuegbarkeit; (b) eine **fehlerhafte MCS-Buchhaltung**
erlaubt Laufzeit-Diebstahl (Lauf ueber das Budget hinaus) oder Aushungerung (nie wieder eingeplant).
Zur Laufzeit prueft `Scheduler::audit` (Codes 1–7); **Ziel von Phase 5:** diese Eigenschaften für das
sequentielle Einkern-Modell **beweisen** (statisch, alle Zustände).

## 2. Sicherheitsmodell

- Jede `Scheduler`-Instanz haelt **genau einen Kern** hinter eigenem Lock (ADR 0005) -> die
  Einkern-Sicht ist die getreue sequentielle Abstraktion.
- Ein **Thread** ist **lauffaehig**, wenn belegt ∧ nicht blockiert ∧ nicht (MCS-)erschoepft.
- **Kopplung:** ein Thread steht **genau dann** in einer Ready-Queue, wenn er lauffaehig und nicht der
  laufende ist (das `audit`-Oracle in einer Aussage).
- **Sequentiell:** die acht per-Kern-Instanzen, Reschedule-IPIs und der Kontextwechsel bleiben
  **ausserhalb** (Concurrency-/HAL-TCB, ADR 0019).

## 3. Zu beweisende Eigenschaften

1. **Runqueue-Kopplung erhalten:** jede Operation bewahrt „in_ready ⟺ lauffaehig ∧ nicht laufend".
2. **MCS-Schranke:** Restbudget ≤ Budget; erschoepft ⟹ Rest 0; **budget==0 ⟹ nie erschoepft**.
3. **Refill/Rettung:** `refill` stellt `remaining == budget` her + reiht wieder ein; `set_budget`
   rettet einen erschoepften Thread vor dem Verlust; `tick_charge` deplaniert bei Erschoepfung sauber.
4. **Fortschritt:** der Idle-Thread (belegt, nie blockiert, budget==0) ist stets bereit -> es gibt
   immer einen waehlbaren Thread (kein Leerlauf-Deadlock).

## 4. Bezug zu ADRs

ADR 0019 (diese Verifikation) · ADR 0005 (per-Kern-paralleler MCS-Scheduler).

## 5. Formale Spezifikation

`Sched { threads: Seq<Thread>, current: Option<nat> }`, `Thread { used, blocked, depleted, in_ready,
prio, budget, remaining }`. `runnable(t)` = `used && !blocked && !depleted`. `coupled` = „für alle i:
`in_ready[i] ⟺ runnable(t_i) && current != Some(i)`" (Codes 1/2/4/7; Code 3 strukturell via Flag).
`budget_inv` = „für alle i: (budget>0 ⟹ remaining≤budget) ∧ (depleted ⟹ remaining==0) ∧ (budget==0 ⟹
!depleted)". `current_valid` = laufender Thread belegt + lauffaehig. `sched_inv` = Konjunktion.

## 6. Verus-Architektur

[`proofs/runqueue.rs`](proofs/runqueue.rs), per `tools/verus-verify.sh` + Verus-CI-Gate. Abstraktes,
**sequentielles Einkern**-Mitgliedschaftsmodell (V3, ADR 0019); realer Code unverändert.

## 7. Beweisstrategie

Jede Operation ist ein `Seq::update` **eines** Thread-Slots (+ ggf. `current`); das `in_ready`-Flag
wird so neu berechnet, dass die Kopplung lokal gilt. Die Erhaltung für alle anderen Threads folgt aus
`update(i,v)[j] == seq[j]` für `j != i` (Fallunterscheidung `i == touched` im `assert forall ... by`).

## 8. Lemmas / 9. Bewiesene Eigenschaften

| Theorem | Aussage | Status |
|---|---|---|
| `block_preserves` / `pick_preserves` | block_current + Auswahl des Naechsten erhalten `sched_inv` | ✅ |
| `unblock_preserves` / `pause_preserves` | Wecken + externes Pausieren erhalten `sched_inv` | ✅ |
| `tick_preserves` | Budget-Tick erhaelt `sched_inv` (deplaniert bei Erschoepfung sauber) | ✅ |
| `refill_preserves` | Refill erhaelt `sched_inv` **und** stellt `remaining==budget` + Bereitschaft her | ✅ |
| `setbudget_preserves` | `set_budget` rettet erschoepften Thread (kein TCB-Verlust/DoS) | ✅ |
| `no_lost_thread` | lauffaehig ∧ nicht laufend ⟹ in einer Ready-Queue (Code 7) | ✅ |
| `ready_implies_runnable` | in der Queue ⟹ lauffaehig (Codes 1/2) | ✅ |
| `mcs_bound` | Restbudget ≤ Budget (kein Lauf ueber das Budget) | ✅ |
| `roundrobin_no_starve` | budget==0 ⟹ nie erschoepft (keine Aushungerung) | ✅ |
| `idle_always_selectable` | Idle stets bereit ⟹ es gibt immer einen waehlbaren Thread (Fortschritt) | ✅ |

(13 verified inkl. `main`.)

## 10. Noch offene Eigenschaften

- **Prioritaets-Queue-Trennung** (audit-Code 6 — jeder Thread in der Queue **seiner** Prioritaet) +
  „höchste nichtleere Prioritaet zuerst" als bewiesene Auswahlregel (hier strukturell/Modell-implizit).
- **Budget-Donation** (intra-core IPC: der Aufrufer leiht dem Server seinen Scheduling-Context;
  geteiltes Wurzel-Konto, verschachtelte Calls) — die naechste Stufe, baut auf der MCS-Schranke auf.
- **Zombie-/Reap-Lebenszyklus** (Slot-Freigabe, Generation-Bump gegen ABA, Stack-Rueckgewinnung).

## 11. Bekannte Grenzen

- **Sequentiell / Einkern:** **Nebenlaeufigkeit** (acht Instanzen, Reschedule-IPIs, Kontextwechsel,
  fehlende Migration) liegt **ausserhalb** — dafür wäre ein Concurrency-Modellprüfer (Loom/TLA+) nötig.
- **Mitgliedschafts-Abstraktion:** die konkrete Ringpuffer-`RunQueue` (head/tail/count) wird nicht
  bitgenau nachgebaut; ihre Konsistenz sichern `Scheduler::audit` + der hwfuzz auf dem **echten** Code.

## 12. Trusted Computing Base

1. Lock-Serialisierung je Kern (`SCHEDS[core]`) + Kontextwechsel (HAL) — Concurrency-/HAL-TCB.
2. Modell↔Code-Treue (Mitgliedschaft statt Ringpuffer) — durch `Scheduler::audit` + hwfuzz abgesichert.

## 13. Verbindung zu Runtime-Audits / Kani

- **Laufzeit:** `Scheduler::audit` (Codes 1–7, in `ipc_audit` als Code 10+ aggregiert), hwfuzz
  (randomisierte spawn/block/unblock/kill/tick/Budget-Sequenzen + Audit je Epoche).
- **Verus (hier):** beweist, dass die Kopplung + MCS-Buchhaltung **für alle Zustände** stimmt. Die
  Ebenen ergänzen sich (Verus die Logik, hwfuzz die Ringpuffer-/Nebenlaeufigkeitsrealitaet empirisch).

## 14. Verifikationsfortschritt / Nächste Ausbaustufen

- ✅ Runqueue-Kopplung (Codes 1/2/3/4/7) + MCS-Budget (Schranke/Refill/keine Aushungerung) +
  Fortschritt.
- ⏳ Prioritaets-Auswahlregel (Code 6) · Budget-Donation · Zombie/Reap · (später) Nebenlaeufigkeit
  via Loom/TLA+.
