# ADR 0008 — DMA-Capabilities mit SMMUv3-Erzwingung hinter generischer DmaEnforcer-Abstraktion

Status: **Akzeptiert** (ext-23, umgesetzt D0–D5)

## Kontext

Mit ext-22 besitzt Caprock drei kernel-getrennte Domänen und Hardware-Caps für **MMIO** und
**IRQ**. Bewusst aufgeschoben war **DMA** — die einzige HW-Cap-Kategorie, bei der ein
bus-masterndes Gerät **direkt** Physikspeicher liest/schreibt, **vorbei an der CPU-MMU**. Ein
HardwareLand-Backend, das die DMA-Register eines Geräts programmiert, könnte das Gerät sonst
Kernel- oder Fremddomänenspeicher überschreiben lassen, obwohl die *eigene* VSpace des Backends
isoliert ist — ein direkter Bruch von Isolations-Invariante #1.

Langfristiges Ziel sind reale Plattformen (STM32MP25-Serie u. a.) mit voller hardwaregestützter
DMA-Isolation per ARM SMMUv3.

## Entscheidung

**Der DMA-Mechanismus (`DmaCap`) ist vom Enforcement-Treiber (`DmaEnforcer`) entkoppelt; SMMUv3
ist die Implementierung, nicht die Architektur.**

### DMA-Mechanismus — `DmaCap` (SMMU-agnostisch)
`ObjectKind::Dma { phys, len }` ist die Autorität über eine **kernel-ausgeschnittene,
kontiguierliche RAM-Region**, die als DMA-Puffer dient. Nur kernelseitig geprägt (kein
User-Syscall fordert beliebige physische Bereiche an), nur in **HardwareLand** installierbar
(`kind_is_hardware` schließt `Dma` ein → `Caps::domain_audit` Code 1). Das Capability-Modell,
`install_dma_cap`, `alloc_dma_region`, das Mapping (EL0-RW **Normal-Non-Cacheable** in die
isolierte Backend-VSpace) und die Backend-Schnittstellen sind **ohne jeden SMMU-Bezug**
entworfen.

**Anders als Mmio/Irq ist DMA echtes RAM.** Die Finalisierung (`delete_leaf`) gibt die Region via
`free_region` zurück — DMA-use-after-free-sicher, **weil** die Revoke-Reihenfolge garantiert,
dass vorher kein Gerät mehr in die Region schreiben kann:

```
enforcer.disable_dma(binding)   // Durchsetzung entziehen (SMMU: STE invalidieren + TLBI + SYNC)
        ↓
VSpace-Unmap (zurück auf EL1-only) + flush_asid
        ↓
free_region(...)                // erst jetzt RAM freigeben
```

### Enforcement — `DmaEnforcer`-Trait (IOMMU-neutral)
Eine kernel-interne Trait-Abstraktion: `init` / `enable_dma(binding)` / `disable_dma(binding)` /
`audit`. Eine `DmaBinding` trägt nur generische IDs (StreamID, PhysRegion, Backend-PD), **keine**
SMMU-Registerdetails. Die einzige Stelle, die SMMU-Register/STE/CD/Queues kennt, ist
`SmmuV3Enforcer` (+ `hal::smmu`). Der öffentliche DMA-Pfad spricht ausschließlich das Trait an.
Ein künftiger `NullIommuEnforcer` oder anderer IOMMU-Treiber implementiert dasselbe Trait, **ohne**
öffentlichen Code (DmaCap, install_dma_cap, Mapping, Treiber, HardwareLand, Audits) zu berühren.

### SMMUv3-Implementierung (Stage-1, Default-Abort)
`SmmuV3Enforcer` bringt die SMMU hoch (Command-/Event-Queue + lineare Stream-Tabelle, CR0
SMMUEN|CMDQEN|EVENTQEN). Die Stream-Tabelle ist genullt → **Default-Abort** (jeder Stream
abortet, bis eine STE installiert wird). `enable_dma` installiert je gebundenem Gerät genau eine
**STE → CD → Stage-1-Pagetable** (VMSAv8-64, T0SZ=25, 4-KiB-Granule), die **ausschließlich** die
DmaCap-Region identitäts-abbildet; alles andere bleibt ungemappt → Translation-Fault (Event-Queue).

### Adress-Programmierung backend-direkt, kernel-auditiert
virtio legt Buffer-Physadressen in RAM-**Deskriptoren** ab (nicht in MMIO-Registern), daher greift
keine kernel-mediierte Register-Prüfung. Stattdessen baut das vertrauenswürdige Backend Ringe mit
Physadressen aus seiner DmaCap; die SMMU ist die hardwareseitige erzwingende Instanz.

## Zweistufige Durchsetzung (Nutzer-Entscheidung)

Während der Validierung zeigte sich (per QEMU-SMMU-Tracing, s. u.), dass QEMU für **emulierte**
Geräte den DMA nicht durch den SMMU-Translate-Pfad routet — die hardwareseitige Erzwingung ist
unter QEMU mit einem emulierten Gerät nicht *vorführbar*. Daraufhin wurde die Architektur
zweistufig ausgelegt:

- **Level 1 — Software-Disziplin (hardware-unabhängig, demonstrierbar):** der vertrauenswürdige
  Treiber validiert via `dma_addr_in_region` **jede** Geräte-DMA-Deskriptor-Adresse gegen die
  DmaCap, **bevor** er das Gerät programmiert. Eine Out-of-Window-Adresse wird abgewiesen → das
  Gerät wird gar nicht erst programmiert. Wirkt auf jeder Plattform; in QEMU im Testlauf bewiesen.
- **Level 2 — SMMUv3 (Hardware-Backstop):** die installierte Stage-1-STE beschränkt den
  tatsächlichen Bus-Zugriff des Geräts hardwareseitig — der Backstop für den Fall, dass ein
  kompromittiertes/fehlerhaftes Backend die Software-Prüfung umgeht. Greift auf realer Hardware.

## Verglichene Alternativen

- **Keine IOMMU, nur Software:** tractable, aber keine HW-Garantie. **Verworfen** (Sicherheit #1).
- **SMMU-zwingend (ursprüngliche Festlegung):** höchste Garantie, aber nicht portabel auf SoCs
  ohne IOMMU und in QEMU mit emulierten Geräten nicht demonstrierbar. **Ersetzt** durch das
  zweistufige Modell (Level 1 demonstrierbar + portabel, Level 2 als HW-Backstop).
- **Stage-2 / nested SMMU:** mächtiger (Gast-Virtualisierung), hier unnötig. **Verworfen.**

## Sicherheits-Invarianten (von Oracles abgesichert)

`dma_audit()` (in `ipc_audit` als Code 40+ aggregiert):
1. DmaCap-Regionen page-aligned, im mappbaren GiB-1-Fenster, disjunkt vom Kernel-Image
   (`floor = kernel_end`) — eine Region über dem Kernel-Image wird abgewiesen.
2. Zwei verschiedene DmaCap-Regionen überlappen nie (objektgranular, Cap-Kopien zählen einfach).
3. Revoke-Reihenfolge (`disable_dma` → Unmap → free) → kein DMA-use-after-free.
4. `enforcer.audit()`: keine globalen SMMU-Fehler (GERROR), Event-Queue im Normalbetrieb leer.

Ressourcen-Baseline (hwfuzz): MMIO/IRQ-Delete fasst den RAM-Allokator **nicht** an (Gerät != RAM);
**DMA**-alloc ↔ free ist **balanciert** (Region zurück zur Baseline je Epoche).

## Befund: QEMU-SMMUv3 + emulierte Geräte

Per QEMU-SMMU-Tracing (`-trace 'smmu*'`, 8 Läufe) belegt: QEMU 11 erzeugt zwar die IOMMU-Memory-
Regions (`smmu_add_mr`) und verarbeitet alle SMMU-Commands (CR0, CMD_SYNC, CFGI_STE, TLBI), routet
aber den DMA **emulierter** Geräte (hier virtio-rng-pci) **nicht** durch `smmuv3_translate` — **null**
`smmuv3_translate*`/`smmu_ptw*`/`decode_cd`-Events, sowohl als integrierter Bus-0-Endpunkt als auch
hinter einem `pcie-root-port` (Bus 1, RID 0x100), und selbst mit einer **V=0**-STE (die laut Spec
aborten müsste) gelingt der DMA. Ursache: QEMUs emulierte SMMUv3 übersetzt für rein-emulierte Geräte
(ohne IOMMU-MAP/UNMAP-Notifier, wie sie nur VFIO-Passthrough registriert) den Bus-Zugriff nicht.

**Konsequenz:** Die Level-2-Hardware-Erzwingung ist unter QEMU mit emulierten Geräten **nicht
beobachtbar** (`cj_smmu_enforced=false`, ehrlich berichtet). Die Stage-1-Konfiguration ist dennoch
korrekt installiert und würde auf realer Hardware greifen. Die demonstrierbare Erzwingung im
Testlauf leistet Level 1 (Software-Bounds); der Kronjuwel-Test zeigt zusätzlich, dass die
Software-Prüfung lasttragend ist (ohne sie schreibt das Gerät das Out-of-Window-Ziel).

## Konsequenzen

- DMA-fähige Treiber (z. B. NVMe, NIC) leben als HardwareLand-Backends mit DmaCap + MmioCap;
  die SMMU beschränkt den Bus-Zugriff auf realer HW, die Software-Disziplin auf jeder Plattform.
- Portabel: dieselbe Architektur/API auf SoCs mit **und** ohne SMMU; der Enforcer ist austauschbar.
- Bewusst aufgeschoben: Stage-2/nested, mehrere gleichzeitige DMA-Geräte, Hotplug, Scatter-Gather
  über mehrere DmaCaps, ein realer `NullIommuEnforcer`, Verifikation der Hardware-Erzwingung auf
  echter SMMU-Hardware (oder QEMU-VFIO-Pfad).
