# SEL4Lake — adversariale externe Testdienste (ext-27)

Dieser **eigene** Cargo-Workspace (getrennt vom Kernel **und** von `programs/`) baut die
**adversarialen Testdienste**: extern gebaute, statisch gelinkte `ET_EXEC`-ELF64-EL0-Programme,
die der generische Binary-Loader (ADR 0011) zur Laufzeit **wie Drittsoftware** lädt — **nicht**
Teil des Kernel-Images. Sie greifen den Kernel **und sich gegenseitig** ausschließlich über die
Syscall-ABI an und beweisen so empirisch die Isolations-Invarianten (Architektur: [ADR 0012](../docs/adr/0012-adversarial-test-services.md)).

## Bauen

```sh
cd tests && cargo build --release
```

Erzeugt die ELFs unter `tests/build/target/aarch64-sel4lake-user/release/<name>.elf` (eigene
Target-Spec/Linker wie `programs/`). `test-qemu.sh` legt sie per `tools/mkarchive.py` ins
Boot-Archiv (Domäne pro Eintrag). Einzige Abhängigkeit: das SDK `../programs/libsel4lake`.

## Beobachtungsprotokoll — „der Dienst ist sein eigener Richter"

Jeder Dienst hält in **Slot 0** eine Report-Notification (gemintet WRITE, Badge `SUCCESS_<id>`). Er
fährt seine Angriffsbatterie, prüft **jeden** Rückgabecode gegen den **erwarteten** Fehler und
signalisiert Slot 0 **genau dann**, wenn **alle** Angriffe korrekt abgewiesen wurden. Ließe der
Kernel einen Angriff durch, bliebe das Signal aus → der Kernel-Test (Idle-Manager) läuft in den
Timeout → **FAIL**. Fatale Speicher-Batterien signalisieren zuerst ein **PRE**-Badge, dann erfolgt
die fatale Dereferenzierung → Fault; der Kernel beobachtet PRE + `el0_fault_count`++ + Survival.

## Struktur

| Verzeichnis | Dienst | Domäne | Schwerpunkt |
|---|---|---|---|
| `services/userland/aggressor/` | `aggressor-u` | UserLand | Cap-Confusion + Autoritäts-Eskalation (alle abgewiesen). |

(Weitere Dienste je Domäne folgen in den ext-27-Phasen T1–T4: `intruder-u`, `aggressor-h`,
`intruder-h`, `aggressor-t`, `intruder-t`.)

## Abgrenzung

Aus einem geladenen EL0-Prozess sind **nur** ABI-Operationen ausdrückbar (kein
`copy`/`mint`/`move`/`delete`/`revoke`, keine CDT-Manipulation — dafür gibt es keinen Syscall).
Diese Cap-/CDT-Angriffe bleiben daher im **In-Kernel**-Selbsttest (`captest`/`fuzz`); ext-27 deckt
die ABI- + Hardware-Isolations-Angriffsfläche ab.
