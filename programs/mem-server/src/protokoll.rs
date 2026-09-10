//! **Laufzeitspeicher-Protokoll PD→Server (Stufe 2).**
//!
//! Der Draht sind CALL/REPLY-Worte (`[u64; 4]`, vier Register — mehr trägt ein REPLY nicht):
//!
//! ```text
//! Client → Server   [ART, A, B, 0]
//!   ANFORDERN (1):  A = Länge, B = Zweck-Kennziffer → [KODE, Handle, 0, 0]
//!   AUFLOESEN (2):  A = Handle                     → [KODE, CPU, DEV, Länge]
//!   ABBILDEN  (3):  A = Handle („habe per SYS_MAP abgebildet")
//!   AUSBLENDEN(4):  A = Handle („habe per SYS_UNMAP entfernt")
//!   FREIGEBEN (5):  A = Handle
//! Server → Client   [ENTZUG (6), Handle, 0, 0]  („bilde aus, der Schein ist tot")
//!
//! KODE 0 = OK, sonst [`fehler_kode`]. Die Antwort auf ANFORDERN trägt bewusst NUR das
//! Handle: Die Koordinaten (CPU/DEV/Länge) holt der zweite Schritt (AUFLOESEN — der Lesepfad
//! vor `SYS_MAP`). Ein REPLY mit vier Registern könnte beides tragen; zwei Schritte trennen
//! aber Vergabe (Politik: wer bekommt was) von Auflösung (Mechanik: wo liegt es) — und genau
//! diese Trennung prüft „zweite PD sieht nichts" auf dem Draht statt in einer Struktur.
//! ```
//!
//! ## Was hier läuft und was gestellt ist
//!
//! [`bedienen`] ist die Server-Seite gegen einen Fake-Peer (der echte `MemoryServer`, keine
//! Nachbildung); [`ClientSeite`] ist die Client-Zustandsmaschine (Scheine, Sichten, Abbildungen).
//! Gestellt ist der Transport selbst: Statt echter Caps trägt der Schein Handle + Offset +
//! Länge, statt gemappter Frames liegt im Test ein Schatten-`Vec`.
//!
//! ## Was zum echten Transfer fehlt (Kernel-Hilfe, exakter Patch-Text in der Aufgabe)
//!
//! `CCOPY` leitet *dasselbe* Objekt ab — einen *Teilbereich* als eigene Memory-Cap gibt es
//! nicht. Der Vorschlag ist seit 2026-09-10 gebaut (`CSUB`, syscall **37** — s.
//! `caprock_abi::sys::CSUB`; Stand dieses Dokuments: 31, ueberholt):
//! `x1` = Quell-Slot (Memory-Cap), `MSG0` = freier Ziel-Slot, `MSG1` = Offset,
//! `MSG2` = Länge → abgeleitete Memory-Cap mit verengter Region, Rechten aus dem Schnitt,
//! Fehlern `ERR_BADCAP` / `ERR_NOSPACE` / `ERR_SUBREGION` (21, existiert seit K1b).
//! Erst damit wird aus dem Schein eine Cap, die der Client per `SYS_MAP` abbildet und der
//! Server per `revoke` tatsächlich einzieht — inklusive der Abbildung (heute räumt erst
//! `destroy_pd`/`vspace_teardown` Mappings ab; bei lebender PD bleibt der Entzug kooperativ,
//! s. [`SpeicherFehler::NochAbgebildet`](super::SpeicherFehler::NochAbgebildet)).
//!
//! Ebenfalls nicht hier (benannt, nicht gebaut): Demand-Paging (Seitenfehler holt nichts nach —
//! der `FaultHandler`-Pfad stellt zu, mappt aber nicht) und COW (kein
//! geteilt-lesbar-mit-Kopie-beim-Schreiben; lebende Grants überlappen grundsätzlich nie).
//!
//! ## Die Zusicherung, auf die es ankommt
//!
//! „Zweite PD sieht nichts" ist hier eine **Transport**-Eigenschaft: Alle Isolationstests
//! sprechen den Server ausschließlich über Worte ([`bedienen`]) an — kein direkter Aufruf,
//! keine geteilte Struktur. Was der Test über den Draht nicht auflösen kann, kann eine PD
//! über den Draht nicht auflösen. Und der Server sieht nie Client-Daten: Die Nachrichten
//! tragen Handles, Längen und Zwecke — keine Bytes (s. `server_sieht_keine_daten`).

use super::{MemoryServer, SpeicherFehler, Zweck};
use caprock_wait::Park;

/// Nachrichtenart Client → Server: Speicher anfordern (`A` = Länge, `B` = Zweck-Kennziffer).
pub const ART_ANFORDERN: u64 = 1;
/// Nachrichtenart Client → Server: Schein auflösen (`A` = Handle).
pub const ART_AUFLOESEN: u64 = 2;
/// Nachrichtenart Client → Server: Abbildung per `SYS_MAP` melden (`A` = Handle).
pub const ART_ABBILDEN: u64 = 3;
/// Nachrichtenart Client → Server: Entfernung per `SYS_UNMAP` melden (`A` = Handle).
pub const ART_AUSBLENDEN: u64 = 4;
/// Nachrichtenart Client → Server: Schein zurückgeben (`A` = Handle).
pub const ART_FREIGEBEN: u64 = 5;
/// Nachrichtenart Server → Client: Schein entzogen (`A` = Handle — ausblenden, dann freigeben).
pub const ART_ENTZUG: u64 = 6;

/// Eine Nachricht auf dem Draht: Art plus zwei Worte Nutzlast. Genau vier Register — was nicht
/// hineinpasst, gehört in einen zweiten Schritt (s. Modul-Doku).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Nachricht {
    /// Die Art (`ART_*`).
    pub art: u64,
    /// Erstes Nutzwort (Bedeutung je Art).
    pub a: u64,
    /// Zweites Nutzwort (nur ANFORDERN nutzt es: Zweck-Kennziffer).
    pub b: u64,
}

impl Nachricht {
    /// Anfrage bauen: Länge plus Zweck-Kennziffer (unbekannte Kennziffern sind absichtlich
    /// baubar — der Server antwortet [`SpeicherFehler::ZweckAbgelehnt`], s. `zweck_politik`).
    pub fn anfordern(len: u64, zweck_kennziffer: u64) -> Self {
        Nachricht { art: ART_ANFORDERN, a: len, b: zweck_kennziffer }
    }

    /// Schein auflösen / Abbilden melden / Ausblenden melden / Freigeben — je ein Handle.
    pub fn aufloesen(handle: u64) -> Self {
        Nachricht { art: ART_AUFLOESEN, a: handle, b: 0 }
    }

    /// Abbilden melden.
    pub fn abbilden(handle: u64) -> Self {
        Nachricht { art: ART_ABBILDEN, a: handle, b: 0 }
    }

    /// Ausblenden melden.
    pub fn ausblenden(handle: u64) -> Self {
        Nachricht { art: ART_AUSBLENDEN, a: handle, b: 0 }
    }

    /// Freigeben.
    pub fn freigeben(handle: u64) -> Self {
        Nachricht { art: ART_FREIGEBEN, a: handle, b: 0 }
    }

    /// Serverseitiger Entzug (Richtung Server → Client).
    pub fn entzug(handle: u64) -> Self {
        Nachricht { art: ART_ENTZUG, a: handle, b: 0 }
    }

    /// Auf den Draht: vier Register.
    pub fn als_worte(self) -> [u64; 4] {
        [self.art, self.a, self.b, 0]
    }

    /// Vom Draht lesen.
    pub fn aus_worten(w: [u64; 4]) -> Self {
        Nachricht { art: w[0], a: w[1], b: w[2] }
    }
}

/// Fehlerkode auf dem Draht: 0 = OK, sonst der benannte Ausgang. Jeder Fehler hat genau einen
/// Kode — „geht nicht" allein sagte nicht, ob der Aufrufer kleiner, später oder gar nicht
/// fragen soll.
pub fn fehler_kode(f: SpeicherFehler) -> u64 {
    match f {
        SpeicherFehler::LeereAnfrage => 1,
        SpeicherFehler::KeinPlatz => 2,
        SpeicherFehler::TabelleVoll => 3,
        SpeicherFehler::UnbekannterSchein => 4,
        SpeicherFehler::FremderSchein => 5,
        SpeicherFehler::BereitsZurueck => 6,
        SpeicherFehler::ZweckAbgelehnt => 7,
        SpeicherFehler::KontingentErschoepft => 8,
        SpeicherFehler::NochAbgebildet => 9,
        SpeicherFehler::NichtAbgebildet => 10,
        SpeicherFehler::Entzogen => 11,
        SpeicherFehler::UnbekannteArt => 12,
        SpeicherFehler::Sperre(_) => 13,
    }
}

/// Kode zurück — `None` heisst: diesen Kode vergibt der Server nie.
pub fn fehler_aus_kode(k: u64) -> Option<SpeicherFehler> {
    match k {
        0 => None,
        1 => Some(SpeicherFehler::LeereAnfrage),
        2 => Some(SpeicherFehler::KeinPlatz),
        3 => Some(SpeicherFehler::TabelleVoll),
        4 => Some(SpeicherFehler::UnbekannterSchein),
        5 => Some(SpeicherFehler::FremderSchein),
        6 => Some(SpeicherFehler::BereitsZurueck),
        7 => Some(SpeicherFehler::ZweckAbgelehnt),
        8 => Some(SpeicherFehler::KontingentErschoepft),
        9 => Some(SpeicherFehler::NochAbgebildet),
        10 => Some(SpeicherFehler::NichtAbgebildet),
        11 => Some(SpeicherFehler::Entzogen),
        12 => Some(SpeicherFehler::UnbekannteArt),
        // Die Sperre ist server-lokal (vor jeder Antwort) und geht nie über den Draht —
        // sonderbar wäre sie als Kode trotzdem nicht: Kode 13 nennt sie beim Namen, statt
        // sie in „unbekannt" zu falten.
        13 => Some(SpeicherFehler::Sperre(caprock_wait::LockError::KeinWarteplatz)),
        _ => None,
    }
}

/// Die Server-Seite des Protokolls: eine Nachricht (vier Worte) gegen den **echten**
/// [`MemoryServer`] bedienen, Antwort in vier Worten. `pd` ist die Absender-PD — die
/// Isolation steckt darin, dass jede Prüfung hier durchgeht statt direkt aufzurufen.
pub fn bedienen(
    s: &mut MemoryServer,
    p: &dyn Park,
    pd: u32,
    worte: [u64; 4],
) -> [u64; 4] {
    let n = Nachricht::aus_worten(worte);
    let ok = |x: u64, y: u64, z: u64| [0, x, y, z];
    let ab = |f: SpeicherFehler| [fehler_kode(f), 0, 0, 0];
    match n.art {
        ART_ANFORDERN => {
            let zweck = match Zweck::aus_kennziffer(n.b) {
                Some(z) => z,
                None => return ab(SpeicherFehler::ZweckAbgelehnt),
            };
            match s.anfordern_mit_zweck(p, pd, n.a, zweck) {
                Ok(schein) => ok(schein.handle, 0, 0),
                Err(f) => ab(f),
            }
        }
        ART_AUFLOESEN => match s.aufloesen(p, pd, n.a) {
            Ok(sicht) => ok(sicht.cpu, sicht.dev, sicht.len),
            Err(f) => ab(f),
        },
        ART_ABBILDEN => match s.abbilden_melden(p, pd, n.a) {
            Ok(()) => ok(0, 0, 0),
            Err(f) => ab(f),
        },
        ART_AUSBLENDEN => match s.ausblenden_melden(p, pd, n.a) {
            Ok(()) => ok(0, 0, 0),
            Err(f) => ab(f),
        },
        ART_FREIGEBEN => match s.zurueckgeben(p, pd, n.a) {
            Ok(()) => ok(0, 0, 0),
            Err(f) => ab(f),
        },
        _ => ab(SpeicherFehler::UnbekannteArt),
    }
}

/// Was der Client falsch machen kann, ohne dass der Server je gefragt wird. Eigene Absagen
/// statt stiller Worte: Wer abbildet, was er nicht kennt, schickt nichts — er scheitert hier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClientFehler {
    /// Der Server hat abgesagt (benannt, s. [`fehler_kode`]).
    Protokoll(SpeicherFehler),
    /// Dieses Handle kennt der Client nicht — kein Wort geht raus.
    UnbekannterSchein,
    /// Dasselbe Handle zweimal als Antwort angenommen — kein Wort, kein zweiter Eintrag.
    DoppelSchein,
    /// Abbilden ohne vorheriges Auflösen: Der Client wüsste nicht einmal, was er abbildet.
    KeineSicht,
    /// Ausblenden ohne Abbilden.
    KeineAbbildung,
    /// Freigeben bei noch gemeldeter Abbildung — der Client hält sich selbst an, bevor der
    /// Server es müsste ([`SpeicherFehler::NochAbgebildet`], aber eine Runde früher).
    NochAbgebildet,
    /// Der Schein ist entzogen — auflösen und abbilden sind zu, ausblenden und freigeben offen.
    Entzogen,
    /// Die Antwort passt nicht zur Frage (falsche Länge, Kode ohne Namen).
    UnerwarteteAntwort,
}

/// Zustand eines Client-Eintrags: Schein → Sicht → Abbildung, Entzug von überall.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClientZustand {
    /// Handle bekannt, Lage unbekannt.
    Schein,
    /// Aufgelöst, noch nicht abgebildet.
    SichtBekannt { cpu: u64, dev: u64 },
    /// Per `SYS_MAP` abgebildet (serverseitig gemeldet).
    Abgebildet { cpu: u64, dev: u64 },
    /// Serverseitig entzogen; `noch_abgebildet` heisst: Das `SYS_UNMAP` steht noch aus — die
    /// ehrliche Stelle (der Zähler [`ClientSeite::offene_entzogene_abbildungen`] zählt genau
    /// diese).
    Entzogen { noch_abgebildet: bool },
}

#[derive(Clone, Copy)]
struct ClientEintrag {
    handle: u64,
    len: u64,
    zustand: ClientZustand,
}

/// Wie viele Scheine ein Client höchstens gleichzeitig führt (eigene Schranke, eigener Name
/// im Fehlerfall gibt es nicht — darüber antwortet der Server mit `TabelleVoll`).
pub const CLIENT_MAX: usize = 16;

/// Die Client-Zustandsmaschine: baut Nachrichten, verbucht Antworten, hält sich an die
/// Reihenfolge (anfordern → auflösen → abbilden → ausblenden → freigeben), bevor der Server
/// sie halten muss. Trägt **keine Bytes** — nur Handles, Längen und Sichten. Genau das ist
/// die Isolation „Server sieht Client-Daten nie" von der anderen Seite: Der Client schickt
/// keine.
pub struct ClientSeite {
    eintraege: [Option<ClientEintrag>; CLIENT_MAX],
}

impl ClientSeite {
    /// Leere Client-Seite.
    pub fn neu() -> Self {
        const LEER: Option<ClientEintrag> = None;
        ClientSeite { eintraege: [LEER; CLIENT_MAX] }
    }

    fn finden(&self, handle: u64) -> Option<usize> {
        let mut i = 0;
        while i < CLIENT_MAX {
            if let Some(e) = self.eintraege[i] {
                if e.handle == handle {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    fn freien_platz(&self) -> Option<usize> {
        let mut i = 0;
        while i < CLIENT_MAX {
            if self.eintraege[i].is_none() {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// ANFORDERN bauen (Länge plus Zweck — das `mmap`-mit-Zweck der Stufe 2).
    pub fn anfrage(&self, len: u64, zweck: Zweck) -> Nachricht {
        Nachricht::anfordern(len, zweck.kennziffer())
    }

    /// Antwort auf ANFORDERN verbuchen: `[KODE, Handle, 0, 0]`. `erwartet_len` ist die
    /// angefragte Länge (die Antwort trägt keine — der Client kennt seine Frage).
    pub fn antwort_anfordern(
        &mut self,
        antwort: [u64; 4],
        erwartet_len: u64,
    ) -> Result<u64, ClientFehler> {
        if antwort[0] != 0 {
            return Err(ClientFehler::Protokoll(
                fehler_aus_kode(antwort[0]).ok_or(ClientFehler::UnerwarteteAntwort)?,
            ));
        }
        let handle = antwort[1];
        if handle == 0 || self.finden(handle).is_some() {
            return Err(ClientFehler::DoppelSchein);
        }
        let platz = self.freien_platz().ok_or(ClientFehler::UnerwarteteAntwort)?;
        let len = super::aufgerundet(erwartet_len).ok_or(ClientFehler::UnerwarteteAntwort)?;
        self.eintraege[platz] =
            Some(ClientEintrag { handle, len, zustand: ClientZustand::Schein });
        Ok(handle)
    }

    /// AUFLOESEN bauen.
    pub fn aufloesen(&self, handle: u64) -> Result<Nachricht, ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        let e = self.eintraege[i].expect("eben gefunden");
        if matches!(e.zustand, ClientZustand::Entzogen { .. }) {
            return Err(ClientFehler::Entzogen);
        }
        Ok(Nachricht::aufloesen(handle))
    }

    /// Antwort auf AUFLOESEN verbuchen: `[KODE, CPU, DEV, Länge]`. Die Länge muss zur Frage
    /// passen (aufgerundet) — sonst hat jemand geantwortet, der nicht der Server ist.
    pub fn antwort_aufloesen(
        &mut self,
        handle: u64,
        antwort: [u64; 4],
    ) -> Result<(u64, u64), ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        if antwort[0] != 0 {
            let f = fehler_aus_kode(antwort[0]).ok_or(ClientFehler::UnerwarteteAntwort)?;
            if f == SpeicherFehler::Entzogen {
                let e = self.eintraege[i].expect("eben gefunden");
                let noch = matches!(
                    e.zustand,
                    ClientZustand::Abgebildet { .. } | ClientZustand::Entzogen { noch_abgebildet: true }
                );
                self.eintraege[i] =
                    Some(ClientEintrag { zustand: ClientZustand::Entzogen { noch_abgebildet: noch }, ..e });
                return Err(ClientFehler::Entzogen);
            }
            return Err(ClientFehler::Protokoll(f));
        }
        let e = self.eintraege[i].expect("eben gefunden");
        if antwort[3] != e.len {
            return Err(ClientFehler::UnerwarteteAntwort);
        }
        let (cpu, dev) = (antwort[1], antwort[2]);
        self.eintraege[i] = Some(ClientEintrag {
            zustand: ClientZustand::SichtBekannt { cpu, dev },
            ..e
        });
        Ok((cpu, dev))
    }

    /// ABBILDEN bauen — erst nach AUFLOESEN. Der Client meldet, was er per `SYS_MAP` gelegt
    /// hat; ohne Sicht gäbe es nichts zu melden.
    pub fn abbilden(&self, handle: u64) -> Result<Nachricht, ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        let e = self.eintraege[i].expect("eben gefunden");
        match e.zustand {
            ClientZustand::SichtBekannt { .. } => Ok(Nachricht::abbilden(handle)),
            ClientZustand::Entzogen { .. } => Err(ClientFehler::Entzogen),
            _ => Err(ClientFehler::KeineSicht),
        }
    }

    /// Antwort auf ABBILDEN verbuchen.
    pub fn antwort_abbilden(
        &mut self,
        handle: u64,
        antwort: [u64; 4],
    ) -> Result<(), ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        if antwort[0] != 0 {
            let f = fehler_aus_kode(antwort[0]).ok_or(ClientFehler::UnerwarteteAntwort)?;
            if f == SpeicherFehler::Entzogen {
                let e = self.eintraege[i].expect("eben gefunden");
                self.eintraege[i] = Some(ClientEintrag {
                    zustand: ClientZustand::Entzogen { noch_abgebildet: false },
                    ..e
                });
                return Err(ClientFehler::Entzogen);
            }
            return Err(ClientFehler::Protokoll(f));
        }
        let e = self.eintraege[i].expect("eben gefunden");
        match e.zustand {
            ClientZustand::SichtBekannt { cpu, dev } => {
                self.eintraege[i] =
                    Some(ClientEintrag { zustand: ClientZustand::Abgebildet { cpu, dev }, ..e });
                Ok(())
            }
            _ => Err(ClientFehler::UnerwarteteAntwort),
        }
    }

    /// AUSBLENDEN bauen — nur bei gemeldeter Abbildung (oder entzogenem Rest).
    pub fn ausblenden(&self, handle: u64) -> Result<Nachricht, ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        let e = self.eintraege[i].expect("eben gefunden");
        match e.zustand {
            ClientZustand::Abgebildet { .. }
            | ClientZustand::Entzogen { noch_abgebildet: true } => {
                Ok(Nachricht::ausblenden(handle))
            }
            _ => Err(ClientFehler::KeineAbbildung),
        }
    }

    /// Antwort auf AUSBLENDEN verbuchen.
    pub fn antwort_ausblenden(
        &mut self,
        handle: u64,
        antwort: [u64; 4],
    ) -> Result<(), ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        if antwort[0] != 0 {
            return Err(ClientFehler::Protokoll(
                fehler_aus_kode(antwort[0]).ok_or(ClientFehler::UnerwarteteAntwort)?,
            ));
        }
        let e = self.eintraege[i].expect("eben gefunden");
        match e.zustand {
            ClientZustand::Abgebildet { cpu, dev } => {
                self.eintraege[i] =
                    Some(ClientEintrag { zustand: ClientZustand::SichtBekannt { cpu, dev }, ..e });
                Ok(())
            }
            ClientZustand::Entzogen { noch_abgebildet: true } => {
                self.eintraege[i] = Some(ClientEintrag {
                    zustand: ClientZustand::Entzogen { noch_abgebildet: false },
                    ..e
                });
                Ok(())
            }
            _ => Err(ClientFehler::UnerwarteteAntwort),
        }
    }

    /// FREIGEBEN bauen — nur ohne gemeldete Abbildung. Der Client hält sich selbst an
    /// (kein Wort geht raus); der Server prüft es erneut (Transport gegen Direktaufruf).
    pub fn freigeben(&self, handle: u64) -> Result<Nachricht, ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        let e = self.eintraege[i].expect("eben gefunden");
        match e.zustand {
            ClientZustand::Abgebildet { .. }
            | ClientZustand::Entzogen { noch_abgebildet: true } => {
                Err(ClientFehler::NochAbgebildet)
            }
            _ => Ok(Nachricht::freigeben(handle)),
        }
    }

    /// Antwort auf FREIGEBEN verbuchen: Bei OK erlischt der Eintrag (der Schein ist tot —
    /// jede weitere Nutzung scheitert client-seitig, bevor sie den Draht erreicht).
    pub fn antwort_freigeben(
        &mut self,
        handle: u64,
        antwort: [u64; 4],
    ) -> Result<(), ClientFehler> {
        let i = self.finden(handle).ok_or(ClientFehler::UnbekannterSchein)?;
        if antwort[0] != 0 {
            return Err(ClientFehler::Protokoll(
                fehler_aus_kode(antwort[0]).ok_or(ClientFehler::UnerwarteteAntwort)?,
            ));
        }
        self.eintraege[i] = None;
        Ok(())
    }

    /// Entzug vom Server verarbeiten (`[ENTZUG, Handle, 0, 0]`). `false` = unbekanntes Handle
    /// (kein Zustand geändert). Die Abbildung bleibt dabei gemeldet, bis sie ausgeblendet wird —
    /// genau diese offenen Reste zählt [`Self::offene_entzogene_abbildungen`].
    pub fn entzug_verarbeiten(&mut self, handle: u64) -> bool {
        let Some(i) = self.finden(handle) else { return false };
        let e = self.eintraege[i].expect("eben gefunden");
        let noch = matches!(
            e.zustand,
            ClientZustand::Abgebildet { .. } | ClientZustand::Entzogen { noch_abgebildet: true }
        );
        self.eintraege[i] =
            Some(ClientEintrag { zustand: ClientZustand::Entzogen { noch_abgebildet: noch }, ..e });
        true
    }

    /// Entzogene, aber noch abgebildete Grants: Speicher, den der Server bereits eingezogen hat
    /// und den der Client noch liest. Kein Fehlerzähler, sondern die benannte Grenze des
    /// Protokolls — solange hier etwas steht, ist die Wiedervergabe blockiert (der Server hält
    /// die Region reserviert) und der Client liest Speicher, der ihm nicht mehr gehört.
    pub fn offene_entzogene_abbildungen(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < CLIENT_MAX {
            if let Some(e) = self.eintraege[i] {
                if matches!(e.zustand, ClientZustand::Entzogen { noch_abgebildet: true }) {
                    n += 1;
                }
            }
            i += 1;
        }
        n
    }

    /// Geführte Scheine (Test- und Bilanzhilfe).
    pub fn scheine(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < CLIENT_MAX {
            if self.eintraege[i].is_some() {
                n += 1;
            }
            i += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        muster, ABNAHME_GRANT, ARENA_BYTES, ARENA_CPU_BASIS, ARENA_DEV_BASIS, MAX_GRANTS,
        MemoryServer, PAGE_SIZE, PD_BOOT, START_KAPITAL, Zweck,
    };
    use super::*;
    use caprock_wait::Tid;
    use core::cell::RefCell;
    use std::vec::Vec;

    const PD_A: u32 = 1;
    const PD_B: u32 = 2;
    const TEST_TID: Tid = 1;

    /// Stand-in für den Kernel (wie in `super::tests`, eigene Kopie — kein geteilter Zustand).
    struct FakePark {
        ich: Tid,
        marken: RefCell<[u32; 8]>,
        blockiert: RefCell<u32>,
        weckrufe: RefCell<u32>,
    }

    impl FakePark {
        fn neu(ich: Tid) -> Self {
            FakePark {
                ich,
                marken: RefCell::new([0; 8]),
                blockiert: RefCell::new(0),
                weckrufe: RefCell::new(0),
            }
        }
    }

    impl Park for FakePark {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            let mut m = self.marken.borrow_mut();
            if m[self.ich as usize] > 0 {
                m[self.ich as usize] -= 1;
            } else {
                *self.blockiert.borrow_mut() += 1;
            }
        }
        fn unpark(&self, t: Tid) {
            self.marken.borrow_mut()[t as usize] += 1;
            *self.weckrufe.borrow_mut() += 1;
        }
    }

    fn server() -> MemoryServer {
        MemoryServer::neu(ARENA_CPU_BASIS, ARENA_DEV_BASIS, ARENA_BYTES).expect("Arena")
    }

    /// Gestellter Client-Speicher: Schatten-Bytes, die NUR der Client anfasst. Der Server kennt
    /// weder diesen Typ noch eine Instanz davon — strukturell, nicht per Disziplin.
    struct ClientSpeicher {
        schatten: Vec<u8>,
    }

    impl ClientSpeicher {
        fn neu() -> Self {
            ClientSpeicher { schatten: std::vec![0u8; ARENA_BYTES as usize] }
        }
        fn offset_von_cpu(cpu: u64) -> usize {
            (cpu - ARENA_CPU_BASIS) as usize
        }
        fn schreiben(&mut self, cpu: u64, len: u64, handle: u64) {
            let o = Self::offset_von_cpu(cpu);
            assert!(o + len as usize <= self.schatten.len(), "Sicht liegt im Schatten");
            let mut i = 0u64;
            while i < len {
                self.schatten[o + i as usize] = muster(handle, i);
                i += 1;
            }
        }
        fn pruefen(&self, cpu: u64, len: u64, handle: u64) -> bool {
            let o = Self::offset_von_cpu(cpu);
            let mut i = 0u64;
            while i < len {
                if self.schatten[o + i as usize] != muster(handle, i) {
                    return false;
                }
                i += 1;
            }
            true
        }
        fn nullen(&self, cpu: u64, len: u64) -> bool {
            let o = Self::offset_von_cpu(cpu);
            self.schatten[o..o + len as usize].iter().all(|&x| x == 0)
        }
    }

    /// Voller Vergabe-Rundweg über Worte: anfordern → auflösen → schreiben → abbilden →
    /// ausblenden → freigeben. Nur [`bedienen`] spricht mit dem Server. `beschreiben` = false
    /// lässt den Bereich unbeschrieben (frische Region einer zweiten PD: Nullen statt Muster).
    fn rundweg(
        s: &mut MemoryServer,
        p: &FakePark,
        c: &mut ClientSeite,
        speicher: &mut ClientSpeicher,
        pd: u32,
        len: u64,
        zweck: Zweck,
        beschreiben: bool,
    ) -> u64 {
        let aw = bedienen(s, p, pd, c.anfrage(len, zweck).als_worte());
        let h = c.antwort_anfordern(aw, len).expect("anfordern ok");
        let lw = bedienen(s, p, pd, c.aufloesen(h).expect("eintrag").als_worte());
        assert_eq!(lw[0], 0, "aufloesen ok");
        let (cpu, _dev) = c.antwort_aufloesen(h, lw).expect("sicht ok");
        if beschreiben {
            speicher.schreiben(cpu, c.finden(h).map(|i| c.eintraege[i].expect("e").len).unwrap_or(0), h);
        }
        let bw = bedienen(s, p, pd, c.abbilden(h).expect("sicht da").als_worte());
        c.antwort_abbilden(h, bw).expect("abbilden ok");
        h
    }

    fn rueckweg(
        s: &mut MemoryServer,
        p: &FakePark,
        c: &mut ClientSeite,
        pd: u32,
        h: u64,
    ) {
        let uw = bedienen(s, p, pd, c.ausblenden(h).expect("abgebildet").als_worte());
        c.antwort_ausblenden(h, uw).expect("ausblenden ok");
        let fw = bedienen(s, p, pd, c.freigeben(h).expect("nicht abgebildet").als_worte());
        c.antwort_freigeben(h, fw).expect("freigeben ok");
    }

    #[test]
    fn laufzeit_rundweg_ueber_worte() {
        // Stufe-2-Abnahme, Positivpfad: 1 MiB zur Laufzeit anfordern (Größe + Zweck), voll
        // schreiben, abbilden, ausblenden, zurückgeben — danach bilanziert die Arena exakt.
        // Exakte Ausgaben: erstes Handle nach dem Startkapital ist Gen 2 in Slot 1, Offset 4 KiB.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut c = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let h = rundweg(&mut s, &park, &mut c, &mut speicher, PD_A, ABNAHME_GRANT, Zweck::Allgemein, true);
        assert_eq!(h, (2u64 << 32) | 1, "erstes Laufzeit-Handle: Gen 2, Slot 1");
        let (cpu, dev) = match c.eintraege[c.finden(h).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, dev } => (cpu, dev),
            _ => panic!("abgebildet erwartet"),
        };
        assert_eq!(cpu, ARENA_CPU_BASIS + START_KAPITAL as u64);
        assert_eq!(dev, ARENA_DEV_BASIS + START_KAPITAL as u64);
        assert!(speicher.pruefen(cpu, ABNAHME_GRANT, h), "jedes Byte geschrieben und lesbar");
        assert_eq!(s.grants_live(), 2, "Startkapital + Grant");
        assert_eq!(s.zweck_benutzt(Zweck::Allgemein), START_KAPITAL + ABNAHME_GRANT);
        rueckweg(&mut s, &park, &mut c, PD_A, h);
        assert_eq!(c.scheine(), 0, "Client führt nichts mehr");
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
        assert_eq!(s.zweck_benutzt(Zweck::Allgemein), START_KAPITAL);
    }

    #[test]
    fn zweite_pd_sieht_nichts_transport() {
        // Isolation als Transport-Eigenschaft: PD_B spricht den Server NUR über Worte an —
        // kein Direktaufruf, keine geteilte Struktur — und bekommt für PD_As Handle dreimal
        // die benannte Absage. Ihr frischer Bereich zeigt Nullen statt Muster.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut ca = ClientSeite::neu();
        let mut cb = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let ha = rundweg(&mut s, &park, &mut ca, &mut speicher, PD_A, ABNAHME_GRANT, Zweck::Allgemein, true);

        // B löst fremd auf — über den Draht.
        let fremd = bedienen(&mut s, &park, PD_B, Nachricht::aufloesen(ha).als_worte());
        assert_eq!(fremd[0], fehler_kode(SpeicherFehler::FremderSchein));
        // B bildet fremd ab.
        let fremd_ab = bedienen(&mut s, &park, PD_B, Nachricht::abbilden(ha).als_worte());
        assert_eq!(fremd_ab[0], fehler_kode(SpeicherFehler::FremderSchein));
        // B gibt fremd zurück.
        let fremd_fr = bedienen(&mut s, &park, PD_B, Nachricht::freigeben(ha).als_worte());
        assert_eq!(fremd_fr[0], fehler_kode(SpeicherFehler::FremderSchein));

        // B bekommt eine disjunkte eigene Region und sieht dort Nullen.
        let hb = rundweg(&mut s, &park, &mut cb, &mut speicher, PD_B, PAGE_SIZE, Zweck::Allgemein, false);
        let (cpu_b, _) = match cb.eintraege[cb.finden(hb).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, dev } => (cpu, dev),
            _ => panic!("abgebildet erwartet"),
        };
        let (cpu_a, _) = match ca.eintraege[ca.finden(ha).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, dev } => (cpu, dev),
            _ => panic!("abgebildet erwartet"),
        };
        assert!(cpu_b < cpu_a || cpu_b >= cpu_a + ABNAHME_GRANT, "disjunkte Bereiche");
        assert!(speicher.nullen(cpu_b, PAGE_SIZE), "frischer Bereich: Nullen, kein Muster");
        assert!(speicher.pruefen(cpu_a, ABNAHME_GRANT, ha), "A unversehrt");

        rueckweg(&mut s, &park, &mut ca, PD_A, ha);
        rueckweg(&mut s, &park, &mut cb, PD_B, hb);
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
    }

    #[test]
    fn doppelvergabe_und_use_after_free() {
        // Doppelvergabe: Derselbe Bereich kommt erst nach ordnungsgemäßer Rückgabe zurück
        // (First-fit: derselbe Offset, neues Handle). Use-after-free: Der alte Schein löst
        // danach NICHT auf — weder direkt noch über Worte, weder zum Lesen noch zum Abbilden.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut ca = ClientSeite::neu();
        let mut cb = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let ha = rundweg(&mut s, &park, &mut ca, &mut speicher, PD_A, ABNAHME_GRANT, Zweck::Allgemein, true);
        let (cpu_a, _) = match ca.eintraege[ca.finden(ha).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, dev } => (cpu, dev),
            _ => panic!("abgebildet erwartet"),
        };
        rueckweg(&mut s, &park, &mut ca, PD_A, ha);

        // B bekommt denselben Offset mit neuem Handle.
        let hb = rundweg(&mut s, &park, &mut cb, &mut speicher, PD_B, ABNAHME_GRANT, Zweck::Allgemein, true);
        assert_ne!(hb, ha, "neuer Schein, neues Handle");
        let (cpu_b, _) = match cb.eintraege[cb.finden(hb).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, dev } => (cpu, dev),
            _ => panic!("abgebildet erwartet"),
        };
        assert_eq!(cpu_b, cpu_a, "First-fit: derselbe Bereich, neue PD");

        // As alter Schein ist tot — über den Draht, in allen drei Rollen.
        let alt_auf = bedienen(&mut s, &park, PD_A, Nachricht::aufloesen(ha).als_worte());
        assert_eq!(alt_auf[0], fehler_kode(SpeicherFehler::UnbekannterSchein));
        let alt_ab = bedienen(&mut s, &park, PD_A, Nachricht::abbilden(ha).als_worte());
        assert_eq!(alt_ab[0], fehler_kode(SpeicherFehler::UnbekannterSchein));
        let alt_fr = bedienen(&mut s, &park, PD_A, Nachricht::freigeben(ha).als_worte());
        assert_eq!(alt_fr[0], fehler_kode(SpeicherFehler::UnbekannterSchein));
        // Client-seitig ist er ohnehin erloschen: kein Eintrag, kein Wort.
        assert_eq!(ca.aufloesen(ha).err(), Some(ClientFehler::UnbekannterSchein));

        rueckweg(&mut s, &park, &mut cb, PD_B, hb);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
    }

    #[test]
    fn noch_abgebildet_revoke_form() {
        // Die Revoke-Form: Freigabe bei gemeldeter Abbildung scheitert — client-seitig (kein
        // Wort) UND server-seitig (benannt, für den Fall, dass jemand direkt aufruft). Die
        // Region wird solange NICHT wiedervergeben: B bekommt einen anderen Offset.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut ca = ClientSeite::neu();
        let mut cb = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let ha = rundweg(&mut s, &park, &mut ca, &mut speicher, PD_A, ABNAHME_GRANT, Zweck::Allgemein, true);

        // Client hält sich selbst an: kein Wort geht raus.
        assert_eq!(ca.freigeben(ha).err(), Some(ClientFehler::NochAbgebildet));
        // Wer direkt aufruft, bekommt die benannte Absage über den Draht.
        let direkt = bedienen(&mut s, &park, PD_A, Nachricht::freigeben(ha).als_worte());
        assert_eq!(direkt[0], fehler_kode(SpeicherFehler::NochAbgebildet));

        // B bekommt NICHT denselben Bereich (er ist noch abgebildet, nicht frei). 512 KiB
        // statt 1 MiB: Die Arena (2 MiB) fasst keine zwei 1-MiB-Grants plus Startkapital.
        let hb = rundweg(&mut s, &park, &mut cb, &mut speicher, PD_B, 512 * 1024, Zweck::Allgemein, true);
        let off_a = ClientSpeicher::offset_von_cpu(match ca.eintraege[ca.finden(ha).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, .. } => cpu,
            _ => panic!("abgebildet erwartet"),
        }) as u64;
        let off_b = ClientSpeicher::offset_von_cpu(match cb.eintraege[cb.finden(hb).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, .. } => cpu,
            _ => panic!("abgebildet erwartet"),
        }) as u64;
        assert_ne!(off_b, off_a, "noch abgebildet heisst noch nicht wiedervergebbar");

        // Ordnungsgemäßer Weg: ausblenden, dann freigeben — danach ist die Region wieder da.
        rueckweg(&mut s, &park, &mut ca, PD_A, ha);
        rueckweg(&mut s, &park, &mut cb, PD_B, hb);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
    }

    #[test]
    fn entzug_benannt_und_reserviert() {
        // Serverseitiger Entzug (z. B. Aufräumpfad): Der Schein löst nicht mehr auf (ENTZOGEN,
        // nicht „unbekannt" und nicht „zurück"), die Region bleibt reserviert, bis sie
        // zurückgegeben wird. Der Client zählt den offenen Rest ehrlich.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut ca = ClientSeite::neu();
        let mut cb = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let ha = rundweg(&mut s, &park, &mut ca, &mut speicher, PD_A, ABNAHME_GRANT, Zweck::Allgemein, true);
        s.entziehen(&park, PD_A, ha).expect("entzug ok");

        // Auflösen und Abbilden sind zu — benannt.
        let auf = bedienen(&mut s, &park, PD_A, Nachricht::aufloesen(ha).als_worte());
        assert_eq!(auf[0], fehler_kode(SpeicherFehler::Entzogen));
        let ab = bedienen(&mut s, &park, PD_A, Nachricht::abbilden(ha).als_worte());
        assert_eq!(ab[0], fehler_kode(SpeicherFehler::Entzogen));

        // Die Region ist NICHT frei: B bekommt einen anderen Offset, die Bilanz steht.
        // (512 KiB — die Arena fasst keinen zweiten 1-MiB-Grant neben dem reservierten.)
        let hb = rundweg(&mut s, &park, &mut cb, &mut speicher, PD_B, 512 * 1024, Zweck::Allgemein, true);
        let off_b = ClientSpeicher::offset_von_cpu(match cb.eintraege[cb.finden(hb).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, .. } => cpu,
            _ => panic!("abgebildet erwartet"),
        }) as u64;
        assert_ne!(off_b, START_KAPITAL, "entzogen heisst reserviert, nicht frei");
        assert_eq!(
            s.vergeben_bytes(),
            START_KAPITAL + ABNAHME_GRANT + 512 * 1024,
            "entzogener Grant bilanziert weiter als vergeben"
        );

        // Client-Seite: Entzug trifft ein, Abbildung steht noch aus — offener Rest zählt.
        assert!(ca.entzug_verarbeiten(ha), "eigener Schein, bekannt");
        assert_eq!(ca.offene_entzogene_abbildungen(), 1);
        assert_eq!(ca.aufloesen(ha).err(), Some(ClientFehler::Entzogen));
        assert_eq!(ca.freigeben(ha).err(), Some(ClientFehler::NochAbgebildet));
        // Ausblenden geht noch, danach Freigeben — erst dann ist die Region wirklich frei.
        let uw = bedienen(&mut s, &park, PD_A, ca.ausblenden(ha).expect("noch abgebildet").als_worte());
        ca.antwort_ausblenden(ha, uw).expect("ausblenden ok");
        assert_eq!(ca.offene_entzogene_abbildungen(), 0);
        let fw = bedienen(&mut s, &park, PD_A, ca.freigeben(ha).expect("entzogen, aber frei").als_worte());
        ca.antwort_freigeben(ha, fw).expect("freigeben nach Entzug ok");
        // Entzogener Schein doppelt freigeben: benannt, kein doppelter Bereich.
        let doppelt = bedienen(&mut s, &park, PD_A, Nachricht::freigeben(ha).als_worte());
        assert_eq!(doppelt[0], fehler_kode(SpeicherFehler::BereitsZurueck));

        rueckweg(&mut s, &park, &mut cb, PD_B, hb);
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
    }

    #[test]
    fn erschoepfung_benannt() {
        // Drei Erschöpfungen, drei Namen: Kontingent (Zweck), Tabelle (Grants), Arena (Bytes).
        // Danach bedient der Server weiter — Erschöpfung ist Last, kein Zustand.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut c = ClientSeite::neu();
        s.kontingent_setzen(Zweck::DmaPuffer, PAGE_SIZE);

        // Kontingent: eine Seite geht, die zweite nicht.
        let a1 = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::DmaPuffer).als_worte());
        let h1 = c.antwort_anfordern(a1, PAGE_SIZE).expect("erste Seite ok");
        let a2 = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::DmaPuffer).als_worte());
        assert_eq!(a2[0], fehler_kode(SpeicherFehler::KontingentErschoepft));
        assert!(c.antwort_anfordern(a2, PAGE_SIZE).err().is_some(), "Absage wird kein Schein");
        assert_eq!(c.scheine(), 1, "nur der bediente Schein steht");
        // Anderer Zweck ist unberührt: Allgemein ist nicht das DmaPuffer-Kontingent.
        let a3 = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::Allgemein).als_worte());
        assert_eq!(a3[0], 0, "fremdes Kontingent, eigene Antwort");
        let h3 = c.antwort_anfordern(a3, PAGE_SIZE).expect("allgemein ok");

        // Tabelle: 16 Fächer, 3 belegt (Startkapital + h1 + h3) — 13 weitere füllen sie.
        let mut volle: Vec<u64> = Vec::new();
        while s.grants_live() < MAX_GRANTS {
            let aw = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::Allgemein).als_worte());
            // Client-Seite ist kleiner als die Server-Tabelle (16 = 16): Bei vollem Client
            // trägt der Test das Handle direkt (der Draht kennt keine Client-Grenze).
            if aw[0] != 0 {
                panic!("Tabelle sollte noch Platz haben, Kode {}", aw[0]);
            }
            volle.push(aw[1]);
        }
        let voll = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::Allgemein).als_worte());
        assert_eq!(voll[0], fehler_kode(SpeicherFehler::TabelleVoll));

        // Aufräumen: Client-Scheine plus direkte Handles zurückgeben (ausgeblendet war nie
        // etwas — abgebildet ist nichts, also ist die Rückgabe offen).
        for h in volle {
            s.zurueckgeben(&park, PD_A, h).expect("direkte Rueckgabe");
        }
        let f1 = bedienen(&mut s, &park, PD_A, Nachricht::freigeben(h1).als_worte());
        c.antwort_freigeben(h1, f1).expect("h1 zurueck");
        let f3 = bedienen(&mut s, &park, PD_A, Nachricht::freigeben(h3).als_worte());
        c.antwort_freigeben(h3, f3).expect("h3 zurueck");

        // Arena: 8 MiB passen in keine 2-MiB-Arena — danach geht es weiter.
        let riese = bedienen(
            &mut s,
            &park,
            PD_A,
            c.anfrage(8 * 1024 * 1024, Zweck::Allgemein).als_worte(),
        );
        assert_eq!(riese[0], fehler_kode(SpeicherFehler::KeinPlatz));
        let nach = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::DmaPuffer).als_worte());
        assert_eq!(nach[0], 0, "nach Freigabe ist das Kontingent wieder da");
        let hn = c.antwort_anfordern(nach, PAGE_SIZE).expect("wieder Platz");
        let fnw = bedienen(&mut s, &park, PD_A, Nachricht::freigeben(hn).als_worte());
        c.antwort_freigeben(hn, fnw).expect("Rueckgabe");
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
    }

    #[test]
    fn zweck_politik() {
        // Unbekannte Kennziffer und verbotener Zweck: beides ZWECKABGELEHNT über den Draht —
        // keine Mengenaussage, sondern Politik. Erlaubtes bleibt erlaubnisfähig.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let c = ClientSeite::neu();

        let fremd = bedienen(&mut s, &park, PD_A, Nachricht::anfordern(PAGE_SIZE, 99).als_worte());
        assert_eq!(fremd[0], fehler_kode(SpeicherFehler::ZweckAbgelehnt));

        s.zweck_verbieten(Zweck::Stapel, true);
        let verbot = bedienen(
            &mut s,
            &park,
            PD_A,
            c.anfrage(PAGE_SIZE, Zweck::Stapel).als_worte(),
        );
        assert_eq!(verbot[0], fehler_kode(SpeicherFehler::ZweckAbgelehnt));
        let erlaubt = bedienen(
            &mut s,
            &park,
            PD_A,
            c.anfrage(PAGE_SIZE, Zweck::Allgemein).als_worte(),
        );
        assert_eq!(erlaubt[0], 0, "Verbot trifft nur den verbotenen Zweck");
        s.zurueckgeben(&park, PD_A, erlaubt[1]).expect("Rueckgabe");
        s.zweck_verbieten(Zweck::Stapel, false);
        let wieder = bedienen(
            &mut s,
            &park,
            PD_A,
            c.anfrage(PAGE_SIZE, Zweck::Stapel).als_worte(),
        );
        assert_eq!(wieder[0], 0, "aufgehobenes Verbot fragt wieder");
        s.zurueckgeben(&park, PD_A, wieder[1]).expect("Rueckgabe");
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
    }

    #[test]
    fn server_sieht_keine_daten() {
        // Isolation von der anderen Seite: Zwei identische Nachrichtenfolgen mit verschiedenen
        // Nutzdaten führen zur identischen Server-Bilanz — der Server rechnet Offsets, keine
        // Bytes. Strukturell dazu: Eine Nachricht ist 24 Byte, eine Antwort 32 — kein Byte
        // Nutzdaten passt hinein, weil kein Feld dafür existiert.
        assert_eq!(core::mem::size_of::<Nachricht>(), 24, "kein Platz für Nutzdaten");
        assert_eq!(core::mem::size_of::<[u64; 4]>(), 32, "vier Register, nicht mehr");

        fn folge(payload: u8) -> (usize, u64, u64, u64) {
            let park = FakePark::neu(TEST_TID);
            let mut s = server();
            let mut c = ClientSeite::neu();
            let aw = bedienen(&mut s, &park, PD_A, c.anfrage(ABNAHME_GRANT, Zweck::Allgemein).als_worte());
            let h = c.antwort_anfordern(aw, ABNAHME_GRANT).expect("ok");
            let lw = bedienen(&mut s, &park, PD_A, c.aufloesen(h).expect("e").als_worte());
            let (cpu, _) = c.antwort_aufloesen(h, lw).expect("sicht");
            // Die Nutzdaten: Der Client beschreibt seinen Schatten — der Server sieht davon
            // nichts, weil keine Nachricht je ein Byte trägt.
            let mut schatten = std::vec![payload; ARENA_BYTES as usize];
            let o = ClientSpeicher::offset_von_cpu(cpu);
            let mut i = 0u64;
            while i < ABNAHME_GRANT {
                schatten[o + i as usize] = muster(h, i).wrapping_add(payload);
                i += 1;
            }
            let bw = bedienen(&mut s, &park, PD_A, c.abbilden(h).expect("e").als_worte());
            c.antwort_abbilden(h, bw).expect("ok");
            let bilanz =
                (s.grants_live(), s.vergeben_bytes(), s.freie_bytes(), s.zweck_benutzt(Zweck::Allgemein));
            let uw = bedienen(&mut s, &park, PD_A, c.ausblenden(h).expect("e").als_worte());
            c.antwort_ausblenden(h, uw).expect("ok");
            let fw = bedienen(&mut s, &park, PD_A, c.freigeben(h).expect("e").als_worte());
            c.antwort_freigeben(h, fw).expect("ok");
            assert_eq!(s.vergeben_bytes(), START_KAPITAL);
            bilanz
        }

        assert_eq!(folge(0x00), folge(0xFF), "Nutzdaten ändern nichts an der Server-Bilanz");
        assert_eq!(folge(0x00), (2, START_KAPITAL + ABNAHME_GRANT, ARENA_BYTES - START_KAPITAL - ABNAHME_GRANT, START_KAPITAL + ABNAHME_GRANT));
    }

    #[test]
    fn client_schutz_vor_sich_selbst() {
        // Die Client-Seite hält die Reihenfolge ein, bevor der Server gefragt wird: Abbilden
        // ohne Sicht, Ausblenden ohne Abbildung, Freigeben bei Abbildung, Antworten auf
        // Unbekanntes — alles scheitert lokal, kein Wort geht raus.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut c = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let live_vorher = s.grants_live();

        let aw = bedienen(&mut s, &park, PD_A, c.anfrage(PAGE_SIZE, Zweck::Allgemein).als_worte());
        let h = c.antwort_anfordern(aw, PAGE_SIZE).expect("ok");
        // Abbilden ohne Sicht: lokal.
        assert_eq!(c.abbilden(h).err(), Some(ClientFehler::KeineSicht));
        // Ausblenden ohne Abbildung: lokal.
        assert_eq!(c.ausblenden(h).err(), Some(ClientFehler::KeineAbbildung));
        // Antwort auf Unbekanntes: lokal.
        assert_eq!(
            c.antwort_aufloesen(0xDEAD_BEEF, [0, 0, 0, 0]).err(),
            Some(ClientFehler::UnbekannterSchein)
        );
        // Dieselbe Antwort zweimal annehmen: DoppelSchein, kein zweiter Eintrag.
        assert_eq!(
            c.antwort_anfordern(aw, PAGE_SIZE).err(),
            Some(ClientFehler::DoppelSchein)
        );
        assert_eq!(c.scheine(), 1);
        assert_eq!(s.grants_live(), live_vorher + 1, "kein Wort, kein Grant: Server unberührt");

        // Normal weiter — danach ist alles wieder sauber.
        let lw = bedienen(&mut s, &park, PD_A, c.aufloesen(h).expect("e").als_worte());
        let (cpu, _) = c.antwort_aufloesen(h, lw).expect("sicht");
        speicher.schreiben(cpu, PAGE_SIZE, h);
        let bw = bedienen(&mut s, &park, PD_A, c.abbilden(h).expect("e").als_worte());
        c.antwort_abbilden(h, bw).expect("ok");
        assert_eq!(c.freigeben(h).err(), Some(ClientFehler::NochAbgebildet));
        rueckweg(&mut s, &park, &mut c, PD_A, h);
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
        assert!(speicher.pruefen(cpu, PAGE_SIZE, h));
    }

    #[test]
    fn unbekannte_art_benannt() {
        // Was der Server nicht kennt, bekommt einen Namen statt Stille: Kode 12.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let antwort = bedienen(&mut s, &park, PD_A, [99, 0, 0, 0]);
        assert_eq!(antwort[0], fehler_kode(SpeicherFehler::UnbekannteArt));
        assert_eq!(s.grants_live(), 1, "unbekannte Art ändert nichts");
    }

    #[test]
    fn zweck_rundweg_dmapuffer() {
        // Zweck-Pfad mit Gerätesicht: Ein DMA-Puffer löst auf CPU- UND Gerätesicht auf —
        // der Client mappt die eine, das Gerät die andere (hier: beide arithmetisch belegt).
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut c = ClientSeite::neu();
        let mut speicher = ClientSpeicher::neu();
        let h = rundweg(&mut s, &park, &mut c, &mut speicher, PD_A, PAGE_SIZE, Zweck::DmaPuffer, true);
        let (cpu, dev) = match c.eintraege[c.finden(h).unwrap()].unwrap().zustand {
            ClientZustand::Abgebildet { cpu, dev } => (cpu, dev),
            _ => panic!("abgebildet erwartet"),
        };
        assert_eq!(cpu, ARENA_CPU_BASIS + START_KAPITAL as u64);
        assert_eq!(dev, ARENA_DEV_BASIS + START_KAPITAL as u64);
        assert_ne!(cpu, dev, "zwei Sichten, zwei Adressen");
        assert_eq!(s.zweck_benutzt(Zweck::DmaPuffer), PAGE_SIZE);
        assert_eq!(s.zweck_benutzt(Zweck::Allgemein), START_KAPITAL);
        assert!(speicher.pruefen(cpu, PAGE_SIZE, h));
        rueckweg(&mut s, &park, &mut c, PD_A, h);
        assert_eq!(s.zweck_benutzt(Zweck::DmaPuffer), 0);
    }

    #[test]
    fn pd_boot_konstante_gilt() {
        // Die Boot-PD aus dem Startkapital ist dieselbe, die hier anfragt — sonst wäre jede
        // Stufe-1-Aussage über den Draht eine andere PD.
        assert_eq!(PD_A, PD_BOOT);
        assert_eq!(PD_A, super::super::PD_BOOT);
    }
}
