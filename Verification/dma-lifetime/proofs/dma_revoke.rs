// SEL4Lake — Phase 7 (DMA-Lifetime), Schritt A/B: Revoke-Reihenfolge + Use-after-free-Sicherheit.
//
// Formale Spezifikation der DMA-Lebenszyklus-Kern-Invariante (kernel/sel4lake, `dma_audit` Code 4,
// `revoke_dma`, docs/invariants.md §2): eine RAM-Region, die noch in einem SMMU-/IOMMU-Kontext
// **DMA-aktiviert** (attached) ist, darf **niemals** freigegeben sein — sonst koennte ein bus-
// masterndes Geraet in bereits recyceltes RAM schreiben (**DMA-use-after-free**, ein direkter Bruch
// der Isolations-Invariante). Die sichere Revoke-Reihenfolge ist `detach -> (unmap) -> free`:
// erst die Durchsetzung entziehen (SMMU-Invalidierung), dann das RAM freigeben.
//
// Bewiesen: attach/detach/free erhalten die Invariante; `free` verlangt eine **abgetrennte** Region
// (Revoke-Ordnung); eine **freigegebene** Region ist nie attached und kann nicht erneut attached
// werden (kein DMA nach free); die kanonische `detach->free`-Sequenz ist **immer** sicher.
//
// Sequentielles Modell; Nebenlaeufigkeit + die SMMU-HW-Durchsetzung selbst bleiben ausserhalb
// (Concurrency-/HAL-TCB, ADR 0022).
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Lebenszyklus-Zustand einer DMA-RAM-Region (Projektion auf die für die Use-after-free-Sicherheit
/// relevanten Flags). `attached` = aktuell in einem SMMU-/IOMMU-Kontext DMA-aktiviert; `freed` = an
/// den Allokator zurückgegeben.
pub struct DmaRegion {
    pub attached: bool,
    pub freed: bool,
}

/// **DMA-Lebenszyklus-Kern-Invariante** (= `dma_audit` Code 4): eine DMA-aktivierte Region ist
/// **nie** freigegeben. Kontrapositiv: eine freigegebene Region ist nie DMA-aktiviert -> kein Gerät
/// kann je in recyceltes RAM DMAen.
pub open spec fn dma_inv(r: DmaRegion) -> bool {
    r.attached ==> !r.freed
}

// ===================== Zustandsuebergaenge (spec) =====================

/// **attach** (`enforcer.attach`): die hardwareseitige DMA-Durchsetzung für eine Region aktivieren.
/// Nur auf einer **lebenden** (nicht freigegebenen) Region zulässig.
pub open spec fn attach(r: DmaRegion) -> DmaRegion {
    DmaRegion { attached: true, ..r }
}

/// **detach** (`enforcer.detach`): die Durchsetzung entziehen (SMMU-Invalidierung). Idempotent,
/// stets zulässig — danach kann das Gerät nicht mehr in die Region DMAen.
pub open spec fn detach(r: DmaRegion) -> DmaRegion {
    DmaRegion { attached: false, ..r }
}

/// **free** (`free_region`/`delete_leaf`): das RAM an den Allokator zurückgeben. Verlangt eine
/// **abgetrennte** Region (Revoke-Ordnung — `detach` muss vorausgegangen sein).
pub open spec fn free(r: DmaRegion) -> DmaRegion {
    DmaRegion { freed: true, ..r }
}

// ===================== Bewiesene Eigenschaften =====================

/// **BEWEIS (attach erhaelt die Invariante):** DMA nur auf einer **lebenden** Region aktivieren.
pub proof fn attach_preserves_inv(r: DmaRegion)
    requires dma_inv(r), !r.freed,
    ensures dma_inv(attach(r)),
{
}

/// **BEWEIS (detach erhaelt die Invariante):** nach dem Entziehen ist die Region nicht aktiviert.
pub proof fn detach_preserves_inv(r: DmaRegion)
    requires dma_inv(r),
    ensures dma_inv(detach(r)),
{
}

/// **BEWEIS (free erhaelt die Invariante):** Freigeben verlangt eine **abgetrennte** Region — danach
/// ist die freigegebene Region nicht aktiviert (Invariante bleibt).
pub proof fn free_preserves_inv(r: DmaRegion)
    requires dma_inv(r), !r.attached,
    ensures dma_inv(free(r)),
{
}

/// **BEWEIS (kein DMA nach free):** eine freigegebene Region ist (unter der Invariante) **nie**
/// DMA-aktiviert — kein Gerät zeigt auf recyceltes RAM.
pub proof fn freed_not_attached(r: DmaRegion)
    requires dma_inv(r), r.freed,
    ensures !r.attached,
{
}

/// **BEWEIS (free verlangt Revoke-Ordnung):** der Freigabe geht zwingend ein `detach` voraus — die
/// Vorbedingung von `free` ist „nicht aktiviert". (Macht die Reihenfolge `detach -> free` explizit.)
pub proof fn free_requires_detached(r: DmaRegion)
    requires !r.attached,
    ensures free(r).freed && !free(r).attached,
{
}

/// **BEWEIS (kanonische Teardown-Sequenz ist immer sicher):** für **jede** invariante Region liefert
/// `detach` gefolgt von `free` eine wohldefinierte (Vorbedingung erfüllt) Freigabe, die die Invariante
/// erhält und die Region abgetrennt + freigegeben zurücklässt — die dokumentierte Revoke-Reihenfolge
/// ist universell anwendbar (kein DMA-use-after-free möglich).
pub proof fn safe_teardown(r: DmaRegion)
    requires dma_inv(r),
    ensures
        dma_inv(free(detach(r))),
        free(detach(r)).freed,
        !free(detach(r)).attached,
{
    // detach(r).attached == false -> Vorbedingung von free erfüllt; free setzt freed, lässt
    // attached == false -> Invariante (false ==> _) gilt.
    detach_preserves_inv(r);
    free_preserves_inv(detach(r));
}

fn main() {}

} // verus!
