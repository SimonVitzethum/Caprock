# ext-24 — Generische DMA-Infrastruktur (Richtung, Kohärenz, Multi-Region, Scatter-Gather, Stream-Gruppen)

Status: **fertig** (E0–E5), Suite grün (48 Checks, `test-qemu.sh`).

## Ziel

Vor der Implementierung konkreter DMA-Geräte (virtio-net, NVMe, USB …) die geräteunabhängigen
DMA-Grundfunktionen einmal generisch bereitstellen, damit jedes künftige HardwareLand-Backend
**ohne Architekturänderung** darauf aufbaut. **Additiv** — keine bestehende API gebrochen
(ext-23 + alle Tests bleiben gültig). Siehe [ADR 0009](../adr/0009-generic-dma-infrastructure.md).

## Phasen

- **E0 — DmaCap-Attribute.** `DmaDir { DeviceRead | DeviceWrite | Bidirectional }` +
  `DmaCoherence { Coherent | NonCoherent }`; `ObjectKind::Dma{phys,len,dir,coherence}`;
  `install_dma`/`install_dma_cap` bleiben (Defaults), `install_dma_ex`/`install_dma_cap_ex` neu;
  `dma_cap_attrs`-Lookup. delete_leaf/for_each_dma additiv (`{..}`).
- **E1 — DmaHandle + Kohärenz + Cache-Maintenance + richtungsminimales AP.** `DmaHandle{iova,len}`
  (iova=PA, entkoppelt); `hal::mmu::dma_cache_clean/invalidate` (DC CVAC/CIVAC) +
  `system::dma_prepare/dma_complete`; `vspace_map_dma` kohärenz-aware; `hal::smmu`-Stage-1-Leaf
  mit `ro` (DeviceRead → read-only) + `cacheable` (Coherent → Normal-WB).
- **E2 — DmaContext + Stream-Gruppen.** Je StreamID-Gruppe 1 STE-Gruppe → 1 CD → 1 Stage-1-
  Tabelle, **mehrere Regionen**. `hal::smmu::stage1_create/map_region/unmap_region/read_leaf` +
  `tlbi_sync`. `dma_attach`/`dma_detach` (additiv), `dma_group_add` (N StreamIDs/Kontext).
  `enable_dma`/`disable_dma` auf das Kontext-Modell refactored (rückwärtskompatibel).
- **E3 — Scatter-Gather.** `DmaSgEntry{handle,offset,len}` + `dma_sg_validate` (Level-1, jedes
  Segment in einer angehängten Region); gerätespezifisches Deskriptorformat bleibt im Backend.
- **E4 — DmaPool.** Bump-Sub-Allokator über eine angehängte Region (`new`/`alloc`/`reset`/
  `remaining`); Sub-Puffer sind `DmaHandle` innerhalb der SMMU-gemappten Eltern-Region.
- **E5 — Fuzzer + Doku.** hwfuzz churnt zusätzlich Richtung/Kohärenz (`install_dma_cap_ex`) +
  Kontext-Attach/SG-Validierung/Detach je Epoche (balanciert). ADR 0009, dieser Bericht, Memory.

## Schichtung

```
DmaCap (Mechanismus)   Region + Richtung + Kohärenz; Ownership/Bounds/Lifetime/Audit
DmaEnforcer (Treiber)  SmmuV3Enforcer: DmaContext (STE-Gruppe → CD → Stage-1, N Regionen)
DmaHandle{iova}        gerätesichtbare Adresse, von der PA entkoppelt (iova=PA heute)
Backend (Service)      DmaPool, DmaSgEntry + dma_sg_validate, dma_prepare/complete
```

## Verbesserungen über die Ausgangsliste hinaus

- **Richtungsminimale SMMU-Rechte** (Sicherheit): `DeviceRead` → Stage-1 read-only (Puffer
  gegen ein fehlerhaftes Gerät schreibgeschützt).
- **DmaHandle/IOVA-Entkopplung** (Future-Proofing): spätere Bounce-Buffer/SG-Kompaktierung/
  Remaps ohne API-Bruch.
- **Cache-Maintenance-Primitive** (`dma_prepare`/`dma_complete`).
- **DmaPool** (Sub-Allokation für Deskriptor-Ringe/mbufs).

## Verifikation

`./test-qemu.sh` → **48/48 ALL PASS** (bei freier CPU). Neuer Check `dmagen`: Multi-Region
(3 Regionen in 1 Kontext), Richtung **strukturell** (DeviceRead-Leaf RO, DeviceWrite RW),
Kohärenz (WB/NC), Stream-Gruppe (2 StreamIDs/1 Kontext), Scatter-Gather valide + Out-of-Window
abgewiesen, DmaPool disjunkt + Erschöpfung, balanciert (total_free zurück zur Baseline). Backward-
Compat: virtiorng/hwfuzz weiter grün trotz Enforcer-Kontext-Refactor. Host-Last-Flakiness bekannt
(TCG-Starvation, kein Kernel-Bug).

Wie ext-23: die SMMU-Hardware-Erzwingung ist unter QEMU für emulierte Geräte nicht beobachtbar
— ext-24 ist **strukturell** (Stage-1-Leaves zurückgelesen) + **funktional** auf der Level-1-
Software-Schicht verifiziert; auf realer SMMU-Hardware greift die richtungsminimale Konfiguration.

## Bewusst aufgeschoben

Nicht-identische IOVA-Allokation (Bounce/Remap) hinter dem `DmaHandle`, Slab-DmaPool, gerätespe-
zifische SG-Deskriptor-Builder, Stage-2/nested SMMU, Verifikation auf echter SMMU-Hardware.
