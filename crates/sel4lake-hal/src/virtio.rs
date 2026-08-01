//! Minimaler **virtio-pci (modern)** RNG-Treiber (ext-23, D4) — das DMA-Beweisgerät.
//!
//! **Arch-neutral seit 2026-08-01 (todo A-5.2).** Das Modul lag bis dahin unter `aarch64/`, obwohl
//! nichts daran ARM-spezifisch war: virtio-pci ist ein PCI-Standard, und die beiden einzigen
//! Berührungspunkte mit der Architektur — `cpu::dsb_sy()` (auf x86 `mfence`) und der
//! PCI-Konfigurationszugriff — gibt es auf beiden Zweigen. Es lag dort, weil es dort entstanden
//! ist. Dieselbe Fehlerform wie beim Farbtest: Code auf einem Zweig, der auf dem anderen fehlt,
//! ohne dass ihn jemand dorthin gelegt hätte.
//!
//! Treiber-/Protokoll-Logik (Trusted-/kernel-seitig): findet die virtio-Strukturen über die
//! PCI-Capability-Liste, handshaket (Reset -> ACK -> DRIVER -> VERSION_1 -> FEATURES_OK ->
//! Virtqueue 0 -> DRIVER_OK), legt einen WRITE-Deskriptor in die DMA-Region und pollt den
//! used-Ring. Das **Gerät** DMAt die Zufallsbytes selbst in die Region — und genau dieser
//! Bus-Master-Zugriff wird von der SMMU (Stage-1, StreamID = PCI-RID) auf die DMA-Region
//! beschränkt. Physadressen in den Deskriptoren stammen aus der DmaCap-Region (backend-direkt,
//! SMMU-erzwungen). Die Virtqueue liegt vollständig in der DMA-Region.

use crate::cpu;
use crate::pcie::{self, PciDevice};

// virtio-pci Capability-Typen (cfg_type im Vendor-Cap 0x09).
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const CAP_ID_VNDR: u8 = 0x09;

// device_status-Bits.
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

// VIRTIO_F_VERSION_1 = Bit 32 (Feature-Select 1, Bit 0).
const VIRTIO_F_VERSION_1_HI: u32 = 1 << 0;
/// `VIRTIO_F_ACCESS_PLATFORM` = Bit 33 (Feature-Select 1, Bit 1).
///
/// Das ist die Zusage des Geräts, Adressen **so zu behandeln, wie die Plattform sie meint** —
/// also durch die IOMMU zu laufen statt physisch am Kernel vorbei. Ohne dieses Bit greift ein
/// emuliertes virtio-Gerät in QEMU direkt auf `address_space_memory` zu und ignoriert die SMMU,
/// auch wenn `iommu=smmuv3` gesetzt ist. Solange IOVA == PA war, fiel das nicht auf; seit ext-36
/// Schritt b ist es der Unterschied zwischen „funktioniert" und „liest Müll oberhalb des RAM".
/// Der Treiber verlangt es deshalb **verbindlich** und bricht sonst ab, statt still auf
/// physische Adressen zurückzufallen — genau dieser Rückfall wäre die Achsenverwechslung.
const VIRTIO_F_ACCESS_PLATFORM_HI: u32 = 1 << 1;

// virtq-Deskriptor-Flag: Gerät schreibt in den Puffer.
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// common_cfg-Register-Offsets (virtio_pci_common_cfg).
mod cc {
    #[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    #[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
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
const OFF_DESC: u64 = 0x000; // Deskriptor-Tabelle (Q_SIZE * 16)
const OFF_AVAIL: u64 = 0x100; // avail-Ring
const OFF_USED: u64 = 0x200; // used-Ring
const OFF_DATA: u64 = 0x800; // Datenpuffer (Gerät DMAt hierher)
const DATA_LEN: u32 = 64;
/// Offset des Datenpuffers in der DMA-Region (für den Aufrufer, der die Bytes ausliest).
pub const DATA_OFFSET: u64 = OFF_DATA;
/// Länge des Datenpuffers (Bytes, die das Gerät schreibt) — für die Bounds-Prüfung.
pub const DATA_LEN_BYTES: u32 = DATA_LEN;

#[inline]
unsafe fn rd8(a: u64) -> u8 {
    core::ptr::read_volatile(a as *const u8)
}
#[inline]
unsafe fn rd16(a: u64) -> u16 {
    core::ptr::read_volatile(a as *const u16)
}
#[inline]
unsafe fn rd32(a: u64) -> u32 {
    core::ptr::read_volatile(a as *const u32)
}
#[inline]
unsafe fn wr8(a: u64, v: u8) {
    core::ptr::write_volatile(a as *mut u8, v)
}
#[inline]
unsafe fn wr16(a: u64, v: u16) {
    core::ptr::write_volatile(a as *mut u16, v)
}
#[inline]
unsafe fn wr32(a: u64, v: u32) {
    core::ptr::write_volatile(a as *mut u32, v)
}
#[inline]
unsafe fn wr64(a: u64, v: u64) {
    core::ptr::write_volatile(a as *mut u64, v)
}

/// Aufgelöste Adressen der virtio-Strukturen (im BAR, kernel-Device-gemappt).
pub struct VirtioRng {
    common: u64,      // common_cfg-Basisadresse
    notify_base: u64, // notify-Struktur-Basisadresse
    notify_mul: u32,  // queue_notify_off-Multiplikator
}

/// Die virtio-Capabilities des Geräts parsen und common_cfg + notify lokalisieren. `None`,
/// wenn die nötigen Caps fehlen oder ihr BAR nicht zugewiesen ist.
pub fn probe(dev: &PciDevice) -> Option<VirtioRng> {
    let mut common = 0u64;
    let mut notify_base = 0u64;
    let mut notify_mul = 0u32;
    let mut cap = pcie::cap_ptr(dev) as u16;
    let mut guard = 0;
    while cap != 0 && guard < 48 {
        guard += 1;
        let id = pcie::cfg_read8(dev.bus, dev.dev, dev.func, cap);
        let next = pcie::cfg_read8(dev.bus, dev.dev, dev.func, cap + 1) as u16;
        if id == CAP_ID_VNDR {
            let cfg_type = pcie::cfg_read8(dev.bus, dev.dev, dev.func, cap + 3);
            let bar = pcie::cfg_read8(dev.bus, dev.dev, dev.func, cap + 4) as usize;
            let offset = pcie::cfg_read32(dev.bus, dev.dev, dev.func, cap + 8);
            let bar_base = if bar < 6 { dev.bars[bar] } else { 0 };
            if bar_base != 0 {
                let addr = bar_base + offset as u64;
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => common = addr,
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        notify_base = addr;
                        notify_mul = pcie::cfg_read32(dev.bus, dev.dev, dev.func, cap + 16);
                    }
                    _ => {}
                }
            }
        }
        cap = next;
    }
    if common == 0 || notify_base == 0 {
        return None;
    }
    Some(VirtioRng {
        common,
        notify_base,
        notify_mul,
    })
}

impl VirtioRng {
    unsafe fn status(&self, v: u8) {
        wr8(self.common + cc::DEVICE_STATUS, v);
    }
    unsafe fn get_status(&self) -> u8 {
        rd8(self.common + cc::DEVICE_STATUS)
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
    pub unsafe fn request_polled(
        &self,
        cpu_base: u64,
        dev_base: u64,
        desc_addr: u64,
        max_poll: u64,
    ) -> (bool, u32) {
        // CPU-Sicht: hier schreibt/liest der Treiber die Ringe.
        let desc = cpu_base + OFF_DESC;
        let avail = cpu_base + OFF_AVAIL;
        let used = cpu_base + OFF_USED;
        // Gerätesicht: das kommt in die Queue-Adressregister.
        let dev_desc = dev_base + OFF_DESC;
        let dev_avail = dev_base + OFF_AVAIL;
        let dev_used = dev_base + OFF_USED;

        // 1. Reset.
        self.status(0);
        for _ in 0..100_000 {
            if self.get_status() == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // 2./3. ACKNOWLEDGE + DRIVER.
        self.status(STATUS_ACK);
        self.status(STATUS_ACK | STATUS_DRIVER);
        // 4. Feature-Negotiation: VIRTIO_F_VERSION_1 + VIRTIO_F_ACCESS_PLATFORM. Letzteres muss
        // das Gerät anbieten — bietet es das nicht an, würde es die IOMMU umgehen, und der
        // Treiber hätte keine Möglichkeit, das zu bemerken. Also hier abbrechen.
        wr32(self.common + cc::DEVICE_FEATURE_SELECT, 1);
        let offered_hi = rd32(self.common + cc::DEVICE_FEATURE);
        if offered_hi & VIRTIO_F_ACCESS_PLATFORM_HI == 0 {
            return (false, 0);
        }
        wr32(self.common + cc::DRIVER_FEATURE_SELECT, 0);
        wr32(self.common + cc::DRIVER_FEATURE, 0);
        wr32(self.common + cc::DRIVER_FEATURE_SELECT, 1);
        wr32(
            self.common + cc::DRIVER_FEATURE,
            VIRTIO_F_VERSION_1_HI | VIRTIO_F_ACCESS_PLATFORM_HI,
        );
        // 5. FEATURES_OK.
        self.status(STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        if self.get_status() & STATUS_FEATURES_OK == 0 {
            return (false, 0); // Gerät akzeptiert die Features nicht
        }
        // 6. Virtqueue 0 einrichten.
        wr16(self.common + cc::QUEUE_SELECT, 0);
        let qmax = rd16(self.common + cc::QUEUE_SIZE);
        let qsize = if qmax == 0 || qmax >= Q_SIZE { Q_SIZE } else { qmax };
        wr16(self.common + cc::QUEUE_SIZE, qsize);
        wr64(self.common + cc::QUEUE_DESC, dev_desc);
        wr64(self.common + cc::QUEUE_DRIVER, dev_avail);
        wr64(self.common + cc::QUEUE_DEVICE, dev_used);
        let notify_off = rd16(self.common + cc::QUEUE_NOTIFY_OFF);
        wr16(self.common + cc::QUEUE_ENABLE, 1);
        // 7. DRIVER_OK.
        self.status(STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);

        // 8. used-Ring-Ausgangsstand merken, Deskriptor + avail-Ring bauen.
        let used_idx0 = rd16(used + 2);
        wr64(desc, desc_addr); //          desc[0].addr = Zielpuffer-Physadresse
        wr32(desc + 8, DATA_LEN); //       desc[0].len
        wr16(desc + 12, VIRTQ_DESC_F_WRITE); // desc[0].flags = Gerät schreibt
        wr16(desc + 14, 0); //             desc[0].next
        wr16(avail + 4, 0); //             avail.ring[0] = Deskriptor 0
        cpu::dsb_sy();
        wr16(avail + 2, 1); //             avail.idx = 1 (Anfrage sichtbar)
        cpu::dsb_sy();
        // 9. Notify: queue_notify_off * multiplier.
        let notify_addr = self.notify_base + (notify_off as u64) * (self.notify_mul as u64);
        wr16(notify_addr, 0);
        cpu::dsb_sy();

        // 10. used-Ring pollen (das Gerät DMAt die Bytes + advanced used.idx). Grosszuegige
        // Obergrenze: der Poll bricht normal nach wenigen tausend Iterationen ab (Gerät hat
        // geantwortet); die hohe Schranke greift nur im seltenen TCG-Timing-Jitter-Fall, in dem
        // das emulierte Geraet spaeter fertig wird (Burn-in #1: ~1/2000 Laeufe `used_adv=false`).
        // Worst Case dann ~hunderte ms Busy-Wait statt eines Schein-Fehlschlags.
        for _ in 0..max_poll {
            if rd16(used + 2) != used_idx0 {
                let len = rd32(used + 8); // used.ring[0].len
                return (true, len);
            }
            core::hint::spin_loop();
        }
        (false, 0)
    }
}
