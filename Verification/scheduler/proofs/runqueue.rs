// SEL4Lake — Phase 5 (Scheduler), Schritt A/B: Runqueue-Konsistenz + MCS-Budget.
//
// Formale Spezifikation der Scheduler-Kern-Invarianten (crate sel4lake-sched, `Scheduler::audit`):
// die Ready-Queue-Mitgliedschaft jedes Threads stimmt EXAKT mit seinem Zustand ueberein
// (kein toter/blockierter/erschoepfter Thread in der Queue, kein Duplikat, der laufende Thread
// nicht zugleich bereit, kein verlorener lauffaehiger Thread) UND die MCS-Budget-Buchhaltung ist
// konsistent (Restbudget <= Budget, Erschoepfung deplaniert, Refill stellt wieder her, ein
// Round-Robin-Thread (budget==0) wird nie erschoepft -> keine Aushungerung).
//
// Sequentielles Einkern-Modell: jede `Scheduler`-Instanz haelt genau einen Kern hinter eigenem
// Lock (ADR 0005). Nebenlaeufigkeit, Prioritaets-Queue-Trennung (audit-Code 6 = strukturell) und
// Budget-Donation (intra-core IPC) bleiben ausserhalb (s. ADR 0019).
//
// Lauf:  tools/verus-verify.sh
//
// ------------------------------------------------------------------------------------------------
// **WIE WEIT DIESES MODELL TRAEGT** — festgehalten von `tools/verus-modelltreue-sched.sh`.
// ------------------------------------------------------------------------------------------------
//
// Dieses Modell hat 7 Thread-Felder und 7 Uebergaenge. `crates/sel4lake-sched/src/lib.rs::Tcb` hat
// 20 Felder, und 20 Funktionen schreiben Scheduler-Zustand. Ein 1:1-Strukturvergleich wie bei
// `unlink` (s. `Verification/capability-system/proofs/cap_space.rs`) waere hier unehrlich — er
// muesste so weit aufgeweicht werden, dass er nicht mehr anschlagen KANN. Der Waechter prueft
// deshalb: Feld- und Uebergangs-ABDECKUNG (echte Kreuzpruefung: kein TCB-Feld und keine
// zustandsschreibende Funktion darf unbenannt bleiben) und haelt je Paar die begruendete
// **Uebertragungsluecke** fest. Was er nicht prueft, steht in seinem Kopf.
//
// Fuenf Befunde aus seinem ersten Lauf (2026-08-03). `lib.rs` blieb unangetastet.
//
// **BEFUND B1 — `unblock` weicht wirklich ab.** Der Code reiht BEDINGUNGSLOS wieder ein:
//
//     if self.tcbs[s].blocked { self.tcbs[s].blocked = false; self.enqueue_ready(s); }
//
// `unblock` unten setzt `in_ready: !t.depleted`. Ein Thread, der blockiert UND MCS-erschoepft ist,
// landet im Code also in der Ready-Liste — `ridx` waere verletzt, `unblock_preserves` gilt fuer
// diesen Zustandsuebergang nicht. Erreichbar: Konto erschoepft (`on_tick`), dann `pause`, dann
// `unblock`. Das Modell nimmt hier NICHT den Code auf, sondern die Invariante — der Beweis sagt,
// was der Code tun muesste, nicht was er tut.
//
// Zwei Folgen, aus dem Quelltext GELESEN, nicht gemessen (wer sie belegen will, braucht einen
// hwfuzz-Fall `budget setzen -> erschoepfen -> PAUSE -> RESUME`):
//   * der Thread ist wieder einplanbar, obwohl `remaining == 0` — bis zum Refill laeuft er auf
//     einem erschoepften Budget. Das ist genau die Laufzeit, die `mcs_bound` ausschliessen soll.
//   * `on_tick` hat im Zweig `remaining == 0` keinen Waechter „war schon erschoepft": laeuft der
//     Thread erneut, wird `depleted_count` ein zweites Mal erhoeht (der Refill senkt es nur
//     einmal -> der Zaehler driftet nach oben, und der Refill-Scan laeuft danach in JEDEM Tick)
//     und `next_refill` wird auf `now + period` zurueckgeschoben — der Refill verschiebt sich,
//     solange der Thread laeuft.
//
// **BEFUND B2 — die Richtung „in der Queue ⟹ nicht erschoepft" hat keinen Audit-Code.**
// `Scheduler::audit` prueft im Queue-Lauf `used` (1), `blocked` (2) und `current` (4), aber nie
// `depleted`. B1 kann zur Laufzeit deshalb nicht auffallen: derselbe Fall, der die Aussage
// widerlegen wuerde, wird nie beobachtet. (Vgl. die leere Event-Queue ohne `CD.R`, CLAUDE.md.)
//
// **BEFUND B3 — `budget_inv` hat ueberhaupt keine Laufzeitentsprechung.** `audit` liest weder
// `budget` noch `remaining`. `mcs_bound`/`roundrobin_no_starve` sind statisch bewiesen und zur
// Laufzeit ungeprueft; der hwfuzz kann sie also nicht als Oracle benutzen.
//
// **BEFUND B4 — `pause` deplaniert den laufenden Thread nicht.** Der Code setzt nur
// `blocked = true` (`remove_from_ready` ist ein No-Op fuer `current`); `pause` unten setzt
// zusaetzlich `current: None`. Der Doc-Kommentar dort nennt das eine Abstraktion („der naechste
// Tick deplaniert ihn") — sie hat ein beobachtbares Fenster: bis zum naechsten Tick ist
// `current_valid` im Code falsch, und es gibt keinen Audit-Code dafuer.
//
// **BEFUND B5 — fuenf Uebergangsklassen des Codes haben hier gar keine Entsprechung.**
// `switch_to` (IPC-Fastpath: blockiert den Aufrufer UND macht den Server laufend, dazu
// Budget-Donation), `exit_current`/`kill`/`record_zombie` (Zombie/Reap), `spawn*`/`init_core`/
// `alloc_tcb` (Erzeugung) und `detach_for_migration`/`attach_migrated` (Migration). Das ist in
// README §10/§11 als offen benannt; der Waechter zaehlt es jetzt mit, damit eine NEUE solche
// Funktion nicht stillschweigend dazukommt.
use vstd::prelude::*;

verus! {

/// Abstrakter Thread-Zustand (TCB-Projektion auf die scheduling-relevanten Felder).
pub struct Thread {
    pub used: bool,       // TCB-Slot belegt
    pub blocked: bool,    // blockiert (IPC-Wait/pause) -> nicht in der Ready-Queue
    pub depleted: bool,   // MCS-Budget erschoepft -> off-queue bis Refill
    pub in_ready: bool,   // steht in einer Ready-Queue (Mitgliedschafts-Flag, modelliert die Queues)
    pub prio: nat,        // Prioritaet (0..NPRIO-1)
    pub budget: nat,      // MCS-Budget je Periode (0 = unbeschraenkt / Round-Robin)
    pub remaining: nat,   // Restbudget der aktuellen Periode
}

/// Ein Kern-Scheduler: TCB-Partition + (ueber `in_ready` modellierte) Ready-Queues + laufender Thread.
pub struct Sched {
    pub threads: Seq<Thread>,
    pub current: Option<nat>,  // lokaler Slot des laufenden Threads
}

/// **Lauffaehig:** belegt, nicht blockiert, Budget nicht erschoepft. Genau diese Threads gehoeren
/// (sofern nicht gerade laufend) in eine Ready-Queue.
pub open spec fn runnable(t: Thread) -> bool {
    t.used && !t.blocked && !t.depleted
}

/// **Runqueue-Kopplung an Slot i** (Herz von `audit`, Codes 1/2/4/7): ein Thread steht **genau dann**
/// in einer Ready-Queue, wenn er lauffaehig und nicht der laufende ist. (Kein Duplikat — Code 3 —
/// ist durch die Modellierung als Mitgliedschafts-*Flag* strukturell ausgeschlossen.)
pub open spec fn ridx(s: Sched, i: int) -> bool {
    s.threads[i].in_ready <==> (runnable(s.threads[i]) && s.current != Some(i as nat))
}

/// Runqueue-Kopplung fuer alle Threads.
pub open spec fn coupled(s: Sched) -> bool {
    forall|i: int| #![trigger s.threads[i]] 0 <= i < s.threads.len() ==> ridx(s, i)
}

/// **MCS-Budget-Kopplung an Slot i:** Restbudget nie groesser als Budget; erschoepft => Rest 0;
/// ein Round-Robin-Thread (budget==0) ist **nie** erschoepft (kann nie ausgehungert werden).
pub open spec fn bidx(s: Sched, i: int) -> bool {
    (s.threads[i].budget > 0 ==> s.threads[i].remaining <= s.threads[i].budget)
    && (s.threads[i].depleted ==> s.threads[i].remaining == 0)
    && (s.threads[i].budget == 0 ==> !s.threads[i].depleted)
}

pub open spec fn budget_inv(s: Sched) -> bool {
    forall|i: int| #![trigger s.threads[i]] 0 <= i < s.threads.len() ==> bidx(s, i)
}

/// Der laufende Thread existiert, ist belegt und selbst lauffaehig (nie blockiert/erschoepft).
pub open spec fn current_valid(s: Sched) -> bool {
    s.current is Some ==> {
        let c = s.current->Some_0 as int;
        0 <= c < s.threads.len() && s.threads[c].used
            && !s.threads[c].blocked && !s.threads[c].depleted
    }
}

/// **Scheduler-Gesamtinvariante.**
pub open spec fn sched_inv(s: Sched) -> bool {
    coupled(s) && budget_inv(s) && current_valid(s)
}

// ===================== Zustandsuebergaenge (spec) =====================

/// **block_current:** der laufende Thread `c` blockiert (IPC-Wait) und wird deplaniert.
pub open spec fn block_current(s: Sched) -> Sched {
    let c = s.current->Some_0 as int;
    let t = s.threads[c];
    Sched {
        threads: s.threads.update(c, Thread { blocked: true, in_ready: false, ..t }),
        current: None,
    }
}

/// **pick:** einen bereiten Thread `n` als naechsten laufenden waehlen (dequeue_highest).
pub open spec fn pick(s: Sched, n: int) -> Sched {
    let t = s.threads[n];
    Sched {
        threads: s.threads.update(n, Thread { in_ready: false, ..t }),
        current: Some(n as nat),
    }
}

/// **unblock:** einen blockierten Thread `i` wieder bereit machen (idempotent im Code; hier auf den
/// blockierten Fall spezifiziert). Wird genau dann wieder eingereiht, wenn nicht erschoepft.
///
/// **ACHTUNG (BEFUND B1, s. Dateikopf):** `Scheduler::unblock` prueft `depleted` NICHT und reiht
/// bedingungslos ein. Dieses `!t.depleted` ist die Forderung der Invariante, nicht der Zustand des
/// Codes. `tools/verus-modelltreue-sched.sh` haelt die Abweichung fest.
pub open spec fn unblock(s: Sched, i: int) -> Sched {
    let t = s.threads[i];
    Sched {
        threads: s.threads.update(i, Thread { blocked: false, in_ready: !t.depleted, ..t }),
        ..s
    }
}

/// **pause:** einen Thread `i` externen blockieren (SYS_PDCTL PAUSE); ist er der laufende, wird er
/// zugleich deplaniert (Abstraktion von „der naechste Tick deplaniert ihn").
///
/// **ACHTUNG (BEFUND B4, s. Dateikopf):** diese Deplanierung ist im Code NICHT sofort. Bis zum
/// naechsten Tick bleibt ein pausierter `current` laufend und blockiert zugleich — in diesem
/// Fenster ist `current_valid` am echten Scheduler falsch, und `audit` hat keinen Code dafuer.
pub open spec fn pause(s: Sched, i: int) -> Sched {
    let t = s.threads[i];
    Sched {
        threads: s.threads.update(i, Thread { blocked: true, in_ready: false, ..t }),
        current: if s.current == Some(i as nat) { None } else { s.current },
    }
}

/// **tick_charge:** ein Zeitscheiben-Tick belastet das Budget des laufenden Threads `c`. Erreicht das
/// Restbudget 0, wird `c` erschoepft + deplaniert; sonst nur dekrementiert.
pub open spec fn tick_charge(s: Sched) -> Sched {
    let c = s.current->Some_0 as int;
    let t = s.threads[c];
    if t.remaining == 1 {
        // Erschoepfung: deplanieren bis zum Refill.
        Sched {
            threads: s.threads.update(c, Thread { remaining: 0, depleted: true, in_ready: false, ..t }),
            current: None,
        }
    } else {
        // Eine Zeitscheibe verbraucht, weiter lauffaehig.
        Sched {
            threads: s.threads.update(c, Thread { remaining: (t.remaining - 1) as nat, ..t }),
            ..s
        }
    }
}

/// **refill:** das Budget eines erschoepften Threads `i` ist abgelaufen -> auffuellen + wieder bereit.
pub open spec fn refill(s: Sched, i: int) -> Sched {
    let t = s.threads[i];
    Sched {
        threads: s.threads.update(i, Thread { remaining: t.budget, depleted: false, in_ready: true, ..t }),
        ..s
    }
}

/// **set_budget:** einem Thread `i` ein neues Budget zuweisen; setzt Erschoepfung zurueck und reiht
/// ihn (falls jetzt lauffaehig und nicht laufend) wieder ein.
pub open spec fn set_budget(s: Sched, i: int, b: nat) -> Sched {
    let t = s.threads[i];
    Sched {
        threads: s.threads.update(i, Thread {
            budget: b,
            remaining: b,
            depleted: false,
            in_ready: !t.blocked && s.current != Some(i as nat),
            ..t
        }),
        ..s
    }
}

// ===================== Bewiesene Eigenschaften =====================

/// **BEWEIS (block_current erhaelt die Invariante).** Der laufende Thread blockiert: er ist danach
/// nicht lauffaehig und nicht laufend -> nicht in der Queue; alle anderen unberuehrt.
pub proof fn block_preserves(s: Sched)
    requires sched_inv(s), s.current is Some,
    ensures sched_inv(block_current(s)),
{
    let s2 = block_current(s);
    let c = s.current->Some_0 as int;
    assert forall|i: int| #![trigger s2.threads[i]] 0 <= i < s2.threads.len() implies ridx(s2, i) by {
        if i != c { assert(s2.threads[i] == s.threads[i]); }
    }
    assert forall|i: int| #![trigger s2.threads[i]] 0 <= i < s2.threads.len() implies bidx(s2, i) by {
        if i != c { assert(s2.threads[i] == s.threads[i]); }
    }
}

/// **BEWEIS (pick erhaelt die Invariante).** Ein bereiter Thread wird laufend: er verlaesst die Queue;
/// die Kopplung bleibt, weil „laufend" genau „nicht bereit" erzwingt.
pub proof fn pick_preserves(s: Sched, n: int)
    requires
        sched_inv(s), s.current is None,
        0 <= n < s.threads.len(), s.threads[n].in_ready,
    ensures sched_inv(pick(s, n)),
{
    let s2 = pick(s, n);
    // Aus der Kopplung folgt: n ist lauffaehig (stand in der Queue, current war None).
    assert(ridx(s, n));
    assert forall|i: int| #![trigger s2.threads[i]] 0 <= i < s2.threads.len() implies ridx(s2, i) by {
        if i != n { assert(s2.threads[i] == s.threads[i]); }
    }
    assert forall|i: int| #![trigger s2.threads[i]] 0 <= i < s2.threads.len() implies bidx(s2, i) by {
        if i != n { assert(s2.threads[i] == s.threads[i]); }
    }
}

/// **BEWEIS (unblock erhaelt die Invariante).**
pub proof fn unblock_preserves(s: Sched, i: int)
    requires
        sched_inv(s), 0 <= i < s.threads.len(),
        s.threads[i].used, s.threads[i].blocked,
    ensures sched_inv(unblock(s, i)),
{
    let s2 = unblock(s, i);
    // Ein blockierter Thread ist nie der laufende (current_valid).
    assert(s.current != Some(i as nat));
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies ridx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies bidx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
}

/// **BEWEIS (pause erhaelt die Invariante).**
pub proof fn pause_preserves(s: Sched, i: int)
    requires
        sched_inv(s), 0 <= i < s.threads.len(), s.threads[i].used,
    ensures sched_inv(pause(s, i)),
{
    let s2 = pause(s, i);
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies ridx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies bidx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
}

/// **BEWEIS (tick_charge erhaelt die Invariante + MCS-Schranke):** ein Tick haelt Restbudget <= Budget
/// und deplaniert bei Erschoepfung sauber (kein Weiterlauf ueber das Budget hinaus).
pub proof fn tick_preserves(s: Sched)
    requires
        sched_inv(s), s.current is Some,
        s.threads[s.current->Some_0 as int].budget > 0,
        s.threads[s.current->Some_0 as int].remaining > 0,
    ensures sched_inv(tick_charge(s)),
{
    let s2 = tick_charge(s);
    let c = s.current->Some_0 as int;
    assert forall|i: int| #![trigger s2.threads[i]] 0 <= i < s2.threads.len() implies ridx(s2, i) by {
        if i != c { assert(s2.threads[i] == s.threads[i]); }
    }
    assert forall|i: int| #![trigger s2.threads[i]] 0 <= i < s2.threads.len() implies bidx(s2, i) by {
        if i != c { assert(s2.threads[i] == s.threads[i]); }
    }
}

/// **BEWEIS (refill erhaelt die Invariante + stellt das Budget wieder her):** der erschoepfte Thread
/// wird wieder lauffaehig (`remaining == budget`, nicht mehr erschoepft) und steht wieder bereit.
pub proof fn refill_preserves(s: Sched, i: int)
    requires
        sched_inv(s), 0 <= i < s.threads.len(),
        s.threads[i].used, s.threads[i].depleted, s.threads[i].budget > 0,
        !s.threads[i].blocked, s.current != Some(i as nat),
    ensures
        sched_inv(refill(s, i)),
        refill(s, i).threads[i].remaining == s.threads[i].budget,
        refill(s, i).threads[i].in_ready,
{
    let s2 = refill(s, i);
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies ridx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies bidx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
}

/// **BEWEIS (set_budget erhaelt die Invariante):** ein erschoepfter Thread geht nicht verloren
/// (Erschoepfung zurueckgesetzt, falls lauffaehig wieder eingereiht) -> kein TCB-Leak/DoS.
pub proof fn setbudget_preserves(s: Sched, i: int, b: nat)
    requires
        sched_inv(s), 0 <= i < s.threads.len(), s.threads[i].used,
    ensures sched_inv(set_budget(s, i, b)),
{
    let s2 = set_budget(s, i, b);
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies ridx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
    assert forall|j: int| #![trigger s2.threads[j]] 0 <= j < s2.threads.len() implies bidx(s2, j) by {
        if j != i { assert(s2.threads[j] == s.threads[j]); }
    }
}

// ===================== Korollare (globale Eigenschaften) =====================

/// **KEIN VERLORENER THREAD (audit-Code 7):** jeder lauffaehige, nicht laufende Thread steht in einer
/// Ready-Queue — er kann nicht „gestrandet" sein.
pub proof fn no_lost_thread(s: Sched, i: int)
    requires
        sched_inv(s), 0 <= i < s.threads.len(),
        runnable(s.threads[i]), s.current != Some(i as nat),
    ensures s.threads[i].in_ready,
{
    assert(ridx(s, i));
}

/// **KEIN TOTER/BLOCKIERTER/ERSCHOEPFTER THREAD IN DER QUEUE (audit-Codes 1/2):** alles in einer
/// Ready-Queue ist lauffaehig.
pub proof fn ready_implies_runnable(s: Sched, i: int)
    requires sched_inv(s), 0 <= i < s.threads.len(), s.threads[i].in_ready,
    ensures runnable(s.threads[i]),
{
    assert(ridx(s, i));
}

/// **MCS-SCHRANKE:** das Restbudget ueberschreitet nie das Budget (keine unbegrenzte Laufzeit).
pub proof fn mcs_bound(s: Sched, i: int)
    requires sched_inv(s), 0 <= i < s.threads.len(), s.threads[i].budget > 0,
    ensures s.threads[i].remaining <= s.threads[i].budget,
{
    assert(bidx(s, i));
}

/// **KEINE AUSHUNGERUNG (Round-Robin):** ein Thread ohne Budget-Grenze (budget==0) wird nie erschoepft
/// -> er bleibt einplanbar, solange belegt und nicht blockiert.
pub proof fn roundrobin_no_starve(s: Sched, i: int)
    requires sched_inv(s), 0 <= i < s.threads.len(), s.threads[i].budget == 0,
    ensures !s.threads[i].depleted,
{
    assert(bidx(s, i));
}

/// **FORTSCHRITT (Idle ist immer bereit):** der Idle-Thread (belegt, nie blockiert, budget==0, nicht
/// laufend) ist stets lauffaehig und steht bereit -> es gibt immer einen waehlbaren Thread (kein
/// Leerlauf-Deadlock; `dequeue_highest` ist nie leer).
pub proof fn idle_always_selectable(s: Sched, idle: int)
    requires
        sched_inv(s), 0 <= idle < s.threads.len(),
        s.threads[idle].used, !s.threads[idle].blocked, s.threads[idle].budget == 0,
        s.current != Some(idle as nat),
    ensures s.threads[idle].in_ready,
{
    assert(bidx(s, idle));         // budget==0 -> !depleted
    assert(runnable(s.threads[idle]));
    assert(ridx(s, idle));
}

fn main() {}

} // verus!
