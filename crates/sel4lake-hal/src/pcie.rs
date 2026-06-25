//! Minimaler **PCIe-ECAM**-Treiber (ext-23, D1) für QEMU `virt` (`pci-host-ecam-generic`).
//!
//! Wird kernel-/Trusted-seitig genutzt, um das DMA-Beweisgerät (`virtio-rng-pci`) zu finden,
//! ihm eine BAR zuzuweisen, Memory-Space + **Bus-Master** zu aktivieren und seine **RID**
//! (= SMMU-StreamID) zu bestimmen. Es ist **kein** User-Pfad: ein HardwareLand-Backend erhält
//! anschließend nur `MmioCap(BAR)` + `DmaCap` + (optional) `IrqCap`.
//!
//! Adress-Fakten (QEMU 11 `virt`, via DTB verifiziert):
//! - ECAM `@0x40_1000_0000` (Config-Space, `bus<<20 | dev<<15 | fn<<12 | off`).
//! - 32-bit-MMIO-Fenster (BAR-Zuweisung): CPU `0x1000_0000 .. 0x3eff_0000` (in GiB 0,
//!   bereits EL1-Device in der globalen Kernel-Map).
//! - RID == StreamID (identitäts-`iommu-map` der SMMU).

use core::sync::atomic::{AtomicU64, Ordering};

/// ECAM-Basis (Config-Space) auf QEMU `virt` (High-ECAM).
pub const ECAM_BASE: u64 = 0x40_1000_0000;
/// GiB-Index der ECAM-Region (für das globale Device-Mapping).
pub const ECAM_GIB: usize = (ECAM_BASE / (1 << 30)) as usize; // 256

/// 32-bit-MMIO-Fenster (BAR-Zuweisung).
const MMIO32_BASE: u64 = 0x1000_0000;
const MMIO32_END: u64 = 0x3eff_0000;
/// Bump-Allokator über das 32-bit-MMIO-Fenster (BAR-Vergabe, monoton).
static MMIO32_NEXT: AtomicU64 = AtomicU64::new(MMIO32_BASE);

/// Red-Hat/virtio PCI-Vendor-ID.
pub const VIRTIO_VENDOR: u16 = 0x1af4;

// --- PCI-Config-Space-Offsets ---
const CFG_VENDOR: u16 = 0x00;
const CFG_DEVICE: u16 = 0x02;
const CFG_COMMAND: u16 = 0x04;
const CFG_CLASS: u16 = 0x08; // [31:8] = class/subclass/prog-if, [7:0] = revision
const CFG_HEADER_TYPE: u16 = 0x0e;
const CFG_BAR0: u16 = 0x10;
const CFG_CAP_PTR: u16 = 0x34;
const CFG_INT_PIN: u16 = 0x3d;

// Command-Register-Bits.
const CMD_MEM_SPACE: u16 = 1 << 1;
const CMD_BUS_MASTER: u16 = 1 << 2;

/// Ein enumeriertes PCI(e)-Gerät mit zugewiesenen BARs.
#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u32, // class/subclass/prog-if (Bits 31:8 des CFG_CLASS-Worts)
    pub int_pin: u8,
    /// Zugewiesene BAR-Basisadressen (`0` = keine/unbenutzt). 64-bit-BARs belegen zwei Slots
    /// (`bars[i]` = volle 64-bit-Adresse, `bars[i+1]` = 0 als Platzhalter).
    pub bars: [u64; 6],
    /// Größe je BAR (Bytes); `0` = keine.
    pub bar_size: [u64; 6],
}

impl PciDevice {
    /// Die **RID** (Requester-ID) = SMMU-StreamID dieses Geräts (identitäts-`iommu-map`).
    pub fn rid(&self) -> u32 {
        ((self.bus as u32) << 8) | ((self.dev as u32) << 3) | (self.func as u32)
    }
}

#[inline]
fn cfg_addr(bus: u8, dev: u8, func: u8, off: u16) -> u64 {
    ECAM_BASE
        + ((bus as u64) << 20)
        + ((dev as u64) << 15)
        + ((func as u64) << 12)
        + (off as u64 & 0xfff)
}

/// 32-bit-Config-Read. SAFETY: ECAM ist global EL1-Device-gemappt (s. `map_device_block_global`).
pub fn cfg_read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
    unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u32) }
}
/// 32-bit-Config-Write.
pub fn cfg_write32(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
    unsafe { core::ptr::write_volatile(cfg_addr(bus, dev, func, off) as *mut u32, val) }
}
pub fn cfg_read16(bus: u8, dev: u8, func: u8, off: u16) -> u16 {
    let w = cfg_read32(bus, dev, func, off & !0x3);
    (w >> ((off as u32 & 0x2) * 8)) as u16
}
fn cfg_write16(bus: u8, dev: u8, func: u8, off: u16, val: u16) {
    let aligned = off & !0x3;
    let shift = (off as u32 & 0x2) * 8;
    let mut w = cfg_read32(bus, dev, func, aligned);
    w &= !(0xffffu32 << shift);
    w |= (val as u32) << shift;
    cfg_write32(bus, dev, func, aligned, w);
}
pub fn cfg_read8(bus: u8, dev: u8, func: u8, off: u16) -> u8 {
    let w = cfg_read32(bus, dev, func, off & !0x3);
    (w >> ((off as u32 & 0x3) * 8)) as u8
}

/// Bus 0 nach dem ersten Gerät mit Vendor `vendor` durchsuchen. BARs werden dabei
/// dimensioniert + im 32-bit-Fenster zugewiesen, Memory-Space + Bus-Master aktiviert.
/// `None`, wenn kein passendes Gerät gefunden wird.
pub fn find_by_vendor(vendor: u16) -> Option<PciDevice> {
    for dev in 0u8..32 {
        let v = cfg_read16(0, dev, 0, CFG_VENDOR);
        if v == 0xffff || v == 0x0000 {
            continue; // kein Gerät in diesem Slot
        }
        if v != vendor {
            continue;
        }
        let mut d = PciDevice {
            bus: 0,
            dev,
            func: 0,
            vendor: v,
            device: cfg_read16(0, dev, 0, CFG_DEVICE),
            class: cfg_read32(0, dev, 0, CFG_CLASS) >> 8,
            int_pin: cfg_read8(0, dev, 0, CFG_INT_PIN),
            bars: [0; 6],
            bar_size: [0; 6],
        };
        assign_bars(&mut d);
        enable_mem_and_bus_master(&d);
        return Some(d);
    }
    None
}

/// Alle BARs eines Geräts dimensionieren und Memory-BARs im 32-bit-Fenster zuweisen.
fn assign_bars(d: &mut PciDevice) {
    let mut i = 0usize;
    while i < 6 {
        let off = CFG_BAR0 + (i as u16) * 4;
        let orig = cfg_read32(0, d.dev, 0, off);
        // Größe sondieren: all-1s schreiben, Maske zurücklesen, Original wiederherstellen.
        cfg_write32(0, d.dev, 0, off, 0xffff_ffff);
        let mask = cfg_read32(0, d.dev, 0, off);
        cfg_write32(0, d.dev, 0, off, orig);
        if mask == 0 {
            i += 1;
            continue; // BAR nicht implementiert
        }
        let is_io = (mask & 0x1) == 1;
        if is_io {
            i += 1; // I/O-BARs ignorieren wir (modernes virtio nutzt Memory-BARs)
            continue;
        }
        let is_64 = (mask & 0b110) == 0b100;
        let size = (!(mask & 0xffff_fff0)).wrapping_add(1) as u64;
        if size == 0 {
            i += 1;
            continue;
        }
        // Im 32-bit-Fenster, größen-ausgerichtet, vergeben.
        let base = {
            let align = size.max(0x1000);
            let mut cur = MMIO32_NEXT.load(Ordering::Relaxed);
            cur = (cur + align - 1) & !(align - 1);
            let next = cur + size;
            if next > MMIO32_END {
                i += if is_64 { 2 } else { 1 };
                continue; // Fenster erschöpft
            }
            MMIO32_NEXT.store(next, Ordering::Relaxed);
            cur
        };
        cfg_write32(0, d.dev, 0, off, (base as u32) & 0xffff_fff0 | (mask & 0xf));
        if is_64 {
            cfg_write32(0, d.dev, 0, off + 4, (base >> 32) as u32);
        }
        d.bars[i] = base;
        d.bar_size[i] = size;
        i += if is_64 { 2 } else { 1 };
    }
}

/// Memory-Space + Bus-Master im Command-Register aktivieren (Bus-Master ist Pflicht für DMA).
fn enable_mem_and_bus_master(d: &PciDevice) {
    let cmd = cfg_read16(0, d.dev, d.func, CFG_COMMAND);
    cfg_write16(
        0,
        d.dev,
        d.func,
        CFG_COMMAND,
        cmd | CMD_MEM_SPACE | CMD_BUS_MASTER,
    );
}

/// Ist Bus-Master aktiv? (Verifikation nach `find_by_vendor`.)
pub fn bus_master_enabled(d: &PciDevice) -> bool {
    cfg_read16(0, d.dev, d.func, CFG_COMMAND) & CMD_BUS_MASTER != 0
}

/// Header-Type des Geräts (Bit 7 = Multifunktion).
pub fn header_type(d: &PciDevice) -> u8 {
    cfg_read8(0, d.dev, d.func, CFG_HEADER_TYPE)
}

/// Capabilities-Pointer (Offset des ersten Capability-Eintrags, 0 = keine).
pub fn cap_ptr(d: &PciDevice) -> u8 {
    cfg_read8(0, d.dev, d.func, CFG_CAP_PTR)
}
