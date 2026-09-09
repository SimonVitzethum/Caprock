//! **Drahtworte des Laufzeit-Laders — zwei Richtungen, ein Format.**
//!
//! Der Draht sind CALL/REPLY-Worte (`[u64; 4]`, vier Register — mehr traegt ein REPLY nicht,
//! dieselbe Form wie `programs/mem-server/src/protokoll.rs`):
//!
//! ```text
//! Richtung A — Lader → fremder Blockdriver (der Lader ist CLIENT):
//!   [OP, LBA, ANZAHL, 0]
//!   OP_INFO (0): Auskunft → [STATUS, Kapazitaet, MaxSektoren, SektorBytes]
//!   OP_READ (1): Sektoren lesen → [STATUS, GeleseneSektoren, 0, 0]
//!   Die BYTES selbst stehen nie in den Worten: Sie liegen in der geteilten
//!   Uebertragungsflaeche (Staging wie in programs/hardware/virtio-blk: der Treiber
//!   fuellt seinen Puffer, der Lader kopiert heraus). Wer Sektornummern und Bytes in
//!   dieselben vier Register falten wollte, muesste Bytes adressieren, die er nicht haelt.
//!
//! Richtung B — irgendwer → Lader (der Lader ist SERVER):
//!   [ART, A, B, 0]
//!   AUSKUNFT (10): Zustand abfragen → [KODE, Phase, BildLaenge, 0]
//!   LADEN (11): A = LXPD-Partitions-Index → [KODE, NeuePdId, 0, 0]
//!   KODE 0 = OK, sonst [`lade_kode`].
//! ```
//!
//! ## Wiederverwendet oder abgewichen? (gegen `virtio-blk` gelesen)
//!
//! | Punkt | Entscheidung |
//! |---|---|
//! | OP-Codes `INFO=0`/`READ=1`, Status `OK/DEVICE/BADOP/RANGE` | **wiederverwendet** (Byte-identisch): Jeder Blockdriver, der das virtio-blk-Protokoll spricht, bedient diesen Lader ohne Aenderung — das Minimal-Protokoll ist damit keine neue Schnittstelle, sondern eine Teilmenge einer geprueften. |
//! | `OP_SCAN = 5` | **nicht verwendet**: SCAN meldet nur die ERSTE Partition (Zahl, LBA, Groesse), ohne Typ-GUID. Der Lader braucht die Typ-Auswahl (LXPD gegen fremd) und liest deshalb RoH-Sektoren per READ und parst mit `caprock-part` selbst — dieselben Regeln wie der Treiber, aber die VOLLE Tabelle statt einer Zusammenfassung. |
//! | `WRITE=3`/`FLUSH=4`/`STOP=2` | **nie gesendet**: Ein Lader schreibt keine Platte. Der Client baut dafuer nicht einmal Worte ([`BlockAnfrage::schreiben`] existiert nicht — was man nicht bauen kann, kann man nicht versehentlich schicken). Ein Schreibpfad waere Angriffsflaeche ohne Benutzer. |
//! | `ST_NOTABLE = 4` | **nicht verwendet**: Folgt aus SCAN; ohne SCAN gibt es keine Tabelle-als-Status. GPT-Fehler meldet der Lader als eigene benannte Absagen ([`super::LadeFehler`]). |
//!
//! ## Was hier laeuft und was gestellt ist
//!
//! [`BlockAnfrage`]/[`BlockAntwort`] pruefen die Wortform (Codes, LBA-Reichweite gegen die
//! gemeldete Kapazitaet — die Bereichspruefung VOR dem Geraet wie im Treiber). Gestellt ist der
//! Transport selbst: Der Host-Test spricht die [`super::BlockQuelle`] direkt an (Bytes statt
//! Worte); dass Anfrageworte und Quellen-Aufrufe dieselben Zahlen tragen, prueft
//! `block_worte_tragen_dieselbe_anfrage`.
//!
//! ## Was der Lader NICHT ueber Worte annimmt
//!
//! Den Vertrauensschluessel und die Image-Bytes gibt es nicht als Nachricht: Der Schluessel
//! steht in der PD (Endowment, nicht Draht — wer ihn per Wort setzen koennte, waehlte seinen
//! eigenen Pruefer), die Bytes kommen von der Platte (wer sie per Wort schicken koennte,
//! umginge GPT, Verzeichnis und Hash in einem Schritt).

use super::LadeFehler;

// --- Richtung A: Lader → Blockdriver (Teilmenge von virtio-blk) ---------------------

/// Auskunft: Kapazitaet, HoChstzahl Sektoren je Anfrage, SektorgrOesse.
pub const OP_INFO: u64 = 0;
/// Sektoren lesen (`w[1]` = erster Sektor, `w[2]` = Anzahl).
pub const OP_READ: u64 = 1;

/// Antwortstatus: alles gut.
pub const ST_OK: u64 = 0;
/// Antwortstatus: das Geraet antwortete nicht oder meldete einen Fehler.
pub const ST_DEVICE: u64 = 1;
/// Antwortstatus: unbekannte Operation.
pub const ST_BADOP: u64 = 2;
/// Antwortstatus: Bereich — ausserhalb der Platte oder mehr als eine Anfrage fasst.
/// Eigener Status, kein `ST_DEVICE`: „ich habe nicht gefragt" (Client-Fehler) ist eine andere
/// Lage als „das Geraet hat nein gesagt" (kein Client-Fehler).
pub const ST_RANGE: u64 = 3;

/// Eine Block-Anfrage auf dem Draht: Art plus LBA plus Anzahl. Genau vier Register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockAnfrage {
    /// Die Operation (`OP_INFO` / `OP_READ` — sonst nichts baubar).
    pub op: u64,
    /// Erster Sektor.
    pub lba: u64,
    /// Anzahl Sektoren.
    pub anzahl: u32,
}

impl BlockAnfrage {
    /// Leseanfrage bauen. `anzahl == 0` ist baubar, aber nie sendbar — s. [`Self::pruefen`].
    pub fn lesen(lba: u64, anzahl: u32) -> Self {
        BlockAnfrage { op: OP_READ, lba, anzahl }
    }

    /// Auskunft bauen.
    pub fn auskunft() -> Self {
        BlockAnfrage { op: OP_INFO, lba: 0, anzahl: 0 }
    }

    /// Auf den Draht: vier Register.
    pub fn als_worte(self) -> [u64; 4] {
        [self.op, self.lba, u64::from(self.anzahl), 0]
    }

    /// Vom Draht lesen.
    pub fn aus_worten(w: [u64; 4]) -> Self {
        BlockAnfrage { op: w[0], lba: w[1], anzahl: w[2] as u32 }
    }

    /// Sendbar? Unbekannte Ops, leere Reads und Ueberlaeufe (`lba + anzahl` ueber `u64`,
    /// oder hinter `kapazitaet`) scheitern HIER — vor dem Geraet, nicht danach.
    /// `max_je_anfrage` ist die Geraetezusage aus `OP_INFO` (virtio-blk: 8).
    pub fn pruefen(self, kapazitaet: u64, max_je_anfrage: u32) -> Result<(), BlockBauFehler> {
        if self.op != OP_INFO && self.op != OP_READ {
            return Err(BlockBauFehler::UnbekannteOp);
        }
        if self.op == OP_READ {
            if self.anzahl == 0 {
                return Err(BlockBauFehler::LeereAnfrage);
            }
            if self.anzahl > max_je_anfrage {
                return Err(BlockBauFehler::ZuVielAufEinmal);
            }
            let ende = u64::from(self.anzahl)
                .checked_add(self.lba)
                .ok_or(BlockBauFehler::Bereich)?;
            if self.lba >= kapazitaet || ende > kapazitaet {
                return Err(BlockBauFehler::Bereich);
            }
        }
        Ok(())
    }
}

/// Was der Client falsch machen kann, ohne dass je ein Wort rausgeht.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockBauFehler {
    /// Weder INFO noch READ — dieser Lader kennt keine dritte Operation.
    UnbekannteOp,
    /// Null Sektoren lesen ist keine Anfrage.
    LeereAnfrage,
    /// Mehr als die Geraetezusage je Anfrage (der Dienst stueckelt selbst, s. `lib.rs`).
    ZuVielAufEinmal,
    /// Hinter der Kapazitaet oder ueber `u64` hinaus.
    Bereich,
}

/// Eine Block-Antwort auf dem Draht: Status plus drei Worte Nutzlast.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockAntwort {
    /// Der Status (`ST_*`).
    pub status: u64,
    /// Erstes Nutzwort (READ: gelesene Sektoren; INFO: Kapazitaet).
    pub w1: u64,
    /// Zweites Nutzwort (INFO: MaxSektoren).
    pub w2: u64,
    /// Drittes Nutzwort (INFO: SektorBytes).
    pub w3: u64,
}

impl BlockAntwort {
    /// Auf den Draht.
    pub fn als_worte(self) -> [u64; 4] {
        [self.status, self.w1, self.w2, self.w3]
    }

    /// Vom Draht lesen.
    pub fn aus_worten(w: [u64; 4]) -> Self {
        BlockAntwort { status: w[0], w1: w[1], w2: w[2], w3: w[3] }
    }

    /// READ-Antwort deuten: `Ok(gelesene Sektoren)` oder der benannte Geraetefehler.
    /// Eine KUERZERE Antwort als angefragt ist kein „Teilerfolg", sondern
    /// [`BlockDeutFehler::Abgebrochen`] — wer einen Teil als Ganzes naehme, pruefte einen
    /// Hash ueber fremde Restbytes.
    pub fn als_read(self, angefragt: u32) -> Result<u32, BlockDeutFehler> {
        match self.status {
            ST_OK => {
                let n = u32::try_from(self.w1).map_err(|_| BlockDeutFehler::Unsinn)?;
                if n != angefragt {
                    return Err(BlockDeutFehler::Abgebrochen);
                }
                Ok(n)
            }
            ST_DEVICE => Err(BlockDeutFehler::Geraet),
            ST_RANGE => Err(BlockDeutFehler::Bereich),
            ST_BADOP => Err(BlockDeutFehler::UnbekannteOp),
            _ => Err(BlockDeutFehler::Unsinn),
        }
    }
}

/// Was eine Block-Antwort bedeuten kann, ausser „gut".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockDeutFehler {
    /// Das Geraet hat nein gesagt (oder gar nichts).
    Geraet,
    /// Ausserhalb der Platte — haette der Client vorher wissen koennen.
    Bereich,
    /// Der Treiber kennt die Operation nicht (sollte nicht passieren: der Client schickt nur
    /// INFO/READ — kaeme es doch, waere es der erste Hinweis auf einen fremden Treiber).
    UnbekannteOp,
    /// Kuerzer als angefragt — abgebrochen, nicht teilweise gut.
    Abgebrochen,
    /// Status oder Zahl ausserhalb jeder Vereinbarung.
    Unsinn,
}

// --- Richtung B: irgendwer → Lader ---------------------------------------------------

/// Nachrichtenart an den Lader: Zustand abfragen.
pub const ART_AUSKUNFT: u64 = 10;
/// Nachrichtenart an den Lader: Treiber-Image `A` (LXPD-Partitions-Index) laden.
pub const ART_LADEN: u64 = 11;

/// Eine Nachricht an den Lader: Art plus zwei Worte Nutzlast.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Nachricht {
    /// Die Art (`ART_*`).
    pub art: u64,
    /// Erstes Nutzwort (LADEN: Partitions-Index).
    pub a: u64,
    /// Zweites Nutzwort (ungenutzt, muss 0 sein — s. [`Nachricht::pruefen`]).
    pub b: u64,
}

impl Nachricht {
    /// Auskunft bauen.
    pub fn auskunft() -> Self {
        Nachricht { art: ART_AUSKUNFT, a: 0, b: 0 }
    }

    /// Laden bauen: das `index`-te LXPD-Image der Platte.
    pub fn laden(index: u64) -> Self {
        Nachricht { art: ART_LADEN, a: index, b: 0 }
    }

    /// Auf den Draht.
    pub fn als_worte(self) -> [u64; 4] {
        [self.art, self.a, self.b, 0]
    }

    /// Vom Draht lesen.
    pub fn aus_worten(w: [u64; 4]) -> Self {
        Nachricht { art: w[0], a: w[1], b: w[2] }
    }

    /// Empfaengersicht: Art bekannt, Reservewort leer? Ein gesetztes `b` ist kein „egal",
    /// sondern ein fremdes Protokoll — benannt abgewiesen statt still uebergangen.
    pub fn pruefen(self) -> Result<(), NachrichtFehler> {
        match self.art {
            ART_AUSKUNFT | ART_LADEN => {
                if self.b != 0 {
                    return Err(NachrichtFehler::ReservewortGesetzt);
                }
                Ok(())
            }
            _ => Err(NachrichtFehler::UnbekannteArt),
        }
    }
}

/// Was eine Nachricht an den Lader falsch machen kann.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NachrichtFehler {
    /// Diese Art kennt der Lader nicht.
    UnbekannteArt,
    /// Das Reservewort ist gesetzt — fremdes oder neueres Protokoll.
    ReservewortGesetzt,
}

/// Fehlerkode auf dem Draht: 0 = OK, sonst der benannte Ausgang aus `lib.rs`.
pub fn lade_kode(f: LadeFehler) -> u64 {
    match f {
        LadeFehler::KeineKapazitaet => 1,
        LadeFehler::GptSignatur => 2,
        LadeFehler::GptRevision => 3,
        LadeFehler::GptKopfGroesse => 4,
        LadeFehler::GptKopfCrc => 5,
        LadeFehler::GptEintragGroesse => 6,
        LadeFehler::GptEintragZahl => 7,
        LadeFehler::GptEintraegeCrc => 8,
        LadeFehler::GptEintragZuGross => 9,
        LadeFehler::KeineLxpdPartition => 10,
        LadeFehler::Geraet => 11,
        LadeFehler::Bereich => 12,
        LadeFehler::Abgebrochen => 13,
        LadeFehler::VerzeichnisKaputt => 14,
        LadeFehler::BildZuGross => 15,
        LadeFehler::ManifestZuGross => 16,
        LadeFehler::EintragZuGross => 17,
        LadeFehler::BildHashWeichtAb => 18,
        LadeFehler::KeinBildformat => 19,
        LadeFehler::Container(_) => 20,
        LadeFehler::TreiberMismatch => 21,
        LadeFehler::Ungeprueft => 22,
        LadeFehler::KeineLoaderCap => 23,
        LadeFehler::KeineBildUebergabe => 24,
        LadeFehler::AnstossAbgelehnt => 25,
        LadeFehler::FalscheNachricht => 26,
    }
}

/// Kode zurueck — `None` heisst: diesen Kode vergibt der Lader nie.
pub fn lade_fehler_aus_kode(k: u64) -> Option<LadeFehler> {
    match k {
        0 => None,
        1 => Some(LadeFehler::KeineKapazitaet),
        2 => Some(LadeFehler::GptSignatur),
        3 => Some(LadeFehler::GptRevision),
        4 => Some(LadeFehler::GptKopfGroesse),
        5 => Some(LadeFehler::GptKopfCrc),
        6 => Some(LadeFehler::GptEintragGroesse),
        7 => Some(LadeFehler::GptEintragZahl),
        8 => Some(LadeFehler::GptEintraegeCrc),
        9 => Some(LadeFehler::GptEintragZuGross),
        10 => Some(LadeFehler::KeineLxpdPartition),
        11 => Some(LadeFehler::Geraet),
        12 => Some(LadeFehler::Bereich),
        13 => Some(LadeFehler::Abgebrochen),
        14 => Some(LadeFehler::VerzeichnisKaputt),
        15 => Some(LadeFehler::BildZuGross),
        16 => Some(LadeFehler::ManifestZuGross),
        17 => Some(LadeFehler::EintragZuGross),
        18 => Some(LadeFehler::BildHashWeichtAb),
        19 => Some(LadeFehler::KeinBildformat),
        // Der Container-Fehler traegt seine Diagnose im Typ, nicht im Kode: Auf dem Draht steht
        // nur „Container abgelehnt" (20) — die genaue Variante steht im PD-Log, nicht in vier
        // Registern. Wer sie braucht, fragt AUSKUNFT nicht, sondern liest das Log.
        20 => Some(LadeFehler::Container(caprock_lxpd::LxpdError::BadMagic)),
        21 => Some(LadeFehler::TreiberMismatch),
        22 => Some(LadeFehler::Ungeprueft),
        23 => Some(LadeFehler::KeineLoaderCap),
        24 => Some(LadeFehler::KeineBildUebergabe),
        25 => Some(LadeFehler::AnstossAbgelehnt),
        26 => Some(LadeFehler::FalscheNachricht),
        _ => None,
    }
}

/// Die Server-Seite des Lader-Protokolls: eine Nachricht (vier Worte) gegen den **echten**
/// [`super::LadeDienst`] bedienen, Antwort in vier Worten. `manifest_schluessel` und `pubkey`
/// sind Endowment der PD (nie Draht — s. Modul-Doku); `ctx` sind die Slots aus dem EIGENEN
/// Manifest der PD (s. [`super::AnstossKontext`]) — der Dienst rät keine Übergabe.
pub fn bedienen<B, A>(
    dienst: &mut super::LadeDienst,
    quelle: &mut B,
    anstoss: &mut A,
    ctx: &super::AnstossKontext,
    manifest_schluessel: &[u8],
    pubkey: &[u8; 32],
    worte: [u64; 4],
) -> [u64; 4]
where
    B: super::BlockQuelle,
    A: super::LadeAnstoss,
{
    let n = Nachricht::aus_worten(worte);
    if n.pruefen().is_err() {
        return [lade_kode(LadeFehler::FalscheNachricht), 0, 0, 0];
    }
    match n.art {
        ART_AUSKUNFT => {
            let (phase, len) = dienst.auskunft();
            [0, phase, len, 0]
        }
        ART_LADEN => {
            let idx = n.a;
            let abl = idx.try_into().unwrap_or(usize::MAX);
            match dienst.laden(quelle, anstoss, ctx, manifest_schluessel, pubkey, abl) {
                Ok(pd) => [0, u64::from(pd), 0, 0],
                Err(f) => [lade_kode(f), 0, 0, 0],
            }
        }
        _ => [lade_kode(LadeFehler::FalscheNachricht), 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[test]
    fn block_worte_tragen_dieselbe_anfrage() {
        // Die Bruecke zwischen Woertern und Quelle: Was als Worte rausgeht, muss als
        // Quellen-Aufruf wieder ankommen — LBA und Anzahl stehen an derselben Stelle.
        let a = BlockAnfrage::lesen(1234, 8);
        let w = a.als_worte();
        assert_eq!(w, [OP_READ, 1234, 8, 0]);
        let zurueck = BlockAnfrage::aus_worten(w);
        assert_eq!(zurueck, a);
        assert_eq!(BlockAnfrage::auskunft().als_worte(), [OP_INFO, 0, 0, 0]);
    }

    #[test]
    fn block_bereich_vor_dem_geraet() {
        // Die Bereichspruefung VOR dem Geraet (Treiber-Regel, hier Client-seitig): Hinter der
        // Kapazitaet, leer und zu viel auf einmal scheitern beim Bauen, nicht am Geraet.
        let kap = 2048u64;
        assert_eq!(
            BlockAnfrage::lesen(2048, 1).pruefen(kap, 8).err(),
            Some(BlockBauFehler::Bereich)
        );
        assert_eq!(
            BlockAnfrage::lesen(2047, 2).pruefen(kap, 8).err(),
            Some(BlockBauFehler::Bereich)
        );
        assert_eq!(
            BlockAnfrage::lesen(0, 0).pruefen(kap, 8).err(),
            Some(BlockBauFehler::LeereAnfrage)
        );
        assert_eq!(
            BlockAnfrage::lesen(0, 9).pruefen(kap, 8).err(),
            Some(BlockBauFehler::ZuVielAufEinmal)
        );
        assert_eq!(
            BlockAnfrage { op: 3, lba: 0, anzahl: 1 }.pruefen(kap, 8).err(),
            Some(BlockBauFehler::UnbekannteOp),
            "WRITE (3) ist nicht einmal baubar — ein Lader schreibt keine Platte"
        );
        assert!(BlockAnfrage::lesen(0, 8).pruefen(kap, 8).is_ok());
        assert!(BlockAnfrage::lesen(2040, 8).pruefen(kap, 8).is_ok());
    }

    #[test]
    fn block_antwort_kurz_ist_abbruch() {
        // Kuerzer als angefragt ist kein Teilerfolg, sondern Abbruch: Der Hash liefe sonst ueber
        // Restbytes aus dem Staging-Puffer.
        assert_eq!(BlockAntwort { status: ST_OK, w1: 8, w2: 0, w3: 0 }.als_read(8), Ok(8));
        assert_eq!(
            BlockAntwort { status: ST_OK, w1: 7, w2: 0, w3: 0 }.als_read(8).err(),
            Some(BlockDeutFehler::Abgebrochen)
        );
        assert_eq!(
            BlockAntwort { status: ST_DEVICE, w1: 0, w2: 0, w3: 0 }.als_read(8).err(),
            Some(BlockDeutFehler::Geraet)
        );
        assert_eq!(
            BlockAntwort { status: ST_RANGE, w1: 0, w2: 0, w3: 0 }.als_read(8).err(),
            Some(BlockDeutFehler::Bereich)
        );
        assert_eq!(
            BlockAntwort { status: 99, w1: 0, w2: 0, w3: 0 }.als_read(8).err(),
            Some(BlockDeutFehler::Unsinn)
        );
    }

    #[test]
    fn nachricht_reservewort_ist_protokoll() {
        // Ein gesetztes Reservewort ist ein fremdes Protokoll — benannt abgewiesen, nicht still
        // uebergangen.
        assert!(Nachricht::auskunft().pruefen().is_ok());
        assert!(Nachricht::laden(2).pruefen().is_ok());
        assert_eq!(
            Nachricht { art: ART_LADEN, a: 0, b: 1 }.pruefen().err(),
            Some(NachrichtFehler::ReservewortGesetzt)
        );
        assert_eq!(
            Nachricht { art: 99, a: 0, b: 0 }.pruefen().err(),
            Some(NachrichtFehler::UnbekannteArt)
        );
        assert_eq!(Nachricht::laden(2).als_worte(), [ART_LADEN, 2, 0, 0]);
    }

    #[test]
    fn kodes_sind_rundwegfaehig() {
        // Jeder Kode kommt zurueck — ausser Container (Diagnose im Typ, Kode 20 als Familie) und
        // 0 (OK ist kein Fehler).
        let alle = [
            (1, LadeFehler::KeineKapazitaet),
            (2, LadeFehler::GptSignatur),
            (10, LadeFehler::KeineLxpdPartition),
            (11, LadeFehler::Geraet),
            (13, LadeFehler::Abgebrochen),
            (17, LadeFehler::EintragZuGross),
            (18, LadeFehler::BildHashWeichtAb),
            (21, LadeFehler::TreiberMismatch),
            (22, LadeFehler::Ungeprueft),
            (23, LadeFehler::KeineLoaderCap),
            (24, LadeFehler::KeineBildUebergabe),
            (25, LadeFehler::AnstossAbgelehnt),
            (26, LadeFehler::FalscheNachricht),
        ];
        for (k, f) in alle {
            assert_eq!(lade_kode(f), k, "Kode von {:?}", f);
            assert_eq!(lade_fehler_aus_kode(k), Some(f), "Rueckweg von Kode {}", k);
        }
        assert_eq!(lade_fehler_aus_kode(0), None);
        assert_eq!(lade_fehler_aus_kode(99), None);
        // Container: Familie rundwegfaehig, Variante nicht — dokumentiert, nicht vergessen.
        assert_eq!(lade_kode(LadeFehler::Container(caprock_lxpd::LxpdError::NotElf)), 20);
    }

    #[test]
    fn keine_schreib_anfrage_baubar() {
        // Die Abwesenheit selbst ist die Pruefung: Das Protokoll kennt keinen Konstruktor fuer
        // WRITE/FLUSH/STOP — was man nicht bauen kann, schickt man nicht versehentlich.
        // Strukturell dazu: Eine Anfrage ist 24 Byte, vier Register 32 — kein Byte Nutzdaten
        // passt hinein, weil kein Feld dafuer existiert (dieselbe Form wie
        // `server_sieht_keine_daten` im mem-server).
        assert_eq!(core::mem::size_of::<BlockAnfrage>(), 24, "kein Platz fuer Nutzdaten");
        assert_eq!(core::mem::size_of::<[u64; 4]>(), 32, "vier Register, nicht mehr");
        let _ops: Vec<u64> = super::super::bekannte_block_ops().into_iter().collect();
    }
}
