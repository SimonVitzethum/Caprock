#!/usr/bin/env bash
# **Loest die GELESENEN Scheduler-Befunde B1/B2 aus -- am echten Quelltext.** (B-7.3, Strang Scheduler)
#
# ================================================================================================
# WARUM ES DIESE DATEI GIBT
# ================================================================================================
# `tools/verus-modelltreue-sched.sh` hat am 2026-08-03 fuenf Befunde gemeldet. Alle fuenf sind aus
# dem Quelltext HERGELEITET -- kein einziger ist je AUSGELOEST worden. Genau diese Differenz hat
# dieses Projekt mehrfach bezahlt: eine Aussage sieht wahr aus, weil der Fall, der sie widerlegen
# koennte, nie laeuft (leere Event-Queue ohne `CD.R`; `virtio-rng` als "Beleg" fuer den DMA-Pfad).
# Ein Befund, den niemand ausloest, ist eine Lesart, kein Messwert.
#
# Dieses Werkzeug fuehrt den ECHTEN `crates/sel4lake-sched/src/lib.rs` aus -- unveraendert, bis auf
# die eine `#![no_std]`-Zeile, die einem Host-Binary im Weg steht. Es gibt KEINE handgeschriebene
# Zweitfassung des Schedulers; das waere die B-7.2-Falle ("ein Beweis ueber eine Kopie beweist
# etwas ueber die Kopie"). Gegen die HAL steht ein Stellvertreter mit genau EINER Funktion
# (`init_thread_frame`); `sel4lake-slab` und `sel4lake-sync` werden im ECHTEN Quelltext gelinkt.
#
# ================================================================================================
# WAS GEMESSEN WIRD
# ================================================================================================
#   P0  **Sprechprobe des Messgeraets.** Die Messgroesse "steht in einer Ready-Liste" wird durch
#       LAUFEN der intrusiven Liste bestimmt (nicht durch Lesen des `queued`-Flags), und es wird
#       gezeigt, dass sie BEIDE Antworten geben kann. Ein Messgeraet, das nur "ja" sagen kann,
#       misst nichts.
#   P1  **Positivkontrolle.** Dieselbe Folge (`pause` -> `unblock`) an einem Thread mit RESTBUDGET.
#       Er muss danach in der Liste stehen, `depleted_count` muss 0 bleiben, `audit()` 0.
#       Ohne diesen Schritt ist jede Zahl aus M1..M6 wertlos.
#   M1  Die Kette aus dem Befund: Konto erschoepfen -> PAUSE -> RESUME. Steht der erschoepfte
#       Thread in der Ready-Liste? Wird er `current`? Was sagt `audit()`?
#   M2  Dieselbe Wirkung ueber einen Pfad, der KEINE PdControl-Cap braucht: Budget-Donation
#       (`switch_to`) + `unblock` des Aufrufers -- die Form, die `unblock_with_error`
#       (kernel/src/system.rs:6588) auf dem Reply-/Leichen-Pfad hat.
#   M3  Die Zahlenreihen: `depleted_count`, `next_refill`, `depletions` ueber N Runden --
#       einmal MIT pause/unblock je Runde (M3a), einmal OHNE (M3b, Kontrolle).
#   M4  `refill_depleted` reiht einen PAUSIERTEN Thread wieder ein -- eine ZWEITE, von `unblock`
#       unabhaengige Ursache. Gemessen ueber `audit()` und ueber die Wirkung (er wird `current`).
#   M5  Nach dem Refill: `depleted_count` gegen die WIRKLICHE Zahl erschoepfter Konten.
#   M6  **Die Verwechslungskontrolle.** Dieselbe Zahlenreihe entsteht auch ohne jedes `unblock`,
#       wenn kein anderer Thread lauffaehig ist (`dequeue_highest() == None` laesst `current`
#       stehen). Deshalb belegt M1 zusaetzlich, dass eine Alternative bereitstand.
#
# ------------------------------------------------------------------------------------------------
# D-REIHE (2026-08-03, nach D8): der DONEE-ZWEIG in `refill_depleted`
# ------------------------------------------------------------------------------------------------
# Der `_`-Zweig hat seit D8 zwei Waechter; der Donee-Zweig hat KEINEN -- er setzt `blocked = false`
# bedingungslos und reiht bedingungslos ein. Gemessen wird, ob das erreichbar schadet.
#   P2  **Positivkontrolle des Donee-Zweigs.** Der Donee war WIRKLICH auf das Konto-Budget
#       geblockt und wird beim Refill zu Recht bereit -- und laeuft danach auf einem Konto MIT
#       Budget. Ohne diesen Schritt ist keine Zahl aus D1..D7 etwas wert. Steht in `pk`.
#   D1  (F1) Der Donee ist aus einem ANDEREN Grund blockiert: die PAUSE wird gesetzt, waehrend
#       das Konto noch Budget hat (`konto_depleted = 0`) -- damit ist die Blockade nachweislich
#       die PAUSE. Der Refill hebt sie mit auf, weil beides dasselbe Bit ist.
#       D1b: dieselbe PAUSE nach der Erschoepfung ist ein No-Op und meldet trotzdem Erfolg.
#   D2  (F2) Der Donee ist SELBST erschoepft, wenn der Zweig ihn einreiht -> Audit-Code 9.
#   D3  (F3) Kann `sc_donee` veralten? (a) Tod des Donee, (b) Migration -- mit Sprechprobe.
#   D4  Das KONTO stirbt, waehrend der Donee auf sein Budget geblockt ist.
#   D5  VERSCHACHTELTE Spende (fs -> Blockdienst -> Treiber): der zweite CALL ueberschreibt
#       `sc_donee`, der innere REPLY loescht es -- der aeussere Server behaelt seinen `sc_donor`.
#   D6  `unblock` prueft das EIGENE Konto des Threads, nicht das belastete.
#   D7  `set_budget` auf das Konto raeumt `depleted` weg -- und damit den einzigen Wecker.
#   Jede dieser Messungen traegt eine Verhungerungsprobe (`ticks_donee_lief`,
#   `D1.nach_resume.ticks_mit_budget`): ein Donee, der NIE WIEDER laeuft, ist schlimmer als einer,
#   der zu frueh laeuft.
#
# **NICHT gemessen:** Nebenlaeufigkeit (der Waechter ist sequentiell, die Kern-Locks bleiben
# ausserhalb), der echte Kontextwechsel (HAL-Stellvertreter), und die Erreichbarkeit aus dem
# Syscall-Pfad des Kernels (dazu braeuchte es einen hwfuzz-Fall; hier wird nur die Aufruffolge
# nachgestellt, die `sel4lake-ipc` / `kernel/src/system.rs` an dieser Stelle absetzen).
#
# ================================================================================================
# GEGENPROBE
# ================================================================================================
# Dieselbe Folge laeuft an mehreren Fassungen. Die Mutationen liegen ausschliesslich auf KOPIEN;
# `crates/` wird nie beschrieben.
#   echt  der unveraenderte Quelltext
#   V0    der Stand VOR D8 (beide Waechter und Audit-Code 9 entfernt) -- die Sprechprobe der
#         Gegenprobenmechanik: die alten Befunde muessen hier wieder auftauchen
#   H-a   der Waechter des `_`-Zweigs WOERTLICH in den Donee-Zweig uebertragen -- faellt in der
#         Positivkontrolle durch (der Donee ist dort IMMER blockiert, also weckt ihn niemand)
#   H-b   der Vorschlag: der GRUND der Blockade wird mitgeschrieben (`budget_blocked`), und der
#         Refill weckt, wer WEGEN DIESES KONTOS blockiert ist -- statt des einen Slots `sc_donee`
#
# Aufruf:  tools/sched-erschoepfung-messen.sh [--nur-echt]
# Rueckgabe: 0 = Messung gelaufen (Positivkontrolle bestanden) · 2 = Werkzeugfehler.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CODE_STD="$ROOT/crates/sel4lake-sched/src/lib.rs"
CYCLES_STD="$ROOT/crates/sel4lake-sched/src/cycles.rs"
SLAB_STD="$ROOT/crates/sel4lake-slab/src/lib.rs"
SYNC_STD="$ROOT/crates/sel4lake-sync/src/lib.rs"
RUSTC="${RUSTC:-rustc}"

WURZEL="$(mktemp -d)"
trap 'rm -rf "$WURZEL"' EXIT
trap 'rm -rf "$WURZEL"; exit 130' INT TERM

# ================================================================================================
# 1. Der Anbau: HAL-Stellvertreter + Messung. Bildet den Scheduler NICHT nach -- er misst ihn.
# ================================================================================================
anbau_schreiben() {
    cat > "$1" <<'RSEOF'

// ================================================================================================
// Ab hier: der Anbau des Messwerkzeugs. Nicht Teil von `sel4lake-sched`.
// ================================================================================================

/// Stellvertreter fuer `sel4lake-hal`. Der Scheduler braucht davon genau eine Funktion. Was ein
/// echter Frame ist, sagt dieser Lauf NICHT -- hier zaehlt nur, dass jeder Thread einen
/// unterscheidbaren `sp` bekommt.
mod sel4lake_hal {
    pub mod exception {
        pub fn init_thread_frame(
            stack_top: usize,
            _entry: usize,
            _arg: usize,
            _user: bool,
            _user_sp: usize,
        ) -> usize {
            stack_top
        }
    }
}

mod messung {
    use crate::{
        attach_directory, core_storage_align, core_storage_bytes, directory_align,
        directory_bytes, Scheduler, ThreadId, NIL, NOT_QUEUED, NPRIO,
    };

    const CAP: usize = 32;
    /// Dummy-Frame. Der Scheduler legt ihn nur ab; sein Wert spielt keine Rolle.
    const F: usize = 0xF000;
    const PRIO_IDLE: u8 = 0;
    const PRIO_T: u8 = 1;
    const BUDGET: u32 = 3;
    const PERIOD: u32 = 100;

    // --- Aufbau -------------------------------------------------------------------------------

    fn roh(bytes: usize, align: usize) -> *mut u8 {
        let l = std::alloc::Layout::from_size_align(bytes, align).expect("Layout");
        // SAFETY: frisch alloziert, exklusiv, wird nie freigegeben (Slab-Vertrag: kein Drop).
        let p = unsafe { std::alloc::alloc(l) };
        assert!(!p.is_null(), "kein Speicher");
        p
    }

    /// Ein frischer Kern mit frischem Directory. Jedes Szenario steht fuer sich.
    fn aufbau() -> (Scheduler, ThreadId) {
        // SAFETY: frischer, exklusiver Speicher; das Directory wird je Szenario neu angehaengt
        // (der alte Block bleibt liegen -- ein Messbinary lebt Millisekunden).
        unsafe { attach_directory(roh(directory_bytes(CAP), directory_align()), CAP) };
        let mut s = Scheduler::new();
        // SAFETY: wie oben.
        unsafe { s.attach_storage(0, roh(core_storage_bytes(CAP), core_storage_align()), CAP) };
        let idle = s.init_core(0, PRIO_IDLE).expect("init_core");
        (s, idle)
    }

    fn faden(s: &mut Scheduler, prio: u8, nr: usize) -> ThreadId {
        s.spawn(0, 0x1000 + nr, 0, 0x10_0000 + nr * 0x1000, 0x1000, prio)
            .expect("spawn")
    }

    // --- Messgroessen -------------------------------------------------------------------------
    //
    // Alles hier LIEST nur. Die Mitgliedschaft in einer Ready-Liste wird durch **Laufen** der
    // intrusiven Liste bestimmt, nicht durch Lesen des `queued`-Flags: das Flag ist eine
    // Behauptung des Schedulers ueber sich selbst, die Liste ist die Sache.

    fn lok(s: &Scheduler, t: ThreadId) -> usize {
        s.resolve(t).expect("Thread nicht aufloesbar")
    }

    /// In welcher Prioritaetsliste steht `local` **wirklich**? (Liste gelaufen.)
    fn in_liste(s: &Scheduler, local: usize) -> Option<usize> {
        for p in 0..NPRIO {
            let mut i = s.queues[p].head;
            let mut n = 0usize;
            while i != NIL {
                if i as usize == local {
                    return Some(p);
                }
                i = s.tcbs[i as usize].qnext;
                n += 1;
                assert!(n <= CAP + 1, "Listenzyklus in Prioritaet {p}");
            }
        }
        None
    }

    /// Was BEHAUPTET das Flag? Weicht es von der Liste ab, ist das selbst ein Befund.
    fn flag_liste(s: &Scheduler, local: usize) -> Option<usize> {
        let q = s.tcbs[local].queued;
        if q == NOT_QUEUED {
            None
        } else {
            Some(q as usize)
        }
    }

    fn ja(b: bool) -> i64 {
        if b {
            1
        } else {
            0
        }
    }

    /// Wie viele Konten sind WIRKLICH erschoepft (Tabelle gezaehlt)? Die zweite, unabhaengig
    /// hergeleitete Zahl neben `depleted_count` -- eine allein koennte zufaellig stimmen.
    fn echte_erschoepfte(s: &Scheduler) -> i64 {
        (0..s.tcbs.len())
            .filter(|&i| s.tcbs[i].used && s.tcbs[i].depleted)
            .count() as i64
    }

    /// Vollstaendiger Blick auf einen Thread, ohne Adjektive.
    fn zeile(s: &Scheduler, name: &str, local: usize) {
        let t = &s.tcbs[local];
        println!(
            "     {name:<10} blocked={} depleted={} remaining={:<3} budget={:<3} next_refill={:<5} \
in_liste={} flag={} current={}",
            ja(t.blocked),
            ja(t.depleted),
            t.remaining,
            t.budget,
            t.next_refill,
            match in_liste(s, local) {
                Some(p) => p as i64,
                None => -1,
            },
            match flag_liste(s, local) {
                Some(p) => p as i64,
                None => -1,
            },
            ja(s.current == Some(local)),
        );
    }

    // --- Protokoll ----------------------------------------------------------------------------

    pub struct Bericht {
        pub zeilen: Vec<(String, i64)>,
    }

    impl Bericht {
        fn k(&mut self, name: &str, wert: i64) {
            self.zeilen.push((name.to_string(), wert));
        }
    }

    // ==============================================================================================
    // P0 -- Sprechprobe: kann die Messgroesse ueberhaupt BEIDE Antworten geben?
    // ==============================================================================================
    fn p0(b: &mut Bericht) {
        println!("  P0  Sprechprobe des Messgeraets (ohne Budget, ohne Erschoepfung)");
        let (mut s, _idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);

        b.k("P0.nach_spawn.in_liste", ja(in_liste(&s, lt).is_some()));
        zeile(&s, "nach spawn", lt);
        s.pause(t);
        b.k("P0.nach_pause.in_liste", ja(in_liste(&s, lt).is_some()));
        zeile(&s, "nach pause", lt);
        s.unblock(t);
        b.k("P0.nach_unblock.in_liste", ja(in_liste(&s, lt).is_some()));
        zeile(&s, "nach unblock", lt);
        b.k("P0.audit", s.audit() as i64);
        b.k(
            "P0.liste_gleich_flag",
            ja(in_liste(&s, lt) == flag_liste(&s, lt)),
        );
    }

    // ==============================================================================================
    // P1 -- Positivkontrolle: dieselbe Folge an einem Thread mit RESTBUDGET.
    //
    // Der entscheidende Aufruf (`unblock` an einem blockierten, nicht eingereihten, nicht
    // laufenden Thread) steht hier in genau derselben Lage wie in M1. Der EINZIGE Unterschied
    // ist, ob das Konto leer war.
    // ==============================================================================================
    fn p1(b: &mut Bericht) {
        println!("  P1  Positivkontrolle -- pause/unblock mit RESTBUDGET");
        let (mut s, idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        let li = lok(&s, idle);
        assert!(s.set_budget(t, 5, PERIOD), "set_budget");

        s.on_tick(0, F, false); // YIELD: idle einreihen, hoechste Prioritaet waehlen -> t
        b.k("P1.t_ist_current", ja(s.current == Some(lt)));
        s.on_tick(0, F, true); // 5 -> 4
        s.on_tick(0, F, true); // 4 -> 3

        s.pause(t); // blocked = true, laeuft aber noch
        s.on_tick(0, F, true); // deplaniert ihn sauber (requeue = false), 3 -> 2
        b.k("P1.vor_unblock.blocked", ja(s.tcbs[lt].blocked));
        b.k("P1.vor_unblock.depleted", ja(s.tcbs[lt].depleted));
        b.k("P1.vor_unblock.remaining", s.tcbs[lt].remaining as i64);
        b.k("P1.vor_unblock.in_liste", ja(in_liste(&s, lt).is_some()));
        b.k("P1.vor_unblock.current", ja(s.current == Some(lt)));
        zeile(&s, "vor unblock", lt);

        s.unblock(t);
        b.k("P1.nach_unblock.in_liste", ja(in_liste(&s, lt).is_some()));
        b.k("P1.nach_unblock.depleted", ja(s.tcbs[lt].depleted));
        b.k("P1.nach_unblock.remaining", s.tcbs[lt].remaining as i64);
        b.k("P1.depleted_count", s.depleted_count as i64);
        b.k("P1.depletions", s.budget_stats().0 as i64);
        b.k("P1.audit", s.audit() as i64);
        zeile(&s, "nach unblock", lt);

        // Wirkung: wird er eingeplant, und laeuft er mit ECHTEM Budget?
        s.on_tick(0, F, false);
        b.k("P1.wird_current", ja(s.current == Some(lt)));
        b.k("P1.idle_war_bereit", ja(in_liste(&s, li).is_some()));
        s.on_tick(0, F, true);
        b.k("P1.remaining_sinkt", s.tcbs[lt].remaining as i64);
    }

    // ==============================================================================================
    // M1 -- die Kette aus dem Befund: erschoepfen -> PAUSE -> RESUME.
    // ==============================================================================================
    fn m1(b: &mut Bericht) {
        println!("  M1  erschoepfen -> pause -> unblock");
        let (mut s, idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        let li = lok(&s, idle);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");

        s.on_tick(0, F, false);
        assert_eq!(s.current, Some(lt), "t muss laufen");
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        // Nach der Erschoepfung: deplaniert, off-queue, NICHT blockiert.
        b.k("M1.nach_erschoepfung.depleted", ja(s.tcbs[lt].depleted));
        b.k("M1.nach_erschoepfung.remaining", s.tcbs[lt].remaining as i64);
        b.k(
            "M1.nach_erschoepfung.in_liste",
            ja(in_liste(&s, lt).is_some()),
        );
        b.k("M1.nach_erschoepfung.current", ja(s.current == Some(lt)));
        b.k("M1.nach_erschoepfung.depleted_count", s.depleted_count as i64);
        b.k("M1.nach_erschoepfung.audit", s.audit() as i64);
        let refill_vor = s.tcbs[lt].next_refill as i64;
        b.k("M1.next_refill_vor", refill_vor);
        zeile(&s, "erschoepft", lt);

        s.pause(t);
        b.k("M1.nach_pause.blocked", ja(s.tcbs[lt].blocked));
        b.k("M1.nach_pause.in_liste", ja(in_liste(&s, lt).is_some()));

        s.unblock(t);
        // ==> DIE MESSUNG
        b.k("M1.nach_unblock.in_liste", ja(in_liste(&s, lt).is_some()));
        b.k("M1.nach_unblock.depleted", ja(s.tcbs[lt].depleted));
        b.k("M1.nach_unblock.remaining", s.tcbs[lt].remaining as i64);
        b.k("M1.nach_unblock.audit", s.audit() as i64);
        b.k(
            "M1.liste_gleich_flag",
            ja(in_liste(&s, lt) == flag_liste(&s, lt)),
        );
        zeile(&s, "nach unblock", lt);

        // Stand eine Alternative bereit? Ohne diese Zahl koennte "wird current" auch heissen
        // "es gab nichts anderes" (s. M6). Gemessen wird sie **im Augenblick der Wahl**, also
        // nach dem Umplanungsschritt -- davor ist der Idle-Thread `current` und steht deshalb
        // in keiner Liste. (Genau diese Verwechslung hat die erste Fassung gemacht.)
        s.on_tick(0, F, false);
        b.k("M1.alternative_bei_wahl", ja(in_liste(&s, li).is_some()));
        b.k("M1.wird_current", ja(s.current == Some(lt)));
        b.k("M1.remaining_beim_laufen", s.tcbs[lt].remaining as i64);
        b.k("M1.depleted_beim_laufen", ja(s.tcbs[lt].depleted));

        // Ein voller Tick auf leerem Konto.
        let dep_vor = s.budget_stats().0 as i64;
        s.on_tick(0, F, true);
        b.k("M1.depletions_delta", s.budget_stats().0 as i64 - dep_vor);
        b.k("M1.depleted_count_danach", s.depleted_count as i64);
        b.k("M1.echte_erschoepfte_danach", echte_erschoepfte(&s));
        b.k("M1.next_refill_nach", s.tcbs[lt].next_refill as i64);
        b.k("M1.next_refill_verschoben", s.tcbs[lt].next_refill as i64 - refill_vor);
    }

    // ==============================================================================================
    // M2 -- dieselbe Wirkung ohne PdControl: Budget-Donation + `unblock` des Aufrufers.
    //
    // `switch_to` ist der IPC-CALL-Fastpath: der Aufrufer blockiert und leiht dem Server sein
    // Konto. Erschoepft sich das Konto, waehrend der Server laeuft, ist der AUFRUFER blockiert
    // UND erschoepft. Weckt ihn danach irgendwer (`unblock_with_error` auf dem Leichen-/
    // Abbruchpfad, kernel/src/system.rs:6588), greift dieselbe Stelle.
    // ==============================================================================================
    fn m2(b: &mut Bericht) {
        println!("  M2  Budget-Donation -> unblock des Aufrufers (kein PdControl noetig)");
        let (mut s, idle) = aufbau();
        let c = faden(&mut s, PRIO_T, 1); // Aufrufer
        let srv = faden(&mut s, PRIO_T, 2); // Server
        let lc = lok(&s, c);
        let ls = lok(&s, srv);
        let li = lok(&s, idle);
        assert!(s.set_budget(c, BUDGET, PERIOD), "set_budget");

        s.on_tick(0, F, false);
        assert_eq!(s.current, Some(lc), "der Aufrufer muss laufen");
        s.switch_to(0, F, srv); // CALL-Fastpath: c blockiert, srv laeuft gegen cs Konto
        b.k("M2.nach_call.caller_blocked", ja(s.tcbs[lc].blocked));
        b.k("M2.nach_call.server_current", ja(s.current == Some(ls)));

        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("M2.nach_erschoepfung.caller_depleted", ja(s.tcbs[lc].depleted));
        b.k("M2.nach_erschoepfung.caller_blocked", ja(s.tcbs[lc].blocked));
        b.k("M2.nach_erschoepfung.server_blocked", ja(s.tcbs[ls].blocked));
        b.k(
            "M2.nach_erschoepfung.caller_in_liste",
            ja(in_liste(&s, lc).is_some()),
        );
        zeile(&s, "caller", lc);
        zeile(&s, "server", ls);

        s.unblock(c); // das tut der Kernel, wenn der Ruf abbricht / der Partner stirbt
        b.k("M2.nach_unblock.in_liste", ja(in_liste(&s, lc).is_some()));
        b.k("M2.nach_unblock.depleted", ja(s.tcbs[lc].depleted));
        b.k("M2.nach_unblock.remaining", s.tcbs[lc].remaining as i64);
        b.k("M2.nach_unblock.audit", s.audit() as i64);
        zeile(&s, "caller", lc);

        s.on_tick(0, F, false);
        b.k("M2.alternative_bei_wahl", ja(in_liste(&s, li).is_some()));
        b.k("M2.wird_current", ja(s.current == Some(lc)));
    }

    // ==============================================================================================
    // M3 -- die Zahlenreihen. (a) mit pause/unblock je Runde, (b) ohne -- die Kontrolle.
    // ==============================================================================================
    fn m3(b: &mut Bericht) {
        const RUNDEN: usize = 6;

        println!("  M3a Reihe MIT pause/unblock je Runde");
        let (mut s, idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        let li = lok(&s, idle);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        let mut dc = Vec::new();
        let mut nr = Vec::new();
        let mut dp = Vec::new();
        let mut ec = Vec::new();
        let mut leer_ticks = 0i64;
        dc.push(s.depleted_count as i64);
        nr.push(s.tcbs[lt].next_refill as i64);
        dp.push(s.budget_stats().0 as i64);
        ec.push(echte_erschoepfte(&s));
        for r in 0..RUNDEN {
            s.pause(t);
            s.unblock(t);
            s.on_tick(0, F, false); // t wird gewaehlt
            let lief_leer = s.current == Some(lt)
                && s.tcbs[lt].remaining == 0
                && s.tcbs[lt].depleted
                && in_liste(&s, li).is_some(); // ... und eine Alternative stand bereit
            s.on_tick(0, F, true); // ein voller Tick auf leerem Konto
            if lief_leer {
                leer_ticks += 1;
            }
            dc.push(s.depleted_count as i64);
            nr.push(s.tcbs[lt].next_refill as i64);
            dp.push(s.budget_stats().0 as i64);
            ec.push(echte_erschoepfte(&s));
            let _ = r;
        }
        println!("     depleted_count : {dc:?}");
        println!("     next_refill    : {nr:?}");
        println!("     depletions     : {dp:?}");
        println!("     echt erschoepft: {ec:?}");
        println!("     Ticks auf leerem Konto (mit bereitstehender Alternative): {leer_ticks}");
        b.k("M3a.runden", RUNDEN as i64);
        b.k("M3a.leer_ticks", leer_ticks);
        b.k("M3a.depleted_count_ende", *dc.last().unwrap());
        b.k("M3a.echte_erschoepfte_ende", *ec.last().unwrap());
        b.k("M3a.next_refill_drift", nr.last().unwrap() - nr[0]);
        b.k("M3a.depletions_ende", *dp.last().unwrap());

        println!("  M3b Kontrolle: dieselben Ticks OHNE pause/unblock");
        let (mut s2, _i2) = aufbau();
        let t2 = faden(&mut s2, PRIO_T, 1);
        let l2 = lok(&s2, t2);
        assert!(s2.set_budget(t2, BUDGET, PERIOD), "set_budget");
        s2.on_tick(0, F, false);
        for _ in 0..BUDGET {
            s2.on_tick(0, F, true);
        }
        let mut dc2 = vec![s2.depleted_count as i64];
        let mut nr2 = vec![s2.tcbs[l2].next_refill as i64];
        for _ in 0..(RUNDEN * 2) {
            s2.on_tick(0, F, true);
            dc2.push(s2.depleted_count as i64);
            nr2.push(s2.tcbs[l2].next_refill as i64);
        }
        println!("     depleted_count : {dc2:?}");
        println!("     next_refill    : {nr2:?}");
        b.k("M3b.depleted_count_ende", *dc2.last().unwrap());
        b.k("M3b.next_refill_drift", nr2.last().unwrap() - nr2[0]);
    }

    // ==============================================================================================
    // M4 -- `refill_depleted` reiht einen PAUSIERTEN Thread wieder ein. Zweite, von `unblock`
    //       unabhaengige Ursache: PAUSE allein reicht.
    // ==============================================================================================
    fn m4(b: &mut Bericht) {
        println!("  M4  erschoepfen -> pause -> Refill abwarten (KEIN unblock)");
        let (mut s, idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        let li = lok(&s, idle);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        s.pause(t);
        b.k("M4.vor_refill.blocked", ja(s.tcbs[lt].blocked));
        b.k("M4.vor_refill.in_liste", ja(in_liste(&s, lt).is_some()));
        b.k("M4.vor_refill.audit", s.audit() as i64);

        let (_, refills_vor) = s.budget_stats();
        let mut ticks = 0i64;
        while s.budget_stats().1 == refills_vor && ticks < (PERIOD as i64 + 10) {
            s.on_tick(0, F, true);
            ticks += 1;
        }
        b.k("M4.ticks_bis_refill", ticks);
        b.k("M4.refill_passiert", ja(s.budget_stats().1 > refills_vor));
        // Der Refill laeuft am ANFANG von `on_tick`; danach waehlt derselbe Aufruf den
        // naechsten Thread. Gemessen wird also der Zustand unmittelbar nach diesem Aufruf.
        b.k("M4.nach_refill.blocked", ja(s.tcbs[lt].blocked));
        b.k("M4.nach_refill.in_liste", ja(in_liste(&s, lt).is_some()));
        b.k("M4.nach_refill.audit", s.audit() as i64);
        b.k("M4.idle_bereit", ja(in_liste(&s, li).is_some()));
        // ==> DIE WIRKUNG: laeuft ein PAUSIERTER Thread wieder?
        b.k("M4.pausierter_ist_current", ja(s.current == Some(lt)));
        b.k("M4.beim_laufen_blocked", ja(s.tcbs[lt].blocked));
        zeile(&s, "nach refill", lt);
        // Und wie lange? Der naechste Umplanungsschritt deplaniert ihn (requeue = !blocked).
        s.on_tick(0, F, true);
        b.k("M4.eine_runde_spaeter_current", ja(s.current == Some(lt)));

        // --- M4b: derselbe Fehler, aber mit einem hoeher priorisierten Laeufer daneben.
        // Dann bleibt der pausierte Thread in der Liste STEHEN, statt sofort gewaehlt zu
        // werden -- und `audit()` sieht ihn.
        println!("  M4b dasselbe, aber ein hoeher priorisierter Thread laeuft daneben");
        let (mut s, _idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        s.pause(t);
        let hog = faden(&mut s, PRIO_T + 1, 2); // hoehere Prioritaet, unbeschraenkt
        let _ = hog;
        let (_, refills_vor) = s.budget_stats();
        let mut ticks = 0i64;
        while s.budget_stats().1 == refills_vor && ticks < (PERIOD as i64 + 10) {
            s.on_tick(0, F, true);
            ticks += 1;
        }
        b.k("M4b.refill_passiert", ja(s.budget_stats().1 > refills_vor));
        b.k("M4b.nach_refill.blocked", ja(s.tcbs[lt].blocked));
        b.k("M4b.nach_refill.in_liste", ja(in_liste(&s, lt).is_some()));
        b.k("M4b.nach_refill.audit", s.audit() as i64);
        zeile(&s, "nach refill", lt);
    }

    // ==============================================================================================
    // M5 -- nach dem Refill: `depleted_count` gegen die wirkliche Zahl erschoepfter Konten.
    // ==============================================================================================
    fn m5(b: &mut Bericht) {
        println!("  M5  Drift des Zaehlers ueber einen vollstaendigen Refill hinweg");
        let (mut s, _idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        // Drei pause/unblock-Runden, jede kostet einen Tick auf leerem Konto.
        for _ in 0..3 {
            s.pause(t);
            s.unblock(t);
            s.on_tick(0, F, false);
            s.on_tick(0, F, true);
        }
        b.k("M5.vor_refill.depleted_count", s.depleted_count as i64);
        b.k("M5.vor_refill.echte_erschoepfte", echte_erschoepfte(&s));
        let (_, refills_vor) = s.budget_stats();
        let mut ticks = 0i64;
        while s.budget_stats().1 == refills_vor && ticks < 4 * PERIOD as i64 {
            s.on_tick(0, F, true);
            ticks += 1;
        }
        b.k("M5.refill_passiert", ja(s.budget_stats().1 > refills_vor));
        b.k("M5.nach_refill.depleted_count", s.depleted_count as i64);
        b.k("M5.nach_refill.echte_erschoepfte", echte_erschoepfte(&s));
        b.k(
            "M5.zaehler_luegt_um",
            s.depleted_count as i64 - echte_erschoepfte(&s),
        );
        b.k("M5.scan_laeuft_weiter_je_tick", ja(s.depleted_count > 0));
        zeile(&s, "nach refill", lt);
    }

    // ==============================================================================================
    // M6 -- die VERWECHSLUNGSKONTROLLE. Dieselbe Reihe entsteht ohne jedes `unblock`, sobald
    //       nichts anderes lauffaehig ist: `dequeue_highest() == None` laesst `current` stehen.
    //       Deshalb traegt "wird current" die Aussage nur ZUSAMMEN mit "eine Alternative war da".
    // ==============================================================================================
    fn m6(b: &mut Bericht) {
        println!("  M6  Verwechslungskontrolle: kein anderer Thread lauffaehig, kein unblock");
        let (mut s, idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false); // t wird current, idle steht in der Liste
        s.pause(idle); // ... und wird herausgenommen: keine Alternative mehr
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        let mut dc = vec![s.depleted_count as i64];
        let mut nr = vec![s.tcbs[lt].next_refill as i64];
        for _ in 0..5 {
            s.on_tick(0, F, true);
            dc.push(s.depleted_count as i64);
            nr.push(s.tcbs[lt].next_refill as i64);
        }
        println!("     depleted_count : {dc:?}");
        println!("     next_refill    : {nr:?}");
        b.k("M6.depleted_count_ende", *dc.last().unwrap());
        b.k("M6.next_refill_drift", nr.last().unwrap() - nr[0]);
        b.k("M6.bleibt_current", ja(s.current == Some(lt)));
        b.k("M6.unblock_kam_vor", 0);
    }

    // ==============================================================================================
    // M7 -- die Frage an JEDE Behebung: verhungert er? Wer den Thread nicht einreiht, muss
    //       zeigen, dass ihn spaeter jemand einreiht. Eine halbe Behebung ist schlimmer als
    //       der Befund.
    // ==============================================================================================
    fn m7(b: &mut Bericht) {
        println!("  M7  Verhungert er? erschoepfen -> pause -> unblock -> 3 Perioden zusehen");
        let (mut s, _idle) = aufbau();
        let t = faden(&mut s, PRIO_T, 1);
        let lt = lok(&s, t);
        assert!(s.set_budget(t, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        s.pause(t);
        s.unblock(t); // das RESUME kam an -- was macht diese Fassung daraus?
        b.k("M7.nach_unblock.blocked", ja(s.tcbs[lt].blocked));

        let (_, refills_vor) = s.budget_stats();
        let mut mit_budget = 0i64; // Ticks, die er mit ECHTEM Restbudget verbraucht hat
        let mut auf_leer = 0i64; // Ticks, die er auf leerem Konto verbraucht hat
        for _ in 0..(3 * PERIOD as i64) {
            let laeuft = s.current == Some(lt);
            let leer = s.tcbs[lt].remaining == 0 && s.tcbs[lt].depleted;
            s.on_tick(0, F, true);
            if laeuft && leer {
                auf_leer += 1;
            } else if laeuft {
                mit_budget += 1;
            }
        }
        b.k("M7.refills", (s.budget_stats().1 - refills_vor) as i64);
        b.k("M7.ticks_mit_budget", mit_budget);
        b.k("M7.ticks_auf_leerem_konto", auf_leer);
        b.k("M7.ende.blocked", ja(s.tcbs[lt].blocked));
        b.k(
            "M7.ende.einplanbar",
            ja(in_liste(&s, lt).is_some() || s.current == Some(lt)),
        );
        zeile(&s, "am Ende", lt);
    }

    // ==============================================================================================
    // D-REIHE -- der DONEE-ZWEIG in `refill_depleted` (2026-08-03, der von D8 offen gelassene Punkt)
    //
    //     match self.tcbs[slot].sc_donee {
    //         Some(d) if d != slot => { self.tcbs[d].blocked = false; self.enqueue_ready(d); }
    //         _ => { if !blocked && current != Some(slot) { enqueue_ready(slot) } }
    //     }
    //
    // Der `_`-Zweig hat seit D8 zwei Waechter, der Donee-Zweig KEINEN: er setzt `blocked = false`
    // bedingungslos und reiht bedingungslos ein. Gemessen wird, ob das erreichbar schadet.
    // ==============================================================================================

    /// Ticken, bis ein Refill passiert (oder die Schranke reisst). Rueckgabe: (Ticks, passiert).
    fn bis_refill(s: &mut Scheduler, grenze: i64) -> (i64, bool) {
        let (_, vor) = s.budget_stats();
        let mut t = 0i64;
        while s.budget_stats().1 == vor && t < grenze {
            s.on_tick(0, F, true);
            t += 1;
        }
        (t, s.budget_stats().1 > vor)
    }

    /// Aufrufer (= Konto) ruft Server (= Donee) und erschoepft das Konto, waehrend der Server
    /// laeuft. Danach ist der Donee auf das Konto-Budget geblockt -- der Zustand, den der
    /// Donee-Zweig aufloest. Rueckgabe: (Scheduler, idle, konto, donee) als lokale Slots.
    fn donee_aufbau(budget: u32) -> (Scheduler, usize, usize, usize) {
        let (mut s, idle) = aufbau();
        let a = faden(&mut s, PRIO_T, 1);
        let srv = faden(&mut s, PRIO_T, 2);
        let (la, ls, li) = (lok(&s, a), lok(&s, srv), lok(&s, idle));
        assert!(s.set_budget(a, budget, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        assert_eq!(s.current, Some(la), "der Aufrufer muss laufen");
        s.switch_to(0, F, srv); // IPC-CALL-Fastpath: a blockiert, srv laeuft gegen as Konto
        assert_eq!(s.current, Some(ls), "der Server muss laufen");
        (s, li, la, ls)
    }

    /// Der Handle zu einem lokalen Slot (fuer die Aufrufe, die eine `ThreadId` wollen).
    fn tid(s: &Scheduler, local: usize) -> ThreadId {
        s.id(local)
    }

    // ==============================================================================================
    // P2 -- POSITIVKONTROLLE DES DONEE-ZWEIGS. Ohne sie ist jede Zahl aus D1..D7 wertlos.
    //       Gesunder Fall: der Donee war WIRKLICH auf das Konto-Budget geblockt und wird beim
    //       Refill zu Recht wieder bereit -- und laeuft danach auf einem Konto MIT Budget.
    // ==============================================================================================
    fn p2(b: &mut Bericht) {
        println!("  P2  Positivkontrolle DONEE-ZWEIG -- der gesunde Fall");
        let (mut s, li, la, ls) = donee_aufbau(BUDGET);
        b.k("P2.donee_link", ja(s.tcbs[la].sc_donee == Some(ls)));
        b.k("P2.donee_hat_kein_eigenes_budget", ja(s.tcbs[ls].budget == 0));
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        // Der Zustand, den der Donee-Zweig aufloesen soll.
        b.k("P2.erschoepft.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("P2.erschoepft.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("P2.erschoepft.donee_in_liste", ja(in_liste(&s, ls).is_some()));
        b.k("P2.erschoepft.audit", s.audit() as i64);
        zeile(&s, "donee", ls);
        let (t, passiert) = bis_refill(&mut s, PERIOD as i64 + 10);
        b.k("P2.ticks_bis_refill", t);
        b.k("P2.refill_passiert", ja(passiert));
        b.k("P2.nach_refill.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("P2.nach_refill.donee_current", ja(s.current == Some(ls)));
        b.k("P2.nach_refill.alternative_bereit", ja(in_liste(&s, li).is_some()));
        b.k("P2.nach_refill.konto_remaining", s.tcbs[la].remaining as i64);
        b.k("P2.nach_refill.audit", s.audit() as i64);
        // ==> Die WIRKUNG: laeuft er, und wird dabei ein Konto MIT Budget belastet?
        let r = s.tcbs[la].remaining as i64;
        s.on_tick(0, F, true);
        b.k(
            "P2.tick_auf_gefuelltem_konto",
            ja(r > 0 && s.tcbs[la].remaining as i64 == r - 1),
        );
        zeile(&s, "donee", ls);
    }

    // ==============================================================================================
    // D1 -- (F1) Ist der Donee zum Zeitpunkt des Refills aus einem ANDEREN Grund blockiert?
    //       Die PAUSE wird gesetzt, WAEHREND das Konto noch Budget hat -- damit ist bewiesen,
    //       dass die Blockade die PAUSE ist und nicht das Budget. Danach erschoepft das Konto,
    //       und der Refill hebt beides auf, weil beides dasselbe Bit ist.
    // ==============================================================================================
    fn d1(b: &mut Bericht) {
        println!("  D1  (F1) PAUSE am Donee -- gesetzt VOR der Erschoepfung");
        let (mut s, li, la, ls) = donee_aufbau(2);
        s.on_tick(0, F, true); // 2 -> 1: der Donee laeuft weiter, Konto noch nicht leer
        b.k("D1.vor_pause.donee_current", ja(s.current == Some(ls)));
        b.k("D1.vor_pause.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D1.vor_pause.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("D1.vor_pause.konto_remaining", s.tcbs[la].remaining as i64);
        let ok = s.pause(tid(&s, ls));
        // ==> Die Blockade stammt NACHWEISLICH von der PAUSE: das Konto ist noch nicht leer.
        b.k("D1.pause_meldet_erfolg", ja(ok));
        b.k("D1.nach_pause.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D1.nach_pause.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("D1.nach_pause.depleted_count", s.depleted_count as i64);
        s.on_tick(0, F, true); // 1 -> 0: jetzt erschoepft das Konto
        b.k("D1.nach_erschoepfung.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("D1.nach_erschoepfung.donee_current", ja(s.current == Some(ls)));
        let (t, passiert) = bis_refill(&mut s, PERIOD as i64 + 10);
        b.k("D1.ticks_bis_refill", t);
        b.k("D1.refill_passiert", ja(passiert));
        // ==> DIE MESSUNG: haelt die PAUSE ueber den Refill hinweg?
        b.k("D1.nach_refill.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D1.pausierter_ist_current", ja(s.current == Some(ls)));
        b.k("D1.alternative_bereit", ja(in_liste(&s, li).is_some()));
        b.k("D1.audit", s.audit() as i64);
        let r = s.tcbs[la].remaining as i64;
        s.on_tick(0, F, true);
        b.k(
            "D1.pausierter_verbraucht_budget",
            ja(r > 0 && s.tcbs[la].remaining as i64 == r - 1),
        );
        zeile(&s, "donee", ls);
        // Punkt 6 (die Frage an JEDE Behebung): wer die PAUSE haelt, muss zeigen, dass der
        // Donee nach dem RESUME wieder laeuft. Ein Donee, der nie wieder laeuft, ist schlimmer
        // als einer, der zu frueh laeuft.
        s.unblock(tid(&s, ls)); // RESUME
        let mut lief = 0i64;
        let mut mit_budget = 0i64;
        for _ in 0..(2 * PERIOD as i64) {
            let laeuft = s.current == Some(ls);
            let voll = s.tcbs[la].remaining > 0 && !s.tcbs[la].depleted;
            s.on_tick(0, F, true);
            if laeuft {
                lief += 1;
                if voll {
                    mit_budget += 1;
                }
            }
        }
        b.k("D1.nach_resume.ticks_gelaufen", lief);
        b.k("D1.nach_resume.ticks_mit_budget", mit_budget);
        b.k("D1.nach_resume.blocked", ja(s.tcbs[ls].blocked));

        // --- D1b: dieselbe PAUSE NACH der Erschoepfung. Sie ist ein No-Op (`blocked` steht
        // schon) und meldet trotzdem Erfolg -- die beiden Gruende teilen sich EIN Bit.
        println!("  D1b PAUSE am Donee -- gesetzt NACH der Erschoepfung (No-Op, meldet Erfolg)");
        let (mut s, _li, _la, ls) = donee_aufbau(BUDGET);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        let vor = ja(s.tcbs[ls].blocked);
        let ok = s.pause(tid(&s, ls));
        b.k("D1b.vor_pause.donee_blocked", vor);
        b.k("D1b.pause_meldet_erfolg", ja(ok));
        let (_, passiert) = bis_refill(&mut s, PERIOD as i64 + 10);
        b.k("D1b.refill_passiert", ja(passiert));
        b.k("D1b.pausierter_ist_current", ja(s.current == Some(ls)));
    }

    // ==============================================================================================
    // D2 -- (F2) Kann der Donee SELBST erschoepft sein, waehrend der Donee-Zweig ihn einreiht?
    //       Der Server hat ein EIGENES Konto und hat es leergefahren, bevor der Aufruf kommt.
    //       `switch_to` prueft `depleted` nicht -- der Donee-Zweig danach auch nicht.
    //       Ein hoeher priorisierter Laeufer haelt ihn in der Liste, sonst kann `audit()` ihn
    //       nicht sehen (er waere sofort `current`).
    // ==============================================================================================
    fn d2(b: &mut Bericht) {
        println!("  D2  (F2) der Donee ist SELBST erschoepft, wenn der Refill ihn einreiht");
        let (mut s, _idle) = aufbau();
        let srv = faden(&mut s, PRIO_T, 1); // zuerst gespawnt -> laeuft zuerst
        let a = faden(&mut s, PRIO_T, 2);
        let (la, ls) = (lok(&s, a), lok(&s, srv));
        assert!(s.set_budget(srv, 2, 4 * PERIOD), "set_budget srv");
        // `a` hat hier noch KEIN Budget -- sonst faehrt der Rundlauf beide Konten zugleich leer
        // und die Messung haette zwei Ursachen.
        let mut n = 0;
        while !s.tcbs[ls].depleted && n < 20 {
            s.on_tick(0, F, true);
            n += 1;
        }
        assert!(s.tcbs[ls].depleted, "srv muss sein eigenes Konto leerfahren");
        // Ohne Budgetverbrauch (YIELD) zum Aufrufer umschalten, dann erst sein Konto setzen.
        while s.current != Some(la) && n < 40 {
            s.on_tick(0, F, false);
            n += 1;
        }
        assert!(s.set_budget(a, BUDGET, PERIOD), "set_budget a");
        b.k("D2.srv_eigen_depleted", ja(s.tcbs[ls].depleted));
        b.k("D2.srv_blocked", ja(s.tcbs[ls].blocked));
        b.k("D2.a_ist_current", ja(s.current == Some(la)));
        s.switch_to(0, F, tid(&s, ls)); // CALL an den erschoepften Server
        b.k("D2.nach_call.srv_current", ja(s.current == Some(ls)));
        b.k("D2.nach_call.srv_depleted", ja(s.tcbs[ls].depleted));
        b.k("D2.nach_call.audit", s.audit() as i64);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("D2.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("D2.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D2.echte_erschoepfte", echte_erschoepfte(&s));
        let _hog = faden(&mut s, PRIO_T + 1, 3); // haelt ihn nach dem Refill in der Liste
        let (_, passiert) = bis_refill(&mut s, PERIOD as i64 + 10);
        b.k("D2.refill_passiert", ja(passiert));
        b.k("D2.nach_refill.donee_depleted", ja(s.tcbs[ls].depleted));
        b.k("D2.nach_refill.donee_in_liste", ja(in_liste(&s, ls).is_some()));
        // ==> DIE MESSUNG: Audit-Code 9 ist genau dafuer da (D8, 2026-08-03).
        b.k("D2.nach_refill.audit", s.audit() as i64);
        zeile(&s, "donee", ls);
    }

    // ==============================================================================================
    // D3 -- (F3) Kann ein VERALTETER `sc_donee`-Eintrag hier hereinlaufen?
    //       (a) der Donee stirbt, (b) der Donee migriert. Beides mit Sprechprobe.
    // ==============================================================================================
    fn d3(b: &mut Bericht) {
        println!("  D3  (F3) veralteter sc_donee: gestorbener / migrierter Donee");
        let (mut s, _li, la, ls) = donee_aufbau(BUDGET);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("D3a.vor_kill.sc_donee_zeigt_auf_donee", ja(s.tcbs[la].sc_donee == Some(ls)));
        let gid_vor = s.tcbs[ls].gid as i64;
        assert!(s.kill(tid(&s, ls), 0), "kill Donee");
        b.k("D3a.nach_kill.sc_donee_geloescht", ja(s.tcbs[la].sc_donee.is_none()));
        b.k("D3a.nach_kill.slot_frei", ja(!s.tcbs[ls].used));
        let (_, passiert) = bis_refill(&mut s, PERIOD as i64 + 10);
        b.k("D3a.refill_passiert", ja(passiert));
        b.k("D3a.toter_slot_in_liste", ja(in_liste(&s, ls).is_some()));
        b.k("D3a.nach_refill.audit", s.audit() as i64);
        b.k("D3a.gid_vor", gid_vor);

        // (b) Migration -- kommt ein Donee ueberhaupt aus dem Kern heraus?
        let (mut s, _li, la, ls) = donee_aufbau(BUDGET);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("D3b.donee_nicht_current", ja(s.current != Some(ls)));
        b.k("D3b.konto_nicht_current", ja(s.current != Some(la)));
        b.k("D3b.donee_detach_verweigert", ja(s.detach_for_migration(tid(&s, ls)).is_none()));
        b.k("D3b.konto_detach_verweigert", ja(s.detach_for_migration(tid(&s, la)).is_none()));
        // Sprechprobe: das Messgeraet kann auch JA sagen -- ein Thread OHNE Spende geht heraus.
        let frei = faden(&mut s, PRIO_T, 3);
        b.k("D3b.sprechprobe_fremder_detach_geht", ja(s.detach_for_migration(frei).is_some()));
    }

    // ==============================================================================================
    // D4 -- Das KONTO stirbt, waehrend der Donee auf sein Budget geblockt ist. `record_zombie`
    //       loest die Spende, laesst den Donee aber BLOCKIERT zurueck -- und mit dem Konto
    //       verschwindet der einzige Wecker.
    // ==============================================================================================
    fn d4(b: &mut Bericht) {
        println!("  D4  das KONTO stirbt, waehrend der Donee auf sein Budget geblockt ist");
        let (mut s, _li, la, ls) = donee_aufbau(BUDGET);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("D4.vor_kill.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D4.vor_kill.depleted_count", s.depleted_count as i64);
        assert!(s.kill(tid(&s, la), 0), "kill Konto");
        b.k("D4.nach_kill.donee_sc_donor_geloescht", ja(s.tcbs[ls].sc_donor.is_none()));
        b.k("D4.nach_kill.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D4.nach_kill.depleted_count", s.depleted_count as i64);
        b.k("D4.nach_kill.audit", s.audit() as i64);
        let mut lief = 0i64;
        for _ in 0..(3 * PERIOD as i64) {
            if s.current == Some(ls) {
                lief += 1;
            }
            s.on_tick(0, F, true);
        }
        // ==> DIE MESSUNG: laeuft er je wieder? (Ein Donee, der nie wieder laeuft, ist
        // schlimmer als einer, der zu frueh laeuft.)
        b.k("D4.ticks_donee_lief", lief);
        b.k("D4.ende.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D4.ende.donee_in_liste", ja(in_liste(&s, ls).is_some()));
        b.k("D4.ende.audit", s.audit() as i64);
        zeile(&s, "donee", ls);
    }

    // ==============================================================================================
    // D5 -- VERSCHACHTELTE Spende (fs -> Blockdienst -> Treiber ist genau diese Form).
    //       `sc_donee` ist EIN Slot, die Spende aber ein STAPEL: der zweite CALL ueberschreibt
    //       den Eintrag, und der REPLY des inneren Servers loescht ihn ganz. Der aeussere
    //       Server behaelt seinen `sc_donor` -- und wird spaeter gegen dasselbe Konto belastet.
    //       Kein Privileg noetig: zwei CALLs und ein REPLY.
    // ==============================================================================================
    fn d5(b: &mut Bericht) {
        println!("  D5  verschachtelte Spende: der innere REPLY loescht sc_donee");
        let (mut s, _idle) = aufbau();
        let a = faden(&mut s, PRIO_T, 1); // Wurzelkonto (der Mandant)
        let mid = faden(&mut s, PRIO_T, 2); // mittlerer Server
        let inn = faden(&mut s, PRIO_T, 3); // innerer Server
        let (la, lm, lin) = (lok(&s, a), lok(&s, mid), lok(&s, inn));
        assert!(s.set_budget(a, BUDGET, PERIOD), "set_budget");
        s.on_tick(0, F, false);
        assert_eq!(s.current, Some(la), "a muss laufen");
        s.switch_to(0, F, mid); // CALL a -> mid
        b.k("D5.nach_call1.sc_donee_ist_mid", ja(s.tcbs[la].sc_donee == Some(lm)));
        s.switch_to(0, F, inn); // CALL mid -> inn, dasselbe Wurzelkonto
        b.k("D5.nach_call2.sc_donee_ist_inn", ja(s.tcbs[la].sc_donee == Some(lin)));
        b.k("D5.nach_call2.mid_sc_donor_bleibt", ja(s.tcbs[lm].sc_donor == Some(la)));
        b.k("D5.nach_call2.mid_blocked", ja(s.tcbs[lm].blocked));
        // REPLY des inneren Servers -- genau die Folge aus `sel4lake_ipc::Endpoint::reply`:
        // erst `end_donation`, dann `unblock(caller)`.
        s.end_donation(0);
        s.unblock(tid(&s, lm));
        b.k("D5.nach_reply.sc_donee_ist_none", ja(s.tcbs[la].sc_donee.is_none()));
        b.k("D5.nach_reply.mid_sc_donor_bleibt", ja(s.tcbs[lm].sc_donor == Some(la)));
        b.k("D5.nach_reply.mid_in_liste", ja(in_liste(&s, lm).is_some()));
        s.block_current(0, F); // der innere Server geht zurueck ins RECV
        b.k("D5.mid_ist_current", ja(s.current == Some(lm)));
        // `mid` laeuft weiter gegen das Konto von `a` -- bis es erschoepft.
        let mut n = 0;
        while !s.tcbs[la].depleted && n < 10 {
            s.on_tick(0, F, true);
            n += 1;
        }
        b.k("D5.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("D5.mid_blocked", ja(s.tcbs[lm].blocked));
        b.k(
            "D5.sc_donee_beim_erschoepfen",
            match s.tcbs[la].sc_donee {
                Some(d) => d as i64,
                None => -1,
            },
        );
        b.k("D5.audit", s.audit() as i64);
        let mut lief = 0i64;
        for _ in 0..(3 * PERIOD as i64) {
            if s.current == Some(lm) {
                lief += 1;
            }
            s.on_tick(0, F, true);
        }
        // ==> DIE MESSUNG: der Refill kommt, aber niemand weckt `mid`.
        b.k("D5.refills", s.budget_stats().1 as i64);
        b.k("D5.ticks_mid_lief", lief);
        b.k("D5.ende.mid_blocked", ja(s.tcbs[lm].blocked));
        b.k("D5.ende.mid_in_liste", ja(in_liste(&s, lm).is_some()));
        b.k("D5.ende.a_blocked", ja(s.tcbs[la].blocked));
        b.k("D5.ende.a_depleted", ja(s.tcbs[la].depleted));
        b.k("D5.ende.audit", s.audit() as i64);
        zeile(&s, "mid", lm);
        zeile(&s, "a (Konto)", la);
    }

    // ==============================================================================================
    // D6 -- `unblock` prueft das EIGENE Konto des Threads, nicht das belastete. Bei einem Donee
    //       sind das zwei verschiedene. Damit steht D8 ueber diesen Zweig noch offen.
    // ==============================================================================================
    fn d6(b: &mut Bericht) {
        println!("  D6  `unblock` am Donee: geprueft wird das eigene, belastet das fremde Konto");
        let (mut s, li, la, ls) = donee_aufbau(BUDGET);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("D6.vor_unblock.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D6.vor_unblock.donee_eigen_depleted", ja(s.tcbs[ls].depleted));
        b.k("D6.vor_unblock.konto_depleted", ja(s.tcbs[la].depleted));
        s.unblock(tid(&s, ls)); // RESUME am Server (PdControl) bzw. `thaw_thread`
        b.k("D6.nach_unblock.donee_in_liste", ja(in_liste(&s, ls).is_some()));
        b.k("D6.nach_unblock.audit", s.audit() as i64);
        let dep_vor = s.budget_stats().0 as i64;
        let dc_vor = s.depleted_count as i64;
        s.on_tick(0, F, false);
        b.k("D6.wird_current", ja(s.current == Some(ls)));
        b.k("D6.alternative_bei_wahl", ja(in_liste(&s, li).is_some()));
        b.k("D6.konto_remaining_beim_laufen", s.tcbs[la].remaining as i64);
        s.on_tick(0, F, true); // ein voller Tick auf leerem Konto
        b.k("D6.depletions_delta", s.budget_stats().0 as i64 - dep_vor);
        b.k("D6.depleted_count_delta", s.depleted_count as i64 - dc_vor);
        b.k("D6.echte_erschoepfte", echte_erschoepfte(&s));
        b.k("D6.zaehler_luegt_um", s.depleted_count as i64 - echte_erschoepfte(&s));
    }

    // ==============================================================================================
    // D7 -- `set_budget` auf das Konto raeumt `depleted` weg -- und damit den einzigen Anlass,
    //       aus dem der Donee je wieder geweckt wuerde. (`kernel/src/threads/mod.rs:5578` prueft
    //       genau diese Strandung -- aber nur fuer das Konto selbst, nicht fuer seinen Donee.)
    // ==============================================================================================
    fn d7(b: &mut Bericht) {
        println!("  D7  `set_budget` auf das Konto strandet den Donee");
        let (mut s, _li, la, ls) = donee_aufbau(BUDGET);
        for _ in 0..BUDGET {
            s.on_tick(0, F, true);
        }
        b.k("D7.vor.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D7.vor.depleted_count", s.depleted_count as i64);
        assert!(s.set_budget(tid(&s, la), BUDGET, PERIOD), "set_budget erneut");
        b.k("D7.nach.konto_depleted", ja(s.tcbs[la].depleted));
        b.k("D7.nach.depleted_count", s.depleted_count as i64);
        b.k("D7.nach.donee_blocked", ja(s.tcbs[ls].blocked));
        let mut lief = 0i64;
        for _ in 0..(3 * PERIOD as i64) {
            if s.current == Some(ls) {
                lief += 1;
            }
            s.on_tick(0, F, true);
        }
        b.k("D7.ticks_donee_lief", lief);
        b.k("D7.ende.donee_blocked", ja(s.tcbs[ls].blocked));
        b.k("D7.ende.audit", s.audit() as i64);
    }

    // ==============================================================================================

    pub fn run(fassung: &str) -> i32 {
        println!("== Messung am echten sel4lake-sched (Fassung: {fassung}) ==");
        let mut b = Bericht { zeilen: Vec::new() };
        p0(&mut b);
        p1(&mut b);
        m1(&mut b);
        m2(&mut b);
        m3(&mut b);
        m4(&mut b);
        m5(&mut b);
        m6(&mut b);
        m7(&mut b);
        p2(&mut b);
        d1(&mut b);
        d2(&mut b);
        d3(&mut b);
        d4(&mut b);
        d5(&mut b);
        d6(&mut b);
        d7(&mut b);

        // Die Positivkontrolle entscheidet, ob irgendeine andere Zahl etwas wert ist.
        let hol = |n: &str| -> i64 {
            b.zeilen
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| *v)
                .unwrap_or(-999)
        };
        let pk = hol("P0.nach_spawn.in_liste") == 1
            && hol("P0.nach_pause.in_liste") == 0      // das Messgeraet kann NEIN sagen
            && hol("P0.nach_unblock.in_liste") == 1
            && hol("P0.liste_gleich_flag") == 1
            && hol("P0.audit") == 0
            && hol("P1.t_ist_current") == 1
            && hol("P1.vor_unblock.blocked") == 1
            && hol("P1.vor_unblock.depleted") == 0
            && hol("P1.vor_unblock.in_liste") == 0
            && hol("P1.vor_unblock.current") == 0
            && hol("P1.nach_unblock.in_liste") == 1
            && hol("P1.depleted_count") == 0
            && hol("P1.depletions") == 0
            && hol("P1.audit") == 0
            && hol("P1.wird_current") == 1
            // --- Positivkontrolle des DONEE-Zweigs (2026-08-03). Ohne sie ist keine Zahl aus
            //     D1..D7 etwas wert -- und eine Behebung, die den gesunden Donee nicht mehr
            //     weckt, faellt HIER durch statt still zu verhungern.
            && hol("P2.donee_link") == 1
            && hol("P2.erschoepft.konto_depleted") == 1
            && hol("P2.erschoepft.donee_blocked") == 1
            && hol("P2.erschoepft.donee_in_liste") == 0 // das Messgeraet kann NEIN sagen
            && hol("P2.erschoepft.audit") == 0
            && hol("P2.refill_passiert") == 1
            && hol("P2.nach_refill.donee_blocked") == 0
            && hol("P2.nach_refill.donee_current") == 1
            && hol("P2.nach_refill.alternative_bereit") == 1
            && hol("P2.nach_refill.audit") == 0
            && hol("P2.tick_auf_gefuelltem_konto") == 1;

        println!();
        println!("  -- Werte ({}) --", fassung);
        for (k, v) in &b.zeilen {
            println!("  {k}={v}");
        }
        println!();
        println!(
            "  POSITIVKONTROLLE: {}",
            if pk { "BESTANDEN" } else { "DURCHGEFALLEN" }
        );
        println!("-- {} Messwerte, Fassung {} --", b.zeilen.len(), fassung);
        if pk {
            0
        } else {
            1
        }
    }
}

fn main() {
    let f = std::env::args().nth(1).unwrap_or_else(|| "echt".to_string());
    std::process::exit(messung::run(&f));
}
RSEOF
}

# ================================================================================================
# 2. Bauen.
# ================================================================================================
no_std_raus() {   # no_std_raus <quelle> <ziel> <muster>
    python3 - "$1" "$2" "$3" <<'PY'
import io, sys
q, z, m = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    s = io.open(q, encoding='utf-8').read()
except OSError as e:
    sys.exit("FEHLER: %s nicht lesbar (%s)." % (q, e))
if m not in s:
    sys.exit("FEHLER: %r in %s NICHT GEFUNDEN.\n"
             "        Das Werkzeug nimmt an, dies sei eine no_std-Kernel-Crate; ist sie es\n"
             "        nicht, stimmt seine Annahme ueber den Gegenstand nicht mehr." % (m, q))
s = s.replace(m, "// %s -- fuer den Host-Lauf des Messwerkzeugs entfernt." % m.strip(), 1)
io.open(z, 'w', encoding='utf-8').write(s)
PY
}

einbetten() {   # einbetten <quelle> <modulname> <muster>  -> stdout
    python3 - "$1" "$2" "$3" <<'PY'
import io, sys
q, name, m = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    s = io.open(q, encoding='utf-8').read()
except OSError as e:
    sys.exit("FEHLER: %s nicht lesbar (%s)." % (q, e))
if m not in s:
    sys.exit("FEHLER: %r in %s NICHT GEFUNDEN." % (m, q))
s = s.replace(m, "// %s -- als Modul eingebettet." % m.strip(), 1)
print("// Aus %s uebernommen -- ECHTER Quelltext, unveraendert bis auf die no_std-Zeile." % q)
print("mod %s {" % name)
for z in s.splitlines():
    print(("    " + z) if z.strip() else z)
print("}")
PY
}

harness_bauen() {   # harness_bauen <lib.rs> <arbeitsverzeichnis>
    local code="$1" W="$2" ausgabe

    for f in "$code" "$CYCLES_STD" "$SLAB_STD" "$SYNC_STD"; do
        [ -f "$f" ] || { echo "  FEHLER: $f fehlt." >&2; return 2; }
    done

    if ! no_std_raus "$code" "$W/harness.rs" '#![no_std]'; then return 2; fi
    cp "$CYCLES_STD" "$W/cycles.rs" || return 2

    if ! ausgabe="$(einbetten "$SLAB_STD" sel4lake_slab '#![no_std]' 2>&1)"; then
        printf '%s\n' "$ausgabe" >&2; return 2
    fi
    printf '%s\n' "$ausgabe" >> "$W/harness.rs"
    if ! ausgabe="$(einbetten "$SYNC_STD" sel4lake_sync '#![cfg_attr(not(loom), no_std)]' 2>&1)"; then
        printf '%s\n' "$ausgabe" >&2; return 2
    fi
    printf '%s\n' "$ausgabe" >> "$W/harness.rs"

    anbau_schreiben "$W/anbau.rs"
    cat "$W/anbau.rs" >> "$W/harness.rs"

    rm -f "$W/mess.bin"
    if ! ausgabe="$($RUSTC --edition 2021 -A warnings "$W/harness.rs" -o "$W/mess.bin" 2>&1)"; then
        echo "  FEHLER: das Messwerkzeug liess sich nicht uebersetzen." >&2
        printf '%s\n' "$ausgabe" | sed 's/^/    /' >&2
        echo "    Ein Uebersetzungsfehler ist hier ein BEFUND: der echte Quelltext passt nicht" >&2
        echo "    mehr zu dem, was das Werkzeug von ihm annimmt." >&2
        return 2
    fi
    [ -x "$W/mess.bin" ] || { echo "  FEHLER: kein lauffaehiges Binary." >&2; return 2; }
    return 0
}

fahren() {   # fahren <arbeitsverzeichnis> <fassung> [--leise]
    local W="$1" fassung="$2" leise="${3:-}" ausgabe rc n
    ausgabe="$("$W/mess.bin" "$fassung" 2>&1)"; rc=$?
    n="$(printf '%s\n' "$ausgabe" | sed -n 's/^-- \([0-9]*\) Messwerte.*/\1/p')"
    if [ -z "$n" ] || [ "$n" -lt 1 ]; then
        echo "  FEHLER: kein Messwert entstanden -- ein leerer Lauf ist kein Ergebnis." >&2
        printf '%s\n' "$ausgabe" | sed 's/^/    /' >&2
        return 2
    fi
    printf '%s\n' "$ausgabe" > "$W/ausgabe.txt"
    [ -n "$leise" ] || printf '%s\n' "$ausgabe" | sed 's/^/  /'
    return "$rc"
}

mutieren() {   # mutieren <zieldatei> <python-programm>
    python3 - "$1" "$2" <<'PY'
import io, sys
p, prog = sys.argv[1], sys.argv[2]
s = io.open(p, encoding='utf-8').read()
vorher = s
ns = {'s': s}
exec(prog, ns)
if ns['s'] == vorher:
    sys.exit("FEHLER: die Mutation hat NICHTS geaendert -- der Anker passt nicht mehr.\n"
             "        Eine Gegenprobe an unveraendertem Quelltext ist keine Gegenprobe.")
io.open(p, 'w', encoding='utf-8').write(ns['s'])
PY
}

wert() {   # wert <ausgabedatei> <schluessel>
    sed -n "s/^  $2=\(-\?[0-9]*\)$/\1/p" "$1" | head -1
}

# ================================================================================================
# 3. Ablauf.
# ================================================================================================
NUR_ECHT=0
[ "${1:-}" = "--nur-echt" ] && NUR_ECHT=1

echo "== sched-erschoepfung-messen: die GELESENEN Befunde B1/B2 ausloesen =="
echo
echo "  Herkunft (der Gegenstand, nicht eine Nachbildung):"
for f in "$CODE_STD" "$CYCLES_STD" "$SLAB_STD" "$SYNC_STD"; do
    printf '    %-44s %5s Zeilen  %s\n' "${f#$ROOT/}" "$(wc -l < "$f")" "$(sha256sum "$f" | cut -c1-12)"
done
echo

W_ECHT="$WURZEL/echt"; mkdir -p "$W_ECHT"
harness_bauen "$CODE_STD" "$W_ECHT" || exit 2
fahren "$W_ECHT" echt; RC_ECHT=$?
[ "$RC_ECHT" = 2 ] && exit 2
echo

if [ "$RC_ECHT" != 0 ]; then
    echo "ABBRUCH: die Positivkontrolle ist durchgefallen. Jede weitere Zahl waere wertlos." >&2
    exit 1
fi
[ "$NUR_ECHT" = 1 ] && exit 0

# --- Gegenproben ---------------------------------------------------------------------------
#
# **Die Anker sind der GEGENSTAND, nicht seine Geschichte.** Bis zum 2026-08-03 standen hier die
# Mutationen der D8-Messung; nach deren Behebung passte kein einziger Anker mehr, und die
# Gegenproben brachen mit "der Anker passt nicht mehr" ab -- die Messung am echten Quelltext lief
# weiter gruen. Eine Gegenprobe, die nach der Behebung ihres eigenen Befundes stumm abbricht, ist
# genau die Form aus der Fallenliste ("ein Waechter, der nach seiner eigenen Behebung
# weiterschreit"). Die Mutationen unten beziehen sich deshalb auf den HEUTIGEN Quelltext.

# V0 -- der Stand VOR D8. Sprechprobe der Gegenprobenmechanik: die alten Befunde muessen hier
#       wieder auftauchen, sonst misst der Vergleich nichts.
V0_UNBLOCK='s = s.replace("""            if !self.tcbs[s].depleted {
                self.enqueue_ready(s);
            }""", """            self.enqueue_ready(s);""", 1)'
V0_REFILL='s = s.replace("""                    _ => {
                        if !self.tcbs[slot].blocked && self.current != Some(slot) {
                            self.enqueue_ready(slot);
                        }
                    }""", """                    _ => self.enqueue_ready(slot),""", 1)'
V0_AUDIT='s = s.replace("""                if t.depleted {
                    return 9;
                }
""", "", 1)'

# H-a -- der Waechter des `_`-Zweigs WOERTLICH in den Donee-Zweig uebertragen. Sieht symmetrisch
#        aus und ist es nicht: der Donee ist an dieser Stelle IMMER blockiert (`on_tick` hat ihn
#        gerade blockiert), der Zweig wird also nie wirksam.
H_A='s = s.replace("""                    Some(d) if d != slot => {
                        // Der Donee war auf das Konto-Budget geblockt -> wieder bereit.
                        self.tcbs[d].blocked = false;
                        self.enqueue_ready(d);
                    }""", """                    Some(d) if d != slot => {
                        // H-a: der woertlich uebertragene Waechter aus dem `_`-Zweig.
                        if !self.tcbs[d].blocked && self.current != Some(d) {
                            self.enqueue_ready(d);
                        }
                    }""", 1)'

# H-b -- der Vorschlag: der GRUND der Blockade wird mitgeschrieben (`budget_blocked`), und der
#        Refill weckt genau die Threads, die WEGEN DIESES KONTOS blockiert sind -- statt des
#        einen Slots in `sc_donee`, den der zweite CALL ueberschreibt und der innere REPLY loescht.
H_B1='s = s.replace("""    /// True, wenn das Budget erschöpft ist (Thread weder laufend noch in Ready-Queue,
    /// wartet auf Refill).
    depleted: bool,""", """    /// True, wenn das Budget erschöpft ist (Thread weder laufend noch in Ready-Queue,
    /// wartet auf Refill).
    depleted: bool,
    /// H-b: blockiert, WEIL das belastete Konto leer ist -- im Unterschied zu einer Blockade
    /// aus IPC oder PAUSE. `blocked` allein kann das nicht sagen, und genau daran haengt der
    /// Donee-Zweig: er hebt eine Blockade auf, deren Grund er nicht kennt.
    budget_blocked: bool,""", 1)'
H_B2='s = s.replace("""        depleted: false,
""", """        depleted: false,
        budget_blocked: false,
""", 1)'
H_B3='s = s.replace("""                    if acct != cur {
                        // `cur` ist ein Donee, der gegen ein fremdes (erschöpftes) Konto
                        // lief -> auf den Refill blocken (nicht „verloren": blocked=true,
                        // der Refill des Kontos macht ihn wieder bereit).
                        self.tcbs[cur].blocked = true;
                    }""", """                    if acct != cur && !self.tcbs[cur].blocked {
                        // H-b: nur wer nicht schon aus einem ANDEREN Grund blockiert ist, wird
                        // hier blockiert -- und der Grund wird mitgeschrieben.
                        self.tcbs[cur].blocked = true;
                        self.tcbs[cur].budget_blocked = true;
                    }""", 1)'
H_B4='s = s.replace("""                match self.tcbs[slot].sc_donee {
                    Some(d) if d != slot => {
                        // Der Donee war auf das Konto-Budget geblockt -> wieder bereit.
                        self.tcbs[d].blocked = false;
                        self.enqueue_ready(d);
                    }""", """                // H-b: die Spende ist ein STAPEL. `sc_donee` ist nur ihre Spitze, wird vom
                // zweiten CALL ueberschrieben und vom inneren REPLY geloescht -- geweckt wird
                // deshalb, wer WEGEN DIESES KONTOS blockiert ist, und nur der.
                for d in 0..self.tcbs.len() {
                    if d != slot
                        && self.tcbs[d].used
                        && self.tcbs[d].budget_blocked
                        && self.tcbs[d].sc_donor == Some(slot)
                    {
                        self.tcbs[d].budget_blocked = false;
                        self.tcbs[d].blocked = false;
                        self.enqueue_ready(d);
                    }
                }
                match self.tcbs[slot].sc_donee {
                    Some(d) if d != slot => {
                        let _ = d; // erledigt der Lauf darueber
                    }""", 1)'
H_B5='s = s.replace("""        if self.tcbs[s].blocked {
""", """        if self.tcbs[s].blocked {
            // H-b: WARUM ist er blockiert? Eine Blockade auf ein leeres Konto hebt nur der
            // Refill auf. Der Waechter ist hier -- anders als G-a -- gefahrlos, weil der Wecker
            // BENANNT ist: `refill_depleted` weckt genau `budget_blocked`.
            if self.tcbs[s].budget_blocked {
                return true;
            }
            let acct = self.tcbs[s].sc_donor.unwrap_or(s);
            if acct != s && self.tcbs[acct].depleted {
                // Zurueck in den Lauf ja -- aber nicht auf ein leeres Konto. Die Blockade
                // wechselt den GRUND, und der neue Grund hat einen Wecker.
                self.tcbs[s].budget_blocked = true;
                return true;
            }
""", 1)'
H_B6='s = s.replace("""        if let Some(d) = self.tcbs[local].sc_donee {
            self.tcbs[d].sc_donor = None;
        }""", """        // H-b: ALLE Empfaenger dieser Spende loesen, nicht nur die Spitze -- und wer auf
        // dieses Konto geblockt war, verliert mit ihm seinen Wecker und muss hier frei.
        for d in 0..self.tcbs.len() {
            if d != local && self.tcbs[d].used && self.tcbs[d].sc_donor == Some(local) {
                self.tcbs[d].sc_donor = None;
                if self.tcbs[d].budget_blocked {
                    self.tcbs[d].budget_blocked = false;
                    self.tcbs[d].blocked = false;
                    self.enqueue_ready(d);
                }
            }
        }""", 1)'
H_B7='s = s.replace("""            if was_depleted {
                self.depleted_count -= 1;
                if self.current != Some(s) && !self.tcbs[s].blocked {""", """            if was_depleted {
                self.depleted_count -= 1;
                // H-b: mit `depleted` verschwindet der Anlass, aus dem der Donee je wieder
                // geweckt wuerde -- also hier wecken.
                for d in 0..self.tcbs.len() {
                    if d != s
                        && self.tcbs[d].used
                        && self.tcbs[d].budget_blocked
                        && self.tcbs[d].sc_donor == Some(s)
                    {
                        self.tcbs[d].budget_blocked = false;
                        self.tcbs[d].blocked = false;
                        self.enqueue_ready(d);
                    }
                }
                if self.current != Some(s) && !self.tcbs[s].blocked {""", 1)'
H_B8='s = s.replace("""        if !self.tcbs[s].blocked {
            self.tcbs[s].blocked = true;""", """        // H-b: PAUSE UEBERNIMMT die Blockade. Ohne diese Zeile ist sie ein No-Op an einem
        // Thread, der schon auf sein Konto-Budget geblockt ist (er ist ja `blocked`) -- und der
        // Refill hebt sie dann mit auf, obwohl PAUSE Erfolg gemeldet hat.
        self.tcbs[s].budget_blocked = false;
        if !self.tcbs[s].blocked {
            self.tcbs[s].blocked = true;""", 1)'

echo "== Gegenproben (Mutationen ausschliesslich auf KOPIEN) =="
for fassung in V0 H-a H-b; do
    Wg="$WURZEL/$fassung"; mkdir -p "$Wg"
    cp "$CODE_STD" "$Wg/lib.rs" || exit 2
    case "$fassung" in
        V0)  mutieren "$Wg/lib.rs" "$V0_UNBLOCK" || exit 2
             mutieren "$Wg/lib.rs" "$V0_REFILL"  || exit 2
             mutieren "$Wg/lib.rs" "$V0_AUDIT"   || exit 2 ;;
        H-a) mutieren "$Wg/lib.rs" "$H_A" || exit 2 ;;
        H-b) for teil in "$H_B1" "$H_B2" "$H_B3" "$H_B4" "$H_B5" "$H_B6" "$H_B7" "$H_B8"; do
                 mutieren "$Wg/lib.rs" "$teil" || exit 2
             done ;;
    esac
    harness_bauen "$Wg/lib.rs" "$Wg" || exit 2
    fahren "$Wg" "$fassung" --leise; rc=$?
    if [ "$rc" = 2 ]; then exit 2; fi
    pk="BESTANDEN"; [ "$rc" != 0 ] && pk="DURCHGEFALLEN"
    echo "  $fassung gebaut und gefahren -- Positivkontrolle: $pk"
done
echo

# --- Vergleich der entscheidenden Groessen ---------------------------------------------------
SCHLUESSEL="
M1.nach_unblock.in_liste
M1.wird_current
M1.depletions_delta
M4.nach_refill.in_liste
M4.pausierter_ist_current
M4b.nach_refill.audit
M5.zaehler_luegt_um
M7.ticks_mit_budget
M7.ende.blocked
P2.nach_refill.donee_blocked
P2.nach_refill.donee_current
P2.tick_auf_gefuelltem_konto
D1.nach_pause.donee_blocked
D1.nach_refill.donee_blocked
D1.pausierter_ist_current
D1.alternative_bereit
D1.pausierter_verbraucht_budget
D1.nach_resume.ticks_gelaufen
D1.nach_resume.ticks_mit_budget
D1.nach_resume.blocked
D1b.pausierter_ist_current
D2.nach_refill.donee_depleted
D2.nach_refill.donee_in_liste
D2.nach_refill.audit
D3a.nach_kill.sc_donee_geloescht
D3a.toter_slot_in_liste
D3b.donee_detach_verweigert
D3b.sprechprobe_fremder_detach_geht
D4.nach_kill.donee_blocked
D4.ticks_donee_lief
D4.ende.donee_blocked
D4.ende.audit
D5.sc_donee_beim_erschoepfen
D5.refills
D5.ticks_mid_lief
D5.ende.mid_blocked
D5.ende.audit
D6.nach_unblock.donee_in_liste
D6.wird_current
D6.depletions_delta
D6.zaehler_luegt_um
D7.ticks_donee_lief
D7.ende.donee_blocked
"

printf '  %-36s %6s %6s %6s %6s\n' "Groesse" "echt" "V0" "H-a" "H-b"
printf '  %-36s %6s %6s %6s %6s\n' "------------------------------------" \
       "------" "------" "------" "------"
for k in $SCHLUESSEL; do
    ve="$(wert "$W_ECHT/ausgabe.txt" "$k")"
    v0="$(wert "$WURZEL/V0/ausgabe.txt" "$k")"
    va="$(wert "$WURZEL/H-a/ausgabe.txt" "$k")"
    vb="$(wert "$WURZEL/H-b/ausgabe.txt" "$k")"
    printf '  %-36s %6s %6s %6s %6s\n' "$k" "${ve:--}" "${v0:--}" "${va:--}" "${vb:--}"
done
echo
echo "  Lesart:"
echo "   * V0 ist der Stand VOR D8 -- die Sprechprobe der Gegenprobenmechanik. Tauchen dort die"
echo "     alten Befunde (M1/M4/M5) nicht wieder auf, misst der ganze Vergleich nichts."
echo "   * H-a ist der woertlich uebertragene Waechter. Er faellt in der POSITIVKONTROLLE durch:"
echo "     der Donee ist an dieser Stelle IMMER blockiert, also weckt ihn niemand mehr."
echo "   * H-b traegt den GRUND der Blockade mit. D1/D4/D5/D6/D7 muessen verschwinden UND"
echo "     D1.nach_resume.ticks_mit_budget > 0 bleiben -- sonst ist es dieselbe halbe Behebung"
echo "     wie G-a bei D8."
echo "   * D3a/D3b sind das Ergebnis 'nicht ausloesbar': ein veralteter sc_donee entsteht nicht,"
echo "     weil Tod ihn loescht und Migration verweigert wird (Sprechprobe: ohne Spende geht sie)."
exit 0
