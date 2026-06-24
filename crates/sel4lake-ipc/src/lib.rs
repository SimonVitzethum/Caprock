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
    /// Der zuletzt empfangene Aufrufer, der auf eine Antwort wartet.
    caller: Option<ThreadId>,
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
        if let Some(server) = self.receivers.dequeue() {
            let sframe = ops.frame_of(server).expect("server frame");
            transfer(frame, sframe);
            frame_set_reg(sframe, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(sframe, reg::EP_BADGE, 0);
            self.caller = Some(caller);
            if server.core() == core {
                ops.switch_to(core, frame, server) // intra-Kern: direkt zum Server
            } else {
                ops.unblock(server); // anderer Kern: Server dort wecken (+IPI)
                ops.block_current(core, frame) // Aufrufer blockiert, nächster lokaler Thread
            }
        } else {
            self.senders.enqueue(caller);
            ops.block_current(core, frame)
        }
    }

    /// `RECV`: auf einen Aufrufer warten. Gibt den fortzusetzenden Frame zurück
    /// (der eigene, falls sofort ein Sender da war; sonst ein anderer Thread).
    pub fn recv(&mut self, ops: &mut dyn SchedOps, core: usize, frame: usize) -> usize {
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        let server = ops.current_id(core);
        if let Some(sender) = self.senders.dequeue() {
            // Aufrufer (ggf. auf anderem Kern, blockiert) -> Nachricht übernehmen.
            // Er bleibt blockiert (wartet auf die Antwort); kein Wecken nötig.
            let cframe = ops.frame_of(sender).expect("sender frame");
            transfer(cframe, frame);
            frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
            frame_set_reg(frame, reg::EP_BADGE, 0);
            self.caller = Some(sender);
            frame // Server läuft sofort weiter (kein Wechsel)
        } else {
            self.receivers.enqueue(server);
            ops.block_current(core, frame)
        }
    }

    /// `REPLY`: dem zuletzt empfangenen Aufrufer antworten (entblockt ihn, ggf.
    /// kern-übergreifend per `unblock`+IPI). Der Server läuft weiter. Ein etwaiger
    /// Capability-Transfer (`grant`) wird vom Kernel **vor** diesem Aufruf erledigt,
    /// solange noch dieser Endpoint-Lock + der CAPS-Lock gehalten werden.
    pub fn reply(&mut self, ops: &mut dyn SchedOps, core: usize, frame: usize) -> usize {
        let _ = core;
        if !self.used {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
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
    pub fn mark_used(&mut self) {
        *self = Notification {
            used: true,
            ..Notification::EMPTY
        };
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
