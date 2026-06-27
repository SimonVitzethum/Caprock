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
| **Kani** (Tier 1) | bounded Model Checking | Speichersicherheit/Panik-/OOB-Freiheit (`docs/verification.md`) |
| **Verus** (Tier 2, hier) | deduktiv, SMT | funktionale Korrektheit: Operationen erhalten die Invariante **für alle Zustände** |

Strategie/Stufenmodell + Aufwand: `ARMTest/formale-verifikation-aufwand.md`. Pipeline/Tooling:
`docs/verification.md`.

## Reihenfolge der Komponenten

| Phase | Komponente | Ordner | Status |
|---|---|---|---|
| **1** | **Capability-System** | [`capability-system/`](capability-system/) | **Kern bewiesen** (volle `cap_inv` + install/copy/mint/delete; move/revoke = Reachability-Ausbaustufe) |
| **2** | **Loader** (Zertifikat/Hash/Integrität/Zustandsautomat) | [`loader/`](loader/) | **Kern bewiesen** (Gate-Soundness/Revocation/Atomarität); Endowment offen |
| **3** | **Region-Runtime** (Konservierung/Ownership/Balance) | [`region-runtime/`](region-runtime/) | **Kern bewiesen** (kein Leak/keine Doppel-Freigabe); RegionSource/Zero-Copy offen |
| **4** | **IPC** (CALL/REPLY-Rendezvous, Endpoint-Konsistenz) | [`ipc/`](ipc/) | **Kern bewiesen** (kein Verlust/Duplikat, Fortschritt); Reply-Caps/Nebenläufigkeit offen |
| 5 | Scheduler | [`scheduler/`](scheduler/) | bewusst zuletzt |

## Hardware-Vertrauensgrenze (bewusst außerhalb)

SMP · Deferred-IRQ · Locking · MMIO · DMA · Kontextwechsel · HAL · Seitentabellen bleiben außerhalb der
funktionalen Verifikation und werden über dokumentierte `// SAFETY:`-Verträge + eine kleine, klar
definierte TCB beschrieben (Aufwandsanalyse: `ARMTest/unsafe-memory-safety-aufwand.md`).

## Ausführen

```sh
tools/verus-verify.sh      # alle Verus-Beweise (CI-Gate: .gitea/workflows/verus.yml)
tools/kani-verify.sh       # alle Kani-Beweise   (CI-Gate: .gitea/workflows/kani.yml)
```
