# ADR 0023 — Concurrency-Verifikation der Sync-Primitive (Loom)

Status: **angenommen** · Datum: 2026-06-29 · ergänzt die funktionale Verifikation (Verus/Kani).
Bezug: ADR 0015 (Verifikationsansatz), `crates/sel4lake-sync`, `Verification/concurrency/`,
`docs/invariants.md` §1a (IRQ-safe Locks).

## Kontext

Verus (Tier 2) ist **single-threaded** und kann **Nebenläufigkeit** (Interleavings über Kerne) nicht
erfassen — das wurde durchgängig als HW-/Concurrency-Vertrauensgrenze ausgeklammert. Die
sicherheitskritischen Synchronisationsprimitive (`SpinLock` = IRQ-safe Ticket-Lock, `RwSpinLock` =
writer-bevorzugend) tragen aber die gesamte SMP-Korrektheit. Der `RwSpinLock` enthält eine **subtile**
Stelle: der Release löscht NUR das WRITER-Bit (`fetch_and(!WRITER)`, **nicht** `store(0)`), weil ein
Leser transient optimistisch hochgezählt haben kann. Dieses Argument war bisher nur **per Code-Lektüre**
begründet.

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 nur Code-Review | manuelle Interleaving-Analyse | fehleranfällig; das transiente-Leser-Argument ist subtil |
| V2 TLA+/Spin | formales Modell der Lock-Logik | mächtig, aber separates Modell (Drift zur Realität), hoher Aufwand |
| **V3 Loom (gewählt)** | exhaustive Interleaving-Exploration des **echten Rust-Codes** (getreue Kopie mit loom-Atomics) | prüft die reale Lock-Logik über ALLE Interleavings; klein; mit Sensitivitäts-Gegenprobe |

## Entscheidung

**V3** — ein eigenständiges Loom-Artefakt (`Verification/concurrency/loom/`): **getreue Kopien** der
`RwSpinLock`- und `TicketLock`-Logik mit `loom`-Atomics + `loom::cell::UnsafeCell`. `loom::model()`
exploriert **alle** Thread-Interleavings und prüft: gegenseitiger Ausschluss, **kein Lost-Update**
(zwei Writer → exakt +2), **kein torn read** (ein Reader sieht nur konsistente Werte), und dass der
`fetch_and(!WRITER)`-Release einen **transienten Leser-Zähler** erhält. Via `tools/loom-verify.sh`
(kopiert nach `$TMPDIR` — Loom braucht Host-std + crates.io, der Workspace erzwingt sonst build-std).

**Sensitivitäts-Gegenprobe:** mit injiziertem `store(0)`-statt-`fetch_and`-Bug **fängt Loom** ihn (zwei
der drei RwSpinLock-Beweise schlagen fehl) → der Harness ist nachweislich aussagekräftig (analog zu den
Kernel-Sensitivitätstests).

## Konsequenzen

- **Positiv:** der RwSpinLock-Release (`fetch_and`) ist als **notwendig** maschinen-verifiziert (über
  alle Interleavings), nicht nur per Lektüre; der Ticket-Lock-Ausschluss ebenso. **Erweitert** (zweite
  Stufe) auf zwei weitere Modelle: die **globale Lock-Hierarchie** (`hierarchy.rs` — die belegten
  Schachtelungen CAPS→MEM/DMA_CTX→MEM/SCHEDS→FP_STATES nebenläufig sind deadlock-frei; eine Inversion
  deadlockt nachweislich) und die **Cross-Core-IPC**-Pfade (`crosscore.rs` — one-lock-per-op: je ein
  SCHEDS-Lock, nie zwei → deadlock-frei; zwei gehaltene SCHEDS deadlocken nachweislich). 8 Modelle, je
  sensitivitäts-gegengeprüft.
- **Grenzen/offen:** die Hierarchie-/Cross-Core-Modelle nutzen **Repräsentanten** der realen Locks
  (nicht jeden Pfad statisch); der vollständige „jeder Pfad schachtelt aufsteigend"-Nachweis bleibt beim
  Lock-Ordering-Sweep + den Audits. DAIF-IRQ-Maskierung ist orthogonal (nicht modelliert).
