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

// Die Crate ist `no_std` (sie laeuft in einer Treiber-PD ohne Betriebssystem). Die
// DMA-Arithmetik ist aber reine Rechnung und damit auf dem Host pruefbar — der Testharness
// braucht dafuer `std`. Nur unter `cfg(test)`, der PD-Build sieht davon nichts.
#[cfg(test)]
extern crate std;

/// **Grosse, zusammenhängende DMA** (Z26, Vorbedingung 2): die benannte Absage und die
/// Achsenprüfung. Eigene Datei aus demselben Grund wie `irte.rs` in der HAL — reine
/// Grössenarithmetik, mit Literalen auslösbar.
pub mod gross;

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

/// **Die Stromrichtung eines einzelnen DMA-Auftrags** (Gegenstück zu `enum dma_data_direction`).
///
/// Auf dieser Maschine ist die Richtung für die Abbildung **bedeutungslos**: der Pool ist
/// kohärent abgebildet, CPU- und Gerätesicht zeigen auf denselben Speicher ohne Zwischencache.
/// Die Richtung steht trotzdem im Typ, damit ein automatisch portierter Treiber seine
/// `DMA_TO_DEVICE`/`DMA_FROM_DEVICE`/`DMA_BIDIRECTIONAL`-Stelle 1:1 wiederfindet, statt sie beim
/// Portieren zu verlieren — und damit eine künftige nicht-kohärente Abbildung sie auswerten kann,
/// ohne alle Aufrufer anzufassen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StromRichtung {
    /// CPU schreibt, Gerät liest (`DMA_TO_DEVICE`).
    NachGeraet,
    /// Gerät schreibt, CPU liest (`DMA_FROM_DEVICE`).
    VomGeraet,
    /// Beide Richtungen (`DMA_BIDIRECTIONAL`).
    Beide,
}

/// **Das Gegenstück zu `dma_map_single` je Anfrage — reine Arithmetik, null Syscalls.**
///
/// Ein Alias über [`DmaPool::map`]: die CPU-Adresse muss im Pool liegen, sonst `None` (s. dort).
/// `dir` wird heute **nicht ausgewertet** (kohärent auf x86) und ist trotzdem Pflicht, damit der
/// Aufruf beim Portieren seine Richtung behält.
pub fn map_single(pool: &DmaPool, cpu: u64, len: u64, dir: StromRichtung) -> Option<DmaBuf> {
    let _ = dir;
    pool.map(cpu, len)
}

/// **Das Gegenstück zu `dma_unmap_single` — heute ein No-op mit Doku.**
///
/// Es gibt nichts rückgängig zu machen: [`map_single`] legt keine dauerhafte Buchhaltung an,
/// und die Abbildung ist kohärent. Die Funktion existiert, damit portierter Code seinen
/// `unmap`-Aufruf behält — fällt die Kohärenz je weg, ist dies die Stelle, die etwas tun muss,
/// und alle Aufrufer stehen schon.
pub fn unmap_single(_buf: DmaBuf, _dir: StromRichtung) {
    // Absichtlich leer: kohärent auf x86, keine Buchhaltung in `map_single`.
}

/// **Das Gegenstück zu `dma_sync_single_for_cpu` — heute ein No-op mit Doku.**
///
/// Auf kohärent abgebildetem Speicher sieht die CPU ohne weitere Massnahme, was das Gerät
/// geschrieben hat. Auf einer nicht-kohärenten Abbildung müsste hier ein Cache-Invalidieren
/// stehen; der Aufruf ist deshalb bereits vorhanden und wird dann gefüllt, statt nachgetragen.
pub fn sync_fuer_cpu(_buf: &DmaBuf, _dir: StromRichtung) {
    // Absichtlich leer: kohärent auf x86.
}

/// **Das Gegenstück zu `dma_sync_single_for_device` — heute ein No-op mit Doku.**
///
/// Spiegel zu [`sync_fuer_cpu`]: vor dem Gerätestart müsste auf nicht-kohärenter Abbildung ein
/// Cache-Writeback stehen. Hier kohärent, also nichts zu tun — aber der Aufruf bleibt, damit der
/// Treiber an beiden Übergaben synchronisiert, nicht nur an einer.
pub fn sync_fuer_geraet(_buf: &DmaBuf, _dir: StromRichtung) {
    // Absichtlich leer: kohärent auf x86.
}

/// **Ein Streulisten-Eintrag in CPU-Sicht** (Gegenstück zu `struct scatterlist`, Adressteil).
///
/// Die Gerätesicht trägt das Ergebnis ([`DmaBuf`]); hier steht nur, was der Treiber anfragt.
/// `len == 0` ist kein leeres Segment, sondern ein Formfehler — [`map_sg`] weist die ganze Liste
/// dann ab (gibt 0), statt ein Null-Segment an das Gerät zu reichen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SgEintrag {
    pub cpu: u64,
    pub len: u64,
}

/// **Was das Gerät an einer Adresse verträgt** (Gegenstück zu DMA-Maske und Segmentgrenzen).
///
/// Alle drei Felder sind Zusagen des Geräts, nicht der Maschine: eine 32-Bit-Netzkarte auf einer
/// 64-Bit-Maschine meldet `maske = 0xFFFF_FFFF`, und ein Gerät mit 64-KiB-Segmentgrenze meldet
/// `grenze = 0xFFFF`. `0` heisst jeweils „keine Schranke" — ausser bei `maske`, wo `0` alles
/// abweist (keine Gerätesicht unter Null ist ansprechbar).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GeraeteGrenzen {
    /// Höchste ansprechbare Geräteadresse (`dma_mask`): es muss `dev + len - 1 <= maske` gelten.
    pub maske: u64,
    /// Segmentgrenzen-Maske: die Bits, die über ein Segment **gleich bleiben müssen**. Kreutzt
    /// `[dev, dev+len)` eine Gerätegrenze, gilt `((dev ^ (dev+len-1)) & grenze) != 0`. Für
    /// „kein Segment kreuzt eine 64-KiB-Grenze" steht hier `0xFFFF_0000` (Bits ab 16 konstant),
    /// nicht `0xFFFF` — eine Low-Maske forderte gleiche Low-Bits und liesse nur Ein-Byte-Segmente
    /// durch. `0` heisst „keine Grenze".
    pub grenze: u64,
    /// Grösstes einzelnes Segment (`max_seg_size`). `0` heisst „unbegrenzt".
    pub max_seg: u64,
}

/// **Warum eine Geräteadresse das Gerät nicht erreichen darf.**
///
/// Reihenfolge der Prüfung in [`pruefe_grenzen`], und sie ist nicht beliebig: Form zuerst (Leer,
/// Überlauf — Fehler des Aufrufers), dann die Reichweite (Maske), dann die Form des Segments
/// (Grenze, Grösse). Wer die Grösse vor der Maske prüfte, meldete „zu gross" für eine Adresse,
/// die das Gerät nie erreicht — eine Zahl, die in die falsche Richtung schickt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrenzenFehler {
    /// Länge 0 — kein Segment, und ein Null-Segment am Gerät ist ein Deskriptor, den der Treiber
    /// für gültig hielte.
    Leer,
    /// `dev + len` läuft in `u64` über — dann sind Masken- und Grenzprüfung darunter wertlos.
    Ueberlauf,
    /// `dev + len - 1 > maske`: das Gerät kann das Ende nicht adressieren.
    MaskeVerletzt { dev: u64, len: u64, maske: u64 },
    /// Das Segment kreuzt eine Gerätegrenze (`(dev ^ ende) & grenze != 0`).
    GrenzeVerletzt { dev: u64, len: u64, grenze: u64 },
    /// Das Segment ist länger als `max_seg` — der Aufrufer muss teilen, nicht das Gerät.
    SegmentZuGross { len: u64, max_seg: u64 },
}

/// **Die Grenzprüfung einer einzelnen Geräteadresse.**
///
/// Prüft gegen [`GeraeteGrenzen`] in der Reihenfolge aus [`GrenzenFehler`]. Reine Arithmetik über
/// der Gerätesicht — woher `dev` kommt (Pool, Bounce, Gross-DMA), ist dieser Funktion egal.
pub fn pruefe_grenzen(g: &GeraeteGrenzen, dev: u64, len: u64) -> Result<(), GrenzenFehler> {
    if len == 0 {
        return Err(GrenzenFehler::Leer);
    }
    // `ende - 1` ist die letzte adressierte Byte-Adresse; `len > 0` oben macht das Abziehen sicher.
    let ende = dev.checked_add(len).ok_or(GrenzenFehler::Ueberlauf)?;
    let letzte = ende - 1;
    if letzte > g.maske {
        return Err(GrenzenFehler::MaskeVerletzt {
            dev,
            len,
            maske: g.maske,
        });
    }
    if g.grenze != 0 && ((dev ^ letzte) & g.grenze) != 0 {
        return Err(GrenzenFehler::GrenzeVerletzt {
            dev,
            len,
            grenze: g.grenze,
        });
    }
    if g.max_seg != 0 && len > g.max_seg {
        return Err(GrenzenFehler::SegmentZuGross {
            len,
            max_seg: g.max_seg,
        });
    }
    Ok(())
}

/// **Wie viel von `[dev, dev+len)` liegt noch vor der nächsten Gerätegrenze?**
///
/// Antwort ist die Restlänge im **ersten** Segment: passt alles, kommt `len` zurück; kreuzt der
/// Bereich eine Grenze, kommt die Länge bis zur Grenze zurück — der Aufrufer legt dort den
/// Schnitt. `grenze == 0` heisst „keine Grenze" und gibt `len`. `None` nur bei Formfehlern
/// (`len == 0`, Überlauf der Endadresse).
///
/// Die Maske nennt Bits, die konstant bleiben müssen (s. [`GeraeteGrenzen::grenze`]): das erste
/// Segment endet am nächsten Kippen eines gesetzten Bits. Das ist das Minimum über alle
/// gesetzten Bitstellen `k` von `2^k - (dev mod 2^k)` — höchstens 64 Schritte, typisch wenige.
pub fn teile_an_grenze(dev: u64, len: u64, grenze: u64) -> Option<u64> {
    if len == 0 {
        return None;
    }
    dev.checked_add(len)?;
    if grenze == 0 {
        return Some(len);
    }
    // Abstand zum nächsten Kippen je gesetzter Bitstelle; das kleinste gewinnt.
    let mut erste: u64 = len;
    let mut m = grenze;
    while m != 0 {
        let k = m.trailing_zeros();
        // `2^k` passt immer (`k < 64`), die Maske darunter auch.
        let schritt = 1u64 << k;
        let rest = dev & (schritt - 1);
        // `rest < schritt`, also `schritt - rest >= 1`: das Segment hat dort noch Platz.
        let abstand = schritt - rest;
        if abstand < erste {
            erste = abstand;
        }
        // Früh raus, wenn nichts Kleineres mehr kommen kann.
        if erste == 1 {
            break;
        }
        m &= m - 1; // niedrigstes gesetztes Bit löschen
    }
    Some(if erste < len { erste } else { len })
}

/// **Das Gegenstück zu `dma_map_sg` — Eintrag für Eintrag, nie verschmelzend.**
///
/// Für jeden Eintrag: [`DmaPool::map`] plus [`pruefe_grenzen`]. Benachbarte Einträge werden
/// **nicht** zusammengefasst — ein Linux-`dma_map_sg` darf verschmelzen und meldet dann weniger
/// Segmente zurück, als es bekam; wer die Rückgabe als „Eingabezahl" liest, programmiert den
/// nächsten Deskriptor auf das falsche Segment. Hier gilt: **Eingabezahl bei Erfolg, sonst 0** —
/// ein Zwischenergebnis gibt es nicht, `raus` steht bei Fehlschlag ganz auf `None`.
///
/// `raus` muss mindestens so lang sein wie `sg`; ist es kürzer, kommt `0` (der Aufrufer hätte
/// sonst ein Teilergebnis, das wie ein Vollergebnis aussieht). Einträge hinter `sg.len()`
/// werden auf `None` gestellt, damit kein alter Inhalt als Segment gelesen wird.
pub fn map_sg(
    pool: &DmaPool,
    sg: &[SgEintrag],
    raus: &mut [Option<DmaBuf>],
    grenzen: &GeraeteGrenzen,
) -> usize {
    if raus.len() < sg.len() {
        return 0;
    }
    for (i, e) in sg.iter().enumerate() {
        let buf = match pool.map(e.cpu, e.len) {
            Some(b) => b,
            None => {
                // Pool-fremd oder ausserhalb: kein Teilergebnis — alles auf `None`.
                for r in raus.iter_mut().take(sg.len()) {
                    *r = None;
                }
                return 0;
            }
        };
        if pruefe_grenzen(grenzen, buf.dev(), buf.len()).is_err() {
            for r in raus.iter_mut().take(sg.len()) {
                *r = None;
            }
            return 0;
        }
        raus[i] = Some(buf);
    }
    for r in raus.iter_mut().skip(sg.len()) {
        *r = None;
    }
    sg.len()
}

/// **Ein ausgelagerter Bounce-Puffer: das Stück plus die Marke, auf die es zurückgeht.**
///
/// Der Bounce-Puffer löst den Fall, für den [`DmaPool::map`] `None` gibt: ein Stapel- oder
/// Heap-Puffer, der nicht im Pool liegt. Der Treiber lagert in den Pool ein (kopiert hin),
/// lässt das Gerät auf dem Poolstück arbeiten und kopiert zurück — das Kopieren ist
/// Treibersache, Vergeben und Freigeben stehen hier. Die Marke liegt **dabei**, damit Freigabe
/// nicht mit einer fremden oder gestrigen Marke geschieht (s. `Mark` in [`DmaPool::release`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BounceSlot {
    /// Das Poolstück, auf dem das Gerät arbeitet.
    pub buf: DmaBuf,
    /// Der Poolstand vor der Einlagerung — dorthin geht [`bounce_freigeben`] zurück.
    pub marke: Mark,
}

/// **In den Pool einlagern: Marke merken, dann vergeben.**
///
/// Gibt `None`, wenn der Pool das Stück nicht hergibt (erschöpft, Länge 0, krumme Ausrichtung) —
/// der Poolstand bleibt dann unverändert, es gibt nichts freizugeben.
pub fn bounce_einlagern(pool: &mut DmaPool, bedarf: u64, align: u64) -> Option<BounceSlot> {
    let marke = pool.mark();
    let buf = pool.alloc(bedarf, align)?;
    Some(BounceSlot { buf, marke })
}

/// **Den Bounce-Puffer freigeben: zurück auf die gespeicherte Marke.**
///
/// Gibt `false`, wenn die Marke in der Zukunft liegt (darf nicht geschehen — die Marke stammt
/// aus [`bounce_einlagern`] am selben Pool); der Pool bleibt dann unverändert.
pub fn bounce_freigeben(pool: &mut DmaPool, slot: BounceSlot) -> bool {
    pool.release(slot.marke)
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

#[cfg(test)]
mod strom_tests {
    // Host-Tests fuer die DMA-Schemen: Richtung, Streuliste, Geraetegrenzen, Bounce.
    // Reine Literale, keine Maschine — derselbe Grund wie beim Rest der Crate.
    use super::*;

    const CPU: u64 = 0x4000_0000;
    const DEV: u64 = 0x1_0000_0000;
    const LEN: u64 = 0x10000;

    fn pool() -> DmaPool {
        match DmaPool::new(CPU, DEV, LEN) {
            Ok(p) => p,
            Err(_) => panic!("Pool liess sich nicht anlegen"),
        }
    }

    fn offene_grenzen() -> GeraeteGrenzen {
        GeraeteGrenzen {
            maske: u64::MAX,
            grenze: 0,
            max_seg: 0,
        }
    }

    #[test]
    fn richtung_ist_egal_aber_pflicht() {
        // Kohaerent auf x86: alle drei Richtungen liefern dieselbe Abbildung.
        let p = pool();
        let a = map_single(&p, CPU, 64, StromRichtung::NachGeraet).unwrap();
        let b = map_single(&p, CPU, 64, StromRichtung::VomGeraet).unwrap();
        let c = map_single(&p, CPU, 64, StromRichtung::Beide).unwrap();
        assert_eq!(a.dev(), b.dev());
        assert_eq!(a.dev(), c.dev());
        // Und pool-fremd bleibt pool-fremd, egal mit welcher Richtung gefragt wird.
        assert!(map_single(&p, 0xDEAD_0000, 8, StromRichtung::Beide).is_none());
    }

    #[test]
    fn sync_und_unmap_sind_noops() {
        // Sie duerfen nichts tun — aber sie muessen aufrufbar sein, damit portierter Code
        // seine Uebergaben behaelt.
        let p = pool();
        let b = map_single(&p, CPU, 128, StromRichtung::Beide).unwrap();
        sync_fuer_cpu(&b, StromRichtung::VomGeraet);
        sync_fuer_geraet(&b, StromRichtung::NachGeraet);
        // Nach dem Synchronisieren liegt der Puffer noch da, wo er lag.
        assert_eq!(b.dev(), DEV);
        assert_eq!(b.len(), 128);
        unmap_single(b, StromRichtung::Beide);
    }

    #[test]
    fn maske_verletzt_wird_benannt() {
        // Eine 32-Bit-Karte (maske 0xFFFF_FFFF) erreicht eine IOVA oberhalb 4 GiB nicht.
        let g = GeraeteGrenzen {
            maske: 0xFFFF_FFFF,
            grenze: 0,
            max_seg: 0,
        };
        let e = pruefe_grenzen(&g, 0x1_0000_0000, 4096).unwrap_err();
        assert_eq!(
            e,
            GrenzenFehler::MaskeVerletzt {
                dev: 0x1_0000_0000,
                len: 4096,
                maske: 0xFFFF_FFFF
            }
        );
        // Die Gegenprobe: unterhalb der Maske geht es durch.
        assert!(pruefe_grenzen(&g, 0xFFFF_E000, 4096).is_ok());
    }

    #[test]
    fn maskenkante_zaehlt_das_letzte_byte() {
        // `dev + len - 1 <= maske`: genau auf die Kante passt noch, eins darueber nicht.
        let g = GeraeteGrenzen {
            maske: 0xFFFF_FFFF,
            grenze: 0,
            max_seg: 0,
        };
        assert!(pruefe_grenzen(&g, 0xFFFF_F000, 4096).is_ok());
        assert!(matches!(
            pruefe_grenzen(&g, 0xFFFF_F001, 4096).unwrap_err(),
            GrenzenFehler::MaskeVerletzt { .. }
        ));
    }

    #[test]
    fn boundary_split_teilt_am_richtigen_byte() {
        // 64-KiB-Grenze (Bits ab 16 konstant): 0x1_FFF0 + 0x20 kreuzt sie, 16 passen davor.
        let grenze = 0xFFFF_0000u64;
        assert_eq!(teile_an_grenze(0x1_FFF0, 0x20, grenze), Some(16));
        // Ohne Uebertritt kommt die volle Laenge zurueck.
        assert_eq!(teile_an_grenze(0x1_0000, 0x20, grenze), Some(0x20));
        // Ohne Grenze (`0`) passt immer alles.
        assert_eq!(teile_an_grenze(0x1_FFF0, 0x20, 0), Some(0x20));
        // Und die Pruefung weist den kreuzenden Bereich ab, statt ihn still zuzulassen.
        let g = GeraeteGrenzen {
            maske: u64::MAX,
            grenze,
            max_seg: 0,
        };
        assert!(matches!(
            pruefe_grenzen(&g, 0x1_FFF0, 0x20).unwrap_err(),
            GrenzenFehler::GrenzeVerletzt { .. }
        ));
        assert!(pruefe_grenzen(&g, 0x1_0000, 0x20).is_ok());
    }

    #[test]
    fn max_seg_weist_zu_lang_ab() {
        let g = GeraeteGrenzen {
            maske: u64::MAX,
            grenze: 0,
            max_seg: 4096,
        };
        assert!(pruefe_grenzen(&g, 0x1_0000, 4096).is_ok());
        assert_eq!(
            pruefe_grenzen(&g, 0x1_0000, 4097).unwrap_err(),
            GrenzenFehler::SegmentZuGross {
                len: 4097,
                max_seg: 4096
            }
        );
    }

    #[test]
    fn sg_mit_pool_fremd_gibt_null_und_kein_teilergebnis() {
        let p = pool();
        let sg = [
            SgEintrag { cpu: CPU, len: 64 },
            SgEintrag {
                cpu: 0xDEAD_0000,
                len: 64,
            }, // Stapelpuffer-Fall
        ];
        let mut raus = [None, None, None];
        assert_eq!(map_sg(&p, &sg, &mut raus, &offene_grenzen()), 0);
        // Kein Teilergebnis: auch der gueltige erste Eintrag steht auf `None`.
        assert!(raus[0].is_none());
        assert!(raus[1].is_none());
    }

    #[test]
    fn sg_erfolg_zaehlt_eingaben_und_verschmilzt_nie() {
        // Zwei benachbarte Eintraege blieben beim Verschmelzen EIN Segment — hier sind es zwei.
        let p = pool();
        let sg = [
            SgEintrag { cpu: CPU, len: 64 },
            SgEintrag {
                cpu: CPU + 64,
                len: 64,
            },
        ];
        let mut raus = [None, None];
        assert_eq!(map_sg(&p, &sg, &mut raus, &offene_grenzen()), 2);
        let a = raus[0].unwrap();
        let b = raus[1].unwrap();
        assert_eq!(a.dev(), DEV);
        assert_eq!(b.dev(), DEV + 64);
        // Benachbart, aber zwei Stuecke: wer verschmoelze, meldete ein Segment statt zwei.
        assert_eq!(a.dev() + a.len(), b.dev());
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
    }

    #[test]
    fn sg_grenzverletzung_gibt_null() {
        let p = pool();
        let g = GeraeteGrenzen {
            maske: u64::MAX,
            grenze: 0xFFFF_0000,
            max_seg: 0,
        };
        // `[DEV, DEV+0x20)` kreuzt keine 64-KiB-Grenze (DEV ist ausgerichtet) — also zuerst eine
        // Adresse suchen, die es tut: CPU+0xFFF0 im Pool entspricht DEV+0xFFF0.
        let sg = [SgEintrag {
            cpu: CPU + 0xFFF0,
            len: 0x20,
        }];
        let mut raus = [None];
        assert_eq!(map_sg(&p, &sg, &mut raus, &g), 0);
        assert!(raus[0].is_none());
    }

    #[test]
    fn bounce_alloc_free_ist_wiederverwendbar() {
        let mut p = pool();
        let belegt = p.used();
        let slot = bounce_einlagern(&mut p, 256, 64).unwrap();
        assert_eq!(slot.buf.len(), 256);
        assert_eq!(slot.buf.cpu() % 64, 0);
        assert!(bounce_freigeben(&mut p, slot));
        // Nach der Freigabe liegt derselbe Platz wieder an: Bounce ist kein Leck.
        assert_eq!(p.used(), belegt);
        let wieder = bounce_einlagern(&mut p, 256, 64).unwrap();
        assert_eq!(wieder.buf.cpu(), slot.buf.cpu());
        assert_eq!(wieder.buf.dev(), slot.buf.dev());
        assert!(bounce_freigeben(&mut p, wieder));
    }

    #[test]
    fn bounce_erschoepfung_laesst_den_stand_unveraendert() {
        let mut p = pool();
        let belegt = p.used();
        assert!(bounce_einlagern(&mut p, LEN + 1, 1).is_none());
        assert_eq!(p.used(), belegt);
    }
}
