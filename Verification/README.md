# SEL4Lake — Formale Verifikation (komponentenweise)

Dieser Ordner dokumentiert die **schrittweise funktionale Verifikation des Kernelkerns** mit **Verus**
(deduktive Korrektheit) — eigenständig verständlich **ohne Quellcode**. Jede Komponente hat einen
eigenen, vollständigen Unterordner.

## Mehrschichtige Verifikation

Die Ebenen ersetzen sich **nicht**, sondern **ergänzen** + sichern sich gegenseitig ab:

| Ebene | Werkzeug | Aussage |
|---|---|---|
| Runtime-Audits | `*_audit()` im Kernel | Invarianten an Quiescenz-Punkten geprüft (echter Code) |
| Fuzzer | `fuzz`/`ipcfuzz`/`hwfuzz` (Feature `kernel-fuzz`) | randomisierte Op-Sequenzen + Audit je Epoche |
| **Kani** (Tier 1) | bounded Model Checking | Speichersicherheit/Panik-/OOB-Freiheit (`docs/verification.md`); + Kat-A-`unsafe` ([`unsafe-safety/`](unsafe-safety/)) |
| **Verus** (Tier 2, hier) | deduktiv, SMT | funktionale Korrektheit: Operationen erhalten die Invariante **für alle Zustände** |
| **Loom** | exhaustive Interleaving-Exploration | Nebenläufigkeit der Sync-Primitive (RwSpinLock/Ticket-Lock): Ausschluss/kein Lost-Update/kein torn read ([`concurrency/`](concurrency/)) |

Strategie/Stufenmodell + Aufwand: `ARMTest/formale-verifikation-aufwand.md`. Pipeline/Tooling:
`docs/verification.md`.

## Reihenfolge der Komponenten

| Phase | Komponente | Ordner | Status |
|---|---|---|---|
| **1** | **Capability-System** | [`capability-system/`](capability-system/) | **Kern bewiesen** (volle `cap_inv` + install/copy/mint/delete; move/revoke = Reachability-Ausbaustufe) |
| **2** | **Loader** (Zertifikat/Hash/Integrität/Zustandsautomat) | [`loader/`](loader/) | **Kern bewiesen** (Gate-Soundness/Revocation/Atomarität); Endowment offen |
| **3** | **Region-Runtime** (Konservierung/Ownership/Balance) | [`region-runtime/`](region-runtime/) | **Kern bewiesen** (kein Leak/keine Doppel-Freigabe); RegionSource/Zero-Copy offen |
| **4** | **IPC** (CALL/REPLY-Rendezvous, Endpoint-Konsistenz) | [`ipc/`](ipc/) | **Kern bewiesen** (kein Verlust/Duplikat, Fortschritt); Reply-Caps/Nebenläufigkeit offen |
| **5** | **Scheduler** (Runqueue-Konsistenz, MCS-Budget) | [`scheduler/`](scheduler/) | **Kern bewiesen** (Queue-Kopplung/MCS-Schranke/keine Aushungerung/Fortschritt); Prio-Auswahl/Donation offen |
| **6** | **Notifications** (asynchroner Signalkanal) | [`notifications/`](notifications/) | **Kern bewiesen** (kein Signalverlust/genau-einmal-Konsum/kein Lost-Wakeup); Multi-Waiter/Binding offen |
| **7** | **DMA-Lifetime** (Revoke-Reihenfolge) | [`dma-lifetime/`](dma-lifetime/) | **Kern bewiesen** (kein DMA-use-after-free: `detach→free`); Kontext-Aggregation offen |

**Gesamtstand:** sieben Komponenten sind **im Kern funktional verifiziert** (Verus, **90 verified**
über 16 Dateien, CI-gated); zusätzlich ist die **Speichersicherheit der Kategorie-A-`unsafe`-Stellen**
mit Kani bewiesen (s. [`unsafe-safety/`](unsafe-safety/)). Bewusst als nächste Ausbaustufen offen:
CDT-Reachability (move/revoke, Phase 1), Capability-Endowment (Phase 2), RegionSource/Zero-Copy
(Phase 3), Reply-Caps/Cap-Transfer (Phase 4), Prioritäts-Auswahlregel/Budget-Donation (Phase 5),
Multi-Waiter/Binding (Phase 6), Kontext-Aggregation (Phase 7) sowie durchgängig die **Nebenläufigkeit**
(Loom/TLA+, s. Hardware-Vertrauensgrenze).

Daneben dokumentiert [`unsafe-safety/`](unsafe-safety/) (ADR 0021) die schrittweise Kani-Speicher-
sicherheit der Software-`unsafe`-Stellen (Kategorie A); die Hardware-`unsafe` (Pagetables/MMIO/Assembly,
Kategorie B) bleibt die kleine, dokumentierte HAL-TCB.

## Hardware-Vertrauensgrenze (bewusst außerhalb)

SMP · Deferred-IRQ · Locking · MMIO · DMA · Kontextwechsel · HAL · Seitentabellen bleiben außerhalb der
funktionalen Verifikation und werden über dokumentierte `// SAFETY:`-Verträge + eine kleine, klar
definierte TCB beschrieben (Aufwandsanalyse: `ARMTest/unsafe-memory-safety-aufwand.md`).

## Ausführen

```sh
tools/verus-verify.sh      # alle Verus-Beweise (CI-Gate: .gitea/workflows/verus.yml)
tools/kani-verify.sh       # alle Kani-Beweise   (CI-Gate: .gitea/workflows/kani.yml)
tools/loom-verify.sh       # Loom-Concurrency-Modelle der Sync-Primitive (CI-Gate: .gitea/workflows/loom.yml)
```
