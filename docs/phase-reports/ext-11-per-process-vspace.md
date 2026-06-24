# Ausbaustufe 11 — Per-Prozess-VSpaces (Weg C, Hybrid)

**Datum:** 2026-06-24 · **Status:** umgesetzt & in QEMU verifiziert (erstes Inkrement).

Beantwortet die Sicherheitsfrage „wie verhindert man, dass ein kompromittierter
nativer (C-)Prozess fremden Prozessspeicher liest/schreibt — perfekte Trennung, nur
IPC". Bisher (ADR 0002, SAS) konnte jeder EL0-Code **jedes** User-RAM lesen/schreiben
(alles `UserRw`), und Capabilities/EL0 schützen das nicht (sie gaten Kernel-Objekte
bzw. die Kernel-Grenze, nicht rohe Loads/Stores). Weg C führt **per-Prozess-
Adressräume** für isolierte PDs ein, neben der schnellen SAS-Spur für
vertrauenswürdige Rust-PDs.

## Modell (Hybrid)

- **Vertrauenswürdige PD** → globale SAS-Map (`TTBR0 = global_root`, ASID 0) — wie
  bisher, schnell, deterministisch, Zero-Copy-fähig.
- **Isolierte PD** → eigene VSpace (eigene Tabellen + ASID), die **nur** mappt:
  - den Kernel (EL1-only, geteilt) — damit Traps/IPC funktionieren,
  - die geteilte `.user_text` (EL0-RX, global) — der Code,
  - **genau** ihre private 2-MiB-Region (EL0-RW, `nG`) — ihre Daten/ihr Stack.
  Alles übrige RAM ist in ihrer VSpace **EL1-only** → ein Streupointer in fremdes
  RAM erzeugt einen Fault → der Kernel beendet die PD.

Adressierung bleibt **Identity** (VA==PA, kein Relinking): isoliert wird über die
**Präsenz** der Mappings, nicht über Übersetzung. Das passt zur SAS-Philosophie und
hält die Änderung am Kernel minimal.

## Umsetzung

- **mmu** ([mmu.rs](crates/sel4lake-hal/src/mmu.rs)): erste 2 MiB sind reine
  Kernel-L3 (kein EL0-freies-RAM mehr → von jeder VSpace teilbar; User-RAM ab 2 MiB).
  User-Daten (`UserRw`) sind jetzt `nG` (ASID-spezifisch), User-Code/Kernel/Device
  global. Neu: `global_root`, `set_user_vspace(root, asid)`,
  `build_isolated_vspace(l1, l2, region_base, region_len)` (baut Kernel-EL1-only +
  eine EL0-Region in zwei frische 4-KiB-Frames).
- **system** ([system.rs](kernel/src/system.rs)): `VSPACE_OF[thread]` (gepacktes
  TTBR0 je Thread; 0 = SAS), `sync_vspace` setzt `TTBR0` beim Kontextwechsel passend
  zum einlaufenden Thread (nur wenn sich die VSpace ändert → kein `isb` im
  All-Trusted-Fall). `spawn_isolated` alloziert die Region + Tabellen, baut die
  VSpace, vergibt eine ASID. Der `el0_fault`-Hook merkt `iso_faulted`, wenn ein
  Thread **in einer isolierten VSpace** faultet.
- **TLB/ASID**: kein Flush pro Switch — Kernel/Code sind global (`nG=0`, in jeder
  VSpace gültig), User-Daten `nG` + ASID-getaggt; ASID 0 = SAS, ab 1 = isolierte PDs.

### Warum das deadlock-/korrektheitssicher ist
- Der Kernel ist in **jeder** VSpace identisch (Identity, global) gemappt → das
  TTBR0-Umschalten mitten im Kernel stört die Kernel-Ausführung nicht (`isb`
  ordnet). Register-IPC kopiert zwischen TrapFrames auf Kernel-Stacks (im
  Kernelimage, EL1-only, überall gemappt) → funktioniert kern-/VSpace-übergreifend.
- Die erste 2-MiB-Kernel-L3 enthält **kein** EL0-zugängliches freies RAM mehr
  (sonst Leck über die geteilte L3); User-RAM beginnt bei 2 MiB.

## Verifiziertes Ergebnis

`./test-qemu.sh` → **ALL PASS** (21 Checks). Neuer `vspace`-Check — A/B-Kontrast auf
**derselben** fremden Adresse X (ein gepoktes Geheimnis):
- **SAS-Probe** (vertrauenswürdig, EL0) liest X → erlaubt (`SAS-Probe las X=true`).
- **Isolierte Probe** (eigene VSpace, EL0) meldet sich erst per `SIGNAL` (läuft auf
  EL0 + IPC geht: `lief+IPC=true`), liest dann X → **Fault** (`faultete=true`,
  `EC=0x24 FAR=0x40411000`), las X **nie** (`las-X=false`) → Kernel beendet sie.

Damit ist **hardware-erzwungene User↔User-Trennung** belegt: ein kompromittierter
(isolierter) Prozess kann fremden Speicher nicht erreichen; Kommunikation läuft
ausschließlich über IPC. 5/5 gespacete Läufe stabil (Truncations nur unter
Host-Überlast bei vielen parallelen QEMUs, kein Kernel-Hang).

## Offene Punkte (Folge-Inkremente)

- **Allgemeiner VMM**: aktuell eine feste 2-MiB-Region je isolierter PD (in GiB 1);
  Page-Tables (L1+L2, 8 KiB) werden beim PD-Ende geleakt. Künftig: mehrere
  Regionen/Frames, `map`/`unmap` als cap-gatete Syscalls (Frame-Caps statt linearer
  `MemoryCap`), VSpace-Teardown.
- **Shared-Memory-IPC** zwischen isolierten PDs: denselben Frame in beide VSpaces
  mappen (cap-gewährt) — Zero-Copy über die Isolationsgrenze.
- Untrusted **nativer** Code echt ausführen (eigenes `.user_text` je isolierter PD
  laden, statt geteilte Demo-`.user_text`); alternativ SFI/WASM (Weg B) für SAS-Erhalt.
- ADR 0002 ist um den Hybrid-Nachtrag zu ergänzen (Bedrohungsmodell deckt nun
  isolierte Prozesse hardware-seitig ab).
