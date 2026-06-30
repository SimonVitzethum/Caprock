//! x86_64 Interrupt Descriptor Table + Exception-Dispatch (Branch arch/x86_64, Stufe 2).
//!
//! Eine 256-Eintrag-IDT; die ersten 32 Vektoren (CPU-Exceptions) zeigen auf Assembler-Stubs, die
//! einen einheitlichen Frame aufbauen (Vektor + Error-Code normalisiert, dann alle GPRs) und
//! [`x86_isr_handler`] rufen. Benigne Traps (Breakpoint #3) kehren via `iretq` zurück; fatale
//! Faults werden gedumpt und halten an. Pendant zu `sel4lake-hal::exception` (aarch64-VBAR).

use super::{emit_raw, halt, put_hex};
use core::arch::global_asm;
use core::ptr::addr_of;

global_asm!(
    r#"
/* Gemeinsamer Stub-Rumpf: Error-Code (echt od. 0) + Vektor liegen bereits auf dem Stack.
   Wir sichern alle GPRs, übergeben rsp (Frame-Zeiger) in rdi und rufen den Rust-Handler. */
.macro isr_noerr vec
isr_stub_\vec:
    push 0
    push \vec
    jmp isr_common
.endm
.macro isr_err vec
isr_stub_\vec:
    push \vec
    jmp isr_common
.endm

isr_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp                 /* Zeiger auf den IsrFrame */
    cld
    call x86_isr_handler
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16                  /* Vektor + Error-Code verwerfen */
    iretq

/* CPU-Exceptions 0..31. Error-Code-Vektoren (8,10-14,17,21,29,30) nutzen isr_err. */
isr_noerr 0
isr_noerr 1
isr_noerr 2
isr_noerr 3
isr_noerr 4
isr_noerr 5
isr_noerr 6
isr_noerr 7
isr_err   8
isr_noerr 9
isr_err   10
isr_err   11
isr_err   12
isr_err   13
isr_err   14
isr_noerr 15
isr_noerr 16
isr_err   17
isr_noerr 18
isr_noerr 19
isr_noerr 20
isr_err   21
isr_noerr 22
isr_noerr 23
isr_noerr 24
isr_noerr 25
isr_noerr 26
isr_noerr 27
isr_noerr 28
isr_err   29
isr_err   30
isr_noerr 31

/* Tabelle der Stub-Adressen (für das Füllen der IDT in Rust). */
.section .rodata
.globl isr_stub_table
isr_stub_table:
    .quad isr_stub_0,  isr_stub_1,  isr_stub_2,  isr_stub_3
    .quad isr_stub_4,  isr_stub_5,  isr_stub_6,  isr_stub_7
    .quad isr_stub_8,  isr_stub_9,  isr_stub_10, isr_stub_11
    .quad isr_stub_12, isr_stub_13, isr_stub_14, isr_stub_15
    .quad isr_stub_16, isr_stub_17, isr_stub_18, isr_stub_19
    .quad isr_stub_20, isr_stub_21, isr_stub_22, isr_stub_23
    .quad isr_stub_24, isr_stub_25, isr_stub_26, isr_stub_27
    .quad isr_stub_28, isr_stub_29, isr_stub_30, isr_stub_31
.text
"#
);

/// Von den Assembler-Stubs aufgebauter Frame (Reihenfolge = Push-Reihenfolge, niedrigste Adresse
/// zuerst). `#[repr(C)]`, damit das Layout exakt zum Stub passt.
#[repr(C)]
struct IsrFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// Mnemonik für die ersten CPU-Exception-Vektoren (Diagnose).
fn vec_name(v: u64) -> &'static str {
    match v {
        0 => "#DE divide-error",
        3 => "#BP breakpoint",
        6 => "#UD invalid-opcode",
        8 => "#DF double-fault",
        13 => "#GP general-protection",
        14 => "#PF page-fault",
        _ => "exception",
    }
}

#[no_mangle]
extern "C" fn x86_isr_handler(frame: &IsrFrame) {
    emit_raw("\n[x86 EXCEPTION] vector=");
    super::put_dec(frame.vector);
    emit_raw(" (");
    emit_raw(vec_name(frame.vector));
    emit_raw(") rip=");
    put_hex(frame.rip);
    emit_raw(" err=");
    put_hex(frame.error_code);
    if frame.vector == 14 {
        // #PF: CR2 = fehlerhafte Adresse.
        let cr2: u64;
        // SAFETY: nur Lesen von CR2 (Fault-Adresse).
        unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) }
        emit_raw(" cr2=");
        put_hex(cr2);
    }
    emit_raw("\n");
    if frame.vector == 3 {
        return; // Breakpoint: benigne -> iretq setzt fort
    }
    emit_raw("[x86] fataler Fault -> halt\n");
    halt();
}

// --- IDT-Aufbau ---

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    off_lo: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    off_mid: u16,
    off_hi: u32,
    zero: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        IdtEntry { off_lo: 0, selector: 0, ist: 0, type_attr: 0, off_mid: 0, off_hi: 0, zero: 0 }
    }
    fn set_handler(&mut self, handler: u64) {
        self.off_lo = handler as u16;
        self.off_mid = (handler >> 16) as u16;
        self.off_hi = (handler >> 32) as u32;
        self.selector = 0x08; // Kernel-Code-Segment (GDT-Eintrag 1, s. Boot-GDT)
        self.ist = 0;
        self.type_attr = 0x8E; // present, DPL0, 64-bit-Interrupt-Gate
        self.zero = 0;
    }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

extern "C" {
    static isr_stub_table: [u64; 32];
}

/// IDT mit den 32 Exception-Stubs füllen und per `lidt` laden.
pub fn init() {
    // SAFETY: einmaliger, alleiniger Aufbau der statischen IDT vor jeder Interrupt-Freigabe
    // (Primärkern, single-threaded). Zugriff über Rohzeiger vermeidet `static_mut_refs`.
    unsafe {
        let idt = addr_of!(IDT) as *mut IdtEntry;
        let stubs = addr_of!(isr_stub_table) as *const u64;
        for i in 0..32 {
            (*idt.add(i)).set_handler(*stubs.add(i));
        }
        let idtr = Idtr {
            limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: idt as u64,
        };
        core::arch::asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
    }
}

/// Einen Software-Breakpoint auslösen (Vektor 3) — Selbsttest des Exception-Dispatch.
pub fn test_breakpoint() {
    // SAFETY: `int3` löst den (installierten) Breakpoint-Handler aus; dieser kehrt zurück.
    unsafe { core::arch::asm!("int3", options(nomem, nostack, preserves_flags)) }
}
