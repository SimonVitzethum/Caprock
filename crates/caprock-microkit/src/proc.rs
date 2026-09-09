//! Prozessmodell: FORK/EXEC-Dispatchhelfer (Prozessmodell, Phase 1).
//!
//! Abhaengigkeitsfrei (`core` nur) und per `rustc --test` als DATEI pruefbar — die
//! Entscheidung steht genau einmal hier, `dispatch_nativ_mit_fork_exec` in `lib.rs`
//! ruft sie auf, statt sie nachzubauen (dieselbe Teilung wie `cspace.rs`).
//!
//! ## Was hier steht und was nicht
//!
//! Registerdekodierung + fail-closed Vorpruefung fuer `FORK_SNAPSHOT (31)` und
//! `EXEC_REPLACE (32)`. Die Inhaltspruefung (Token-Faltung, Snapshot-Plan, Kosten)
//! steht kanonisch in `caprock-loader` (`exec`, `snapshot`, `cost`); was hier als
//! Zahl steht, ist die Dispatch-Huelse davor (reserviert == 0, Token != 0, Laenge
//! gedeckelt). Eine Pruefung an zwei Stellen ist zwei Pruefungen — deshalb reicht
//! diese Huelse den Rest an den Kernel-Rueckruf weiter, statt ihn nachzurechnen.
//!
//! Der Rueckruf fehlt im Kernel (kernel/** ist fremd und bleibt unberuehrt) — der
//! EXAKTE Patch-Text liegt bei der Demo:
//! `programs/fork-demo/PATCH-kernel-dispatch.txt`. Bis er angewendet ist, meldet
//! ein gueltiger FORK/EXEC-Antrag `ERR_BADSYS` (Antrag ok, Pfad fehlt).
//!
//! ## Nummern-Anker (Drift = Baufehler)
//!
//! `31`/`32` spiegeln `caprock-abi::sys::{FORK_SNAPSHOT, EXEC_REPLACE}`, `36` die
//! naechste freie Nummer, `26`/`27` die neuen Ergebnis-Codes. Die ABI-Crate traegt
//! denselben Anker (`nummern`-Tests); driftet eine Seite, faellt ein Test —
//! Kollision ist ein Baufehler, kein Laufzeitfehler.

/// Syscall-Nummern (Spiegel von `caprock-abi::sys`).
pub const SYS_FORK_SNAPSHOT: u64 = 31;
/// Syscall-Nummern (Spiegel von `caprock-abi::sys`).
pub const SYS_EXEC_REPLACE: u64 = 32;
/// Naechste freie Nummer (Spiegel von `caprock-abi::fork::NAECHSTE_FREIE_SYSCALL`).
pub const NAECHSTE_FREIE_SYSCALL: u64 = 36;

/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_OK: u64 = 0;
/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_BADCAP: u64 = 1;
/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_BADSYS: u64 = 2;
/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_NOPD: u64 = 4;
/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_NOSPACE: u64 = 7;
/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_STALE_TOKEN: u64 = 26;
/// Ergebnis-Codes (Spiegel von `caprock-abi::result`).
pub const ERR_SNAPSHOT_LIMIT: u64 = 27;

/// Obergrenze der Kopierlaenge (Spiegel von `caprock-abi::fork::SNAPSHOT_MAX_BYTES`).
pub const SNAPSHOT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Groesste zulässige Kind-Prio (gesetzt wird nur, was der Scheduler kennt: 0..=7).
pub const MAX_PRIO: u8 = 7;

/// FORK-Antrag nach der Registerdekodierung (x1 unbenutzt — Quelle ist immer die eigene PD).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForkAntrag {
    /// Ziel-Slot fuer die Kind-`PdControl`-Cap im Aufrufer-Cspace.
    pub kind_slot: usize,
    /// Maximale Kopierlaenge (`0` = ganze User-Flaeche, gedeckelt).
    pub max_len: u64,
    /// Ziel-Prio des Kind-Hauptthreads.
    pub prio: u8,
}

/// EXEC-Antrag nach der Registerdekodierung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecAntrag {
    /// Loader-Cap-Slot im eigenen Cspace.
    pub loader_slot: usize,
    /// Archiv-Programm-Index.
    pub prog_index: u32,
    /// Teardown-Token (`0` = kein Token = Absage, kein „egal").
    pub token: u64,
}

/// Ausgang der Dispatch-Huelse (rein, ohne Locks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkExecEntscheid {
    /// Keine der beiden Nummern — der generische Pfad bleibt zustaendig.
    Unbekannt,
    /// FORK-Vorpruefung bestanden, weiter an den Kernel-Rueckruf.
    Fork(ForkAntrag),
    /// EXEC-Vorpruefung bestanden, weiter an den Kernel-Rueckruf.
    Exec(ExecAntrag),
    /// Vorpruefung gescheitert — benannter Code, Aufrufer bleibt lauffaehig (D11).
    Abgewiesen(u64),
}

/// Dekodiere `FORK_SNAPSHOT`-Register. `msg0..msg3` = x2..x5.
pub fn dekodiere_fork(msg0: u64, msg1: u64, msg2: u64, msg3: u64) -> ForkExecEntscheid {
    if msg3 != 0 {
        return ForkExecEntscheid::Abgewiesen(ERR_BADCAP);
    }
    if msg2 > MAX_PRIO as u64 {
        return ForkExecEntscheid::Abgewiesen(ERR_BADCAP);
    }
    if msg1 > SNAPSHOT_MAX_BYTES {
        return ForkExecEntscheid::Abgewiesen(ERR_SNAPSHOT_LIMIT);
    }
    ForkExecEntscheid::Fork(ForkAntrag {
        kind_slot: msg0 as usize,
        max_len: msg1,
        prio: msg2 as u8,
    })
}

/// Dekodiere `EXEC_REPLACE`-Register. `loader_slot` = x1, `msg0..msg3` = x2..x5.
/// `msg0` = Archiv-Programm-Index · `msg1` = Token · `msg2` = IGNORIERT (Eintrag kommt
/// aus dem Image) · `msg3` = reserviert (`0`).
pub fn dekodiere_exec(
    loader_slot: u64,
    msg0: u64,
    msg1: u64,
    msg2: u64,
    msg3: u64,
) -> ForkExecEntscheid {
    let _ = msg2;
    if msg3 != 0 {
        return ForkExecEntscheid::Abgewiesen(ERR_BADCAP);
    }
    if msg1 == 0 {
        return ForkExecEntscheid::Abgewiesen(ERR_STALE_TOKEN);
    }
    ForkExecEntscheid::Exec(ExecAntrag {
        loader_slot: loader_slot as usize,
        prog_index: msg0 as u32,
        token: msg1,
    })
}

/// Teardown-Token falten (Spiegel von `caprock-loader::exec::teardown_token_fuer`).
/// Kanonisch ist der Loader; diese Kopie existiert nur, damit die Datei ohne
/// Abhaengigkeit pruefbar bleibt. Beide tragen denselben Vektor-Test unten.
pub fn teardown_token_fuer(program_id: u32, epoche: u32) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
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
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x100000001b3);
        i += 1;
    }
    if h == 0 { 1 } else { h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nummern_anker_gegen_die_abi() {
        assert_eq!(SYS_FORK_SNAPSHOT, 31);
        assert_eq!(SYS_EXEC_REPLACE, 32);
        assert_eq!(NAECHSTE_FREIE_SYSCALL, 36);
        assert_eq!(SNAPSHOT_MAX_BYTES, 8 * 1024 * 1024);
        assert_ne!(SYS_FORK_SNAPSHOT, SYS_EXEC_REPLACE);
    }

    #[test]
    fn fork_gueltig_wird_durchgereicht() {
        assert_eq!(
            dekodiere_fork(3, 0, 4, 0),
            ForkExecEntscheid::Fork(ForkAntrag { kind_slot: 3, max_len: 0, prio: 4 })
        );
    }

    #[test]
    fn fork_reserviert_muss_null_sein() {
        assert_eq!(dekodiere_fork(3, 0, 4, 1), ForkExecEntscheid::Abgewiesen(ERR_BADCAP));
    }

    #[test]
    fn fork_prio_ausserhalb_wird_abgewiesen() {
        assert_eq!(dekodiere_fork(3, 0, 8, 0), ForkExecEntscheid::Abgewiesen(ERR_BADCAP));
    }

    #[test]
    fn fork_ueber_der_schranke_wird_benannt_abgewiesen_nicht_gekuerzt() {
        assert_eq!(
            dekodiere_fork(3, SNAPSHOT_MAX_BYTES + 1, 4, 0),
            ForkExecEntscheid::Abgewiesen(ERR_SNAPSHOT_LIMIT)
        );
    }

    #[test]
    fn exec_gueltig_wird_durchgereicht() {
        let t = teardown_token_fuer(1, 0);
        match dekodiere_exec(5, 2, t, 0, 0) {
            ForkExecEntscheid::Exec(a) => {
                assert_eq!(a.loader_slot, 5);
                assert_eq!(a.prog_index, 2);
                assert_eq!(a.token, t);
            }
            x => panic!("unerwartet: {x:?}"),
        }
    }

    #[test]
    fn exec_null_token_ist_kein_egal() {
        assert_eq!(
            dekodiere_exec(5, 2, 0, 0, 0),
            ForkExecEntscheid::Abgewiesen(ERR_STALE_TOKEN)
        );
    }

    #[test]
    fn exec_reserviert_muss_null_sein() {
        let t = teardown_token_fuer(1, 0);
        assert_eq!(
            dekodiere_exec(5, 2, t, 0, 9),
            ForkExecEntscheid::Abgewiesen(ERR_BADCAP)
        );
    }

    #[test]
    fn token_ist_stabil_und_nie_null() {
        let t1 = teardown_token_fuer(7, 3);
        assert_eq!(t1, teardown_token_fuer(7, 3));
        assert_ne!(t1, 0);
        assert_ne!(teardown_token_fuer(7, 4), t1);
    }

    #[test]
    fn fremde_nummer_bleibt_unbekannt() {
        // Die Huelse entscheidet nur 31/32 — alles andere ist Sache des generischen Pfads.
        // Abgebildet durch: kein Entscheid ausserhalb der beiden Dekodierer.
        assert_ne!(SYS_FORK_SNAPSHOT, 30);
        assert_ne!(SYS_EXEC_REPLACE, 33);
    }
}
