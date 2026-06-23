/* SEL4Lake kernel link script — QEMU `virt` (aarch64).
 *
 * QEMU loads an ELF passed via `-kernel` according to its program headers and
 * jumps to the ELF entry point (`_start`). RAM on the `virt` machine starts at
 * 0x4000_0000; QEMU's conventional kernel load offset is 0x4008_0000, which we
 * use as the kernel base. There is no virtual memory in SEL4Lake — these are
 * real physical addresses (see docs/adr/0002-no-virtual-memory-sas.md).
 */

ENTRY(_start)

PHDRS {
    text   PT_LOAD FLAGS(5); /* R-X */
    rodata PT_LOAD FLAGS(4); /* R-- */
    data   PT_LOAD FLAGS(6); /* RW- */
}

SECTIONS {
    . = 0x40080000;
    __kernel_start = .;

    /* Section boundaries are 4 KiB-aligned so the MMU can enforce W^X per page
     * (.text = R-X, .rodata = R--, everything else = RW + XN). See
     * docs/adr/0002-no-virtual-memory-sas.md and sel4lake-hal::mmu. */
    .text : ALIGN(4096) {
        __text_start = .;
        KEEP(*(.text.boot))   /* _start must lead the image */
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    } :text

    .rodata : ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    } :rodata

    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.got .got.plt)
    } :data

    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;

        /* Boot stack for the primary core (grows down from the top). Placed
         * after .bss so it is not touched by the BSS-zeroing loop. */
        . = ALIGN(16);
        __boot_stack_bottom = .;
        . += 0x10000; /* 64 KiB */
        __boot_stack_top = .;

        /* Secondary-core stacks: one 64 KiB slot per core index (0 unused).
         * Core `c` uses [__sec_stacks_bottom + c*64KiB, +64KiB); top is passed
         * to the core via the PSCI context-id argument. */
        . = ALIGN(16);
        __sec_stacks_bottom = .;
        . += 0x10000 * 8;
        __sec_stacks_top = .;
    } :data

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame .eh_frame_hdr)
        *(.interp)
    }
}
