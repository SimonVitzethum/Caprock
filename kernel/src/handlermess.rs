//! **Die Prüfzeile `handler`** (Z26/A3, 2026-08-10) — misst das Kernel-Primitiv für umgeleitete
//! Syscalls an seiner **Wirkung**, nicht an seinen Bits.
//!
//! ## Was hier gemessen wird, und warum genau das
//!
//! Das Primitiv besteht aus drei Zusicherungen. Zwei davon lassen sich mit Literalen auf dem Host
//! prüfen (`crates/caprock-sched/src/redirect.rs`, 22 Tests + `tools/redirect-negativ.sh`); die
//! **dritte kann es nicht**, weil sie eine Aussage über den laufenden Scheduler ist:
//!
//! > **Kein fremder Wecker hebt den Handler-Grund auf.**
//!
//! Das ist die fünfte Instanz der Klasse aus Z24/D9, diesmal vorhergesagt statt gefunden: ein Gast
//! wartet auf seine Persönlichkeits-PD; `thaw`, `resume`, `unpark` oder `unblock` heben die
//! Blockade auf, und er läuft **mit halbem Syscall** weiter — mit einem Frame, den sein Kernel
//! noch nicht fertig geschrieben hat. Diese Zeile prüft, dass das **nicht** geht.
//!
//! ## Gemessen wird die WIRKUNG, nicht das Bit
//!
//! Die Sonde führt einen Rundenzähler. „Wurde fälschlich geweckt" heisst hier: **der Zähler
//! bewegt sich**. Ein Prüfer, der stattdessen `is_handler_blocked` vorher und nachher liest, misst
//! eine Größe, die auch dann gleich bleibt, wenn der Thread schon losgelaufen und wieder blockiert
//! ist — dieselbe Falle wie die erste Fassung der `park`-Zeile, die `is_parked` an einem
//! IPC-Wartenden las (dort ist das Bit in **beiden** Fällen falsch).
//!
//! ## Und die Sprechprobe
//!
//! Jede Aussage über Abwesenheit („niemand hat ihn geweckt") braucht daneben den Nachweis, dass
//! der Zähler sich überhaupt bewegen **kann**. Deshalb steht am Ende `laeuft-nach-reply`: nach
//! `handler_reply` **muss** er sich bewegen. Ohne diese Zeile wäre „bewegt sich nicht" von
//! „bewegt sich nie" nicht zu unterscheiden — ein toter Thread bestünde jede andere Aussage.

#![cfg(feature = "selftest")]

use crate::system;
use caprock_hal::{self as hal, println};
use caprock_sched::redirect::{self, BindUrteil};
use caprock_sched::ThreadId;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Rundenzähler der Sonde. **Die Messgröße** — nicht ein Bit, sondern Fortschritt.
static SONDE_RUNDEN: AtomicUsize = AtomicUsize::new(0);
/// `ThreadId` der Sonde, sobald sie zugelassen ist (`0` = noch keine).
static SONDE_TID: AtomicU64 = AtomicU64::new(0);
/// Ergebnis der einmaligen Messung: Bit 0 = alles bestanden, Bit 1 = gelaufen.
static MESS: AtomicUsize = AtomicUsize::new(0);

/// Die Sonde: zählt Runden und gibt ab. Sie tut **nichts** anderes — die Umleitung wird ihr von
/// aussen angetan, damit die Messung nicht davon abhängt, dass die Sonde selbst mitspielt.
extern "C" fn sonde(_arg: usize) -> ! {
    loop {
        SONDE_RUNDEN.fetch_add(1, Ordering::Release);
        hal::syscall::invoke(caprock_abi::sys::YIELD, 0, [0; 4], 0);
    }
}

/// Ergebnisbit für `all_done()`: hat die Messung **bestanden**?
pub fn urteil() -> bool {
    MESS.load(Ordering::Acquire) & 1 != 0
}

/// Ist die Messung überhaupt gelaufen? (Getrennt vom Urteil — „nicht gelaufen" ist kein
/// bestandener Test, und die beiden dürfen nicht gleich aussehen.)
pub fn gelaufen() -> bool {
    MESS.load(Ordering::Acquire) & 2 != 0
}

/// Ein paar Ticks warten (dieselbe Form wie im `park`-Prüfer).
fn warten() {
    let t0 = hal::timer::ticks(0);
    let mut wache = 0u64;
    while hal::timer::ticks(0) < t0 + 3 && wache < 200_000_000 {
        core::hint::spin_loop();
        wache += 1;
    }
}

/// Bewegt sich der Rundenzähler in einem Zeitfenster?
fn bewegt_sich() -> bool {
    let a = SONDE_RUNDEN.load(Ordering::Acquire);
    for _ in 0..8 {
        warten();
        if SONDE_RUNDEN.load(Ordering::Acquire) != a {
            return true;
        }
    }
    false
}

/// **Die Messung.** Läuft genau einmal; das Urteil steht danach in [`urteil`].
pub fn messen() {
    let ok = messen_inner();
    MESS.store(if ok { 3 } else { 2 }, Ordering::Release);
}

fn messen_inner() -> bool {
    // ------------------------------------------------------------------------------------
    // 0. Die ABI-Konstanten müssen ÜBEREINSTIMMEN — zwei Dateien, eine Zahl
    // ------------------------------------------------------------------------------------
    //
    // `redirect.rs` ist abhängigkeitsfrei (damit host-testbar) und trägt `SLOT_BYTES` und
    // `ERR_HANDLER_GONE` deshalb **noch einmal** als Literal. Zwei Zahlen, die dasselbe bedeuten
    // und getrennt gepflegt werden, laufen auseinander — genau die Form, gegen die A-3.4 die
    // Slices statt der Konstanten eingeführt hat. Hier ist die Klammer.
    let abi_stimmt = redirect::SLOT_BYTES as u64 == caprock_abi::redirect_msg::SLOT_BYTES
        && redirect::ERR_HANDLER_GONE == caprock_abi::result::ERR_HANDLER_GONE;

    // ------------------------------------------------------------------------------------
    // 1. Das Zyklusverbot — gegen die ECHTE PD-Tabelle, nicht gegen ein Modell davon
    // ------------------------------------------------------------------------------------
    //
    // Die Host-Tests prüfen `pruefe_bindung` gegen eine Kantentabelle aus Literalen. Das belegt
    // die Regel, nicht ihre **Verdrahtung**: dass der Kernel wirklich `handler_pd_of` der echten
    // Tabelle liest und die richtige Schrittschranke mitgibt, sieht nur eine Messung hier.
    //
    // Zwei PD-Ids, die es gibt (0 und 1 existieren immer -- der Root-Task und die erste geladene
    // PD; und selbst wenn nicht, sind sie im Graphen zulässige Knoten mit leerer Kante).
    let vorher_frei = system::sidecar_frei(1);
    // (a) frisch: zulässig
    let a_ok = system::pruefe_bindung(0, 1, true, false) == BindUrteil::Ok;
    // (b) auf sich selbst: abgewiesen
    let a_selbst = system::pruefe_bindung(0, 0, true, false) == BindUrteil::SelbstBindung;
    // (c) **der Zweierzyklus** — die Aussage aus Z26/Nachtrag 3. Kante 1 -> 0 setzen (PD 1 wird
    //     von PD 0 behandelt), dann 0 -> 1 verlangen: das schlösse den Kreis.
    let gesetzt = system::handler_kante_setzen(1, 0);
    let a_zyklus = system::pruefe_bindung(0, 1, true, false) == BindUrteil::Zyklus;
    // (d) und eine GESTAPELTE Persönlichkeit bleibt erlaubt: 2 -> 1 ist azyklisch.
    //     Ohne diese Zeile wäre ein Prüfer, der einfach alles abweist, grün.
    let a_stapel = system::pruefe_bindung(2, 1, true, false) == BindUrteil::Ok;
    let geloest = system::handler_kante_loesen(1);
    // (e) nach dem Lösen ist 0 -> 1 wieder zulässig. **Die Gegenrichtung**: eine Kante, die nach
    //     dem letzten Thread stehen bliebe, verböte für immer eine Bindung, die längst zulässig
    //     ist — ein Verbot aus einer Leiche, von einem echten nicht zu unterscheiden.
    let a_wieder = system::pruefe_bindung(0, 1, true, false) == BindUrteil::Ok;
    let zyklus_ok = a_ok && a_selbst && gesetzt && a_zyklus && a_stapel && geloest && a_wieder;

    // ------------------------------------------------------------------------------------
    // 2. Die Sidecar-Slots: einzeln vergeben, einzeln frei — und NIE zweimal derselbe
    // ------------------------------------------------------------------------------------
    //
    // Der Fehler, gegen den das steht, ist stumm: zwei Gäste im selben Slot, und der eine liest
    // den halben Syscall des anderen. Ein Zähler statt einer Maske hätte ihn.
    let s0 = system::sidecar_belegen(1);
    let s1 = system::sidecar_belegen(1);
    let verschieden = matches!((s0, s1), (Some(a), Some(b)) if a != b);
    if let Some(a) = s0 {
        system::sidecar_freigeben(1, a);
    }
    // Nach EINER Freigabe muss GENAU dieser Slot wieder kommen -- die Maske vergibt das kleinste
    // freie Bit, und das ist jetzt wieder `a`.
    let s2 = system::sidecar_belegen(1);
    let wiederverwendet = s2 == s0;
    if let Some(a) = s2 {
        system::sidecar_freigeben(1, a);
    }
    if let Some(b) = s1 {
        system::sidecar_freigeben(1, b);
    }
    let nachher_frei = system::sidecar_frei(1);
    // **Buchhaltung geht auf** -- die Zeile, die ein Leck fände.
    let slots_ok = verschieden && wiederverwendet && nachher_frei == vorher_frei;

    // ------------------------------------------------------------------------------------
    // 3. DIE HAUPTAUSSAGE: kein fremder Wecker hebt den Handler-Grund auf
    // ------------------------------------------------------------------------------------
    let raw = SONDE_TID.load(Ordering::Acquire);
    let Some(tid) = (raw != 0).then(|| ThreadId::from_raw(raw)) else {
        println!("handler : FAILURES (keine Sonde -- ohne sie ist nichts gemessen)");
        return false;
    };
    // **Positivkontrolle ZUERST**: läuft die Sonde überhaupt? Ohne sie bestünde jede folgende
    // Aussage auch ein toter Thread.
    let laeuft_vorher = bewegt_sich();

    // Den Handler-Grund anhängen (im Betrieb macht das der Dispatch beim Zustellen).
    let markiert = system::mark_handler_wait(tid);
    let steht = !bewegt_sich() && system::is_handler_blocked(tid);

    // (a) `unpark` -- der Wecker des PARK-Grundes. Er darf hier NICHTS tun.
    system::unpark_thread(tid);
    let nach_unpark = !bewegt_sich();
    // (b) `resume` -- der Wecker des PAUSE-Grundes. Ebenso.
    system::resume_thread(tid);
    let nach_resume = !bewegt_sich();
    // (c) Und der schärfste Fall, wörtlich der aus Z26/Nachtrag 3: **pausieren und wieder
    //     fortsetzen**, während der Handler-Grund steht. `pause` fügt `PAUSE` hinzu, `resume`
    //     nimmt `PAUSE` weg -- `HANDLER` bleibt, die Menge ist nicht leer, er läuft nicht.
    //     Mit einem einzelnen Bit statt der Menge liefe er hier los.
    system::pause_thread(tid);
    system::resume_thread(tid);
    let nach_thaw = !bewegt_sich();

    // (d) **Die Sprechprobe**: der EINE zuständige Wecker muss wirken. Ohne diese Zeile ist
    //     „bewegt sich nicht" von „bewegt sich nie" nicht zu unterscheiden.
    let geantwortet = system::handler_reply(tid);
    let laeuft_nach_reply = bewegt_sich();

    let bits = system::reasons_bits(tid).unwrap_or(0xff);
    let gebunden = system::handler_bound_count(hal::cpu::core_id());

    let alles = abi_stimmt
        && zyklus_ok
        && slots_ok
        && laeuft_vorher
        && markiert
        && steht
        && nach_unpark
        && nach_resume
        && nach_thaw
        && geantwortet
        && laeuft_nach_reply;

    println!(
        "handler : {} abi-stimmt={abi_stimmt} zyklusverbot={zyklus_ok} slots={slots_ok}",
        if alles { "ALL PASS" } else { "FAILURES" }
    );
    println!(
        "handler : laeuft-vorher={laeuft_vorher} markiert={markiert} steht={steht} \
         nach-unpark={nach_unpark} nach-resume={nach_resume} nach-pause-resume={nach_thaw} \
         laeuft-nach-reply={laeuft_nach_reply}"
    );
    println!(
        "handler : Urteile{:?} gebunden={gebunden} restgrund={bits:#04x} \
         fault-umgeleitet={} runden={}",
        caprock_microkit::handler_urteile(),
        system::handler_fault_count(),
        SONDE_RUNDEN.load(Ordering::Acquire)
    );
    alles
}

/// Die Sonde starten. Muss **vor** [`messen`] laufen (der Thread braucht Zeit, überhaupt
/// anzulaufen).
pub fn sonde_starten() {
    if let Some(p) = system::spawn_parked(sonde as *const () as usize, 0, system::IDLE_PRIO) {
        if let Some(tid) = system::admit(p) {
            SONDE_TID.store(tid.to_raw(), Ordering::Release);
        }
    }
}
