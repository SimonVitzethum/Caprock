//! Boot trampoline for aarch64 / QEMU `virt`.
//!
//! Entered directly by QEMU (`-kernel`) at EL1 with `x0` = DTB pointer. The
//! trampoline:
//!   1. masks all exceptions,
//!   2. parks every secondary core (Phase 0 is single-core),
//!   3. installs the boot stack,
//!   4. zeroes `.bss`,
//!   5. branches to `kernel_main(dtb)`.
//!
//! This is the only assembly in the kernel and lives in a permitted unsafe
//! domain (early boot / CPU bring-up). `x0` is preserved end-to-end so the DTB
//! pointer reaches `kernel_main` as its first argument.

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.boot, "ax"
.globl _start
_start:
    msr     daifset, #0xf             // mask Debug, SError, IRQ, FIQ

    mrs     x9, mpidr_el1             // isolate affinity Aff0..Aff2
    movz    x10, #0xffff
    movk    x10, #0xff, lsl #16
    and     x9, x9, x10
    cbz     x9, 2f                    // affinity 0 -> primary core; else park
1:
    wfe
    b       1b

2:
    mov     x9, #(1 << 20)            // CPACR_EL1.FPEN = 0b01: FP/SIMD nur an EL0
    msr     cpacr_el1, x9             // trappen (Lazy-FP); EL1-Kernel ist soft-float
    isb

    adrp    x9, __boot_stack_top      // install boot stack (SP must be 16-aligned)
    add     x9, x9, #:lo12:__boot_stack_top
    mov     sp, x9

    adrp    x9, __bss_start           // zero .bss: [__bss_start, __bss_end)
    add     x9, x9, #:lo12:__bss_start
    adrp    x10, __bss_end
    add     x10, x10, #:lo12:__bss_end
3:
    cmp     x9, x10
    b.hs    4f
    str     xzr, [x9], #8
    b       3b
4:
    bl      kernel_main               // x0 still holds the DTB pointer
5:
    wfe                               // kernel_main is `-> !`; this is a guard
    b       5b

// Secondary cores enter here via PSCI CPU_ON. PSCI passes the context-id in x0;
// we use it as the stack top for this core (MMU is still off — see
// kernel_secondary_main, which enables it first). DTB is not needed here.
.globl _start_secondary
_start_secondary:
    msr     daifset, #0xf             // mask exceptions during bring-up
    mov     x9, #(1 << 20)            // CPACR_EL1.FPEN = 0b01: FP/SIMD nur an EL0 trappen
    msr     cpacr_el1, x9             // (wie Primärkern; EL1 ist soft-float)
    isb
    mov     sp, x0                    // x0 = context-id = this core's stack top
    bl      kernel_secondary_main
6:
    wfe
    b       6b
"#
);
