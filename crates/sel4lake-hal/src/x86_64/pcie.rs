//! PCI(e)-Konfigurationsraum über **ECAM** (x86_64) — API-gleich zum aarch64-Modul.
//!
//! Zwei Unterschiede zur ARM-Fassung, beide von der Plattform vorgegeben:
//!
//! * **Wo das ECAM-Fenster liegt**, steht auf ARM im Device Tree und ist auf dem `virt`-Board
//!   fest; hier kommt es aus der ACPI-**MCFG** (s. [`super::acpi`]) und wird beim ersten
//!   Zugriff einmalig übernommen.
//! * **Wer die BARs vergibt.** Auf `virt` tut das der Kernel selbst (die ARM-Fassung
//!   dimensioniert BARs und konfiguriert Root-Ports). Auf dem PC hat das die Firmware
//!   (SeaBIOS/UEFI) beim Boot bereits erledigt — hier werden die zugewiesenen Werte nur
//!   **gelesen**. Das ist kein Sonderweg, sondern die übliche Rollenverteilung auf dieser
//!   Plattform.

use core::sync::atomic::{AtomicU64, Ordering};

/// Basis des ECAM-Fensters (`0` = noch nicht ermittelt).
static ECAM_BASE: AtomicU64 = AtomicU64::new(0);
/// Höchster zu durchsuchender Bus (aus der MCFG; Default konservativ).
static MAX_BUS: AtomicU64 = AtomicU64::new(0);

/// Vendor-ID von virtio-Geräten (wie auf ARM).
pub const VIRTIO_VENDOR: u16 = 0x1af4;
/// Device-IDs des virtio-RNG (legacy + modern).
pub const VIRTIO_RNG_DEVICES: [u16; 2] = [0x1005, 0x1044];

// Konfigurationsraum-Offsets.
const CFG_VENDOR: u16 = 0x00;
const CFG_DEVICE: u16 = 0x02;
const CFG_COMMAND: u16 = 0x04;
const CFG_CLASS: u16 = 0x08;
const CFG_BAR0: u16 = 0x10;
const CFG_INT_PIN: u16 = 0x3D;
const CMD_BUS_MASTER: u16 = 1 << 2;

/// Ein enumeriertes PCI(e)-Gerät.
#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u32,
    pub int_pin: u8,
    /// Von der Firmware zugewiesene BAR-Basisadressen (`0` = unbenutzt).
    pub bars: [u64; 6],
}

impl PciDevice {
    /// **RID** (Requester-ID) — die Identität, unter der das Gerät DMA anfordert und die die
    /// IOMMU zur Übersetzung heranzieht (auf ARM die SMMU-StreamID).
    pub fn rid(&self) -> u32 {
        ((self.bus as u32) << 8) | ((self.dev as u32) << 3) | (self.func as u32)
    }
}

/// ECAM-Fenster aus der ACPI-MCFG übernehmen (idempotent). `false`, wenn es keine MCFG gibt.
pub fn init() -> bool {
    if ECAM_BASE.load(Ordering::Acquire) != 0 {
        return true;
    }
    let Some((base, start, end)) = super::acpi::pci_ecam() else {
        return false;
    };
    let _ = start;
    MAX_BUS.store(end as u64, Ordering::Release);
    ECAM_BASE.store(base, Ordering::Release);
    true
}

fn cfg_addr(bus: u8, dev: u8, func: u8, off: u16) -> Option<u64> {
    let base = ECAM_BASE.load(Ordering::Acquire);
    if base == 0 {
        return None;
    }
    Some(base + ((bus as u64) << 20) + ((dev as u64) << 15) + ((func as u64) << 12) + (off as u64 & 0xfff))
}

/// 32-bit-Konfigurationslesen. `0xffff_ffff` (= „kein Gerät"), solange kein ECAM bekannt ist.
pub fn cfg_read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
    match cfg_addr(bus, dev, func, off) {
        // SAFETY: Das ECAM-Fenster stammt aus der ACPI-MCFG und liegt im identity-gemappten,
        // uncacheable MMIO-Bereich (s. `mmu::init_primary`); volatile Zugriffe auf
        // Konfigurationsregister aliasen keinen Rust-Speicher.
        Some(a) => unsafe { core::ptr::read_volatile(a as *const u32) },
        None => 0xffff_ffff,
    }
}

/// 32-bit-Konfigurationsschreiben (No-Op ohne bekanntes ECAM).
pub fn cfg_write32(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
    if let Some(a) = cfg_addr(bus, dev, func, off) {
        // SAFETY: wie `cfg_read32`.
        unsafe { core::ptr::write_volatile(a as *mut u32, val) };
    }
}

pub fn cfg_read16(bus: u8, dev: u8, func: u8, off: u16) -> u16 {
    (cfg_read32(bus, dev, func, off & !3) >> ((off & 3) * 8)) as u16
}

pub fn cfg_read8(bus: u8, dev: u8, func: u8, off: u16) -> u8 {
    (cfg_read32(bus, dev, func, off & !3) >> ((off & 3) * 8)) as u8
}

/// Ein Gerät vollständig einlesen (BARs wie von der Firmware zugewiesen).
fn read_device(bus: u8, dev: u8, func: u8) -> PciDevice {
    let mut d = PciDevice {
        bus,
        dev,
        func,
        vendor: cfg_read16(bus, dev, func, CFG_VENDOR),
        device: cfg_read16(bus, dev, func, CFG_DEVICE),
        class: cfg_read32(bus, dev, func, CFG_CLASS) >> 8,
        int_pin: cfg_read8(bus, dev, func, CFG_INT_PIN),
        bars: [0; 6],
    };
    let mut i = 0;
    while i < 6 {
        let lo = cfg_read32(bus, dev, func, CFG_BAR0 + (i as u16) * 4);
        if lo & 1 != 0 {
            i += 1; // I/O-BAR: hier nicht verwendet
            continue;
        }
        let is64 = (lo >> 1) & 0b11 == 0b10;
        let mut addr = (lo & !0xf) as u64;
        if is64 {
            let hi = cfg_read32(bus, dev, func, CFG_BAR0 + ((i + 1) as u16) * 4);
            addr |= (hi as u64) << 32;
            d.bars[i] = addr;
            i += 2;
        } else {
            d.bars[i] = addr;
            i += 1;
        }
    }
    d
}

/// Jedes vorhandene Gerät über `f(bus, dev, vendor, device, class)` melden.
pub fn dump_devices(f: &mut dyn FnMut(u8, u8, u16, u16, u32)) {
    if !init() {
        return;
    }
    let max = MAX_BUS.load(Ordering::Acquire) as u16;
    for bus in 0..=max.min(255) {
        for dev in 0u8..32 {
            let v = cfg_read16(bus as u8, dev, 0, CFG_VENDOR);
            if v == 0xffff || v == 0 {
                continue;
            }
            let d = read_device(bus as u8, dev, 0);
            f(d.bus, d.dev, d.vendor, d.device, d.class);
        }
    }
}

/// Das erste Gerät mit `vendor` (und, falls `devices` nicht leer, passender Device-ID) suchen
/// und **Bus-Master** aktivieren (ohne das kann es kein DMA anfordern).
pub fn find(vendor: u16, devices: &[u16]) -> Option<PciDevice> {
    if !init() {
        return None;
    }
    let max = MAX_BUS.load(Ordering::Acquire) as u16;
    for bus in 0..=max.min(255) {
        for dev in 0u8..32 {
            let v = cfg_read16(bus as u8, dev, 0, CFG_VENDOR);
            if v != vendor {
                continue;
            }
            let did = cfg_read16(bus as u8, dev, 0, CFG_DEVICE);
            if !devices.is_empty() && !devices.contains(&did) {
                continue;
            }
            let d = read_device(bus as u8, dev, 0);
            let cmd = cfg_read16(d.bus, d.dev, d.func, CFG_COMMAND);
            cfg_write32(
                d.bus,
                d.dev,
                d.func,
                CFG_COMMAND,
                (cmd | CMD_BUS_MASTER) as u32,
            );
            return Some(d);
        }
    }
    None
}

/// Ist Bus-Master für dieses Gerät aktiv?
pub fn bus_master_enabled(d: &PciDevice) -> bool {
    cfg_read16(d.bus, d.dev, d.func, CFG_COMMAND) & CMD_BUS_MASTER != 0
}

/// Ein Gerät über seine **RID** stilllegen und bereits abgesetzte Writes spülen — identisch zur
/// aarch64-Fassung (dort steht die ausführliche Begründung der beiden Schritte und ihrer Grenzen).
///
/// Gibt den vorherigen Inhalt des Command-Registers zurück ([`restore_command`]).
pub fn quiesce_by_rid(rid: u32) -> u16 {
    let (bus, dev, func) = (
        (rid >> 8) as u8,
        ((rid >> 3) & 0x1f) as u8,
        (rid & 0x7) as u8,
    );
    // Command (16 bit) und Status (16 bit) teilen sich das Wort bei Offset 0x04. Status-Bits
    // sind write-1-to-clear -> die obere Hälfte wird als 0 zurückgeschrieben, damit der
    // Schreibzugriff keine Fehlerbits quittiert.
    let cmd = cfg_read16(bus, dev, func, CFG_COMMAND);
    cfg_write32(bus, dev, func, CFG_COMMAND, (cmd & !CMD_BUS_MASTER) as u32);
    let _ = cfg_read16(bus, dev, func, CFG_VENDOR); // Flush-Read
    cmd
}

/// Das Command-Register eines Geräts wiederherstellen (nach [`quiesce_by_rid`]).
pub fn restore_command(rid: u32, cmd: u16) {
    let (bus, dev, func) = (
        (rid >> 8) as u8,
        ((rid >> 3) & 0x1f) as u8,
        (rid & 0x7) as u8,
    );
    cfg_write32(bus, dev, func, CFG_COMMAND, cmd as u32);
}
