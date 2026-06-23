# Phasenbericht 7 — Hot-Reload

**Datum:** 2026-06-23 · **Status:** abgeschlossen, in QEMU verifiziert (8 Kerne)

## Was umgesetzt wurde

Das Kernversprechen der Architektur: eine Komponente (Server-PD) wird im
**laufenden System ersetzt — ohne Kernel-Neustart** und für Clients transparent,
weil die **Endpoint-Capability stabil** bleibt (ADR 0006).

### Neue Primitive

- `sel4lake-ipc`: `EndpointTable::retire_receiver(ep, tid)` zieht einen
  blockierten Empfänger von einem Endpoint zurück (Quiesce).
- `sel4lake-microkit`: `PdTable::clear_cap(pd, slot)` entzieht einer PD eine
  Capability (Autoritätsentzug).
- `kernel/system`: Wrapper `endpoint_retire_receiver` / `clear_pd_cap`.

### Hot-Reload-Ablauf (`kernel/src/threads.rs`)

Der **Reload-Manager** (Idle-Thread des Primärkerns) führt aus:

1. **Quiesce:** v1 als Endpoint-Empfänger zurückziehen *und* ihm die Recv-Cap
   entziehen. Neue Client-Aufrufe blockieren dann am Endpoint (queuen) statt
   verloren zu gehen.
2. **Swap:** Server v2 starten und an seine PD binden (die bereits eine Recv-Cap
   auf *denselben* Endpoint hält). Atomar gegen Preemption (`local_irq_disable`),
   damit v2 nicht vor dem Bind läuft.
3. **Resume:** v2 empfängt am Endpoint und bearbeitet die (ggf. gequeueten)
   Aufrufe. Der Client merkt nichts — gleiche Send-Cap, gleicher Endpoint.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS**. Server v1 verdoppelt, v2 (nach Reload)
verdreifacht:

```
ipc     : v1 call(5) -> 10   v1 call(6) -> 12      ipc    : ALL PASS
       [ Hot-Reload: v1 zurückgezogen, v2 gestartet ]
reload  : v2 call(5) -> 15   v2 call(6) -> 18      reload : ALL PASS
          (Komponente ohne Kernel-Neustart getauscht)
```

Beweis: Batch 1 (verdoppelt) wird von v1 bedient, Batch 2 (verdreifacht) vom
ersetzten v2 — über **denselben** Endpoint und **dieselbe** Client-Send-Cap, ohne
Reboot. Plus weiterhin: MMU, memtest, captest, sched (Preemption), 8/8 Kerne.

## Getroffene Entscheidungen

- **Stabile Endpoint-Cap als Schnittstelle, austauschbarer Server dahinter**
  (ADR 0006): genau das Modell „Capability = stabile Identität, Komponente =
  Inhalt". Der Client referenziert den Endpoint über seine unveränderte Send-Cap.
- **Quiesce über das Endpoint-Verhalten:** ohne Empfänger blockieren Sender am
  Endpoint (queuen) — kein Aufruf geht im Swap-Fenster verloren.
- **Deterministische Retirierung (gegen Race):** v1 wird sowohl als Empfänger
  zurückgezogen (falls blockiert → geparkt) als auch die Recv-Cap entzogen (falls
  v1 erst später erneut empfangen will → Recv-Fehler → v1 zieht sich selbst
  zurück). Damit ist der Swap unabhängig vom genauen Timing korrekt.
- **Reload-Manager als (privilegierter) Idle-Thread:** orchestriert die Policy
  über Kernel-Primitive — entspricht dem „Reload-Manager-PD" aus ADR 0006.

## Unsafe-Bilanz

Keine neuen `unsafe`-Stellen: `retire_receiver`/`remove`/`clear_cap` sind reine
Datenstruktur-Logik (0 unsafe in ipc/microkit). Kernel-Crate weiterhin **0
`unsafe`-Blöcke**.

## Risiken / offene Punkte

- **Kein Zustands-Checkpoint:** der Demo-Server ist zustandslos. Für stateful
  Komponenten fehlt der Checkpoint/Restore-Schritt (Zustand in eine Memory-Cap
  exportieren und an v2 übergeben — im SAS zero-copy, ADR 0006). Vorgemerkt.
- **v1 wird nur geparkt, nicht abgeräumt:** TCB + Stack des alten Servers werden
  nicht freigegeben (Thread-Exit/Join fehlt, Phase 4). Speicherleck pro Reload.
- **Statisch verdrahteter Reload:** der Manager kennt v1/v2 fest. Ein generischer
  Reload-Dienst (beliebige PD per Name/Cap ersetzen) + deklaratives Systembild
  sind die nächste Ausbaustufe.
- **Übertragene, aber nicht abgeschlossene Aufrufe:** im Demo-Fenster ruft der
  Client nicht (er wartet auf `RELOADED`); ein echter Drain in-flight laufender
  Requests (statt nur Quiesce) ist für nebenläufige Clients nötig.

## Stand der geplanten Roadmap

Phasen 0–7 sind abgeschlossen und in QEMU verifiziert (`./test-qemu.sh` = ALL
PASS): Bring-up, HAL (MMU/W^X, GIC, Timer, SMP), capability-basiertes
Speichermodell, Capability-System (CDT), Scheduler, IPC, Microkit/cap-gesicherte
IPC und Hot-Reload.

## Mögliche nächste Ausbaustufen (über die Roadmap hinaus)

1. **Lazy-FP** (FP/SIMD-Kontext bei Bedarf sichern) + **Bitmap-Prioritäten** +
   **Per-Kern-Locks** (Scheduler/IPC entkoppeln vom globalen Lock).
2. **Thread-Lebenszyklus** (Exit/Join, TCB+Stack freigeben) und **TCBs als Caps**.
3. **Capability-Transfer in IPC** + **Zero-Copy-Nutzdaten via Memory-Cap**;
   **Notifications**.
4. **Zustands-Checkpoint** für stateful Hot-Reload; generischer Reload-Dienst.
5. **DTB-Parsing** statt fester Plattformwerte; erste echte Userland-Treiber
   (EL0) — erfordert die Entscheidung zu Hardware-Härtung untrusted Codes (ADR 0002).
