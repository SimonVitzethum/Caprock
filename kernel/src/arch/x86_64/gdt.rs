//! x86_64 GDT mit Ring-0/Ring-3-Segmenten + TSS (Branch arch/x86_64, Stufe 3a/4).
//!
//! Segmentreihenfolge gemäß `syscall`/`sysret`-STAR-Layout: Kernel-Code (0x08), Kernel-Daten (0x10),
//! User-Daten (0x18), User-Code (0x20), dann der **TSS**-Deskriptor (0x28, 16 Byte = 2 Slots). Die
//! TSS trägt `RSP0` — den Kernel-Stack, auf den die CPU bei einem Interrupt/Trap **aus Ring 3**
//! umschaltet (sonst Triple-Fault). Damit darf der LAPIC-Timer Ring-3-Code präemptieren (Stufe 4).

use core::arch::asm;
use core::ptr::addr_of;

pub const KCODE: u16 = 0x08;
#[allow(dead_code)]
pub const KDATA: u16 = 0x10;
pub const UDATA3: u16 = 0x18 | 3;
pub const UCODE3: u16 = 0x20 | 3;
const TSS_SEL: u16 = 0x28;

/// 64-bit Task State Segment (nur `rsp0` wird genutzt — Stack für Ring-3-Interrupts).
#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iopb: u16,
}
impl Tss {
    const fn new() -> Self {
        Tss {
            reserved0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iopb: 0,
        }
    }
}

const INT_STACK_WORDS: usize = 1024; // 8 KiB Interrupt-Stack (RSP0)

// 7 Slots: null, kcode, kdata, udata, ucode, TSS_lo, TSS_hi.
static mut GDT: [u64; 7] = [
    0x0000000000000000, // 0x00 null
    0x00209A0000000000, // 0x08 Kernel-Code  (P, DPL0, S, exec, L)
    0x0000920000000000, // 0x10 Kernel-Daten (P, DPL0, S, write)
    0x0000F20000000000, // 0x18 User-Daten   (P, DPL3, S, write)
    0x0020FA0000000000, // 0x20 User-Code    (P, DPL3, S, exec, L)
    0x0000000000000000, // 0x28 TSS-Deskriptor (low, zur Laufzeit gesetzt)
    0x0000000000000000, //      TSS-Deskriptor (high)
];
static mut TSS: Tss = Tss::new();
static mut INT_STACK: [u64; INT_STACK_WORDS] = [0; INT_STACK_WORDS];

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// 64-bit-TSS-Systemdeskriptor (16 Byte) als zwei u64 bauen.
fn tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
    let low = (limit as u64 & 0xFFFF)
        | ((base & 0x00FF_FFFF) << 16)
        | (0x89u64 << 40) // P=1, DPL0, type=0x9 (available 64-bit TSS)
        | (((limit as u64 >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    let high = base >> 32;
    (low, high)
}

/// GDT (inkl. TSS) bauen + laden, RSP0 setzen, TR laden. Kernel-Code/-Daten liegen an denselben
/// Selektoren wie die Boot-GDT (0x08/0x10) -> kein Segment-Reload nötig.
pub fn init() {
    // SAFETY: einmaliger Aufbau der statischen GDT/TSS (Primärkern). Rohzeiger gegen static_mut_refs.
    unsafe {
        // RSP0 = Spitze des Interrupt-Stacks (rsp0 liegt bei TSS-Offset 4, ggf. unausgerichtet).
        let rsp0 = addr_of!(INT_STACK) as u64 + (INT_STACK_WORDS * 8) as u64;
        let tss = core::ptr::addr_of_mut!(TSS) as *mut u8;
        core::ptr::write_unaligned(tss.add(4) as *mut u64, rsp0);

        // TSS-Deskriptor in GDT[5..7] eintragen.
        let (lo, hi) = tss_descriptor(addr_of!(TSS) as u64, (core::mem::size_of::<Tss>() - 1) as u32);
        let gdt = core::ptr::addr_of_mut!(GDT);
        (*gdt)[5] = lo;
        (*gdt)[6] = hi;

        let gdtr = Gdtr {
            limit: (core::mem::size_of::<[u64; 7]>() - 1) as u16,
            base: gdt as u64,
        };
        asm!("lgdt [{}]", in(reg) &gdtr, options(readonly, nostack, preserves_flags));
        asm!("ltr {0:x}", in(reg) TSS_SEL, options(nostack, preserves_flags));
    }
}
