//! **Z4d stage 1: an open transaction must not cross the cut — the probe.**
//!
//! It measures the one thing `bootckpt` structurally cannot: `bootckpt`'s subject is a pure
//! counting thread with no IPC role at all, so its cut is empty in every run, and a gate that never
//! fires is indistinguishable from a missing gate. This probe brings a subject that **is** in a
//! relationship.
//!
//! ## The claim, and it is an equivalence
//!
//! For every role a thread actually holds at a channel: *the channel migrates ⇔ the thread holding
//! that role migrates.* Both directions are refusals, and the line measures **both at the same
//! topology in the same run**, plus the positive control that a relationship contained entirely
//! inside the cut passes. Without that third half, "refuses everything" would look exactly like
//! "refuses the right thing".
//!
//! ## Why the second direction is the point
//!
//! `Scope::endpoints` was documented as "endpoints whose both sides are part of the checkpoint" and
//! was never held against the machine's actual IPC state — a caller wrote a number down and the
//! rule believed it. `Scope::EMPTY` hid the hole (with an empty scope every relationship cap is
//! refused, so the unchecked half was unreachable from the only call site), and an unreachable hole
//! is still a hole. `fremder-partner-abgewiesen` is that half: the endpoint travels, the client
//! blocked at it does not, and he waits forever on a rendezvous point that has left the machine.
//!
//! ## The carriers are its own, and they do work only a LIVING thread can do
//!
//! Every counted relationship is established by a real `CALL`/`RECV`/`WAIT` from EL0 and then
//! **observed** through `thread_quiescence` before anything is judged — the probe waits for the
//! state instead of assuming it. A `CALL` whose server is not yet in `RECV` lands in the *sender*
//! queue, which is a different relationship than the one described; measuring it would answer a
//! question nobody asked.

use crate::system;
use caprock_abi::sys;
use caprock_cap::checkpoint::{
    BuildRefusal, Channel, CutRefusal, Edge, EdgeRole, Image, Scope,
};
use caprock_hal::println;
use core::sync::atomic::{AtomicBool, Ordering};

/// Der Ausgang dieser Sonde — **dreiwertig** (2026-08-25, s. `crate::befund`). Vorgabe ist
/// `NichtGefahren`: eine Sonde, die an einem SKIP-Ausgang abbricht, ist weder bestanden noch
/// durchgefallen, und genau deshalb konnte sie bis heute nicht in `all_done()` haengen.
static CKPTCUT_OK: crate::befund::AtomicBefund = crate::befund::AtomicBefund::neu();

/// Urteil der `ckptcut`-Zeile, fuer die Hochlaufwege.
pub fn urteil() -> crate::befund::Befund {
    CKPTCUT_OK.lesen()
}

/// Ein Syscall aus EL0/Ring 3 mit einem Argument.
///
/// # Safety
/// Nur aus User-Kontext zu rufen.
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

/// **Der Aufrufer in PD X.** Ein `CALL`, auf das nie geantwortet wird — er haengt als `as_caller`
/// an einem Server einer **fremden** PD. Das ist das Subjekt beider Richtungen.
#[link_section = ".user_text"]
extern "C" fn ruf(_arg: usize) -> ! {
    // SAFETY: User-Kontext, freigegebener Einsprung.
    unsafe { user_syscall1(sys::CALL, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// **Der Server in PD Y.** Empfaengt und antwortet nie: der Zustand soll stabil sein, nicht
/// wahrscheinlich. Ein Paar, das nur „meistens" in der Beziehung steht, ergaebe eine Pruefzeile,
/// die manchmal etwas anderes misst.
#[link_section = ".user_text"]
extern "C" fn horch(_arg: usize) -> ! {
    // SAFETY: User-Kontext, freigegebener Einsprung.
    unsafe { user_syscall1(sys::RECV, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// **Der wartende Empfaenger in PD Y** — an einem eigenen Kanal, an dem niemand ruft.
///
/// Er ist der Fall **ohne Partner**: `partner_of` gibt fuer ihn `None`, und die naheliegende
/// Lesart waere, dass er gefahrlos wandern darf, weil er niemanden zuruecklaesst. Er laesst sich
/// selbst zurueck — nach dem Umzug wartet er auf einen Kanal, den er nicht mehr hat.
#[link_section = ".user_text"]
extern "C" fn lausch(_arg: usize) -> ! {
    loop {
        // SAFETY: User-Kontext, freigegebener Einsprung.
        unsafe { user_syscall1(sys::RECV, 1) };
    }
}

/// **Der Notification-Wartende in PD X.** Belegt die zweite Kanalart: ohne ihn waere der
/// Notification-Zweig des Sammlers ungefahrener Code, und ungefahren heisst vermutlich kaputt,
/// wenn er zum ersten Mal gebraucht wird.
#[link_section = ".user_text"]
extern "C" fn wartet(_arg: usize) -> ! {
    loop {
        // SAFETY: User-Kontext, freigegebener Einsprung.
        unsafe { user_syscall1(sys::WAIT, 2) };
    }
}

/// Ticks verstreichen lassen — **in Ticks und nicht in Runden**. Eine Zaehlschleife misst die
/// Geschwindigkeit des Wartenden, nicht den Fortschritt der anderen.
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
    let cap = system::install_endpoint_cap(ep as u32, caprock_mem::Rights::RW).ok()?;
    system::pd_install_cap(pd, slot, cap);
    Some(ep)
}

/// Einen EL0-Thread in einer PD starten.
fn thread_in(
    pd: usize,
    entry: usize,
    arg: usize,
    prio: u8,
) -> Option<caprock_sched::ThreadId> {
    let (p, _) = system::spawn_isolated_parked(entry, arg, prio)?;
    system::bind_pd_parked(&p, pd);
    system::admit(p)
}

/// Ein leerer Kantenpuffer, den der Sammler fuellt.
fn leerer_puffer() -> [Edge; system::CUT_EDGES_MAX] {
    [Edge {
        channel: Channel::Endpoint(0),
        thread: 0,
        role: EdgeRole::Sender,
    }; system::CUT_EDGES_MAX]
}

/// Ein Bild bauen und **nur den Grund** zurueckgeben — die Caps sind hier leer, es geht um den
/// Schnitt. `None` heisst „ein Checkpoint entstuende".
fn urteil_ueber(subject: u64, scope: &Scope, edges: &[Edge]) -> Option<BuildRefusal> {
    match Image::build([0u8; 32], 1, 1, 1, &[], scope, subject, edges) {
        Ok(_) => None,
        Err(r) => Some(r),
    }
}

/// **Die Sonde.** Von beiden Hochlaufwegen gerufen.
///
/// Der Aufbau steht getrennt vom Ablauf, weil jeder Abbruch hier ein **SKIP mit Grund** ist und
/// kein Urteil: „nicht messbar" ist weder bestanden noch durchgefallen, und ein Aufbau, der still
/// auf die Erfolgszeile durchfaellt, waere die schlimmere Haelfte davon.
pub fn messen(prio: u8) {
    let (Some(pd_x), Some(pd_y)) = (system::create_pd(), system::create_pd()) else {
        println!("ckptcut: SKIP (nicht genug PDs frei)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    // Der gemeinsame Kanal: X ruft, Y empfaengt -- beide Seiten brauchen die Cap.
    let Some(ep_gemein) = ep_in_pd(pd_x, 0) else {
        println!("ckptcut: SKIP (kein gemeinsamer Endpoint)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    let Ok(cap_y) = system::install_endpoint_cap(ep_gemein as u32, caprock_mem::Rights::RW) else {
        println!("ckptcut: SKIP (keine Endpoint-Cap fuer PD Y)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    system::pd_install_cap(pd_y, 0, cap_y);
    // Der Kanal des wartenden Empfaengers -- Slot 1 in PD Y.
    let Some(ep_still) = ep_in_pd(pd_y, 1) else {
        println!("ckptcut: SKIP (kein Endpoint fuer den Lauscher)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    // Die Notification -- Slot 2 in PD X.
    let Some(ntfn) = system::create_notification() else {
        println!("ckptcut: SKIP (keine Notification)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    let Ok(cap_n) = system::install_notification_cap(ntfn as u32, caprock_mem::Rights::RW) else {
        println!("ckptcut: SKIP (keine Notification-Cap)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    system::pd_install_cap(pd_x, 2, cap_n);
    ablauf(pd_x, pd_y, ep_gemein, ep_still, ntfn, prio);
}

/// Der eigentliche Ablauf, nachdem der Aufbau steht.
fn ablauf(
    pd_x: usize,
    pd_y: usize,
    ep_gemein: usize,
    ep_still: usize,
    ntfn: usize,
    prio: u8,
) {
    // **Reihenfolge mit Absicht: erst die Empfaenger, dann der Aufrufer.** Andersherum stuende der
    // Aufrufer in der SENDERschlange statt in der Beziehung, um die es geht.
    let (Some(t_horch), Some(t_lausch), Some(t_wartet)) = (
        thread_in(pd_y, horch as *const () as usize, 0, prio),
        thread_in(pd_y, lausch as *const () as usize, 0, prio),
        thread_in(pd_x, wartet as *const () as usize, 0, prio),
    ) else {
        println!("ckptcut: SKIP (Empfaenger nicht startbar)");
        CKPTCUT_OK.uebersprungen();
        return;
    };
    warte(2);
    let Some(t_ruf) = thread_in(pd_x, ruf as *const () as usize, 0, prio) else {
        println!("ckptcut: SKIP (Aufrufer nicht startbar)");
        CKPTCUT_OK.uebersprungen();
        return;
    };

    // **Auf den Zustand warten, statt ihn vorauszusetzen.** Beobachtet wird die Groesse selbst,
    // mit Frist -- eine Zaehlschleife maesse die Geschwindigkeit des Wartenden.
    let mut runden = 0;
    while runden < 12 {
        let qr = system::thread_quiescence(t_ruf);
        let qh = system::thread_quiescence(t_horch);
        let ql = system::thread_quiescence(t_lausch);
        let qw = system::thread_quiescence(t_wartet);
        if qr.as_caller && qh.as_reply_owner && ql.as_receiver && qw.as_receiver {
            break;
        }
        warte(1);
        runden += 1;
    }
    let q_ruf = system::thread_quiescence(t_ruf);
    let q_horch = system::thread_quiescence(t_horch);
    let beziehung_steht = q_ruf.as_caller && q_horch.as_reply_owner;
    let lauscher_steht = system::thread_quiescence(t_lausch).as_receiver;
    let warter_steht = system::thread_quiescence(t_wartet).as_receiver;

    let ep_id = ep_gemein as u32;
    let still_id = ep_still as u32;
    let ntfn_id = ntfn as u32;
    let s_ruf = t_ruf.to_raw();
    let s_horch = t_horch.to_raw();
    let s_lausch = t_lausch.to_raw();
    let s_wartet = t_wartet.to_raw();

    // **Erheben und urteilen mit DEMSELBEN Umfang.** `cut_edges` nimmt den `Scope` selbst und nicht
    // drei getrennte Listen: sonst koennte eine Messung ueber die eine Menge und ein Urteil ueber
    // eine andere gefaellt werden -- ein sauberes Urteil ueber einen unvollstaendigen Befund, und
    // genau die Form von „Zuteiler und Pruefer brauchen EINE Quelle".

    // --- 1. Sprechprobe: der Sammler FINDET die Beziehung ---------------------------------------
    //
    // Ohne sie ist jedes Urteil darunter leer: ein Schnitt ohne Kanten ist sauber, und „nichts
    // gefunden" saehe genau wie „nichts da" aus. Gefragt wird nach einer **bekannten** Kante, nicht
    // nach einer Zahl > 0 -- die Zahl waere auch von einer fremden Beziehung zu haben.
    let scope_nur_ruf = Scope {
        threads: core::slice::from_ref(&s_ruf),
        ..Scope::EMPTY
    };
    let mut puffer = leerer_puffer();
    let Ok(n_offen) = system::cut_edges(t_ruf, &scope_nur_ruf, &mut puffer) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- der Schnitt waere gekuerzt)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let kanten_offen = &puffer[..n_offen];
    let ruf_kante_gefunden = kanten_offen.iter().any(|e| {
        e.channel == Channel::Endpoint(ep_id) && e.thread == s_ruf && e.role == EdgeRole::Caller
    });

    // --- 2. Haelfte A: der Teilnehmer wandert, sein Kanal bleibt (Z4d woertlich) ------------------
    let offener_ruf_abgewiesen = matches!(
        urteil_ueber(s_ruf, &scope_nur_ruf, kanten_offen),
        Some(BuildRefusal::Cut(_, CutRefusal::ChannelNotInScope))
    );

    // --- 3. Haelfte B: der Kanal wandert, dieser Teilnehmer bleibt --------------------------------
    //
    // **Die Haelfte, die vorher niemand sehen konnte.** Der Umfang nennt den Endpoint; der Server
    // auf der anderen Seite steht nicht darin. Frueher hiess das „uebertragbar" -- und drueben
    // wartete ein Server auf einen Aufruf, dessen Rendezvouspunkt die Maschine verlassen hat.
    let eps_gemein = [ep_id];
    let scope_ep_und_ruf = Scope {
        endpoints: &eps_gemein,
        threads: core::slice::from_ref(&s_ruf),
        ..Scope::EMPTY
    };
    let mut puffer_b = leerer_puffer();
    let Ok(n_b) = system::cut_edges(t_ruf, &scope_ep_und_ruf, &mut puffer_b) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- Haelfte B)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let kanten_b = &puffer_b[..n_b];
    // Sprechprobe fuer genau diese Haelfte: der FREMDE Partner muss in der Erhebung stehen. Ohne
    // ihn urteilte die Regel ueber eine Kante, die sie nie gesehen hat.
    let fremde_kante_gefunden = kanten_b.iter().any(|e| {
        e.channel == Channel::Endpoint(ep_id)
            && e.thread == s_horch
            && e.role == EdgeRole::ReplyOwner
    });
    let fremder_partner_abgewiesen = matches!(
        urteil_ueber(s_ruf, &scope_ep_und_ruf, kanten_b),
        Some(BuildRefusal::Cut(_, CutRefusal::PeerNotInScope))
    );

    // --- 4. Die Positivkontrolle: eine GESCHLOSSENE Beziehung geht durch --------------------------
    //
    // Ohne sie belegen die beiden Absagen nur, dass irgendetwas abgewiesen wird. Dieselbe Regel wie
    // beim Gruppenschnitt (Z23/S3), in den Worten des Checkpoints: **eine Beziehung, deren beide
    // Enden im Schnitt liegen, ist keine offene Beziehung des Schnitts.**
    //
    // **Und dies ist die einzige Messung mit einem ZWEITEN Thread im Umfang** -- also die einzige,
    // die den Threadteil von Pass 2 ueberhaupt faehrt. Eine Liste, die immer leer ist, ist
    // ungefahrener Code, und ungefahren heisst vermutlich kaputt, wenn er zum ersten Mal gebraucht
    // wird.
    let beide = [s_ruf, s_horch];
    let scope_geschlossen = Scope {
        endpoints: &eps_gemein,
        threads: &beide,
        ..Scope::EMPTY
    };
    let mut puffer_g = leerer_puffer();
    let Ok(n_g) = system::cut_edges(t_ruf, &scope_geschlossen, &mut puffer_g) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- Positivkontrolle)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let geschlossene_beziehung_geht =
        urteil_ueber(s_ruf, &scope_geschlossen, &puffer_g[..n_g]).is_none();

    // --- 5. Keine Rolle ist ausgenommen: der wartende Empfaenger OHNE Partner ---------------------
    let scope_nur_lausch = Scope {
        threads: core::slice::from_ref(&s_lausch),
        ..Scope::EMPTY
    };
    let mut puffer_l = leerer_puffer();
    let Ok(n_l) = system::cut_edges(t_lausch, &scope_nur_lausch, &mut puffer_l) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- Lauscher)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let kanten_l = &puffer_l[..n_l];
    let lausch_kante_gefunden = kanten_l.iter().any(|e| {
        e.channel == Channel::Endpoint(still_id)
            && e.thread == s_lausch
            && e.role == EdgeRole::Receiver
    });
    let wartender_empfaenger_abgewiesen = matches!(
        urteil_ueber(s_lausch, &scope_nur_lausch, kanten_l),
        Some(BuildRefusal::Cut(_, CutRefusal::ChannelNotInScope))
    );
    // ... und mit seinem Kanal im Umfang wandert er. Beide Richtungen, sonst prueft die Zeile eine
    // Konstante.
    let eps_still = [still_id];
    let scope_lausch_mit_kanal = Scope {
        endpoints: &eps_still,
        threads: core::slice::from_ref(&s_lausch),
        ..Scope::EMPTY
    };
    let mut puffer_l2 = leerer_puffer();
    let Ok(n_l2) = system::cut_edges(t_lausch, &scope_lausch_mit_kanal, &mut puffer_l2) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- Lauscher mit Kanal)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let lausch_mit_kanal_geht =
        urteil_ueber(s_lausch, &scope_lausch_mit_kanal, &puffer_l2[..n_l2]).is_none();

    // --- 6. Die zweite Kanalart: eine Notification --------------------------------------------
    let scope_nur_warter = Scope {
        threads: core::slice::from_ref(&s_wartet),
        ..Scope::EMPTY
    };
    let mut puffer_n = leerer_puffer();
    let Ok(n_n) = system::cut_edges(t_wartet, &scope_nur_warter, &mut puffer_n) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- Notification)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let kanten_n = &puffer_n[..n_n];
    let ntfn_kante_gefunden = kanten_n.iter().any(|e| {
        e.channel == Channel::Notification(ntfn_id)
            && e.thread == s_wartet
            && e.role == EdgeRole::Receiver
    });
    let ntfn_abgewiesen = matches!(
        urteil_ueber(s_wartet, &scope_nur_warter, kanten_n),
        Some(BuildRefusal::Cut(_, CutRefusal::ChannelNotInScope))
    );
    let ntfns_mit = [ntfn_id];
    let scope_warter_mit_kanal = Scope {
        notifications: &ntfns_mit,
        threads: core::slice::from_ref(&s_wartet),
        ..Scope::EMPTY
    };
    let mut puffer_n2 = leerer_puffer();
    let Ok(n_n2) = system::cut_edges(t_wartet, &scope_warter_mit_kanal, &mut puffer_n2) else {
        println!("ckptcut: FAILURES (Kantenpuffer zu klein -- Notification mit Kanal)");
        CKPTCUT_OK.gemessen(false);
        return;
    };
    let ntfn_mit_kanal_geht =
        urteil_ueber(s_wartet, &scope_warter_mit_kanal, &puffer_n2[..n_n2]).is_none();

    // --- 7. Und die ALTE Absage bleibt unterscheidbar --------------------------------------------
    //
    // Ein Cap-Grund und ein Schnitt-Grund duerfen nicht ineinanderfallen: der erste wird durch
    // Warten nie besser, der zweite kann sich in einem Tick von selbst aufloesen. Waeren sie
    // gleich, liesse die Absage jemanden auf etwas warten, das sich nicht bewegt.
    let mmio = [Some(caprock_cap::ObjectKind::Mmio {
        phys: 0xfe00_0000,
        len: 0x1000,
    })];
    let cap_grund_bleibt_getrennt = matches!(
        Image::build([0u8; 32], 1, 1, 1, &mmio, &scope_nur_ruf, s_ruf, kanten_offen),
        Err(BuildRefusal::Cap(0, caprock_cap::checkpoint::LocalReason::DeviceWindow))
    );

    let ok = beziehung_steht
        && lauscher_steht
        && warter_steht
        && ruf_kante_gefunden
        && offener_ruf_abgewiesen
        && fremde_kante_gefunden
        && fremder_partner_abgewiesen
        && geschlossene_beziehung_geht
        && lausch_kante_gefunden
        && wartender_empfaenger_abgewiesen
        && lausch_mit_kanal_geht
        && ntfn_kante_gefunden
        && ntfn_abgewiesen
        && ntfn_mit_kanal_geht
        && cap_grund_bleibt_getrennt;
    CKPTCUT_OK.gemessen(ok);
    println!(
        "ckptcut : {} (beziehung-steht={} lauscher-steht={} warter-steht={} \
         ruf-kante-gefunden={} offener-ruf-abgewiesen={} fremde-kante-gefunden={} \
         fremder-partner-abgewiesen={} geschlossene-beziehung-geht={} \
         lausch-kante-gefunden={} wartender-empfaenger-abgewiesen={} lausch-mit-kanal-geht={} \
         ntfn-kante-gefunden={} ntfn-abgewiesen={} ntfn-mit-kanal-geht={} \
         cap-grund-getrennt={} kanten={}/{}/{}/{})",
        if ok { "ALL PASS" } else { "FAILURES" },
        beziehung_steht,
        lauscher_steht,
        warter_steht,
        ruf_kante_gefunden,
        offener_ruf_abgewiesen,
        fremde_kante_gefunden,
        fremder_partner_abgewiesen,
        geschlossene_beziehung_geht,
        lausch_kante_gefunden,
        wartender_empfaenger_abgewiesen,
        lausch_mit_kanal_geht,
        ntfn_kante_gefunden,
        ntfn_abgewiesen,
        ntfn_mit_kanal_geht,
        cap_grund_bleibt_getrennt,
        n_offen,
        n_b,
        n_l,
        n_n,
    );
}
