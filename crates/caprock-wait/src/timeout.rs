//! **Warten mit Frist** (A2-Rest) — `wait_event_timeout`, Completion mit Frist, `msleep`.
//!
//! Der dritte Baustein der Linux-Compat: `wait_event` und [`super::Completion`] kennen kein
//! „bis wann", und `msleep` war bisher eine Zahl mit Dokumentation statt einer Funktion (s.
//! [`super::msleep_berechne_ticks`]). Was allen dreien fehlte, war eine Frist im `park` — und
//! die gibt es seit `SYS_PARK_TIMEOUT` (ABI 29): Frist in Ticks in `MSG0`, Rueckkehr mit `OK`
//! (geweckt oder Marke verbraucht) oder `ERR_TIMEOUT` (abgelaufen).
//!
//! ## Der Vertrag ist EIN Trait mit EINER Methode mehr
//!
//! [`TimeoutPark`] erweitert [`super::Park`] um `park_timeout` — dieselbe Weckmarken-Semantik
//! wie `park`, nur dass der Aufruf nach `ticks` Ticks mit `false` zurueckkehrt, statt fuer
//! immer zu schlafen. Alles uebrige gilt unveraendert: die Warteschlange ist eine Liste im
//! Speicher der PD, die Reihenfolge ist Pruefen-vor-Einreihen-vor-Parken, der Ueberlauf ist
//! benannt ([`super::LockError::KeinWarteplatz`]).
//!
//! ## Die Frist ist ABSOLUT, nicht je Runde
//!
//! Alle drei Funktionen rechnen `ende = now + ticks` einmal und parken danach mit dem REST.
//! Ein Weckruf, der die Bedingung nicht erfuellt (fremde Marke, gestohlener Weckruf),
//! verlaengert die Wartezeit nicht — er beginnt nur die naechste Runde. Wer je Runde neu
//! zaehlte, bekaeme bei Dauerfeuer nie eine Frist zu sehen.
//!
//! ## Wer austraegt, wenn die Frist gewinnt
//!
//! Laeuft die Frist ab, steht der eigene Eintrag noch in der Schlange — und duerfte dort
//! nicht bleiben: der naechste `complete`-Weckruf traefe einen Thread, der nicht mehr wartet,
//! und hinterliesse ihm eine Marke, die sein naechstes `park` grundlos sofort zurueckkehren
//! liesse. Deshalb traegt jede Funktion hier ihren Eintrag beim Fristablauf selbst aus
//! ([`WaitQueue::entfernen`]). Ein `complete`, das GLEICHZEITIG eintrifft, geht dabei nicht
//! verloren: der Fertig-Zaehler bleibt stehen, der naechste Aufruf holt ihn ab — die Linux-Kante
//! („Bedingung wurde wahr, waehrend die Frist ablief") ist hier eine verzoegerte Abholung,
//! kein verlorener Weckruf.
//!
//! Kein `unsafe`, keine Abhaengigkeit ausser dem Elternmodul.

use super::{Clock, LockError, Park, Tid, WaitQueue};

/// **Der Vertrag mit dem Kernel, wenn eine Frist dazugehoert.**
///
/// Erweitert [`Park`] um genau eine Methode — was hier nicht steht, kostet die TCB auch
/// nicht: `SYS_PARK_TIMEOUT` ist ein Syscall ohne Cap, wie `SYS_PARK`.
///
/// ## Die Abbildung auf die ABI-Codes
///
/// * `true` = **geweckt**: der Kernel kehrte mit `OK` zurueck — per `UNPARK` geweckt oder eine
///   Weckmarke verbraucht. Die Bedingung MUSS der Aufrufer pruefen (fremde Marke, gestohlener
///   Weckruf); `true` heisst „sieh nach", nicht „sie gilt".
/// * `false` = **Frist abgelaufen**: der Kernel kehrte mit `ERR_TIMEOUT` zurueck.
///
/// `ticks == 0` heisst „keine Frist" und verhaelt sich wie `park` (schlafen bis zum Weckruf) —
/// die Aufrufer hier rufen ihn deshalb mit `0` nie auf, sondern kehren vorher ueber die
/// Bedingung oder den abgelaufenen Rest zurueck. Wer `park_timeout(0)` unmittelbar riefe,
/// schliefe bis zum naechsten Weckruf statt bis gleich.
pub trait TimeoutPark: Park {
    /// Parken mit Frist in Ticks. `true` = geweckt, `false` = Frist abgelaufen.
    fn park_timeout(&self, ticks: u64) -> bool;
}

impl super::WaitQueue {
    /// Alle Eintraege von `t` austragen. Rueckgabe: ob mindestens einer drinstand.
    ///
    /// Idempotent — wer nicht drinsteht, aendert nichts. Die Fristpfade brauchen genau das:
    /// beim Ablauf steht der eigene Eintrag noch (einmal je Runde, bei gestohlenem Weckruf
    /// auch mehrmals), und liegenbleiben duerfte er nicht (s. Modul-Doku).
    pub fn entfernen(&mut self, t: Tid) -> bool {
        let mut gefunden = false;
        let mut rest = 0;
        let mut i = 0;
        while i < self.n {
            if self.tids[i] == t {
                gefunden = true;
            } else {
                self.tids[rest] = self.tids[i];
                rest += 1;
            }
            i += 1;
        }
        self.n = rest;
        gefunden
    }
}

/// **Warten, bis `cond` gilt — hoechstens `ticks` Ticks** — die `wait_event_timeout`-Form.
///
/// Die Linux-Abbildung: Rueckgabe `true` heisst „Bedingung erfüllt" (dort: Rest > 0),
/// `false` heisst „Frist abgelaufen" (dort: 0). `Err` heisst wie bei [`super::wait_event`]:
/// kein Warteplatz — dann wurde NICHT geparkt.
///
/// Die Reihenfolge ist dieselbe wie bei [`super::wait_event`] (erst pruefen, dann einreihen,
/// dann parken), und der Weckruf kommt von derselben Stelle: wer `cond` wahr macht, weckt
/// ueber `q` (Pop-vor-Wecken). `ticks == 0` prueft die Bedingung genau einmal und parkt nie.
pub fn wait_event_timeout(
    p: &dyn TimeoutPark,
    clk: &dyn Clock,
    q: &mut WaitQueue,
    cond: impl Fn() -> bool,
    ticks: u64,
) -> Result<bool, LockError> {
    let ende = clk.now().saturating_add(ticks);
    loop {
        if cond() {
            return Ok(true);
        }
        let rest = ende.saturating_sub(clk.now());
        if rest == 0 {
            // Frist um, ohne je (wieder) geparkt zu haben — ein Eintrag aus einer frueheren
            // Runde duerfte nicht liegenbleiben (s. Modul-Doku). Idempotent, auch beim
            // allerersten Durchlauf.
            q.entfernen(p.me());
            return Ok(false);
        }
        if !q.push(p.me()) {
            // **Nicht parken** — dieselbe Begruendung wie bei `wait_event`: wer schlaeft,
            // ohne in der Liste zu stehen, wird nie geweckt.
            return Err(LockError::KeinWarteplatz);
        }
        if !p.park_timeout(rest) {
            // Frist abgelaufen — der eigene Eintrag steht noch; austragen (s. Modul-Doku).
            q.entfernen(p.me());
            return Ok(false);
        }
        // Geweckt, aber `cond` gilt noch nicht (fremde Marke, gestohlener Weckruf): mit dem
        // REST weiter, nicht von vorn — die Frist ist absolut (s. Modul-Doku). Der Wecker hat
        // den Eintrag bereits ausgetragen (Pop-vor-Wecken).
    }
}

impl super::Completion {
    /// Warten, bis fertig — hoechstens `ticks` Ticks — die
    /// `wait_for_completion_timeout`-Form.
    ///
    /// `Ok(true)` = fertig (und **verbraucht**, wie bei [`super::Completion::warten`]),
    /// `Ok(false)` = Frist abgelaufen (nichts verbraucht — der Zaehler bleibt stehen, ein
    /// gleichzeitiges `complete` holt der naechste Aufruf ab), `Err` = kein Warteplatz.
    pub fn warten_timeout(
        &mut self,
        p: &dyn TimeoutPark,
        clk: &dyn Clock,
        ticks: u64,
    ) -> Result<bool, LockError> {
        let ende = clk.now().saturating_add(ticks);
        loop {
            if self.fertig > 0 {
                self.fertig -= 1;
                return Ok(true);
            }
            let rest = ende.saturating_sub(clk.now());
            if rest == 0 {
                self.warten.entfernen(p.me());
                return Ok(false);
            }
            if !self.warten.push(p.me()) {
                return Err(LockError::KeinWarteplatz);
            }
            if !p.park_timeout(rest) {
                self.warten.entfernen(p.me());
                return Ok(false);
            }
            // Geweckt, aber `fertig` steht noch aus: mit dem REST weiter (s. `wait_event_timeout`).
        }
    }
}

/// **Der aufrufende Thread schlaeft `ms` Millisekunden** — die `msleep`-Form.
///
/// Die Ticks stammen aus [`super::msecs_to_jiffies`] (saettigend, wie dort), das Ende steht
/// danach fest: ein Weckruf verkuerzt den Schlaf NICHT, er beginnt nur die naechste Runde
/// mit dem Rest — genau die Eigenschaft, derentwegen `msleep_berechne_ticks` allein kein
/// `msleep` war (s. dort). `ms == 0` parkt nie und kehrt sofort zurueck.
pub fn msleep(p: &dyn TimeoutPark, clk: &dyn Clock, ms: u64) {
    let ticks = super::msecs_to_jiffies(ms, clk.hz());
    let ende = clk.now().saturating_add(ticks);
    loop {
        let rest = ende.saturating_sub(clk.now());
        if rest == 0 {
            return;
        }
        // Rueckgabe IGNORIERT (geweckt oder Frist): ein Schlaf dauert die volle Zeit.
        p.park_timeout(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Clock, Completion, LockError, Park, Tid, WaitQueue};
    use super::{msleep, wait_event_timeout, TimeoutPark};
    use core::cell::Cell;

    /// Stellvertreter mit Drehbuch: die ersten `weckrufe` Park-Aufrufe kehren geweckt zurueck,
    /// danach mit Fristablauf; jede Runde schiebt die Uhr um `vorschub` vor. `park`/`unpark`
    /// zaehlen nur (Telemetrie statt Kernel).
    struct TestPark {
        ich: Tid,
        hz: u64,
        jetzt: Cell<u64>,
        vorschub: u64,
        weckrufe: Cell<u64>,
        parks: Cell<u64>,
        weckungen: Cell<u64>,
    }

    impl TestPark {
        fn neu(hz: u64, weckrufe: u64, vorschub: u64) -> TestPark {
            TestPark {
                ich: 7,
                hz,
                jetzt: Cell::new(0),
                vorschub,
                weckrufe: Cell::new(weckrufe),
                parks: Cell::new(0),
                weckungen: Cell::new(0),
            }
        }
    }

    impl Park for TestPark {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            self.parks.set(self.parks.get() + 1);
        }
        fn unpark(&self, _t: Tid) {
            self.weckungen.set(self.weckungen.get() + 1);
        }
    }

    impl Clock for TestPark {
        fn hz(&self) -> u64 {
            self.hz
        }
        fn now(&self) -> u64 {
            self.jetzt.get()
        }
    }

    impl TimeoutPark for TestPark {
        fn park_timeout(&self, _ticks: u64) -> bool {
            self.parks.set(self.parks.get() + 1);
            self.jetzt
                .set(self.jetzt.get().saturating_add(self.vorschub));
            if self.weckrufe.get() > 0 {
                self.weckrufe.set(self.weckrufe.get() - 1);
                return true;
            }
            false
        }
    }

    #[test]
    fn entfernen_tilgt_alle_eigenen_und_nur_die() {
        let mut q = WaitQueue::new();
        assert!(q.push(1));
        assert!(q.push(2));
        assert!(q.push(1));
        assert!(q.entfernen(1));
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop(), Some(2));
        assert!(!q.entfernen(9));
        assert!(q.is_empty());
    }

    #[test]
    fn wait_event_timeout_bedingung_gilt_sofort_ohne_park() {
        let p = TestPark::neu(1000, 0, 0);
        let mut q = WaitQueue::new();
        assert_eq!(wait_event_timeout(&p, &p, &mut q, || true, 100), Ok(true));
        assert_eq!(p.parks.get(), 0);
        assert!(q.is_empty());
    }

    #[test]
    fn wait_event_timeout_weckruf_mit_bedingung() {
        let p = TestPark::neu(1000, 1, 0);
        let mut q = WaitQueue::new();
        let aufrufe = Cell::new(0u64);
        let ok = wait_event_timeout(
            &p,
            &p,
            &mut q,
            || {
                aufrufe.set(aufrufe.get() + 1);
                aufrufe.get() >= 2
            },
            100,
        );
        assert_eq!(ok, Ok(true));
        assert_eq!(p.parks.get(), 1);
        // Der Stellvertreter weckt OHNE Pop — ein echter Warten-Wecker traegt vorher aus
        // (Pop-vor-Wecken); der Eintrag gehoert also ihm, nicht uns. Was hier steht, ist
        // genau dieser Pop, nachgeholt.
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop(), Some(7));
        assert!(q.is_empty());
    }

    #[test]
    fn wait_event_timeout_frist_ist_absolut_trotz_stoerfeuer() {
        // Zwei gestohlene Weckrufe, die Uhr laeuft (Vorschub 2 bei Frist 5): drei Runden,
        // dann Ablauf — je Runde neu gezaehlt, kaeme die Frist bei Dauer-`true` nie.
        let p = TestPark::neu(1000, 2, 2);
        let mut q = WaitQueue::new();
        assert_eq!(
            wait_event_timeout(&p, &p, &mut q, || false, 5),
            Ok(false)
        );
        assert_eq!(p.parks.get(), 3);
        assert!(q.is_empty());
    }

    #[test]
    fn wait_event_timeout_voll_meldet_statt_zu_parken() {
        let p = TestPark::neu(1000, 0, 0);
        let mut q = WaitQueue::new();
        for _ in 0..super::super::WARTEPLAETZE {
            assert!(q.push(1));
        }
        let vorher = p.parks.get();
        assert_eq!(
            wait_event_timeout(&p, &p, &mut q, || false, 100),
            Err(LockError::KeinWarteplatz)
        );
        assert_eq!(p.parks.get(), vorher);
    }

    #[test]
    fn warten_timeout_holt_fertig_ohne_park() {
        let p = TestPark::neu(1000, 0, 0);
        let mut c = Completion::new();
        c.complete(&p);
        assert_eq!(c.warten_timeout(&p, &p, 100), Ok(true));
        assert_eq!(p.parks.get(), 0);
        assert_eq!(c.offen(), 0);
    }

    #[test]
    fn warten_timeout_frist_traegt_aus_und_verliert_kein_complete() {
        let p = TestPark::neu(1000, 0, 0);
        let mut c = Completion::new();
        assert_eq!(c.warten_timeout(&p, &p, 5), Ok(false));
        assert_eq!(p.parks.get(), 1);
        // Der Fristablauf hat den Eintrag ausgetragen: `complete` weckt NIEMANDEN (keine
        // Marke an einen Thread, der nicht mehr wartet) — und der Abschluss steht trotzdem.
        assert_eq!(c.complete(&p), 0);
        assert_eq!(p.weckungen.get(), 0);
        assert_eq!(c.warten_timeout(&p, &p, 5), Ok(true));
    }

    #[test]
    fn warten_timeout_voll_meldet_statt_zu_parken() {
        let p = TestPark::neu(1000, 0, 0);
        let mut c = Completion::new();
        for _ in 0..super::super::WARTEPLAETZE {
            let _ = c.warten_schritt(&p);
        }
        let vorher = p.parks.get();
        assert_eq!(c.warten_timeout(&p, &p, 100), Err(LockError::KeinWarteplatz));
        assert_eq!(p.parks.get(), vorher);
    }

    #[test]
    fn msleep_dauert_trotz_weckruf_die_volle_zeit() {
        // Dauerfeuer an Weckrufen, die Uhr laeuft (Vorschub 1): 10 ms bei 1000 Hz sind
        // 10 Ticks, also 10 Runden — ein Weckruf verkuerzt nichts.
        let p = TestPark::neu(1000, u64::MAX, 1);
        msleep(&p, &p, 10);
        assert_eq!(p.parks.get(), 10);
        assert_eq!(p.now(), 10);
    }

    #[test]
    fn msleep_ohne_zeit_parkt_nicht() {
        let p = TestPark::neu(1000, 0, 0);
        msleep(&p, &p, 0);
        assert_eq!(p.parks.get(), 0);
    }
}
