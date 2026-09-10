//! EXEC-Replace: neues Image in die BESTEHENDE PD laden (Prozessmodell, Phase 1).
//!
//! Reine Prueflogik (`core` nur, kein `unsafe`): was der Kernel-Dispatch vor dem ersten
//! Rueckbau wissen muss. Der Rueckbau selbst (Threads abziehen, Slots loeschen, Mappings
//! loesen, neu laden) liegt im Kernel (Patch-Text in `caprock-microkit::proc`); was hier
//! steht, ist die Ordnung, die er einhalten muss — und jede Abweichung ist ein benannter
//! Fehler, kein stiller Ueberrest.
//!
//! ## Teardown-Token-Form
//!
//! `token == 0` heisst „kein Token" und wird ABGEWIESEN (`ERR_STALE_TOKEN`), nicht als
//! „egal" gelesen. Das Token bindet Antrag an Stand: `token = teardown_token_fuer(
//! program_id, epoche)`. Wer mit dem Token von gestern kommt, bekommt keinen halb
//! geraeumten Kind-Zustand, sondern eine Absage — ein Ueberrest (alter Thread, alte Cap,
//! altes Mapping) ist ein Baufehler.
//!
//! ## Geordnete Rueckzugsreihenfolge (fuer den Kernel-Patch)
//!
//! 1. Alle Threads der PD ausser dem Aufrufer abziehen (`KILL`-Ordnung).
//! 2. Alle Slots loeschen ausser Loader-Cap + Aufrufer-Stack-Cap.
//! 3. VSpace-Inhalt loesen, aber ASID + PD-Slot + Budget BEHALTEN (kein `vspace_teardown`:
//!    die PD lebt weiter, nur ihr Inhalt geht).
//! 4. Neu laden (`load_into_pd_mit`-Ordnung: Endowment verbuchen, Segmente, Stack,
//!    Tabellen, Thread wiederverwenden oder neu).
//!
//! Wer 3 vor 2 tut, loescht Caps, deren Speicher noch gemappt ist; wer 4 vor 1 tut,
//! laedt ueber laufende Threads. Die Reihenfolge ist der Inhalt.

/// Epoche + Programm-ID falten zu einem Token, das kein gueltiger „egal"-Wert ist.
pub fn teardown_token_fuer(program_id: u32, epoche: u32) -> u64 {
    // FNV-1a ueber 8 Bytes, danach `0` verboten (s. `pruefe_exec_antrag`).
    //
    // Die Offset-Basis ist der FNV-Standard (`14695981039346656037`), kein Caprock-Eigenwert --
    // und genau deshalb steht der Vektor-Test unten mit einem unabhaengig nachgerechneten Wert:
    // Am 2026-09-09 stand hier ein Tippfehler (`...2325` statt `...25c5`), und die
    // Selbstkonsistenz-Tests (Loader-Spiegel in `proc.rs` inklusive) blieben allesamt gruen,
    // weil sie denselben falschen Wert spiegeln. Ein Standard, den nur Spiegel pruefen, ist
    // keiner.
    let mut h: u64 = 0xcbf29ce4842225c5;
    let bytes = [
        (program_id & 0xff) as u8,
        ((program_id >> 8) & 0xff) as u8,
        ((program_id >> 16) & 0xff) as u8,
        ((program_id >> 24) & 0xff) as u8,
        (epoche & 0xff) as u8,
        ((epoche >> 8) & 0xff) as u8,
        ((epoche >> 16) & 0xff) as u8,
        ((epoche >> 24) & 0xff) as u8,
    ];
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    if h == 0 { 1 } else { h }
}

/// Warum ein EXEC-Antrag abgelehnt wurde (jeder Ausgang mit eigenem Namen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecAbweisung {
    /// `token == 0`: kein Token genannt (kein „egal").
    KeinToken,
    /// Token passt nicht zu `(program_id, epoche)`: alter Stand.
    AlterStand,
    /// Kein Segment oder mehr als die Teardown-Buchhaltung fasst.
    SegmentZahl,
    /// Eintrag ausserhalb der unteren Adresshaelfte (kein Kernel-Sprung).
    SchlechterEintrag,
}

/// Obergrenze der User-Adressen (Spiegel von `caprock-abi::USER_VA_TOP`).
pub const USER_VA_TOP: u64 = 1 << 47;
/// Teardown-Buchhaltung (Spiegel von `cost::MAX_IMG_SEGS`).
pub const MAX_SEGS: usize = 64;

/// Pruefe einen EXEC-Antrag VOR dem ersten Rueckbau. `erwartet` = Token, das der Kernel
/// aus `(program_id, epoche)` ableitet — der Vergleich steht hier, damit der Dispatch
/// ihn nicht nachbaut (eine Pruefung an zwei Stellen ist zwei Pruefungen).
pub fn pruefe_exec_antrag(
    program_id: u32,
    epoche: u32,
    token: u64,
    eintrag: u64,
    nseg: usize,
) -> Result<(), ExecAbweisung> {
    if token == 0 {
        return Err(ExecAbweisung::KeinToken);
    }
    if token != teardown_token_fuer(program_id, epoche) {
        return Err(ExecAbweisung::AlterStand);
    }
    if nseg == 0 || nseg > MAX_SEGS {
        return Err(ExecAbweisung::SegmentZahl);
    }
    if eintrag == 0 || eintrag >= USER_VA_TOP {
        return Err(ExecAbweisung::SchlechterEintrag);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_ist_stabil_und_nie_null() {
        let t1 = teardown_token_fuer(7, 3);
        assert_eq!(t1, teardown_token_fuer(7, 3));
        assert_ne!(t1, 0);
        assert_ne!(teardown_token_fuer(7, 4), t1);
        assert_ne!(teardown_token_fuer(8, 3), t1);
    }

    #[test]
    fn token_basis_ist_fnv_standard() {
        // Unabhaengig nachgerechnet (FNV-1a, Basis 0xcbf29ce4842225c5, Bytes
        // `[01 00 00 00 00 00 00 00]`): haette je Seite einen anderen Tippfehler in der
        // Konstanten, fiele genau dieser Test -- die Spiegel-Tests unten faenden ihn nie
        // (s. Doku an `teardown_token_fuer`).
        assert_eq!(teardown_token_fuer(1, 0), 0xdc17a8e5d14d8644);
    }

    #[test]
    fn gueltiger_antrag_passiert() {
        let t = teardown_token_fuer(1, 0);
        assert_eq!(pruefe_exec_antrag(1, 0, t, 0x400_0000, 2), Ok(()));
    }

    #[test]
    fn null_token_ist_kein_egal() {
        assert_eq!(
            pruefe_exec_antrag(1, 0, 0, 0x400_0000, 2),
            Err(ExecAbweisung::KeinToken)
        );
    }

    #[test]
    fn alter_stand_wird_benannt_abgewiesen() {
        let frisch = teardown_token_fuer(1, 1);
        let alt = teardown_token_fuer(1, 0);
        assert_ne!(frisch, alt);
        assert_eq!(
            pruefe_exec_antrag(1, 1, alt, 0x400_0000, 2),
            Err(ExecAbweisung::AlterStand)
        );
    }

    #[test]
    fn leeres_und_zu_grosses_image_fallen_vor_dem_rueckbau() {
        let t = teardown_token_fuer(1, 0);
        assert_eq!(
            pruefe_exec_antrag(1, 0, t, 0x400_0000, 0),
            Err(ExecAbweisung::SegmentZahl)
        );
        assert_eq!(
            pruefe_exec_antrag(1, 0, t, 0x400_0000, 65),
            Err(ExecAbweisung::SegmentZahl)
        );
    }

    #[test]
    fn kernel_eintrag_ist_kein_exec_ziel() {
        let t = teardown_token_fuer(1, 0);
        assert_eq!(pruefe_exec_antrag(1, 0, t, 0, 2), Err(ExecAbweisung::SchlechterEintrag));
        assert_eq!(
            pruefe_exec_antrag(1, 0, t, USER_VA_TOP, 2),
            Err(ExecAbweisung::SchlechterEintrag)
        );
    }
}
