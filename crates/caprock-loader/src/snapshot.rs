//! FORK-Snapshot: Adressraum-Schnappschuss als VOLLE KOPIE (Phase 1, ehrlich).
//!
//! Reine Plan-Pruefung (`core` nur, kein `unsafe`): welche Seiten kopiert werden, ob sie in
//! die Schranke passen, was es kostet. Das Kopieren selbst (Quell-Seitentabellen laufen
//! via `vspace_resolve`, Ziel-Seiten allozieren + mappen — dieselbe Ordnung wie
//! `load_into_pd_mit`) liegt im Kernel (Patch-Text in `caprock-microkit::proc`).
//!
//! ## Kein COW-Versprechen
//!
//! COW ist Phase 2 und braucht zwei Dinge, die heute FEHLEN (benannt, nicht verschwiegen):
//!
//! 1. **Dirty-Tracking**: kein Write-Protect-Bit wird je gesetzt; keine Tabelle traegt
//!    „kopiert-beim-Schreiben". Ohne das ist „geteilt" nicht „beobachtet-geteilt",
//!    sondern schlicht geteilt — ein Geschwister beschreibt die Seite des anderen.
//! 2. **Seitenfehler-Pfad, der nachlaedt statt beendet**: Faults beenden heute den Thread
//!    (`fault_dispatch` ohne Bindung = nativer Pfad = Ende). Ein COW-Fault mueste
//!    alloziieren + kopieren + fortsetzen — ein Pfad, den es nicht gibt.
//!
//! Wer COW ohne beides verspricht, teilt Seiten still statt zu kopieren. Phase 1 kopiert
//! deshalb alles — teuer, aber zaehlbar.
//!
//! ## Schranken
//!
//! `SNAPSHOT_MAX_BYTES` (8 MiB, Spiegel von `caprock-abi::fork`) deckelt die Kopierlaenge;
//! `MAX_SEGS` (64, Spiegel der Teardown-Buchhaltung) die Stueckzahl. Darueber gibt es
//! `ERR_SNAPSHOT_LIMIT`, nicht eine gekuerzte Kopie: ein halb adressierter Kind-Raum ist
//! ein Korruptionspfad, kein Komfort.

/// Obergrenze der Kopierlaenge (Spiegel von `caprock-abi::fork::SNAPSHOT_MAX_BYTES`).
pub const SNAPSHOT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Stueckzahl gegen die Teardown-Buchhaltung.
pub const MAX_SEGS: usize = 64;
/// Seitengroesse.
pub const PAGE: u64 = 4096;

/// Eine zu kopierende Seite/Region (VA im Quell-Raum, Laenge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapSeg {
    pub va: u64,
    pub len: u64,
}

/// Warum ein Snapshot-Plan abgelehnt wurde.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapAbweisung {
    /// Keine Region genannt (ein leeres Kind ist kein Kind).
    Leer,
    /// Mehr Stuecke als die Buchhaltung fasst.
    ZuVieleStuecke,
    /// Leere/krumme Region (nicht seitenausgerichtet, Laenge 0 oder kein Vielfaches).
    SchlechteGeometrie,
    /// Zwei Regionen ueberlappen (doppelt kopiert = doppelt gezaehlt).
    Ueberlappung,
    /// Summe ueber `SNAPSHOT_MAX_BYTES` (volle Kopie, keine Kuerzung).
    ZuGross,
}

/// Pruefe einen Snapshot-Plan VOR der ersten Allokation. Gibt die Kopierlaenge zurueck.
pub fn pruefe_snapshot_plan(segs: &[SnapSeg]) -> Result<u64, SnapAbweisung> {
    if segs.is_empty() {
        return Err(SnapAbweisung::Leer);
    }
    if segs.len() > MAX_SEGS {
        return Err(SnapAbweisung::ZuVieleStuecke);
    }
    let mut sum: u64 = 0;
    for s in segs {
        if s.len == 0 || s.va % PAGE != 0 || s.len % PAGE != 0 {
            return Err(SnapAbweisung::SchlechteGeometrie);
        }
        sum = sum.checked_add(s.len).ok_or(SnapAbweisung::ZuGross)?;
    }
    // Paarweise Disjunktheit (ein doppeltes Stueck waere doppelt gezaehlt + kopiert).
    let mut i = 0;
    while i < segs.len() {
        let mut j = i + 1;
        while j < segs.len() {
            let (a, b) = (segs[i], segs[j]);
            if a.va < b.va.saturating_add(b.len) && b.va < a.va.saturating_add(a.len) {
                return Err(SnapAbweisung::Ueberlappung);
            }
            j += 1;
        }
        i += 1;
    }
    if sum > SNAPSHOT_MAX_BYTES {
        return Err(SnapAbweisung::ZuGross);
    }
    Ok(sum)
}

/// Kopierkosten in Seitenrahmen (Daten) + Tabellen (1 je ≤512 Seiten + 1).
pub fn snapshot_kosten(sum_bytes: u64) -> (u64, u64) {
    let daten = sum_bytes.div_ceil(PAGE);
    let tabellen = daten.div_ceil(512).max(1) + 1;
    (daten, tabellen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zwei_seiten_werden_gezaehlt() {
        let segs = [SnapSeg { va: 0x400_0000, len: 4096 }, SnapSeg { va: 0x400_1000, len: 4096 }];
        assert_eq!(pruefe_snapshot_plan(&segs), Ok(8192));
        assert_eq!(snapshot_kosten(8192), (2, 2));
    }

    #[test]
    fn leer_ist_kein_plan() {
        assert_eq!(pruefe_snapshot_plan(&[]), Err(SnapAbweisung::Leer));
    }

    #[test]
    fn krumme_region_faellt_vor_der_allokation() {
        let segs = [SnapSeg { va: 0x400_0800, len: 4096 }];
        assert_eq!(pruefe_snapshot_plan(&segs), Err(SnapAbweisung::SchlechteGeometrie));
        let segs = [SnapSeg { va: 0x400_0000, len: 1000 }];
        assert_eq!(pruefe_snapshot_plan(&segs), Err(SnapAbweisung::SchlechteGeometrie));
    }

    #[test]
    fn ueberlappung_wird_benannt() {
        let segs = [
            SnapSeg { va: 0x400_0000, len: 8192 },
            SnapSeg { va: 0x400_1000, len: 4096 },
        ];
        assert_eq!(pruefe_snapshot_plan(&segs), Err(SnapAbweisung::Ueberlappung));
    }

    #[test]
    fn zu_gross_wird_abgewiesen_nicht_gekuerzt() {
        let segs = [SnapSeg { va: 0x400_0000, len: SNAPSHOT_MAX_BYTES + 4096 }];
        assert_eq!(pruefe_snapshot_plan(&segs), Err(SnapAbweisung::ZuGross));
    }

    #[test]
    fn zu_viele_stuecke_fallen() {
        let segs = [SnapSeg { va: 0x400_0000, len: 4096 }; 65];
        assert_eq!(pruefe_snapshot_plan(&segs), Err(SnapAbweisung::ZuVieleStuecke));
    }
}
