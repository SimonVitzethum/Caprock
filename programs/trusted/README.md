# TrustedSAS-Programme (zertifiziert)

TrustedSAS-Programme behalten **geladen** ihre Trust-Stufe (dürfen `PdControl`/`Loader`-Caps halten),
laufen aber wie alle geladenen Prozesse **EL0-isoliert**. Damit nur explizit auditierter, signierter
Code in diese Vertrauensstufe gelangt, lädt der Kernel ein TrustedSAS-Image **nur mit gültigem
Ed25519-Zertifikat** (ext-28, [ADR 0014](../../docs/adr/0014-trusted-sas-certificates.md);
Gate: `kernel::loader::verify_image`).

## Anforderungen an ein TrustedSAS-Programm

1. **Vollständig `#![forbid(unsafe_code)]`** — kein `unsafe` im Programm selbst.
2. Einzige zulässige `unsafe`-Quelle im gesamten Dependency-Baum ist die auditierte Syscall-ABI
   `libcaprock` (**Allowlist**). Keine weiteren Abhängigkeiten mit `unsafe`.
3. Entry über `libcaprock::entry!(run)` mit sicherer `fn run(arg: usize) -> !` — die
   `#[no_mangle] _start`-Glue lebt in der Allowlist-Schicht, nicht im Programm.

Referenz: [`svc-demo/`](svc-demo/) — minimales, sauberes, zertifiziertes Demo.

## Bauen + Signieren

```sh
# 1. ELF bauen (programs-Workspace)
cd programs && cargo build --release

# 2. Auditieren (Unsafe-Allowlist) + hashen + signieren -> Zertifikat
tools/sign_trusted.py \
  --crate programs/trusted/<name> \
  --elf programs/build/target/aarch64-caprock-user/release/<name>.elf \
  --program-id <id> --version <v> --policy internal-test \
  --key keys/trusted-test.ed25519 \
  --out certs/<name>.cert
```

`sign_trusted.py` bricht ab (kein Zertifikat), wenn der Unsafe-Audit fehlschlägt — das Programm ist
dann nicht zertifizierbar. `--program-id`/`--version` **müssen** zum Boot-Archiv-Eintrag passen
(Identitäts-Bindung). Der Audit-Bericht liegt als `<cert>.audit.txt` daneben und ist über
`unsafe_audit_hash` ins Zertifikat eingebunden.

## Ins Boot-Archiv legen

`tools/mkarchive.py`-Spec für TrustedSAS (Domäne `0`), mit leerem Manifest-Feld + Zertifikat:

```
<id>:<name>:0:<version>:<elf>::certs/<name>.cert
```

Der Kernel prüft beim Laden: Signatur über die gesamte Nachricht, `key_id` in der read-only
Kernel-Key-DB (nicht revoziert), `binary_hash == SHA-256(ELF)`, `manifest_hash`,
`program_id`/`version`-Konsistenz, Anti-Downgrade, `unsafe_status == ALL_PASS`. Schlägt **eine**
Prüfung fehl → `LoaderError::Unverified` (kein Thread/keine PD).

Schlüsselverwaltung (Erzeugen/Rotation/Revocation):
[`docs/runbook-trusted-keys.md`](../../docs/runbook-trusted-keys.md).

> Hinweis: `tests/services/trusted/` enthält **adversariale** Testdienste (ext-27): `aggressor-t`
> ist forbid-rein und wird zertifiziert (lädt + attackiert), `intruder-t` trägt absichtlich `unsafe`
> und ist daher **nicht** zertifizierbar — es belegt im Selbsttest, dass unzertifiziertes TrustedSAS
> abgewiesen wird.
