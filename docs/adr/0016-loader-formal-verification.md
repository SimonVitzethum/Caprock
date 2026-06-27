# ADR 0016 — Formale funktionale Verifikation des Loaders (Verus, Phase 2)

Status: **angenommen** · Datum: 2026-06-27 · Phase 2 der funktionalen Verifikation.
Bezug: ADR 0011 (Binary-Loader), ADR 0014 (TrustedSAS-Zertifikate), ADR 0015 (Verifikationsansatz),
`Verification/loader/`, `docs/verification.md`.

## Kontext

Phase 1 (Capability-System) hat die Methodik etabliert: abstraktes, faithful Modell + Verus-Beweis,
realer Code unverändert, mehrschichtig (Audit+Fuzzer+Kani+Verus). Phase 2 verifiziert den **Loader** —
konkret die **Sicherheitslogik** `verify_image`/`verify_trusted_cert` (ADR 0014) + den Lade-Zustands-
automaten. Die **Parser** (cert/archive/elf) sind bereits mit **Kani** panik-/OOB-frei bewiesen
(`docs/verification.md`) — sie bilden die Grundlage; Phase 2 baut die **funktionale** Schicht darüber.

## Analyse

`verify_image(prog)`: UserLand/HardwareLand → `true` (hardware-isoliert); TrustedSAS →
`verify_trusted_cert` = Konjunktion von Checks (Algorithmus Ed25519 + Siglänge 64, `key_id` in der
read-only Key-DB nicht-revoziert + selbstkonsistent, Signatur über die Nachricht, `binary_hash`/
`manifest_hash`-Bindung, `program_id`/`version`-Identität, Anti-Downgrade, `unsafe_status==ALL_PASS`).
`load_image` ruft `verify_image` **vor** jeder Ressourcenvergabe → atomar (kein partieller Ladezustand).

## Variantenvergleich

| Variante | Beschreibung | Bewertung |
|---|---|---|
| V1 Krypto mitmodellieren | Ed25519/SHA-256 in Verus | unnötig + riesig; Krypto = etablierte Bibliothek (TCB). Verworfen. |
| **V2 Krypto abstrahieren (gewählt)** | `sig_valid`/`*_hash_ok` als durch die Signatur geschützte **Fakten**; die Gate-/Zustandsautomaten-**Logik** beweisen | erfasst genau die Loader-Eigenheit (Entscheidungslogik + Atomarität); die Krypto-Treue tragen die Bibliotheken + der Round-Trip-Test (ext-28) |
| V3 reale-Code-Annotation | `loader.rs` direkt annotieren | verändert die Architektur (Verbot); aufgeschoben |

## Entscheidung

**V2** — die Krypto-/Hash-Fakten abstrahieren, die **Gate- + Lade-Zustandsautomaten-Logik** formal
verifizieren. Zu beweisende Eigenschaften: **Soundness** (kein unverifiziertes TrustedSAS lädt),
**Programmintegritäts-/Identitäts-Bindung**, **Revocation** (+ Vollständigkeit), **Domänen-Gating**
(UserLand/HardwareLand brauchen kein Cert), **Atomarität** (genau ein Ladeausgang, kein inkonsistenter
Zustand), **Determinismus** (read-only Key-DB → reproduzierbar). Abstraktes Modell; realer Code
unverändert; mehrschichtig (die Parser-Speichersicherheit liefert Kani).

## Konsequenzen

- **Positiv:** die TrustedSAS-Lade-Sicherheit (ADR 0014) ist formal bewiesen, nicht nur per Selbsttest/
  Fuzzer geprüft. Klare Schichtung: Kani (Parser-Speichersicherheit) + Verus (Gate-/Automaten-Logik).
- **Grenzen/TCB:** die Krypto-Primitive (ed25519-dalek/sha2) + die Modell↔Code-Treue sind Vertrauens-
  basis (abgesichert durch den Host-Round-Trip + die Laufzeit-Selbsttests/`trust_audit`). Das
  Capability-**Endowment** der geladenen PD (Domänen-Policy beim Laden) ist eine spätere Ausbaustufe,
  baut auf Phase 1 (Domänen-Policy bereits bewiesen) auf.
