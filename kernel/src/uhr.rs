//! **Stufe A / A1: die Uhr hat eine BEDEUTUNG** — arch-neutral.
//!
//! Der Zaehler war nie das Problem: `rdtsc` und `CNTVCT_EL0` sind aus Ring 3 lesbar. Was fehlte,
//! ist die **Rate** — was ein Schritt wert ist —, und die ist gegen die Plattformuhr geeicht und
//! damit Kernelwissen. `SYS_CLOCK` gibt sie heraus.
//!
//! ## Warum die Messung nicht zirkulaer sein darf
//!
//! Die naheliegende Pruefung waere „der Syscall liefert dieselbe Zahl wie `cycles_per_sec()`".
//! Das belegt, dass ein Wert durchgereicht wird, und **nichts ueber seine Richtigkeit** — eine um
//! den Faktor zehn falsche Eichung kaeme genauso durch.
//!
//! Gemessen wird deshalb gegen eine **zweite, unabhaengige Uhr**: die Tick-Zaehlung des Kernels.
//! Ein EL0-Thread liest `CLOCK` zweimal, der Kernel klammert dasselbe Fenster mit `ticks()`, und
//! verglichen werden die beiden **Zeitspannen**:
//!
//! ```text
//!   (c1 - c0) / hz   gegen   (t1 - t0) / tick_hz
//! ```
//!
//! Weichen sie um mehr als die Toleranz ab, ist die Rate falsch — und zwar **egal, ob der Fehler
//! im Syscall, in der Eichung oder im Zaehler steckt**. Genau das soll eine Messung koennen.
//!
//! ## Was diese Zeile NICHT sagt
//!
//! Nichts ueber **Fristen** (A2). `WAIT` hat weiterhin keine Deadline, und die Blockade-Invarianten
//! sind unangetastet. Wer aus „die Uhr geht" auf „ein ausbleibender Interrupt ist erkennbar"
//! schliesst, hat A1 mit A2 verwechselt.

use crate::system;
use caprock_hal::println;

static UHR_OK: crate::befund::AtomicBefund = crate::befund::AtomicBefund::neu();

pub fn urteil() -> crate::befund::Befund {
    UHR_OK.lesen()
}

const PAGE: u64 = 4096;
/// Stapelseiten des Messthreads.
const STACK_PAGES: u64 = 4;
/// Arena: ein Stapelfenster plus die Parameterseite.
const ARENA_PAGES: u64 = STACK_PAGES + 1;
const PARAM_OFF: u64 = STACK_PAGES * PAGE;

// --- Parameterseite ---------------------------------------------------------------------------
const P_LOS: u64 = 0x00; // der Kernel gibt frei
const P_HZ: u64 = 0x08; // Rate, wie der Syscall sie meldet
const P_C0: u64 = 0x10; // Zaehlerstand am Anfang
const P_C: u64 = 0x18; // laufend fortgeschrieben
const P_LIEF: u64 = 0x20; // != 0, sobald der Thread ueberhaupt lief
// --- A2: die Frist ---------------------------------------------------------------------------
const P_NTFN: u64 = 0x28; // Cap-Slot der Notification, die der Kernel endowt hat
const P_T_CODE: u64 = 0x30; // Ergebnis des Wartens OHNE Signal (erwartet: ERR_TIMEOUT)
const P_T_TICKS: u64 = 0x38; // wie lange es gedauert hat, in Zaehlschritten
const P_S_CODE: u64 = 0x40; // Ergebnis des Wartens MIT Signal (erwartet: OK)
const P_S_TICKS: u64 = 0x48; // dito
const P_A2_FERTIG: u64 = 0x50;
/// Der Kernel gibt das A1-Fenster frei -- **erst danach** darf der Thread in eine Frist gehen.
const P_A1_ENDE: u64 = 0x58;
/// Die Frist der Sonde, in Ticks. Kurz genug, dass der Lauf nicht haengt; lang genug, dass der
/// Unterschied zwischen „Frist" und „Signal" in der Messung sichtbar wird.
const FRIST_TICKS: u64 = 10;

/// **Wie breit das Messfenster ist**, in Ticks des Kernels.
///
/// In Ticks und nicht in Runden: eine Zaehlschleife maesse die Geschwindigkeit des Wartenden.
/// Zwanzig Ticks sind bei 100 Hz rund 200 ms — breit genug, dass die Aufloesung der Tick-Uhr
/// (±1 Tick = 5 %) unter der Toleranz bleibt.
const FENSTER_TICKS: u64 = 20;

/// **Zulaessige Abweichung der beiden Zeitspannen, in Prozent.**
///
/// **8 %, und die Zahl ist hergeleitet statt gewaehlt.**
///
/// Nach unten: die Tick-Uhr zaehlt in ganzen Ticks, der Zykluszaehler nicht -- ueber die rund
/// 30 Ticks des Fensters sind das bis zu 3,3 % Quantisierung. Gemessen wurde 0 %.
///
/// Nach oben: sie muss **kleiner sein als die Verfaelschung der Gegenprobe**. `docs/linux-…` §5
/// schreibt fuer A1 „eine um 10 % verfaelschte Rate melden -- der Vergleich muss fallen"; mit 12 %
/// waere genau diese Gegenprobe durchgegangen, und die Zeile haette nie bewiesen, dass sie eine
/// falsche Eichung SIEHT.
const TOLERANZ_PROZENT: u64 = 8;

// ================================================================================================
// Der Messthread (EL0)
// ================================================================================================

/// **`SYS_CLOCK` aus EL0** — `(Rate, Stand)`.
///
/// # Safety
/// Nur aus User-Kontext. `#[inline(always)]`, sonst laege der Helfer in `.text` und waere aus
/// Ring 3 nicht ausfuehrbar.
#[inline(always)]
unsafe fn user_clock() -> (u64, u64) {
    let hz: u64;
    let now: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: siehe Funktionsdoku. Ergebnis: MSG0 = rsi, MSG1 = rdx (s. `ABI_TO_GPR`).
    unsafe {
        core::arch::asm!("int 0x80",
                         inlateout("rax") caprock_abi::sys::CLOCK => _,
                         out("rsi") hz, out("rdx") now,
                         clobber_abi("sysv64"));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("svc #0",
                         inlateout("x0") caprock_abi::sys::CLOCK => _,
                         out("x2") hz, out("x3") now,
                         clobber_abi("C"));
    }
    (hz, now)
}

/// Der Messthread: er wartet auf die Freigabe, liest die Uhr und schreibt sie fort.
///
/// **`.user_text` ist keine Formalie** — ein Ring-3-Einsprung in `.text` faultet an seiner eigenen
/// Einsprungadresse, und im Protokoll sieht das aus wie ein kaputter Mechanismus.
#[link_section = ".user_text"]
extern "C" fn messer(arg: usize) -> ! {
    let p = arg as u64;
    // SAFETY: identitaetsgemappte, EL0-zugaengliche Seite; der Kernel schreibt sie vor dem Spawn.
    let lies = |off: u64| unsafe { core::ptr::read_volatile((p + off) as *const u64) };
    // SAFETY: dieselbe Seite.
    let schreib = |off: u64, v: u64| unsafe {
        core::ptr::write_volatile((p + off) as *mut u64, v)
    };
    schreib(P_LIEF, 1);
    // **Auf die Freigabe warten**, damit der Kernel das Fenster klammern kann. Ohne sie waere der
    // Anfang des Fensters die Startzeit des Threads -- eine Groesse, die niemand kennt.
    while lies(P_LOS) == 0 {
        core::hint::spin_loop();
    }
    // SAFETY: User-Kontext.
    let (hz, c0) = unsafe { user_clock() };
    schreib(P_HZ, hz);
    schreib(P_C0, c0);
    schreib(P_C, c0);

    // **A1 zuerst, A2 danach** -- und die Reihenfolge ist erzwungen, nicht gehofft: waehrend einer
    // Frist schreibt dieser Thread `P_C` nicht fort, und die A1-Zeile laese daraus einen stehenden
    // Zaehler. Der Kernel setzt `P_A1_ENDE`, wenn sein Fenster zu ist.
    while lies(P_A1_ENDE) == 0 {
        // SAFETY: User-Kontext.
        let (_, c) = unsafe { user_clock() };
        schreib(P_C, c);
    }

    // --- A2: die Frist, in BEIDE Richtungen ---------------------------------------------------
    //
    // Erst OHNE Signal: die Frist muss ihn wecken, und zwar mit `ERR_TIMEOUT`. Dann MIT Signal
    // (der Kernel hat vorher signalisiert, das Bit steht also schon): dasselbe Warten muss
    // **frueher** und mit `OK` zurueckkommen.
    //
    // **Die Positivkontrolle ist die Haelfte, die entscheidet.** Ohne sie waere „es kam
    // `ERR_TIMEOUT`" auch mit einem kaputten Wecker wahr -- ein Thread, der nie geweckt wird,
    // sieht von einem, den die Frist weckt, nicht anders aus, wenn niemand den Gegenfall faehrt.
    let ntfn = lies(P_NTFN);
    // SAFETY: User-Kontext.
    let (_, a) = unsafe { user_clock() };
    let code = unsafe { user_wait_frist(ntfn, FRIST_TICKS) };
    // SAFETY: User-Kontext.
    let (_, b) = unsafe { user_clock() };
    schreib(P_T_CODE, code);
    schreib(P_T_TICKS, b - a);

    // Zweiter Gang: der Kernel hat inzwischen signalisiert.
    // SAFETY: User-Kontext.
    let (_, a2) = unsafe { user_clock() };
    let code2 = unsafe { user_wait_frist(ntfn, FRIST_TICKS) };
    // SAFETY: User-Kontext.
    let (_, b2) = unsafe { user_clock() };
    schreib(P_S_CODE, code2);
    schreib(P_S_TICKS, b2 - a2);
    schreib(P_A2_FERTIG, 1);

    loop {
        // SAFETY: User-Kontext.
        let (_, c) = unsafe { user_clock() };
        schreib(P_C, c);
    }
}

/// **`SYS_WAIT` mit Frist aus EL0** (A2) — gibt den Ergebniscode zurueck.
///
/// # Safety
/// Nur aus User-Kontext. `#[inline(always)]` — s. [`user_clock`].
#[inline(always)]
unsafe fn user_wait_frist(cap: u64, ticks: u64) -> u64 {
    let code: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: siehe Funktionsdoku. `MSG0` = rsi traegt die Frist.
    unsafe {
        core::arch::asm!("int 0x80",
                         inlateout("rax") caprock_abi::sys::WAIT => code,
                         in("rdi") cap, inlateout("rsi") ticks => _,
                         clobber_abi("sysv64"));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("svc #0",
                         inlateout("x0") caprock_abi::sys::WAIT => code,
                         in("x1") cap, inlateout("x2") ticks => _,
                         clobber_abi("C"));
    }
    code
}

/// **Die Tick-Zahl DIESES Kerns.**
///
/// `ticks()` ist **je Kern** gefuehrt. Die erste Fassung las hart `ticks(0)`, waehrend die Sonde
/// auf einem anderen Kern lief: `t1 - t0` war **null**, `warte` fiel ueber seine Notbremse heraus
/// statt ueber die Tick-Bedingung, und die Zeile meldete `tick=0us` neben einem voellig gesunden
/// `uhr=159990us`. Gefangen hat es das Konjunkt selbst (`klein > 0`) -- eine Fassung ohne diese
/// Untergrenze haette `abweichung=100%` als „passt nicht" gelesen und die Uhr beschuldigt.
fn jetzt_ticks() -> u64 {
    caprock_hal::timer::ticks(caprock_hal::cpu::core_id())
}

/// Das Etikett, mit dem der Kernel die A2-Positivkontrolle signalisiert.
const A2_BADGE: u64 = 0xA2;

/// Ticks verstreichen lassen.
fn warte(ticks: u64) {
    let t0 = jetzt_ticks();
    let mut wache = 0u64;
    while jetzt_ticks().wrapping_sub(t0) < ticks && wache < 400_000_000 {
        core::hint::spin_loop();
        wache += 1;
    }
}

fn lies(p: u64, off: u64) -> u64 {
    // SAFETY: identitaetsgemappte Parameterseite.
    unsafe { core::ptr::read_volatile((p + off) as *const u64) }
}

/// **Die Messung.**
pub fn messen() {
    let Some(pd) = system::create_pd() else {
        println!("uhr    : SKIP (keine PD frei)");
        UHR_OK.uebersprungen();
        return;
    };
    let Some(arena) = system::alloc(ARENA_PAGES * PAGE, PAGE) else {
        println!("uhr    : SKIP (kein Speicher)");
        UHR_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    let (a_base, a_len) = (arena.base(), arena.len());
    // SAFETY: frisch alloziert, identitaetsgemappt. **Der Kernel nullt, nicht der gemessene Pfad**
    // -- sonst waere „hier steht ein Wert" von „hier stand schon einer" nicht zu unterscheiden.
    unsafe {
        let mut o = 0u64;
        while o < a_len {
            core::ptr::write_volatile((a_base + o) as *mut u64, 0);
            o += 8;
        }
    }
    let Ok(cap) = system::cap_install(arena) else {
        println!("uhr    : SKIP (Memory-Cap nicht installierbar)");
        UHR_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    if !system::install_pd_cap(pd, 0, cap) {
        println!("uhr    : SKIP (Cap-Slot nicht belegbar)");
        UHR_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    }
    // **Die Notification der A2-Positivkontrolle** -- ohne sie gibt es nur die halbe Messung.
    let ntfn_id = system::create_notification();
    if let Some(n) = ntfn_id {
        if let Ok(nc) = system::install_notification_cap(n as u32, caprock_mem::Rights::RWX) {
            if system::install_pd_cap(pd, 1, nc) {
                // SAFETY: identitaetsgemappte Seite der eben allozierten Arena.
                unsafe { core::ptr::write_volatile((a_base + PARAM_OFF + P_NTFN) as *mut u64, 1) };
            }
        }
    }
    let param = a_base + PARAM_OFF;
    // Der Messthread bekommt seinen Stapel vom Allokator (wie der Elternthread der TLS-Sonde) --
    // die Arena traegt hier nur die Parameterseite. Ein eigener Stapel ist richtig, weil der
    // Messgegenstand die UHR ist und nicht die Stapelvergabe.
    let Some(p) =
        system::spawn_user_parked(messer as *const () as usize, param as usize, system::IDLE_PRIO)
    else {
        println!("uhr    : SKIP (Messthread nicht startbar)");
        UHR_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    };
    system::bind_pd_parked(&p, pd);
    if system::admit(p).is_none() {
        println!("uhr    : SKIP (Messthread nicht zulassbar)");
        UHR_OK.uebersprungen();
        let _ = system::free_pd_slot(pd);
        return;
    }

    // Auf „er laeuft" warten -- die Sprechprobe VOR der Messung.
    let mut w = 0;
    while w < 40 && lies(param, P_LIEF) == 0 {
        warte(1);
        w += 1;
    }

    // --- Das Fenster: der Kernel klammert, der Thread misst ---------------------------------
    // **Der Zaehler wird vom KERNEL geklammert, die RATE kommt ueber die ABI.**
    //
    // Die erste Fassung nahm beide Zaehlerstaende aus der Ablage des EL0-Threads -- und der laeuft
    // auf `IDLE_PRIO`: er schreibt seinen Stand nur, wenn er dran ist, und sein letzter Wert
    // hinkte dem Fensterende um 140 ms nach (`uhr=159890us` gegen `tick=300000us`, Abweichung
    // 46 %). Gemessen wurde damit die **Einplanung**, nicht die Uhr.
    //
    // Zirkulaer wird es dadurch nicht: `hz` stammt weiterhin aus dem **Syscall**, den der
    // EL0-Thread abgesetzt hat. Gefragt ist ja, ob die ueber die ABI gemeldete Rate den
    // Hardwarezaehler richtig in Zeit uebersetzt -- und die Gegenprobe dafuer ist die Tick-Uhr.
    let t0 = jetzt_ticks();
    let k0 = caprock_hal::timer::cycles();
    // SAFETY: identitaetsgemappte Seite.
    unsafe { core::ptr::write_volatile((param + P_LOS) as *mut u64, 1) };
    warte(FENSTER_TICKS);
    let t1 = jetzt_ticks();
    let k1 = caprock_hal::timer::cycles();
    let (hz, u0, u1) = (lies(param, P_HZ), lies(param, P_C0), lies(param, P_C));
    let (c0, c1) = (k0, k1);

    // **Die TICK-Rate, nicht `timer::freq()`** -- und der Unterschied hat einen Lauf gekostet.
    //
    // `freq()` ist die Zaehlfrequenz der Zeitbasis (x86: der kalibrierte LAPIC-Takt, hier rund
    // **1 GHz**), nicht die Rate, mit der der Kernel Ticks zaehlt. Die erste Fassung rechnete
    // `30 Ticks * 1e6 / 1e9` und bekam ganzzahlig **0**: die Zeile beschuldigte die Uhr, waehrend
    // die Ticks sauber liefen (1823 -> 1853). Gefangen hat es die Untergrenze `klein > 0` -- ohne
    // sie waere aus einer Division ein Befund geworden.
    //
    // `TICK_HZ` ist die Zahl, die `timer::init` bekommen hat, und sie steht an EINER Stelle.
    let tick_hz = crate::TICK_HZ;
    let lief = lies(param, P_LIEF) != 0;
    // **Untergrenze UND Obergrenze** -- ein einseitiger Vergleich ist gruen, sobald die Messung
    // ausfaellt („Null ist ein Befund").
    let rate_plausibel = hz >= 1_000_000 && hz <= 100_000_000_000;
    // **Was der EL0-Thread selbst gesehen hat** -- er darf nachhinken (er wird verdraengt), aber
    // nicht ueberholen und nicht stehen. Ohne dieses Konjunkt saege die Zeile die Uhr des Kernels
    // an und behauptete etwas ueber die ABI.
    let u_delta = u1.saturating_sub(u0);
    let zaehler_waechst = c1 > c0 && u_delta > 0 && u_delta <= (c1 - c0);

    // Beide Zeitspannen in Mikrosekunden -- ganzzahlig, ohne Fliesskomma (der Kernel hat keines).
    let us_uhr = if hz > 0 { (c1 - c0).saturating_mul(1_000_000) / hz } else { 0 };
    let us_tick = (t1 - t0).saturating_mul(1_000_000) / tick_hz;
    let (gross, klein) = if us_uhr > us_tick { (us_uhr, us_tick) } else { (us_tick, us_uhr) };
    let abweichung = if gross > 0 { (gross - klein) * 100 / gross } else { 100 };
    let passt_zum_tick = klein > 0 && abweichung <= TOLERANZ_PROZENT;

    // --- A2: die Frist, vom Kernel getrieben --------------------------------------------------
    //
    // **Zwei Richtungen, und die zweite ist die, die zaehlt.** „Es kam `ERR_TIMEOUT`" ist auch mit
    // einem kaputten Wecker wahr -- ein Thread, der nie geweckt wird, sieht von einem, den die
    // Frist weckt, nicht anders aus, solange niemand den Gegenfall faehrt.
    // SAFETY: identitaetsgemappte Seite.
    unsafe { core::ptr::write_volatile((param + P_A1_ENDE) as *mut u64, 1) };
    // 1. Ohne Signal: die Frist muss wecken.
    let mut w = 0;
    while w < 60 && lies(param, P_T_CODE) == 0 {
        warte(1);
        w += 1;
    }
    // 2. Mit Signal: **vorher** signalisieren, damit das Bit schon steht, wenn er wartet.
    if let Some(n) = ntfn_id {
        system::signal_from_kernel_ntfn(n, A2_BADGE);
    }
    let mut w = 0;
    while w < 60 && lies(param, P_A2_FERTIG) == 0 {
        warte(1);
        w += 1;
    }
    let t_code = lies(param, P_T_CODE);
    let t_dauer = lies(param, P_T_TICKS);
    let s_code = lies(param, P_S_CODE);
    let s_dauer = lies(param, P_S_TICKS);
    let frist_zyklen = if hz > 0 { FRIST_TICKS * hz / crate::TICK_HZ } else { u64::MAX };

    let frist_weckt = t_code == caprock_abi::result::ERR_TIMEOUT;
    // **Nicht frueher als die Frist.** Ein Wecker, der sofort feuert, saehe an `t_code` allein
    // genauso aus -- und waere kein Warten, sondern ein Rueckgabewert.
    let frist_nicht_zu_frueh = t_dauer >= frist_zyklen * 8 / 10;
    let signal_gewinnt = s_code == caprock_abi::result::OK;
    // **Und es war frueher.** Ohne diese Zahl waere „das Signal gewinnt" auch dann wahr, wenn in
    // Wahrheit wieder die Frist gefeuert haette und nur der Code stimmte.
    let signal_frueher = s_dauer < t_dauer;

    let ok = lief
        && rate_plausibel
        && zaehler_waechst
        && passt_zum_tick
        && frist_weckt
        && frist_nicht_zu_frueh
        && signal_gewinnt
        && signal_frueher;
    println!(
        "uhr    : {} (lief={lief} rate={hz} Hz rate-plausibel={rate_plausibel} \
         zaehler-waechst={zaehler_waechst} uhr={us_uhr}us tick={us_tick}us \
         abweichung={abweichung}% <= {TOLERANZ_PROZENT}% passt-zum-tick={passt_zum_tick} \
         el0-zaehler-delta={u_delta} (darf nachhinken, nicht ueberholen) \
         | A2: frist-weckt={frist_weckt} (code={t_code}, erwartet {}) \
         nicht-zu-frueh={frist_nicht_zu_frueh} ({t_dauer} >= {} Zyklen) \
         signal-gewinnt={signal_gewinnt} (code={s_code}) signal-frueher={signal_frueher} \
         ({s_dauer} < {t_dauer}) \
         -- A1: gemessen wird NICHT, ob der Syscall dieselbe Zahl liefert wie die HAL (das waere \
         zirkulaer und bei falscher Eichung genauso gruen), sondern ob die ueber die ABI gemeldete \
         Rate zur ZWEITEN Uhr des Systems passt. A2 misst BEIDE Richtungen: ohne Signal muss die \
         Frist wecken, mit Signal muss sie es NICHT -- eine Richtung allein ist auch mit einem \
         kaputten Wecker wahr)",
        if ok { "ALL PASS" } else { "FAILURES" },
        caprock_abi::result::ERR_TIMEOUT,
        frist_zyklen * 8 / 10
    );
    UHR_OK.gemessen(ok);
}
