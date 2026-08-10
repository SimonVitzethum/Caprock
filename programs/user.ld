/* Linker-Skript für extern geladene Caprock-EL0-Programme (ext-26).
 *
 * ET_EXEC, an einer KANONISCHEN festen VA gelinkt (0x4100_0000, in GiB 1). Der Loader lädt das
 * Programm an BELIEBIGE allokierte Physadressen und mappt seine PT_LOAD-Segmente per
 * vspace_map_page_at(va -> pa) in die isolierte VSpace (VA != PA, nur diese private VSpace).
 *
 * W^X: Code (R+X), Rodata (R), Daten (R+W) liegen in GETRENNTEN, 4-KiB-ausgerichteten PT_LOAD-
 * Segmenten, damit der Loader sie mit unterschiedlichen Rechten mappen kann. Entry = _start. */

ENTRY(_start)

PHDRS {
    text   PT_LOAD FLAGS(5); /* R + X */
    rodata PT_LOAD FLAGS(4); /* R     */
    data   PT_LOAD FLAGS(6); /* R + W */
}

SECTIONS {
    . = 0x41000000;

    .text : ALIGN(4096) {
        *(.text._start)   /* Entry zuerst */
        *(.text .text.*)
    } :text

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(4096) {
        *(.data .data.*)
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
        *(.ARM.exidx .ARM.exidx.*)
    }
}
