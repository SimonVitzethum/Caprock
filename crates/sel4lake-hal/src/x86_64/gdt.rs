//! GDT + TSS (x86_64).
//!
//! Im Long Mode ist Segmentierung weitgehend abgeschaltet — die GDT wird trotzdem gebraucht:
//! sie liefert die **Selektoren** für Ring 0 und Ring 3 (Privilegwechsel per `iretq`/`syscall`
//! laufen über CS/SS) und die **TSS** mit `RSP0`: dorthin schaltet die CPU den Stack um, wenn
//! ein Interrupt aus Ring 3 eintrifft. Ohne gültiges `RSP0` würde jeder Trap aus dem User-Modus
//! auf dem User-Stack landen — ein direkter Privilegienbruch.

use core::arch::asm;

#[repr(C, packed)]
struct Tss {
    _res0: u32,
    /// Stackzeiger, auf den bei einem Trap **aus Ring 3** umgeschaltet wird.
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    _res1: u64,
    ist: [u64; 7],
    _res2: u64,
    _res3: u16,
    iomap_base: u16,
}

static mut TSS: Tss = Tss {
    _res0: 0,
    rsp0: 0,
    rsp1: 0,
    rsp2: 0,
    _res1: 0,
    ist: [0; 7],
    _res2: 0,
    _res3: 0,
    iomap_base: core::mem::size_of::<Tss>() as u16, // kein I/O-Bitmap -> Ring 3 darf kein Port-I/O
};

/// GDT: null, kcode, kdata, udata, ucode, TSS (zwei Slots — der TSS-Deskriptor ist 16 Byte).
static mut GDT: [u64; 7] = [0; 7];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Code-/Datensegment-Deskriptor im Long Mode: nur die Flags zählen (Basis/Limit ignoriert).
const fn seg(code: bool, dpl: u64) -> u64 {
    let mut d = (1 << 44) | (1 << 47) | (dpl << 45); // S=1 (nicht-System), P=1, DPL
    d |= 1 << 41; // RW (Daten schreibbar / Code lesbar)
    if code {
        d |= (1 << 43) | (1 << 53); // Executable + L (64-bit)
    }
    d
}

/// GDT + TSS aufbauen und laden (pro Kern; hier einkernig).
pub fn init() {
    // SAFETY: statische Tabellen, ausschließlich hier beim Boot beschrieben; das Laden von
    // GDTR/TR und der Segmentregister ist eine erlaubte Low-Level-Domäne.
    unsafe {
        let gdt = &mut *core::ptr::addr_of_mut!(GDT);
        gdt[0] = 0;
        gdt[1] = seg(true, 0); // 0x08 kernel code
        gdt[2] = seg(false, 0); // 0x10 kernel data
        gdt[3] = seg(false, 3); // 0x18 user data
        gdt[4] = seg(true, 3); //  0x20 user code
        // TSS-Deskriptor (System, 16 Byte).
        let tss_addr = core::ptr::addr_of!(TSS) as u64;
        let limit = (core::mem::size_of::<Tss>() - 1) as u64;
        gdt[5] = limit
            | ((tss_addr & 0xFF_FFFF) << 16)
            | (0x9 << 40) // Typ: 64-bit TSS available
            | (1 << 47) // present
            | (((tss_addr >> 24) & 0xFF) << 56);
        gdt[6] = tss_addr >> 32;

        let ptr = DescriptorTablePointer {
            limit: (core::mem::size_of_val(gdt) - 1) as u16,
            base: gdt.as_ptr() as u64,
        };
        asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
        // CS lässt sich nur per Far-Return neu laden.
        asm!(
            "push 0x08",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            tmp = lateout(reg) _,
            options(preserves_flags)
        );
        asm!(
            "mov ax, 0x10", "mov ds, ax", "mov es, ax", "mov ss, ax",
            "mov ax, 0", "mov fs, ax", "mov gs, ax",
            out("ax") _, options(nostack, preserves_flags)
        );
        asm!("mov ax, 0x28", "ltr ax", out("ax") _, options(nostack, preserves_flags));
    }
}

/// Kernel-Stackzeiger setzen, auf den ein Trap **aus Ring 3** umschaltet.
///
/// Bei jedem Wechsel zu einem Ring-3-Thread zu setzen (dessen eigener Kernel-Stack), sonst
/// liefe der nächste Trap dieses Threads auf dem Stack des vorigen.
pub fn set_kernel_stack(top: u64) {
    // SAFETY: `TSS` ist statisch; nur `rsp0` wird geschrieben (die CPU liest es beim
    // Privilegwechsel).
    unsafe {
        core::ptr::addr_of_mut!(TSS.rsp0).write_volatile(top);
    }
}
