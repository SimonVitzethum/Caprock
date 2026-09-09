#![forbid(unsafe_code)]
//! **Linux-`kmalloc`-Schablone (A-Zustand) über dem vorhandenen [`Heap`](crate::heap::Heap).**
//!
//! Abbildung auf Linux-Begriffe (A-Zustand aus `docs/linux-kompatibilitaet-caprock.md`):
//!
//! | Linux                     | Hier                                                     |
//! |---------------------------|----------------------------------------------------------|
//! | `kmalloc(len, gfp)`       | [`Kmalloc::kmalloc`] (roher Zeiger, s. unten)            |
//! | `kfree(ptr)`              | [`Kmalloc::kfree`] (braucht zusaetzlich `len`, s. Grenze 2) |
//! | `ksize(ptr)` (truesize)   | [`ksize`] (reine Funktion der Anfrage­laenge)            |
//! | `krealloc(p, neu, gfp)`   | [`Kmalloc::krealloc`] (Umzug OHNE Byte-Kopie, s. Grenze 3) |
//! | `GFP_KERNEL`              | [`Gfp::KERNEL`] (darf ueber die Quelle wachsen)          |
//! | `GFP_ATOMIC`              | [`Gfp::ATOMIC`] (waechst nie, nur Modul-Vorrat)          |
//! | `ARCH_KMALLOC_MINALIGN`   | [`ARCH_KMALLOC_MINALIGN`] (16, Zweierpotenz)             |
//!
//! # Warum rohe Zeiger
//!
//! Die Rueckgaben sind `Option<*mut u8>` (null = `None`, nie ein hängender Zeiger), weil der
//! Linux-Shim hinter rohen Zeigern arbeitet: Er uebergibt Adressen an seiten­fremde Verbraucher
//! (Treiber-PDs, DMA-Bindung), die keine Rust-Leihen tragen koennen. Besitz bleibt trotzdem
//! eindeutig: Jeder herausgegebene Zeiger gehoert genau einem Aufrufer, bis er ihn mit passender
//! Laenge an [`Kmalloc::kfree`] zurueckgibt. Das Modul selbst dereferenziert nie — es verwaltet
//! nur Adressen als Zahlen (Ausrichtung per Modulo, Gleichheit per Vergleich).
//!
//! # A-Zustand-Grenzen (ehrlich, mit Ausbau-Richtung)
//!
//! 1. **ATOMIC/KERNEL-Trennung.** [`Heap`](crate::heap::Heap) trennt nicht: Sein Slab-Pfad fordert
//!    bei leerer Free-Liste und vollem Bump immer ueber die Quelle an. Darum bedient der
//!    ATOMIC-Pfad **ausschliesslich** den Modul-Vorrat und erreicht den Heap-Codepfad gar nicht
//!    (fruehe Rueckkehr in `bedienen`, per Zaehl­quelle deterministisch geprueft). Ausbau: sobald
//!    der Heap einen wachstumsfreien try-Pfad anbietet, darf ATOMIC zusaetzlich Bump/Free-Liste
//!    des Heaps nutzen.
//! 2. **Keine Heap-Rueckgabe.** `Heap::deallocate` ist `unsafe`, dieses Modul ist per
//!    `#![forbid(unsafe_code)]` bewusst `unsafe`-frei. Freigegebene Bloecke parken daher im
//!    Modul-Vorrat (Limit [`VORRAT_PLAETZE`]) statt in den Heap zurueck­zukehren; was bei vollem
//!    Vorrat oder unkenn­tlicher Laenge nicht parkbar ist, zaehlt [`Kmalloc::verworfen`] statt
//!    still zu versickern. Speicher­sicher ist beides: Die Bloecke bleiben Heap-besessen und
//!    werden spaetestens beim Heap-Abbau an die Quelle zurueck­gegeben — nur das
//!    Einzel­element-shrink (sofortiges `release`) fehlt noch.
//! 3. **Kein Byte-Kopieren.** [`Kmalloc::krealloc`] zieht um, kopiert aber keine Nutzlast: Kopieren
//!    braeuchte `unsafe`, das es hier nicht gibt. Die Kopie obliegt dem Shim (er arbeitet hinter
//!    rohen Zeigern und besitzt dafuer die Mittel). Ausbau: Kopie ins Modul ziehen, sobald dort
//!    ein gepruefter sicherer Kopier­pfad existiert.
//! 4. **Klassentabelle.** Die Klassen stehen hier als eigene Konstante ([`KLASSEN`]), rechnerisch
//!    identisch zu den privaten Klassen in `heap.rs` (Zweier­potenzen 16..=2048, per
//!    Uebersetzungs­zeit­pruefung festgenagelt). Ein gemeinsamer Klassen­vertrag mit dem Heap
//!    folgt, sobald dieser seine Tabelle teilt.

use crate::heap::{Heap, RegionSource};
use caprock_sync::SpinLock;
use core::alloc::{Allocator, Layout};

/// Mindestausrichtung jeder Vergabe (Bytes). Zweierpotenz, mindestens 8 — gewaehlt 16: deckt
/// Zeiger/`u64` ab und entspricht der kleinsten Heap-Klasse, sodass keine Vergabe je unter dieser
/// Schwelle liegt (Slab-Slots sind klassen­ausgerichtet ≥ 16, Gross-Regionen seiten­ausgerichtet).
pub const ARCH_KMALLOC_MINALIGN: usize = 16;

/// Groessenklassen (Bytes, aufsteigend). Rechnerisch identisch zu den privaten Klassen in
/// `heap.rs` (s. Grenze 4 oben).
pub const KLASSEN: [usize; 8] = [16, 32, 64, 128, 256, 512, 1024, 2048];

/// Groesste bedienbare Anfrage (Bytes, 1 MiB). Jenseits davon antwortet jede Vergabe mit
/// [`KmallocFehler::ZuGross`], **ohne** die Quelle zu beruehren. Schablonen-Grenze: Der Heap
/// traegt hoechstens 8 gleichzeitige Gross-Allokationen; ein Megabyte haelt grosse DMA-Puffer
/// fern, die dorthin nicht gehoeren.
pub const MAX_KMALLOC: usize = 1 << 20;

/// Plaetze im Modul-Vorrat (wieder­verwendbare Bloecke + ATOMIC-Reserve). Festes Feld statt
/// `Vec`: kein globaler Allokator noetig (Bare-Metal-tauglich), deterministische Schranke.
pub const VORRAT_PLAETZE: usize = 64;

/// Groesste bedienbare Ausrichtung (Bytes): eine Seite. Slab-Bloecke tragen ihre
/// Klassen­ausrichtung (≤ 2048), Gross-Regionen kommen laut `RegionSource`-Vertrag
/// seiten­ausgerichtet — mehr ist ohne Quell­zusatz nicht belegbar.
pub const MAX_AUSR: usize = crate::page::PAGE_SIZE as usize;

// --- Uebersetzungszeit-Naegel (reine Arithmetik, kein Speicherzugriff) ---
const _: () = assert!(ARCH_KMALLOC_MINALIGN.is_power_of_two());
const _: () = assert!(ARCH_KMALLOC_MINALIGN >= 8);
const _: () = assert!(KLASSEN[0] >= ARCH_KMALLOC_MINALIGN);
const _: () = assert!(MAX_AUSR.is_power_of_two());
const _: () = assert!(MAX_AUSR as u64 == crate::page::PAGE_SIZE);

/// Die Klassentabelle enthaelt exakt die Zweier­potenzen 16..=2048 (kein Tippfehler, keine Luecke).
const fn klassen_sind_zweierpotenzen() -> bool {
    let mut i = 0;
    while i < KLASSEN.len() {
        if KLASSEN[i] != (16usize << i) {
            return false;
        }
        i += 1;
    }
    true
}
const _: () = assert!(klassen_sind_zweierpotenzen());

/// Vergabe­kontext nach Linux-Vorbild, als Struktur mit Konstanten (kein `bitflags`-Crate).
///
/// Nur zwei Belegungen sind belegt; unbekannte Bits werden ignoriert (nicht abgewiesen — die
/// Schablone kennt noch keine Sonder­bits, und raten waere falscher als ignorieren).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gfp(u32);

impl Gfp {
    /// Darf „schlafen": Vergabe darf ueber die Quelle wachsen ([`Heap`] fordert Regionen an).
    /// Entspricht `GFP_KERNEL` ohne Sonder­bits.
    pub const KERNEL: Self = Self(0b01);
    /// Darf nicht „schlafen": Vergabe wächst nie (nur Modul-Vorrat, s. Grenze 1).
    /// Entspricht `GFP_ATOMIC` ohne Sonder­bits.
    pub const ATOMIC: Self = Self(0b10);

    /// Rohe Bits (Shim-Uebergabe, Debug).
    pub const fn bits(self) -> u32 {
        self.0
    }
    /// Vereinigung zweier Kontexte (Bit-Oder).
    pub const fn vereint(self, anderer: Self) -> Self {
        Self(self.0 | anderer.0)
    }
    /// Ob alle Bits von `anderer` gesetzt sind.
    pub const fn enthaelt(self, anderer: Self) -> bool {
        self.0 & anderer.0 == anderer.0
    }
    /// Atomarer Kontext: wahr, sobald das ATOMIC-Bit gesetzt ist — **auch neben KERNEL**.
    /// Bei gemischten Bits gewinnt die sichere Seite (nie wachsen); raten duerfen wir hier nicht.
    pub const fn ist_atomar(self) -> bool {
        self.0 & Self::ATOMIC.0 != 0
    }
}

/// Warum eine Vergabe scheiterte. Jeder Ausgang benannt — ein gemeinsames `None` wuerde
/// „zu gross" mit „darf nicht wachsen" vermengen, und das sind verschiedene Lagen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KmallocFehler {
    /// Anfrage unbedienbar: leer (`len == 0`, kein `ZERO_SIZE_PTR` im A-Zustand), jenseits
    /// [`MAX_KMALLOC`], Heap erschoepft — oder Ausrichtung abgewiesen (keine Zweier­potenz,
    /// unter [`ARCH_KMALLOC_MINALIGN`], ueber [`MAX_AUSR`]).
    ZuGross,
    /// ATOMIC-Anfrage bei leerem Vorrat. Die Quelle wurde **nicht** beruehrt (Grenze 1);
    /// der Aufrufer muss warten, stueckeln oder den Vorrat vor­waermen.
    AtomarWaechstNicht,
}

impl core::fmt::Display for KmallocFehler {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZuGross => write!(
                f,
                "Anfrage unbedienbar (leer, jenseits MAX_KMALLOC, Heap erschoepft oder Ausrichtung abgewiesen)"
            ),
            Self::AtomarWaechstNicht => {
                write!(f, "ATOMIC-Anfrage bei leerem Vorrat (kein Wachstum erlaubt)")
            }
        }
    }
}

/// Auf `vielfaches` (Zweier­potenz) aufgerundet; `None` bei Ueberlauf oder `vielfaches == 0`.
/// Reine Arithmetik — der einzige Ueberlauf­schutz, den die Schablone braucht (`usize::MAX`-
/// Anfragen duerfen nicht wrappen, s. Test `zu_gross_beruehrt_quelle_nicht`).
const fn aufrunden(wert: usize, vielfaches: usize) -> Option<usize> {
    if vielfaches == 0 || !vielfaches.is_power_of_two() {
        return None;
    }
    match wert.checked_add(vielfaches - 1) {
        Some(summe) => Some(summe & !(vielfaches - 1)),
        None => None,
    }
}

/// Bedien­schluessel einer Anfrage: zugesagte Nutz­groesse + zugesagte Ausrichtung.
/// Zwei gleiche Schluessel beschreiben austausch­bare Bloecke (Wieder­verwendung, `krealloc`-
/// Kurzschluss); ungleiche nicht.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bedarf {
    nutz: usize,
    ausr: usize,
}

/// Schluessel aus (`len`, `ausr`) — `None` = unbedienbar (s. [`KmallocFehler::ZuGross`]).
/// Reine Funktion, ohne Heap- oder Quellen­kontakt.
const fn bedarf_fuer(len: usize, ausr: usize) -> Option<Bedarf> {
    if len == 0 || len > MAX_KMALLOC {
        return None;
    }
    if !ausr.is_power_of_two() || ausr < ARCH_KMALLOC_MINALIGN || ausr > MAX_AUSR {
        return None;
    }
    // Wirksam ist das Maximum aus Wunsch und Ausrichtung (Linux-`effective`, Heap-Nachbau):
    // Wer 64-Byte-Ausrichtung fuer 16 Byte verlangt, bekommt einen 64-Byte-Slot.
    let wirksam = if len > ausr { len } else { ausr };
    let mut i = 0;
    while i < KLASSEN.len() {
        if wirksam <= KLASSEN[i] {
            return Some(Bedarf {
                nutz: KLASSEN[i],
                ausr,
            });
        }
        i += 1;
    }
    // Gross-Pfad: Nutz­groesse auf Mindest­ausrichtung gerundet; die geforderte Ausrichtung
    // (≤ Seite) traegt die seiten­ausgerichtete Region der Quelle, nicht die Groesse.
    match aufrunden(len, ARCH_KMALLOC_MINALIGN) {
        Some(nutz) => Some(Bedarf { nutz, ausr }),
        None => None,
    }
}

/// Klassengroesse („truesize") einer Anfrage­laenge: die Nutz­groesse, die eine Bedienung dieser
/// Laenge haette. `0` heisst unbedienbar (leer oder jenseits [`MAX_KMALLOC`]) — keine Zusage.
pub const fn ksize(len: usize) -> usize {
    match bedarf_fuer(len, ARCH_KMALLOC_MINALIGN) {
        Some(bedarf) => bedarf.nutz,
        None => 0,
    }
}

/// Ein geparkter Block: Adresse als Zahl (nie dereferenziert) + sein Schluessel.
#[derive(Clone, Copy)]
struct Eintrag {
    zeiger: *mut u8,
    nutz: usize,
    ausr: usize,
}

/// Modul-Vorrat: wieder­verwendbare Bloecke (Free-Listen-Ersatz ohne `unsafe`, s. Grenze 1–2).
/// Festes Feld — park­bar ist, was passt; der Rest wird gezaehlt, nicht verschwiegen.
struct Vorrat {
    plaetze: [Option<Eintrag>; VORRAT_PLAETZE],
    wiederverwendet: u64,
    verworfen: u64,
}

impl Vorrat {
    const fn leer() -> Self {
        Self {
            plaetze: [None; VORRAT_PLAETZE],
            wiederverwendet: 0,
            verworfen: 0,
        }
    }

    fn belegt(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < self.plaetze.len() {
            if self.plaetze[i].is_some() {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Block fuer `bedarf` entnehmen: erst exakte Nutz­groesse (deckende Ausrichtung), dann den
    /// kleinsten ausreichenden (deckende Ausrichtung). Deterministisch (Feld­reihenfolge).
    fn entnehmen(&mut self, bedarf: Bedarf) -> Option<*mut u8> {
        // Pass 1: exakt.
        let mut i = 0;
        while i < self.plaetze.len() {
            if let Some(e) = self.plaetze[i] {
                if e.nutz == bedarf.nutz && e.ausr >= bedarf.ausr {
                    self.plaetze[i] = None;
                    return Some(e.zeiger);
                }
            }
            i += 1;
        }
        // Pass 2: kleinster ausreichender (gegen Gross-Block-Verschwendung bei Klein­anfragen).
        let mut best: Option<usize> = None;
        let mut best_nutz = usize::MAX;
        let mut j = 0;
        while j < self.plaetze.len() {
            if let Some(e) = self.plaetze[j] {
                if e.nutz >= bedarf.nutz && e.ausr >= bedarf.ausr && e.nutz < best_nutz {
                    best = Some(j);
                    best_nutz = e.nutz;
                }
            }
            j += 1;
        }
        match best {
            Some(k) => {
                let e = self.plaetze[k].expect("Vorrat: Bestplatz soeben geprueft");
                self.plaetze[k] = None;
                Some(e.zeiger)
            }
            None => None,
        }
    }

    /// Block parken; `false`, wenn der Vorrat voll ist (Aufrufer zaehlt `verworfen`).
    fn parken(&mut self, eintrag: Eintrag) -> bool {
        let mut i = 0;
        while i < self.plaetze.len() {
            if self.plaetze[i].is_none() {
                self.plaetze[i] = Some(eintrag);
                return true;
            }
            i += 1;
        }
        false
    }
}

/// `kmalloc`-Vergabe ueber [`Heap`]: Klassen-Slabs (≤ 2048) + dedizierte Gross-Regionen,
/// mit ATOMIC-Vorrat und Rueckstau-Zaehler. `&self`-Methoden (Innen­veraenderlichkeit ueber
/// `SpinLock`, wie der Heap selbst) — eine Instanz darf global geteilt werden.
pub struct Kmalloc<S: RegionSource> {
    heap: Heap<S>,
    vorrat: SpinLock<Vorrat>,
}

impl<S: RegionSource> Kmalloc<S> {
    /// Leere Vergabe anlegen; Regionen kommen lazy bei der ersten KERNEL-Anfrage (Heap-Verhalten).
    pub const fn new(quelle: S) -> Self {
        Self {
            heap: Heap::new(quelle),
            vorrat: SpinLock::new(Vorrat::leer()),
        }
    }

    /// Der unterliegende Heap (Telemetrie: `allocated_bytes()`, `region_count()`).
    pub fn heap(&self) -> &Heap<S> {
        &self.heap
    }
    /// Derzeit geparkte Bloecke (Vorrat­fuellung — Vor­waerm­stand, s. Test `atomic_waechst_nicht`).
    pub fn vorrat_belegt(&self) -> usize {
        self.vorrat.lock().belegt()
    }
    /// Wie oft eine Vergabe aus dem Vorrat bedient wurde (Wieder­verwendungs­nachweis).
    pub fn wiederverwendet(&self) -> u64 {
        self.vorrat.lock().wiederverwendet
    }
    /// Wie viele Freigaben nicht parkbar waren (Vorrat voll oder unkenn­tlich) und bis zum
    /// Heap-Abbau gebunden bleiben (Grenze 2). In ruhigen Tests steht hier 0.
    pub fn verworfen(&self) -> u64 {
        self.vorrat.lock().verworfen
    }

    /// `kmalloc(len, gfp)`: `len` Bytes mit mindestens [`ARCH_KMALLOC_MINALIGN`]-Ausrichtung.
    /// `None` bei unbedienbarer Anfrage, leerem ATOMIC-Vorrat oder erschoepftem Heap.
    ///
    /// Roher Zeiger, weil der Shim hinter rohen Zeigern arbeitet (s. Modul­kopf): Der Zeiger ist
    /// gueltig und exklusiv, bis er mit passender Laenge an [`Kmalloc::kfree`] zurueck­geht;
    /// nutzbar sind mindestens [`ksize`] (`len`)-Bytes ab dem Zeiger.
    pub fn kmalloc(&self, len: usize, gfp: Gfp) -> Option<*mut u8> {
        self.kmalloc_ergebnis(len, gfp).ok()
    }

    /// Wie [`Kmalloc::kmalloc`], mit benannter Fehler­ursache statt blossem `None`.
    pub fn kmalloc_ergebnis(&self, len: usize, gfp: Gfp) -> Result<*mut u8, KmallocFehler> {
        let Some(bedarf) = bedarf_fuer(len, ARCH_KMALLOC_MINALIGN) else {
            return Err(KmallocFehler::ZuGross);
        };
        self.bedienen(bedarf, gfp).ok_or(if gfp.ist_atomar() {
            // Einziger ATOMIC-Fehlweg: Vorrat leer (Quelle unberuehrt, Grenze 1).
            KmallocFehler::AtomarWaechstNicht
        } else {
            // Einziger KERNEL-Fehlweg: Heap (und damit Quelle) erschoepft.
            KmallocFehler::ZuGross
        })
    }

    /// Vergabe mit geforderter Ausrichtung (Zweier­potenz, [`ARCH_KMALLOC_MINALIGN`]..=[`MAX_AUSR`]).
    /// Jede andere Ausrichtung wird abgewiesen (`None`) — „fast passend" gibt es hier nicht.
    pub fn kmalloc_ausgerichtet(
        &self,
        len: usize,
        ausr: usize,
        gfp: Gfp,
    ) -> Option<*mut u8> {
        let bedarf = bedarf_fuer(len, ausr)?;
        self.bedienen(bedarf, gfp)
    }

    /// Vergabe­kern: erst Vorrat (beide Kontexte), dann — nur KERNEL — Heap (darf wachsen).
    /// Der ATOMIC-Pfad kehrt bei leerem Vorrat zurueck und erreicht den Heap-Aufruf unten nie.
    fn bedienen(&self, bedarf: Bedarf, gfp: Gfp) -> Option<*mut u8> {
        // Der Treffer wird in `let` gebunden (nicht im `if let`-Kopf): Das Temporär im Kopf lebte
        // bis zum Block­ende — der Guard bliebe gehalten, und das zweite `lock()` unten wuerde
        // denselben Ticket-Lock reentrant ziehen (Selbst-Deadlock, gemessen 2026-09-09: zwei
        // hängende Host-Tests). `let` gibt den Guard am Strich­punkt frei.
        let treffer = self.vorrat.lock().entnehmen(bedarf);
        if let Some(zeiger) = treffer {
            let mut v = self.vorrat.lock();
            v.wiederverwendet = v.wiederverwendet.saturating_add(1);
            return Some(zeiger);
        }
        if gfp.ist_atomar() {
            return None;
        }
        let plan = Layout::from_size_align(bedarf.nutz, bedarf.ausr).ok()?;
        let block = <Heap<S> as Allocator>::allocate(&self.heap, plan).ok()?;
        // `as_ptr` ist sicher (kein Deref), der Fett-zu-duenn-Cast behaelt die Adresse.
        Some(block.as_ptr() as *mut u8)
    }

    /// `kfree(ptr, len)`: Block mit der Anfrage­laenge von damals zurueck­geben. `len` muss zur
    /// Vergabe passen (Schablonen­vertrag im A-Zustand — Linux liest die Groesse aus einem Kopf,
    /// den es hier noch nicht gibt; Fehl­passung ist speicher­sicher, aber verschwendet).
    /// Null­zeiger und Null­laenge sind No-Ops (Linux-nah).
    pub fn kfree(&self, zeiger: *mut u8, len: usize) {
        self.kfree_ausgerichtet(zeiger, len, ARCH_KMALLOC_MINALIGN);
    }

    /// Wie [`Kmalloc::kfree`], mit der Ausrichtung von damals (s. [`Kmalloc::kmalloc_ausgerichtet`]).
    pub fn kfree_ausgerichtet(&self, zeiger: *mut u8, len: usize, ausr: usize) {
        if zeiger.is_null() {
            return;
        }
        let Some(bedarf) = bedarf_fuer(len, ausr) else {
            // Unkenn­tlich (leer, zu gross, Ausrichtung): nicht parkbar — gezaehlt (Grenze 2).
            let mut v = self.vorrat.lock();
            v.verworfen = v.verworfen.saturating_add(1);
            return;
        };
        let mut v = self.vorrat.lock();
        if !v.parken(Eintrag {
            zeiger,
            nutz: bedarf.nutz,
            ausr: bedarf.ausr,
        }) {
            v.verworfen = v.verworfen.saturating_add(1);
        }
    }

    /// `krealloc`: Ummelden auf `neu_len` (gleicher Kontext `gfp`).
    ///
    /// - Null­zeiger → reine Vergabe (wie `kmalloc`).
    /// - `neu_len == 0` → Freigabe, Rueckgabe `None` (kein `ZERO_SIZE_PTR` im A-Zustand).
    /// - Gleiche Klasse → derselbe Zeiger (kein Umzug, wie Linux).
    /// - Sonst: neue Vergabe, alte Freigabe. **Die Nutzlast wird NICHT kopiert** (Grenze 3) —
    ///   das Kopieren obliegt dem Shim.
    /// - Misslingt die neue Vergabe, bleibt der alte Block gueltig (Linux-Semantik).
    pub fn krealloc(
        &self,
        alt: *mut u8,
        alt_len: usize,
        neu_len: usize,
        gfp: Gfp,
    ) -> Option<*mut u8> {
        if alt.is_null() {
            return self.kmalloc(neu_len, gfp);
        }
        if neu_len == 0 {
            self.kfree(alt, alt_len);
            return None;
        }
        let alt_bedarf = bedarf_fuer(alt_len, ARCH_KMALLOC_MINALIGN);
        let neu_bedarf = bedarf_fuer(neu_len, ARCH_KMALLOC_MINALIGN);
        match (alt_bedarf, neu_bedarf) {
            (Some(a), Some(n)) if a == n => Some(alt),
            (_, Some(_)) => {
                let neu = self.kmalloc(neu_len, gfp)?;
                self.kfree(alt, alt_len);
                Some(neu)
            }
            (_, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Host-Tests der Schablone gegen eine zaehlende Quelle ueber einem **falschen** Fenster:
    //! Die Adressen sind erfunden (`FENSTER_BASIS`), aber das ist hier kein Mangel — das Modul
    //! dereferenziert nie (reine Adress­arithmetik), und der Heap beruehrt Speicher nur auf
    //! Free-Listen-Pfaden, die der A-Zustand gar nicht betritt (kein `deallocate`-Aufruf).
    //! Geprueft werden: Schluessel, Rueck­wege, ATOMIC-Still­stand (Quell­zaehler),
    //! Absagen — alles ohne ein einziges `unsafe` (auch hier nicht).
    use super::*;
    use crate::{Purpose, Region, RegionTag};
    use caprock_mem::PhysAllocator;
    extern crate std;
    use std::sync::{Arc, Mutex};

    /// Erfundenes Fenster (niemals dereferenziert, s. Modul­kopf).
    const FENSTER_BASIS: u64 = 0x1_0000_0000;

    struct Innen {
        phys: Mutex<PhysAllocator>,
        anfragen: Mutex<u64>,
        freigaben: Mutex<u64>,
    }

    /// Quelle mit Anfrage­zaehler: `request`-Aufrufe sind Wachstum — der ATOMIC-Test friert
    /// diesen Zaehler ein und beobachtet ihn.
    #[derive(Clone)]
    struct ZaehlerQuelle {
        innen: Arc<Innen>,
    }

    impl ZaehlerQuelle {
        fn neu(fenster_lang: u64) -> Self {
            let mut phys = PhysAllocator::new();
            assert!(phys.add_region(FENSTER_BASIS, fenster_lang));
            Self {
                innen: Arc::new(Innen {
                    phys: Mutex::new(phys),
                    anfragen: Mutex::new(0),
                    freigaben: Mutex::new(0),
                }),
            }
        }
        fn anfragen(&self) -> u64 {
            *self.innen.anfragen.lock().unwrap()
        }
        fn freigaben(&self) -> u64 {
            *self.innen.freigaben.lock().unwrap()
        }
    }

    impl RegionSource for ZaehlerQuelle {
        fn request(&self, min_len: usize, zweck: Purpose) -> Option<Region> {
            *self.innen.anfragen.lock().unwrap() += 1;
            let cap = self
                .innen
                .phys
                .lock()
                .unwrap()
                .alloc(min_len as u64, 4096)?;
            Some(Region::from_cap(cap, RegionTag::new(0, zweck)))
        }
        fn release(&self, region: Region) {
            *self.innen.freigaben.lock().unwrap() += 1;
            // Echte Rueck­gabe an den `PhysAllocator` ist Test­zwecken fremd (der Heap ruft
            // `release` beim Abbau; gezaehlt ist belegt). Ablegen ohne Verlust­meldung waere
            // schlimmer als Vergessen mit Zaehler — s. `cap::MemoryCap`-Doku im Quell­crate.
            drop(region);
        }
    }

    #[test]
    fn ksize_klassen() {
        assert_eq!(ksize(0), 0);
        assert_eq!(ksize(1), 16);
        assert_eq!(ksize(16), 16);
        assert_eq!(ksize(17), 32);
        assert_eq!(ksize(33), 64);
        assert_eq!(ksize(100), 128);
        assert_eq!(ksize(128), 128);
        assert_eq!(ksize(129), 256);
        assert_eq!(ksize(1024), 1024);
        assert_eq!(ksize(1025), 2048);
        assert_eq!(ksize(2048), 2048);
        // Gross-Pfad: auf Mindest­ausrichtung gerundet, nicht auf Klasse.
        assert_eq!(ksize(2049), 2064);
        assert_eq!(ksize(4096), 4096);
        assert_eq!(ksize(MAX_KMALLOC), MAX_KMALLOC);
        // Jenseits der Schablonen-Grenze: keine Zusage (0), auch bei Ueberlauf­laengen.
        assert_eq!(ksize(MAX_KMALLOC + 1), 0);
        assert_eq!(ksize(usize::MAX), 0);
    }

    #[test]
    fn minalign_und_zweierpotenz_absagen() {
        let k = Kmalloc::new(ZaehlerQuelle::neu(4 << 20));
        // Absagen: keine Zweier­potenz, zu klein, zu gross, null.
        for ausr in [0, 1, 3, 6, 8, 12, 24, 48, 8192, 1 << 20] {
            assert!(
                k.kmalloc_ausgerichtet(64, ausr, Gfp::KERNEL).is_none(),
                "ausr {ausr} muss abgewiesen werden"
            );
        }
        // Zusagen: jede Vergabe traegt mindestens MINALIGN, geforderte Ausrichtung exakt.
        for ausr in [16, 32, 64, 4096] {
            let z = k
                .kmalloc_ausgerichtet(64, ausr, Gfp::KERNEL)
                .expect("bedienbare Ausrichtung");
            assert_eq!(
                (z as usize) % ausr,
                0,
                "Adresse muss {ausr}-ausgerichtet sein"
            );
            assert_eq!((z as usize) % ARCH_KMALLOC_MINALIGN, 0);
            k.kfree_ausgerichtet(z, 64, ausr);
        }
        assert_eq!(k.verworfen(), 0);
    }

    #[test]
    fn null_laenge_abgewiesen() {
        let quelle = ZaehlerQuelle::neu(1 << 20);
        let k = Kmalloc::new(quelle.clone());
        // Kein ZERO_SIZE_PTR im A-Zustand: Null ist kein Block, sondern eine Absage.
        assert!(k.kmalloc(0, Gfp::KERNEL).is_none());
        assert!(k.kmalloc(0, Gfp::ATOMIC).is_none());
        assert_eq!(
            k.kmalloc_ergebnis(0, Gfp::KERNEL),
            Err(KmallocFehler::ZuGross)
        );
        // Freigabe von Nichts ist ein No-Op (kein Zaehler­zuwachs, kein Verwerfen).
        k.kfree(core::ptr::null_mut(), 100);
        k.kfree(core::ptr::null_mut(), 0);
        assert_eq!(k.verworfen(), 0);
        assert_eq!(quelle.anfragen(), 0);
        // Realloc-Rand: null+0 bleibt null.
        assert!(k.krealloc(core::ptr::null_mut(), 0, 0, Gfp::KERNEL).is_none());
    }

    #[test]
    fn frei_und_realloc_rundweg() {
        let k = Kmalloc::new(ZaehlerQuelle::neu(4 << 20));
        // Vergabe → Freigabe → Wieder­vergabe reicht denselben Block (LIFO-Vorrat).
        let z1 = k.kmalloc(100, Gfp::KERNEL).expect("erste Vergabe");
        assert_eq!(ksize(100), 128);
        k.kfree(z1, 100);
        assert_eq!(k.vorrat_belegt(), 1);
        let z2 = k.kmalloc(100, Gfp::KERNEL).expect("Wieder­vergabe");
        assert_eq!(z1, z2);
        // Realloc in gleicher Klasse zieht nicht um.
        let z3 = k
            .krealloc(z2, 100, 120, Gfp::KERNEL)
            .expect("klassen­gleiches Realloc");
        assert_eq!(z3, z2);
        // Realloc mit Wachstum zieht um; der Alt­block landet wieder im Vorrat.
        let z4 = k
            .krealloc(z3, 120, 1000, Gfp::KERNEL)
            .expect("wachsendes Realloc");
        assert_ne!(z4, z3);
        assert_eq!(ksize(1000), 1024);
        let z5 = k.kmalloc(100, Gfp::KERNEL).expect("Alt­block-Recycling");
        assert_eq!(z5, z3);
        k.kfree(z5, 100);
        k.kfree(z4, 1000);
        assert_eq!(k.verworfen(), 0);
        assert!(k.wiederverwendet() >= 2);
    }

    #[test]
    fn atomic_waechst_nicht() {
        let quelle = ZaehlerQuelle::neu(1 << 20);
        let k = Kmalloc::new(quelle.clone());
        // Leerer Vorrat: ATOMIC scheitert SOFORT — und die Quelle bleibt unberuehrt.
        assert_eq!(quelle.anfragen(), 0);
        assert!(k.kmalloc(64, Gfp::ATOMIC).is_none());
        assert_eq!(
            k.kmalloc_ergebnis(64, Gfp::ATOMIC),
            Err(KmallocFehler::AtomarWaechstNicht)
        );
        assert_eq!(quelle.anfragen(), 0);
        // KERNEL darf wachsen und waermt damit den Vorrat vor.
        let a = k.kmalloc(64, Gfp::KERNEL).expect("Vor­waermen a");
        let b = k.kmalloc(64, Gfp::KERNEL).expect("Vor­waermen b");
        assert!(quelle.anfragen() > 0);
        k.kfree(a, 64);
        k.kfree(b, 64);
        // Ab hier ist der Quell­zaehler eingefroren: jede ATOMIC-Vergabe kommt aus dem Vorrat.
        let stand = quelle.anfragen();
        let x = k.kmalloc(64, Gfp::ATOMIC).expect("Vorrat 1");
        let y = k.kmalloc(64, Gfp::ATOMIC).expect("Vorrat 2");
        assert_eq!(quelle.anfragen(), stand);
        // Vorrat leer → Absage, aber WEITERHIN kein Wachstum (das ist die Zusicherung).
        assert!(k.kmalloc(64, Gfp::ATOMIC).is_none());
        assert_eq!(
            k.kmalloc_ergebnis(64, Gfp::ATOMIC),
            Err(KmallocFehler::AtomarWaechstNicht)
        );
        assert_eq!(quelle.anfragen(), stand);
        assert!(k.wiederverwendet() >= 2);
        k.kfree(x, 64);
        k.kfree(y, 64);
        assert_eq!(k.verworfen(), 0);
    }

    #[test]
    fn zu_gross_beruehrt_quelle_nicht() {
        let quelle = ZaehlerQuelle::neu(1 << 20);
        let k = Kmalloc::new(quelle.clone());
        let stand = quelle.anfragen();
        for gfp in [Gfp::KERNEL, Gfp::ATOMIC] {
            assert!(k.kmalloc(MAX_KMALLOC + 1, gfp).is_none());
            assert!(k.kmalloc(usize::MAX, gfp).is_none());
            assert_eq!(
                k.kmalloc_ergebnis(usize::MAX, gfp),
                Err(KmallocFehler::ZuGross)
            );
        }
        // Validierung VOR jedem Heap-/Quell­kontakt: der Zaehler steht still.
        assert_eq!(quelle.anfragen(), stand);
        // Echte Er­schoepfung (winziges Fenster): KERNEL meldet ZuGross statt zu haengen.
        let eng = ZaehlerQuelle::neu(70_000);
        let ke = Kmalloc::new(eng.clone());
        assert!(ke.kmalloc(32, Gfp::KERNEL).is_some());
        assert!(ke.kmalloc(50_000, Gfp::KERNEL).is_none());
        assert_eq!(
            ke.kmalloc_ergebnis(50_000, Gfp::KERNEL),
            Err(KmallocFehler::ZuGross)
        );
    }

    #[test]
    fn atomar_bit_gewinnt_gegen_kernel() {
        let quelle = ZaehlerQuelle::neu(1 << 20);
        let k = Kmalloc::new(quelle.clone());
        // Gemischte Bits → sichere Seite: kein Wachstum, auch mit KERNEL-Bit.
        let beide = Gfp::KERNEL.vereint(Gfp::ATOMIC);
        assert!(beide.ist_atomar());
        assert!(beide.enthaelt(Gfp::KERNEL));
        let stand = quelle.anfragen();
        assert!(k.kmalloc(32, beide).is_none());
        assert_eq!(
            k.kmalloc_ergebnis(32, beide),
            Err(KmallocFehler::AtomarWaechstNicht)
        );
        assert_eq!(quelle.anfragen(), stand);
        // Algebra-Kontrolle der Kontexte.
        assert!(!Gfp::KERNEL.ist_atomar());
        assert!(Gfp::ATOMIC.ist_atomar());
        assert!(Gfp::KERNEL.enthaelt(Gfp::KERNEL));
        assert_eq!(Gfp::KERNEL.bits(), 0b01);
    }

    #[test]
    fn abbau_gibt_regionen_zurueck() {
        let quelle = ZaehlerQuelle::neu(1 << 20);
        {
            let k = Kmalloc::new(quelle.clone());
            let _ = k.kmalloc(32, Gfp::KERNEL).expect("Wachstum");
            assert!(quelle.anfragen() > 0);
            // Geparkte Bloecke sind blosse Adressen — der Heap gibt beim Abbau alles zurueck.
        }
        assert!(quelle.freigaben() > 0);
    }
}
