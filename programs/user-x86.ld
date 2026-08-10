/* Linker-Skript für extern geladene Caprock-Ring-3-Programme auf **x86_64** (A-1.5).
 *
 * Gleiche Bauart wie `user.ld` (aarch64) — nur die feste Link-Adresse ist eine andere, und das
 * ist keine Geschmacksfrage:
 *
 * Auf x86 hat eine isolierte VSpace **ihr eigenes** Page Directory nur für GiB 0; GiB 1..3 teilen
 * sich alle isolierten Adressräume statisch (`ISO_PD_HIGH`, ohne `US`). `vspace_map_page_at`
 * weist deshalb jede VA ab, die nicht in `[16 MiB, 1 GiB)` liegt. Die aarch64-Adresse
 * 0x4100_0000 liegt dort in GiB 1 — auf x86 wäre sie knapp **über** dem Fenster und ließe sich
 * nicht mappen. Also 0x2000_0000 (512 MiB): weit über dem Kernel-Image, weit unter der Grenze,
 * und mit reichlich Abstand zum Stack-Fenster (`LOADED_STACK_VA`, s. kernel/src/system.rs).
 *
 * W^X: Code (R+X), Rodata (R), Daten (R+W) in GETRENNTEN, 4-KiB-ausgerichteten PT_LOAD-Segmenten.
 * Entry = _start. */

ENTRY(_start)

PHDRS {
    text   PT_LOAD FLAGS(5); /* R + X */
    rodata PT_LOAD FLAGS(4); /* R     */
    data   PT_LOAD FLAGS(6); /* R + W */
}

SECTIONS {
    . = 0x20000000;

    .text : ALIGN(4096) {
        *(.text._start)   /* Entry zuerst */
        *(.text .text.*)
    } :text

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(4096) {
        *(.data .data.*)
        *(.got .got.plt)
    } :data

    /* **ALIGN(4096), nicht ALIGN(8)** (2026-08-10) -- und der Grund ist gemessen, nicht
       vorsorglich: `wasmhost` ist das einzige Programm mit einem SCHREIBBAREN Segment und hat
       keine `.data`. lld verwirft die leere `.data`-Ausgabesektion, das RW-PT_LOAD beginnt also
       bei `.bss` -- mit dessen 8-Byte-Ausrichtung, konkret `0x2004_6700`.
       `vspace_map_page_at` weist eine krumme VA beim ALLERERSTEN Aufruf ab (`va % PAGE != 0`, in
       BEIDEN HALs). Das Image war damit auf keiner Architektur ladbar, und der Ladepfad meldete
       den Fehlschlag als „Speicher fuer eine Seitentabelle" -- eine Ursache, die er nicht gemessen
       hatte.
       Kernelseitig die VA abzurunden waere KEINE Behebung, sondern ein W^X-Loch: `.rodata` reicht
       in dieselbe Seite, sie waere erst RO und dann RW gemappt. Die Ausrichtung gehoert dorthin,
       wo das Segment entsteht. Bewacht von `tools/segment-ausrichtung.sh`. */
    .bss : ALIGN(4096) {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame .eh_frame_hdr)
    }
}
