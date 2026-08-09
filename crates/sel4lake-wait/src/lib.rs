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
}
