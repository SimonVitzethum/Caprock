# ADR 0012 — Adversariale externe Testdienste (ext-27)

Status: **angenommen** · Datum: 2026-06-26 · Kontext: ext-26 (Binary-Loader, L0–L6 fertig)

## Kontext

Mit dem Binary-Loader (ADR 0011) lädt der Kernel jetzt **extern gebaute** EL0-Programme aus einem
Boot-Archiv in isolierte PDs **aller drei Domänen** (TrustedSAS/HardwareLand/UserLand) — alle
hardware-isoliert (EL0). Damit existiert erstmals ein Vehikel, um die Sicherheitsarchitektur
**von außen** anzugreifen: nicht durch in den Kernel kompilierte Selbsttests, sondern durch
**eigenständige, extern gebaute Dienste**, die wie echte Drittsoftware geladen werden und
ausschließlich über die Syscall-ABI mit dem Kernel interagieren.

ext-27 baut eine **adversariale Testumgebung** aus **sechs** solcher Dienste (zwei je Domäne), die
**aggressiv den Kernel und sich gegenseitig** angreifen — über **alle Domänen-Kombinationen** —
und damit die Isolations-Invarianten **empirisch** beweisen. Jeder Angriff wird dokumentiert:
angegriffene Invariante, erwartetes Verhalten, beobachtetes Verhalten, Kernel-Korrektheits-Urteil.

## Maßgebliche Architekturfakten (Constraint → Angriffstaxonomie)

Ein geladener EL0-Dienst kann **ausschließlich** über `svc #0` agieren und dabei **nur** Caps in
den **eigenen** Cspace-Slots referenzieren. Er kann **keine** Caps fälschen, **keinen** fremden
Cspace erreichen, läuft in einer **isolierten VSpace**. Daraus folgt die **vollständige**
Angriffstaxonomie eines externen Dienstes:

- **A — Cap-Confusion.** Syscall auf (a) leeren Slot, (b) Cap **falschen Typs**, (c) Cap mit
  **falschen Rechten** → der Kernel MUSS `ERR_BADCAP` / `ERR_RIGHTS` / `ERR_BADSYS` zurückgeben.
- **B — Autoritäts-Eskalation.** `PDCTL`/`LOAD`/`KILL` **ohne** die gating-Cap → `ERR_BADCAP`;
  `PDCTL` **mit** Cap aber falscher Domäne (Aufrufer nicht TrustedSAS / Ziel nicht UserLand) →
  `ERR_RIGHTS`. Beweist: **Domänen-Trust-Stufe ≠ Privileg** — selbst ein TrustedSAS-Dienst hat nur
  die Autorität seiner **tatsächlich gehaltenen** Caps.
- **C — Speicher-Isolation (hardware-erzwungen).** Dereferenzierung einer **fremden/Kernel-VA** →
  Translation-Fault → der angreifende Thread wird terminiert, **Kernel und Opfer überleben**
  (Canary unverändert). Domänen-**unabhängig** (auch ein TrustedSAS-EL0-Dienst faultet).
- **D — DoS/Flood.** Enge Syscall-Schleifen / Signal-Fluten → Kernel bleibt **lebendig**, andere
  Kerne/Dienste laufen weiter, Audits bleiben 0.
- **E — Cross-Service.** Über die Matrix {T,H,U}×{T,H,U}: **kein** Dienst kann die privaten
  Ressourcen (Speicher/Endpoints/PD-Steuerung) eines anderen lesen/schreiben/steuern.

**Nicht aus einem geladenen Prozess ausdrückbar** (es gibt keinen Syscall dafür): kernel-interne
Cap-Operationen (`copy`/`mint`/`move`/`delete`/`revoke`, CDT-Manipulation). Diese Angriffe
**bleiben** im In-Kernel-Selbsttest (`captest`, `fuzz`, `cap_audit_cdt`) — ext-27 deckt die
**ABI-ausdrückbare** plus die **hardware-isolations**-Angriffsfläche ab. Diese Aufteilung ist
bewusst und in der Architektur dokumentiert.

## Entscheidung

### 1. Externe Dienste in eigenem `tests/`-Workspace, geladen wie Drittsoftware
Die sechs Dienste liegen in `tests/services/{userland,hardware,trusted}/` — ein **eigener**
Cargo-Workspace, getrennt von `programs/` (legitime Programme) und vom Kernel. Sie hängen **nur**
vom SDK `programs/libcaprock` ab (kein Kernel-Workspace), bauen mit derselben Target-Spec/Linker
und werden per `tools/mkarchive.py` ins Boot-Archiv gelegt — **nicht** Teil des Kernel-Images. Die
Domäne jedes Dienstes legt der **Archiv-Eintrag** fest (nicht das Binary), sodass dieselbe
Angriffslogik domänen-übergreifend instanziiert werden kann.

### 2. Beobachtungsprotokoll — „der Dienst ist sein eigener Richter"
Jeder Dienst bekommt eine **Report-Notification-Cap** (Slot 0, gemintet WRITE, Badge `SUCCESS_<id>`)
endowt. Der Dienst fährt seine Angriffsbatterie, prüft **jeden** Rückgabecode gegen den
**erwarteten** Fehler und signalisiert Slot 0 **genau dann**, wenn **alle** Angriffe korrekt
abgewiesen wurden. Wäre der Kernel fehlerhaft (ließe einen Angriff durch), signalisiert der Dienst
**nicht** → der Kernel-Test läuft in den Timeout → **FAIL**. Der externe Dienst ist damit das
**Orakel** der Kernel-Korrektheit für die nicht-fatalen Batterien (A/B/D).

Für die **fatale** Speicher-Isolations-Batterie (C) signalisiert der Dienst zuerst ein **PRE**-Badge
(nicht-fataler Teil bestanden), dann führt er die fatale Dereferenzierung aus → Fault → terminiert.
Der Kernel beobachtet: PRE-Badge erhalten **+** `el0_fault_count` erhöht **+** Kernel überlebt **+**
(Cross-Service) Opfer-Canary unverändert.

### 3. Kernel-seitige Verifikation nach jeder Attacke
Der Idle-Manager (bestehende Selbsttest-State-Machine) lädt jeden Dienst, endowt die nötigen Caps,
pollt das Report-Badge und verifiziert zusätzlich: `domain_audit == 0`, `vspace_audit == 0`,
`cap_audit_cdt == 0`, `loader_audit == 0`, `ipc_audit == 0`, und beim Teardown die
**Ressourcen-Baseline** (freies MEM/VSpaces/Kstack-Slots wiederhergestellt → kein Leck).

### 4. Die sechs Dienste (2 je Domäne, je domänen-spezifisch betont)
| Dienst | Domäne | Schwerpunkt |
|---|---|---|
| `aggressor-u` | UserLand | Cap-Confusion (A) + Eskalation (B): ein UserLand-Prozess hat **null** Management-Autorität → PDCTL/LOAD/KILL → `ERR_BADCAP`. |
| `intruder-u`  | UserLand | Speicher-Isolation (C): eigene VSpace beweisen, dann fatale Kernel-VA-Deref → Fault. |
| `aggressor-h` | HardwareLand | Kanal-/Autoritäts-Missbrauch: ein HW-Backend versucht Management + fremdes Signalisieren → abgewiesen. |
| `intruder-h`  | HardwareLand | Speicher-Isolation aus einem HW-Backend (Fault domänen-unabhängig). |
| `aggressor-t` | TrustedSAS | **Trust ≠ Privileg**: höchste Cap-Autorität, doch **ohne** PdControl/Loader-Cap → PDCTL/LOAD → `ERR_BADCAP`; plus DoS-Storm. |
| `intruder-t`  | TrustedSAS | Speicher-Isolation: auch ein **trusted** EL0-Dienst faultet auf Kernel-VA. |

Die **„aggressor"**-Dienste betreiben die nicht-fatalen Syscall-Batterien (selbst-richtend); die
**„intruder"**-Dienste betreiben die fatale Speicher-Isolation und — co-geladen mit einem Opfer —
die **Cross-Service**-Matrix (E): der Intruder versucht, das Canary-Frame des Opfers zu erreichen;
da er **keine** Cap darauf hält und es **nicht** in seiner VSpace liegt, faultet jeder Zugriff →
Canary intakt = Isolation bewiesen, für jedes (Angreifer-Domäne, Opfer-Domäne)-Paar.

## Konsequenzen

- **Empirischer Isolationsbeweis von außen.** Die Architektur wird nicht nur durch in-Kernel-Asserts
  geprüft, sondern durch echte, extern geladene Angreifer — der härteste verfügbare Test.
- **Der Kernel bleibt unverändert** (Vorgabe „Kernel als fertiges Produkt"): ext-27 fügt nur
  Testdienste + kernel-seitige Test-Harness-Schritte (Idle-Manager) hinzu, **keine** neue
  Kernel-Funktionalität. Etwaige dabei gefundene Defekte sind Bugfixes, keine Features.
- **Klare Abgrenzung dokumentiert:** Cap-/CDT-Angriffe bleiben in-Kernel (nicht ABI-ausdrückbar);
  ext-27 deckt die ABI- + Hardware-Isolations-Fläche ab.
- **Wiederverwendbar:** neue Dienste = neues Crate in `tests/services/` + Archiv-Eintrag + ein
  Idle-Manager-Schritt. Das SDK `libcaprock` trägt die gemeinsame Angriffslogik.

## Alternativen (verworfen)

- **In-Kernel-Angreifer-Threads** (wie die bestehenden Fuzzer): bereits vorhanden und beibehalten,
  aber **kein** Beweis für *extern geladene* Drittsoftware — genau das ist der Mehrwert von ext-27.
- **Ein einziges parametrisiertes Angreifer-Binary** (per Boot-Arg gesteuert): DRY, aber die Vorgabe
  verlangt **sechs** eigenständige Dienste; die domänen-spezifische Betonung macht sie zudem als
  Dokumentation wertvoller. Gemeinsame Logik liegt stattdessen im SDK.
