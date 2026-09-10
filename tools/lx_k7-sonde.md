# K7-FORK-Sonde: Entwurf (Strang 9, Patch-Text für bringup.rs-Besitz)

Stand 2026-09-10: `SWEEP_K7_FORK_PFAD=10` zählt, aber der Sweep übt den FORK-Pfad
nicht (`bringup.rs`-Prosa: „eigene Sonde offen"). Dieser Entwurf macht daraus
Pfad 8. NICHT angewendet (bringup.rs geteilt/timing-sensibel) — zur Review/Anwendung
durch B. Voller Hunk-Text im Strang-9-Bericht (Session-Protokoll 2026-09-10).

## Befunde (tragen den Entwurf)

1. Sweep-Kontext kann nicht forken: `dispatch_fork` braucht Quell-ASID (`asid==0` →
   `ERR_NOPD`). Quell-PD: `spawn_isolated_native`-PD (registrierte Frames + Eintritt).
2. Max. 9 von 10 provozierbar: `MANGEL_L2_TABELLE` (`system.rs:4266`) strukturell tot
   (frische ASID → immer `Some`), wie Klasse 6/`4207`. Kstack/VSpace-Callee-Meldungen
   laufen über bestehende `benannt_alloc`-Stellen mit.
3. Kind startet am PARK-Eintritt (`FORK_PARK_EINTRITT`), Zweit-Thread am FORK-Stub —
   sonst Fork-Bombe (jedes Kind forkt erneut).

## Bausteine (alle `kernel/src/arch/x86_64/bringup.rs`, `#[cfg(feature="selftest")]`)

- `SWEEP_PFADE` 8→9, `PFAD_FORK=8`, `SWEEP_KMAX` += 16 (10 Stellen + Callee + Reserve).
- Klasse-7-Prosa: „9 von 10 provoziert, L2 tot wie Klasse 6".
- `sweep_versuch`: Sonden-Aufbau (native PD + Zweit-Thread via `spawn_parked`/`admit_in_pd`)
  VOR `mangel_vergiften()`; `nicht_fahrbar`-Rückweg bei Fehlschlag.
- Neuer `PFAD_FORK`-Arm: exakt EIN Fork je GO (Handshake-Statics `FORK_GO/FERTIG/CODE/KIND`,
  Muster Q_PROBE), danach Aufräumen (`destroy_loaded` + `free_pd_slot`, gelungen wie
  abgewiesen dank `kind_aufraeumen`).
- Urteil: 9 Codes fordern (ohne L2), `stumm/keiner/menge_falsch==0`; Report um
  `FORK={} ({}, Toepfe)` erweitern; `provoziert`-Rechnung nur bei `je_pfad[FORK]>0`.
- Bilanz: Aufräumen je Durchgang + Sonden-Abbau am Ende (Konjunkte!).

## Risiken

Fork-Bombe bei Eintritts-Verwechslung (Mitigation: Stub≠Eintritt per Konstruktion +
max. 1 lebendes Kind/Durchgang); `forkexec_syscall`-Erreichbarkeit vorausgesetzt;
kern-lokale Sperre (Präemption → ehrlich „gedeckelt"); Timing-Baselines verschieben
sich (Sonde läuft nach allen Urteilen, wie übriger Sweep).
