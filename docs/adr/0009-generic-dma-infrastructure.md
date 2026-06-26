# ADR 0009 — Generische DMA-Infrastruktur (Richtung, Kohärenz, Multi-Region, Scatter-Gather, Stream-Gruppen)

Status: **Akzeptiert** (ext-24, umgesetzt E0–E5)

## Kontext

ext-23 lieferte `DmaCap` (eine Region) + die `DmaEnforcer`-Abstraktion (SMMUv3) + Level-1-
Software-Bounds. Bevor konkrete DMA-Geräte (virtio-net, NVMe, USB, Ethernet) implementiert
werden, sollen die geräteunabhängigen DMA-Grundfunktionen einmal generisch bereitgestellt
werden, sodass jedes künftige HardwareLand-Backend ohne Architekturänderung darauf aufbaut.

**Harte Vorgabe:** keine bestehende API aufbrechen — rein **additiv** (neue Enum-Felder mit
Defaults, neue Trait-Methoden mit Default-Impl, neue `_ex`-Konstruktoren neben den alten).
ext-23 + die bestehenden Tests bleiben gültig.

## Entscheidung

### 1. Richtung + Cache-Kohärenz als **DmaCap-Attribute** (cap-rein)
`ObjectKind::Dma { phys, len, dir, coherence }` mit `DmaDir { DeviceRead | DeviceWrite |
Bidirectional }` und `DmaCoherence { Coherent | NonCoherent }`. Die Cap kodiert die volle
Autorität inkl. Richtung/Kohärenz. `install_dma`/`install_dma_cap` bleiben (Defaults
Bidirectional/NonCoherent = ext-23-Verhalten); `install_dma_ex`/`install_dma_cap_ex` neu.

- **Richtungsminimale Hardware-Rechte (Sicherheitsgewinn):** der Enforcer mappt die Stage-1-
  Seite gemäß Richtung — `DeviceRead` → **read-only** (das Gerät kann den Puffer nicht
  korrumpieren), `DeviceWrite`/`Bidirectional` → RW. (Verworfen: Richtung erst beim Binden —
  die Autorität wäre nicht vollständig in der Cap.)
- **Kohärenz** → Speicher-Attribute: `Coherent` → Normal-WB (cacheable) + Cache-Maintenance,
  `NonCoherent` → Normal-NC (Default). Backend-VSpace-Mapping (`vspace_map_dma`) ist
  kohärenz-aware.

### 2. **DmaHandle**-Abstraktion (IOVA ≠ PA entkoppelt)
`DmaHandle { iova, len }` ist die *gerätesichtbare* Adresse. Backends programmieren das Gerät
mit `handle.iova`. Heute `iova == phys` (identitäts), aber alle Pfade gehen über das Handle —
damit später **ohne API-Bruch**: Scatter-Gather-Kompaktierung in ein zusammenhängendes IOVA-
Fenster, Bounce-Buffer für 32-bit-Geräte, nicht-identische Remaps. (Verworfen: IOVA=PA
hartkodiert — eine spätere Entkopplung müsste die DMA-API anfassen.)

### 3. **DmaContext** (mehrere Regionen je StreamID; Stream-Gruppen)
Ein Übersetzungskontext je StreamID-**Gruppe**: 1..N STEs → **ein** CD → **eine** Stage-1-
Tabelle, die **mehrere** Regionen abbildet. `dma_attach(stream_id, dma_cap)` hängt eine Region
additiv in den (ggf. neuen) Kontext ein und gibt ein `DmaHandle` zurück; `dma_detach` entfernt
sie (letzte Region → Kontext-Abbau). `dma_group_add(leader, member)` lässt mehrere StreamIDs
denselben Kontext teilen (Multi-Function/SR-IOV/Bridge ohne RID-Translation). Ersetzt die
ext-23-1:1-Bindung; `enable_dma`/`disable_dma` sind rückwärtskompatibel darauf abgebildet
(1 Region = Kontext mit 1 Region). (Verworfen: `DmaCap` hält eine Regionsliste — schweres Cap-
Objekt, bräche das „1 Cap = 1 Region"-Modell.)

### 4. **Scatter-Gather** = Framework-validierte Liste über die angehängten Regionen
`DmaSgEntry { handle, offset, len }`; `dma_sg_validate(stream_id, &[…])` prüft (Level 1) jedes
Segment gegen das Regions-Set des Kontexts. Das **gerätespezifische** Deskriptorformat (virtio-
desc, NVMe-PRP/SGL, NIC-Ring) baut das Backend aus validierten Segmenten — die SG-Logik bleibt
geräteunabhängig. (Verworfen: SG-Logik je Gerät im Kernel — Geräte-Wissen im Kernel.)

### 5. Verbesserungen über die Ausgangsliste hinaus
- **Cache-Maintenance-Primitive** `dma_prepare(handle,dir)`/`dma_complete(handle,dir)`
  (intern `dc cvac`/`dc civac` + `dsb`) — Backends rufen sie geräteunabhängig um Transfers.
- **DmaPool** — Bump-Sub-Allokator über eine angehängte Region (Deskriptor-Ringe, mbufs); jeder
  Sub-Puffer ist ein `DmaHandle` innerhalb der bereits SMMU-gemappten + Level-1-validierbaren
  Eltern-Region. **(In der Konsolidierung K2 entfernt:** redundant — ein Sub-Puffer ist nur ein
  Teilbereich `[handle.iova+offset, +len)`, validiert über den kanonischen SG-/Containment-Pfad
  `DmaSgEntry`+`dma_sg_validate`→`region_contains`. Ein Backend führt bei Bedarf einen trivialen
  Offset-Cursor selbst. Siehe `docs/phase-reports/consolidation-ext22-25.md`.)**

## Schichtung (unverändert tragend)

```
DmaCap (Mechanismus)         ── Region + Richtung + Kohärenz, Ownership/Bounds/Lifetime/Audit
   │ dma_attach / dma_detach / dma_group_add
DmaEnforcer (Treiber)        ── SmmuV3Enforcer: DmaContext (STE-Gruppe -> CD -> Stage-1, N Regionen)
   │ DmaHandle{iova}         ── iova=PA heute, entkoppelt für später
Backend (Service)            ── DmaPool, DmaSgEntry + dma_sg_validate, dma_prepare/complete
                                geräte­spezifischer Deskriptor bleibt hier
```

## Befund-Kontinuität (ext-23)

Die SMMU-Hardware-Erzwingung ist unter QEMU für emulierte Geräte weiterhin nicht beobachtbar
(s. [ADR 0008](0008-dma-smmu.md)). ext-24 ist daher **strukturell** verifiziert (Stage-1-Leaves
zurückgelesen: Richtungs-AP + Kohärenz-Attribut) + **funktional** auf der Level-1-Software-
Schicht (Multi-Region-Validierung, SG-Validierung, DmaPool, Kontext-/Gruppen-Bookkeeping). Auf
realer SMMU-Hardware (STM32MP25 u. a.) greift die richtungsminimale Stage-1-Konfiguration.

## Konsequenzen

- Künftige DMA-Geräte: `install_dma_cap_ex` (Richtung/Kohärenz) → `dma_attach` (eine oder
  mehrere Regionen, ggf. Gruppe) → DmaPool/SG nach Bedarf → `dma_prepare`/`dma_complete` um
  Transfers. Keine Architekturänderung nötig.
- Audits: `dma_audit` (Bounds/Disjunktheit, Code 40+) unverändert; Richtung/Kohärenz beeinflussen
  die Bounds nicht. hwfuzz churnt zusätzlich Richtung/Kohärenz + Attach/SG/Detach.
- Bewusst aufgeschoben: nicht-identische IOVA-Allokation (Bounce/Remap) hinter dem `DmaHandle`,
  Stage-2/nested, DMA-Pool als Slab (statt Bump), gerätespezifische SG-Deskriptor-Builder.
