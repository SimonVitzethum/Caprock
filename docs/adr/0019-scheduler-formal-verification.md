# ADR 0019 — Formale funktionale Verifikation des Schedulers (Verus, Phase 5)

Status: **angenommen** · Datum: 2026-06-27 · Phase 5 (letzte) der funktionalen Verifikation.
Bezug: ADR 0015 (Verifikationsansatz), ADR 0005 (per-Kern-paralleler MCS-Scheduler),
`Verification/scheduler/`, Laufzeit-`Scheduler::audit`.

## Kontext

Der Scheduler (`caprock-sched`) entscheidet, **welcher** Thread laeuft. Eine inkonsistente
Ready-Queue (toter/blockierter Eintrag, Duplikat, verlorener lauffaehiger Thread) bricht die
Verfuegbarkeit; eine fehlerhafte MCS-Budget-Buchhaltung erlaubt Laufzeit-Diebstahl (ein Thread laeuft
ueber sein Budget hinaus) oder Aushungerung (ein Thread wird nie wieder eingeplant). Zur Laufzeit
prueft `Scheduler::audit` (Codes 1–7) + der hwfuzz; Phase 5 **beweist** die Kern-Invarianten des
sequentiellen Einkern-Modells. **Nebenlaeufigkeit** (die acht per-Kern-Instanzen hinter je eigenem
Lock, Reschedule-IPIs, Kontextwechsel) bleibt **ausserhalb** (Concurrency-/HAL-TCB, vgl. ADR 0015).

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 nebenlaeufiges Mehrkern-Modell | acht Instanzen + IPIs + Migration im Interleaving | Verus ist single-threaded; braucht Loom/TLA+ -> ausserhalb (ADR 0015). Der Code haelt jede Instanz hinter eigenem Lock -> Einkern-Sicht ist die faithful sequentielle Abstraktion |
| V2 konkrete Ringpuffer-Queues | `RunQueue` (buf/head/tail/count) bitgenau nachbauen | beweist die Ringpuffer-Implementierung, nicht die *scheduling-Eigenschaft*; viel Ballast, geringer Sicherheitsgewinn (Ringpuffer ist klein + fuzzer-/Kani-naher Kandidat) |
| **V3 Mitgliedschafts-Abstraktion (gewählt)** | Ready-Queue als per-Thread-Flag `in_ready`; Invariante koppelt es an den Thread-Zustand | erfasst **genau** die `audit`-Eigenschaft (Codes 1/2/3/4/7) + die MCS-Budget-Invarianten; Duplikatfreiheit (Code 3) ist durch das Flag strukturell; klein + faithful zur serialisierten Realitaet |

## Entscheidung

**V3** — das sequentielle Einkern-Modell mit **Mitgliedschafts-Kopplung** modellieren und beweisen:

1. **Runqueue-Kopplung** (`coupled`, Herz von `audit`): ein Thread steht **genau dann** in einer
   Ready-Queue, wenn er **lauffaehig** (belegt ∧ nicht blockiert ∧ nicht erschoepft) und **nicht der
   laufende** ist. Deckt audit-Codes 1 (kein toter Eintrag), 2 (kein blockierter), 4 (current nicht
   bereit), 7 (kein verlorener) ab; Code 3 (kein Duplikat) ist strukturell (Flag statt Multimenge).
2. **MCS-Budget-Kopplung** (`budget_inv`): Restbudget ≤ Budget; erschoepft ⟹ Rest 0; **budget==0 ⟹
   nie erschoepft** (Round-Robin hungert nie aus).
3. Alle Operationen (`block_current`, `pick`, `unblock`, `pause`, `tick_charge`, `refill`,
   `set_budget`) **erhalten** beide Invarianten; `refill` stellt das Budget wieder her, `set_budget`
   rettet einen erschoepften Thread vor dem Verlust, `tick_charge` deplaniert bei Erschoepfung sauber.

Abstraktes Modell (V2 i. S. v. ADR 0015 — faithful, realer Code unveraendert).

## Konsequenzen

- **Positiv:** die Scheduler-Kern-Eigenschaften (Ready-Queue-Konsistenz, kein verlorener Thread,
  MCS-Schranke, keine Aushungerung, Fortschritt via stets-bereitem Idle) sind bewiesen und ergaenzen
  `Scheduler::audit` + den hwfuzz. **Phase 5 schliesst die funktionale Kern-Verifikation ab.**
- **Grenzen/offen:** **Prioritaets-Queue-Trennung** (audit-Code 6 — Thread in der Queue seiner
  Prioritaet) ist strukturell und hier nicht modelliert; **Budget-Donation** (intra-core IPC,
  geteilter Scheduling-Context), **Zombie-/Reap-Lebenszyklus** und **Nebenlaeufigkeit** (Mehrkern,
  IPIs, Migration, Kontextwechsel) bleiben ausdruecklich ausserhalb (Folgestufen bzw. HW-TCB).
