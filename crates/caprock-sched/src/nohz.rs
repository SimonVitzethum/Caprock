//! **Die NOHZ-Entscheidung als reine Funktion** (B-5.2/Z5).
//!
//! ## Warum das hier steht und nicht nur im Scheduler
//!
//! `Scheduler::nohz_stand` haengt an Tabellen und Locks und wird auf dem Host nie bauen -- wie
//! `caprock-sched` als Ganzes. Die NOHZ-Frage ist aber eine **Entscheidung, keine Hardware**:
//! braucht dieser Kern seinen periodischen Tick noch, oder darf er schweigen, bis ihn ein
//! benannter Grund weckt? Jede Falle davon ist mit **Literalen** ausloesbar. Dieselbe
//! Begruendung wie bei [`crate::cycles`], [`crate::redirect`] und `fristen`: was mit Literalen
//! ausloesbar ist, wird auf dem Host geprueft (`rustc --test --edition 2021 -O
//! crates/caprock-sched/src/nohz.rs`).
//!
//! ## Was hier steht -- und was ausdruecklich nicht
//!
//! Hier steht die Entscheidung ueber **einen Kern in einem Augenblick**: gegeben die Zahl der
//! bereiten Rivalen (ohne den laufenden), der naechste benannte Weckruf als Delta in Ticks
//! (`None` = keiner bewaffnet) und ob der laufende Thread ein begrenztes Budget traegt, einer
//! von drei Ausgaengen. Die Messung (Zaehlen der Rivalen, Minimum aus `naechste_frist` und
//! Refill-Zeitpunkten, Budgetpruefung) bleibt in [`crate::Scheduler::nohz_stand`], und das ist
//! kein Zufall: die Messung haelt einen Lock, die Entscheidung keinen. Wer beides vermischte,
//! bekame eine „reine" Funktion, die unter einem Lock die Welt anhaelt -- und eine
//! Host-Pruefung, die den Lock braucht.
//!
//! Der Vertrag in beide Richtungen: `nohz_plan` schreibt **keinen** Zustand, und der Aufrufer
//! (Idle-Pfad in `kernel/src/threads/nohz.rs`, Tick-Pfad in `system::reschedule`) wertet
//! **jeden** Ausgang aus und armiert den Timer entsprechend um. Ein vierter Fall („weiss
//! nicht") waere die D18-Form: ein Ausgang, den kein Test je sieht.
//!
//! ## Die Reihenfolge der Pruefungen ist die Semantik (fail-closed)
//!
//! Rivale vor Schranke vor Weckruf: Wer zu verdraengen hat, tickt -- gleich was die Uhr sonst
//! sagt. Wer ein begrenztes Budget traegt, tickt ebenfalls: die Erschoepfung wird **auf dem
//! Tick** erkannt (`remaining -= 1` in `on_tick`), und ohne Tick liefe der Thread ueber sein
//! Budget hinaus. Erst wer weder Rivalen noch Schranke hat, darf auf den Weckruf hoeren -- und
//! ein Weckruf, der sofort faellig ist (`0`/`1`), ist ein Tick mit Extraschritten, kein
//! One-Shot. Jede Umordnung wuerde DIESEN Tests brechen, nicht irgendeinen berkelauf.

/// Die drei Ausgaenge der NOHZ-Entscheidung -- genau einer je Kern je Augenblick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nohz {
    /// Periodisch weiter ticken wie bisher. Der unbedingte Ausgang: zwei Gruende
    /// (Rivale, Schranke) und ein Grenzfall (sofort faelliger Weckruf) fuehren hierher.
    Periodic,
    /// Genau einmal in `ticks` Ticks feuern, danach Stille. Nur wenn **kein** Rivale, **keine**
    /// Schranke und ein echter kuenftiger Weckruf (`>= 2`) vorliegt.
    OneShot {
        /// Delta in Ticks ab jetzt (`>= 2`, s. [`nohz_plan`]).
        ticks: u64,
    },
    /// Timer ganz entwaffnen: niemand zu verdraengen, nichts bewaffnet. Geweckt wird durch
    /// alles, was kein Tick ist -- IPI, Geraete-IRQ, Weckmarke.
    Disarmed,
}

/// Entscheide ueber **einen** Kern: periodisch ticken, einmalig bewaffnen oder schweigen.
///
/// * `rivalen`: bereite Threads auf diesem Kern **ohne** den laufenden. Jeder Eintrag in
///   einer Ready-Queue ist per Audit-Invariante (Codes 2/9) wirklich lauffaehig -- ein
///   „bereiter", der es nicht ist, waere dort laengst rot geworden.
/// * `weckruf`: naechster benannter Weckruf als **Delta** in Ticks ab jetzt (`None` = kein
///   Wecker bewaffnet: keine Frist, kein erschopftes Budget mit Refill). Das Minimum aus
///   `naechste_frist` und allen `next_refill`-Zeitpunkten bildet der Aufrufer; ein zu frueh
///   stehender Wert (die dokumentierte `naechste_frist`-Schieflage) erzeugt hoechstens einen
///   ueberfluessigen Tick -- die sichere Richtung.
/// * `hat_schranke`: der laufende Thread traegt ein begrenztes Budget (`budget > 0`). Seine
///   Erschoepfung wird auf dem Tick erkannt; ohne Tick kein Refill, ohne Refill keine
///   Schranke.
///
/// Reine Funktion: kein Zustand, keine Uhr, keine Tabelle. Jede Falle unten ist mit
/// Literalen ausloesbar, ohne Maschine und ohne Tick.
pub fn nohz_plan(rivalen: usize, weckruf: Option<u64>, hat_schranke: bool) -> Nohz {
    // 1. Wer zu verdraengen hat, tickt -- gleich was die Uhr sonst sagt. Ein zweiter
    //    lauffaehiger Thread ohne Tick ist ein verhungernder Thread, kein gesparter Tick.
    if rivalen > 0 {
        return Nohz::Periodic;
    }
    // 2. Wer eine Schranke traegt, tickt ebenfalls. B-5.1 hat Abrechnung (Zyklen) und
    //    Durchsetzung (Ticks) entkoppelt -- die Durchsetzung braucht den Tick noch, und
    //    genau das ist der Punkt von Z5: erst Bedarf, dann Stille.
    if hat_schranke {
        return Nohz::Periodic;
    }
    // 3. Weder Rivale noch Schranke: der Weckruf entscheidet.
    match weckruf {
        None => Nohz::Disarmed,
        // Faellig oder im naechsten Tick faellig: ein One-Shot dafuer ist ein Tick mit
        // Extraschritten (Armieren, Feuern, Rearmieren) -- und drei Stellen statt einer,
        // an denen etwas schiefgehen kann. Periodik ist hier nicht die bequeme, sondern
        // die sparsame Fassung.
        Some(d) if d <= 1 => Nohz::Periodic,
        Some(d) => Nohz::OneShot { ticks: d },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Der Normalfall in alle drei Richtungen: Rivale tickt, Weckruf armiert, Stille schweigt.
    #[test]
    fn drei_ausgaenge_aus_drei_lagen() {
        assert_eq!(nohz_plan(2, None, false), Nohz::Periodic);
        assert_eq!(nohz_plan(0, Some(50), false), Nohz::OneShot { ticks: 50 });
        assert_eq!(nohz_plan(0, None, false), Nohz::Disarmed);
    }

    /// **Fail-closed, erste Haelfte:** Fristen und Weckrufe aendern nichts, solange ein
    /// zweiter Thread lauffaehig ist. Wer hier auf den Weckruf hoerte, liesse den Rivalen
    /// verhungern -- mit Fristen/Zweit-Threads tickt es wie bisher, und dieser Test haelt
    /// das fest.
    #[test]
    fn rivals_schlagen_jeden_weckruf() {
        assert_eq!(nohz_plan(1, None, false), Nohz::Periodic);
        assert_eq!(nohz_plan(1, Some(1000), false), Nohz::Periodic);
        assert_eq!(nohz_plan(3, Some(2), true), Nohz::Periodic);
    }

    /// **Fail-closed, zweite Haelfte:** ein begrenztes Budget haelt den Tick, auch ohne
    /// Rivalen und ohne Weckruf. Die Erschoepfung wird auf dem Tick erkannt (`remaining`
    /// faellt dort); ohne Tick liefe der Thread ueber sein Budget -- Abrechnung (B-5.1)
    /// ohne Durchsetzung waere eine Messung ohne Folge.
    #[test]
    fn schranke_haelt_den_tick_ohne_rivalen() {
        assert_eq!(nohz_plan(0, None, true), Nohz::Periodic);
        assert_eq!(nohz_plan(0, Some(1000), true), Nohz::Periodic);
    }

    /// Der Grenzfall ist kein One-Shot: `0` (jetzt faellig) und `1` (naechster Tick)
    /// bleiben periodisch. Ein One-Shot dafuer feuerte praktisch als Tick -- nur mit
    /// zwei zusaetzlichen Registerprogrammierungen dazwischen.
    #[test]
    fn sofort_faellig_bleibt_periodisch() {
        assert_eq!(nohz_plan(0, Some(0), false), Nohz::Periodic);
        assert_eq!(nohz_plan(0, Some(1), false), Nohz::Periodic);
        // ... und `2` ist der erste echte One-Shot: ein voller Tick Abstand liegt dazwischen.
        assert_eq!(nohz_plan(0, Some(2), false), Nohz::OneShot { ticks: 2 });
    }

    /// Die Schieflage-Richtung: ein zu FRUEH stehender Weckruf (die dokumentierte
    /// `naechste_frist`-Eigenschaft: darf zu frueh stehen, nie zu spaet) erzeugt einen
    /// ueberfluessigen Tick, keinen verlorenen. Zu spaet duerfte er nie stehen -- dafuer
    /// gibt es hier keinen Test, weil die Zusage im Scheduler steht, nicht in der
    /// Entscheidung.
    #[test]
    fn frueher_weckruf_kostet_nur_einen_tick() {
        // Weckruf in 2 Ticks, obwohl in Wahrheit nichts anliegt: ein One-Shot, ein
        // unnoetiges Aufwachen, danach Stille -- kein Verlust, nur ein Tick.
        assert_eq!(nohz_plan(0, Some(2), false), Nohz::OneShot { ticks: 2 });
    }

    /// Rechenkern-Lage aus Z5: genau ein lauffaehiger Thread (kein Rivale), kein Budget,
    /// keine Frist -- der Fall, fuer den NOHZ gebaut ist. Jeder Tick darauf ist reiner
    /// Verlust („OS-Noise"), und genau dieser Ausgang schaltet ihn ab.
    #[test]
    fn rechenkern_ohne_frist_schweigt() {
        assert_eq!(nohz_plan(0, None, false), Nohz::Disarmed);
    }

    /// ... und sobald derselbe Kern eine Frist bewaffnet, programmiert der Weckruf den
    /// Timer um statt ersatzlos zu schweigen: die naechste Frist feuert puenktlich, kein
    /// Tick ohne Grund dazwischen.
    #[test]
    fn frist_programmiert_den_timer_um() {
        assert_eq!(
            nohz_plan(0, Some(237), false),
            Nohz::OneShot { ticks: 237 }
        );
    }
}
