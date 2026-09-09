//! FORK/EXEC-Demo (Host): derselbe Antrag, den eine PD an den Kernel stellte.
//!
//! Kein OS-Bau: Diese Demo laeuft auf dem Host und faehrt die REINEN Pruefungen,
//! die der Kernel-Pfad vor dem ersten Seiteneffekt faehrt (`caprock-loader::cost`,
//! `::exec`, `::snapshot`). Was hier `OK` meldet, wuerde der Dispatch an den
//! Rueckruf weiterreichen; was hier abgewiesen wird, kaeme im Kernel als benannter
//! Code zurueck — der Aufrufer bleibt in beiden Faellen lauffaehig (D11).
//!
//! Marker (fuer die Abnahme von Hand, nicht fuer eine Suite — `test-qemu*.sh` und
//! `kernel/**` sind fremder Besitz und werden hier NICHT angefasst):
//!
//! ```text
//! fork-demo : SPAWN fahrbar (arena-Beleg s. Kernel-Suite `arena : ALL PASS`)
//! fork-demo : FORK-Plan OK, 2 Segmente, 8192 Bytes
//! fork-demo : EXEC-Antrag OK, Token passt
//! fork-demo : PD-Kosten, 3 Stuecke, 7 Seiten
//! ```

use caprock_loader::{cost, exec, snapshot};

fn main() {
    // (a) SPAWN fahrbar: der Beleg steht in der Kernel-Suite (`arena : ALL PASS`,
    // `spawnarena::messen` — erster Lauf von SYS_SPAWN ueberhaupt, s. threads/mod.rs).
    // Host-seitig ist SPAWN durch `caprock-cap::spawncheck` gedeckt (eigene Host-Tests
    // dort); eine PD aus einer PD erzeugt der LADEPFAD (`SYS_LOAD`), dessen Kosten
    // unten stehen. QEMU-Suite-Eintraege sind fremder Besitz (B) und bleiben unberuehrt.
    println!("fork-demo : SPAWN fahrbar (arena-Beleg s. Kernel-Suite `arena : ALL PASS`)");

    // (c) FORK-Snapshot: volle Kopie, zwei Seiten.
    let segs =
        [snapshot::SnapSeg { va: 0x400_0000, len: 4096 }, snapshot::SnapSeg { va: 0x400_1000, len: 4096 }];
    let sum = snapshot::pruefe_snapshot_plan(&segs).expect("FORK-Plan muss passen");
    let (daten, tabellen) = snapshot::snapshot_kosten(sum);
    println!("fork-demo : FORK-Plan OK, {} Segmente, {sum} Bytes ({daten} Daten + {tabellen} Tabellen)", segs.len());

    // (b) EXEC-Replace: Token bindet Antrag an Stand.
    let token = exec::teardown_token_fuer(1, 0);
    exec::pruefe_exec_antrag(1, 0, token, 0x400_0000, 2).expect("EXEC-Antrag muss passen");
    println!("fork-demo : EXEC-Antrag OK, Token passt");

    // Was eine PD-Erzeugung kostet (Lesart von `load_into_pd_mit`, s. cost-Modul).
    let k = cost::pd_kosten(&[8192, 4096]).expect("Kosten muessen passen");
    println!(
        "fork-demo : PD-Kosten, {} Stuecke, {} Seiten ({} Tabellen)",
        k.stuecke, k.daten_seiten, k.tabellen_rahmen
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_plaene_sind_gueltig() {
        let segs =
            [snapshot::SnapSeg { va: 0x400_0000, len: 4096 }, snapshot::SnapSeg { va: 0x400_1000, len: 4096 }];
        assert_eq!(snapshot::pruefe_snapshot_plan(&segs), Ok(8192));
        let t = exec::teardown_token_fuer(1, 0);
        assert_eq!(exec::pruefe_exec_antrag(1, 0, t, 0x400_0000, 2), Ok(()));
        assert!(cost::pd_kosten(&[8192, 4096]).is_some());
    }

    #[test]
    fn demo_absagen_sind_benannt() {
        // Null-Token ist kein „egal", Ueberlappung keine Kopie, 65 Segmente kein Kind.
        assert_eq!(
            exec::pruefe_exec_antrag(1, 0, 0, 0x400_0000, 2),
            Err(exec::ExecAbweisung::KeinToken)
        );
        let ueber = [
            snapshot::SnapSeg { va: 0x400_0000, len: 8192 },
            snapshot::SnapSeg { va: 0x400_1000, len: 4096 },
        ];
        assert_eq!(
            snapshot::pruefe_snapshot_plan(&ueber),
            Err(snapshot::SnapAbweisung::Ueberlappung)
        );
        assert_eq!(cost::pd_kosten(&[4096u64; 65]), None);
    }
}
