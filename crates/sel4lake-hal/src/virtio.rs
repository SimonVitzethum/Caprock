//! **virtio-pci: das Auffinden der Strukturen** — die Treiberlogik liegt in `sel4lake-virtio`.
//!
//! Was hier bleibt, ist Bus-Enumeration: ein Lauf durch die PCI-Capability-Liste, der
//! `common_cfg` und die Notify-Struktur lokalisiert. Das ist dieselbe Taetigkeit wie
//! `pcie::find` und gehoert aus demselben Grund in den Kern — der Konfigurationsraum ist
//! geraeteweit, wer ihn liest, sieht jedes Geraet der Maschine.
//!
//! Was gegangen ist, ist das **Protokoll** (Handshake, Virtqueue, used-Ring): es haengt an nichts
//! Privilegiertem und gehoert in eine Userland-Treiber-PD (todo A-5.1). Gesetzte Regel (Simon,
//! 2026-08-01): keine Treiber im Mikrokern. Der Schnitt liegt genau dort, wo die Autoritaet
//! aufhoert -- nicht dort, wo es beim Aufteilen bequem war.

use crate::cpu;
use crate::pcie::{self, PciDevice};
pub use sel4lake_virtio::{VirtioRng, DATA_LEN_BYTES, DATA_OFFSET};

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const CAP_ID_VNDR: u8 = 0x09;

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
    // Die Barriere wird HEREINGEREICHT (s. Crate-Doku von `sel4lake-virtio`): auf aarch64 ist
    // `dsb sy` noetig, weil Device-Memory nicht in der inner-shareable Domaene liegt. Eine
    // "arch-neutrale" Barriere haette die Semantik still abgeschwaecht.
    Some(VirtioRng::new(common, notify_base, notify_mul, cpu::dsb_sy))
}

