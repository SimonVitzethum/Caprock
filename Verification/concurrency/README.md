# Verifikation — Nebenläufigkeit (Loom) + Krypto-Review

> **Status:** Sync-Primitive concurrency-verifiziert (Loom, 4 Modelle, alle Interleavings,
> sensitivitäts-geprüft). Krypto-/Trust-Flow review-verifiziert (kein Befund).

Bezug: [ADR 0023](../../docs/adr/0023-concurrency-verification-loom.md), `crates/sel4lake-sync`,
`crates/sel4lake-trust`, `kernel/src/loader.rs` (verify_trusted_cert), `docs/invariants.md` §1a.

## 1. Loom — Concurrency-Verifikation der Sync-Primitive

Verus ist single-threaded; **Nebenläufigkeit** war bisher ausgeklammert. [Loom](https://docs.rs/loom)
exploriert **alle** Thread-Interleavings des **echten** Lock-Codes. Das Artefakt
[`loom/`](loom/) enthält **getreue Kopien** der Lock-Logik aus `sel4lake-sync` (mit `loom`-Atomics +
`loom::cell::UnsafeCell` statt `core`):

| Modell | Lock | Eigenschaft | Status |
|---|---|---|---|
| `one_writer_one_reader_no_torn_read` | RwSpinLock | Reader sieht nur konsistente Werte (Mutual-Exclusion W↔R) | ✅ |
| `two_writers_no_lost_update` | RwSpinLock | zwei Writer → final **exakt +2** (kein Lost-Update) | ✅ |
| `writer_with_transient_reader_increment` | RwSpinLock | `fetch_and(!WRITER)`-Release erhält den transienten Leser-Zähler (kein Unterlauf) | ✅ |
| `ticket_mutual_exclusion` | Ticket-SpinLock | FIFO-Ticket-Lock: zwei Threads → final **exakt +2** | ✅ |

**Die subtile Stelle (RwSpinLock-Release):** der Release löscht NUR das WRITER-Bit
(`fetch_and(!RW_WRITER)`), **nicht** `store(0)` — sonst überschriebe er einen Leser, der während des
Write-Holds transient optimistisch `state` hochgezählt hat (→ Unterlauf bei dessen `fetch_sub`).

**Sensitivitäts-Gegenprobe (Pflicht):** mit injiziertem `store(0)`-statt-`fetch_and`-Bug **fängt Loom
den Fehler** — `one_writer_one_reader_no_torn_read` + `writer_with_transient_reader_increment` schlagen
fehl (Loom findet das korrumpierende Interleaving). Der Harness ist also nachweislich aussagekräftig.

### Ausführen

```sh
tools/loom-verify.sh      # kopiert nach $TMPDIR + RUSTFLAGS="--cfg loom" cargo test --release
```

Eigenständig (NICHT im Kernel-Workspace), da Loom Host-`std` + crates.io braucht und der Workspace
sonst build-std/Custom-Target erzwingt (gleiches Muster wie `tools/kani-verify.sh`).

### Grenzen

- Verifiziert die **einzelnen Primitive**, nicht die globale **Lock-Hierarchie** (CAPS < EPS/NTFNS <
  SCHEDS < FP — Azyklizität = Deadlock-Freiheit, separat per `docs/invariants.md` §1 + Lock-Audit).
- DAIF-IRQ-Maskierung (Reentranz-Schutz innerhalb eines Kerns) ist orthogonal zur Thread-Concurrency
  und nicht modelliert (vgl. den IRQ-safe-Lock-Deadlock-Fix, `docs/invariants.md` §1a).
- Cross-Core-Wake/IPI-Pfade (`wake_remote`, `unblock`+IPI) als Loom-Modell = mögliche Folgestufe.

## 2. Krypto-/Trust-Review (kein Befund)

Gezielter Review des TrustedSAS-Zertifikats-Flows (`sel4lake-trust` + `loader::verify_trusted_cert` +
`sel4lake-loader/cert.rs`). **Ergebnis: kein Befund** — der Flow ist korrekt:

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
