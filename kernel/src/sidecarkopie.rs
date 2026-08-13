//! **Die Nutzlast: Frame ↔ Sidecar** (Z26/A3, 2026-08-13).
//!
//! Bis heute stellte `zustellen()` die **Nachricht** zu — welcher Gast, welcher Slot, welcher
//! Anlass — und sonst nichts. Der Handler erfuhr *dass* und *wer*, aber nicht *was*. Ein Primitiv,
//! das den Frame nicht überträgt, ist richtig und **nicht benutzbar**.
//!
//! ## Warum diese Datei im Kernel liegt und nicht in `caprock-sched`/`caprock-microkit`
//!
//! Sie ist die einzige Stelle, an der drei Dinge zusammenkommen, die sonst getrennt bleiben:
//!
//! * die **Physadresse** des Fensters — der Kernel erreicht sie über die Identitätskarte, und
//!   diese Sicht hat nur er (GiB 0 ist identisch abgebildet, `PdMappable`-Speicher liegt dort);
//! * der **architekturabhängige** Frame (`caprock_hal::exception`);
//! * das **Format** des Slots (`caprock_sched::redirect`, abhängigkeitsfrei und host-getestet).
//!
//! `caprock-microkit` weiss nichts von Physadressen, `caprock-sched` nichts von der Speicherkarte,
//! und `redirect.rs` darf keine Abhängigkeit haben, weil ihre Tests ohne Maschine laufen müssen.
//! Die Klammer gehört also hierhin — und sie ist **dünn**: zwei Funktionen, keine Entscheidung.
//! Entschieden wird in `redirect.rs` ([`redirect::uebernehmbar`], [`redirect::slot_gueltig`]),
//! damit die Entscheidung mit Literalen widerlegbar bleibt.
//!
//! ## Die Reihenfolge beim Ablegen ist die halbe Zusicherung
//!
//! Erst der Rumpf, **dann** die Kennung. Ein Handler, der [`redirect::MAGIE`] sieht, muss den
//! Frame schon sehen können; andersherum läse er einen halben Frame und hielte ihn für einen
//! ganzen. Dieselbe Ordnung wie überall dort, wo eine Marke eine Nutzlast freigibt — und dieselbe
//! Klasse Fehler wie „`used` gehört dem Gerät" bei der wiederverwendeten Virtqueue.
//!
//! ## Lesen und Schreiben sind zwei Autoritäten, nicht eine
//!
//! [`ablegen`] schreibt den **ganzen** Frame ins Fenster; [`uebernehmen`] holt nur die
//! Allzweckregister zurück. Der Filter ist [`redirect::uebernehmbar`], er steht in der
//! abhängigkeitsfreien Datei und hat dort einen Test, der die Ring-Wörter **namentlich** sperrt.
//! Der Grund in einem Satz: die Quelle ist Speicher einer Persönlichkeits-PD, und `cs`/`ss`
//! (x86) bzw. `spsr` (aarch64) tragen den Ring. Ein Handler, der sie zurückschreiben dürfte,
//! beförderte seinen Gast — die Rechteausweitung, gegen die die Weiche steht, von der anderen
//! Seite.

use caprock_hal::exception as hal_exception;
use caprock_sched::redirect;
use core::sync::atomic::{AtomicU64, Ordering};

/// **Zustellungszähler** — Quelle von [`redirect::KOPF_GEN`].
///
/// Global und monoton, nicht je Slot: ein Handler bedient mehrere Gäste, und „diese Zahl habe ich
/// noch nie gesehen" ist die Aussage, die er braucht. Eine Zahl je Slot wäre dieselbe Aussage mit
/// mehr Zustand.
///
/// **Warum es diese Zahl überhaupt gibt:** ohne sie ist eine *ausgebliebene* Zustellung von einer
/// *wiederholten* nicht zu unterscheiden — der Slot trägt ja noch den Frame von vorhin. Genau die
/// Verwechslung, die dieses Projekt als `rx_used` gegen „Daten sind angekommen" bezahlt hat, und
/// als „ein reproduzierbarer Wert kann nicht belegen, dass er geerbt wurde" ein zweites Mal.
static ZUSTELLUNGEN: AtomicU64 = AtomicU64::new(0);

/// Wie oft [`ablegen`] den Frame **nicht** ablegen konnte. Ein Zähler und kein Schweigen: ohne ihn
/// wäre ein Fenster, das nie beschrieben wird, von einem Gast ohne Syscalls nicht zu
/// unterscheiden.
static ABLAGE_FEHLER: AtomicU64 = AtomicU64::new(0);
/// Wie oft [`uebernehmen`] nichts zurückschreiben konnte (kein Frame im Slot, falsche Breite).
static UEBERNAHME_FEHLER: AtomicU64 = AtomicU64::new(0);

/// `(Zustellungen, Ablagefehler, Übernahmefehler)` — für die Prüfzeile.
pub fn bilanz() -> (u64, u64, u64) {
    (
        ZUSTELLUNGEN.load(Ordering::Relaxed),
        ABLAGE_FEHLER.load(Ordering::Relaxed),
        UEBERNAHME_FEHLER.load(Ordering::Relaxed),
    )
}

/// Adresse des Slots `slot` im Fenster `sidecar`, **oder `None`**.
///
/// Die Schranke ist [`redirect::slot_gueltig`] gegen [`caprock_microkit::SIDECAR_SLOTS`] — die
/// Breite der Belegungsmaske. Beide Zahlen an **einer** Stelle gegeneinander zu halten ist der
/// Punkt: ein Slot daneben heisst hier nicht „Absturz", sondern **ein Gast schreibt in den Frame
/// eines anderen**.
///
/// Dass das Fenster gross genug für alle Slots ist, wird **beim Prägen der Cap** entschieden
/// (`system::install_syscall_handler_cap` -> [`redirect::fenster_deckt`]) und nicht hier: eine
/// Schranke gehört an die Vergabe, nicht an den Zugriff.
fn slot_adresse(sidecar: u64, slot: u16) -> Option<u64> {
    if sidecar == 0 || !redirect::slot_gueltig(slot, caprock_microkit::SIDECAR_SLOTS) {
        return None;
    }
    Some(sidecar + redirect::slot_offset(slot) as u64)
}

/// **Den Trap-Frame des Gastes in seinen Sidecar-Slot legen.**
///
/// Gibt `false` zurück, wenn der Slot nicht adressierbar ist oder der Frame nicht hinter den Kopf
/// passt — und dann wird **nichts** geschrieben. Ein halb beschriebener Slot wäre schlimmer als
/// ein leerer: er trüge die Kennung nicht, aber alte Daten mit neuer Länge.
pub fn ablegen(frame: usize, sidecar: u64, slot: u16, anlass: u64, code: u64) -> bool {
    let Some(basis) = slot_adresse(sidecar, slot) else {
        ABLAGE_FEHLER.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    let mut w = [0u64; hal_exception::FRAME_WOERTER];
    let n = hal_exception::frame_woerter(frame, &mut w);
    if n == 0 || !redirect::frame_passt(n) {
        ABLAGE_FEHLER.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    // Monoton, und **vor** dem Schreiben geholt: zwei Kerne, die gleichzeitig zustellen, bekommen
    // verschiedene Zahlen, und jeder schreibt in seinen eigenen Slot.
    let gen = ZUSTELLUNGEN.fetch_add(1, Ordering::Relaxed) + 1;
    let p = basis as *mut u64;
    // SAFETY: `basis` liegt im Sidecar-Fenster der Handler-PD. Das Fenster ist eine vom Kernel
    // allozierte, identisch abgebildete RAM-Region (`Zone::PdMappable`, also GiB 0); `slot_adresse`
    // hält den Index innerhalb der Maskenbreite, und dass das Fenster alle Slots deckt, ist beim
    // Prägen der Cap entschieden. Geschrieben werden ausschliesslich Wörter unterhalb von
    // `SLOT_WOERTER`.
    unsafe {
        // 1. Der RUMPF: Kopffelder ohne die Kennung, dann der Frame.
        p.add(redirect::KOPF_VERSION)
            .write_volatile(redirect::FORMAT_VERSION);
        p.add(redirect::KOPF_GEN).write_volatile(gen);
        p.add(redirect::KOPF_ANLASS).write_volatile(anlass);
        p.add(redirect::KOPF_CODE).write_volatile(code);
        p.add(redirect::KOPF_NGPR)
            .write_volatile(hal_exception::FRAME_GPR as u64);
        p.add(redirect::KOPF_NGESAMT).write_volatile(n as u64);
        p.add(redirect::KOPF_ARCH)
            .write_volatile(hal_exception::FRAME_ARCH);
        // **Die Gruppenkennung wird GESCHRIEBEN, nicht ausgelassen** — mit `0` = „keine Angabe".
        // Ein Feld, das nur beim ersten Mal genullt ist, trägt beim zweiten Mal den Wert von
        // vorhin: Slots werden wiederverwendet, und das ist genau die Form „nur den Treiberteil
        // der Virtqueue zu nullen reicht nicht".
        p.add(redirect::KOPF_GRUPPE).write_volatile(0);
        for i in 0..redirect::KOPF_RESERVIERT_N {
            p.add(redirect::KOPF_RESERVIERT + i).write_volatile(0);
        }
        for (i, x) in hal_exception::FRAME_ABI_WORT.iter().enumerate() {
            p.add(redirect::KOPF_ABI + i).write_volatile(*x);
        }
        for i in 0..n {
            p.add(redirect::frame_wort(i)).write_volatile(w[i]);
        }
        // 2. Erst jetzt die KENNUNG. Vorher wäre sie ein Versprechen auf Daten, die noch nicht da
        //    sind — und der Handler läuft möglicherweise auf einem anderen Kern.
        core::sync::atomic::fence(Ordering::Release);
        p.add(redirect::KOPF_MAGIE).write_volatile(redirect::MAGIE);
    }
    true
}

/// Ausgang von [`uebernehmen`] — **jede Absage mit eigenem Namen**.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Uebernahme {
    /// Die Allzweckregister stehen im Frame des Gastes.
    Ok,
    /// Der Slot ist nicht adressierbar (kein Fenster, Slot ausserhalb der Maske).
    KeinFenster,
    /// Im Slot steht **kein** Frame ([`redirect::MAGIE`] fehlt). Der Frame des Gastes bleibt, wie
    /// der IPC-Transport ihn hinterlassen hat — daran ist die Gegenprobe „das Ergebnis wird nicht
    /// zurückgeschrieben" ablesbar.
    KeinFrame,
    /// Der Kopf ist da, aber **nicht lesbar** (fremde Version/Architektur/Breite, gesetztes
    /// reserviertes Feld). Der Gast bekommt [`redirect::ERR_HANDLER_ABI`] — abgewiesen statt
    /// ausgelegt.
    FremderKopf(redirect::KopfUrteil),
}

/// **Das Ergebnis aus dem Sidecar-Slot in den Frame des Gastes zurück.**
///
/// Nur die Allzweckregister (s. [`redirect::uebernehmbar`]), und **erst nach der Kopfprüfung**
/// ([`redirect::kopf_pruefen`]). Die Reihenfolge ist der Punkt: die Quelle ist Speicher einer
/// Persönlichkeits-PD, und was hier gelesen wird, schreibt der Kernel unmittelbar als
/// **Registerinhalt** in einen laufenden Thread. Ein Leser, der bei fremder Version weiterliest,
/// legt fremde Bytes im eigenen Sinn aus — dieselbe Regel wie bei der Zustandsübergabe (A-4.3).
pub fn uebernehmen(frame: usize, sidecar: u64, slot: u16) -> Uebernahme {
    let Some(basis) = slot_adresse(sidecar, slot) else {
        UEBERNAHME_FEHLER.fetch_add(1, Ordering::Relaxed);
        return Uebernahme::KeinFenster;
    };
    let p = basis as *const u64;
    // Den KOPF am Stück lesen und dann prüfen — nicht Feld für Feld befragen. `kopf_pruefen` ist
    // abhängigkeitsfrei und host-getestet; hier soll keine zweite Fassung derselben Regel stehen.
    let mut kopf = [0u64; redirect::FRAME_WORT];
    for (i, k) in kopf.iter_mut().enumerate() {
        // SAFETY: wie in `ablegen`; `i < FRAME_WORT < SLOT_WOERTER`.
        *k = unsafe { p.add(i).read_volatile() };
    }
    core::sync::atomic::fence(Ordering::Acquire);
    let urteil = redirect::kopf_pruefen(
        &kopf,
        hal_exception::FRAME_ARCH,
        hal_exception::FRAME_GPR as u64,
        hal_exception::FRAME_WOERTER as u64,
    );
    match urteil {
        redirect::KopfUrteil::Ok => {}
        redirect::KopfUrteil::KeinFrame => {
            UEBERNAHME_FEHLER.fetch_add(1, Ordering::Relaxed);
            return Uebernahme::KeinFrame;
        }
        u => {
            UEBERNAHME_FEHLER.fetch_add(1, Ordering::Relaxed);
            return Uebernahme::FremderKopf(u);
        }
    }
    let mut w = [0u64; hal_exception::FRAME_GPR];
    for i in 0..hal_exception::FRAME_WOERTER {
        // **Der Filter steht hier, sichtbar, und nicht in der Schleifengrenze.** Eine Schleife, die
        // nur bis `FRAME_GPR` läuft, wäre dasselbe Verhalten und keine ablesbare Entscheidung —
        // und `uebernehmbar` hätte im Kernel keinen Aufrufer, so wie `slot_gueltig` und
        // `fenster_deckt` bis zum 2026-08-13 keinen hatten.
        if redirect::uebernehmbar(i, hal_exception::FRAME_GPR) {
            // SAFETY: wie oben; `i < FRAME_GPR == w.len()`.
            w[i] = unsafe { p.add(redirect::frame_wort(i)).read_volatile() };
        }
    }
    if hal_exception::frame_gpr_uebernehmen(frame, &w) == hal_exception::FRAME_GPR {
        Uebernahme::Ok
    } else {
        UEBERNAHME_FEHLER.fetch_add(1, Ordering::Relaxed);
        Uebernahme::KeinFrame
    }
}
