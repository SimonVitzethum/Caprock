// Unter Loom braucht die Crate `std` (die `loom`-Crate ist keine `no_std`-Crate). Im Kernel-Build
// ist `loom` nie gesetzt, dort bleibt es exakt beim bisherigen `#![no_std]`.
#![cfg_attr(not(loom), no_std)]
//! Minimale `no_std`-Synchronisationsprimitive.
//!
//! Wichtig: Atomare Lade-/Speicher-Exklusiv-Operationen (LDXR/STXR), auf denen
//! `core::sync::atomic` aufbaut, sind auf aarch64 nur mit **aktivierter MMU**
//! und cacheable Normal-Memory wohldefiniert. Ein [`SpinLock`] darf daher erst
//! benutzt werden, nachdem die MMU eingeschaltet ist (siehe `caprock-hal::mmu`
//! und ADR 0002).
//!
//! # Nebenläufigkeits-Verifikation (Loom, B-7.2)
//!
//! Die Loom-Beweise am Dateiende laufen gegen **diesen** Quelltext, nicht gegen eine Nachbildung.
//! Vorher lag unter `Verification/concurrency/loom/` eine von Hand gepflegte „getreue Kopie" der
//! Lock-Logik — und ein Beweis über eine Kopie beweist etwas über die Kopie. Nichts hielt die
//! beiden zusammen: eine Änderung an der Reihenfolge hier hätte den Beweis dort nicht berührt.
//!
//! Möglich wird das durch **einen einzigen Unterschied**: unter `--cfg loom` kommen `AtomicU32`
//! und `Ordering` aus `loom::sync::atomic` statt aus `core::sync::atomic`, und die Warteschleife
//! gibt an den Loom-Scheduler ab statt `spin_loop` auszuführen. Alles Weitere — Ticket-Arithmetik,
//! Speicherordnungen, Writer-Vorrang, `fetch_and(!WRITER)` im Release, die RAII-Guards — ist
//! wörtlich derselbe Code, den der Kernel ausführt.
//!
//! **Was die Loom-Beweise NICHT abdecken** (s. auch [`irq_save_disable`]):
//!
//! * **Die IRQ-Maskierung.** Loom modelliert Threads, keine Unterbrechungen; ein Interrupt, der
//!   denselben Kern mitten im kritischen Abschnitt trifft, ist kein Interleaving zweier Threads
//!   und in Looms Modell **prinzipiell nicht darstellbar**. Unter Loom (Host-Ziel) greifen die
//!   No-Op-Stubs, `IRQ_MASKING_IMPLEMENTED` ist dort `false`. Der reentrante Ticket-Deadlock aus
//!   dem Modulanfang liegt damit **ausserhalb** dieses Beweises — er wird von der
//!   Übersetzungszeit-Zusicherung weiter unten gehalten, nicht von Loom.
//! * **Die Datenzelle.** `data` bleibt auch unter Loom eine `core::cell::UnsafeCell`, nicht
//!   `loom::cell::UnsafeCell` — Letztere gibt Zeiger nur über einen Scope-Guard heraus und ist
//!   mit `Deref`/`DerefMut` (die ein `&T` mit der Lebensdauer des Guards zurückgeben müssen)
//!   nicht vereinbar, ohne die öffentliche API zu verbiegen. Loom prüft hier also das
//!   **Synchronisationsprotokoll**, nicht die Zugriffe auf die Nutzlast; dass der Ausschluss
//!   wirklich trägt, prüfen die Beweise über beobachtbare Werte (Lost-Update, torn read).
//! * **Speicherordnung realer Hardware.** Loom prüft das C11-Modell, nicht die Eigenheiten von
//!   aarch64 oder x86.

use core::ops::{Deref, DerefMut};

// Auch die **Nutzlastzelle** kommt unter Loom aus dem Modellprüfer. Das ist nicht Kosmetik:
// `core::cell::UnsafeCell` ist für Loom unsichtbar, und ein Zugriff, den es nicht sieht, kann es
// auch nicht gegen die Atomics ordnen. Gemessen: mit `core`-Zelle blieb eine zu schwache
// Speicherordnung im Ticket-Release (`Release` -> `Relaxed`) von allen Beweisen **unbemerkt**.
// Mit `loom::cell::UnsafeCell` faellt sie auf — der Zugriff liegt dann im verfolgten Fenster
// zwischen `get_mut()` und dem Verwerfen des Zeigergriffs.
#[cfg(not(loom))]
use core::cell::UnsafeCell;
#[cfg(loom)]
use loom::cell::UnsafeCell;

// **Der eine cfg-Schalter, an dem B-7.2 hängt.** Unter Loom kommen die Atomics aus dem
// Modellprüfer, sonst aus `core` — der übrige Quelltext ist identisch.
#[cfg(not(loom))]
use core::sync::atomic::{AtomicU32, Ordering};
#[cfg(loom)]
use loom::sync::atomic::{AtomicU32, Ordering};

/// Warteschleifen-Hinweis.
///
/// Im Kernel ein `spin_loop` (PAUSE/YIELD — Energie und Hyperthread-Nachbar). Unter Loom **muss**
/// hier an den Scheduler abgegeben werden: Looms Threads laufen kooperativ, eine Schleife ohne
/// Abgabepunkt liefe endlos, statt die Interleavings zu explorieren.
#[inline(always)]
fn spin_hint() {
    #[cfg(not(loom))]
    core::hint::spin_loop();
    #[cfg(loom)]
    loom::thread::yield_now();
}

// --- IRQ-Sicherheit der SpinLocks (Bugfix: reentranter Ticket-Lock-Deadlock) ---
//
// Ein [`SpinLock`] ist ein FIFO-**Ticket**-Lock. Wird derselbe Lock vom IRQ-/Reschedule-Pfad UND
// von Thread-/Idle-Kontext genommen (im Kernel: `SCHEDS[core]` und `NTFNS[]` werden im
// Timer-Tick-Reschedule sowie aus `idle->reap_core`/Syscalls genommen), entsteht ohne IRQ-Maske
// ein Deadlock: feuert der Timer-Tick, während Thread-/Idle-Kontext den Lock hält/erwartet, zieht
// der Reschedule-Hook ein ZWEITES Ticket auf denselben Lock — der erste Halter ist aber im
// IRQ-Handler suspendiert und gibt sein Ticket nie frei. Daher maskiert `lock()` IRQs am eigenen
// Kern VOR dem Ticket-Ziehen und der Guard stellt den vorherigen Zustand beim `Drop` wieder her
// (nesting-sicher: jeder Guard sichert den Stand von VOR seinem Lock).

/// Belegt, dass DIESE Auswahl eine echte Maskierung mitbringt. Steht bewusst **neben** der
/// Implementierung und nicht in einer eigenen `cfg`-Kette: eine zweite Kette waere eine
/// Wiederholung der Bedingung, und Wiederholungen laufen auseinander. So kann der Waechter
/// weiter unten nur wahr sein, wenn die Funktionen wirklich gebaut werden.
#[cfg(target_arch = "aarch64")]
const IRQ_MASKING_IMPLEMENTED: bool = true;

/// DAIF (Interrupt-Maske) sichern + IRQs am aktuellen Kern maskieren. Gibt den vorherigen
/// DAIF-Zustand zurück (für [`irq_restore`]).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn irq_save_disable() -> u64 {
    let daif: u64;
    // SAFETY: reines Lesen + Setzen des DAIF-Systemregisters (I-Bit); keine Speicherwirkung.
    unsafe {
        core::arch::asm!("mrs {0}, DAIF", out(reg) daif, options(nomem, nostack, preserves_flags));
        core::arch::asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }
    daif
}

/// Den zuvor gesicherten DAIF-Zustand zurückschreiben (I-Bit). Der äußerste Guard gibt so „IRQs an"
/// wieder frei; war IRQ schon maskiert (verschachtelt/im Trap), bleibt es maskiert.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn irq_restore(daif: u64) {
    // SAFETY: schreibt nur das zuvor gelesene DAIF zurück; keine Speicherwirkung.
    unsafe {
        core::arch::asm!("msr DAIF, {0}", in(reg) daif, options(nomem, nostack, preserves_flags));
    }
}

/// RFLAGS sichern + IRQs am aktuellen Kern maskieren (x86_64-Gegenstück zu DAIF).
///
/// **Diese Fassung hat gefehlt, und das war ein echter Fehler**, kein Schönheitsmangel: bis
/// 2026-07-29 galt für x86_64 der No-Op-Zweig unten, der für *Host*-Builds gedacht war. Damit war
/// auf x86 **kein einziger `SpinLock` IRQ-sicher** — also genau der reentrante Ticket-Deadlock,
/// vor dem der Kommentar am Modulanfang warnt: der Timer-Tick trifft einen Kontext, der ein
/// Ticket hält, der Handler zieht ein neues, und das alte wird nie bedient.
///
/// Beobachtbar war das als **sporadisches Stehenbleiben mitten in einer `println!`-Ausgabe**
/// (die Konsolensperre wird am häufigsten genommen), mit wechselnden Abbruchstellen und einer
/// Rate von etwa einem Achtel der Läufe. Betroffen waren alle Locks, nicht nur die Konsole.
///
/// Die Unterscheidung ist `target_os`, nicht `target_arch`: der Kernel baut gegen
/// `x86_64-unknown-none` (`target_os = "none"`), die Host-Tests gegen
/// `x86_64-unknown-linux-gnu`. Nur Ersteres darf `cli` ausführen — im Userspace ist die
/// Instruktion privilegiert und würde eine SIGSEGV auslösen.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const IRQ_MASKING_IMPLEMENTED: bool = true;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline(always)]
fn irq_save_disable() -> u64 {
    let flags: u64;
    // SAFETY: `pushfq`/`pop` liest nur das Flags-Register (Stack wird ausgeglichen), `cli`
    // maskiert Interrupts am eigenen Kern. Beides sind die vorgesehenen Low-Level-Operationen.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }
    flags
}

/// Den zuvor gesicherten RFLAGS.IF-Zustand wiederherstellen.
///
/// Bewusst **nicht** `popfq`: das würde alle Flags zurückschreiben, auch die
/// Vergleichsergebnisse des unterbrochenen Codes. Wiederhergestellt wird nur das, was
/// [`irq_save_disable`] verändert hat — und war IRQ vorher schon maskiert (verschachtelter Guard
/// oder Trap-Kontext), bleibt es maskiert.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[inline(always)]
fn irq_restore(flags: u64) {
    if flags & (1 << 9) != 0 {
        // SAFETY: gibt die Interrupt-Zustellung am eigenen Kern wieder frei — genau der
        // Zustand, der vor `irq_save_disable` galt.
        unsafe { core::arch::asm!("sti", options(nomem, nostack, preserves_flags)) };
    }
}

// Host-Builds (z. B. `cargo test` anderer Crates): keine Interrupt-Maske — No-Op.
// Die Bedingung muss **beide** Bare-Metal-Ziele ausschließen; stünde hier nur
// `not(target_arch = "aarch64")`, fiele der x86-Kernel wieder in diesen Zweig.
// `dead_code` ist hier die **Aussage**, nicht ein Versehen: auf Host-Zielen (Tests, Loom) greift
// der Wächter unten nicht, weil es dort nichts zu maskieren gibt. Genau deshalb deckt der
// Loom-Beweis die IRQ-Sicherheit auch nicht ab — er läuft auf einem Ziel, auf dem diese Konstante
// `false` sein DARF. Die Zusicherung trägt allein `target_os = "none"`.
#[allow(dead_code)]
#[cfg(not(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "none"))))]
const IRQ_MASKING_IMPLEMENTED: bool = false;

#[cfg(not(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "none"))))]
#[inline(always)]
fn irq_save_disable() -> u64 {
    0
}
#[cfg(not(any(target_arch = "aarch64", all(target_arch = "x86_64", target_os = "none"))))]
#[inline(always)]
fn irq_restore(_daif: u64) {}

// --- Wächter: baut das Bare-Metal-Ziel wirklich den maskierenden Zweig? -----------------------
//
// Der x86-Fehler von 2026-07-29 bestand nicht aus falschem Code, sondern aus einer falschen
// `cfg`-Auswahl: der No-Op-Zweig für Host-Builds fing das Kernel-Ziel mit. Kein Test hat das
// gemerkt, weil jeder Test entweder auf dem Host lief (dort ist der No-Op richtig) oder auf ARM
// (dort war die Auswahl richtig). Ein Fehler in der Auswahl ist für eine Testsuite unsichtbar,
// solange sie nur Verhalten prüft.
//
// Deshalb hier eine Prüfung der Auswahl selbst, zur Übersetzungszeit: auf JEDEM Ziel ohne
// Betriebssystem (`target_os = "none"` — also jedes Kernel-Ziel, heutige wie künftige) MUSS eine
// echte Maskierung vorhanden sein. Wer eine neue Architektur hinzufügt und die Maskierung
// vergisst, bekommt einen Übersetzungsfehler statt eines sporadischen Deadlocks.

#[cfg(target_os = "none")]
const _: () = assert!(
    IRQ_MASKING_IMPLEMENTED,
    "Bare-Metal-Ziel ohne Interrupt-Maskierung in SpinLock::lock. Der Ticket-Lock ist dann \
     reentrant aus dem IRQ-Pfad erreichbar und verklemmt sporadisch (s. Modulanfang). \
     `irq_save_disable`/`irq_restore` fuer diese Architektur implementieren."
);

/// Fairer Ticket-Spinlock.
///
/// Wer zuerst zieht, kommt zuerst dran (FIFO) — das ist deterministischer als
/// ein CAS-Lock, der einzelne Kerne aushungern lassen kann.
pub struct SpinLock<T: ?Sized> {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: Der Lock serialisiert jeden Zugriff auf `data`; gleichzeitig kann
// höchstens ein Guard existieren. Damit ist geteilter Zugriff zwischen Kernen
// sicher, sofern `T` über Thread-/Kerngrenzen bewegt werden darf (`T: Send`).
unsafe impl<T: ?Sized + Send> Sync for SpinLock<T> {}
unsafe impl<T: ?Sized + Send> Send for SpinLock<T> {}

/// Denselben Konstruktorrumpf einmal `const` (Kernel) und einmal ohne (Loom) erzeugen.
///
/// `loom::sync::atomic::AtomicU32::new` ist **nicht** `const` — es meldet sich beim Modellprüfer
/// an, und das geht zur Übersetzungszeit nicht. Der Kernel braucht `const fn new` dagegen zwingend
/// (die Locks stehen in `static`s). Der Rumpf steht deshalb genau **einmal** hier und wird für
/// beide Fälle expandiert; ihn zweimal hinzuschreiben wäre wieder die Doppelung, gegen die B-7.2
/// überhaupt angetreten ist.
/// Der Parametername wird mit hereingegeben: `macro_rules!` ist hygienisch, ein im Makro
/// erzeugtes `value` waere fuer den uebergebenen Rumpf ein anderes Symbol.
macro_rules! ctor {
    ($val:ident, $($body:tt)*) => {
        #[cfg(not(loom))]
        pub const fn new($val: T) -> Self { $($body)* }
        #[cfg(loom)]
        pub fn new($val: T) -> Self { $($body)* }
    };
}

impl<T> SpinLock<T> {
    ctor! { value,
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
            data: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> SpinLock<T> {
    /// Sperrt und gibt einen Guard zurück, der beim Verlassen automatisch
    /// freigibt.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        // IRQ-sicher: IRQs am eigenen Kern maskieren, BEVOR ein Ticket gezogen wird (sonst kann der
        // Timer-Tick/Reschedule denselben Lock reentrant ziehen -> Ticket-Deadlock; s. o.).
        let daif = irq_save_disable();
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.now_serving.load(Ordering::Acquire) != ticket {
            spin_hint();
        }
        SpinGuard {
            lock: self,
            daif,
            #[cfg(loom)]
            ptr: Some(self.data.get_mut()),
        }
    }
}

/// RAII-Guard: hält den Lock, gibt bei `Drop` frei (und stellt den IRQ-Zustand wieder her).
pub struct SpinGuard<'a, T: ?Sized> {
    lock: &'a SpinLock<T>,
    /// DAIF-Zustand vor dem Locken (beim `Drop` wiederhergestellt).
    daif: u64,
    /// Nur unter Loom: der verfolgte Zeigergriff auf die Nutzlast. Seine Lebensdauer IST das
    /// Zugriffsfenster, das der Modellprüfer gegen die Atomics ordnet.
    /// **`Option`, damit das Zugriffsfenster VOR der Freigabe geschlossen werden kann.** Felder
    /// werden erst NACH dem `Drop`-Rumpf verworfen; ein hier gehaltener Griff waere also noch
    /// offen, waehrend der Lock schon frei ist — der naechste Halter oeffnete seinen Griff, und
    /// Loom meldete zu Recht „currently writing to cell". Im echten Code endet die Ausleihe mit
    /// dem letzten `deref`, also VOR der Freigabe; `take()` bildet genau das nach.
    #[cfg(loom)]
    ptr: Option<loom::cell::MutPtr<T>>,
}

impl<T: ?Sized> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: Solange dieser Guard lebt, hält er exklusiv den Lock; kein
        // anderer Zugriff auf `data` existiert gleichzeitig.
        #[cfg(not(loom))]
        return unsafe { &*self.lock.data.get() };
        // SAFETY: wie oben; unter Loom über den verfolgten Zeigergriff.
        #[cfg(loom)]
        return unsafe { &*self.ptr.as_ref().unwrap().deref() };
    }
}

impl<T: ?Sized> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: wie `deref`; exklusiver Besitz durch den lebenden Guard.
        #[cfg(not(loom))]
        return unsafe { &mut *self.lock.data.get() };
        #[cfg(loom)]
        return unsafe { self.ptr.as_ref().unwrap().deref() };
    }
}

impl<T: ?Sized> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        // Zugriffsfenster schliessen, BEVOR der Lock freigegeben wird (s. Feld `ptr`).
        #[cfg(loom)]
        drop(self.ptr.take());
        // Nächstes Ticket bedienen; `Release` veröffentlicht alle Schreibzugriffe
        // unter dem Lock an den nächsten Halter. DANACH den IRQ-Zustand wiederherstellen.
        self.lock.now_serving.fetch_add(1, Ordering::Release);
        irq_restore(self.daif);
    }
}

/// **Writer-bevorzugender Reader-Writer-Spinlock.**
///
/// Mehrere Leser dürfen den kritischen Abschnitt gleichzeitig betreten (geteilt);
/// ein Schreiber bekommt ihn exklusiv. Genau das, was eine Capability-Tabelle braucht:
/// die heißen IPC-Lookups (nur lesend) laufen auf verschiedenen Kernen **parallel**,
/// während die seltenen Mutationen (install/copy/mint/move/delete/revoke/grant)
/// exklusiv serialisiert werden. Schreiber haben Vorrang (angemeldete Schreiber lassen
/// keine neuen Leser mehr eintreten) — so hungern die seltenen Schreiber nicht aus.
///
/// Sperrordnung: Ein `RwSpinLock` nimmt **dieselbe** Position wie ein `SpinLock` an
/// derselben Stelle ein; `read()` und `write()` liegen an derselben Ordnungsposition
/// (shared vs. exklusiv desselben Locks). Wie [`SpinLock`] erst nach MMU-An benutzbar.
///
/// **IRQ-Sicherheit (wie [`SpinLock`], Mechanismus statt Konvention):** `read()`/`write()`
/// maskieren IRQs am eigenen Kern, BEVOR sie sich anmelden, und der Guard stellt den
/// Vorzustand beim `Drop` wieder her. Ohne das hinge die Deadlockfreiheit daran, dass
/// **jeder** Aufrufer im preemptierbaren EL1-Threadkontext selbst maskiert: wird ein Halter
/// dort vom Timer-Tick verdrängt und läuft auf demselben Kern ein Syscall an (im Trap sind
/// IRQs hardwareseitig maskiert), der denselben Lock nimmt, spinnt dieser Kern für immer —
/// der Halter kann nie wieder eingeplant werden. Die Maskierung gehört deshalb in den Lock,
/// nicht in die Aufrufer (die bestehenden `local_irq_disable`-Klammern an den Aufrufstellen
/// bleiben gültig und sind durch das nesting-sichere Save/Restore unschädlich).
pub struct RwSpinLock<T: ?Sized> {
    /// Bit 31 (`WRITER`) gesetzt = ein Schreiber hält exklusiv; Bits 0..30 = Anzahl
    /// aktiver Leser.
    state: AtomicU32,
    /// Anzahl angemeldeter Schreiber (auch wartender). Solange > 0 treten neue Leser
    /// zurück -> Writer-Vorrang, keine Schreiber-Aushungerung.
    writers_waiting: AtomicU32,
    data: UnsafeCell<T>,
}

const RW_WRITER: u32 = 1 << 31;

// SAFETY: Der Lock serialisiert Schreiber exklusiv gegen alle anderen und lässt nur
// gleichzeitige *Leser* zu (die nur `&T` erhalten) — geteilter Zugriff ist damit
// datenrennenfrei, sofern `T: Send + Sync` über Kerngrenzen bewegt/geteilt werden darf.
unsafe impl<T: ?Sized + Send> Sync for RwSpinLock<T> {}
unsafe impl<T: ?Sized + Send> Send for RwSpinLock<T> {}

impl<T> RwSpinLock<T> {
    ctor! { value,
        Self {
            state: AtomicU32::new(0),
            writers_waiting: AtomicU32::new(0),
            data: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> RwSpinLock<T> {
    /// Geteilt (lesend) sperren. Blockiert nur, solange ein Schreiber hält oder
    /// angemeldet ist. IRQ-sicher: maskiert IRQs am eigenen Kern für die Dauer des Guards.
    pub fn read(&self) -> RwReadGuard<'_, T> {
        // IRQs VOR der Anmeldung maskieren (s. Typ-Doku): ein Halter darf nicht verdrängt
        // werden, sonst spinnt ein Syscall auf demselben Kern für immer.
        let daif = irq_save_disable();
        loop {
            // Angemeldeten Schreibern den Vortritt lassen (kein Writer-Starving).
            while self.writers_waiting.load(Ordering::Acquire) != 0 {
                spin_hint();
            }
            // Optimistisch als Leser eintragen.
            let prev = self.state.fetch_add(1, Ordering::Acquire);
            if prev & RW_WRITER == 0 && self.writers_waiting.load(Ordering::Acquire) == 0 {
                return RwReadGuard {
                    lock: self,
                    daif,
                    #[cfg(loom)]
                    ptr: Some(self.data.get()),
                };
            }
            // Ein Schreiber kam dazwischen -> Eintrag zurücknehmen und erneut versuchen.
            self.state.fetch_sub(1, Ordering::Release);
        }
    }

    /// Exklusiv (schreibend) sperren. IRQ-sicher wie [`read`](Self::read).
    pub fn write(&self) -> RwWriteGuard<'_, T> {
        let daif = irq_save_disable();
        self.writers_waiting.fetch_add(1, Ordering::Acquire); // Intent -> Leser warten
        loop {
            // WRITER nur setzen, wenn weder Leser noch ein anderer Schreiber aktiv ist.
            if self
                .state
                .compare_exchange(0, RW_WRITER, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return RwWriteGuard {
                    lock: self,
                    daif,
                    #[cfg(loom)]
                    ptr: Some(self.data.get_mut()),
                };
            }
            spin_hint();
        }
    }
}

/// RAII-Guard für geteilten Lesezugriff (`Deref`, kein `DerefMut`).
pub struct RwReadGuard<'a, T: ?Sized> {
    lock: &'a RwSpinLock<T>,
    /// DAIF-Zustand vor dem Locken (beim `Drop` wiederhergestellt).
    daif: u64,
    /// Nur unter Loom: **lesender** Zeigergriff — mehrere davon dürfen gleichzeitig bestehen,
    /// genau das ist die geteilte Leserphase.
    /// **`Option`, damit das Zugriffsfenster VOR der Freigabe geschlossen werden kann.** Felder
    /// werden erst NACH dem `Drop`-Rumpf verworfen; ein hier gehaltener Griff waere also noch
    /// offen, waehrend der Lock schon frei ist — der naechste Halter oeffnete seinen Griff, und
    /// Loom meldete zu Recht „currently writing to cell". Im echten Code endet die Ausleihe mit
    /// dem letzten `deref`, also VOR der Freigabe; `take()` bildet genau das nach.
    #[cfg(loom)]
    ptr: Option<loom::cell::ConstPtr<T>>,
}

impl<T: ?Sized> Deref for RwReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: Solange dieser Read-Guard lebt, ist das WRITER-Bit nicht gesetzt;
        // es existiert kein exklusiver Schreiber -> nur geteilter Lesezugriff (`&T`).
        #[cfg(not(loom))]
        return unsafe { &*self.lock.data.get() };
        #[cfg(loom)]
        return unsafe { self.ptr.as_ref().unwrap().deref() };
    }
}

impl<T: ?Sized> Drop for RwReadGuard<'_, T> {
    fn drop(&mut self) {
        // Zugriffsfenster schliessen, BEVOR der Lock freigegeben wird (s. Feld `ptr`).
        #[cfg(loom)]
        drop(self.ptr.take());
        self.lock.state.fetch_sub(1, Ordering::Release);
        irq_restore(self.daif); // erst freigeben, DANN den IRQ-Zustand zurück
    }
}

/// RAII-Guard für exklusiven Schreibzugriff (`Deref` + `DerefMut`).
pub struct RwWriteGuard<'a, T: ?Sized> {
    lock: &'a RwSpinLock<T>,
    /// DAIF-Zustand vor dem Locken (beim `Drop` wiederhergestellt).
    daif: u64,
    /// Nur unter Loom: **schreibender** Zeigergriff — er darf mit keinem anderen Griff
    /// überlappen, sonst meldet der Modellprüfer ein Datenrennen.
    /// **`Option`, damit das Zugriffsfenster VOR der Freigabe geschlossen werden kann.** Felder
    /// werden erst NACH dem `Drop`-Rumpf verworfen; ein hier gehaltener Griff waere also noch
    /// offen, waehrend der Lock schon frei ist — der naechste Halter oeffnete seinen Griff, und
    /// Loom meldete zu Recht „currently writing to cell". Im echten Code endet die Ausleihe mit
    /// dem letzten `deref`, also VOR der Freigabe; `take()` bildet genau das nach.
    #[cfg(loom)]
    ptr: Option<loom::cell::MutPtr<T>>,
}

impl<T: ?Sized> Deref for RwWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: exklusiver Besitz durch das gesetzte WRITER-Bit; kein anderer Zugriff.
        #[cfg(not(loom))]
        return unsafe { &*self.lock.data.get() };
        #[cfg(loom)]
        return unsafe { &*self.ptr.as_ref().unwrap().deref() };
    }
}

impl<T: ?Sized> DerefMut for RwWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: wie `deref`; exklusiver Besitz durch den lebenden Write-Guard.
        #[cfg(not(loom))]
        return unsafe { &mut *self.lock.data.get() };
        #[cfg(loom)]
        return unsafe { self.ptr.as_ref().unwrap().deref() };
    }
}

impl<T: ?Sized> Drop for RwWriteGuard<'_, T> {
    fn drop(&mut self) {
        // Zugriffsfenster schliessen, BEVOR der Lock freigegeben wird (s. Feld `ptr`).
        #[cfg(loom)]
        drop(self.ptr.take());
        // NUR das WRITER-Bit löschen (nicht `store(0)`): ein Leser könnte transient
        // optimistisch hochgezählt haben und gleich wieder zurücknehmen; `fetch_and`
        // bewahrt eine solche Leserzahl -> kein Unterlauf.
        self.lock.state.fetch_and(!RW_WRITER, Ordering::Release);
        self.lock.writers_waiting.fetch_sub(1, Ordering::Release);
        irq_restore(self.daif); // erst freigeben, DANN den IRQ-Zustand zurück
    }
}

// --- Nebenläufigkeits-Verifikation (Tier 2, Loom — erschöpfende Interleaving-Exploration) ------
//
// **Der Punkt von B-7.2:** diese Beweise laufen über `SpinLock`/`RwSpinLock` **aus dieser Datei**.
// Es gibt keine zweite Fassung mehr, die auseinanderlaufen könnte. Wer oben eine Speicherordnung
// abschwächt oder `fetch_and(!WRITER)` durch `store(0)` ersetzt, bricht diese Tests — vorher brach
// er nur die Kopie unter `Verification/`, also nichts.
//
// Was NICHT abgedeckt ist, steht am Dateianfang (IRQ-Maskierung, Datenzelle, HW-Speichermodell).
// Gefahren von `tools/loom-verify.sh`.
#[cfg(all(loom, test))]
mod loom_proofs {
    use super::*;
    use loom::sync::Arc;
    use loom::thread;

    /// **BEWEIS (Ticket-SpinLock):** zwei Kerne erhöhen je um 1. Gegenseitiger Ausschluss heisst:
    /// kein Lost-Update, in **jedem** Interleaving genau 2.
    #[test]
    fn ticket_lock_kein_lost_update() {
        loom::model(|| {
            let lock = Arc::new(SpinLock::new(0u32));
            let a = lock.clone();
            let b = lock.clone();
            let ta = thread::spawn(move || *a.lock() += 1);
            let tb = thread::spawn(move || *b.lock() += 1);
            ta.join().unwrap();
            tb.join().unwrap();
            assert_eq!(*lock.lock(), 2, "Ticket-Lock: Lost-Update / kein Ausschluss");
        });
    }

    /// **BEWEIS (Ticket-SpinLock, Zustand):** nach balancierten Lock/Unlock ist der Ticketzustand
    /// wieder ausgeglichen (`next_ticket == now_serving`) — der Lock ist erneut nehmbar und hat
    /// kein Ticket verloren.
    #[test]
    fn ticket_lock_zustand_kehrt_zurueck() {
        loom::model(|| {
            let lock = Arc::new(SpinLock::new(0u32));
            let a = lock.clone();
            let ta = thread::spawn(move || {
                let _g = a.lock();
            });
            {
                let _g = lock.lock();
            }
            ta.join().unwrap();
            assert_eq!(
                lock.next_ticket.load(Ordering::Relaxed),
                lock.now_serving.load(Ordering::Relaxed),
                "Ticketzustand nicht ausgeglichen"
            );
        });
    }

    /// **BEWEIS (RwSpinLock):** ein Schreiber, ein Leser. Der Leser sieht nur konsistente Werte,
    /// nie einen halben Schreibvorgang.
    #[test]
    fn rw_ein_schreiber_ein_leser_kein_torn_read() {
        loom::model(|| {
            let lock = Arc::new(RwSpinLock::new(0u64));
            let w = lock.clone();
            let tw = thread::spawn(move || *w.write() = 7);
            let seen = *lock.read();
            assert!(seen == 0 || seen == 7, "torn read: {seen}");
            tw.join().unwrap();
            assert_eq!(*lock.read(), 7);
        });
    }

    /// **BEWEIS (RwSpinLock):** zwei Schreiber, je +1 -> exklusiver Zugriff -> kein Lost-Update.
    ///
    /// Fängt insbesondere einen kaputten Release (`store(0)` statt `fetch_and(!WRITER)`) **und**
    /// fehlende Exklusivität.
    #[test]
    fn rw_zwei_schreiber_kein_lost_update() {
        loom::model(|| {
            let lock = Arc::new(RwSpinLock::new(0u64));
            let a = lock.clone();
            let b = lock.clone();
            let ta = thread::spawn(move || *a.write() += 1);
            let tb = thread::spawn(move || *b.write() += 1);
            ta.join().unwrap();
            tb.join().unwrap();
            assert_eq!(*lock.read(), 2, "Lost-Update / kein gegenseitiger Ausschluss");
        });
    }

    /// **BEWEIS (RwSpinLock, Zählerarithmetik):** ein Leser, der transient hochzählt, während der
    /// Schreiber hält/freigibt, darf den Zustand nicht beschädigen. Am Ende ist `state` exakt `0`
    /// — kein hängendes WRITER-Bit, kein Unterlauf der Leserzahl.
    ///
    /// Das ist die Eigenschaft, für die der Release `fetch_and(!WRITER)` und nicht `store(0)` ist.
    #[test]
    fn rw_transienter_leser_beschaedigt_den_zaehler_nicht() {
        loom::model(|| {
            let lock = Arc::new(RwSpinLock::new(0u64));
            let w = lock.clone();
            let r = lock.clone();
            let tw = thread::spawn(move || *w.write() += 1);
            let tr = thread::spawn(move || {
                let _ = *r.read();
            });
            tw.join().unwrap();
            tr.join().unwrap();
            assert_eq!(*lock.read(), 1);
            assert_eq!(lock.state.load(Ordering::Relaxed), 0, "state nicht zurueck auf frei");
            assert_eq!(lock.writers_waiting.load(Ordering::Relaxed), 0);
        });
    }

    /// **BEWEIS (Writer-Vorrang):** ein angemeldeter Schreiber lässt keine neuen Leser mehr ein.
    /// Ohne diese Eigenschaft könnten Leser einen Schreiber aushungern.
    ///
    /// Geprüft über das Ergebnis: der Leser sieht entweder den Vor- oder den Nachzustand, und der
    /// Schreiber kommt in **jedem** Interleaving durch (kein Verklemmen, `join` kehrt zurück).
    #[test]
    fn rw_schreiber_kommt_in_jedem_interleaving_durch() {
        loom::model(|| {
            let lock = Arc::new(RwSpinLock::new(0u64));
            let w = lock.clone();
            let r = lock.clone();
            let tr = thread::spawn(move || *r.read());
            let tw = thread::spawn(move || *w.write() = 5);
            let seen = tr.join().unwrap();
            tw.join().unwrap();
            assert!(seen == 0 || seen == 5, "torn read: {seen}");
            assert_eq!(*lock.read(), 5, "Schreiber kam nicht durch");
        });
    }
}

// Formale Verifikation (Tier 1, Kani — bounded Model Checking). Nur unter `cargo kani` kompiliert,
// im Normal-Build inert. WICHTIG zur REICHWEITE: Kani ist ein **single-threaded** Modellprüfer und
// **kein** Nebenläufigkeits-Checker — die Kern-Aussage eines Locks (gegenseitiger Ausschluss unter
// *gleichzeitigem* Zugriff über Kerngrenzen, Interleavings) ist **außerhalb** von Kanis Reichweite
// (dafür bräuchte es Loom/TLA+, mögliche spätere Ergänzung). Kani beweist hier die single-threaded
// abgesicherten Eigenschaften: Memory-Safety der Guard-Derefs (kein UB), Lock/Unlock-Round-Trip +
// Daten-Persistenz, und die **Zähler-Arithmetik** (Reader-Count/Writer-Bit panik-/overflow-/
// underflow-frei, Zustands-Rückkehr auf „frei"). Unter dem Kani-Host-Target greifen die No-Op-
// IRQ-Stubs (kein DAIF-Asm).
#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use core::sync::atomic::Ordering::Relaxed;

    /// **BEWEIS (SpinLock, single-thread):** `lock()` liefert exklusiven Zugriff (Guard-Deref
    /// memory-safe), Schreiben+Lesen über den Guard ist konsistent, nach `Drop` ist der Lock wieder
    /// frei (Ticket-Zustand `next==serving`) und **erneut sperrbar**, die Daten persistieren.
    #[kani::proof]
    #[kani::unwind(2)]
    fn spinlock_roundtrip() {
        let lock = SpinLock::new(0u32);
        let v: u32 = kani::any();
        {
            let mut g = lock.lock();
            *g = v;
            assert!(*g == v); // Schreiben+Lesen über den Guard
        } // Drop -> freigeben
        // Ticket-Zustand zurück auf „frei": next_ticket == now_serving.
        assert!(lock.next_ticket.load(Relaxed) == lock.now_serving.load(Relaxed));
        {
            let g = lock.lock(); // erneut sperrbar
            assert!(*g == v); // Daten persistierten
        }
    }

    /// **BEWEIS (RwSpinLock, single-thread):** `write()` liefert exklusiven Schreibzugriff,
    /// anschließend sieht ein `read()` den geschriebenen Wert (Guard-Derefs memory-safe); nach allen
    /// Drops ist der Zustand wieder `0` (frei, kein hängendes WRITER-Bit, keine Leserzahl).
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_write_then_read() {
        let lock = RwSpinLock::new(0u32);
        let v: u32 = kani::any();
        {
            let mut g = lock.write();
            *g = v;
        }
        {
            let g = lock.read();
            assert!(*g == v);
        }
        assert!(lock.state.load(Relaxed) == 0);
        assert!(lock.writers_waiting.load(Relaxed) == 0);
    }

    /// **BEWEIS (RwSpinLock-Arithmetik, single-thread):** verschachtelte Leser zählen den Reader-Count
    /// korrekt hoch/runter (kein Overflow/Underflow), und ein Schreiber-Zyklus löscht **nur** das
    /// WRITER-Bit (`fetch_and`) ohne die Leserzahl zu beschädigen. Nach balancierten Acquire/Release
    /// ist der Zustand exakt `0`. (Genau die dokumentierte „kein-Unterlauf"-Invariante des Writer-Drops.)
    // --- Was NUR Kani sieht (B-7.1) -----------------------------------------------------------
    //
    // Die drei Beweise oben laufen mit **konkreten** Werten (ein Leser, zwei Leser, ein Schreiber)
    // durch je einen Ablauf. Loom deckt dieselbe Grössenordnung ab, nur über Interleavings. Beide
    // sagen damit nichts über den Zustandsraum aus, in dem die eigentlichen Zusicherungen des
    // Zustandsworts leben: 31 Bit Leserzahl neben einem Schreiberbit, u32-Ticketzähler, die
    // überlaufen dürfen.
    //
    // Die folgenden Beweise nehmen den Zustand **symbolisch** — sie gelten für ALLE Leserzahlen
    // und ALLE Ticketstände, nicht für die zwei, drei, die ein Testlauf zufällig erreicht. Das ist
    // die Klasse, die ein Interleaving-Prüfer prinzipiell nicht erreicht: Loom müsste 2^31 Threads
    // starten, um dort hinzukommen.
    //
    // **Alle rufen den echten Code** (`read()`, `write()`, die `Drop`-Impls) — der Zustand wird nur
    // vorher gesetzt, die Logik nicht nachgebaut.

    /// **BEWEIS (Release bewahrt JEDE Leserzahl):** hält ein Schreiber und hat ein Leser transient
    /// hochgezählt, muss der Release **nur** das WRITER-Bit löschen. Für eine *beliebige*
    /// Leserzahl, nicht für die eine, die ein Testlauf trifft.
    ///
    /// Das ist die Zusicherung, für die `RwWriteGuard::drop` `fetch_and(!RW_WRITER)` benutzt und
    /// nicht `store(0)` — mit `store(0)` verlöre der Lock hier die Leserzahl, und der spätere
    /// `fetch_sub` des Lesers liefe in einen **Unterlauf**.
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_release_bewahrt_beliebige_leserzahl() {
        let lock = RwSpinLock::new(0u8);
        let n: u32 = kani::any();
        // Legale transiente Leserzahl: sie teilt sich das Wort mit dem Schreiberbit.
        kani::assume(n < RW_WRITER);
        {
            let _w = lock.write(); // ECHTER Acquire: state == RW_WRITER
            // Genau das, was ein optimistischer Leser in `read()` tut, bevor er zurücktritt.
            lock.state.fetch_add(n, Relaxed);
        } // ECHTER Release
        assert!(
            lock.state.load(Relaxed) == n,
            "Release hat die transiente Leserzahl beschaedigt"
        );
        assert!(lock.writers_waiting.load(Relaxed) == 0);
    }

    /// **BEWEIS (ein Leser setzt nie das Schreiberbit):** für **jede** Leserzahl unterhalb der
    /// Wortgrenze lässt `read()` das WRITER-Bit unberührt, und der Guard-Drop führt exakt dorthin
    /// zurück, wo er angefangen hat.
    ///
    /// Die Leserzahl belegt Bit 0..30, das Schreiberbit ist Bit 31 — ein `fetch_add` ohne obere
    /// Schranke könnte hineinlaufen. Dass es das unterhalb der Grenze **nie** tut, ist eine
    /// Aussage über 2^31 Zustände; ein Lauf mit zwei Lesern belegt sie nicht.
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_leser_setzt_nie_das_writer_bit() {
        let lock = RwSpinLock::new(0u8);
        let n: u32 = kani::any();
        kani::assume(n < RW_WRITER - 1); // s. `rwlock_leserzahl_grenze_ist_scharf`
        lock.state.store(n, Relaxed);
        {
            let _r = lock.read(); // ECHTER Acquire
            assert!(lock.state.load(Relaxed) == n + 1, "Leserzahl nicht um genau 1 erhoeht");
            assert!(
                lock.state.load(Relaxed) & RW_WRITER == 0,
                "ein Leser hat das WRITER-Bit gesetzt -- er saehe fuer alle anderen wie ein \
                 Schreiber aus"
            );
        }
        assert!(lock.state.load(Relaxed) == n, "Guard-Drop kehrt nicht zum Ausgangswert zurueck");
    }

    /// **BEWEIS (die Grenze ist scharf, nicht theoretisch):** bei `RW_WRITER - 1` Lesern kippt die
    /// Eigenschaft — der nächste Leser setzt Bit 31 und ist von einem Schreiber nicht mehr zu
    /// unterscheiden.
    ///
    /// Der Beweis steht hier, damit die Annahme des vorigen **nicht leer** ist. Eine Zusicherung
    /// unter einer Bedingung, von der niemand weiss, ob sie je greift, ist keine Zusicherung,
    /// sondern eine Formulierung. Praktisch ist die Grenze bequem — 2^31 gleichzeitige Leser
    /// hiesse 2^31 Kerne oder Schachtelungstiefen — aber sie ist eine **Grenze** und steht damit
    /// im Code, nicht in einer Annahme, die nie jemand ausgesprochen hat.
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_leserzahl_grenze_ist_scharf() {
        let lock = RwSpinLock::new(0u8);
        lock.state.store(RW_WRITER - 1, Relaxed);
        let _r = lock.read();
        assert!(
            lock.state.load(Relaxed) & RW_WRITER != 0,
            "Grenze nicht erreicht -- dann waere die Annahme im Nachbarbeweis unnoetig"
        );
    }

    /// **BEWEIS (Ticketüberlauf ist unschädlich):** von einem **beliebigen** Ticketstand aus —
    /// auch dicht an `u32::MAX`, wo `fetch_add` überläuft — sperrt und entsperrt der Ticket-Lock
    /// sauber, und der Zustand kehrt ins Gleichgewicht zurück.
    ///
    /// Loom startet bei 0 und kommt mit zwei, drei Threads nie in die Nähe des Überlaufs. Genau
    /// dort trägt die Konstruktion aber nur, weil `lock()` auf **Ungleichheit** wartet statt auf
    /// „grösser gleich": ein `>=`-Vergleich wäre über den Wrap hinweg falsch.
    #[kani::proof]
    #[kani::unwind(3)]
    fn spinlock_ticket_ueberlauf_ist_unschaedlich() {
        let lock = SpinLock::new(0u32);
        let t: u32 = kani::any();
        // Freier Lock bei beliebigem Ticketstand (einschliesslich u32::MAX).
        lock.next_ticket.store(t, Relaxed);
        lock.now_serving.store(t, Relaxed);
        let v: u32 = kani::any();
        {
            let mut g = lock.lock(); // ECHTER Acquire, fetch_add darf ueberlaufen
            *g = v;
        }
        assert!(
            lock.next_ticket.load(Relaxed) == lock.now_serving.load(Relaxed),
            "Ticketzustand nach dem Wrap nicht ausgeglichen"
        );
        // Und der Lock ist danach WIEDER nehmbar -- ein Wrap darf ihn nicht stilllegen.
        {
            let g = lock.lock();
            assert!(*g == v, "Daten ueber den Wrap hinweg verloren");
        }
    }

    /// **BEWEIS (Schreiberanmeldung bleibt ausgeglichen):** von einer **beliebigen** Zahl bereits
    /// angemeldeter Schreiber aus lässt ein vollständiger `write()`-Zyklus `writers_waiting` exakt
    /// dort, wo er ihn vorgefunden hat.
    ///
    /// Driftete der Zähler auch nur um eins nach oben, bliebe er für immer > 0 — und `read()`
    /// wartet auf genau diese Null. Aus dem Writer-Vorrang würde eine **dauerhafte Lesersperre**,
    /// und zwar erst nach vielen Zyklen, also weit entfernt von der Ursache.
    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_writers_waiting_bleibt_ausgeglichen() {
        let lock = RwSpinLock::new(0u8);
        let w: u32 = kani::any();
        kani::assume(w < u32::MAX); // sonst waere schon das Anmelden ein Ueberlauf
        lock.writers_waiting.store(w, Relaxed);
        {
            let _g = lock.write(); // ECHTER Acquire (state == 0 -> CAS greift sofort)
            assert!(lock.writers_waiting.load(Relaxed) == w + 1, "Anmeldung nicht gezaehlt");
        }
        assert!(
            lock.writers_waiting.load(Relaxed) == w,
            "writers_waiting driftet -- Leser wuerden dauerhaft ausgesperrt"
        );
        assert!(lock.state.load(Relaxed) == 0);
    }

    #[kani::proof]
    #[kani::unwind(3)]
    fn rwlock_state_arithmetic() {
        let lock = RwSpinLock::new(0u8);
        {
            let _r1 = lock.read();
            assert!(lock.state.load(Relaxed) == 1);
            let _r2 = lock.read(); // zwei gleichzeitige Leser (im selben Thread)
            assert!(lock.state.load(Relaxed) == 2);
        } // beide Drop -> Reader-Count zurück auf 0 (kein Underflow)
        assert!(lock.state.load(Relaxed) == 0);
        {
            let _w = lock.write();
            assert!(lock.state.load(Relaxed) == RW_WRITER); // nur WRITER-Bit, Leserzahl 0
        } // Drop: fetch_and(!WRITER) -> 0, writers_waiting -> 0
        assert!(lock.state.load(Relaxed) == 0);
        assert!(lock.writers_waiting.load(Relaxed) == 0);
    }
}
