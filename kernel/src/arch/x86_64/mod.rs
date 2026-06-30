//! x86_64-Boot-Glue + minimale first-light-HAL (Port-Branch `arch/x86_64`, Stufe 0).
//!
//! Der Kernel wird als **Multiboot1**-Image von QEMU (`-kernel`) geladen und startet im
//! 32-bit-Protected-Mode. Das Trampolin (`_start`, `.code32`) richtet identitätsgemappte
//! Seitentabellen (erste 1 GiB, 2-MiB-Seiten) ein, aktiviert PAE + Long-Mode (EFER.LME) +
//! Paging, lädt eine 64-bit-GDT und springt nach `long_mode` (`.code64`). Dort werden Stack
//! und `.bss` gesetzt und `x86_rust_entry` gerufen.
//!
//! Stufe 0 ist bewusst self-contained (kein `sel4lake-hal`): nur 16550-Serial + Banner. Die volle
//! HAL (Paging-API, IDT, APIC, Timer, Syscall, SMP) folgt in den Stufen 1–5; der Kernel-Kern
//! (Caps/Sched/IPC/…) ist arch-agnostisch und wird schrittweise für x86_64 aktiviert.

use core::arch::{asm, global_asm};

global_asm!(
    r#"
/* Rust-`global_asm!` nutzt Intel-Syntax als Default. */

/* ---- Multiboot1-Header (Magic, Flags=0, Checksum) — muss in den ersten 8 KiB liegen ---- */
.section .multiboot, "a"
.align 8
    .long 0x1BADB002
    .long 0x00000000
    .long 0xE4524FFE

/* ---- 32-bit-Eintritt (Protected Mode, von QEMU-Multiboot) ---- */
.section .boot.text, "ax"
.code32
.globl _start
_start:
    lea esp, [boot_stack_top]            /* temporärer 32-bit-Stack */
    cld

    /* PML4 + PDPT nullen (nur Eintrag [0] wird gesetzt; Rest MUSS 0 sein). Beide sind im
       Linker zusammenhängend; PD wird vollständig gefüllt und braucht keine Nullung. */
    lea edi, [pml4]
    mov ecx, 2048                        /* 2 Tabellen * 4096 B / 4 = 2048 dwords */
    xor eax, eax
    rep stosd

    /* Seitentabellen aufbauen: PML4[0]->PDPT, PDPT[0]->PD, PD[i]= i*2MiB | present|rw|PS */
    lea edi, [pml4]
    lea eax, [pdpt]
    or  eax, 0x3                         /* present | writable */
    mov [edi], eax
    mov dword ptr [edi+4], 0

    lea edi, [pdpt]
    lea eax, [pd]
    or  eax, 0x3
    mov [edi], eax
    mov dword ptr [edi+4], 0

    lea edi, [pd]                        /* 512 Einträge à 2 MiB = 1 GiB identitätsgemappt */
    xor ecx, ecx
1:
    mov eax, ecx
    shl eax, 21                          /* ecx * 2 MiB */
    or  eax, 0x83                        /* present | writable | page-size (2 MiB) */
    mov [edi + ecx*8], eax
    mov dword ptr [edi + ecx*8 + 4], 0
    inc ecx
    cmp ecx, 512
    jb  1b

    /* CR3 = PML4 */
    lea eax, [pml4]
    mov cr3, eax

    /* CR4.PAE = 1 (Bit 5) */
    mov eax, cr4
    or  eax, 1 << 5
    mov cr4, eax

    /* EFER.LME = 1 (MSR 0xC0000080, Bit 8) */
    mov ecx, 0xC0000080
    rdmsr
    or  eax, 1 << 8
    wrmsr

    /* CR0.PG = 1 (Bit 31) + Schutz behalten (PE bereits gesetzt) */
    mov eax, cr0
    or  eax, 1 << 31
    mov cr0, eax

    /* 64-bit-GDT laden, dann per Far-Return (retf) CS=0x08 (L-Bit) laden -> 64-bit. Robuster als
       ein far jump in LLVM-Intel-Syntax. Reihenfolge: erst CS pushen (liegt unten), dann Offset
       (oben); retf poppt Offset->EIP, dann CS. */
    lgdt [gdt64_ptr]
    push 0x08
    lea eax, [long_mode]
    push eax
    retf

.code64
long_mode:
    mov ax, 0x10                         /* Datensegmente = GDT-Eintrag 2 */
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    lea rsp, [boot_stack_top]

    /* .bss nullen [__bss_start, __bss_end) */
    lea rdi, [__bss_start]
    lea rcx, [__bss_end]
    sub rcx, rdi
    xor eax, eax
    cld
    rep stosb

    call x86_rust_entry
2:  hlt
    jmp 2b

/* ---- 64-bit-GDT: null, 64-bit-Code (L), Data ---- */
.section .rodata
.align 8
gdt64:
    .quad 0x0000000000000000
    .quad 0x00209A0000000000             /* Code: P,DPL0,S,Exec,Long */
    .quad 0x0000920000000000             /* Data: P,DPL0,S,Write */
gdt64_ptr:
    .word gdt64_ptr - gdt64 - 1
    .quad gdt64

/* ---- Boot-Seitentabellen + Stack (NOLOAD, vor Paging beschrieben) ---- */
.section .boot.bss, "aw", @nobits
.align 4096
pml4: .skip 4096
pdpt: .skip 4096
pd:   .skip 4096
.align 16
boot_stack:     .skip 0x4000
boot_stack_top:
"#
);

// --- 16550-UART (COM1 @ 0x3F8), reine Port-I/O. ---
const COM1: u16 = 0x3F8;

#[inline]
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: x86-Port-I/O auf ein festes Geräteregister; kein Speicherzugriff.
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: wie `outb`.
    asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}

fn serial_init() {
    // SAFETY: Standard-16550-Init-Sequenz auf COM1 (Geräteregister).
    unsafe {
        outb(COM1 + 1, 0x00); // Interrupts aus
        outb(COM1 + 3, 0x80); // DLAB an
        outb(COM1 + 0, 0x01); // Teiler lo (115200 Baud)
        outb(COM1 + 1, 0x00); // Teiler hi
        outb(COM1 + 3, 0x03); // 8N1, DLAB aus
        outb(COM1 + 2, 0xC7); // FIFO an + leeren, 14-Byte-Schwelle
        outb(COM1 + 4, 0x0B); // RTS/DSR/OUT2
    }
}

fn serial_putc(b: u8) {
    // SAFETY: pollt LSR (THRE, Bit 5), dann THR schreiben.
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        outb(COM1, b);
    }
}

/// Lock-/atomic-freie Rohausgabe (für first-light + Panic).
pub fn emit_raw(s: &str) {
    for &b in s.as_bytes() {
        if b == b'\n' {
            serial_putc(b'\r');
        }
        serial_putc(b);
    }
}

struct SerialWriter;
impl core::fmt::Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        emit_raw(s);
        Ok(())
    }
}

/// Formatierte Rohausgabe (für den Panic-Handler).
pub fn emit_fmt(args: core::fmt::Arguments) {
    use core::fmt::Write;
    let _ = SerialWriter.write_fmt(args);
}

/// 64-bit-Rust-Eintritt (aus dem Boot-Trampolin). Stufe 0: Banner + Halt.
#[no_mangle]
pub extern "C" fn x86_rust_entry() -> ! {
    serial_init();
    emit_raw("\n");
    emit_raw("========================================\n");
    emit_raw(" SEL4Lake -- x86_64 first light (COM1)\n");
    emit_raw(" Multiboot -> Long Mode (PAE, 1 GiB identity) OK.\n");
    emit_raw(" Branch arch/x86_64, Stufe 0: Boot + Serial.\n");
    emit_raw(" Paging/IDT/APIC/Timer/Syscall/SMP folgen (Stufe 1-5).\n");
    emit_raw("========================================\n");
    emit_raw("x86_64 first light: ALL PASS\n");
    halt();
}

/// CPU anhalten (Panic/Ende). `hlt` in Schleife.
pub fn halt() -> ! {
    loop {
        // SAFETY: `hlt` hält bis zum nächsten Interrupt; in der Schleife = dauerhaftes Parken.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) }
    }
}
