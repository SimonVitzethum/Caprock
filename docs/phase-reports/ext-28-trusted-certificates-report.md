# ext-28 — TrustedSAS-Zertifikate (Abschlussbericht)

Architektur/Entscheidungen: [ADR 0014](../adr/0014-trusted-sas-certificates.md).
Schlüssel-Runbook: [trusted-key-rotation.md](../runbook-trusted-keys.md).
Plan: [ext-28-trusted-certificates-plan.md](ext-28-trusted-certificates-plan.md).

## Ziel

Ein als **TrustedSAS** deklariertes Image wird **nur** geladen, wenn ein **gültiges,
kryptographisch auf genau dieses Binary gebundenes Zertifikat** vorliegt. TrustedSAS behält seine
Trust-Stufe (darf `PdControl`/`Loader`-Caps halten) — also muss der Kernel sicherstellen, dass nur
explizit signierter, auditierter Code in diese Vertrauensstufe gelangt. **UserLand/HardwareLand
bleiben unverändert** (hardware-isoliert, kein Zertifikat nötig: ein bösartiges EL0-Image faultet
nur sich selbst).

Zentrale Sicherheitsregeln (ADR 0014):
- Der Kernel hält **ausschließlich öffentliche** Schlüssel; private Schlüssel liegen **nie** im
  Kernel/Repo.
- Die Key-DB ist in den Kernel kompiliert und **nur per Firmware-/Kernel-Update** änderbar —
  **nie** per Syscall.
- **Etablierte Krypto, keine Eigenentwicklung:** Ed25519 (RFC 8032, `ed25519-dalek`) + SHA-256
  (`sha2`).
- Ein Build-/Signier-Tool beweist host-seitig, dass das Programm **unsafe-frei** ist (Allowlist
  **nur** `libcaprock`), hasht Binary + Manifest, baut das Zertifikat und signiert es.

## Vertrauenskette (Ende zu Ende)

```
 HOST (privat)                          ARCHIV                 KERNEL (nur öffentlich)
 ────────────                           ──────                 ──────────────────────
 tools/sign_trusted.py                  boot-archive.bin       loader::verify_image (DOMAIN_TRUSTED)
  1. Unsafe-Audit (cargo metadata,       Eintrag (v2):          1. TrustedCert::parse  (bounds, panik-frei)
     Allowlist {libcaprock})              blob (ELF)           2. signature_algorithm_id==Ed25519, |sig|==64
  2. SHA-256(ELF), SHA-256(Manifest)       manifest            3. key_id -> TRUSTED_KEYS (read-only, kompiliert)
  3. Zertifikatsnachricht füllen           cert  <─────────┐      revoked? key_id==fingerprint(pubkey)?
     (eingefrorenes Format)              ─────────────────┘   4. verify_strict(pubkey, message, sig)
  4. Ed25519-Signatur über die                                 5. binary_hash==SHA-256(ELF) & manifest_hash==…
     VOLLE Nachricht (privater Key)       gen_trusted_key.py    6. program_id/version == Archiv-Eintrag
                                          erzeugt keys/* +      7. version >= MIN_VERSION[program_id]
 keys/trusted-test.ed25519 (gitignored)   kernel/src/           8. unsafe_status == ALL_PASS
                                          trusted_keys.rs       sonst -> LoaderError::Unverified (kein Laden)
```

Ohne den privaten Schlüssel entsteht kein gültiges Zertifikat; jede Manipulation an Binary,
Manifest, Identität, Algorithmus oder Unsafe-Status bricht die Signatur über die **gesamte**
Nachricht.

## Zertifikatsformat (eingefroren)

Definiert + bounds-geprüft geparst in [`crates/caprock-loader/src/cert.rs`](../../crates/caprock-loader/src/cert.rs)
(`#![forbid(unsafe_code)]`, panik-frei, host-fuzzbar). Little-Endian, die **gesamte** Nachricht
`[0..msg_len)` wird signiert:

| Offset | Feld | |
|---|---|---|
| 0 | `magic` u32 = `0x5453_4331` ("TSC1") | |
| 4 | `cert_format_version` u16 = 1 | Build-Identität / Krypto- & Policy-IDs … |
| 6 | `sig_format_version` u16 = 1 | |
| 8 | `signature_algorithm_id` u16 | Ed25519 = 1 |
| 10 | `certificate_policy_id` u32 | v1 / formal / internal-test / production |
| 14 | `build_rules_version` u16 = 1 | |
| 16 | `audit_protocol_version` u16 = 1 | |
| 18 | `unsafe_rules_version` u16 = 1 | |
| 20 | `allowlist_rules_version` u16 = 1 | |
| 22 | `flags` u16 / 24 `reserved` u16 | |
| 26 | `program_id` u32 / 30 `version` u32 | Identitäts-Bindung |
| 34 | `binary_hash` [32] | **bindet das Zertifikat an genau dies Binary** |
| 66 | `manifest_hash` [32] | |
| 98 | `key_id` [16] | = `SHA-256(pubkey)[..16]` |
| 114 | `unsafe_status` u32 | Bitflags; muss `ALL_PASS` |
| 118 | `unsafe_audit_hash` [32] | bindet den vollständigen Audit-Bericht |
| 150 | `build_info_len` u16 / 152 `build_info[..]` | rustc/Toolchain/Target/Profil |
| `msg_len` | `signature[..]` | variabel; Ed25519 = 64 B |

Variable Signaturlänge + `signature_algorithm_id`/`certificate_policy_id` erlauben künftige Krypto-/
Policy-Wechsel **ohne** Strukturänderung. Das Format ist seit `41ece2d` **stabil**; C2–C5 bauen
ausschließlich darauf auf.

## Unsafe-Audit + Allowlist + `entry!`-Makro

Ein TrustedSAS-Programm wird **vollständig ohne `unsafe`** entwickelt (`#![forbid(unsafe_code)]`).
Die **einzige** zugelassene Ausnahme im gesamten Dependency-Baum ist die explizit auditierte
Syscall-ABI-Schicht `libcaprock` (Allowlist). `tools/sign_trusted.py` setzt das durch:

- `cargo metadata` → transitiver App-Dep-Baum (Sysroot `core`/`alloc`/`compiler_builtins` =
  vertraute Sprach-Laufzeit, außer Scope).
- Je Crate: Scan auf reale `unsafe`-Nutzung; das Programm-Crate muss `#![forbid(unsafe_code)]`
  tragen + 0 `unsafe` haben; `unsafe` ist **nur** in `libcaprock` erlaubt.
- Jede Verletzung → **Abbruch, KEIN Zertifikat**. Ein Audit-Bericht (`<cert>.audit.txt`) listet die
  `unsafe`-Anzahl je Crate; sein SHA-256 wird als `unsafe_audit_hash` im Zertifikat verankert.

**Spannung gelöst:** In aktuellem Rust ist `#[no_mangle]` ein *unsafe* Attribut und von
`forbid(unsafe_code)` blockiert. Der ELF-Entry-Point `_start` gehört daher in die auditierte
SDK-Schicht: `libcaprock::entry!(run)` erzeugt die `#[no_mangle]`-Glue (Makro-Hygiene der externen
Crate), das Programm selbst stellt nur eine **sichere** `fn run(arg: usize) -> !` bereit und bleibt
forbid-rein. Das ist **kein** „Trampolin zum Verstecken von Programm-`unsafe`", sondern
Standard-Runtime-Support in der Allowlist-Crate.

## Sicherheitsanalyse (Angriff → Abwehr)

| Angriff | Abwehr (verify_image / Tool) |
|---|---|
| Binary nachträglich manipuliert | `binary_hash == SHA-256(ELF)` + Signatur über die Nachricht |
| Einzelnes Zertifikatsfeld gefälscht | Signatur über die **gesamte** Nachricht (jedes Feld geschützt) |
| Zertifikat eines anderen Binaries aufheften (Replay/Transplantation) | `binary_hash` + `program_id`/`version` an den Archiv-Eintrag gebunden |
| Downgrade auf alte verwundbare Version | `version >= MIN_VERSION[program_id]` (firmware-gepflegt) |
| Selbst-signiertes Zertifikat (fremder Key) | `key_id` muss in der read-only Kernel-Key-DB stehen |
| Kompromittierter/rotierter Schlüssel | `revoked`-Flag in der DB → alle Zertifikate dieser Key-ID abgelehnt; Rotation via Firmware-Update |
| `unsafe`-Schmuggel ins Trusted-Programm | Host-Audit (Allowlist) → kein Cert; Kernel verlangt `unsafe_status == ALL_PASS` |
| Gefälschter Unsafe-Status | `unsafe_status` signiert; `unsafe_audit_hash` bindet den Bericht |
| Selbst-Eskalation einer geladenen TrustedSAS-PD | unverändert: geladen EL0-isoliert; Macht nur aus tatsächlich gehaltenen Caps (siehe `aggrt`) |
| Key-DB per Syscall ändern | Es gibt **keinen** solchen Syscall; DB ist kompiliert + read-only |

## Verifikation (Belege)

**Host (eigenständig, außerhalb des build-std-Workspace):**
- `caprock-trust`: 5/5 Tests (RFC-8032-Vektoren) unter `verify_strict`.
- `caprock-loader`: 26 Tests (Archiv v2 + Cert-Parser inkl. Negativfälle).
- Round-Trip `python-cryptography` ↔ `ed25519-dalek`: Signatur gültig, `key_id ==
  fingerprint(pubkey)`, `binary_hash == sha256(ELF)`, `ALL_PASS`, Tamper abgelehnt.
- `sign_trusted.py` lehnt ein Programm mit `unsafe` ab (FAIL, kein Cert, Exit 1).

**Kernel-Selbsttest (`== ALL PASS == -> system_off`):**
- `loadhw`/`load`: **positiv** — `trusted-x` (= signiertes `svc-demo`, `program_id 12`) lädt als
  EL0-isolierte TrustedSAS-PD; `trust_audit == 0` (Key-DB selbstkonsistent + Live-Oracle: gültiges
  Cert akzeptiert, manipuliertes abgelehnt).
- `aggrt`: **positiv** — zertifizierter TrustedSAS-Aggressor lädt + attackiert; „Trust ≠ Privileg"
  weiterhin belegt.
- `intrt`: **negativ (umgewidmet)** — `intruder-t` trägt absichtlich `unsafe` (nicht
  zertifizierbar), liegt **ohne** Zertifikat im Archiv → `verify_image` weist das Laden mit
  `Unverified` ab; **kein** Thread/keine PD; Audits 0.
- `cross`: zertifizierter `aggressor-t` lädt nebenläufig mit den anderen Domänen.

**Fuzzer (`KERNEL_FUZZ=1`, ADR 0013):**
- `certfuzz`: 603 Zertifikatsvarianten (Byte-Mutation, Truncation, Feld-Korruption, Zufallsmüll +
  Identitäts-/Binary-Transplantation eines gültigen Certs auf falsche `program_id`/`version`/ELF)
  durch das `verify_image`-Gate → **alle** abgelehnt, echtes Cert akzeptiert, kein Crash/OOB
  (no-alloc Krypto), `trust_audit + loader_audit == 0`, `total_free` unverändert.

## Geänderte/neue Dateien

| Datei | Rolle |
|---|---|
| `crates/caprock-loader/src/cert.rs` | Eingefrorener Cert-Parser (forbid-unsafe, panik-frei) |
| `crates/caprock-trust/` | Ed25519-`verify_strict` + SHA-256 + `fingerprint` + `TrustedKey` (no_std, no-alloc) |
| `crates/caprock-loader/src/archive.rs` | Archiv-Format v2 (`cert_off/cert_len`) |
| `kernel/src/loader.rs` | `verify_image`/`verify_trusted_cert` (Gate) + `trust_audit` + `verify_only` |
| `kernel/src/trusted_keys.rs` | Read-only Key-DB (autogeneriert; nur PubKeys) |
| `kernel/src/threads/{mod.rs,fuzz.rs}` | `trust_audit`-Wiring, `intruder-t`-Umwidmung, `certfuzz` |
| `programs/libcaprock/src/lib.rs` | `entry!`-Makro (Entry-Glue in der Allowlist-Schicht) |
| `programs/trusted/svc-demo/` | Sauberes, zertifiziertes Demo-TrustedSAS-Programm |
| `tools/sign_trusted.py` | Unsafe-Audit + Hashes + Ed25519-Signatur (host) |
| `tools/gen_trusted_key.py` | Keypair + Key-DB-Generierung |
| `tools/mkarchive.py` | Cert-Blob je Eintrag (7. Spec-Feld) |
| `docs/adr/0014-trusted-sas-certificates.md` | Architekturentscheidung |
| `docs/runbook-trusted-keys.md` | Rotation/Revocation/Erst-Setup |

## Bewusst aufgeschoben

- Mehrere Root-Keys / Cross-Signing / Key-Hierarchie (DB unterstützt mehrere Keys, genutzt wird
  einer).
- `certificate_policy_id`-**Erzwingung** (heute signiert, aber nicht policy-geprüft) + formal
  verifizierte Programme (`POLICY_TRUSTEDSAS_FORMAL`).
- Reale Plattform-Schlüsselverwahrung (HSM/Secure Boot) statt lokaler `keys/`-Testschlüssel.
