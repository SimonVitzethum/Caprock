# ADR 0018 — Formale funktionale Verifikation der IPC (Verus, Phase 4)

Status: **angenommen** · Datum: 2026-06-27 · Phase 4 der funktionalen Verifikation.
Bezug: ADR 0015 (Verifikationsansatz), `Verification/ipc/`, Laufzeit-`ipc_audit`.

## Kontext

IPC ist der einzige erlaubte Kommunikationskanal isolierter PDs. Die Korrektheit des CALL/REPLY-
Rendezvous + die Konsistenz der Endpoint-Warteschlangen (kein Nachrichtenverlust, keine Duplikate)
sind sicherheitskritisch. Zur Laufzeit prueft das `ipc_audit` + der IPC-Fuzzer; Phase 4 **beweist** das
sequentielle Protokollmodell. **Nebenlaeufigkeit** (gleichzeitige Mehrkern-Zugriffe) bleibt **ausserhalb**
(durch die `CAPS`/`EPS`-Locks serialisiert — Concurrency-/HAL-TCB, vgl. ADR 0015 + die Kani-Sync-Grenze).

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 nebenlaeufiges Modell (Interleavings) | gleichzeitige Sender/Empfaenger auf mehreren Kernen | Verus ist single-threaded; braucht Loom/TLA+ -> ausserhalb (s. ADR 0015) |
| **V2 sequentielles Protokoll (gewählt)** | Endpoint als Sender-/Empfaenger-Queues; `send`/`recv`/(`reply`) als Zustandsuebergaenge | erfasst die Kern-Invariante (Rendezvous-Ausschluss, kein Verlust/Duplikat, Fortschritt); klein + faithful zur serialisierten Realitaet |

## Entscheidung

**V2** — das sequentielle Endpoint-Protokoll modellieren und beweisen: (1) **Rendezvous-Ausschluss**
(`send`/`recv` erhalten „nie Sender **und** Empfaenger zugleich blockiert"); (2) **kein
Nachrichtenverlust/keine Duplizierung** (`send` erhoeht die Gesamtzahl um genau 1; `recv` stellt eine
anstehende Nachricht **genau einmal** zu); (3) **Rendezvous-Fortschritt** (bei vorhandenem Partner
sofortige Zustellung, kein Deadlock). Abstraktes Modell; realer Code unveraendert.

## Konsequenzen

- **Positiv:** das CALL/REPLY-Kernprotokoll (Endpoint-Konsistenz, Nachrichten-Erhaltung, Fortschritt)
  ist bewiesen, ergaenzt `ipc_audit` + den IPC-Fuzzer.
- **Grenzen/offen:** **Reply-Caps** (eine ausstehende Antwort gehoert zu genau einem Aufrufer) +
  Reply-Liveness (ERR_SERVER_GONE) + Cap-Transfer-in-IPC sind die naechsten Stufen; **Nebenlaeufigkeit**
  bleibt ausdruecklich ausserhalb (Loom/TLA+/Concurrency-Logik, Hardware-Vertrauensgrenze).
