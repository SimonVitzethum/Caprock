//! FORK/EXEC-Planlogik des Ladepfads — rein, zaehlbar, host-testbar.
//!
//! Was hier steht: die Entscheidungen VOR der ersten Allokation (Snapshot-Plan pruefen,
//! EXEC-Token falten und vergleichen), abhaengigkeitsfrei (`core` nur, kein `unsafe`, keine
//! Locks). Was hier ausdruecklich NICHT steht: Kopieren, Mappen, Toeten, Binden — das liegt in
//! `system.rs` (`dispatch_fork`/`dispatch_exec`), weil nur der Kernel weiss, ob ein Geraet eine
//! Region erreicht, was eine VSpace enthaelt und wo das Archiv liegt.
//!
//! Die Teilung ist dieselbe wie bei `caprock-loader` gegenueber `loader.rs`: die reine Pruefung
//! ist kanonisch genau einmal hier, der privilegierte Vollzug ruft sie auf, statt sie
//! nachzubauen. Eine Pruefung an zwei Stellen ist zwei Pruefungen.
//!
//! Kanonisch sind die Kosten- und Token-Funktionen in `caprock-loader` (`snapshot`, `exec`);
//! was hier als Zahl steht, ist die Abbildung auf ABI-Codes plus die `max_len`-Deckelung aus
//! `caprock-abi::fork`. Driftet eine Seite, faellt ein Test (s. die Anker-Tests unten).

use caprock_abi::result;
use caprock_loader::exec::{self, ExecAbweisung};
use caprock_loader::snapshot::{self, SnapAbweisung, SnapSeg};

/// Warum ein FORK-Plan abgelehnt wurde — jeder Ausgang mit eigenem Namen (D11: ein
/// Sammelcode ist als Diagnose wertlos).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkFehler {
    /// Keine Region genannt (ein leeres Kind ist kein Kind).
    Leer,
    /// Mehr Stuecke, als die Teardown-Buchhaltung fasst.
    ZuViele,
    /// Krumme Region (nicht seitenausgerichtet, Laenge 0 oder kein Vielfaches).
    Geometrie,
    /// Zwei Regionen ueberlappen (doppelt kopiert = doppelt gezaehlt).
    Ueberlappung,
    /// Summe ueber der Schranke (volle Kopie oder Absage, nie Kuerzung).
    ZuGross,
}

/// Einen [`ForkFehler`] auf den ABI-Code abbilden — genau einmal hier, nicht je Aufrufer.
///
/// `Leer` ist `ERR_BADSYS`, nicht `ERR_NOPD`: der Dispatch weist die leere Quellmenge vorher
/// als `ERR_NOPD` ab (keine Frames registriert = keine PD-Aussage); kommt ein leerer Plan bis
/// hierher, ist das ein innerer Widerspruch, kein fehlendes Subjekt. `Geometrie` und
/// `Ueberlappung` ebenso: die Liste stammt aus der kernel-eigenen Buchhaltung
/// (`loaded_snapshot`), und was der Kernel selbst gesammelt hat, ist per Bau
/// seitenausgerichtet und disjunkt — ein Treffer hier heisst „die Buchhaltung luegt".
pub fn fork_fehler_code(f: ForkFehler) -> u64 {
    match f {
        ForkFehler::Leer => result::ERR_BADSYS,
        ForkFehler::ZuViele => result::ERR_SNAPSHOT_LIMIT,
        ForkFehler::Geometrie => result::ERR_BADSYS,
        ForkFehler::Ueberlappung => result::ERR_BADSYS,
        ForkFehler::ZuGross => result::ERR_SNAPSHOT_LIMIT,
    }
}

fn snap_code(a: SnapAbweisung) -> ForkFehler {
    match a {
        SnapAbweisung::Leer => ForkFehler::Leer,
        SnapAbweisung::ZuVieleStuecke => ForkFehler::ZuViele,
        SnapAbweisung::SchlechteGeometrie => ForkFehler::Geometrie,
        SnapAbweisung::Ueberlappung => ForkFehler::Ueberlappung,
        SnapAbweisung::ZuGross => ForkFehler::ZuGross,
    }
}

/// Einen FORK-Plan aus der VA-Liste der Quell-PD pruefen — VOR der ersten Allokation.
///
/// `vas` = `(VA, Laenge)` je zu kopierendem Stueck (aus `loaded_snapshot`, beim Mappen
/// gesammelt, nicht geraten). `max_len` = Deckel des Aufrufers (`0` = ganze Flaeche).
/// `plan` = Kratzflaeche des Aufrufers, mindestens `vas.len()` gross.
///
/// Gibt `(Stueckzahl, Kopierlaenge)`. Keine Kuerzung: passt die Summe nicht unter `max_len`,
/// kommt `ZuGross` — ein halb kopierter Kind-Raum ist ein Korruptionspfad, kein Komfort.
pub fn fork_plan_pruefen(
    vas: &[(u64, u64)],
    max_len: u64,
    plan: &mut [SnapSeg],
) -> Result<(usize, u64), ForkFehler> {
    if vas.is_empty() {
        return Err(ForkFehler::Leer);
    }
    if vas.len() > snapshot::MAX_SEGS || vas.len() > plan.len() {
        return Err(ForkFehler::ZuViele);
    }
    let mut i = 0;
    while i < vas.len() {
        plan[i] = SnapSeg { va: vas[i].0, len: vas[i].1 };
        i += 1;
    }
    let summe = snapshot::pruefe_snapshot_plan(&plan[..vas.len()]).map_err(snap_code)?;
    if max_len != 0 && summe > max_len {
        return Err(ForkFehler::ZuGross);
    }
    Ok((vas.len(), summe))
}

/// Einen EXEC-Antrag gegen `(program_id, Epoche)` pruefen — VOR dem ersten Rueckbau.
///
/// Kanonisch ist `caprock-loader::exec::pruefe_exec_antrag`; was hier steht, ist nur die
/// Abbildung auf ABI-Codes: `KeinToken`/`AlterStand` = `ERR_STALE_TOKEN` (wer das Token nicht
/// nennt oder das von gestern bringt, bekommt keinen halb geraeumten Zustand, sondern eine
/// Absage), `SegmentZahl` = `ERR_SNAPSHOT_LIMIT` (die Schranke der Teardown-Buchhaltung, wie
/// beim Fork), `SchlechterEintrag` = `ERR_BADCAP` (kein Kernel-Sprung ueber EXEC).
pub fn exec_antrag_pruefen(
    program_id: u32,
    epoche: u32,
    token: u64,
    eintrag: u64,
    nseg: usize,
) -> Result<(), u64> {
    exec::pruefe_exec_antrag(program_id, epoche, token, eintrag, nseg).map_err(|a| match a {
        ExecAbweisung::KeinToken | ExecAbweisung::AlterStand => result::ERR_STALE_TOKEN,
        ExecAbweisung::SegmentZahl => result::ERR_SNAPSHOT_LIMIT,
        ExecAbweisung::SchlechterEintrag => result::ERR_BADCAP,
    })
}

/// Das Teardown-Token zu `(program_id, Epoche)` — Spiegel, kanonisch im Loader.
pub fn teardown_token(program_id: u32, epoche: u32) -> u64 {
    exec::teardown_token_fuer(program_id, epoche)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zwei_stuecke_passieren_mit_summe() {
        let vas = [(0x400_0000u64, 4096u64), (0x400_1000u64, 4096u64)];
        let mut plan = [SnapSeg { va: 0, len: 0 }; 4];
        assert_eq!(fork_plan_pruefen(&vas, 0, &mut plan), Ok((2, 8192)));
    }

    #[test]
    fn leer_ist_kein_plan() {
        let mut plan = [SnapSeg { va: 0, len: 0 }; 4];
        assert_eq!(fork_plan_pruefen(&[], 0, &mut plan), Err(ForkFehler::Leer));
        assert_eq!(fork_fehler_code(ForkFehler::Leer), result::ERR_BADSYS);
    }

    #[test]
    fn deckel_kuerzt_nicht_sondern_weist_ab() {
        let vas = [(0x400_0000u64, 8192u64)];
        let mut plan = [SnapSeg { va: 0, len: 0 }; 4];
        assert_eq!(fork_plan_pruefen(&vas, 4096, &mut plan), Err(ForkFehler::ZuGross));
        assert_eq!(fork_fehler_code(ForkFehler::ZuGross), result::ERR_SNAPSHOT_LIMIT);
        // Derselbe Plan ohne Deckel passiert — der Deckel war der Grund, nicht die Form.
        assert_eq!(fork_plan_pruefen(&vas, 0, &mut plan), Ok((1, 8192)));
    }

    #[test]
    fn krumme_region_faellt_vor_der_allokation() {
        let vas = [(0x400_0800u64, 4096u64)];
        let mut plan = [SnapSeg { va: 0, len: 0 }; 4];
        assert_eq!(fork_plan_pruefen(&vas, 0, &mut plan), Err(ForkFehler::Geometrie));
    }

    #[test]
    fn ueberlappung_wird_benannt() {
        let vas = [(0x400_0000u64, 8192u64), (0x400_1000u64, 4096u64)];
        let mut plan = [SnapSeg { va: 0, len: 0 }; 4];
        assert_eq!(fork_plan_pruefen(&vas, 0, &mut plan), Err(ForkFehler::Ueberlappung));
    }

    #[test]
    fn zu_viele_stuecke_fallen_und_zu_kleiner_puffer_auch() {
        let vas = [(0x400_0000u64, 4096u64); 65];
        let mut plan = [SnapSeg { va: 0, len: 0 }; 65];
        assert_eq!(fork_plan_pruefen(&vas, 0, &mut plan), Err(ForkFehler::ZuViele));
        let vas = [(0x400_0000u64, 4096u64); 2];
        let mut klein = [SnapSeg { va: 0, len: 0 }; 1];
        assert_eq!(fork_plan_pruefen(&vas, 0, &mut klein), Err(ForkFehler::ZuViele));
    }

    #[test]
    fn token_ist_stabil_und_null_ist_kein_egal() {
        let t = teardown_token(7, 3);
        assert_eq!(t, teardown_token(7, 3));
        assert_ne!(t, 0);
        assert_eq!(exec_antrag_pruefen(7, 3, 0, 0x400_0000, 2), Err(result::ERR_STALE_TOKEN));
        assert_eq!(
            exec_antrag_pruefen(7, 4, t, 0x400_0000, 2),
            Err(result::ERR_STALE_TOKEN)
        );
    }

    #[test]
    fn gueltiger_exec_antrag_passiert() {
        let t = teardown_token(1, 0);
        assert_eq!(exec_antrag_pruefen(1, 0, t, 0x400_0000, 2), Ok(()));
    }

    #[test]
    fn exec_schranken_sind_benannt() {
        let t = teardown_token(1, 0);
        assert_eq!(
            exec_antrag_pruefen(1, 0, t, 0x400_0000, 65),
            Err(result::ERR_SNAPSHOT_LIMIT)
        );
        assert_eq!(
            exec_antrag_pruefen(1, 0, t, 0, 2),
            Err(result::ERR_BADCAP)
        );
    }

    #[test]
    fn nummern_und_codes_liegen_auf_der_abi() {
        assert_eq!(caprock_abi::sys::FORK_SNAPSHOT, 31);
        assert_eq!(caprock_abi::sys::EXEC_REPLACE, 32);
        assert_eq!(caprock_abi::fork::SNAPSHOT_MAX_BYTES, snapshot::SNAPSHOT_MAX_BYTES);
        assert_eq!(snapshot::MAX_SEGS, exec::MAX_SEGS);
    }
}
