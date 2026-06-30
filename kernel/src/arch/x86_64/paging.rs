//! x86_64 4-Level-Paging mit W^X (Branch arch/x86_64, Stufe 1).
//!
//! Ersetzt die grobe 1-GiB-Identity-Map des Boot-Trampolins durch eine Rust-verwaltete
//! Tabellenhierarchie (PML4 → PDPT → PD → PT). Die ersten 16 MiB werden identitätsgemappt, mit
//! **W^X pro 4-KiB-Seite**: `.text` = R-X (NX=0, RW=0), `.rodata` = R-- (NX=1, RW=0), alles übrige
//! (.data/.bss/Stack/Tabellen/low) = RW + NX. Honoriert wird das über **EFER.NXE** (NX-Bit) und
//! **CR0.WP** (auch Ring 0 respektiert das Read-only-Bit). Pendant zu `sel4lake-hal::mmu` (aarch64).

use super::{emit_raw, put_hex};
use core::arch::asm;
use core::ptr::addr_of_mut;

const P: u64 = 1 << 0; //  present
const RW: u64 = 1 << 1; // writable
const NX: u64 = 1 << 63; // no-execute (braucht EFER.NXE)
const PAGE: u64 = 4096;
const NPT: usize = 8; // 8 PTs × 2 MiB = 16 MiB identitätsgemappt

#[repr(C, align(4096))]
struct Table([u64; 512]);

static mut PML4: Table = Table([0; 512]);
static mut PDPT: Table = Table([0; 512]);
static mut PD: Table = Table([0; 512]);
static mut PT: [Table; NPT] = [const { Table([0; 512]) }; NPT];

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// W^X-Flags für die identitätsgemappte 4-KiB-Seite bei `addr`.
fn page_flags(addr: u64) -> u64 {
    // SAFETY: nur Adressberechnung über Linker-Symbole, kein Speicherzugriff.
    let (ts, te, rs, re) = unsafe {
        (
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
        )
    };
    if addr >= ts && addr < te {
        P // .text: present, read-only, ausführbar (NX=0)
    } else if addr >= rs && addr < re {
        P | NX // .rodata: present, read-only, nicht ausführbar
    } else {
        P | RW | NX // Rest: RW, nie ausführbar
    }
}

/// EFER.NXE (MSR 0xC000_0080, Bit 11) setzen: das NX-Bit in den Tabellen wird honoriert.
fn enable_nxe() {
    // SAFETY: read-modify-write des EFER-MSR (nur NXE-Bit); kein Speichereffekt.
    unsafe {
        let (mut lo, hi): (u32, u32);
        asm!("rdmsr", in("ecx") 0xC000_0080u32, out("eax") lo, out("edx") hi, options(nomem, nostack));
        lo |= 1 << 11;
        asm!("wrmsr", in("ecx") 0xC000_0080u32, in("eax") lo, in("edx") hi, options(nomem, nostack));
    }
}

/// Tabellen bauen (16 MiB Identity, W^X), dann CR3 laden + CR0.WP setzen.
pub fn init() {
    enable_nxe();
    // SAFETY: einmaliger, alleiniger Aufbau der statischen Tabellen vor SMP (Primärkern). Danach
    // CR3-Wechsel: der laufende Code (.text, R-X), der Stack (RW) und die Tabellen (RW) bleiben in
    // der neuen Map identisch gemappt -> Ausführung läuft unterbrechungsfrei weiter.
    unsafe {
        let pml4 = addr_of_mut!(PML4) as *mut Table;
        let pdpt = addr_of_mut!(PDPT) as *mut Table;
        let pd = addr_of_mut!(PD) as *mut Table;
        let pt0 = addr_of_mut!(PT) as *mut Table;

        (*pml4).0[0] = (pdpt as u64) | P | RW;
        (*pdpt).0[0] = (pd as u64) | P | RW;
        for i in 0..NPT {
            let pti = pt0.add(i);
            (*pd).0[i] = (pti as u64) | P | RW;
            for j in 0..512 {
                let addr = (i as u64 * 512 + j as u64) * PAGE;
                (*pti).0[j] = addr | page_flags(addr);
            }
        }

        asm!("mov cr3, {}", in(reg) pml4 as u64, options(nostack, preserves_flags));

        // CR0.WP (Bit 16): auch Ring 0 ehrt das Read-only-Bit -> echtes W^X für Kernel-Seiten.
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 |= 1 << 16;
        asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags));
    }
}

/// Bericht: Sektionsgrenzen der aktiven W^X-Map (Diagnose).
pub fn report() {
    // SAFETY: nur Adressberechnung über Linker-Symbole.
    let (ts, te, rs, re) = unsafe {
        (
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
        )
    };
    emit_raw("paging  : .text(R-X)=[");
    put_hex(ts);
    emit_raw(",");
    put_hex(te);
    emit_raw(") .rodata(R--)=[");
    put_hex(rs);
    emit_raw(",");
    put_hex(re);
    emit_raw(")\n");
}

/// PTE einer identitätsgemappten Adresse aus den statischen Tabellen lesen (16-MiB-Fenster).
fn pte(addr: u64) -> u64 {
    let page = (addr / PAGE) as usize;
    let (ti, pi) = (page / 512, page % 512);
    if ti >= NPT {
        return 0;
    }
    // SAFETY: read-only Zugriff auf die statische, identity-gemappte PT-Tabelle.
    unsafe {
        let pt0 = addr_of_mut!(PT) as *const Table;
        (*pt0.add(ti)).0[pi]
    }
}

/// W^X-Bits stichprobenartig prüfen (nicht-invasiv): `.text` muss R-X (RW=0, NX=0), `.rodata` muss
/// R-- (RW=0, NX=1) sein. Die *Durchsetzung* ist separat per #PF bewiesen (ein Write auf `.rodata`
/// faultet mit cr2=.rodata) — diese Funktion bestätigt die Tabellen-Bits, ohne den Boot zu beenden.
pub fn verify_wx() -> bool {
    // SAFETY: nur Adressberechnung über Linker-Symbole.
    let (t, r) = unsafe { (sym(&__text_start), sym(&__rodata_start)) };
    let tpte = pte(t);
    let rpte = pte(r);
    let text_ok = (tpte & RW) == 0 && (tpte & NX) == 0; // R-X
    let rodata_ok = (rpte & RW) == 0 && (rpte & NX) != 0; // R-- (nicht ausführbar)
    text_ok && rodata_ok
}
