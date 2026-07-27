//! 4-Level-Paging mit W^X-Identity-Map (x86_64) — API-gleich zum aarch64-MMU-Modul.
//!
//! ## Gleiches Modell, andere Deskriptoren
//!
//! Wie auf ARM ist die Abbildung eine **Identity-Map** (virtuell == physisch, ADR 0002) mit
//! strikter W^X-Trennung; nur die Deskriptorformate unterscheiden sich:
//!
//! | Eigenschaft | aarch64 | x86_64 |
//! |---|---|---|
//! | Ebenen | L1/L2/L3 (4 KiB / 2 MiB / 1 GiB) | PML4/PDPT/PD/PT (dieselben Größen) |
//! | „nicht ausführbar" | `UXN`/`PXN` getrennt für EL0/EL1 | **ein** `NX`-Bit (braucht `EFER.NXE`) |
//! | User-Zugriff | `AP[1]` | `US`-Bit |
//! | Schreibschutz für Ring 0 | über `AP` | `CR0.WP` **muss** gesetzt sein, sonst ignoriert Ring 0 `RW` |
//! | Adressraum-Tag | ASID | PCID |
//!
//! Die ersten 16 MiB werden mit **4-KiB-Seiten** abgebildet, damit das Kernel-Image
//! seitengenau in `.text` (R-X), `.rodata` (R--/NX) und Daten (RW/NX) zerfällt; darüber
//! genügen 2-MiB-Blöcke. Das oberste GiB unter 4 GiB ist **uncacheable** (LAPIC/IOAPIC/PCI).
//!
//! ## Noch nicht portiert
//!
//! Die `vspace_*`-Funktionen (per-Prozess-Adressräume isolierter PDs, ext-11ff) melden hier
//! ehrlich „nicht unterstützt" statt etwas Halbes zu tun: der Kernel fängt das ab und
//! erzeugt auf x86 keine isolierten PDs. Sie brauchen zusätzlich PCID-Verwaltung und ein
//! per-VSpace-Tabellenlayout — ein eigener Portierungsschritt (s. `todo.md`).

use super::cpu;

// --- Deskriptor-Bits ------------------------------------------------------------------------

const P: u64 = 1 << 0; //  present
const RW: u64 = 1 << 1; // schreibbar
const US: u64 = 1 << 2; // im User-Modus (Ring 3) zugreifbar
const PWT: u64 = 1 << 3; // write-through
const PCD: u64 = 1 << 4; // cache disable (MMIO)
const PS: u64 = 1 << 7; // page size: 2-MiB-Block statt Tabellenverweis
const NX: u64 = 1 << 63; // not executable (braucht EFER.NXE)

const PAGE: u64 = 4096;
const TWO_MIB: u64 = 2 * 1024 * 1024;
const ONE_GIB: u64 = 1 << 30;

/// Untergrenze des für User-RAM nutzbaren Bereichs.
///
/// Unterhalb liegen auf dem PC das BIOS-/VGA-Fenster, das Kernel-Image (ab 1 MiB) und die
/// Boot-Seitentabellen. 16 MiB ist die Grenze, bis zu der wir mit 4-KiB-Seiten abbilden.
pub const USER_RAM_MIN: u64 = 16 * 1024 * 1024;
/// Ende des ersten GiB (Grenze für isolierte Regionen — wie auf aarch64).
pub const GIB1_END: u64 = ONE_GIB;
/// Größe der Region einer isolierten PD.
pub const ISO_REGION_SIZE: u64 = TWO_MIB;

/// Rechte einer Kernel-Abbildung (wie aarch64 `Perm`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Perm {
    /// Lesen + Ausführen (Kernel-Code).
    Rx,
    /// Nur lesen.
    Ro,
    /// Lesen + Schreiben, nicht ausführbar.
    Rw,
    /// EL0/Ring-3 lesbar+schreibbar, nicht ausführbar.
    UserRw,
    /// EL0/Ring-3 lesbar+ausführbar (User-Code).
    UserRx,
}

/// Rechte einer User-Abbildung (wie aarch64 `UserPerm`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UserPerm {
    Ro,
    Rw,
    Rx,
}

const fn perm_bits(p: Perm) -> u64 {
    match p {
        Perm::Rx => P,                    // ausführbar, nur lesbar, Ring 0
        Perm::Ro => P | NX,               //
        Perm::Rw => P | RW | NX,          //
        Perm::UserRw => P | RW | US | NX, //
        Perm::UserRx => P | US,           // ausführbar für Ring 3, nicht schreibbar
    }
}

// --- Tabellen -------------------------------------------------------------------------------

/// Eine 4-KiB-Seitentabelle (512 Einträge).
#[repr(C, align(4096))]
struct Table([u64; 512]);

impl Table {
    const EMPTY: Table = Table([0; 512]);
}

/// Anzahl der mit **4-KiB-Seiten** abgebildeten 2-MiB-Blöcke (= die ersten 16 MiB).
const FINE_BLOCKS: usize = 8;
/// Wie viele GiB identity-abgebildet werden (deckt RAM + MMIO-Fenster unter 4 GiB ab).
const MAPPED_GIB: usize = 4;

static mut PML4: Table = Table::EMPTY;
static mut PDPT: Table = Table::EMPTY;
static mut PD: [Table; MAPPED_GIB] = [const { Table::EMPTY }; MAPPED_GIB];
static mut PT: [Table; FINE_BLOCKS] = [const { Table::EMPTY }; FINE_BLOCKS];

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __kernel_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// Erste freie physische Adresse hinter dem Kernel-Image (inkl. Seitentabellen).
pub fn kernel_end() -> u64 {
    // SAFETY: Linker-Symbol, nur die Adresse wird gelesen.
    let end = unsafe { sym(&__kernel_end) };
    (end + PAGE - 1) & !(PAGE - 1)
}

/// Rechte einer 4-KiB-Seite im Bereich der ersten 16 MiB nach dem Image-Layout bestimmen.
fn fine_perm(pa: u64) -> u64 {
    // SAFETY: Linker-Symbole, nur Adressen.
    let (ts, te, rs, re) = unsafe {
        (
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
        )
    };
    if pa >= ts && pa < te {
        perm_bits(Perm::Rx) // Code: ausführbar, NICHT schreibbar
    } else if pa >= rs && pa < re {
        perm_bits(Perm::Ro) // Konstanten: nur lesbar, NX
    } else {
        perm_bits(Perm::Rw) // alles andere: RW, NX
    }
}

/// Ist dieser 2-MiB-Block Geräte-Speicher (MMIO)? Das oberste GiB unter 4 GiB trägt auf dem
/// PC LAPIC (0xFEE0_0000), IOAPIC und die PCI-Fenster.
fn is_device(pa: u64) -> bool {
    pa >= 0xC000_0000
}

/// Seitentabellen aufbauen und aktivieren (Primärkern).
pub fn init_primary() {
    // NX-Bit freischalten (sonst ist Bit 63 reserviert -> #GP beim Laden von CR3).
    const MSR_EFER: u32 = 0xC000_0080;
    const EFER_NXE: u64 = 1 << 11;
    // SAFETY: EFER existiert auf jeder x86_64-CPU; wir setzen nur NXE.
    unsafe {
        let efer = cpu::rdmsr(MSR_EFER);
        cpu::wrmsr(MSR_EFER, efer | EFER_NXE);
    }

    // SAFETY: die Tabellen sind statische, 4-KiB-ausgerichtete Arrays, die ausschließlich hier
    // (beim Boot, einkernig, vor dem Laden von CR3) beschrieben werden.
    unsafe {
        let pml4 = &mut *core::ptr::addr_of_mut!(PML4);
        let pdpt = &mut *core::ptr::addr_of_mut!(PDPT);
        let pd = &mut *core::ptr::addr_of_mut!(PD);
        let pt = &mut *core::ptr::addr_of_mut!(PT);

        pml4.0[0] = (pdpt as *const Table as u64) | P | RW;
        for (g, table) in pd.iter_mut().enumerate() {
            pdpt.0[g] = (table as *const Table as u64) | P | RW;
        }
        // Erste 16 MiB: 4-KiB-Granularität (seitengenaues W^X über das Kernel-Image).
        for (b, table) in pt.iter_mut().enumerate() {
            for (i, e) in table.0.iter_mut().enumerate() {
                let pa = (b as u64) * TWO_MIB + (i as u64) * PAGE;
                *e = pa | fine_perm(pa);
            }
            pd[0].0[b] = (table as *const Table as u64) | P | RW;
        }
        // Rest: 2-MiB-Blöcke, RW + NX; MMIO-Bereiche uncacheable.
        for g in 0..MAPPED_GIB {
            let first = if g == 0 { FINE_BLOCKS } else { 0 };
            for i in first..512 {
                let pa = (g as u64) * ONE_GIB + (i as u64) * TWO_MIB;
                let mut bits = P | RW | NX | PS;
                if is_device(pa) {
                    bits |= PCD | PWT;
                }
                pd[g].0[i] = pa | bits;
            }
        }
        activate(pml4 as *const Table as u64);
    }
}

/// **Rechte einer einzelnen 4-KiB-Seite ändern** (nur im fein abgebildeten Bereich < 16 MiB).
///
/// Gebraucht für die AP-Trampolin-Seite: sie muss beim Boot **beschreibbar** sein (der BSP
/// kopiert den Block hinein) und danach **ausführbar, nicht schreibbar** (der Sekundärkern
/// führt dort weiter, nachdem er `CR0.PG` gesetzt hat — läge sie dann NX, gäbe es genau in
/// diesem Moment einen #PF ohne IDT und damit einen Triple Fault). Beides gleichzeitig wäre
/// eine W^X-Verletzung; der Wechsel ist die saubere Auflösung.
///
/// Gibt `false` für Adressen außerhalb des 4-KiB-granularen Bereichs oder unausgerichtete.
pub fn protect_page(pa: u64, perm: Perm) -> bool {
    if pa % PAGE != 0 || pa >= (FINE_BLOCKS as u64) * TWO_MIB {
        return false;
    }
    let block = (pa / TWO_MIB) as usize;
    let idx = ((pa % TWO_MIB) / PAGE) as usize;
    // SAFETY: `PT` ist die statische Seitentabelle dieses Kernels; Block-/Index-Bereich ist eben
    // geprüft. Der Eintrag wird atomar (ein 64-bit-Store) ersetzt und die Adresse danach aus dem
    // TLB geworfen.
    unsafe {
        let pt = &mut *core::ptr::addr_of_mut!(PT);
        pt[block].0[idx] = pa | perm_bits(perm);
    }
    flush_va_global(pa);
    true
}

/// CR3 auf `root` setzen und `CR0.WP` erzwingen.
///
/// **`CR0.WP` ist sicherheitskritisch:** ohne dieses Bit ignoriert Ring 0 das `RW`-Bit der
/// Seitentabellen — die W^X-Trennung gälte dann nur für Ring 3, und ein Kernel-Bug könnte
/// `.text` überschreiben.
fn activate(root: u64) {
    // SAFETY: `root` zeigt auf die eben aufgebaute, gültige PML4. Das Laden von CR3/CR0 ist
    // eine erlaubte Low-Level-Domäne und serialisiert selbst.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        const CR0_WP: u64 = 1 << 16;
        core::arch::asm!("mov cr0, {}", in(reg) cr0 | CR0_WP, options(nomem, nostack, preserves_flags));
    }
}

/// Sekundärkern: dieselbe (globale) Tabelle aktivieren.
pub fn init_secondary() {
    // SAFETY: die Tabelle wurde vom Primärkern fertig aufgebaut (Reihenfolge über den
    // SMP-Bring-up sichergestellt).
    unsafe { activate(core::ptr::addr_of!(PML4) as u64) };
}

/// Status für den Boot-Report (aarch64: `SCTLR_EL1`-Flags M/C/I).
///
/// x86: Paging ist im Long Mode immer an; Caches sind an, wenn `CR0.CD` **nicht** gesetzt
/// ist. Zusätzlich melden wir `CR0.WP` als drittes Flag (die Entsprechung zu „I": erst damit
/// wirkt W^X auch gegen Ring 0).
pub fn sctlr_flags() -> (bool, bool, bool) {
    let cr0: u64;
    // SAFETY: reines Lesen eines Steuerregisters.
    unsafe { core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags)) };
    const CR0_CD: u64 = 1 << 30;
    const CR0_WP: u64 = 1 << 16;
    (true, cr0 & CR0_CD == 0, cr0 & CR0_WP != 0)
}

/// Wurzel der globalen Identity-Map (aarch64: `global_root`) — der CR3-Wert.
pub fn global_root() -> u64 {
    core::ptr::addr_of!(PML4) as u64
}

/// Adressraum wechseln (aarch64: `TTBR0` + ASID; hier CR3 + PCID).
///
/// Solange es keine isolierten Adressräume gibt (s. Modul-Doku), wird nur die globale Wurzel
/// gesetzt; ein Wechsel auf `root == global_root()` ist ein No-Op.
pub fn set_user_vspace(root: u64, _asid: u16) {
    let cur: u64;
    // SAFETY: Lesen/Schreiben von CR3 (Adressraumwechsel) — erlaubte Domäne; `root` ist
    // entweder die globale Wurzel oder eine vom Kernel aufgebaute Tabelle.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cur, options(nomem, nostack, preserves_flags));
        if cur & !0xfff != root & !0xfff {
            core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
        }
    }
}

/// Größte nutzbare Adressraum-Kennung (aarch64: ASID; x86: PCID).
///
/// `0` bedeutet „keine getaggten Adressräume" — der Kernel legt dann keine isolierten PDs an
/// (die `vspace_*`-Funktionen sind auf x86 noch nicht portiert).
pub fn max_asid() -> u16 {
    0
}

/// TLB-Einträge eines Adressraums verwerfen (aarch64: `tlbi aside1is`).
pub fn flush_asid(_asid: u16) {
    flush_all();
}

/// Eine einzelne Adresse global aus dem TLB werfen.
pub fn flush_va_global(va: u64) {
    // SAFETY: `invlpg` invalidiert nur einen TLB-Eintrag; keine Speicherwirkung.
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) va, options(nostack, preserves_flags)) };
}

/// Kompletten TLB verwerfen (CR3 neu laden).
fn flush_all() {
    let cr3: u64;
    // SAFETY: CR3-Round-Trip verwirft die nicht-globalen TLB-Einträge.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

// --- DMA-Cache-Wartung ------------------------------------------------------------------------

/// Auf x86 sind DMA-Zugriffe **hardware-kohärent** zum CPU-Cache (Snooping) — anders als auf
/// aarch64 ist hier keine Cache-Wartung nötig. Die Barriere stellt nur die Reihenfolge sicher.
pub fn dma_cache_clean(_va: u64, _len: u64) {
    cpu::dsb_sy();
}

/// Siehe [`dma_cache_clean`].
pub fn dma_cache_invalidate(_va: u64, _len: u64) {
    cpu::dsb_sy();
}

// --- Per-Prozess-Adressräume (noch nicht portiert) ---------------------------------------------
//
// Diese Funktionen melden ehrlich „nicht unterstützt", statt etwas Halbes zu tun. Der Kernel
// wertet die Rückgabe aus und legt auf x86 keine isolierten PDs an. Der fehlende Teil ist
// nicht die Mechanik (die Tabellen oben zeigen sie), sondern die Verwaltung: PCID-Vergabe,
// per-VSpace-Tabellenpool und der Teardown-Pfad.

/// Nicht portiert (s. o.) — meldet „keine Verletzung" für Audits.
pub fn vspace_wx_ok(_l2_phys: u64) -> u32 {
    0
}
/// Nicht portiert (s. o.).
pub fn vspace_create_base(_l1_phys: u64, _l2_phys: u64) {}
/// Nicht portiert (s. o.).
pub fn vspace_map_block(_l2_phys: u64, _phys: u64) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_map_code_block(_l2_phys: u64, _phys: u64) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_unmap_block(_l2_phys: u64, _phys: u64) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_map_page(
    _l2_phys: u64,
    _phys: u64,
    _perm: UserPerm,
    _alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_map_page_at(
    _l2_phys: u64,
    _va: u64,
    _phys: u64,
    _perm: UserPerm,
    _alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_unmap_page(_l2_phys: u64, _phys: u64) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_map_dma(
    _l2_phys: u64,
    _phys: u64,
    _len: u64,
    _coherent: bool,
    _alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_collect_l3s(_l2_phys: u64, _free_l3: &mut dyn FnMut(u64)) {}
/// Nicht portiert (s. o.).
pub fn vspace_map_device(
    _l1_phys: u64,
    _phys: u64,
    _len: u64,
    _ro: bool,
    _alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    false
}
/// Nicht portiert (s. o.).
pub fn vspace_collect_device_tables(_l1_phys: u64, _free: &mut dyn FnMut(u64)) {}
/// Nicht portiert (s. o.).
pub fn vspace_device_wx_ok(_l1_phys: u64) -> bool {
    true
}
/// Geräte-Block global abbilden — auf x86 ist der gesamte MMIO-Bereich unter 4 GiB bereits
/// uncacheable identity-abgebildet (s. `init_primary`).
pub fn map_device_block_global(_gib: usize) -> bool {
    true
}
