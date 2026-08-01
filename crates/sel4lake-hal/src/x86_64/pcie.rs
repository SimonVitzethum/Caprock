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

/// Bus-Master-Bit im Command-Register (der Kernel braucht es für die Save/Restore-Semantik).
pub const CMD_BUS_MASTER_BIT: u16 = CMD_BUS_MASTER;

fn rid_parts(rid: u32) -> (u8, u8, u8) {
    (
        (rid >> 8) as u8,
        ((rid >> 3) & 0x1f) as u8,
        (rid & 0x7) as u8,
    )
}

/// Command (16 bit) und Status (16 bit) teilen sich das Wort bei Offset 0x04. Die Status-Hälfte
/// wird als 0 zurückgeschrieben (write-1-to-clear), damit der Zugriff keine Fehlerbits quittiert.
fn write_cmd(bus: u8, dev: u8, func: u8, cmd: u16) {
    cfg_write32(bus, dev, func, CFG_COMMAND, cmd as u32);
}

/// Bus-Master **löschen**; gibt das vorherige Command-Register zurück (Save/Restore, nie
/// „auf 1 setzen"). Das Spülen ist ein eigener Schritt: [`flush_posted_writes`].
///
/// Zwei Schritte, die verschiedene Dinge tun und einander **nicht** ersetzen:
/// 1. Bus-Master löschen — danach keine **neuen** Memory-Requests mehr.
/// 2. Ein **Read vom selben Gerät** — PCIe garantiert, dass eine Completion posted Writes nicht
///    überholt, die zuvor abgesetzten sind danach also zugestellt.
///
/// Gewählt ist ein **Config-Read**, weil zum Teardown-Zeitpunkt kein BAR garantiert gemappt ist.
/// Der verbreitetere Idiom ist ein **MMIO-Read aus einem BAR**: die Ordnungsgarantie gilt für
/// beide, aber Config-Space ist in manchen Endpoints über einen separaten Pfad implementiert —
/// wo ein BAR sicher verfügbar ist, wäre der MMIO-Read die stärkere Wahl.
///
/// **Grenzen.** Gilt nur, solange *Relaxed Ordering* / *ID-Based Ordering* für diese Funktion
/// nicht aktiv sind, und setzt voraus, dass das Gerät `BME` respektiert — ein **kompromittiertes**
/// tut das nicht. Gegen das wirkt allein das Entfernen der Übersetzung (STE/Stage-1).
///
/// **Die Verschachtelungs-Buchhaltung liegt NICHT hier**, sondern im Übersetzungskontext des
/// Kernels (`DmaCtx.quiesce_depth`, parallel zur StreamID-Liste): dort ist sie durch denselben
/// Lock geschützt wie die RID selbst und kann nicht überlaufen, weil die Kapazität dieselbe
/// Quelle hat wie die Kontextobergrenze.
pub fn clear_bus_master(rid: u32) -> u16 {
    let (bus, dev, func) = rid_parts(rid);
    let cmd = cfg_read16(bus, dev, func, CFG_COMMAND);
    write_cmd(bus, dev, func, cmd & !CMD_BUS_MASTER);
    cmd
}

/// Bereits abgesetzte Writes dieses Geräts spülen (Schritt 2, s. [`clear_bus_master`]).
///
/// **Getrennt** vom Entwaffnen, damit ein Aufrufer mit mehreren Geräten erst **alle**
/// entwaffnen und dann **alle** spülen kann: so ist kein Gerät mehr scharf, während ein anderes
/// noch spült.
pub fn flush_posted_writes(rid: u32) -> u16 {
    let (bus, dev, func) = rid_parts(rid);
    cfg_read16(bus, dev, func, CFG_VENDOR)
}

/// Das Command-Register lesen (Rücklesen nach einem Schreibzugriff).
pub fn read_command(rid: u32) -> u16 {
    let (bus, dev, func) = rid_parts(rid);
    cfg_read16(bus, dev, func, CFG_COMMAND)
}

/// Ein Command-Register unverändert zurückschreiben.
pub fn write_command(rid: u32, cmd: u16) {
    let (bus, dev, func) = rid_parts(rid);
    write_cmd(bus, dev, func, cmd);
}

/// Die **PCI-Topologie** in `out` einlesen (Schritt 2 der VT-d-Zuteilung).
///
/// Liefert reine Daten (`dmar::DevNode`), damit die Gruppenbildung gegen eine eingespeiste
/// Topologie geprüft werden kann. Erfasst wird, was für Isolation und Aliasing zählt: ist der
/// Knoten eine Bridge (und welchen Bus überspannt sie), ist er PCIe oder konventionell, trägt er
/// ACS mit den vier relevanten Fähigkeiten, ist er mehrfunktional — und wer sein Elternteil ist.
pub fn read_topology(out: &mut [super::dmar::DevNode]) -> usize {
    let mut n = 0;
    // Erst alle Knoten sammeln, dann die Elternbeziehung über die Busbereiche der Bridges.
    for bus in 0..=255u16 {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                if n >= out.len() {
                    return n;
                }
                let b = bus as u8;
                let vendor = cfg_read16(b, dev, func, CFG_VENDOR);
                if vendor == 0xFFFF {
                    if func == 0 {
                        break; // Funktion 0 fehlt -> das Gerät gibt es nicht
                    }
                    continue;
                }
                let hdr = cfg_read8(b, dev, func, 0x0E);
                let bridge = hdr & 0x7f == 1;
                let node = super::dmar::DevNode {
                    segment: 0, // ECAM-Segment 0 (s. `nonzero_segment` in der DMAR-Auswertung)
                    bus: b,
                    dev,
                    func,
                    bridge,
                    sec_bus: if bridge { cfg_read8(b, dev, func, 0x19) } else { 0 },
                    sub_bus: if bridge { cfg_read8(b, dev, func, 0x1A) } else { 0 },
                    pcie: has_cap(b, dev, func, 0x10),
                    acs: acs_enabled(b, dev, func),
                    multifunction: hdr & 0x80 != 0,
                    parent: usize::MAX,
                };
                out[n] = node;
                n += 1;
                if func == 0 && hdr & 0x80 == 0 {
                    break; // kein Multifunktionsgerät
                }
            }
        }
    }
    // Elternbeziehung: die Bridge, deren [sec_bus, sub_bus] den Bus des Knotens enthält und
    // dabei den engsten Bereich hat (verschachtelte Bridges).
    for i in 0..n {
        let mut best = usize::MAX;
        let mut best_span = u16::MAX;
        for j in 0..n {
            if i == j || !out[j].bridge {
                continue;
            }
            if out[i].bus >= out[j].sec_bus && out[i].bus <= out[j].sub_bus {
                let span = out[j].sub_bus as u16 - out[j].sec_bus as u16;
                if span < best_span {
                    best_span = span;
                    best = j;
                }
            }
        }
        out[i].parent = best;
    }
    n
}

/// Trägt das Gerät die Capability `id` in der Standard-Capability-Liste?
fn has_cap(bus: u8, dev: u8, func: u8, id: u8) -> bool {
    if cfg_read16(bus, dev, func, 0x06) & (1 << 4) == 0 {
        return false; // keine Capability-Liste
    }
    let mut off = cfg_read8(bus, dev, func, 0x34) & 0xfc;
    for _ in 0..48 {
        if off < 0x40 {
            return false;
        }
        let cap = cfg_read8(bus, dev, func, off as u16);
        if cap == id {
            return true;
        }
        off = cfg_read8(bus, dev, func, off as u16 + 1) & 0xfc;
    }
    false
}

/// **ACS** mit den vier für die Isolationsgranularität relevanten Fähigkeiten aktiv?
///
/// Source Validation, Translation Blocking, P2P Request Redirect und Upstream Forwarding. Fehlt
/// eine davon, können Geräte unterhalb dieser Bridge an der IOMMU vorbei miteinander reden — die
/// Gruppe erstreckt sich dann über die Bridge hinaus. Geprüft wird das **Control**-Register:
/// vorhanden, aber abgeschaltet ist dasselbe wie nicht vorhanden.
fn acs_enabled(bus: u8, dev: u8, func: u8) -> bool {
    const ACS_EXT_CAP_ID: u16 = 0x000D;
    const NEEDED: u16 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 4); // SV, TB, RR, UF
    let mut off: u16 = 0x100;
    for _ in 0..48 {
        let hdr = cfg_read32(bus, dev, func, off);
        if hdr == 0 || hdr == 0xFFFF_FFFF {
            return false;
        }
        if (hdr & 0xffff) as u16 == ACS_EXT_CAP_ID {
            let ctrl = cfg_read16(bus, dev, func, off + 6);
            return ctrl & NEEDED == NEEDED;
        }
        let next = ((hdr >> 20) & 0xfff) as u16;
        if next < 0x100 {
            return false;
        }
        off = next;
    }
    false
}

/// Bus-Master aktivieren (nach dem Installieren einer Übersetzung).
pub fn arm_bus_master(rid: u32) {
    let (bus, dev, func) = rid_parts(rid);
    let cmd = cfg_read16(bus, dev, func, CFG_COMMAND);
    write_cmd(bus, dev, func, cmd | CMD_BUS_MASTER);
}

/// **Zeiger auf die erste PCI-Capability** (Konfigurationsoffset `0x34`), oder `0`.
///
/// Gebraucht von [`crate::virtio`], das seine Strukturen ausschliesslich ueber die
/// Capability-Liste findet. Fehlte auf x86, solange der virtio-Treiber unter `aarch64/` lag
/// (A-5.2) — die ARM-Fassung hat die Funktion seit jeher.
pub fn cap_ptr(d: &PciDevice) -> u8 {
    // Nur gueltig, wenn das Status-Register die Capability-Liste ueberhaupt meldet (Bit 4);
    // sonst steht an 0x34 Muell, und die Liste liefe in eine erfundene Kette.
    if cfg_read16(d.bus, d.dev, d.func, 0x06) & (1 << 4) == 0 {
        return 0;
    }
    cfg_read8(d.bus, d.dev, d.func, 0x34) & 0xfc
}
