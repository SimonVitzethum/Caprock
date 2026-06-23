//! Capability-Space: Slot-Tabelle + Capability-Derivation-Tree (CDT).

use crate::object::{Object, ObjectKind};
use sel4lake_mem::{MemoryCap, PhysAllocator, Rights};

/// Anzahl Capability-Slots (CTEs) im Space.
const NSLOTS: usize = 256;
/// Anzahl verwaltbarer Objekte.
const NOBJECTS: usize = 128;

/// Sicheres Capability-Handle: Slot-Index + Generation (erkennt stale Pointer).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapPtr {
    slot: usize,
    gen: u32,
}

/// Fehler einer Capability-Operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapError {
    /// Kein freier Capability-Slot mehr.
    NoSlot,
    /// Kein freier Objekt-Eintrag mehr.
    NoObject,
    /// Ungültiges/abgelaufenes Handle.
    Invalid,
    /// Operation auf einem Cap mit Kindern (erst `revoke` nötig).
    HasChildren,
}

/// Ableitungs-Metadaten eines Slots (MDB-Knoten / CDT-Verkettung).
#[derive(Clone, Copy)]
struct Mdb {
    parent: Option<usize>,
    first_child: Option<usize>,
    next_sibling: Option<usize>,
    prev_sibling: Option<usize>,
}

impl Mdb {
    const EMPTY: Mdb = Mdb {
        parent: None,
        first_child: None,
        next_sibling: None,
        prev_sibling: None,
    };
}

/// Ein Capability-Slot (CTE: Capability + MDB-Knoten).
#[derive(Clone, Copy)]
struct CapSlot {
    used: bool,
    gen: u32,
    object: usize, // Index in die Objekt-Tabelle
    rights: Rights,
    badge: u64,
    mdb: Mdb,
}

impl CapSlot {
    const EMPTY: CapSlot = CapSlot {
        used: false,
        gen: 0,
        object: 0,
        rights: Rights::NONE,
        badge: 0,
        mdb: Mdb::EMPTY,
    };
}

/// Lesbare Sicht auf einen Capability (für Diagnose/Tests).
#[derive(Clone, Copy, Debug)]
pub struct CapInfo {
    pub kind: ObjectKind,
    pub rights: Rights,
    pub badge: u64,
    pub refcount: u32,
    pub child_count: usize,
}

/// Ein Capability-Space: Slot-Tabelle + Objekt-Tabelle.
pub struct CapSpace {
    slots: [CapSlot; NSLOTS],
    objects: [Object; NOBJECTS],
}

impl Default for CapSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl CapSpace {
    pub const fn new() -> Self {
        Self {
            slots: [CapSlot::EMPTY; NSLOTS],
            objects: [Object::EMPTY; NOBJECTS],
        }
    }

    // --- Installation eines Wurzel-Caps ---

    /// Eine [`MemoryCap`] in den Space einbringen: legt ein Memory-Objekt
    /// (refcount 1) an und einen Wurzel-Cap (ohne Eltern) darauf. Die übergebene
    /// Cap wird konsumiert — der Besitz liegt nun beim Capability-System.
    pub fn install_memory(&mut self, cap: MemoryCap) -> Result<CapPtr, CapError> {
        self.install(ObjectKind::Memory(cap.region()), cap.rights())
    }

    /// Einen Endpoint als Objekt + Wurzel-Cap (mit gegebenen Rechten) einbringen.
    pub fn install_endpoint(&mut self, ep_id: u32, rights: Rights) -> Result<CapPtr, CapError> {
        self.install(ObjectKind::Endpoint(ep_id), rights)
    }

    /// Eine Thread-Capability (Tcb) für `thread_raw` (gepacktes ThreadId) einbringen.
    pub fn install_tcb(&mut self, thread_raw: u64, rights: Rights) -> Result<CapPtr, CapError> {
        self.install(ObjectKind::Tcb(thread_raw), rights)
    }

    /// Eine Notification-Capability einbringen.
    pub fn install_notification(&mut self, ntfn_id: u32, rights: Rights) -> Result<CapPtr, CapError> {
        self.install(ObjectKind::Notification(ntfn_id), rights)
    }

    /// Wurzel-Objekt + -Cap anlegen (gemeinsame Logik für alle Objekttypen).
    fn install(&mut self, kind: ObjectKind, rights: Rights) -> Result<CapPtr, CapError> {
        let obj = self.alloc_object(kind)?;
        let slot = match self.alloc_slot(obj, rights, 0) {
            Ok(s) => s,
            Err(e) => {
                self.objects[obj].used = false; // Objekt zurückrollen
                return Err(e);
            }
        };
        Ok(self.ptr(slot))
    }

    /// Objektart, Rechte und Badge eines Caps auflösen (für cap-gesicherte
    /// Invokation; der Badge identifiziert z. B. die Signalquelle bei Notifications).
    pub fn lookup(&self, ptr: CapPtr) -> Option<(ObjectKind, Rights, u64)> {
        let slot = self.resolve(ptr).ok()?;
        let obj = self.slots[slot].object;
        Some((
            self.objects[obj].kind,
            self.slots[slot].rights,
            self.slots[slot].badge,
        ))
    }

    // --- Ableitungsoperationen ---

    /// Capability ableiten: neuer Kind-Cap auf dasselbe Objekt, Rechte
    /// eingeschränkt auf `src.rights ∩ rights` (keine Eskalation). Badge wird
    /// übernommen.
    pub fn copy(&mut self, src: CapPtr, rights: Rights) -> Result<CapPtr, CapError> {
        let s = self.resolve(src)?;
        let obj = self.slots[s].object;
        let new_rights = self.slots[s].rights.intersect(rights);
        let badge = self.slots[s].badge;
        let dst = self.alloc_slot(obj, new_rights, badge)?;
        self.objects[obj].refcount += 1;
        self.link_child(s, dst);
        Ok(self.ptr(dst))
    }

    /// Wie [`copy`](Self::copy), zusätzlich mit gesetztem Badge.
    pub fn mint(&mut self, src: CapPtr, rights: Rights, badge: u64) -> Result<CapPtr, CapError> {
        let dst = self.copy(src, rights)?;
        self.slots[dst.slot].badge = badge;
        Ok(dst)
    }

    /// Cap in einen frischen Slot verschieben (Identität/Position im CDT bleibt).
    /// Das alte Handle wird ungültig; das neue wird zurückgegeben.
    pub fn move_cap(&mut self, src: CapPtr) -> Result<CapPtr, CapError> {
        let s = self.resolve(src)?;
        let dst = self.free_slot_index()?;

        // Inhalt übernehmen (Generation des Zielslots beibehalten).
        let gen = self.slots[dst].gen;
        self.slots[dst] = self.slots[s];
        self.slots[dst].gen = gen;

        // Alle Verweise auf `s` auf `dst` umbiegen.
        let mdb = self.slots[s].mdb;
        match mdb.prev_sibling {
            Some(p) => self.slots[p].mdb.next_sibling = Some(dst),
            None => {
                if let Some(par) = mdb.parent {
                    self.slots[par].mdb.first_child = Some(dst);
                }
            }
        }
        if let Some(n) = mdb.next_sibling {
            self.slots[n].mdb.prev_sibling = Some(dst);
        }
        let mut child = mdb.first_child;
        while let Some(c) = child {
            self.slots[c].mdb.parent = Some(dst);
            child = self.slots[c].mdb.next_sibling;
        }

        // Quellslot freigeben (Generation erhöhen -> altes Handle ungültig).
        self.release_slot(s);
        Ok(self.ptr(dst))
    }

    /// Einen Cap löschen. Der Cap darf keine Kinder haben (sonst `HasChildren`;
    /// vorher `revoke`). Wird damit die letzte Referenz auf das Objekt entfernt,
    /// wird das Objekt finalisiert (Speicher an `alloc` zurückgegeben).
    pub fn delete(&mut self, alloc: &mut PhysAllocator, ptr: CapPtr) -> Result<(), CapError> {
        let slot = self.resolve(ptr)?;
        if self.slots[slot].mdb.first_child.is_some() {
            return Err(CapError::HasChildren);
        }
        self.delete_leaf(alloc, slot);
        Ok(())
    }

    /// Alle Abkömmlinge von `ptr` rekursiv löschen (der Cap selbst bleibt).
    /// Anschließend ist `ptr` kinderlos.
    pub fn revoke(&mut self, alloc: &mut PhysAllocator, ptr: CapPtr) -> Result<(), CapError> {
        let slot = self.resolve(ptr)?;
        // Wiederholt zu einem Blatt unterhalb von `slot` absteigen und löschen.
        while let Some(child) = self.slots[slot].mdb.first_child {
            let mut leaf = child;
            while let Some(c) = self.slots[leaf].mdb.first_child {
                leaf = c;
            }
            self.delete_leaf(alloc, leaf);
        }
        Ok(())
    }

    // --- Inspektion ---

    /// Sicht auf einen Cap (oder `None` bei ungültigem Handle).
    pub fn inspect(&self, ptr: CapPtr) -> Option<CapInfo> {
        let slot = self.resolve(ptr).ok()?;
        let obj = self.slots[slot].object;
        Some(CapInfo {
            kind: self.objects[obj].kind,
            rights: self.slots[slot].rights,
            badge: self.slots[slot].badge,
            refcount: self.objects[obj].refcount,
            child_count: self.child_count(slot),
        })
    }

    // --- intern ---

    fn ptr(&self, slot: usize) -> CapPtr {
        CapPtr {
            slot,
            gen: self.slots[slot].gen,
        }
    }

    fn resolve(&self, ptr: CapPtr) -> Result<usize, CapError> {
        if ptr.slot < NSLOTS && self.slots[ptr.slot].used && self.slots[ptr.slot].gen == ptr.gen {
            Ok(ptr.slot)
        } else {
            Err(CapError::Invalid)
        }
    }

    fn free_slot_index(&self) -> Result<usize, CapError> {
        self.slots
            .iter()
            .position(|s| !s.used)
            .ok_or(CapError::NoSlot)
    }

    fn alloc_slot(&mut self, object: usize, rights: Rights, badge: u64) -> Result<usize, CapError> {
        let i = self.free_slot_index()?;
        let gen = self.slots[i].gen;
        self.slots[i] = CapSlot {
            used: true,
            gen,
            object,
            rights,
            badge,
            mdb: Mdb::EMPTY,
        };
        Ok(i)
    }

    fn release_slot(&mut self, slot: usize) {
        let gen = self.slots[slot].gen.wrapping_add(1);
        self.slots[slot] = CapSlot::EMPTY;
        self.slots[slot].gen = gen;
    }

    fn alloc_object(&mut self, kind: ObjectKind) -> Result<usize, CapError> {
        let i = self
            .objects
            .iter()
            .position(|o| !o.used)
            .ok_or(CapError::NoObject)?;
        let gen = self.objects[i].gen;
        self.objects[i] = Object {
            used: true,
            kind,
            refcount: 1,
            gen,
        };
        Ok(i)
    }

    fn child_count(&self, slot: usize) -> usize {
        let mut n = 0;
        let mut c = self.slots[slot].mdb.first_child;
        while let Some(i) = c {
            n += 1;
            c = self.slots[i].mdb.next_sibling;
        }
        n
    }

    /// `child` vorne in die Kinderliste von `parent` einhängen.
    fn link_child(&mut self, parent: usize, child: usize) {
        let old_first = self.slots[parent].mdb.first_child;
        self.slots[child].mdb.parent = Some(parent);
        self.slots[child].mdb.prev_sibling = None;
        self.slots[child].mdb.next_sibling = old_first;
        if let Some(f) = old_first {
            self.slots[f].mdb.prev_sibling = Some(child);
        }
        self.slots[parent].mdb.first_child = Some(child);
    }

    /// `slot` aus der Geschwister-/Kinderverkettung lösen.
    fn unlink(&mut self, slot: usize) {
        let mdb = self.slots[slot].mdb;
        match mdb.prev_sibling {
            Some(p) => self.slots[p].mdb.next_sibling = mdb.next_sibling,
            None => {
                if let Some(par) = mdb.parent {
                    self.slots[par].mdb.first_child = mdb.next_sibling;
                }
            }
        }
        if let Some(n) = mdb.next_sibling {
            self.slots[n].mdb.prev_sibling = mdb.prev_sibling;
        }
        self.slots[slot].mdb = Mdb::EMPTY;
    }

    /// Ein Blatt (Cap ohne Kinder) löschen: aushängen, Refcount senken,
    /// ggf. Objekt finalisieren (Speicher zurückgeben), Slot freigeben.
    fn delete_leaf(&mut self, alloc: &mut PhysAllocator, slot: usize) {
        let obj = self.slots[slot].object;
        self.unlink(slot);
        self.release_slot(slot);

        self.objects[obj].refcount -= 1;
        if self.objects[obj].refcount == 0 {
            // Memory-Objekte geben ihre Region an den Allokator zurück;
            // Endpoints halten keinen Allokator-Speicher (Zustand im IPC-Subsystem).
            if let ObjectKind::Memory(region) = self.objects[obj].kind {
                alloc.free_region(region);
            }
            let gen = self.objects[obj].gen.wrapping_add(1);
            self.objects[obj] = Object::EMPTY;
            self.objects[obj].gen = gen;
        }
    }
}
