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
const NG: u64 = 1 << 11; //       non-global: Eintrag ist ASID-spezifisch
const PXN: u64 = 1 << 53; //      Privileged Execute Never
const UXN: u64 = 1 << 54; //      Unprivileged Execute Never

const PAGE: u64 = 4096;
const TWO_MIB: u64 = 2 * 1024 * 1024;
const ONE_GIB: u64 = 1 << 30;
const RAM_BASE: u64 = 0x4000_0000;
/// Ausgabe-Adressbits eines Tabellen-/Seiten-Deskriptors (Bits 47:12).
const ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

/// Zugriffsrecht einer gemappten User-Seite (4 KiB) in einer isolierten VSpace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UserPerm {
    /// EL0 Read+Write, nicht ausführbar (Daten/Stack).
    Rw,
    /// EL0 Read+Execute, read-only (Code, W^X).
    Rx,
    /// EL0 Read-only (Konstanten/rodata).
    Ro,
}

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
///
/// User-**Daten** (`UserRw`) sind `nG` (ASID-spezifisch): die globale SAS-Map nutzt
/// ASID 0, isolierte VSpaces eigene ASIDs — so leckt freies User-RAM der SAS-Map
/// nicht per global-getaggtem TLB-Eintrag in eine isolierte VSpace. User-**Code**
/// (`UserRx`, `.user_text`) bleibt global (nG=0): er wird read-only von allen
/// VSpaces geteilt. Kernel/Device sind ebenfalls global (in jeder VSpace gemappt).
fn normal_leaf(addr: u64, perm: Perm, page: bool) -> u64 {
    let kind = if page { PAGE_DESC } else { BLOCK_DESC };
    let perm_bits = match perm {
        Perm::Rx => AP_RO, //                   ausführbar bei EL1 (PXN=UXN=0)
        Perm::Ro => AP_RO | PXN | UXN,
        Perm::Rw => AP_RW | PXN | UXN,
        Perm::UserRw => AP_RW_EL0 | PXN | UXN | NG, // EL0+EL1 RW, kein Code, ASID-spezifisch
        Perm::UserRx => AP_RO_EL0 | PXN, //      EL0 ausführbar (UXN=0), EL1 nicht (PXN), global
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
        Perm::UserRx // EL0-ausführbarer User-Code (global, von allen VSpaces geteilt)
    } else if addr >= ke {
        // Freies RAM in den ersten 2 MiB: **EL1-only**. Diese L3 wird von jeder
        // isolierten VSpace geteilt (Kernelimage + .user_text); sie darf daher kein
        // EL0-zugängliches freies RAM enthalten. User-RAM wird erst ab 2 MiB
        // alloziert (siehe init_mem-Aufrunden).
        Perm::Rw
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

/// Mindest-Basis für **User**-RAM: 2 MiB ab RAM-Anfang. Die ersten 2 MiB sind die
/// geteilte Kernel-L3 (Kernelimage + `.user_text`, von jeder VSpace genutzt) und
/// enthalten kein EL0-zugängliches freies RAM. Der Allokator beginnt hier.
pub const USER_RAM_MIN: u64 = RAM_BASE + TWO_MIB;

/// Ende von GiB 1 (RAM-Anfang + 1 GiB). Die private User-Region einer isolierten
/// VSpace muss in `[USER_RAM_MIN, GIB1_END)` liegen (die per-PD-L2 deckt GiB 1 ab).
pub const GIB1_END: u64 = RAM_BASE + ONE_GIB;

/// Größe der privaten User-Region je isolierter PD (ein L2-Block).
pub const ISO_REGION_SIZE: u64 = TWO_MIB;

// ---------------------------------------------------------------------------
// Per-Prozess-VSpaces (Weg C, Hybrid): die SAS-Map bleibt für vertrauenswürdige
// PDs; isolierte PDs erhalten eine eigene VSpace, die nur den Kernel (EL1-only,
// geteilt) + ihre eigene User-Region (EL0) mappt. Adressierung bleibt Identity.
// ---------------------------------------------------------------------------

/// Wurzel der globalen SAS-Map (Kernel + alles RAM EL0-zugänglich); ASID 0.
pub fn global_root() -> u64 {
    table_phys(&L1_TABLE)
}

/// `TTBR0_EL1` auf eine VSpace-Wurzel `root` mit `asid` setzen. Kein TLB-Flush:
/// Kernel/Device/Code sind global (nG=0, in jeder VSpace gültig), User-Daten sind
/// `nG` und ASID-getaggt — verschiedene ASIDs kollidieren nicht. Pro Kontextwechsel
/// vom Kernel aufzurufen.
pub fn set_user_vspace(root: u64, asid: u16) {
    let ttbr0 = ((asid as u64) << 48) | root;
    // SAFETY: Schreiben von TTBR0_EL1 (Adressraum-Wurzel) + isb. Kernel/Code sind in
    // jeder VSpace identisch (global) gemappt, daher läuft der Kernel unterbrechungs-
    // frei weiter. MMU-/Registerdomäne.
    unsafe {
        asm!("msr TTBR0_EL1, {}", "isb", in(reg) ttbr0, options(nostack, preserves_flags));
    }
}

/// EL1-only Normal-Block (Kernel sieht das RAM, EL0 nicht), global. Für die
/// „Rest-RAM"-Einträge einer isolierten VSpace (1-GiB- bzw. 2-MiB-Blöcke).
fn kernel_block(addr: u64) -> u64 {
    addr | AF | SH_INNER | ATTR_NORMAL | AP_RW | PXN | UXN | BLOCK_DESC
}

/// User-RW 2-MiB-Block, `nG` (ASID-spezifisch): die eigene Region einer isolierten PD.
fn user_block(addr: u64) -> u64 {
    addr | AF | SH_INNER | ATTR_NORMAL | AP_RW_EL0 | PXN | UXN | NG | BLOCK_DESC
}

/// User-RX 2-MiB-Block, `nG`: privat geladener **Code** (EL0 read+execute, nicht
/// schreibbar -> W^X; an EL1 nicht ausführbar (PXN)).
fn user_code_block(addr: u64) -> u64 {
    addr | AF | SH_INNER | ATTR_NORMAL | AP_RO_EL0 | PXN | NG | BLOCK_DESC
}

/// Ist `desc` ein **gültiger** Deskriptor, der eine **EL0-zugängliche, schreibbare UND
/// ausführbare** Seite/Block beschreibt — also eine **W^X-Verletzung**?
/// EL0-Zugriff = Bit 6 gesetzt (AP_*_EL0); schreibbar = Bit 7 klar (AP[2]=0);
/// EL0-ausführbar = `UXN` (Bit 54) klar.
fn is_wx_violation(desc: u64) -> bool {
    if desc & 0b1 == 0 {
        return false; // ungültiger Eintrag
    }
    let el0 = (desc >> 6) & 1 == 1;
    let writable = (desc >> 7) & 1 == 0;
    let el0_exec = desc & UXN == 0;
    el0 && writable && el0_exec
}

/// **VMM-Property-Walker** (read-only, Fuzzer-Oracle): durchläuft die GiB-1-L2-Tabelle
/// `l2_phys` einer VSpace **und** alle daran hängenden L3-Tabellen und prüft die
/// **W^X-Invariante** (keine EL0-Seite ist gleichzeitig schreibbar und ausführbar)
/// sowie die Struktur (Tabellen-Deskriptoren zeigen 4-KiB-ausgerichtet in den
/// RAM-Bereich). Gibt `0` bei Konsistenz zurück, sonst `1` (W^X-Verletzung) bzw. `2`
/// (struktureller Defekt: L3-Zeiger außerhalb des RAM / unausgerichtet).
pub fn vspace_wx_ok(l2_phys: u64) -> u32 {
    let ram_end = RAM_BASE + (4u64 << 30); // 4 GiB RAM (großzügige Obergrenze)
    // SAFETY: `l2_phys` ist eine gültige, identity-gemappte L2-Tabelle (512 Einträge).
    let l2 = unsafe { core::slice::from_raw_parts(l2_phys as *const u64, 512) };
    for &e in l2.iter() {
        match e & 0b11 {
            0b01 => {
                // 2-MiB-Block direkt.
                if is_wx_violation(e) {
                    return 1;
                }
            }
            0b11 => {
                // Tabellen-Deskriptor -> L3. Zeiger validieren, dann Seiten prüfen.
                let l3_phys = e & ADDR_MASK;
                if l3_phys < RAM_BASE || l3_phys >= ram_end || l3_phys % PAGE != 0 {
                    return 2;
                }
                // SAFETY: validierter, identity-gemappter L3-Frame (512 Einträge).
                let l3 = unsafe { core::slice::from_raw_parts(l3_phys as *const u64, 512) };
                for &p in l3.iter() {
                    if is_wx_violation(p) {
                        return 1;
                    }
                }
            }
            _ => {} // 0b00 = ungültig (z. B. Guard-/ungemappte Seite) -> ok
        }
    }
    0
}

/// Die **Basis** einer isolierten VSpace in zwei frische 4-KiB-Frames bauen
/// (`l1_phys` = Wurzel, `l2_phys` = L2 für GiB 1): Device (EL1-only), das Kernelimage
/// über die **geteilte** Kernel-L3 (inkl. `.user_text` EL0-RX) und alles RAM
/// **EL1-only** (Kernel sieht alles, EL0 nichts). User-Frames werden erst über
/// [`vspace_map_block`] hinzugefügt. Der Aufrufer läuft in der globalen Map
/// (Identity), daher sind `l1_phys`/`l2_phys` direkt beschreibbar.
pub fn vspace_create_base(l1_phys: u64, l2_phys: u64) {
    // SAFETY: frisch allozierte, 4-KiB-ausgerichtete RAM-Frames, in der globalen
    // Identity-Map gültig + beschreibbar. Genau zwei Tabellen werden initialisiert.
    let l1 = unsafe { core::slice::from_raw_parts_mut(l1_phys as *mut u64, 512) };
    let l2 = unsafe { core::slice::from_raw_parts_mut(l2_phys as *mut u64, 512) };

    // L2 (GiB 1): [0] -> gemeinsame Kernel-L3; alles andere EL1-only (kein EL0).
    for (i, e) in l2.iter_mut().enumerate() {
        *e = if i == 0 {
            table_desc(table_phys(&L3_TABLE))
        } else {
            kernel_block(RAM_BASE + i as u64 * TWO_MIB)
        };
    }
    // L1: [0] Device, [1] -> L2 (GiB 1), [2..=8] restliches RAM EL1-only, Rest leer.
    for e in l1.iter_mut() {
        *e = 0;
    }
    l1[0] = DEVICE_BLOCK;
    l1[1] = table_desc(l2_phys);
    for (i, slot) in l1.iter_mut().enumerate().take(9).skip(2) {
        *slot = kernel_block(i as u64 * ONE_GIB);
    }
    cpu::dsb_sy();
}

/// Index eines 2-MiB-Blocks in der GiB-1-L2 für die physische Adresse `phys`.
/// `None`, wenn `phys` nicht in `[USER_RAM_MIN, GIB1_END)` 2-MiB-ausgerichtet liegt.
fn l2_block_index(phys: u64) -> Option<usize> {
    if phys < USER_RAM_MIN || phys >= GIB1_END || phys % TWO_MIB != 0 {
        return None;
    }
    Some(((phys - RAM_BASE) / TWO_MIB) as usize)
}

/// Einen 2-MiB-Frame `phys` als **EL0-RW** (nG) in die VSpace mit GiB-1-Tabelle
/// `l2_phys` mappen (identity). Gibt `false` bei ungültiger/unausgerichteter Adresse.
/// Der Aufrufer muss anschließend die ASID flushen ([`flush_asid`]).
pub fn vspace_map_block(l2_phys: u64, phys: u64) -> bool {
    let Some(idx) = l2_block_index(phys) else {
        return false;
    };
    // SAFETY: `l2_phys` ist eine gültige, in der globalen Map beschreibbare L2-Tabelle.
    let l2 = unsafe { core::slice::from_raw_parts_mut(l2_phys as *mut u64, 512) };
    l2[idx] = user_block(phys);
    cpu::dsb_sy();
    true
}

/// Wie [`vspace_map_block`], aber als **EL0-RX** (privat geladener Code, W^X).
pub fn vspace_map_code_block(l2_phys: u64, phys: u64) -> bool {
    let Some(idx) = l2_block_index(phys) else {
        return false;
    };
    // SAFETY: gültige, in der globalen Map beschreibbare L2-Tabelle.
    let l2 = unsafe { core::slice::from_raw_parts_mut(l2_phys as *mut u64, 512) };
    l2[idx] = user_code_block(phys);
    cpu::dsb_sy();
    true
}

/// Einen zuvor gemappten 2-MiB-Frame `phys` wieder auf **EL1-only** zurücksetzen
/// (EL0 kann nicht mehr darauf zugreifen). Der Aufrufer muss die ASID flushen.
pub fn vspace_unmap_block(l2_phys: u64, phys: u64) -> bool {
    let Some(idx) = l2_block_index(phys) else {
        return false;
    };
    // SAFETY: wie `vspace_map_block`.
    let l2 = unsafe { core::slice::from_raw_parts_mut(l2_phys as *mut u64, 512) };
    l2[idx] = kernel_block(phys);
    cpu::dsb_sy();
    true
}

// --- 4-KiB-Seiten-Mapping (L3) ---
//
// Feingranular gegenüber den 2-MiB-Blöcken: ermöglicht gemischte RX/RO/RW-Rechte,
// Guard Pages und kleine Frames. Wird ein 2-MiB-Block erstmals seitenweise belegt,
// legt der Kernel eine L3-Tabelle an (alle 512 Seiten zunächst **EL1-only**, den
// Block spiegelnd) und hängt sie in die L2 ein; einzelne Seiten werden dann auf EL0
// gesetzt. L3-Frames werden beim Teardown über [`vspace_collect_l3s`] eingesammelt.

/// EL1-only 4-KiB-Seite (Spiegel beim Aufsplitten eines Blocks: Kernel sieht das RAM).
fn kernel_page(phys: u64) -> u64 {
    phys | AF | SH_INNER | ATTR_NORMAL | AP_RW | PXN | UXN | PAGE_DESC
}

/// EL0-User-Seite (4 KiB) mit gegebenem Recht, `nG` (ASID-spezifisch).
fn user_page(phys: u64, perm: UserPerm) -> u64 {
    let bits = match perm {
        UserPerm::Rw => AP_RW_EL0 | PXN | UXN,
        UserPerm::Rx => AP_RO_EL0 | PXN, //        EL0 ausführbar (UXN=0), W^X
        UserPerm::Ro => AP_RO_EL0 | PXN | UXN,
    };
    phys | AF | SH_INNER | ATTR_NORMAL | bits | NG | PAGE_DESC
}

/// Eine einzelne 4-KiB-Seite `phys` (identity) mit `perm` in die VSpace mit
/// GiB-1-L2 `l2_phys` mappen. Existiert für den 2-MiB-Block noch keine L3, wird über
/// `alloc_l3` eine neue 4-KiB-Tabelle angefordert (alle Seiten EL1-only, den Block
/// spiegelnd) und eingehängt. `false` bei ungültiger/unausgerichteter Adresse oder
/// fehlgeschlagener L3-Allokation. Der Aufrufer flusht anschließend die ASID.
pub fn vspace_map_page(
    l2_phys: u64,
    phys: u64,
    perm: UserPerm,
    alloc_l3: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    if phys < USER_RAM_MIN || phys >= GIB1_END || phys % PAGE != 0 {
        return false;
    }
    let i2 = ((phys - RAM_BASE) / TWO_MIB) as usize;
    // SAFETY: gültige, in der globalen Map beschreibbare L2-Tabelle.
    let l2 = unsafe { core::slice::from_raw_parts_mut(l2_phys as *mut u64, 512) };
    let l3_phys = if l2[i2] & 0b11 == TABLE_DESC {
        l2[i2] & ADDR_MASK
    } else {
        let new = match alloc_l3() {
            Some(p) => p,
            None => return false,
        };
        // SAFETY: frischer, identity-gemappter 4-KiB-Frame für die neue L3.
        let l3 = unsafe { core::slice::from_raw_parts_mut(new as *mut u64, 512) };
        let blk = RAM_BASE + i2 as u64 * TWO_MIB;
        for (j, e) in l3.iter_mut().enumerate() {
            *e = kernel_page(blk + j as u64 * PAGE); // zunächst alles EL1-only
        }
        l2[i2] = table_desc(new);
        new
    };
    // SAFETY: gültige L3-Tabelle (gerade angelegt oder bestehend).
    let l3 = unsafe { core::slice::from_raw_parts_mut(l3_phys as *mut u64, 512) };
    l3[((phys >> 12) & 0x1ff) as usize] = user_page(phys, perm);
    cpu::dsb_sy();
    true
}

/// Eine einzelne 4-KiB-Seite wieder auf **EL1-only** zurücksetzen (EL0-Zugriff
/// faultet). `false`, wenn der Block nicht seitenweise gemappt ist. ASID flushen.
pub fn vspace_unmap_page(l2_phys: u64, phys: u64) -> bool {
    if phys < USER_RAM_MIN || phys >= GIB1_END || phys % PAGE != 0 {
        return false;
    }
    let i2 = ((phys - RAM_BASE) / TWO_MIB) as usize;
    // SAFETY: gültige L2-Tabelle.
    let l2 = unsafe { core::slice::from_raw_parts_mut(l2_phys as *mut u64, 512) };
    if l2[i2] & 0b11 != TABLE_DESC {
        return false; // Block (nicht seitenweise) -> keine Einzelseite zu entmappen
    }
    let l3_phys = l2[i2] & ADDR_MASK;
    // SAFETY: gültige L3-Tabelle.
    let l3 = unsafe { core::slice::from_raw_parts_mut(l3_phys as *mut u64, 512) };
    l3[((phys >> 12) & 0x1ff) as usize] = kernel_page(phys);
    cpu::dsb_sy();
    true
}

/// Alle **per-PD-L3-Tabellen** dieser VSpace einsammeln (für den Teardown): ruft
/// `free_l3(l3_phys)` für jeden L2-Eintrag, der auf eine L3 zeigt — außer Index 0
/// (die geteilte Kernel-L3). Danach gibt der Aufrufer L2 + L1 frei.
pub fn vspace_collect_l3s(l2_phys: u64, free_l3: &mut dyn FnMut(u64)) {
    // SAFETY: gültige L2-Tabelle (read-only Scan).
    let l2 = unsafe { core::slice::from_raw_parts(l2_phys as *const u64, 512) };
    for &e in l2.iter().skip(1) {
        if e & 0b11 == TABLE_DESC {
            free_l3(e & ADDR_MASK);
        }
    }
}

/// TLB-Einträge einer ASID invalidieren (nach map/unmap bzw. VSpace-Teardown).
/// Global getaggte Kernel-/Code-Einträge bleiben gültig.
pub fn flush_asid(asid: u16) {
    let arg = (asid as u64) << 48;
    // SAFETY: TLB-Invalidate nach ASID; reine MMU-Wartung + Barrieren.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi aside1, {a}",
            "dsb ish",
            "isb",
            a = in(reg) arg,
            options(nostack, preserves_flags),
        );
    }
}
