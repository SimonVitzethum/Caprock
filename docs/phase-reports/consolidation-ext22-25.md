# Konsolidierungsphase — ext-22 … ext-25 (Architektur-Härtung vor weiteren Features)

Status: **fertig** (K1–K6 + O-A/B/C), Suite grün (49 Checks, `test-qemu.sh`).

## Ziel

Vor weiteren großen Ausbaustufen das Gesamtsystem aus ext-22 (Domänen) … ext-25 (Region-Runtime)
auf **organisch gewachsene Duplikation, zu spezifische APIs, implizite Invarianten und unnötige
Kopplung** prüfen und in **kleinen, getesteten** Schritten begradigen — **keine** neuen Features.
Befund der Analyse: die Schichtung (Domäne=Policy, Cap=Autorität, `DmaEnforcer`=IOMMU-neutraler
Treiber, Region-Runtime=sicheres Substrat) ist tragfähig; die Reibung stammt daher, dass der
DMA-Pfad (ext-23/24) **vor** der Region-Runtime (ext-25) entstand und eigene Region-/Bump-/Bounds-
Maschinerie mitbrachte, die die Runtime heute teils überflüssig macht.

## Durchgeführte Schritte (je build → `test-qemu.sh` 49/49 → commit)

### Wichtig (verifikationstragend)
- **K1 — Invarianten explizit + Revoke-Audit.** `docs/invariants.md` macht die bisher impliziten
  Invarianten explizit: **Sperrordnung** als totale Rang-Hierarchie (R0 `CAPS` … R4 `MEM` innerster,
  mit allen aus dem Code belegten Schachtelungen), **DMA-Revoke-Reihenfolge**, **Region-Balance**,
  **`RegionView`/`Pod`-Sicherheitsvertrag** (das gesamte Speicher-`unsafe`), Domänen-/Cap-Policy,
  SMMU-unter-QEMU-Befund, Audit-Katalog. Neuer **`dma_audit()` Code 4**: keine in einem SMMU-Kontext
  gemappte Region überlappt freies RAM (sonst `free` vor `disable_dma` → DMA-use-after-free); trägt
  `PhysAllocator::overlaps_free`. Veralteter `system.rs`-Modul-Doc-Kopf (nicht mehr existenter
  `RES`-Lock) korrigiert.
- **K3 — eine MEM-Carve-Stelle.** `alloc_dma_region` carvt jetzt über die kanonische
  `KernelRegionSource::request` (`Purpose::Dma`) statt direkt `MEM.lock().alloc`. Bewusst **nicht**
  auf Region-Besitz umgestellt: das DmaCap-Besitzmodell ist (phys,len)-basiert (Lebensdauer an der
  DmaCap, Freigabe via `delete_leaf`→`free_region`), ein zweites **legitimes** Besitzmodell, kein
  Duplikat — in `invariants.md` §2/§3 begründet (volle Region-Besitz-Umstellung wäre Feature-Scale).

### Sinnvoll
- **K4 — kanonisches `region_contains`.** Die identische Containment-Formel aus `dma_addr_in_region`
  (Level 1), `dma_addr_in_context` (Multi-Region) und `dma_sg_validate` (SG) liegt nun **einmal**;
  alle drei bauen darauf auf.
- **K5 — Test-/Telemetrie-Surface getrennt.** `dma_ctx_*`, `smmu_*`, `dma_audit_with_floor`,
  `peek_dma_words` in `pub(crate) mod testsupport` — nicht Teil der zu verifizierenden DMA-Kern-API
  (16 Aufrufer, alle `threads.rs`, migriert).
- **K6 — ein Mapping-Eintrittspunkt.** `map_region_into_thread(tid, phys, len, MappingKind)` ersetzt
  `map_mmio_into_thread`/`map_dma_into_thread`/`_ex` (3→1). `MappingKind{Device{ro}, Dma{coherent}}`
  wählt Tabellen-Level + Attribute.

### Optional
- **K2 — `DmaPool` entfernt.** Redundant zum SG-/Containment-Pfad (ein Sub-Puffer ist ein
  Teilbereich, validiert via `DmaSgEntry`+`dma_sg_validate`). `dmagen` demonstriert disjunkte
  Sub-Puffer + Erschöpfung jetzt über diesen Pfad.
- **O-A — `DmaEnforcer::attach/detach`** (vorher `enable_dma`/`disable_dma`): IOMMU-neutral,
  konsistent mit den public Wrappern `dma_attach`/`dma_detach`. Reine, compiler-verifizierte
  Umbenennung; public API unverändert.
- **O-B — Hot-Reload-Zustand als Region.** Der Zähler-Zustand (`ckpt`-Test) liegt nun in einer
  `Region` (`Purpose::HotReloadState`, `system::CS_STATE_REGION`) und wird über die **sichere**
  RegionView-API (`hotreload_state_get/set`) angesprochen — kein rohes `peek_u64`/`poke_u64` mehr
  (`CS_STATE_BASE` entfernt). Dasselbe Substrat wie der Prozess-Heap (ADR 0010).
- **O-C — Audit-Namen entwirrt.** `Caps::dma_audit` → `Caps::dma_bounds_audit` (Cap-Bounds-Ebene),
  löst die Namensgleichheit mit dem aggregierenden `system::dma_audit` auf.

## Ergebnis / Bilanz

- **Entfernt:** `DmaPool` (Struct + 4 Methoden), 2 von 3 Mapping-Funktionen, `CS_STATE_BASE` +
  roher Zähler-peek/poke, die duplizierte Containment-Formel (3×→1), eine ad-hoc-`MEM`-Carve-Stelle.
- **Hinzugefügt:** `docs/invariants.md` (Verifikationsreferenz), `dma_audit` Code 4 +
  `overlaps_free` (neue, abgesicherte Invariante), `mod testsupport` (klar getrennte Test-API),
  `MappingKind`/`map_region_into_thread`, `system::hotreload_state_*` (Region-basiert).
- **Bewusst NICHT gemacht (begründet):** DMA-Puffer vollständig auf `Region`-Besitz umstellen —
  das DmaCap-(phys,len)-Modell ist ein zweites legitimes Besitzmodell; eine Umstellung wäre
  Feature-Scale-Rearchitektur, keine Konsolidierung (siehe K3 + `invariants.md` §3).
- **Schichtung unverändert tragend**; alle Änderungen sind kleine strukturelle Begradigungen, die
  die Vor-Verifikations-Fläche verkleinern und Invarianten dokumentieren/absichern.

## Verifikation

`./test-qemu.sh` → **49/49 ALL PASS** (sauberer Lauf bei freier CPU; Host-Last-Flakiness bekannt —
die gegatete Kette `smmubind…hwfuzz` erreicht unter TCG-Starvation das Ende nicht, kein Kernel-Bug).
Jeder der 9 Schritte einzeln gebaut, getestet und committet (`git.simon.jocraft.cc`, master).
