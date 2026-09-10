//! WASI-Importe der Stufe 2a (Strang 8/WASM-WASI).
//!
//! Norm: `programs/mem-server/SPEZIFIKATION.md`, Anhang Stufe 2a, Punkt 3
//! (Negativliste). Genau vier Importe aus genau einem Modul
//! (`wasi_snapshot_preview1`, s. [`WASI_MODUL`][super::WASI_MODUL]) sind erlaubt:
//! `fd_write` (nur fd 1/2 an die Log-Senke), `clock_time_get` (nur monoton),
//! `proc_exit` (beendet die PD) und `random_get` (Host-Zufall per Cap).
//! Alles andere weist die Tabelle ab — per [`pruefe`], das an
//! [`pruefe_import`][super::pruefe_import] aus `lib.rs` delegiert (keine
//! Zweitliste hier, kein Stub-Linken).
//!
//! Regeln, die hier gelten:
//! - Kein Speicherzugriff ausserhalb der [`Speicher`]-Methoden (`laden_*`,
//!   `speichern_*`, `kopieren_*`): kein Index, kein Slice, kein Zeiger.
//! - Kein echtes fd: `fd_write` schreibt an das Trait [`LogSenke`], die Zeit
//!   kommt aus [`Uhr`], der Zufall aus [`Zufall`] — der Host-Test steckt
//!   Fakes an (kein OS, kein `unsafe`, keine Deps, `no_std`-faehig).
//! - Jede Absage ist benannt ([`WasiFehler`]); kein Panic, kein stiller Fallback.
//!
//! ## Abweichung von `lib.rs` (Patch-Text an Strang 6/WASM-ENG)
//!
//! `Trap` in `lib.rs` kennt kein `Beendet`: [`proc_exit`] gibt daher
//! [`WasiEnde::Beendet`] zurueck (kein Fehler, sondern Terminierung mit Kode).
//! Vorschlag an ENG: `Trap::Beendet(u32)` aufnehmen (Name `"Beendet"`); Fehler-
//! Abbildung entfaellt — ein Exit ist kein [`WasmFehler`], der Kode reist per
//! REPLY-Wort + Log-Zeile an den Aufrufer (Anhang, offene Frage 3). Ebenso
//! kennt `WasmFehler` kein `FdAbgelehnt`/`UhrAbgelehnt` (Anhang Punkt 3 nennt
//! sie als `WasiFehler::*`): Sie leben hier als [`WasiFehler`] und bilden auf
//! `ImportAbgelehnt`/`OobZugriff` ab ([`WasiFehler::als_wasm_fehler`]), bis ENG
//! sie bei Bedarf uebernimmt. `lib.rs` selbst bleibt unangetastet (Strang 6).

use super::mem::Speicher;
use super::{Trap, WASI_MODUL, WasmFehler, pruefe_import};

/// WASI-Kennziffer der monotonen Uhr (`CLOCK_MONOTONIC` in `preview1`:
/// 0 = realtime, 1 = monoton, 2/3 = CPU-Zeit). Nur diese ist erlaubt.
pub const UHR_MONOTON: u32 = 1;
/// Erlaubte Dateikennziffern: 1 (stdout) und 2 (stderr) → beide an die Log-Senke.
pub const FD_LOG_MIN: u32 = 1;
/// Obere erlaubte Dateikennziffer (s. oben).
pub const FD_LOG_MAX: u32 = 2;
/// Breite eines `iovec`-Eintrags im Linearspeicher: `ptr: u32, len: u32` (LE).
pub const IOV_BREITE: u64 = 8;
/// Blockgroesse fuers Durchreichen ohne Alloc (kein Heap in der PD).
const BLOCK: usize = 256;

/// Log-Senke des Hosts (`fd_write` auf fd 1/2 landet hier, nie auf einem fd).
pub trait LogSenke {
    /// Empfaengt die Log-Bytes eines `fd_write` (darf stueckeln).
    fn log(&mut self, bytes: &[u8]);
}

/// Monotone Uhr des Hosts (`clock_time_get` fragt nur diese).
pub trait Uhr {
    /// Nanosekunden seit einem beliebigen, aber monotonen Ursprung.
    fn nanos(&self) -> u64;
}

/// Host-Zufall per Cap (`random_get`; kein deterministischer Fallback).
pub trait Zufall {
    /// Fuellt den Puffer mit Zufallsbytes (darf rufen, bis er voll ist).
    fn fuellen(&mut self, buf: &mut [u8]);
}

/// Benannte Absagen der WASI-Schicht (Anhang Punkt 3 + 5: benannt melden).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WasiFehler {
    /// `fd_write` auf fd ≠ 1/2: schreibt nichts (Anhang: BADF, kein Umleiten).
    FdAbgelehnt,
    /// `clock_time_get` mit ID ≠ [`UHR_MONOTON`].
    UhrAbgelehnt,
    /// `iovs`, Nutzlast oder Zielzeiger ausserhalb des Grant-Speichers.
    OobZugriff,
    /// Import steht nicht auf der 2a-Liste (Name im Aufrufkontext, s. [`pruefe`]).
    ImportAbgelehnt,
}

impl WasiFehler {
    /// Der benannte Grund als stabile Zeichenkette (Log + REPLY-Kontext).
    pub fn name(self) -> &'static str {
        match self {
            WasiFehler::FdAbgelehnt => "FdAbgelehnt",
            WasiFehler::UhrAbgelehnt => "UhrAbgelehnt",
            WasiFehler::OobZugriff => "OobZugriff",
            WasiFehler::ImportAbgelehnt => "ImportAbgelehnt",
        }
    }

    /// Derselbe Grund als [`WasmFehler`] (Meldung an den Aufrufer, bis ENG
    /// `FdAbgelehnt`/`UhrAbgelehnt` uebernimmt: fd/Uhr → Import, OOB → OOB).
    pub fn als_wasm_fehler(self) -> WasmFehler {
        match self {
            WasiFehler::FdAbgelehnt | WasiFehler::UhrAbgelehnt | WasiFehler::ImportAbgelehnt => {
                WasmFehler::ImportAbgelehnt
            }
            WasiFehler::OobZugriff => WasmFehler::OobZugriff,
        }
    }

    /// Derselbe Grund als [`Trap`] (PD endet benannt, kein Panic).
    pub fn als_trap(self) -> Trap {
        match self {
            WasiFehler::FdAbgelehnt | WasiFehler::UhrAbgelehnt | WasiFehler::ImportAbgelehnt => {
                Trap::ImportAbgelehnt
            }
            WasiFehler::OobZugriff => Trap::OobZugriff,
        }
    }
}

/// Terminierung per `proc_exit`: kein Fehler, sondern Ende mit Kode
/// (s. Modul-Doku: wandert nach `Trap::Beendet(u32)`, sobald ENG ihn kennt).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WasiEnde {
    /// PD beendet, Kode an Aufrufer (REPLY-Wort + Log-Zeile).
    Beendet(u32),
}

impl WasiEnde {
    /// Der Kode, mit dem die PD endet.
    pub fn kode(self) -> u32 {
        match self {
            WasiEnde::Beendet(k) => k,
        }
    }
}

/// Import erlaubt? Delegiert an `lib.rs` (eine Liste, kein Zweitwissen).
pub fn ist_erlaubt(name: &str) -> bool {
    super::ist_import_erlaubt(WASI_MODUL, name)
}

/// Import pruefen: Erlaubtes geht durch, alles andere scheitert benannt.
/// Der Name steht im Aufrufkontext (Aufrufer loggt ihn); der Fehler bleibt
/// ohne Lifetime (Muster wie [`pruefe_import`][super::pruefe_import]).
pub fn pruefe(name: &str) -> Result<(), WasiFehler> {
    pruefe_import(WASI_MODUL, name).map_err(|_| WasiFehler::ImportAbgelehnt)
}

/// `fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr)`: schreibt die `iovs_len`
/// `iovec`-Eintraege ab `iovs_ptr` (je `ptr: u32, len: u32`, LE) an die
/// Log-Senke — aber nur fuer fd 1/2. Gibt die geschriebenen Bytes zurueck und
/// spiegelt sie nach `nwritten_ptr`. Jede Absage ist benannt, ein abgelehntes
/// fd schreibt nichts (auch nicht `nwritten`).
pub fn fd_write(
    speicher: &mut Speicher<'_>,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    nwritten_ptr: u32,
    senke: &mut impl LogSenke,
) -> Result<u32, WasiFehler> {
    if fd < FD_LOG_MIN || fd > FD_LOG_MAX {
        return Err(WasiFehler::FdAbgelehnt);
    }
    // Eintragszahl mal Eintragsbreite — Ueberlauf waere OOB, kein Wrap.
    let tafel = (iovs_len as u64).checked_mul(IOV_BREITE).ok_or(WasiFehler::OobZugriff)?;
    super::zugriff_pruefen(iovs_ptr as u64, tafel, speicher.len())
        .map_err(|_| WasiFehler::OobZugriff)?;
    let mut geschrieben: u32 = 0;
    let mut i: u32 = 0;
    while i < iovs_len {
        let basis = (i as u64)
            .checked_mul(IOV_BREITE)
            .and_then(|o| (iovs_ptr as u64).checked_add(o))
            .ok_or(WasiFehler::OobZugriff)?;
        // Jeder Eintrag laeuft ueber die Speicher-Methoden (nie roh).
        let ptr = speicher.laden_u32(basis as u32).map_err(|_| WasiFehler::OobZugriff)?;
        let len = speicher
            .laden_u32(basis as u32 + 4)
            .map_err(|_| WasiFehler::OobZugriff)?;
        // Nutzlast blockweise an die Senke (kein Heap, kein Sammelpuffer).
        let mut rest = len;
        let mut ab = ptr;
        while rest > 0 {
            let nimm = if rest as u64 > BLOCK as u64 { BLOCK as u32 } else { rest };
            let mut block = [0u8; BLOCK];
            speicher
                .kopieren_aus(ab, &mut block[..nimm as usize])
                .map_err(|_| WasiFehler::OobZugriff)?;
            senke.log(&block[..nimm as usize]);
            ab = ab.checked_add(nimm).ok_or(WasiFehler::OobZugriff)?;
            rest -= nimm;
            geschrieben = geschrieben.checked_add(nimm).ok_or(WasiFehler::OobZugriff)?;
        }
        i += 1;
    }
    speicher
        .speichern_u32(nwritten_ptr, geschrieben)
        .map_err(|_| WasiFehler::OobZugriff)?;
    Ok(geschrieben)
}

/// `clock_time_get(id, _praezision, ziel)`: schreibt die monotone Zeit
/// (Nanosekunden, LE-u64) nach `ziel`. Nur [`UHR_MONOTON`]; jede andere ID
/// schreibt nichts und meldet [`WasiFehler::UhrAbgelehnt`].
pub fn clock_time_get(
    speicher: &mut Speicher<'_>,
    id: u32,
    _praezision: u64,
    ziel: u32,
    uhr: &impl Uhr,
) -> Result<(), WasiFehler> {
    if id != UHR_MONOTON {
        return Err(WasiFehler::UhrAbgelehnt);
    }
    speicher
        .speichern_u64(ziel, uhr.nanos())
        .map_err(|_| WasiFehler::OobZugriff)?;
    Ok(())
}

/// `proc_exit(code)`: beendet die PD mit Kode (Rueckgabe statt Divergenz,
/// damit der Host-Test den Kode prueft; die Engine behandelt sie als Ende,
/// nie als Fortsetzung).
pub fn proc_exit(code: u32) -> WasiEnde {
    WasiEnde::Beendet(code)
}

/// `random_get(ptr, len)`: fuellt `len` Bytes ab `ptr` mit Host-Zufall.
/// `len = 0` ist erlaubt (reine Grenzpruefung, kein Zufallsverbrauch auf dem
/// leeren Bereich — der Fake-Zaehler bleibt stehen).
pub fn random_get(
    speicher: &mut Speicher<'_>,
    ptr: u32,
    len: u32,
    zufall: &mut impl Zufall,
) -> Result<(), WasiFehler> {
    super::zugriff_pruefen(ptr as u64, len as u64, speicher.len())
        .map_err(|_| WasiFehler::OobZugriff)?;
    let mut rest = len;
    let mut ab = ptr;
    while rest > 0 {
        let nimm = if rest as u64 > BLOCK as u64 { BLOCK as u32 } else { rest };
        let mut block = [0u8; BLOCK];
        zufall.fuellen(&mut block[..nimm as usize]);
        speicher
            .kopieren_in(ab, &block[..nimm as usize])
            .map_err(|_| WasiFehler::OobZugriff)?;
        ab = ab.checked_add(nimm).ok_or(WasiFehler::OobZugriff)?;
        rest -= nimm;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeLog {
        bytes: [u8; 256],
        len: usize,
    }

    impl FakeLog {
        fn neu() -> Self {
            FakeLog { bytes: [0u8; 256], len: 0 }
        }
        fn inhalt(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }

    impl LogSenke for FakeLog {
        fn log(&mut self, bytes: &[u8]) {
            let mut i = 0;
            while i < bytes.len() && self.len < self.bytes.len() {
                self.bytes[self.len] = bytes[i];
                self.len += 1;
                i += 1;
            }
        }
    }

    struct FakeUhr(u64);

    impl Uhr for FakeUhr {
        fn nanos(&self) -> u64 {
            self.0
        }
    }

    /// Zaehler-Zufall: Byte `n` ist `(start + n) mod 256` — Luecken sieht man.
    struct FakeZufall {
        naechstes: u8,
        rufe: u32,
    }

    impl FakeZufall {
        fn neu(start: u8) -> Self {
            FakeZufall { naechstes: start, rufe: 0 }
        }
    }

    impl Zufall for FakeZufall {
        fn fuellen(&mut self, buf: &mut [u8]) {
            if buf.is_empty() {
                return;
            }
            self.rufe += 1;
            let mut i = 0;
            while i < buf.len() {
                buf[i] = self.naechstes;
                self.naechstes = self.naechstes.wrapping_add(1);
                i += 1;
            }
        }
    }

    /// Grant mit zwei `iovec`-Einträgen ab 0 und Nutzlast ab 16:
    /// `[0]="Hallo, " (16,7)`, `[1]="Welt!" (23,5)`, `nwritten` bei 64.
    fn grant_mit_iovs() -> [u8; 128] {
        let mut g = [0u8; 128];
        // Eintrag 0: ptr 16, len 7.
        g[0..4].copy_from_slice(&16u32.to_le_bytes());
        g[4..8].copy_from_slice(&7u32.to_le_bytes());
        // Eintrag 1: ptr 23, len 5.
        g[8..12].copy_from_slice(&23u32.to_le_bytes());
        g[12..16].copy_from_slice(&5u32.to_le_bytes());
        g[16..23].copy_from_slice(b"Hallo, ");
        g[23..28].copy_from_slice(b"Welt!");
        g
    }

    #[test]
    fn fd_write_ok_schreibt_an_senke() {
        let mut g = grant_mit_iovs();
        let mut s = Speicher::neu(&mut g);
        let mut senke = FakeLog::neu();
        let n = fd_write(&mut s, 1, 0, 2, 64, &mut senke).expect("fd 1 schreibt");
        assert_eq!(n, 12);
        assert_eq!(senke.inhalt(), b"Hallo, Welt!");
        // Rueckschrieb steht im Speicher (LE-u32 bei 64).
        assert_eq!(s.laden_u32(64), Ok(12));
        // fd 2 schreibt ebenso (stderr teilt die Senke).
        let mut senke2 = FakeLog::neu();
        let n2 = fd_write(&mut s, 2, 8, 1, 68, &mut senke2).expect("fd 2 schreibt");
        assert_eq!(n2, 5);
        assert_eq!(senke2.inhalt(), b"Welt!");
        assert_eq!(s.laden_u32(68), Ok(5));
    }

    #[test]
    fn fd_drei_abgelehnt_schreibt_nichts() {
        let mut g = grant_mit_iovs();
        let mut s = Speicher::neu(&mut g);
        s.speichern_u32(64, 0xDEAD).expect("Marke legen");
        let mut senke = FakeLog::neu();
        for fd in [0u32, 3, 9, u32::MAX] {
            assert_eq!(
                fd_write(&mut s, fd, 0, 2, 64, &mut senke).err(),
                Some(WasiFehler::FdAbgelehnt),
                "fd {fd} abgewiesen"
            );
        }
        // Nichts geschrieben, `nwritten` unberuehrt (Marke steht).
        assert!(senke.inhalt().is_empty());
        assert_eq!(s.laden_u32(64), Ok(0xDEAD));
        assert_eq!(WasiFehler::FdAbgelehnt.als_wasm_fehler(), WasmFehler::ImportAbgelehnt);
        assert_eq!(WasiFehler::FdAbgelehnt.als_trap(), Trap::ImportAbgelehnt);
    }

    #[test]
    fn iovs_oob_trappt_benannt() {
        let mut g = grant_mit_iovs();
        let mut s = Speicher::neu(&mut g);
        let mut senke = FakeLog::neu();
        // Tafel ausserhalb: Start hinter dem Ende.
        assert_eq!(
            fd_write(&mut s, 1, 200, 1, 64, &mut senke).err(),
            Some(WasiFehler::OobZugriff)
        );
        // Tafel laeuft ueber das Ende (Eintrag 1 halb draussen).
        assert_eq!(
            fd_write(&mut s, 1, 124, 1, 64, &mut senke).err(),
            Some(WasiFehler::OobZugriff)
        );
        // Nutzlast ausserhalb: Eintrag zeigt hinter den Grant.
        s.speichern_u32(0, 120).expect("ptr umbiegen");
        s.speichern_u32(4, 16).expect("len umbiegen");
        assert_eq!(
            fd_write(&mut s, 1, 0, 1, 64, &mut senke).err(),
            Some(WasiFehler::OobZugriff)
        );
        // `nwritten` ausserhalb: schreiben ginge, der Rueckschrieb nicht.
        let mut h = grant_mit_iovs();
        let mut t = Speicher::neu(&mut h);
        assert_eq!(
            fd_write(&mut t, 1, 0, 2, 200, &mut senke).err(),
            Some(WasiFehler::OobZugriff)
        );
        assert_eq!(WasiFehler::OobZugriff.als_wasm_fehler(), WasmFehler::OobZugriff);
    }

    #[test]
    fn clock_nur_monoton() {
        let mut g = [0u8; 32];
        let mut s = Speicher::neu(&mut g);
        let uhr = FakeUhr(0x0102030405060708);
        clock_time_get(&mut s, UHR_MONOTON, 0, 8, &uhr).expect("monoton geht");
        assert_eq!(s.laden_u64(8), Ok(0x0102030405060708));
        // Jede andere ID schreibt nichts (Marke bei 16 bleibt).
        s.speichern_u64(16, 0xFFFF).expect("Marke legen");
        let uhr2 = FakeUhr(42);
        for id in [0u32, 2, 3, 9, u32::MAX] {
            assert_eq!(
                clock_time_get(&mut s, id, 0, 16, &uhr2).err(),
                Some(WasiFehler::UhrAbgelehnt),
                "ID {id} abgewiesen"
            );
        }
        assert_eq!(s.laden_u64(16), Ok(0xFFFF));
        // Praezision ist Hinweis, nicht Filter: grob geht ebenso.
        clock_time_get(&mut s, UHR_MONOTON, 1_000_000_000, 24, &uhr2)
            .expect("Praezision egal");
        assert_eq!(s.laden_u64(24), Ok(42));
    }

    #[test]
    fn clock_ziel_oob() {
        let mut g = [0u8; 32];
        let mut s = Speicher::neu(&mut g);
        let uhr = FakeUhr(7);
        assert_eq!(
            clock_time_get(&mut s, UHR_MONOTON, 0, 29, &uhr).err(),
            Some(WasiFehler::OobZugriff)
        );
    }

    #[test]
    fn proc_exit_traegt_kode() {
        assert_eq!(proc_exit(0), WasiEnde::Beendet(0));
        assert_eq!(proc_exit(3).kode(), 3);
        assert_eq!(proc_exit(u32::MAX).kode(), u32::MAX);
    }

    #[test]
    fn random_laenge_und_folge() {
        let mut g = [0u8; 64];
        let mut s = Speicher::neu(&mut g);
        let mut z = FakeZufall::neu(0xA0);
        random_get(&mut s, 8, 16, &mut z).expect("16 B gehen");
        let mut zurueck = [0u8; 16];
        s.kopieren_aus(8, &mut zurueck).expect("drinnen");
        let mut erwartet = [0u8; 16];
        let mut i = 0;
        while i < 16 {
            erwartet[i] = 0xA0u8.wrapping_add(i as u8);
            i += 1;
        }
        assert_eq!(zurueck, erwartet);
        // `len = 0` prueft nur die Grenze (kein Zufallsverbrauch).
        let mut z2 = FakeZufall::neu(0x01);
        random_get(&mut s, 64, 0, &mut z2).expect("Nullfuellung am Ende");
        assert_eq!(z2.rufe, 0);
        assert_eq!(random_get(&mut s, 65, 0, &mut z2).err(), Some(WasiFehler::OobZugriff));
        // Ueber das Ende ist benannt, nicht panisch.
        assert_eq!(random_get(&mut s, 60, 8, &mut z2).err(), Some(WasiFehler::OobZugriff));
        assert_eq!(random_get(&mut s, u32::MAX, 4, &mut z2).err(), Some(WasiFehler::OobZugriff));
    }

    #[test]
    fn import_tabelle_nur_vier() {
        // Die vier Erlaubten gehen durch — ueber die Pruefung aus `lib.rs`.
        for name in ["fd_write", "clock_time_get", "proc_exit", "random_get"] {
            assert!(ist_erlaubt(name), "{name} erlaubt");
            assert!(pruefe(name).is_ok(), "{name} geht durch");
        }
        // Alles andere scheitert benannt (kein Stub, kein Fallback).
        for name in [
            "path_open",
            "fd_read",
            "fd_seek",
            "sock_recv",
            "thread_spawn",
            "clock_time_set",
            "poll_oneoff",
            "fd_close",
            "",
        ] {
            assert!(!ist_erlaubt(name), "{name} nicht erlaubt");
            assert_eq!(pruefe(name).err(), Some(WasiFehler::ImportAbgelehnt));
        }
        // Falsches Modul schliesst auch erlaubte Namen aus (lib.rs-Regel).
        assert_eq!(
            super::pruefe_import("wasi_unstable", "fd_write").err(),
            Some(WasmFehler::ImportAbgelehnt)
        );
    }
}
