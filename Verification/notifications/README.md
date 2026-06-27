# Verifikation — Notifications (Phase 6)

> **Status:** Kern bewiesen — das asynchrone Signal-/Wait-Protokoll (kein Signalverlust,
> genau-einmal-Konsum, kein Lost-Wakeup) ist formal verifiziert (8 verified, CI-gated).
> Eigenständig verständlich (ohne Quellcode).

Bezug: [ADR 0020](../../docs/adr/0020-notifications-formal-verification.md), ADR 0018 (IPC),
Laufzeit-`Notification::audit`, `docs/verification.md`.

## 1. Motivation und Ziel

Notifications sind der **asynchrone** Signalkanal (Badge-akkumulierte Ereignisse, u. a. Deferred-IRQ-
Zustellung an HardwareLand-Backends). Geht ein Signal verloren oder bleibt ein Wartender trotz
anstehender Signale blockiert (**Lost-Wakeup**), ist der Kanal fehlerhaft. **Ziel:** das Signal-/Wait-
Kernprotokoll **beweisen** (statisch, alle Zustände).

## 2. Sicherheitsmodell

- Ein **Notification-Objekt** akkumuliert Badge-Bits (`pending`, per ODER) und hat **höchstens einen**
  blockierten Wartenden (`waiter`).
- **Invariante:** ein blockierter Wartender sitzt **nie** auf unzugestellten Signalen — gäbe es ein
  pending-Bit, wäre er sofort geweckt worden (`waiter is Some ==> pending leer`).
- **Sequentiell:** eigener Lock je Objekt; kern-übergreifendes Wecken (`unblock`+IPI) +
  Nebenläufigkeit bleiben **ausserhalb** (Concurrency-/HAL-TCB, ADR 0020).

## 3. Zu beweisende Eigenschaften

1. **Invariante erhalten:** `signal`/`wait`/`purge` bewahren „Wartender ⟹ kein pending".
2. **Kein Signalverlust:** jedes signalisierte Bit ist danach im pending-Wort **oder** an den
   geweckten Wartenden zugestellt.
3. **Genau-einmal-Konsum:** `wait` holt das gesamte pending-Wort ab und leert es (kein Rest/Duplikat).
4. **Fortschritt:** bei anstehendem Wort blockiert `wait` nicht (kein Lost-Wakeup).

## 4. Bezug zu ADRs

ADR 0020 (diese Verifikation) · ADR 0018 (synchrones IPC, Schwesterkanal).

## 5. Formale Spezifikation

`Ntfn { used, pending: Set<nat>, waiter: Option<nat> }`. Badge-Bits als **Menge** (ODER ≙ Vereinigung).
`ntfn_inv` = `waiter is Some ==> pending =~= ∅`. `signal(n, badge)` = pending ∪ badge, bei Wartendem
sofortige Zustellung (`delivered`) + Leeren; `wait(n, tid)` = bei nichtleerem Wort Drain, sonst
Blockieren; `purge(n)` = Wartenden entfernen.

## 6. Verus-Architektur

[`proofs/notification.rs`](proofs/notification.rs), per `tools/verus-verify.sh` + Verus-CI-Gate.
Abstraktes, sequentielles Mengenmodell (V2, ADR 0020); realer Code unverändert.

## 7. Beweisstrategie

Mengen-Reasoning (`union`/`subset_of`/`=~=`, Verus-`Set`); die Invariante macht den `wait`-Drain-Fall
mit der Lost-Wakeup-Freiheit konsistent (ein Wartender ⟹ leeres Wort ⟹ kein verpasstes Signal).

## 8. Lemmas / 9. Bewiesene Eigenschaften

| Theorem | Aussage | Status |
|---|---|---|
| `signal_preserves_inv` / `wait_preserves_inv` / `purge_preserves_inv` | alle Operationen erhalten `ntfn_inv` | ✅ |
| `signal_no_loss` | jedes signalisierte Bit landet in pending **oder** delivered (kein Verlust) | ✅ |
| `signal_wakes_waiter` | bei Wartendem: er erhält das **gesamte** Wort, danach kein Wartender + leer | ✅ |
| `wait_consumes_all` | `wait` holt **genau** das alte Wort ab und leert es (genau-einmal-Konsum) | ✅ |
| `wait_progress` | anstehendes Wort ⟹ `wait` blockiert nicht (kein Lost-Wakeup) | ✅ |

(8 verified inkl. `main`.)

## 10. Noch offene Eigenschaften

- **Mehrere Wartende** (aktuell ein Konsument je Notification) — Warteschlange + faire Zustellung.
- **Notification-Binding an einen TCB** (gebundene asynchrone Zustellung an einen bestimmten Thread).

## 11. Bekannte Grenzen

- **Sequentiell:** kern-übergreifendes Wecken (`unblock`+IPI) + **Nebenläufigkeit** liegen
  **ausserhalb** (Loom/TLA+).
- **Mengen- statt Bit-Wort-Modell:** die konkrete `u64`-Badge-Akkumulation (`|=`) ist mengentheoretisch
  abstrahiert; ihre Treue sichern `Notification::audit` + der Fuzzer auf dem **echten** Code.

## 12. Trusted Computing Base

1. Lock-Serialisierung je Objekt + kern-übergreifendes `unblock`+IPI — Concurrency-/HAL-TCB.
2. Modell↔Code-Treue (Set statt u64-Bitwort) — durch `Notification::audit` + Fuzzer abgesichert.

## 13. Verbindung zu Runtime-Audits / Kani

- **Laufzeit:** `Notification::audit` (kein toter Wartender, in `ipc_audit` als Code 3 aggregiert),
  Fuzzer (randomisierte signal/wait/purge-Sequenzen + Audit je Epoche).
- **Verus (hier):** beweist, dass kein Signal verloren geht und kein Lost-Wakeup auftritt — für **alle**
  Zustände. Die Ebenen ergänzen sich.

## 14. Verifikationsfortschritt / Nächste Ausbaustufen

- ✅ Signal-/Wait-Kernprotokoll (kein Verlust/genau-einmal/kein Lost-Wakeup).
- ⏳ Mehrere Wartende (faire Queue) · TCB-Binding · (später) Nebenläufigkeit via Loom/TLA+.
