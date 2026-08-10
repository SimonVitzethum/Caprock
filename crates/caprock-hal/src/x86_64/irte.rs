//! **Interrupt-Remapping-Einträge und MSI-Adressen kodieren** (Z22, P1) — reine Bitrechnung.
//!
//! ## Warum das eine eigene Datei ist
//!
//! Genau aus dem Grund, aus dem `dmar.rs` und `cycles.rs` eigene Dateien sind: hier passiert
//! **keine** Hardware, sondern Schieberei. Die Fallen sind ein Bit an der falschen Stelle, und ein
//! Bit an der falschen Stelle in einer IRTE äussert sich als „das Gerät unterbricht einfach nicht"
//! — ohne Fehlermeldung, ohne Fault, ohne irgendetwas, das nach einem Fehler aussieht. Ein
//! Host-Test mit Literalen trifft das; eine QEMU-Suite braucht dafür ein Gerät, einen Treiber und
//! Glück.
//!
//! `forbid(unsafe_code)`, keine `use`-Zeile ausser `super::*` im Testmodul → als **Datei**
//! prüfbar:
//! ```text
//! rustc --test --edition 2021 -O crates/caprock-hal/src/x86_64/irte.rs -o /tmp/t && /tmp/t
//! ```
//!
//! ## Was hier die Sicherheitsaussage trägt: `SVT` und `SID`
//!
//! Eine Treiber-PD besitzt das MMIO-Fenster ihres Geräts — also **auch dessen MSI-X-Tabelle**.
//! Sie schreibt die Adress-/Datenwerte dort selbst hinein, und das ist gewollt: es spart einen
//! Syscall je Vektor und der Kernel muss das Tabellenformat nicht kennen.
//!
//! Damit könnte sie aber den **Handle einer fremden IRTE** eintragen und so den Interrupt einer
//! anderen PD auslösen. Genau dagegen steht [`SVT_SID`]: die Einheit prüft bei jeder
//! Interrupt-Nachricht, ob die **Quell-BDF** zu der im Eintrag hinterlegten passt. Der Handle
//! einer fremden IRTE, abgeschickt vom eigenen Gerät, wird abgewiesen — nicht umgeleitet.
//!
//! Ohne diese Prüfung wäre „die PD programmiert ihre MSI-X-Tabelle selbst" ein Loch, und zwar
//! eines, das im Normalbetrieb nie auffällt.
//!
//! ## Vorbedingung, die NICHT hier steht
//!
//! `EIME` (Extended Interrupt Mode) ist im Bring-up **aus** — es gilt nur im x2APIC-Modus, und den
//! meldet nicht jede Plattform. Deshalb kodiert [`irte_build`] das Ziel im **xAPIC**-Format
//! (8-Bit-APIC-ID in Bits 47:40). Wer EIME einschaltet, muss hier mit ändern; [`irte_build`] weist
//! eine APIC-ID über 255 deshalb **ab**, statt sie abzuschneiden.
//!
//! ## Die VERGABE (seit 2026-08-10) — und für WEN sie da ist
//!
//! Bis hierher war das eine reine Kodierung ohne Abnehmer: die Remapping-Tabelle stand seit B-3.2
//! auf lauter „not present", `intc::enable_intid`/`mask_intid`/`route_spi` sind auf x86 No-Ops, und
//! **jeder** Treiber pollt. Der zweite Teil dieser Datei ist der Zuteiler: welcher Index gehört
//! welchem Gerät, was steht darin, und was passiert, wenn keiner mehr frei ist.
//!
//! **Das ist nicht GPU-Arbeit.** Ein Treiber, der pollt, kostet einen Kern; das gilt für NVMe, für
//! eine Netzkarte, für einen USB-Controller und für eine GPU gleichermassen. Die Vergabe ist
//! **je Gerät** formuliert (eine BDF, ein Block von Handles) und kennt keine Geräteklasse — sie
//! muss auch keine kennen, weil `SVT`/`SID` das Einzige ist, was hier Autorität trägt.
//!
//! **Was dagegen SEHR WOHL von der Geräteart abhängt, und deshalb benannt ist statt geraten:**
//! [`Vektorform`]. MSI-X hat je Vektor eine eigene Tabellenzeile mit eigener Adresse — jeder Vektor
//! bekommt einen eigenen Handle, und die Handles müssen weder zusammenhängen noch ausgerichtet
//! sein. Klassisches MSI hat **ein** Adress-/Datenpaar; das Gerät erzeugt seine übrigen Vektoren,
//! indem es den *Subhandle* im Datenwort hochzählt (`SHV = 1`, Index = Handle + Subhandle). Dort
//! müssen die Handles zusammenhängen, und die Anzahl ist wegen des `MME`-Feldes der
//! MSI-Capability eine Zweierpotenz. `request_irq` in einem Linux-Treiber ist über beide Formen
//! derselbe Aufruf — hier sind sie zwei Fälle.

#![forbid(unsafe_code)]

/// Ein 128-Bit-Eintrag der Interrupt-Remapping-Tabelle.
///
/// Zwei Worte, und die Reihenfolge ist Teil der Aussage: `lo` **zuletzt** schreiben. Solange
/// `lo.P == 0` ist der Eintrag nicht vorhanden; wer `lo` zuerst schriebe, machte einen Eintrag
/// gültig, dessen Quellprüfung (`hi`) noch nicht darinsteht.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Irte {
    pub lo: u64,
    pub hi: u64,
}

/// `SVT` = 01: die Einheit prüft **Quell-BDF gegen `SID`**. Das ist die Zeile, die verhindert,
/// dass ein Gerät den Interrupt-Handle eines anderen benutzt.
pub const SVT_SID: u64 = 1;
/// `SQ` = 00: alle 16 Bits der `SID` müssen passen (keine Maskierung von Funktions- oder
/// Gerätenummer).
pub const SQ_ALLE_16: u64 = 0;

/// Zustellungsart „Fixed" (000) — kein Lowest-Priority. Lowest-Priority überlässt dem Chipsatz
/// die Kernwahl; ein Treiber, dessen IRQ-Thread an einen Kern gebunden ist, will das nicht.
const DLM_FIXED: u64 = 0;

/// **Einen IRTE bauen: vorhanden, flankengetriggert, fixed, physisch adressiert, quellgeprüft.**
///
/// * `vector` — der IDT-Vektor, unter dem der Kernel ihn erwartet.
/// * `apic_id` — Ziel-LAPIC (xAPIC-Format, also ≤ 255; s. Modul-Doku zu `EIME`).
/// * `sid` — die BDF des berechtigten Geräts: `(bus << 8) | (dev << 3) | func`.
///
/// `None`, wenn Vektor oder APIC-ID nicht darstellbar sind. **Abweisen statt abschneiden**: eine
/// abgeschnittene APIC-ID zeigt auf einen anderen Kern, und der Interrupt käme still am falschen
/// Ort an — schlimmer als gar keiner, weil er wie ein Erfolg aussieht.
///
/// Vektoren unter 32 sind CPU-Ausnahmen und werden ebenfalls abgewiesen; ein Gerät, das Vektor 14
/// auslöst, sähe für den Kernel wie ein Seitenfehler aus.
pub fn irte_build(vector: u8, apic_id: u32, sid: u16) -> Option<Irte> {
    if vector < 32 {
        return None;
    }
    if apic_id > 0xFF {
        return None; // s. Modul-Doku: ohne EIME passt nur eine 8-Bit-ID
    }
    // lo:
    //   0    P   = 1  (vorhanden)
    //   1    FPD = 0  (Faults werden gemeldet -- eine stumme Einheit sieht aus wie eine heile)
    //   2    DM  = 0  (physisch)
    //   3    RH  = 0
    //   4    TM  = 0  (flankengetriggert -- MSI IST flankengetriggert; das Maskieren, das ein
    //                  level-getriggerter SPI braucht, entfaellt damit)
    //   7:5  DLM = 000 (fixed)
    //  23:16 Vector
    //  47:40 Destination (xAPIC-ID)
    let lo = 1 | (DLM_FIXED << 5) | ((vector as u64) << 16) | ((apic_id as u64) << 40);
    // hi:
    //  15:0  SID
    //  17:16 SQ
    //  19:18 SVT
    let hi = (sid as u64) | (SQ_ALLE_16 << 16) | (SVT_SID << 18);
    Some(Irte { lo, hi })
}

/// Eine BDF zu einer `SID` zusammensetzen. `None` bei unmöglichen Werten — `dev` hat 5 Bit,
/// `func` 3.
pub fn sid_from_bdf(bus: u8, dev: u8, func: u8) -> Option<u16> {
    if dev > 31 || func > 7 {
        return None;
    }
    Some(((bus as u16) << 8) | ((dev as u16) << 3) | (func as u16))
}

/// **Die MSI-Adresse im „Remappable"-Format** — das, was die PD in ihre MSI-X-Tabelle schreibt.
///
/// ```text
/// 31:20  0xFEE
/// 19:5   Handle[14:0]
/// 4      SHV (SubHandle Valid) = 0
/// 3      Interrupt Format = 1  <-- DIESES Bit unterscheidet remapped von compatibility
/// 2      Handle[15]
/// 1:0    0
/// ```
///
/// Bit 3 ist der ganze Unterschied: ohne es wäre die Nachricht im **Compatibility**-Format, und
/// das ist genau das, was der Bring-up mit `CFI` abgeschaltet hat. Eine Adresse ohne dieses Bit
/// führt also nicht zu einem falschen Interrupt, sondern zu **gar keinem** — und das sieht aus wie
/// ein stummes Gerät.
pub fn msi_addr(handle: u16) -> u32 {
    let h = handle as u32;
    0xFEE0_0000 | ((h & 0x7FFF) << 5) | (1 << 3) | ((h >> 15) << 2)
}

/// Das MSI-Datenwort im Remappable-Format bei `SHV = 0`: **null**.
///
/// Es steht hier als Funktion und nicht als „schreib halt 0 hin", weil die Null eine Bedeutung
/// hat: Vektor und Ziel stehen in der IRTE, nicht in der Nachricht. Wer hier den Vektor
/// hineinschriebe (wie im Compatibility-Format üblich), bekäme einen Sub-Handle, den niemand
/// vergeben hat.
pub fn msi_data() -> u32 {
    0
}

/// Die MSI-Adresse mit **`SHV = 1`** — für klassisches MSI mit mehr als einer Nachricht.
///
/// Das Gerät bekommt genau **eine** Adresse; die weiteren Vektoren entstehen dadurch, dass es den
/// **Subhandle** im Datenwort hochzählt. Die Einheit rechnet `Index = Handle + Subhandle`.
///
/// Wer hier `SHV = 0` liesse und trotzdem mehrere Nachrichten freischaltete, bekäme für **alle**
/// Vektoren denselben IRTE-Index — also denselben CPU-Vektor. Das sieht aus wie ein Treiber, der
/// seine Warteschlangen nicht auseinanderhält, und nicht wie ein falsches Bit.
pub fn msi_addr_shv(handle: u16) -> u32 {
    msi_addr(handle) | (1 << 4)
}

// ================================================================================================
// Die Vergabe (Z22 P1, zweiter Teil — 2026-08-10)
// ================================================================================================

/// Einträge der Interrupt-Remapping-Tabelle: 256 × 16 B = **eine** 4-KiB-Seite.
///
/// **Eine Quelle für Tabellengrösse und Grössenfeld.** Bis zum 2026-08-10 standen beide Zahlen in
/// `vtd.rs` und der Zuteiler hätte hier seine eigene dritte gehabt — genau die Form, die
/// `iova_window_clear_of_msi` schon einmal gekostet hat („Zuteiler und Prüfer brauchen EINE
/// Quelle"). Jetzt liest `vtd.rs` diese beiden Konstanten, und
/// [`tests::groessenfeld_passt_zur_tabellengroesse`] hält sie aneinander.
pub const IRT_EINTRAEGE: usize = 256;

/// `IRTA.S` — die Einheit liest die Tabellengrösse als `2^(S+1)`.
pub const IRTA_GROESSENFELD: u64 = 7;

/// **Für welche Geräteart** ein Vektorblock vergeben wird. Der Unterschied ist keine
/// Geschmacksfrage, sondern verändert, welche Indizes überhaupt zulässig sind — s. Modul-Doku.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vektorform {
    /// **MSI-X.** Je Vektor eine Tabellenzeile mit eigener Adresse ⇒ eigener Handle je Vektor,
    /// keine Ausrichtungsbedingung. Der Treiber schreibt `anzahl` Zeilen.
    MsiX,
    /// **Klassisches MSI.** Ein Adress-/Datenpaar, `SHV = 1`, Index = Handle + Subhandle ⇒ die
    /// Handles müssen **zusammenhängen**, und die Anzahl ist eine Zweierpotenz ≤ 32 (das `MME`-Feld
    /// der MSI-Capability kennt nur 1/2/4/8/16/32).
    ///
    /// **Zusätzlich ausgerichtet vergeben, und das ist eine bewusste Verschärfung.** Ob die
    /// Einheit Ausrichtung *verlangt*, ist aus der Addition `Handle + Subhandle` nicht
    /// herzuleiten — sie tut es vermutlich nicht. Ausgerichtet zu vergeben kostet nur
    /// Tabellenplatz und kann nicht falsch sein; ungerichtet zu vergeben wäre eine Wette auf eine
    /// Lesart, die hier niemand gemessen hat. Steht als offener Punkt im Bericht.
    Msi,
}

/// **Warum eine Vergabe nicht zustande kam.** Kein `None`: „die Tabelle ist voll" und „dein Vektor
/// ist eine CPU-Ausnahme" sind verschiedene Befunde mit verschiedenen Adressaten, und der zweite
/// ist ein Programmfehler des Aufrufers.
///
/// Die Lehre aus D11 eine Ebene tiefer: wer eine Kapazität einführt, muss den Überlauf **benennen**
/// — sonst ist die Schranke kein Schutz, sondern ein Loch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VergabeFehler {
    /// Interrupt Remapping läuft nicht (keine Tabelle, `IRE` aus, oder QI fehlt). **Kein
    /// Rückfall auf das Compatibility-Format** — das hat der Bring-up mit `GCMD.CFI = 0` gerade
    /// abgeschaltet, und ein Rückfall darauf wäre die Umgehung der ganzen Tabelle.
    TabelleNichtAktiv,
    /// `anzahl == 0`. Ein Block ohne Vektoren ist kein Block; ihn durchzulassen ergäbe ein Ticket,
    /// das der Treiber für gültig hält.
    AnzahlNull,
    /// MSI verlangt eine Zweierpotenz (`MME`), es kam eine andere Zahl.
    AnzahlKeineZweierpotenz { anzahl: usize },
    /// MSI kennt höchstens 32 Nachrichten je Gerät.
    ZuVieleMsiNachrichten { anzahl: usize },
    /// Mehr Vektoren angefordert, als die Tabelle überhaupt fasst — das ist keine Erschöpfung,
    /// sondern ein Wunsch, der auf dieser Plattform **nie** erfüllbar ist.
    GroesserAlsTabelle { anzahl: usize, kapazitaet: usize },
    /// **Kein freier Block.** Mit den Zahlen, die den Fall handhabbar machen: wie viele Einträge
    /// überhaupt noch frei sind und wie gross der grösste zusammenhängende (bzw. ausgerichtete)
    /// Block ist. „Voll" und „zerstückelt" sind verschiedene Lagen mit verschiedenen Abhilfen.
    KeinFreierBlock {
        angefordert: usize,
        frei: usize,
        groesster_block: usize,
    },
    /// Der Vektor (oder ein abgeleiteter Vektor des Blocks) liegt unter 32 — das sind
    /// CPU-Ausnahmen. Ein Gerät auf Vektor 14 sähe für den Kernel wie ein Seitenfehler aus.
    VektorReserviert { vektor: u32 },
    /// Der letzte Vektor des Blocks liefe über 255 hinaus.
    VektorbereichLaeuftUeber { basis: u8, anzahl: usize },
    /// APIC-ID > 255 ohne `EIME` — abgeschnitten zeigte sie auf einen **anderen** Kern.
    ApicIdZuGross { apic_id: u32 },
    /// Die Einheit hat die Invalidierung des Interrupt-Entry-Cache nicht bestätigt. Der Eintrag
    /// wird dabei **zurückgenommen**; ein präsenter Eintrag, dessen Wirksamkeit niemand abgewartet
    /// hat, ist genau die Zusage, die dieses Projekt nicht ausspricht.
    IecInvalidierungFehlgeschlagen { index: u16 },
    /// Freigabe eines Blocks, der so nie vergeben wurde (falscher Handle, doppelte Freigabe).
    NichtVergeben { index: u16, anzahl: usize },
}

/// **Der Index-Allokator über der Remapping-Tabelle** — ein Bitfeld, sonst nichts.
///
/// Bewusst ohne Freiliste: 256 Einträge passen in vier `u64`, die Suche ist linear über 256 Bits
/// und läuft einmal je Gerät beim Aufsetzen. Ein Allokator mit Datenstruktur wäre hier mehr Code
/// als Nutzen — und mehr Code, der beim Teardown falsch sein kann.
#[derive(Clone, Copy)]
pub struct IrteAllocator {
    belegt: [u64; IRT_EINTRAEGE / 64],
    /// Höchststand — die Zahl, mit der sich eine Tabellengrösse begründen lässt, statt sie zu
    /// raten. Fällt beim Freigeben **nicht**.
    hoechststand: usize,
}

impl Default for IrteAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl IrteAllocator {
    pub const fn new() -> Self {
        IrteAllocator {
            belegt: [0; IRT_EINTRAEGE / 64],
            hoechststand: 0,
        }
    }

    fn bit(&self, i: usize) -> bool {
        self.belegt[i / 64] & (1u64 << (i % 64)) != 0
    }
    fn setze(&mut self, i: usize) {
        self.belegt[i / 64] |= 1u64 << (i % 64);
    }
    fn loesche(&mut self, i: usize) {
        self.belegt[i / 64] &= !(1u64 << (i % 64));
    }

    /// Wie viele Einträge sind vergeben?
    pub fn belegt_anzahl(&self) -> usize {
        self.belegt.iter().map(|w| w.count_ones() as usize).sum()
    }
    /// Wie viele sind frei?
    pub fn frei_anzahl(&self) -> usize {
        IRT_EINTRAEGE - self.belegt_anzahl()
    }
    /// Höchster je erreichter Belegungsstand.
    pub fn hoechststand(&self) -> usize {
        self.hoechststand
    }
    /// Ist dieser Index vergeben? (Für Audits und Tests.)
    pub fn ist_belegt(&self, index: u16) -> bool {
        (index as usize) < IRT_EINTRAEGE && self.bit(index as usize)
    }

    /// Der längste zusammenhängende freie Lauf — die Zahl, die [`VergabeFehler::KeinFreierBlock`]
    /// von „voll" unterscheidbar macht.
    pub fn groesster_block(&self) -> usize {
        let mut best = 0;
        let mut lauf = 0;
        for i in 0..IRT_EINTRAEGE {
            if self.bit(i) {
                lauf = 0;
            } else {
                lauf += 1;
                if lauf > best {
                    best = lauf;
                }
            }
        }
        best
    }

    /// **Einen Block reservieren.** `ausgerichtet` verlangt zusätzlich `index % anzahl == 0`
    /// (s. [`Vektorform::Msi`]).
    ///
    /// Fail-closed: gibt es keinen passenden Block, wird **abgesagt** — es wird ausdrücklich nicht
    /// auf „dann eben ein kleinerer" zurückgefallen. Ein Treiber, der vier Warteschlangen
    /// aufsetzt und einen Vektor bekommt, merkt das erst unter Last.
    pub fn reserviere(
        &mut self,
        anzahl: usize,
        ausgerichtet: bool,
    ) -> Result<u16, VergabeFehler> {
        if anzahl == 0 {
            return Err(VergabeFehler::AnzahlNull);
        }
        if anzahl > IRT_EINTRAEGE {
            return Err(VergabeFehler::GroesserAlsTabelle {
                anzahl,
                kapazitaet: IRT_EINTRAEGE,
            });
        }
        let schritt = if ausgerichtet { anzahl } else { 1 };
        let mut start = 0;
        while start + anzahl <= IRT_EINTRAEGE {
            if (start..start + anzahl).all(|i| !self.bit(i)) {
                for i in start..start + anzahl {
                    self.setze(i);
                }
                let jetzt = self.belegt_anzahl();
                if jetzt > self.hoechststand {
                    self.hoechststand = jetzt;
                }
                return Ok(start as u16);
            }
            start += schritt;
        }
        Err(VergabeFehler::KeinFreierBlock {
            angefordert: anzahl,
            frei: self.frei_anzahl(),
            groesster_block: self.groesster_block(),
        })
    }

    /// Einen Block freigeben. **Nur, wenn er vollständig vergeben war** — sonst gäbe eine
    /// doppelte oder falsche Freigabe einen Index frei, den ein anderes Gerät gerade hält, und
    /// dessen Interrupt landete danach bei einem fremden Treiber.
    pub fn gib_frei(&mut self, index: u16, anzahl: usize) -> Result<(), VergabeFehler> {
        let start = index as usize;
        if anzahl == 0 || start + anzahl > IRT_EINTRAEGE {
            return Err(VergabeFehler::NichtVergeben { index, anzahl });
        }
        if !(start..start + anzahl).all(|i| self.bit(i)) {
            return Err(VergabeFehler::NichtVergeben { index, anzahl });
        }
        for i in start..start + anzahl {
            self.loesche(i);
        }
        Ok(())
    }
}

/// **Was ein Treiber braucht, um seine Interrupts zu bekommen** — und der Beleg, dass die Einträge
/// stehen.
///
/// Kein loses Zahlentripel: das Ticket trägt Handle **und** Anzahl **und** Form zusammen, weil
/// nur alle drei gemeinsam sagen, was in die MSI-/MSI-X-Struktur des Geräts gehört. Dieselbe
/// Begründung wie bei `caprock_dma::DmaBuf`, der CPU- und Gerätesicht zusammenhält.
///
/// `#[must_use]`: ein weggeworfenes Ticket ist ein Block Tabellenplatz, den niemand mehr
/// freigeben kann — die Indizes stehen im Allokator und der Eintrag ist präsent.
#[must_use = "ein verworfenes MSI-Ticket laesst praesente IRTEs stehen, die niemand mehr freigibt"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MsiZiel {
    handle: u16,
    anzahl: usize,
    form: Vektorform,
    basis_vektor: u8,
}

impl MsiZiel {
    /// Der erste vergebene IRTE-Index.
    pub fn handle(&self) -> u16 {
        self.handle
    }
    pub fn anzahl(&self) -> usize {
        self.anzahl
    }
    pub fn form(&self) -> Vektorform {
        self.form
    }
    /// Der CPU-Vektor des ersten Eintrags; Eintrag `i` liegt auf `basis_vektor + i`.
    pub fn basis_vektor(&self) -> u8 {
        self.basis_vektor
    }

    /// **Was für Vektor `i` in die Gerätestruktur geschrieben wird**: `(Adresse, Datenwort,
    /// CPU-Vektor)`.
    ///
    /// * `MsiX` — je Zeile eine eigene Adresse mit eigenem Handle, Datenwort 0.
    /// * `Msi` — **eine** Adresse mit `SHV = 1`; der Treiber schreibt nur `eintrag(0)` in die
    ///   MSI-Capability, das Gerät erzeugt die übrigen Subhandles selbst. `eintrag(i)` sagt
    ///   trotzdem, was dann auf dem Bus steht — das ist die Grösse, gegen die man einen
    ///   Interrupt-Zähler prüft.
    pub fn eintrag(&self, i: usize) -> Option<(u32, u32, u8)> {
        if i >= self.anzahl {
            return None;
        }
        let vektor = (self.basis_vektor as usize).checked_add(i)?;
        if vektor > 0xFF {
            return None;
        }
        match self.form {
            Vektorform::MsiX => Some((
                msi_addr(self.handle.wrapping_add(i as u16)),
                msi_data(),
                vektor as u8,
            )),
            Vektorform::Msi => Some((msi_addr_shv(self.handle), i as u32, vektor as u8)),
        }
    }
}

/// **Der Wunsch eines Geräts.** Als eigener Typ, damit an der Aufrufstelle nicht fünf lose Zahlen
/// stehen, von denen zwei `u8`-artig aussehen und vertauschbar sind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vektorwunsch {
    /// Erster CPU-Vektor; Eintrag `i` bekommt `basis_vektor + i`.
    pub basis_vektor: u8,
    /// Ziel-LAPIC (xAPIC-Format, ≤ 255 ohne `EIME`).
    pub apic_id: u32,
    /// Die **BDF des Geräts** — das ist die Sicherheitsaussage, s. Modul-Doku zu `SVT`/`SID`.
    pub sid: u16,
    pub form: Vektorform,
    pub anzahl: usize,
}

/// **Der Zugriff auf die Tabelle** — als Trait, damit die REIHENFOLGE prüfbar ist.
///
/// Die Reihenfolge ist hier keine Stilfrage: solange `lo.P == 0` ist der Eintrag nicht vorhanden.
/// Wer `lo` zuerst schriebe, machte einen Eintrag gültig, dessen Quellprüfung (`hi`) noch nicht
/// darinsteht — für ein Zeitfenster dürfte **jedes** Gerät diesen Handle benutzen. Genau die Sorte
/// Fenster, die im Normalbetrieb nie auffällt.
///
/// Der Grund für das Trait ist die Prüfbarkeit: gegen einen aufzeichnenden Stellvertreter lässt
/// sich die Reihenfolge mit Literalen belegen, ohne Einheit, ohne QEMU und ohne Gerät. Derselbe
/// Weg wie bei `caprock-wait` und beim IPC-Modelltreue-Wächter.
pub trait IrtZugriff {
    /// Das obere Wort (Quellprüfung) schreiben.
    fn schreibe_hi(&mut self, index: u16, hi: u64);
    /// Das untere Wort (Präsenz, Vektor, Ziel) schreiben.
    fn schreibe_lo(&mut self, index: u16, lo: u64);
    /// Den Interrupt-Entry-Cache für **genau diesen** Index invalidieren. `false` = die Einheit
    /// hat nicht bestätigt.
    fn invalidiere(&mut self, index: u16) -> bool;
    /// **Zurücklesen**: `(lo, hi)` an `index`.
    ///
    /// Nicht bloss Bequemlichkeit: ein Schreiber, der sein eigenes Ergebnis bestätigt, bestätigt
    /// nichts (die Lehre aus der Dateisystem-PD und `tools/checkfat.py`). `vergib` sagt „ich habe
    /// geschrieben"; erst das Zurücklesen sagt, was **an der berechneten Adresse steht** — und
    /// genau die Adressarithmetik ist das, was der reinen Kodierung fehlt.
    fn lies(&self, index: u16) -> (u64, u64);
}

/// Prüft die Kodierbarkeit **aller** Einträge des Blocks, bevor irgendetwas reserviert wird.
///
/// **Warum vorher und nicht unterwegs:** eine Absage in der Mitte hinterliesse einen halb
/// geschriebenen Block, und der Rückbau davon ist der Pfad, den niemand testet. „Eine halbe
/// Zuteilung ist schlimmer als keine" steht bei `assign_driver_device` schon einmal, aus demselben
/// Grund.
fn pruefe_kodierbar(w: &Vektorwunsch) -> Result<(), VergabeFehler> {
    if w.apic_id > 0xFF {
        return Err(VergabeFehler::ApicIdZuGross { apic_id: w.apic_id });
    }
    let letzter = (w.basis_vektor as usize)
        .checked_add(w.anzahl.saturating_sub(1))
        .ok_or(VergabeFehler::VektorbereichLaeuftUeber {
            basis: w.basis_vektor,
            anzahl: w.anzahl,
        })?;
    if letzter > 0xFF {
        return Err(VergabeFehler::VektorbereichLaeuftUeber {
            basis: w.basis_vektor,
            anzahl: w.anzahl,
        });
    }
    if w.basis_vektor < 32 {
        return Err(VergabeFehler::VektorReserviert {
            vektor: w.basis_vektor as u32,
        });
    }
    Ok(())
}

/// Formbedingungen (s. [`Vektorform`]) — getrennt von der Kodierung, weil sie von der **Geräteart**
/// kommen und nicht von der Kodierung.
fn pruefe_form(w: &Vektorwunsch) -> Result<bool, VergabeFehler> {
    if w.anzahl == 0 {
        return Err(VergabeFehler::AnzahlNull);
    }
    match w.form {
        Vektorform::MsiX => Ok(false),
        Vektorform::Msi => {
            if !w.anzahl.is_power_of_two() {
                return Err(VergabeFehler::AnzahlKeineZweierpotenz { anzahl: w.anzahl });
            }
            if w.anzahl > 32 {
                return Err(VergabeFehler::ZuVieleMsiNachrichten { anzahl: w.anzahl });
            }
            Ok(true)
        }
    }
}

/// **Einen Vektorblock vergeben: reservieren, eintragen, invalidieren.**
///
/// `aktiv` ist die Zusage des Aufrufers, dass Interrupt Remapping läuft (`IRT` gesetzt, `IRE` an,
/// QI da). Sie steht als Argument und nicht als globale Abfrage, damit diese Funktion ohne
/// Hardware prüfbar bleibt — und damit der **`false`**-Fall genauso geprüft werden kann wie der
/// `true`-Fall.
///
/// Reihenfolge je Eintrag: `hi` (Quellprüfung) → `lo` (Präsenz) → Invalidierung. Scheitert eine
/// Invalidierung, werden **alle** bereits geschriebenen Einträge wieder auf `P = 0` gesetzt und
/// der Block freigegeben. Das ist nicht Kosmetik: ein präsenter Eintrag ohne bestätigte
/// Invalidierung ist ein Eintrag, von dem niemand weiss, ob die Einheit ihn sieht.
pub fn vergib<Z: IrtZugriff>(
    z: &mut Z,
    a: &mut IrteAllocator,
    aktiv: bool,
    w: &Vektorwunsch,
) -> Result<MsiZiel, VergabeFehler> {
    if !aktiv {
        return Err(VergabeFehler::TabelleNichtAktiv);
    }
    let ausgerichtet = pruefe_form(w)?;
    pruefe_kodierbar(w)?;
    let handle = a.reserviere(w.anzahl, ausgerichtet)?;

    for i in 0..w.anzahl {
        let index = handle + i as u16;
        let vektor = w.basis_vektor + i as u8;
        // `pruefe_kodierbar` hat den ganzen Block schon abgenommen; ein `None` hier wäre ein
        // Widerspruch zwischen beiden Stellen und darf nicht als „geht halt nicht" durchgehen.
        let Some(e) = irte_build(vektor, w.apic_id, w.sid) else {
            ruecknahme(z, a, handle, w.anzahl, i);
            return Err(VergabeFehler::VektorReserviert {
                vektor: vektor as u32,
            });
        };
        z.schreibe_hi(index, e.hi);
        z.schreibe_lo(index, e.lo);
        if !z.invalidiere(index) {
            ruecknahme(z, a, handle, w.anzahl, i + 1);
            return Err(VergabeFehler::IecInvalidierungFehlgeschlagen { index });
        }
    }
    Ok(MsiZiel {
        handle,
        anzahl: w.anzahl,
        form: w.form,
        basis_vektor: w.basis_vektor,
    })
}

/// Einen halb geschriebenen Block zurücknehmen: `geschrieben` Einträge auf `P = 0`, dann den
/// **ganzen reservierten** Block (`reserviert` Einträge) freigeben.
///
/// Zwei Zahlen, und sie sind verschieden: geschrieben wurde bis zur Fehlerstelle, reserviert war
/// von Anfang an der volle Block. Nur `geschrieben` freizugeben liesse den Rest für immer belegt —
/// eine Tabelle, die nach ein paar fehlgeschlagenen Vergaben voll ist und bei der niemand sagen
/// kann warum. (Die erste Fassung dieser Funktion hatte genau diesen Fehler und gab **gar nichts**
/// frei; gefunden hat ihn der Test `fehlgeschlagene_vergabe_leckt_keinen_tabellenplatz`.)
///
/// Die Invalidierung wird hier **versucht, aber nicht erzwungen** — sie ist gerade
/// fehlgeschlagen. Der belastbare Teil ist `lo = 0`: ein nicht vorhandener Eintrag wird von der
/// Einheit abgewiesen, sobald ihr Cache fällt.
fn ruecknahme<Z: IrtZugriff>(
    z: &mut Z,
    a: &mut IrteAllocator,
    handle: u16,
    reserviert: usize,
    geschrieben: usize,
) {
    for i in 0..geschrieben {
        let index = handle + i as u16;
        z.schreibe_lo(index, 0);
        let _ = z.invalidiere(index);
    }
    let _ = a.gib_frei(handle, reserviert);
}

/// **Einen vergebenen Block wieder einziehen** (Teardown, Hot-Reload).
///
/// Erst `P = 0`, dann invalidieren, dann freigeben — in dieser Reihenfolge. Umgekehrt gäbe es ein
/// Fenster, in dem der Index schon wieder vergeben werden darf, während die Einheit noch den alten
/// Eintrag zwischengespeichert hat: der Interrupt des alten Geräts landete dann beim neuen.
///
/// Schlägt die Invalidierung fehl, wird der Index **nicht** freigegeben und der Fehler benannt.
/// Ein verlorener Tabelleneintrag ist gegenüber einem fehlgeleiteten Interrupt das kleinere Übel —
/// dieselbe Abwägung wie bei den Regionen, deren Gerät die Stilllegung nicht bestätigt.
pub fn zieh_ein<Z: IrtZugriff>(
    z: &mut Z,
    a: &mut IrteAllocator,
    ziel: &MsiZiel,
) -> Result<(), VergabeFehler> {
    let mut ok = true;
    let mut erster_fehler = None;
    for i in 0..ziel.anzahl {
        let index = ziel.handle + i as u16;
        z.schreibe_lo(index, 0);
        if !z.invalidiere(index) {
            ok = false;
            if erster_fehler.is_none() {
                erster_fehler = Some(index);
            }
        }
    }
    if !ok {
        return Err(VergabeFehler::IecInvalidierungFehlgeschlagen {
            index: erster_fehler.unwrap_or(ziel.handle),
        });
    }
    a.gib_frei(ziel.handle, ziel.anzahl)
}

// ================================================================================================
// Der Selbsttest — hier, damit er auf dem HOST läuft
// ================================================================================================
//
// **Warum er nicht drüben in `vtd.rs` steht.** Ein Selbsttest, der nur auf einer bootenden
// Maschine läuft, ist genau so viel wert, wie diese Maschine bootet — und in dieser Sitzung
// bootete sie nicht (s. Bericht: `objcopy` 2.46.1). Die Logik des Tests (welche Aussagen, welche
// Reihenfolge, welche Absagen) ist reine Rechnung; nur `wo die Tabelle liegt` ist Hardware.
// Also steht die Logik hier und läuft gegen einen **speicherbasierten** Stellvertreter mit
// synthetischer Topologie, und `vtd.rs` instanziiert sie über die echte Tabelle.
//
// Das ist dieselbe Aufteilung wie bei `dmar.rs` und den RMRR-Gruppenfällen, die q35 mit seinen
// 0 RMRRs strukturell nicht zeigen kann.

/// Ergebnis des Vergabe-Selbsttests. **Ein Feld je Aussage**, nicht ein `bool` — sonst ist „rot"
/// eine Zahl ohne Diagnose.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct VergabeBericht {
    /// Waren die Vorbedingungen da? Ist das `false`, sagt **kein anderes Feld** etwas.
    pub sprechfaehig: bool,
    /// Eine Vergabe kam zustande.
    pub vergabe: bool,
    /// Das **untere** Wort steht an der berechneten Adresse (zurückgelesen).
    pub eintrag_steht: bool,
    /// Das **obere** Wort trägt `SVT = 01` und die SID (zurückgelesen).
    ///
    /// Getrennt von [`Self::eintrag_steht`], damit eine Mutation genau **ein** Konjunkt umlegt:
    /// verschluckt jemand die `hi`-Schreibung, fällt nur diese Zeile; verschluckt er `lo`, nur
    /// die andere. Zusammengefasst wären beide Gegenproben von einem Adressfehler nicht zu
    /// unterscheiden.
    pub quellpruefung: bool,
    /// Zwei Geräte: verschiedene Handles, verschiedene Adressen, verschiedene `hi`.
    pub getrennt: bool,
    /// Die MSI-Adresse trägt das Remappable-Bit (ohne es gibt es GAR KEINEN Interrupt, weil
    /// `GCMD.CFI = 0` das Compatibility-Format abgeschaltet hat).
    pub adresse_remappable: bool,
    /// Ein CPU-Ausnahmevektor wird abgewiesen — **und die Belegung wächst dabei nicht**.
    pub fail_closed: bool,
    /// Eine volle Tabelle wird **benannt** (`KeinFreierBlock` mit `frei = 0`), nicht still
    /// gekürzt.
    pub erschoepfung_benannt: bool,
    /// Der Einzug gibt den Index zurück **und** setzt den Eintrag auf `P = 0`.
    pub einzug: bool,
    /// Belegung am Ende — muss 0 sein, sonst hat der Selbsttest selbst Tabellenplatz verloren.
    pub belegt_am_ende: usize,
    /// Höchststand über den ganzen Test (Telemetrie).
    pub hoechststand: usize,
}

impl VergabeBericht {
    pub fn ok(&self) -> bool {
        self.sprechfaehig
            && self.vergabe
            && self.eintrag_steht
            && self.quellpruefung
            && self.getrennt
            && self.adresse_remappable
            && self.fail_closed
            && self.erschoepfung_benannt
            && self.einzug
            && self.belegt_am_ende == 0
    }
}

/// **Der Selbsttest der Vergabe.** `aktiv` wie bei [`vergib`].
///
/// `sid_a`/`sid_b` sind zwei **verschiedene** Quellen; ob dahinter echte Geräte stecken, ist für
/// die Aussage gleichgültig — die Einheit prüft die SID im Eintrag, nicht ob dort ein Gerät
/// steckt. (Ein echtes Gerät zu nehmen hiesse ausserdem, ihm im Hochlauf eine IRTE wegzunehmen.)
///
/// Der Test räumt hinter sich auf; bliebe etwas stehen, wäre es ein Block, den nachher ein Gerät
/// nicht bekommt — deshalb ist `belegt_am_ende` ein eigenes Feld und kein Kommentar.
pub fn selbsttest<Z: IrtZugriff>(
    z: &mut Z,
    a: &mut IrteAllocator,
    aktiv: bool,
    sid_a: u16,
    sid_b: u16,
    vektor: u8,
) -> VergabeBericht {
    let mut b = VergabeBericht {
        sprechfaehig: aktiv,
        ..Default::default()
    };
    if !aktiv {
        return b; // jede weitere Aussage wäre vakuum
    }

    let z_a = match vergib(
        z,
        a,
        aktiv,
        &Vektorwunsch {
            basis_vektor: vektor,
            apic_id: 0,
            sid: sid_a,
            form: Vektorform::MsiX,
            anzahl: 1,
        },
    ) {
        Ok(t) => t,
        Err(_) => return b,
    };
    b.vergabe = true;

    if let Some(e) = irte_build(vektor, 0, sid_a) {
        let (lo, hi) = z.lies(z_a.handle());
        b.eintrag_steht = lo == e.lo;
        b.quellpruefung = (hi >> 18) & 0b11 == SVT_SID && hi & 0xFFFF == sid_a as u64;
    }

    let (addr_a, _, _) = match z_a.eintrag(0) {
        Some(t) => t,
        None => return b,
    };
    b.adresse_remappable = addr_a & (1 << 3) != 0 && addr_a & 0xFFF0_0000 == 0xFEE0_0000;

    let z_b = match vergib(
        z,
        a,
        aktiv,
        &Vektorwunsch {
            basis_vektor: vektor,
            apic_id: 0,
            sid: sid_b,
            form: Vektorform::MsiX,
            anzahl: 1,
        },
    ) {
        Ok(t) => t,
        Err(_) => return b,
    };
    let (addr_b, _, _) = match z_b.eintrag(0) {
        Some(t) => t,
        None => return b,
    };
    b.getrennt = z_a.handle() != z_b.handle()
        && addr_a != addr_b
        && z.lies(z_a.handle()).1 != z.lies(z_b.handle()).1;

    // Fail-closed, **beide Hälften**: eine Absage, die trotzdem einen Index verbraucht, wäre ein
    // Leck in Höflichkeitsform.
    let vor = a.belegt_anzahl();
    let abgewiesen = matches!(
        vergib(
            z,
            a,
            aktiv,
            &Vektorwunsch {
                basis_vektor: 14,
                apic_id: 0,
                sid: sid_a,
                form: Vektorform::MsiX,
                anzahl: 1
            }
        ),
        Err(VergabeFehler::VektorReserviert { vektor: 14 })
    );
    b.fail_closed = abgewiesen && a.belegt_anzahl() == vor;

    // Erschöpfung: den Rest der Tabelle von Hand belegen (über `vergib` ginge es nicht — oberhalb
    // der CPU-Ausnahmen gibt es nur 224 Vektoren), dann muss die nächste Vergabe auf dem ECHTEN
    // Pfad **benannt** absagen.
    let frei = a.frei_anzahl();
    let rest = a.reserviere(frei, false).ok();
    b.erschoepfung_benannt = matches!(
        vergib(
            z,
            a,
            aktiv,
            &Vektorwunsch {
                basis_vektor: vektor,
                apic_id: 0,
                sid: sid_a,
                form: Vektorform::MsiX,
                anzahl: 1
            }
        ),
        Err(VergabeFehler::KeinFreierBlock {
            angefordert: 1,
            frei: 0,
            groesster_block: 0
        })
    );
    if let Some(h) = rest {
        let _ = a.gib_frei(h, frei);
    }

    let eingezogen = zieh_ein(z, a, &z_a).is_ok() && zieh_ein(z, a, &z_b).is_ok();
    b.einzug = eingezogen && z.lies(z_a.handle()).0 & 1 == 0;

    b.belegt_am_ende = a.belegt_anzahl();
    b.hoechststand = a.hoechststand();
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eintrag_ist_vorhanden_und_flankengetriggert() {
        let e = irte_build(0x40, 0, 0x0100).unwrap();
        assert_eq!(e.lo & 1, 1, "P muss gesetzt sein");
        assert_eq!(e.lo & (1 << 1), 0, "FPD aus -- Faults sollen gemeldet werden");
        assert_eq!(e.lo & (1 << 2), 0, "DM = physisch");
        assert_eq!(e.lo & (1 << 4), 0, "TM = flankengetriggert (MSI)");
        assert_eq!((e.lo >> 5) & 0b111, 0, "DLM = fixed, nicht lowest-priority");
    }

    #[test]
    fn vektor_und_ziel_stehen_an_ihrem_platz() {
        let e = irte_build(0x42, 3, 0).unwrap();
        assert_eq!((e.lo >> 16) & 0xFF, 0x42);
        assert_eq!((e.lo >> 40) & 0xFF, 3);
    }

    #[test]
    fn quellpruefung_ist_eingeschaltet() {
        // **Die Sicherheitsaussage dieser Datei.** Ohne SVT=01 duerfte jedes Geraet jeden
        // Handle benutzen -- und die PD schreibt ihre MSI-X-Tabelle selbst.
        let sid = sid_from_bdf(0x00, 0x1f, 3).unwrap();
        let e = irte_build(0x40, 0, sid).unwrap();
        assert_eq!((e.hi >> 18) & 0b11, SVT_SID);
        assert_eq!((e.hi >> 16) & 0b11, SQ_ALLE_16);
        assert_eq!(e.hi & 0xFFFF, sid as u64);
    }

    #[test]
    fn bdf_wird_richtig_gepackt() {
        assert_eq!(sid_from_bdf(0, 0, 0), Some(0x0000));
        assert_eq!(sid_from_bdf(0, 1, 0), Some(0x0008));
        assert_eq!(sid_from_bdf(0, 0x1f, 7), Some(0x00FF));
        assert_eq!(sid_from_bdf(0x12, 3, 1), Some(0x1219));
        // Abweisen statt abschneiden: eine ueberlaufende Geraetenummer wuerde in die Busnummer
        // hineinlaufen und den Eintrag einem FREMDEN Geraet zuordnen.
        assert_eq!(sid_from_bdf(0, 32, 0), None);
        assert_eq!(sid_from_bdf(0, 0, 8), None);
    }

    #[test]
    fn cpu_ausnahmevektoren_werden_abgewiesen() {
        // Ein Geraet auf Vektor 14 saehe fuer den Kernel wie ein Seitenfehler aus.
        assert!(irte_build(14, 0, 0).is_none());
        assert!(irte_build(31, 0, 0).is_none());
        assert!(irte_build(32, 0, 0).is_some());
    }

    #[test]
    fn zu_grosse_apic_id_wird_abgewiesen_statt_abgeschnitten() {
        // Abgeschnitten zeigte sie auf einen ANDEREN Kern -- der Interrupt kaeme still am
        // falschen Ort an, und das sieht aus wie Erfolg.
        assert!(irte_build(0x40, 0x100, 0).is_none());
        assert!(irte_build(0x40, 0xFF, 0).is_some());
    }

    #[test]
    fn msi_adresse_traegt_das_remappable_bit() {
        // Ohne Bit 3 ist die Nachricht im Compatibility-Format -- und das hat der Bring-up
        // ueber CFI abgeschaltet. Ergebnis waere GAR KEIN Interrupt, nicht ein falscher.
        assert_eq!(msi_addr(0) & (1 << 3), 1 << 3);
        assert_eq!(msi_addr(0) & 0xFFF0_0000, 0xFEE0_0000);
    }

    #[test]
    fn handle_wird_geteilt_wie_die_spezifikation_es_verlangt() {
        // Bits 14:0 nach 19:5, Bit 15 nach Bit 2. Die zweite Haelfte wird beim Schreiben von Hand
        // gern vergessen -- und faellt erst ab Handle 32768 auf.
        assert_eq!((msi_addr(1) >> 5) & 0x7FFF, 1);
        assert_eq!((msi_addr(0x7FFF) >> 5) & 0x7FFF, 0x7FFF);
        assert_eq!(msi_addr(0x7FFF) & (1 << 2), 0);
        assert_eq!(msi_addr(0x8000) & (1 << 2), 1 << 2);
        assert_eq!((msi_addr(0x8000) >> 5) & 0x7FFF, 0);
        assert_eq!((msi_addr(0xFFFF) >> 5) & 0x7FFF, 0x7FFF);
        assert_eq!(msi_addr(0xFFFF) & (1 << 2), 1 << 2);
    }

    #[test]
    fn shv_ist_aus_und_das_datenwort_ist_null() {
        assert_eq!(msi_addr(7) & (1 << 4), 0);
        // Vektor und Ziel stehen in der IRTE. Wer hier den Vektor hineinschriebe, erzeugte einen
        // Sub-Handle, den niemand vergeben hat.
        assert_eq!(msi_data(), 0);
    }

    #[test]
    fn zwei_eintraege_fuer_verschiedene_geraete_sind_unterscheidbar() {
        // Die Aussage, auf der die Isolation ruht: gleicher Vektor, gleicher Kern, aber
        // verschiedene Quellen -> verschiedene `hi`.
        let a = irte_build(0x40, 0, sid_from_bdf(0, 4, 0).unwrap()).unwrap();
        let b = irte_build(0x40, 0, sid_from_bdf(0, 5, 0).unwrap()).unwrap();
        assert_eq!(a.lo, b.lo);
        assert_ne!(a.hi, b.hi);
    }

    // ============================================================================================
    // Die VERGABE
    // ============================================================================================

    /// **Der aufzeichnende Stellvertreter.** Er merkt sich, WAS in welcher REIHENFOLGE passiert
    /// ist -- das ist die einzige Art, eine Reihenfolgezusicherung zu pruefen, ohne eine Einheit
    /// zu haben. `invalidierung_faellt_ab` laesst die Invalidierung ab einem bestimmten Aufruf
    /// scheitern; damit ist der Rueckbaupfad ausloesbar, den sonst niemand je faehrt.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Op {
        Hi(u16, u64),
        Lo(u16, u64),
        Inv(u16),
    }

    /// Welche Schreibung der Stellvertreter **verschluckt** — die Gegenproben. Jede legt genau
    /// EIN Konjunkt des Berichts um; das ist der Grund, warum `eintrag_steht` (lo) und
    /// `quellpruefung` (hi) getrennte Felder sind.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Defekt {
        Keiner,
        /// `hi` kommt nie in der Tabelle an -> nur `quellpruefung` muss fallen.
        HiVerschluckt,
        /// `lo` kommt nie an -> nur `eintrag_steht` muss fallen.
        LoVerschluckt,
        /// **Falsche Adressarithmetik**: geschrieben wird an `index + 1`.
        ///
        /// Die erste Fassung war `index / 2` („Schrittweite 8 statt 16 Byte") -- und die hat
        /// **nicht angeschlagen**, weil `0 / 2 == 0` ist: der erste Eintrag landete an genau
        /// derselben Stelle, und der Selbsttest fasst nur den ersten an. Das ist die Falle
        /// „eine Gegenprobe, die die geprüfte Sache gar nicht erreichen KANN" -- dieselbe Form
        /// wie die nicht erfuellbare `pdcolor`-Gegenprobe. Ein konstanter Versatz verschiebt
        /// **jeden** Index.
        ///
        /// Legt `eintrag_steht` UND `quellpruefung` zugleich um -- die Kopplung ist
        /// **strukturell**: eine falsche Adresse trifft beide Worte eines Eintrags.
        VersetzterPlatz,
    }

    struct Sicht {
        ops: [Op; 64],
        n: usize,
        invalidierung_faellt_ab: usize, // usize::MAX = nie
        inv_zaehler: usize,
        /// **Die Tabelle als Speicher** — synthetische Topologie statt Einheit. `IRT_EINTRAEGE`
        /// Eintraege zu je zwei Worten, wie im echten 4-KiB-Frame.
        tabelle: [[u64; 2]; IRT_EINTRAEGE],
        defekt: Defekt,
    }

    impl Sicht {
        fn neu() -> Sicht {
            Sicht {
                ops: [Op::Inv(0xFFFF); 64],
                n: 0,
                invalidierung_faellt_ab: usize::MAX,
                inv_zaehler: 0,
                tabelle: [[0; 2]; IRT_EINTRAEGE],
                defekt: Defekt::Keiner,
            }
        }
        fn mit_defekt(d: Defekt) -> Sicht {
            let mut s = Sicht::neu();
            s.defekt = d;
            s
        }
        /// Der Platz, an den GESCHRIEBEN wird (die Fehlstelle sitzt hier, nicht im Lesen —
        /// sonst waere die Gegenprobe eine ueber den Leser).
        fn schreibplatz(&self, index: u16) -> usize {
            match self.defekt {
                Defekt::VersetzterPlatz => (index as usize + 1) % IRT_EINTRAEGE,
                _ => index as usize % IRT_EINTRAEGE,
            }
        }
        fn merke(&mut self, o: Op) {
            if self.n < self.ops.len() {
                self.ops[self.n] = o;
                self.n += 1;
            }
        }
        fn ops(&self) -> &[Op] {
            &self.ops[..self.n]
        }
        /// Position der ersten Operation, die `f` erfuellt.
        fn pos(&self, f: impl Fn(&Op) -> bool) -> Option<usize> {
            self.ops().iter().position(f)
        }
        /// Letzter geschriebener `lo`-Wert eines Index (`None` = nie geschrieben).
        fn lo_von(&self, index: u16) -> Option<u64> {
            let mut v = None;
            for o in self.ops() {
                if let Op::Lo(i, w) = *o {
                    if i == index {
                        v = Some(w);
                    }
                }
            }
            v
        }
    }

    impl IrtZugriff for Sicht {
        fn schreibe_hi(&mut self, index: u16, hi: u64) {
            self.merke(Op::Hi(index, hi));
            if self.defekt != Defekt::HiVerschluckt {
                let p = self.schreibplatz(index);
                self.tabelle[p][1] = hi;
            }
        }
        fn schreibe_lo(&mut self, index: u16, lo: u64) {
            self.merke(Op::Lo(index, lo));
            if self.defekt != Defekt::LoVerschluckt {
                let p = self.schreibplatz(index);
                self.tabelle[p][0] = lo;
            }
        }
        fn invalidiere(&mut self, index: u16) -> bool {
            self.merke(Op::Inv(index));
            self.inv_zaehler += 1;
            self.inv_zaehler <= self.invalidierung_faellt_ab
        }
        fn lies(&self, index: u16) -> (u64, u64) {
            let e = self.tabelle[index as usize % IRT_EINTRAEGE];
            (e[0], e[1])
        }
    }

    fn wunsch(anzahl: usize, form: Vektorform) -> Vektorwunsch {
        Vektorwunsch {
            basis_vektor: 0x40,
            apic_id: 0,
            sid: sid_from_bdf(0, 4, 0).unwrap(),
            form,
            anzahl,
        }
    }

    #[test]
    fn groessenfeld_passt_zur_tabellengroesse() {
        // **Zwei Zahlen aus derselben Hand sind keine zwei Quellen** -- aber zwei Zahlen, die
        // dieselbe Sache beschreiben und auseinanderlaufen koennen, sind ein Loch. `IRTA.S` sagt
        // der Einheit `2^(S+1)`; steht dort etwas anderes als die Zahl, ueber die der Allokator
        // vergibt, liest die Einheit an Eintraegen vorbei oder ueber das Ende hinaus.
        assert_eq!(1usize << (IRTA_GROESSENFELD + 1), IRT_EINTRAEGE);
    }

    #[test]
    fn reihenfolge_hi_vor_lo_je_eintrag() {
        // **Die Reihenfolgezusicherung dieser Datei.** Solange `lo.P == 0` ist der Eintrag nicht
        // vorhanden; wer `lo` zuerst schriebe, machte ihn gueltig, BEVOR die Quellpruefung
        // darinsteht -- fuer dieses Fenster duerfte jedes Geraet den Handle benutzen.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let ziel = vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).unwrap();
        let h = ziel.handle();
        let p_hi = z.pos(|o| matches!(o, Op::Hi(i, _) if *i == h)).unwrap();
        let p_lo = z.pos(|o| matches!(o, Op::Lo(i, _) if *i == h)).unwrap();
        let p_inv = z.pos(|o| matches!(o, Op::Inv(i) if *i == h)).unwrap();
        assert!(p_hi < p_lo, "hi muss vor lo stehen");
        assert!(p_lo < p_inv, "invalidiert wird NACH dem Schreiben");
    }

    #[test]
    fn ohne_aktives_remapping_gibt_es_keinen_eintrag() {
        // Fail-closed und OHNE Rueckfall: das Compatibility-Format hat der Bring-up mit
        // `GCMD.CFI = 0` abgeschaltet, ein Rueckfall darauf umginge die ganze Tabelle.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        assert_eq!(
            vergib(&mut z, &mut a, false, &wunsch(1, Vektorform::MsiX)).unwrap_err(),
            VergabeFehler::TabelleNichtAktiv
        );
        assert_eq!(z.ops().len(), 0, "es darf NICHTS geschrieben worden sein");
        assert_eq!(a.belegt_anzahl(), 0, "und nichts reserviert");
    }

    #[test]
    fn volle_tabelle_wird_benannt_statt_still_gekuerzt() {
        // **Die Absage, um die es geht.** `None` waere hier zu wenig: der Aufrufer muss „voll"
        // von „zerstueckelt" unterscheiden koennen, weil die Abhilfen verschieden sind.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        for _ in 0..IRT_EINTRAEGE {
            assert!(vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).is_ok());
        }
        let e = vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).unwrap_err();
        assert_eq!(
            e,
            VergabeFehler::KeinFreierBlock {
                angefordert: 1,
                frei: 0,
                groesster_block: 0
            }
        );
    }

    #[test]
    fn zerstueckelt_und_voll_sind_unterscheidbar() {
        // Frei ist reichlich, zusammenhaengend nichts Grosses -- die zweite Lage, und sie sagt
        // etwas anderes als „voll".
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        for _ in 0..IRT_EINTRAEGE {
            let _ = vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).unwrap();
        }
        // Jeden zweiten wieder hergeben -> 128 frei, groesster Block 1.
        for i in (0..IRT_EINTRAEGE as u16).step_by(2) {
            a.gib_frei(i, 1).unwrap();
        }
        let e = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::MsiX)).unwrap_err();
        assert_eq!(
            e,
            VergabeFehler::KeinFreierBlock {
                angefordert: 4,
                frei: 128,
                groesster_block: 1
            }
        );
    }

    #[test]
    fn mehr_als_die_tabelle_ist_kein_erschoepfungsfall() {
        // „passt nie" gegen „passt gerade nicht": der erste ist ein Programmfehler des Aufrufers,
        // der zweite ein Betriebszustand.
        let mut a = IrteAllocator::new();
        assert_eq!(
            a.reserviere(IRT_EINTRAEGE + 1, false).unwrap_err(),
            VergabeFehler::GroesserAlsTabelle {
                anzahl: IRT_EINTRAEGE + 1,
                kapazitaet: IRT_EINTRAEGE
            }
        );
    }

    #[test]
    fn nicht_die_tabelle_begrenzt_einen_block_sondern_der_vektorraum() {
        // **Ein Befund, den erst der Test gezeigt hat.** Die erste Fassung dieser Zeile erwartete
        // fuer 257 Vektoren `GroesserAlsTabelle` -- sie bekam `VektorbereichLaeuftUeber`. Beide
        // Aussagen sind wahr, aber die zweite kommt zuerst, und zwar strukturell:
        //
        //   Tabelle:    256 Eintraege
        //   Vektorraum: 224 (32..=255; darunter liegen die CPU-Ausnahmen)
        //
        // Ein Block braucht AUFEINANDERFOLGENDE Vektoren auf EINEM Kern. Damit ist
        // `GroesserAlsTabelle` ueber `vergib` gar nicht erreichbar -- die groesste Anzahl, die
        // `pruefe_kodierbar` passiert, ist 224. Die Tabellengrenze bindet erst ueber MEHRERE
        // Geraete hinweg (jedes mit eigenem Block, ggf. auf verschiedenen Kernen mit denselben
        // Vektornummern).
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        assert_eq!(
            vergib(&mut z, &mut a, true, &wunsch(IRT_EINTRAEGE + 1, Vektorform::MsiX)).unwrap_err(),
            VergabeFehler::VektorbereichLaeuftUeber {
                basis: 0x40,
                anzahl: IRT_EINTRAEGE + 1
            }
        );
        // Und die Zahl selbst: ab Vektor 32 sind genau 224 Eintraege eines Blocks kodierbar.
        let mut w = wunsch(224, Vektorform::MsiX);
        w.basis_vektor = 32;
        assert!(vergib(&mut z, &mut a, true, &w).is_ok());
        let mut w = wunsch(225, Vektorform::MsiX);
        w.basis_vektor = 32;
        assert!(matches!(
            vergib(&mut z, &mut a, true, &w).unwrap_err(),
            VergabeFehler::VektorbereichLaeuftUeber { .. }
        ));
    }

    #[test]
    fn msi_verlangt_eine_zweierpotenz_msix_nicht() {
        // **Die Stelle, an der die GERAETEART die Bedingung bestimmt.** `MME` in der
        // MSI-Capability kennt nur 1/2/4/8/16/32; MSI-X hat je Vektor eine Zeile.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        assert_eq!(
            vergib(&mut z, &mut a, true, &wunsch(3, Vektorform::Msi)).unwrap_err(),
            VergabeFehler::AnzahlKeineZweierpotenz { anzahl: 3 }
        );
        assert!(vergib(&mut z, &mut a, true, &wunsch(3, Vektorform::MsiX)).is_ok());
        assert_eq!(
            vergib(&mut z, &mut a, true, &wunsch(64, Vektorform::Msi)).unwrap_err(),
            VergabeFehler::ZuVieleMsiNachrichten { anzahl: 64 }
        );
    }

    #[test]
    fn msi_bekommt_einen_ausgerichteten_block() {
        // Bewusste Verschaerfung (s. `Vektorform::Msi`): der erste MSI-X-Vektor liegt auf 0,
        // ein MSI-Viererblock danach also NICHT auf 1, sondern auf 4.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let _ = vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).unwrap();
        let m = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::Msi)).unwrap();
        assert_eq!(m.handle() % 4, 0);
        assert_eq!(m.handle(), 4);
    }

    #[test]
    fn msix_darf_unausgerichtet_liegen() {
        // Die Gegenprobe zur Zeile davor: ohne die Formunterscheidung wuerde MSI-X denselben
        // Sprung machen und Tabellenplatz verschenken.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let _ = vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).unwrap();
        let m = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::MsiX)).unwrap();
        assert_eq!(m.handle(), 1);
    }

    #[test]
    fn jeder_eintrag_des_blocks_traegt_die_quellpruefung() {
        // **Die Sicherheitsaussage ueber den ganzen Block, nicht nur ueber den ersten Eintrag.**
        // Ein Block, dessen zweiter Eintrag ohne SID stuende, waere von jedem Geraet benutzbar --
        // und ein Test, der nur `handle()` ansieht, saehe das nie.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let w = wunsch(4, Vektorform::Msi);
        let m = vergib(&mut z, &mut a, true, &w).unwrap();
        for i in 0..4u16 {
            let idx = m.handle() + i;
            let hi = z
                .ops()
                .iter()
                .find_map(|o| match o {
                    Op::Hi(j, v) if *j == idx => Some(*v),
                    _ => None,
                })
                .expect("jeder Eintrag braucht ein hi");
            assert_eq!(hi & 0xFFFF, w.sid as u64, "SID im Eintrag {idx}");
            assert_eq!((hi >> 18) & 0b11, SVT_SID, "SVT im Eintrag {idx}");
        }
    }

    #[test]
    fn jeder_eintrag_bekommt_seinen_eigenen_vektor() {
        // Ein Block mit viermal demselben Vektor sieht aus wie ein Treiber, der seine
        // Warteschlangen nicht auseinanderhaelt.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let m = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::Msi)).unwrap();
        for i in 0..4usize {
            let idx = m.handle() + i as u16;
            let lo = z.lo_von(idx).unwrap();
            assert_eq!((lo >> 16) & 0xFF, 0x40 + i as u64);
        }
    }

    #[test]
    fn fehlgeschlagene_vergabe_leckt_keinen_tabellenplatz() {
        // **Der Test, der den Fehler in der ersten Fassung von `ruecknahme` gefunden hat:** sie
        // gab GAR NICHTS frei. Nach ein paar fehlgeschlagenen Vergaben waere die Tabelle voll
        // gewesen, ohne dass ein einziges Geraet einen Vektor hat.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        z.invalidierung_faellt_ab = 1; // die ZWEITE Invalidierung scheitert
        let e = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::Msi)).unwrap_err();
        assert!(matches!(e, VergabeFehler::IecInvalidierungFehlgeschlagen { .. }));
        assert_eq!(a.belegt_anzahl(), 0, "der reservierte Block muss zurueck sein");
    }

    #[test]
    fn fehlgeschlagene_vergabe_laesst_keinen_praesenten_eintrag_stehen() {
        // Getrennt von der Zeile davor, weil es eine ANDERE Aussage ist: der Tabellenplatz kann
        // zurueck sein, waehrend in der Tabelle noch ein praesenter Eintrag steht.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        z.invalidierung_faellt_ab = 1;
        let _ = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::Msi)).unwrap_err();
        for i in 0..2u16 {
            assert_eq!(z.lo_von(i), Some(0), "Eintrag {i} muss auf P=0 zurueck");
        }
    }

    #[test]
    fn einziehen_loescht_erst_und_gibt_dann_frei() {
        // Umgekehrt gaebe es ein Fenster, in dem der Index schon wieder vergeben werden darf,
        // waehrend die Einheit den alten Eintrag noch zwischengespeichert hat.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let m = vergib(&mut z, &mut a, true, &wunsch(2, Vektorform::Msi)).unwrap();
        assert_eq!(a.belegt_anzahl(), 2);
        zieh_ein(&mut z, &mut a, &m).unwrap();
        assert_eq!(a.belegt_anzahl(), 0);
        for i in 0..2u16 {
            assert_eq!(z.lo_von(m.handle() + i), Some(0));
        }
    }

    #[test]
    fn einziehen_ohne_bestaetigte_invalidierung_sperrt_den_index() {
        // Ein verlorener Tabelleneintrag ist gegenueber einem fehlgeleiteten Interrupt das
        // kleinere Uebel -- dieselbe Abwaegung wie bei einer Region ohne bestaetigte Stilllegung.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let m = vergib(&mut z, &mut a, true, &wunsch(1, Vektorform::MsiX)).unwrap();
        z.invalidierung_faellt_ab = z.inv_zaehler; // ab jetzt scheitert jede
        assert!(matches!(
            zieh_ein(&mut z, &mut a, &m).unwrap_err(),
            VergabeFehler::IecInvalidierungFehlgeschlagen { .. }
        ));
        assert!(a.ist_belegt(m.handle()), "der Index bleibt gesperrt");
    }

    #[test]
    fn doppelte_freigabe_wird_abgewiesen() {
        // Sonst gaebe eine doppelte Freigabe einen Index frei, den ein anderes Geraet haelt --
        // und dessen Interrupt landete danach beim falschen Treiber.
        let mut a = IrteAllocator::new();
        let h = a.reserviere(2, false).unwrap();
        assert!(a.gib_frei(h, 2).is_ok());
        assert_eq!(
            a.gib_frei(h, 2).unwrap_err(),
            VergabeFehler::NichtVergeben { index: h, anzahl: 2 }
        );
    }

    #[test]
    fn teilweise_freigabe_eines_fremden_blocks_wird_abgewiesen() {
        let mut a = IrteAllocator::new();
        let h = a.reserviere(2, false).unwrap();
        // 2..4 gehoert niemandem -> die Freigabe von 1..3 ist halb falsch und muss ganz fallen.
        assert_eq!(
            a.gib_frei(h + 1, 2).unwrap_err(),
            VergabeFehler::NichtVergeben {
                index: h + 1,
                anzahl: 2
            }
        );
        assert!(a.ist_belegt(h + 1), "nichts darf halb freigegeben sein");
    }

    #[test]
    fn cpu_ausnahmevektoren_werden_auch_in_der_vergabe_abgewiesen() {
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let mut w = wunsch(1, Vektorform::MsiX);
        w.basis_vektor = 14;
        assert_eq!(
            vergib(&mut z, &mut a, true, &w).unwrap_err(),
            VergabeFehler::VektorReserviert { vektor: 14 }
        );
        assert_eq!(a.belegt_anzahl(), 0);
    }

    #[test]
    fn ein_block_der_ueber_vektor_255_liefe_wird_ganz_abgewiesen() {
        // **Vorher pruefen, nicht unterwegs.** Ein Block, dessen dritter Eintrag nicht mehr
        // kodierbar ist, duerfte nicht mit zwei geschriebenen Eintraegen dastehen.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let mut w = wunsch(4, Vektorform::Msi);
        w.basis_vektor = 254;
        assert_eq!(
            vergib(&mut z, &mut a, true, &w).unwrap_err(),
            VergabeFehler::VektorbereichLaeuftUeber {
                basis: 254,
                anzahl: 4
            }
        );
        assert_eq!(z.ops().len(), 0, "kein halb geschriebener Block");
        assert_eq!(a.belegt_anzahl(), 0);
    }

    #[test]
    fn zu_grosse_apic_id_wird_auch_in_der_vergabe_abgewiesen() {
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let mut w = wunsch(1, Vektorform::MsiX);
        w.apic_id = 0x100;
        assert_eq!(
            vergib(&mut z, &mut a, true, &w).unwrap_err(),
            VergabeFehler::ApicIdZuGross { apic_id: 0x100 }
        );
        assert_eq!(a.belegt_anzahl(), 0);
    }

    #[test]
    fn msix_ticket_gibt_je_vektor_eine_eigene_adresse() {
        // MSI-X: `anzahl` Tabellenzeilen, `anzahl` Handles, `anzahl` Adressen.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let m = vergib(&mut z, &mut a, true, &wunsch(3, Vektorform::MsiX)).unwrap();
        let (a0, d0, v0) = m.eintrag(0).unwrap();
        let (a1, d1, v1) = m.eintrag(1).unwrap();
        assert_ne!(a0, a1, "MSI-X: je Zeile eine eigene Adresse");
        assert_eq!((d0, d1), (0, 0), "MSI-X: Datenwort ist 0 (SHV=0)");
        assert_eq!(a0 & (1 << 4), 0, "MSI-X: SHV muss AUS sein");
        assert_eq!((v0, v1), (0x40, 0x41));
        assert!(m.eintrag(3).is_none(), "ueber den Block hinaus gibt es nichts");
    }

    #[test]
    fn msi_ticket_gibt_eine_adresse_und_zaehlt_im_datenwort() {
        // MSI: EINE Adresse mit SHV=1, das Geraet zaehlt den Subhandle hoch.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let m = vergib(&mut z, &mut a, true, &wunsch(4, Vektorform::Msi)).unwrap();
        let (a0, d0, _) = m.eintrag(0).unwrap();
        let (a1, d1, _) = m.eintrag(1).unwrap();
        assert_eq!(a0, a1, "MSI: dieselbe Adresse fuer alle Nachrichten");
        assert_eq!(a0 & (1 << 4), 1 << 4, "MSI mit mehreren Nachrichten braucht SHV=1");
        assert_eq!((d0, d1), (0, 1), "der Subhandle zaehlt im Datenwort");
    }

    #[test]
    fn zwei_geraete_bekommen_verschiedene_handles() {
        // Die Aussage, die die ganze Vergabe traegt: kein Index geht zweimal weg.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let mut w1 = wunsch(1, Vektorform::MsiX);
        w1.sid = sid_from_bdf(0, 4, 0).unwrap();
        let mut w2 = wunsch(1, Vektorform::MsiX);
        w2.sid = sid_from_bdf(0, 5, 0).unwrap();
        let m1 = vergib(&mut z, &mut a, true, &w1).unwrap();
        let m2 = vergib(&mut z, &mut a, true, &w2).unwrap();
        assert_ne!(m1.handle(), m2.handle());
        assert_ne!(m1.eintrag(0).unwrap().0, m2.eintrag(0).unwrap().0);
    }

    // --- Der Selbsttest gegen eine synthetische Tabelle ----------------------------------------

    fn st(z: &mut Sicht) -> VergabeBericht {
        let mut a = IrteAllocator::new();
        selbsttest(
            z,
            &mut a,
            true,
            sid_from_bdf(0, 0x1e, 0).unwrap(),
            sid_from_bdf(0, 0x1e, 1).unwrap(),
            0x70,
        )
    }

    #[test]
    fn selbsttest_ist_gruen_an_einer_heilen_tabelle() {
        // **Die Positivkontrolle.** Ohne sie belegen die Gegenproben unten nichts: waere der
        // Bericht schon unmutiert rot, waere jede Mutation ein Scheinbeleg.
        let mut z = Sicht::neu();
        let b = st(&mut z);
        assert!(b.ok(), "{b:?}");
        assert_eq!(b.belegt_am_ende, 0);
        assert!(b.hoechststand >= IRT_EINTRAEGE, "die Erschoepfung muss wirklich gefuellt haben");
    }

    #[test]
    fn ohne_vorbedingungen_meldet_der_selbsttest_nichts_statt_gruen() {
        // Ein leerer Lauf ist kein Testergebnis. `sprechfaehig=false` muss `ok()` kippen, sonst
        // waere eine Maschine ohne Interrupt Remapping stillschweigend „bestanden".
        let mut z = Sicht::neu();
        let mut a = IrteAllocator::new();
        let b = selbsttest(&mut z, &mut a, false, 0x00f0, 0x00f1, 0x70);
        assert!(!b.ok());
        assert!(!b.sprechfaehig);
        assert!(!b.vergabe, "ohne Vorbedingungen darf nichts vergeben worden sein");
    }

    #[test]
    fn gegenprobe_hi_verschluckt_legt_genau_die_quellpruefung_um() {
        // Die Sicherheitsaussage: ohne `hi` traegt der Eintrag keine Quellpruefung, und JEDES
        // Geraet duerfte den Handle benutzen.
        let mut z = Sicht::mit_defekt(Defekt::HiVerschluckt);
        let b = st(&mut z);
        assert!(!b.quellpruefung, "die zustaendige Zeile muss fallen");
        assert!(b.eintrag_steht, "und NUR sie");
        // `getrennt` haengt mit an `hi` -- zwei Quellen unterscheiden sich GENAU dort. Das ist
        // strukturell und wird deshalb hier ausdruecklich NICHT als eigenes Konjunkt behauptet
        // (eine Behauptung, die nur durch eine Kopplung wahr wird, ist keine).
        assert!(b.adresse_remappable && b.fail_closed && b.erschoepfung_benannt && b.einzug);
    }

    #[test]
    fn gegenprobe_lo_verschluckt_legt_genau_den_eintrag_um() {
        let mut z = Sicht::mit_defekt(Defekt::LoVerschluckt);
        let b = st(&mut z);
        assert!(!b.eintrag_steht, "die zustaendige Zeile muss fallen");
        assert!(b.quellpruefung, "und NUR sie");
        assert!(b.getrennt, "zwei Quellen bleiben ueber `hi` unterscheidbar");
        assert!(b.adresse_remappable && b.fail_closed && b.erschoepfung_benannt);
    }

    #[test]
    fn gegenprobe_versetzter_platz_faellt_an_beiden_worten() {
        // **Eine Kopplung, die ich nicht aufloese, sondern benenne:** eine falsche Adresse trifft
        // BEIDE Worte eines Eintrags. Es gibt keine Mutation der Adressarithmetik, die nur eines
        // davon verschiebt -- wer hier „genau ein Konjunkt" verlangte, verlangte etwas
        // Unmoegliches.
        let mut z = Sicht::mit_defekt(Defekt::VersetzterPlatz);
        let b = st(&mut z);
        assert!(!b.ok());
        assert!(!b.eintrag_steht || !b.quellpruefung);
    }

    #[test]
    fn hoechststand_faellt_beim_freigeben_nicht() {
        // Die Zahl, mit der sich eine Tabellengroesse begruenden laesst, statt sie zu raten.
        let (mut z, mut a) = (Sicht::neu(), IrteAllocator::new());
        let m = vergib(&mut z, &mut a, true, &wunsch(8, Vektorform::Msi)).unwrap();
        assert_eq!(a.hoechststand(), 8);
        zieh_ein(&mut z, &mut a, &m).unwrap();
        assert_eq!(a.belegt_anzahl(), 0);
        assert_eq!(a.hoechststand(), 8);
    }
}
