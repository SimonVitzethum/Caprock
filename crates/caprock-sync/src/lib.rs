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

// --- Die SPERRHALTEDAUER als Zahl mit Schwelle (C9) --------------------------------------------
//
// **Warum das Modul HIER steht und nicht in einer eigenen Datei.** `tools/loom-verify.sh` kopiert
// genau EINE Datei (`crates/caprock-sync/src/lib.rs`) in ein eigenstaendiges Crate; ein zweites
// Modul waere dort nicht vorhanden, und der Loom-Beweis liesse sich nicht mehr uebersetzen. Die
// Dateigrenze ist damit keine Stilfrage, sondern eine Bedingung des Tier-2-Gates.
//
// **Warum die Messung nicht aus `caprock-hal` kommt** (obwohl es dort `timer::cycles()` gibt, und
// zwar mit zwischengespeicherter Merkmalserkennung): `caprock-sync` ist ABHAENGIGKEITSFREI, und
// beide Beweis-Gates haengen genau daran. `tools/kani-verify.sh` baut die Crate mit einem
// erzeugten Manifest ohne `[dependencies]`, `tools/loom-verify.sh` ebenso. Eine Kante
// `caprock-sync -> caprock-hal` risse **beide** Gates, und zwar mit einem Uebersetzungsfehler in
// $TMPDIR, den niemand mit dieser Zeile in Verbindung braechte.
//
// Der Grund hinter dem Auftrag bleibt trotzdem gewahrt, und er heisst nicht „nimm die HAL",
// sondern **„kein `cpuid` im heissen Pfad"**: `zyklen()` unten fuehrt ueberhaupt keine
// Merkmalserkennung durch. Es liest den Zaehler, den die HAL auf derselben Architektur liest
// (x86: TSC, aarch64: `CNTPCT_EL0`) -- nur ohne die serialisierende Klammer. S. dort.
pub mod sperrwacht {
    //! **Wie lange war der Kern am Stueck mit maskierten Interrupts unterbrechbarkeitsfrei?**
    //!
    //! # Warum es das gibt
    //!
    //! [`SpinLock::lock`](super::SpinLock::lock) maskiert die IRQs des eigenen Kerns (s.
    //! Modulanfang) und der Guard gibt sie beim `Drop` wieder frei. Zwischen diesen beiden Punkten
    //! kann dieser Kern **nicht praemptiert werden** -- kein Timer-Tick, keine Umplanung, keine
    //! Zustellung. Diese Dauer ist die Latenzzusage eines Mikrokerns, und sie war bis hierher eine
    //! **unsichtbare Groesse**: kein Zaehler, keine Schwelle, keine Pruefzeile.
    //!
    //! Der Anlass ist gemessen und steht in `CLAUDE.md`: im C8-Verifizierer haette
    //! `while let Some(a) = SCHLANGE.lock().entnehmen() { laden(&a) }` die gesamte
    //! Ed25519-/SHA-2-Pruefung unter der Sperre gefahren, weil der Guard eines
    //! `while let`-Scrutinees bis zum ENDE DES RUMPFES lebt. Die Suite war gruen, jeder Messwert
    //! stimmte, und das Latenzloch sah **keine** Pruefzeile an. Gefunden wurde es mit einem
    //! `Drop`-Zeugen von Hand -- also durch Glueck, nicht durch einen Mechanismus.
    //!
    //! # Was gemessen wird
    //!
    //! Das **Maximum** ueber alle Sperrhaltungen, mit der **Stelle**, an der es entstand
    //! (`#[track_caller]` -> Datei + Zeile des `lock()`-Aufrufs). Eine Zahl ohne Ort waere ein
    //! Alarm ohne Adresse -- genau die Form, an der dieses Projekt bei „Hoechststand 12008 B"
    //! schon einmal gelernt hat, dass ein Rekord ohne Subjekt unauffindbar bleibt.
    //!
    //! Das Fenster ist **`irq_save_disable()` bis kurz vor `irq_restore()`**, also die wirklich
    //! maskierte Zeit einschliesslich des Wartens auf das Ticket. Das ist Absicht: wer 200 000
    //! Zyklen auf einen fremden Halter wartet, ist genauso lange nicht praemptierbar wie der
    //! Halter selbst.
    //!
    //! **Verschachtelte Guards messen jeder ihr eigenes Fenster.** Der aeussere schliesst den
    //! inneren ein und ist damit die wahre maskierte Dauer -- er gewinnt das Maximum, und das ist
    //! richtig so.
    //!
    //! # Was es NICHT sieht (und das gehoert in die Zeile, nicht in eine Fussnote)
    //!
    //! * **Maskierung ohne Sperre.** `local_irq_disable()` von Hand, Trap-Kontext, der Panikpfad
    //!   -- dort ist ebenfalls nicht praemptierbar, und diese Marke schweigt dazu. Sie misst
    //!   Sperrhaltedauer, nicht Maskierungsdauer.
    //! * **Den zweitlaengsten Halter.** Gehalten wird EIN Maximum je Lauf, nicht eine Verteilung.
    //! * **Die Zeit zwischen zwei Messungen.** Ein Kern, der lange gar keine Sperre nimmt, faellt
    //!   hier nicht auf.
    //!
    //! # Fail-closed
    //!
    //! Faellt die Messung aus, ist der Hoechststand `0` -- und `0` ist von „alles kurz" nicht zu
    //! unterscheiden. Dagegen stehen drei Dinge: [`MESSUNG_VORHANDEN`] (die Architektur hat
    //! ueberhaupt einen Zaehler), [`Stand::gesehen`] (es wurde ueberhaupt eine Sperrhaltung
    //! gemessen) und die Eichung im Kernel (das Messgeraet trennt bekannte Dauern). Erst die drei
    //! zusammen unterscheiden ein arbeitendes Messgeraet von einem stummen.

    #[cfg(feature = "sperrwacht")]
    use core::panic::Location;
    #[cfg(feature = "sperrwacht")]
    use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

    // ------------------------------------------------------------------------------------------
    // DER ZEITSTEMPEL -- ohne `cpuid`, ohne Merkmalserkennung, ohne Abhaengigkeit
    // ------------------------------------------------------------------------------------------

    /// Traegt diese Uebersetzung ueberhaupt einen Zaehler? `false` heisst **nicht gemessen** und
    /// muss im Urteil als solches ausscheiden -- niemals als „alles kurz".
    #[cfg(all(feature = "sperrwacht", target_arch = "x86_64", target_os = "none"))]
    pub const MESSUNG_VORHANDEN: bool = true;
    #[cfg(all(feature = "sperrwacht", target_arch = "aarch64"))]
    pub const MESSUNG_VORHANDEN: bool = true;
    #[cfg(any(
        not(feature = "sperrwacht"),
        not(any(all(target_arch = "x86_64", target_os = "none"), target_arch = "aarch64"))
    ))]
    pub const MESSUNG_VORHANDEN: bool = false;

    /// Ein Zyklenstempel des **heissen Pfads**.
    ///
    /// **Bewusst NICHT die serialisierende Fassung aus `caprock_hal::timer::cycles()`.** Die
    /// klammert mit `rdtscp`/`lfence` (x86) bzw. `isb` (aarch64), weil sie fuer *kurze* Sektionen
    /// gebaut ist, deren Grenzen sonst verrutschen. Hier ist die Lage umgekehrt:
    ///
    /// * Gemessen wird gegen eine Schwelle von **einem Timer-Tick** (10 ms, also zweistellige
    ///   Millionen Zyklen). Ein paar Dutzend Zyklen Verschiebung an den Raendern aendern an dem
    ///   Urteil nichts -- die Klammer kostete Genauigkeit, die niemand liest.
    /// * Dieser Pfad laeuft in **jedem** Syscall. `lfence` und `isb` sind genau dort teuer.
    ///
    /// Was hier ausdruecklich NICHT passiert, ist `cpuid`: unter KVM ist das ein bedingungsloser
    /// VM-Exit (gemessen: 3556 statt 51 Zyklen), und deshalb faellt auch die
    /// `rdtscp`-Verfuegbarkeitsfrage weg -- `rdtsc` gibt es seit dem Pentium.
    #[cfg(all(feature = "sperrwacht", target_arch = "x86_64", target_os = "none"))]
    #[inline(always)]
    pub fn zyklen() -> u64 {
        // SAFETY: `rdtsc` ist ein reiner Lesebefehl ohne Speicherwirkung; er ist auf jedem
        // 64-Bit-x86 vorhanden und in Ring 0 nie gesperrt (CR4.TSD betrifft nur Ring > 0).
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    /// aarch64: derselbe Zaehler, den `caprock_hal::aarch64::timer::cycles()` liest -- damit passt
    /// die Schwelle (aus `cycles_per_sec()`) zur Messung. Ohne das `isb`, s. x86-Fassung.
    #[cfg(all(feature = "sperrwacht", target_arch = "aarch64"))]
    #[inline(always)]
    pub fn zyklen() -> u64 {
        let v: u64;
        // SAFETY: `CNTPCT_EL0` ist ein architektonisch definiertes, nur lesbares Systemregister.
        unsafe {
            core::arch::asm!("mrs {v}, cntpct_el0", v = out(reg) v, options(nomem, nostack));
        }
        v
    }

    /// Host-/Loom-/Kani-Ziele: kein Zaehler. Gibt `0`, und [`MESSUNG_VORHANDEN`] ist dort `false`
    /// -- das Urteil im Kernel scheidet diesen Fall ausdruecklich aus.
    #[cfg(all(
        feature = "sperrwacht",
        not(any(all(target_arch = "x86_64", target_os = "none"), target_arch = "aarch64"))
    ))]
    #[inline(always)]
    pub fn zyklen() -> u64 {
        0
    }

    /// **Ohne das Feature gibt es die Funktion trotzdem** -- sie gibt `0`.
    ///
    /// Das ist keine Bequemlichkeit, sondern die Bedingung fuer das A/B: die Aufschlagsmessung im
    /// Kernel muss in **beiden** Bauten uebersetzen, sonst gibt es keine Vergleichszahl. Eine
    /// Messung, die nur im gemessenen Zustand laeuft, kann den Aufschlag nicht kennen.
    #[cfg(not(feature = "sperrwacht"))]
    #[inline(always)]
    pub fn zyklen() -> u64 {
        0
    }

    // **Waechter der Auswahl, dieselbe Bauform wie `IRQ_MASKING_IMPLEMENTED` oben.** Wer die
    // Wacht auf einem Bare-Metal-Ziel einschaltet, fuer das es keinen Zaehler gibt, bekommt einen
    // Uebersetzungsfehler statt einer Zeile, die dauerhaft `0` meldet. Eine ausgefallene Messung
    // sieht sonst genau wie ein gesundes System aus.
    #[cfg(all(feature = "sperrwacht", target_os = "none"))]
    const _: () = assert!(
        MESSUNG_VORHANDEN,
        "sperrwacht auf einem Bare-Metal-Ziel ohne Zyklenzaehler: die Marke meldete dauerhaft 0, \
         und 0 ist von 'alles kurz' nicht zu unterscheiden. `zyklen()` fuer diese Architektur \
         implementieren oder das Feature dort nicht setzen."
    );

    // ------------------------------------------------------------------------------------------
    // DAS WASSERZEICHEN
    // ------------------------------------------------------------------------------------------

    /// Wieviele Sperrhaltungen wurden ueberhaupt gemessen (seit Beginn, ueber alle Kerne).
    #[cfg(feature = "sperrwacht")]
    static GESEHEN: AtomicU64 = AtomicU64::new(0);
    /// Stand von [`GESEHEN`] bei der letzten Ruecksetzung -- `gesehen_seit` ist die Differenz.
    #[cfg(feature = "sperrwacht")]
    static GESEHEN_BASIS: AtomicU64 = AtomicU64::new(0);
    /// Laengste gemessene Sperrhaltung **seit der Ruecksetzung**, in Zyklen.
    #[cfg(feature = "sperrwacht")]
    static MAX: AtomicU64 = AtomicU64::new(0);
    /// Die Stelle dazu (`lock()`-Aufrufer). Nullzeiger = keine.
    #[cfg(feature = "sperrwacht")]
    static STELLE: AtomicPtr<Location<'static>> = AtomicPtr::new(core::ptr::null_mut());
    /// Laengste Sperrhaltung **ohne die erklaerten Schuldner** (s. [`schulden_setzen`]).
    ///
    /// **Warum es diesen zweiten Kanal gibt.** Es gibt genau EIN Maximum. Ein bekannter,
    /// erklaerter Langhalter belegt es dauerhaft und **verdeckt damit alles darunter** -- eine
    /// neue, kuerzere, aber trotzdem verbotene Haltung waere unsichtbar, und die Zeile bliebe
    /// stumm, obwohl sie spricht. Das ist dieselbe Form wie „eine rote Zeile faerbt die Suite und
    /// macht sie fuer alles andere unbrauchbar", nur andersherum.
    ///
    /// Dieser Kanal laesst die erklaerten Stellen aus. Gegen ihn laeuft die eigentliche Schwelle;
    /// die Schuldner werden getrennt gegen ihren eigenen Deckel geprueft.
    #[cfg(feature = "sperrwacht")]
    static MAX_BEREINIGT: AtomicU64 = AtomicU64::new(0);
    /// Die Stelle dazu.
    #[cfg(feature = "sperrwacht")]
    static STELLE_BEREINIGT: AtomicPtr<Location<'static>> = AtomicPtr::new(core::ptr::null_mut());
    /// Laengste Sperrhaltung **vor** der Ruecksetzung (Hochlauf) -- gerettet, nicht verworfen.
    #[cfg(feature = "sperrwacht")]
    static MAX_HOCHLAUF: AtomicU64 = AtomicU64::new(0);
    /// Die Stelle dazu.
    #[cfg(feature = "sperrwacht")]
    static STELLE_HOCHLAUF: AtomicPtr<Location<'static>> = AtomicPtr::new(core::ptr::null_mut());
    /// Wie oft wurde der Hochlauf abgeschlossen. **Eine Ratsche:** das Urteil verlangt genau `1`
    /// (die Eichung). Wer eine rote Zeile durch ein zweites Leerraeumen gruen macht, faellt auf.
    #[cfg(feature = "sperrwacht")]
    static HOCHLAUF_ABSCHLUESSE: AtomicU64 = AtomicU64::new(0);
    /// Wie oft wurde ein Eich-Artefakt verworfen. Dieselbe Ratsche fuer den zweiten Weg.
    #[cfg(feature = "sperrwacht")]
    static EICH_VERWERFUNGEN: AtomicU64 = AtomicU64::new(0);
    /// Wie oft lief der Zaehler **rueckwaerts** (Zeitstempel des Endes kleiner als der des
    /// Anfangs). Das darf auf einem Kern nicht vorkommen; `> 0` ist ein Befund ueber den
    /// Zeitgeber (Migration mitten in der Sektion ist ausgeschlossen -- IRQs sind maskiert).
    ///
    /// **Kein `wrapping_sub`.** Ein um 100 Zyklen zurueckspringender Zaehler ergaebe rund `2^64`,
    /// und diese Zahl risse jede Schwelle -- „rueckwaerts" heisst **verworfen und gezaehlt**.
    #[cfg(feature = "sperrwacht")]
    static RUECKWAERTS: AtomicU64 = AtomicU64::new(0);

    /// Ein **erklaerter Langhalter**: eine Datei, ein Grund, ein eigener Deckel.
    ///
    /// **Je Posten ein eigener Deckel, und das ist keine Kosmetik.** Ein gemeinsamer Deckel waere
    /// eine Zahl fuer zwei Tatsachen: der grosszuegigste Posten deckte alle anderen mit ab, und
    /// ein Schuldner koennte um das Sechzigfache wachsen, ohne dass eine Zeile spricht. Dieselbe
    /// Form wie ein Bit, das zwei Gruende traegt.
    pub struct Schuldposten {
        /// Die Datei, deren `lock()`-Aufrufe ausgenommen sind.
        pub datei: &'static str,
        /// Hoechstwert, den dieser Posten haben darf, in Zyklen. **Ratsche: darf nur fallen.**
        pub deckel: u64,
        /// Warum das hier steht — im Bericht sichtbar, nicht in einem Kommentar versteckt.
        pub grund: &'static str,
    }

    /// Die **erklaerten Langhalter** — eine Menge von NAMEN, keine Zahl.
    ///
    /// Eine Kardinalzahl waere hier eine Ratsche mit einem Loch: sie griffe gegen Zuwachs, nicht
    /// gegen **Austausch** — und Austausch fuehlt sich beim Umbauen wie Fortschritt an. Dieselbe
    /// Lehre wie bei `IDENTITY_DEBTS`.
    pub struct Schuldliste {
        /// Die Posten, in der Reihenfolge ihrer Messplaetze.
        pub posten: &'static [Schuldposten],
    }

    /// Wieviele Schuldposten je einen eigenen Messplatz bekommen. Mehr Posten sind erlaubt, aber
    /// ohne eigenen Hoechststand — deshalb prueft [`schulden_setzen`] die Laenge (s. dort).
    pub const SCHULD_PLAETZE: usize = 8;

    /// Die gesetzte Liste (Nullzeiger = keine; dann sind beide Kanaele identisch).
    #[cfg(feature = "sperrwacht")]
    static SCHULDEN: AtomicPtr<Schuldliste> = AtomicPtr::new(core::ptr::null_mut());

    /// Hoechststand **je Schuldposten** — der Deckel wird gegen diese Zahl geprueft.
    #[cfg(feature = "sperrwacht")]
    static SCHULD_MAX: [AtomicU64; SCHULD_PLAETZE] =
        [const { AtomicU64::new(0) }; SCHULD_PLAETZE];

    /// Die erklaerten Langhalter hinterlegen. **Einmal beim Hochlauf**, vor SMP.
    ///
    /// Was hier steht, ist eine **benannte, datierte Schuld** und keine stille Ausnahme: der
    /// Bericht druckt Liste, Gruende und Deckel, und das Urteil laesst keinen Posten wachsen.
    ///
    /// Gibt `false`, wenn die Liste mehr Posten hat als es Messplaetze gibt — dann bekaemen die
    /// ueberzaehligen keinen eigenen Hoechststand und waeren **ausgenommen, ohne geprueft zu
    /// werden**. Das ist genau die Sorte stiller Freibrief, gegen die die Liste angetreten ist;
    /// der Aufrufer laesst sein Urteil daran scheitern.
    #[cfg(feature = "sperrwacht")]
    #[must_use]
    pub fn schulden_setzen(l: &'static Schuldliste) -> bool {
        if l.posten.len() > SCHULD_PLAETZE {
            return false;
        }
        SCHULDEN.store(l as *const _ as *mut _, Ordering::Release);
        true
    }

    #[cfg(not(feature = "sperrwacht"))]
    #[must_use]
    pub fn schulden_setzen(l: &'static Schuldliste) -> bool {
        l.posten.len() <= SCHULD_PLAETZE
    }

    /// Auf welchem Messplatz steht diese Stelle (`None` = kein Schuldner)?
    ///
    /// Laeuft **nur im seltenen Zweig** (wenn ein neuer bereinigter Hoechststand anstuende), nicht
    /// bei jeder Freigabe -- der Zeichenkettenvergleich kostet den heissen Pfad damit nichts.
    #[cfg(feature = "sperrwacht")]
    fn schuldplatz(stelle: &'static Location<'static>) -> Option<usize> {
        let raw = SCHULDEN.load(Ordering::Acquire);
        if raw.is_null() {
            return None;
        }
        // SAFETY: in `SCHULDEN` landet ausschliesslich ein `&'static Schuldliste` aus
        // `schulden_setzen`; der Zeiger wird als Ganzes atomar geschrieben und gelesen.
        let l: &'static Schuldliste = unsafe { &*(raw as *const Schuldliste) };
        l.posten.iter().position(|p| p.datei == stelle.file())
    }

    /// Hoechststand eines Schuldpostens (nach Messplatz).
    ///
    /// **Was diese Zahl ist und was nicht:** sie wird nur fortgeschrieben, wenn die Haltung
    /// groesser war als der bereinigte Hoechststand — sie ist also eine **Untergrenze**. Fuer den
    /// Zweck reicht das genau: ueberschreitet ein Schuldner die Schwelle, liegt er zwangslaeufig
    /// ueber dem bereinigten Hoechststand (der in einem gesunden Lauf darunter bleibt) und wird
    /// damit erfasst. Ein Schuldner unterhalb des bereinigten Hoechststands ist erst recht
    /// unterhalb der Schwelle.
    #[cfg(feature = "sperrwacht")]
    pub fn schuld_hoechststand(platz: usize) -> u64 {
        SCHULD_MAX
            .get(platz)
            .map_or(0, |a| a.load(Ordering::Relaxed))
    }

    #[cfg(not(feature = "sperrwacht"))]
    pub fn schuld_hoechststand(_platz: usize) -> u64 {
        0
    }

    /// Was der Anfang einer Sperrhaltung festhaelt. Liegt im Guard, nicht in einem `static` --
    /// eine per-Kern-Ablage waere hier falsch, weil Guards **verschachteln**.
    #[cfg(feature = "sperrwacht")]
    #[derive(Clone, Copy)]
    pub struct Wacht {
        t0: u64,
        stelle: &'static Location<'static>,
    }

    /// Ohne Feature ein ZST: der Guard waechst nicht, der Code verschwindet.
    #[cfg(not(feature = "sperrwacht"))]
    #[derive(Clone, Copy)]
    pub struct Wacht;

    /// Anfang einer Sperrhaltung -- **unmittelbar nach** dem Maskieren zu rufen.
    ///
    /// `#[track_caller]` reicht die Stelle des `lock()`-**Aufrufers** durch (die Attribute
    /// propagieren ueber `lock()` hinweg). Der Preis ist ein impliziter Zeiger je Aufruf, kein
    /// Laufzeitcode: `Location::caller()` liest eine statische Struktur.
    #[cfg(feature = "sperrwacht")]
    #[track_caller]
    #[inline(always)]
    pub fn beginn() -> Wacht {
        Wacht {
            t0: zyklen(),
            stelle: Location::caller(),
        }
    }

    #[cfg(not(feature = "sperrwacht"))]
    #[inline(always)]
    pub fn beginn() -> Wacht {
        Wacht
    }

    /// Ende einer Sperrhaltung -- **unmittelbar vor** dem Wiederherstellen der IRQ-Maske.
    ///
    /// Was hinter diesem Punkt noch maskiert laeuft (dieser Funktionsrumpf und `irq_restore`),
    /// geht in die Zahl nicht ein. Die Marke unterschaetzt damit um eine Handvoll Zyklen; sie
    /// **ueberschaetzt nie**, und das ist die Richtung, die zu einer Schwelle passt.
    #[cfg(feature = "sperrwacht")]
    #[inline(always)]
    pub fn ende(w: Wacht) {
        let t1 = zyklen();
        if t1 < w.t0 {
            RUECKWAERTS.fetch_add(1, Ordering::Relaxed);
            return; // verworfen, nicht „fast einmal herum"
        }
        let dauer = t1 - w.t0;
        GESEHEN.fetch_add(1, Ordering::Relaxed);
        // **Der Vergleich VOR dem Schreiben ist der Grund, warum das im heissen Pfad tragbar ist.**
        // Ein `fetch_max` je Freigabe waere ein Schreibzugriff auf eine geteilte Cachezeile aus
        // jedem Kern; der `load` hier laesst die Zeile geteilt (`Relaxed`, also ein L1-Treffer),
        // und geschrieben wird nur bei einem echten neuen Hoechststand -- logarithmisch selten.
        if dauer > MAX.load(Ordering::Relaxed) {
            // Das Rennen zweier Kerne um denselben neuen Hoechststand ist bekannt und
            // hingenommen: `fetch_max` haelt die ZAHL korrekt, die Stelle kann in diesem Fall vom
            // Verlierer stammen. Fuer eine Diagnose reicht das -- ein Lock an dieser Stelle waere
            // eine Sperre im Freigabepfad jeder Sperre.
            if MAX.fetch_max(dauer, Ordering::Relaxed) < dauer {
                STELLE.store(w.stelle as *const _ as *mut _, Ordering::Relaxed);
            }
        }
        // Der bereinigte Kanal: derselbe Vergleich, aber die erklaerten Schuldner bleiben draussen
        // und landen stattdessen auf ihrem eigenen Messplatz. `schuldplatz` laeuft erst hinter dem
        // `load` -- also nur, wenn ohnehin ein neuer Hoechststand anstuende, und das ist
        // logarithmisch selten. Der Zeichenkettenvergleich kostet den heissen Pfad damit nichts.
        if dauer > MAX_BEREINIGT.load(Ordering::Relaxed) {
            match schuldplatz(w.stelle) {
                Some(i) => {
                    SCHULD_MAX[i].fetch_max(dauer, Ordering::Relaxed);
                }
                None => {
                    if MAX_BEREINIGT.fetch_max(dauer, Ordering::Relaxed) < dauer {
                        STELLE_BEREINIGT.store(w.stelle as *const _ as *mut _, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    #[cfg(not(feature = "sperrwacht"))]
    #[inline(always)]
    pub fn ende(_w: Wacht) {}

    /// Der Messstand.
    #[derive(Clone, Copy)]
    pub struct Stand {
        /// Gemessene Sperrhaltungen **seit der Ruecksetzung**. `0` heisst „nichts gemessen" und
        /// darf nie wie „alles kurz" gelesen werden.
        pub gesehen: u64,
        /// Gemessene Sperrhaltungen insgesamt (einschliesslich vor der Ruecksetzung).
        pub gesehen_gesamt: u64,
        /// Laengste Sperrhaltung seit der Ruecksetzung, in Zyklen.
        pub max: u64,
        /// Datei der laengsten Sperrhaltung (`""` = keine).
        pub datei: &'static str,
        /// Zeile dazu (`0` = keine).
        pub zeile: u32,
        /// Laengste Sperrhaltung **ohne die erklaerten Schuldner** — gegen DIESE Zahl laeuft die
        /// Schwelle, damit ein bekannter Langhalter nicht alles darunter verdeckt.
        pub max_bereinigt: u64,
        /// Datei dazu (`""` = keine).
        pub datei_bereinigt: &'static str,
        /// Zeile dazu (`0` = keine).
        pub zeile_bereinigt: u32,
        /// Laengste Sperrhaltung **vor** der Ruecksetzung (Hochlauf).
        pub max_hochlauf: u64,
        /// Datei dazu.
        pub datei_hochlauf: &'static str,
        /// Zeile dazu.
        pub zeile_hochlauf: u32,
        /// Wie oft wurde der Hochlauf abgeschlossen (muss genau `1` sein: die Eichung).
        pub hochlauf_abschluesse: u64,
        /// Wie oft wurde ein Eich-Artefakt verworfen (muss genau `1` sein: die Eichung).
        pub eich_verwerfungen: u64,
        /// Wie oft lief der Zaehler rueckwaerts (muss `0` sein).
        pub rueckwaerts: u64,
    }

    /// Einen Zeigerplatz in Datei/Zeile aufloesen.
    #[cfg(feature = "sperrwacht")]
    fn ort(p: &AtomicPtr<Location<'static>>) -> (&'static str, u32) {
        let raw = p.load(Ordering::Relaxed);
        if raw.is_null() {
            return ("", 0);
        }
        // SAFETY: in `STELLE`/`STELLE_HOCHLAUF` landet ausschliesslich ein `&'static
        // Location<'static>` aus `Location::caller()` -- ein Zeiger auf statische Daten, der die
        // ganze Laufzeit gueltig bleibt. Der Zeiger wird als GANZES atomar geschrieben und
        // gelesen; es gibt kein Paar, das zerreissen koennte (genau deshalb ein `AtomicPtr` und
        // nicht ein `str`-Zeiger nebst Laenge in zwei Zellen).
        let l: &'static Location<'static> = unsafe { &*(raw as *const Location<'static>) };
        (l.file(), l.line())
    }

    /// Den Messstand lesen.
    #[cfg(feature = "sperrwacht")]
    pub fn stand() -> Stand {
        let (datei, zeile) = ort(&STELLE);
        let (datei_bereinigt, zeile_bereinigt) = ort(&STELLE_BEREINIGT);
        let (datei_hochlauf, zeile_hochlauf) = ort(&STELLE_HOCHLAUF);
        let gesamt = GESEHEN.load(Ordering::Relaxed);
        Stand {
            gesehen: gesamt.saturating_sub(GESEHEN_BASIS.load(Ordering::Relaxed)),
            gesehen_gesamt: gesamt,
            max: MAX.load(Ordering::Relaxed),
            datei,
            zeile,
            max_bereinigt: MAX_BEREINIGT.load(Ordering::Relaxed),
            datei_bereinigt,
            zeile_bereinigt,
            max_hochlauf: MAX_HOCHLAUF.load(Ordering::Relaxed),
            datei_hochlauf,
            zeile_hochlauf,
            hochlauf_abschluesse: HOCHLAUF_ABSCHLUESSE.load(Ordering::Relaxed),
            eich_verwerfungen: EICH_VERWERFUNGEN.load(Ordering::Relaxed),
            rueckwaerts: RUECKWAERTS.load(Ordering::Relaxed),
        }
    }

    /// Ohne Feature: alles `0`/leer. Das Urteil im Kernel faellt darueber durch
    /// ([`MESSUNG_VORHANDEN`] ist `false`), statt „alles kurz" zu melden.
    #[cfg(not(feature = "sperrwacht"))]
    pub fn stand() -> Stand {
        Stand {
            gesehen: 0,
            gesehen_gesamt: 0,
            max: 0,
            datei: "",
            zeile: 0,
            max_bereinigt: 0,
            datei_bereinigt: "",
            zeile_bereinigt: 0,
            max_hochlauf: 0,
            datei_hochlauf: "",
            zeile_hochlauf: 0,
            hochlauf_abschluesse: 0,
            eich_verwerfungen: 0,
            rueckwaerts: 0,
        }
    }

    // ------------------------------------------------------------------------------------------
    // DIE ZWEI SCHNITTE -- beide vom MESSENDEN, beide gezaehlt, beide mit eigenem Namen
    // ------------------------------------------------------------------------------------------
    //
    // **Warum zwei Funktionen und nicht eine mit einem Schalter.** `zuruecksetzen(retten: bool)`
    // waere ein waehlbares Argument an einer Stelle, an der die Wahl die Bedeutung traegt -- genau
    // die Form, die `Va::identity(reason, pa)` gekostet hat. Zwei Namen sind nicht verwechselbar.
    //
    // **Warum das kein „Marke im gemessenen Pfad loeschen" ist:** geschnitten wird vom MESSENDEN
    // (der Eichung), nicht vom gemessenen Pfad -- genau die Trennung, an der `MANGEL_VERGIFTET`
    // gescheitert ist. Beide Schnitte tragen eine Ratsche; das Urteil im Kernel verlangt von
    // jedem exakt `1`.

    /// **Den Hochlauf abschliessen** -- der bisherige Hoechststand wird nach `MAX_HOCHLAUF`
    /// **gerettet, nicht verworfen**, und der Live-Kanal faengt sauber an.
    ///
    /// Ohne diesen Schnitt muesste die Eichung ihre kuenstliche Haltung gegen einen unbekannten
    /// Vorstand messen; mit ihm ist der Vorstand `0` und die Aussage „genau diese Dauer wurde
    /// gemeldet" ueberhaupt formulierbar. Was der Hochlauf gehalten hat, bleibt im Bericht
    /// sichtbar und geht in das Urteil ein -- es ist der Abschnitt mit den laengsten Sperren.
    #[cfg(feature = "sperrwacht")]
    pub fn hochlauf_abschliessen() {
        MAX_HOCHLAUF.store(MAX.load(Ordering::Relaxed), Ordering::Relaxed);
        STELLE_HOCHLAUF.store(STELLE.load(Ordering::Relaxed), Ordering::Relaxed);
        MAX.store(0, Ordering::Relaxed);
        STELLE.store(core::ptr::null_mut(), Ordering::Relaxed);
        MAX_BEREINIGT.store(0, Ordering::Relaxed);
        STELLE_BEREINIGT.store(core::ptr::null_mut(), Ordering::Relaxed);
        GESEHEN_BASIS.store(GESEHEN.load(Ordering::Relaxed), Ordering::Relaxed);
        HOCHLAUF_ABSCHLUESSE.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(not(feature = "sperrwacht"))]
    pub fn hochlauf_abschliessen() {}

    /// **Das Artefakt der Eichung verwerfen** und seinen Wert zurueckgeben.
    ///
    /// Die Eichung haelt eine Sperre fuer eine bekannte, absichtlich lange Dauer. Bliebe sie
    /// stehen, nennte die Berichtszeile auf Dauer die Eichung selbst als laengsten Halter -- eine
    /// Adresse, die niemandem hilft, und eine Zahl, die kein Befund ist.
    ///
    /// Verworfen wird **nicht stillschweigend**: der Rueckgabewert steht im Bericht, damit
    /// nachlesbar bleibt, was das Messgeraet an sich selbst gemessen hat.
    #[cfg(feature = "sperrwacht")]
    pub fn eichung_verwerfen() -> u64 {
        let v = MAX.load(Ordering::Relaxed);
        MAX.store(0, Ordering::Relaxed);
        STELLE.store(core::ptr::null_mut(), Ordering::Relaxed);
        MAX_BEREINIGT.store(0, Ordering::Relaxed);
        STELLE_BEREINIGT.store(core::ptr::null_mut(), Ordering::Relaxed);
        GESEHEN_BASIS.store(GESEHEN.load(Ordering::Relaxed), Ordering::Relaxed);
        EICH_VERWERFUNGEN.fetch_add(1, Ordering::Relaxed);
        v
    }

    #[cfg(not(feature = "sperrwacht"))]
    pub fn eichung_verwerfen() -> u64 {
        0
    }
}

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
    ///
    /// `#[track_caller]` (nur mit `sperrwacht`) reicht die **Stelle** dieses Aufrufs an die
    /// Sperrhaltedauer-Marke durch — ohne sie waere ihr Hoechststand eine Zahl ohne Adresse.
    #[cfg_attr(feature = "sperrwacht", track_caller)]
    pub fn lock(&self) -> SpinGuard<'_, T> {
        // IRQ-sicher: IRQs am eigenen Kern maskieren, BEVOR ein Ticket gezogen wird (sonst kann der
        // Timer-Tick/Reschedule denselben Lock reentrant ziehen -> Ticket-Deadlock; s. o.).
        let daif = irq_save_disable();
        // Die Uhr laeuft ab HIER, also einschliesslich der Wartezeit auf das Ticket: wer auf einen
        // fremden Halter wartet, ist genauso lange nicht praemptierbar wie der Halter selbst.
        let wacht = sperrwacht::beginn();
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.now_serving.load(Ordering::Acquire) != ticket {
            spin_hint();
        }
        SpinGuard {
            lock: self,
            daif,
            wacht,
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
    /// Anfangsstempel + Stelle der Sperrhaltedauer-Marke. Ohne `sperrwacht` ein ZST.
    wacht: sperrwacht::Wacht,
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
        // Die Uhr endet VOR `irq_restore` — gemessen wird die maskierte Zeit, nicht die Haltezeit.
        sperrwacht::ende(self.wacht);
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
    #[cfg_attr(feature = "sperrwacht", track_caller)]
    pub fn read(&self) -> RwReadGuard<'_, T> {
        // IRQs VOR der Anmeldung maskieren (s. Typ-Doku): ein Halter darf nicht verdrängt
        // werden, sonst spinnt ein Syscall auf demselben Kern für immer.
        let daif = irq_save_disable();
        let wacht = sperrwacht::beginn();
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
                    wacht,
                    #[cfg(loom)]
                    ptr: Some(self.data.get()),
                };
            }
            // Ein Schreiber kam dazwischen -> Eintrag zurücknehmen und erneut versuchen.
            self.state.fetch_sub(1, Ordering::Release);
        }
    }

    /// Exklusiv (schreibend) sperren. IRQ-sicher wie [`read`](Self::read).
    #[cfg_attr(feature = "sperrwacht", track_caller)]
    pub fn write(&self) -> RwWriteGuard<'_, T> {
        let daif = irq_save_disable();
        let wacht = sperrwacht::beginn();
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
                    wacht,
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
    /// Anfangsstempel + Stelle der Sperrhaltedauer-Marke. Ohne `sperrwacht` ein ZST.
    wacht: sperrwacht::Wacht,
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
        sperrwacht::ende(self.wacht); // s. `SpinGuard::drop`
        irq_restore(self.daif); // erst freigeben, DANN den IRQ-Zustand zurück
    }
}

/// RAII-Guard für exklusiven Schreibzugriff (`Deref` + `DerefMut`).
pub struct RwWriteGuard<'a, T: ?Sized> {
    lock: &'a RwSpinLock<T>,
    /// DAIF-Zustand vor dem Locken (beim `Drop` wiederhergestellt).
    daif: u64,
    /// Anfangsstempel + Stelle der Sperrhaltedauer-Marke. Ohne `sperrwacht` ein ZST.
    wacht: sperrwacht::Wacht,
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
        sperrwacht::ende(self.wacht); // s. `SpinGuard::drop`
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
