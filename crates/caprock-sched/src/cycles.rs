//! **Verbrauch per Zyklenstempel** (B-5.1) — die Arithmetik, ohne Kernel, ohne HAL, ohne Uhr.
//!
//! ## Warum Ticks die falsche Rechnung sind
//!
//! Abgerechnet wird heute in Ticks: `on_tick` zieht dem laufenden Thread genau dann einen Tick
//! ab, wenn der Timer feuert. Wer kurz **vor** dem Tick blockiert, hat gerechnet und zahlt
//! **nichts**; wer kurz **nach** dem Tick anläuft, zahlt einen vollen Tick für fast nichts. Bei
//! 100 Hz ist das ein Fehler von bis zu 10 ms **je Umplanung**, und er ist nicht zufällig verteilt:
//! ein Thread, der systematisch kurz vor dem Tick blockiert, rechnet dauerhaft umsonst. Für eine
//! Cloud, die CPU-Zeit verkauft, ist das kein Rundungsfehler, sondern ein Abrechnungsfehler mit
//! Methode.
//!
//! Ein **schnellerer Tick** wäre die falsche Antwort: er erhöht Auflösung *und* Overhead. Ein
//! Stempel beim Ein- und Auslasten erhöht nur die Auflösung.
//!
//! ## Warum das hier steht und nicht im Scheduler
//!
//! Die Rechnung ist reine `u64`-Arithmetik — und **genau dort liegen die Fallen**. Dieses Modul
//! hat deshalb keine einzige Abhängigkeit: es ist auf dem Host prüfbar (`tools/host-tests.sh
//! cycles`), während `caprock-sched` als Ganzes an `caprock-hal` hängt und es nie sein wird.
//! Die **Uhr** gehört nicht hierher: wer misst, reicht den Zählerstand herein. Damit ist jede
//! Falle unten mit einem Literal auslösbar statt nur auf einer bestimmten Maschine.
//!
//! ## Die drei Fallen
//!
//! 1. **Rückwärtssprung**, und er wird hier **nicht** als Wrap behandelt — s. [`measure`].
//! 2. **Unplausible Differenz** — ein einzelner kaputter Stempel darf kein Konto leerräumen.
//! 3. **Migration** — ein Zyklenzähler ist nur *innerhalb eines Kerns* eine Zeitachse.

/// Ein Zyklenstempel: Zählerstand **und** der Kern, auf dem er genommen wurde.
///
/// Der Kern gehört zwingend dazu. Ein Zyklenzähler ist keine systemweite Uhr: zwischen Sockeln
/// (und unter jedem Hypervisor) dürfen die Zähler auseinanderlaufen. Eine Differenz über einen
/// Kernwechsel hinweg ist deshalb keine Dauer, sondern eine Zufallszahl.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stamp {
    pub cycles: u64,
    pub core: u16,
}

/// Taugt die Zeitquelle überhaupt als Zeitachse?
///
/// Auf x86 sagt `CPUID.80000007H:EDX.InvariantTSC` zu, dass der Zähler mit konstanter Rate läuft
/// und im Idle nicht stehenbleibt. **Ohne diese Zusage ist eine Zyklendifferenz keine Zeit** —
/// unter TCG ist sie es nicht, und auf einer gemieteten Maschine ist sie es nur, wenn der Wirt
/// es zusichert. Eine Abrechnung, die das ignoriert, stellt Rechnungen über erfundene Zahlen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// Invarianter Zähler — konstante Rate, läuft durch.
    Invariant,
    /// Nicht zugesichert. Es wird **nichts** abgerechnet (und das ist sichtbar, s. [`CycleStats`]).
    Untrusted,
}

/// Warum eine Probe verworfen wurde.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reject {
    /// Der Zähler lief rückwärts.
    Backward,
    /// Die Differenz ist grösser als jede Zeitscheibe sein kann.
    Implausible,
    /// Ein- und Auslasten auf verschiedenen Kernen.
    CoreChanged,
    /// Die Zeitquelle ist nicht als invariant zugesichert.
    SourceUntrusted,
}

/// Ergebnis einer Messung zwischen zwei Stempeln.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sample {
    /// Gültige Dauer in Zyklen.
    Valid(u64),
    /// Verworfen, mit Grund. **Nicht** „null Zyklen": eine verworfene Probe ist eine fehlende
    /// Messung, keine gemessene Null.
    Rejected(Reject),
}

/// Obergrenze einer plausiblen Zeitscheibe in Zyklen (Vorgabe).
///
/// `2^40` sind bei 5 GHz gut drei Minuten — weit jenseits jeder Zeitscheibe, aber weit diesseits
/// des Unsinns, den ein verrutschter Stempel erzeugt (`2^63`-Klasse). Die Grenze ist bewusst
/// **grosszügig**: sie soll kaputte Proben fangen, nicht lange Proben. Wer sie eng zöge, würde
/// legitime Messungen verwerfen und die Abrechnung nach unten verfälschen — also genau den
/// Fehler machen, gegen den B-5.1 antritt, nur in die andere Richtung.
pub const MAX_PLAUSIBLE_SLICE: u64 = 1 << 40;

/// Die Dauer zwischen zwei Stempeln — oder der Grund, warum es keine gibt.
///
/// ## Rückwärts ist ein Fehler, kein Wrap
///
/// Die naheliegende Zeile wäre `now.wrapping_sub(prev)`. Sie ist falsch, und zwar teuer: ein
/// Zähler, der um 100 Zyklen zurückspringt, ergäbe damit eine Differenz von rund `2^64` — ein
/// Konto, das so belastet wird, ist sofort und dauerhaft erschöpft. Aus einem Messfehler von
/// 20 Nanosekunden würde ein Thread, der nie wieder läuft.
///
/// Und die Unterscheidung ist gar nicht möglich: aus **einem** Stempelpaar lässt sich ein Wrap
/// nicht von einem Rückwärtssprung trennen. Man muss sich also entscheiden, welcher Fall
/// wahrscheinlicher ist — und das ist eindeutig: ein 64-Bit-Zähler bei 5 GHz wrappt nach etwa
/// 117 Jahren, ein nicht-invarianter Zähler springt beim ersten Frequenzwechsel zurück. Deshalb:
/// rückwärts heisst **verworfen**, nicht „fast einmal herum".
pub fn measure(prev: Stamp, now: Stamp, source: Source, max_plausible: u64) -> Sample {
    if source != Source::Invariant {
        return Sample::Rejected(Reject::SourceUntrusted);
    }
    if prev.core != now.core {
        return Sample::Rejected(Reject::CoreChanged);
    }
    if now.cycles < prev.cycles {
        return Sample::Rejected(Reject::Backward);
    }
    let d = now.cycles - prev.cycles;
    if d > max_plausible {
        return Sample::Rejected(Reject::Implausible);
    }
    Sample::Valid(d)
}

/// Das Konto eines Threads: Summe **und** die Gründe, warum sie unvollständig sein könnte.
///
/// Die Ablehnungszähler sind kein Beiwerk. `consumed == 0` hat zwei völlig verschiedene
/// Bedeutungen — „hat nicht gerechnet" und „konnte nicht gemessen werden" — und eine Abrechnung,
/// die beide gleich darstellt, stellt im zweiten Fall eine Null in Rechnung, die sie nicht belegen
/// kann. [`CycleStats::measurable`] trennt sie. Dieselbe Form wie die Sprechprobe der
/// IOMMU-Einheiten: wer über Abwesenheit entscheidet, muss belegen, dass er messen konnte.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CycleStats {
    /// Summe der gültigen Zyklen.
    pub consumed: u64,
    /// Anzahl gültiger Proben.
    pub samples: u64,
    pub rejected_backward: u32,
    pub rejected_implausible: u32,
    pub rejected_core: u32,
    pub rejected_source: u32,
}

impl CycleStats {
    pub const EMPTY: CycleStats = CycleStats {
        consumed: 0,
        samples: 0,
        rejected_backward: 0,
        rejected_implausible: 0,
        rejected_core: 0,
        rejected_source: 0,
    };

    /// Eine Probe verbuchen.
    ///
    /// `consumed` sättigt: ein Überlauf würde die Summe **kleiner** machen, und eine Abrechnung,
    /// die beim Überlauf günstiger wird, ist die falsche Richtung. (Bei 5 GHz dauert der Überlauf
    /// 117 Jahre — die Sättigung ist trotzdem billiger als die Frage, ob es reicht.)
    pub fn apply(&mut self, s: Sample) {
        match s {
            Sample::Valid(d) => {
                self.consumed = self.consumed.saturating_add(d);
                self.samples += 1;
            }
            Sample::Rejected(Reject::Backward) => {
                self.rejected_backward = self.rejected_backward.saturating_add(1)
            }
            Sample::Rejected(Reject::Implausible) => {
                self.rejected_implausible = self.rejected_implausible.saturating_add(1)
            }
            Sample::Rejected(Reject::CoreChanged) => {
                self.rejected_core = self.rejected_core.saturating_add(1)
            }
            Sample::Rejected(Reject::SourceUntrusted) => {
                self.rejected_source = self.rejected_source.saturating_add(1)
            }
        }
    }

    /// Anzahl verworfener Proben (alle Gründe).
    pub fn rejected(&self) -> u64 {
        self.rejected_backward as u64
            + self.rejected_implausible as u64
            + self.rejected_core as u64
            + self.rejected_source as u64
    }

    /// **Trägt die Zahl überhaupt?** `false` heisst: es gab keine einzige gültige Probe —
    /// `consumed` ist dann keine Null, sondern eine Leerstelle, und darf nicht als Verbrauch
    /// berichtet werden.
    pub fn measurable(&self) -> bool {
        self.samples > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C0: u16 = 0;
    fn st(c: u64, core: u16) -> Stamp {
        Stamp { cycles: c, core }
    }

    /// Der Normalfall: Differenz zweier Stempel desselben Kerns.
    #[test]
    fn differenz_auf_demselben_kern() {
        let s = measure(st(1_000, C0), st(1_250, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE);
        assert_eq!(s, Sample::Valid(250));
        // Auch eine Dauer von 0 ist eine gueltige Messung (Stempel unmittelbar hintereinander).
        assert_eq!(
            measure(st(7, C0), st(7, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE),
            Sample::Valid(0)
        );
    }

    /// **Die teuerste Falle:** ein Rueckwaertssprung darf NICHT als Wrap durchgehen.
    ///
    /// Mit `wrapping_sub` waere die Differenz hier rund `2^64` -- ein Konto, das so belastet
    /// wird, ist sofort und dauerhaft erschoepft. Der Test prueft deshalb beides: dass verworfen
    /// wird, UND dass keine riesige Zahl herauskommt.
    #[test]
    fn rueckwaerts_wird_verworfen_und_nicht_gewrappt() {
        let s = measure(st(1_000, C0), st(900, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE);
        assert_eq!(s, Sample::Rejected(Reject::Backward));
        if let Sample::Valid(d) = s {
            panic!("Rueckwaertssprung als Dauer {d} verbucht -- das ist der wrapping_sub-Fehler");
        }
        // Auch ein Sprung um genau 1 zaehlt.
        assert_eq!(
            measure(st(1, C0), st(0, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE),
            Sample::Rejected(Reject::Backward)
        );
    }

    /// Eine Differenz jenseits jeder Zeitscheibe ist ein kaputter Stempel, keine lange Rechnung.
    #[test]
    fn unplausible_differenz_wird_verworfen() {
        let s = measure(st(0, C0), st(MAX_PLAUSIBLE_SLICE + 1, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE);
        assert_eq!(s, Sample::Rejected(Reject::Implausible));
        // Genau auf der Grenze ist noch gueltig -- die Schranke schneidet nicht ins Legitime.
        assert_eq!(
            measure(st(0, C0), st(MAX_PLAUSIBLE_SLICE, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE),
            Sample::Valid(MAX_PLAUSIBLE_SLICE)
        );
    }

    /// **Migration:** zwischen zwei Kernen ist die Differenz keine Dauer. Auch dann nicht, wenn
    /// sie zufaellig plausibel aussieht -- genau deshalb wird der Kern mitgestempelt.
    #[test]
    fn kernwechsel_wird_verworfen() {
        let s = measure(st(1_000, 0), st(1_250, 1), Source::Invariant, MAX_PLAUSIBLE_SLICE);
        assert_eq!(s, Sample::Rejected(Reject::CoreChanged));
        // Der Kern wird VOR der Zahl geprueft: ein Kernwechsel mit rueckwaerts laufendem
        // Zaehler meldet den Kernwechsel, nicht den Ruecksprung -- die Ursache, nicht die Folge.
        assert_eq!(
            measure(st(1_000, 0), st(10, 1), Source::Invariant, MAX_PLAUSIBLE_SLICE),
            Sample::Rejected(Reject::CoreChanged)
        );
    }

    /// Ohne zugesicherte Zeitquelle wird **gar nicht** gerechnet -- auch nicht „ungefaehr".
    #[test]
    fn untrusted_quelle_liefert_keine_zahl() {
        let s = measure(st(1_000, C0), st(1_250, C0), Source::Untrusted, MAX_PLAUSIBLE_SLICE);
        assert_eq!(s, Sample::Rejected(Reject::SourceUntrusted));
    }

    /// **Null verbraucht ist nicht dasselbe wie nicht gemessen.** Das ist die Aussage, an der
    /// eine Abrechnung haengt: ein Konto ohne gueltige Probe darf keine Null berichten.
    #[test]
    fn null_verbraucht_ist_nicht_nicht_gemessen() {
        let mut a = CycleStats::EMPTY;
        assert!(!a.measurable(), "frisches Konto ist nicht messbar");
        assert_eq!(a.consumed, 0);

        // Eine verworfene Probe macht es NICHT messbar.
        a.apply(Sample::Rejected(Reject::SourceUntrusted));
        assert!(!a.measurable(), "eine verworfene Probe ist keine Messung");
        assert_eq!(a.consumed, 0);
        assert_eq!(a.rejected(), 1);

        // Erst eine gueltige Probe -- und die darf 0 Zyklen lang sein.
        a.apply(Sample::Valid(0));
        assert!(a.measurable(), "gueltige Probe ueber 0 Zyklen IST eine Messung");
        assert_eq!(a.consumed, 0);
    }

    /// Die Gruende werden getrennt gezaehlt: „verworfen" allein sagt nicht, ob die Maschine
    /// nicht messen kann (Quelle) oder ob etwas nicht stimmt (rueckwaerts).
    #[test]
    fn gruende_werden_getrennt_gezaehlt() {
        let mut a = CycleStats::EMPTY;
        a.apply(Sample::Valid(100));
        a.apply(Sample::Valid(50));
        a.apply(Sample::Rejected(Reject::Backward));
        a.apply(Sample::Rejected(Reject::CoreChanged));
        a.apply(Sample::Rejected(Reject::CoreChanged));
        a.apply(Sample::Rejected(Reject::Implausible));
        assert_eq!(a.consumed, 150);
        assert_eq!(a.samples, 2);
        assert_eq!(a.rejected_backward, 1);
        assert_eq!(a.rejected_core, 2);
        assert_eq!(a.rejected_implausible, 1);
        assert_eq!(a.rejected_source, 0);
        assert_eq!(a.rejected(), 4);
    }

    /// Die Summe saettigt statt ueberzulaufen -- ein Ueberlauf machte die Rechnung **guenstiger**.
    #[test]
    fn summe_saettigt_statt_ueberzulaufen() {
        let mut a = CycleStats::EMPTY;
        a.consumed = u64::MAX - 10;
        a.apply(Sample::Valid(100));
        assert_eq!(a.consumed, u64::MAX, "Ueberlauf haette die Summe kleiner gemacht");
    }

    /// Ein vollstaendiger Zyklus, wie ihn der Scheduler faehrt: stempeln, laufen, abrechnen.
    /// Belegt, dass sich mehrere Zeitscheiben zur Gesamtdauer summieren -- die Eigenschaft, die
    /// der Tick-Rechnung fehlt (dort zahlt eine Scheibe unter einem Tick **nichts**).
    #[test]
    fn kurze_scheiben_summieren_sich_statt_zu_verschwinden() {
        let mut a = CycleStats::EMPTY;
        // Zehn Scheiben zu je 1000 Zyklen -- jede einzelne weit unter einem 10-ms-Tick.
        let mut t = 0u64;
        for _ in 0..10 {
            let vorher = st(t, C0);
            t += 1_000;
            a.apply(measure(vorher, st(t, C0), Source::Invariant, MAX_PLAUSIBLE_SLICE));
        }
        assert_eq!(a.consumed, 10_000, "kurze Scheiben duerfen nicht verschwinden");
        assert_eq!(a.samples, 10);
    }
}
