# ADR 0014 — TrustedSAS-Zertifikate (signaturbasierte Lade-Autorisierung)

Status: **angenommen** · Datum: 2026-06-27 · Kontext: ext-26/27 (Loader + adversariale Tests)

## Kontext

Der Binary-Loader (ADR 0011) lädt Programme aller drei Domänen EL0-isoliert. Die `Loader`-Cap
gegatet **wer** laden darf; es gibt aber **keine** Prüfung der **Authentizität/Integrität** des
geladenen Binaries. Für **TrustedSAS** (die höchste Trust-Stufe; darf PdControl/Loader-Caps halten)
soll künftig gelten: ein Binary wird **nur** geladen, wenn es ein gültiges **kryptographisches
Zertifikat** besitzt, das nur mit einem **privaten** Signaturschlüssel erzeugt werden kann. Der
Kernel hält **ausschließlich öffentliche** Schlüssel. UserLand/HardwareLand bleiben unverändert.
Die bestehende Capability-/Domänen-/Loader-/Audit-Architektur bleibt vollständig erhalten und wird
nur um diese Vertrauensprüfung ergänzt (Hook `loader::verify_image`).

## Entscheidung

### 1. Signaturverfahren: Ed25519 (RFC 8032), etablierte Implementierung
**Ed25519** — klein (32 B PubKey, 64 B Sig), schnelle no-alloc-Verifikation, keine Nonce-Fallen,
deterministisch. **Keine Eigenentwicklung:** Kernel-Verifikation über **`ed25519-dalek` v2**
(`default-features=false`, no_std, no-alloc; gegen das Custom-Target gespiket — baut sauber) +
**`sha2`** (SHA-256). Signieren passiert **host-seitig** im Build-Tool (RNG nur dort).

### 2. Zertifikatsformat — die **gesamte** Nachricht wird signiert
Nicht nur der Binary-Hash, sondern eine **vollständige Zertifikatsnachricht** wird mit Ed25519
signiert; **jedes** Feld ist damit kryptographisch geschützt (kein Feld nachträglich änder-/
austauschbar). Die Nachricht umfasst die **Build-Identität / das Zertifizierungsverfahren**
(Format-/Signatur-/Buildregel-/Audit-Protokoll-/Unsafe-Regel-/Allowlist-Regel-Version),
**Compiler-/Build-Informationen** und den **Unsafe-Prüfstatus** (§6) — der Signierer bezeugt damit
Identität **und** „nach genau diesem TrustedSAS-Verfahren erzeugt".
**Eingefrorenes Format** (algorithmus-/policy-agnostisch — künftige Krypto-/Policy-Wechsel ohne
Strukturänderung über `signature_algorithm_id`/`certificate_policy_id` + die **variable** Signaturlänge):
```text
Offset Feld (Little-Endian)
0    magic:u32                = 0x5453_4331 ("TSC1")
--- Build-Identitaet / Krypto- & Policy-Identifier (alle signiert) ---
4    cert_format_version:u16   = 1   (Zertifikatsformat)
6    sig_format_version:u16    = 1   (Signaturformat-Version)
8    signature_algorithm_id:u16      (Signaturverfahren; Ed25519 = 1)
10   certificate_policy_id:u32       (Zertifizierungspolitik; z. B. interne Test- / Produktion / formal)
14   build_rules_version:u16         (Compiler-/Buildregel)
16   audit_protocol_version:u16      (TrustedSAS-Audit-Protokoll)
18   unsafe_rules_version:u16        (Unsafe-Pruefregeln)
20   allowlist_rules_version:u16     (Allowlist-Regeln)
---
22   flags:u16                 (Eigenschaften)
24   reserved:u16              (=0, signiert)
26   program_id:u32
30   version:u32
34   binary_hash:[u8;32]       (SHA-256 des ELF — bindet das Zertifikat FEST an genau dieses Binary)
66   manifest_hash:[u8;32]     (SHA-256 des Manifests)
98   key_id:[u8;16]            (128-bit-Fingerprint = SHA-256(pubkey)[..16])
114  unsafe_status:u32         (Bitflags PROGRAM_FORBID|PROJECT_CLEAN|ALLOWLIST_OK; muss ALL_PASS sein)
118  unsafe_audit_hash:[u8;32] (SHA-256 des vollstaendigen Unsafe-Audit-Berichts -> bindet ihn)
150  build_info_len:u16
152  build_info:[..]           (UTF-8: rustc/toolchain/target/profil/zeitstempel)
--- signierte Nachricht endet (msg_len = 152 + build_info_len) ---
msg_len  signature:[..]        (variabel; Algorithmus laut signature_algorithm_id, Ed25519 = 64 B)
```
Der Kernel akzeptiert ein **TrustedSAS**-Binary nur, wenn **alle** gelten: magic/`cert_format_version`
ok; `signature_algorithm_id == Ed25519` + Signaturlänge 64; `key_id` in der read-only Key-DB (Key
nicht `revoked`); Ed25519-Signatur über die **gesamte** Nachricht mit diesem PubKey gültig;
**`binary_hash == SHA256(prog.elf)`** (feste Binary-Bindung) und `manifest_hash ==
SHA256(prog.manifest)`; `program_id/version` konsistent zum Archiv-Eintrag; `version >=
min_version[program_id]` (Anti-Downgrade); `unsafe_status == UNSAFE_ALL_PASS`. Die übrigen
Felder — `certificate_policy_id` + die Verfahrens-Versionen (sig/build/audit/unsafe/allowlist) —
werden **vollständig signiert**
(also integritätsgeschützt + nachvollziehbar), aber zunächst **nicht** erzwungen — der Kernel kann sie
in späteren Versionen prüfen (z. B. eine Mindest-Audit-Protokoll-Version verlangen), ohne dass sich
das Format ändert. Weil die Bindung **kryptographisch** über `binary_hash` läuft (nicht positionell),
ist die separate Ablage des Zertifikats im Archiv sicher: ein Zertifikat passt **ausschließlich** zu
genau dem Binary, dessen Hash es signiert — jede Manipulation des ELF oder jedes Cert↔Binary-Mismatch
wird abgelehnt.

### 3. Key-ID = 128-bit-Fingerprint des PubKey
`key_id = SHA-256(pubkey)[..16]`. **Selbst-zertifizierend** (Kernel prüft beim DB-Laden
`key_id == fingerprint(pubkey)`), 128-bit-Kollisionsresistenz (weit jenseits einiger Dutzend Keys),
16 B = schneller Lookup + kompaktes Zertifikat. Der volle PubKey ist **nicht** Teil des Zertifikats
(nur die Key-ID); die Sig bindet ohnehin den tatsächlichen Schlüssel. (Eine arbiträre 64-bit-Counter-
ID wäre nicht selbst-zertifizierend; der volle Key wäre unnötig groß.)

### 4. Read-only Key-DB im Kernel (kein Syscall, nur Firmware-/Kernel-Update)
Statische, in den Kernel **kompilierte** Tabelle `TRUSTED_KEYS: &[TrustedKey { key_id, pubkey,
revoked }]` (generiert aus den öffentlichen Schlüsseln) + `MIN_VERSION: &[(program_id, min_version)]`.
**Nicht** über Syscalls änderbar; ein laufendes System kann seine Root-Keys **nicht** selbst ändern
— Erweiterung/Änderung ausschließlich durch Neukompilieren (Firmware-/Kernel-Update). Private
Schlüssel liegen **nie** im Kernel.

### 5. Zertifikat im Boot-Archiv (eigener Blob)
Archiv-Format-Version → 2: die freien Entry-`reserved`-Felder werden `cert_off`/`cert_len`. Der
quellen-agnostische `Program`-Deskriptor erhält `cert:&[u8]`. Das **Parsing** des Zertifikats liegt
im `sel4lake-loader` (no_std, `#![forbid(unsafe_code)]`, host-fuzzbar); die **Krypto-Verifikation**
in einem neuen Crate **`sel4lake-trust`** (hält die `ed25519-dalek`/`sha2`-Dep). Die Key-DB liegt im
Kernel; `loader::verify_image` orchestriert: TrustedSAS → Zertifikat zwingend + verifiziert; UserLand/
HardwareLand → unverändert (kein Zertifikat nötig).

### 6. Build-/Signier-Tool (die „Compiler-Erweiterung")
Ein Host-Build-Schritt für ein TrustedSAS-Programm:
1. **unsafe-Audit** des gesamten App-Dependency-Baums (`cargo geiger`): das Programm-Crate und
   **alle projektinternen Crates** müssen **0** `unsafe` enthalten; `unsafe` ist **ausschließlich**
   in einer expliziten **Allowlist** zulässig — genau **`libsel4lake`** (Syscall-ABI/SVC-Stub).
   Jede `unsafe`-Nutzung außerhalb → **sofortiger Abbruch, kein Zertifikat**. Ein **Audit-Bericht**
   listet die Anzahl der `unsafe`-Blöcke je erlaubter Crate (Soll: nur `libsel4lake` > 0).
   (Scope: der Rust-**Sysroot** core/alloc/compiler_builtins ist die vertraute Sprach-Laufzeit —
   wie die CPU/ISA — und außerhalb des Audits; sonst bräche jeder Build.)
2. ELF-Hash + Manifest-Hash (SHA-256) berechnen.
3. Zertifikatsnachricht (Format §2) füllen — inkl. **`unsafe_status`** (Ergebnis des Audits aus 1),
   **`unsafe_audit_hash`** (SHA-256 des vollständigen Audit-Berichts) und **`build_info`** (rustc/
   Toolchain/Target/Profil/Zeitstempel) — und die **gesamte** Nachricht mit dem **privaten** Ed25519-
   Schlüssel signieren. Damit sind Audit-Ergebnis + Build-Provenienz Teil der signierten Aussage.
4. Zertifikat ins Archiv legen (erweitertes `tools/mkarchive.py`).
Ohne den privaten Schlüssel entsteht **kein** gültiges Zertifikat; Besitz von Zertifikat/PubKey/
Key-ID genügt **nie** (asymmetrisch). Das Zertifikat bezeugt: „dieses Programm wurde vollständig
ohne `unsafe` (außer der auditierten Syscall-ABI) **erfolgreich nach den TrustedSAS-Regeln gebaut**,
ist fest an genau dieses Binary gebunden und stammt von diesem Schlüssel" — die **vollständige
Vertrauenskette** Compiler-/Buildprüfung → Unsafe-Audit → Zertifikatserstellung → Ed25519-Signatur →
Kernel-Verifikation. Der Kernel prüft nur noch Signatur + Integrität der Nachricht; alle darin
enthaltenen Aussagen sind dadurch automatisch kryptographisch geschützt.

## Sicherheitsanalyse (Schutz von Anfang an)

| Angriff | Schutz |
|---|---|
| **Binary-Manipulation** | `binary_hash` (SHA-256 des ELF) liegt in der **signierten** Nachricht + Kernel prüft `==SHA256(prog.elf)` → jede ELF-Änderung bricht die Bindung → abgelehnt. |
| **Feld-Manipulation (Version/Flags/Hashes/Status)** | die **gesamte** Nachricht ist signiert → kein Einzelfeld nachträglich änder-/austauschbar (auch `unsafe_status`/`build_info`). |
| **Replay / Zertifikatsaustausch** | Cert bindet `binary_hash`+`manifest_hash`+`program_id`+`version`; Cert auf fremdes Binary → Hash-Mismatch → abgelehnt. |
| **Gefälschter „bestanden"-Status** | `unsafe_status` ist signiert; das Tool signiert nur bei tatsächlich bestandenem Audit; der Kernel erzwingt `==UNSAFE_ALL_PASS`. |
| **Downgrade** | firmware-gebackene `min_version[program_id]`; `version < min_version` → abgelehnt (zustandsloses Boot-System → statische, mit-Update-gepflegte Policy). |
| **Schlüsselrotation** | mehrere Keys (per Key-ID); neuen per Update aufnehmen + neu signieren, alten entfernen → dessen Certs ungültig. |
| **Kompromittierter Signierschlüssel** | **Revocation** = Key per Update entfernen / `revoked` setzen → alle Certs der Key-ID abgelehnt; mehrere Keys begrenzen den Blast-Radius. |
| **Selbst-Eskalation** | Key-DB nicht per Syscall änderbar; nur der private Key signiert. |
| **unsafe-Schmuggel in TrustedSAS** | Build-Tool verweigert Zertifikat bei jeglichem `unsafe` außerhalb der Allowlist. |

## Konsequenzen

- TrustedSAS-Laden ist authentifiziert + integritätsgeprüft + unsafe-frei (bis auf die auditierte
  ABI). UserLand/HardwareLand **unverändert**.
- **Selbsttest-Integration:** der Selbsttest lädt TrustedSAS-Testbinaries (trusted-x, ext-27 aggrt/
  intrt). Mit der neuen Pflicht braucht jedes TrustedSAS-Binary ein gültiges Zertifikat → der
  Selbsttest erhält einen **dedizierten Test-Root-Key** (klar markiert) in der Key-DB und signiert
  seine Trusted-Testbinaries damit beim Build. Der adversariale `intruder-t` (enthält bewusst
  `unsafe`) ist damit **nicht** zertifizierbar → sein Test wird umgewidmet zu „unsigniertes/unsafe-
  behaftetes TrustedSAS-Binary wird vom Loader abgelehnt" (testet direkt das neue Feature). Die
  Hardware-Isolations-Aussage tragen weiter intru/intrh.
- Neue Krypto-Dep **nur kernel-seitig** (Verifier); die Programm-Trust-Basis bleibt klein/unsafe-frei.
- Muster für künftige signierte Quellen (Flash/Netz): dieselbe `Program.cert` + `verify_image`.

## Alternativen (verworfen)

- **Eigenes/handgerolltes Signaturverfahren** — Vorgabe „keine Eigenentwicklung".
- **PubKey 1:1 im Zertifikat** — größer, kein Vorteil ggü. Key-ID (Sig bindet den Key ohnehin).
- **Key-Installation per Syscall** — Vorgabe verbietet es (nur Firmware-/Kernel-Update).
- **`ed25519-compact`** — baut ebenfalls + ist self-contained (1 Dep); für ein **Sicherheits**-Feature
  gibt der breitere Audit-Stand von `ed25519-dalek` den Ausschlag (Deps liegen kernel-seitig).
- **Trampolin/Build-Injektion für „0 unsafe überall"** — vom Nutzer ausdrücklich nicht gewünscht,
  da die kleine auditierte ABI-Crate dasselbe Ziel erreicht.
