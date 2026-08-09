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
use sel4lake_cap::{CapPtr, CapSpace, DmaCoherence, DmaDir, ObjectKind};
use sel4lake_hal::cpu::array_index_nospec;
use sel4lake_hal::exception::{frame_reg, frame_set_reg};
use sel4lake_ipc::{Endpoint, Notification};
use sel4lake_mem::Rights;
use sel4lake_sched::{SchedOps, ThreadId};
use sel4lake_slab::Slab;
use sel4lake_sync::{RwSpinLock, SpinLock};

/// Größe des PD-Pools. **Öffentlich seit A-3.4**: wie viele Cap-Slots das System vorhalten muss,
/// folgt aus `NPDS * CAP_BUDGET_PER_PD` — diese Rechnung gehört an *eine* Stelle
/// ([`CAP_SLOTS_FOR_ALL_PDS`]) und nicht als abgeschriebene Zahl in den Boot-Code.
/// **Ziel-Zahl gleichzeitiger Protection Domains** (A-3.4, Teil 3).
///
/// War 256 und lag als `[Pd; NPDS]` im `.bss`. Damit waren 10000 Threads (`TARGET_THREADS`) zwar
/// darstellbar, aber nur solange sie sich Adressräume **teilen** — als 10000 isolierte Tenants
/// nicht, und genau das ist der Punkt des Systems. Die Tabelle wird jetzt beim Boot alloziert
/// (`PdTable::attach`), die Zahl kostet also RAM, keine Struktur.
///
/// Auf `TARGET_THREADS` abgestimmt: im Grenzfall bekommt jeder Thread seine eigene PD. Was das
/// kostet, meldet der Boot-Report — die Entscheidung hängt an einer gemessenen Größe.
pub const NPDS: usize = 10_000;
/// Cap-Slots je PD-Cspace (Adressraum der lokalen Slot-Indizes).
const NCAPS: usize = 16;

/// **Cap-Budget je PD** (Fairness-/DoS-Schranke).
///
/// Der globale [`CapSpace`] ist eine **systemweit geteilte** Tabelle fester Größe. `NCAPS`
/// begrenzt nur den lokalen Index-Adressraum einer PD, nicht ihren Verbrauch an globalen
/// Slots: ohne zusätzliche Schranke könnten wenige PDs (`NPDS * NCAPS` ≫ Slots der globalen
/// Tabelle) die Tabelle füllen und damit **allen anderen** PDs jede weitere Cap-Installation
/// verweigern — ein Cross-PD-Denial-of-Service über eine geteilte Ressource.
///
/// Das Budget deckelt, wie viele Slots des eigenen Cspace eine PD gleichzeitig belegen darf.
/// Es ist bewusst deutlich kleiner als `NCAPS`: die vorhandenen PDs nutzen 1–4 Slots, und so
/// bleibt selbst bei vielen gleichzeitig geladenen PDs Tabellenkapazität für alle übrig.
/// Überschreitung wird **abgewiesen** (kein Eintrag, keine Ableitung), nie still verworfen.
pub const CAP_BUDGET_PER_PD: usize = 8;

/// **Wie viele globale Cap-Slots die Summe aller PD-Budgets braucht** (A-3.4).
///
/// Der Kommentar an [`CAP_BUDGET_PER_PD`] sagt, das Budget verhindere einen Cross-PD-DoS über die
/// geteilte Tabelle. Nachgerechnet stimmte das nicht: 256 × 8 = 2048 Slots Bedarf standen gegen
/// **256** vorhandene. Das Budget deckelte den Verbrauch **einer** PD, die Summe prüfte niemand —
/// 32 PDs mit vollem Budget füllten die Tabelle, die 33. bekam nichts. Die Zusage war also eine
/// Annahme über das Verhalten der PDs, keine Eigenschaft des Systems.
///
/// Seit A-3.4 dimensioniert der Kernel die Tabelle **hiernach**. Damit ist die Aussage „jede PD
/// bekommt ihr Budget" eine Eigenschaft des Aufbaus. Der Weg dahin war ausdrücklich *nicht*, die
/// PD-Zahl auf 32 zu senken: das hätte dieselbe Zusage gerettet, indem es das System kleiner macht.
pub const CAP_SLOTS_FOR_ALL_PDS: usize = NPDS * CAP_BUDGET_PER_PD;

/// **Slots für die Caps, die der Kernel selbst hält** (Wurzel-Caps auf Speicherregionen,
/// Endpoints/Notifications der Bringup-Kanäle, Loader-Cap des Root-Task, DMA-/MMIO-/IRQ-Caps der
/// Treiber).
///
/// Die Reserve steht **neben** der Summe aller PD-Budgets, nicht in ihr: eine PD, die ihr Budget
/// ausschöpft, soll dem Kernel keinen Slot wegnehmen können — und umgekehrt. Bis hierher war das
/// eine Zahl im Boot-Code mit einem Kommentar daneben; nachgezählt hat sie niemand. Womit sie
/// nachgezählt wird, steht an [`Caps::nonbudget_slots`].
pub const CAP_SLOTS_KERNEL_RESERVE: usize = 256;

/// **Gesamte Slot-Kapazität des globalen Cap-Space** — Summe aller PD-Budgets plus Kernel-Reserve.
///
/// Stand als `CAP_SLOTS_FOR_ALL_PDS + 256` im Boot-Code des Kernels. Dieselbe Begründung wie bei
/// [`CAP_SLOTS_FOR_ALL_PDS`]: die Rechnung gehört an *eine* Stelle, sonst läuft die abgeschriebene
/// Zahl beim nächsten Drehen an [`NPDS`] oder [`CAP_BUDGET_PER_PD`] still auseinander — und still
/// auseinandergelaufen heißt hier: eine PD innerhalb ihres Budgets bekommt `NoSlot`.
pub const CAP_SLOTS_TOTAL: usize = CAP_SLOTS_FOR_ALL_PDS + CAP_SLOTS_KERNEL_RESERVE;

/// **Wie viele Endpoint-Objekte alle PDs zusammen brauchen** (A-3.4 Teil 4).
///
/// Dieselbe Rechnung eine Ebene weiter, und derselbe Fund: `NENDPOINTS = 32` in `sel4lake-ipc`
/// stand gegen [`NPDS`] = 10 000. Eine PD, die als Server auftreten will, braucht **mindestens
/// einen** Endpoint; ab der 33. war keiner mehr zu haben. Die Zusage „10 000 Tenants" endete
/// damit an einer Zahl, die nie mitgewachsen ist — Threads (Teil 1), Caps (Teil 2) und
/// Adressräume (Teil 3) waren gedreht, die Kommunikation nicht. Ein Tenant ohne Endpoint ist
/// aber kein Tenant, sondern ein Prozess, mit dem niemand reden kann.
///
/// Ein Endpoint je PD ist die **untere** Schranke, nicht die bequeme: mehr Endpoints je PD
/// (eine PD mit mehreren Diensten) sind damit nicht gedeckt und müssten die Zahl erhöhen.
pub const ENDPOINTS_FOR_ALL_PDS: usize = NPDS;

/// **Wie viele Notification-Objekte alle PDs zusammen brauchen** (A-3.4 Teil 4).
///
/// Wie [`ENDPOINTS_FOR_ALL_PDS`]: eine asynchrone Signalquelle je PD als untere Schranke.
/// Notifications sind mit Abstand die billigsten Objekte (drei Felder, keine Warteschlange) —
/// hier zu sparen bringt nichts und kostet dieselbe Zusage.
pub const NOTIFICATIONS_FOR_ALL_PDS: usize = NPDS;

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

    /// Eine Cap **policy- und budgetgeprüft** in den Cspace einer PD eintragen: der Cap-Typ
    /// muss in der Domäne der PD erlaubt sein (Hardware-Caps nur in HardwareLand, `PdControl`
    /// nur in TrustedSas) UND die PD darf ihr [`CAP_BUDGET_PER_PD`] nicht überschreiten. Gibt
    /// `false` zurück (ohne Eintrag), wenn eines von beiden verletzt ist.
    /// Zentraler Enforcement-Punkt — alle Cap-Installationen sollten hierüber laufen.
    pub fn install_cap_checked(&mut self, pd: usize, slot: usize, cap: CapPtr) -> bool {
        if !self.cap_allowed(pd, cap) || !self.budget_allows(pd, slot) {
            return false;
        }
        self.pds.install_cap(pd, slot, cap);
        true
    }

    /// Darf PD `pd` den Slot `slot` (neu) belegen, ohne ihr [`CAP_BUDGET_PER_PD`] zu
    /// überschreiten? Ein **Überschreiben** eines bereits belegten Slots erhöht den
    /// Verbrauch nicht und ist immer erlaubt.
    pub fn budget_allows(&self, pd: usize, slot: usize) -> bool {
        if self.pds.cap_at(pd, slot).is_some() {
            return true; // Ersetzen, kein zusätzlicher Slot
        }
        self.pds.cap_count(pd) < CAP_BUDGET_PER_PD
    }

    /// **Die Summenprüfung** (A-3.4, Abschluss): wie viele globale Slots gehen auf **kein**
    /// PD-Budget?
    ///
    /// [`budget_allows`](Self::budget_allows) deckelt den Verbrauch **einer** PD; dass die Summe
    /// aller Budgets in die Tabelle passt, trägt seit A-3.4 die Dimensionierung
    /// ([`CAP_SLOTS_TOTAL`]). Ungeprüft blieb die andere Seite derselben Rechnung: der Kernel
    /// selbst installiert Wurzel-Caps, und nichts hindert ihn daran, mehr als
    /// [`CAP_SLOTS_KERNEL_RESERVE`] zu belegen. Dann ist jede einzelne PD innerhalb ihres Budgets
    /// und die Zusage „jede PD bekommt ihr Budget" trotzdem gebrochen — mit `NoSlot` an einer
    /// Stelle, die nichts falsch gemacht hat.
    ///
    /// Gezählt wird **nicht** über `used_slots() − Σ cap_count(pd)`: halten zwei PDs denselben
    /// [`CapPtr`], überzählt die Summe, und der Fehlbetrag zeigt in die unsichere Richtung (die
    /// Prüfung ginge durch, obwohl sie es nicht sollte). Stattdessen wird jeder von einer PD
    /// gehaltene Slot in `seen` markiert und danach ausgezählt, was belegt und unmarkiert blieb.
    ///
    /// `seen` muss mindestens so viele Einträge haben wie die Slot-Tabelle; sein Inhalt beim
    /// Eintritt ist gleichgültig (wird genullt). `None` heißt „konnte nicht laufen", nicht „in
    /// Ordnung" — siehe [`CapSpace::unmarked_used_slots`].
    pub fn nonbudget_slots(&self, seen: &mut [bool]) -> Option<usize> {
        let (nslots, _) = self.cspace.capacity();
        if seen.len() < nslots {
            return None;
        }
        seen[..nslots].fill(false);
        for pd in 0..self.pds.capacity() {
            if !self.pds.is_used(pd) {
                continue;
            }
            for cap in self.pds.caps_of(pd).into_iter().flatten() {
                // Ein nicht auflösbares Handle markiert nichts — es ist kein Verbrauch.
                let _ = self.cspace.mark_slot(cap, seen);
            }
        }
        self.cspace.unmarked_used_slots(seen)
    }

    /// Hält der Kernel sich an seine Reserve? `Some(true)`/`Some(false)`, `None` = die Prüfung
    /// konnte nicht laufen (zu kleine Zählfläche) — die drei Fälle bleiben getrennt.
    pub fn kernel_reserve_ok(&self, seen: &mut [bool]) -> Option<bool> {
        self.nonbudget_slots(seen)
            .map(|n| n <= CAP_SLOTS_KERNEL_RESERVE)
    }

    /// Erlaubt die Domänen-Policy die Cap-Art von `cap` in PD `pd`? Reiner Test (kein Eintrag) —
    /// damit Pfade, die VOR dem Eintragen eine Ableitung/Kopie erzeugen (Grant in IPC), die Policy
    /// **prüfen, bevor** sie eine Kopie anlegen (kein Policy-Bypass + keine verwaiste Kopie bei
    /// Ablehnung). Identisch zur Prüfung in [`install_cap_checked`].
    pub fn cap_allowed(&self, pd: usize, cap: CapPtr) -> bool {
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
        for pd in 0..self.pds.capacity() {
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
pub struct Pd {
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
    partner: Option<u16>,
    /// Stabile **Backend-Id** (ext-22): identifiziert ein HardwareLand-Backend unabhängig vom
    /// PD-Index (für später: mehrere NICs/USB-Controller, Hotplug, PCIe). `0` = keins.
    backend_id: u16,
    /// Endpoint-/Notification-ID des **dedizierten Kanals** zum Partner (HardwareLand-Backend).
    /// Das Backend darf NUR über diesen Kanal kommunizieren. `u32::MAX` = keiner.
    chan_ep: u32,
    chan_ntfn: u32,
    /// **Empfangs-Slot für per IPC übertragene Caps** (A-3.2). Der Empfänger legt fest, wo eine
    /// gegrantete Cap landet — nicht der Sender. Voreinstellung ist der frühere feste Slot
    /// ([`sel4lake_abi::GRANT_RECV_SLOT`]), damit bestehende Programme unverändert laufen.
    ///
    /// Warum der Empfänger und nicht der Sender: der Sender kennt den Cspace des Empfängers
    /// nicht. Dürfte er den Slot wählen, könnte ein Server jeden Cap seines Clients verdrängen —
    /// eine Schreiboperation in fremdes Eigentum, verkleidet als Antwort.
    recv_slot: usize,
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
        recv_slot: sel4lake_abi::GRANT_RECV_SLOT,
    };
}

/// Tabelle aller Protection Domains.
pub struct PdTable {
    pds: Slab<Pd>,
}

impl Default for PdTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PdTable {
    pub const fn new() -> Self {
        Self {
            pds: Slab::empty(),
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
        if pd < self.pds.len() && self.pds[pd].used {
            self.pds[pd] = Pd::EMPTY;
            true
        } else {
            false
        }
    }

    /// Die lokalen Cap-Slots einer PD (für den Teardown: jeden installierten Cap löschen). Gibt
    /// die belegten `(slot, CapPtr)` zurück.
    pub fn caps_of(&self, pd: usize) -> [Option<CapPtr>; NCAPS] {
        if pd < self.pds.len() && self.pds[pd].used {
            self.pds[pd].cspace
        } else {
            [None; NCAPS]
        }
    }

    /// Die (unveränderliche) Domäne einer PD.
    pub fn domain_of(&self, pd: usize) -> Option<Domain> {
        if pd < self.pds.len() && self.pds[pd].used {
            Some(self.pds[pd].domain)
        } else {
            None
        }
    }

    /// Den unveränderlichen Trusted-Partner einer HardwareLand-PD (PD-Index).
    pub fn partner_of(&self, pd: usize) -> Option<usize> {
        if pd < self.pds.len() {
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
        if partner >= self.pds.len() {
            return None;
        }
        let i = self.pds.iter().position(|p| !p.used)?;
        self.pds[i] = Pd {
            used: true,
            domain: Domain::HardwareLand,
            partner: Some(partner as u16),
            backend_id,
            chan_ep: ep,
            chan_ntfn: ntfn,
            ..Pd::EMPTY
        };
        Some(i)
    }

    /// Die stabile Backend-Id einer PD (0 = kein Backend).
    pub fn backend_id_of(&self, pd: usize) -> u16 {
        if pd < self.pds.len() {
            self.pds[pd].backend_id
        } else {
            0
        }
    }

    /// Die Kanal-IDs (Endpoint, Notification) einer HardwareLand-Backend-PD.
    pub fn chan_of(&self, pd: usize) -> (u32, u32) {
        if pd < self.pds.len() {
            (self.pds[pd].chan_ep, self.pds[pd].chan_ntfn)
        } else {
            (u32::MAX, u32::MAX)
        }
    }

    /// Ist `pd` belegt?
    pub fn is_used(&self, pd: usize) -> bool {
        pd < self.pds.len() && self.pds[pd].used
    }

    /// Der gebundene Thread einer PD (für die Domäne↔VSpace-Isolationsprüfung).
    pub fn thread_of(&self, pd: usize) -> Option<ThreadId> {
        if pd < self.pds.len() {
            self.pds[pd].thread
        } else {
            None
        }
    }

    /// Anzahl der PD-Slots (für Audit-Iteration) — die **tatsächliche**, beim Boot zugewiesene.
    ///
    /// War eine Konstante (`NPDS`). Seit A-3.4 Teil 3 ist sie die Länge der angehängten Tabelle:
    /// eine Audit-Schleife, die über die Konstante läuft statt über die reale Kapazität, würde
    /// vor dem `attach` ins Leere greifen und danach womöglich an der Tabelle vorbei.
    pub fn capacity(&self) -> usize {
        self.pds.len()
    }

    /// Der Tabelle ihren Speicher geben (einmalig, beim Boot — vor der ersten PD).
    ///
    /// # Safety
    /// Vertrag von [`Slab::attach`]: exklusiver, ausgerichteter, dauerhafter Speicher für
    /// mindestens `len` Elemente, genau einmal.
    pub unsafe fn attach(&mut self, ptr: *mut Pd, len: usize) {
        // SAFETY: an den Aufrufer durchgereicht (s. Funktionsdoku).
        unsafe { self.pds.attach(ptr, len, |_| Pd::EMPTY) };
    }

    /// Den Thread einer PD setzen (Affinität Thread<->PD).
    pub fn bind_thread(&mut self, pd: usize, thread: ThreadId) {
        if pd < self.pds.len() {
            self.pds[pd].thread = Some(thread);
        }
    }

    /// Eine globale Capability in den Cspace einer PD an `slot` eintragen.
    pub fn install_cap(&mut self, pd: usize, slot: usize, cap: CapPtr) {
        if pd < self.pds.len() && slot < NCAPS {
            self.pds[pd].cspace[slot] = Some(cap);
        }
    }

    /// Einen Cap-Slot einer PD leeren (Autorität entziehen — Hot-Reload).
    /// Den **Empfangs-Slot** einer PD setzen (A-3.2). `false`, wenn PD oder Slot ungültig sind.
    pub fn set_recv_slot(&mut self, pd: usize, slot: usize) -> bool {
        if pd < self.pds.len() && self.pds[pd].used && slot < NCAPS {
            self.pds[pd].recv_slot = slot;
            true
        } else {
            false
        }
    }

    /// Der Empfangs-Slot einer PD (Vorgabe: [`sel4lake_abi::GRANT_RECV_SLOT`]).
    pub fn recv_slot(&self, pd: usize) -> usize {
        if pd < self.pds.len() && self.pds[pd].used {
            self.pds[pd].recv_slot
        } else {
            sel4lake_abi::GRANT_RECV_SLOT
        }
    }

    /// Anzahl der Cap-Slots je PD-Cspace (Adressraum der lokalen Slot-Indizes).
    pub const fn caps_per_pd() -> usize {
        NCAPS
    }

    pub fn clear_cap(&mut self, pd: usize, slot: usize) {
        if pd < self.pds.len() && slot < NCAPS {
            self.pds[pd].cspace[slot] = None;
        }
    }

    fn pd_of(&self, thread: ThreadId) -> Option<usize> {
        self.pds
            .iter()
            .position(|p| p.used && p.thread == Some(thread))
    }

    /// Anzahl der aktuell belegten Cap-Slots einer PD (Verbrauch gegen [`CAP_BUDGET_PER_PD`]).
    pub fn cap_count(&self, pd: usize) -> usize {
        if pd < self.pds.len() && self.pds[pd].used {
            self.pds[pd].cspace.iter().filter(|c| c.is_some()).count()
        } else {
            0
        }
    }

    fn cap_at(&self, pd: usize, slot: usize) -> Option<CapPtr> {
        if slot < NCAPS {
            // Spectre-v1-Härtung: `slot` stammt bei jedem Syscall aus einem EL0-Register.
            // Die Verzweigung oben schützt nur den architektonischen Pfad — eine falsch
            // vorhergesagte Verzweigung könnte den Zugriff spekulativ mit einem beliebigen
            // Index ausführen und den Treffer im Cache hinterlassen. Die Maske macht den
            // Index zusätzlich datenabhängig gültig.
            self.pds[pd].cspace[array_index_nospec(slot, NCAPS)]
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
    // A-3.4 Teil 4: **Slices** statt `&[_; NENDPOINTS]`. Die Schranke ist damit die
    // tatsächlich beim Boot zugewiesene Kapazität (`eps.len()`), nicht eine Konstante, die
    // neben der Tabelle her existiert und bei einer Änderung stillschweigend auseinanderläuft.
    eps: &[SpinLock<Endpoint>],
    ntfns: &[SpinLock<Notification>],
    // ext-26: `SYS_LOAD`-Callback in den kernel-spezifischen Binary-Loader. `(Archiv-Index,
    // AUFRUFER-PD, Endowment) -> neue PD-Id`. Vom Dispatch erst NACH dem Freigeben von `caps`
    // gerufen (der Loader re-lockt `CAPS`/`MEM`/`SCHEDS` selbst). `endow` = aus dem
    // Aufrufer-Cspace delegierte Caps (Slot, Cap).
    //
    // **Die Aufrufer-PD kam mit A-5.1 dazu**, und nicht aus Bequemlichkeit: ein
    // HardwareLand-Backend ist per Entwurf an einen **Partner** gebunden (ext-22, unveraenderlich,
    // 1:N). Wer es laedt, ist dieser Partner -- eine andere Antwort gibt es nicht, und sie muss
    // vom Dispatch kommen, weil nur er weiss, wer gerade syscallt.
    load: fn(u32, usize, &[(usize, CapPtr)]) -> Option<usize>,
    // Cap löschen (Finalisierung inkl. Speicherfreigabe/Call-Abbruch) — der Kernel sperrt darin
    // selbst `CAPS`+`MEM` und bricht finalisierte Reply-Calls ab. Nur zu rufen, wenn hier KEIN
    // Lock mehr gehalten wird. Wird für die beim Grant verdrängte Cap gebraucht (s. `grant_cap`)
    // und für `SYS_CDELETE`. **Rückgabe:** ob gelöscht wurde — `false` heißt, dass noch abgeleitete
    // Caps (CDT-Kinder) daran hängen. Für den Grant-Pfad ist das nur Telemetrie, für `CDELETE` die
    // Bedingung, unter der der Slot überhaupt geräumt werden darf.
    delete_cap: fn(CapPtr) -> bool,
) -> usize {
    let nr = frame_reg(frame, reg::SYSNO_RESULT);
    if nr == sys::YIELD {
        return ops.on_tick(core, frame);
    }
    if nr == sys::PARK {
        // Selbst-Park: Aufrufer schlafen legen; kein Capability nötig (wirkt nur auf ihn selbst).
        //
        // **Kehrt sofort zurück, wenn eine Weckmarke vorliegt** (Z22, P4) -- ohne das wäre die
        // Folge „Bedingung prüfen (falsch)" → „parken" unterbrechbar, und ein dazwischen
        // eintreffendes `UNPARK` ginge verloren. Wer `PARK` weiterhin als „für immer anhalten"
        // benutzt (Fuzz-Threads, geparkte Demo-Threads), merkt davon nichts: ohne ein `UNPARK`
        // wird nie eine Marke gesetzt.
        return match ops.park_current(core, frame) {
            Some(next) => next,
            None => {
                frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
                frame
            }
        };
    }
    if nr == sys::UNPARK {
        // Ziel muss in DERSELBEN PD liegen. Fail-closed in allen drei Richtungen: der Aufrufer
        // ohne PD, das Ziel ohne PD, oder verschiedene PDs -> abweisen. Siehe `sys::UNPARK`,
        // warum das ohne Cap auskommt und trotzdem keine Autorität hinzufügt.
        let target = ThreadId::from_raw(frame_reg(frame, reg::EP_BADGE));
        let me = ops.current_id(core);
        // **Auf sich selbst wirken braucht keine Autoritaet** -- derselbe Grund, aus dem `PARK`
        // ohne Cap auskommt. Und es ist nicht bloss ein Sonderfall: „erst die Marke setzen, dann
        // schlafen" ist die Redewendung, mit der ein Thread einen Weckruf ueberlebt, der ihn
        // erreicht, bevor er ueberhaupt eingeschlafen ist. Ohne diese Zeile waere sie in einem
        // Thread ohne PD (Boot-Umgebung) nicht formulierbar.
        let erlaubt = target == me || {
            let g = caps.read();
            match (g.pds.pd_of(me), g.pds.pd_of(target)) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            }
        };
        if !erlaubt {
            frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP);
            return frame;
        }
        ops.unpark(target);
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
        return frame;
    }
    if nr == sys::EXIT {
        // Selbst-Beenden: Stack/TCB werden zurückgewonnen; kein Capability nötig.
        return ops.exit_current(core, frame);
    }

    let deny = |code: u64| -> usize {
        frame_set_reg(frame, reg::SYSNO_RESULT, code);
        frame
    };

    // `CDELETE` **vor** der generischen Cap-Auflösung (A-3.1): die Operation braucht den rohen
    // `CapPtr` und interessiert sich weder für Objektart noch für Rechte. Das ist kein Schlupfloch
    // — der Slot gehört dem Aufrufer, und **Autorität abzugeben darf nie an einer Erlaubnis
    // hängen**. Ein Dienst, der seine empfangenen Caps nicht loswird, läuft sonst gegen
    // `CAP_BUDGET_PER_PD` und ist irgendwann handlungsunfähig, ohne dass jemand ihm etwas
    // entzogen hätte.
    if nr == sys::CDELETE {
        let thread = ops.current_id(core);
        let slot = frame_reg(frame, reg::EP_BADGE) as usize;
        // Reihenfolge: Slot **räumen**, dann löschen, bei Misserfolg zurücklegen.
        //
        // Andersherum (erst löschen, dann räumen) gäbe es ein Fenster, in dem der Slot auf ein
        // bereits finalisiertes Objekt zeigt — und die Finalisierung gibt Speicher frei und bricht
        // Calls ab, sie darf also nicht unter gehaltenem `CAPS` laufen. Die hier gewählte Richtung
        // hat nur ein Fenster, in dem der Slot leer ist, und in dem kann ihn niemand benutzen: der
        // einzige Thread, der ihn benutzen könnte, führt gerade diesen Syscall aus.
        let removed = {
            let mut g = caps.write();
            let Some(pd) = g.pds.pd_of(thread) else {
                return deny(result::ERR_NOPD);
            };
            let Some(cap) = g.pds.cap_at(pd, slot) else {
                return deny(result::ERR_BADCAP);
            };
            g.pds.clear_cap(pd, slot);
            (pd, cap)
        }; // CAPS freigegeben — `delete_cap` sperrt CAPS/MEM/SCHEDS selbst
        let (pd, cap) = removed;
        if !delete_cap(cap) {
            // Abgeleitete Caps hängen noch daran: unverändert zurücklegen. Ein „halb gelöschter"
            // Cap (aus dem Cspace entfernt, im CapSpace noch da) wäre für den Aufrufer unerreichbar
            // und für den Kernel weiterhin belegt — genau das Leck, gegen das CDELETE antritt.
            caps.write().pds.install_cap(pd, slot, cap);
            return deny(result::ERR_HASCHILDREN);
        }
        frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
        return frame;
    }

    // `CCOPY`/`CMOVE`/`SETRECV` (A-3.2) — wie `CDELETE` vor der generischen Auflösung: sie
    // arbeiten auf **Slots** des eigenen Cspace, nicht auf einem Objekt hinter einer Cap.
    if nr == sys::CCOPY || nr == sys::CMOVE || nr == sys::SETRECV {
        let thread = ops.current_id(core);
        let src_slot = frame_reg(frame, reg::EP_BADGE) as usize;
        let dst_slot = frame_reg(frame, reg::MSG0) as usize;
        let mask = frame_reg(frame, reg::MSG0 + 1);
        let new_badge = frame_reg(frame, reg::MSG0 + 2);
        // Ein bei `install_cap_checked` abgelehnter Kopie-Cap muss gelöscht werden — aber
        // **ohne** gehaltenen Lock (die Finalisierung nimmt CAPS/MEM selbst). Deshalb wird er
        // hier herausgereicht statt sofort behandelt.
        let mut orphan: Option<CapPtr> = None;
        let code = {
            let mut g = caps.write();
            let Some(pd) = g.pds.pd_of(thread) else {
                return deny(result::ERR_NOPD);
            };
            match nr {
                sys::SETRECV => {
                    if g.pds.set_recv_slot(pd, src_slot) {
                        result::OK
                    } else {
                        result::ERR_BADCAP
                    }
                }
                sys::CMOVE => {
                    let Some(src) = g.pds.cap_at(pd, src_slot) else {
                        return deny(result::ERR_BADCAP);
                    };
                    if src_slot == dst_slot {
                        result::OK // nichts zu tun; kein Sonderfall, der irgendwo aufschlägt
                    } else if dst_slot >= PdTable::caps_per_pd() {
                        result::ERR_BADCAP
                    } else if g.pds.cap_at(pd, dst_slot).is_some() {
                        // Ein belegtes Ziel wird NICHT stillschweigend überschrieben: das wäre ein
                        // Cap-Verlust, den der Aufrufer nicht angeordnet hat. Wer den Slot räumen
                        // will, ruft CDELETE — sichtbar und mit eigenem Ergebnis.
                        result::ERR_NOSPACE
                    } else {
                        // Reines Umhängen: derselbe Cap, keine Ableitung, Budget unverändert
                        // (ein Slot frei, einer belegt) -> Policy-Prüfung wäre hier ohne Inhalt.
                        g.pds.clear_cap(pd, src_slot);
                        g.pds.install_cap(pd, dst_slot, src);
                        result::OK
                    }
                }
                _ => {
                    let Some(src) = g.pds.cap_at(pd, src_slot) else {
                        return deny(result::ERR_BADCAP);
                    };
                    let Some((_, have, _)) = g.cspace.lookup(src) else {
                        return deny(result::ERR_BADCAP);
                    };
                    if dst_slot >= PdTable::caps_per_pd() || g.pds.cap_at(pd, dst_slot).is_some() {
                        result::ERR_NOSPACE
                    } else if !g.budget_allows(pd, dst_slot) {
                        result::ERR_NOSPACE
                    } else {
                        // **Nie mehr Rechte als das Original.** Die Maske kann nur einschränken;
                        // ein Programm, das RWX anfordert, bekommt den Schnitt mit dem, was es
                        // selbst hat — sonst wäre CCOPY ein Rechte-Aufwertungsdienst.
                        let want = rights_from_bits(mask).intersect(have);
                        // `0` = Badge erben. Ein eigenes Badge ist erlaubt, weil der Aufrufer das
                        // Objekt bereits besitzt: er vergibt ein Etikett auf eigener Autoritaet.
                        let derived = if new_badge != 0 {
                            g.cspace.mint(src, want, new_badge)
                        } else {
                            g.cspace.copy(src, want)
                        };
                        match derived {
                            Ok(new) => {
                                if g.install_cap_checked(pd, dst_slot, new) {
                                    result::OK
                                } else {
                                    orphan = Some(new);
                                    result::ERR_RIGHTS
                                }
                            }
                            Err(_) => result::ERR_NOSPACE,
                        }
                    }
                }
            }
        }; // CAPS freigegeben
        if let Some(c) = orphan {
            let _ = delete_cap(c);
        }
        return deny(code); // `deny` setzt nur x0 und gibt den Frame zurück — auch für OK richtig
    }

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
            // `ep_id` kommt aus der (cap-geprüften) Endpoint-Cap; die Schranke wird zusätzlich
            // spekulationssicher maskiert (s. `cap_at`), da die Cap-Auswahl EL0-gesteuert ist.
            let ep = array_index_nospec(ep_id as usize, eps.len());
            if ep_id as usize >= eps.len() {
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
                        let replaced = match e.caller() {
                            Some(caller) => grant_cap(&mut g, caller, pd, (tag & 0xff) as usize),
                            None => None,
                        };
                        drop(g); // CAPS vor dem Rendezvous freigeben (EPS bleibt gehalten)
                        let next = e.reply(ops, core, frame);
                        drop(e); // EPS freigeben, BEVOR das Löschen CAPS/MEM/SCHEDS nimmt
                        // Die im Empfangs-Slot verdrängte Cap freigeben (sonst bliebe sie
                        // unerreichbar in der geteilten Tabelle belegt -> Cross-PD-DoS).
                        if let Some(old) = replaced {
                            let _ = delete_cap(old);
                        }
                        next
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
            let n = array_index_nospec(id as usize, ntfns.len());
            if id as usize >= ntfns.len() {
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
            //
            // **A-5.1: MMIO- und DMA-Caps gehen hier ebenfalls durch.** Bis dahin mappte der
            // Kernel die Fenster eines HardwareLand-Backends beim Erzeugen selbst; ein Treiber,
            // der sein Gerät erst *bekommt* und dann bedient, muss es aber selbst aufziehen und
            // wieder loslassen können — sonst ist „Hot-Reload" eine Eigenschaft, die der Kernel
            // für ihn ausübt.
            //
            // Was zurückkommt, ist die **Beschreibung des Fensters**, nicht neue Autorität: Basis,
            // Länge und (bei DMA) die Gerätesicht der Region, die der Aufrufer bereits hält. Ohne
            // sie bräuchte ein Treiber einen Boot-Info-Block, den jedes Programm ungefragt lesen
            // kann — und der wäre eine Autoritätsquelle neben dem Manifest (s. `loader::boot_arg`).
            // `ro_kind` = "das OBJEKT verlangt schreibgeschuetzt", unabhaengig von den Rechten.
            // Nur eine DMA-Cap in Richtung `DeviceRead` sagt das: dort liest das Geraet, und der
            // Puffer ist gegen es schreibgeschuetzt. Ein MMIO-Fenster sagt es NICHT -- Register
            // schreibt man, sonst steuert man nichts. (Genau hier lag beim ersten Anlauf ein
            // Fehler: MMIO pauschal `ro` gemappt, und der Treiber faultete beim allerersten
            // Schreiben auf `device_status`. Ob ein Fenster schreibbar ist, sagt die **Cap**.)
            let (base, len, dma, ro_kind) = match kind {
                ObjectKind::Memory(r) => (r.base, r.len, None, false),
                ObjectKind::Mmio { phys, len } => (phys, len, None, false),
                ObjectKind::Dma { phys, len, dir, coherence } => (
                    phys,
                    len,
                    Some(coherence == DmaCoherence::Coherent),
                    dir == DmaDir::DeviceRead,
                ),
                _ => return deny(result::ERR_BADCAP),
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
            // Geräte-Fenster (MMIO/DMA) laufen über `map_window`: ihre Speicherattribute folgen
            // dem Objekt, nicht den Rechten (s. Trait-Doku). Plain-RAM bleibt auf `map_frame`.
            let is_device = !matches!(kind, ObjectKind::Memory(_));
            if is_device {
                // Ein Register-Fenster nur-lesbar zu mappen wäre ein Treiber, der nichts
                // steuern kann; das Schreibrecht muss aus der Cap kommen.
                let ro = ro_kind || !rights.contains(Rights::WRITE);
                if nr == sys::MAP {
                    match ops.map_window(thread, base, len, ro, dma) {
                        Some(iova) => {
                            frame_set_reg(frame, reg::SYSNO_RESULT, result::OK);
                            frame_set_reg(frame, reg::MSG0, base);
                            frame_set_reg(frame, reg::MSG1, len);
                            frame_set_reg(frame, reg::MSG2, iova);
                        }
                        None => frame_set_reg(frame, reg::SYSNO_RESULT, result::ERR_BADCAP),
                    }
                } else {
                    let ok = ops.unmap_window(thread, base, len);
                    frame_set_reg(
                        frame,
                        reg::SYSNO_RESULT,
                        if ok { result::OK } else { result::ERR_BADCAP },
                    );
                }
                return frame;
            }
            let ok = if nr == sys::MAP {
                ops.map_frame(thread, base, len, perm_code)
            } else {
                ops.unmap_frame(thread, base, len)
            };
            if ok && nr == sys::MAP {
                frame_set_reg(frame, reg::MSG0, base);
                frame_set_reg(frame, reg::MSG1, len);
                frame_set_reg(frame, reg::MSG2, 0);
            }
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
            let badge = frame_reg(frame, reg::MSG0 + 2); // x4: Badge fuer die delegierte Cap (0=erben)
            // Den zu delegierenden Caller-Cap (x3) aufloesen + ableiten (CDT-Kind, gleiche Rechte)
            // unter CAPS.write; danach CAPS freigeben -- der Loader re-lockt CAPS/MEM/SCHEDS selbst.
            //
            // **Badge (x4):** wer eine Notification/einen Endpoint weiterreicht, darf der Kopie ein
            // eigenes Badge geben. Das ist keine Bequemlichkeit: `SYS_SIGNAL` verodert das Badge der
            // benutzten CAP, nicht ein Nachrichtenwort — ohne unterschiedliche Badges beim Vergeben
            // sind die Signale zweier Kinder ununterscheidbar. Erlaubt ist es, weil der Aufrufer das
            // Objekt bereits besitzt: er vergibt ein Etikett auf seiner eigenen Autorität, er
            // erwirbt keine neue. `0` = Badge des Originals erben (bisheriges Verhalten).
            let endowed = if delegate != u64::MAX {
                let mut g = caps.write();
                let Some(c) = g.pds.cap_at(pd, delegate as usize) else {
                    return deny(result::ERR_BADCAP);
                };
                let Some((_, r, _)) = g.cspace.lookup(c) else {
                    return deny(result::ERR_BADCAP);
                };
                let derived = if badge != 0 {
                    g.cspace.mint(c, r, badge)
                } else {
                    g.cspace.copy(c, r)
                };
                match derived {
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
            match load(index, pd, endow) {
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

/// Eine Rechte-Bitmaske aus der ABI (1=R, 2=W, 4=X) in [`Rights`] übersetzen. Unbekannte Bits
/// werden **ignoriert**, nicht abgelehnt: `Rights` kennt heute genau drei, und eine Ablehnung
/// würde ein Programm brechen, das aus Bequemlichkeit `u64::MAX` schickt — es bekommt so den
/// Schnitt mit dem, was es hat, und das ist die sichere Richtung.
fn rights_from_bits(mask: u64) -> Rights {
    let mut r = Rights::NONE;
    if mask & 1 != 0 {
        r = r.union(Rights::READ);
    }
    if mask & 2 != 0 {
        r = r.union(Rights::WRITE);
    }
    if mask & 4 != 0 {
        r = r.union(Rights::EXEC);
    }
    r
}

/// Eine Capability vom Server (lokaler Slot `grant_slot` in PD `server_pd`) an den
/// `caller` delegieren: im globalen `CapSpace` ableiten (Kind im CDT) und in den
/// Cspace des Aufrufer-PDs an [`GRANT_RECV_SLOT`](sel4lake_abi::GRANT_RECV_SLOT)
/// eintragen. Läuft unter dem gehaltenen `CAPS`-Lock (vor dem REPLY-Rendezvous, also
/// bevor der Aufrufer geweckt wird/laufen kann).
///
/// **Rückgabe:** die Cap, die im Empfangs-Slot **verdrängt** wurde (falls dort schon eine
/// lag) — der Aufrufer MUSS sie anschließend löschen. Ohne das bliebe sie für immer im
/// globalen `CapSpace` belegt, ohne von irgendeiner PD noch erreichbar zu sein: ein Server,
/// der wiederholt mit `GRANT_FLAG` antwortet, könnte so die **systemweit geteilte**
/// Slot-/Objekt-Tabelle erschöpfen und allen PDs jede weitere Cap-Installation verwehren.
/// Das Löschen passiert bewusst NICHT hier: die Finalisierung kann Speicher freigeben
/// (Allokator) bzw. einen Call abbrechen (EPS/SCHEDS) — beides ist unter dem hier
/// gehaltenen `CAPS`+`EPS`-Paar sperrordnungswidrig. Der Dispatch erledigt es, nachdem
/// beide Locks gefallen sind (s. [`dispatch`]).
fn grant_cap(
    caps: &mut Caps,
    caller: ThreadId,
    server_pd: usize,
    grant_slot: usize,
) -> Option<CapPtr> {
    let src = caps.pds.cap_at(server_pd, grant_slot)?;
    let cpd = caps.pds.pd_of(caller)?;
    // **Domänen-Policy VOR der Ableitung prüfen** (konsistent zum zentralen Enforcement-Punkt
    // install_cap_checked) — sonst könnte ein Server eine policy-fremde Cap (z. B. HW-/Loader-/
    // PdControl-Cap) per Grant in eine fremde Domäne schleusen (Bypass; bisher nur von domain_audit
    // nachträglich DETEKTIERT statt VERHINDERT). Die Kopie hat dieselbe Art/Id wie `src` -> die
    // Prüfung auf `src` ist äquivalent; bei Ablehnung wird gar keine Kopie erzeugt (kein Leck).
    if !caps.cap_allowed(cpd, src) {
        return None;
    }
    // Budget des EMPFÄNGERS prüfen, bevor eine Kopie entsteht (ein Grant darf das Cap-Budget
    // einer PD nicht umgehen — sonst wäre der Grant-Pfad das Schlupfloch um die Schranke aus
    // `install_cap_checked`). Ein belegter Empfangs-Slot wird ersetzt, nicht zusätzlich belegt.
    // A-3.2: **der Empfänger** bestimmt den Slot (`SYS_SETRECV`), nicht der Sender. Ohne das
    // landete jede gegrantete Cap im festen Slot 1 — ein Server konnte damit den Cap verdrängen,
    // den sein Client dort zufällig hielt. Wer den Slot wählt, schreibt in fremdes Eigentum.
    let recv_slot = caps.pds.recv_slot(cpd);
    if !caps.budget_allows(cpd, recv_slot) {
        return None;
    }
    // Cap ableiten (erbt die Rechte) und beim Aufrufer eintragen (Policy bereits geprüft).
    // Der zuvor dort liegende Cap wird verdrängt und zum Löschen zurückgemeldet.
    match caps.cspace.copy(src, Rights::RWX) {
        Ok(new_cap) => {
            let replaced = caps.pds.cap_at(cpd, recv_slot);
            caps.pds.install_cap(cpd, recv_slot, new_cap);
            replaced
        }
        Err(_) => None,
    }
}
