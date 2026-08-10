//! **Descriptor-Typestate**: `Owned<Driver>` / `Owned<Device>` (todo E).
//!
//! # Einordnung — und die gehoert in den Code, nicht in eine Notiz
//!
//! **Das hier ist ERGONOMIE, nicht TCB.** Eine Compile-Zeit-Disziplin innerhalb einer Treiber-PD
//! traegt an der Vertrauensgrenze **nichts**. Sie faengt Fehler des Treiber**autors**; gegen das
//! Verhalten eines kompromittierten Treibers richtet sie nichts aus — der uebersetzt seinen Code
//! selbst, oder gar nicht, und das Geraet gehorcht ihm trotzdem. Was einen boesartigen Treiber
//! eindaemmt, ist die IOMMU-Zuteilung (B-3, A-5.4), und ausschliesslich die.
//!
//! Wer hier je liest „der Puffer ist geschuetzt": **nein.** Er ist *nicht mehr benennbar* — und
//! zwar nur in dem Code, der durch diese Typen geht. Das sind zwei verschiedene Aussagen, und die
//! Verwechslung waere genau die Sorte, die dieses Projekt an anderer Stelle schon bezahlt hat
//! (`rx_used` heisst „das Geraet hat gehandelt", nicht „Daten kamen an").
//!
//! Konkret gilt die Zusage **nicht**:
//! * ausserhalb dieser Crate. Der Aufrufer bekommt `cpu_base` als `u64` und darf die Region roh
//!   adressieren — der Kernel-Selbsttest tut das, und er darf es;
//! * gegen `core::ptr::write_volatile` an einer selbst gerechneten Adresse. Jede Zugriffsform hier
//!   ist ohnehin `unsafe`, weil sie auf rohen Adressen arbeitet. Der Typestate macht daraus keine
//!   sichere Schnittstelle; er macht daraus eine **benannte** Uebergabe.
//!
//! # Wogegen es dann hilft
//!
//! Gegen genau einen Fehler, und der ist real und haeufig: **der Puffer steht armiert in der
//! Queue und wird im Treibercode weiter beschrieben.** Das Geraet liest oder schreibt dabei
//! gleichzeitig. Das Ergebnis ist kein Absturz, sondern ein halb altes, halb neues Paket — ein
//! Fehlerbild, das sich unter Last anders zeigt als beim Einzelversuch und deshalb genau dann
//! auftritt, wenn niemand hinsieht.
//!
//! Der Uebergang ist deshalb ein **Zug**, keine Vereinbarung: [`crate::Queue::arm`] nimmt den
//! Puffer `by value`. [`Owned`] ist weder `Copy` noch `Clone`; nach dem Armieren gibt es den
//! Namen nicht mehr. Und `Owned<Device>` hat **keinen einzigen** Zugriffsweg — nicht einen
//! privaten, nicht einen `unsafe`. Ein Zugriff braucht erst den Rueckweg
//! ([`crate::Queue::reclaim`]), und der braucht den Abschlussbeleg des Geraets.
//!
//! # Der Rueckweg ohne Beleg ist BENANNT, nicht verboten
//!
//! [`crate::Queue::reclaim_unproven`] gibt es, weil es einen Fall gibt, in dem der Treiber den
//! Puffer **ohne** used-Fortschritt lesen muss und lesen soll: `blk` liest das Statusbyte auch
//! nach einem Poll-Timeout. Bleibt dort `0xff` stehen, ist damit belegt, dass das Geraet nichts
//! geschrieben hat — und nicht bloss, dass wir zu frueh aufgehoert haben zu warten. Das ist eine
//! Aussage, die man verlieren wuerde, wenn der Typestate diesen Weg verboten haette.
//!
//! Solche Wege verschwinden nicht, wenn man sie verbietet; sie wandern dann in ein `unsafe`-Loch
//! ohne Namen. Also bekommen sie einen Namen und eine Begruendungspflicht an der Aufrufstelle.

use core::marker::PhantomData;

/// Zustand: der Puffer gehoert dem **Treiber**. Nur hier gibt es Zugriffswege.
///
/// Unbewohnter Typ: es gibt keinen Wert davon, er steht ausschliesslich im Typ. Ein
/// Zustandsmarker, den man versehentlich konstruieren und herumreichen kann, ist einer, der
/// irgendwann in einer Struktur landet und dort etwas anderes bedeutet.
pub enum Driver {}

/// Zustand: der Puffer ist **armiert** — er steht in einem Deskriptor und gehoert dem Geraet.
///
/// Dieser Typ hat **keine** Zugriffsmethode. Das ist die ganze Zusicherung.
pub enum Device {}

/// Ein Puffer in der DMA-Region, mit **beiden** Achsen und einem Besitzer im Typ.
///
/// Die zwei Adressen stehen absichtlich zusammen — und zwar hier und nur hier. Anderswo in dieser
/// Crate gilt weiter die Regel aus [`crate::Queue`]: „beide Adressen in einer Struktur zu halten,
/// aus der man sich je nach Zweck die passende greift", ist der Weg, auf dem aus zwei Achsen eine
/// wird. Der Unterschied ist, dass hier **niemand waehlt**: `cpu` ist von aussen nicht lesbar und
/// wird ausschliesslich von den Zugriffsmethoden benutzt, `dev` ausschliesslich vom Deskriptor.
/// Es gibt keinen Getter, der die eine liefert, wo die andere gemeint war.
pub struct Owned<S> {
    /// Treibersicht (die CPU adressiert darueber).
    cpu: u64,
    /// Geraetesicht (steht im Deskriptor). Mit IOMMU-Fenster != 0 ist das eine **andere** Zahl.
    dev: u64,
    len: u32,
    _s: PhantomData<S>,
}

// **Kein `Clone`, kein `Copy` — und das ist kein Vergessen.** Waere `Owned` kopierbar, machte
// `arm(.., buf, ..)` den Namen nicht ungueltig, und der ganze Mechanismus waere eine Verzierung.
// Gemessen (`tools/typestate-negativ.sh` gegen eine Mutation): mit einem handgeschriebenen
// `impl<S> Copy for Owned<S>` uebersetzt „armiert und trotzdem weiter beschrieben" wieder.
//
// **Die Falle dabei, falls jemand das nachprueft:** ein `#[derive(Clone, Copy)]` an dieser Stelle
// waere ein NO-OP. Das Derive erzeugt die Bounds `S: Clone + Copy`, und `Driver`/`Device` sind
// unbewohnte Enums ohne diese Impls — `Owned<Driver>` bliebe also bewegt-statt-kopiert, und die
// Mutation saehe wie ein Beleg fuer Robustheit aus, obwohl sie gar nichts geaendert hat. Selbst
// gemessen: erst das manuelle Impl kippt den Fall.
impl<S> Owned<S> {
    /// Die **Geraetesicht** — die Zahl, die in den Deskriptor geht.
    pub fn dev_addr(&self) -> u64 {
        self.dev
    }
    /// Laenge in Bytes.
    pub fn len(&self) -> u32 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Zustandswechsel — **nur** crate-intern, und nur ueber [`crate::Queue`].
    pub(crate) fn transition<T>(self) -> Owned<T> {
        Owned { cpu: self.cpu, dev: self.dev, len: self.len, _s: PhantomData }
    }
}

impl Owned<Driver> {
    /// Einen Puffer aus zwei rohen Adressen bilden.
    ///
    /// # Safety
    /// `cpu` und `dev` muessen die beiden Sichten **desselben** Speichers sein, der Bereich
    /// `[cpu, cpu+len)` muss dem Aufrufer allein gehoeren, und es darf kein zweites `Owned`
    /// darauf geben. Der bequeme Weg ist [`Region::carve`] — das erzwingt die Disjunktheit.
    pub const unsafe fn from_raw(cpu: u64, dev: u64, len: u32) -> Self {
        Owned { cpu, dev, len, _s: PhantomData }
    }

    /// Die **Geraetesicht** ersetzen, ohne die Treibersicht anzufassen.
    ///
    /// Genau ein Zweck: der Kreuz-DMA-Nachweis (A-5.4). Dort wird dem Geraet die IOVA einer
    /// **fremden** Region genannt, waehrend der Treiber weiter seinen eigenen Puffer liest — das
    /// ist die Anordnung, in der „nichts kam an" ueberhaupt etwas bedeutet. Wuerden beide Sichten
    /// wandern, scheiterte der Versuch an der falschen Stelle.
    ///
    /// # Safety
    /// Der Aufrufer sagt zu, dass er genau das pruefen will — **nicht**, dass ihm `dev` gehoert.
    pub unsafe fn retarget_device_view(&mut self, dev: u64) {
        self.dev = dev;
    }

    /// # Safety
    /// `off + Breite` muss innerhalb `len` liegen; der Speicher muss gemappt sein.
    pub unsafe fn wr8(&mut self, off: u64, v: u8) {
        crate::wr8(self.cpu + off, v)
    }
    /// # Safety
    /// wie [`Self::wr8`].
    pub unsafe fn wr32(&mut self, off: u64, v: u32) {
        crate::wr32(self.cpu + off, v)
    }
    /// # Safety
    /// wie [`Self::wr8`].
    pub unsafe fn wr64(&mut self, off: u64, v: u64) {
        crate::wr64(self.cpu + off, v)
    }
    /// # Safety
    /// wie [`Self::wr8`].
    pub unsafe fn rd8(&self, off: u64) -> u8 {
        crate::rd8(self.cpu + off)
    }
    /// # Safety
    /// wie [`Self::wr8`].
    pub unsafe fn rd64(&self, off: u64) -> u64 {
        crate::rd64(self.cpu + off)
    }
    /// `n` Bytes ab `off` nullen.
    ///
    /// Als eigene Methode, weil das Halb-Nullen dieses Projekt schon einmal einen Testbefund
    /// gekostet hat: `arp_probe` nullte acht Byte, und die zweite Probe las die Antwort der
    /// ersten (A-5.4).
    ///
    /// # Safety
    /// wie [`Self::wr8`].
    pub unsafe fn zero(&mut self, off: u64, n: u64) {
        for i in 0..n {
            crate::wr8(self.cpu + off + i, 0);
        }
    }
}

/// Die DMA-Region eines Treibers — die **einzige** unverdaechtige Quelle von `Owned<Driver>`.
///
/// `carve` schneidet **monoton** heraus: jeder Puffer beginnt hinter dem Ende des vorigen. Damit
/// koennen zwei `Owned` derselben Region sich nicht ueberlappen, und die Zusicherung
/// „genau ein Besitzer" haengt nicht an der Sorgfalt an der Aufrufstelle. Alle drei Treiber dieser
/// Crate legen ihre Puffer ohnehin aufsteigend an; die Einschraenkung kostet also nichts und
/// verhindert den Fall, in dem jemand zwei Sichten auf denselben Puffer haelt und eine davon
/// armiert.
pub struct Region {
    cpu: u64,
    dev: u64,
    len: u64,
    /// Ende des zuletzt herausgeschnittenen Puffers.
    cut: u64,
}

impl Region {
    /// # Safety
    /// `cpu`/`dev` muessen die beiden Sichten derselben Region sein, `[cpu, cpu+len)` muss dem
    /// Aufrufer allein gehoeren.
    pub const unsafe fn from_raw(cpu: u64, dev: u64, len: u64) -> Self {
        Region { cpu, dev, len, cut: 0 }
    }

    /// Einen Puffer `[off, off+len)` herausschneiden.
    ///
    /// `None`, wenn er hinter das Regionsende reichte oder **hinter den Schnitt zurueckgriffe**.
    /// Beides ist ein Treiberfehler, und beides fail-closed: kein Puffer statt eines, der sich mit
    /// einem armierten ueberlappt.
    pub fn carve(&mut self, off: u64, len: u32) -> Option<Owned<Driver>> {
        let end = off.checked_add(len as u64)?;
        if off < self.cut || end > self.len {
            return None;
        }
        self.cut = end;
        // SAFETY: `[off, end)` liegt in der Region, beginnt hinter jedem vorher vergebenen Stueck
        // und wird kein zweites Mal vergeben (`cut` ist monoton).
        Some(unsafe { Owned::from_raw(self.cpu + off, self.dev + off, len) })
    }
}

/// **Beleg, dass das Geraet die Kette abgeschlossen hat** — ein used-Ring-Eintrag.
///
/// Er ist der Schluessel fuer [`crate::Queue::reclaim`]: ohne ihn gibt es den Puffer nur ueber den
/// benannten Ausweg zurueck. Das ist dieselbe Trennung wie bei `rx_used` gegen „Daten angekommen":
/// dieser Beleg sagt, dass das Geraet **fertig** ist, und sonst nichts.
#[derive(Clone, Copy)]
pub struct Completion {
    id: u32,
    len: u32,
}

impl Completion {
    pub(crate) fn new(id: u32, len: u32) -> Self {
        Completion { id, len }
    }
    /// Deskriptor-Index des Kettenkopfs, den das Geraet zurueckgibt.
    pub fn id(&self) -> u32 {
        self.id
    }
    /// Vom Geraet gemeldete Zahl **geschriebener** Bytes (alle geraeteschreibbaren Glieder).
    pub fn len(&self) -> u32 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carve_ist_monoton_und_begrenzt() {
        // SAFETY: Testadressen, es wird nie dereferenziert.
        let mut r = unsafe { Region::from_raw(0x1000, 0x7000_0000, 0x2000) };
        let a = r.carve(0x800, 16).expect("erster Schnitt");
        assert_eq!(a.dev_addr(), 0x7000_0800);
        assert_eq!(a.len(), 16);
        // Rueckgriff hinter den Schnitt -> abgewiesen (waere eine Ueberlappung).
        assert!(r.carve(0x800, 16).is_none());
        assert!(r.carve(0x804, 4).is_none());
        // Direkt anschliessend geht.
        let b = r.carve(0x810, 1).expect("zweiter Schnitt");
        assert_eq!(b.dev_addr(), 0x7000_0810);
        // Ueber das Regionsende hinaus -> abgewiesen.
        assert!(r.carve(0x1ff0, 32).is_none());
        // Ueberlauf -> abgewiesen, nicht gewrappt.
        assert!(r.carve(u64::MAX - 4, 32).is_none());
    }

    #[test]
    fn retarget_laesst_die_treibersicht_stehen() {
        // SAFETY: Testadressen, es wird nie dereferenziert.
        let mut r = unsafe { Region::from_raw(0x1000, 0x7000_0000, 0x2000) };
        let mut b = r.carve(0, 64).unwrap();
        assert_eq!(b.dev_addr(), 0x7000_0000);
        // SAFETY: Testadressen.
        unsafe { b.retarget_device_view(0xdead_0000) };
        assert_eq!(b.dev_addr(), 0xdead_0000);
        // Die Laenge bleibt die des eigenen Puffers -- eine fremde Geraetesicht macht ihn nicht
        // groesser.
        assert_eq!(b.len(), 64);
    }

    #[test]
    fn completion_traegt_beide_zahlen_getrennt() {
        let c = Completion::new(3, 513);
        assert_eq!(c.id(), 3);
        assert_eq!(c.len(), 513);
        assert!(!c.is_empty());
    }
}
