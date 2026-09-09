//! **Speicher-Server in Userspace (Z14 Stufe 1).**
//!
//! Eine PD hält eine große Memory-Cap (hier: eine Arena über einem [`DmaPool`]-Fenster) und gibt
//! auf Anfrage abgeleitete Bereiche per IPC-REPLY heraus; der Client mappt sie mit `SYS_MAP`.
//! Das ist `brk`/`mmap` — **ohne eine einzige neue Kernelzeile**, weil Cap-Ableitung (`CCOPY`)
//! und Cap-Transfer beide schon stehen. Der Kern bleibt unberührt, die Politik („wer bekommt wie
//! viel") wandert dorthin, wo sie hingehört: in eine PD.
//!
//! ## Was hier läuft und was gestellt ist
//!
//! Auf dem Host läuft die **Politik**: Vergabe, Überlappungsfreiheit, Rückgabe, benannte Absagen.
//! Gestellt ist der Transport: statt echter Caps trägt ein [`GrantSchein`] (Handle + Offset +
//! Länge), statt gemappter Frames liegt im Test ein Schatten-`Vec` — der Server rechnet Offsets,
//! der Test schreibt Bytes. Was damit belegt ist: Die Abnahme aus Z14 — eine PD, die beim Laden
//! 4 KiB bekommt, fordert 1 MiB an, schreibt sie voll und gibt sie zurück, und eine zweite PD
//! bekommt dieselbe Region **nicht** zu sehen.
//!
//! ## Verwendete Schemen (Pfadabhängigkeiten, alle `no_std` + `forbid(unsafe_code)`)
//!
//! - `caprock-dma`: [`DmaPool`] als Adressmodell der Arena (CPU-Sicht gegen Gerätesicht,
//!   `map` leitet die Geräteadresse ab — ein Stapelpuffer bekäme hier keine, genau wie dort).
//! - `caprock-wait`: [`Completion`] für die Bereitschaftsmeldung („Grant liegt bereit"),
//!   [`Mutex`] für den Ausschluss zwischen Server-Threads.
//! - `caprock-region` (`page` only): [`PAGE_SIZE`], [`MemMap`] für die Seitenbuchhaltung der
//!   Arena (`heap.rs` ist der Prozess-Allokator und wird hier bewusst NICHT benutzt — der Server
//!   verwaltet **fremden** Speicher für andere PDs, nicht seinen eigenen: die Vergabe ist eine
//!   Freiliste über Offsets, der Heap-Gedanke ohne den Heap-Typ, weil `Heap` eine
//!   `RegionSource` mit echten `Region`s und eine Allocator-Nightly braucht — PD-Laufzeit, keine
//!   Host-Arithmetik. Die Disziplin ist dieselbe: Bump für Frisches, Freiliste für
//!   Zurückgegebenes, benannte Erschöpfung statt stiller Wiederverwendung).
//!
//! ## Die Zusicherung, auf die es ankommt
//!
//! Lebende Grants überlappen nie, und ein fremder Schein löst nie auf: [`SpeicherFehler::FremderSchein`]
//! ist eine **eigene** Absage, kein `UnbekannterSchein`. Wer beides zusammenwürfe, nähme dem
//! Aufrufer genau die Unterscheidung, die „zweite PD sieht nichts" prüft.

#![no_std]
#![forbid(unsafe_code)]

// The crate is `no_std` (it runs in a server PD without an OS). The test
// harness needs `std` for the shadow bytes — test-only, the PD build never
// sees it (same pattern as `caprock-dma` / `caprock-wait` / `lx-shim-demo`).
#[cfg(test)]
extern crate std;

use caprock_dma::DmaPool;
use caprock_region::page::{MemMap, PAGE_SIZE};
use caprock_wait::{Completion, LockError, Mutex, Park, Tid};

/// Die Arena des Servers: 2 MiB — Startkapital (4 KiB) + 1-MiB-Grant + Luft für die zweite PD.
pub const ARENA_BYTES: u64 = 2 * 1024 * 1024;
/// Was eine PD beim Laden hält (Z14-Abnahme: „beim Laden 4 KiB").
pub const START_KAPITAL: u64 = 4096;
/// Was die Abnahme zur Laufzeit anfordert.
pub const ABNAHME_GRANT: u64 = 1024 * 1024;
/// Die PD, die beim Aufsetzen das Startkapital bekommt.
pub const PD_BOOT: u32 = 1;

/// CPU-Sicht der Arena (muss ungleich der Gerätesicht sein — `DmaPool` weist Identität ab).
pub const ARENA_CPU_BASIS: u64 = 0x4000_0000;
/// Gerätesicht der Arena.
pub const ARENA_DEV_BASIS: u64 = 0x1000_0000;

/// Wie viele lebende Grants der Server höchstens führt. Eine Schranke mit Namen statt einem
/// Loch: darüber gibt es [`SpeicherFehler::TabelleVoll`] (D11 — *wer eine Kapazität einführt,
/// muss den Überlauf benennen*).
pub const MAX_GRANTS: usize = 16;

/// Warum eine Anfrage nicht bedient wurde. Jeder Ausgang hat einen eigenen Namen — „geht nicht"
/// allein sagte nicht, ob der Aufrufer kleiner fragen, später fragen oder gar nicht fragen soll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpeicherFehler {
    /// Länge 0 angefordert — kein Grant, keine Absicht erkennbar.
    LeereAnfrage,
    /// Die Arena gibt das nicht mehr her (zu groß oder zerstückelt). Der Aufrufer wird
    /// **nicht** blockiert — dieselbe Form wie `ERR_EP_FULL`: eine Lastaussage, kein Verbot.
    KeinPlatz,
    /// Mehr lebende Grants als [`MAX_GRANTS`]. Der Aufrufer gibt erst etwas zurück, dann fragt
    /// er wieder — wiederholbar, sobald ein Schein zurückgegeben wurde.
    TabelleVoll,
    /// Diesen Schein hat der Server nie ausgestellt.
    UnbekannterSchein,
    /// Diesen Schein hat eine **andere** PD — er lebt, aber nicht für den Aufrufer. Das ist die
    /// Isolationsabsage: die zweite PD sieht die Region der ersten nicht, und der Versuch wird
    /// benannt statt still bedient oder still fallengelassen.
    FremderSchein,
    /// Dieser Schein ist bereits zurückgegeben — doppelte Freigabe, kein doppelter Bereich.
    BereitsZurueck,
    /// Der Server-Ausschluss selbst schlug fehl.
    Sperre(LockError),
}

/// Ein ausgestellter Grant: Handle (capability-ähnlich, opaker `u64`) plus Lage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GrantSchein {
    /// Opakes Handle: `(Generation << 32) | Slot`. Nie 0.
    pub handle: u64,
    /// Offset in der Arena (Index in die Schatten-Bytes des Tests, Basis des `SYS_MAP`).
    pub offset: u64,
    /// Länge in Byte (seitausgerichtet, ≥ 1 Seite).
    pub len: u64,
}

/// Die Auflösung eines Scheins: Lage plus beide Sichten (CPU für den Client, Gerät fürs IOMMU).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GrantSicht {
    pub offset: u64,
    pub len: u64,
    pub cpu: u64,
    pub dev: u64,
}

#[derive(Clone, Copy)]
struct Grant {
    handle: u64,
    pd: u32,
    offset: u64,
    len: u64,
    cpu: u64,
    dev: u64,
    live: bool,
}

/// Deterministisches Füllmuster: Byte `i` des Grants mit `handle`.
/// Der Test schreibt damit voll und liest zurück — was nicht dem Muster folgt, wurde nicht von
/// dieser PD geschrieben (die Positivkontrolle der Z14-Abnahme auf Byte-Ebene).
pub fn muster(handle: u64, index: u64) -> u8 {
    ((handle.wrapping_add(index).wrapping_mul(31).wrapping_add(7)) & 0xFF) as u8
}

/// Überlappen zwei Bereiche? Reine Arithmetik, überlaufsicher — die Stelle, an der „zweite PD
/// sieht nichts" entschieden wird, bevor irgendein Schein ausgestellt ist.
pub fn ueberlappt(a_off: u64, a_len: u64, b_off: u64, b_len: u64) -> bool {
    let Some(a_ende) = a_off.checked_add(a_len) else { return true };
    let Some(b_ende) = b_off.checked_add(b_len) else { return true };
    a_off < b_ende && b_off < a_ende
}

fn runde_auf_seite(len: u64) -> Option<u64> {
    let r = len % PAGE_SIZE;
    if r == 0 { Some(len) } else { len.checked_add(PAGE_SIZE - r) }
}

/// Der Speicher-Server: eine Arena, eine Freiliste, eine Grant-Tabelle.
pub struct MemoryServer {
    pool: DmaPool,
    fenster: MemMap,
    cpu_basis: u64,
    grants: [Option<Grant>; MAX_GRANTS],
    freie: [(u64, u64); MAX_GRANTS],
    freie_n: usize,
    naechste_gen: u32,
    bereit: Completion,
    sperre: Mutex,
}

const KEIN_GRANT: Option<Grant> = None;

impl MemoryServer {
    /// Aufsetzen über einem Fenster. Stellt sofort das Startkapital für [`PD_BOOT`] aus
    /// (4 KiB bei Offset 0) — der Zustand „beim Laden 4 KiB" ist damit kein Kommentar, sondern
    /// der erste Grant der Tabelle.
    pub fn neu(cpu_basis: u64, dev_basis: u64, arena_len: u64) -> Option<Self> {
        if arena_len < START_KAPITAL || arena_len % PAGE_SIZE != 0 {
            return None;
        }
        let pool = DmaPool::new(cpu_basis, dev_basis, arena_len).ok()?;
        let seiten = arena_len / PAGE_SIZE;
        // Startkapital: direkt aus der Arena geschnitten (vor dem Einzug in die Tabelle).
        let stamm = pool.map(cpu_basis, START_KAPITAL)?;
        let mut s = MemoryServer {
            pool,
            fenster: MemMap::new(cpu_basis, seiten),
            cpu_basis,
            grants: [KEIN_GRANT; MAX_GRANTS],
            freie: [(0, 0); MAX_GRANTS],
            freie_n: 0,
            naechste_gen: 1,
            bereit: Completion::new(),
            sperre: Mutex::new(),
        };
        // Startkapital: Bump-frei, direkt aus der Arena geschnitten.
        let buf = stamm;
        s.grants[0] = Some(Grant {
            handle: s.frisch_handle(0),
            pd: PD_BOOT,
            offset: 0,
            len: START_KAPITAL,
            cpu: buf.cpu(),
            dev: buf.dev(),
            live: true,
        });
        s.freie[0] = (START_KAPITAL, arena_len - START_KAPITAL);
        s.freie_n = 1;
        Some(s)
    }

    fn frisch_handle(&mut self, slot: usize) -> u64 {
        let g = self.naechste_gen;
        self.naechste_gen = self.naechste_gen.wrapping_add(1);
        if self.naechste_gen == 0 {
            self.naechste_gen = 1; // Handle 0 gibt es nicht
        }
        ((g as u64) << 32) | (slot as u64)
    }

    fn slot_zu_handle(&self, handle: u64) -> Option<usize> {
        let slot = (handle & 0xFFFF_FFFF) as usize;
        if slot >= MAX_GRANTS {
            return None;
        }
        match self.grants[slot] {
            Some(g) if g.handle == handle => Some(slot),
            _ => None,
        }
    }

    fn freien_bereich_nehmen(&mut self, bedarf: u64) -> Option<u64> {
        // First-fit über die Freiliste (der Heap-Gedanke: Zurückgegebenes wird wiederverwendet,
        // nicht vergessen — ein Bump-Zeiger allein könnte das bei zwei PDs im Wechsel nicht).
        let mut i = 0;
        while i < self.freie_n {
            let (off, len) = self.freie[i];
            if len >= bedarf {
                if len == bedarf {
                    self.freie[i] = self.freie[self.freie_n - 1];
                    self.freie_n -= 1;
                } else {
                    self.freie[i] = (off + bedarf, len - bedarf);
                }
                return Some(off);
            }
            i += 1;
        }
        None
    }

    fn freien_bereich_ablegen(&mut self, off: u64, len: u64) {
        if self.freie_n >= MAX_GRANTS {
            return; // kann nicht passieren: höchstens so viele freie Bereiche wie Grants
        }
        self.freie[self.freie_n] = (off, len);
        self.freie_n += 1;
        // Sortieren + Nachbarn verschmelzen (n ≤ 16, Einfügesortierung reicht).
        let mut i = 1;
        while i < self.freie_n {
            let mut j = i;
            while j > 0 && self.freie[j - 1].0 > self.freie[j].0 {
                let t = self.freie[j - 1];
                self.freie[j - 1] = self.freie[j];
                self.freie[j] = t;
                j -= 1;
            }
            i += 1;
        }
        let mut w = 0;
        let mut i = 0;
        while i < self.freie_n {
            let (o, l) = self.freie[i];
            if w > 0 && self.freie[w - 1].0 + self.freie[w - 1].1 == o {
                self.freie[w - 1].1 += l;
            } else {
                self.freie[w] = (o, l);
                w += 1;
            }
            i += 1;
        }
        self.freie_n = w;
    }

    /// Anfordern: `len` Byte für `pd`. Gibt den Schein — der Client löst ihn per
    /// [`Self::aufloesen`] auf und mappt per `SYS_MAP`. Benannte Absagen statt Blockade.
    pub fn anfordern(
        &mut self,
        p: &dyn Park,
        pd: u32,
        len: u64,
    ) -> Result<GrantSchein, SpeicherFehler> {
        self.sperre.lock(p).map_err(SpeicherFehler::Sperre)?;
        let ergebnis = self.anfordern_inner(pd, len);
        self.sperre.unlock(p);
        ergebnis
    }

    fn anfordern_inner(&mut self, pd: u32, len: u64) -> Result<GrantSchein, SpeicherFehler> {
        if len == 0 {
            return Err(SpeicherFehler::LeereAnfrage);
        }
        let bedarf = runde_auf_seite(len).ok_or(SpeicherFehler::KeinPlatz)?;
        // Erst der Tabellenplatz (D11 vor Last: eine volle Tabelle ist keine volle Arena, und
        // die Antwort unterscheidet sich). Ein zurückgegebenes Fach (`live == false`) ist wieder
        // ein freies Fach — sonst wäre die Tabelle nach 16 Grants für immer voll, egal wie oft
        // zurückgegeben wird, und `TabelleVoll` wäre keine Lastaussage, sondern ein Leck.
        let mut slot = None;
        let mut i = 0;
        while i < MAX_GRANTS {
            if self.grants[i].map(|g| g.live) != Some(true) {
                slot = Some(i);
                break;
            }
            i += 1;
        }
        let slot = slot.ok_or(SpeicherFehler::TabelleVoll)?;
        let off = self.freien_bereich_nehmen(bedarf).ok_or(SpeicherFehler::KeinPlatz)?;
        // Lebende Grants überlappen nie — geprüft, nicht geglaubt (die Stelle, an der die
        // Z14-Abnahme „zweite PD sieht nichts" entscheidet).
        let mut k = 0;
        while k < MAX_GRANTS {
            if let Some(g) = self.grants[k] {
                if g.live && ueberlappt(off, bedarf, g.offset, g.len) {
                    return Err(SpeicherFehler::KeinPlatz);
                }
            }
            k += 1;
        }
        let cpu = self.cpu_basis.checked_add(off).ok_or(SpeicherFehler::KeinPlatz)?;
        let buf = self.pool.map(cpu, bedarf).ok_or(SpeicherFehler::KeinPlatz)?;
        // `MemMap`-Buchhaltung: der Bereich liegt im beschriebenen Fenster (Seiten-Nachweis wie
        // im lx-shim-demo-Ring — eine Adresse ausserhalb bekäme hier kein `Some`).
        let _ = self.fenster.addr_zu_pfn(cpu).ok_or(SpeicherFehler::KeinPlatz)?;
        let handle = self.frisch_handle(slot);
        self.grants[slot] = Some(Grant {
            handle,
            pd,
            offset: off,
            len: bedarf,
            cpu: buf.cpu(),
            dev: buf.dev(),
            live: true,
        });
        Ok(GrantSchein { handle, offset: off, len: bedarf })
    }

    /// Zurückgeben: der Schein erlischt, der Bereich wandert in die Freiliste. Doppelte Freigabe
    /// und fremde Scheine werden benannt abgewiesen — ein doppelter Bereich wäre zwei PDs mit
    /// demselben Speicher, also genau der Bruch der Isolation.
    pub fn zurueckgeben(
        &mut self,
        p: &dyn Park,
        pd: u32,
        handle: u64,
    ) -> Result<(), SpeicherFehler> {
        self.sperre.lock(p).map_err(SpeicherFehler::Sperre)?;
        let ergebnis = self.zurueckgeben_inner(pd, handle);
        self.sperre.unlock(p);
        ergebnis
    }

    fn zurueckgeben_inner(&mut self, pd: u32, handle: u64) -> Result<(), SpeicherFehler> {
        let slot = self.slot_zu_handle(handle).ok_or(SpeicherFehler::UnbekannterSchein)?;
        let g = self.grants[slot].expect("Slot eben belegt geprüft");
        if !g.live {
            return Err(SpeicherFehler::BereitsZurueck);
        }
        if g.pd != pd {
            return Err(SpeicherFehler::FremderSchein);
        }
        self.grants[slot] = Some(Grant { live: false, ..g });
        self.freien_bereich_ablegen(g.offset, g.len);
        Ok(())
    }

    /// Auflösen: Lage + beide Sichten zu einem lebenden eigenen Schein. Der Lesepfad vor
    /// `SYS_MAP` — und die Stelle, an der ein fremder Schein mit [`SpeicherFehler::FremderSchein`]
    /// abgewiesen wird statt aufzulösen: die zweite PD sieht die Region der ersten nicht.
    pub fn aufloesen(
        &mut self,
        p: &dyn Park,
        pd: u32,
        handle: u64,
    ) -> Result<GrantSicht, SpeicherFehler> {
        self.sperre.lock(p).map_err(SpeicherFehler::Sperre)?;
        let ergebnis = self.aufloesen_inner(pd, handle);
        self.sperre.unlock(p);
        ergebnis
    }

    fn aufloesen_inner(&mut self, pd: u32, handle: u64) -> Result<GrantSicht, SpeicherFehler> {
        let slot = self.slot_zu_handle(handle).ok_or(SpeicherFehler::UnbekannterSchein)?;
        let g = self.grants[slot].expect("Slot eben belegt geprüft");
        if !g.live {
            return Err(SpeicherFehler::BereitsZurueck);
        }
        if g.pd != pd {
            return Err(SpeicherFehler::FremderSchein);
        }
        Ok(GrantSicht { offset: g.offset, len: g.len, cpu: g.cpu, dev: g.dev })
    }

    /// Bereitschaft melden: ein ausgestellter Grant liegt zur Abholung bereit (IRQ-`complete`).
    /// Gibt zurück, wie viele Wartende geweckt wurden.
    pub fn grant_bereit_melden(&mut self, p: &dyn Park) -> usize {
        self.bereit.complete(p)
    }

    /// Ein Warteschritt auf die Bereitschaft (`wait_for_completion`). `Ok(true)` = bereit (und
    /// verbraucht), `Ok(false)` = geparkt, erneut rufen.
    pub fn auf_bereit_warten(&mut self, p: &dyn Park) -> Result<bool, LockError> {
        self.bereit.warten_schritt(p)
    }

    /// Offene (nicht abgeholte) Bereitschaftsmeldungen.
    pub fn bereit_offen(&self) -> u32 {
        self.bereit.offen()
    }

    /// Lebende Grants.
    pub fn grants_live(&self) -> usize {
        self.grants.iter().filter(|g| matches!(g, Some(x) if x.live)).count()
    }

    /// Vergebene Bytes (lebende Grants, seitausgerichtet).
    pub fn vergeben_bytes(&self) -> u64 {
        self.grants.iter().filter_map(|g| g.as_ref()).filter(|g| g.live).map(|g| g.len).sum()
    }

    /// Freie Bytes der Arena.
    pub fn freie_bytes(&self) -> u64 {
        let mut n = 0;
        let mut i = 0;
        while i < self.freie_n {
            n += self.freie[i].1;
            i += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;
    use std::vec::Vec;

    const PD_A: u32 = 1; // Boot-PD mit Startkapital
    const PD_B: u32 = 2; // die zweite PD der Abnahme
    const TEST_TID: Tid = 1;
    const B_TID: Tid = 2;

    /// Stand-in für den Kernel: Weckmarke mit Bestand (die Eigenschaft, an der alles hängt).
    struct FakePark {
        ich: Tid,
        marken: RefCell<[u32; 8]>,
        blockiert: RefCell<u32>,
        weckrufe: RefCell<u32>,
    }

    impl FakePark {
        fn neu(ich: Tid) -> Self {
            FakePark {
                ich,
                marken: RefCell::new([0; 8]),
                blockiert: RefCell::new(0),
                weckrufe: RefCell::new(0),
            }
        }
        fn blockierte(&self) -> u32 {
            *self.blockiert.borrow()
        }
    }

    impl Park for FakePark {
        fn me(&self) -> Tid {
            self.ich
        }
        fn park(&self) {
            let mut m = self.marken.borrow_mut();
            if m[self.ich as usize] > 0 {
                m[self.ich as usize] -= 1;
            } else {
                *self.blockiert.borrow_mut() += 1;
            }
        }
        fn unpark(&self, t: Tid) {
            self.marken.borrow_mut()[t as usize] += 1;
            *self.weckrufe.borrow_mut() += 1;
        }
    }

    fn server() -> MemoryServer {
        MemoryServer::neu(ARENA_CPU_BASIS, ARENA_DEV_BASIS, ARENA_BYTES).expect("Arena")
    }

    fn schatten() -> Vec<u8> {
        std::vec![0u8; ARENA_BYTES as usize]
    }

    fn vollschreiben(schatten: &mut [u8], schein: &GrantSchein) {
        let o = schein.offset as usize;
        let l = schein.len as usize;
        assert!(o + l <= schatten.len(), "Grant liegt in den Schatten-Bytes");
        let mut i = 0u64;
        while i < schein.len {
            schatten[o + i as usize] = muster(schein.handle, i);
            i += 1;
        }
    }

    fn voll_pruefen(schatten: &[u8], schein: &GrantSchein) -> bool {
        let o = schein.offset as usize;
        let mut i = 0u64;
        while i < schein.len {
            if schatten[o + i as usize] != muster(schein.handle, i) {
                return false;
            }
            i += 1;
        }
        true
    }

    #[test]
    fn startkapital_4kib() {
        // Z14-Abnahme, Anfangszustand: die Boot-PD hält beim Laden 4 KiB — als Grant der
        // Tabelle, nicht als Kommentar. Offset 0, beide Sichten aus dem Pool abgeleitet.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        // Handle des Startkapitals: Slot 0, erste Generation.
        let sicht = s.aufloesen(&park, PD_A, (1u64 << 32) | 0).expect("Startkapital loest auf");
        assert_eq!(sicht.offset, 0);
        assert_eq!(sicht.len, START_KAPITAL);
        assert_eq!(sicht.cpu, ARENA_CPU_BASIS);
        assert_eq!(sicht.dev, ARENA_DEV_BASIS);
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
        assert_eq!(s.grants_live(), 1);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
    }

    #[test]
    fn mib_anfordern_vollschreiben_zurueck() {
        // Z14-Abnahme, Positivpfad: 1 MiB anfordern, VOLLschreiben, zurückgeben — danach steht
        // die Arena wie vorher (Vergabe + Freiliste bilanzieren exakt).
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut schatten = schatten();
        let schein = s.anfordern(&park, PD_A, ABNAHME_GRANT).expect("1 MiB Grant");
        assert_eq!(schein.len, ABNAHME_GRANT);
        assert_eq!(schein.offset % PAGE_SIZE, 0);
        let sicht = s.aufloesen(&park, PD_A, schein.handle).expect("eigener Schein");
        assert_eq!((sicht.offset, sicht.len), (schein.offset, schein.len));
        assert_eq!(sicht.cpu, ARENA_CPU_BASIS + schein.offset);
        assert_eq!(sicht.dev, ARENA_DEV_BASIS + schein.offset);
        vollschreiben(&mut schatten, &schein);
        assert!(voll_pruefen(&schatten, &schein), "jedes Byte geschrieben und lesbar");
        s.zurueckgeben(&park, PD_A, schein.handle).expect("Rueckgabe");
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
        assert_eq!(s.aufloesen(&park, PD_A, schein.handle).err(), Some(SpeicherFehler::BereitsZurueck));
    }

    #[test]
    fn zweite_pd_sieht_nichts() {
        // Z14-Abnahme, Isolation: PD_A hält den beschriebenen 1-MiB-Grant; PD_B bekommt eine
        // disjunkte Region, sieht Nullen statt Muster, und PD_As Schein löst für PD_B NICHT auf.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut schatten = schatten();
        let a = s.anfordern(&park, PD_A, ABNAHME_GRANT).expect("A: 1 MiB");
        vollschreiben(&mut schatten, &a);
        let b = s.anfordern(&park, PD_B, PAGE_SIZE).expect("B: eigene Seite");
        assert!(!ueberlappt(a.offset, a.len, b.offset, b.len), "disjunkte Bereiche");
        // B liest seinen FRISCHEN Bereich: Nullen, kein Muster von A.
        let bo = b.offset as usize;
        assert!(schatten[bo..bo + b.len as usize].iter().all(|&x| x == 0));
        // Und As Schein ist für B ein fremder Schein — benannt, nicht aufgelöst.
        assert_eq!(
            s.aufloesen(&park, PD_B, a.handle).err(),
            Some(SpeicherFehler::FremderSchein)
        );
        assert_eq!(
            s.zurueckgeben(&park, PD_B, a.handle).err(),
            Some(SpeicherFehler::FremderSchein)
        );
        s.zurueckgeben(&park, PD_A, a.handle).expect("A gibt zurueck");
        s.zurueckgeben(&park, PD_B, b.handle).expect("B gibt zurueck");
    }

    #[test]
    fn fremder_schein_ist_eigene_absage() {
        // **Sprechprobe der Unterscheidung selbst**: „lebt, aber nicht für dich" und „gibt es
        // nicht" müssen verschiedene Codes sein — sonst wäre die Isolation von Raterei nicht zu
        // unterscheiden.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let a = s.anfordern(&park, PD_A, PAGE_SIZE).expect("A: Seite");
        assert_eq!(
            s.aufloesen(&park, PD_B, a.handle).err(),
            Some(SpeicherFehler::FremderSchein)
        );
        assert_eq!(
            s.aufloesen(&park, PD_B, 0xDEAD_BEEF).err(),
            Some(SpeicherFehler::UnbekannterSchein)
        );
        assert_ne!(
            SpeicherFehler::FremderSchein,
            SpeicherFehler::UnbekannterSchein,
            "fremd und unbekannt muessen unterscheidbar sein"
        );
        s.zurueckgeben(&park, PD_A, a.handle).expect("A gibt zurueck");
    }

    #[test]
    fn doppelte_rueckgabe_benannt() {
        // Zweimal zurückgeben gibt den Bereich nicht zweimal frei — sonst bekämen zwei PDs
        // denselben Speicher (der Bruch der Isolation durch die Hintertür).
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let a = s.anfordern(&park, PD_A, PAGE_SIZE).expect("A: Seite");
        let vorher = s.vergeben_bytes();
        s.zurueckgeben(&park, PD_A, a.handle).expect("erste Rueckgabe");
        assert_eq!(
            s.zurueckgeben(&park, PD_A, a.handle).err(),
            Some(SpeicherFehler::BereitsZurueck)
        );
        assert_eq!(s.vergeben_bytes(), vorher - PAGE_SIZE);
        assert_eq!(s.vergeben_bytes() + s.freie_bytes(), ARENA_BYTES);
    }

    #[test]
    fn leere_und_riesige_anfrage() {
        // 0 ist keine Anfrage, 8 MiB passen in keine 2-MiB-Arena — beide benannt, beide ohne
        // Blockade, und der Server bedient danach weiter.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        assert_eq!(
            s.anfordern(&park, PD_A, 0).err(),
            Some(SpeicherFehler::LeereAnfrage)
        );
        assert_eq!(
            s.anfordern(&park, PD_A, 8 * 1024 * 1024).err(),
            Some(SpeicherFehler::KeinPlatz)
        );
        // Krumme Länge wird aufgerundet, nicht abgewiesen: 100 Byte kaufen eine Seite.
        let k = s.anfordern(&park, PD_A, 100).expect("krumm ist ok");
        assert_eq!(k.len, PAGE_SIZE);
        s.zurueckgeben(&park, PD_A, k.handle).expect("Rueckgabe");
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
    }

    #[test]
    fn tabelle_voll_benannt() {
        // Die Kapazität hat einen Namen: der 17. lebende Grant wird abgewiesen (nicht blockiert,
        // nicht vergessen), und nach einer Rückgabe geht es weiter.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut scheine = Vec::new();
        // Slot 0 hält das Startkapital — 15 weitere Grants füllen die Tabelle.
        while scheine.len() < MAX_GRANTS - 1 {
            scheine.push(s.anfordern(&park, PD_A, PAGE_SIZE).expect("Platz in Tabelle"));
        }
        assert_eq!(s.grants_live(), MAX_GRANTS);
        assert_eq!(
            s.anfordern(&park, PD_A, PAGE_SIZE).err(),
            Some(SpeicherFehler::TabelleVoll)
        );
        let ersten = scheine.pop().expect("einer geht zurueck");
        s.zurueckgeben(&park, PD_A, ersten.handle).expect("Rueckgabe");
        let nach = s.anfordern(&park, PD_A, PAGE_SIZE).expect("wieder Platz");
        s.zurueckgeben(&park, PD_A, nach.handle).expect("Rueckgabe");
        for sc in scheine {
            s.zurueckgeben(&park, PD_A, sc.handle).expect("Rueckgabe");
        }
        assert_eq!(s.vergeben_bytes(), START_KAPITAL);
    }

    #[test]
    fn rueckgabe_rezykliert_bereich() {
        // Zurückgegeben heisst wiederverwendbar: derselbe 1-MiB-Bereich kommt zurück (First-fit),
        // vollständig beschreibbar — Bounce ist kein Leck, Freiliste kein Vergessen.
        let park = FakePark::neu(TEST_TID);
        let mut s = server();
        let mut schatten = schatten();
        let a = s.anfordern(&park, PD_A, ABNAHME_GRANT).expect("1 MiB");
        let off = a.offset;
        s.zurueckgeben(&park, PD_A, a.handle).expect("Rueckgabe");
        let b = s.anfordern(&park, PD_A, ABNAHME_GRANT).expect("erneut 1 MiB");
        assert_eq!(b.offset, off, "First-fit gibt denselben Bereich zurueck");
        assert_ne!(b.handle, a.handle, "neuer Schein, neues Handle");
        vollschreiben(&mut schatten, &b);
        assert!(voll_pruefen(&schatten, &b));
        s.zurueckgeben(&park, PD_A, b.handle).expect("Rueckgabe");
    }

    #[test]
    fn bereitschaft_rundweg() {
        // `Completion`-Fähigkeit wie im Treiber-Shim: Warten parkt, Melden weckt, verbraucht
        // wird genau einmal — und Melden-vor-Warten geht nicht verloren.
        let park_a = FakePark::neu(TEST_TID);
        let park_b = FakePark::neu(B_TID);
        let mut s = server();
        assert_eq!(s.auf_bereit_warten(&park_a), Ok(false));
        assert_eq!(park_a.blockierte(), 1);
        assert_eq!(s.grant_bereit_melden(&park_b), 1); // ein Wartender geweckt
        assert_eq!(s.bereit_offen(), 1);
        assert!(s.auf_bereit_warten(&park_a).expect("Platz")); // verbraucht es
        assert_eq!(s.bereit_offen(), 0);
        // Und der schnelle Pfad: Melden-vor-Warten geht nicht verloren.
        assert_eq!(s.grant_bereit_melden(&park_b), 0); // niemand wartete
        assert_eq!(park_a.blockierte(), 1); // kein neues Parken noetig
        assert!(s.auf_bereit_warten(&park_a).expect("Platz"));
    }
}
