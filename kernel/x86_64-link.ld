/* SEL4Lake-Kernel-Linker — x86_64 (Multiboot1, QEMU `-kernel`).
 *
 * Geladen bei 1 MiB (physisch = virtuell, identitätsgemappt). Der Multiboot-Header MUSS in den
 * ersten 8 KiB der Datei liegen -> `.multiboot` zuerst. `.boot.bss` (Boot-Seitentabellen + -Stack)
 * liegt BEWUSST außerhalb von [__bss_start, __bss_end): das Trampolin nutzt CR3 (-> pml4) und den
 * Stack noch, wenn der Long-Mode-Code `.bss` nullt — würde die Nullung sie erfassen, verschwände
 * die aktive Identity-Map / der Stack. PML4+PDPT werden stattdessen im 32-bit-Trampolin genullt.
 */

ENTRY(_start)

SECTIONS {
    . = 1M;
    __kernel_start = .;

    .boot : ALIGN(4096) {
        KEEP(*(.multiboot))   /* Multiboot-Header zuerst (in den ersten 8 KiB) */
        *(.boot.text)         /* 32-bit-Trampolin + 64-bit long_mode */
    }

    .text : ALIGN(4096) {
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.got .got.plt)
    }

    /* Genullte Kernel-BSS (NUR diese Spanne wird vom Long-Mode-Code genullt). */
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* Boot-Seitentabellen + Boot-Stack: NICHT genullt (s. o.), nach __bss_end. */
    .boot_bss (NOLOAD) : ALIGN(4096) {
        *(.boot.bss)
    }

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame .eh_frame_hdr)
        *(.ARM.exidx*)
    }
}
