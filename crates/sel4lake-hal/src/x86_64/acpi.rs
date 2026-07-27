//! Minimaler **ACPI-Tabellenleser** (x86_64) — das Gegenstück zum Device Tree auf ARM.
//!
//! Auf QEMU `virt` beschreibt ein DTB die Plattform; auf dem PC tun das die ACPI-Tabellen.
//! Gebraucht werden hier genau zwei:
//!
//! * **MADT** (`APIC`): welche CPUs es gibt (LAPIC-IDs) — Grundlage für den SMP-Start.
//! * **MCFG**: die Basis des **PCI-ECAM**-Fensters (memory-mapped Konfigurationsraum).
//!
//! Reines, **bounds-geprüftes Lesen** eines vom Firmware-Bereich gelieferten Speicherbereichs:
//! Länge und Prüfsumme jeder Tabelle werden validiert, bevor Felder gelesen werden — eine
//! kaputte/fehlende Tabelle führt zu `None`, nie zu einem Fehlzugriff. Das entspricht der
//! Haltung des DTB-Parsers auf ARM (dort ganz ohne `unsafe`; hier bleibt der Zugriff auf den
//! Firmware-Speicher `unsafe`, weil er nicht als Rust-Slice vorliegt).

/// Ein Bereich physischen Speichers, den die Firmware bereitstellt, als Slice lesen.
///
/// # Safety
/// `[phys, phys+len)` muss identity-gemappter, gültiger Speicher sein (ACPI-Tabellen liegen im
/// reservierten Firmware-Bereich unter 4 GiB, den `mmu::init_primary` abbildet).
unsafe fn bytes(phys: u64, len: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(phys as *const u8, len) }
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    let lo = rd_u32(b, off)? as u64;
    let hi = rd_u32(b, off + 4)? as u64;
    Some((hi << 32) | lo)
}

/// Alle Bytes einer Tabelle aufaddiert müssen 0 ergeben (ACPI-Prüfsumme).
fn checksum_ok(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// Den **RSDP** suchen: im EBDA-Zeiger (0x40E) und im BIOS-Bereich 0xE0000..0x100000.
/// Rückgabe: physische Adresse des RSDP.
fn find_rsdp() -> Option<u64> {
    const SIG: &[u8; 8] = b"RSD PTR ";
    let mut scan = |start: u64, len: u64| -> Option<u64> {
        let mut a = start;
        while a + 20 <= start + len {
            // SAFETY: Firmware-Bereich unter 1 MiB, identity-gemappt (s. `mmu::init_primary`).
            let b = unsafe { bytes(a, 20) };
            if &b[..8] == SIG && checksum_ok(&b[..20]) {
                return Some(a);
            }
            a += 16; // der RSDP liegt 16-Byte-ausgerichtet
        }
        None
    };
    // 1) EBDA (Segmentadresse im BIOS-Datenbereich).
    // SAFETY: feste BIOS-Datenbereich-Adresse, identity-gemappt.
    let ebda_seg = unsafe { core::ptr::read_volatile(0x40E as *const u16) } as u64;
    if ebda_seg != 0 {
        if let Some(a) = scan(ebda_seg << 4, 1024) {
            return Some(a);
        }
    }
    // 2) BIOS-ROM-Bereich.
    scan(0xE_0000, 0x2_0000)
}

/// Header jeder ACPI-Tabelle: 4-Byte-Signatur + 4-Byte-Länge.
fn table_at(phys: u64) -> Option<(&'static [u8; 4], &'static [u8])> {
    // SAFETY: Kopfzeile (8 Byte) lesen, um die Länge zu erfahren; ACPI-Tabellen liegen im
    // identity-gemappten Firmware-Bereich.
    let head = unsafe { bytes(phys, 8) };
    let len = rd_u32(head, 4)? as usize;
    if !(36..0x10_0000).contains(&len) {
        return None; // unplausibel -> Tabelle verwerfen statt ihr zu folgen
    }
    // SAFETY: wie oben, jetzt mit der gemeldeten (plausibilisierten) Länge.
    let full = unsafe { bytes(phys, len) };
    if !checksum_ok(full) {
        return None;
    }
    let sig: &[u8; 4] = full[..4].try_into().ok()?;
    Some((sig, full))
}

/// Über alle Tabellen der RSDT/XSDT iterieren und die erste mit Signatur `want` liefern.
fn find_table(want: &[u8; 4]) -> Option<&'static [u8]> {
    let rsdp_phys = find_rsdp()?;
    // SAFETY: RSDP wurde eben validiert (Signatur + Prüfsumme).
    let rsdp = unsafe { bytes(rsdp_phys, 36) };
    let revision = rsdp[15];
    let (root_phys, entry_size) = if revision >= 2 {
        // ACPI 2.0+: XSDT (64-bit-Einträge), eigene erweiterte Prüfsumme über 36 Byte.
        if !checksum_ok(&rsdp[..36]) {
            return None;
        }
        (rd_u64(rsdp, 24)?, 8)
    } else {
        (rd_u32(rsdp, 16)? as u64, 4)
    };
    let (_, root) = table_at(root_phys)?;
    let n = (root.len() - 36) / entry_size;
    for i in 0..n {
        let off = 36 + i * entry_size;
        let phys = if entry_size == 8 {
            rd_u64(root, off)?
        } else {
            rd_u32(root, off)? as u64
        };
        if let Some((sig, tbl)) = table_at(phys) {
            if sig == want {
                return Some(tbl);
            }
        }
    }
    None
}

/// Ergebnis der MADT-Auswertung: die LAPIC-IDs aller **nutzbaren** CPUs.
pub struct Cpus {
    ids: [u8; super::MAX_CPUS],
    n: usize,
}

impl Cpus {
    pub fn count(&self) -> usize {
        self.n
    }
    /// LAPIC-ID der `i`-ten CPU (Index 0 ist üblicherweise der Bootkern).
    pub fn id(&self, i: usize) -> Option<u8> {
        if i < self.n {
            Some(self.ids[i])
        } else {
            None
        }
    }
}

/// CPUs aus der **MADT** lesen (Eintragstyp 0 = Processor Local APIC, Flag Bit 0 = enabled).
pub fn cpus() -> Option<Cpus> {
    let madt = find_table(b"APIC")?;
    let mut out = Cpus {
        ids: [0; super::MAX_CPUS],
        n: 0,
    };
    // Einträge beginnen hinter Header(36) + LocalApicAddr(4) + Flags(4).
    let mut off = 44;
    while off + 2 <= madt.len() {
        let etype = madt[off];
        let elen = madt[off + 1] as usize;
        if elen < 2 || off + elen > madt.len() {
            break; // defekte Kette -> abbrechen statt weiterzuraten
        }
        if etype == 0 && elen >= 8 {
            let apic_id = madt[off + 3];
            let flags = rd_u32(madt, off + 4)?;
            if flags & 1 != 0 && out.n < super::MAX_CPUS {
                out.ids[out.n] = apic_id;
                out.n += 1;
            }
        }
        off += elen;
    }
    if out.n == 0 {
        return None;
    }
    Some(out)
}

/// Registerbasis der ersten **DMA-Remapping-Einheit** aus der ACPI-**DMAR** (VT-d).
///
/// Aufbau: Header(36) + HostAddressWidth(1) + Flags(1) + reserviert(10), dann Remapping-
/// Strukturen à `(Typ:u16, Länge:u16, …)`. Typ 0 = DRHD; dort liegt die Registerbasis bei
/// Offset 8. `None`, wenn es keine DMAR gibt (dann hat die Plattform keine IOMMU).
pub fn dmar_unit_base() -> Option<u64> {
    let dmar = find_table(b"DMAR")?;
    let mut off = 48;
    while off + 4 <= dmar.len() {
        let etype = u16::from_le_bytes([dmar[off], dmar[off + 1]]);
        let elen = u16::from_le_bytes([dmar[off + 2], dmar[off + 3]]) as usize;
        if elen < 4 || off + elen > dmar.len() {
            break; // defekte Kette -> abbrechen statt weiterzuraten
        }
        if etype == 0 && elen >= 16 {
            return rd_u64(dmar, off + 8);
        }
        off += elen;
    }
    None
}

/// Basis des **PCI-ECAM**-Fensters (MCFG, erste Segmentgruppe) + erste/letzte Bus-Nummer.
pub fn pci_ecam() -> Option<(u64, u8, u8)> {
    let mcfg = find_table(b"MCFG")?;
    // Header(36) + reserviert(8), dann Einträge à 16 Byte.
    let off = 44;
    if off + 16 > mcfg.len() {
        return None;
    }
    let base = rd_u64(mcfg, off)?;
    let start_bus = mcfg[off + 10];
    let end_bus = mcfg[off + 11];
    Some((base, start_bus, end_bus))
}
