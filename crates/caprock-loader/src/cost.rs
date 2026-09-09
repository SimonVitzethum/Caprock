//! Was eine PD-Erzeugung kostet (Prozessmodell, lesend — keine Policy, nur Rechnung).
//!
//! Gelesen aus `kernel/src/system.rs::load_into_pd_mit` + `load_elf_mit` (Stand 2026-09-09).
//! Die Funktion dort alloziert in dieser Reihenfolge, jeder Schritt mit eigenem
//! `MANGEL_*`-Grund und eigenem Rueckbau (`cleanup`/`vspace_teardown`):
//!
//! | Schritt | was | Menge | bei Fehler |
//! |---|---|---|
//! | 0. Endowment-Vorbuchung | `endowment_pruefen` | 0 Frames | sofort `None` |
//! | 1. Farbstreifen (optional) | `claim_stripe` | 0 Frames, 1 von 4 Streifen | `MANGEL_FARBSTREIFEN` |
//! | 2. EL0-Kernel-Stack | `claim_user_kstack_masked` | 4 KiB + Wache | Streifen zurueck |
//! | 3. VSpace | `create_vspace_masked` | ASID-Slot + L1 + L2 (je 4 KiB) | Stack + Streifen zurueck |
//! | 4. Segmente | `mem_alloc_masked_anywhere` je Stueck | `ceil(memsz/4096)*4096` je Segment | `cleanup` |
//! | 5. Stack | dito | `LOADED_STACK_BYTES` = 16 KiB | `cleanup` |
//! | 6. Seitentabellen | `pt_rahmen` je 4-KiB-Seite | 1 Rahmen je ≤512 Seiten + User-Fenster | `cleanup` |
//! | 7. Thread | `spawn_user_at_parked` | 1 TCB-Slot + EL1-Stack-Buchung | `cleanup` |
//! | 8. PD-Slot | `create_pd_mit_budget` (liegt VOR 0.) | 1 PD-Eintrag + Budget + Cspace-Lauf | `free` |
//!
//! Fail-closed-Heuristik: `stuecke_noetig > MAX_IMG_SEGS (64)` wird VOR jeder Allokation
//! abgewiesen (`loader: ABGEWIESEN`), damit `loaded_register` (Teardown-Buchhaltung,
//! genau 64 Plaetze) nie ueberlaeuft. Ein Image, das mehr Stuecke braucht, leckt sonst
//! beim Teardown — lieber gar nicht laden als ein Leck.
//!
//! Diese Datei rechnet Schritt 4–6 nach (die einzige Groesse, die vom Image abhaengt).
//! PD-Slot, ASID, Stack und Thread sind Konstanten des Kernels und stehen hier als
//! benannte Summanden, nicht als Magie.

/// Teardown-Buchhaltung je Programm (Spiegel von `kernel/src/system.rs::MAX_IMG_SEGS`).
pub const MAX_IMG_SEGS: usize = 64;
/// Stack eines geladenen Programms (Spiegel von `LOADED_STACK_BYTES`).
pub const LOADED_STACK_BYTES: u64 = 16 * 1024;
/// Seitengroesse.
pub const PAGE: u64 = 4096;

/// Was eine PD-Erzeugung am Image haengt (Schritte 4–6 der Tabelle oben).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdKosten {
    /// Aufgerundete Segment-Bytes (Summe `ceil(memsz/4096)*4096`).
    pub segment_bytes: u64,
    /// Seitenrahmen dafuer (inkl. 16-KiB-Stack).
    pub daten_seiten: u64,
    /// Geschaetzte Seitentabellen-Rahmen (1 je ≤512 Seiten + 1 User-Fenster + L1/L2).
    pub tabellen_rahmen: u64,
    /// Stuecke gegen `MAX_IMG_SEGS` (Segmente + Stackstuecke, ungefaerbt je 1).
    pub stuecke: usize,
}

/// Rechne die Image-abhaengigen Kosten nach. `memsz_liste` = `memsz` je PT_LOAD-Segment.
/// `None` = wuerde die Teardown-Buchhaltung sprengen (fail-closed, vor jeder Allokation).
pub fn pd_kosten(memsz_liste: &[u64]) -> Option<PdKosten> {
    let mut segment_bytes: u64 = 0;
    let mut stuecke: usize = 0;
    for &memsz in memsz_liste {
        let auf = memsz.div_ceil(PAGE).max(1) * PAGE;
        segment_bytes = segment_bytes.checked_add(auf)?;
        // Ungefaerbt: ein Stueck je Segment (Fastpath in `load_into_pd_mit`).
        stuecke = stuecke.checked_add(1)?;
    }
    // Stack zaehlt mit (liegt in derselben `seglist`, wenn das Mapping scheitert).
    stuecke = stuecke.checked_add(LOADED_STACK_BYTES.div_ceil(PAGE) as usize)?;
    if stuecke > MAX_IMG_SEGS {
        return None;
    }
    let daten_seiten = segment_bytes
        .checked_add(LOADED_STACK_BYTES)?
        .div_ceil(PAGE);
    // 1 Rahmen je ≤512 Seiten + 1 User-Fenster + L1/L2.
    let tabellen_rahmen = daten_seiten.div_ceil(512).max(1) + 1 + 2;
    Some(PdKosten { segment_bytes, daten_seiten, tabellen_rahmen, stuecke })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_kostet_zwei_segmente_plus_stack() {
        // Typisches Kleinstprogramm: Code + Daten + 16-KiB-Stack.
        let k = pd_kosten(&[8192, 4096]).unwrap();
        assert_eq!(k.segment_bytes, 8192 + 4096);
        assert_eq!(k.daten_seiten, (8192 + 4096 + 16384) / 4096);
        assert_eq!(k.stuecke, 2 + 4);
    }

    #[test]
    fn leeres_image_kostet_nur_den_stack() {
        let k = pd_kosten(&[]).unwrap();
        assert_eq!(k.segment_bytes, 0);
        assert_eq!(k.daten_seiten, 16384 / 4096);
        assert_eq!(k.stuecke, 4);
    }

    #[test]
    fn zu_viele_segmente_werden_vor_jeder_allokation_abgewiesen() {
        // 65 Segmente > MAX_IMG_SEGS: `None`, kein Teilzustand.
        let segs = [4096u64; 65];
        assert_eq!(pd_kosten(&segs), None);
    }

    #[test]
    fn genau_60_segmente_plus_stack_passen_noch() {
        // 60 + 4 Stackstuecke = 64 = MAX_IMG_SEGS: gerade noch OK.
        let segs = [4096u64; 60];
        let k = pd_kosten(&segs).unwrap();
        assert_eq!(k.stuecke, 64);
    }

    #[test]
    fn ein_segment_mehr_sprengt_die_buchhaltung() {
        let segs = [4096u64; 61];
        assert_eq!(pd_kosten(&segs), None);
    }

    #[test]
    fn memsz_wird_auf_seiten_aufgerundet() {
        let k = pd_kosten(&[1]).unwrap();
        assert_eq!(k.segment_bytes, 4096);
    }
}
