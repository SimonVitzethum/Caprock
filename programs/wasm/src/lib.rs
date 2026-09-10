//! **WASM-2a-Engine-Gerueste (`caprock-wasm`, Strang 6/WASM-ENG).**
//!
//! Norm: `programs/mem-server/SPEZIFIKATION.md`, Anhang Stufe 2a. Diese Crate baut das
//! Geruest — Parser, Validator, Fuel, Traps —, aber keine Ausfuehrung: Es gibt keinen
//! Interpreter und keinen Speicher. Der Linearspeicher (`mem`) und die WASI-Importe
//! (`wasi`) sind nur Anker-Module; ihren Inhalt liefern die Straenge 7/8.
//!
//! Regeln, die hier gelten (alle aus dem Anhang):
//! - Ein Modul hat genau einen Speicher mit `min > 0` und ohne `max` (Punkt 6).
//! - `memory.grow` mit `delta > 0` trappt benannt; `delta = 0` fragt nur an (Punkt 2).
//! - Nur vier WASI-Importe sind erlaubt; alles andere scheitert benannt (Punkt 3).
//! - Jede Absage ist benannt ([`WasmFehler`]); kein Panic, kein stiller Fallback.
//! - Fuel zaehlt Instruktionen, Budget 0 startet nicht (Punkt 4).
//!
//! ## Was gestellt bleibt
//!
//! - Kein Interpreter: [`Fuel`] zaehlt verbrauchte Schritte, fuehrt aber nichts aus.
//! - Kein Speicher: Die OOB-Pruefung ([`zugriff_pruefen`]) rechnet nur Grenzen; die
//!   Engine-Pruefung pro Lade/Speichern baut Strang 7 darauf.
//! - Der Import-Name reist NEBEN dem Fehler (Funktionsargument), nicht in ihm: Das haelt
//!   [`WasmFehler`] ohne Lifetime und `Copy` — die Anker-Module uebernehmen es so.

#![no_std]
#![forbid(unsafe_code)]

// Die Crate ist `no_std` (sie laeuft in einer WASM-PD ohne OS). Das Test-Harnisch
// braucht `std` dafuer — nur im Test, der PD-Bau sieht es nie (Muster wie
// `caprock-dma` / `lx-shim-demo`).
#[cfg(test)]
extern crate std;

/// Anker: Grant-gebundener Linearspeicher (Inhalt: Strang 7).
pub mod mem;
/// Anker: WASI-Importe der Stufe 2a (Inhalt: Strang 8).
pub mod wasi;

/// WASM-Magie `\0asm`.
pub const WASM_MAGIE: [u8; 4] = [0x00, 0x61, 0x73, 0x6D];
/// Einzige bekannte WASM-Version.
pub const WASM_VERSION: u32 = 1;
/// Sektionskennziffer der Memory-Sektion.
pub const SEKTION_SPEICHER: u8 = 5;
/// Eine WASM-Seite: 64 KiB (Anhang Punkt 1: Laenge = `min * 64 KiB`).
pub const WASM_SEITE: u64 = 65536;
/// Grant-Seite fuers Aufrunden (bestehende `aufgerundet`-Regel).
pub const GRANT_SEITE: u64 = 4096;
/// Festes Fuel-Budget der Stufe 2a (Anhang, offene Frage 1: fix pro 2a).
pub const FUEL_FIX: u64 = 100_000_000;
/// Zugehoeriges WASI-Modul: Nur von hier ist ueberhaupt etwas importierbar.
pub const WASI_MODUL: &str = "wasi_snapshot_preview1";

/// Jede Absage der Stufe 2a mit Namen. `Copy`, ohne Lifetime: Der Import-Name steht im
/// Aufrufkontext (s. [`pruefe_import`]), nicht im Fehler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WasmFehler {
    /// Kein Modul uebergeben (leere Bytes, fehlender `LOAD_IMAGE`-Schein).
    KeinModul,
    /// Erste vier Bytes sind nicht `\0asm`.
    MagieFalsch,
    /// Version ist nicht 1.
    VersionFalsch,
    /// Bytes enden mitten in Header, LEB128 oder Sektion.
    UnerwartetesEnde,
    /// Sektionslast passt nicht in die Restbytes.
    SektionAbgeschnitten,
    /// Speicherform ausserhalb 2a: kein Speicher, `min = 0` oder `max` gesetzt.
    SpeicherformAbgelehnt,
    /// Import steht nicht auf der 2a-Liste (Name im Aufrufkontext).
    ImportAbgelehnt,
    /// Fuel-Budget aufgebraucht — oder Budget 0 beim Start.
    BudgetErschoepft,
    /// Lade/Speichern ausserhalb des Linearspeichers.
    OobZugriff,
    /// `memory.grow` mit `delta > 0` (2a kennt kein Wachstum).
    WachstumAbgelehnt,
    /// Division durch Null.
    DivisionDurchNull,
    /// `unreachable` erreicht.
    Unerreichbar,
}

impl WasmFehler {
    /// Der benannte Grund als stabile Zeichenkette (Log + REPLY-Kontext).
    pub fn name(self) -> &'static str {
        match self {
            WasmFehler::KeinModul => "KeinModul",
            WasmFehler::MagieFalsch => "MagieFalsch",
            WasmFehler::VersionFalsch => "VersionFalsch",
            WasmFehler::UnerwartetesEnde => "UnerwartetesEnde",
            WasmFehler::SektionAbgeschnitten => "SektionAbgeschnitten",
            WasmFehler::SpeicherformAbgelehnt => "SpeicherformAbgelehnt",
            WasmFehler::ImportAbgelehnt => "ImportAbgelehnt",
            WasmFehler::BudgetErschoepft => "BudgetErschoepft",
            WasmFehler::OobZugriff => "OobZugriff",
            WasmFehler::WachstumAbgelehnt => "WachstumAbgelehnt",
            WasmFehler::DivisionDurchNull => "DivisionDurchNull",
            WasmFehler::Unerreichbar => "Unerreichbar",
        }
    }
}

/// WASM-Traps der Stufe 2a (Anhang Punkt 5): Jeder beendet die PD mit benanntem Grund —
/// niemals Kernel-Panic, niemals Server-Zustandsaenderung.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trap {
    /// Lade/Speichern ueber die Grant-Grenze hinaus.
    OobZugriff,
    /// `memory.grow(delta > 0)`.
    WachstumAbgelehnt,
    /// Verbotener WASI-Import.
    ImportAbgelehnt,
    /// Fuel aufgebraucht.
    BudgetErschoepft,
    /// Division durch Null.
    DivisionDurchNull,
    /// `unreachable`.
    Unerreichbar,
}

impl Trap {
    /// Der benannte Grund als stabile Zeichenkette.
    pub fn name(self) -> &'static str {
        match self {
            Trap::OobZugriff => "OobZugriff",
            Trap::WachstumAbgelehnt => "WachstumAbgelehnt",
            Trap::ImportAbgelehnt => "ImportAbgelehnt",
            Trap::BudgetErschoepft => "BudgetErschoepft",
            Trap::DivisionDurchNull => "DivisionDurchNull",
            Trap::Unerreichbar => "Unerreichbar",
        }
    }

    /// Derselbe Grund als [`WasmFehler`] (Meldung an den Aufrufer).
    pub fn als_fehler(self) -> WasmFehler {
        match self {
            Trap::OobZugriff => WasmFehler::OobZugriff,
            Trap::WachstumAbgelehnt => WasmFehler::WachstumAbgelehnt,
            Trap::ImportAbgelehnt => WasmFehler::ImportAbgelehnt,
            Trap::BudgetErschoepft => WasmFehler::BudgetErschoepft,
            Trap::DivisionDurchNull => WasmFehler::DivisionDurchNull,
            Trap::Unerreichbar => WasmFehler::Unerreichbar,
        }
    }
}

/// Was der Parser aus einem Modul liest: Sektionszahl plus erster Memory-Eintrag.
// 2a kennt nur einen Speicher (Punkt 1: genau EIN Grant); weitere Eintraege zaehlen
// als Sektionen mit, werden aber nicht gelesen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ModulInfo {
    /// Gezaehlte Sektionen hinter dem 8-Byte-Header.
    pub sektionen: u32,
    /// Ob eine Memory-Sektion mit mindestens einem Eintrag vorlag.
    pub hat_speicher: bool,
    /// `memory.min` (Seiten) des ersten Eintrags.
    pub speicher_min: u32,
    /// `memory.max` (Seiten) des ersten Eintrags, falls gesetzt.
    pub speicher_max: Option<u32>,
}

/// Vorzeichenlose LEB128 lesen; `pos` steht danach hinter dem Wert. Schranken statt
/// Panic: abgebrochene Folgen und Ueberlaeufe sind [`WasmFehler::UnerwartetesEnde`].
fn lese_uleb128(bytes: &[u8], pos: &mut usize) -> Result<u32, WasmFehler> {
    let mut ergebnis: u32 = 0;
    let mut schicht: u32 = 0;
    loop {
        let b = match bytes.get(*pos) {
            Some(&x) => x,
            None => return Err(WasmFehler::UnerwartetesEnde),
        };
        *pos += 1;
        // Fuenf Bytes tragen hoechstens 32 Bit; das sechste waere Ueberlauf.
        if schicht >= 35 {
            return Err(WasmFehler::UnerwartetesEnde);
        }
        let stueck = (b & 0x7F) as u32;
        // Bits oberhalb 32 duerfen nicht gesetzt sein (sonst > u32).
        if schicht == 28 && stueck > 0x0F {
            return Err(WasmFehler::UnerwartetesEnde);
        }
        ergebnis |= stueck << schicht;
        if b & 0x80 == 0 {
            return Ok(ergebnis);
        }
        schicht += 7;
    }
}

/// Modul parsen: Magie/Version pruefen, Sektionen zaehlen, ersten Memory-Eintrag lesen.
/// Gibt nie Panic — jede Absage ist ein benannter [`WasmFehler`].
pub fn parse_modul(bytes: &[u8]) -> Result<ModulInfo, WasmFehler> {
    if bytes.is_empty() {
        return Err(WasmFehler::KeinModul);
    }
    if bytes.len() < 8 {
        return Err(WasmFehler::UnerwartetesEnde);
    }
    if bytes[0] != WASM_MAGIE[0]
        || bytes[1] != WASM_MAGIE[1]
        || bytes[2] != WASM_MAGIE[2]
        || bytes[3] != WASM_MAGIE[3]
    {
        return Err(WasmFehler::MagieFalsch);
    }
    let version = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    if version != WASM_VERSION {
        return Err(WasmFehler::VersionFalsch);
    }
    let mut info = ModulInfo { sektionen: 0, hat_speicher: false, speicher_min: 0, speicher_max: None };
    let mut pos = 8;
    while pos < bytes.len() {
        let kennziffer = match bytes.get(pos) {
            Some(&x) => x,
            None => return Err(WasmFehler::UnerwartetesEnde),
        };
        pos += 1;
        let laenge = lese_uleb128(bytes, &mut pos)? as usize;
        let start = pos;
        // Die Last muss vollständig in den Restbytes liegen — ohne Ueberlauf zu rechnen.
        if laenge > bytes.len().saturating_sub(start) {
            return Err(WasmFehler::SektionAbgeschnitten);
        }
        let ende = start + laenge;
        if kennziffer == SEKTION_SPEICHER && !info.hat_speicher {
            let mut p = start;
            let eintraege = lese_uleb128(bytes, &mut p)?;
            // Nur den ersten Eintrag lesen (2a: ein Speicher); der Rest zaehlt als Last.
            if eintraege > 0 {
                // Der Eintrag muss innerhalb der Sektionslast enden, nicht nur im Modul.
                let merkmale = lese_uleb128(bytes, &mut p)?;
                let min = lese_uleb128(bytes, &mut p)?;
                if p > ende {
                    return Err(WasmFehler::SektionAbgeschnitten);
                }
                let max = if merkmale & 1 != 0 {
                    let m = lese_uleb128(bytes, &mut p)?;
                    if p > ende {
                        return Err(WasmFehler::SektionAbgeschnitten);
                    }
                    Some(m)
                } else {
                    None
                };
                info.hat_speicher = true;
                info.speicher_min = min;
                info.speicher_max = max;
            }
        }
        info.sektionen += 1;
        pos = ende;
    }
    Ok(info)
}

/// Modul validieren (Anhang Punkt 6): 2a kennt nur festes `min > 0` ohne `max`.
// `max` verspricht Wachstum, das 2a nicht einloest — also Absage statt Ignorieren.
// Gibt bei Erfolg die Seitenzahl zurueck (fuer genau EIN `ANFORDERN`).
pub fn validiere(info: &ModulInfo) -> Result<u32, WasmFehler> {
    if !info.hat_speicher || info.speicher_min == 0 || info.speicher_max.is_some() {
        return Err(WasmFehler::SpeicherformAbgelehnt);
    }
    Ok(info.speicher_min)
}

/// Grant-Laenge fuers einzige `ANFORDERN`: `min * 64 KiB`, aufgerundet auf Seiten.
// Absagen: `LeereAnfrage`-Form (`min = 0`) und Ueberlauf — beide benannt.
pub fn grant_laenge(min_seiten: u32) -> Result<u64, WasmFehler> {
    if min_seiten == 0 {
        return Err(WasmFehler::SpeicherformAbgelehnt);
    }
    let bytes = (min_seiten as u64).checked_mul(WASM_SEITE).ok_or(WasmFehler::SpeicherformAbgelehnt)?;
    let aufgerundet = bytes.checked_add(GRANT_SEITE - 1).ok_or(WasmFehler::SpeicherformAbgelehnt)?
        / GRANT_SEITE
        * GRANT_SEITE;
    Ok(aufgerundet)
}

/// Import erlaubt? (Anhang Punkt 3: Negativliste — nur vier Namen aus genau einem Modul.)
pub fn ist_import_erlaubt(modul: &str, name: &str) -> bool {
    if modul != WASI_MODUL {
        return false;
    }
    matches!(name, "fd_write" | "clock_time_get" | "proc_exit" | "random_get")
}

/// Import pruefen: Erlaubtes geht durch, alles andere scheitert benannt statt einen Stub
/// zu linken. Der Name steht im Aufrufkontext (Aufrufer loggt ihn); der Fehler bleibt
/// ohne Lifetime (s. Modul-Doku).
pub fn pruefe_import(modul: &str, name: &str) -> Result<(), WasmFehler> {
    if ist_import_erlaubt(modul, name) {
        Ok(())
    } else {
        Err(WasmFehler::ImportAbgelehnt)
    }
}

/// Fuel: zaehlt Instruktionen, nicht Wandzeit (Anhang Punkt 4). Budget 0 startet nicht —
// die Absage faellt beim Anlegen, nicht beim ersten Schritt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fuel {
    rest: u64,
}

impl Fuel {
    /// Budget anlegen; 0 heisst „startet nicht".
    pub fn neu(budget: u64) -> Result<Self, WasmFehler> {
        if budget == 0 {
            return Err(WasmFehler::BudgetErschoepft);
        }
        Ok(Fuel { rest: budget })
    }

    /// `n` Instruktionen verbrauchen; reicht es nicht, trappt es benannt.
    pub fn verbrauch(&mut self, n: u64) -> Result<(), WasmFehler> {
        if n > self.rest {
            return Err(WasmFehler::BudgetErschoepft);
        }
        self.rest -= n;
        Ok(())
    }

    /// Eine Instruktion (der Normalfall des Interpreters).
    pub fn schritt(&mut self) -> Result<(), WasmFehler> {
        self.verbrauch(1)
    }

    /// Verbliebenes Budget (Bilanzhilfe, kein Nachschub — 2a kennt keinen).
    pub fn rest(self) -> u64 {
        self.rest
    }
}

/// Speicherzugriff pruefen (Anhang Punkt 5): Jeder Lade-/Speicherzugriff muss gegen die
/// Grant-Grenze halten — OOB ist ein benannter PD-Fault, kein Beweis gegen die Engine.
// Ueberlaufsicher gerechnet (`offset + len` darf nicht wrappen).
pub fn zugriff_pruefen(offset: u64, len: u64, speicher_len: u64) -> Result<(), WasmFehler> {
    if len > speicher_len || offset > speicher_len - len {
        return Err(WasmFehler::OobZugriff);
    }
    Ok(())
}

/// `memory.grow` der Stufe 2a (Anhang Punkt 2): `delta = 0` fragt die Groesse an,
// jedes echte Wachstum trappt benannt — kein `-1` ohne Trap-Namen, kein Anhaengen.
pub fn wachstum(delta: u32, aktuell_seiten: u32) -> Result<u32, WasmFehler> {
    if delta == 0 {
        Ok(aktuell_seiten)
    } else {
        Err(WasmFehler::WachstumAbgelehnt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kleinster gueltiger Rumpf: Header + Memory-Sektion mit gegebenem Eintrags-Kern.
    /// Der Aufrufer gibt den Kern ab dem Zaehler vor (z. B. `[1, 0, 1]` = ein Eintrag,
    /// Merkmale 0, min 1); Laenge und Huelle baut der Helfer.
    fn rumpf(kern: &[u8]) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = 0x00;
        b[1] = 0x61;
        b[2] = 0x73;
        b[3] = 0x6D;
        b[4] = 0x01;
        // b[5..8] = 0: Version 1.
        b[8] = SEKTION_SPEICHER;
        b[9] = kern.len() as u8;
        let mut i = 0;
        while i < kern.len() && 10 + i < b.len() {
            b[10 + i] = kern[i];
            i += 1;
        }
        b
    }

    /// Gueltige 2a-Form: ein Eintrag, Merkmale 0 (kein max), min 1.
    fn gueltig() -> [u8; 16] {
        rumpf(&[1, 0, 1])
    }

    fn nutzlaenge(b: &[u8; 16]) -> usize {
        10 + b[9] as usize
    }

    #[test]
    fn magie_falsch() {
        let mut b = gueltig();
        b[0] = 0xFF;
        let n = nutzlaenge(&b);
        assert_eq!(parse_modul(&b[..n]).err(), Some(WasmFehler::MagieFalsch));
    }

    #[test]
    fn version_falsch() {
        let mut b = gueltig();
        b[4] = 0x02;
        let n = nutzlaenge(&b);
        assert_eq!(parse_modul(&b[..n]).err(), Some(WasmFehler::VersionFalsch));
    }

    #[test]
    fn kein_modul_und_abgebrochen_benannt() {
        assert_eq!(parse_modul(&[]).err(), Some(WasmFehler::KeinModul));
        // Vier Bytes sind weder leer noch ein Header — abgebrochen, nicht „kein Modul".
        assert_eq!(parse_modul(&[0x00, 0x61, 0x73, 0x6D]).err(), Some(WasmFehler::UnerwartetesEnde));
        // Sektionslast laenger als die Restbytes.
        let ab = [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x05, 0x09, 0x01];
        assert_eq!(parse_modul(&ab).err(), Some(WasmFehler::SektionAbgeschnitten));
    }

    #[test]
    fn min_null_abgelehnt() {
        // Ein Eintrag, Merkmale 0, min 0 — die `LeereAnfrage`-Form (Anhang Punkt 1/6).
        let b = rumpf(&[1, 0, 0]);
        let n = nutzlaenge(&b);
        let info = parse_modul(&b[..n]).expect("parst, scheitert erst am Validator");
        assert_eq!(info.speicher_min, 0);
        assert_eq!(validiere(&info).err(), Some(WasmFehler::SpeicherformAbgelehnt));
        // Ohne Memory-Sektion gilt dasselbe.
        let nackt = [0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        let nackt_info = parse_modul(&nackt).expect("nackt parst");
        assert_eq!(nackt_info.sektionen, 0);
        assert_eq!(validiere(&nackt_info).err(), Some(WasmFehler::SpeicherformAbgelehnt));
    }

    #[test]
    fn max_gesetzt_abgelehnt() {
        // Ein Eintrag, Merkmale 1, min 1, max 2 — verspricht Wachstum, das 2a ablehnt.
        let b = rumpf(&[1, 1, 1, 2]);
        let n = nutzlaenge(&b);
        let info = parse_modul(&b[..n]).expect("parst, scheitert erst am Validator");
        assert_eq!(info.speicher_max, Some(2));
        assert_eq!(validiere(&info).err(), Some(WasmFehler::SpeicherformAbgelehnt));
    }

    #[test]
    fn gueltig_gibt_genau_ein_anfordern() {
        let b = gueltig();
        let n = nutzlaenge(&b);
        let info = parse_modul(&b[..n]).expect("gueltig parst");
        assert_eq!(info.sektionen, 1);
        assert_eq!(validiere(&info), Ok(1));
        // Eine Seite = 64 KiB, bereits seitengerecht — genau EIN Grant dieser Laenge.
        assert_eq!(grant_laenge(1), Ok(65536));
    }

    #[test]
    fn import_negativliste() {
        // Die vier Erlaubten gehen durch — aus genau einem Modul.
        assert!(pruefe_import(WASI_MODUL, "fd_write").is_ok());
        assert!(pruefe_import(WASI_MODUL, "clock_time_get").is_ok());
        assert!(pruefe_import(WASI_MODUL, "proc_exit").is_ok());
        assert!(pruefe_import(WASI_MODUL, "random_get").is_ok());
        // Alles andere scheitert benannt: kein Pfad-Dienst, keine Sockets, keine Threads.
        for name in ["path_open", "fd_read", "sock_recv", "thread_spawn", "poll_oneoff"] {
            assert_eq!(
                pruefe_import(WASI_MODUL, name).err(),
                Some(WasmFehler::ImportAbgelehnt),
                "verboten: {name}"
            );
        }
        // Falsches Modul schliesst auch erlaubte Namen aus.
        assert_eq!(
            pruefe_import("wasi_unstable", "fd_write").err(),
            Some(WasmFehler::ImportAbgelehnt)
        );
    }

    #[test]
    fn fuel_null_startet_nicht() {
        // Budget 0: Absage beim Anlegen, nicht beim ersten Schritt (Anhang Punkt 4).
        assert_eq!(Fuel::neu(0).err(), Some(WasmFehler::BudgetErschoepft));
    }

    #[test]
    fn fuel_erschoepfung_bricht_ab() {
        let mut f = Fuel::neu(3).expect("Budget 3 startet");
        f.schritt().expect("1");
        f.schritt().expect("2");
        f.schritt().expect("3");
        assert_eq!(f.rest(), 0);
        assert_eq!(f.schritt().err(), Some(WasmFehler::BudgetErschoepft));
        // Ueberverbrauch auf einmal trappt ebenso (kein Wrap auf 0).
        let mut g = Fuel::neu(2).expect("Budget 2 startet");
        assert_eq!(g.verbrauch(3).err(), Some(WasmFehler::BudgetErschoepft));
    }

    #[test]
    fn oob_ist_benannter_fault() {
        let speicher = 65536u64;
        // Drinnen: Letztes Byte und leerer Zugriff am Ende gehen durch.
        assert!(zugriff_pruefen(65535, 1, speicher).is_ok());
        assert!(zugriff_pruefen(65536, 0, speicher).is_ok());
        // Draussen: ein Byte zu weit, Wrap per Ueberlauf, alles ueber der Grenze.
        assert_eq!(zugriff_pruefen(65536, 1, speicher).err(), Some(WasmFehler::OobZugriff));
        assert_eq!(zugriff_pruefen(u64::MAX, 1, speicher).err(), Some(WasmFehler::OobZugriff));
        assert_eq!(zugriff_pruefen(u64::MAX, u64::MAX, speicher).err(), Some(WasmFehler::OobZugriff));
    }

    #[test]
    fn wachstum_nur_anfrage() {
        // `delta = 0` fragt an, jedes echte Wachstum trappt (Anhang Punkt 2).
        assert_eq!(wachstum(0, 1), Ok(1));
        assert_eq!(wachstum(1, 1).err(), Some(WasmFehler::WachstumAbgelehnt));
    }

    #[test]
    fn trap_namen_eindeutig() {
        // Jeder Trap hat einen Namen, jeder Name genau einen Trap — kein „unbekannt",
        // keine zwei Traps teilen sich einen Namen (Anhang Punkt 5: benannt melden).
        let trappen = [
            Trap::OobZugriff,
            Trap::WachstumAbgelehnt,
            Trap::ImportAbgelehnt,
            Trap::BudgetErschoepft,
            Trap::DivisionDurchNull,
            Trap::Unerreichbar,
        ];
        let mut i = 0;
        while i < trappen.len() {
            let n = trappen[i].name();
            assert!(n != "unbekannt" && !n.is_empty(), "Trap {i} ist benannt");
            // Der Fehler traegt denselben Namen (Meldung an den Aufrufer).
            assert_eq!(trappen[i].als_fehler().name(), n);
            let mut j = 0;
            while j < i {
                assert!(trappen[j].name() != n, "Name {n} doppelt");
                j += 1;
            }
            i += 1;
        }
    }
}
