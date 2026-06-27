# Verifikation — ipc (Phase 4)

> **Status:** geplant. Wird erst nach Abschluss der vorhergehenden Phase begonnen (Methodik:
> Analyse -> Variantenvergleich -> ADR -> Spezifikation -> Implementierung -> Beweise -> Doku ->
> Integration -> Build -> Validierung -> Commit).

Bezug: [Verification/README.md](../README.md), `docs/verification.md`,
`ARMTest/formale-verifikation-aufwand.md`.

## Geplanter Umfang

CALL/REPLY-Protokoll, Endpoint-Invarianten, Reply-Caps, Capability-Transfer, Nachrichtenzustaende, keine verlorenen/doppelten Nachrichten, deadlock-freie Zustandsuebergaenge (soweit im Modell moeglich).

## Grundlage

Baut auf dem verifizierten Capability-System (Reply-Caps, Cap-Transfer) auf.

## Doku-Struktur (wird bei Phasenstart gefuellt)

Motivation/Ziel · Sicherheitsmodell · zu beweisende Invarianten · ADR-Bezug · formale Spezifikation ·
Verus-Architektur · Beweisstrategie · Lemmas · bewiesene Eigenschaften · offene Eigenschaften ·
bekannte Grenzen · Trusted Computing Base · Verbindung zu Runtime-Audits/Kani · Verifikationsfortschritt ·
naechste Ausbaustufen.
