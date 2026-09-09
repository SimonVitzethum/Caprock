//! **Warten, ohne den Kernel zu fragen** (Z22, P2) — Mutex, Warteschlange und Completion für
//! eine PD mit mehreren Threads.
//!
//! ## Wofür das da ist
//!
//! Ein Linux-Treiber ist voll von `spin_lock_irqsave`, `wait_event`, `complete()` und
//! `mutex_lock`. In Linux heisst „Sperre gegen den Interrupt" wörtlich *Interrupts abschalten* —
//! in einer PD gibt es das nicht und braucht es auch nicht: der Interrupt kommt dort als
//! **Notification an einen Thread** an (ein *threaded IRQ*), und was den Handler gegen den Rest
//! des Treibers ausschliesst, ist gewöhnlicher Ausschluss zwischen **Threads derselben PD**.
//!
//! Genau den baut diese Crate — und zwar so, dass er **im unbestrittenen Fall den Kernel gar
//! nicht anfasst**.
//!
//! ## Der Vertrag mit dem Kernel ist EIN Trait mit ZWEI Methoden
//!
//! [`Park`] — „lege mich schlafen" und „wecke Thread T". Mehr nimmt diese Crate nicht in
//! Anspruch, und mehr steht in der TCB dafür auch nicht: `SYS_PARK`/`SYS_UNPARK`, zwei Bits je
//! TCB. Jede Warteschlange hier ist eine **Liste im Speicher der PD** — kein Kernelobjekt, kein
//! Cap-Slot, keine Kapazitätsgrenze im Kernel (`Notification` fasst genau **einen** Wartenden,
//! s. D11).
//!
//! ## Die Eigenschaft, um die alles kreist
//!
//! Zwischen „ich habe festgestellt, dass ich warten muss" und „ich schlafe" liegt ein Fenster.
//! Trifft der Weckruf hinein, ginge er verloren und der Thread schliefe für immer. Deshalb
//! **hinterlegt `unpark` immer eine Marke**, auch an einem wachen Thread, und `park` verbraucht
//! sie, statt zu schlafen. Diese Crate darf sich darauf verlassen — und die Tests hier führen
//! genau diese Reihenfolge vor, mit einem Stellvertreter statt einem Kernel.
//!
//! Abhängigkeitsfrei und `forbid(unsafe_code)`.

#![no_std]
#![forbid(unsafe_code)]

pub mod timeout;

/// Ein Thread-Bezeichner, wie ihn die PD sieht. Undurchsichtig — diese Crate rechnet nicht damit,
/// sie reicht ihn nur an [`Park`] durch.
pub type Tid = u64;

/// **Der ganze Vertrag mit dem Kernel.**
///
/// Zwei Methoden, und das ist Absicht: was hier nicht steht, kann die TCB auch nicht kosten.
pub trait Park {
    /// Die eigene [`Tid`].
    fn me(&self) -> Tid;
    /// Schlafen legen — **es sei denn, eine Weckmarke liegt vor**; dann sofort zurückkehren und
    /// die Marke verbrauchen. Diese Bedingung ist tragend, nicht bequem (s. Modul-Doku).
    fn park(&self);
    /// `t` wecken. Die Marke wird **immer** hinterlegt, auch wenn `t` gerade wach ist.
    fn unpark(&self, t: Tid);
}

/// Wie viele Wartende eine Warteschlange fasst.
///
/// **Der Überlauf ist benannt, nicht still** (D11): `push` gibt `false`, und jeder Aufrufer
/// hier behandelt das. Ein `if platz { .. }` ohne `else` wäre genau der Fehler, der einen Thread
/// dauerhaft hängen lässt, während jeder Prüfer Ordnung meldet.
pub const WARTEPLAETZE: usize = 32;

/// Eine Warteschlange von Threads — **eine Liste im Speicher der PD**, kein Kernelobjekt.
pub struct WaitQueue {
    tids: [Tid; WARTEPLAETZE],
    n: usize,
    /// Wie oft ein Eintragen mangels Platz abgewiesen wurde. Telemetrie, damit „die Schlange ist
    /// zu kurz" sichtbar wird, statt sich als Hänger zu äussern.
    abgewiesen: u32,
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    pub const fn new() -> WaitQueue {
        WaitQueue {
            tids: [0; WARTEPLAETZE],
            n: 0,
            abgewiesen: 0,
        }
    }

    /// Einreihen. `false` = **kein Platz** — der Aufrufer darf sich dann nicht schlafen legen.
    #[must_use = "ein abgewiesenes Einreihen, gefolgt von `park`, ist ein Thread, der nie wieder aufwacht"]
    pub fn push(&mut self, t: Tid) -> bool {
        if self.n >= WARTEPLAETZE {
            self.abgewiesen += 1;
            return false;
        }
        self.tids[self.n] = t;
        self.n += 1;
        true
    }

    /// Den ältesten Wartenden herausnehmen (FIFO — ein LIFO liesse den ersten Wartenden bei Last
    /// beliebig lange liegen).
    pub fn pop(&mut self) -> Option<Tid> {
        if self.n == 0 {
            return None;
        }
        let t = self.tids[0];
        self.tids.copy_within(1..self.n, 0);
        self.n -= 1;
        Some(t)
    }

    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    /// Wie oft mangels Platz abgewiesen wurde. `> 0` heisst: die Schlange ist zu kurz bemessen.
    pub fn abgewiesen(&self) -> u32 {
        self.abgewiesen
    }

    /// Alle wecken (`wake_up_all`). Gibt zurück, wie viele geweckt wurden.
    pub fn wake_all(&mut self, p: &dyn Park) -> usize {
        let mut n = 0;
        while let Some(t) = self.pop() {
            p.unpark(t);
            n += 1;
        }
        n
    }

    /// Einen wecken (`wake_up`). `false`, wenn niemand wartete.
    pub fn wake_one(&mut self, p: &dyn Park) -> bool {
        match self.pop() {
            Some(t) => {
                p.unpark(t);
                true
            }
            None => false,
        }
    }
}

/// **Ein Mutex ohne Kernelobjekt.**
///
/// Der unbestrittene Weg ist ein Flag und **null Syscalls**; erst der bestrittene reiht ein und
/// parkt. Damit ist die teure Seite genau dort, wo auch der Streit ist.
///
/// Kein `MutexGuard` und kein Inhalt: diese Crate hütet **Ausschluss**, nicht Daten. Ein Shim,
/// der Linux-Code fährt, hat seine Daten ohnehin hinter rohen Zeigern — ein Guard-Typ hier
/// suggerierte eine Zusicherung, die er nicht geben kann.
pub struct Mutex {
    gesperrt: bool,
    inhaber: Tid,
    warten: WaitQueue,
}

impl Default for Mutex {
    fn default() -> Self {
        Self::new()
    }
}

/// Warum ein `lock` nicht gelang.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockError {
    /// Die Warteschlange ist voll. **Benannt statt still** — der Aufrufer parkt dann NICHT.
    KeinWarteplatz,
    /// Der Aufrufer hält die Sperre bereits. Ein Selbst-Deadlock ist in Treibercode ein
    /// verbreiteter Fehler und äussert sich sonst als Hänger; hier wird er gemeldet.
    /// (Die eigene Fallenliste kennt die Form: `match lock() { .. None => lock() }`.)
    SelbstDeadlock,
}

impl Mutex {
    pub const fn new() -> Mutex {
        Mutex {
            gesperrt: false,
            inhaber: 0,
            warten: WaitQueue::new(),
        }
    }

    /// Versuchen, ohne zu warten. `true` = bekommen.
    pub fn try_lock(&mut self, p: &dyn Park) -> bool {
        if self.gesperrt {
            return false;
        }
        self.gesperrt = true;
        self.inhaber = p.me();
        true
    }

    /// **Einen Warteschritt.** Gibt `Ok(true)`, wenn die Sperre erlangt wurde; `Ok(false)` heisst
    /// „geparkt und wieder aufgewacht, bitte noch einmal".
    ///
    /// Warum kein `while`-Rumpf hier drin: `park` gehört dem Aufrufer, und diese Crate darf ihn
    /// nicht in einer Schleife festhalten, deren Abbruchbedingung sie selbst prüft — der
    /// Aufrufer soll sehen, dass es eine Schleife ist. `lock` unten ist die bequeme Fassung.
    pub fn lock_schritt(&mut self, p: &dyn Park) -> Result<bool, LockError> {
        if self.gesperrt {
            if self.inhaber == p.me() {
                return Err(LockError::SelbstDeadlock);
            }
            if !self.warten.push(p.me()) {
                // **Nicht parken.** Wer sich schlafen legt, ohne in der Liste zu stehen, wird nie
                // geweckt -- exakt die Form von D11, und dort meldete jeder Prüfer Ordnung.
                return Err(LockError::KeinWarteplatz);
            }
            p.park();
            return Ok(false);
        }
        self.gesperrt = true;
        self.inhaber = p.me();
        Ok(true)
    }

    /// Sperren, bis es klappt.
    pub fn lock(&mut self, p: &dyn Park) -> Result<(), LockError> {
        loop {
            if self.lock_schritt(p)? {
                return Ok(());
            }
        }
    }

    /// Freigeben und **einen** Wartenden wecken. `false`, wenn der Aufrufer die Sperre gar nicht
    /// hielt — auch das wird gemeldet statt stillschweigend gutgeheissen: eine fremde Freigabe
    /// gäbe zwei Threads gleichzeitig Zutritt.
    pub fn unlock(&mut self, p: &dyn Park) -> bool {
        if !self.gesperrt || self.inhaber != p.me() {
            return false;
        }
        self.gesperrt = false;
        self.inhaber = 0;
        self.warten.wake_one(p);
        true
    }

    pub fn is_locked(&self) -> bool {
        self.gesperrt
    }
    pub fn wartende(&self) -> usize {
        self.warten.len()
    }
}

/// **Das Gegenstück zu Linux' `struct completion`** — „die Anfrage ist fertig".
///
/// Das ist die Form, in der ein threaded IRQ mit dem Rest des Treibers redet: der Handler ruft
/// [`Self::complete`], der wartende Thread stand in [`Self::warten_schritt`].
///
/// **`fertig` ist ein Zähler, kein Flag**, und das ist der Punkt: ein `complete()`, das eintrifft,
/// **bevor** jemand wartet, muss erhalten bleiben. Ein Flag genügte dafür zwar auch — ein Zähler
/// hält aber zusätzlich fest, wenn mehr Abschlüsse kamen als abgeholt wurden, und das ist eine
/// Aussage über das Gerät.
pub struct Completion {
    fertig: u32,
    warten: WaitQueue,
}

impl Default for Completion {
    fn default() -> Self {
        Self::new()
    }
}

impl Completion {
    pub const fn new() -> Completion {
        Completion {
            fertig: 0,
            warten: WaitQueue::new(),
        }
    }

    /// Aus dem IRQ-Thread: „fertig". Weckt alle Wartenden.
    pub fn complete(&mut self, p: &dyn Park) -> usize {
        self.fertig = self.fertig.saturating_add(1);
        self.warten.wake_all(p)
    }

    /// Ein Warteschritt. `true` = fertig (und **verbraucht**), `false` = geparkt gewesen.
    ///
    /// Die Reihenfolge ist tragend: **erst** den Zähler prüfen, **dann** einreihen, **dann**
    /// parken. Ein `complete`, das zwischen Prüfung und Parken eintrifft, hinterlegt im Kernel
    /// eine Weckmarke, und das `park` kehrt sofort zurück.
    pub fn warten_schritt(&mut self, p: &dyn Park) -> Result<bool, LockError> {
        if self.fertig > 0 {
            self.fertig -= 1;
            return Ok(true);
        }
        if !self.warten.push(p.me()) {
            return Err(LockError::KeinWarteplatz);
        }
        p.park();
        Ok(false)
    }

    /// Warten, bis fertig.
    pub fn warten(&mut self, p: &dyn Park) -> Result<(), LockError> {
        loop {
            if self.warten_schritt(p)? {
                return Ok(());
            }
        }
    }

    /// Wie viele Abschlüsse noch nicht abgeholt sind.
    pub fn offen(&self) -> u32 {
        self.fertig
    }
}

/// **Zeit, in der Einheit, in der Treiber denken.**
///
/// Linux zählt Zeit in *Jiffies*: Ticks seit dem Start, `HZ` Stück je Sekunde. Eine PD hat
/// keinen globalen Tick — was sie hat, ist eine Uhr, die jemand stellt (der PD-Start, ein
/// Timer-Thread, eines Tages der Kernel). Dieses Trait benennt die zwei Dinge, die Treibercode
/// braucht: die Auflösung und den Stand. Mehr nicht — wie bei [`Park`] gilt: was hier nicht
/// steht, kostet auch nichts.
pub trait Clock {
    /// Ticks je Sekunde (Linux: `HZ`).
    fn hz(&self) -> u64;
    /// Aktueller Stand in Ticks (Linux: `jiffies`).
    fn now(&self) -> u64;
}

/// Millisekunden in Ticks — die `msecs_to_jiffies`-Form, aber **sättigend statt
/// überlaufend**: was nicht darstellbar ist, wird `u64::MAX`, nicht 0.
///
/// Ein Überlauf, der zu 0 würde, liesse einen Timeout sofort verfallen — dieselbe Fehlerform
/// wie ein verlorener Weckruf, nur in der Zeit: der Prüfer meldet Ordnung, der Thread wartet
/// nie. Die Division stutzt ab (kein Aufrunden wie in Linux); wer Aufrunden braucht, addiert
/// vorher `1000 - 1` — sichtbar am Aufrufort, nicht versteckt hier.
pub fn msecs_to_jiffies(ms: u64, hz: u64) -> u64 {
    match ms.checked_mul(hz) {
        Some(v) => v / 1000,
        None => u64::MAX,
    }
}

/// Der aktuelle Stand der Uhr in Ticks — `now()`, unter dem Namen, den Treibercode erwartet
/// (`jiffies`). Kein Rechnen, kein Runden: ein Jiffy ist hier ein Uhr-Tick, und die Auflösung
/// steckt in [`Clock::hz`], nicht in dieser Funktion.
pub fn jiffies<C: Clock>(clk: &C) -> u64 {
    clk.now()
}

/// Wie viele Timer ein Rad fasst. **Benannt statt still**, wie bei [`WARTEPLAETZE`]: `after`
/// gibt `false`, und der Aufrufer behandelt das — ein Timer, der nie eingetragen wurde und
/// trotzdem erwartet wird, ist ein Weckruf, der nie kommt.
pub const TIMERPLAETZE: usize = 32;

/// Ein Eintrag im [`TimerWheel`]: „wecke `tid`, sobald `faellig` erreicht ist".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimerEintrag {
    /// Tick-Stand, ab dem geweckt wird (`faellig <= now` heisst fällig).
    pub faellig: u64,
    pub tid: Tid,
}

/// **Ein Zeitrad ohne Kernelobjekt** — die `timer_list`-Form für eine PD: eine Liste im
/// Speicher der PD, kein Kernel-Timer, keine Frist im [`Park`].
///
/// Der Rad-Gedanke ist absichtlich dünn: Eintragen ([`Self::after`]) legt eine Fälligkeit ab,
/// Austragen ([`Self::expire`]) weckt alle Fälligen in Einreihungsfolge (FIFO — dieselbe
/// Begründung wie bei [`WaitQueue::pop`]). Wer das Rad dreht — also `expire` mit frischem
/// `now` ruft —, ist Sache des Treibers (typisch der Timer-Thread); diese Crate hütet die
/// Liste, nicht den Takt.
pub struct TimerWheel {
    eintraege: [Option<TimerEintrag>; TIMERPLAETZE],
    n: usize,
}

impl Default for TimerWheel {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerWheel {
    pub const fn new() -> TimerWheel {
        TimerWheel {
            eintraege: [None; TIMERPLAETZE],
            n: 0,
        }
    }

    /// Eintragen: „wecke `tid` in `ticks` Ticks, von `now` aus". Die Fälligkeit sättigt
    /// (`saturating_add`): ein Überlauf verlegt den Timer an das Ende der Zeit, nicht an den
    /// Anfang — ein Timer, der sofort verfiele, weil die Rechnung überlief, wäre wieder die
    /// Zeit-Form des verlorenen Weckrufs. `false` = **kein Platz** — der Aufrufer darf dann
    /// nicht davon ausgehen, geweckt zu werden.
    #[must_use = "ein abgewiesenes Eintragen, auf das trotzdem gewartet wird, ist ein Thread, der nie wieder aufwacht"]
    pub fn after(&mut self, now: u64, ticks: u64, tid: Tid) -> bool {
        if self.n >= TIMERPLAETZE {
            return false;
        }
        self.eintraege[self.n] = Some(TimerEintrag {
            faellig: now.saturating_add(ticks),
            tid,
        });
        self.n += 1;
        true
    }

    /// Austragen: alle Fälligen (`faellig <= now`) wecken, in Einreihungsfolge. Gibt zurück,
    /// wie viele geweckt wurden. Nicht-Fällige bleiben stehen, an ihrer relativen Reihenfolge
    /// ändert sich nichts.
    pub fn expire(&mut self, now: u64, p: &dyn Park) -> usize {
        let mut geweckt = 0;
        let mut rest = 0;
        for i in 0..self.n {
            match self.eintraege[i] {
                Some(e) if e.faellig <= now => {
                    p.unpark(e.tid);
                    geweckt += 1;
                }
                Some(e) => {
                    self.eintraege[rest] = Some(e);
                    rest += 1;
                }
                None => {}
            }
        }
        for i in rest..self.n {
            self.eintraege[i] = None;
        }
        self.n = rest;
        geweckt
    }

    /// Austragen ohne Wecken (`del_timer`): alle Einträge von `tid` entfernen, bevor sie
    /// fällig werden. Rückgabe: ob mindestens einer drinstand. Idempotent — wer nicht
    /// drinsteht, ändert nichts. Wer nach dem Ablauf austrägt, findet nichts mehr: `expire`
    /// hat den Eintrag bereits verbraucht, und `false` heisst dann „nichts zu tun", nicht
    /// „fehlgeschlagen" (die Linux-Kante „del_timer nach Ablauf" ist hier kein Fehler).
    pub fn absagen(&mut self, tid: Tid) -> bool {
        let mut gefunden = false;
        let mut rest = 0;
        for i in 0..self.n {
            match self.eintraege[i] {
                Some(e) if e.tid == tid => {
                    gefunden = true;
                }
                Some(e) => {
                    self.eintraege[rest] = Some(e);
                    rest += 1;
                }
                None => {}
            }
        }
        for i in rest..self.n {
            self.eintraege[i] = None;
        }
        self.n = rest;
        gefunden
    }

    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
}

/// **Warten, bis `cond` gilt** — die `wait_event`-Form.
///
/// Das Muster ist dasselbe wie bei [`Completion::warten_schritt`], und die Reihenfolge ist aus
/// demselben Grund tragend: **erst** prüfen, **dann** einreihen, **dann** parken. Ein Weckruf,
/// der zwischen Prüfung und Parken eintrifft, hinterlegt im Kernel eine Marke, und das `park`
/// kehrt sofort zurück. Wer die Prüfung erst nach dem Einreihen vornähme, schliefe bei bereits
/// erfüllter Bedingung; wer ohne Einreihen parkte, würde nie geweckt (D11).
///
/// Der Weckruf kommt von aussen: wer `cond` wahr macht, weckt über `q` (`wake_one`/`wake_all`) —
/// genau wie der IRQ-Thread bei [`Completion::complete`]. Diese Funktion hütet nur die
/// wartende Seite.
pub fn wait_event(
    p: &dyn Park,
    q: &mut WaitQueue,
    cond: impl Fn() -> bool,
) -> Result<(), LockError> {
    loop {
        if cond() {
            return Ok(());
        }
        if !q.push(p.me()) {
            // **Nicht parken** — dieselbe Begründung wie überall in dieser Datei: wer schläft,
            // ohne in der Liste zu stehen, wird nie geweckt.
            return Err(LockError::KeinWarteplatz);
        }
        p.park();
    }
}

/// Millisekunden in Schlaf-Ticks umrechnen — die Hälfte von `msleep`, die ohne Kernel geht.
///
/// Warum es kein blockierendes `msleep` gibt: [`Park`] kennt **keine Frist**. Wer „100 ms
/// schlafen" in eine blosse Park-Schleife übersetzte, bekäme beides falsch: ohne Weckruf
/// schliefe er für immer (kein Timer weckt ihn — das Rad oben weckt nur, wer es dreht), und
/// mit Weckruf schliefe er zu kurz (jeder Weckruf beendet den Schlaf, nicht die Zeit). Ein
/// Schlaf braucht eine Frist im `park`, und die gibt es erst mit dem A2-Rest (`park_timeout`
/// ist ein Kernel-/ABI-Eingriff und gehört nicht in diese Crate). Bis dahin sind diese Ticks
/// eine Zahl mit Dokumentation statt einer Funktion mit Hänger: `sleep_bis` zählt den Rest
/// herunter, und der Aufrufer parkt — und wacht — wie bei [`wait_event`].
pub fn msleep_berechne_ticks(ms: u64, hz: u64) -> u64 {
    msecs_to_jiffies(ms, hz)
}

/// Rest-Ticks bis `ziel`, von `now` aus — **sättigend 0 statt unterlaufend**: wer hinter dem
/// Ziel liegt, ist fertig, nicht „noch 2^64 Ticks schuldig". Ein Unterlauf würde einen längst
/// fälligen Schlaf in die ferne Zukunft verlegen — die Zeit-Form des verlorenen Weckrufs.
pub fn sleep_bis(now: u64, ziel: u64) -> u64 {
    ziel.saturating_sub(now)
}

/// Ein Auftrag in der [`Workqueue`]. Undurchsichtig wie [`Tid`]: diese Crate rechnet nicht
/// damit, sie reicht ihn nur vom Einreicher an den Bearbeiter durch.
pub type JobId = u64;

/// **Eine Auftragsliste für spätere Arbeit** — die `workqueue`-Form für eine PD.
///
/// Der Einreicher ruft [`Self::queue`], der Bearbeiter zieht per [`Self::run_ein_job`] und
/// führt aus. Zwei Dinge bewusst nicht: `queue` weckt niemanden — die Liste weiss nicht, wer
/// zieht, und ein Weckruf an niemanden wäre eine Marke an niemanden (der Bearbeiter zieht, er
/// wird nicht gerufen). Und `run_ein_job` führt aus statt zu wecken — der Auftrag ist Arbeit,
/// kein Wartender. Wer einen schlafenden Bearbeiter braucht, legt eine [`Completion`] daneben
/// und ruft dort `complete`: Liste hier, Weckruf dort, jede Seite eine Aufgabe.
pub struct Workqueue {
    q: WaitQueue,
}

impl Default for Workqueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Workqueue {
    pub const fn new() -> Workqueue {
        Workqueue {
            q: WaitQueue::new(),
        }
    }

    /// Einreichen. `Err(KeinWarteplatz)` = **kein Platz** — der Auftrag ist dann NICHT
    /// eingetragen und darf nicht als erledigt gelten; stilles Fallenlassen wäre ein
    /// Auftrag, der nie läuft, während jeder Prüfer Ordnung meldet.
    ///
    /// (`p` braucht es heute nicht — kein Weckruf, s. Struktur-Doku —, es steht in der
    /// Signatur, damit jeder Aufrufer den Park-Vertrag dabehat wie bei Mutex und Completion.)
    pub fn queue(&mut self, _p: &dyn Park, job: JobId) -> Result<(), LockError> {
        if !self.q.push(job) {
            return Err(LockError::KeinWarteplatz);
        }
        Ok(())
    }

    /// **Einen Auftrag abarbeiten**: den ältesten ziehen (FIFO) und `f` damit ausführen.
    /// `true` = es gab Arbeit, `false` = die Liste war leer. (`p` wie bei [`Self::queue`]:
    /// Vertrags-Symmetrie, heute ohne Weckruf — gezogen wird, nicht gerufen.)
    pub fn run_ein_job(&mut self, _p: &dyn Park, f: impl Fn(JobId)) -> bool {
        match self.q.pop() {
            Some(job) => {
                f(job);
                true
            }
            None => false,
        }
    }

    /// „Leerspülen" als **Nachweis, nicht als Blockade**: `true` heisst „nichts mehr
    /// ausstehend" (`len == 0`). Ein blockierendes `flush` ginge hier nicht — blockieren hiesse
    /// parken, und parken ohne Weckruf hiesse hängen (s. `msleep`-Doku). Wer auf Leere warten
    /// muss, prüft in einer [`wait_event`]-Schleife mit `self.is_empty()` als Bedingung.
    pub fn flush(&self) -> bool {
        self.q.is_empty()
    }

    pub fn len(&self) -> usize {
        self.q.len()
    }
    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }
    /// Wie oft mangels Platz abgewiesen wurde — s. [`WaitQueue::abgewiesen`].
    pub fn abgewiesen(&self) -> u32 {
        self.q.abgewiesen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    /// **Ein Stellvertreter für den Kernel** — mit genau der Eigenschaft, um die es geht: eine
    /// Weckmarke, die auch an einem wachen Thread liegen bleibt.
    ///
    /// Derselbe Weg wie bei `ipctreue`: die Logik gegen Stellvertreter zu prüfen ist billiger als
    /// eine Maschine und trifft die Fallen genauso — sie sind Reihenfolgen, keine Hardware.
    ///
    /// **Der Zustand wird GETEILT, nicht kopiert.** Die erste Fassung gab jeder Thread-Sicht ihr
    /// eigenes Marken-Array; ein `unpark` aus Sicht 1 landete dann nirgends, wo Sicht 2 es sehen
    /// konnte, und der Lost-Wakeup-Test fiel durch — an der Attrappe, nicht am Code. Schlimmer:
    /// die übrigen Tests **bestanden** dabei, obwohl sie an demselben Loch vorbeimassen. Ein
    /// Stellvertreter, der die zu prüfende Kopplung nicht hat, prüft sie auch nicht.
    struct Kernel {
        marken: RefCell<[u32; 8]>,
        /// Wie oft ein `park` tatsächlich blockiert hätte (Marke fehlte).
        blockiert: RefCell<u32>,
    }

    impl Kernel {
        fn neu() -> Kernel {
            Kernel {
                marken: RefCell::new([0; 8]),
                blockiert: RefCell::new(0),
            }
        }
        fn sicht(&self, ich: Tid) -> Sicht<'_> {
            Sicht { ich, k: self }
        }
        fn blockierte(&self) -> u32 {
            *self.blockiert.borrow()
        }
    }

    /// Die Sicht **eines** Threads auf denselben Kernel.
    struct Sicht<'a> {
        ich: Tid,
        k: &'a Kernel,
    }

    impl Park for Sicht<'_> {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            let mut m = self.k.marken.borrow_mut();
            let i = self.ich as usize;
            if m[i] > 0 {
                m[i] -= 1; // Marke verbraucht -> kein Blockieren
            } else {
                *self.k.blockiert.borrow_mut() += 1;
            }
        }
        fn unpark(&self, t: Tid) {
            // **Immer** hinterlegen, auch wenn `t` wach ist -- das ist der Vertrag.
            self.k.marken.borrow_mut()[t as usize] += 1;
        }
    }

    #[test]
    fn unbestrittenes_lock_faehrt_ohne_park() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut m = Mutex::new();
        assert!(m.lock_schritt(&p).unwrap());
        assert_eq!(k.blockierte(), 0); // null Syscalls im unbestrittenen Fall
        assert!(m.unlock(&p));
    }

    #[test]
    fn selbst_deadlock_wird_gemeldet_statt_zu_haengen() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut m = Mutex::new();
        assert!(m.lock_schritt(&p).unwrap());
        // Ohne diese Meldung waere das ein Haenger -- und ein Haenger sieht aus wie ein
        // Kernelfehler, nicht wie ein Treiberfehler.
        assert_eq!(m.lock_schritt(&p), Err(LockError::SelbstDeadlock));
    }

    #[test]
    fn fremde_freigabe_wird_abgewiesen() {
        let k = Kernel::neu();
        let p1 = k.sicht(1);
        let mut m = Mutex::new();
        assert!(m.lock_schritt(&p1).unwrap());
        let p2 = k.sicht(2);
        // Waere das erlaubt, haetten zwei Threads gleichzeitig Zutritt.
        assert!(!m.unlock(&p2));
        assert!(m.is_locked());
    }

    #[test]
    fn bestrittenes_lock_reiht_ein_und_parkt() {
        let k = Kernel::neu();
        let p1 = k.sicht(1);
        let mut m = Mutex::new();
        assert!(m.lock_schritt(&p1).unwrap());
        let p2 = k.sicht(2);
        assert!(!m.lock_schritt(&p2).unwrap()); // eingereiht + geparkt
        assert_eq!(m.wartende(), 1);
        assert_eq!(k.blockierte(), 1);
    }

    #[test]
    fn voller_warteraum_parkt_NICHT() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut m = Mutex::new();
        assert!(m.lock_schritt(&p).unwrap());
        // Die Schlange fuellen -- lauter verschiedene Tids.
        for t in 0..WARTEPLAETZE {
            assert!(m.warten.push(t as Tid + 100));
        }
        let p2 = k.sicht(2);
        // **Der D11-Fall.** Kein Platz -> Absage, und vor allem: NICHT parken. Ein Thread, der
        // schlafen geht, ohne in der Liste zu stehen, wird nie geweckt.
        assert_eq!(m.lock_schritt(&p2), Err(LockError::KeinWarteplatz));
        assert_eq!(k.blockierte(), 0);
        assert_eq!(m.warten.abgewiesen(), 1);
    }

    #[test]
    fn unlock_weckt_genau_einen() {
        let k = Kernel::neu();
        let p1 = k.sicht(1);
        let mut m = Mutex::new();
        assert!(m.lock_schritt(&p1).unwrap());
        let p2 = k.sicht(2);
        let _ = m.lock_schritt(&p2);
        let p3 = k.sicht(3);
        let _ = m.lock_schritt(&p3);
        assert_eq!(m.wartende(), 2);
        assert!(m.unlock(&p1));
        assert_eq!(m.wartende(), 1);
    }

    #[test]
    fn warteschlange_ist_fifo() {
        let mut q = WaitQueue::new();
        assert!(q.push(7));
        assert!(q.push(8));
        assert!(q.push(9));
        // LIFO liesse den ersten Wartenden unter Last beliebig lange liegen.
        assert_eq!(q.pop(), Some(7));
        assert_eq!(q.pop(), Some(8));
        assert_eq!(q.pop(), Some(9));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn completion_vor_dem_warten_geht_nicht_verloren() {
        // **Die Kernaussage.** Der IRQ-Thread ist schneller als der Wartende -- in einem Treiber
        // der Normalfall, nicht der Grenzfall.
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut c = Completion::new();
        assert_eq!(c.complete(&p), 0); // niemand wartete
        assert!(c.warten_schritt(&p).unwrap()); // trotzdem sofort fertig
        assert_eq!(k.blockierte(), 0);
    }

    #[test]
    fn completion_zaehlt_statt_zu_flaggen() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut c = Completion::new();
        c.complete(&p);
        c.complete(&p);
        assert_eq!(c.offen(), 2);
        assert!(c.warten_schritt(&p).unwrap());
        assert_eq!(c.offen(), 1); // ein Flag haette hier 0 gesagt und einen Abschluss verloren
    }

    #[test]
    fn completion_weckt_alle_wartenden() {
        let k = Kernel::neu();
        let p1 = k.sicht(1);
        let mut c = Completion::new();
        let p2 = k.sicht(2);
        let p3 = k.sicht(3);
        assert!(!c.warten_schritt(&p2).unwrap());
        assert!(!c.warten_schritt(&p3).unwrap());
        assert_eq!(c.complete(&p1), 2);
    }

    #[test]
    fn weckruf_zwischen_pruefung_und_park_geht_nicht_verloren() {
        // **Das Fenster, um das sich alles dreht** -- hier von Hand nachgestellt: der Weckruf
        // trifft ein, NACHDEM der Thread festgestellt hat, dass er warten muss, und BEVOR er
        // schlaeft. Ohne die Marke im Kernel schliefe er fuer immer.
        let k = Kernel::neu();
        let p1 = k.sicht(1);
        let mut c = Completion::new();
        let p2 = k.sicht(2);
        assert_eq!(c.offen(), 0); // Thread 2 stellt fest: ich muss warten
        assert!(c.warten.push(2)); // ... reiht sich ein
        let _ = c.complete(&p1); // <-- HIER trifft der Weckruf ein
        p2.park(); // ... und ERST JETZT schlaeft er
        assert_eq!(k.blockierte(), 0); // die Marke hat ihn durchgelassen
    }

    /// Eine Uhr zum Hinstellen — Auflösung und Stand, mehr braucht [`Clock`] nicht.
    struct Uhr {
        hz: u64,
        stand: u64,
    }

    impl Clock for Uhr {
        fn hz(&self) -> u64 {
            self.hz
        }
        fn now(&self) -> u64 {
            self.stand
        }
    }

    /// Zeichnet auf, WEN sie in welcher Reihenfolge geweckt hat — für FIFO-Aussagen, für die
    /// Zähler im `Kernel`-Stellvertreter zu grob sind.
    struct Folge {
        ich: Tid,
        folge: RefCell<[Tid; TIMERPLAETZE]>,
        n: RefCell<usize>,
    }

    impl Folge {
        fn neu(ich: Tid) -> Folge {
            Folge {
                ich,
                folge: RefCell::new([0; TIMERPLAETZE]),
                n: RefCell::new(0),
            }
        }
        fn anzahl(&self) -> usize {
            *self.n.borrow()
        }
        fn aufzeichnung(&self) -> ([Tid; TIMERPLAETZE], usize) {
            (*self.folge.borrow(), *self.n.borrow())
        }
    }

    impl Park for Folge {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {}
        fn unpark(&self, t: Tid) {
            let mut n = self.n.borrow_mut();
            if *n < TIMERPLAETZE {
                self.folge.borrow_mut()[*n] = t;
                *n += 1;
            }
        }
    }

    #[test]
    fn jiffies_ist_der_uhrstand() {
        let u = Uhr { hz: 250, stand: 12345 };
        assert_eq!(u.hz(), 250);
        assert_eq!(jiffies(&u), 12345);
    }

    #[test]
    fn jiffies_umrechnung_millis_in_ticks() {
        // 1 s bei 100 Hz sind 100 Ticks; 10 ms bei 1000 Hz sind 10 Ticks.
        assert_eq!(msecs_to_jiffies(1000, 100), 100);
        assert_eq!(msecs_to_jiffies(10, 1000), 10);
        assert_eq!(msleep_berechne_ticks(1000, 250), 250);
        // Die Division stutzt ab — sichtbar hier, nicht versteckt in der Funktion.
        assert_eq!(msecs_to_jiffies(1, 300), 0);
        // Keine Auflösung heisst keine Ticks, nicht Division durch null (geteilt wird durch 1000).
        assert_eq!(msecs_to_jiffies(1000, 0), 0);
    }

    #[test]
    fn jiffies_umrechnung_saettigt_statt_zu_ueberlaufen() {
        // Ein Überlauf, der zu 0 würde, liesse den Timeout sofort verfallen.
        assert_eq!(msecs_to_jiffies(u64::MAX, u64::MAX), u64::MAX);
        assert_eq!(msecs_to_jiffies(u64::MAX, 1000), u64::MAX);
        assert_eq!(msleep_berechne_ticks(u64::MAX, 100), u64::MAX);
    }

    #[test]
    fn timer_vor_faelligkeit_kein_weckruf() {
        let f = Folge::neu(0);
        let mut rad = TimerWheel::new();
        assert!(rad.after(1000, 100, 5));
        assert_eq!(rad.expire(1050, &f), 0);
        assert_eq!(f.anzahl(), 0);
        assert_eq!(rad.len(), 1); // nicht fällig heisst nicht weg
    }

    #[test]
    fn timer_faellig_weckt_fifo() {
        let f = Folge::neu(0);
        let mut rad = TimerWheel::new();
        assert!(rad.after(0, 10, 1));
        assert!(rad.after(0, 5, 2));
        assert!(rad.after(0, 10, 3));
        assert_eq!(rad.expire(7, &f), 1); // nur tid 2 fällig
        assert_eq!(rad.len(), 2);
        assert_eq!(rad.expire(10, &f), 2); // Rest fällig
        assert!(rad.is_empty());
        let (folge, n) = f.aufzeichnung();
        // Fälligkeit entscheidet WER, Einreihung entscheidet in welcher FOLGE.
        assert_eq!(n, 3);
        assert_eq!([folge[0], folge[1], folge[2]], [2, 1, 3]);
    }

    #[test]
    fn timer_voll_weist_ab_statt_zu_verlieren() {
        let f = Folge::neu(0);
        let mut rad = TimerWheel::new();
        for t in 0..TIMERPLAETZE as Tid {
            assert!(rad.after(0, 10, t));
        }
        assert!(!rad.after(0, 10, 999)); // kein Platz — darf nicht als eingetragen gelten
        assert_eq!(rad.len(), TIMERPLAETZE);
        assert_eq!(rad.expire(u64::MAX, &f), TIMERPLAETZE);
    }

    #[test]
    fn timer_absagen_entfernt_ohne_zu_wecken() {
        let f = Folge::neu(0);
        let mut rad = TimerWheel::new();
        assert!(rad.after(0, 10, 1));
        assert!(rad.after(0, 10, 2));
        assert!(rad.absagen(1)); // del_timer vor Ablauf
        assert_eq!(rad.len(), 1);
        assert_eq!(rad.expire(100, &f), 1); // nur tid 2 wird geweckt
        let (folge, n) = f.aufzeichnung();
        assert_eq!((n, folge[0]), (1, 2));
        assert!(!rad.absagen(1)); // nach Ablauf: nichts zu tun, kein Fehler
        assert!(!rad.absagen(9)); // nie eingetragen: idempotent
    }

    #[test]
    fn wait_event_kehrt_sofort_zurueck_wenn_bedingung_gilt() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut q = WaitQueue::new();
        wait_event(&p, &mut q, || true).unwrap();
        assert_eq!(k.blockierte(), 0);
        assert!(q.is_empty()); // nicht einmal eingereiht
    }

    #[test]
    fn wait_event_nutzt_die_marke_statt_zu_blockieren() {
        // Die `wait_event`-Form von „complete vor warten": der Weckruf trifft VOR dem Warten
        // ein, die Bedingung wird erst danach wahr.
        let k = Kernel::neu();
        let p1 = k.sicht(1);
        let mut q = WaitQueue::new();
        p1.unpark(2); // Marke für Thread 2 hinterlegen
        let p2 = k.sicht(2);
        let aufrufe = RefCell::new(0u32);
        wait_event(&p2, &mut q, || {
            let mut a = aufrufe.borrow_mut();
            *a += 1;
            *a > 1 // beim ersten Prüfen falsch, danach wahr
        })
        .unwrap();
        assert_eq!(k.blockierte(), 0); // die Marke hat das Parken durchgelassen
    }

    #[test]
    fn wait_event_voll_meldet_statt_zu_parken() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut q = WaitQueue::new();
        for t in 0..WARTEPLAETZE {
            assert!(q.push(t as Tid + 100));
        }
        assert_eq!(wait_event(&p, &mut q, || false), Err(LockError::KeinWarteplatz));
        assert_eq!(k.blockierte(), 0);
    }

    #[test]
    fn workqueue_arbeitet_fifo_ab_und_flush_belegt_leere() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut wq = Workqueue::new();
        assert!(wq.flush()); // leerer Nachweis auf leerer Liste
        wq.queue(&p, 11).unwrap();
        wq.queue(&p, 22).unwrap();
        wq.queue(&p, 33).unwrap();
        assert!(!wq.flush());
        let gesehen = RefCell::new([0 as JobId; 4]);
        let n = RefCell::new(0usize);
        while wq.run_ein_job(&p, |job| {
            let i = *n.borrow();
            gesehen.borrow_mut()[i] = job;
            *n.borrow_mut() += 1;
        }) {}
        assert_eq!(*n.borrow(), 3);
        let g = *gesehen.borrow();
        // FIFO: wer zuerst eingereicht wurde, läuft zuerst — kein Auftrag verhungert.
        assert_eq!([g[0], g[1], g[2]], [11, 22, 33]);
        assert!(wq.flush());
        assert!(!wq.run_ein_job(&p, |_| panic!("leere Liste läuft nichts")));
    }

    #[test]
    fn workqueue_voll_meldet_statt_zu_verlieren() {
        let k = Kernel::neu();
        let p = k.sicht(1);
        let mut wq = Workqueue::new();
        for j in 0..WARTEPLAETZE as JobId {
            wq.queue(&p, j).unwrap();
        }
        assert_eq!(wq.queue(&p, 999), Err(LockError::KeinWarteplatz));
        assert_eq!(wq.len(), WARTEPLAETZE);
    }

    #[test]
    fn sleep_bis_saettigt_bei_erreichtem_ziel() {
        assert_eq!(sleep_bis(100, 150), 50);
        assert_eq!(sleep_bis(150, 150), 0);
        // Hinter dem Ziel ist der Schlaf fertig — ein Unterlauf legte ihn in die ferne Zukunft.
        assert_eq!(sleep_bis(151, 150), 0);
        assert_eq!(sleep_bis(u64::MAX, 0), 0);
    }
}

// ── E3: PD-lokales Präemptions-Gatter und Spinlock-Shim ───────────────────────
//
// E3-Entscheidung (`docs/linux-kompatibilitaet-caprock.md`, Abschnitt „Folgefrage:
// Nebenläufigkeit der kthreads — ENTSCHIEDEN 2026-08-26"): Alle Threads einer PD laufen
// auf dem Kern ihres Aufrufers (`CONFIG_SMP=n`), werden aber per Timer-Tick umgeplant
// (`CONFIG_PREEMPT=y`). In genau dieser Lage spinnt die richtige `spin_lock`-Abbildung
// gar nicht — sie verhindert nur den Wechsel (`include/linux/spinlock_up.h`:
// `spin_lock() → preempt_disable()`, `spin_unlock() → preempt_enable()`).
//
// Was hier steht, ist die PD-Seite davon: ein Zähler-Gatter je Thread ([`Preempt`]), ein
// nicht-spinnender [`Spinlock`] darüber und die Stop-Seite eines Kernel-Threads
// ([`Kthread`]). Die Spawn-Seite bleibt bewusst beim Kernel (`SYS_SPAWN`) — diese Crate
// hütet Warten und Ausschluss, kein Erzeugen.

/// **Das PD-lokale Präemptions-Gatter** — „nicht auf einen anderen Thread **derselben
/// PD** umschalten" (E3-Zuschnitt).
///
/// Drei Punkte aus der Entscheidung, alle tragend:
///
/// * **Nur die eigene PD.** Das Gatter ist ein Zähler je Thread, kein Kernelausschluss:
///   wer es hält, wird nicht von einem anderen Thread derselben PD verdrängt — mehr
///   nicht. Eine **fremde PD bleibt jederzeit preemptibel**; es gibt keinen geteilten
///   Zustand mit ihr, also braucht es auch keinen Ausschluss gegen sie. Ein globaler
///   Scheduling-Override wäre ein System-DoS aus einer Treiber-PD heraus — genau die
///   Autorität, die eine PD nicht haben darf.
/// * **Zähler, kein Schalter.** Verschachteltes Halten braucht so viele `enable` wie
///   `disable`; der Stand gehört dem Thread, nicht der Sperre.
/// * **Voraussetzung: SC-Budget-Deckel.** Das Gatter darf nicht unbegrenzt gehalten werden
///   können — gedeckelt wird es durch das verbleibende SC-Budget (zweite Absicherung der
///   E3-Entscheidung). Die Kapazität hat damit einen Namen statt eine stille Form: wer das
///   Gatter überhält, scheitert am Budget, nicht am Zufall des Timers.
///
/// Sobald `SYS_SPAWN` eine Kernwahl bekommt, fällt die Voraussetzung (`SMP=n`) und diese
/// Entscheidung ist neu zu stellen — dann gilt `CONFIG_SMP=y`, und `spin_lock` verlässt
/// Klasse A.
pub trait Preempt {
    /// Wechsel innerhalb der eigenen PD verhindern (Zähler + 1).
    fn preempt_disable(&self);
    /// Einen gehaltenen Wechsel freigeben (Zähler − 1).
    fn preempt_enable(&self);
}

/// **Ein Spinlock, der gar nicht spinnt** — die `spinlock_up.h`-Form für
/// `CONFIG_SMP=n + PREEMPT=y`.
///
/// Der unbestrittene Weg ist Gatter-zu plus Flag und **null Syscalls**; erst der
/// bestrittene reiht ein und parkt. Gespinnt wird nie: ohne Parallelität innerhalb der PD
/// genügt es, den Wechsel zu verhindern — wer spinnen würde, während der Halter verdrängt
/// ist, belegte den einzigen Kern und liesse den Halter nie wieder dran (der naive
/// Spinlock deadlockt in dieser Konstellation zwangsläufig).
///
/// Abgrenzung gegen [`Mutex`] — **das Gatter ist der ganze Unterschied**: [`Mutex`] kennt
/// nur Schlafen (einreihen + parken, kein Präemptions-Gatter), [`Spinlock`] hält
/// zusätzlich das Gatter, solange er gehalten wird. Faustregel: Linux-`spin_lock` (kurze
/// kritische Abschnitte, auch im IRQ-Thread) → hierher; Linux-`mutex_lock` (lange
/// Abschnitte, nie im IRQ-Thread) → [`Mutex`]. Wer das Gatter nicht braucht, nimmt
/// [`Mutex`] — ein gehaltenes Gatter ohne Not verlängert nur die Latenz der
/// PD-Geschwister.
///
/// Das Gatter liegt **nur über dem kritischen Abschnitt, nicht über dem Schlaf**: der
/// bestrittene Schritt gibt es vor dem Parken frei, und der Weckruf trägt über die
/// Park-Marke wie bei [`wait_event`]. Ein über dem Schlaf gehaltenes Gatter verbrennte
/// SC-Budget ohne Nutzen (der Schlafende läuft ohnehin nicht) und wäre ein globaler
/// Ausschluss durch die Hintertür. Regel in kurz: Erfolg hält das Gatter (der Aufrufer
/// schuldet [`Self::unlock`]), Parken und Fehler geben es frei — kein Leck in beiden
/// Ausgängen.
pub struct Spinlock {
    /// Belegt oder frei — das Flag aus der Modul-Doku, hier mit Gatter davor.
    locked: bool,
    /// Wer hält — die Inhaber-Marke wie bei [`Mutex`]: ohne sie wären Selbstdeadlock und
    /// Fremdfreigabe nicht meldbar, sondern ein stiller Hänger bzw. doppelter Zutritt.
    inhaber: Tid,
    /// Die Wartenden — dieselbe Liste im Speicher der PD wie überall in dieser Crate.
    wartend: WaitQueue,
}

impl Default for Spinlock {
    fn default() -> Self {
        Self::new()
    }
}

impl Spinlock {
    pub const fn new() -> Spinlock {
        Spinlock {
            locked: false,
            inhaber: 0,
            wartend: WaitQueue::new(),
        }
    }

    /// Versuchen, ohne zu warten. `true` = bekommen (Gatter gehalten bis [`Self::unlock`]).
    ///
    /// `p` dient nur der Inhaber-Marke — geparkt wird im try-Weg nie, bestritten heisst
    /// hier Absage (`false`) statt Schlaf. Der Fehlweg gibt das Gatter sofort frei, damit
    /// auch ein Fehlversuch den Zähler symmetrisch lässt.
    pub fn try_lock(&mut self, pre: &dyn Preempt, p: &dyn Park) -> bool {
        pre.preempt_disable();
        if self.locked {
            // Auch der Selbstversuch ist hier nur `false` — wie `Mutex::try_lock`, das
            // ebenfalls nicht unterscheidet, sondern absagt.
            pre.preempt_enable();
            return false;
        }
        self.locked = true;
        self.inhaber = p.me();
        true
    }

    /// **Einen Sperr-Schritt.** Gibt `Ok(true)`, wenn die Sperre erlangt wurde; `Ok(false)`
    /// heisst „eingereiht und geparkt, bitte noch einmal" (Schleife s. [`Self::lock`]).
    ///
    /// Gatter-Regel je Ausgang: `Ok(true)` hält das Gatter (der Aufrufer schuldet
    /// [`Self::unlock`]); `Ok(false)` gibt es vor dem Parken frei (der Schlafende braucht
    /// keinen Wechsel-Schutz, und das SC-Budget dankt es); `Err` gibt es ebenfalls frei —
    /// ein Fehlerpfad mit gehaltenem Gatter wäre ein Leck mit Zähler.
    pub fn lock_schritt(&mut self, pre: &dyn Preempt, p: &dyn Park) -> Result<bool, LockError> {
        pre.preempt_disable();
        if self.locked {
            if self.inhaber == p.me() {
                // Selbst-Deadlock: gemeldet statt gehangen — wie bei `Mutex`.
                pre.preempt_enable();
                return Err(LockError::SelbstDeadlock);
            }
            if !self.wartend.push(p.me()) {
                // **Nicht parken** — dieselbe Begründung wie überall in dieser Datei: wer
                // schläft, ohne in der Liste zu stehen, wird nie geweckt.
                pre.preempt_enable();
                return Err(LockError::KeinWarteplatz);
            }
            // Gatter frei, dann schlafen: ein Weckruf, der dazwischen eintrifft,
            // hinterlegt die Park-Marke, und das `park` kehrt sofort zurück.
            pre.preempt_enable();
            p.park();
            return Ok(false);
        }
        self.locked = true;
        self.inhaber = p.me();
        Ok(true)
    }

    /// Sperren, bis es klappt — die bequeme Fassung über [`Self::lock_schritt`].
    pub fn lock(&mut self, pre: &dyn Preempt, p: &dyn Park) -> Result<(), LockError> {
        loop {
            if self.lock_schritt(pre, p)? {
                return Ok(());
            }
        }
    }

    /// Freigeben, **einen** Wartenden wecken und das Gatter freigeben. `false`, wenn der
    /// Aufrufer die Sperre gar nicht hielt — wie bei [`Mutex`]: eine fremde Freigabe gäbe
    /// zwei Threads gleichzeitig Zutritt. Die abgelehnte Freigabe fasst das Gatter nicht
    /// an — sie hält nichts, also gibt sie nichts frei.
    pub fn unlock(&mut self, pre: &dyn Preempt, p: &dyn Park) -> bool {
        if !self.locked || self.inhaber != p.me() {
            return false;
        }
        self.locked = false;
        self.inhaber = 0;
        self.wartend.wake_one(p);
        pre.preempt_enable();
        true
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }
    pub fn wartende(&self) -> usize {
        self.wartend.len()
    }
}

/// **Die Stop-Seite eines Kernel-Threads** — die `kthread_should_stop`/`kthread_stop`-Form.
///
/// Die **Spawn-Seite bleibt beim Kernel** (`SYS_SPAWN`): wer einen Thread erzeugt, braucht
/// eine Cap und einen Kern-Eintrag — beides hat diese Crate nicht und will es nicht haben.
/// Was sie hütet, ist nur die Absprache zwischen Stopper und Läufer: ein Flag plus ein
/// Weckruf. Der Läufer prüft [`Self::should_stop`] an einer für ihn passenden Stelle (etwa
/// je Schleifendurchgang), der Stopper ruft [`Self::stop`] — Flag setzen, Weckruf hinterher,
/// damit ein gerade parkender Läufer die Fahne sieht, statt bis zum nächsten Timer zu
/// schlafen.
pub struct Kthread {
    tid: Tid,
    stop: bool,
}

impl Kthread {
    /// Ein Läufer, wie ihn der Stopper sieht — `tid` kommt vom `SYS_SPAWN`-Aufrufer.
    pub fn run(tid: Tid) -> Kthread {
        Kthread { tid, stop: false }
    }

    pub fn tid(&self) -> Tid {
        self.tid
    }

    /// „Soll ich aufhören?" — die Läufer-Seite, billig genug für jede Runde.
    pub fn should_stop(&self) -> bool {
        self.stop
    }

    /// „Hör auf" — Flag setzen und den Läufer wecken, falls er gerade parkt. Im Flag
    /// idempotent; jeder Aufruf hinterlegt je eine Park-Marke (ein Stopper stoppt einmal —
    /// wie `kthread_stop` einmal gerufen wird).
    pub fn stop(&mut self, p: &dyn Park) {
        self.stop = true;
        p.unpark(self.tid);
    }
}

#[cfg(test)]
mod tests_e3_gatter {
    use super::*;
    use core::cell::RefCell;

    /// Stellvertreter für Kernel **und** Gatter in einem: dieselbe Sicht implementiert
    /// [`Park`] (Marken wie im Schwester-Modul) und [`Preempt`] (Zähler). Der Zustand wird
    /// geteilt wie dort — ein Zähler je Sicht prüfte keine Symmetrie, sondern addierte
    /// Äpfel.
    struct Kern {
        marken: RefCell<[u32; 8]>,
        blockiert: RefCell<u32>,
        gatter: RefCell<i32>,
    }

    impl Kern {
        fn neu() -> Kern {
            Kern {
                marken: RefCell::new([0; 8]),
                blockiert: RefCell::new(0),
                gatter: RefCell::new(0),
            }
        }
        fn sicht(&self, ich: Tid) -> Sicht<'_> {
            Sicht { ich, k: self }
        }
        fn blockierte(&self) -> u32 {
            *self.blockiert.borrow()
        }
        /// Gatterstand: 0 heisst symmetrisch (jedes disable hat sein enable gefunden);
        /// negativ hiesse Freigabe ohne Halten — auch das wäre ein Leck, mit anderem
        /// Vorzeichen.
        fn stand(&self) -> i32 {
            *self.gatter.borrow()
        }
    }

    /// Die Sicht **eines** Threads auf denselben Kernel.
    struct Sicht<'a> {
        ich: Tid,
        k: &'a Kern,
    }

    impl Park for Sicht<'_> {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            let mut m = self.k.marken.borrow_mut();
            let i = self.ich as usize;
            if m[i] > 0 {
                m[i] -= 1; // Marke verbraucht -> kein Blockieren
            } else {
                *self.k.blockiert.borrow_mut() += 1;
            }
        }
        fn unpark(&self, t: Tid) {
            // **Immer** hinterlegen, auch wenn `t` wach ist -- das ist der Vertrag.
            self.k.marken.borrow_mut()[t as usize] += 1;
        }
    }

    impl Preempt for Sicht<'_> {
        fn preempt_disable(&self) {
            *self.k.gatter.borrow_mut() += 1;
        }
        fn preempt_enable(&self) {
            *self.k.gatter.borrow_mut() -= 1;
        }
    }

    #[test]
    fn spin_unbestritten_faehrt_ohne_park() {
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut s = Spinlock::new();
        assert!(s.lock_schritt(&p, &p).unwrap());
        assert_eq!(k.blockierte(), 0); // null Syscalls im unbestrittenen Fall
        assert_eq!(k.stand(), 1); // gehalten, solange gehalten
        assert!(s.unlock(&p, &p));
        assert_eq!(k.stand(), 0);
    }

    #[test]
    fn spin_bestritten_reiht_ein_und_parkt() {
        let k = Kern::neu();
        let p1 = k.sicht(1);
        let mut s = Spinlock::new();
        s.lock(&p1, &p1).unwrap();
        let p2 = k.sicht(2);
        assert!(!s.lock_schritt(&p2, &p2).unwrap()); // eingereiht + geparkt
        assert_eq!(s.wartende(), 1);
        assert_eq!(k.blockierte(), 1);
        // Nur der Halter hält das Gatter — der Wartende gab seins vor dem Schlaf frei.
        assert_eq!(k.stand(), 1);
    }

    #[test]
    fn spin_gatter_symmetrisch_kein_leck_bei_unlock() {
        // Voller Zyklus mit umstrittener Übergabe: halten, warten, wecken, übernehmen,
        // freigeben — am Ende muss der Zähler wieder bei 0 stehen.
        let k = Kern::neu();
        let p1 = k.sicht(1);
        let mut s = Spinlock::new();
        s.lock(&p1, &p1).unwrap();
        assert_eq!(k.stand(), 1);
        let p2 = k.sicht(2);
        assert!(!s.lock_schritt(&p2, &p2).unwrap());
        assert!(s.unlock(&p1, &p1)); // weckt p2
        assert_eq!(k.stand(), 0);
        s.lock(&p2, &p2).unwrap(); // übernimmt die freigewordene Sperre ohne Parken
        assert_eq!(k.blockierte(), 1); // nur das eine Parken von oben
        assert_eq!(s.wartende(), 0);
        assert!(s.unlock(&p2, &p2));
        assert_eq!(k.stand(), 0); // kein Leck
    }

    #[test]
    fn spin_selbstdeadlock_wird_gemeldet_statt_zu_haengen() {
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut s = Spinlock::new();
        s.lock(&p, &p).unwrap();
        // Ohne diese Meldung wäre das ein Hänger mit gehaltenem Gatter — und ein Hänger
        // sieht aus wie ein Kernelfehler, nicht wie ein Treiberfehler.
        assert_eq!(s.lock_schritt(&p, &p), Err(LockError::SelbstDeadlock));
        assert_eq!(k.blockierte(), 0); // gemeldet statt geparkt
        assert_eq!(k.stand(), 1); // Fehlerpfad symmetrisch, äusseres Halten unberührt
        assert!(s.unlock(&p, &p));
        assert_eq!(k.stand(), 0);
    }

    #[test]
    fn spin_fremde_freigabe_wird_abgewiesen() {
        let k = Kern::neu();
        let p1 = k.sicht(1);
        let mut s = Spinlock::new();
        s.lock(&p1, &p1).unwrap();
        let p2 = k.sicht(2);
        // Wäre das erlaubt, hätten zwei Threads gleichzeitig Zutritt.
        assert!(!s.unlock(&p2, &p2));
        assert!(s.is_locked());
        // Die abgelehnte Freigabe fasst das Gatter nicht an.
        assert_eq!(k.stand(), 1);
        assert!(s.unlock(&p1, &p1));
        assert_eq!(k.stand(), 0);
    }

    #[test]
    fn spin_voll_meldet_statt_zu_parken() {
        let k = Kern::neu();
        let p1 = k.sicht(1);
        let mut s = Spinlock::new();
        s.lock(&p1, &p1).unwrap();
        // Die Schlange füllen — Tids im Bereich des Stellvertreters (0..8), damit das
        // abschliessende `unlock` beim Wecken nicht ins Leere greift; den Kernel fasst das
        // Füllen ohnehin nicht an.
        for t in 0..WARTEPLAETZE {
            assert!(s.wartend.push((t % 5) as Tid + 3));
        }
        let p2 = k.sicht(2);
        // **Der D11-Fall.** Kein Platz -> Absage, und vor allem: NICHT parken. Ein Thread,
        // der schlafen geht, ohne in der Liste zu stehen, wird nie geweckt.
        assert_eq!(s.lock_schritt(&p2, &p2), Err(LockError::KeinWarteplatz));
        assert_eq!(k.blockierte(), 0);
        assert_eq!(k.stand(), 1); // Fehlerpfad symmetrisch
        assert!(s.unlock(&p1, &p1));
        assert_eq!(k.stand(), 0);
    }

    #[test]
    fn spin_try_lock_haelt_gatter_bis_unlock() {
        let k = Kern::neu();
        let p1 = k.sicht(1);
        let mut s = Spinlock::new();
        assert!(s.try_lock(&p1, &p1));
        assert_eq!(k.blockierte(), 0);
        assert_eq!(k.stand(), 1); // gehalten bis unlock
        let p2 = k.sicht(2);
        assert!(!s.try_lock(&p2, &p2)); // bestritten: Absage statt Parken
        assert_eq!(k.blockierte(), 0);
        assert_eq!(k.stand(), 1); // Fehlweg symmetrisch, Fremdversuch ändert nichts
        assert!(s.unlock(&p1, &p1));
        assert_eq!(k.stand(), 0);
    }

    #[test]
    fn kthread_stop_weckt_und_meldet() {
        let k = Kern::neu();
        let p1 = k.sicht(1);
        let mut kt = Kthread::run(2);
        assert_eq!(kt.tid(), 2);
        assert!(!kt.should_stop());
        kt.stop(&p1); // Flag setzen + Marke an Tid 2
        assert!(kt.should_stop());
        let p2 = k.sicht(2);
        p2.park(); // verbraucht die Marke statt zu blockieren
        assert_eq!(k.blockierte(), 0);
    }

    #[test]
    fn spin_mutex_abgrenzung_gatter_nur_beim_spinlock() {
        // Die dokumentierte Abgrenzung, festgenagelt: `Mutex` kennt kein Gatter — ein voller
        // Zyklus lässt den Zähler unberührt. `Spinlock` hält es, solange er gehalten wird.
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut m = Mutex::new();
        m.lock(&p).unwrap();
        assert_eq!(k.stand(), 0);
        assert!(m.unlock(&p));
        assert_eq!(k.stand(), 0);
        let mut s = Spinlock::new();
        s.lock(&p, &p).unwrap();
        assert_eq!(k.stand(), 1);
        assert!(s.unlock(&p, &p));
        assert_eq!(k.stand(), 0);
    }
}

// ── Blockierendes Leerspülen (`flush_blockierend`, LogFlush-Strang) ─────────
//
// `flush` oben ist ein Nachweis („steht nichts mehr aus?"), keine Blockade: blockieren hiesse
// parken, und parken ohne Weckruf hiesse hängen. Diese Funktion ist die blockierende Fassung —
// und sie parkt trotzdem nur, wo ein Weckruf sicher kommt: sie reiht einen Drain-Marker ein
// ([`FLUSH_MARKE`]), zieht die Liste selbst ab (jeder Job läuft vor der Rückkehr, in FIFO-Folge
// wie bei [`Workqueue::run_ein_job`]) und wartet das Marker-Ende per [`Completion`] ab. Der
// Marker wird dabei verbraucht — `fertig` steht danach genau einmal, und das fristlose `warten`
// kehrt ohne ein einziges Parken zurück. Käme der Marker nie an (nur bei fremdem Ziehen mitten
// im Flush möglich, was die `&mut`-Signatur bereits ausschliesst), parkte der Aufrufer — und
// wachte per Marke wieder auf: Jobs terminieren per Vertrag, also braucht es hier keinen
// Timeout-Spielraum (PARK ohne Frist ist ok).
//
// Der Preis steht in der Signatur: für den Marker braucht es EINEN freien Platz — ist die
// Schlange voll, kommt die benannte Absage ([`LockError::KeinWarteplatz`]), statt dass ein
// Auftrag still nie liefe. Wer mit voller Schlange spült, zieht erst (`run_ein_job`) oder
// bemisst grösser.
//
// [`FLUSH_MARKE`] ist reserviert: was per [`Workqueue::queue`] eingereicht wird, ist nie
// `u64::MAX` — sonst hielte der Flush einen echten Job für den Marker (er liefe nicht, und der
// Abschluss käme zu früh). Die Marke verlässt diese Funktion nie: ein fremder Bearbeiter sieht
// sie nicht, weil es keinen zweiten Bearbeiter geben kann (`&mut self`).

/// Der Drain-Marker für [`Workqueue::flush_blockierend`] — reserviert, s. Abschnitts-Doku.
pub const FLUSH_MARKE: JobId = u64::MAX;

impl Workqueue {
    /// Blockierend leerspülen: alle ausstehenden Jobs laufen vor der Rückkehr.
    ///
    /// * Leer → sofort `Ok(())`: keine Marke, kein Parken, kein Zählen.
    /// * Voll (kein Platz für die Marke) → `Err(KeinWarteplatz)`: benannt statt still — und
    ///   es wurde NICHTS gezogen (alles-oder-nichts, kein halb gespülter Stand).
    /// * Sonst: Marker einreihen, alles ziehen (`tp` je Job, Marker → `complete`), dann das
    ///   Ende per [`Completion`] abwarten — fristlos, weil Jobs per Vertrag terminieren.
    ///
    /// `p` ist der Park-Vertrag mit Frist-Option ([`timeout::TimeoutPark`]) — genutzt wird das
    /// fristlose Parken; `_clk` steht für die Vertrags-Symmetrie mit `warten_timeout` dabei
    /// (die Fristlage kennt der Aufrufer, diese Funktion braucht keine — wie `queue`/`run_ein_job`
    /// ihr ungenutztes `p` mittragen); `tp` (task procedure) führt je gezogenen Job aus —
    /// dieselbe Form wie das `f` von [`Self::run_ein_job`].
    pub fn flush_blockierend(
        &mut self,
        p: &dyn crate::timeout::TimeoutPark,
        _clk: &dyn crate::Clock,
        tp: impl Fn(JobId),
    ) -> Result<(), crate::LockError> {
        let park: &dyn crate::Park = p;
        if self.is_empty() {
            return Ok(());
        }
        // Benannte Absage bei voller Schlange — der Flush trägt die Marke nicht ein, solange
        // sie nicht passt; ein Auftrag, der nie liefe, während jeder Prüfer Ordnung meldet.
        self.queue(park, FLUSH_MARKE)?;
        let mut fertig = crate::Completion::new();
        // Selbst abziehen, in FIFO-Folge wie `run_ein_job` — deshalb läuft jeder Job vor der
        // Rückkehr, und die Marke kommt genau dann an, wenn alles davor lief.
        while let Some(job) = self.q.pop() {
            if job == FLUSH_MARKE {
                fertig.complete(park);
            } else {
                tp(job);
            }
        }
        // Warten per Completion, fristlos: die Marke ist verbraucht, der Zähler steht — kein
        // Parken im Regelfall, und im Restfall ein Parken mit sicherem Weckruf.
        fertig.warten(park)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests_flush_blockierend {
    use super::*;
    use crate::timeout::TimeoutPark;
    use core::cell::{Cell, RefCell};

    /// Stellvertreter mit Park-Marke (wie die Schwester-Module), Uhr und Frist-Parken in einem.
    /// Der Flush parkt fristlos — `park_timeout` steht nur, weil die Signatur ihn verlangt, und
    /// verhält sich wie ein abgelaufener Rest (zählt, weckt nicht).
    struct Kern {
        marken: RefCell<[u32; 8]>,
        blockiert: Cell<u32>,
        frist_parks: Cell<u32>,
        jetzt: Cell<u64>,
    }

    impl Kern {
        fn neu() -> Kern {
            Kern {
                marken: RefCell::new([0; 8]),
                blockiert: Cell::new(0),
                frist_parks: Cell::new(0),
                jetzt: Cell::new(0),
            }
        }
        fn sicht(&self, ich: Tid) -> Sicht<'_> {
            Sicht { ich, k: self }
        }
        fn blockierte(&self) -> u32 {
            self.blockiert.get()
        }
        fn frist_geparkt(&self) -> u32 {
            self.frist_parks.get()
        }
    }

    struct Sicht<'a> {
        ich: Tid,
        k: &'a Kern,
    }

    impl Park for Sicht<'_> {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            let mut m = self.k.marken.borrow_mut();
            let i = self.ich as usize;
            if m[i] > 0 {
                m[i] -= 1; // Marke verbraucht -> kein Blockieren
            } else {
                self.k.blockiert.set(self.k.blockiert.get() + 1);
            }
        }
        fn unpark(&self, t: Tid) {
            // **Immer** hinterlegen, auch wenn `t` wach ist -- das ist der Vertrag.
            self.k.marken.borrow_mut()[t as usize] += 1;
        }
    }

    impl Clock for Sicht<'_> {
        fn hz(&self) -> u64 {
            1000
        }
        fn now(&self) -> u64 {
            self.k.jetzt.get()
        }
    }

    impl TimeoutPark for Sicht<'_> {
        fn park_timeout(&self, _ticks: u64) -> bool {
            self.k.frist_parks.set(self.k.frist_parks.get() + 1);
            false
        }
    }

    #[test]
    fn flush_leer_kehrt_sofort_zurueck() {
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut wq = Workqueue::new();
        wq.flush_blockierend(&p, &p, |_: JobId| {
            panic!("leerer Flush zieht nichts")
        })
        .unwrap();
        assert_eq!(k.blockierte(), 0); // kein Parken auf leerer Liste
        assert_eq!(k.frist_geparkt(), 0); // fristlos, wie dokumentiert
    }

    #[test]
    fn flush_laesst_jobs_vor_rueckkehr_laufen() {
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut wq = Workqueue::new();
        wq.queue(&p, 11).unwrap();
        wq.queue(&p, 22).unwrap();
        wq.queue(&p, 33).unwrap();
        let gesehen = RefCell::new([0 as JobId; 4]);
        let n = Cell::new(0usize);
        wq.flush_blockierend(&p, &p, |job| {
            let i = n.get();
            gesehen.borrow_mut()[i] = job;
            n.set(i + 1);
        })
        .unwrap();
        assert_eq!(n.get(), 3);
        let g = *gesehen.borrow();
        // FIFO: wer zuerst eingereicht wurde, läuft zuerst — kein Auftrag verhungert.
        assert_eq!([g[0], g[1], g[2]], [11, 22, 33]);
        assert!(wq.is_empty());
        // Die Marke ist verbraucht, der Abschluss stand schon: kein Parken nötig gewesen.
        assert_eq!(k.blockierte(), 0);
        assert_eq!(k.frist_geparkt(), 0);
    }

    #[test]
    fn flush_voll_meldet_statt_zu_parken() {
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut wq = Workqueue::new();
        for j in 0..WARTEPLAETZE as JobId {
            wq.queue(&p, j).unwrap();
        }
        assert_eq!(
            wq.flush_blockierend(&p, &p, |_: JobId| {
                panic!("abgewiesener Flush zieht nichts")
            }),
            Err(LockError::KeinWarteplatz)
        );
        assert_eq!(wq.len(), WARTEPLAETZE); // nichts gezogen: alles-oder-nichts
        assert_eq!(k.blockierte(), 0);
        assert_eq!(wq.abgewiesen(), 1); // die Marke wurde gezählt abgewiesen
    }

    #[test]
    fn flush_mit_genau_einem_freien_platz() {
        // Der Grenzfall zum vorigen Test: die Marke passt gerade noch — dann läuft alles.
        let k = Kern::neu();
        let p = k.sicht(1);
        let mut wq = Workqueue::new();
        for j in 0..(WARTEPLAETZE as JobId - 1) {
            wq.queue(&p, j).unwrap();
        }
        let n = Cell::new(0u32);
        wq.flush_blockierend(&p, &p, |_| n.set(n.get() + 1)).unwrap();
        assert_eq!(n.get(), WARTEPLAETZE as u32 - 1);
        assert!(wq.is_empty());
        assert_eq!(k.blockierte(), 0);
    }
}
