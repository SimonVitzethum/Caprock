# ADR 0020 — Formale funktionale Verifikation der Notifications (Verus, Phase 6)

Status: **angenommen** · Datum: 2026-06-27 · Phase 6 der funktionalen Verifikation (weitere Komponente).
Bezug: ADR 0015 (Verifikationsansatz), ADR 0018 (IPC), `Verification/notifications/`,
Laufzeit-`Notification::audit`.

## Kontext

Notifications sind der **asynchrone**, nicht-blockierende Signalkanal (Badge-akkumulierte Ereignisse,
u. a. die Deferred-IRQ-Zustellung an HardwareLand-Backends). Sie ergänzen das synchrone IPC (ADR 0018):
ein `signal` ODERt Badge-Bits ins pending-Wort und weckt einen etwaigen Wartenden; ein `wait` holt das
Wort ab oder blockiert. Geht ein Signal verloren oder bleibt ein Wartender trotz anstehender Signale
blockiert (Lost-Wakeup), ist der Kanal fehlerhaft. Zur Laufzeit prueft `Notification::audit` (toter
Wartender) + der Fuzzer; Phase 6 **beweist** das sequentielle Signal-/Wait-Protokoll.
**Nebenlaeufigkeit** + kern-uebergreifendes Wecken (`unblock`+IPI) bleiben **ausserhalb** (Concurrency-/
HAL-TCB, ADR 0015).

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 u64-Bitwort bitgenau | pending als `u64`, `signal`=`\|=`, Beweise per bit_vector | erzwingt den SMT-Bit-Vektor-Solver für jede Eigenschaft; viel Ballast, der Sicherheitsgewinn (Bit-Akkumulation) ist mengentheoretisch sauberer ausdrueckbar |
| **V2 Badge-Bits als `Set<nat>` (gewählt)** | pending als Bit-Menge; `signal`=Vereinigung, `wait`=Drain | erfasst **genau** „kein Bit geht verloren / wird genau einmal konsumiert"; klein + faithful (ODER ≙ Vereinigung) |

## Entscheidung

**V2** — das sequentielle Notification-Protokoll modellieren und beweisen: (1) **Invariante**
`waiter is Some ==> pending leer` (ein blockierter Wartender sitzt nie auf unzugestellten Signalen);
(2) **kein Signalverlust** (jedes signalisierte Bit ist danach im pending-Wort **oder** an den
geweckten Wartenden zugestellt); (3) **genau-einmal-Konsum** (`wait` holt das gesamte pending-Wort ab
und leert es — kein Rest, kein Duplikat); (4) **Fortschritt** (bei anstehendem Wort blockiert `wait`
nicht — kein Lost-Wakeup); `signal`/`wait`/`purge` erhalten die Invariante. Abstraktes Modell; realer
Code unveraendert.

## Konsequenzen

- **Positiv:** das Signal-/Wait-Kernprotokoll (kein Signalverlust, genau-einmal-Konsum, kein
  Lost-Wakeup) ist bewiesen, ergaenzt `Notification::audit` + den Fuzzer.
- **Grenzen/offen:** **mehrere Wartende** (aktuell ein Konsument je Notification), **Notification-
  Binding an einen TCB** (gebundene Zustellung) und **Nebenlaeufigkeit** (kern-uebergreifendes
  `unblock`+IPI) bleiben ausdruecklich ausserhalb (Folgestufen bzw. HW-TCB).
