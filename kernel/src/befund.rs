//! **How a probe came out — three values, not two.**
//!
//! ## The hole this closes
//!
//! `all_done()` reads a `bool`. A probe that could not run has to be booked as one of the two,
//! and both bookings are wrong:
//!
//! * as `true` — then *not measured* is indistinguishable from *passed*. That is the `vnet` line:
//!   `println!("vnet : SKIP …"); VNET_OK.store(true, …)`. The only network evidence in the system
//!   reports itself green when the card is absent, and in the load suite `vnet` does not appear
//!   at all.
//! * as `false` — then a machine that legitimately lacks the device never reaches its report and
//!   runs into the watchdog. From outside that looks exactly like a hang, and this repository has
//!   twice paid days for that particular indistinguishability.
//!
//! Faced with those two, the tree picked both — differently in different places. `pprobe` and
//! `isohigh` print `SKIP` and let the **shell** decide; `vnet` sets its own kernel-side gate to
//! `true`. The first is honest, the second is not, and nothing said which was intended.
//!
//! ## The division of labour, and it is the point
//!
//! **The kernel says which of the three. The suite says whether a `NichtGefahren` is acceptable
//! *here*.** A run without a block device may skip `vnet`; the load suite may not, because it
//! brings the card. That is a property of the run, not of the kernel, and it therefore belongs in
//! the suite — where `endow` already makes exactly this distinction (`SKIP` in the main suite,
//! `FAIL` in the load suite).
//!
//! ## Why `NichtGefahren` passes the gate
//!
//! Because the gate exists to trigger the *report*, not to pronounce the verdict. A skipped probe
//! must not stop the report — it must appear **in** it. Whoever wants it to be fatal says so in
//! the suite, and then the missing line is a named failure instead of a watchdog.

/// Der Ausgang einer Sonde.
///
/// **Kein `Option<bool>`**, und das ist kein Geschmack: `None` heisst „kein Wert", und genau
/// dagegen steht der ganze Typ. Ein `NichtGefahren` ist ein **Befund** — die Sonde hat
/// festgestellt, dass ihre Frage hier nicht entscheidbar ist. Wer das als „kein Wert" schreibt,
/// laedt jeden Aufrufer ein, `unwrap_or(true)` zu tippen, und das ist die `vnet`-Zeile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Befund {
    /// Gemessen, und die Aussage haelt.
    Bestanden,
    /// Gemessen, und die Aussage haelt **nicht**. Nur dieser Wert gattert.
    Durchgefallen,
    /// **Nicht entscheidbar** — kein Geraet, keine Startmenge, kein Platz. Weder bestanden noch
    /// durchgefallen, und es steht als solches im Bericht.
    NichtGefahren,
}

impl Befund {
    /// Verhindert dieser Ausgang den Abschluss? **Nur `Durchgefallen`.**
    pub fn gattert(self) -> bool {
        self == Befund::Durchgefallen
    }

    /// Das Wort fuer die Berichtszeile.
    pub fn wort(self) -> &'static str {
        match self {
            Befund::Bestanden => "ALL PASS",
            Befund::Durchgefallen => "FAILURES",
            Befund::NichtGefahren => "SKIP",
        }
    }

    /// Fuer den Bericht, der heute Booleans druckt: `NichtGefahren` ist **nicht** `true`.
    ///
    /// Bewusst kein `From<Befund> for bool` — eine stillschweigende Umwandlung waere genau der
    /// Weg zurueck in das Problem. Wer ein `bool` will, muss sagen, wohin er `SKIP` bucht.
    pub fn ist_bestanden(self) -> bool {
        self == Befund::Bestanden
    }
}

impl From<bool> for Befund {
    /// Eine gemessene Aussage hat nur zwei Ausgaenge — die vorhandenen `bool`-Sonden wandern damit
    /// unveraendert in die Liste, ohne dass jede einzeln umgeschrieben werden muss.
    fn from(b: bool) -> Self {
        if b {
            Befund::Bestanden
        } else {
            Befund::Durchgefallen
        }
    }
}

/// **Ein [`Befund`] in einem Atomic** — weil `Befund` selbst keiner sein kann.
///
/// Jede Sonde braucht dieselben drei Zustaende in einem `static`, und jede haette sie sonst mit
/// eigenen `u8`-Konstanten nachgebaut. Drei Nachbauten sind drei Gelegenheiten, `NichtGefahren`
/// versehentlich auf denselben Wert wie `Bestanden` zu legen — und genau diese Verwechslung ist
/// der Fehler, gegen den der Typ antritt.
pub struct AtomicBefund(core::sync::atomic::AtomicU8);

impl AtomicBefund {
    /// **Vorgabe ist `NichtGefahren`, nicht `Durchgefallen`.**
    ///
    /// Eine Sonde, die nie lief, ist nicht durchgefallen — und sie hat auch nicht bestanden. Die
    /// alte Vorgabe `false` war der Grund, warum die drei `urteil()` nicht in `all_done()`
    /// haengen konnten: auf einer Maschine ohne das noetige Geraet haetten sie den Lauf in den
    /// Watchdog geschickt.
    pub const fn neu() -> Self {
        Self(core::sync::atomic::AtomicU8::new(2))
    }

    pub fn setzen(&self, b: Befund) {
        let v = match b {
            Befund::Durchgefallen => 0,
            Befund::Bestanden => 1,
            Befund::NichtGefahren => 2,
        };
        self.0.store(v, core::sync::atomic::Ordering::Release);
    }

    /// Kurzform fuer den Normalfall „gemessen, und so ging es aus".
    pub fn gemessen(&self, ok: bool) {
        self.setzen(Befund::from(ok));
    }

    /// Kurzform fuer „hier nicht entscheidbar".
    pub fn uebersprungen(&self) {
        self.setzen(Befund::NichtGefahren);
    }

    pub fn lesen(&self) -> Befund {
        match self.0.load(core::sync::atomic::Ordering::Acquire) {
            0 => Befund::Durchgefallen,
            1 => Befund::Bestanden,
            _ => Befund::NichtGefahren,
        }
    }
}
