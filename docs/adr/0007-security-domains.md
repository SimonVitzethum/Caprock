# ADR 0007 — Drei Sicherheitsdomänen (Trusted-SAS / HardwareLand / UserLand)

Status: **Akzeptiert** (ext-22, umgesetzt P1–P6)

## Kontext

SEL4Lake hatte bis ext-21 zwei *operative* Prozessklassen: SAS-PDs (globaler
Adressraum, vertrauenswürdiges Rust) und isolierte PDs (eigene VSpace+ASID). Es gab
**keinen** Domänen-Begriff, keine Hardware-Capabilities (IRQs/MMIO waren kernel-intern),
keine Management-Capabilities und keine erzwungene Kommunikations-Policy — Kommunikation
war rein Cap-Besitz-basiert.

Ziel: drei **kernel-getrennte, auditierbare** Domänen, sodass nahezu der gesamte OS-Code
speichersicheres Rust bleibt und nur die unvermeidlichen Hardwarezugriffe in kleinen,
isolierten Backends stattfinden.

## Entscheidung

**Domäne = Policy, VSpace = Isolationsmechanismus.** Jede Protection Domain trägt ein
**unveränderliches** Domänen-Attribut, das die erlaubten Cap-Typen und
Kommunikationsbeziehungen bestimmt. Die eigene VSpace setzt die Isolation nur durch.

- **Trusted-SAS** — globaler Adressraum (VSPACE_OF==0), nur speichersicheres Rust ohne
  `unsafe`. Treiber-/Protokolllogik. Default für alle Altbestand-PDs.
- **HardwareLand** — isolierte VSpace, hält Hardware-Caps (MMIO/IRQ), kleiner `unsafe`-Kern.
  Genau **ein** unveränderlicher Trusted-SAS-Partner; kommuniziert ausschließlich mit ihm.
- **UserLand** — isolierte VSpace, keinerlei Hardware-Rechte, keine direkte Kommunikation
  mit HardwareLand.

### Verglichene Varianten (Domänenmodell)
- **A — reine Cap-Konvention (kein Kernel-Tag):** minimal, aber keine erzwingbare/
  auditierbare Invariante. **Verworfen.**
- **B — Domänen-Tag am `Pd` + Kernel-Enforcement (gewählt):** auditierbar, erzwingbar,
  komponiert mit dem bestehenden PD/VSpace/Cap-Modell ohne Mediator; ermöglicht ein
  `domain_audit()`-Oracle.
- **C — Domäne als Cap-Objekt + Policy-Server:** schwergewichtig, Mediator wird Engpass.
  **Verworfen.**

### Erzwungene Invarianten
1. **Cap-Typ-Policy** (`Caps::install_cap_checked`, zentraler Punkt): Hardware-Caps
   (`Mmio`/`Irq`, generische Kategorie `kind_is_hardware` — `Dma` später ohne ABI-Änderung
   einhängbar) **nur** in HardwareLand; `PdControl` **nur** in TrustedSas.
2. **Isolation**: untrusted Domänen (HardwareLand/UserLand) **müssen** isoliert laufen
   (VSPACE_OF!=0). TrustedSas darf isoliert ODER global laufen (mehr Isolation ist keine
   Verletzung — wichtig für Rückwärtskompatibilität). Geprüft nur für **lebende** Threads.
3. **Unveränderliche paarweise Bindung**: ein HardwareLand-Backend wird bei der Erzeugung
   (`create_hardware_backend`) an genau einen TrustedSas-Partner + genau einen Kanal
   (Endpoint+Notification) gebunden; es darf **nur** Kommunikations-Caps für diesen Kanal
   halten. Kardinalität **1 Trusted : N HardwareLand** (strukturell; N:1 ausgeschlossen,
   da `partner` ein Einzelfeld ist). Stabile `backend_id` (für künftige Mehrgeräte/Hotplug).
4. **Management nur cap-gated** (`ObjectKind::PdControl` + `SYS_PDCTL`): nur eine TrustedSas-
   PD mit der `PdControl`-Cap für eine UserLand-Ziel-PD darf deren Lifecycle steuern
   (PAUSE/RESUME/START/STOP); jede Op ist auf Cap + caller==TrustedSas + target==UserLand
   gegated.
5. **W^X auch für Device-Mappings**: Device-Seiten sind stets PXN|UXN; `vspace_device_wx_ok`
   prüft es.

All dies ist im `domain_audit()`-Oracle gebündelt (Codes 1=HW-Cap falsche Domäne,
2=PdControl falsche Domäne, 3=untrusted läuft global, 4=Backend ohne Trusted-Partner,
5=Backend hält Fremd-Comm-Cap) und in `ipc_audit()` (Code 30+) aggregiert — beide Fuzzer
prüfen es laufend.

### Generische Hardware-Infrastruktur (HardwareLand)
- **MMIO**: `vspace_map_device` mappt **beliebige** MMIO-Regionen EL0-Device (Device-nGnRnE,
  PXN|UXN, nG) in eine isolierte VSpace; spaltet den GiB-0-1-GiB-Device-Block bei Bedarf in
  L2/L3 auf (Rest EL1-only → Kernel-MMIO bleibt erreichbar). `MmioCap` ist nur kernelseitig
  prägbar (kein User-Syscall fordert beliebige Bereiche an), nur in HardwareLand installierbar.
  **`Mmio`/`Irq`-Delete fasst den RAM-Allokator NIE an** (Gerät != RAM).
- **IRQ**: `IrqCap` + `route_spi` (GICD_ITARGETSR — fehlte; ohne Routing erreicht ein SPI
  keinen Kern) + **Deferred-Zustellung**: `handle_irq` (IRQ-Kontext) ist LOCK-FREI (Atomics +
  GIC-Maske + EOI), setzt pending; der Reschedule-Pfad drained VOR dem SCHEDS-Lock und
  signalisiert die gebundene Notification (`signal_from_kernel`).

PL031-RTC ist der erste Proof of Concept dieser generischen Infrastruktur (MMIO-Read +
Match-IRQ); UART/VirtIO/NVMe/USB/GPU nutzen dieselben Primitive unverändert.

## IRQ-Lock-Sicherheit (make-or-break)

Die Deferred-IRQ-Zustellung ist **deadlock-frei**, weil:
- IRQs sind **im gesamten Trap maskiert** (boot.rs `daifset #0xf`; DAIF wird erst beim `eret`
  aus SPSR wiederhergestellt). Ein Kern, der `NTFNS`/`SCHEDS`/`EPS` hält (stets innerhalb
  eines Traps), kann daher **keinen** IRQ nehmen → **keine Same-Core-Reentranz**.
- `handle_irq`/`irq_hook` nehmen **keinen** Lock (nur Atomics + GIC-MMIO).
- `drain_pending_irqs` läuft im Reschedule-Pfad (Trap, IRQs maskiert) **vor** dem SCHEDS-Lock
  und nimmt `NTFNS < SCHEDS` — die etablierte Ordnung. Cross-Core ist nur **begrenztes**
  Spinnen (keine Zyklen, gleiche Ordnung wie alle Pfade).

**Abhängigkeit (dokumentiert):** Diese Garantie beruht darauf, dass NTFNS/SCHEDS-Sektionen
mit maskierten IRQs laufen (in-Trap). Würde künftig ein Pfad IRQs *innerhalb* eines Traps
freigeben, müsste `drain_pending_irqs` weiterhin maskiert laufen.

## Konsequenzen
- HardwareLand ist mit **MMIO + IRQ** vollständig bewiesen (RTC am echten PL031).
- `DmaCap` bleibt als HW-Cap-Kategorie **vorgehalten** (ohne ABI-/Strukturänderung
  nachrüstbar) — Implementierung erst, wenn ein DMA-Gerät/Virtio vorliegt.
- Bestehende 36 Checks unverändert grün (Default-Domäne TrustedSas ⇒ neue Prüfungen sind
  für den Altbestand No-Ops). `./test-qemu.sh` = **42/42 ALL PASS**.
- Sensitivitätsgeprüft: jede neue Invariante einzeln gebrochen → das zuständige Oracle/der
  Test greift (domain-Regel 3, pdctl-Policy, chan-Kanal-Enforcement, rtc-Device-Mapping,
  irq-SPI-Routing, hwfuzz-Baseline).
