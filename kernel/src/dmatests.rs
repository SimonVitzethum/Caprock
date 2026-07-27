//! **Arch-neutrale DMA-Tests** (ext-38).
//!
//! Diese Prüfungen lagen bis hierher in `threads/mod.rs`, und das Modul ist
//! `#[cfg(target_arch = "aarch64")]`. Auf x86 haben sie damit nicht *geskippt* — es gab sie
//! **gar nicht**. Das Abnahmekriterium für die VT-d-Zuteilung („die vorhandenen Tests hören auf
//! zu skippen, ohne x86-Sonderpfade") war so nicht einlösbar: ein Test, der auf einer
//! Architektur nicht existiert, kann dort auch nicht grün werden.
//!
//! Sie stehen deshalb jetzt hier, arch-neutral, und werden von beiden Bring-up-Pfaden gerufen.
//! Das ist zugleich der einzige echte Test der Behauptung, die Cap-Seite sei arch-neutral: ein
//! Interface mit einer Implementierung ist keine Abstraktion, sondern eine Umbenennung.

use crate::system;
use sel4lake_cap::{DmaCoherence, DmaDir};
use sel4lake_mem::Rights;
use sel4lake_hal as hal;

#[derive(Default)]
pub struct DmaWin {
    pub ok: bool,
    pub narrow: bool,
    pub exhausted: bool,
    pub intact: bool,
    pub balanced: bool,
    pub undeclared: u32,
}

#[derive(Default)]
pub struct DmaTok {
    pub ok: bool,
    /// Hat `dma_attach` ueberhaupt eine Uebersetzung installiert? Ohne das pruefen `tears`
    /// und `freed` nichts -- deshalb eigenes Feld statt einer stillen Konjunktion.
    pub attached: bool,
    pub tears: bool,
    pub freed: bool,
    pub pending: bool,
    pub audit7: bool,
}

/// **IOVA-Fenstergrenzen testen** (ext-36b Nacharbeit).
///
/// Zwei Grenzen, die es vorher gab, aber nicht als Eigenschaft: die Adressbreite des Geraets und
/// das Ende des Fensters. Beide muessen zu einem sauberen Fehlschlag fuehren -- eine
/// abgeschnittene Adresse trifft sonst irgendetwas anderes, und ein erschoepftes Fenster darf
/// keinen halb aufgebauten Kontext hinterlassen.
pub fn run_dmawin(live_sid: u32) -> DmaWin {
    let mut r = DmaWin::default();
    let _ = live_sid;
    let (narrow_sid, wide_sid) = (0x50u32, 0x51u32);
    hal::cpu::local_irq_disable();
    let (Some(r1), Some(r2)) = (
        system::alloc_dma_region(0x1000),
        system::alloc_dma_region(0x1000),
    ) else {
        hal::cpu::local_irq_enable();
        return r;
    };
    let free0 = system::total_free();
    let (rej_w0, _, rej_d0) = system::testsupport::dma_iova_rejects();

    // 1. Geraet, das die Fensteradresse nicht absetzen kann -> `attach` muss abweisen.
    //
    //    Die Breite ist bewusst 20 Bit und nicht 32: das Fenster beginnt oberhalb `RAM_TOP`, und
    //    wie hoch das liegt, haengt an der MASCHINE, nicht an der Architektur. Auf dem
    //    ARM-Testaufbau sind es ~5 GiB (32 Bit reichen nicht), auf dem x86-Aufbau mit 512 MiB
    //    RAM reichen 32 Bit muehelos -- derselbe Test waere dort gruen, ohne die Eigenschaft zu
    //    pruefen. 20 Bit (1 MiB) liegen unter jeder plausiblen Fensterbasis.
    system::dma_declare_device_addr_bits(narrow_sid, 20);
    let narrow = system::dma_enable(narrow_sid, r1.base, r1.len).is_none()
        && system::testsupport::dma_iova_rejects().2 == rej_d0 + 1
        && system::testsupport::dma_ctx_region_count(narrow_sid) == 0; // kein halber Kontext

    // 2. Fenster voll: erste Region geht durch, dann den Bump ans Ende schieben -> zweite
    //    Zuteilung muss sauber scheitern, und der Kontext muss danach unveraendert nutzbar sein.
    let first = system::dma_enable(wide_sid, r1.base, r1.len);
    let exhausted = if first.is_some() {
        system::testsupport::dma_ctx_exhaust_window(wide_sid);
        let second = system::dma_enable(wide_sid, r2.base, r2.len);
        second.is_none() && system::testsupport::dma_iova_rejects().0 == rej_w0 + 1
    } else {
        false
    };
    // Der Kontext traegt weiterhin genau seine erste Region (kein Teilabbau, kein Leck).
    let intact = system::testsupport::dma_ctx_region_count(wide_sid) == 1
        && system::dma_audit() == 0
        && system::vspace_audit() == 0;
    if first.is_some() {
        system::dma_disable(wide_sid, r1.base, r1.len);
    }
    let balanced = system::total_free() == free0;
    system::free_raw_region(r1.base, r1.len);
    system::free_raw_region(r2.base, r2.len);
    hal::cpu::local_irq_enable();

    r.narrow = narrow;
    r.exhausted = exhausted;
    r.intact = intact;
    r.balanced = balanced;
    r.undeclared = system::testsupport::dma_undeclared_devices();
    r.ok = narrow && exhausted && intact && balanced && system::domain_audit() == 0;
    r
}


/// **Teardown-Token testen** (ext-37).
///
/// Zwei Eigenschaften, die vorher nicht existierten:
/// * `cap_delete` **allein** genuegt und ist sicher — die Cap-Crate gibt die Region nicht mehr
///   selbst frei, sondern meldet sie; der Kernel legt still, unmappt, synchronisiert und gibt
///   erst dann zurueck. Vorher war die Reihenfolge eine bewiesene Vorbedingung des Gesamtsystems.
/// * Ist die Stilllegung **nicht bestaetigt**, wird die Uebersetzung trotzdem entfernt (immer
///   zwingend), die Physadresse aber nie zurueckgegeben. Ein Leck statt eines Use-after-free —
///   als Entscheidung, beschraenkt und auditiert.
pub fn run_dmatok(live_sid: u32) -> DmaTok {
    let mut r = DmaTok::default();
    hal::cpu::local_irq_disable();
    // 1. Attach + `cap_delete` OHNE `dma_detach`.
 // echtes Geraet -> Stilllegung bestaetigbar
    // Basislinie VOR dem Ausschneiden: nach dem `cap_delete` muss exakt sie wieder dastehen —
    // die Region selbst und alle Tabellen-Frames, die das Anhaengen alloziert hat.
    let free0 = system::total_free();
    let Some(r1) = system::alloc_dma_region(0x1000) else {
        hal::cpu::local_irq_enable();
        return r;
    };
    let Ok(cap1) = system::install_dma_cap_ex(
        r1.base,
        r1.len,
        DmaDir::Bidirectional,
        DmaCoherence::NonCoherent,
        Rights::RW,
    ) else {
        hal::cpu::local_irq_enable();
        return r;
    };
    let attached = system::dma_attach(live_sid, cap1).is_some();
    r.attached = attached;
    let _ = system::cap_delete(cap1); // KEIN dma_detach
    let tears = attached && system::testsupport::dma_ctx_region_count(live_sid) == 0;
    let freed = system::total_free() == free0 && system::dma_audit() == 0;

    // 2. Geraet, dessen Stilllegung nicht bestaetigt werden kann (StreamID ohne Geraet: das
    //    Konfigurations-Read liefert 0xFFFF). Die Region darf NICHT zurueck in den Allokator.
    let (pend0, unexp0) = system::testsupport::dma_pending_stats();
    let ghost_sid = 0x60u32;
    let free1 = system::total_free();
    let Some(r2) = system::alloc_dma_region(0x1000) else {
        hal::cpu::local_irq_enable();
        return r;
    };
    let Ok(cap2) = system::install_dma_cap_ex(
        r2.base,
        r2.len,
        DmaDir::Bidirectional,
        DmaCoherence::NonCoherent,
        Rights::RW,
    ) else {
        hal::cpu::local_irq_enable();
        return r;
    };
    let attached2 = system::dma_attach(ghost_sid, cap2).is_some();
    let pending = {
        // Im KILL-Kontext: dort ist Pending erwartbar und darf nicht als Anomalie zaehlen.
        let _kill = system::KillScope::enter();
        let _ = system::cap_delete(cap2);
        let (pend1, unexp1) = system::testsupport::dma_pending_stats();
        attached2
            && pend1 == pend0 + 1 //                       geparkt
            && unexp1 == unexp0 //                         und zwar erwartbar (KILL)
            // Alle Tabellen-Frames sind zurueck, **nur die Region nicht** — genau um ihre
            // Groesse fehlt gegenueber der Basislinie.
            && system::total_free() == free1 - r2.len
            && system::testsupport::dma_ctx_region_count(ghost_sid) == 0 // Unmap trotzdem erfolgt
    };
    // Code 7: weder frei noch uebersetzt.
    let audit7 = system::dma_audit() == 0 && system::vspace_audit() == 0;
    hal::cpu::local_irq_enable();

    r.tears = tears;
    r.freed = freed;
    r.pending = pending;
    r.audit7 = audit7;
    r.ok = tears && freed && pending && audit7 && system::domain_audit() == 0;
    r
}

