//! **Der DMA-Pool einer Treiber-PD** (Z22, P3) — der Allokator liegt im Userland, der Kernel
//! mappt **einmal**.
//!
//! ## Warum das hierher gehört und nicht in den Kernel
//!
//! Ein Linux-Treiber ruft `dma_alloc_coherent` beim Aufsetzen und `dma_map_single` **je Anfrage**.
//! Wäre jedes davon ein Syscall, läge im heissen Pfad eines NVMe-Treibers ein Kernel-Eintritt je
//! Block. Er muss dort aber gar nicht sein: der Kernel hat die Region längst gemappt und den
//! IOVA-Bereich vergeben — was danach passiert, ist **Arithmetik innerhalb eines bereits
//! gewährten Fensters** und fügt keine Autorität hinzu.
//!
//! Der heisse Pfad ist damit **null Syscalls**.
//!
//! ## Das Loch, das dieser Typ schliesst
//!
//! Bis hierher reichte die Treiber-PD zwei lose `u64` durch — `dma_cpu` und `dma_dev` —, die per
//! **Konvention** zusammengehörten. Der Kernel trennt genau diese beiden Achsen seit jeher im Typ
//! (`addr::Pa` gegen `addr::Iova`, und `DmaRegion::identity` wurde absichtlich entfernt); in der
//! PD standen sie als nackte Zahlen nebeneinander. Die eigene Fallenliste nennt die Form:
//!
//! > Eine Funktion, die vollständig in `u64` rechnet, ist keine Kante, sondern ein Loch.
//!
//! Ein [`DmaBuf`] trägt **beide** Adressen und lässt sich nicht falsch herum auspacken.
//!
//! ## Was hier ausdrücklich NICHT geht — und warum das die Hauptsache ist
//!
//! [`DmaPool::map`] ist das Gegenstück zu `dma_map_single`, und es gibt **`None`** für jede
//! Adresse ausserhalb des Pools. Das ist kein fehlendes Feature, sondern der Zweck: einen
//! Stapelpuffer für DMA anzumelden ist in Linux-Treibern ein verbreiteter Fehler, und ohne diese
//! Prüfung entstünde daraus eine IOVA, die auf **fremden** Speicher zeigt. Ein Shim, der solche
//! Puffer braucht, kopiert sie in den Pool — das ist, was ein Bounce-Buffer ist.
//!
//! Abhängigkeitsfrei und `forbid(unsafe_code)`: die Fallen hier sind reine Adressarithmetik und
//! lassen sich mit **Literalen** auslösen, ohne Maschine — derselbe Grund wie bei
//! `caprock-sched::cycles` und `caprock-part`.

#![no_std]
#![forbid(unsafe_code)]

/// Warum ein Pool nicht entstehen konnte. Ein `Option` wäre hier zu wenig: „geht nicht" und
/// „geht nicht, WEIL die Gerätesicht gleich der CPU-Sicht ist" sind sehr verschiedene Befunde,
/// und der zweite ist ein Sicherheitsbefund.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PoolError {
    /// Länge 0 — ein Pool ohne Platz ist kein Pool, und ein Allokator, der immer `None` gibt,
    /// sieht aus wie ein voller.
    Leer,
    /// Keine Gerätesicht (`0`). Der Kernel gibt `0` zurück, wenn es keine IOVA gibt; damit zu
    /// rechnen hiesse, Offsets als absolute Geräteadressen auszugeben.
    KeineGeraetesicht,
    /// **CPU-Sicht und Gerätesicht sind gleich.** Das ist die Identitätsabbildung, und die soll
    /// es hier nicht geben (s. `docs/invariants.md` §2a-2e, `DmaRegion::identity` wurde
    /// entfernt). Sie durchzulassen hiesse: der Treiber liefe korrekt, solange die beiden
    /// zufällig übereinstimmen, und bräche, sobald jemand die Trennung durchsetzt.
    Identitaet,
    /// Die Region läuft in `u64` über — dann sind alle Bereichsprüfungen darunter wertlos.
    Ueberlauf,
}

/// **Ein Stück DMA-Speicher: CPU-Sicht und Gerätesicht, unzertrennlich.**
///
/// Es gibt keinen öffentlichen Konstruktor aus rohen Adressen — derselbe Gedanke wie bei
/// `addr::Va` im Kernel. Ein `DmaBuf` entsteht **nur** aus einem [`DmaPool`], und damit ist die
/// Zusicherung „diese beiden Adressen bezeichnen denselben Speicher" eine Eigenschaft des Typs
/// und nicht der Sorgfalt des Aufrufers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaBuf {
    cpu: u64,
    dev: u64,
    len: u64,
}

impl DmaBuf {
    /// Die Adresse, unter der **dieses Programm** den Puffer sieht.
    pub fn cpu(&self) -> u64 {
        self.cpu
    }
    /// Die Adresse, unter der **das Gerät** ihn sieht (IOVA). Nie gleich [`Self::cpu`] — der Pool
    /// weist die Identität beim Anlegen ab.
    pub fn dev(&self) -> u64 {
        self.dev
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Ein Teilstück. **Beide** Achsen wandern um denselben Offset weiter — das ist die Stelle,
    /// an der eine Deskriptorkette entsteht, und die Stelle, an der von Hand gerechnet
    /// regelmässig eine der beiden vergessen wird.
    pub fn sub(&self, off: u64, len: u64) -> Option<DmaBuf> {
        if off.checked_add(len)? > self.len {
            return None;
        }
        Some(DmaBuf {
            cpu: self.cpu.checked_add(off)?,
            dev: self.dev.checked_add(off)?,
            len,
        })
    }
}

/// **Der Pool über der gewährten Region.**
///
/// Vergabe ist ein Bump-Zeiger mit Marke/Freigabe, kein Freilisten-Allokator — und das ist keine
/// Sparsamkeit, sondern passt zur Nutzung: ein Treiber legt seine Ringe **einmal** beim Aufsetzen
/// an ([`Self::alloc`]) und braucht je Anfrage nur noch Adressen **innerhalb** dessen, was er
/// schon hat ([`Self::map`]). Für kurzlebige Puffer gibt es [`Self::mark`]/[`Self::release`].
pub struct DmaPool {
    cpu_base: u64,
    dev_base: u64,
    len: u64,
    next: u64,
    /// Höchststand — Telemetrie. Ein Pool, der knapp wird, soll das sagen können, bevor eine
    /// Allokation fehlschlägt.
    hoechststand: u64,
}

/// Eine Marke im Bump-Zeiger, an die [`DmaPool::release`] zurückgeht.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark(u64);

impl DmaPool {
    /// Aus dem, was `map_window` geliefert hat. **Fail-closed in vier Richtungen** — siehe
    /// [`PoolError`].
    pub fn new(cpu_base: u64, dev_base: u64, len: u64) -> Result<DmaPool, PoolError> {
        if len == 0 {
            return Err(PoolError::Leer);
        }
        if dev_base == 0 {
            return Err(PoolError::KeineGeraetesicht);
        }
        if cpu_base == dev_base {
            return Err(PoolError::Identitaet);
        }
        // Beide Achsen müssen die ganze Länge tragen, sonst ist jede Bereichsprüfung darunter
        // eine Attrappe hinter einem übergelaufenen Produkt.
        if cpu_base.checked_add(len).is_none() || dev_base.checked_add(len).is_none() {
            return Err(PoolError::Ueberlauf);
        }
        Ok(DmaPool {
            cpu_base,
            dev_base,
            len,
            next: 0,
            hoechststand: 0,
        })
    }

    /// Vergeben, ausgerichtet. `align` muss eine Zweierpotenz sein (`0`/krumme Werte → `None`,
    /// nicht stillschweigend auf 1 gerundet: eine Virtqueue mit falscher Ausrichtung wird vom
    /// Gerät nicht abgelehnt, sondern **falsch gelesen**).
    pub fn alloc(&mut self, len: u64, align: u64) -> Option<DmaBuf> {
        if len == 0 || align == 0 || !align.is_power_of_two() {
            return None;
        }
        let off = (self.next.checked_add(align - 1)?) & !(align - 1);
        let ende = off.checked_add(len)?;
        if ende > self.len {
            return None;
        }
        self.next = ende;
        if ende > self.hoechststand {
            self.hoechststand = ende;
        }
        Some(DmaBuf {
            cpu: self.cpu_base + off,
            dev: self.dev_base + off,
            len,
        })
    }

    /// **Das Gegenstück zu `dma_map_single` — und die wichtigste Absage in dieser Datei.**
    ///
    /// Gibt die Gerätesicht zu einer CPU-Adresse, die **im Pool liegt**. Für alles andere `None`.
    /// Ein Stapel- oder Heap-Puffer bekommt hier also keine IOVA, und das ist beabsichtigt:
    /// ohne diese Prüfung entstünde aus `dma_map_single(&lokale_variable)` eine Geräteadresse,
    /// die auf fremden Speicher zeigt — ein Gerät, das dorthin schreibt, beschädigt etwas, das
    /// niemand mit DMA in Verbindung bringt.
    pub fn map(&self, cpu: u64, len: u64) -> Option<DmaBuf> {
        let off = cpu.checked_sub(self.cpu_base)?;
        if off.checked_add(len)? > self.len {
            return None;
        }
        Some(DmaBuf {
            cpu,
            dev: self.dev_base + off,
            len,
        })
    }

    /// Den ganzen Pool als ein Stück (für einen Treiber, der sein Layout selbst festlegt).
    pub fn whole(&self) -> DmaBuf {
        DmaBuf {
            cpu: self.cpu_base,
            dev: self.dev_base,
            len: self.len,
        }
    }

    /// Stand merken.
    pub fn mark(&self) -> Mark {
        Mark(self.next)
    }
    /// Auf einen gemerkten Stand zurück. **Eine Marke aus der Zukunft wird abgewiesen** — sonst
    /// gäbe ein vertauschtes Paar von Marken den Bump-Zeiger frei, und zwei Anfragen bekämen
    /// denselben Puffer.
    pub fn release(&mut self, m: Mark) -> bool {
        if m.0 > self.next {
            return false;
        }
        self.next = m.0;
        true
    }

    pub fn used(&self) -> u64 {
        self.next
    }
    pub fn capacity(&self) -> u64 {
        self.len
    }
    /// Höchster je erreichter Füllstand — die Zahl, mit der sich die Poolgrösse begründen lässt,
    /// statt sie zu raten.
    pub fn hoechststand(&self) -> u64 {
        self.hoechststand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU: u64 = 0x4000_0000;
    const DEV: u64 = 0x1_0000_0000; // IOVA-Fenster liegt oberhalb RAM_TOP
    const LEN: u64 = 0x10000;

    fn pool() -> DmaPool {
        match DmaPool::new(CPU, DEV, LEN) {
            Ok(p) => p,
            Err(_) => panic!("Pool liess sich nicht anlegen"),
        }
    }

    #[test]
    fn identitaet_wird_abgewiesen() {
        // Die Kernaussage des ganzen Projekts, hier als Konstruktorbedingung.
        assert_eq!(DmaPool::new(CPU, CPU, LEN).err(), Some(PoolError::Identitaet));
    }

    #[test]
    fn keine_geraetesicht_wird_abgewiesen() {
        assert_eq!(DmaPool::new(CPU, 0, LEN).err(), Some(PoolError::KeineGeraetesicht));
    }

    #[test]
    fn leer_wird_abgewiesen() {
        assert_eq!(DmaPool::new(CPU, DEV, 0).err(), Some(PoolError::Leer));
    }

    #[test]
    fn ueberlauf_wird_abgewiesen() {
        assert_eq!(DmaPool::new(u64::MAX - 4, DEV, 16).err(), Some(PoolError::Ueberlauf));
    }

    #[test]
    fn beide_achsen_laufen_gleich_weit() {
        let mut p = pool();
        let b = p.alloc(4096, 4096).unwrap();
        // Das ist die Aussage, die zwei lose `u64` nicht garantieren koennen.
        assert_eq!(b.cpu() - CPU, b.dev() - DEV);
        assert_ne!(b.cpu(), b.dev());
    }

    #[test]
    fn ausrichtung_gilt_auf_beiden_achsen() {
        let mut p = pool();
        let _ = p.alloc(1, 1).unwrap(); // next = 1, also krumm
        let b = p.alloc(16, 4096).unwrap();
        assert_eq!(b.cpu() % 4096, 0);
        assert_eq!(b.dev() % 4096, 0);
    }

    #[test]
    fn krumme_ausrichtung_wird_abgewiesen() {
        let mut p = pool();
        // Nicht stillschweigend auf 1 runden: eine falsch ausgerichtete Virtqueue wird vom Geraet
        // nicht abgelehnt, sondern falsch gelesen.
        assert!(p.alloc(16, 3).is_none());
        assert!(p.alloc(16, 0).is_none());
    }

    #[test]
    fn erschoepfung_gibt_none_statt_ueberzulaufen() {
        let mut p = pool();
        assert!(p.alloc(LEN, 1).is_some());
        assert!(p.alloc(1, 1).is_none());
    }

    #[test]
    fn map_ausserhalb_gibt_keine_iova() {
        // **Der Fall, um den es geht:** `dma_map_single` auf einen Stapelpuffer.
        let p = pool();
        assert!(p.map(CPU - 1, 8).is_none()); // davor
        assert!(p.map(CPU + LEN, 8).is_none()); // dahinter
        assert!(p.map(CPU + LEN - 4, 8).is_none()); // ragt hinaus
        assert!(p.map(0xDEAD_0000, 8).is_none()); // irgendwo
    }

    #[test]
    fn map_innerhalb_trifft_dieselbe_achse_wie_alloc() {
        let mut p = pool();
        let b = p.alloc(512, 64).unwrap();
        let m = p.map(b.cpu(), 512).unwrap();
        assert_eq!(m.dev(), b.dev());
    }

    #[test]
    fn sub_bewegt_beide_achsen_und_prueft_die_grenze() {
        let mut p = pool();
        let b = p.alloc(1024, 64).unwrap();
        let s = b.sub(256, 256).unwrap();
        assert_eq!(s.cpu(), b.cpu() + 256);
        assert_eq!(s.dev(), b.dev() + 256);
        assert!(b.sub(1024, 1).is_none());
        assert!(b.sub(0, 1025).is_none());
        assert!(b.sub(u64::MAX, 1).is_none()); // Ueberlauf in der Addition
    }

    #[test]
    fn marke_gibt_frei_und_eine_zukuenftige_wird_abgewiesen() {
        let mut p = pool();
        let m = p.mark();
        let a = p.alloc(128, 8).unwrap();
        let zukunft = p.mark();
        assert!(p.release(m));
        // **Die Absage zuerst pruefen, und das ist keine Kosmetik:** die erste Fassung dieses
        // Tests belegte zwischendurch neu, womit `zukunft` gar nicht mehr in der Zukunft lag --
        // er haette die Eigenschaft, um die es geht, gar nicht ausloesen koennen. Eine veraltete
        // Marke gaebe hier Speicher frei, den jemand inzwischen wieder haelt.
        assert!(!p.release(zukunft));
        let b = p.alloc(128, 8).unwrap();
        assert_eq!(a.cpu(), b.cpu()); // derselbe Platz, wie gewollt
    }

    #[test]
    fn hoechststand_faellt_beim_freigeben_nicht() {
        let mut p = pool();
        let m = p.mark();
        let _ = p.alloc(4096, 8).unwrap();
        assert_eq!(p.hoechststand(), 4096);
        assert!(p.release(m));
        assert_eq!(p.used(), 0);
        assert_eq!(p.hoechststand(), 4096); // die Zahl, mit der man die Poolgroesse begruendet
    }
}
