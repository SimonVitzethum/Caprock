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
use caprock_abi::{result, sys};
use caprock_hal::{self as hal, println, syscall::invoke};
use caprock_mem::Rights;
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

/// [`messen`], aber nur beim ersten Aufruf. Der Hochlauf ruft sie aus seiner Warteschleife —
/// eine Messung, die je Umdrehung liefe, druckte je Umdrehung und pausierte je Umdrehung eine
/// fremde Sonde.
pub fn messen_einmal() {
    if MESS.load(Ordering::Acquire) & 2 == 0 {
        messen();
    }
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
        && redirect::ERR_HANDLER_GONE == caprock_abi::result::ERR_HANDLER_GONE
        && redirect::ERR_HANDLER_ABI == caprock_abi::result::ERR_HANDLER_ABI
        // **Und die Klammer um die NUTZLAST** (2026-08-13). Die Frame-Wortzahlen stehen in der
        // HAL, die Slot-Arithmetik in der abhängigkeitsfreien Datei; laufen sie auseinander,
        // schreibt der Kernel über die Slotkante oder liest zu wenig zurück. Beides ist stumm.
        && redirect::frame_passt(hal::exception::FRAME_WOERTER)
        && hal::exception::FRAME_GPR < hal::exception::FRAME_WOERTER
        && hal::exception::FRAME_ABI_WORT.len() == redirect::KOPF_ABI_N
        // **Beide Kennungen zulassen, nicht die eigene festschreiben.** Diese Datei ist
        // arch-neutral; ein `== ARCH_X86_64` waere auf aarch64 eine Aussage, die dort NIE gelten
        // kann -- ein Kriterium, das die geprueffte Sache nicht erreichen kann, ist keins.
        // Geprueft wird, dass die HAL eine der beiden BENANNTEN Kennungen fuehrt.
        && (hal::exception::FRAME_ARCH == redirect::ARCH_X86_64
            || hal::exception::FRAME_ARCH == redirect::ARCH_AARCH64)
        // Jeder ABI-Registerindex liegt in den ÜBERNEHMBAREN Wörtern. Läge einer dahinter, könnte
        // der Handler das Ergebnisregister zwar schreiben, und es käme nie beim Gast an.
        && hal::exception::FRAME_ABI_WORT
            .iter()
            .all(|&i| redirect::uebernehmbar(i as usize, hal::exception::FRAME_GPR));

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

// ================================================================================================
// DIE NUTZLAST: EIN ECHTER UMGELEITETER SYSCALL (Z26/A3, 2026-08-13)
// ================================================================================================
//
// Die Zeile `handler` oben misst das Primitiv über den **Kernel-Prüfpfad**: `mark_handler_wait`
// und `handler_reply` von Hand, an einer Sonde ohne Bindung. Das war bis heute die einzige
// Möglichkeit, denn **kein Pfad prägte eine Handler-Cap** — `SYS_SETHANDLER` konnte nie
// erfolgreich sein, und ohne Bindung gibt es keine Umleitung.
//
// Diese Zeile misst etwas anderes, und sie misst es an der **Wirkung**:
//
//   Ein Gast setzt einen Syscall ab, dessen Nummer der Caprock-Kernel **nicht kennt** (39, in
//   Linux `getpid`). Der Kernel leitet ihn um. Ein Handler-Thread in einer **eigenen PD** liest
//   den Trap-Frame aus dem Sidecar, rechnet aus dem gelesenen Argument die Antwort und schreibt
//   sie in das Wort, das die ABI-Indextabelle als `x0` ausweist. Der Gast läuft weiter und
//   **sieht das Ergebnis**.
//
// ## Warum die Antwort BERECHNET wird und keine Konstante ist
//
// `antwort = argument + 1`. Eine feste Zahl könnte der Handler auch dann melden, wenn er den
// Frame gar nicht gelesen hat — und dann bewiese die Zeile nur, dass jemand geantwortet hat.
// Genau die Unterscheidung `rx_used` gegen „Daten sind angekommen", die dieses Projekt schon
// zweimal bezahlt hat (die zweite Fassung war Z4 Stufe 2: ein reproduzierbarer Wert kann nicht
// belegen, dass er geerbt wurde).
//
// ## Und warum die Antwort NICHT über die Nachricht kommen kann
//
// Der Handler antwortet mit einem **Köder** in seinem Nachrichtenwort `x2`. Käme das Ergebnis
// über den IPC-Transport, fände der Gast den Köder in `msg[0]` vor. Er findet dort statt dessen
// seinen **eigenen** Wert wieder — weil das Zurückschreiben aus dem Sidecar den Transport
// überschreibt. Ohne diesen Köder wäre „der Gast sieht ein Ergebnis" auch dann wahr, wenn das
// Sidecar gar nicht benutzt würde: `Endpoint::reply` setzt `x0` des Aufrufers ohnehin auf `OK`.
//
// ## Was diese Zeile NICHT belegt
//
// Der Gast ist ein **Kernel-Thread** (Ring 0 / EL1), kein EL0-Programm. Der Trap-Pfad ist
// derselbe (`int 0x80` legt denselben Frame an, `iretq` stellt ihn wieder her) und die
// zurückgeschriebenen Wörter sind dieselben — aber „ein EL0-Gast wird umgeleitet" ist damit
// **nicht** gemessen, sondern erschlossen. Das steht hier, weil ein erschlossener Fall in diesem
// Projekt schon dreimal ein ungeprüfter war.

/// Syscall-Nummer, die der Gast absetzt. **Der Caprock-Kernel kennt sie nicht** (`sys::` reicht
/// bis 19); nativ bearbeitet ergäbe sie `ERR_BADCAP`. Dass der Gast trotzdem eine sinnvolle
/// Antwort bekommt, ist die Aussage: die Nummer bedeutet etwas in der **Persönlichkeit**, nicht
/// im Kernel. 39 ist `getpid` in der Linux-x86_64-ABI.
const GAST_SYSNO: u64 = 39;
/// Das Argument, das der Gast in `x1` mitgibt. Der Handler muss es **im Sidecar** vorfinden.
const GAST_ARG: u64 = 0x00C0_FFEE;
/// Der Wert, den der Gast in `x2` mitgibt und nach dem Syscall **unverändert** wiederfinden muss.
const GAST_X2: u64 = 0x0000_5EED;
/// Der Köder, den der Handler in sein Antwort-Nachrichtenwort legt. Er darf den Gast **nie**
/// erreichen — der Sidecar-Rückschrieb überschreibt den Transport.
const KOEDER: u64 = 0x00BA_DBAD;

/// Endpoint-Id + 1 (`0` = nicht angelegt).
static R_EP: AtomicU64 = AtomicU64::new(0);
/// Handler-PD + 1.
static R_HPD: AtomicU64 = AtomicU64::new(0);
/// Gast-PD + 1.
static R_GPD: AtomicU64 = AtomicU64::new(0);
/// Physische Basis des Sidecar-Fensters (`0` = keins).
static R_SIDECAR: AtomicU64 = AtomicU64::new(0);
/// Rohe `ThreadId` des Gastes.
static R_GAST_TID: AtomicU64 = AtomicU64::new(0);
/// **Die Startfreigabe.** `0` = warten · `1` = erste Runde · `2` = zweite Runde (fail-closed).
///
/// Der Gast darf **vor** der Bindung keinen Syscall absetzen: ungebunden liefe er nativ, bekäme
/// `ERR_BADCAP` und die Zeile mässe die Reihenfolge zweier Ereignisse statt der Eigenschaft
/// (dieselbe Lehre wie beim `qgate`-Aufbau).
static R_START: AtomicU64 = AtomicU64::new(0);
/// Was der Gast in `x0` zurückbekam (`u64::MAX` = noch nichts).
static R_GAST_X0: AtomicU64 = AtomicU64::new(u64::MAX);
/// Was der Gast in `x2` zurückbekam.
static R_GAST_MSG0: AtomicU64 = AtomicU64::new(u64::MAX);
/// `1`, sobald der Gast aus dem ersten umgeleiteten Syscall zurück ist.
static R_GAST_FERTIG: AtomicU64 = AtomicU64::new(0);
/// `1`, falls der Gast aus dem **zweiten** Syscall zurückkam. Muss `0` bleiben: da ist die
/// Handler-PD stillgelegt, und die Fail-closed-Regel sagt „faulten, nicht nativ weiterlaufen".
static R_GAST_ZWEITES: AtomicU64 = AtomicU64::new(0);
/// Rundenzähler des Gastes zwischen den beiden Syscalls — die Größe, an der „läuft" ablesbar ist.
static R_GAST_RUNDEN: AtomicU64 = AtomicU64::new(0);
/// Ergebniscode von `SYS_SETHANDLER` (`u64::MAX` = nicht gelaufen).
static R_BIND_CODE: AtomicU64 = AtomicU64::new(u64::MAX);
/// Basis, die der Handler aus `SYS_MAP` auf seine Fenster-Cap bekommen hat.
static R_H_BASIS: AtomicU64 = AtomicU64::new(0);
/// Ergebniscode des `RECV` des Handlers (`u64::MAX` = noch keins).
static R_H_RECV: AtomicU64 = AtomicU64::new(u64::MAX);
/// **Was der Handler im Slot vorgefunden hat** — ein Bitfeld, damit im Fehlerfall ablesbar ist,
/// *welche* Aussage gefallen ist.
///
/// Bit 0 Kopf lesbar · 1 Zähler frisch · 2 Anlass = Syscall · 3 Nummer stimmt · 4 Argument stimmt.
static R_H_SAH: AtomicU64 = AtomicU64::new(0);
/// Wie oft der Handler eine Umleitung bedient hat.
static R_H_BEDIENT: AtomicU64 = AtomicU64::new(0);
/// Ergebnis der einmaligen Messung: Bit 0 = bestanden, Bit 1 = gelaufen.
static R_MESS: AtomicUsize = AtomicUsize::new(0);

/// Ergebnisbit für `all_done()`.
pub fn redirect_urteil() -> bool {
    R_MESS.load(Ordering::Acquire) & 1 != 0
}

/// **Der GAST.** Er tut nichts als einen Syscall abzusetzen, dessen Nummer der Kernel nicht kennt.
///
/// Er weiss von der Umleitung nichts — sie wird ihm von aussen angetan. Das ist der Punkt: eine
/// Persönlichkeit ist etwas, das man einem Programm **antut**, nicht etwas, woran es mitwirkt.
extern "C" fn gast(_arg: usize) -> ! {
    while R_START.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    let r = invoke(GAST_SYSNO, GAST_ARG, [GAST_X2, 0, 0, 0], 0);
    R_GAST_X0.store(r.result, Ordering::Release);
    R_GAST_MSG0.store(r.msg[0], Ordering::Release);
    R_GAST_FERTIG.store(1, Ordering::Release);
    // Zwischenrunde: der Zähler ist die Positivkontrolle für den Fail-closed-Teil. Ohne ihn wäre
    // „er steht" von „er hat nie gelaufen" nicht zu unterscheiden.
    while R_START.load(Ordering::Acquire) < 2 {
        R_GAST_RUNDEN.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
    // **Der zweite Syscall — mit stillgelegter Handler-PD.** Er darf NICHT zurückkehren: fällt der
    // Handler weg, faultet der Gast, statt auf die native ABI zurückzufallen. Käme er zurück,
    // hätte der Entzug einer Cap ihn **befördert**.
    let _ = invoke(GAST_SYSNO, GAST_ARG, [GAST_X2, 0, 0, 0], 0);
    R_GAST_ZWEITES.store(1, Ordering::Release);
    loop {
        R_GAST_RUNDEN.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
}

/// **Der HANDLER** — eine Persönlichkeits-PD in Miniatur.
///
/// Bedient wird über `RECV`/`REPLY` auf der **`SyscallHandler`-Cap** (Slot 0) — den beiden
/// Operationen, die der Dispatch auf einer Handler-Cap zulässt (`CALL` weist er ab, sonst gäbe
/// ein Handler sich als Gast aus und stellte sich selbst eine Umleitung zu).
///
/// ## Woher er sein Fenster kennt — und was daran GEMESSEN ist und was nicht
///
/// Die Autorität über das Fenster ist **cap-förmig**: in Slot 1 der Handler-PD steht eine
/// `Memory`-Cap über genau diese Region, und die Messung prüft das am **PD-Tabellen-Audit**
/// (`fenster-in-handler-pd`), nicht an einer Zusage.
///
/// Die **Adresse** bekommt er dagegen beim Spawn mitgegeben, und der Grund ist ein Befund:
/// `SYS_MAP` verlangt eine **eigene VSpace** (`map_frame` gibt `false` bei `asid == 0`). Diese
/// Handler-PD teilt sich den globalen Adressraum, dort ist das Fenster **von selbst** sichtbar,
/// und ein Mapping-Schritt wäre gegenstandslos. Der Todo-Eintrag „das Fenster wird nirgends
/// angelegt **und in die Handler-PD gemappt**" wirft damit zwei Fälle zusammen: das *Anlegen*
/// und das *Prägen der Cap* fehlten wirklich (und stehen jetzt), das *Mappen* ist nur für eine
/// **isolierte** Handler-PD überhaupt eine Handlung. **Diese Zeile misst den isolierten Fall
/// NICHT** — das steht hier, weil ein erschlossener Fall in diesem Projekt schon dreimal ein
/// ungeprüfter war.
///
/// Er bildet die Registerlage der Architektur **nicht** nach: wo `x0` und `x1` im Frame stehen,
/// liest er aus der ABI-Indextabelle des Kopfes.
extern "C" fn handler(arg: usize) -> ! {
    let basis = arg as u64;
    R_H_BASIS.store(basis, Ordering::Release);
    let mut letzte_gen = 0u64;
    loop {
        let r = invoke(sys::RECV, 0, [0; 4], 0);
        R_H_RECV.store(r.result, Ordering::Release);
        if r.result != result::OK {
            loop {
                core::hint::spin_loop();
            }
        }
        let slot = r.msg[0];
        let anlass = r.msg[2];
        let p = (basis + slot * redirect::SLOT_BYTES as u64) as *mut u64;
        let mut sah = 0u64;
        // Den Kopf am Stück lesen und **prüfen**, bevor irgendein Frame-Wort angefasst wird.
        let mut kopf = [0u64; redirect::FRAME_WORT];
        for (i, k) in kopf.iter_mut().enumerate() {
            // SAFETY: `basis` kommt aus `SYS_MAP` auf der eigenen Fenster-Cap, `slot` aus der
            // Umleitungsnachricht des Kernels; gelesen werden Wörter innerhalb des Slots.
            *k = unsafe { p.add(i).read_volatile() };
        }
        let urteil = redirect::kopf_pruefen(
            &kopf,
            hal::exception::FRAME_ARCH,
            hal::exception::FRAME_GPR as u64,
            hal::exception::FRAME_WOERTER as u64,
        );
        if urteil == redirect::KopfUrteil::Ok {
            sah |= 1;
            if kopf[redirect::KOPF_GEN] > letzte_gen {
                sah |= 2;
                letzte_gen = kopf[redirect::KOPF_GEN];
            }
            if anlass == redirect::Anlass::Syscall as u64
                && kopf[redirect::KOPF_ANLASS] == redirect::Anlass::Syscall as u64
            {
                sah |= 4;
            }
            let i0 = kopf[redirect::KOPF_ABI] as usize;
            let i1 = kopf[redirect::KOPF_ABI + 1] as usize;
            // SAFETY: wie oben; die Indizes kommen aus der vom Kernel geschriebenen Tabelle und
            // sind durch `KOPF_NGESAMT` beschränkt.
            let nr = unsafe { p.add(redirect::frame_wort(i0)).read_volatile() };
            let a1 = unsafe { p.add(redirect::frame_wort(i1)).read_volatile() };
            if nr == GAST_SYSNO {
                sah |= 8;
            }
            if a1 == GAST_ARG {
                sah |= 16;
            }
            // **Die Antwort wird aus dem GELESENEN Argument berechnet.** Eine Konstante könnte er
            // auch melden, ohne den Frame gesehen zu haben.
            // SAFETY: wie oben.
            unsafe {
                p.add(redirect::frame_wort(i0))
                    .write_volatile(a1.wrapping_add(1));
            }
        }
        R_H_SAH.store(sah, Ordering::Release);
        R_H_BEDIENT.fetch_add(1, Ordering::Release);
        // Der **Köder** im Nachrichtenwort: er darf den Gast nie erreichen.
        let _ = invoke(sys::REPLY, 0, [KOEDER, 0, 0, 0], 0);
    }
}

/// **Der BINDER** — die dritte Partei.
///
/// Er ist weder Gast noch Handler und hält beide Autoritäten: die Tcb-Cap des Gastes (Slot 0,
/// *wessen* Syscalls) und die Handler-Cap (Slot 1, *wohin*). Ohne die Tcb-Cap könnte jede PD, die
/// zufällig eine Handler-Cap hält, die Syscalls eines fremden Threads an sich ziehen.
extern "C" fn binder(_arg: usize) -> ! {
    let r = invoke(sys::SETHANDLER, 0, [1, u64::MAX, 0, 0], 0);
    R_BIND_CODE.store(r.result, Ordering::Release);
    if r.result == result::OK {
        R_START.store(1, Ordering::Release);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// **Den Aufbau anlegen.** Muss vor [`redirect_messen`] laufen (die Threads brauchen Zeit).
///
/// Die Reihenfolge ist nicht beliebig: die Handler-Cap muss **vor** dem Handler-Thread stehen
/// (sonst `RECV` auf leerem Cspace — D0 in klein), der Gast **vor** dem Binder (der braucht seine
/// Tcb-Cap), und der Gast darf **vor** der Bindung keinen Syscall absetzen (deshalb `R_START`).
pub fn redirect_aufbau() {
    let laenge = system::sidecar_fenster_bytes();
    let (Some(ep), Some(hpd), Some(gpd), Some(bpd)) = (
        system::create_endpoint(),
        system::create_pd(),
        system::create_pd(),
        system::create_pd(),
    ) else {
        return;
    };
    let Some(fenster) = system::alloc(laenge, 4096) else {
        return;
    };
    let phys = fenster.base();
    // **Genullt, bevor irgendetwas es liest.** Recycelter Speicher könnte zufällig die Kennung
    // tragen, und dann wäre „hier steht ein Frame" wahr, ohne dass je einer abgelegt wurde.
    for i in 0..(laenge / 8) {
        // SAFETY: frisch allozierte, identisch abgebildete RAM-Region des Kernels.
        unsafe { core::ptr::write_volatile((phys + i * 8) as *mut u64, 0) };
    }
    let Ok(cap_mem) = system::cap_install(fenster) else {
        return;
    };
    let Ok(cap_h) =
        system::install_syscall_handler_cap(ep as u32, hpd as u16, phys, laenge, Rights::RW)
    else {
        return;
    };
    if !system::install_pd_cap(hpd, 0, cap_h) || !system::install_pd_cap(hpd, 1, cap_mem) {
        return;
    }
    // Der Handler zuerst: er soll im `RECV` stehen, bevor der Gast ruft.
    let Some(hp) = system::spawn_parked(
        handler as *const () as usize,
        phys as usize,
        system::IDLE_PRIO,
    ) else {
        return;
    };
    if system::admit_in_pd(hpd, hp).is_none() {
        return;
    }
    let Some(gp) = system::spawn_parked(gast as *const () as usize, 0, system::IDLE_PRIO) else {
        return;
    };
    let Some(gtid) = system::admit_in_pd(gpd, gp) else {
        return;
    };
    let Ok(cap_tcb) = system::install_tcb_cap(gtid, Rights::RW) else {
        return;
    };
    let Ok(cap_h2) = system::cap_copy(cap_h, Rights::RW) else {
        return;
    };
    if !system::install_pd_cap(bpd, 0, cap_tcb) || !system::install_pd_cap(bpd, 1, cap_h2) {
        return;
    }
    let Some(bp) = system::spawn_parked(binder as *const () as usize, 0, system::IDLE_PRIO) else {
        return;
    };
    if system::admit_in_pd(bpd, bp).is_none() {
        return;
    }
    R_EP.store(ep as u64 + 1, Ordering::Release);
    R_HPD.store(hpd as u64 + 1, Ordering::Release);
    R_GPD.store(gpd as u64 + 1, Ordering::Release);
    R_SIDECAR.store(phys, Ordering::Release);
    R_GAST_TID.store(gtid.to_raw(), Ordering::Release);
}

/// **Die Messung.** Läuft genau einmal; das Urteil steht danach in [`redirect_urteil`].
pub fn redirect_messen() {
    if R_MESS.load(Ordering::Acquire) & 2 != 0 {
        return;
    }
    let ok = redirect_messen_inner();
    R_MESS.store(if ok { 3 } else { 2 }, Ordering::Release);
}

fn redirect_messen_inner() -> bool {
    let (hpd, gtid_raw) = (
        R_HPD.load(Ordering::Acquire),
        R_GAST_TID.load(Ordering::Acquire),
    );
    if hpd == 0 || gtid_raw == 0 {
        println!(
            "redirect: FAILURES (Aufbau unvollstaendig: hpd={hpd} gast={gtid_raw:#x} -- ohne \
             Gast und Handler-PD ist NICHTS gemessen, und ein SKIP hier waere ein Schluss von \
             Schweigen auf Abwesenheit)"
        );
        return false;
    }
    let hpd = (hpd - 1) as usize;
    let gtid = ThreadId::from_raw(gtid_raw);

    // --- 1. Der Umlauf: warten, bis der Gast aus seinem umgeleiteten Syscall zurueck ist -----
    let mut fertig = false;
    for _ in 0..512 {
        if R_GAST_FERTIG.load(Ordering::Acquire) == 1 {
            fertig = true;
            break;
        }
        warten();
    }
    let bind_code = R_BIND_CODE.load(Ordering::Acquire);
    let sah = R_H_SAH.load(Ordering::Acquire);
    let recv = R_H_RECV.load(Ordering::Acquire);
    let x0 = R_GAST_X0.load(Ordering::Acquire);
    let msg0 = R_GAST_MSG0.load(Ordering::Acquire);
    let basis = R_H_BASIS.load(Ordering::Acquire);
    let bedient = R_H_BEDIENT.load(Ordering::Acquire);
    let (zust, ablage_fehler, uebernahme_fehler) = crate::sidecarkopie::bilanz();

    let gebunden = bind_code == result::OK;
    // **Das Fenster steht als Cap im Cspace der Handler-PD** -- am PD-Tabellen-Audit abgelesen,
    // nicht an einer Zusage. Zwei Aussagen in einer: die `Memory`-Cap deckt genau die Region,
    // und die `SyscallHandler`-Cap zeigt auf **dieselbe**. Zwei Caps mit verschiedenen Fenstern
    // waeren zwei Wahrheiten ueber einen Gast.
    let phys = R_SIDECAR.load(Ordering::Acquire);
    let laenge = system::sidecar_fenster_bytes();
    let mut slots = [None; 8];
    let n = system::pd_object_kinds(hpd, &mut slots);
    let mut region_cap = false;
    let mut handler_cap = false;
    for k in slots.iter().take(n).flatten() {
        match k {
            caprock_cap::ObjectKind::Memory(r) if r.base == phys && r.len == laenge => {
                region_cap = true
            }
            caprock_cap::ObjectKind::SyscallHandler { sidecar, len, .. }
                if *sidecar == phys && *len == laenge =>
            {
                handler_cap = true
            }
            _ => {}
        }
    }
    let fenster_in_handler_pd = phys != 0 && basis == phys && region_cap && handler_cap;
    let handler_sah_frame = sah == 0b11111;
    // **DIE Aussage**: der Gast sieht das ERGEBNIS, und es ist aus seinem eigenen Argument
    // gerechnet -- eine Konstante haette der Handler auch ohne den Frame melden koennen.
    let gast_sieht_ergebnis = x0 == GAST_ARG.wrapping_add(1);
    // Und der Koeder ist NICHT durchgekommen: der Rueckschrieb aus dem Sidecar hat den
    // IPC-Transport ueberschrieben. Ohne diese Zeile waere „ein Ergebnis kam an" auch dann wahr,
    // wenn das Sidecar gar nicht benutzt wuerde.
    let register_aus_sidecar = msg0 == GAST_X2;

    // --- 2. Fail-closed: faellt der Handler weg, FAULTET der Gast ----------------------------
    //
    // Positivkontrolle zuerst -- ohne sie bestuende „er steht" auch ein toter Thread.
    let laeuft_vorher = bewegt_sich_gast();
    let stillgelegt = system::pd_quiesce(hpd, true);
    R_START.store(2, Ordering::Release);
    let mut blockiert = false;
    for _ in 0..512 {
        if system::is_handler_blocked(gtid) {
            blockiert = true;
            break;
        }
        warten();
    }
    let steht = !bewegt_sich_gast();
    // **Er ist NICHT nativ weitergelaufen.** Waere er das, haette ihn der Entzug befoerdert: die
    // Nummer 39 kennt der Caprock-Kernel nicht, er bekaeme `ERR_BADCAP` und liefe weiter.
    let kein_rueckfall = R_GAST_ZWEITES.load(Ordering::Acquire) == 0;
    let _ = system::pd_quiesce(hpd, false);

    let alles = fertig
        && gebunden
        && fenster_in_handler_pd
        && handler_sah_frame
        && gast_sieht_ergebnis
        && register_aus_sidecar
        && bedient >= 1
        && zust >= 1
        && ablage_fehler == 0
        && uebernahme_fehler == 0
        && laeuft_vorher
        && stillgelegt
        && blockiert
        && steht
        && kein_rueckfall;

    println!(
        "redirect: {} EIN ECHTER UMGELEITETER SYSCALL (Gast-PD {} · Handler-PD {hpd} · Endpoint {} \
         · Sidecar {:#x}, {} B): bindung={bind_code} (erwartet {}) \
         fenster-in-handler-pd={fenster_in_handler_pd} handler-recv={recv} handler-sah-frame={handler_sah_frame} \
         (Bits {sah:#07b} = Argument|Nummer|Anlass|frisch|Kopf) bedient={bedient} umlauf-fertig={fertig}",
        if alles { "ALL PASS" } else { "FAILURES" },
        R_GPD.load(Ordering::Acquire).saturating_sub(1),
        R_EP.load(Ordering::Acquire).saturating_sub(1),
        phys,
        laenge,
        result::OK
    );
    println!(
        "redirect: der Gast setzte Syscall {GAST_SYSNO} ab -- eine Nummer, die der Caprock-Kernel \
         NICHT kennt (nativ waere das {} = ERR_BADCAP) -- und bekam x0={x0:#x} (erwartet {:#x} = \
         Argument+1, vom Handler AUS DEM SIDECAR gerechnet): gast-sieht-ergebnis={gast_sieht_ergebnis} \
         · x2={msg0:#x} (erwartet {GAST_X2:#x}, NICHT der Koeder {KOEDER:#x} aus der \
         Antwortnachricht): register-aus-sidecar={register_aus_sidecar}",
        result::ERR_BADCAP,
        GAST_ARG.wrapping_add(1)
    );
    println!(
        "redirect: fail-closed nach Stilllegung der Handler-PD: laeuft-vorher={laeuft_vorher} \
         stillgelegt={stillgelegt} handler-blockiert={blockiert} zaehler-steht={steht} \
         kein-rueckfall-auf-die-native-ABI={kein_rueckfall} (ein Rueckfall machte aus dem Entzug \
         einer Cap eine BEFOERDERUNG) · Sidecar-Bilanz: {zust} Zustellungen, {ablage_fehler} \
         Ablage-, {uebernahme_fehler} Uebernahmefehler"
    );
    alles
}

/// Bewegt sich der Rundenzähler des Gastes?
fn bewegt_sich_gast() -> bool {
    let a = R_GAST_RUNDEN.load(Ordering::Acquire);
    for _ in 0..8 {
        warten();
        if R_GAST_RUNDEN.load(Ordering::Acquire) != a {
            return true;
        }
    }
    false
}
