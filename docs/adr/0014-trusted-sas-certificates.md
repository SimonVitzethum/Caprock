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

### 2. Zertifikatsformat (fester Layout, signierter Payload + Signatur)
```text
Offset Feld
0   magic:u32         = 0x54534331 ("TSC1")
4   format_version:u16
6   flags:u16         (Eigenschaften)
8   program_id:u32
12  version:u32
16  binary_hash:[u8;32]    (SHA-256 des ELF)
48  manifest_hash:[u8;32]  (SHA-256 des Manifests)
80  key_id:[u8;16]         (128-bit-Fingerprint = SHA-256(pubkey)[..16])
--- signierter Payload (96 B) ---
96  signature:[u8;64]      (Ed25519 ueber Bytes [0..96))
Gesamt: 160 B
```
Der Kernel akzeptiert ein **TrustedSAS**-Binary nur, wenn **alle** gelten: magic/format_version ok;
`key_id` in der read-only Key-DB; Ed25519-Signatur über den Payload mit diesem PubKey gültig;
`binary_hash==SHA256(elf)` und `manifest_hash==SHA256(manifest)`; `program_id/version` konsistent
zum Archiv-Eintrag; `version >= min_version[program_id]` (Anti-Downgrade); Key nicht `revoked`.

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
3. Zertifikat (Format §2) füllen, mit dem **privaten** Ed25519-Schlüssel signieren.
4. Zertifikat ins Archiv legen (erweitertes `tools/mkarchive.py`).
Ohne den privaten Schlüssel entsteht **kein** gültiges Zertifikat; Besitz von Zertifikat/PubKey/
Key-ID genügt **nie** (asymmetrisch). Das Zertifikat bezeugt: „dieses Programm wurde vollständig
ohne `unsafe` entwickelt — einzige Ausnahme die explizit auditierte Syscall-ABI-Schicht".

## Sicherheitsanalyse (Schutz von Anfang an)

| Angriff | Schutz |
|---|---|
| **Replay / Zertifikatsaustausch** | Cert bindet `binary_hash`+`manifest_hash`+`program_id`+`version`; Cert auf fremdes Binary → Hash-Mismatch → abgelehnt. |
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
