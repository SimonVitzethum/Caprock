//! **virtio-pci (modern): die Treiberlogik — kernfrei.**
//!
//! Diese Crate haengt an **nichts**. Kein `sel4lake-hal`, kein Kernel, keine Architektur. Das ist
//! die Voraussetzung dafuer, dass der Treiber dorthin kann, wo er hingehoert: in eine
//! **Userland-Treiber-PD** (todo A-5.1). Gesetzte Regel (Simon, 2026-08-01): in den Mikrokern
//! gehoeren keine Treiber.
//!
//! ## Was hier liegt und was nicht
//!
//! Hier liegt das **Protokoll**: Handshake, Feature-Aushandlung, Virtqueue, Notify, used-Ring.
//! Alles davon braucht nur zwei Dinge — die aufgeloesten Adressen der virtio-Strukturen und eine
//! Speicherbarriere.
//!
//! Nicht hier liegt das **Auffinden** dieser Adressen: das ist ein Lauf durch den
//! PCI-Konfigurationsraum, also Bus-Enumeration. Die bleibt im Kern (`hal::virtio::probe`), und
//! zwar aus einem Grund, der die Trennlinie gut zeigt: der Konfigurationsraum ist **geraeteweit**.
//! Wer ihn lesen darf, sieht jedes Geraet der Maschine. Ein Treiber-PD bekommt deshalb das
//! Ergebnis — die Adressen seines eigenen Geraets — und nicht das Werkzeug, es selbst zu suchen.
//!
//! ## Aufbau: ein Transport, drei Geraete
//!
//! [`Transport`] ist der geraeteunabhaengige Teil (Status, Features, Queues, Notify,
//! geraetespezifischer Konfigurationsraum), [`Queue`] der Split-Virtqueue-Ring. Darauf sitzen
//! [`VirtioRng`], [`blk::VirtioBlk`] und [`net::VirtioNet`].
//!
//! Der Schnitt entstand mit A-5.2: `VirtioRng` war ein Monolith, in dem Handshake und Anfrage
//! ineinander lagen. Solange es ein Geraet gab, fiel das nicht auf. Bei zweien waere die Wahl
//! gewesen, den Handshake zu **kopieren** — und damit zwei Fassungen derselben Zustandsmaschine zu
//! haben, von denen eine irgendwann still zurueckbleibt. Der virtio-Handshake ist genau die Sorte
//! Ablauf, bei der eine vergessene Zeile (`FEATURES_OK` nicht zurueckgelesen) nicht auffaellt, bis
//! ein Geraet die Features ablehnt.
//!
//! ## Die Barriere wird hereingereicht, nicht gewaehlt
//!
//! [`Transport::new`] nimmt einen Funktionszeiger. Der naheliegende Weg waere
//! `core::sync::atomic::fence(SeqCst)` gewesen — arch-neutral und ohne Parameter. Er waere falsch:
//! auf aarch64 uebersetzt das zu `dmb ish` (inner shareable), und Device-Memory liegt nicht in
//! dieser Domaene. Der bisherige Code nimmt dort `dsb sy`. Eine "arch-neutrale" Barriere haette
//! die Semantik still abgeschwaecht, und der ARM-Zweig haette es vielleicht ueberlebt — bis er es
//! nicht mehr tut.

#![no_std]

pub mod blk;
pub mod net;

// device_status-Bits.
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

/// `VIRTIO_F_VERSION_1` = Bit 32. Ohne dieses Bit ist das Geraet "transitional" und benutzt das
/// alte Registerlayout — dieser Treiber spricht ausschliesslich modern.
pub const F_VERSION_1: u64 = 1 << 32;
/// `VIRTIO_F_ACCESS_PLATFORM` = Bit 33.
///
/// Das ist die Zusage des Geräts, Adressen **so zu behandeln, wie die Plattform sie meint** —
/// also durch die IOMMU zu laufen statt physisch am Kernel vorbei. Ohne dieses Bit greift ein
/// emuliertes virtio-Gerät in QEMU direkt auf `address_space_memory` zu und ignoriert die SMMU,
/// auch wenn `iommu=smmuv3` gesetzt ist. Solange IOVA == PA war, fiel das nicht auf; seit ext-36
/// Schritt b ist es der Unterschied zwischen „funktioniert" und „liest Müll oberhalb des RAM".
/// Der Treiber verlangt es deshalb **verbindlich** und bricht sonst ab, statt still auf
/// physische Adressen zurückzufallen — genau dieser Rückfall wäre die Achsenverwechslung.
pub const F_ACCESS_PLATFORM: u64 = 1 << 33;

/// virtq-Deskriptor-Flag: die Kette geht weiter (`next` ist gueltig).
pub const VIRTQ_DESC_F_NEXT: u16 = 1;
/// virtq-Deskriptor-Flag: Gerät schreibt in den Puffer.
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

/// common_cfg-Register-Offsets (virtio_pci_common_cfg).
mod cc {
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    pub const DEVICE_FEATURE: u64 = 0x04;
    pub const DRIVER_FEATURE_SELECT: u64 = 0x08;
    pub const DRIVER_FEATURE: u64 = 0x0c;
    pub const DEVICE_STATUS: u64 = 0x14;
    pub const QUEUE_SELECT: u64 = 0x16;
    pub const QUEUE_SIZE: u64 = 0x18;
    pub const QUEUE_ENABLE: u64 = 0x1c;
    pub const QUEUE_NOTIFY_OFF: u64 = 0x1e;
    pub const QUEUE_DESC: u64 = 0x20;
    pub const QUEUE_DRIVER: u64 = 0x28;
    pub const QUEUE_DEVICE: u64 = 0x30;
}

// Virtqueue-Layout innerhalb der DMA-Region (Offsets, eine Page reicht).
const Q_SIZE: u16 = 8;
const OFF_DATA: u64 = 0x800; // Datenpuffer (Gerät DMAt hierher)
const DATA_LEN: u32 = 64;
/// Offset des Datenpuffers in der DMA-Region (für den Aufrufer, der die Bytes ausliest).
pub const DATA_OFFSET: u64 = OFF_DATA;
/// Länge des Datenpuffers (Bytes, die das Gerät schreibt) — für die Bounds-Prüfung.
pub const DATA_LEN_BYTES: u32 = DATA_LEN;

#[inline]
pub(crate) unsafe fn rd8(a: u64) -> u8 {
    core::ptr::read_volatile(a as *const u8)
}
#[inline]
pub(crate) unsafe fn rd16(a: u64) -> u16 {
    core::ptr::read_volatile(a as *const u16)
}
#[inline]
pub(crate) unsafe fn rd32(a: u64) -> u32 {
    core::ptr::read_volatile(a as *const u32)
}
#[inline]
pub(crate) unsafe fn rd64(a: u64) -> u64 {
    core::ptr::read_volatile(a as *const u64)
}
#[inline]
pub(crate) unsafe fn wr8(a: u64, v: u8) {
    core::ptr::write_volatile(a as *mut u8, v)
}
#[inline]
pub(crate) unsafe fn wr16(a: u64, v: u16) {
    core::ptr::write_volatile(a as *mut u16, v)
}
#[inline]
pub(crate) unsafe fn wr32(a: u64, v: u32) {
    core::ptr::write_volatile(a as *mut u32, v)
}
#[inline]
pub(crate) unsafe fn wr64(a: u64, v: u64) {
    core::ptr::write_volatile(a as *mut u64, v)
}

// --- Auffinden der Strukturen auf der EIGENEN Konfigurationsraum-Seite (A-5.1) -----------------

/// virtio-pci Capability-Typen (`cfg_type` im Vendor-Cap `0x09`).
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;
const CAP_ID_VNDR: u8 = 0x09;
/// Offset der Capability-Liste im PCI-Konfigurationsraum.
const CFG_CAP_PTR: u64 = 0x34;
/// Offset von BAR0.
const CFG_BAR0: u64 = 0x10;

/// Die virtio-Strukturen auf **einer** Konfigurationsraum-Seite auflösen.
///
/// ## Warum das hier liegen darf und nicht im Kern bleiben muss
///
/// Der Einwand gegen einen Capability-Lauf im Treiber war: der Konfigurationsraum ist
/// **geräteweit**, wer ihn lesen darf, sieht jede Maschine. Das stimmt für ein globales
/// Adressregister und für das ECAM-Fenster als Ganzes — aber nicht für **eine Funktion**: ECAM
/// bildet `(bus, dev, func)` auf je 4 KiB ab, also auf genau eine Seite. Wer diese eine Seite
/// gemappt bekommt, sieht sein eigenes Gerät und sonst nichts.
///
/// Damit kann der Kern behalten, was ihm gehört (die **Enumeration**: welches Gerät gibt es, wem
/// wird es zugeteilt), und der Treiber trotzdem selbst auflösen, was in seinem Gerät steht. Die
/// Alternative wäre gewesen, dass der Kern die virtio-Strukturen auflöst und dem Treiber reicht —
/// dann müsste der Kern virtio kennen, und genau das soll er nicht.
///
/// `cfg` ist die **Adresse der gemappten Seite** (nicht des ECAM-Fensters). `None`, wenn die
/// nötigen Capabilities fehlen oder ihr BAR nicht zugewiesen ist.
///
/// # Safety
/// `cfg` muss auf die gemappte Konfigurationsraum-Seite genau dieser Funktion zeigen, und die
/// darin genannten BARs müssen in der eigenen VSpace erreichbar sein.
pub unsafe fn probe_ecam(cfg: u64, fence: fn()) -> Option<Transport> {
    let mut common = 0u64;
    let mut notify_base = 0u64;
    let mut notify_mul = 0u32;
    let mut device_cfg = 0u64;
    let mut cap = rd8(cfg + CFG_CAP_PTR) as u64;
    let mut guard = 0;
    // Die Schranke ist kein Stilmittel: eine Capability-Liste ist eine verkettete Liste im
    // Konfigurationsraum eines Geräts. Ein defektes (oder böswilliges) Gerät kann sie im Kreis
    // legen, und ein Treiber, der ihr ohne Schranke folgt, hängt.
    while cap != 0 && guard < 48 {
        guard += 1;
        let id = rd8(cfg + cap);
        let next = rd8(cfg + cap + 1) as u64;
        if id == CAP_ID_VNDR {
            let cfg_type = rd8(cfg + cap + 3);
            let bar = rd8(cfg + cap + 4) as u64;
            let offset = rd32(cfg + cap + 8);
            let bar_base = bar_addr(cfg, bar);
            if bar_base != 0 {
                let addr = bar_base + offset as u64;
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => common = addr,
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        notify_base = addr;
                        notify_mul = rd32(cfg + cap + 16);
                    }
                    VIRTIO_PCI_CAP_DEVICE_CFG => device_cfg = addr,
                    _ => {}
                }
            }
        }
        cap = next;
    }
    if common == 0 || notify_base == 0 {
        return None;
    }
    Some(Transport::new(common, notify_base, notify_mul, device_cfg, fence))
}

/// Die Basisadresse des BAR `i` aus der Konfigurationsraum-Seite lesen (64-Bit-BARs zusammengesetzt).
///
/// # Safety
/// wie [`probe_ecam`].
unsafe fn bar_addr(cfg: u64, i: u64) -> u64 {
    if i >= 6 {
        return 0;
    }
    let lo = rd32(cfg + CFG_BAR0 + i * 4);
    if lo & 1 != 0 {
        return 0; // I/O-BAR: dieser Treiber benutzt nur Speicher-BARs
    }
    let mut addr = (lo & !0xf) as u64;
    if (lo >> 1) & 0b11 == 0b10 && i < 5 {
        addr |= (rd32(cfg + CFG_BAR0 + (i + 1) * 4) as u64) << 32;
    }
    addr
}

/// Ein **Split-Virtqueue-Ring** in der DMA-Region, aus Sicht der **CPU**.
///
/// Hier steht bewusst nur die eine Achse. Die Gerätesicht derselben Struktur kennt allein
/// [`Transport::queue_setup`] — sie geht dort in die Queue-Adressregister und wird danach nicht
/// mehr gebraucht. Beide Adressen in einer Struktur zu halten, aus der man sich je nach Zweck die
/// passende greift, ist genau die Bequemlichkeit, die aus zwei Achsen eine macht: bis ext-36
/// trugen sie denselben Wert, mit einem IOVA-Fenster ≠ 0 fallen sie auseinander, und ein Treiber,
/// der sie vermischt, programmiert dem Gerät eine Adresse, die es nicht auflösen kann — oder
/// beschreibt eine, unter der nichts liegt.
#[derive(Clone, Copy)]
pub struct Queue {
    cpu: u64,
    size: u16,
}

impl Queue {
    /// Platzbedarf einer Queue in der Region. Bei `size <= 8`: Deskriptoren 128 Byte, avail
    /// 22 Byte, used 70 Byte — die Offsets unten lassen reichlich Luft, damit zwei Queues
    /// (virtio-net: RX und TX) ohne Rechnerei nebeneinander liegen koennen.
    pub const BYTES: u64 = 0x400;
    const OFF_DESC: u64 = 0x000;
    const OFF_AVAIL: u64 = 0x100;
    const OFF_USED: u64 = 0x200;

    fn desc(&self) -> u64 {
        self.cpu + Self::OFF_DESC
    }
    fn avail(&self) -> u64 {
        self.cpu + Self::OFF_AVAIL
    }
    fn used(&self) -> u64 {
        self.cpu + Self::OFF_USED
    }

    /// Einen Deskriptor setzen. `addr` ist die **Geraetesicht** des Puffers.
    ///
    /// # Safety
    /// `self.cpu` muss auf beschreibbaren, dem Aufrufer allein gehoerenden Speicher zeigen.
    pub unsafe fn set_desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let d = self.desc() + (i as u64) * 16;
        wr64(d, addr);
        wr32(d + 8, len);
        wr16(d + 12, flags);
        wr16(d + 14, next);
    }

    /// Den Kettenkopf `head` in den avail-Ring haengen und sichtbar machen.
    ///
    /// # Safety
    /// wie [`Self::set_desc`].
    pub unsafe fn publish(&self, head: u16, fence: fn()) {
        let idx = rd16(self.avail() + 2);
        wr16(self.avail() + 4 + (idx % self.size) as u64 * 2, head);
        fence(); // der Deskriptor muss stehen, BEVOR der Index ihn freigibt
        wr16(self.avail() + 2, idx.wrapping_add(1));
        fence();
    }

    /// Der aktuelle Stand des used-Rings (vom Geraet geschrieben).
    ///
    /// # Safety
    /// wie [`Self::set_desc`].
    pub unsafe fn used_idx(&self) -> u16 {
        rd16(self.used() + 2)
    }

    /// `(id, len)` des used-Eintrags an Ringposition `slot % size`.
    ///
    /// # Safety
    /// wie [`Self::set_desc`].
    pub unsafe fn used_entry(&self, slot: u16) -> (u32, u32) {
        let e = self.used() + 4 + (slot % self.size) as u64 * 8;
        (rd32(e), rd32(e + 4))
    }

    /// Warten, bis der used-Ring ueber `from` hinausgeht. `None` = das Geraet hat nicht geantwortet.
    ///
    /// # Safety
    /// wie [`Self::set_desc`].
    pub unsafe fn poll_used(&self, from: u16, max_poll: u64) -> Option<(u32, u32)> {
        for _ in 0..max_poll {
            if self.used_idx() != from {
                return Some(self.used_entry(from));
            }
            core::hint::spin_loop();
        }
        None
    }
}

/// Der geraeteunabhaengige Teil von virtio-pci (modern).
///
/// Enthält **keine** Autorität: wer diese Struktur hat, kann das Gerät bedienen, aber nichts
/// finden, was er nicht schon hatte. Genau deshalb darf sie einer Treiber-PD gereicht werden.
#[derive(Clone, Copy)]
pub struct Transport {
    common: u64,      // common_cfg-Basisadresse
    notify_base: u64, // notify-Struktur-Basisadresse
    notify_mul: u32,  // queue_notify_off-Multiplikator
    /// geraetespezifischer Konfigurationsraum (`VIRTIO_PCI_CAP_DEVICE_CFG`); `0` = nicht vorhanden.
    ///
    /// Der RNG braucht ihn nicht — er hat keine Konfiguration. `blk` (Kapazitaet) und `net` (MAC)
    /// schon, und beides sind Werte, die ein Test **pruefen** kann: eine gemeldete Kapazitaet, die
    /// zur Groesse des Abbilds passt, ist ein Beleg; ein Puffer voller Nullen ist keiner.
    device_cfg: u64,
    /// Speicherbarriere — s. Crate-Doku: sie wird hereingereicht, nicht hier gewählt.
    fence: fn(),
}

impl Transport {
    /// Aus aufgelösten Adressen bauen. `fence` muss Speicher **gegen Device-Zugriffe** ordnen
    /// (aarch64: `dsb sy`, x86: `mfence`) — nicht bloß gegen andere Kerne.
    pub const fn new(
        common: u64,
        notify_base: u64,
        notify_mul: u32,
        device_cfg: u64,
        fence: fn(),
    ) -> Self {
        Self { common, notify_base, notify_mul, device_cfg, fence }
    }

    /// Hat das Geraet einen geraetespezifischen Konfigurationsraum gemeldet?
    pub fn has_device_cfg(&self) -> bool {
        self.device_cfg != 0
    }

    /// Adresse des `common_cfg`-Fensters.
    ///
    /// Gebraucht vom **Zuteiler** (A-5.1): er muss wissen, welches BAR er einer Treiber-PD
    /// mitgeben muss, und die virtio-Strukturen liegen irgendwo darin. Das ist die einzige
    /// Stelle, an der jemand ausserhalb des Treibers diese Adresse braucht -- und er braucht sie,
    /// um Autoritaet zuzuteilen, nicht um das Geraet zu bedienen.
    pub fn common_addr(&self) -> u64 {
        self.common
    }

    /// Die hereingereichte Barriere (fuer Treiber, die sie selbst setzen muessen).
    pub fn fence(&self) {
        (self.fence)()
    }

    unsafe fn status(&self, v: u8) {
        wr8(self.common + cc::DEVICE_STATUS, v);
    }
    unsafe fn get_status(&self) -> u8 {
        rd8(self.common + cc::DEVICE_STATUS)
    }

    /// Reset + `ACKNOWLEDGE` + `DRIVER` — die ersten drei Schritte jedes virtio-Hochlaufs.
    ///
    /// # Safety
    /// `common` muss auf das common_cfg-Fenster des Geraets zeigen (Device-Memory).
    pub unsafe fn reset(&self) {
        self.status(0);
        for _ in 0..100_000 {
            if self.get_status() == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        self.status(STATUS_ACK);
        self.status(STATUS_ACK | STATUS_DRIVER);
    }

    /// Die vom Geraet angebotenen Features (beide Haelften).
    ///
    /// # Safety
    /// wie [`Self::reset`].
    pub unsafe fn offered(&self) -> u64 {
        wr32(self.common + cc::DEVICE_FEATURE_SELECT, 0);
        let lo = rd32(self.common + cc::DEVICE_FEATURE) as u64;
        wr32(self.common + cc::DEVICE_FEATURE_SELECT, 1);
        let hi = rd32(self.common + cc::DEVICE_FEATURE) as u64;
        lo | (hi << 32)
    }

    /// `want` aushandeln und `FEATURES_OK` **zurueckgelesen**.
    ///
    /// Das Zurueckgelesene ist der Punkt: das Geraet darf die Auswahl ablehnen, und ein Treiber, der
    /// nur schreibt, faehrt dann mit einer Annahme weiter, die das Geraet nie bestaetigt hat.
    ///
    /// # Safety
    /// wie [`Self::reset`].
    pub unsafe fn negotiate(&self, want: u64) -> bool {
        wr32(self.common + cc::DRIVER_FEATURE_SELECT, 0);
        wr32(self.common + cc::DRIVER_FEATURE, want as u32);
        wr32(self.common + cc::DRIVER_FEATURE_SELECT, 1);
        wr32(self.common + cc::DRIVER_FEATURE, (want >> 32) as u32);
        self.status(STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        self.get_status() & STATUS_FEATURES_OK != 0
    }

    /// Virtqueue `idx` aufsetzen. `cpu_base`/`dev_base` sind die beiden Sichten **dieser** Queue
    /// (nicht der ganzen Region). Gibt die Queue und ihren `queue_notify_off`.
    ///
    /// `None`, wenn das Geraet die Queue nicht hat (`QUEUE_SIZE == 0` nach dem Schreiben) — das
    /// unterscheidet "Queue 1 gibt es nicht" von "Queue 1 antwortet nicht", und nur die zweite
    /// Lage ist ein Treiberfehler.
    ///
    /// # Safety
    /// wie [`Self::reset`]; `cpu_base` muss auf freien, dem Aufrufer gehoerenden Speicher zeigen.
    pub unsafe fn queue_setup(
        &self,
        idx: u16,
        cpu_base: u64,
        dev_base: u64,
        want_size: u16,
    ) -> Option<(Queue, u16)> {
        wr16(self.common + cc::QUEUE_SELECT, idx);
        let qmax = rd16(self.common + cc::QUEUE_SIZE);
        if qmax == 0 {
            return None; // diese Queue gibt es nicht
        }
        let qsize = if qmax >= want_size { want_size } else { qmax };
        wr16(self.common + cc::QUEUE_SIZE, qsize);
        let q = Queue { cpu: cpu_base, size: qsize };
        // **Die ganze Ringstruktur wird genullt, nicht nur der Treiberteil.**
        //
        // Naheliegend waere, nur `avail` anzufassen -- `used` gehoert schliesslich dem Geraet.
        // Das ist genau falsch, und der Fehler faellt erst auf, wenn eine Region **wiederverwendet**
        // wird (A-5.1: die zweite Fassung eines Treibers erbt die Region der ersten):
        //
        //   * das Geraet setzt seinen used-Index beim Reset auf 0 zurueck;
        //   * im Speicher steht aber noch der Endstand der vorigen Fassung, etwa 1;
        //   * der neue Treiber merkt sich diesen Stand als Ausgangswert und wartet auf eine
        //     Aenderung -- das Geraet schreibt nach der ersten Anfrage wieder genau 1.
        //
        // Ergebnis: der Treiber wartet auf einen Fortschritt, der bereits eingetreten ist, und
        // laeuft in seine Poll-Schranke. Das sieht aus wie ein stummes Geraet und ist eine nicht
        // zurueckgesetzte Ringstruktur. Die Spec ist eindeutig: **der Treiber** initialisiert den
        // Speicher der Queue, bevor er sie freigibt -- und vor `QUEUE_ENABLE` gehoert er ihm.
        wr16(q.avail(), 0); // avail.flags
        wr16(q.avail() + 2, 0); // avail.idx
        wr16(q.used(), 0); // used.flags
        wr16(q.used() + 2, 0); // used.idx
        wr64(self.common + cc::QUEUE_DESC, dev_base + Queue::OFF_DESC);
        wr64(self.common + cc::QUEUE_DRIVER, dev_base + Queue::OFF_AVAIL);
        wr64(self.common + cc::QUEUE_DEVICE, dev_base + Queue::OFF_USED);
        let notify_off = rd16(self.common + cc::QUEUE_NOTIFY_OFF);
        wr16(self.common + cc::QUEUE_ENABLE, 1);
        Some((q, notify_off))
    }

    /// `DRIVER_OK` — ab hier darf das Geraet arbeiten.
    ///
    /// # Safety
    /// wie [`Self::reset`].
    pub unsafe fn driver_ok(&self) {
        self.status(STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);
    }

    /// Das Geraet ueber neue Eintraege in Queue `queue` benachrichtigen.
    ///
    /// # Safety
    /// wie [`Self::reset`]; `notify_off` muss aus [`Self::queue_setup`] derselben Queue stammen.
    pub unsafe fn kick(&self, notify_off: u16, queue: u16) {
        let addr = self.notify_base + (notify_off as u64) * (self.notify_mul as u64);
        wr16(addr, queue);
        (self.fence)();
    }

    /// Geraetespezifischer Konfigurationsraum, 8 Bit. `0`, wenn es keinen gibt.
    ///
    /// # Safety
    /// `off` muss innerhalb des vom Geraet gemeldeten Konfigurationsraums liegen.
    pub unsafe fn cfg8(&self, off: u64) -> u8 {
        if self.device_cfg == 0 {
            return 0;
        }
        rd8(self.device_cfg + off)
    }

    /// Geraetespezifischer Konfigurationsraum, 64 Bit. `0`, wenn es keinen gibt.
    ///
    /// # Safety
    /// wie [`Self::cfg8`].
    pub unsafe fn cfg64(&self, off: u64) -> u64 {
        if self.device_cfg == 0 {
            return 0;
        }
        rd64(self.device_cfg + off)
    }
}

/// **virtio-rng**: das DMA-Beweisgeraet.
///
/// Es hat weder Konfigurationsraum noch Anfrageformat — eine einzige beschreibbare Deskriptorzelle,
/// und das Geraet fuellt sie. Genau deshalb war es das erste: es prueft den **Transport**, ohne
/// dass ein Geraeteprotokoll dazwischensteht.
///
/// Was es aber **nicht** kann: die Gegenrichtung zeigen. Der RNG laesst das Geraet nur *schreiben*.
/// Ob es unseren Speicher auch *lesen* darf — also ob ein Anfragekopf ueberhaupt bei ihm ankommt —
/// steht damit nicht fest. Das belegen erst `blk` und `net` (A-5.2).
pub struct VirtioRng {
    t: Transport,
}

impl VirtioRng {
    /// Aus aufgelösten Adressen bauen. `fence` muss Speicher **gegen Device-Zugriffe** ordnen
    /// (aarch64: `dsb sy`, x86: `mfence`) — nicht bloß gegen andere Kerne.
    pub const fn new(common: u64, notify_base: u64, notify_mul: u32, fence: fn()) -> Self {
        Self { t: Transport::new(common, notify_base, notify_mul, 0, fence) }
    }

    /// Aus einem bereits aufgeloesten Transport bauen.
    pub const fn from_transport(t: Transport) -> Self {
        Self { t }
    }

    /// Vollständiger Handshake + eine RNG-Anfrage. Die Virtqueue + der Datenpuffer liegen in
    /// `[dma_base, dma_base+dma_len)` (in die DMA-Region, SMMU-erzwungen). `desc_addr` ist die
    /// Physadresse, die dem Gerät als Zielpuffer genannt wird — normalerweise `dma_base+OFF_DATA`
    /// (in-window). Für den Kronjuwel-Sensitivitätstest kann eine **Out-of-Window**-Adresse
    /// übergeben werden -> die SMMU faultet, das Gerät schreibt NICHT. Gibt
    /// `(used_advanced, written_len)`: ob der used-Ring fortschritt + die vom Gerät gemeldete
    /// Byte-Zahl. Für In-Window erwartet: used_advanced=true, written_len>0.
    /// `cpu_base` ist die **physische** Basis der Virtqueue (die CPU beschreibt die Ringe
    /// darüber), `dev_base` die **Gerätesicht** derselben Struktur (sie wird in die
    /// Queue-Adressregister programmiert), `desc_addr` die Gerätesicht des Zielpuffers.
    ///
    /// Bis ext-36 war das **ein** Parameter, weil beide Adressen denselben Wert trugen. Mit einem
    /// IOVA-Fenster ≠ 0 fallen sie auseinander, und ein Treiber, der sie vermischt, programmiert
    /// dem Gerät eine Adresse, die es nicht auflösen kann — oder beschreibt eine, unter der
    /// nichts liegt. Deshalb getrennt, auch wo es umständlicher aussieht.
    ///
    /// # Safety
    /// Die MMIO-Adressen muessen zum Geraet gehoeren, die DMA-Region dem Aufrufer.
    pub unsafe fn request(&self, cpu_base: u64, dev_base: u64, desc_addr: u64) -> (bool, u32) {
        self.request_polled(cpu_base, dev_base, desc_addr, 50_000_000)
    }

    /// Wie [`Self::request`], aber mit waehlbarer Poll-Obergrenze.
    ///
    /// Gebraucht fuer den **erwarteten Fehlschlag**: soll geprueft werden, dass die IOMMU einen
    /// nicht zugeteilten Strom blockt, wartet man auf eine Antwort, die per Entwurf nie kommt.
    /// 50 Mio Umdrehungen sind dann keine Grosszuegigkeit, sondern hunderte Millisekunden
    /// Leerlauf je Lauf. Der Aufrufer, der einen Fehlschlag ERWARTET, sagt das mit einer kurzen
    /// Schranke — und traegt selbst die Verantwortung, dass sie nicht zu kurz ist.
    ///
    /// # Safety
    /// wie [`Self::request`].
    pub unsafe fn request_polled(
        &self,
        cpu_base: u64,
        dev_base: u64,
        desc_addr: u64,
        max_poll: u64,
    ) -> (bool, u32) {
        // 1.-3. Reset, ACKNOWLEDGE, DRIVER.
        self.t.reset();
        // 4. Feature-Negotiation: VIRTIO_F_VERSION_1 + VIRTIO_F_ACCESS_PLATFORM. Letzteres muss
        // das Gerät anbieten — bietet es das nicht an, würde es die IOMMU umgehen, und der
        // Treiber hätte keine Möglichkeit, das zu bemerken. Also hier abbrechen.
        if self.t.offered() & F_ACCESS_PLATFORM == 0 {
            return (false, 0);
        }
        // 5. FEATURES_OK.
        if !self.t.negotiate(F_VERSION_1 | F_ACCESS_PLATFORM) {
            return (false, 0); // Gerät akzeptiert die Features nicht
        }
        // 6. Virtqueue 0 einrichten.
        let Some((q, notify_off)) = self.t.queue_setup(0, cpu_base, dev_base, Q_SIZE) else {
            return (false, 0);
        };
        // 7. DRIVER_OK.
        self.t.driver_ok();

        // 8. used-Ring-Ausgangsstand merken, Deskriptor + avail-Ring bauen.
        let used_idx0 = q.used_idx();
        q.set_desc(0, desc_addr, DATA_LEN, VIRTQ_DESC_F_WRITE, 0);
        q.publish(0, self.t.fence);
        // 9. Notify: queue_notify_off * multiplier.
        self.t.kick(notify_off, 0);

        // 10. used-Ring pollen (das Gerät DMAt die Bytes + advanced used.idx). Grosszuegige
        // Obergrenze: der Poll bricht normal nach wenigen tausend Iterationen ab (Gerät hat
        // geantwortet); die hohe Schranke greift nur im seltenen TCG-Timing-Jitter-Fall, in dem
        // das emulierte Geraet spaeter fertig wird (Burn-in #1: ~1/2000 Laeufe `used_adv=false`).
        // Worst Case dann ~hunderte ms Busy-Wait statt eines Schein-Fehlschlags.
        match q.poll_used(used_idx0, max_poll) {
            Some((_id, len)) => (true, len),
            None => (false, 0),
        }
    }
}
