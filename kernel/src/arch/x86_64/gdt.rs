//! x86_64 Global Descriptor Table mit Ring-0- + Ring-3-Segmenten (Branch arch/x86_64, Stufe 3a).
//!
//! Das Boot-Trampolin nutzte eine minimale GDT (nur Kernel-Code/-Daten). Für `syscall`/`sysret` +
//! Ring-3 brauchen wir zusätzlich User-Segmente in der von `syscall`/`sysret` vorgeschriebenen
//! Reihenfolge (STAR-Layout): Kernel-Code (0x08), Kernel-Daten (0x10), User-Daten (0x18),
//! User-Code (0x20). `sysretq`: CS = STAR_base+16, SS = STAR_base+8 (RPL 3); `syscall`: CS =
//! STAR_base, SS = +8. Eine TSS folgt mit Ring-3-Interrupts (Stufe 4); der Stufe-3a-Demo läuft mit
//! maskierten Interrupts (IF=0), daher hier noch ohne TSS.

use core::arch::asm;
use core::ptr::addr_of;

// Selektoren (Index<<3 | RPL).
pub const KCODE: u16 = 0x08;
#[allow(dead_code)]
pub const KDATA: u16 = 0x10;
pub const UDATA3: u16 = 0x18 | 3;
pub const UCODE3: u16 = 0x20 | 3;

static mut GDT: [u64; 5] = [
    0x0000000000000000, // 0x00 null
    0x00209A0000000000, // 0x08 Kernel-Code  (P, DPL0, S, exec, L)
    0x0000920000000000, // 0x10 Kernel-Daten (P, DPL0, S, write)
    0x0000F20000000000, // 0x18 User-Daten   (P, DPL3, S, write)
    0x0020FA0000000000, // 0x20 User-Code    (P, DPL3, S, exec, L)
];

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// Neue GDT laden. Kernel-Code/-Daten liegen an denselben Selektoren wie die Boot-GDT (0x08/0x10),
/// daher ist kein Segment-Reload nötig — die User-Segmente (0x18/0x20) kommen hinzu.
pub fn init() {
    // SAFETY: einmaliges Laden der statischen GDT (Primärkern). Rohzeiger vermeidet static_mut_refs.
    unsafe {
        let base = addr_of!(GDT) as u64;
        let gdtr = Gdtr {
            limit: (core::mem::size_of::<[u64; 5]>() - 1) as u16,
            base,
        };
        asm!("lgdt [{}]", in(reg) &gdtr, options(readonly, nostack, preserves_flags));
    }
}
