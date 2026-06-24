#![no_std]
//! Synchrone IPC über Endpoints (ADR 0004).
//!
//! Ein **Endpoint** ist ein Rendezvous-Punkt: Sender (Aufrufer) und Empfänger
//! (Server) treffen sich, Nachrichten-Register werden zwischen ihren TrapFrames
//! kopiert. Trifft kein Partner ein, blockiert der Thread (verlässt die
//! Ready-Queue des Schedulers); beim Rendezvous wird über den
//! Scheduler-Fastpath direkt zum Partner gewechselt.
//!
//! Phase 5 implementiert das synchrone **Call/Recv/Reply**-Muster (RPC). Jede
//! Operation gibt den als Nächstes fortzusetzenden TrapFrame zurück (passend zum
//! Trap-Pfad in `sel4lake-hal::exception`).
//!
//! Reine Orchestrierung über Scheduler + Frame-Accessoren — **kein eigenes
//! `unsafe`** (der Frame-Zugriff ist in der HAL gekapselt).

use sel4lake_abi::{reg, result, MSG_WORDS};
use sel4lake_hal::exception::{frame_reg, frame_set_reg};
use sel4lake_sched::{SchedOps, ThreadId};

const NENDPOINTS: usize = 32;
const NNOTIFICATIONS: usize = 32;
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

#[derive(Clone, Copy)]
struct Endpoint {
    used: bool,
    /// Blockierte Aufrufer, die auf einen Empfänger warten.
    senders: TidQueue,
    /// Blockierte Empfänger, die auf einen Aufrufer warten.
    receivers: TidQueue,
    /// Der zuletzt empfangene Aufrufer, der auf eine Antwort wartet.
    caller: Option<ThreadId>,
}

impl Endpoint {
    const EMPTY: Endpoint = Endpoint {
        used: false,
        senders: TidQueue::EMPTY,
        receivers: TidQueue::EMPTY,
        caller: None,
    };
}

/// Tabelle aller Endpoints.
pub struct EndpointTable {
    eps: [Endpoint; NENDPOINTS],
}

impl Default for EndpointTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Nachrichten-Register (Datenwörter + Tag) von `src` nach `dst` kopieren.
fn transfer(src: usize, dst: usize) {
    for i in 0..MSG_WORDS {
        frame_set_reg(dst, reg::MSG0 + i, frame_reg(src, reg::MSG0 + i));
    }
    frame_set_reg(dst, reg::TAG, frame_reg(src, reg::TAG));
}

impl EndpointTable {
    pub const fn new() -> Self {
        Self {
            eps: [Endpoint::EMPTY; NENDPOINTS],
        }
    }

    /// Einen neuen Endpoint anlegen; gibt seine ID zurück.
    pub fn create(&mut self) -> Option<usize> {
        let i = self.eps.iter().position(|e| !e.used)?;
        self.eps[i] = Endpoint {
            used: true,
            ..Endpoint::EMPTY
        };
        Some(i)
    }

    fn valid(&self, ep: usize) -> bool {
        ep < NENDPOINTS && self.eps[ep].used
    }

    /// Der aktuell auf eine Antwort wartende Aufrufer eines Endpoints (für den
    /// Capability-Transfer bei `REPLY`).
    pub fn caller(&self, ep: usize) -> Option<ThreadId> {
        if self.valid(ep) {
            self.eps[ep].caller
        } else {
            None
        }
    }

    /// Einen blockierten Empfänger von einem Endpoint zurückziehen (Hot-Reload).
    /// Der Thread bleibt anschließend blockiert (geparkt) und bedient den
    /// Endpoint nicht mehr. Gibt `true`, falls er Empfänger war.
    pub fn retire_receiver(&mut self, ep: usize, tid: ThreadId) -> bool {
        if self.valid(ep) {
            self.eps[ep].receivers.remove(tid)
        } else {
            false
        }
    }

    /// `CALL`: Nachricht senden und auf Antwort warten. Gibt den fortzusetzenden
    /// Frame zurück (immer ein anderer Thread, da der Aufrufer blockiert).
    /// **Kern-übergreifend:** Liegt der Empfänger auf einem anderen Kern, wird seine
    /// Nachricht in seinen Frame übertragen und er per `unblock` (+IPI) geweckt;
    /// der Aufrufer blockiert auf seinem Kern. Auf demselben Kern: Rendezvous-
    /// Fastpath (`switch_to`).
    pub fn call(&mut self, ops: &mut dyn SchedOps, core: usize, ep: usize, frame: usize) -> usize {
        if !self.valid(ep) {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        let caller = ops.current_id(core);
        if let Some(server) = self.eps[ep].receivers.dequeue() {
            // Empfänger wartet -> Nachricht in seinen (blockierten) Frame übertragen.
            let sframe = ops.frame_of(server).expect("server frame");
            transfer(frame, sframe);
            frame_set_reg(sframe, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(sframe, reg::EP_BADGE, 0);
            self.eps[ep].caller = Some(caller);
            if server.core() == core {
                ops.switch_to(core, frame, server) // intra-Kern: direkt zum Server
            } else {
                ops.unblock(server); // anderer Kern: Server dort wecken (+IPI)
                ops.block_current(core, frame) // Aufrufer blockiert, nächster lokaler Thread
            }
        } else {
            // Kein Empfänger -> Aufrufer reiht sich als Sender ein und blockiert.
            self.eps[ep].senders.enqueue(caller);
            ops.block_current(core, frame)
        }
    }

    /// `RECV`: auf einen Aufrufer warten. Gibt den fortzusetzenden Frame zurück
    /// (der eigene, falls sofort ein Sender da war; sonst ein anderer Thread).
    pub fn recv(&mut self, ops: &mut dyn SchedOps, core: usize, ep: usize, frame: usize) -> usize {
        if !self.valid(ep) {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        let server = ops.current_id(core);
        if let Some(sender) = self.eps[ep].senders.dequeue() {
            // Aufrufer (ggf. auf anderem Kern, blockiert) wartet -> dessen Nachricht
            // in den eigenen Frame übernehmen. Der Aufrufer bleibt blockiert (wartet
            // auf die Antwort); kein Wecken nötig.
            let cframe = ops.frame_of(sender).expect("sender frame");
            transfer(cframe, frame);
            frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(frame, reg::EP_BADGE, 0);
            self.eps[ep].caller = Some(sender); // diesem wird geantwortet
            frame // Server läuft sofort weiter (kein Wechsel)
        } else {
            // Kein Aufrufer -> Server reiht sich als Empfänger ein und blockiert.
            self.eps[ep].receivers.enqueue(server);
            ops.block_current(core, frame)
        }
    }

    /// `REPLY`: dem zuletzt empfangenen Aufrufer antworten (entblockt ihn, ggf.
    /// kern-übergreifend per `unblock`+IPI). Der Server läuft weiter (kein Wechsel).
    pub fn reply(&mut self, ops: &mut dyn SchedOps, core: usize, ep: usize, frame: usize) -> usize {
        let _ = core;
        if !self.valid(ep) {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        if let Some(caller) = self.eps[ep].caller.take() {
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

#[derive(Clone, Copy)]
struct Notification {
    used: bool,
    /// Akkumulierte Badge-Bits noch nicht abgeholter Signale.
    pending: u64,
    /// Blockierter Wartender (Phase: ein Konsument je Notification).
    waiter: Option<ThreadId>,
}

impl Notification {
    const EMPTY: Notification = Notification {
        used: false,
        pending: 0,
        waiter: None,
    };
}

/// Tabelle aller Notification-Objekte.
pub struct NotificationTable {
    ntfns: [Notification; NNOTIFICATIONS],
}

impl Default for NotificationTable {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationTable {
    pub const fn new() -> Self {
        Self {
            ntfns: [Notification::EMPTY; NNOTIFICATIONS],
        }
    }

    pub fn create(&mut self) -> Option<usize> {
        let i = self.ntfns.iter().position(|n| !n.used)?;
        self.ntfns[i] = Notification {
            used: true,
            ..Notification::EMPTY
        };
        Some(i)
    }

    fn valid(&self, n: usize) -> bool {
        n < NNOTIFICATIONS && self.ntfns[n].used
    }

    /// `SIGNAL`: `badge` ins Notification-Wort ODERn und einen etwaigen Wartenden
    /// wecken. **Nicht blockierend** — der Signalgeber läuft weiter (`frame`).
    pub fn signal(&mut self, ops: &mut dyn SchedOps, ntfn: usize, badge: u64, frame: usize) -> usize {
        if !self.valid(ntfn) {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        self.ntfns[ntfn].pending |= badge;
        if let Some(w) = self.ntfns[ntfn].waiter.take() {
            // Wartender (ggf. auf anderem Kern) -> Badge in seinen Frame, dann wecken
            // (kern-übergreifend per unblock+IPI).
            if let Some(wframe) = ops.frame_of(w) {
                frame_set_reg(wframe, reg::SYSNO_RESULT, result::OK);
                frame_set_reg(wframe, reg::EP_BADGE, self.ntfns[ntfn].pending);
                self.ntfns[ntfn].pending = 0;
            }
            ops.unblock(w);
        }
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
        frame
    }

    /// `WAIT`: akkumulierten Badge abholen (sofort, falls vorhanden) oder
    /// blockieren, bis signalisiert wird. Liefert den Badge in `x1`.
    pub fn wait(&mut self, ops: &mut dyn SchedOps, core: usize, ntfn: usize, frame: usize) -> usize {
        if !self.valid(ntfn) {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        if self.ntfns[ntfn].pending != 0 {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(frame, reg::EP_BADGE, self.ntfns[ntfn].pending);
            self.ntfns[ntfn].pending = 0;
            frame
        } else {
            self.ntfns[ntfn].waiter = Some(ops.current_id(core));
            ops.block_current(core, frame)
        }
    }
}
