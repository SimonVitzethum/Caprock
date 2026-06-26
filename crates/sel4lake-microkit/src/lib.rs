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

use sel4lake_abi::{pdctl, reg, result, sys};
use sel4lake_cap::{CapPtr, CapSpace, ObjectKind};
use sel4lake_hal::exception::{frame_reg, frame_set_reg};
use sel4lake_ipc::{Endpoint, Notification, NENDPOINTS, NNOTIFICATIONS};
use sel4lake_mem::Rights;
use sel4lake_sched::{SchedOps, ThreadId};
use sel4lake_sync::{RwSpinLock, SpinLock};

const NPDS: usize = 96;
/// Cap-Slots je PD-Cspace.
const NCAPS: usize = 16;

/// **Sicherheitsdomäne** einer Protection Domain (ext-22).
///
/// Drei kernel-getrennte Klassen mit erzwungenen Regeln (siehe `domain_audit`):
/// - [`Domain::TrustedSas`]: läuft im globalen Adressraum (VSPACE_OF==0), nur
///   speichersicheres Rust ohne `unsafe`. Treiber-/Protokolllogik. Default für alle
///   bestehenden PDs (Rückwärtskompatibilität).
/// - [`Domain::HardwareLand`]: isolierte VSpace, darf Hardware-Caps (MMIO/IRQ) halten,
///   kleiner `unsafe`-Hardwarekern. Genau **ein** unveränderlicher Trusted-Partner.
/// - [`Domain::UserLand`]: isolierte VSpace, **keinerlei** Hardware-Rechte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Domain {
    TrustedSas,
    HardwareLand,
    UserLand,
}

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

    /// Eine Cap **policy-geprüft** in den Cspace einer PD eintragen: der Cap-Typ muss in
    /// der Domäne der PD erlaubt sein (Hardware-Caps nur in HardwareLand, `PdControl` nur
    /// in TrustedSas). Gibt `false` zurück (ohne Eintrag), wenn die Policy es verbietet.
    /// Zentraler Enforcement-Punkt — alle Cap-Installationen sollten hierüber laufen.
    pub fn install_cap_checked(&mut self, pd: usize, slot: usize, cap: CapPtr) -> bool {
        let Some(domain) = self.pds.domain_of(pd) else {
            return false;
        };
        let Some((kind, _, _)) = self.cspace.lookup(cap) else {
            return false;
        };
        if !domain_allows_kind(domain, kind) {
            return false;
        }
        // HardwareLand-Backend: Kommunikations-Caps NUR für den eigenen Kanal (paarweise
        // Bindung) — ein Backend darf niemals eine Endpoint-/Notification-Cap zu irgendetwas
        // anderem als seinem Partner-Kanal erhalten.
        if domain == Domain::HardwareLand {
            let (cep, cntfn) = self.pds.chan_of(pd);
            match kind {
                ObjectKind::Endpoint(id) if id != cep => return false,
                ObjectKind::Notification(id) if id != cntfn => return false,
                _ => {}
            }
        }
        self.pds.install_cap(pd, slot, cap);
        true
    }

    /// **Domänen-Policy-Oracle** (ext-22). `0` = konsistent, sonst Anomalie-Code:
    /// - `1` = Hardware-Cap (MMIO/IRQ/DMA) in einer Nicht-HardwareLand-PD.
    /// - `2` = `PdControl`-Cap in einer Nicht-TrustedSas-PD.
    /// - `3` = Isolations-Verletzung: eine **HardwareLand/UserLand**-PD läuft im globalen
    ///   Adressraum (VSPACE_OF==0) statt isoliert. Die sicherheitsrelevante Richtung ist
    ///   „untrusted Domänen MÜSSEN isoliert sein"; eine TrustedSas-PD darf isoliert ODER
    ///   global laufen (mehr Isolation ist nie eine Verletzung — wichtig für Rückwärts-
    ///   kompatibilität, da bestehende isolierte PDs per Default als TrustedSas getaggt sind).
    ///
    /// - `4` = HardwareLand-Backend ohne gültige Partner-Bindung (Partner fehlt oder ist
    ///   keine TrustedSas-PD).
    /// - `5` = HardwareLand-Backend hält eine Endpoint-/Notification-Cap, die NICHT zu seinem
    ///   dedizierten Partner-Kanal gehört (verbotene Kommunikationsbeziehung).
    ///
    /// `vspace_is_global(tid)` meldet, ob der Thread im globalen SAS-Adressraum läuft
    /// (VSPACE_OF==0). Die 1:N-Kardinalität ist strukturell (ein `partner`-Feld); das
    /// UserLand↔HardwareLand-Kommunikationsverbot folgt in P6.
    pub fn domain_audit(&self, is_live_global: &dyn Fn(ThreadId) -> bool) -> u32 {
        for pd in 0..PdTable::capacity() {
            if !self.pds.is_used(pd) {
                continue;
            }
            let domain = match self.pds.domain_of(pd) {
                Some(d) => d,
                None => continue,
            };
            // Cap-Typ-Policy: erlaubte Cap-Typen je Domäne.
            for slot in 0..NCAPS {
                if let Some(cap) = self.pds.cap_at(pd, slot) {
                    if let Some((kind, _, _)) = self.cspace.lookup(cap) {
                        if kind_is_hardware(kind) && domain != Domain::HardwareLand {
                            return 1;
                        }
                        if kind_is_pd_control(kind) && domain != Domain::TrustedSas {
                            return 2;
                        }
                    }
                }
            }
            // Isolations-Mechanismus: untrusted Domänen MÜSSEN isoliert sein. TrustedSas
            // darf global ODER isoliert sein (mehr Isolation ist keine Verletzung).
            if domain != Domain::TrustedSas {
                if let Some(tid) = self.pds.thread_of(pd) {
                    // Verletzung nur, wenn der gebundene Thread LEBT und global läuft
                    // (ein toter/gestoppter Thread zählt nicht).
                    if is_live_global(tid) {
                        return 3;
                    }
                }
            }
            // Paarweise Bindung (HardwareLand-Backend):
            if domain == Domain::HardwareLand {
                // Regel 4: der Partner muss eine (existierende) TrustedSas-PD sein.
                match self.pds.partner_of(pd) {
                    Some(t) if self.pds.domain_of(t) == Some(Domain::TrustedSas) => {}
                    _ => return 4,
                }
                // Regel 5: ein Backend hält NUR Kommunikations-Caps für seinen eigenen Kanal.
                let (cep, cntfn) = self.pds.chan_of(pd);
                for slot in 0..NCAPS {
                    if let Some(cap) = self.pds.cap_at(pd, slot) {
                        if let Some((kind, _, _)) = self.cspace.lookup(cap) {
                            match kind {
                                ObjectKind::Endpoint(id) if id != cep => return 5,
                                ObjectKind::Notification(id) if id != cntfn => return 5,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        0
    }

    /// **DMA-Bounds-Oracle** (ext-23, SMMU-agnostisch; Cap-Ebene). Prüft NUR die hardware-
    /// unabhängigen DmaCap-Bounds-Invarianten über alle (distinct) DMA-Objekte — die volle
    /// DMA-Policy (inkl. Enforcer-Durchsetzung + Revoke-Ordnung) ist `system::dma_audit`, das dies
    /// aufruft. (Konsolidierung O-C: umbenannt von `dma_audit`, um die Namensgleichheit mit dem
    /// aggregierenden `system::dma_audit` aufzulösen.) `0` = konsistent, sonst:
    /// - `1` = eine DMA-Region ist nicht 4-KiB-ausgerichtet/leer ODER liegt außerhalb des
    ///   mappbaren RAM-Fensters `[floor, ceil)` (mit `floor` = Kernel-Image-Ende deckt das
    ///   insbesondere „Region überlappt das Kernel-Image" ab — sie wäre nicht aus freiem RAM
    ///   ausgeschnitten).
    /// - `2` = zwei verschiedene DMA-Regionen überlappen einander (eine Region doppelt vergeben).
    ///
    /// „DmaCap nur in HardwareLand" deckt bereits [`Caps::domain_audit`] (Code 1) ab, da
    /// `kind_is_hardware` nun `Dma` einschließt. Die hardware-erzwungene Durchsetzung (SMMU)
    /// liegt hinter dem kernel-internen `DmaEnforcer` und wird separat auditiert.
    pub fn dma_bounds_audit(&self, floor: u64, ceil: u64) -> u32 {
        let mut regs: [(u64, u64); 32] = [(0, 0); 32];
        let mut n = 0usize;
        let mut bad = 0u32;
        self.cspace.for_each_dma(&mut |phys, len| {
            if len == 0
                || phys % 4096 != 0
                || len % 4096 != 0
                || phys < floor
                || phys.saturating_add(len) > ceil
            {
                bad = 1;
            }
            if n < regs.len() {
                regs[n] = (phys, len);
                n += 1;
            }
        });
        if bad != 0 {
            return bad;
        }
        // Paarweise Disjunktheit (verschiedene Objekte dürfen sich nie überlappen).
        for i in 0..n {
            for j in (i + 1)..n {
                let (a0, al) = regs[i];
                let (b0, bl) = regs[j];
                if a0 < b0 + bl && b0 < a0 + al {
                    return 2;
                }
            }
        }
        0
    }
}

/// Ist `kind` ein **Hardware-Cap** (MMIO/IRQ/DMA)? Generische Kategorie: alle Cap-Typen, die
/// direkten Geräte-Zugriff autorisieren und daher ausschließlich HardwareLand vorbehalten sind.
/// (ext-22: MMIO/IRQ; ext-23: zusätzlich DMA.)
fn kind_is_hardware(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Mmio { .. } | ObjectKind::Irq { .. } | ObjectKind::Dma { .. }
    )
}

/// Ist `kind` eine **TrustedSas-Autoritäts-Cap** (`PdControl` oder `Loader`, ext-26)? Solche dürfen
/// nur TrustedSas-PDs halten.
fn kind_is_pd_control(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::PdControl { .. } | ObjectKind::Loader { .. })
}

/// Darf eine PD der Domäne `domain` eine Cap des Typs `kind` halten?
/// HW-Caps nur HardwareLand; `PdControl`/`Loader` nur TrustedSas; alles andere überall.
fn domain_allows_kind(domain: Domain, kind: ObjectKind) -> bool {
    if kind_is_hardware(kind) {
        domain == Domain::HardwareLand
    } else if kind_is_pd_control(kind) {
        domain == Domain::TrustedSas
    } else {
        true
    }
}

/// Eine Protection Domain: ein Thread + ein Capability-Space + eine Sicherheitsdomäne.
#[derive(Clone, Copy)]
struct Pd {
    used: bool,
    thread: Option<ThreadId>,
    /// Lokaler Cap-Index -> globaler CapPtr.
    cspace: [Option<CapPtr>; NCAPS],
    /// Sicherheitsdomäne — **unveränderlich** nach der Erzeugung (Policy: bestimmt die
    /// erlaubten Cap-Typen + Kommunikationsbeziehungen). Migration = neue PD, nicht umschalten.
    domain: Domain,
    /// Nur für [`Domain::HardwareLand`]: der **eine** unveränderliche Trusted-SAS-Partner
    /// (PD-Index), mit dem dieses Backend ausschließlich kommunizieren darf. Bei der Erzeugung
    /// gesetzt; `None` für Trusted/User.
    partner: Option<u8>,
    /// Stabile **Backend-Id** (ext-22): identifiziert ein HardwareLand-Backend unabhängig vom
    /// PD-Index (für später: mehrere NICs/USB-Controller, Hotplug, PCIe). `0` = keins.
    backend_id: u16,
    /// Endpoint-/Notification-ID des **dedizierten Kanals** zum Partner (HardwareLand-Backend).
    /// Das Backend darf NUR über diesen Kanal kommunizieren. `u32::MAX` = keiner.
    chan_ep: u32,
    chan_ntfn: u32,
}

impl Pd {
    const EMPTY: Pd = Pd {
        used: false,
        thread: None,
        cspace: [None; NCAPS],
        domain: Domain::TrustedSas, // Default: alle Altbestand-PDs sind Trusted-SAS
        partner: None,
        backend_id: 0,
        chan_ep: u32::MAX,
        chan_ntfn: u32::MAX,
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

    /// Eine neue (leere) PD anlegen; gibt ihre ID zurück. Default-Domäne Trusted-SAS
    /// (Rückwärtskompatibilität — alle bestehenden Aufrufer bleiben unverändert).
    pub fn create(&mut self) -> Option<usize> {
        self.create_in_domain(Domain::TrustedSas)
    }

    /// Eine neue PD in einer bestimmten **Sicherheitsdomäne** anlegen. Die Domäne ist
    /// danach **unveränderlich** (es gibt bewusst keinen Setter).
    pub fn create_in_domain(&mut self, domain: Domain) -> Option<usize> {
        let i = self.pds.iter().position(|p| !p.used)?;
        self.pds[i] = Pd {
            used: true,
            domain,
            ..Pd::EMPTY
        };
        Some(i)
    }

    /// Eine PD **freigeben** (ext-26, L4): den Slot leeren (used=false, Cspace/Domäne/Partner
    /// zurückgesetzt). Der Aufrufer ist dafür verantwortlich, die im Cspace gehaltenen Caps vorher
    /// zu löschen (sonst lecken globale Cap-Objekte) und den Thread/die VSpace abzubauen. Gibt
    /// `false`, wenn die PD nicht belegt war.
    pub fn free(&mut self, pd: usize) -> bool {
        if pd < NPDS && self.pds[pd].used {
            self.pds[pd] = Pd::EMPTY;
            true
        } else {
            false
        }
    }

    /// Die lokalen Cap-Slots einer PD (für den Teardown: jeden installierten Cap löschen). Gibt
    /// die belegten `(slot, CapPtr)` zurück.
    pub fn caps_of(&self, pd: usize) -> [Option<CapPtr>; NCAPS] {
        if pd < NPDS && self.pds[pd].used {
            self.pds[pd].cspace
        } else {
            [None; NCAPS]
        }
    }

    /// Die (unveränderliche) Domäne einer PD.
    pub fn domain_of(&self, pd: usize) -> Option<Domain> {
        if pd < NPDS && self.pds[pd].used {
            Some(self.pds[pd].domain)
        } else {
            None
        }
    }

    /// Den unveränderlichen Trusted-Partner einer HardwareLand-PD (PD-Index).
    pub fn partner_of(&self, pd: usize) -> Option<usize> {
        if pd < NPDS {
            self.pds[pd].partner.map(|p| p as usize)
        } else {
            None
        }
    }

    /// Eine **HardwareLand-Backend-PD** mit unveränderlicher Partner-Bindung + dediziertem
    /// Kanal anlegen (ext-22, P3). `partner` = Trusted-SAS-PD (1:N erlaubt — mehrere Backends
    /// je Partner; N:1 ist strukturell ausgeschlossen, da `partner` ein einzelnes Feld ist).
    /// `ep`/`ntfn` = der einzige Kanal, über den dieses Backend kommunizieren darf. Alle
    /// Felder sind nach der Erzeugung **unveränderlich** (kein Setter).
    pub fn create_hardware_backend(
        &mut self,
        partner: usize,
        backend_id: u16,
        ep: u32,
        ntfn: u32,
    ) -> Option<usize> {
        if partner >= NPDS {
            return None;
        }
        let i = self.pds.iter().position(|p| !p.used)?;
        self.pds[i] = Pd {
            used: true,
            domain: Domain::HardwareLand,
            partner: Some(partner as u8),
            backend_id,
            chan_ep: ep,
            chan_ntfn: ntfn,
            ..Pd::EMPTY
        };
        Some(i)
    }

    /// Die stabile Backend-Id einer PD (0 = kein Backend).
    pub fn backend_id_of(&self, pd: usize) -> u16 {
        if pd < NPDS {
            self.pds[pd].backend_id
        } else {
            0
        }
    }

    /// Die Kanal-IDs (Endpoint, Notification) einer HardwareLand-Backend-PD.
    pub fn chan_of(&self, pd: usize) -> (u32, u32) {
        if pd < NPDS {
            (self.pds[pd].chan_ep, self.pds[pd].chan_ntfn)
        } else {
            (u32::MAX, u32::MAX)
        }
    }

    /// Ist `pd` belegt?
    pub fn is_used(&self, pd: usize) -> bool {
        pd < NPDS && self.pds[pd].used
    }

    /// Der gebundene Thread einer PD (für die Domäne↔VSpace-Isolationsprüfung).
    pub fn thread_of(&self, pd: usize) -> Option<ThreadId> {
        if pd < NPDS {
            self.pds[pd].thread
        } else {
            None
        }
    }

    /// Anzahl der PD-Slots (für Audit-Iteration).
    pub const fn capacity() -> usize {
        NPDS
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
    caps: &RwSpinLock<Caps>,
    eps: &[SpinLock<Endpoint>; NENDPOINTS],
    ntfns: &[SpinLock<Notification>; NNOTIFICATIONS],
    // ext-26: `SYS_LOAD`-Callback in den kernel-spezifischen Binary-Loader. `(Archiv-Index,
    // Endowment) -> neue PD-Id`. Vom Dispatch erst NACH dem Freigeben von `caps` gerufen (der
    // Loader re-lockt `CAPS`/`MEM`/`SCHEDS` selbst). `endow` = aus dem Aufrufer-Cspace delegierte
    // Caps (Slot, Cap).
    load: fn(u32, &[(usize, CapPtr)]) -> Option<usize>,
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

    // Cap-Auflösung unter dem **geteilten Read-Lock** auf `CAPS`: der heiße Lookup ist
    // rein lesend und läuft so auf verschiedenen Kernen parallel (kein globaler Engpass
    // mehr). Der Guard wird sofort wieder freigegeben; alle Pfade außer REPLY+grant
    // arbeiten danach ohne CAPS weiter (wie zuvor). Die Werte sind `Copy`/ownend.
    let thread = ops.current_id(core);
    let local = frame_reg(frame, reg::EP_BADGE) as usize;
    let (pd, kind, rights, badge) = {
        let g = caps.read();
        let Some(pd) = g.pds.pd_of(thread) else {
            return deny(result::ERR_NOPD);
        };
        let Some(cap) = g.pds.cap_at(pd, local) else {
            return deny(result::ERR_BADCAP);
        };
        let Some((kind, rights, badge)) = g.cspace.lookup(cap) else {
            return deny(result::ERR_BADCAP);
        };
        (pd, kind, rights, badge)
    }; // CAPS-Read-Lock hier freigegeben

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
                sys::CALL => eps[ep].lock().call(ops, core, frame),
                sys::RECV => eps[ep].lock().recv(ops, core, frame),
                _ => {
                    // REPLY: ggf. Cap-Transfer (grant). Der Transfer MUTIERT den CapSpace,
                    // braucht also den **Write-Lock**; um die Sperrordnung CAPS->EPS zu
                    // wahren, wird CAPS-write VOR dem Endpoint-Lock genommen und nach dem
                    // Transfer (vor dem Rendezvous) freigegeben. Ohne grant ist kein CAPS
                    // nötig — reiner Endpoint-Lock.
                    let tag = frame_reg(frame, reg::TAG);
                    if tag & sel4lake_abi::GRANT_FLAG != 0 {
                        let mut g = caps.write();
                        let mut e = eps[ep].lock();
                        if let Some(caller) = e.caller() {
                            grant_cap(&mut g, caller, pd, (tag & 0xff) as usize);
                        }
                        drop(g); // CAPS vor dem Rendezvous freigeben (EPS bleibt gehalten)
                        e.reply(ops, core, frame)
                    } else {
                        eps[ep].lock().reply(ops, core, frame)
                    }
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
            let ok = ops.kill(ThreadId::from_raw(raw), core);
            frame_set_reg(frame, reg::SYSNO_RESULT, if ok { result::OK } else { result::ERR_BADCAP });
            frame
        }
        sys::MAP | sys::UNMAP => {
            // Frame über eine Memory-Cap in die eigene VSpace mappen/entfernen. Das
            // Recht der Cap bestimmt die Seitenrechte: EXEC -> RX (W^X), WRITE -> RW,
            // sonst READ -> RO. Ohne nutzbares Recht abgelehnt.
            let ObjectKind::Memory(region) = kind else {
                return deny(result::ERR_BADCAP);
            };
            let perm_code = if rights.contains(Rights::EXEC) {
                2u8
            } else if rights.contains(Rights::WRITE) {
                1
            } else if rights.contains(Rights::READ) {
                0
            } else {
                return deny(result::ERR_RIGHTS);
            };
            let ok = if nr == sys::MAP {
                ops.map_frame(thread, region.base, region.len, perm_code)
            } else {
                ops.unmap_frame(thread, region.base, region.len)
            };
            frame_set_reg(frame, reg::SYSNO_RESULT, if ok { result::OK } else { result::ERR_BADCAP });
            frame
        }
        sys::PDCTL => {
            // PD-Management (ext-22): Lifecycle einer Ziel-PD steuern. Gated auf eine
            // PdControl-Cap (WRITE) UND: der Aufrufer muss TrustedSas sein UND das Ziel
            // muss UserLand sein (Defense-in-Depth — nur Trusted steuert nur Untrusted).
            let ObjectKind::PdControl { pd: target } = kind else {
                return deny(result::ERR_BADCAP);
            };
            if !rights.contains(Rights::WRITE) {
                return deny(result::ERR_RIGHTS);
            }
            let target = target as usize;
            let subop = frame_reg(frame, reg::MSG0); // x2 = Sub-Operation
            // Domänen-Policy + Ziel-Thread unter CAPS.read auflösen (dann freigeben).
            let target_tid = {
                let g = caps.read();
                if g.pds.domain_of(pd) != Some(Domain::TrustedSas) {
                    return deny(result::ERR_RIGHTS);
                }
                if g.pds.domain_of(target) != Some(Domain::UserLand) {
                    return deny(result::ERR_RIGHTS);
                }
                g.pds.thread_of(target)
            };
            let ok = match (subop, target_tid) {
                (pdctl::PAUSE, Some(t)) => {
                    ops.pause(t);
                    true
                }
                // START und RESUME: einen (initial) blockierten Ziel-Thread wecken.
                (pdctl::RESUME, Some(t)) | (pdctl::START, Some(t)) => {
                    ops.unblock(t);
                    true
                }
                (pdctl::STOP, Some(t)) => ops.stop(t),
                (pdctl::PAUSE, None)
                | (pdctl::RESUME, None)
                | (pdctl::START, None)
                | (pdctl::STOP, None) => false, // Ziel hat keinen Thread
                _ => return deny(result::ERR_BADSYS), // unbekannte Sub-Operation
            };
            frame_set_reg(frame, reg::SYSNO_RESULT, if ok { result::OK } else { result::ERR_BADCAP });
            frame
        }
        sys::LOAD => {
            // ext-26: cap-gegatetes Laden eines extern gebauten Programms zur Laufzeit. Die
            // `Loader`-Cap ist die Autoritaet; der geladene Prozess erhaelt NUR den/die explizit
            // delegierten Caller-Cap(s) -- keine Sonderrechte ueber die Loader-Cap.
            let ObjectKind::Loader { .. } = kind else {
                return deny(result::ERR_BADCAP);
            };
            if !rights.contains(Rights::WRITE) {
                return deny(result::ERR_RIGHTS);
            }
            let index = frame_reg(frame, reg::MSG0) as u32; // x2: Archiv-Programm-Index
            let delegate = frame_reg(frame, reg::MSG0 + 1); // x3: lokaler Cap-Index (u64::MAX=keiner)
            // Den zu delegierenden Caller-Cap (x3) aufloesen + KOPIEREN (CDT-Kind, gleiche Rechte)
            // unter CAPS.write; danach CAPS freigeben -- der Loader re-lockt CAPS/MEM/SCHEDS selbst.
            let endowed = if delegate != u64::MAX {
                let mut g = caps.write();
                let Some(c) = g.pds.cap_at(pd, delegate as usize) else {
                    return deny(result::ERR_BADCAP);
                };
                let Some((_, r, _)) = g.cspace.lookup(c) else {
                    return deny(result::ERR_BADCAP);
                };
                match g.cspace.copy(c, r) {
                    Ok(copy) => Some(copy),
                    Err(_) => return deny(result::ERR_BADCAP),
                }
            } else {
                None
            }; // CAPS hier freigegeben
            let arr;
            let endow: &[(usize, CapPtr)] = if let Some(c) = endowed {
                arr = [(0usize, c)]; // Konvention L2: delegierter Cap -> Slot 0 der neuen PD
                &arr
            } else {
                &[]
            };
            match load(index, endow) {
                Some(pd_new) => {
                    frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
                    frame_set_reg(frame, reg::EP_BADGE, pd_new as u64); // x1 = neue PD-Id
                    frame
                }
                None => deny(result::ERR_BADCAP), // Laden fehlgeschlagen (Archiv/ELF/Ressourcen)
            }
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
