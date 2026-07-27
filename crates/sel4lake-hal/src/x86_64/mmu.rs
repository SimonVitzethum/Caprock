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

/// Seitenverzeichnisse für GiB 1..3 in der **isolierten** Sicht: identisch zur globalen Map,
/// aber **ohne** `US` — der Kernel sieht dort alles, ein Ring-3-Thread einer isolierten PD
/// nichts. Sie sind statisch und werden von **allen** isolierten Adressräumen geteilt (sie
/// enthalten keine PD-spezifischen Einträge), sparen also je Adressraum drei Frames.
static mut ISO_PD_HIGH: [Table; MAPPED_GIB - 1] = [const { Table::EMPTY }; MAPPED_GIB - 1];

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    /// Ring-3-Code: eigene Seiten, damit sie `US`+ausführbar sein können, ohne dass der
    /// Kernel-`.text` es wird.
    static __user_text_start: u8;
    static __user_text_end: u8;
    static __user_data_start: u8;
    static __user_data_end: u8;
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
    // SAFETY: wie oben — nur Adressen von Linker-Symbolen.
    let (uts, ute, uds, ude) = unsafe {
        (
            sym(&__user_text_start),
            sym(&__user_text_end),
            sym(&__user_data_start),
            sym(&__user_data_end),
        )
    };
    if pa >= ts && pa < te {
        perm_bits(Perm::Rx) // Kernel-Code: ausführbar, NICHT schreibbar, Ring 0
    } else if pa >= rs && pa < re {
        perm_bits(Perm::Ro) // Konstanten: nur lesbar, NX
    } else if pa >= uts && pa < ute {
        perm_bits(Perm::UserRx) // Ring-3-Code: US + ausführbar, nicht schreibbar (W^X)
    } else if pa >= uds && pa < ude {
        perm_bits(Perm::UserRw) // Ring-3-Daten: US + RW + NX
    } else {
        perm_bits(Perm::Rw) // alles andere: RW, NX, Ring 0
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

        // WICHTIG: Die **Zwischenebenen** tragen `US`. Auf x86 ist die effektive Berechtigung
        // die UND-Verknüpfung über alle vier Ebenen — ohne `US` im PML4E/PDPTE/PDE verweigert die
        // CPU jeden Ring-3-Zugriff, egal was im Blatt-Eintrag steht. Die eigentliche Entscheidung
        // fällt damit ausschließlich am Blatt (dort ist `US` nur für User-Seiten gesetzt), genau
        // wie `AP[1]` auf aarch64.
        pml4.0[0] = (pdpt as *const Table as u64) | P | RW | US;
        for (g, table) in pd.iter_mut().enumerate() {
            pdpt.0[g] = (table as *const Table as u64) | P | RW | US;
        }
        // Erste 16 MiB: 4-KiB-Granularität (seitengenaues W^X über das Kernel-Image).
        for (b, table) in pt.iter_mut().enumerate() {
            for (i, e) in table.0.iter_mut().enumerate() {
                let pa = (b as u64) * TWO_MIB + (i as u64) * PAGE;
                *e = pa | fine_perm(pa);
            }
            pd[0].0[b] = (table as *const Table as u64) | P | RW | US;
        }
        // Rest: 2-MiB-Blöcke, RW + NX; MMIO-Bereiche uncacheable.
        for g in 0..MAPPED_GIB {
            let first = if g == 0 { FINE_BLOCKS } else { 0 };
            for i in first..512 {
                let pa = (g as u64) * ONE_GIB + (i as u64) * TWO_MIB;
                let mut bits = P | RW | NX | PS;
                if is_device(pa) {
                    bits |= PCD | PWT;
                } else if pa >= USER_RAM_MIN {
                    // SAS-Modell (ADR 0002, wie `Perm::UserRw` auf aarch64): das allgemeine
                    // User-RAM ist für Ring 3 les-/schreibbar — Stacks und Daten der
                    // Ring-3-Threads liegen darin. Der Kernel selbst (unter 16 MiB) bleibt
                    // supervisor-only; ein Ring-3-Zugriff dorthin faultet.
                    bits |= US;
                }
                pd[g].0[i] = pa | bits;
            }
        }
        // Isolierte Sicht auf GiB 1..3: dieselben Blöcke, aber ohne `US`.
        let iso = &mut *core::ptr::addr_of_mut!(ISO_PD_HIGH);
        for (k, table) in iso.iter_mut().enumerate() {
            let g = (k + 1) as u64;
            for (i, e) in table.0.iter_mut().enumerate() {
                let pa = g * ONE_GIB + (i as u64) * TWO_MIB;
                let mut bits = P | RW | NX | PS; // kein US -> nur der Kernel sieht es
                if is_device(pa) {
                    bits |= PCD | PWT;
                }
                *e = pa | bits;
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

/// Größte nutzbare Adressraum-Kennung.
///
/// Auf ARM ist die ASID ein **Hardware-Tag** im TLB: ein Adressraumwechsel muss nicht flushen,
/// und eine zu große ASID würde auf eine andere aliasen (Isolationsbruch). Auf x86 gäbe es
/// dafür PCIDs; hier ist die Kennung vorerst eine reine **Software**-Nummer des Kernels, und
/// jeder `CR3`-Wechsel flusht den TLB ohnehin vollständig. Damit kann sie nicht aliasen — die
/// Obergrenze ist reine Buchhaltung. (PCID nachzurüsten ist eine reine Optimierung, s. todo.md.)
pub fn max_asid() -> u16 {
    u16::MAX
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

// --- Per-Prozess-Adressräume (ext-33) ---------------------------------------------------------
//
// Ein **isolierter** Adressraum entsteht aus zwei vom Kernel gelieferten Frames plus einem, den
// die HAL selbst anfordert. Der Aufbau spiegelt exakt das ARM-Modell: **der Kernel sieht alles,
// der User nichts** — und in diese Grundfläche werden die Frames der PD als User-Seiten
// „hineingestanzt".
//
// ```text
//   PML4 (l1) ──[0]──> PDPT (alloc) ──[0]──> PD (l2)   GiB 0: Kernel-Image (geteilte PTs)
//                                    │                        + RAM supervisor-only
//                                    │                        + User-Blöcke der PD (US)
//                                    └─[1..3]─> ISO_PD_HIGH   GiB 1..3 supervisor-only (statisch)
//  ```
//
// Die ersten 16 MiB zeigen auf **dieselben** PTs wie die globale Map: dort liegen Kernel-Code
// (supervisor) und `.user_text`/`.user_data` (US) — genau wie auf ARM die geteilte Kernel-L3.

/// Tabelleneintrag, der auf eine tiefere Ebene zeigt. Zwischenebenen sind **permissiv**
/// (`US|RW`); die Entscheidung fällt am Blatt (s. `init_primary`).
const fn table_desc(phys: u64) -> u64 {
    phys | P | RW | US
}

/// 2-MiB-Block, den **nur der Kernel** sieht (ARM: `kernel_block`).
const fn kernel_block(pa: u64) -> u64 {
    pa | P | RW | NX | PS
}

/// 2-MiB-Block als **User-RW** (ARM: `user_block`).
const fn user_block(pa: u64) -> u64 {
    pa | P | RW | US | NX | PS
}

/// 2-MiB-Block als **User-RX** (privat geladener Code, W^X).
const fn user_code_block(pa: u64) -> u64 {
    pa | P | US | PS
}

/// Eine Tabelle als Slice über ihre physische (identity-gemappte) Adresse.
///
/// # Safety
/// `phys` muss auf eine gültige, 4-KiB-ausgerichtete Seitentabelle zeigen, die in der aktuell
/// aktiven Map beschreibbar ist (der Aufrufer läuft in der globalen Identity-Map).
unsafe fn table_mut(phys: u64) -> &'static mut [u64] {
    unsafe { core::slice::from_raw_parts_mut(phys as *mut u64, 512) }
}

/// Index des 2-MiB-Blocks für `phys` innerhalb von GiB 0 (dort liegen die User-Regionen).
/// `None`, wenn `phys` außerhalb von `[USER_RAM_MIN, GIB1_END)` oder unausgerichtet ist.
fn pd_block_index(phys: u64) -> Option<usize> {
    if phys < USER_RAM_MIN || phys >= GIB1_END || phys % TWO_MIB != 0 {
        return None;
    }
    Some((phys / TWO_MIB) as usize)
}

/// **Grundgerüst eines isolierten Adressraums** anlegen (ARM: `vspace_create_base`).
///
/// `l1_phys` wird die PML4, `l2_phys` das Seitenverzeichnis für GiB 0; die PDPT dazwischen
/// kommt aus `alloc` (x86 hat eine Ebene mehr als ARM). Gibt `false`, wenn `alloc` nichts
/// liefert — dann wurde **nichts** verändert.
pub fn vspace_create_base(
    l1_phys: u64,
    l2_phys: u64,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    let Some(pdpt_phys) = alloc() else {
        return false;
    };
    // SAFETY: frisch allozierte, 4-KiB-ausgerichtete RAM-Frames; der Aufrufer läuft in der
    // globalen Identity-Map, in der sie beschreibbar sind. Genau drei Tabellen werden gefüllt.
    unsafe {
        let pd = table_mut(l2_phys);
        let global_pt = &*core::ptr::addr_of!(PT);
        for (i, e) in pd.iter_mut().enumerate() {
            *e = if i < FINE_BLOCKS {
                // Kernel-Image + `.user_text`/`.user_data`: dieselben PTs wie global (geteilt).
                table_desc(&global_pt[i] as *const Table as u64)
            } else {
                kernel_block((i as u64) * TWO_MIB) // RAM: nur der Kernel
            };
        }
        let pdpt = table_mut(pdpt_phys);
        let iso_high = &*core::ptr::addr_of!(ISO_PD_HIGH);
        for (g, e) in pdpt.iter_mut().enumerate() {
            *e = match g {
                0 => table_desc(l2_phys),
                1..=3 => table_desc(&iso_high[g - 1] as *const Table as u64),
                _ => 0, // oberhalb der abgebildeten 4 GiB: nichts
            };
        }
        let pml4 = table_mut(l1_phys);
        for e in pml4.iter_mut() {
            *e = 0;
        }
        pml4[0] = table_desc(pdpt_phys);
    }
    cpu::dsb_sy();
    true
}

/// Einen 2-MiB-Frame als **User-RW** in den Adressraum mit Seitenverzeichnis `l2_phys` mappen.
pub fn vspace_map_block(l2_phys: u64, phys: u64) -> bool {
    let Some(idx) = pd_block_index(phys) else {
        return false;
    };
    // SAFETY: gültiges, in der globalen Map beschreibbares Seitenverzeichnis.
    unsafe { table_mut(l2_phys)[idx] = user_block(phys) };
    cpu::dsb_sy();
    true
}

/// Wie [`vspace_map_block`], aber als **User-RX** (privat geladener Code, W^X).
pub fn vspace_map_code_block(l2_phys: u64, phys: u64) -> bool {
    let Some(idx) = pd_block_index(phys) else {
        return false;
    };
    // SAFETY: wie `vspace_map_block`.
    unsafe { table_mut(l2_phys)[idx] = user_code_block(phys) };
    cpu::dsb_sy();
    true
}

/// Einen Block wieder auf **supervisor-only** zurücksetzen (der User kommt nicht mehr heran).
pub fn vspace_unmap_block(l2_phys: u64, phys: u64) -> bool {
    let Some(idx) = pd_block_index(phys) else {
        return false;
    };
    // SAFETY: wie `vspace_map_block`.
    unsafe { table_mut(l2_phys)[idx] = kernel_block(phys) };
    cpu::dsb_sy();
    true
}

/// Blatt-Bits für eine 4-KiB-User-Seite.
const fn user_page(pa: u64, perm: UserPerm) -> u64 {
    match perm {
        UserPerm::Ro => pa | P | US | NX,
        UserPerm::Rw => pa | P | RW | US | NX,
        UserPerm::Rx => pa | P | US, // ausführbar, nicht schreibbar (W^X)
    }
}

/// Die Seitentabelle für den 2-MiB-Block `idx` beschaffen: existiert dort noch ein Block,
/// wird er in eine Tabelle **aufgeteilt** (alle Seiten zunächst supervisor-only, damit sich
/// die Sicht des Kernels nicht ändert). `None`, wenn kein Frame verfügbar ist.
unsafe fn pt_for(pd: &mut [u64], idx: usize, alloc: &mut dyn FnMut() -> Option<u64>) -> Option<u64> {
    let e = pd[idx];
    if e & PS == 0 && e & P != 0 {
        return Some(e & 0x000f_ffff_ffff_f000); // schon eine Tabelle
    }
    let pt_phys = alloc()?;
    // SAFETY: frisch alloziertes, identity-gemapptes Frame.
    let pt = unsafe { table_mut(pt_phys) };
    let base = (idx as u64) * TWO_MIB;
    for (i, slot) in pt.iter_mut().enumerate() {
        *slot = (base + (i as u64) * PAGE) | P | RW | NX; // supervisor, wie vorher der Block
    }
    pd[idx] = table_desc(pt_phys);
    Some(pt_phys)
}

/// Einen 4-KiB-Frame identity als User-Seite mappen (feingranular, ARM: `vspace_map_page`).
pub fn vspace_map_page(
    l2_phys: u64,
    phys: u64,
    perm: UserPerm,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    vspace_map_page_at(l2_phys, phys, phys, perm, alloc)
}

/// Wie [`vspace_map_page`], aber an einer **wählbaren** virtuellen Adresse.
pub fn vspace_map_page_at(
    l2_phys: u64,
    va: u64,
    phys: u64,
    perm: UserPerm,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    if va % PAGE != 0 || phys % PAGE != 0 || va >= GIB1_END {
        return false;
    }
    let idx = (va / TWO_MIB) as usize;
    if idx < FINE_BLOCKS {
        return false; // Kernel-Image-Bereich wird nicht überschrieben
    }
    // SAFETY: gültiges, beschreibbares Seitenverzeichnis; `pt_for` liefert eine gültige Tabelle.
    unsafe {
        let pd = table_mut(l2_phys);
        let Some(pt_phys) = pt_for(pd, idx, alloc) else {
            return false;
        };
        let pt = table_mut(pt_phys);
        pt[((va % TWO_MIB) / PAGE) as usize] = user_page(phys, perm);
    }
    cpu::dsb_sy();
    true
}

/// Eine zuvor gemappte User-Seite wieder auf supervisor-only zurücksetzen.
pub fn vspace_unmap_page(l2_phys: u64, phys: u64) -> bool {
    if phys % PAGE != 0 || phys >= GIB1_END {
        return false;
    }
    let idx = (phys / TWO_MIB) as usize;
    // SAFETY: gültiges, beschreibbares Seitenverzeichnis.
    unsafe {
        let pd = table_mut(l2_phys);
        let e = pd[idx];
        if e & P == 0 || e & PS != 0 {
            return false; // kein feingranularer Block -> nichts zu entfernen
        }
        let pt = table_mut(e & 0x000f_ffff_ffff_f000);
        let slot = ((phys % TWO_MIB) / PAGE) as usize;
        pt[slot] = phys | P | RW | NX; // wieder supervisor-only
    }
    cpu::dsb_sy();
    true
}

/// DMA-Puffer in einen isolierten Adressraum mappen (User-RW; `coherent` steuert die
/// Cache-Attribute — auf x86 ist DMA hardware-kohärent, nicht-kohärente Puffer werden
/// dennoch uncacheable gemappt, damit die Semantik dieselbe bleibt wie auf ARM).
pub fn vspace_map_dma(
    l2_phys: u64,
    phys: u64,
    len: u64,
    coherent: bool,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    if len == 0 || len % PAGE != 0 {
        return false;
    }
    let mut off = 0;
    while off < len {
        if !vspace_map_page(l2_phys, phys + off, UserPerm::Rw, alloc) {
            return false;
        }
        if !coherent {
            // Blatt nachträglich auf uncacheable stellen.
            let va = phys + off;
            let idx = (va / TWO_MIB) as usize;
            // SAFETY: die Seite wurde eben gemappt, die Tabellen sind gültig.
            unsafe {
                let pd = table_mut(l2_phys);
                let pt = table_mut(pd[idx] & 0x000f_ffff_ffff_f000);
                let slot = ((va % TWO_MIB) / PAGE) as usize;
                pt[slot] |= PCD | PWT;
            }
        }
        off += PAGE;
    }
    cpu::dsb_sy();
    true
}

/// **W^X-Audit** eines isolierten Adressraums: `0` = keine Verletzung, sonst die Anzahl der
/// Einträge, die zugleich für den User schreibbar **und** ausführbar sind.
pub fn vspace_wx_ok(l2_phys: u64) -> u32 {
    let mut bad = 0;
    // SAFETY: gültiges, in der globalen Map lesbares Seitenverzeichnis.
    unsafe {
        let pd = table_mut(l2_phys);
        for (i, &e) in pd.iter().enumerate() {
            if e & P == 0 {
                continue;
            }
            if e & PS != 0 {
                if e & US != 0 && e & RW != 0 && e & NX == 0 {
                    bad += 1;
                }
            } else if i >= FINE_BLOCKS {
                // Nur selbst angelegte Tabellen prüfen (die ersten sind die geteilten Kernel-PTs).
                let pt = table_mut(e & 0x000f_ffff_ffff_f000);
                for &p in pt.iter() {
                    if p & P != 0 && p & US != 0 && p & RW != 0 && p & NX == 0 {
                        bad += 1;
                    }
                }
            }
        }
    }
    bad
}

/// Die **selbst angelegten** Seitentabellen eines Adressraums einsammeln (Teardown).
/// Die geteilten Kernel-PTs der ersten 16 MiB bleiben unangetastet.
pub fn vspace_collect_l3s(l2_phys: u64, free_l3: &mut dyn FnMut(u64)) {
    // SAFETY: gültiges, lesbares Seitenverzeichnis.
    unsafe {
        let pd = table_mut(l2_phys);
        for (i, e) in pd.iter_mut().enumerate().skip(FINE_BLOCKS) {
            if *e & P != 0 && *e & PS == 0 {
                free_l3(*e & 0x000f_ffff_ffff_f000);
                *e = 0;
            }
        }
    }
}

/// Geräte-MMIO in einen isolierten Adressraum mappen — auf x86 liegt MMIO oberhalb von GiB 0
/// in **geteilten**, supervisor-only Tabellen (`ISO_PD_HIGH`). Eine PD-eigene Geräteabbildung
/// bräuchte dort eine private Tabelle; das kommt mit dem HardwareLand-Port (s. todo.md).
pub fn vspace_map_device(
    _l1_phys: u64,
    _phys: u64,
    _len: u64,
    _ro: bool,
    _alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    false
}

/// Die PDPT eines Adressraums einsammeln (x86 hat eine Ebene mehr als ARM; der Kernel ruft
/// dies im Teardown mit der PML4 auf, s. `vspace_collect_l3s` für die unteren Ebenen).
pub fn vspace_collect_device_tables(l1_phys: u64, free: &mut dyn FnMut(u64)) {
    // SAFETY: gültige, lesbare PML4 des abzubauenden Adressraums.
    unsafe {
        let pml4 = table_mut(l1_phys);
        if pml4[0] & P != 0 {
            free(pml4[0] & 0x000f_ffff_ffff_f000);
            pml4[0] = 0;
        }
    }
}

/// Es gibt keine PD-eigenen Gerätetabellen (s. [`vspace_map_device`]) -> nichts zu verletzen.
pub fn vspace_device_wx_ok(_l1_phys: u64) -> bool {
    true
}

/// Geräte-Block global abbilden — auf x86 ist der gesamte MMIO-Bereich unter 4 GiB bereits
/// uncacheable identity-abgebildet (s. `init_primary`).
pub fn map_device_block_global(_gib: usize) -> bool {
    true
}
