# ext-27 — Adversariale externe Testdienste (Plan)

Architektur/Protokoll: [ADR 0012](../adr/0012-adversarial-test-services.md). Sechs extern gebaute
EL0-Dienste (2 je Domäne), geladen wie Drittsoftware über den Binary-Loader (ext-26), greifen
**Kernel und sich gegenseitig** über alle Domänen-Kombinationen an und beweisen die Isolation.

## Methodik (pro Phase)
Bauen (Kernel + `tests/`-Workspace) → `./test-qemu.sh` (jeder neue `<name> : ALL PASS` + finaler
`== ALL PASS ==`) → `tools/hang-stress.sh` (Deadlock-Regression) → Commit+Push. Jede neue Prüfung
**vor** den Fuzzern in der Idle-Manager-Kette gaten. Suite bleibt grün.

## Beobachtungsprotokoll (Kurzform)
Jeder Dienst hält in **Slot 0** eine Report-Notification (Badge `SUCCESS_<id>`). Er prüft jeden
Angriffs-Rückgabecode gegen den **erwarteten** Fehler und signalisiert Slot 0 **nur**, wenn **alle**
korrekt abgewiesen wurden (Self-Judging). Fatale Speicher-Batterie: erst **PRE**-Badge, dann fatale
Deref → Fault. Kernel-Harness pollt das Badge + prüft `domain/vspace/cap_cdt/loader/ipc_audit == 0`
+ Survival + (Teardown) Ressourcen-Baseline.

## Phasen

### T0 — Infrastruktur + SDK + erster UserLand-Aggressor  ✅ Ziel
- `tests/` = eigener Cargo-Workspace (Target-Spec/Linker/`.cargo` wie `programs/`), dep nur auf
  `programs/libcaprock`. SDK-Erweiterung in `libcaprock`: `result`-Codes, `pdctl`-Sub-Ops,
  Wrapper `map/unmap/pdctl/load/kill/call/recv` + die rohe `invoke`-Schnittstelle.
- Dienst `tests/services/userland/aggressor` (`aggressor-u`): Cap-Confusion-Batterie (leere Slots,
  falscher Typ über die Report-Cap, falsche Rechte) + Eskalation (PDCTL/LOAD/KILL ohne Autorität)
  + unbekannte Syscall-Nr → alle `BADCAP`/`RIGHTS`/`BADSYS`. Signalisiert `SUCCESS` iff alles abgewiesen.
- Kernel-Test `aggru`: lädt `aggressor-u` (UserLand), endowt Report-Cap (Slot 0 WRITE) + eine
  read-only Notification (Slot 1, für die Rechte-Probe), pollt `SUCCESS`, prüft Audits == 0, baut ab.
- `test-qemu.sh`: `tests/`-Build + Archiv-Eintrag `20:aggressor-u:2:1:<elf>` + Check `aggru`.

### T1 — UserLand-Intruder (Speicher-Isolation, fatal)
- `tests/services/userland/intruder` (`intruder-u`): mappt sein endowtes Canary-Frame (Slot 1),
  beweist die eigene VSpace (lesen/schreiben), signalisiert **PRE**; dann fatale Kernel-VA-Deref →
  Fault. Kernel-Test `intru`: endowt Canary-Frame, pollt PRE, prüft `el0_fault_count`++ + Kernel
  lebt + Canary (via `mem::peek_u64`) unverändert + Audits == 0.

### T2 — HardwareLand-Dienste
- `aggressor-h` (Backend): Management-Eskalation (PDCTL/LOAD/KILL → BADCAP) + Kanal-Missbrauch
  (Signal/CALL auf fremde/leere Slots) + bounded Flood. `intruder-h`: Speicher-Isolation aus einem
  HW-Backend. Kernel-Tests `aggrh`/`intrh` via `create_hardware_backend` + `load_program_into_pd`.

### T3 — TrustedSAS-Dienste
- `aggressor-t`: **Trust ≠ Privileg** — ohne PdControl/Loader-Cap PDCTL/LOAD → BADCAP; DoS-Storm
  (LOAD auf Invalid-Index + Syscall-Flood) → Kernel lebt, Audits 0. `intruder-t`: trusted EL0
  faultet auf Kernel-VA. Kernel-Tests `aggrt`/`intrt` (geladen EL0-isolierte TrustedSAS-PDs).

### T4 — Cross-Service-Matrix (gegenseitige Angriffe, nebenläufig)
- Mehrere Dienste **gleichzeitig** geladen; jeder Intruder versucht, das Canary eines co-geladenen
  Opfers **anderer Domäne** zu erreichen (kein Cap, fremde VSpace → Fault). Kernel-Test `cross`:
  für die {T,H,U}×{T,H,U}-Paare — alle Canaries intakt, alle Angreifer-Threads terminiert, Kernel
  lebt, alle Audits == 0. Abschluss-Teardown → Ressourcen-Baseline (kein Leck).

### T5 — Abschluss/Doku
- `dma_audit`-artige Gesamtprüfung, finaler Full-Stress (`hang-stress.sh`). ADR 0012 + dieser
  Bericht → `ext-27-adversarial-tests.md` (Angriffsmatrix: je Angriff Invariante/erwartet/beobachtet/
  Urteil). `tests/README.md`. Memory `caprock-project.md`.

## Kritische Dateien
- `tests/` (neu): Workspace + `services/{userland,hardware,trusted}/*`.
- `programs/libcaprock/src/lib.rs` — SDK-Erweiterung (result/pdctl/Wrapper).
- `kernel/src/threads.rs` — neue Test-Funktionen + Idle-Manager-Schritte + `report()`/`all_done()`/DBG.
- `test-qemu.sh` — `tests/`-Build + Archiv-Einträge + neue Checks.

## Verifikation / Constraints
- Kernel-Funktionalität bleibt **unverändert** (nur Testdienste + Harness-Schritte); Defekte = Bugfix.
- Host-Last-Flakiness bekannt (TCG); Deadlock-Freiheit per `hang-stress.sh`. Cap-/CDT-Angriffe bleiben
  in-Kernel (nicht ABI-ausdrückbar). Check-Zahl 55 → ~61.
