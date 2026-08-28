//! **Z23/S3: die Sonde des Gruppenschnitts — architekturneutral.**
//!
//! Sie misst die eine Aussage, die Z4a nicht schon zeigt: **zwei Threads derselben PD, die
//! miteinander IPC treiben, werden GEMEINSAM eingefroren.** Einzeln geht das strukturell nicht —
//! [`system::freeze_thread`] gibt fuer beide `Busy`, und es existiert keine Reihenfolge, die das
//! aufloest. Die Sonde belegt beide Haelften: die Absage im Einzelfall **und** den Erfolg der
//! Gruppe, am selben Paar, im selben Lauf.
//!
//! ## Warum sie ihre Traeger selbst mitbringt
//!
//! Jede gezaehlte Einheit muss eine Arbeit nachweisen, die nur ein **laufender** Traeger leisten
//! kann. Der Server schreibt deshalb einen Rundenzaehler in eine ihm gemappte Seite, und der
//! Kernel liest sie ueber die Physadresse — der Fortschritt ist eine **Wirkung des Ziels**, kein
//! Wert, den der gemessene Pfad selbst setzt. Das ist die Regel, an der die vergiftete
//! Mangel-Marke und die `park`-Zeile gescheitert sind, und die Kapazitaetskurve hat sie ein
//! zweites Mal bezahlt (224 „isolierte Prozesse", die alle sofort faulteten).
//!
//! ## Der Aufbau, und jede PD hat genau einen Grund
//!
//! | PD | Threads | wofuer |
//! |---|---|---|
//! | A | `klient` (haengt als Aufrufer), `server` (schuldet ihm die Antwort **und zaehlt**), `lauscher` (wartet in `RECV`) | der Gruppenschnitt: eine **interne** Beziehung, ein laufender Beleg, ein zurueckzuziehender Empfaenger |
//! | B | `fremdruf` (ruft in PD C hinein) | die **Frist** mit Partnernennung |
//! | C | `taub` (empfaengt und antwortet nie) | die Gegenseite ausserhalb von B |
//!
//! Der `server` ist mit Absicht zugleich Zaehler und Antwortschuldner: so belegt **ein** Thread,
//! dass die PD lief, und steht zugleich in der Beziehung, um die es geht.

use crate::system;
use caprock_abi::sys;
use caprock_hal::println;
use core::sync::atomic::{AtomicBool, Ordering};

/// Der Ausgang dieser Sonde — **dreiwertig** (2026-08-25, s. `crate::befund`). Vorgabe ist
/// `NichtGefahren`: eine Sonde, die an einem SKIP-Ausgang abbricht, ist weder bestanden noch
/// durchgefallen, und genau deshalb konnte sie bis heute nicht in `all_done()` haengen.
static PDFREEZE_OK: crate::befund::AtomicBefund = crate::befund::AtomicBefund::neu();

/// Urteil der `pdfreeze`-Zeile, fuer die Hochlaufwege.
pub fn urteil() -> crate::befund::Befund {
    PDFREEZE_OK.lesen()
}

/// Ein Syscall aus EL0/Ring 3 mit einem Argument.
///
/// # Safety
/// Nur aus User-Kontext zu rufen. `int 0x80` (x86) bzw. `svc #0` (aarch64) ist der dafuer
/// freigegebene Einsprung; der Kernel liest und schreibt nur die ABI-Register dieses Frames.
///
/// **`#[inline(always)]` ist keine Formalie:** ein nicht eingebetteter Helfer laege in `.text`,
/// waere aus User-Modus nicht ausfuehrbar, und der Fehler wanderte nur eine Ebene tiefer (s. die
/// Fallenliste zu `.user_text`).
#[inline(always)]
unsafe fn user_syscall1(nr: u64, a1: u64) -> u64 {
    let out: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("int 0x80", inlateout("rax") nr => out, in("rdi") a1,
                         clobber_abi("sysv64"));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        core::arch::asm!("svc #0", inlateout("x0") nr => out, in("x1") a1,
                         clobber_abi("C"));
    }
    out
}

/// **Der Aufrufer in PD A.** Ein `CALL`, auf das nie geantwortet wird — er bleibt als
/// `as_caller` haengen, und zwar an einem Server **derselben** PD.
#[link_section = ".user_text"]
extern "C" fn klient(arg: usize) -> ! {
    // SAFETY: User-Kontext, freigegebener Einsprung.
    unsafe { user_syscall1(sys::CALL, 0) };
    // **Ab hier zaehlt er** -- und dieser Zaehler ist die ganze Aussage von `klient-laeuft-nicht`.
    // Die erste Fassung las stattdessen `quiescence.as_caller`, also die Rolle im Endpoint: eine
    // Groesse, die der Scheduler gar nicht anfasst. Sie stand auf `true`, ob der Thread lief oder
    // nicht — dieselbe Form wie die `park`-Zeile, die `is_parked` an einem IPC-Wartenden las.
    // Gefunden hat es die Gegenprobe M7, nicht das Gegenlesen.
    let p = arg as *mut u64;
    loop {
        // SAFETY: `arg` zeigt auf das zweite Wort der dem Thread RW gemappten Seite.
        unsafe { p.write_volatile(p.read_volatile().wrapping_add(1)) };
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

/// **Der Server in PD A** — und der lebende Beleg zugleich.
///
/// Erst `RECV` (danach schuldet er die Antwort), dann ein Rundenzaehler in die ihm gemappte Seite.
/// Er antwortet **nie**: der Zustand soll stabil sein, nicht wahrscheinlich. Ein Paar, das nur
/// „meistens" in der Beziehung steht, ergaebe eine Pruefzeile, die manchmal etwas anderes misst.
#[link_section = ".user_text"]
extern "C" fn server(arg: usize) -> ! {
    // SAFETY: User-Kontext, freigegebener Einsprung.
    unsafe { user_syscall1(sys::RECV, 0) };
    let p = arg as *mut u64;
    loop {
        // SAFETY: `arg` ist die VA einer dem Thread RW gemappten Seite (identisch abgebildet).
        unsafe { p.write_volatile(p.read_volatile().wrapping_add(1)) };
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

/// **Der Lauscher in PD A.** Wartet in `RECV` an einem eigenen Kanal, an dem sonst niemand
/// bedient — der Fall, den der Schnitt **zurueckzieht** und nach innen zusperrt (S1b).
#[link_section = ".user_text"]
extern "C" fn lauscher(_arg: usize) -> ! {
    loop {
        // SAFETY: User-Kontext, freigegebener Einsprung.
        unsafe { user_syscall1(sys::RECV, 1) };
    }
}

/// **Der Fremdrufer in PD B.** Ruft in PD C hinein und bleibt dort haengen — die Gegenseite liegt
/// damit **ausserhalb** seiner eigenen PD, und genau das macht PD B unfrierbar.
#[link_section = ".user_text"]
extern "C" fn fremdruf(_arg: usize) -> ! {
    // SAFETY: User-Kontext, freigegebener Einsprung.
    unsafe { user_syscall1(sys::CALL, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// **Der taube Server in PD C.** Empfaengt und antwortet nie.
#[link_section = ".user_text"]
extern "C" fn taub(_arg: usize) -> ! {
    // SAFETY: User-Kontext, freigegebener Einsprung.
    unsafe { user_syscall1(sys::RECV, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// **Der Schlaefer in PD E** (S6): parkt, zaehlt eine Runde, parkt wieder.
///
/// Er misst die zweite Haelfte des Auftauens: ein Thread, der **mit** gesetzter Weckmarke
/// eingefroren wurde, muss nach dem Thaw **sofort** weiterlaufen — und einer ohne Marke darf es
/// nicht. Beide Richtungen, sonst belegt „er laeuft" nichts.
#[link_section = ".user_text"]
extern "C" fn schlaefer(arg: usize) -> ! {
    let p = arg as *mut u64;
    loop {
        // SAFETY: User-Kontext, freigegebener Einsprung.
        unsafe { user_syscall1(sys::PARK, 0) };
        // SAFETY: `arg` zeigt auf ein Wort der dem Thread RW gemappten Seite.
        unsafe { p.write_volatile(p.read_volatile().wrapping_add(1)) };
    }
}

/// **Der Traeger in PD D** (S5): zaehlt nur, damit die PD nachweislich lebt. Ohne ihn waere die
/// `HasDma`-Absage von „die PD ist leer" nicht zu unterscheiden.
#[link_section = ".user_text"]
extern "C" fn zaehler(arg: usize) -> ! {
    let p = arg as *mut u64;
    loop {
        // SAFETY: wie oben.
        unsafe { p.write_volatile(p.read_volatile().wrapping_add(1)) };
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

fn peek(pa: u64) -> u64 {
    // SAFETY: frisch allozierte, identisch abgebildete RAM-Seite.
    unsafe { core::ptr::read_volatile(pa as *const u64) }
}
fn poke(pa: u64, v: u64) {
    // SAFETY: wie `peek`.
    unsafe { core::ptr::write_volatile(pa as *mut u64, v) };
}

/// Ticks verstreichen lassen — **in Ticks und nicht in Runden**. Eine Zaehlschleife misst die
/// Geschwindigkeit des Wartenden, nicht den Fortschritt der anderen; das erste Z4a-Fenster war
/// kuerzer als ein Tick und meldete Stillstand fuer einen voellig gesunden Thread.
fn warte(ticks: u64) {
    let t0 = caprock_hal::timer::ticks(0);
    let mut wache = 0u64;
    while caprock_hal::timer::ticks(0).wrapping_sub(t0) < ticks && wache < 400_000_000 {
        core::hint::spin_loop();
        wache += 1;
    }
}

/// Einen Endpoint anlegen und seine Cap in den Cspace einer PD legen.
fn ep_in_pd(pd: usize, slot: usize) -> Option<usize> {
    let ep = system::create_endpoint()?;
    let cap = system::install_endpoint_cap(
        ep as u32,
        caprock_mem::Rights::RW,
    )
    .ok()?;
    system::pd_install_cap(pd, slot, cap);
    Some(ep)
}

/// Einen EL0-Thread in einer PD starten. `arg` landet im ersten Argumentregister.
fn thread_in(pd: usize, entry: usize, arg: usize, prio: u8, seite: Option<u64>) -> Option<caprock_sched::ThreadId> {
    let (p, _) = system::spawn_isolated_parked(entry, arg, prio)?;
    system::bind_pd_parked(&p, pd);
    if let Some(s) = seite {
        system::map_into_parked(&p, s, 4096, 1); // RW
    }
    system::admit(p)
}

/// **Die Sonde.** Von beiden Hochlaufwegen gerufen.
pub fn messen(prio: u8) {
    use system::{Freeze, PdFreeze};

    // --- Aufbau ---------------------------------------------------------------------------------
    let Some(seite) = system::alloc(4096, 4096).map(|c| c.region().base) else {
        println!("pdfreeze: SKIP (keine Seite fuer den Rundenzaehler)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    poke(seite, 0);
    let (Some(pd_a), Some(pd_b), Some(pd_c)) =
        (system::create_pd(), system::create_pd(), system::create_pd())
    else {
        println!("pdfreeze: SKIP (nicht genug PDs frei)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    // PD A: Slot 0 = der interne Kanal, Slot 1 = der Kanal des Lauschers.
    let (Some(ep_intern), Some(ep_lausch)) = (ep_in_pd(pd_a, 0), ep_in_pd(pd_a, 1)) else {
        println!("pdfreeze: SKIP (keine Endpoints/Caps fuer PD A)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    // PD B und PD C teilen sich einen Kanal -- B ruft, C empfaengt.
    let Some(ep_fremd) = ep_in_pd(pd_b, 0) else {
        println!("pdfreeze: SKIP (kein Endpoint fuer PD B)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    let Ok(cap_c) = system::install_endpoint_cap(
        ep_fremd as u32,
        caprock_mem::Rights::RW,
    ) else {
        println!("pdfreeze: SKIP (keine Cap fuer PD C)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    system::pd_install_cap(pd_c, 0, cap_c);

    // **Reihenfolge mit Absicht: erst die Empfaenger, dann die Aufrufer.** Andersherum stuende der
    // Aufrufer in der Senderschlange statt in der Beziehung, um die es geht -- und die Sonde
    // maesse einen anderen Zustand als den, den sie beschreibt.
    let (Some(t_taub), Some(t_server), Some(t_lausch)) = (
        thread_in(pd_c, taub as *const () as usize, 0, prio, None),
        thread_in(pd_a, server as *const () as usize, seite as usize, prio, Some(seite)),
        thread_in(pd_a, lauscher as *const () as usize, 0, prio, None),
    ) else {
        println!("pdfreeze: SKIP (Empfaenger nicht startbar)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    warte(2); // die drei Empfaenger in ihr `RECV` laufen lassen
    let (Some(t_klient), Some(t_fremd)) = (
        thread_in(pd_a, klient as *const () as usize, seite as usize + 8, prio, Some(seite)),
        thread_in(pd_b, fremdruf as *const () as usize, 0, prio, None),
    ) else {
        println!("pdfreeze: SKIP (Aufrufer nicht startbar)");
        PDFREEZE_OK.uebersprungen();
        return;
    };
    // **Auf den Zustand warten, statt ihn vorauszusetzen.** Ein `CALL`, dessen Server noch nicht in
    // `RECV` steht, landet in der SENDERschlange -- und damit maesse die Sonde einen anderen
    // Zustand als den, den sie beschreibt. Beobachtet wird die Groesse selbst, mit Frist.
    let mut runden = 0;
    while runden < 12 {
        let qk = system::thread_quiescence(t_klient);
        let qs = system::thread_quiescence(t_server);
        if qk.as_caller && qs.as_reply_owner {
            break;
        }
        warte(1);
        runden += 1;
    }

    // --- 1. Sprechprobe: die PD LEBT ------------------------------------------------------------
    //
    // Ohne sie belegt „steht" nichts: ein Zaehler, der nie lief, steht auch.
    let a0 = peek(seite);
    warte(3);
    let a1 = peek(seite);
    let laeuft_vorher = a1 > a0;

    // --- 2. Die Beziehung ist da, und sie ist INTERN --------------------------------------------
    let q_klient = system::thread_quiescence(t_klient);
    let q_server = system::thread_quiescence(t_server);
    let beziehung_steht = q_klient.as_caller && q_server.as_reply_owner;

    // --- 3. EINZELN ist keiner von beiden einzufrieren -------------------------------------------
    //
    // Das ist die Aussage, wegen der es diesen Strang gibt. **Und `freeze_thread` pausiert, bevor
    // es urteilt** — die Absage laesst also `PAUSE` stehen; beide werden gleich wieder aufgetaut,
    // sonst maesse Schritt 5 einen Stillstand, den die Messung selbst verursacht hat.
    let f_klient = system::freeze_thread(t_klient);
    let f_server = system::freeze_thread(t_server);
    let einzeln_klient = matches!(f_klient, Freeze::BusyOn(_, p) if p == t_server);
    let einzeln_server = matches!(f_server, Freeze::BusyOn(_, p) if p == t_klient);
    system::thaw_thread(t_klient);
    system::thaw_thread(t_server);
    warte(2);

    // --- 4. Die FRIST nennt den Partner ausserhalb (S2) ------------------------------------------
    let frist = system::freeze_pd(pd_b, 2);
    let frist_nennt_partner = matches!(
        frist,
        PdFreeze::Deadline { tid, partner: Some(p), .. } if tid == t_fremd && p == t_taub
    );
    let frist_code = frist.code();
    // Die Absage muss **vollstaendig** zurueckgenommen haben: Tore wieder offen.
    let frist_tore_auf = !system::pd_is_quiescing(pd_b);

    // --- 5. Der Gruppenschnitt ------------------------------------------------------------------
    let vor_schnitt = system::group_frozen_total();
    let schnitt = system::freeze_pd(pd_a, 8);
    let schnitt_code = schnitt.code();
    let PdFreeze::Frozen(zeuge) = schnitt else {
        // **Dieselben Feldnamen wie in der Erfolgszeile** -- und das ist keine Kosmetik. Die erste
        // Fassung schrieb hier `Schnitt code=` und `frist=`, waehrend die gruene Zeile
        // `schnitt-code=` und `frist-nennt-partner=` druckt: **zwei Formate fuer dieselbe
        // Tatsache**, und jede Gegenprobe, die gegen das eine geschrieben ist, ist fuer das andere
        // blind. Genau daran sind M2 und M5 gescheitert -- an der Beschriftung, nicht am Befund.
        println!(
            "pdfreeze: FAILURES (laeuft-vorher={} beziehung-intern={} einzeln-unfrierbar={}/{} \
             frist-nennt-partner={} frist-tore-auf={} schnitt-code={} frist-code={})",
            laeuft_vorher,
            beziehung_steht,
            einzeln_klient,
            einzeln_server,
            frist_nennt_partner,
            frist_tore_auf,
            schnitt_code,
            frist_code
        );
        return;
    };
    let umfang = zeuge.len();
    let zurueckgezogen = zeuge.zurueckgezogen();
    let kanaele_zu = zeuge.kanaele_zu();
    let im_schnitt = system::group_frozen_total() - vor_schnitt;

    // 5a. Er STEHT -- ueber ein Fenster von mehreren Ticks, nicht ueber einen Augenblick.
    let b0 = peek(seite);
    warte(4);
    let b1 = peek(seite);
    let steht = b1 == b0;

    // 5b. **S1b, gemessen statt angenommen** — und in ZWEI Zeilen, nicht in einer. Das Tor und der
    //     zurueckgezogene Empfaenger sind zwei verschiedene Zusagen; in einem Konjunkt vereint
    //     koennte die eine die andere decken, und die Gegenprobe wuesste nicht, welche gefallen ist.
    //
    //     Gefragt wird ueber `gate_new_transaction` — DIESELBE Funktion, die `call`/`recv`
    //     ausfuehren, keine Nachbildung, die auseinanderlaufen kann (A-4.2 hat sie genau dafuer
    //     als reine Funktion herausgezogen).
    let kanal_zu =
        system::endpoint_gate(ep_lausch) == Some(caprock_abi::result::ERR_QUIESCING);
    let empfaenger_gezogen = system::endpoint_receivers(ep_lausch) == 0;

    // 5d. **Warum der Schnitt einen EIGENEN Grund braucht** (Z24, achte Instanz) — und das ist die
    //     einzige Zeile, die es misst statt es zu behaupten: ein `RESUME` auf **einen** Teilnehmer
    //     darf ihn nicht aus der Gruppe herausloesen. Mit `PAUSE` als Grund genuegte dieser eine
    //     Aufruf, um die PD halb einzufrieren -- und **jeder Pruefer meldete Ordnung** (D11-Form).
    system::thaw_thread(t_server);
    let d0 = peek(seite);
    warte(3);
    let resume_wirkt_nicht = peek(seite) == d0;
    // 5c. Der INTERNE Kanal bleibt offen -- dort wartet kein Empfaenger, es gibt nichts zuzusperren,
    //     und ein Riegel waere ein Riegel zuviel.
    let intern_offen = system::endpoint_gate(ep_intern).is_none();

    // --- 6. Auftauen ist die Umkehrung ----------------------------------------------------------
    let aufgetaut = system::thaw_pd(zeuge);
    let geweckt = aufgetaut.threads;
    // **Die ZEIT, gemessen statt angenommen.** Der Schnitt stand ueber ein Fenster von vier Ticks
    // (Schritt 5a); die berichtete Dauer muss das mindestens abdecken. Ohne diese Zeile waere die
    // Zahl eine Behauptung -- und eine Standzeit von 0 waere von „nicht gemessen" nicht zu
    // unterscheiden.
    let standzeit_berichtet = aufgetaut.ticks >= 4;
    warte(3);
    let c0 = peek(seite);
    warte(3);
    let c1 = peek(seite);
    let laeuft_danach = c1 > c0;
    let lauscher_zurueck = system::endpoint_receivers(ep_lausch) == 1
        && system::endpoint_gate(ep_lausch).is_none();
    let tore_auf = !system::pd_is_quiescing(pd_a);
    // Der Aufrufer haengt weiter in seiner IPC -- `thaw` entfernt `FREEZE` und **nur** das.
    // Gemessen an seinem eigenen Zaehler, nicht an seiner Rolle im Endpoint: die Rolle aendert
    // sich auch dann nicht, wenn der Thread laeuft (s. `klient`).
    let klient_laeuft_nicht = peek(seite + 8) == 0;

    // --- 7. S5: die DMA-Absage, und zwar MIT Positivkontrolle ------------------------------------
    //
    // Eine Absage, die nie faellt, ist von einer fehlenden Absage nicht zu unterscheiden — und eine
    // Absage, die faellt, belegt fuer sich noch nicht, dass die **Cap** sie ausgeloest hat. Deshalb
    // zweimal dieselbe PD: einmal mit der DMA-Cap (muss `HasDma` geben), einmal ohne (muss
    // durchgehen). Erst die zweite Haelfte macht aus dem Ergebnis eine Aussage ueber die Ursache.
    let (dma_abgewiesen, dma_ohne_cap_geht) = match (system::create_pd(), system::alloc(4096, 4096))
    {
        (Some(pd_d), Some(dmaseite)) => {
            let dpa = dmaseite.region().base;
            let t_d = thread_in(
                pd_d,
                zaehler as *const () as usize,
                seite as usize + 24,
                prio,
                Some(seite),
            );
            let cap = system::install_dma_cap(dpa, 4096, caprock_mem::Rights::RW);
            match (t_d, cap) {
                (Some(_), Ok(c)) => {
                    system::pd_install_cap(pd_d, 2, c);
                    warte(2);
                    // **Den Zeugen nicht wegwerfen.** Die erste Fassung schrieb hier
                    // `matches!(system::freeze_pd(..), PdFreeze::HasDma)` — und liess damit im
                    // Nicht-HasDma-Fall einen `FrozenPd` fallen: die PD blieb eingefroren und
                    // stillgelegt, und die Positivkontrolle danach bekam `AlreadyQuiescing`.
                    // Gefunden hat es die Gegenprobe M8, deren Isolationshaelfte dadurch fiel.
                    //
                    // **`#[must_use]` deckt das nicht ab**: es bindet den RUECKGABEWERT, nicht die
                    // Nutzlast einer gematchten Variante. Ein `matches!` *benutzt* den Wert — der
                    // Typ hat hier getan, was er kann, und das ist weniger, als es aussieht.
                    let mit = match system::freeze_pd(pd_d, 2) {
                        PdFreeze::HasDma => true,
                        PdFreeze::Frozen(z) => {
                            let _ = system::thaw_pd(z);
                            false
                        }
                        _ => false,
                    };
                    system::pd_clear_cap(pd_d, 2);
                    let ohne = match system::freeze_pd(pd_d, 8) {
                        PdFreeze::Frozen(z) => {
                            let _ = system::thaw_pd(z);
                            true
                        }
                        _ => false,
                    };
                    (mit, ohne)
                }
                _ => (false, false),
            }
        }
        _ => (false, false),
    };

    // --- 8. S6, zweite Haelfte: die Weckmarke ueberlebt den Schnitt -------------------------------
    //
    // Beide Richtungen, sonst belegt „er laeuft danach" nichts: **mit** Marke muss er sofort
    // weiterlaufen, **ohne** Marke muss er liegen bleiben. Und waehrend des Schnitts darf das
    // `unpark` ihn NICHT loslaufen lassen — der Grund `FREEZE` steht noch, und eingereiht wird nur
    // bei leerer Menge.
    let (park_stumm_im_schnitt, park_marke_ueberlebt, park_ohne_marke_bleibt) =
        match system::create_pd() {
            Some(pd_e) => {
                match thread_in(
                    pd_e,
                    schlaefer as *const () as usize,
                    seite as usize + 16,
                    prio,
                    Some(seite),
                ) {
                    Some(t_p) => {
                        warte(3); // ihn in sein PARK laufen lassen
                        let p0 = peek(seite + 16);
                        let (a, b) = match system::freeze_pd(pd_e, 8) {
                            PdFreeze::Frozen(z) => {
                                system::unpark_thread(t_p);
                                warte(3);
                                let stumm = peek(seite + 16) == p0;
                                let _ = system::thaw_pd(z);
                                warte(3);
                                (stumm, peek(seite + 16) > p0)
                            }
                            _ => (false, false),
                        };
                        // Zweite Runde: einfrieren und auftauen OHNE Marke -- er muss schlafen.
                        warte(2);
                        let p1 = peek(seite + 16);
                        let c = match system::freeze_pd(pd_e, 8) {
                            PdFreeze::Frozen(z) => {
                                let _ = system::thaw_pd(z);
                                warte(3);
                                peek(seite + 16) == p1
                            }
                            _ => false,
                        };
                        (a, b, c)
                    }
                    None => (false, false, false),
                }
            }
            None => (false, false, false),
        };

    let ok = laeuft_vorher
        && beziehung_steht
        && einzeln_klient
        && einzeln_server
        && frist_nennt_partner
        && frist_tore_auf
        && umfang == 3
        && zurueckgezogen == 1
        && kanaele_zu == 1
        && im_schnitt == 3
        && steht
        && kanal_zu
        && empfaenger_gezogen
        && resume_wirkt_nicht
        && intern_offen
        && geweckt == 3
        && laeuft_danach
        && lauscher_zurueck
        && tore_auf
        && klient_laeuft_nicht
        && dma_abgewiesen
        && dma_ohne_cap_geht
        && park_stumm_im_schnitt
        && park_marke_ueberlebt
        && park_ohne_marke_bleibt
        && standzeit_berichtet;
    PDFREEZE_OK.gemessen(ok);
    println!(
        "pdfreeze: {} (laeuft-vorher={} beziehung-intern={} einzeln-unfrierbar={}/{} \
         frist-nennt-partner={} frist-tore-auf={} umfang={} zurueckgezogen={} kanaele-zu={} \
         im-schnitt={} steht={} kanal-zu={} empfaenger-gezogen={} resume-wirkt-nicht={} \
         intern-offen={} geweckt={} \
         laeuft-danach={} lauscher-zurueck={} tore-auf={} klient-laeuft-nicht={} \
         dma-abgewiesen={} dma-ohne-cap-geht={} park-stumm-im-schnitt={} \
         park-marke-ueberlebt={} park-ohne-marke-bleibt={} standzeit-berichtet={} standzeit={} \
         schnitt-code={} lausch-tid={:#x})",
        if ok { "ALL PASS" } else { "FAILURES" },
        laeuft_vorher,
        beziehung_steht,
        einzeln_klient,
        einzeln_server,
        frist_nennt_partner,
        frist_tore_auf,
        umfang,
        zurueckgezogen,
        kanaele_zu,
        im_schnitt,
        steht,
        kanal_zu,
        empfaenger_gezogen,
        resume_wirkt_nicht,
        intern_offen,
        geweckt,
        laeuft_danach,
        lauscher_zurueck,
        tore_auf,
        klient_laeuft_nicht,
        dma_abgewiesen,
        dma_ohne_cap_geht,
        park_stumm_im_schnitt,
        park_marke_ueberlebt,
        park_ohne_marke_bleibt,
        standzeit_berichtet,
        aufgetaut.ticks,
        schnitt_code,
        t_lausch.to_raw(),
    );
}
