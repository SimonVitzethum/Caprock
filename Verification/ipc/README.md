# Verifikation — IPC (Phase 4)

> **Status:** Kern bewiesen — das CALL/REPLY-Rendezvous-Protokoll (Endpoint-Konsistenz, kein
> Nachrichtenverlust/Duplikat, Fortschritt) ist formal verifiziert (6 verified, CI-gated).
> Eigenständig verständlich (ohne Quellcode).

Bezug: [ADR 0018](../../docs/adr/0018-ipc-formal-verification.md), Laufzeit-`ipc_audit`,
`docs/verification.md`.

## 1. Motivation und Ziel

IPC ist der **einzige** erlaubte Kommunikationskanal isolierter PDs. Geht eine Nachricht verloren,
wird dupliziert oder verpasst ein Sender/Empfänger das Rendezvous, ist das Protokoll fehlerhaft.
**Ziel:** das CALL/REPLY-Kernprotokoll **beweisen** (statisch, alle Zustände).

## 2. Sicherheitsmodell

- Ein **Endpoint** trägt Warteschlangen blockierter **Sender** (mit Nachricht) und **Empfänger**.
- **Rendezvous-Ausschluss:** nie gleichzeitig Sender **und** Empfänger blockiert — sonst hätte ein
  Rendezvous stattgefunden.
- **Sequentiell:** Operationen sind durch die `EPS`-Locks serialisiert; Nebenläufigkeit (Interleavings)
  bleibt **ausserhalb** (Concurrency-/HAL-TCB, ADR 0018).

## 3. Zu beweisende Eigenschaften

1. **Rendezvous-Ausschluss erhalten:** `send`/`recv` bewahren „mind. eine Queue leer".
2. **Kein Nachrichtenverlust / keine Duplizierung:** `send` erhöht die Gesamtzahl um genau 1; `recv`
   stellt eine anstehende Nachricht **genau einmal** zu.
3. **Rendezvous-Fortschritt:** bei vorhandenem Partner sofortige Zustellung (kein Deadlock).

## 4. Bezug zu ADRs

ADR 0018 (diese Verifikation).

## 5. Formale Spezifikation

`Endpoint { senders: Seq, receivers: Seq, delivered }`. `ep_inv` = `senders.len()==0 ||
receivers.len()==0`. `msgs_total` = `delivered + senders.len()` (im Umlauf befindliche Nachrichten).
`send`/`recv` als Zustandsübergänge (Rendezvous oder Blockierung).

## 6. Verus-Architektur

[`proofs/endpoint.rs`](proofs/endpoint.rs), per `tools/verus-verify.sh` + Verus-CI-Gate. Abstraktes,
**sequentielles** Protokollmodell (V2, ADR 0018); realer Code unverändert.

## 7. Beweisstrategie

Spec-Entfaltung der Zustandsübergänge (Verus-SMT); `msgs_total` als Erhaltungsgröße gegen Verlust/
Duplikat.

## 8. Lemmas / 9. Bewiesene Eigenschaften

| Theorem | Aussage | Status |
|---|---|---|
| `send_preserves_inv` / `recv_preserves_inv` | Rendezvous-Ausschluss erhalten | ✅ |
| `send_no_loss` | `send` erhöht die Nachrichtenzahl um **genau 1** | ✅ |
| `recv_delivers_once` | `recv` stellt eine anstehende Nachricht **genau einmal** zu | ✅ |
| `rendezvous_progress` | bei vorhandenem Partner sofortige Zustellung (kein Deadlock) | ✅ |

(6 verified inkl. `main`.)

## 10. Noch offene Eigenschaften

- **Reply-Caps:** eine ausstehende Antwort gehört zu **genau einem** blockierten Aufrufer; Reply-
  Liveness (toter Reply-Owner → `ERR_SERVER_GONE`); Reply-Cap-Server-Migration (Hot-Reload).
- **Capability-Transfer in IPC** (Grant/Delegation) — baut auf dem verifizierten Capability-System.

## 11. Bekannte Grenzen

- **Sequentiell:** **Nebenläufigkeit** (gleichzeitige Mehrkern-Sender/-Empfänger, Interleavings) liegt
  **ausserhalb** — dafür wäre ein Concurrency-Modellprüfer (Loom/TLA+) nötig.
- **Abstraktes Modell:** abgesichert durch `ipc_audit` + den **IPC-Fuzzer** (`ipcfuzz`) auf dem echten
  Code (KILL/Reload/MCS während IPC, Queue-Oracle).

## 12. Trusted Computing Base

1. Lock-Serialisierung (`EPS`/`CAPS`) — Concurrency-/HAL-TCB.
2. Modell↔Code-Treue — durch `ipc_audit` + `ipcfuzz` abgesichert.

## 13. Verbindung zu Runtime-Audits / Kani

- **Laufzeit:** `ipc_audit` (keine toten/dup TCBs in Queues), `ipcfuzz` (nebenläufige Aktoren + Oracle).
- **Verus (hier):** beweist das sequentielle Protokoll. Die Ebenen ergänzen sich (Verus die Logik,
  Fuzzer die Nebenläufigkeit empirisch).

## 14. Verifikationsfortschritt / Nächste Ausbaustufen

- ✅ Endpoint-Rendezvous (Ausschluss/kein Verlust/Fortschritt).
- ⏳ Reply-Caps + Reply-Liveness · Cap-Transfer-in-IPC · (später) Nebenläufigkeit via Loom/TLA+.
