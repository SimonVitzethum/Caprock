# ext-27 — Adversariale externe Testdienste

Status: **fertig** (T0–T5). Sechs extern gebaute EL0-Dienste (2 je Domäne), vom Binary-Loader
(ext-26) wie Drittsoftware geladen, greifen den Kernel **und sich gegenseitig** über alle
Domänen-Kombinationen an und beweisen empirisch die Isolations-Invarianten. Architektur/Protokoll:
[ADR 0012](../adr/0012-adversarial-test-services.md). Suite grün (**62 Checks**), `hang-stress` 10/10.

## Ziel

Den Übergang zur **fertigen** Sicherheitsarchitektur durch die **härteste** verfügbare Prüfung
absichern: nicht durch in den Kernel kompilierte Selbsttests, sondern durch **eigenständige, extern
geladene Drittsoftware**, die ausschließlich über die Syscall-ABI agiert und den Kernel + die
Mitdienste aggressiv attackiert. Der Kernel bleibt dabei **unverändert** (nur Testdienste +
Harness-Schritte; Vorgabe „Kernel als fertiges Produkt").

## Die sechs Dienste (`tests/services/`)

| Dienst | Domäne | Rolle | Schwerpunkt |
|---|---|---|---|
| `aggressor-u` | UserLand | Aggressor | Cap-Confusion + Eskalation (null Management-Autorität). |
| `intruder-u`  | UserLand | Intruder  | Speicher-Isolation (Kernel-RAM-Deref → Fault). |
| `aggressor-h` | HardwareLand | Aggressor | Backend ohne Management-Autorität, nichts außerhalb des Kanals. |
| `intruder-h`  | HardwareLand | Intruder  | Speicher-Isolation domänen-unabhängig. |
| `aggressor-t` | TrustedSAS | Aggressor | **Trust ≠ Privileg** (höchste Autorität, doch nur gehaltene Caps zählen). |
| `intruder-t`  | TrustedSAS | Intruder  | Speicher-Isolation **auch** für die vertraute Domäne. |

## Beobachtungsprotokoll — „der Dienst ist sein eigener Richter"

Jeder Dienst hält in **Slot 0** eine Report-Notification (Badge kernel-gemintet; `SIGNAL` nutzt das
**Cap-Badge**, nicht das Argument → Dienste sind vollständig kernel-gesteuert wiederverwendbar). Der
Dienst prüft **jeden** Angriffs-Rückgabecode gegen den **erwarteten** Fehler und signalisiert Slot 0
**genau dann**, wenn **alle** Angriffe korrekt abgewiesen wurden. Ließe der Kernel einen Angriff
durch, bliebe das Signal aus → der Kernel-Harness (Idle-Manager) läuft in den Timeout → **FAIL**.
Fatale Speicher-Batterien melden ein **PRE**-Badge, dann erfolgt der fatale Zugriff → Fault; der
Harness beobachtet PRE **+** `el0_fault_count++` **+** Survival. Nach **jeder** Attacke verifiziert
der Harness zusätzlich `domain_audit == vspace_audit == cap_audit_cdt == loader_audit == ipc_audit == 0`.

## Angriffsmatrix (je Angriff: Invariante · erwartet · beobachtet · Urteil)

| # | Angriff (aus EL0, via Syscall-ABI) | Angegriffene Invariante | Erwartet | Beobachtet | Test |
|---|---|---|---|---|---|
| 1 | Syscall auf **leeren** Cap-Slot (CALL/SIGNAL/MAP/PDCTL/LOAD/KILL/…) | Cap-Autorität: keine Cap → keine Operation | `ERR_BADCAP` | `ERR_BADCAP` ✓ | aggru/aggrh/aggrt |
| 2 | Syscall auf Cap **falschen Typs** (z. B. CALL/MAP/PDCTL/LOAD/KILL auf eine Notification) | Typ-Sicherheit der Cap-Invokation | `ERR_BADCAP` | `ERR_BADCAP` ✓ | aggru/aggrh/aggrt |
| 3 | `SIGNAL` auf READ-only-Cap · `WAIT` auf WRITE-only-Cap | Rechte-Durchsetzung (`Rights`) | `ERR_RIGHTS` | `ERR_RIGHTS` ✓ | aggru/aggrh/aggrt |
| 4 | Unbekannte Syscall-Nummer (über gültige Cap) | Dispatch-Vollständigkeit | `ERR_BADSYS` | `ERR_BADSYS` ✓ | aggru/aggrh/aggrt |
| 5 | **PDCTL/LOAD/KILL ohne** gating-Cap (Eskalation) | Autorität nur über gehaltene Cap, nicht über Domäne | `ERR_BADCAP` | `ERR_BADCAP` ✓ | aggru/aggrh/aggrt |
| 6 | HardwareLand-Backend erreicht etwas **außerhalb** seines Kanals | Paarweise Kanal-Bindung (`domain_audit` 5) | abgewiesen | nur eigener Kanal nutzbar ✓ | aggrh |
| 7 | TrustedSAS-Dienst nutzt **Domänen-Trust** als Privileg | Trust-Stufe = Cap-Autorität, **nicht** Privileg | `ERR_BADCAP` | `ERR_BADCAP` ✓ | aggrt |
| 8 | EL0-Lesezugriff auf **Kernel-RAM** (`0x4000_0000`) | Hardware-Adressraumtrennung (Invariante #1) | Translation-Fault, Thread terminiert, Kernel lebt | `el0-trap FAR=0x40000000`, Kernel lebt ✓ | intru/intrh/intrt |
| 9 | dito aus **HardwareLand**- bzw. **TrustedSAS**-Domäne | Isolation ist **domänen-unabhängig** | Fault unabhängig von der Domäne | Fault in allen drei Domänen ✓ | intrh/intrt |
| 10 | **Drei** Angreifer **dreier** Domänen **nebenläufig** | Cross-Service-Nicht-Interferenz + Speicher | beide Aggressoren melden unabhängig SUCCESS, Intruder faultet, Canary intakt | bestätigt, Audits 0 ✓ | cross |

In allen Fällen blieb der Kernel lebendig, alle fünf Sicherheits-Oracles meldeten 0, und kein
geladener Dienst konnte Kernel- oder Fremdspeicher lesen/schreiben, fremde Endpoints/Notifications
erreichen oder eine andere PD steuern.

## Architektonische Abgrenzung (bewusst, dokumentiert)

Aus einem geladenen EL0-Prozess sind **nur** ABI-Operationen ausdrückbar. Kernel-interne Cap-
Operationen (`copy`/`mint`/`move`/`delete`/`revoke`, CDT-Manipulation) haben **keinen** Syscall →
sie sind aus keinem Dienst ausdrückbar und bleiben im **In-Kernel**-Selbsttest (`captest`, `fuzz`,
`cap_audit_cdt`, `ipcfuzz`). ext-27 deckt die **ABI-** plus die **Hardware-Isolations**-Angriffs­fläche
ab; zusammen mit den bestehenden In-Kernel-Fuzzern ergibt das die vollständige Abdeckung.

## Phasen

- **T0** — Infrastruktur (`tests/`-Workspace + SDK-Erweiterung) + `aggressor-u` (`aggru`).
- **T1** — `intruder-u` (`intru`), Speicher-Isolation (fataler Kernel-RAM-Zugriff).
- **T2** — HardwareLand: `aggressor-h`/`intruder-h` (`aggrh`/`intrh`), geladen in vor-erstellte Backends.
- **T3** — TrustedSAS: `aggressor-t`/`intruder-t` (`aggrt`/`intrt`), geladen EL0-isoliert.
- **T4** — Cross-Service-Matrix (`cross`): 3 Domänen nebenläufig + kernel-geschütztes Canary.
- **T5** — Abschluss: Angriffsmatrix-Doku, README, Memory, Full-Stress.

## Verifikation

`./test-qemu.sh` → **62/62 ALL PASS** (61 Bestand + `cross`; bzw. 55 vor ext-27 + `aggru`/`intru`/
`aggrh`/`intrh`/`aggrt`/`intrt`/`cross`). Stabilität per `tools/hang-stress.sh` (jede Phase 10/10,
Abschluss 20/20). Die sichtbaren `el0-trap FAR=0x40000000`-Zeilen belegen die Hardware-Isolation
direkt. Build: `cd tests && cargo build` (eigener Workspace); Archiv über `tools/mkarchive.py`.

## Offen (mögliche Erweiterungen)

Shared-Channel-Missbrauch (ein bewusst geteilter Cross-Domain-Endpoint, Flood vs. Liveness des
Opfers), DoS-Quoten/Fairness unter Dauer-Syscall-Flut, geladene Dienste, die per `SYS_LOAD` weitere
Kinder laden (verschachtelte Isolation). Cap-/CDT-Angriffe bleiben bewusst im In-Kernel-Selbsttest.
