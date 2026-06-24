#![no_std]
//! Microkit-Runtime im Kernelimage (ADR 0006): **Protection Domains** und
//! **cap-gesicherte IPC**.
//!
//! Eine Protection Domain (PD) ist die Schutz- und Autoritätseinheit: ein Thread
//! plus ein eigener **Capability-Space** — eine kleine Tabelle, die *lokale*
//! Cap-Indizes auf *globale* [`CapPtr`]s (in den einen `CapSpace`, ADR 0003)
//! abbildet. Ein Thread kann nur Endpoints invozieren, für die seine PD eine
//! Capability hält, und nur mit den Rechten dieser Capability — das schließt die
//! Lücke aus Phase 5 (IPC war per ID adressiert, ungated).
//!
//! Es gibt **kein** zweites Capability-System: die Caps leben im globalen
//! `CapSpace` (mit Derivation-Tree); der PD-Cspace ist nur die Indirektion, die
//! Sichtbarkeit/Zugriff pro PD festlegt. Eine über den CDT widerrufene Cap wird
//! im PD-Cspace automatisch ungültig (stale `CapPtr`).
//!
//! Reine Orchestrierung — **kein eigenes `unsafe`**.

use sel4lake_abi::{reg, result, sys};
use sel4lake_cap::{CapPtr, CapSpace, ObjectKind};
use sel4lake_hal::exception::{frame_reg, frame_set_reg};
use sel4lake_ipc::{Endpoint, Notification, NENDPOINTS, NNOTIFICATIONS};
use sel4lake_mem::Rights;
use sel4lake_sched::{SchedOps, ThreadId};
use sel4lake_sync::SpinLock;

const NPDS: usize = 32;
/// Cap-Slots je PD-Cspace.
const NCAPS: usize = 16;

/// Cap-Management-Zustand hinter **einem** Lock (`CAPS` im Kernel): der globale
/// Capability-Space + die Protection-Domain-Tabelle. Cap-Auflösung und -Verwaltung
/// brauchen beides konsistent zusammen.
pub struct Caps {
    pub cspace: CapSpace,
    pub pds: PdTable,
}

impl Default for Caps {
    fn default() -> Self {
        Self::new()
    }
}

impl Caps {
    pub const fn new() -> Self {
        Caps {
            cspace: CapSpace::new(),
            pds: PdTable::new(),
        }
    }
}

/// Eine Protection Domain: ein Thread + ein Capability-Space.
#[derive(Clone, Copy)]
struct Pd {
    used: bool,
    thread: Option<ThreadId>,
    /// Lokaler Cap-Index -> globaler CapPtr.
    cspace: [Option<CapPtr>; NCAPS],
}

impl Pd {
    const EMPTY: Pd = Pd {
        used: false,
        thread: None,
        cspace: [None; NCAPS],
    };
}

/// Tabelle aller Protection Domains.
pub struct PdTable {
    pds: [Pd; NPDS],
}

impl Default for PdTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PdTable {
    pub const fn new() -> Self {
        Self {
            pds: [Pd::EMPTY; NPDS],
        }
    }

    /// Eine neue (leere) PD anlegen; gibt ihre ID zurück.
    pub fn create(&mut self) -> Option<usize> {
        let i = self.pds.iter().position(|p| !p.used)?;
        self.pds[i] = Pd {
            used: true,
            ..Pd::EMPTY
        };
        Some(i)
    }

    /// Den Thread einer PD setzen (Affinität Thread<->PD).
    pub fn bind_thread(&mut self, pd: usize, thread: ThreadId) {
        if pd < NPDS {
            self.pds[pd].thread = Some(thread);
        }
    }

    /// Eine globale Capability in den Cspace einer PD an `slot` eintragen.
    pub fn install_cap(&mut self, pd: usize, slot: usize, cap: CapPtr) {
        if pd < NPDS && slot < NCAPS {
            self.pds[pd].cspace[slot] = Some(cap);
        }
    }

    /// Einen Cap-Slot einer PD leeren (Autorität entziehen — Hot-Reload).
    pub fn clear_cap(&mut self, pd: usize, slot: usize) {
        if pd < NPDS && slot < NCAPS {
            self.pds[pd].cspace[slot] = None;
        }
    }

    fn pd_of(&self, thread: ThreadId) -> Option<usize> {
        self.pds
            .iter()
            .position(|p| p.used && p.thread == Some(thread))
    }

    fn cap_at(&self, pd: usize, slot: usize) -> Option<CapPtr> {
        if slot < NCAPS {
            self.pds[pd].cspace[slot]
        } else {
            None
        }
    }
}

/// Cap-gesicherter Syscall-Dispatch.
///
/// Liest Syscall-Nummer (`x0`) und *lokalen* Cap-Index (`x1`) aus dem Frame,
/// löst die Capability im Cspace der aufrufenden PD auf, prüft Objekttyp und
/// Rechte und führt dann die IPC-Operation aus. Bei fehlender Cap / falschem
/// Recht wird ein Fehlercode gesetzt und (ohne Blockieren) zurückgekehrt.
///
/// Rechte: `CALL` (senden) braucht `WRITE`, `RECV`/`REPLY` (empfangen) `READ`.
/// Der Dispatch besorgt das **Locking selbst** (feinkörnig): `caps` (Cap-Auflösung
/// + -Verwaltung) ist EIN Lock, jedes Endpoint/Notification hat seinen eigenen.
/// Sperrordnung `CAPS < EPS[i]/NTFNS[i] < SCHEDS` (die `SchedOps` sperren intern je
/// Operation genau eine `SCHEDS`-Instanz). Cap-Auflösung sperrt `CAPS` kurz und gibt
/// ihn frei -> IPC auf verschiedenen Endpoints läuft danach parallel. Nur `REPLY`
/// mit Cap-Transfer hält `CAPS` über das Sperren des Endpoints (CAPS->EPS), gibt ihn
/// aber vor dem Rendezvous frei.
pub fn dispatch(
    frame: usize,
    core: usize,
    ops: &mut dyn SchedOps,
    caps: &SpinLock<Caps>,
    eps: &[SpinLock<Endpoint>; NENDPOINTS],
    ntfns: &[SpinLock<Notification>; NNOTIFICATIONS],
) -> usize {
    let nr = frame_reg(frame, reg::SYSNO_RESULT);
    if nr == sys::YIELD {
        return ops.on_tick(core, frame);
    }
    if nr == sys::PARK {
        // Selbst-Park: Aufrufer blockieren (verlässt die Ready-Queue dauerhaft);
        // kein Capability nötig. Kehrt nie zum Aufrufer zurück.
        return ops.block_current(core, frame);
    }
    if nr == sys::EXIT {
        // Selbst-Beenden: Stack/TCB werden zurückgewonnen; kein Capability nötig.
        return ops.exit_current(core, frame);
    }

    let deny = |code: u64| -> usize {
        frame_set_reg(frame, reg::SYSNO_RESULT, code);
        frame
    };

    // Cap-Auflösung unter `CAPS`. Der Guard bleibt nur so lange gehalten, wie nötig
    // (bei REPLY+grant bis nach dem Transfer; sonst wird er vor dem IPC freigegeben).
    let mut caps_guard = caps.lock();
    let thread = ops.current_id(core);
    let Some(pd) = caps_guard.pds.pd_of(thread) else {
        return deny(result::ERR_NOPD);
    };
    let local = frame_reg(frame, reg::EP_BADGE) as usize;
    let Some(cap) = caps_guard.pds.cap_at(pd, local) else {
        return deny(result::ERR_BADCAP);
    };
    let Some((kind, rights, badge)) = caps_guard.cspace.lookup(cap) else {
        return deny(result::ERR_BADCAP);
    };

    match nr {
        sys::CALL | sys::RECV | sys::REPLY => {
            let ObjectKind::Endpoint(ep_id) = kind else {
                return deny(result::ERR_BADCAP);
            };
            let need = if nr == sys::CALL { Rights::WRITE } else { Rights::READ };
            if !rights.contains(need) {
                return deny(result::ERR_RIGHTS);
            }
            let ep = ep_id as usize;
            if ep >= NENDPOINTS {
                return deny(result::ERR_BADCAP);
            }
            match nr {
                sys::CALL => {
                    drop(caps_guard); // CAPS vor dem Endpoint freigeben
                    eps[ep].lock().call(ops, core, frame)
                }
                sys::RECV => {
                    drop(caps_guard);
                    eps[ep].lock().recv(ops, core, frame)
                }
                _ => {
                    // REPLY: ggf. Cap-Transfer (grant) unter CAPS+EPS (Ordnung
                    // CAPS->EPS), dann CAPS freigeben und das Rendezvous fahren.
                    let tag = frame_reg(frame, reg::TAG);
                    let mut e = eps[ep].lock();
                    if tag & sel4lake_abi::GRANT_FLAG != 0 {
                        if let Some(caller) = e.caller() {
                            grant_cap(&mut caps_guard, caller, pd, (tag & 0xff) as usize);
                        }
                    }
                    drop(caps_guard);
                    e.reply(ops, core, frame)
                }
            }
        }
        sys::SIGNAL | sys::WAIT => {
            let ObjectKind::Notification(id) = kind else {
                return deny(result::ERR_BADCAP);
            };
            let need = if nr == sys::SIGNAL { Rights::WRITE } else { Rights::READ };
            if !rights.contains(need) {
                return deny(result::ERR_RIGHTS);
            }
            let n = id as usize;
            if n >= NNOTIFICATIONS {
                return deny(result::ERR_BADCAP);
            }
            drop(caps_guard);
            if nr == sys::SIGNAL {
                ntfns[n].lock().signal(ops, badge, frame)
            } else {
                ntfns[n].lock().wait(ops, core, frame)
            }
        }
        sys::KILL => {
            let ObjectKind::Tcb(raw) = kind else {
                return deny(result::ERR_BADCAP);
            };
            if !rights.contains(Rights::WRITE) {
                return deny(result::ERR_RIGHTS);
            }
            drop(caps_guard);
            let ok = ops.kill(ThreadId::from_raw(raw), core);
            frame_set_reg(frame, reg::SYSNO_RESULT, if ok { result::OK } else { result::ERR_BADCAP });
            frame
        }
        _ => deny(result::ERR_BADSYS),
    }
}

/// Eine Capability vom Server (lokaler Slot `grant_slot` in PD `server_pd`) an den
/// `caller` delegieren: im globalen `CapSpace` ableiten (Kind im CDT) und in den
/// Cspace des Aufrufer-PDs an [`GRANT_RECV_SLOT`](sel4lake_abi::GRANT_RECV_SLOT)
/// eintragen. Läuft unter dem gehaltenen `CAPS`-Lock (vor dem REPLY-Rendezvous, also
/// bevor der Aufrufer geweckt wird/laufen kann).
fn grant_cap(caps: &mut Caps, caller: ThreadId, server_pd: usize, grant_slot: usize) {
    let Some(src) = caps.pds.cap_at(server_pd, grant_slot) else {
        return;
    };
    let Some(cpd) = caps.pds.pd_of(caller) else {
        return;
    };
    // Cap ableiten (erbt die Rechte) und beim Aufrufer eintragen.
    if let Ok(new_cap) = caps.cspace.copy(src, Rights::RWX) {
        caps.pds.install_cap(cpd, sel4lake_abi::GRANT_RECV_SLOT, new_cap);
    }
}
