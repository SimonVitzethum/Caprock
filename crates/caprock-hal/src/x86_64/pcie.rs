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
/// Device-IDs des virtio-Blockgeraets (legacy + modern) — A-5.2.
pub const VIRTIO_BLK_DEVICES: [u16; 2] = [0x1001, 0x1042];
/// Device-IDs der virtio-Netzkarte (legacy + modern) — A-5.2.
///
/// Die Legacy-ID steht hier, damit die **Suche** ein transitional konfiguriertes Geraet findet.
/// Bedienen kann der Treiber es nicht: er verlangt `VIRTIO_F_VERSION_1` und bricht sonst ab. Das
/// ist die richtige Reihenfolge — ein Geraet, das da ist und nicht modern spricht, soll als
/// Fehlschlag sichtbar werden und nicht als "kein Geraet gefunden".
pub const VIRTIO_NET_DEVICES: [u16; 2] = [0x1000, 0x1041];

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

/// **An all-ones read is an ABORT, not a value** — the config-space answer that means "nobody
/// answered".
///
/// A configuration space that does not respond returns `0xff` in every byte, and there are more
/// ways into that state than "empty slot": surprise removal, a function-level reset in flight, a
/// slot that lost power, a device that gave up. It matters because every field read that way is
/// *plausible* — a capability pointer of `0xfc` that points at itself, a table size of 2048, a
/// control register with every bit set, a BAR that covers all of memory. Whoever treats such a
/// read as data is configuring a device that is not there.
///
/// Two widths, and deliberately no single `u32` helper: a `u16` widened to `u32` is
/// `0x0000_ffff`, which is **not** all ones — the one-function version would answer `false` for
/// exactly the case it exists to catch.
#[inline]
#[must_use]
pub fn all_ones16(v: u16) -> bool {
    v == 0xffff
}

/// See [`all_ones16`].
#[inline]
#[must_use]
pub fn all_ones32(v: u32) -> bool {
    v == 0xffff_ffff
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
            if all_ones16(v) || v == 0 {
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

/// **Jedes Speicher-BAR jeder vorhandenen Funktion melden** — `f(basis, laenge)`.
///
/// Gebraucht für E-Rest 3: die Karte oberhalb von 4 GiB wächst nur dort, wo etwas ist, und
/// „wo etwas ist" steht in den BARs, die die Firmware vergeben hat. Das ist der Pfad von der
/// BAR-Ermittlung zur Seitentabelle — bewusst **hier** und nicht im Kernel, weil er sonst
/// nochmal enumerieren müsste.
///
/// Zwei Feinheiten, die beide Schaden anrichten, wenn man sie übergeht:
/// * **Header-Typ.** Eine Bridge (Typ 1) hat nur zwei BARs; ab `0x18` stehen dort Bus-Nummern.
///   `bar_size` schreibt zum Dimensionieren alle Bits und stellt danach wieder her — auf ein
///   Bus-Nummern-Register angewandt wäre das keine Messung, sondern eine Umkonfiguration der
///   Topologie mitten im Hochlauf.
/// * **Alle acht Funktionen**, nicht nur Funktion 0: ein Multifunktionsgerät legt seine
///   Registerfenster über die Funktionen, und ein nicht abgebildetes Fenster fällt erst beim
///   ersten Zugriff auf — also weit weg von hier.
pub fn for_each_bar(f: &mut dyn FnMut(u64, u64)) {
    if !init() {
        return;
    }
    let max = MAX_BUS.load(Ordering::Acquire) as u16;
    for bus in 0..=max.min(255) {
        for dev in 0u8..32 {
            for func in 0u8..8 {
                let b = bus as u8;
                if all_ones16(cfg_read16(b, dev, func, CFG_VENDOR)) {
                    if func == 0 {
                        break; // Funktion 0 fehlt -> das Geraet gibt es nicht
                    }
                    continue;
                }
                let hdr = cfg_read8(b, dev, func, 0x0E);
                let nbars = match hdr & 0x7f {
                    0 => 6, // Endpoint
                    1 => 2, // Bridge -- ab 0x18 stehen Bus-Nummern, keine BARs
                    _ => 0, // Cardbus o. ae.: hier nichts zu holen
                };
                let d = PciDevice {
                    bus: b,
                    dev,
                    func,
                    vendor: 0,
                    device: 0,
                    class: 0,
                    int_pin: 0,
                    bars: [0; 6],
                };
                let mut i = 0usize;
                while i < nbars {
                    let lo = cfg_read32(b, dev, func, CFG_BAR0 + (i as u16) * 4);
                    if lo & 1 != 0 {
                        i += 1; // I/O-BAR
                        continue;
                    }
                    let is64 = (lo >> 1) & 0b11 == 0b10;
                    let mut addr = (lo & !0xf) as u64;
                    if is64 && i + 1 < nbars {
                        addr |= (cfg_read32(b, dev, func, CFG_BAR0 + ((i + 1) as u16) * 4) as u64)
                            << 32;
                    }
                    if addr != 0 {
                        let len = bar_size(&d, i);
                        if len != 0 {
                            f(addr, len);
                        }
                    }
                    i += if is64 { 2 } else { 1 };
                }
                if func == 0 && hdr & 0x80 == 0 {
                    break; // kein Multifunktionsgeraet
                }
            }
        }
    }
}

/// Ist Bus-Master für dieses Gerät aktiv?
pub fn bus_master_enabled(d: &PciDevice) -> bool {
    cfg_read16(d.bus, d.dev, d.func, CFG_COMMAND) & CMD_BUS_MASTER != 0
}

/// Die **Konfigurationsraum-Seite genau dieser Funktion** (A-5.1). `0`, solange kein ECAM bekannt.
///
/// Das ist der Punkt, an dem ein alter Einwand wegfällt. Bis A-5.1 hieß es: der Konfigurationsraum
/// darf nicht an einen Treiber, denn er ist **geräteweit** — wer ihn liest, sieht jedes Gerät der
/// Maschine. Das gilt für den alten Portpfad (`0xCF8`/`0xCFC`, ein globales Adressregister) und
/// für „das ECAM-Fenster" als Ganzes. Es gilt **nicht** für eine einzelne Funktion: ECAM bildet
/// `(bus, dev, func)` auf je 4 KiB ab, und 4 KiB sind genau eine Seite. Eine Funktion ist damit
/// mappbar, ohne die Nachbarn mitzugeben.
///
/// Deshalb kann der Kern die **Enumeration** behalten (er sucht das Gerät) und der Treiber-PD
/// trotzdem seinen eigenen Capability-Lauf machen (er liest seine eigene Seite). Ohne das müsste
/// der Kern die virtio-Strukturen auflösen und dem Treiber reichen — also virtio kennen, und
/// genau das soll er nicht (A-5.1, Richtungsumkehr).
pub fn cfg_page(d: &PciDevice) -> u64 {
    cfg_addr(d.bus, d.dev, d.func, 0).unwrap_or(0) & !0xfff
}

/// **Größe des BAR `i`** in Bytes (`0` = unbenutzt/kein Speicher-BAR).
///
/// Ermittelt über den üblichen Weg: alle Bits schreiben, zurücklesen, die niedrigsten gesetzten
/// Bits geben die Größe — und **den Originalwert wiederherstellen**. Das Wiederherstellen ist
/// nicht Kosmetik: zwischen dem Schreiben und dem Zurückschreiben zeigt das BAR ins Leere, und
/// jeder Zugriff darauf ginge daneben. Deshalb passiert das **einmal beim Zuteilen** und nicht
/// im laufenden Betrieb.
pub fn bar_size(d: &PciDevice, i: usize) -> u64 {
    if i >= 6 {
        return 0;
    }
    let off = CFG_BAR0 + (i as u16) * 4;
    let lo = cfg_read32(d.bus, d.dev, d.func, off);
    if lo & 1 != 0 {
        return 0; // I/O-BAR
    }
    let is64 = (lo >> 1) & 0b11 == 0b10;
    let hi_off = off + 4;
    let hi = if is64 { cfg_read32(d.bus, d.dev, d.func, hi_off) } else { 0 };
    cfg_write32(d.bus, d.dev, d.func, off, 0xffff_ffff);
    let mask_lo = cfg_read32(d.bus, d.dev, d.func, off) & !0xf;
    let mask_hi = if is64 {
        cfg_write32(d.bus, d.dev, d.func, hi_off, 0xffff_ffff);
        let m = cfg_read32(d.bus, d.dev, d.func, hi_off);
        cfg_write32(d.bus, d.dev, d.func, hi_off, hi);
        m
    } else {
        0xffff_ffff
    };
    cfg_write32(d.bus, d.dev, d.func, off, lo);
    let mask = (mask_lo as u64) | ((mask_hi as u64) << 32);
    if mask == 0 {
        return 0;
    }
    (!mask).wrapping_add(1)
}

// ================================================================================================
// MSI-X (Stufe B, 2026-08-26)
// ================================================================================================
//
// **Bis heute kam `MSI` in dieser Datei NICHT vor** -- und die Capability-Liste wurde ueberhaupt
// nicht abgelaufen. Ohne das schickt ein Geraet nie einen Interrupt, gleich wie vollstaendig die
// IRTE-Vergabe daneben ist. Das war die eigentliche Luecke von `CAP_IRQ` auf x86.
//
// **Wer die Tabelle schreibt, ist eine ENTSCHEIDUNG** (E11): der Kernel, nicht der Treiber.
// Interruptrouting ist Kernelautoritaet, genau wie die IRTE -- und *der Handle ist eine Zahl,
// keine Autoritaet; der Vektor auch nicht.* Ein Treiber, der seine Tabellenzeile selbst schriebe,
// waehlte, wo sein Interrupt landet; SVT/SID faengt die **Zustellung**, aber es gibt keinen Grund,
// sich auf die zweite Verteidigungslinie zu verlassen, wenn die erste umsonst ist.

/// Capability-ID der MSI-X-Struktur (PCI 3.0, `PCI_CAP_ID_MSIX`).
const CAP_ID_MSIX: u8 = 0x11;
/// Offset der Capability-Liste im Konfigurationsraum.
const CFG_CAP_PTR: u16 = 0x34;
/// `Status`-Register; Bit 4 sagt, ob es ueberhaupt eine Capability-Liste gibt.
const CFG_STATUS: u16 = 0x06;
const STATUS_CAP_LIST: u16 = 1 << 4;

/// **Wo die MSI-X-Tabelle eines Geraets liegt.**
///
/// `bar` ist der BAR-**Index** (0..5), nicht die Adresse: erst zusammen mit `PciDevice::bars`
/// ergibt sich die Lage, und diese Trennung ist Absicht -- die Frage „liegt die Tabelle in der
/// BAR, die dem Treiber angeboten wird?" ist eine Frage nach dem INDEX und wird sonst als
/// Adressvergleich formuliert, der bei ueberlappenden Fenstern das Falsche sagt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsixInfo {
    /// Offset der Capability im Konfigurationsraum (fuer `msix_enable`).
    pub cap: u16,
    /// BAR-Index der Tabelle.
    pub bar: u8,
    /// Offset der Tabelle in dieser BAR.
    pub offset: u32,
    /// Anzahl der Tabellenzeilen (`Table Size` + 1).
    pub eintraege: u16,
}

/// **Die MSI-X-Capability eines Geraets finden.** `None` = das Geraet hat keine.
///
/// Die Schrittschranke ist kein Stilmittel, sondern dieselbe Ueberlegung wie in
/// `caprock_virtio::probe_ecam`: eine Capability-Liste ist eine verkettete Liste im
/// Konfigurationsraum **des Geraets**. Ein defektes oder boeswilliges Geraet kann sie im Kreis
/// legen, und wer ihr ohne Schranke folgt, haengt -- im Kernel.
pub fn msix_find(d: &PciDevice) -> Option<MsixInfo> {
    // An absent device answers `0xffff`, and `0xffff` HAS `STATUS_CAP_LIST` set (s.
    // [`all_ones16`]). Without this the walk below follows a list of `0xfc` pointers that points
    // at itself and burns its whole step budget before answering "no MSI-X" — the wrong answer
    // for the right-looking reason.
    let status = cfg_read16(d.bus, d.dev, d.func, CFG_STATUS);
    if all_ones16(status) || status & STATUS_CAP_LIST == 0 {
        return None;
    }
    let mut cap = (cfg_read8(d.bus, d.dev, d.func, CFG_CAP_PTR) & 0xfc) as u16;
    let mut wache = 0;
    while cap != 0 && wache < 48 {
        wache += 1;
        let id = cfg_read8(d.bus, d.dev, d.func, cap);
        if id == 0xff {
            return None; // config space died mid-walk — same case as above, one level in
        }
        if id == CAP_ID_MSIX {
            let ctrl = cfg_read16(d.bus, d.dev, d.func, cap + 2);
            let tbl = cfg_read32(d.bus, d.dev, d.func, cap + 4);
            return Some(MsixInfo {
                cap,
                bar: (tbl & 0b111) as u8,
                offset: tbl & !0b111,
                eintraege: (ctrl & 0x7ff) + 1,
            });
        }
        cap = (cfg_read8(d.bus, d.dev, d.func, cap + 1) & 0xfc) as u16;
    }
    None
}

/// **Liegt die MSI-X-Tabelle in der BAR mit Index `bar_index`?** (E11)
///
/// Die Frage, an der die Zuteilung entscheidet, ob sie das Geraet ueberhaupt vergibt: liegt die
/// Tabelle in der BAR, die der Treiber bekommt, koennte er Adresse und Datenwort selbst schreiben
/// -- und damit waehlen, wo sein Interrupt landet.
pub fn msix_in_bar(info: &MsixInfo, bar_index: usize) -> bool {
    info.bar as usize == bar_index
}

/// **Eine Zeile der MSI-X-Tabelle schreiben** und sie freigeben.
///
/// # Safety
/// `tabelle` muss die identity-gemappte Basis der MSI-X-Tabelle dieses Geraets sein und
/// `zeile < info.eintraege` gelten. Der Aufrufer haelt das Geraet exklusiv.
///
/// Reihenfolge mit Absicht: **erst Adresse und Datenwort, dann die Maske loesen.** Andersherum
/// gaebe es ein Fenster, in dem die Zeile freigegeben ist und noch auf `0` zeigt -- ein Interrupt
/// darin ginge an Vektor 0.
pub unsafe fn msix_write_entry(tabelle: u64, zeile: u16, addr: u32, data: u32) {
    let e = tabelle + (zeile as u64) * 16;
    // SAFETY: siehe Funktionsdoku; die Tabelle ist ein Geraetefenster, also `volatile`.
    unsafe {
        core::ptr::write_volatile(e as *mut u32, addr);
        core::ptr::write_volatile((e + 4) as *mut u32, 0); // Adresse hoch: 0 (unter 4 GiB)
        core::ptr::write_volatile((e + 8) as *mut u32, data);
        core::arch::asm!("mfence", options(nostack, preserves_flags));
        core::ptr::write_volatile((e + 12) as *mut u32, 0); // Vector Control: Maske loesen
    }
}

/// **Eine Zeile ZURUECKLESEN**: `(addr_lo, addr_hi, data, vector_control)`.
///
/// Fuer den Pruefer, und die Betonung liegt auf *zurueck*: die Zeile steht im BAR des Geraets, und
/// wer wissen will, ob sie noch dort steht, muss sie lesen. Nachzurechnen, was der Kernel
/// hineingeschrieben hat, belegte nur, dass er es getan hat — nicht, dass es noch gilt. Ein
/// Geraetereset setzt sie zurueck, und in QEMU tut `virtio_pci_reset` genau das.
///
/// # Safety
/// wie [`msix_write_entry`].
pub unsafe fn msix_read_entry(tabelle: u64, zeile: u16) -> (u32, u32, u32, u32) {
    let e = tabelle + (zeile as u64) * 16;
    // SAFETY: siehe Funktionsdoku.
    unsafe {
        (
            core::ptr::read_volatile(e as *const u32),
            core::ptr::read_volatile((e + 4) as *const u32),
            core::ptr::read_volatile((e + 8) as *const u32),
            core::ptr::read_volatile((e + 12) as *const u32),
        )
    }
}

/// Eine Zeile wieder **maskieren** (Teardown, Gegenprobe).
///
/// # Safety
/// wie [`msix_write_entry`].
pub unsafe fn msix_mask_entry(tabelle: u64, zeile: u16) {
    let e = tabelle + (zeile as u64) * 16;
    // SAFETY: siehe Funktionsdoku.
    unsafe { core::ptr::write_volatile((e + 12) as *mut u32, 1) };
}

/// `MSI-X Control` (Capability + 2) — the two bits this kernel ever writes.
pub const MSIX_CTRL_ENABLE: u16 = 1 << 15;
/// Function Mask: while it is set, **no** vector of this function fires, whatever the per-row
/// mask says. This is the switch that stills a device without disabling MSI-X.
pub const MSIX_CTRL_FUNC_MASK: u16 = 1 << 14;

/// Write `MSI-X Control` and **read it back**.
///
/// The read-back is a spoken-for check and not a convenience: a configuration space that does not
/// answer returns `0xffff` (s. [`all_ones16`]), and a write nobody reads back looks identical
/// whether it landed or vanished.
fn msix_ctrl_write(bus: u8, dev: u8, func: u8, cap: u16, set: u16, clear: u16) -> u16 {
    let ctrl = cfg_read16(bus, dev, func, cap + 2);
    let neu = (ctrl | set) & !clear;
    // The config space is addressed in 32-bit words: the control register sits in the upper half.
    let wort = cfg_read32(bus, dev, func, cap);
    cfg_write32(bus, dev, func, cap, (wort & 0x0000_ffff) | ((neu as u32) << 16));
    cfg_read16(bus, dev, func, cap + 2)
}

/// **Arm MSI-X at the device** (`Enable` on, Function Mask off). Returns the read-back control
/// word — see [`msix_ctrl_write`] for why it is returned rather than discarded.
pub fn msix_enable(d: &PciDevice, info: &MsixInfo) -> u16 {
    msix_ctrl_write(d.bus, d.dev, d.func, info.cap, MSIX_CTRL_ENABLE, MSIX_CTRL_FUNC_MASK)
}

/// Like [`msix_enable`], but addressed by **RID** instead of a `PciDevice`.
///
/// The device assignment carries only the RID (it is the identity under which the device requests
/// DMA) — dragging a `PciDevice` there would mean storing a snapshot of the configuration space
/// and using it later.
pub fn msix_enable_by_rid(rid: u32, cap: u16) -> u16 {
    let (bus, dev, func) = rid_parts(rid);
    msix_ctrl_write(bus, dev, func, cap, MSIX_CTRL_ENABLE, MSIX_CTRL_FUNC_MASK)
}

/// **Das Kontrollregister lesen, ohne zu schreiben** — fuer den Pruefer.
///
/// Bit 15 ist `Enable`, **Bit 14 die Funktionsmaske**: sie unterdrueckt jeden Vektor der Funktion,
/// unabhaengig vom `Vector Control` der einzelnen Zeile. Die Ruecklesung in [`msix_enable_by_rid`]
/// prueft nur Bit 15 -- Bit 14 war damit eine ungeprueft geloeschte Zusage.
pub fn msix_ctrl_lesen(rid: u32, cap: u16) -> u16 {
    let (bus, dev, func) = rid_parts(rid);
    cfg_read16(bus, dev, func, cap + 2)
}

/// **Still the device's MSI-X** — the first step of taking an interrupt grant back.
///
/// Function Mask, **not** `Enable = 0`, and that is the whole point of the function existing
/// separately: a function whose MSI-X Enable is clear falls back to **INTx** (PCI 3.0 §6.8.2 — a
/// function must not use INTx while MSI-X is enabled). Nothing on this kernel routes INTx, so
/// clearing Enable would trade a remapped message for a line interrupt nobody handles. The
/// Function Mask forbids every vector and leaves that fallback shut.
///
/// Returns the read-back control word like [`msix_enable`]; a teardown has nothing left to decide
/// with it, and a caller that wants to check may.
pub fn msix_quiesce_by_rid(rid: u32, cap: u16) -> u16 {
    let (bus, dev, func) = rid_parts(rid);
    msix_ctrl_write(bus, dev, func, cap, MSIX_CTRL_FUNC_MASK, 0)
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
                if all_ones16(vendor) {
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
        if hdr == 0 || all_ones32(hdr) {
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
