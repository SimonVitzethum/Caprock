# Verifikation — Loader (Phase 2)

> **Status:** Kern bewiesen — die **Sicherheitslogik** des Loaders (Zertifikats-Gate + Lade-Zustands-
> automat) ist formal verifiziert (7 verified, CI-gated). Dieses Dokument ist **eigenständig
> verständlich** (ohne Quellcode).

Bezug: [ADR 0016](../../docs/adr/0016-loader-formal-verification.md) (Verifikationsentscheidung),
ADR 0011 (Binary-Loader), ADR 0014 (TrustedSAS-Zertifikate), `docs/verification.md`.

## 1. Motivation und Ziel

Der Loader entscheidet, **welcher Code mit welcher Autorität** läuft. Die kritischste Aussage:
**ein TrustedSAS-Image (das PdControl/Loader-Caps halten darf) wird NUR mit gültigem, an genau dieses
Binary gebundenem Ed25519-Zertifikat geladen.** Bisher prüft das der Selbsttest + Fuzzer; **Ziel von
Phase 2:** diese Lade-Sicherheitslogik **beweisen** (statisch, alle Eingaben).

## 2. Sicherheitsmodell

- Ein Programm trägt eine **Domäne** (TrustedSas/HardwareLand/UserLand) + (für TrustedSAS) ein
  **Zertifikat** mit kryptographisch geschützten, an das Binary gebundenen Fakten.
- Eine in den Kernel kompilierte, **read-only** Key-DB (nur per Firmware-Update änderbar).
- **Gate `verify_image`:** UserLand/HardwareLand laden bedingungslos (hardware-isoliert); TrustedSAS
  nur, wenn das Zertifikat **alle** Checks besteht.
- **Lade-Zustandsautomat:** `verify_image` läuft **vor** jeder Ressourcenvergabe → ein abgewiesenes
  Image erzeugt **weder Thread noch PD** (atomar, kein inkonsistenter Ladezustand).

## 3. Zu beweisende Eigenschaften

1. **Soundness:** TrustedSAS akzeptiert ⟹ gültiger (nicht-revozierter, selbstkonsistenter) Schlüssel
   signierte ein Zertifikat, das Binary/Manifest **bindet**, Identität (program_id/version) trifft,
   nicht downgegradet ist und `unsafe_status == ALL_PASS` hat.
2. **Revocation:** akzeptiert ⟹ Schlüssel nicht zurückgezogen; **alle** Schlüssel der Key-ID
   zurückgezogen ⟹ abgewiesen.
3. **Domänen-Gating:** UserLand/HardwareLand laden immer; nur TrustedSAS ist gegatet.
4. **Atomarität:** `load` liefert **genau einen** Ausgang (`Loaded{domain}` ⟺ Gate bestanden, sonst
   `Rejected`) — kein Zwischen-/Teilzustand.
5. **Determinismus:** read-only Key-DB ⟹ dieselbe Eingabe → dieselbe Entscheidung.

## 4. Bezug zu ADRs

ADR 0016 (diese Verifikation) · ADR 0014 (Zertifikatsformat + Checks) · ADR 0011 (Loader-Architektur).

## 5. Formale Spezifikation

`Cert` bildet **jedes** Feld als einen `verify_trusted_cert`-Check ab (`alg_ed25519`, `sig_len_64`,
`key_id`, `sig_valid`, `binary_hash_ok`, `manifest_hash_ok`, `program_id_ok`, `version_ok`,
`version_ge_min`, `unsafe_all_pass`). `cert_accepted(db, c)` = Konjunktion aller Checks + `valid_key`
(ein nicht-revozierter, selbstkonsistenter Schlüssel der Key-ID existiert). `verify_image` = das
Domänen-Gate. `load` = Gate → `Loaded{domain}` | `Rejected`.

## 6. Verus-Architektur

[`proofs/load_gate.rs`](proofs/load_gate.rs), verifiziert per `tools/verus-verify.sh` + Verus-CI-Gate.
**Krypto abstrahiert** (V2, ADR 0016): `sig_valid`/`*_hash_ok` sind die durch die Signatur geschützten
**Fakten** — die Krypto-Treue tragen die Bibliotheken + der Host-Round-Trip (ext-28). Realer Code
unverändert.

## 7. Beweisstrategie

Die Eigenschaften folgen aus der Definition von `cert_accepted`/`verify_image`/`load` durch
Spec-Entfaltung (Verus-SMT); Revocation-Vollständigkeit nutzt `assert(!valid_key(...))`.

## 8. Lemmas / 9. Bewiesene Eigenschaften

| Theorem | Aussage | Status |
|---|---|---|
| `soundness_trusted` | akzeptiert ⟹ alle Cert-Checks (Bindung/Identität/Anti-Downgrade/ALL_PASS) | ✅ |
| `accepted_key_not_revoked` | akzeptiert ⟹ Schlüssel nicht revoziert | ✅ |
| `all_revoked_rejects` | alle Schlüssel der Key-ID revoziert ⟹ abgewiesen | ✅ |
| `untrusted_loads_unconditionally` | UserLand/HardwareLand laden immer | ✅ |
| `load_atomic` | genau ein Ladeausgang; Loaded ⟺ Gate, Domäne übernommen | ✅ |
| `deterministic` | read-only DB ⟹ reproduzierbare Entscheidung | ✅ |

(7 verified inkl. `main`.)

## 10. Noch offene Eigenschaften

- **Capability-Endowment beim Laden** (die geladene PD erhält genau die domänen-konformen Caps) —
  baut auf der **bewiesenen Domänen-Policy** (Phase 1, `verus/domain_policy.rs`) auf; nächste Stufe.
- **Verbindung zum Parser:** dass die abstrakten `*_ok`-Fakten exakt den Parser-Ausgaben entsprechen
  (Modell↔Code), gestützt durch die Kani-Parser-Beweise + den Host-Round-Trip.

## 11. Bekannte Grenzen

- **Krypto abstrahiert:** Ed25519/SHA-256-Korrektheit ist Bibliotheks-TCB (s. §12).
- **Modell, nicht realer Code:** abgesichert durch Selbsttest (`load`/`loadhw`/`intrt`), `trust_audit`
  + Fuzzer (`certfuzz`) auf dem **echten** Loader.

## 12. Trusted Computing Base

1. Krypto-Primitive (ed25519-dalek `verify_strict`, sha2) — etablierte Bibliotheken.
2. Parser-Speichersicherheit — **mit Kani bewiesen** (cert/archive/elf, `docs/verification.md`).
3. Modell↔Code-Treue — durch Audit+Fuzzer auf dem echten Loader abgesichert.

## 13. Verbindung zu Runtime-Audits / Kani

- **Kani:** cert/archive/elf-Parser panik-/OOB-frei (Tier 1) — die Eingaben der hier modellierten Checks.
- **`trust_audit`** (Laufzeit) + **`certfuzz`** (Fuzzer): prüfen Key-DB-Konsistenz + das Gate auf dem
  echten Code; **Verus** beweist die Gate-/Automaten-Logik. Die Ebenen ergänzen sich.

## 14. Verifikationsfortschritt / Nächste Ausbaustufen

- ✅ Zertifikats-Gate + Lade-Zustandsautomat (Soundness/Revocation/Gating/Atomarität/Determinismus).
- ⏳ Capability-Endowment beim Laden (auf Phase-1-Domänen-Policy aufbauend).
- ⏳ Modell↔Parser-Bindung (Richtung realer Code).
