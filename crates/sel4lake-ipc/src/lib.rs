#![no_std]
//! Synchrone IPC über Endpoints + asynchrone Notifications (ADR 0004).
//!
//! Ein **Endpoint** ist ein Rendezvous-Punkt: Sender (Aufrufer) und Empfänger
//! (Server) treffen sich, Nachrichten-Register werden zwischen ihren TrapFrames
//! kopiert. Trifft kein Partner ein, blockiert der Thread; beim Rendezvous wird
//! über die [`SchedOps`] der Partner geweckt (kern-übergreifend per IPI) bzw. — auf
//! demselben Kern — direkt umgeschaltet.
//!
//! **Feinkörniges Locking:** Jedes [`Endpoint`]/[`Notification`] ist ein
//! eigenständiges Objekt; der Kernel hält je Objekt einen eigenen Lock
//! (`[SpinLock<Endpoint>; N]`). IPC auf verschiedenen Endpoints läuft daher
//! parallel — nur die kurze Cap-Auflösung serialisiert (separater `CAPS`-Lock im
//! Kernel). Die Methoden hier operieren jeweils auf **einem** Objekt (`&mut self`)
//! ohne eigenes Locking; das Locking + die Sperrordnung besorgt der Kernel/Dispatch.
//!
//! Reine Orchestrierung über [`SchedOps`] + Frame-Accessoren — **kein eigenes
//! `unsafe`** (der Frame-Zugriff ist in der HAL gekapselt).

use sel4lake_abi::{reg, result, MSG_WORDS};
use sel4lake_hal::exception::{frame_reg, frame_set_reg};
use sel4lake_sched::{SchedOps, ThreadId};

/// Anzahl Endpoint-Objekte (Größe des per-Endpoint-Lock-Arrays im Kernel).
pub const NENDPOINTS: usize = 32;
/// Anzahl Notification-Objekte.
pub const NNOTIFICATIONS: usize = 32;
const QCAP: usize = 32;

/// FIFO-Warteschlange blockierter Threads (an einem Endpoint).
#[derive(Clone, Copy)]
struct TidQueue {
    buf: [Option<ThreadId>; QCAP],
    head: usize,
    tail: usize,
    count: usize,
}

impl TidQueue {
    const EMPTY: TidQueue = TidQueue {
        buf: [None; QCAP],
        head: 0,
        tail: 0,
        count: 0,
    };

    fn enqueue(&mut self, t: ThreadId) {
        if self.count < QCAP {
            self.buf[self.tail] = Some(t);
            self.tail = (self.tail + 1) % QCAP;
            self.count += 1;
        }
    }

    fn dequeue(&mut self) -> Option<ThreadId> {
        if self.count == 0 {
            return None;
        }
        let t = self.buf[self.head].take();
        self.head = (self.head + 1) % QCAP;
        self.count -= 1;
        t
    }

    /// Iterator über die belegten Einträge (in FIFO-Reihenfolge, für Audits).
    fn for_each(&self, mut f: impl FnMut(ThreadId)) {
        let mut i = self.head;
        for _ in 0..self.count {
            if let Some(t) = self.buf[i] {
                f(t);
            }
            i = (i + 1) % QCAP;
        }
    }

    /// Audit: existiert ein **toter** Eintrag (Prädikat `live` liefert false)?
    fn any_dead(&self, live: &mut dyn FnMut(ThreadId) -> bool) -> bool {
        let mut dead = false;
        self.for_each(|t| {
            if !live(t) {
                dead = true;
            }
        });
        dead
    }

    /// Audit: kommt ein Thread mehrfach vor (Duplikat = Ready-/Queue-Korruption)?
    fn has_dup(&self) -> bool {
        let mut seen: [Option<ThreadId>; QCAP] = [None; QCAP];
        let mut n = 0;
        let mut dup = false;
        self.for_each(|t| {
            if seen[..n].contains(&Some(t)) {
                dup = true;
            }
            seen[n] = Some(t);
            n += 1;
        });
        dup
    }

    /// Einen bestimmten Thread aus der Warteschlange entfernen (für Hot-Reload:
    /// einen blockierten Empfänger zurückziehen). Gibt `true`, falls gefunden.
    fn remove(&mut self, target: ThreadId) -> bool {
        let n = self.count;
        let mut found = false;
        for _ in 0..n {
            if let Some(t) = self.dequeue() {
                if t == target {
                    found = true;
                } else {
                    self.enqueue(t);
                }
            }
        }
        found
    }
}

/// Nachrichten-Register (Datenwörter + Tag) von `src` nach `dst` kopieren.
fn transfer(src: usize, dst: usize) {
    for i in 0..MSG_WORDS {
        frame_set_reg(dst, reg::MSG0 + i, frame_reg(src, reg::MSG0 + i));
    }
    frame_set_reg(dst, reg::TAG, frame_reg(src, reg::TAG));
}

/// Ein Endpoint-Objekt. Der Kernel hält je Endpoint einen eigenen Lock; die
/// Methoden operieren auf genau diesem einen Objekt.
#[derive(Clone, Copy)]
pub struct Endpoint {
    used: bool,
    /// Blockierte Aufrufer, die auf einen Empfänger warten.
    senders: TidQueue,
    /// Blockierte Empfänger, die auf einen Aufrufer warten.
    receivers: TidQueue,
    /// Der zuletzt empfangene Aufrufer, der auf eine Antwort wartet (das **Reply-Token**:
    /// genau ein gültiger Antwort-Empfänger je Rendezvous, beim REPLY automatisch
    /// invalidiert).
    caller: Option<ThreadId>,
    /// Der **Reply-Owner**: der Server-Thread, der `caller` empfangen hat und ihm eine
    /// Antwort schuldet. Stirbt dieser Thread (KILL/EXIT/Fault) vor dem REPLY, wird der
    /// `caller` mit `ERR_SERVER_GONE` entblockt (Liveness — kein dauerhaftes Hängen).
    /// Stets gemeinsam mit `caller` gesetzt/gelöscht.
    reply_owner: Option<ThreadId>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Endpoint {
    pub const EMPTY: Endpoint = Endpoint {
        used: false,
        senders: TidQueue::EMPTY,
        receivers: TidQueue::EMPTY,
        caller: None,
        reply_owner: None,
    };

    /// Ist dieses Objekt belegt (von `create` reserviert)?
    pub fn is_used(&self) -> bool {
        self.used
    }
    /// Als belegt markieren (von der Slot-Reservierung des Kernels).
    pub fn mark_used(&mut self) {
        *self = Endpoint {
            used: true,
            ..Endpoint::EMPTY
        };
    }

    /// Der aktuell auf eine Antwort wartende Aufrufer (für den Cap-Transfer bei
    /// `REPLY`).
    pub fn caller(&self) -> Option<ThreadId> {
        self.caller
    }

    /// Einen blockierten Empfänger zurückziehen (Hot-Reload). Der Thread bleibt
    /// danach blockiert (geparkt). Gibt `true`, falls er Empfänger war.
    pub fn retire_receiver(&mut self, tid: ThreadId) -> bool {
        self.used && self.receivers.remove(tid)
    }

    /// Einen **sterbenden** Thread aus ALLEN Strukturen dieses Endpoints entfernen
    /// (Sender-/Empfänger-Queue + `caller`). Eager-Cleanup beim Thread-Tod: verhindert,
    /// dass tote TCBs in den festen Queues zurückbleiben (Corpse-Fill -> verdrängte
    /// echte Sender) und dass ein REPLY in einen recycelten Frame schreibt. Gibt `true`,
    /// falls der Thread irgendwo eingetragen war.
    pub fn purge_thread(&mut self, tid: ThreadId) -> bool {
        let mut found = self.senders.remove(tid);
        found |= self.receivers.remove(tid);
        if self.caller == Some(tid) {
            // Der sterbende Thread war selbst der wartende Aufrufer -> Token verfällt.
            self.caller = None;
            self.reply_owner = None;
            found = true;
        }
        found
    }

    /// **Reply-Liveness:** stirbt der `owner` (der Server, der eine Antwort schuldet),
    /// bevor er antwortet, verfällt das Reply-Token und der wartende `caller` muss mit
    /// `ERR_SERVER_GONE` entblockt werden. Gibt diesen `caller` zurück (falls vorhanden)
    /// und löscht caller+owner; sonst `None`. Aus den Todespfaden aufzurufen.
    pub fn owner_died(&mut self, owner: ThreadId) -> Option<ThreadId> {
        if self.reply_owner == Some(owner) {
            self.reply_owner = None;
            self.caller.take()
        } else {
            None
        }
    }

    /// **Reply-Cap-Revocation:** einen konkreten ausstehenden `caller` abbrechen (seine
    /// Reply-Cap wurde gelöscht/revoked). Gibt den `caller` zurück (zum Entblocken mit
    /// `ERR_SERVER_GONE`), falls er noch der wartende Aufrufer ist; sonst `None` (Call
    /// bereits beantwortet/anders -> die Reply-Cap war veraltet, No-Op).
    pub fn abort_call(&mut self, caller: ThreadId) -> Option<ThreadId> {
        if self.caller == Some(caller) {
            self.reply_owner = None;
            self.caller.take()
        } else {
            None
        }
    }

    /// **Reply-Cap-Server-Migration (Hot-Reload):** die ausstehende Antwortpflicht des
    /// `old_owner` (das Reload-Opfer) auf die NÄCHSTE Empfänger-Instanz desselben
    /// Endpoints übertragen, OHNE den wartenden Aufrufer abzubrechen. Der Aufrufer wird
    /// wieder als **Sender** eingereiht (er bleibt blockiert; seine ursprüngliche
    /// Nachricht liegt unverändert in seinem Frame). Die nächste `recv`-Instanz (v2)
    /// übernimmt dieselbe Nachricht und wird zum neuen Reply-Owner — der Call wird so von
    /// v2 abgeschlossen statt mit `ERR_SERVER_GONE` zu sterben (Reply-Cap überlebt den
    /// Server-Wechsel). Gibt `true`, falls eine ausstehende Antwortpflicht des `old_owner`
    /// migriert wurde; sonst `false` (kein passender Reply-Owner -> No-Op).
    pub fn migrate_owner(&mut self, old_owner: ThreadId) -> bool {
        if self.used && self.reply_owner == Some(old_owner) {
            if let Some(caller) = self.caller.take() {
                self.reply_owner = None;
                self.senders.enqueue(caller); // erneut zustellbar an die v2-RECV
                return true;
            }
        }
        false
    }

    /// Read-only Audit (Fuzzer-Oracle): prüft beide Queues + `caller` + `reply_owner`
    /// mit einem Lebendigkeits-Prädikat. Gibt `(tote_einträge, duplikat)` zurück. Unter
    /// dem Endpoint-Lock aufzurufen (konsistenter Snapshot).
    pub fn audit(&self, live: &mut dyn FnMut(ThreadId) -> bool) -> (bool, bool) {
        let dead = self.senders.any_dead(live)
            || self.receivers.any_dead(live)
            || self.caller.map_or(false, |c| !live(c))
            || self.reply_owner.map_or(false, |o| !live(o));
        let dup = self.senders.has_dup() || self.receivers.has_dup();
        (dead, dup)
    }

    /// `CALL`: Nachricht senden und auf Antwort warten. Gibt den fortzusetzenden
    /// Frame zurück (immer ein anderer Thread, da der Aufrufer blockiert).
    /// **Kern-übergreifend:** Liegt der Empfänger auf einem anderen Kern, wird die
    /// Nachricht in seinen (blockierten) Frame übertragen und er per `unblock`
    /// (+IPI) geweckt; der Aufrufer blockiert auf seinem Kern. Auf demselben Kern:
    /// Rendezvous-Fastpath (`switch_to`).
    pub fn call(&mut self, ops: &mut dyn SchedOps, core: usize, frame: usize) -> usize {
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        let caller = ops.current_id(core);
        // Einen *lebenden* wartenden Empfänger suchen. Ein zwischenzeitlich
        // gekillter/beendeter Empfänger hat keinen Frame mehr (`frame_of` == None)
        // und bleibt als toter Eintrag in der Queue zurück; solche Leichen werden
        // verworfen statt zu paniken (sonst Kernel-Panik via `.expect`).
        while let Some(server) = self.receivers.dequeue() {
            let Some(sframe) = ops.frame_of(server) else {
                continue; // toter Empfänger -> Eintrag verwerfen, nächsten versuchen
            };
            transfer(frame, sframe);
            frame_set_reg(sframe, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(sframe, reg::EP_BADGE, 0);
            self.caller = Some(caller);
            self.reply_owner = Some(server); // dieser Server schuldet die Antwort
            // Fastpath nur, wenn der Server **auf diesem Kern** lebt. Seit ext-30 kann er
            // migriert sein, deshalb den Besitzer im Thread-Directory nachschlagen (lock-frei)
            // statt ihn aus der ThreadId abzuleiten.
            return if sel4lake_sched::owner_core(server) == Some(core) {
                ops.switch_to(core, frame, server) // intra-Kern: direkt zum Server
            } else {
                ops.unblock(server); // anderer Kern: Server dort wecken (+IPI)
                ops.block_current(core, frame) // Aufrufer blockiert, nächster lokaler Thread
            };
        }
        // Kein lebender Empfänger -> als Sender einreihen und blockieren.
        self.senders.enqueue(caller);
        ops.block_current(core, frame)
    }

    /// `RECV`: auf einen Aufrufer warten. Gibt den fortzusetzenden Frame zurück
    /// (der eigene, falls sofort ein Sender da war; sonst ein anderer Thread).
    pub fn recv(&mut self, ops: &mut dyn SchedOps, core: usize, frame: usize) -> usize {
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        let server = ops.current_id(core);
        // Einen *lebenden* wartenden Aufrufer suchen. Ein zwischenzeitlich
        // gekillter/beendeter Sender hat keinen Frame mehr (`frame_of` == None) und
        // bleibt als toter Eintrag in der Queue; solche Leichen werden verworfen
        // statt zu paniken (sonst Kernel-Panik via `.expect`).
        while let Some(sender) = self.senders.dequeue() {
            // Aufrufer (ggf. auf anderem Kern, blockiert) -> Nachricht übernehmen.
            // Er bleibt blockiert (wartet auf die Antwort); kein Wecken nötig.
            let Some(cframe) = ops.frame_of(sender) else {
                continue; // toter Sender -> Eintrag verwerfen, nächsten versuchen
            };
            transfer(cframe, frame);
            frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(frame, reg::EP_BADGE, 0);
            self.caller = Some(sender);
            self.reply_owner = Some(server); // dieser Server schuldet die Antwort
            return frame; // Server läuft sofort weiter (kein Wechsel)
        }
        // Kein lebender Aufrufer -> als Empfänger einreihen und blockieren.
        self.receivers.enqueue(server);
        ops.block_current(core, frame)
    }

    /// `REPLY`: dem zuletzt empfangenen Aufrufer antworten (entblockt ihn, ggf.
    /// kern-übergreifend per `unblock`+IPI). Der Server läuft weiter. Ein etwaiger
    /// Capability-Transfer (`grant`) wird vom Kernel **vor** diesem Aufruf erledigt,
    /// solange noch dieser Endpoint-Lock + der CAPS-Lock gehalten werden.
    pub fn reply(&mut self, ops: &mut dyn SchedOps, core: usize, frame: usize) -> usize {
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        // Budget-Donation des Servers beenden: der antwortende Server (laufend) gibt das
        // geliehene Konto des Aufrufers frei und läuft wieder auf seinem eigenen Budget.
        ops.end_donation(core);
        // Das Reply-Token einmalig konsumieren (caller + reply_owner); ein zweites
        // REPLY findet `None` -> No-Op (kein Doppel-Reply).
        self.reply_owner = None;
        if let Some(caller) = self.caller.take() {
            if let Some(cframe) = ops.frame_of(caller) {
                transfer(frame, cframe);
                frame_set_reg(cframe, reg::SYSNO_RESULT, result::OK);
            }
            ops.unblock(caller);
        }
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
        frame
    }
}

// ---------------------------------------------------------------------------
// Notifications: asynchrone Badge-Signale (kein Rendezvous).
// ---------------------------------------------------------------------------

/// Ein Notification-Objekt (eigener Lock je Objekt im Kernel).
#[derive(Clone, Copy)]
pub struct Notification {
    used: bool,
    /// Akkumulierte Badge-Bits noch nicht abgeholter Signale.
    pending: u64,
    /// Blockierter Wartender (Phase: ein Konsument je Notification).
    waiter: Option<ThreadId>,
}

impl Default for Notification {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Notification {
    pub const EMPTY: Notification = Notification {
        used: false,
        pending: 0,
        waiter: None,
    };

    pub fn is_used(&self) -> bool {
        self.used
    }
    /// Den aktuell akkumulierten Badge lesen, **ohne** ihn zu konsumieren (Test-/Loader-Telemetrie:
    /// hat jemand signalisiert?). `WAIT` konsumiert ihn regulär.
    pub fn pending_badge(&self) -> u64 {
        self.pending
    }
    pub fn mark_used(&mut self) {
        *self = Notification {
            used: true,
            ..Notification::EMPTY
        };
    }

    /// Einen **sterbenden** Thread als Wartenden entfernen (eager Cleanup beim Tod):
    /// verhindert einen toten Waiter, in dessen recycelten Frame ein späteres SIGNAL
    /// schreiben würde. Gibt `true`, falls er der Wartende war.
    pub fn purge_thread(&mut self, tid: ThreadId) -> bool {
        if self.waiter == Some(tid) {
            self.waiter = None;
            true
        } else {
            false
        }
    }

    /// Read-only Audit (Fuzzer-Oracle): toter Wartender? Unter dem Notification-Lock
    /// aufzurufen.
    pub fn audit(&self, live: &mut dyn FnMut(ThreadId) -> bool) -> bool {
        self.waiter.map_or(false, |w| !live(w))
    }

    /// `SIGNAL`: `badge` ins Notification-Wort ODERn und einen etwaigen Wartenden
    /// wecken (ggf. kern-übergreifend per `unblock`+IPI). **Nicht blockierend.**
    pub fn signal(&mut self, ops: &mut dyn SchedOps, badge: u64, frame: usize) -> usize {
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        self.pending |= badge;
        if let Some(w) = self.waiter.take() {
            if let Some(wframe) = ops.frame_of(w) {
                frame_set_reg(wframe, reg::SYSNO_RESULT, result::OK);
                frame_set_reg(wframe, reg::EP_BADGE, self.pending);
                self.pending = 0;
            }
            ops.unblock(w);
        }
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
        frame
    }

    /// Wie [`signal`](Self::signal), aber **aus dem Kernel** (kein signalisierender Thread/
    /// Frame) — z. B. Deferred-IRQ-Zustellung (ext-22, P5): ein Geräte-Interrupt wird als
    /// Badge-Signal an den wartenden HardwareLand-Backend zugestellt. Setzt den Badge,
    /// entblockt den Waiter (ggf. kern-übergreifend per `unblock`+IPI); schreibt **keinen**
    /// Signalisierer-Frame. Unter dem Notification-Lock aufzurufen (Sperrordnung NTFNS<SCHEDS).
    pub fn signal_from_kernel(&mut self, ops: &mut dyn SchedOps, badge: u64) {
        if !self.used {
            return;
        }
        self.pending |= badge;
        if let Some(w) = self.waiter.take() {
            if let Some(wframe) = ops.frame_of(w) {
                frame_set_reg(wframe, reg::SYSNO_RESULT, result::OK);
                frame_set_reg(wframe, reg::EP_BADGE, self.pending);
                self.pending = 0;
            }
            ops.unblock(w);
        }
    }

    /// `WAIT`: akkumulierten Badge abholen (sofort, falls vorhanden) oder
    /// blockieren, bis signalisiert wird. Liefert den Badge in `x1`.
    pub fn wait(&mut self, ops: &mut dyn SchedOps, core: usize, frame: usize) -> usize {
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        if self.pending != 0 {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(frame, reg::EP_BADGE, self.pending);
            self.pending = 0;
            frame
        } else {
            self.waiter = Some(ops.current_id(core));
            ops.block_current(core, frame)
        }
    }
}
