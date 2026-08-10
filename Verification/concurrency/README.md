# Verifikation — Nebenläufigkeit (Loom) + Krypto-Review

> **Status:** Nebenläufigkeit Loom-verifiziert (8 Modelle, alle Interleavings, sensitivitäts-geprüft):
> Sync-Primitive (RwSpinLock/Ticket-Lock), **globale Lock-Hierarchie** (aufsteigend = deadlock-frei)
> und **Cross-Core-IPC** (one-lock-per-op). Krypto-/Trust-Flow review-verifiziert (kein Befund).

Bezug: [ADR 0023](../../docs/adr/0023-concurrency-verification-loom.md), `crates/caprock-sync`,
`crates/caprock-trust`, `kernel/src/loader.rs` (verify_trusted_cert), `docs/invariants.md` §1a.

## 1. Loom — Concurrency-Verifikation der Sync-Primitive

Verus ist single-threaded; **Nebenläufigkeit** war bisher ausgeklammert. [Loom](https://docs.rs/loom)
exploriert **alle** Thread-Interleavings des **echten** Lock-Codes. Das Artefakt
[`loom/`](loom/) enthält **getreue Kopien** der Lock-Logik aus `caprock-sync` (mit `loom`-Atomics +
`loom::cell::UnsafeCell` statt `core`):

| Modell | Bereich | Eigenschaft | Status |
|---|---|---|---|
| `one_writer_one_reader_no_torn_read` | RwSpinLock | Reader sieht nur konsistente Werte (Mutual-Exclusion W↔R) | ✅ |
| `two_writers_no_lost_update` | RwSpinLock | zwei Writer → final **exakt +2** (kein Lost-Update) | ✅ |
| `writer_with_transient_reader_increment` | RwSpinLock | `fetch_and(!WRITER)`-Release erhält den transienten Leser-Zähler (kein Unterlauf) | ✅ |
| `ticket_mutual_exclusion` | Ticket-SpinLock | FIFO-Ticket-Lock: zwei Threads → final **exakt +2** | ✅ |
| `ascending_nestings_no_deadlock` | **Lock-Hierarchie** | die belegten Kernel-Schachtelungen (CAPS→MEM, DMA_CTX→MEM, SCHEDS→FP_STATES) **nebenläufig** → kein Deadlock | ✅ |
| `disjoint_r1_then_r2_no_deadlock` | **Lock-Hierarchie** | EPS freigeben → dann SCHEDS (R1 vor R2, disjunkt) nebenläufig zu CAPS→MEM | ✅ |
| `concurrent_cross_core_no_deadlock` | **Cross-Core-IPC** | zwei Kerne rufen gleichzeitig cross-core (one-lock-per-op: je ein SCHEDS, nie zwei) → kein Deadlock | ✅ |
| `concurrent_cross_core_shared_ep_no_deadlock` | **Cross-Core-IPC** | wie zuvor, aber über **denselben** Endpoint (EPS serialisiert) → kein Deadlock | ✅ |

**Die subtile Stelle (RwSpinLock-Release):** der Release löscht NUR das WRITER-Bit
(`fetch_and(!RW_WRITER)`), **nicht** `store(0)` — sonst überschriebe er einen Leser, der während des
Write-Holds transient optimistisch `state` hochgezählt hat (→ Unterlauf bei dessen `fetch_sub`).

**Sensitivitäts-Gegenproben (Pflicht — sonst sind die Modelle vacuous):** jeder Bereich wurde mit einem
injizierten Bug gegengeprüft; Loom **fängt** ihn jeweils:
- **RwSpinLock:** `store(0)` statt `fetch_and(!WRITER)` → `one_writer_one_reader_no_torn_read` +
  `writer_with_transient_reader_increment` schlagen fehl (korrumpiertes Interleaving gefunden).
- **Lock-Hierarchie:** eine **Inversion** (`MEM→CAPS` nebenläufig zu `CAPS→MEM`) → Loom meldet
  „deadlock; threads = [Blocked, …]".
- **Cross-Core:** **beide** SCHEDS-Locks zugleich halten (statt one-lock-per-op) → Loom meldet Deadlock.

Damit ist belegt: aufsteigende Schachtelung **ist** deadlock-frei, eine Inversion / zwei gehaltene
SCHEDS **deadlockt** — die Modelle prüfen genau die richtige Eigenschaft.

### Ausführen

```sh
tools/loom-verify.sh      # kopiert nach $TMPDIR + RUSTFLAGS="--cfg loom" cargo test --release
```

Eigenständig (NICHT im Kernel-Workspace), da Loom Host-`std` + crates.io braucht und der Workspace
sonst build-std/Custom-Target erzwingt (gleiches Muster wie `tools/kani-verify.sh`).

### Grenzen

- Die **Lock-Hierarchie** (`hierarchy.rs`) modelliert die im Kernel belegten Schachtelungen
  (invariants.md §1) als Repräsentanten; sie ersetzt nicht den vollständigen statischen Beweis, dass
  **jeder** Pfad aufsteigend schachtelt (das trägt der Lock-Ordering-Sweep + die Audits) — sie zeigt,
  dass die Hierarchie-Disziplin als solche deadlock-frei ist und eine Inversion deadlockt.
- **Cross-Core** (`crosscore.rs`) modelliert das `call()`-`one-lock-per-op`-Muster; reale `unblock`/
  `block_current`/IPI-Details (GIC-SGI) sind abstrahiert (IPI ist asynchrone Aufweck-Notiz, kein Lock).
- DAIF-IRQ-Maskierung (Reentranz-Schutz innerhalb eines Kerns) ist orthogonal zur Thread-Concurrency
  und nicht modelliert (vgl. den IRQ-safe-Lock-Deadlock-Fix, `docs/invariants.md` §1a).

## 2. Krypto-/Trust-Review (kein Befund)

Gezielter Review des TrustedSAS-Zertifikats-Flows (`caprock-trust` + `loader::verify_trusted_cert` +
`caprock-loader/cert.rs`). **Ergebnis: kein Befund** — der Flow ist korrekt:

- **Etablierte Krypto:** Ed25519 (`ed25519-dalek` v2) + SHA-256 (`sha2`), keine Eigenentwicklung;
  **`verify_strict`** (nicht `verify`) → keine Signatur-Malleability an der Vertrauensgrenze.
- **Signatur deckt ALLE geprüften Felder:** `cert.message()` = der gesamte Header (inkl. `binary_hash`,
  `manifest_hash`, `program_id`, `version`, `key_id`, `unsafe_status`) + build_info → ein gültiges
  Zertifikat bindet das Image fest an genau diesen ELF-Hash; kein Feld ohne Signaturbruch manipulierbar.
- **Reihenfolge korrekt:** Signatur **zuerst** verifiziert, **dann** die Feldprüfungen (binary_hash ==
  SHA-256(ELF), program_id/version-Identität, Anti-Downgrade, unsafe_all_pass) gegen signaturgesicherte
  Felder.
- **Key-DB:** Selbstkonsistenz (`key_id == fingerprint(pubkey)`) + Revocation geprüft; Privatkeys nie
  im Kernel; DB read-only + kompiliert.
- **Bounds-sicher:** `TrustedCert::parse` lehnt `data.len() <= msg_len` ab; fixe Offsets < 152 ≤
  data.len(); kein Panic/OOB. **Kein TOCTOU:** `prog.elf` ist immutabel, geladene Bytes ⊆ verifizierte.
- **Empirisch:** `certfuzz` (603 Varianten: Mutation/Truncation/Identitäts-/Binary-Transplantation) →
  alle abgelehnt, echtes Cert akzeptiert, kein Crash/OOB.
