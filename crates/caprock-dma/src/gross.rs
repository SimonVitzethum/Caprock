//! **Grosse, zusammenhängende DMA — die Absage hat einen NAMEN** (Z26, Vorbedingung 2).
//!
//! ## Wofür das ist, und warum es nicht „für die GPU" ist
//!
//! Jeder Treiber mit Ringpuffern braucht ein Stück zusammenhängenden, gerätesichtbaren Speichers:
//! eine NVMe-Submission-/Completion-Queue, die RX-/TX-Ringe einer Netzkarte samt Puffern, die
//! Transfer-Ringe eines USB-Controllers, der Kommandopuffer einer GPU. Der Unterschied zwischen
//! diesen Fällen ist die **Grösse**, nicht die Art. Bis hierher war der Ladepfad auf kleine Stücke
//! ausgelegt (die Treiber-PD bekommt 16 KiB, `DRIVER_DMA_BYTES`), und der Fehlschlag beim Wunsch
//! nach mehr war ein `None`.
//!
//! ## Warum `None` hier zu wenig ist
//!
//! „Es ging nicht" ist bei einer mehrere MiB grossen Anforderung **vier verschiedene Befunde** mit
//! vier verschiedenen Abhilfen:
//!
//! | Befund | Abhilfe |
//! |---|---|
//! | [`GrossDmaFehler::GroesserAlsZone`] | gar keine — auf dieser Maschine strukturell unmöglich |
//! | [`GrossDmaFehler::ZoneErschoepft`] | weniger anfordern, oder früher anfordern |
//! | [`GrossDmaFehler::FreilisteVoll`] | `MAX_FRAGMENTS` erhöhen — freies RAM ist da! |
//! | [`GrossDmaFehler::Identitaet`] | **Sicherheitsbefund**, s. unten |
//!
//! Die dritte Zeile ist die unangenehmste: freies RAM ist reichlich vorhanden, und die Allokation
//! scheitert trotzdem, weil der Reststück-Eintrag nicht mehr in die Freiliste passt
//! (`alloc_in` überspringt ein Fragment, wenn ein beidseitiger Verschnitt die Liste sprengen
//! würde). Mit einem `None` sucht man diesen Fall im Speicherverbrauch — also an der falschen
//! Stelle. Es ist dieselbe Lehre wie bei D11: wer eine Kapazität einführt, muss den Überlauf
//! **benennen**.
//!
//! ## Die zwei Achsen bleiben getrennt
//!
//! Diese Datei rechnet ausschliesslich in **Längen und Zonengrenzen**, nie in Adressen zweier
//! Achsen zugleich. Die einzige Stelle, an der beide vorkommen, ist
//! [`GrossDmaFehler::Identitaet`] — und die ist eine **Absage**, kein Ergebnis: fällt die
//! Gerätesicht mit der CPU-Sicht zusammen, ist die Trennung, auf der `docs/invariants.md`
//! §2a–2e beruht, nicht mehr da, und der Treiber liefe nur so lange richtig, wie die beiden
//! zufällig übereinstimmen. Das Ergebnis der Vergabe trägt der Aufrufer in getrennten Typen
//! (`addr::Pa` / `addr::Iova` im Kernel, [`crate::DmaBuf`] in der PD).
//!
//! Abhängigkeitsfrei und `forbid(unsafe_code)` — die Fallen hier sind reine Grössenarithmetik und
//! lassen sich mit **Literalen** auslösen, ohne Maschine. Derselbe Grund wie beim Rest der Crate.

/// Die Seitengrösse, an der eine DMA-Region ausgerichtet sein muss.
pub const SEITE: u64 = 4096;

/// **Warum eine grosse, zusammenhängende DMA-Anforderung nicht erfüllt wurde.**
///
/// Jede Variante trägt die Zahl, die den Fall handhabbar macht. Ein Fehlerwert ohne Zahlen ist
/// die Prüfer-Krankheit eine Ebene tiefer: er sagt „nein" und nicht „wie viel wäre gegangen".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrossDmaFehler {
    /// Länge 0. Eine Region ohne Platz ist keine Region, und ein Grant darüber wäre ein Zeiger,
    /// den der Treiber für gültig hält.
    LaengeNull,
    /// Die Länge ist kein Vielfaches der Seitengrösse. **Nicht stillschweigend aufrunden**: der
    /// Aufrufer bekäme mehr, als er angefordert hat, und die Buchhaltung beim Freigeben stünde
    /// auf einer anderen Zahl als die beim Belegen.
    NichtSeitenausgerichtet { len: u64 },
    /// Die Längenrechnung läuft in `u64` über — dann sind alle Bereichsprüfungen darunter wertlos.
    Ueberlauf,
    /// **Passt auf dieser Maschine NIE.** Die Anforderung ist grösser als die Zone, aus der
    /// DMA-Regionen überhaupt kommen dürfen. Das ist kein Betriebszustand, sondern ein
    /// Programm- oder Konfigurationsfehler — und es lohnt nicht, es später noch einmal zu
    /// versuchen.
    GroesserAlsZone { angefordert: u64, zone: u64 },
    /// **Passt gerade nicht.** Der grösste zusammenhängende freie Block der Zone ist kleiner als
    /// die Anforderung. `groesster_block` ist die Zahl, mit der sich der Aufrufer entscheiden
    /// kann: kleiner anfordern, oder aufgeben.
    ZoneErschoepft { angefordert: u64, groesster_block: u64 },
    /// **Freies RAM ist da, die Freiliste ist voll.** Ein beidseitiger Verschnitt bräuchte einen
    /// weiteren Eintrag; gibt es keinen, wird das Fragment übersprungen. Wer diesen Fall als
    /// „zu wenig Speicher" liest, sucht an der falschen Stelle.
    FreilisteVoll { fragmente: usize, kapazitaet: usize },
    /// Die Region liess sich nicht in eine DMA-Cap prägen (Ausrichtung gegen das
    /// Cache-Writeback-Granule, s. `dma_granule_ok`).
    CapAbgewiesen,
    /// Es gibt keine Übersetzung: `dma_attach` hat abgelehnt (kein Kontext, IOVA-Fenster
    /// erschöpft, Gerät zu schmal). Die Region bleibt **unangehängt** und wird zurückgegeben.
    KeineUebersetzung,
    /// Die Gerätesicht ist `0`. Damit zu rechnen hiesse, Offsets als absolute Geräteadressen
    /// auszugeben.
    KeineGeraetesicht,
    /// **Gerätesicht == CPU-Sicht.** Ein Sicherheitsbefund, kein Ressourcenproblem: die Trennung
    /// der beiden Achsen (`docs/invariants.md` §2a–2e) ist an dieser Stelle nicht hergestellt.
    /// Durchgelassen liefe der Treiber, solange die beiden zufällig übereinstimmen, und bräche in
    /// dem Augenblick, in dem jemand die Trennung durchsetzt.
    Identitaet { pa: u64, iova: u64 },
}

impl GrossDmaFehler {
    /// **Lohnt ein zweiter Versuch mit derselben Zahl?**
    ///
    /// Der Unterschied zwischen „passt nie" und „passt gerade nicht" ist für einen Aufrufer die
    /// ganze Frage. Ohne diese Auskunft schreibt jeder Aufrufer seine eigene Fallunterscheidung
    /// über die Varianten — und die nächste Variante vergisst er.
    pub fn ist_voruebergehend(self) -> bool {
        matches!(
            self,
            GrossDmaFehler::ZoneErschoepft { .. } | GrossDmaFehler::FreilisteVoll { .. }
        )
    }
}

/// Die Zone, aus der DMA-Regionen kommen dürfen: `[lo, hi)`.
///
/// Auf x86 ist das `[USER_RAM_MIN, GIB1_END)` = `[16 MiB, 1 GiB)`, also **1008 MiB** — und das
/// ist keine Vorliebe, sondern strukturell: `vspace_map_page_at` weist jede VA `>= GIB1_END` ab,
/// eine Region darüber wäre für die PD nicht abbildbar. Wer diese Grenze verschiebt, verschiebt
/// die Obergrenze zusammenhängender DMA auf dieser Architektur.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Zone {
    pub lo: u64,
    pub hi: u64,
}

impl Zone {
    pub fn groesse(&self) -> u64 {
        self.hi.saturating_sub(self.lo)
    }
}

/// Was der Aufrufer über den Zustand des Allokators **gemessen** hat.
///
/// Bewusst Messwerte und keine Nachrechnung: „Zuteiler und Prüfer brauchen EINE Quelle" — die
/// `iova_window_clear_of_msi`-Falle. `groesster_block` kommt aus dem Allokator selbst (im Kernel:
/// eine Abwärtssuche mit echten `alloc`/`free`-Paaren), nicht aus einer zweiten Rechnung über die
/// Freiliste.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lage {
    /// Grösster zusammenhängender Block, den die Zone **jetzt** hergibt.
    pub groesster_block: u64,
    /// Belegte Einträge der Freiliste.
    pub fragmente: usize,
    /// Kapazität der Freiliste.
    pub kapazitaet: usize,
}

/// **Die Klassifikation.** Eine Stelle, an der aus „ging nicht" ein Name wird.
///
/// Reihenfolge der Prüfungen, und sie ist nicht beliebig:
///
/// 1. Form der Anforderung (0, Ausrichtung, Überlauf) — Fehler des Aufrufers, unabhängig von der
///    Maschine.
/// 2. `GroesserAlsZone` — „nie", vor allem Weiteren: die Zahlen unter 3./4. wären für diesen Fall
///    irreführend („der grösste Block ist 900 MiB" klingt nach Fragmentierung, wenn jemand 2 GiB
///    wollte).
/// 3. `ZoneErschoepft` **vor** `FreilisteVoll`: ist der grösste Block ohnehin zu klein, hilft eine
///    grössere Freiliste nicht, und `FreilisteVoll` schickte den Leser in die falsche Richtung.
/// 4. `FreilisteVoll` — der Rest: Platz wäre da, der Eintrag nicht.
pub fn klassifiziere(zone: Zone, len: u64, lage: Lage) -> Result<(), GrossDmaFehler> {
    if len == 0 {
        return Err(GrossDmaFehler::LaengeNull);
    }
    if len % SEITE != 0 {
        return Err(GrossDmaFehler::NichtSeitenausgerichtet { len });
    }
    if zone.lo.checked_add(len).is_none() {
        return Err(GrossDmaFehler::Ueberlauf);
    }
    if len > zone.groesse() {
        return Err(GrossDmaFehler::GroesserAlsZone {
            angefordert: len,
            zone: zone.groesse(),
        });
    }
    if lage.groesster_block < len {
        return Err(GrossDmaFehler::ZoneErschoepft {
            angefordert: len,
            groesster_block: lage.groesster_block,
        });
    }
    if lage.fragmente >= lage.kapazitaet {
        return Err(GrossDmaFehler::FreilisteVoll {
            fragmente: lage.fragmente,
            kapazitaet: lage.kapazitaet,
        });
    }
    Ok(())
}

/// **Den grössten noch belegbaren Block SUCHEN, indem man den Allokator fragt.**
///
/// `probe(len)` muss `true` liefern, wenn eine Allokation dieser Länge **gerade** durchginge —
/// im Kernel ein echtes `alloc`/`free`-Paar. Zurück kommt das grösste seitenausgerichtete `len`,
/// für das `probe` `true` sagte.
///
/// **Warum fragen und nicht rechnen.** Die Alternative wäre, die Freiliste nachzurechnen: Best-Fit,
/// Zonenbeschneidung, Fragment-Schranke. Das wäre eine **zweite Wirklichkeit** neben
/// `PhysAllocator::alloc_in` — dieselbe Form, die `iova_window_clear_of_msi` grün gehalten hat,
/// obwohl das Fenster den Sperrbereich enthielt. Der Allokator ist die einzige Instanz, die die
/// Frage richtig beantworten kann; also wird er gefragt.
///
/// **Warum das hier steht und nicht im Kernel:** eine binäre Suche mit Ausrichtung ist genau die
/// Sorte Code, die um eins danebenliegt, und sie lässt sich mit Literalen prüfen — ohne Maschine,
/// ohne Allokator. Der Kernel liefert nur die `probe`.
///
/// Terminiert nach höchstens `log2(zone_groesse / SEITE) + 1` Aufrufen (x86: **18** bei 1008 MiB).
///
/// **Was der Rückgabewert NICHT ist:** eine Zusage. Zwischen Messung und nächster Anforderung
/// kann ein anderer Kern belegen. Es ist eine Diagnose — „so viel wäre gerade gegangen".
/// **Warum hier KEIN `if mitte <= lo { break }` steht.**
///
/// Die erste Fassung hatte einen — als Schutz gegen das Off-by-eins, an dem eine *ausgerichtete*
/// binäre Suche gern stehenbleibt. Die Gegenprobe hat gezeigt, dass er **nie auslösen kann**, und
/// ein Wächter, der nicht auslösen kann, ist keiner: er sieht wie ein Schutz aus, deckt aber
/// nichts, und beim nächsten Umbau verlässt sich jemand darauf.
///
/// Er kann nicht auslösen, weil beide Schranken **immer seitenausgerichtet** sind:
/// `lo` startet auf 0 und wird nur auf ein ausgerichtetes `mitte` gesetzt; `hi` startet auf
/// `(zone & !(SEITE-1)) + SEITE` und wird ebenfalls nur auf `mitte` gesetzt. Damit ist `hi - lo`
/// ein Vielfaches von `SEITE`, aus der Schleifenbedingung folgt `hi - lo >= 2 * SEITE`, und das
/// Abrunden von `(hi-lo)/2` liefert mindestens `SEITE`. Also gilt stets
/// `lo + SEITE <= mitte <= hi - SEITE` — die Schleife kommt echt voran und terminiert.
///
/// Die Ausrichtung ist damit keine Bequemlichkeit, sondern die Terminierungsbedingung. Wer sie
/// entfernt, braucht den Wächter wieder — und `die_suche_rundet_auf_seiten_ab` fällt dann sofort.
pub fn groesster_block_suche(zone_groesse: u64, mut probe: impl FnMut(u64) -> bool) -> u64 {
    let mut lo = 0u64; // Invariante: `lo` geht (0 trivialerweise), seitenausgerichtet
    let mut hi = (zone_groesse & !(SEITE - 1)) + SEITE; // Invariante: `hi` geht nicht, ausgerichtet
    while hi - lo > SEITE {
        let mitte = (lo + (hi - lo) / 2) & !(SEITE - 1);
        if probe(mitte) {
            lo = mitte;
        } else {
            hi = mitte;
        }
    }
    lo
}

/// **Die Achsenprüfung des Ergebnisses.** Läuft NACH der Vergabe, über die beiden gelieferten
/// Adressen.
///
/// Sie steht hier und nicht beim Aufrufer, weil sie sonst an jeder Vergabestelle einzeln richtig
/// geprüft werden müsste — und die nächste Vergabestelle vergisst sie. Dieselbe Begründung wie
/// beim IOVA-Fenster, das strukturell oberhalb des Interrupt-Nachrichtenbereichs beginnt, statt
/// bei jeder Vergabe geprüft zu werden.
pub fn pruefe_achsen(pa: u64, iova: u64) -> Result<(), GrossDmaFehler> {
    if iova == 0 {
        return Err(GrossDmaFehler::KeineGeraetesicht);
    }
    if iova == pa {
        return Err(GrossDmaFehler::Identitaet { pa, iova });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    /// Die x86-Zone: `[16 MiB, 1 GiB)` — 1008 MiB.
    fn x86_zone() -> Zone {
        Zone {
            lo: 16 * MIB,
            hi: 1024 * MIB,
        }
    }
    fn heil(block: u64) -> Lage {
        Lage {
            groesster_block: block,
            fragmente: 12,
            kapazitaet: 1024,
        }
    }

    #[test]
    fn die_x86_zone_ist_1008_mib() {
        // **Die Zahl, nach der gefragt wurde.** Eine Grenze, die niemand kennt, ist keine.
        assert_eq!(x86_zone().groesse(), 1008 * MIB);
    }

    #[test]
    fn acht_mib_gehen_durch() {
        // Die Grössenordnung, um die es geht: ein Ringpuffer-Satz eines echten Treibers.
        assert!(klassifiziere(x86_zone(), 8 * MIB, heil(512 * MIB)).is_ok());
    }

    #[test]
    fn zu_gross_und_zu_zerstueckelt_sind_unterschiedliche_befunde() {
        // Der ganze Zweck dieser Datei. Beide heissen „ging nicht" und haben nichts miteinander
        // zu tun: der eine ist auf dieser Maschine unmoeglich, der andere ein Betriebszustand.
        let nie = klassifiziere(x86_zone(), 2048 * MIB, heil(512 * MIB)).unwrap_err();
        assert_eq!(
            nie,
            GrossDmaFehler::GroesserAlsZone {
                angefordert: 2048 * MIB,
                zone: 1008 * MIB
            }
        );
        assert!(!nie.ist_voruebergehend());

        let jetzt = klassifiziere(x86_zone(), 64 * MIB, heil(32 * MIB)).unwrap_err();
        assert_eq!(
            jetzt,
            GrossDmaFehler::ZoneErschoepft {
                angefordert: 64 * MIB,
                groesster_block: 32 * MIB
            }
        );
        assert!(jetzt.ist_voruebergehend());
    }

    #[test]
    fn volle_freiliste_ist_kein_speichermangel() {
        // **Der Fall, den ein `None` an der falschen Stelle suchen laesst.** Platz waere da
        // (grosser Block), der Eintrag nicht.
        let e = klassifiziere(
            x86_zone(),
            8 * MIB,
            Lage {
                groesster_block: 512 * MIB,
                fragmente: 1024,
                kapazitaet: 1024,
            },
        )
        .unwrap_err();
        assert_eq!(
            e,
            GrossDmaFehler::FreilisteVoll {
                fragmente: 1024,
                kapazitaet: 1024
            }
        );
        assert!(e.ist_voruebergehend());
    }

    #[test]
    fn erschoepfung_gewinnt_gegen_volle_freiliste() {
        // Die Reihenfolge ist Teil der Aussage: ist der groesste Block ohnehin zu klein, hilft
        // eine groessere Freiliste NICHT -- `FreilisteVoll` schickte den Leser in die falsche
        // Richtung.
        let e = klassifiziere(
            x86_zone(),
            64 * MIB,
            Lage {
                groesster_block: 32 * MIB,
                fragmente: 1024,
                kapazitaet: 1024,
            },
        )
        .unwrap_err();
        assert!(matches!(e, GrossDmaFehler::ZoneErschoepft { .. }));
    }

    #[test]
    fn zu_gross_gewinnt_gegen_alles_andere() {
        // Sonst stuende bei einer 2-GiB-Anforderung „der groesste Block ist 8 MiB" -- eine Zahl,
        // die nach Fragmentierung klingt und den Leser Stunden kostet.
        let e = klassifiziere(
            x86_zone(),
            2048 * MIB,
            Lage {
                groesster_block: 8 * MIB,
                fragmente: 1024,
                kapazitaet: 1024,
            },
        )
        .unwrap_err();
        assert!(matches!(e, GrossDmaFehler::GroesserAlsZone { .. }));
    }

    #[test]
    fn krumme_laenge_wird_abgewiesen_statt_aufgerundet() {
        // Aufgerundet bekaeme der Aufrufer mehr, als er angefordert hat -- und die Buchhaltung
        // beim Freigeben stuende auf einer anderen Zahl als die beim Belegen.
        assert_eq!(
            klassifiziere(x86_zone(), 4097, heil(512 * MIB)).unwrap_err(),
            GrossDmaFehler::NichtSeitenausgerichtet { len: 4097 }
        );
        assert_eq!(
            klassifiziere(x86_zone(), 0, heil(512 * MIB)).unwrap_err(),
            GrossDmaFehler::LaengeNull
        );
    }

    #[test]
    fn ueberlauf_wird_abgewiesen() {
        let z = Zone {
            lo: u64::MAX - SEITE,
            hi: u64::MAX,
        };
        assert_eq!(
            klassifiziere(z, 2 * SEITE, heil(u64::MAX)).unwrap_err(),
            GrossDmaFehler::Ueberlauf
        );
    }

    #[test]
    fn genau_die_zone_geht_noch_durch() {
        // Die Kante: `len == zone.groesse()` ist zulaessig, `+1 Seite` nicht. Ein Off-by-one hier
        // machte die groesste ueberhaupt moegliche Anforderung unmoeglich.
        assert!(klassifiziere(x86_zone(), 1008 * MIB, heil(1008 * MIB)).is_ok());
        assert!(matches!(
            klassifiziere(x86_zone(), 1008 * MIB + SEITE, heil(1008 * MIB)).unwrap_err(),
            GrossDmaFehler::GroesserAlsZone { .. }
        ));
    }

    #[test]
    fn identitaet_ist_ein_sicherheitsbefund_und_kein_ressourcenproblem() {
        // Die Kernaussage des Projekts, hier als Ergebnisbedingung. Durchgelassen liefe der
        // Treiber, solange die beiden Achsen zufaellig uebereinstimmen.
        assert_eq!(
            pruefe_achsen(0x4000_0000, 0x4000_0000).unwrap_err(),
            GrossDmaFehler::Identitaet {
                pa: 0x4000_0000,
                iova: 0x4000_0000
            }
        );
        assert!(!pruefe_achsen(0x4000_0000, 0x4000_0000)
            .unwrap_err()
            .ist_voruebergehend());
        // Und die Gegenprobe: getrennte Achsen gehen durch.
        assert!(pruefe_achsen(0x4000_0000, 0x1_0000_0000).is_ok());
    }

    // --- Die Suche nach dem groessten Block ----------------------------------------------------

    /// Ein synthetischer Allokator: alles bis `grenze` geht, darueber nichts. `zaehler` zaehlt die
    /// Befragungen -- eine Suche, die 250 000 Mal allokiert, waere im Kernel keine Diagnose,
    /// sondern ein Haenger.
    fn suche_mit(grenze: u64, zone: u64) -> (u64, usize) {
        let mut n = 0usize;
        let r = groesster_block_suche(zone, |len| {
            n += 1;
            len <= grenze
        });
        (r, n)
    }

    #[test]
    fn die_suche_findet_die_grenze_genau() {
        let zone = 1008 * MIB;
        for grenze in [
            0,
            SEITE,
            2 * SEITE,
            MIB,
            8 * MIB,
            512 * MIB,
            zone - SEITE,
            zone,
        ] {
            let (r, _) = suche_mit(grenze, zone);
            assert_eq!(r, grenze, "Grenze {grenze} wurde als {r} gemeldet");
        }
    }

    #[test]
    fn die_suche_rundet_auf_seiten_ab() {
        // Eine krumme Grenze darf keine krumme Antwort geben -- `alloc_dma_region` rundet die
        // Laenge selbst auf, und eine krumme Rueckmeldung wuerde als „so viel geht" gelesen und
        // beim naechsten Versuch abgewiesen.
        let (r, _) = suche_mit(8 * MIB + 1234, 1008 * MIB);
        assert_eq!(r, 8 * MIB);
        assert_eq!(r % SEITE, 0);
    }

    #[test]
    fn die_suche_bleibt_unter_zwanzig_befragungen() {
        // **Die Zahl gehoert dazu.** Eine Diagnose, die den Allokator 250 000 Mal anfasst, ist
        // keine Diagnose. `log2(1008 MiB / 4 KiB) = 18`.
        let (_, n) = suche_mit(512 * MIB, 1008 * MIB);
        assert!(n <= 20, "{n} Befragungen -- zu viele");
        let (_, n) = suche_mit(0, 1008 * MIB);
        assert!(n <= 20, "{n} Befragungen bei leerer Zone");
    }

    #[test]
    fn eine_leere_zone_gibt_null_und_haengt_nicht() {
        // Der Randfall, an dem eine ausgerichtete binaere Suche gern stehenbleibt.
        let (r, n) = suche_mit(0, 0);
        assert_eq!(r, 0);
        assert!(n <= 2);
    }

    #[test]
    fn die_suche_ist_die_zahl_die_klassifiziere_braucht() {
        // **Die Verbindung der beiden Haelften, und zwar als Aussage:** was die Suche meldet, muss
        // `klassifiziere` genau eine Seite spaeter wieder durchlassen -- sonst meldete der Kernel
        // „so viel waere gegangen" und wiese denselben Wert im naechsten Atemzug ab.
        let zone = x86_zone();
        let (block, _) = suche_mit(64 * MIB, zone.groesse());
        assert!(klassifiziere(zone, block, heil(block)).is_ok());
        assert!(matches!(
            klassifiziere(zone, block + SEITE, heil(block)).unwrap_err(),
            GrossDmaFehler::ZoneErschoepft {
                groesster_block, ..
            } if groesster_block == block
        ));
    }

    #[test]
    fn keine_geraetesicht_ist_ein_eigener_befund() {
        // `0` heisst „es gibt keine IOVA" und nicht „die IOVA ist 0". Damit zu rechnen hiesse,
        // Offsets als absolute Geraeteadressen auszugeben.
        assert_eq!(
            pruefe_achsen(0x4000_0000, 0).unwrap_err(),
            GrossDmaFehler::KeineGeraetesicht
        );
    }
}
