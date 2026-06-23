//! Kernel-Objekte, auf die Capabilities verweisen.
//!
//! Mehrere Caps können dasselbe Objekt referenzieren (über `copy`/`mint`); ein
//! Referenzzähler bestimmt, wann das Objekt finalisiert wird.

use sel4lake_mem::PhysRegion;

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
}

/// Eintrag der Objekt-Tabelle.
#[derive(Clone, Copy)]
pub(crate) struct Object {
    pub used: bool,
    pub kind: ObjectKind,
    /// Anzahl der auf dieses Objekt verweisenden Capabilities.
    pub refcount: u32,
    /// Generationszähler (gegen stale Objekt-Indizes).
    pub gen: u32,
}

impl Object {
    pub const EMPTY: Object = Object {
        used: false,
        kind: ObjectKind::Memory(PhysRegion::new(0, 0)),
        refcount: 0,
        gen: 0,
    };
}
