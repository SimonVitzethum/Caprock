# ADR 0011 — Generischer Binary-Loader (extern gebaute Prozesse, In-Kernel, cap-gegatet)

Status: **Vorgeschlagen** (ext-26; Architektur + Format vom Nutzer entschieden, Implementierung folgt nach Plan-Freigabe)

## Kontext

Bis ext-25 ist *jeder* Prozess-/PD-Code **in das Kernel-Image kompiliert** (`.user_text`-Sektion)
und wird über **In-Kernel-Funktionszeiger** gespawnt (`spawn` = EL1/TrustedSAS, `spawn_user`/
`spawn_isolated*` = EL0/isoliert). Es gibt **keinen** Mechanismus, extern gebauten Code zu laden:
kein Syscall, kein QEMU-`-device loader`-Einlesen, keine Modul-Tabelle, kein Loader.

Ziel von ext-26: der Übergang von **eingebetteten Prozessen** zu einem **echten Microkernel**, in
dem TrustedSAS-, HardwareLand- und UserLand-Prozesse **extern gebaut, geladen und gestartet** werden.
Der Kernel stellt dafür einen **generischen Binary-Loader** bereit; die bestehende
Sicherheitsarchitektur (Capabilities, Region-Runtime, DMA, Hot-Reload, Domänen, Audits) bleibt
**vollständig erhalten** und wird in den Loader integriert.

## Entscheidung

### 1. Placement: **In-Kernel-Loader, cap-gegatet** (Architektur A)
Der Loader ist Kernel-Code (kleiner, auditierbarer Mechanismus). Das *Laden* ist **kein
Ambient-Recht**: es erfordert eine **`Loader`-Capability** (neuer `ObjectKind::Loader`), die über
einen neuen Syscall `SYS_LOAD` vorgelegt wird. Der Loader vergibt sich **nichts** selbst; der
geladene Prozess erhält ausschließlich Caps, die der **Aufrufer** (Loader-Cap-Inhaber) bereits
besitzt und im Manifest zur Delegation benennt — installiert über das **bestehende**
`install_cap_checked` (Domänen-Policy + Audits gelten unverändert).

*Vergleich (s. u.):* A statt B (Userspace-Loader-Server), weil B viele neue, extrem mächtige
Syscalls (Fremd-PD-Fabrik + Fremd-Cap-Install) erfordert — was die Angriffsfläche **vergrößert**,
die wir minimieren wollen — und einen In-Kernel-Bootstrap-Loader ohnehin braucht. Der einzige
TCB-Zuwachs von A ist der ELF-Parser; er wird minimal gehalten (Safe-Rust, bounds-geprüft, eigener
Fuzzer).

### 2. Format: **Minimal-ELF64, nur `PT_LOAD`, keine Relokationen**
Statisch gelinkte Rust-Binaries als `ET_EXEC` mit **fixer Lade-VA**. Der Loader implementiert NUR:
ELF64-Header validieren (Magic, Class=64, Data=LE, Machine=`EM_AARCH64`=0xB7, Type=`ET_EXEC`),
Program-Header-Tabelle lesen, **`PT_LOAD`-Segmente** (p_offset/p_filesz/p_memsz/p_vaddr/p_flags)
kopieren, `.bss` (memsz>filesz) nullen, Entry-Point anspringen. **Kein** Dynamic-Linking, **keine**
Relokationen, **kein** Symbol-/Section-Parsing. W^X aus `p_flags`: `X`→RX, `W`→RW, sonst RO.

### 3. Boot-Delivery: **Boot-Archiv in reserviertem RAM-Fenster**
QEMU `-device loader,file=boot-archive.bin,addr=MOD_BASE` legt EIN Archiv in ein **oben in RAM
reserviertes Fenster** (`MOD_BASE = ram_end - MOD_WINDOW`). `init_mem` gibt dem `PhysAllocator`
nur `[free_base, MOD_BASE)` — das Fenster ist also **nie** allokierbar (kein Konflikt). Archiv:
`Header{ magic, version, count, reserved }` + `count ×` `Entry{ name[16], blob_off, blob_len,
domain, manifest_off, manifest_len, flags, hash[32] }` + Blobs + Manifeste. Ein Build-Skript
(`tools/mkarchive`, **kein** Kernelcode) assembliert das Archiv aus den extern gebauten Binaries.
Der Kernel liest das Archiv (Safe-Rust, bounds-geprüft) und lädt Einträge.

### 4. Domänen: EL0-isoliert primär, TrustedSAS (EL1) trust-/signatur-gegatet
- **UserLand / HardwareLand (EL0, isolierte VSpace):** vollständig generisch geladen — eigene
  VSpace, Segmente an `p_vaddr` gemappt, W^X, EL0. Ein bösartiges/fehlerhaftes Binary ist
  **hardware-isoliert** (Fault tötet nur den Prozess). **Primärer, sicherer Ziel-Fall.**
- **TrustedSAS (EL1, globaler SAS):** läuft privilegiert in der Identity-Map (VA=PA), memory-safe
  Rust (SIP). **Ehrlicher Vorbehalt:** extern geladener EL1-Code unterläuft die SIP-Annahme
  („no unsafe + bugloser Compiler") — er ist beliebiger privilegierter Maschinencode. TrustedSAS-
  Laden ist daher **nur** für *signierte, vertrauenswürdige* Komponenten sinnvoll und wird hinter
  der (vorbereiteten) **Signaturprüfung** gegatet; ohne gültige Signatur wird EL1-Laden abgelehnt.
  Fixe VA=PA über eine reservierte Programm-Slot-Konvention (da keine Relokationen).

### 5. Capability-Initialisierung über Manifest
Jedes Programm trägt ein **Manifest** (im Archiv): `domain`, `entry_extra` (Startparameter),
**Cap-Endowment-Liste** = Verweise auf **Caps des Aufrufers**, die in die neue PD delegiert werden
(Slot → Recht). Der Loader: `create_pd_in_domain(domain)` → für jeden Eintrag `copy/mint` aus der
Aufrufer-Cap (nie mehr Rechte als der Aufrufer) → `install_cap_checked(new_pd, slot, cap)`
(Domänen-Policy greift) → Entry-Thread spawnen, **Startparameter in `x0`** (Zeiger auf eine
Boot-Info-Struktur in einer dem Prozess gehörenden Region). **Keine Eskalation, keine
Domänen-Umgehung** — strukturell durch `install_cap_checked` garantiert.

### 6. Lebenszyklus: Start / Ende / Hot-Reload / mehrere Prozesse
`SYS_LOAD` gibt eine **`PdControl`-Cap** auf die neue PD zurück (sofern der Aufrufer TrustedSAS ist)
→ Start/Stop/Pause/Resume über das **bestehende** `SYS_PDCTL`. **Hot-Reload:** ein Programm aus
einer neueren Archiv-Version neu laden = neue PD-Instanz laden + `reload_swap` (bestehender ext-7-
Mechanismus, gleicher Endpoint). Mehrere Prozesse: der Loader ist reentrant (pro Aufruf eine PD;
Region-Runtime + VSpace-Allokator tragen die Mehrfachnutzung).

### 7. Signatur + Versionierung: **vorbereitet**, nicht erzwungen
Archiv-/Manifest-Header tragen `version` und ein `hash[32]`-Feld (reserviert). Die Verifikation ist
ein klar markierter Hook (`verify_image(entry) -> bool`, vorerst `true` außer für EL1 = Pflicht
sobald implementiert). So lässt sich Signaturprüfung + Versionsverwaltung später **ohne
Format-/API-Bruch** nachrüsten.

## Architekturvergleich

| Kriterium | **A: In-Kernel, cap-gegatet** ✅ | B: Userspace-Loader-Server | C: Hybrid |
|---|---|---|---|
| ELF-Parse | im Kernel (TCB; minimal+Fuzzer) | im Userspace (Bug isoliert) | im Kernel/Root wählbar |
| Neue Syscalls | **0–1** (`SYS_LOAD`) | **viele, God-Mode-nah** | 1 |
| Neue Angriffsfläche | klein | **groß** (PD-Fabrik + Fremd-Cap-Install) | mittel |
| Bootstrap | keiner | In-Kernel-Bootstrap nötig | mittel |
| „keine Sonderrechte" | Laden cap-gegatet, Caps via checked-Pfad | reinste Lesart | cap-gegatet |
| Komplexität | gering | hoch | mittel |

**Gewählt: A.** B ist philosophisch reiner (Parser aus dem TCB), erkauft das aber mit Syscalls, die
fremde PDs erzeugen und beliebige Caps in fremde PDs installieren — genau die mächtigen Primitive,
die die TCB-Reduktion zunichtemachen. A hält den TCB-Zuwachs auf einen minimalen, fuzzbaren
ELF-Parser begrenzt und nutzt die bestehenden cap-/domänen-geprüften Pfade.

## Sicherheitsmodell / Invarianten

- **Loader-Autorität:** ohne `Loader`-Cap kein `SYS_LOAD` (`ERR_BADCAP`). Der Loader gewährt nur
  Caps, die der Aufrufer hält (Delegation, keine Erzeugung aus dem Nichts).
- **Domänen-Policy unverändert:** alle initialen Caps über `install_cap_checked`; Domäne bei
  PD-Erzeugung fix + unveränderlich; `domain_audit` greift weiter (HW nur HardwareLand, PdControl
  nur TrustedSas, untrusted isoliert).
- **W^X:** geladene Code-Segmente RX, Daten RW/RO — `vspace_audit` prüft weiter (keine
  EL0-Seite W+X).
- **Region-Balance:** geladene Segment-Regionen via Region-Runtime; Teardown gibt alle zurück
  (`churn`/Balance-Audit gilt).
- **Neuer `loader_audit`:** geladene Images konsistent — Segmente disjunkt, innerhalb der
  PD-Regionen, W^X korrekt, Domäne == Manifest, kein Segment überlappt Kernel-Image/andere PDs.
- **Neuer Loader-Fuzzer:** **fehlerhafte ELFs/Archiv** (abgeschnitten, Bad-Offsets, Memsz<Filesz,
  überlappende Segmente, Riesen-Memsz, Bad-Entry, W^X-Verletzung im Manifest, Bad-Magic) müssen
  **abgelehnt** werden, **ohne** den Kernel zu kompromittieren (kein Panic, kein OOB).
- **EL1-Vorbehalt:** TrustedSAS-Laden nur signatur-verifiziert (sobald implementiert); bis dahin
  EL1-Laden defaultmäßig abgelehnt.

## Konsequenzen

- **Neue Strukturen außerhalb des Kernels:** `programs/{trusted,hardware,userland}/…` und
  `tests/{trusted,hardware,userland}-test-N/` als **unabhängige** Cargo-Projekte (eigenes
  Target/Linker, eigene Build/Docs/Tests, keine Querabhängigkeiten). Ein gemeinsames, minimales
  **SDK-Crate** (`libsel4lake`: Syscall-Stubs, `_start`/crt0, Panic-Handler, Boot-Info) ist eine
  *gemeinsame* Abhängigkeit, **keine** gegenseitige zwischen Diensten.
- **Kerneländerung (sanktioniert):** ELF-Loader-Modul, `ObjectKind::Loader`, `SYS_LOAD`,
  Boot-Archiv-Leser, Allokator-Fenster-Reservierung, `loader_audit` + Fuzzer. Die bestehende
  Microkernel-Semantik (IPC, Caps, Domänen, DMA, Hot-Reload) bleibt **unverändert**; der Loader
  *nutzt* sie nur.
- **Bewusst aufgeschoben:** Relokationen/Static-PIE (für beliebig platzierbares TrustedSAS),
  echte Signatur-/Hash-Verifikation, Laufzeit-Archiv-Update über IPC, Demand-Paging.
- **Wichtige Ehrlichkeit (Testumgebung):** kernel-only Cap-Operationen (copy/mint/install/spawn/
  revoke) sind **aus keinem geladenen Prozess** aufrufbar (kein Syscall, keine Linkage) — auch
  nicht aus EL1-TrustedSAS-Binaries. Adversariale **Cap-/CDT-Angriffe** bleiben daher in der
  **In-Kernel-Selftest-Suite**; geladene EL0-Prozesse greifen über die **Syscall-ABI** an
  (ungültige Cap-Indizes, falsche Rechte, IPC an nicht besessene Endpunkte, MAP auf Fremdspeicher,
  PDCTL ohne Autorität, Fluten, Rennen). Das ist die korrekte, ehrliche Abgrenzung.
