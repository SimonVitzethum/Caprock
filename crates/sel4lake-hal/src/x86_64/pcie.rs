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

// --- Quiesce-Buchhaltung je RID (ext-35a) ----------------------------------------------------
//
// Die BME-Wiederherstellung hängt an einer Bedingung, die der naive Ablauf nicht hatte:
// **zwei nebenläufige Teardowns auf demselben Gerät dürfen sich nicht gegenseitig entwaffnen.**
//
// ```text
//   A: BME clear ──flush──┬─ unmap ── tlbi_sync ── BME restore
//   B:      BME clear ────┴──flush──────── unmap ── free
//                                ▲  A schaltet hier wieder scharf:
//                                   B's Flush-Garantie ist ab hier wertlos
// ```
//
// B hat gespült, aber A macht das Gerät wieder aktiv, bevor B seine Region unmappt — das Fenster,
// das der Flush-Read schließen soll, wäre wieder offen. Deshalb ein **Tiefenzähler je RID**:
// entwaffnet wird beim Übergang 0→1, wiederhergestellt erst beim Übergang 1→0.
//
// Der Zähler ist bewusst **unabhängig** von der Serialisierung des Aufrufers. Heute läuft der
// Detach-Pfad ohnehin unter einem globalen Kontext-Lock; die Eigenschaft soll aber nicht daran
// hängen, dass das so bleibt (jemand verfeinert das Kontext-Locking und weiß nicht, dass die
// Quiesce-Wiederherstellung daran hing — genau der teure Fall).

/// Wie viele Geräte gleichzeitig stillgelegt sein können.
const MAX_QUIESCED: usize = 16;

#[derive(Clone, Copy)]
struct QuiesceEnt {
    rid: u32,
    depth: u32,
    /// Command-Register **vor** der ersten Stilllegung (Save/Restore, nie „auf 1 setzen": ein
    /// Gerät, das absichtlich aus war, darf ein Teardown nicht einschalten).
    saved_cmd: u16,
}

static QUIESCED: sel4lake_sync::SpinLock<[QuiesceEnt; MAX_QUIESCED]> =
    sel4lake_sync::SpinLock::new(
        [QuiesceEnt {
            rid: u32::MAX,
            depth: 0,
            saved_cmd: 0,
        }; MAX_QUIESCED],
    );

fn rid_parts(rid: u32) -> (u8, u8, u8) {
    (
        (rid >> 8) as u8,
        ((rid >> 3) & 0x1f) as u8,
        (rid & 0x7) as u8,
    )
}

/// Ein Gerät über seine **RID** (= StreamID) stilllegen und bereits abgesetzte Writes spülen.
///
/// Zwei Schritte, die verschiedene Dinge tun und einander **nicht** ersetzen:
///
/// 1. **Bus-Master löschen.** Danach darf das Gerät keine neuen Memory-Requests mehr absetzen.
///    Über bereits unterwegs befindliche (posted) Writes sagt das nichts.
/// 2. **Read vom selben Gerät.** PCIe garantiert, dass eine Completion posted Writes nicht
///    überholt: die Antwort trifft erst ein, nachdem die zuvor abgesetzten Writes zugestellt
///    sind. Der Rückgabewert ist belanglos — der Zweck ist die Ordnungsgarantie.
///
///    Gewählt ist ein **Config-Read**, weil zum Teardown-Zeitpunkt kein BAR garantiert gemappt
///    ist. Der verbreitetere Idiom ist ein **MMIO-Read aus einem BAR**: die Garantie gilt für
///    beide, aber Config-Space ist in manchen Endpoints über einen separaten Pfad implementiert.
///    Wo ein BAR sicher verfügbar ist, wäre der MMIO-Read die stärkere Wahl.
///
/// **Grenzen.** Die Ordnungsgarantie hält nur, solange *Relaxed Ordering* / *ID-Based Ordering*
/// für diese Funktion nicht aktiv sind. Und sie setzt voraus, dass das Gerät `BME` respektiert —
/// ein **kompromittiertes** tut das nicht. Gegen das wirkt allein das Entfernen der Übersetzung
/// (STE/Stage-1); gegen das gutartige mit In-flight-Writes wirkt allein dieser Schritt.
/// Keiner ersetzt den anderen.
///
/// Verschachtelbar: nur der äußerste Aufruf legt tatsächlich still (s. o.).
pub fn quiesce_by_rid(rid: u32) {
    let (bus, dev, func) = rid_parts(rid);
    let mut t = QUIESCED.lock();
    if let Some(e) = t.iter_mut().find(|e| e.rid == rid) {
        e.depth += 1;
        return; // bereits stillgelegt -> nur zählen
    }
    let Some(e) = t.iter_mut().find(|e| e.rid == u32::MAX) else {
        return; // Tabelle voll: lieber nicht stilllegen als eine Wiederherstellung zu verlieren
    };
    let cmd = cfg_read16(bus, dev, func, CFG_COMMAND);
    cfg_write32(bus, dev, func, CFG_COMMAND, (cmd & !CMD_BUS_MASTER) as u32);
    let _ = cfg_read16(bus, dev, func, CFG_VENDOR); // Flush-Read (s. o.)
    *e = QuiesceEnt {
        rid,
        depth: 1,
        saved_cmd: cmd,
    };
}

/// Die Stilllegung aufheben. Beim Übergang 1→0 wird das Command-Register zurückgeschrieben:
/// mit `restore_bme = true` unverändert, sonst mit gelöschtem Bus-Master (die übrigen Bits —
/// etwa Memory-Space — bleiben erhalten).
///
/// **Kopplung an die ATS-Entscheidung** (s. `docs/invariants.md` §2b): Bus-Master wieder
/// einzuschalten ist nur solide, *weil* nach `CMD_TLBI`+`CMD_SYNC` keine gecachte Übersetzung
/// mehr existiert. Mit **ATS** existiert sie sehr wohl — im ATC des Geräts. Wird ATS je für ein
/// Gerät freigeschaltet, muss vor dieser Wiederherstellung eine **ATC-Invalidierung** stehen,
/// sonst ist sie unsolide.
pub fn release_quiesce(rid: u32, restore_bme: bool) {
    let (bus, dev, func) = rid_parts(rid);
    let mut t = QUIESCED.lock();
    let Some(e) = t.iter_mut().find(|e| e.rid == rid) else {
        return;
    };
    e.depth -= 1;
    if e.depth > 0 {
        return; // ein anderer Teardown hält das Gerät noch still
    }
    let cmd = if restore_bme {
        e.saved_cmd
    } else {
        e.saved_cmd & !CMD_BUS_MASTER
    };
    cfg_write32(bus, dev, func, CFG_COMMAND, cmd as u32);
    e.rid = u32::MAX;
}

/// Bus-Master für eine RID **aktivieren** — Gegenstück zur Stilllegung, nach dem Installieren
/// einer Übersetzung. Ohne das wäre ein Gerät nach einem vollständigen Detach/Re-Attach-Zyklus
/// dauerhaft tot (beim letzten Detach bleibt Bus-Master bewusst aus).
pub fn arm_bus_master(rid: u32) {
    let (bus, dev, func) = rid_parts(rid);
    let cmd = cfg_read16(bus, dev, func, CFG_COMMAND);
    cfg_write32(bus, dev, func, CFG_COMMAND, (cmd | CMD_BUS_MASTER) as u32);
}
