#![no_std]
//! **Slab** — eine zur *Boot-Zeit* dimensionierte Tabelle.
//!
//! Bis ext-30 waren alle Kerneltabellen `.bss`-Arrays mit Compile-Zeit-Konstanten
//! (`[Tcb; PER_CORE]`, `[FpState; MAX_THREADS]`, …). Für das Zielbild — Dual-EPYC-Klasse,
//! 256 Kerne, viele tausend Prozesse — geht das nicht: die Arrays müssten für den
//! **schlimmsten** Fall dimensioniert werden und lägen auch dann im Image, wenn die
//! Maschine klein ist. Ein `Slab<T>` trennt **Typ** von **Kapazität**: die Struktur wird
//! `const` (leer) angelegt — statische Instanzen bleiben also möglich — und bekommt ihren
//! Speicher erst beim Boot, wenn Kernzahl und RAM bekannt sind.
//!
//! ## Sicherheitsvertrag
//!
//! Die einzige `unsafe`-Stelle ist [`Slab::attach`]: sie nimmt Rohspeicher entgegen und
//! **initialisiert jedes Element** (`ptr::write`) — der Speicher darf also uninitialisiert
//! sein. Ab da ist jeder Zugriff bounds-geprüft; `Index`/`IndexMut` paniken bei
//! Überschreitung wie ein gewöhnliches Array (kein UB, kein stiller Fehlzugriff).
//!
//! Ein Slab **besitzt** seinen Speicher für immer (kein `Drop`, keine Rückgabe): Kernel-
//! tabellen leben bis zum Reboot. Das hält den Typ frei von Lebenszeit-Parametern und
//! macht ihn in `static`s verwendbar.

use core::ops::{Index, IndexMut};

/// Eine zur Boot-Zeit dimensionierte Tabelle von `T`.
///
/// Leer konstruierbar (`const`), später **einmalig** mit Speicher versehen.
pub struct Slab<T: 'static> {
    ptr: *mut T,
    len: usize,
}

// SAFETY: Ein `Slab` ist ein Besitz-Handle auf `len` Elemente. Er verhält sich damit wie
// ein `[T]`-Besitzer: über `&mut Slab` gibt es exklusiven, über `&Slab` geteilten Zugriff.
// `Send` verlangt daher nur `T: Send`, `Sync` nur `T: Sync` — exakt wie bei `[T]`.
unsafe impl<T: Send> Send for Slab<T> {}
unsafe impl<T: Sync> Sync for Slab<T> {}

impl<T> Slab<T> {
    /// Leerer Slab (Länge 0). Für `static`-Instanzen, die erst beim Boot Speicher bekommen.
    pub const fn empty() -> Self {
        Self {
            ptr: core::ptr::null_mut(),
            len: 0,
        }
    }

    /// Dem Slab seinen Speicher geben und **jedes** Element mit `init(i)` initialisieren.
    ///
    /// # Safety
    /// * `ptr` zeigt auf mindestens `len * size_of::<T>()` Bytes, korrekt ausgerichtet für
    ///   `T`, und ist **exklusiv** für diesen Slab (niemand sonst hält einen Verweis).
    /// * Der Speicher lebt so lange wie der Slab (Kerneltabellen: bis zum Reboot).
    /// * Der Inhalt darf **uninitialisiert** sein — diese Funktion schreibt jedes Element.
    /// * Nur **einmal** je Slab aufrufen (ein zweiter Aufruf würde den alten Speicher
    ///   vergessen, ohne die alten Werte zu verwerfen).
    pub unsafe fn attach(&mut self, ptr: *mut T, len: usize, mut init: impl FnMut(usize) -> T) {
        for i in 0..len {
            // SAFETY: `i < len`, also liegt `ptr+i` in der zugesicherten Zuteilung. `write`
            // initialisiert, ohne den (uninitialisierten) Altwert zu lesen/verwerfen.
            unsafe { core::ptr::write(ptr.add(i), init(i)) };
        }
        self.ptr = ptr;
        self.len = len;
    }

    /// Anzahl der Elemente (0, solange kein Speicher zugewiesen wurde).
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Element `i` oder `None` bei Überschreitung.
    pub fn get(&self, i: usize) -> Option<&T> {
        if i < self.len {
            // SAFETY: `i < len` und der Speicher ist per `attach`-Vertrag gültig +
            // initialisiert; `&self` erlaubt geteilten Zugriff.
            Some(unsafe { &*self.ptr.add(i) })
        } else {
            None
        }
    }

    /// Element `i` veränderbar, oder `None` bei Überschreitung.
    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        if i < self.len {
            // SAFETY: wie `get`; `&mut self` erlaubt exklusiven Zugriff.
            Some(unsafe { &mut *self.ptr.add(i) })
        } else {
            None
        }
    }

    /// Alle Elemente als Slice.
    pub fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `attach`-Vertrag — `len` initialisierte, exklusiv gehaltene Elemente.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Alle Elemente als veränderbarer Slice.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        if self.len == 0 {
            return &mut [];
        }
        // SAFETY: wie `as_slice`, exklusiv über `&mut self`.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }
}

impl<T> Index<usize> for Slab<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        self.get(i).expect("Slab-Index ausserhalb der Kapazitaet")
    }
}

impl<T> IndexMut<usize> for Slab<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        self.get_mut(i).expect("Slab-Index ausserhalb der Kapazitaet")
    }
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self::empty()
    }
}

/// **Lock-frei lesbare Tabelle von Atomics**, zur Boot-Zeit dimensioniert.
///
/// Für Tabellen, die aus **jedem** Kern ohne Lock gelesen werden müssen (Thread-Directory,
/// `VSPACE_OF`, …). Ein [`Slab`] taugt dafür nicht: er bräuchte `&mut` zum Anhängen und
/// läge damit hinter einem Lock, der genau den heißen Lesepfad serialisieren würde.
///
/// `T` ist ein Atomic-Typ (`T: Sync`) — geteilter Zugriff ist damit datenrennenfrei; die
/// Tabelle selbst wird **einmal beim Boot** angehängt (Zeiger + Länge per `Release`
/// veröffentlicht, Leser per `Acquire`).
pub struct AtomicTable<T: 'static> {
    ptr: core::sync::atomic::AtomicPtr<T>,
    len: core::sync::atomic::AtomicUsize,
}

// SAFETY: Zugriff auf Elemente gibt es nur als `&T` mit `T: Sync`; die Veröffentlichung von
// Zeiger+Länge ist Release/Acquire-geordnet.
unsafe impl<T: Sync> Sync for AtomicTable<T> {}
unsafe impl<T: Sync> Send for AtomicTable<T> {}

impl<T: Sync> AtomicTable<T> {
    pub const fn empty() -> Self {
        Self {
            ptr: core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()),
            len: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Speicher zuweisen + alle Elemente mit `init(i)` initialisieren.
    ///
    /// # Safety
    /// Wie [`Slab::attach`]. Zusätzlich: **nur beim Boot** aufrufen, bevor andere Kerne
    /// lesen können (die Initialisierung selbst ist nicht atomar gegen Leser).
    pub unsafe fn attach(&self, ptr: *mut T, len: usize, mut init: impl FnMut(usize) -> T) {
        use core::sync::atomic::Ordering;
        for i in 0..len {
            // SAFETY: `i < len`, Speicher laut Vertrag gültig; `write` initialisiert.
            unsafe { core::ptr::write(ptr.add(i), init(i)) };
        }
        // Länge zuerst auf 0 lassen, Zeiger setzen, dann Länge veröffentlichen: ein Leser,
        // der `len` sieht, sieht garantiert auch den fertigen Zeiger + Inhalt.
        self.ptr.store(ptr, Ordering::Release);
        self.len.store(len, Ordering::Release);
    }

    /// Element `i` (geteilt), oder `None` bei Überschreitung / noch nicht angehängt.
    pub fn get(&self, i: usize) -> Option<&T> {
        use core::sync::atomic::Ordering;
        if i >= self.len.load(Ordering::Acquire) {
            return None;
        }
        let p = self.ptr.load(Ordering::Acquire);
        if p.is_null() {
            return None;
        }
        // SAFETY: `i < len` und `len` wurde erst NACH dem Zeiger + der Initialisierung
        // veröffentlicht (Release/Acquire) -> das Element ist gültig und initialisiert.
        // `T: Sync` -> geteilter Zugriff aus mehreren Kernen ist datenrennenfrei. Der
        // Speicher lebt bis zum Reboot (Slab-Vertrag: keine Rückgabe).
        Some(unsafe { &*p.add(i) })
    }

    pub fn len(&self) -> usize {
        self.len.load(core::sync::atomic::Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// **Freiliste über Slab-Indizes** — O(1)-Belegen/Freigeben statt linearem Scan.
///
/// Bei tausenden Threads/Caps ist „ersten freien Eintrag suchen" (linearer Scan über die
/// ganze Tabelle) der eigentliche Engpass, nicht die Tabellengröße. Die Freiliste hält die
/// freien Indizes als verkettete Liste **in einem eigenen Slab** (kein Speicher-Overhead je
/// Element in der Nutztabelle, kein Allokator nötig).
pub struct FreeList {
    /// `next[i]` = nächster freier Index nach `i` ([`NONE`] am Ende).
    next: Slab<u32>,
    head: u32,
    count: usize,
}

/// Listenende / „kein Index".
pub const NONE: u32 = u32::MAX;

impl FreeList {
    pub const fn empty() -> Self {
        Self {
            next: Slab::empty(),
            head: NONE,
            count: 0,
        }
    }

    /// Speicher zuweisen und **alle** `len` Indizes als frei einhängen (`0` zuerst).
    ///
    /// # Safety
    /// Wie [`Slab::attach`] (`ptr`/`len` gültig, exklusiv, einmalig).
    pub unsafe fn attach(&mut self, ptr: *mut u32, len: usize) {
        // Kette 0 -> 1 -> … -> len-1 -> NONE; `head = 0` (falls len > 0).
        unsafe {
            self.next.attach(ptr, len, |i| {
                if i + 1 < len {
                    (i + 1) as u32
                } else {
                    NONE
                }
            })
        };
        self.head = if len == 0 { NONE } else { 0 };
        self.count = len;
    }

    /// Einen freien Index belegen (O(1)).
    pub fn alloc(&mut self) -> Option<usize> {
        if self.head == NONE {
            return None;
        }
        let i = self.head as usize;
        self.head = self.next[i];
        self.next[i] = NONE;
        self.count -= 1;
        Some(i)
    }

    /// Einen Index wieder freigeben (O(1)). Doppelte Freigabe ist ein Aufruferfehler und
    /// würde die Liste zyklisch machen — daher nur aus den Teardown-Pfaden aufrufen.
    pub fn free(&mut self, i: usize) {
        if i >= self.next.len() {
            return;
        }
        self.next[i] = self.head;
        self.head = i as u32;
        self.count += 1;
    }

    /// Anzahl noch freier Indizes.
    pub fn available(&self) -> usize {
        self.count
    }

    /// Gesamtkapazität.
    pub fn capacity(&self) -> usize {
        self.next.len()
    }
}

impl Default for FreeList {
    fn default() -> Self {
        Self::empty()
    }
}
