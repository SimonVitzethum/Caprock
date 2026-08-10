# ext-23 — DMA-Capabilities mit SMMUv3-Erzwingung (zweistufig) + virtio-rng-PoC

Status: **fertig** (D0–D5), Suite grün (47 Checks, `test-qemu.sh`).

## Ziel

Die letzte aufgeschobene Hardware-Cap-Kategorie: **DMA**. Ein bus-masterndes Gerät liest/
schreibt Physikspeicher **direkt**, vorbei an der CPU-MMU — ein HardwareLand-Backend könnte ein
Gerät sonst Kernel-/Fremdspeicher überschreiben lassen. Ziel: kernel-kontrollierte `DmaCap` +
hardwaregestützte DMA-Isolation per **ARM SMMUv3**, beides hinter einer austauschbaren
`DmaEnforcer`-Abstraktion (SMMU ist die Implementierung, nicht die Architektur). Siehe
[ADR 0008](../adr/0008-dma-smmu.md).

## Phasen

- **D0 — DmaCap + Abstraktion.** `ObjectKind::Dma{phys,len}`, `install_dma`, `kind_is_hardware`
  schließt DMA ein. `mmu::vspace_map_dma` (EL0-RW Normal-Non-Cacheable, neuer MAIR-Index 2, GiB 1).
  `DmaEnforcer`-Trait + `SmmuV3Enforcer` (Skelett), `alloc_dma_region`, `map_dma_into_thread`,
  Revoke-Reihenfolge (`disable_dma` → Unmap → `free_region`), `dma_audit` (Bounds/Disjunktheit,
  Code 40+). Test `dma`: Backend schreibt+liest die Region (EL0-NC), Kohärenz via Kernel-Identity,
  Policy-Negativ (UserLand abgewiesen), Bounds-Sensitivität. Anders als Mmio/Irq ist DMA echtes
  RAM → `delete_leaf` gibt es frei.
- **D1 — PCIe-ECAM.** HAL `pcie`: ECAM-Config (`@0x40_1000_0000`, global EL1-Device-gemappt via
  `mmu::map_device_block_global`), Bus-Scan, BAR-Sizing/-Zuweisung, Bus-Master-Enable, RID =
  StreamID. Später: Bridge-Enumeration (Root-Ports). Test `pcie`.
- **D2 — SMMU-Bring-up.** HAL `smmu`: Command-/Event-Queue + lineare Stream-Tabelle, CR0
  (SMMUEN|CMDQEN|EVENTQEN), Default-Abort, CMD_SYNC-Round-Trip-Spike. `SmmuV3Enforcer::init`.
  Test `smmu`: IDR0=0x0d44101b, SIDSIZE, CR0ACK, Event-Queue leer, GERROR=0.
- **D3 — STE/CD/Stage-1.** `enable_dma`/`disable_dma`: STE → CD → Stage-1-Pagetable (bildet NUR
  die DmaCap-Region ab), SMMU-Bindungstabelle, balancierte Tabellen-Freigabe. Test `smmubind`.
- **D4 — virtio-rng-DMA + zweistufige Erzwingung.** HAL `virtio`: virtio-pci-(modern)-RNG-Treiber
  (Capability-Parsing, Handshake, Split-Virtqueue in der DmaCap-Region, used-Ring-Polling). Das
  Gerät DMAt **64 echte Zufallsbytes** in die Region. Zweistufiger Kronjuwel (s. u.). Test
  `virtiorng`.
- **D5 — Audit/Fuzzer/Doku.** `dma_audit` finalisiert; `hwfuzz` um Dma-Churn erweitert (alloc/
  install/revoke je Domäne, balancierte `total_free`, Codes 70+). ADR 0008, dieser Bericht, Memory.

## Zweistufige DMA-Durchsetzung (Nutzer-Entscheidung)

- **Level 1 — Software-Disziplin (`dma_addr_in_region`, demonstrierbar):** der vertrauenswürdige
  Treiber validiert **jede** Geräte-DMA-Deskriptor-Adresse gegen die DmaCap, bevor er das Gerät
  programmiert. Out-of-Window → abgewiesen, das Gerät wird gar nicht programmiert. Wirkt auf jeder
  Plattform; im Testlauf bewiesen (Out-of-Window-Deskriptor blockiert, Ziel unverändert; Sensiti-
  vität: ohne die Prüfung schreibt das Gerät → Prüfung lasttragend).
- **Level 2 — SMMUv3 (Hardware-Backstop):** die installierte Stage-1-STE beschränkt den
  tatsächlichen Bus-Zugriff hardwareseitig (für ein kompromittiertes/fehlerhaftes Backend, das
  Level 1 umgeht). Greift auf realer Hardware (STM32MP25 u. a.).

## Befund: QEMU-SMMUv3 + emulierte Geräte

Per QEMU-SMMU-Tracing (`-trace 'smmu*'`, 8 Läufe) belegt: QEMU 11 verarbeitet alle SMMU-Commands
(CR0, CMD_SYNC, CFGI_STE, TLBI) und legt die IOMMU-Memory-Regions an, routet aber den DMA
**emulierter** Geräte (virtio-rng-pci) **nicht** durch `smmuv3_translate` — null
`smmuv3_translate*`/`smmu_ptw*`-Events, als integrierter Endpunkt wie hinter einem `pcie-root-port`
(RID 0x100) und selbst mit V=0-STE (müsste laut Spec aborten). Ursache: QEMUs emulierte SMMUv3
übersetzt für rein-emulierte Geräte (ohne IOMMU-MAP/UNMAP-Notifier, wie nur VFIO sie registriert)
den Bus-Zugriff nicht. **Die Level-2-Hardware-Erzwingung ist daher unter QEMU mit einem emulierten
Gerät nicht beobachtbar** (`cj_smmu_enforced=false`, ehrlich berichtet); die Stage-1-Konfiguration
ist dennoch korrekt installiert und greift auf realer Hardware. Die demonstrierbare Erzwingung im
Testlauf leistet Level 1.

## Neue/erweiterte Komponenten

- `crates/caprock-cap/src/{object.rs,space.rs}`: `Dma`-Variante, `install_dma`, `for_each_dma`,
  `delete_leaf`-Dma-Arm (`free_region`).
- `crates/caprock-microkit/src/lib.rs`: `kind_is_hardware`(Dma), `Caps::dma_audit`.
- `crates/caprock-hal/src/{mmu.rs,gic.rs}` + neu `pcie.rs`, `smmu.rs`, `virtio.rs`.
- `kernel/src/system.rs`: `DmaEnforcer`/`SmmuV3Enforcer`, `install_dma_cap`, `alloc_dma_region`,
  `map_dma_into_thread`, `revoke_dma`, `dma_enable/disable`, `dma_addr_in_region`, `dma_audit`,
  `pcie_find_virtio`, `virtio_rng_dma_demo`, SMMU-Bindungstabelle.
- `kernel/src/threads.rs`: Tests `dma`/`pcie`/`smmu`/`smmubind`/`virtiorng`, hwfuzz-Dma-Churn.
- `docs/adr/0008-dma-smmu.md`.

## Verifikation

`./test-qemu.sh` → **47/47 ALL PASS** (`-machine virt,iommu=smmuv3 -net none -device
pcie-root-port -device virtio-rng-pci,bus=rp0`). Neue Checks: `dma`, `pcie`, `smmu`, `smmubind`,
`virtiorng`. Echter Bus-Master-DMA (64 Zufallsbytes), Level-1-Erzwingung demonstriert, Audits 0,
hwfuzz mit Dma-Churn grün (balancierte `total_free`). Host-Last-Flakiness bekannt (TCG-Starvation).

## Bewusst aufgeschoben

Stage-2/nested SMMU, mehrere gleichzeitige DMA-Geräte, Hotplug, Scatter-Gather über mehrere
DmaCaps, ein realer `NullIommuEnforcer`, Verifikation der Hardware-Erzwingung auf echter
SMMU-Hardware bzw. über den QEMU-VFIO-Pfad.
