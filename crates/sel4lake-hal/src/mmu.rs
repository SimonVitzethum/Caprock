//! MMU: eine einzige, statische Identity-Map mit aktivierten Caches und **W^X**
//! (ADR 0002).
//!
//! Es gibt **keine** per-Prozess-Adressräume und **keine** Adressübersetzung:
//! virtuell == physisch. Die MMU dient (a) Caches/Speculation (Performance) und
//! (b) groben Schutzattributen. Isolation kommt aus Rust + Capabilities, nicht
//! aus per-Prozess-Page-Tables.
//!
//! Layout (4-KiB-Granule, 39-bit VA, Top-Level = L1):
//! ```text
//!  L1[0]      0..1 GiB   Device-nGnRnE, XN         (MMIO: UART, GIC, …)
//!  L1[1]      1..2 GiB   -> L2  (enthält das Kernelimage; feinere Rechte)
//!  L1[2..=8]  2..9 GiB   Normal WB cacheable, RW + XN   (freies RAM)
//!
//!  L2[0]      +0..2 MiB  -> L3  (Kernel-Sektionen, 4-KiB-Seiten)
//!  L2[1..]    +2 MiB..   Normal WB cacheable, RW + XN   (freies RAM in GiB 1)
//!
//!  L3[i]      4-KiB-Seite: .text = R-X, .rodata = R--, sonst RW + XN
//! ```
//!
//! Sämtliches `unsafe` hier ist MMU-/Registerinitialisierung — erlaubte Domäne.

use crate::cpu;
use core::arch::asm;
use core::cell::UnsafeCell;

// Vom Linker bereitgestellte Sektionsgrenzen (4-KiB-ausgerichtet).
extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __user_text_start: u8;
    static __user_text_end: u8;
    static __kernel_end: u8;
}

/// 4-KiB-ausgerichtete Übersetzungstabelle (512 × 64-bit Deskriptoren).
#[repr(C, align(4096))]
struct PageTable([u64; 512]);

/// Sync-Wrapper, damit Tabellen `static` sein können. Schreibzugriff erfolgt nur
/// einmalig durch den Primärkern (vor SMP-Start); danach effektiv read-only.
struct TableStore(UnsafeCell<PageTable>);
// SAFETY: siehe oben — einmaliger, alleiniger Schreibzugriff vor SMP.
unsafe impl Sync for TableStore {}

static L1_TABLE: TableStore = TableStore(UnsafeCell::new(PageTable([0; 512])));
static L2_TABLE: TableStore = TableStore(UnsafeCell::new(PageTable([0; 512])));
static L3_TABLE: TableStore = TableStore(UnsafeCell::new(PageTable([0; 512])));

// --- Deskriptor-Bits (ARMv8-A, Stage-1) ---
const TABLE_DESC: u64 = 0b11; // Tabellen-Deskriptor (zeigt auf nächste Ebene)
const BLOCK_DESC: u64 = 0b01; // Block-Deskriptor (L1: 1 GiB, L2: 2 MiB)
const PAGE_DESC: u64 = 0b11; //  Seiten-Deskriptor (L3: 4 KiB)
const AF: u64 = 1 << 10; //      Access Flag
const SH_INNER: u64 = 0b11 << 8; // Inner Shareable (für Normal-Memory)
const AP_RW: u64 = 0b00 << 6; //  RW, nur EL1
const AP_RO: u64 = 0b10 << 6; //  RO, nur EL1
const AP_RW_EL0: u64 = 0b01 << 6; // RW, EL0 + EL1 (User-Daten/Stacks)
const AP_RO_EL0: u64 = 0b11 << 6; // RO, EL0 + EL1 (User-Code)
const ATTR_DEVICE: u64 = 0 << 2; // MAIR-Index 0
const ATTR_NORMAL: u64 = 1 << 2; // MAIR-Index 1
const PXN: u64 = 1 << 53; //      Privileged Execute Never
const UXN: u64 = 1 << 54; //      Unprivileged Execute Never

const PAGE: u64 = 4096;
const TWO_MIB: u64 = 2 * 1024 * 1024;
const ONE_GIB: u64 = 1 << 30;
const RAM_BASE: u64 = 0x4000_0000;

/// Zugriffsrecht eines Normal-Memory-Leafs.
#[derive(Clone, Copy)]
enum Perm {
    /// Kernel Read + Execute (Code, nur EL1).
    Rx,
    /// Kernel Read only (Konstanten, nur EL1).
    Ro,
    /// Kernel Read + Write, niemals ausführbar (Kernel-Daten/Stacks, nur EL1).
    Rw,
    /// User Read + Write, niemals ausführbar (User-Daten/Stacks, EL0+EL1).
    UserRw,
    /// User Read + Execute (User-Code; EL0 ausführbar, EL1 nicht (PXN)).
    UserRx,
}

/// Device-Block (nicht-cacheable, XN, RW). Für die MMIO-1-GiB-Region.
const DEVICE_BLOCK: u64 = BLOCK_DESC | AF | AP_RW | ATTR_DEVICE | PXN | UXN;

/// Normal-Memory-Leaf (Block oder Page) mit gegebenem Recht erzeugen.
fn normal_leaf(addr: u64, perm: Perm, page: bool) -> u64 {
    let kind = if page { PAGE_DESC } else { BLOCK_DESC };
    let perm_bits = match perm {
        Perm::Rx => AP_RO, //                   ausführbar bei EL1 (PXN=UXN=0)
        Perm::Ro => AP_RO | PXN | UXN,
        Perm::Rw => AP_RW | PXN | UXN,
        Perm::UserRw => AP_RW_EL0 | PXN | UXN, // EL0+EL1 RW, kein Code
        Perm::UserRx => AP_RO_EL0 | PXN, //      EL0 ausführbar (UXN=0), EL1 nicht (PXN)
    };
    addr | AF | SH_INNER | ATTR_NORMAL | perm_bits | kind
}

/// Tabellen-Deskriptor auf die nächste Ebene.
fn table_desc(next_table_phys: u64) -> u64 {
    next_table_phys | TABLE_DESC
}

fn sym(addr_of: &u8) -> u64 {
    addr_of as *const u8 as u64
}

/// Recht für die 4-KiB-Seite, die bei `addr` beginnt (Kernel-Sektionen).
fn perm_for_page(addr: u64) -> Perm {
    // SAFETY: nur Adressberechnung über Linker-Symbole, kein Speicherzugriff.
    let (ts, te, rs, re, us, ue, ke) = unsafe {
        (
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
            sym(&__user_text_start),
            sym(&__user_text_end),
            sym(&__kernel_end),
        )
    };
    if addr >= ts && addr < te {
        Perm::Rx
    } else if addr >= rs && addr < re {
        Perm::Ro
    } else if addr >= us && addr < ue {
        Perm::UserRx // EL0-ausführbarer User-Code
    } else if addr >= ke {
        Perm::UserRw // freies RAM oberhalb des Images (< 2 MiB): EL0-zugänglich
    } else {
        // Kernel-Daten/BSS/Stacks (EL1-only).
        Perm::Rw
    }
}

fn table_mut(store: &TableStore) -> &mut [u64; 512] {
    // SAFETY: einmaliger, alleiniger Schreibzugriff durch den Primärkern vor SMP.
    unsafe { &mut (*store.0.get()).0 }
}

fn table_phys(store: &TableStore) -> u64 {
    store.0.get() as u64
}

/// Identity-Map mit W^X in die statischen Tabellen schreiben (nur Primärkern).
fn build_tables() {
    // L3: erste 2 MiB des RAM (Kernelimage) seitenweise mit Sektionsrechten.
    let l3 = table_mut(&L3_TABLE);
    for (i, entry) in l3.iter_mut().enumerate() {
        let addr = RAM_BASE + i as u64 * PAGE;
        *entry = normal_leaf(addr, perm_for_page(addr), true);
    }

    // L2: [0] -> L3 ; [1..] = restliches GiB 1 als 2-MiB-Blöcke, EL0-zugänglich
    // (freies RAM für User-Stacks/-Daten).
    let l2 = table_mut(&L2_TABLE);
    l2[0] = table_desc(table_phys(&L3_TABLE));
    for (i, entry) in l2.iter_mut().enumerate().skip(1) {
        let addr = RAM_BASE + i as u64 * TWO_MIB;
        *entry = normal_leaf(addr, Perm::UserRw, false);
    }

    // L1: [0] Device ; [1] -> L2 ; [2..=8] freies RAM (EL0-zugänglich).
    let l1 = table_mut(&L1_TABLE);
    l1[0] = DEVICE_BLOCK; // 0..1 GiB (MMIO)
    l1[1] = table_desc(table_phys(&L2_TABLE)); // 1..2 GiB (Kernel-GiB)
    for i in 2..=8u64 {
        l1[i as usize] = normal_leaf(i * ONE_GIB, Perm::UserRw, false);
    }

    // Sicherheitsnetz: das Kernelimage muss in die ersten 2 MiB passen, sonst
    // läge Code in einem nur-RW-Block (nicht ausführbar). Bisher ~1 MiB.
    // SAFETY: reine Adressberechnung.
    let kernel_end = unsafe { sym(&__kernel_end) };
    if kernel_end > RAM_BASE + TWO_MIB {
        crate::console::emit_raw("[mmu] WARNUNG: Kernelimage > 2 MiB, W^X-Mapping unvollständig!\n");
    }
}

// MAIR: attr0 = Device-nGnRnE (0x00), attr1 = Normal WB R/W-allocate (0xFF).
const MAIR_VALUE: u64 = 0x00 | (0xFF << 8);

// TCR_EL1: T0SZ=25 (39-bit VA), 4-KiB-Granule, WB+inner-shareable Walks,
// TTBR1 deaktiviert, IPS=40-bit.
const TCR_VALUE: u64 = 25            // T0SZ
    | (0b01 << 8)                    // IRGN0 = WB
    | (0b01 << 10)                   // ORGN0 = WB
    | (0b11 << 12)                   // SH0 = inner shareable
    | (0b00 << 14)                   // TG0 = 4 KiB
    | (1 << 23)                      // EPD1 = TTBR1 aus
    | (0b010 << 32); //                IPS = 40-bit

const SCTLR_M: u64 = 1 << 0; //   MMU an
const SCTLR_C: u64 = 1 << 2; //   Data-Cache an
const SCTLR_I: u64 = 1 << 12; //  Instruction-Cache an

/// MMU + Caches am aktuellen Kern aktivieren (Tabellen müssen bereits stehen).
fn enable() {
    let ttbr0 = table_phys(&L1_TABLE);
    // SAFETY: vollständige MMU-Aktivierungssequenz (Registerinit + Barrieren).
    // Der ausgeführte Code liegt in Normal-RAM (.text, R-X), daher läuft die
    // Instruktionsausführung unterbrechungsfrei weiter (VA == PA).
    unsafe {
        asm!(
            "msr MAIR_EL1, {mair}",
            "msr TCR_EL1, {tcr}",
            "msr TTBR0_EL1, {ttbr0}",
            "dsb sy",
            "isb",
            "tlbi vmalle1",
            "ic  iallu",
            "dsb sy",
            "isb",
            mair = in(reg) MAIR_VALUE,
            tcr = in(reg) TCR_VALUE,
            ttbr0 = in(reg) ttbr0,
            options(nostack, preserves_flags),
        );

        let mut sctlr: u64;
        asm!("mrs {}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
        sctlr |= SCTLR_M | SCTLR_C | SCTLR_I;
        asm!("msr SCTLR_EL1, {}", in(reg) sctlr, options(nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
    cpu::dsb_sy();
}

/// Primärkern: Tabellen bauen + MMU aktivieren.
pub fn init_primary() {
    build_tables();
    enable();
}

/// Sekundärkern: dieselbe (bereits gebaute) Tabelle aktivieren.
pub fn init_secondary() {
    enable();
}

/// (M, C, I)-Bits aus `SCTLR_EL1` zur Verifikation.
pub fn sctlr_flags() -> (bool, bool, bool) {
    let sctlr: u64;
    // SAFETY: read-only Systemregister.
    unsafe {
        asm!("mrs {}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
    }
    (
        sctlr & SCTLR_M != 0,
        sctlr & SCTLR_C != 0,
        sctlr & SCTLR_I != 0,
    )
}

/// Physische Endadresse des Kernelimages (= erste freie RAM-Adresse), 4-KiB-aligned.
pub fn kernel_end() -> u64 {
    // SAFETY: reine Adressberechnung über ein Linker-Symbol.
    unsafe { sym(&__kernel_end) }
}
