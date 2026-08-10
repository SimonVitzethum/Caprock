/* Caprock-Kernel-Linker — x86_64 (Multiboot1, QEMU `-kernel`).
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

    /* 4-KiB-ausgerichtete Sektionsgrenzen -> die Rust-MMU (paging.rs) kann W^X pro Seite setzen
       (.text = R-X, .rodata = R--, .data/.bss = RW + NX). */
    .text : ALIGN(4096) {
        __text_start = .;
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    }

    .rodata : ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    }

    /* Ring-3-Code (EL0-äquivalent): eigene Seiten, von paging.rs als User-(U/S=1)+ausführbar
       gemappt. Getrennt vom Kernel-.text (supervisor-only). */
    .user_text : ALIGN(4096) {
        __user_text_start = .;
        *(.user_text .user_text.*)
        . = ALIGN(4096);
        __user_text_end = .;
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.got .got.plt)
    }

    /* Ring-3-Daten/-Stack: User-(U/S=1)+RW+NX. */
    .user_data : ALIGN(4096) {
        __user_data_start = .;
        *(.user_data .user_data.*)
        . = ALIGN(4096);
        __user_data_end = .;
    }

    /* AP-Trampolin (ext-31): Ein Application Processor startet nach INIT-SIPI-SIPI im
       16-bit-Real-Mode an `Vektor << 12` — also UNTERHALB von 1 MiB.
       LAUF-Adresse (VMA) = 0x8000, damit alle Labels darin absolute Laufadressen sind
       (Sprungziele, GDT-Zeiger, Parameterblock) — der 16-bit-Teil rechnet segmentrelativ.
       LADE-Adresse (LMA) = hier im Image, denn ein eigenes Ladesegment bei 0x8000 wuerde die
       Segmente nach Adresse sortieren und damit den Multiboot-Header hinter Dateioffset 8192
       schieben — der muss aber in den ersten 8 KiB der DATEI liegen, sonst bootet QEMU
       kommentarlos gar nicht. Der BSP kopiert den Block einmalig von LMA nach VMA. */
    __aptramp_lma = .;
    .aptramp 0x8000 : AT(__aptramp_lma) {
        KEEP(*(.aptramp))
    }
    __aptramp_data_lma = __aptramp_lma + SIZEOF(.aptramp);
    /* Parameter auf einer EIGENEN Seite: die Codeseite bleibt R-X, diese ist RW+NX (W^X). */
    .aptramp_data 0x9000 : AT(__aptramp_data_lma) {
        KEEP(*(.aptramp.data))
    }
    . = __aptramp_data_lma + SIZEOF(.aptramp_data);

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
