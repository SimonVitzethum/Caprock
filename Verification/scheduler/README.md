# Verifikation — scheduler (Phase 5)

> **Status:** geplant. Wird erst nach Abschluss der vorhergehenden Phase begonnen (Methodik:
> Analyse -> Variantenvergleich -> ADR -> Spezifikation -> Implementierung -> Beweise -> Doku ->
> Integration -> Build -> Validierung -> Commit).

Bezug: [Verification/README.md](../README.md), `docs/verification.md`,
`ARMTest/formale-verifikation-aufwand.md`.

## Geplanter Umfang

MCS-Budgets, Runqueue-Konsistenz, Prioritaeten (sequenzielles Modell; SMP/Locking bleiben Hardware-Vertrauensgrenze).

## Grundlage

Bewusst zuletzt, nach Abschluss der vorherigen Komponenten.

## Doku-Struktur (wird bei Phasenstart gefuellt)

Motivation/Ziel · Sicherheitsmodell · zu beweisende Invarianten · ADR-Bezug · formale Spezifikation ·
Verus-Architektur · Beweisstrategie · Lemmas · bewiesene Eigenschaften · offene Eigenschaften ·
bekannte Grenzen · Trusted Computing Base · Verbindung zu Runtime-Audits/Kani · Verifikationsfortschritt ·
naechste Ausbaustufen.
