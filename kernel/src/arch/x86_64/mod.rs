//! x86_64-Boot-Glue + minimale first-light-HAL (Port-Branch `arch/x86_64`, Stufe 0).
//!
//! Der Kernel wird als **Multiboot1**-Image von QEMU (`-kernel`) geladen und startet im
//! 32-bit-Protected-Mode. Das Trampolin (`_start`, `.code32`) richtet identitätsgemappte
//! Seitentabellen (erste 1 GiB, 2-MiB-Seiten) ein, aktiviert PAE + Long-Mode (EFER.LME) +
//! Paging, lädt eine 64-bit-GDT und springt nach `long_mode` (`.code64`). Dort werden Stack
//! und `.bss` gesetzt und `x86_rust_entry` gerufen.
//!
//! Stufe 0 ist bewusst self-contained (kein `caprock-hal`): nur 16550-Serial + Banner. Die volle
//! HAL (Paging-API, IDT, APIC, Timer, Syscall, SMP) folgt in den Stufen 1–5; der Kernel-Kern
//! (Caps/Sched/IPC/…) ist arch-agnostisch und wird schrittweise für x86_64 aktiviert.

use core::arch::{asm, global_asm};

// ext-31: Paging/IDT/LAPIC/GDT/Syscall/Context sind in die HAL gewandert
// (`caprock-hal::x86_64`) — dort stehen sie hinter derselben API wie ihre ARM-Pendants,
// sodass der Kernel-Kern sie ohne `cfg` benutzt. Hier bleibt nur das Boot-Trampolin.
mod bootinfo;
mod bringup;
#[cfg(feature = "selftest")]
mod dmar_selftest;
mod multiboot;

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
    mov [mb_info_ptr], ebx               /* Multiboot-Info-Zeiger retten (Speicherplan!) */

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

    mov edi, [mb_info_ptr]               /* 1. Argument (SysV): Multiboot-Info-Zeiger */
    call x86_rust_entry
2:  hlt
    jmp 2b

/* ---- 64-bit-GDT: null, 64-bit-Code (L), Data ----
   BESCHREIBBAR (`.data`), nicht `.rodata`, und das ist kein Stilfrage:

   Die CPU schreibt beim Laden eines Segmentregisters das Accessed-Bit IN den Deskriptor.
   `retf` mit CS=0x08 und die `mov ds/es/ss/fs/gs` unten machen aus 0x9A also 0x9B und aus
   0x92 ein 0x93 -- ein Hardware-Schreibzugriff auf diese acht Bytes.

   Solange das vor `mmu::init_primary` passiert (Boot-Tabellen: alles RW, CR0.WP aus), faellt
   das nicht auf. Wuerde die GDT spaeter noch einmal geladen, traefe derselbe Schreibzugriff
   eine Ro-Seite mit CR0.WP=1 -> #PF im Boot-Pfad, an einer Stelle, an der niemand einen
   Schreibzugriff vermutet.

   Zweitens -- und deshalb ist es hier aufgefallen -- bricht es die Zusage, auf der A-1.3
   beruht: `[__text_start, __rodata_end)` soll zur Laufzeit unveraenderlich sein, damit der
   Kernel seinen eigenen Code-Hash reproduzieren kann. Ein Accessed-Bit mitten in `.rodata`
   machte den Hash lauffremd, und das Manifest liess sich an keinen Kernel binden. */
.section .data.gdt64, "aw"
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
mb_info_ptr: .skip 8
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
pub(crate) unsafe fn outb(port: u16, val: u8) {
    // SAFETY: x86-Port-I/O auf ein festes Geräteregister; kein Speicherzugriff.
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}
#[inline]
#[allow(dead_code)]
pub(crate) unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: wie `outb`.
    asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}
#[inline]
pub(crate) unsafe fn outw(port: u16, val: u16) {
    // SAFETY: 16-bit-Port-I/O auf ein festes Geräteregister.
    asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags));
}

/// Maschine herunterfahren (Pendant zu `hal::psci::system_off`). QEMU beendet sich über das
/// ACPI-PM1a_CNT-Register (S5). Zwei bekannte QEMU-Ports (q35/pc + älteres piix), dann hlt-Fallback.
pub fn system_off() -> ! {
    // SAFETY: ACPI-Shutdown-Schreibzugriffe (QEMU); ohne Effekt auf echter HW -> Fallback halt().
    unsafe {
        outw(0x604, 0x2000);
        outw(0xB004, 0x2000);
    }
    halt();
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

/// Eine u64 als `0x…`-Hex ausgeben (16 Stellen, lock-frei). Für Exception-/Paging-Dumps.
pub(crate) fn put_hex(mut n: u64) {
    emit_raw("0x");
    let mut buf = [0u8; 16];
    for i in (0..16).rev() {
        let nib = (n & 0xf) as u8;
        buf[i] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        n >>= 4;
    }
    for &c in &buf {
        serial_putc(c);
    }
}

/// Eine u64 dezimal ausgeben (lock-frei).
pub(crate) fn put_dec(mut n: u64) {
    if n == 0 {
        serial_putc(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        serial_putc(buf[i]);
    }
}

/// 64-bit-Rust-Eintritt (aus dem Boot-Trampolin).
///
/// Bis ext-30 lief hier eine Kette von Hardware-Demos (Stufe 0-3). Seit ext-31 übernimmt der
/// **echte Kernel-Kern** (`bringup::run`): dieselben Selbsttests, derselbe Scheduler und
/// dasselbe cap-gesicherte IPC wie auf aarch64. Die alten Demos sind damit erfüllt und
/// entfallen — ihre Aussagen (Paging/W^X, IDT, LAPIC-Timer) prüft der Bring-up implizit,
/// weil ohne sie nichts davon liefe.
#[no_mangle]
pub extern "C" fn x86_rust_entry(multiboot_info: u64) -> ! {
    bringup::run(multiboot_info)
}

/// CPU anhalten (Panic/Ende). `hlt` in Schleife.
pub fn halt() -> ! {
    loop {
        // SAFETY: `hlt` hält bis zum nächsten Interrupt; in der Schleife = dauerhaftes Parken.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) }
    }
}
