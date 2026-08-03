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
# **NICHT gemessen:** Nebenlaeufigkeit (der Waechter ist sequentiell, die Kern-Locks bleiben
# ausserhalb), der echte Kontextwechsel (HAL-Stellvertreter), und die Erreichbarkeit aus dem
# Syscall-Pfad des Kernels (dazu braeuchte es einen hwfuzz-Fall; hier wird nur der Aufruf
# nachgestellt, den `kernel/src/system.rs` an dieser Stelle absetzt).
#
# ================================================================================================
# GEGENPROBE
# ================================================================================================
# Dieselbe Folge laeuft an drei Fassungen. Die Mutationen liegen ausschliesslich auf KOPIEN;
# `crates/` wird nie beschrieben.
#   echt  der unveraenderte Quelltext
#   G-a   minimal-woertlich: `if blocked && !depleted` -- der ganze Rumpf entfaellt
#   G-b   modellgetreu: `blocked = false; if !depleted { enqueue_ready }`
#         (das Modell setzt `Thread { blocked: false, in_ready: !t.depleted }`)
#   G-c   G-b + Waechter in `refill_depleted` (`!blocked && current != Some(slot)`)
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
            && hol("P1.wird_current") == 1;

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
G_A='s = s.replace("        if self.tcbs[s].blocked {\n            self.tcbs[s].blocked = false;\n            self.enqueue_ready(s);\n        }",
                   "        if self.tcbs[s].blocked && !self.tcbs[s].depleted {\n            self.tcbs[s].blocked = false;\n            self.enqueue_ready(s);\n        }", 1)'
G_B='s = s.replace("        if self.tcbs[s].blocked {\n            self.tcbs[s].blocked = false;\n            self.enqueue_ready(s);\n        }",
                   "        if self.tcbs[s].blocked {\n            self.tcbs[s].blocked = false;\n            if !self.tcbs[s].depleted {\n                self.enqueue_ready(s);\n            }\n        }", 1)'
G_C_TEIL='s = s.replace("                    _ => self.enqueue_ready(slot),",
                        "                    _ => {\n                        if !self.tcbs[slot].blocked && self.current != Some(slot) {\n                            self.enqueue_ready(slot);\n                        }\n                    }", 1)'

echo "== Gegenproben (Mutationen ausschliesslich auf KOPIEN) =="
for fassung in G-a G-b G-c G-d; do
    Wg="$WURZEL/$fassung"; mkdir -p "$Wg"
    cp "$CODE_STD" "$Wg/lib.rs" || exit 2
    case "$fassung" in
        G-a) mutieren "$Wg/lib.rs" "$G_A" || exit 2 ;;
        G-b) mutieren "$Wg/lib.rs" "$G_B" || exit 2 ;;
        G-c) mutieren "$Wg/lib.rs" "$G_B" || exit 2
             mutieren "$Wg/lib.rs" "$G_C_TEIL" || exit 2 ;;
        # G-d ist die HALBE Behebung: der woertliche Waechter aus G-a **zusammen** mit dem
        # richtigen Waechter im Refill. Beides fuer sich sieht vernuenftig aus.
        G-d) mutieren "$Wg/lib.rs" "$G_A" || exit 2
             mutieren "$Wg/lib.rs" "$G_C_TEIL" || exit 2 ;;
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
P1.nach_unblock.in_liste
P1.wird_current
M1.nach_unblock.in_liste
M1.nach_unblock.depleted
M1.nach_unblock.audit
M1.alternative_bei_wahl
M1.wird_current
M1.depletions_delta
M1.depleted_count_danach
M1.echte_erschoepfte_danach
M1.next_refill_verschoben
M2.nach_unblock.in_liste
M2.wird_current
M3a.leer_ticks
M3a.depleted_count_ende
M3a.echte_erschoepfte_ende
M3a.next_refill_drift
M4.nach_refill.in_liste
M4.nach_refill.audit
M4.pausierter_ist_current
M4.beim_laufen_blocked
M4b.nach_refill.in_liste
M4b.nach_refill.audit
M5.zaehler_luegt_um
M6.depleted_count_ende
M6.next_refill_drift
M7.nach_unblock.blocked
M7.refills
M7.ticks_mit_budget
M7.ticks_auf_leerem_konto
M7.ende.blocked
M7.ende.einplanbar
"

printf '  %-34s %6s %6s %6s %6s %6s\n' "Groesse" "echt" "G-a" "G-b" "G-c" "G-d"
printf '  %-34s %6s %6s %6s %6s %6s\n' "----------------------------------" \
       "------" "------" "------" "------" "------"
for k in $SCHLUESSEL; do
    ve="$(wert "$W_ECHT/ausgabe.txt" "$k")"
    va="$(wert "$WURZEL/G-a/ausgabe.txt" "$k")"
    vb="$(wert "$WURZEL/G-b/ausgabe.txt" "$k")"
    vc="$(wert "$WURZEL/G-c/ausgabe.txt" "$k")"
    vd="$(wert "$WURZEL/G-d/ausgabe.txt" "$k")"
    printf '  %-34s %6s %6s %6s %6s %6s\n' "$k" "${ve:--}" "${va:--}" "${vb:--}" "${vc:--}" "${vd:--}"
done
echo
echo "  Lesart:"
echo "   * Aendert eine Groesse ihren Wert zwischen 'echt' und 'G-b', dann misst sie genau den"
echo "     fehlenden !depleted-Waechter in unblock -- und nichts anderes."
echo "   * Groessen, die in G-a/G-b UNveraendert bleiben (M4/M4b/M6), haben eine ANDERE Ursache."
echo "   * M7.ticks_mit_budget / M7.ende.blocked sind die Verhungerungsprobe: eine Behebung,"
echo "     die das RESUME verschluckt (G-a, G-d), laesst den Thread stehen."
exit 0
