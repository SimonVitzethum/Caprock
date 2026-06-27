# ext-28 — TrustedSAS-Zertifikate (Implementierungsplan)

Architektur/Entscheidungen: [ADR 0014](../adr/0014-trusted-sas-certificates.md). Ziel: TrustedSAS-
Binaries nur mit gültigem Ed25519-Zertifikat laden (Key-ID → read-only Kernel-Key-DB → Signatur +
Hash-Bindung + Anti-Downgrade). UserLand/HardwareLand unverändert. Methodik je Phase: bauen →
test-qemu (jeder neue Check + finaler `== ALL PASS ==`) → hang-stress → commit.

## C0 — `sel4lake-trust` (Zertifikat-Parser + Ed25519-Verify) + Key-DB-Typen · host-getestet
- `crates/sel4lake-loader`: `cert.rs` — `TrustedCert`-Parser (fester 160-B-Layout, bounds-geprüft,
  `#![forbid(unsafe_code)]`, panik-frei); `Program.cert:&[u8]`; Archiv-Format v2 (`cert_off/len` in
  `reserved`). Host-Unit-Tests + Negativfälle.
- `crates/sel4lake-trust` (neu, no_std): dep `ed25519-dalek` v2 (`default-features=false`) + `sha2`.
  `verify_cert(payload, sig, pubkey)->bool`, `sha256(&[u8])->[u8;32]`, `fingerprint(pubkey)->[u8;16]`,
  Typen `TrustedKey{key_id,pubkey,revoked}`. Host-Tests mit echten Test-Vektoren (signiert via Tool).
- **Verifikation:** `cargo test` (loader-cert-Parser + trust-verify); Kernel baut weiter (Crate noch
  nicht verdrahtet). hang-stress unverändert.

## C1 — Kernel-Verdrahtung: `verify_image` für TrustedSAS
- Kernel-Key-DB: generierte `kernel/src/trusted_keys.rs` (`TRUSTED_KEYS`, `MIN_VERSION`) — zunächst
  mit einem **Test-Root-Key** (Public-Bytes; privat liegt unter `keys/`, NICHT im Repo-Kernel).
- `loader::verify_image`: für `DOMAIN_TRUSTED` → Zertifikat aus `prog.cert` parsen; Key-ID in DB
  suchen (sonst ablehnen); `verify_cert` (Sig); `binary_hash==sha256(prog.elf)` &&
  `manifest_hash==sha256(prog.manifest)`; `program_id/version` konsistent; `version>=min_version`;
  Key nicht revoked. UserLand/HardwareLand → `true` (unverändert). Fehlerpfade → `Unverified`.
- `tools/mkarchive.py`: optionaler Zertifikat-Blob je Eintrag (Archiv v2).
- **Verifikation:** ein In-Kernel-Test `trustload`: ein gültig signiertes TrustedSAS-Binary lädt;
  Negativfälle (kein Cert / falsche Sig / fremde Key-ID / Hash-Mismatch / Downgrade) → abgelehnt.

## C2 — Build-/Signier-Tool („Compiler-Erweiterung") + signiertes Demo-Programm
- `tools/sign-trusted.py` (bzw. Rust-xtask): (1) `cargo geiger` über den App-Dep-Baum → Programm +
  projektinterne Crates **0** unsafe, `unsafe` nur in Allowlist `{libsel4lake}`; sonst Abbruch +
  **Audit-Bericht** (unsafe-Blöcke je erlaubter Crate). (2) SHA-256(ELF)+SHA-256(Manifest).
  (3) Zertifikat füllen. (4) mit privatem Ed25519-Key signieren (host, z. B. `cryptography`/PyNaCl
  oder Rust-Signer). (5) Cert ausgeben (→ Archiv).
- `keys/`: Test-Root-Keypair generieren (privat NUR lokal/`.gitignore`; public → `trusted_keys.rs`).
- `programs/trusted/svc-demo/`: ein echtes, `#![forbid(unsafe_code)]` TrustedSAS-Demoprogramm
  (nutzt nur `libsel4lake`), signiert.
- **Verifikation:** Tool lehnt ein Programm mit unsafe ab (kein Cert); signiert das saubere Demo;
  der Audit-Bericht zeigt `libsel4lake>0, Rest=0`.

## C3 — Selbsttest-Integration (Bestand erhalten)
- Test-Trusted-Binaries signieren (trusted-x + ext-27 aggressor-t) mit dem Test-Root-Key in
  test-qemu.sh/hang-stress.sh (+ Archiv-Cert). `aggrt`/`loadhw`/`check_loadtrusted` laufen weiter.
- `intruder-t` (bewusst unsafe) → **nicht** zertifizierbar: `intrt` umgewidmet zu „unsigniertes/
  nicht-zertifiziertes TrustedSAS-Binary wird vom Loader abgelehnt" (testet das neue Gate direkt).
- **Verifikation:** voller `test-qemu` grün; `aggrt` lädt (signiert), `intrt` lehnt korrekt ab.

## C4 — Zertifikat-Fuzzer + Trust-Audit
- `certfuzz` (Muster `loaderfuzz`): fehlerhafte/getamperte Zertifikate (Bad-Magic/Version/Längen,
  geflippte Hash-/Sig-/Key-ID-Bits, Downgrade, unbekannte Key-ID) durch den vollen `verify_image`-
  Pfad → **alle** abgelehnt, kein Crash/Panic, kein TrustedSAS geladen. Cert-Parser ist
  `forbid(unsafe_code)` → zusätzlich Host-`cargo test`-Fuzz. Hinter Feature `kernel-fuzz`.
- `trust_audit()` (in `ipc_audit`, neuer Code-Bereich): Key-DB-Selbstkonsistenz
  (`key_id==fingerprint(pubkey)`, keine Null/Dubletten) + Invariante „jede geladene TrustedSAS-PD
  entstammt einem verifizierten Cert". Permanent (feature-unabhängig).
- **Verifikation:** `certfuzz : ALL PASS` (kernel-fuzz-Build); `trust_audit==0` im Release.

## C5 — Dokumentation
- ADR 0014 (steht), ext-28-Bericht, README (`programs/trusted/`, Signier-Workflow, Key-Rotation/
  Revocation-Runbook), `docs/invariants.md` (TrustedSAS-Vertrauenskette), Memory.

## Kritische Dateien
- `crates/sel4lake-loader/src/{lib.rs,archive.rs,cert.rs}` — Program.cert, Archiv v2, Cert-Parser.
- `crates/sel4lake-trust/` (neu) — Ed25519-Verify + sha256 + fingerprint + Key-Typen.
- `kernel/src/{loader.rs,trusted_keys.rs}` — verify_image-Verdrahtung + generierte Key-DB.
- `tools/{mkarchive.py,sign-trusted.py}` — Cert-Blob + Signier-/Audit-Tool.
- `programs/trusted/svc-demo/`, `keys/` (privat gitignored).
- `kernel/src/threads/{mod.rs,fuzz.rs}` — trustload-Test, intrt-Umwidmung, certfuzz, trust_audit.

## Constraints
- Cap-/Domänen-/Loader-/Audit-Architektur **erhalten**, nur ergänzt. Privater Key nie im Kernel/Repo.
- Krypto-Dep nur kernel-seitig; Programm-Trust-Basis bleibt unsafe-frei (bis auf `libsel4lake`).
- Etablierte Krypto (ed25519-dalek), keine Eigenentwicklung. Check-Zahl 58 → ~61 (trustload/intrt-
  reframe/certfuzz) + trust_audit.
