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
use sel4lake_ipc::EndpointTable;
use sel4lake_mem::Rights;
use sel4lake_sched::{Scheduler, ThreadId};

const NPDS: usize = 16;
/// Cap-Slots je PD-Cspace.
const NCAPS: usize = 16;

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
pub fn dispatch(
    frame: usize,
    core: usize,
    sched: &mut Scheduler,
    cspace: &CapSpace,
    eps: &mut EndpointTable,
    pds: &PdTable,
) -> usize {
    let nr = frame_reg(frame, reg::SYSNO_RESULT);
    if nr == sys::YIELD {
        return sched.on_tick(core, frame);
    }
    if nr == sys::PARK {
        // Selbst-Park: Aufrufer blockieren (verlässt die Ready-Queue dauerhaft);
        // kein Capability nötig. Kehrt nie zum Aufrufer zurück.
        return sched.block_current(core, frame);
    }

    let deny = |code: u64| -> usize {
        frame_set_reg(frame, reg::SYSNO_RESULT, code);
        frame
    };

    let thread = sched.current_id(core);
    let Some(pd) = pds.pd_of(thread) else {
        return deny(result::ERR_NOPD);
    };
    let local = frame_reg(frame, reg::EP_BADGE) as usize;
    let Some(cap) = pds.cap_at(pd, local) else {
        return deny(result::ERR_BADCAP);
    };
    let Some((ObjectKind::Endpoint(ep_id), rights)) = cspace.lookup(cap) else {
        return deny(result::ERR_BADCAP);
    };
    let ep = ep_id as usize;

    let need = match nr {
        sys::CALL => Rights::WRITE,
        sys::RECV | sys::REPLY => Rights::READ,
        _ => return deny(result::ERR_BADSYS),
    };
    if !rights.contains(need) {
        return deny(result::ERR_RIGHTS);
    }

    match nr {
        sys::CALL => eps.call(sched, core, ep, frame),
        sys::RECV => eps.recv(sched, core, ep, frame),
        sys::REPLY => eps.reply(sched, core, ep, frame),
        _ => frame, // bereits oben behandelt
    }
}
