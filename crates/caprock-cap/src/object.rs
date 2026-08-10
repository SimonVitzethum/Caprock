//! Kernel-Objekte, auf die Capabilities verweisen.
//!
//! Mehrere Caps können dasselbe Objekt referenzieren (über `copy`/`mint`); ein
//! Referenzzähler bestimmt, wann das Objekt finalisiert wird.

use caprock_mem::PhysRegion;

/// **DMA-Richtung** (ext-24): die Zugriffsrichtung des **Geräts** auf den DMA-Puffer. Bestimmt
/// die richtungsminimalen Hardware-Rechte (SMMU-Stage-1-AP) — ein reiner Lese-Puffer ist
/// gegen ein fehlerhaftes Gerät **schreibgeschützt**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaDir {
    /// Das Gerät **liest** aus dem Puffer (CPU schreibt, Gerät liest). SMMU: read-only.
    DeviceRead,
    /// Das Gerät **schreibt** in den Puffer (Gerät schreibt, CPU liest). SMMU: read-write.
    DeviceWrite,
    /// Beide Richtungen. SMMU: read-write.
    Bidirectional,
}

/// **Cache-Kohärenz** (ext-24) eines DMA-Puffers gegenüber der CPU. Bestimmt die Speicher-
/// Attribute (cacheable vs. non-cacheable) und ob Cache-Maintenance (clean/invalidate) um
/// Transfers nötig ist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaCoherence {
    /// Hardware-kohärent (CCI/ACE): Normal **Cacheable**; Maintenance per `dma_prepare`/
    /// `dma_complete` (clean vor Geräte-Read, invalidate nach Geräte-Write).
    Coherent,
    /// Nicht kohärent: Normal **Non-Cacheable**; keine CPU-Cache-Maintenance nötig (Default,
    /// rückwärtskompatibel zu ext-23).
    NonCoherent,
}

/// Art des Objekts, auf das eine Capability verweist. Notifications, TCBs usw.
/// kommen in späteren Phasen hinzu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// Eine physische Speicherregion.
    Memory(PhysRegion),
    /// Ein IPC-Endpoint (referenziert per Endpoint-ID; Zustand liegt in der
    /// Endpoint-Tabelle des IPC-Subsystems).
    Endpoint(u32),
    /// Ein Thread (Thread-Control-Block, referenziert per gepacktem ThreadId-Raw).
    /// Ermöglicht capability-kontrolliertes Beenden (`KILL`).
    Tcb(u64),
    /// Ein Notification-Objekt (asynchrone Badge-Signale; referenziert per ID).
    Notification(u32),
    /// Ein **Scheduling Context** (MCS): CPU-Budget (`budget` Ticks je `period`
    /// Ticks). Die Cap ist die **Autorität**, einem Thread dieses Budget zuzuweisen
    /// — CPU-Zeit wird damit kapabilitätskontrolliert vergeben.
    SchedContext { budget: u32, period: u32 },
    /// Eine **Reply-Capability** (seL4-artig): die einmalige Autorität, einen konkreten
    /// per `CALL` blockierten Aufrufer (`caller`, gepacktes ThreadId-Raw) an Endpoint
    /// `ep` zu beantworten bzw. den Call abzubrechen. Wird beim Löschen/Revoke
    /// **finalisiert** -> der noch wartende Aufrufer wird mit `ERR_SERVER_GONE`
    /// entblockt (Revocation eines ausstehenden Calls). Genau einer je Call,
    /// call-spezifisch (matcht nur, solange `ep` noch diesen `caller` hält).
    Reply { ep: u32, caller: u64 },
    /// Eine **Management-Capability** (ext-22): die Autorität einer TrustedSas-PD, den
    /// Lifecycle einer Ziel-PD `pd` (typisch UserLand) zu steuern — starten/stoppen/
    /// pausieren/fortsetzen/Budget zuweisen/Cap übergeben/Reload (`SYS_PDCTL`). Nicht jede
    /// TrustedSas-PD bekommt sie; sie ist die explizite Steuerungsberechtigung über genau
    /// diese eine Ziel-PD. Hält keinen Allokator-Speicher (keine Finalisierung).
    PdControl { pd: u32 },
    /// Eine **Loader-Capability** (ext-26): die Autorität einer TrustedSas-PD, über den
    /// generischen Binary-Loader ein Programm aus `source` (0 = Boot-Archiv) zur Laufzeit zu
    /// laden + zu starten (`SYS_LOAD`). Wie `PdControl` eine reine **Autoritäts**-Cap (kein
    /// Allokator-Speicher, keine Finalisierung); nur in TrustedSas-PDs installierbar. Sie gewährt
    /// **keine** Sonderrechte am geladenen Prozess — dieser erhält nur die Caps, die der Aufrufer
    /// im `SYS_LOAD` explizit aus seinem eigenen Cspace delegiert.
    Loader { source: u32 },
    /// Eine **MMIO-Capability** (ext-22, HardwareLand): die Autorität, eine konkrete
    /// Geräte-Registerregion `[phys, phys+len)` in die eigene (isolierte) VSpace als
    /// EL0-Device zu mappen. Wird **nur kernelseitig** geprägt (kein User-Syscall erzeugt
    /// beliebige MMIO-Caps) und ist nur in HardwareLand-PDs installierbar. Verweist auf
    /// einen **Geräte**-Bereich (kein RAM-Allokator-Eintrag) -> keine Finalisierung.
    Mmio { phys: u64, len: u64 },
    /// Eine **IRQ-Capability** (ext-22, HardwareLand): die Autorität, den Geräte-Interrupt
    /// `intid` zu empfangen — der Kernel bindet ihn an eine Notification und stellt ihn dem
    /// Backend als Badge-Signal zu (`bind_irq`). Nur kernelseitig geprägt, nur in
    /// HardwareLand-PDs installierbar. Hält keinen RAM-Allokator-Eintrag -> keine Finalisierung.
    Irq { intid: u32 },
    /// Eine **DMA-Capability** (ext-23, HardwareLand): die Autorität über eine kernel-
    /// ausgeschnittene, kontiguierliche **RAM**-Region `[phys, phys+len)`, die als DMA-Puffer
    /// dient (Gerät liest/schreibt sie per Bus-Master). Nur kernelseitig geprägt, nur in
    /// HardwareLand-PDs installierbar. **Anders als Mmio/Irq ist dies echtes RAM** -> die
    /// Finalisierung gibt die Region an den Allokator zurück (`free_region`), **aber nur** weil
    /// die System-Teardown-Reihenfolge (`enforcer.disable_dma` -> VSpace-Unmap) garantiert, dass
    /// vorher kein Gerät mehr hineinschreiben kann (DMA-use-after-free-sicher). Die hardware-
    /// erzwungene Isolation (SMMUv3) liegt hinter der `DmaEnforcer`-Abstraktion im Kernel.
    ///
    /// ext-24: die Cap kodiert zusätzlich die **Richtung** (`dir`, → richtungsminimale SMMU-
    /// Rechte) und die **Cache-Kohärenz** (`coherence`, → Speicher-Attribute + Maintenance).
    /// Die Felder sind additiv; `install_dma` ohne sie nutzt `Bidirectional`/`NonCoherent`
    /// (= ext-23-Verhalten).
    Dma {
        phys: u64,
        len: u64,
        dir: DmaDir,
        coherence: DmaCoherence,
    },
}

/// Eintrag der Objekt-Tabelle.
///
/// **Öffentlich, aber undurchsichtig** (A-3.4): seit die Objekttabelle zur Boot-Zeit
/// dimensioniert wird, legt der *Kernel* den Speicher an (`Slab<Object>`) und braucht dafür
/// den Typ und [`Object::EMPTY`]. Die Felder bleiben `pub(crate)` — von aussen ist ein
/// `Object` ein Platzhalter ohne Innenleben, insbesondere ist `refcount` nicht von aussen
/// veränderbar. Nur so bleibt die Cap-Crate frei von `unsafe` (`forbid(unsafe_code)`), ohne
/// ihre Kapselung dafür aufzugeben.
#[derive(Clone, Copy)]
pub struct Object {
    pub(crate) used: bool,
    pub(crate) kind: ObjectKind,
    /// Anzahl der auf dieses Objekt verweisenden Capabilities.
    pub(crate) refcount: u32,
    /// Generationszähler (gegen stale Objekt-Indizes).
    pub(crate) gen: u32,
}

impl Object {
    pub const EMPTY: Object = Object {
        used: false,
        kind: ObjectKind::Memory(PhysRegion::new(0, 0)),
        refcount: 0,
        gen: 0,
    };
}
