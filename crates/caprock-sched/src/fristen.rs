//! **Die Frist-Entscheidung als reine Funktion** (A2-Rest).
//!
//! ## Warum das hier steht und nicht nur im Scheduler
//!
//! `Scheduler::fristen_faellig` haengt an `caprock-hal` und wird auf dem Host nie bauen -- wie
//! `caprock-sched` als Ganzes. Die Fallen der Frist sind aber **Entscheidungen, keine Hardware**:
//! feuert sie zu frueh, feuert sie nie, gewinnt das Signal oder der Timer, geht der Siebzehnte
//! verloren oder wird er verzoegert. Jede davon ist mit **Literalen** ausloesbar. Dieselbe
//! Begruendung wie bei [`crate::cycles`] und [`crate::redirect`]: was mit Literalen ausloesbar
//! ist, wird auf dem Host geprueft (`rustc --test --edition 2021 -O
//! crates/caprock-sched/src/fristen.rs`).
//!
//! ## Was hier steht -- und was ausdruecklich nicht
//!
//! Hier steht die Entscheidung **je Thread**: gegeben Jetzt, Frist, „wartet er noch aus dem
//! bewachten Grund" und „ist die Ausgabe voll", einer von vier Ausgaengen. Die Schleife
//! (Tabellendurchlauf, `naechste_frist`-Minimum, Einreihen, Melden) bleibt in
//! [`crate::Scheduler::fristen_faellig`], und das ist kein Zufall: der Modell-Treue-Waechter
//! (`tools/verus-modelltreue-sched.sh`) sieht nur `lib.rs`, und die QEMU-Gegenproben
//! (`tools/fristen-negativ.sh` M1/M4) mutieren genau dort. Wuerde die Wirkung hierher wandern,
//! wuerden beide blind -- ein Pruefer, der nicht mehr hinsieht, meldet dauerhaft gruen.
//!
//! Der Vertrag in beide Richtungen: `frist_befund` schreibt **keinen** Zustand, und der Aufrufer
//! wertet **jeden** Ausgang aus. Ein fuenfter Fall („weiss nicht") waere die D18-Form: ein
//! Ausgang, den kein Test je sieht.
//!
//! ## Die Reihenfolge der Pruefungen ist die Semantik
//!
//! Zukunft vor Deckel vor Rennen: eine kuenftige Frist bleibt scharf, auch bei voller Ausgabe;
//! eine faellige Frist bei voller Ausgabe wird **aufgeschoben, nicht verworfen** (der behobene
//! `FRISTEN_JE_TICK`-Fehler: der Deckel greift VOR der Wirkung); erst bei freier Ausgabe zaehlt,
//! ob der bewachte Grund noch steht -- steht er nicht, hat das Signal gewonnen. Dass der Deckel
//! VOR dem Rennen steht, heisst in der Ecke „voll UND schon geweckt": auch die tote Warte wird
//! einen Tick aufgeschoben und im naechsten abgeraeumt, statt sofort entwaffnet. Begrenzt,
//! selbstheilend, und hier festgeschrieben statt still vorausgesetzt.

/// Die vier Ausgaenge der Frist-Entscheidung -- genau einer je Thread je Tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FristBefund {
    /// Die Frist liegt in der Zukunft: sie bleibt scharf, der Aufrufer zieht das Minimum mit.
    Zukunft,
    /// Die Frist ist faellig, der bewachte Grund steht noch, die Ausgabe hat Platz: der Aufrufer
    /// entfernt **genau diesen Grund**, schreibt `ERR_TIMEOUT` und meldet.
    Freigeben,
    /// Die Frist ist faellig, aber die Ausgabe ist voll: die Frist **bleibt stehen**, der Aufrufer
    /// zaehlt den Ueberlauf (`Scheduler::fristen_ueberlauf`) und nimmt den Thread im naechsten
    /// Tick erneut auf. Verzoegerung, kein Verlust -- und jetzt eine benannte Zahl statt Stille.
    Aufschieben,
    /// Der bewachte Grund steht nicht mehr: das Signal (oder ein anderer Wecker) war schneller.
    /// Der Aufrufer entwaffnet still -- **nichts** wird geschrieben, **nichts** gemeldet. Nur ein
    /// Thread, den die Frist WIRKLICH freigegeben hat, bekommt je einen Code; alles andere waere
    /// der Korruptionspfad (Treiber liest „Geraet tot", waehrend die Antwort gerade zugestellt
    /// wurde).
    SignalGewann,
}

/// Entscheide ueber **einen** Thread mit **einer** scharfen Frist.
///
/// `frist == 0` heisst „keine Frist" und wird vom Aufrufer vorher aussortiert -- hier kommt nur
/// an, was scharf ist. Reine Funktion: kein Zustand, keine Uhr, keine Tabelle. Jede Falle unten
/// ist mit Literalen ausloesbar, ohne Maschine und ohne Tick.
pub fn frist_befund(jetzt: u64, frist: u64, grund_noch_da: bool, ausgabe_voll: bool) -> FristBefund {
    if frist > jetzt {
        return FristBefund::Zukunft;
    }
    if ausgabe_voll {
        return FristBefund::Aufschieben;
    }
    if !grund_noch_da {
        return FristBefund::SignalGewann;
    }
    FristBefund::Freigeben
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Normalfall in beide Richtungen: Zukunft bleibt scharf, Faelliges wird freigegeben.
    #[test]
    fn zukunft_bleibt_scharf_faelliges_wird_freigegeben() {
        assert_eq!(frist_befund(10, 20, true, false), FristBefund::Zukunft);
        assert_eq!(frist_befund(10, 10, true, false), FristBefund::Freigeben);
        assert_eq!(frist_befund(10, 5, true, false), FristBefund::Freigeben);
    }

    /// **A2c, die leichte Lage zum Mitschreiben:** das Signal kam VOR dem Tick -- der bewachte
    /// Grund steht nicht mehr, die Frist gehoert zu einem Warten, das vorbei ist.
    #[test]
    fn signal_vor_dem_tick_entwaffnet_still() {
        assert_eq!(frist_befund(10, 10, false, false), FristBefund::SignalGewann);
        assert_eq!(frist_befund(10, 5, false, false), FristBefund::SignalGewann);
    }

    /// **A2c, die getroffene Lage: Signal UND Timer im SELBEN Tick.** Faellig heisst nicht
    /// „der Timer gewinnt" -- steht der Grund nicht mehr, war das Signal schneller, auch wenn
    /// beide im selben Tick liegen. Der Aufrufer schreibt dann nichts: kein `ERR_TIMEOUT`
    /// ueber ein gerade zugestelltes `OK`.
    #[test]
    fn signal_gewinnt_auch_im_selben_tick() {
        // Frist genau jetzt faellig, Grund schon weg: das ist das Fenster, das die alte Sonde
        // (signalisieren VOR dem Warten) nie traf -- hier steht es als Literal.
        assert_eq!(frist_befund(42, 42, false, false), FristBefund::SignalGewann);
        // Und die Gegenrichtung: steht der Grund noch, gibt die Frist frei -- wer zuerst
        // kommt, gewinnt, und beide Ordnungen sind hier festgeschrieben.
        assert_eq!(frist_befund(42, 42, true, false), FristBefund::Freigeben);
    }

    /// **Der Ueberlauf als Regel, nicht als Lauf:** sechzehn Plaetze, siebzehn faellige Fristen.
    /// Die ersten sechzehn geben frei, die siebzehnte wird aufgeschoben -- ihre Frist bleibt
    /// stehen, und im naechsten Tick (Ausgabe wieder frei) gibt sie frei. Kein Ausgang heisst
    /// „verloren": das war der behobene Fehler, und diese Folge haelt ihn behoben.
    #[test]
    fn siebzehnter_wird_aufgeschoben_nicht_verloren() {
        let jetzt = 100;
        for i in 0..16 {
            // Plaetze 0..16: Ausgabe noch nicht voll.
            assert_eq!(
                frist_befund(jetzt, jetzt - i as u64, true, false),
                FristBefund::Freigeben
            );
        }
        // Platz 17: voll -- aufschieben, Frist bleibt stehen.
        assert_eq!(
            frist_befund(jetzt, jetzt, true, true),
            FristBefund::Aufschieben
        );
        // Naechster Tick, Ausgabe frei: dieselbe Frist gibt frei -- Verzoegerung, kein Verlust.
        assert_eq!(
            frist_befund(jetzt + 1, jetzt, true, false),
            FristBefund::Freigeben
        );
    }

    /// Die Zukunft kuemmert sich nicht um den Deckel: eine kuenftige Frist bleibt scharf, auch
    /// wenn die Ausgabe voll ist. Sie belegt keinen Platz und keinen Zaehler.
    #[test]
    fn kuenftige_frist_leidet_nicht_unter_vollem_deckel() {
        assert_eq!(frist_befund(10, 11, true, true), FristBefund::Zukunft);
        assert_eq!(frist_befund(10, 11, false, true), FristBefund::Zukunft);
    }

    /// Die festgeschriebene Ecke: voll UND schon geweckt. Der Deckel steht VOR dem Rennen
    /// (dieselbe Ordnung wie im Scheduler), also wird auch die tote Warte einen Tick
    /// aufgeschoben und im naechsten still abgeraeumt -- statt sofort entwaffnet. Begrenzt
    /// (genau ein Tick) und selbstheilend; wer die Ordnung umdreht, bricht DIESEN Test.
    #[test]
    fn deckel_steht_vor_dem_rennen_auch_fuer_tote_warten() {
        assert_eq!(
            frist_befund(10, 10, false, true),
            FristBefund::Aufschieben
        );
    }
}
