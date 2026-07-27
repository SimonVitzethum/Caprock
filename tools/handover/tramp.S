/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Trampolin fuer die Kern-Uebergabe, Stufe 1b: Real Mode -> Long Mode.
 *
 * Der Code wird als Blob in eine Seite unterhalb 1 MiB kopiert; die SIPI setzt CS = Vektor<<8
 * und IP = 0, das Trampolin liegt also am Segmentanfang. Alle Verweise innerhalb der Seite sind
 * deshalb feste Offsets (s. Layout unten) und brauchen keine Relokation. Genau zwei Werte kennt
 * erst die Laufzeit -- die lineare Adresse des 64-Bit-Stubs und die der Signatur; beide werden
 * vom Modul an markierten Stellen eingetragen.
 *
 * Warum direkt Real Mode -> Long Mode, ohne 32-Bit-Zwischenstufe: `CR0.PE` und `CR0.PG` duerfen
 * in einem einzigen Schreibzugriff gesetzt werden, und der anschliessende Fernsprung in ein
 * 64-Bit-Codesegment schliesst den Uebergang ab. Das ist derselbe Weg, den Linux' eigenes
 * Real-Mode-Trampolin geht, und er hat weniger bewegliche Teile als der Umweg ueber 32 Bit.
 *
 * Seitenlayout (Offsets in der 4-KiB-Seite):
 *   0x000  16-Bit-Code (dieses File)
 *   0x080  64-Bit-Stub
 *   0x0F0  Signatur 1 (u16) -- Real Mode erreicht
 *   0x0F2  Signatur 2 (u16) -- Long Mode erreicht
 *   0x100  GDT (4 Eintraege), vom Modul gebaut
 *   0x120  GDT-Deskriptor fuer `lgdt`
 *   0x130  CR3-Wert (u32)
 */

#define OFF_SIG1  0x0F0
#define OFF_GDTR  0x120
#define OFF_CR3   0x130
/* Selektor des 64-Bit-Codesegments in der vom Modul gebauten GDT. */
#define SEL_CODE64 0x10

	.section .rodata, "a"
	.code16

	.globl sel4lake_t16_start, sel4lake_t16_end, sel4lake_t16_target
sel4lake_t16_start:
	cli

	/* SIPI setzt CS = Vektor<<8, IP = 0 -> DS auf dieselbe Seite legen. */
	mov	%cs, %ax
	mov	%ax, %ds

	/* Signatur 1: der Kern ist angekommen und fuehrt unseren Code aus. */
	mov	$OFF_SIG1, %di
	movw	$0x5345, (%di)

	/* GDT laden (der Deskriptor traegt eine **lineare** Basis, Paging ist noch aus). */
	mov	$OFF_GDTR, %si
	lgdtl	(%si)

	/* CR4.PAE -- Voraussetzung fuer Long Mode. */
	mov	%cr4, %eax
	orl	$(1 << 5), %eax
	mov	%eax, %cr4

	/* CR3 auf die vom Modul gebauten Seitentabellen. */
	mov	$OFF_CR3, %si
	movl	(%si), %eax
	mov	%eax, %cr3

	/* EFER.LME setzen (Long Mode aktivieren, noch nicht aktiv). */
	movl	$0xC0000080, %ecx
	rdmsr
	orl	$(1 << 8), %eax
	wrmsr

	/* PE und PG in einem Zug -- ab hier ist Long Mode aktiv (Compatibility Mode). */
	mov	%cr0, %eax
	orl	$0x80000001, %eax
	mov	%eax, %cr0

	/*
	 * Fernsprung in das 64-Bit-Codesegment. Das Ziel ist eine **lineare** Adresse, die erst
	 * zur Laufzeit feststeht (die Seite, die der Allokator hergegeben hat) -- das Modul
	 * traegt sie in die vier Bytes ab `sel4lake_t16_target` ein.
	 */
	.byte	0x66, 0xea		/* ljmpl ptr16:32 */
sel4lake_t16_target:
	.long	0			/* lineare Adresse des 64-Bit-Stubs (gepatcht) */
	.word	SEL_CODE64
sel4lake_t16_end:

	/* --------------------------------------------------------------------------------- */

	.code64
	.globl sel4lake_t64_start, sel4lake_t64_end, sel4lake_t64_sig
sel4lake_t64_start:
	/*
	 * Absolute Adressierung statt RIP-relativer: der Stub wird verschoben ausgefuehrt, und
	 * eine RIP-relative Referenz zeigte dann auf die Kopie im Modul statt auf die Seite.
	 * Die lineare Adresse der Signatur traegt das Modul ein.
	 */
	movabs	$0, %rax
sel4lake_t64_sig = . - 8
	movw	$0x5346, (%rax)

	/*
	 * Angehalten mit maskierten Interrupts. Der Kern ruehrt ab hier nichts an: kein
	 * Speicherzugriff ausser dieser Seite, keine Interrupts, kein weiterer Tabellenwechsel.
	 * Fuer Linux bleibt er "offline".
	 */
	hlt
	jmp	.
sel4lake_t64_end:
