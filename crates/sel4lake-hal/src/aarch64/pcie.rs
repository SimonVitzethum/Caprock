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
/// virtio-rng PCI-Device-IDs: transitional (0x1005) bzw. modern (0x1040 + 4 = 0x1044). QEMU
/// fügt evtl. eine Default-NIC (virtio-net, 0x1000) hinzu — daher gezielt nach RNG filtern.
pub const VIRTIO_RNG_DEVICES: [u16; 2] = [0x1005, 0x1044];

// --- PCI-Config-Space-Offsets ---
const CFG_VENDOR: u16 = 0x00;
const CFG_DEVICE: u16 = 0x02;
const CFG_COMMAND: u16 = 0x04;
const CFG_CLASS: u16 = 0x08; // [31:8] = class/subclass/prog-if, [7:0] = revision
const CFG_HEADER_TYPE: u16 = 0x0e;
const CFG_BAR0: u16 = 0x10;
const CFG_CAP_PTR: u16 = 0x34;
const CFG_INT_PIN: u16 = 0x3d;
// PCI-zu-PCI-Bridge (Header-Type 1) Bus-/Fenster-Register.
const CFG_PRIMARY_BUS: u16 = 0x18;
const CFG_SECONDARY_BUS: u16 = 0x19;
const CFG_SUBORDINATE_BUS: u16 = 0x1a;
const CFG_IO_BASE: u16 = 0x1c; // I/O-Base(8)/Limit(8)
const CFG_MEM_BASE: u16 = 0x20; // Memory-Base(16)
const CFG_MEM_LIMIT: u16 = 0x22; // Memory-Limit(16)
const CFG_PREF_BASE: u16 = 0x24; // Prefetchable-Base(16)
const CFG_PREF_LIMIT: u16 = 0x26; // Prefetchable-Limit(16)

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
fn cfg_write8(bus: u8, dev: u8, func: u8, off: u16, val: u8) {
    let aligned = off & !0x3;
    let shift = (off as u32 & 0x3) * 8;
    let mut w = cfg_read32(bus, dev, func, aligned);
    w &= !(0xffu32 << shift);
    w |= (val as u32) << shift;
    cfg_write32(bus, dev, func, aligned, w);
}

/// Maximale Bus-Nummer, die durchsucht wird (mit iommu=smmuv3 hängt QEMU Endpunkte hinter
/// Root-Ports auf höheren Bussen ein -> nicht nur Bus 0 absuchen).
const MAX_BUS: u8 = 16;

/// **PCIe-Bridges (Root-Ports) konfigurieren**: jeder Bridge auf Bus 0 einen Sekundärbus
/// zuweisen (Primary/Secondary/Subordinate) und ihr Memory-Window auf das 32-bit-MMIO-Fenster
/// setzen + Memory-Forwarding einschalten — sonst sind Endpunkte dahinter weder per ECAM
/// erreichbar noch ihre BARs zugänglich. Nötig, weil QEMUs SMMUv3 nur Endpunkte **hinter einem
/// Root-Port** übersetzt (integrierte Bus-0-Endpunkte umgehen die SMMU). Einstufig (Root-Ports
/// auf Bus 0, Endpunkte direkt dahinter) — für tiefere Topologien später erweiterbar.
fn configure_bridges() {
    let mut next_bus: u8 = 1;
    for dev in 0u8..32 {
        let v = cfg_read16(0, dev, 0, CFG_VENDOR);
        if v == 0xffff || v == 0x0000 {
            continue;
        }
        let htype = cfg_read8(0, dev, 0, CFG_HEADER_TYPE) & 0x7f;
        if htype != 1 {
            continue; // kein Bridge
        }
        let sec = next_bus;
        next_bus += 1;
        // Bus-Nummern: primary=0, secondary=sec, subordinate=sec (einstufig).
        cfg_write8(0, dev, 0, CFG_PRIMARY_BUS, 0);
        cfg_write8(0, dev, 0, CFG_SECONDARY_BUS, sec);
        cfg_write8(0, dev, 0, CFG_SUBORDINATE_BUS, sec);
        // Memory-Window auf das 32-bit-MMIO-Fenster (Einheiten: 1 MiB, Bits[15:4]=Addr[31:20]).
        cfg_write16(0, dev, 0, CFG_MEM_BASE, ((MMIO32_BASE >> 16) & 0xfff0) as u16);
        cfg_write16(0, dev, 0, CFG_MEM_LIMIT, ((MMIO32_END >> 16) & 0xfff0) as u16);
        // Prefetchable + I/O deaktivieren (Base > Limit).
        cfg_write16(0, dev, 0, CFG_PREF_BASE, 0xfff0);
        cfg_write16(0, dev, 0, CFG_PREF_LIMIT, 0x0000);
        cfg_write16(0, dev, 0, CFG_IO_BASE, 0x00f0);
        // Memory-Space + Bus-Master am Bridge einschalten (Forwarding).
        let cmd = cfg_read16(0, dev, 0, CFG_COMMAND);
        cfg_write16(0, dev, 0, CFG_COMMAND, cmd | CMD_MEM_SPACE | CMD_BUS_MASTER);
    }
}

/// Alle Busse `0..MAX_BUS` nach dem ersten Gerät mit Vendor `vendor` und (falls `devices` nicht
/// leer) passender Device-ID durchsuchen. Konfiguriert zuvor die Bridges (Root-Ports), damit
/// Endpunkte dahinter sichtbar werden. BARs werden dimensioniert + im 32-bit-Fenster zugewiesen,
/// Memory-Space + Bus-Master aktiviert. `None`, wenn nichts Passendes existiert.
pub fn find(vendor: u16, devices: &[u16]) -> Option<PciDevice> {
    configure_bridges();
    for bus in 0u8..MAX_BUS {
        for dev in 0u8..32 {
            let v = cfg_read16(bus, dev, 0, CFG_VENDOR);
            if v == 0xffff || v == 0x0000 {
                continue; // kein Gerät in diesem Slot
            }
            if v != vendor {
                continue;
            }
            let did = cfg_read16(bus, dev, 0, CFG_DEVICE);
            if !devices.is_empty() && !devices.contains(&did) {
                continue; // Vendor passt, aber falscher Gerätetyp (z.B. Default-NIC)
            }
            let mut d = PciDevice {
                bus,
                dev,
                func: 0,
                vendor: v,
                device: did,
                class: cfg_read32(bus, dev, 0, CFG_CLASS) >> 8,
                int_pin: cfg_read8(bus, dev, 0, CFG_INT_PIN),
                bars: [0; 6],
                bar_size: [0; 6],
            };
            assign_bars(&mut d);
            enable_mem_and_bus_master(&d);
            return Some(d);
        }
    }
    None
}

/// Alle Busse `0..MAX_BUS` durchsuchen und jedes vorhandene Gerät über `f(bus, dev, vendor,
/// device, class)` melden (Diagnose der PCIe-Topologie). Konfiguriert zuvor die Bridges.
pub fn dump_devices(f: &mut dyn FnMut(u8, u8, u16, u16, u32)) {
    configure_bridges();
    for bus in 0u8..MAX_BUS {
        for dev in 0u8..32 {
            let v = cfg_read16(bus, dev, 0, CFG_VENDOR);
            if v == 0xffff || v == 0x0000 {
                continue;
            }
            let did = cfg_read16(bus, dev, 0, CFG_DEVICE);
            let class = cfg_read32(bus, dev, 0, CFG_CLASS) >> 8;
            f(bus, dev, v, did, class);
        }
    }
}

/// Alle BARs eines Geräts dimensionieren und Memory-BARs im 32-bit-Fenster zuweisen.
fn assign_bars(d: &mut PciDevice) {
    let mut i = 0usize;
    while i < 6 {
        let off = CFG_BAR0 + (i as u16) * 4;
        let orig = cfg_read32(d.bus, d.dev, 0, off);
        // Größe sondieren: all-1s schreiben, Maske zurücklesen, Original wiederherstellen.
        cfg_write32(d.bus, d.dev, 0, off, 0xffff_ffff);
        let mask = cfg_read32(d.bus, d.dev, 0, off);
        cfg_write32(d.bus, d.dev, 0, off, orig);
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
        cfg_write32(d.bus, d.dev, 0, off, (base as u32) & 0xffff_fff0 | (mask & 0xf));
        if is_64 {
            cfg_write32(d.bus, d.dev, 0, off + 4, (base >> 32) as u32);
        }
        d.bars[i] = base;
        d.bar_size[i] = size;
        i += if is_64 { 2 } else { 1 };
    }
}

/// Memory-Space + Bus-Master im Command-Register aktivieren (Bus-Master ist Pflicht für DMA).
fn enable_mem_and_bus_master(d: &PciDevice) {
    let cmd = cfg_read16(d.bus, d.dev, d.func, CFG_COMMAND);
    cfg_write16(
        d.bus,
        d.dev,
        d.func,
        CFG_COMMAND,
        cmd | CMD_MEM_SPACE | CMD_BUS_MASTER,
    );
}

/// Ist Bus-Master aktiv? (Verifikation nach `find`.)
pub fn bus_master_enabled(d: &PciDevice) -> bool {
    cfg_read16(d.bus, d.dev, d.func, CFG_COMMAND) & CMD_BUS_MASTER != 0
}

/// Header-Type des Geräts (Bit 7 = Multifunktion).
pub fn header_type(d: &PciDevice) -> u8 {
    cfg_read8(d.bus, d.dev, d.func, CFG_HEADER_TYPE)
}

/// Capabilities-Pointer (Offset des ersten Capability-Eintrags, 0 = keine).
pub fn cap_ptr(d: &PciDevice) -> u8 {
    cfg_read8(d.bus, d.dev, d.func, CFG_CAP_PTR)
}

/// Ein Gerät über seine **RID** (= StreamID) stilllegen und bereits abgesetzte Writes spülen.
///
/// Zwei Schritte, die verschiedene Dinge tun und einander **nicht** ersetzen:
///
/// 1. **Bus-Master löschen.** Danach darf das Gerät keine neuen Memory-Requests mehr absetzen.
///    Über bereits unterwegs befindliche (posted) Writes sagt das nichts — die sind abgeschickt.
/// 2. **Config-Read vom selben Gerät.** PCIe garantiert, dass eine Completion posted Writes
///    nicht überholt: die Antwort auf diesen nicht-posted Read trifft erst ein, nachdem die
///    zuvor vom Gerät abgesetzten Writes zugestellt sind. Der Rückgabewert ist belanglos —
///    der Zweck ist die Ordnungsgarantie.
///
/// **Grenzen.** Das gilt nur, solange Relaxed Ordering / ID-Based Ordering für diese Funktion
/// nicht aktiv sind und der Pfad einheitlich ist. Und es setzt voraus, dass das Gerät `BME`
/// respektiert — ein **kompromittiertes** tut das nicht. Gegen das bösartige Gerät wirkt allein
/// das Entfernen der Übersetzung (STE/Stage-1); gegen das gutartige mit In-flight-Writes wirkt
/// allein dieser Schritt. Keiner ersetzt den anderen.
///
/// Gibt den vorherigen Inhalt des Command-Registers zurück ([`restore_command`]).
pub fn quiesce_by_rid(rid: u32) -> u16 {
    let (bus, dev, func) = (
        (rid >> 8) as u8,
        ((rid >> 3) & 0x1f) as u8,
        (rid & 0x7) as u8,
    );
    let cmd = cfg_read16(bus, dev, func, CFG_COMMAND);
    cfg_write16(bus, dev, func, CFG_COMMAND, cmd & !CMD_BUS_MASTER);
    let _ = cfg_read16(bus, dev, func, CFG_VENDOR); // Flush-Read (s. o.)
    cmd
}

/// Das Command-Register eines Geräts wiederherstellen (nach [`quiesce_by_rid`]).
pub fn restore_command(rid: u32, cmd: u16) {
    let (bus, dev, func) = (
        (rid >> 8) as u8,
        ((rid >> 3) & 0x1f) as u8,
        (rid & 0x7) as u8,
    );
    cfg_write16(bus, dev, func, CFG_COMMAND, cmd);
}
