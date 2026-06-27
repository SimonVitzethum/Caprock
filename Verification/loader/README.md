# Verifikation — loader (Phase 2)

> **Status:** geplant. Wird erst nach Abschluss der vorhergehenden Phase begonnen (Methodik:
> Analyse -> Variantenvergleich -> ADR -> Spezifikation -> Implementierung -> Beweise -> Doku ->
> Integration -> Build -> Validierung -> Commit).

Bezug: [Verification/README.md](../README.md), `docs/verification.md`,
`ARMTest/formale-verifikation-aufwand.md`.

## Geplanter Umfang

Zertifikatspruefung, Hash-Bindung, Programmintegritaet, Domaenenzuordnung, Capability-Endowment, Lade-Zustandsautomat, Fehlerpfade, keine inkonsistenten Ladezustaende.

## Grundlage

Die mit Kani bewiesenen Parser (cert/archive/elf — panik-/OOB-frei) bilden die Grundlage.

## Doku-Struktur (wird bei Phasenstart gefuellt)

Motivation/Ziel · Sicherheitsmodell · zu beweisende Invarianten · ADR-Bezug · formale Spezifikation ·
Verus-Architektur · Beweisstrategie · Lemmas · bewiesene Eigenschaften · offene Eigenschaften ·
bekannte Grenzen · Trusted Computing Base · Verbindung zu Runtime-Audits/Kani · Verifikationsfortschritt ·
naechste Ausbaustufen.
