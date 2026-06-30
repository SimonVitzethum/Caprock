//! x86_64 Kontextwechsel zwischen Kernel-Kontexten (Branch arch/x86_64, Stufe 3b).
//!
//! Minimaler kooperativer Switch: `context_switch(old, new)` sichert die **callee-saved** Register
//! (System-V: rbx, rbp, r12–r15) + RSP in `*old`, lädt sie aus `*new` und `ret`et in den neuen
//! Kontext (RIP liegt als Rücksprungadresse auf dem neuen Stack). Ein frischer Kontext bekommt seine
//! Einsprung-Adresse als „Rücksprung" auf den Stack gelegt (`context_init`). Pendant zum aarch64-
//! Kontextwechsel des Schedulers (dort x19–x30/sp); hier x86-Registerlayout.

use super::{emit_raw, put_dec};
use core::arch::global_asm;
use core::ptr::addr_of_mut;

/// Gesicherter Kernel-Kontext (Reihenfolge == Offsets im Assembler unten).
#[repr(C)]
struct Context {
    rbx: u64, // +0
    rbp: u64, // +8
    r12: u64, // +16
    r13: u64, // +24
    r14: u64, // +32
    r15: u64, // +40
    rsp: u64, // +48
}
impl Context {
    const fn zero() -> Self {
        Context { rbx: 0, rbp: 0, r12: 0, r13: 0, r14: 0, r15: 0, rsp: 0 }
    }
}

global_asm!(
    r#"
.section .text
.globl context_switch
context_switch:                  /* rdi = *mut old, rsi = *const new */
    mov [rdi + 0],  rbx
    mov [rdi + 8],  rbp
    mov [rdi + 16], r12
    mov [rdi + 24], r13
    mov [rdi + 32], r14
    mov [rdi + 40], r15
    mov [rdi + 48], rsp
    mov rbx, [rsi + 0]
    mov rbp, [rsi + 8]
    mov r12, [rsi + 16]
    mov r13, [rsi + 24]
    mov r14, [rsi + 32]
    mov r15, [rsi + 40]
    mov rsp, [rsi + 48]
    ret                          /* poppt RIP des neuen Kontexts (Einsprung bzw. yield-Rückkehr) */
"#
);

extern "C" {
    fn context_switch(old: *mut Context, new: *const Context);
}

const STACK_WORDS: usize = 1024; // 8 KiB Kernel-Stack für Thread B

static mut CTX_MAIN: Context = Context::zero();
static mut CTX_B: Context = Context::zero();
static mut B_STACK: [u64; STACK_WORDS] = [0; STACK_WORDS];
static mut B_RUNS: u64 = 0;

/// Thread B: läuft kooperativ, yieldet jedes Mal zurück zu A (Kontext A = `CTX_MAIN`).
extern "C" fn thread_b() -> ! {
    loop {
        // SAFETY: kooperativ, kein Preempt (IF=0 im Demo); B ist alleiniger Schreiber von B_RUNS.
        unsafe {
            *addr_of_mut!(B_RUNS) += 1;
            emit_raw("ctxsw   : -> Thread B laeuft, yield zurueck zu A\n");
            context_switch(addr_of_mut!(CTX_B), addr_of_mut!(CTX_MAIN));
        }
    }
}

/// Demo: 3× kooperativer Wechsel A <-> B. Beweist Sichern/Wiederherstellen + Stack-Wechsel.
pub fn demo() {
    // SAFETY: einmaliger Aufbau + kooperative Wechsel (Primärkern, IF=0). Rohzeiger gegen static_mut_refs.
    unsafe {
        let stack_top = addr_of_mut!(B_STACK) as u64 + (STACK_WORDS * 8) as u64;
        let rsp16 = (stack_top - 8) & !0xF; // 16-ausgerichtet -> nach `ret` ist RSP%16==8 (System V)
        *(rsp16 as *mut u64) = thread_b as u64; // Einsprungadresse als erste „Rücksprung"-Adresse
        (*addr_of_mut!(CTX_B)).rsp = rsp16;

        for _ in 0..3 {
            emit_raw("ctxsw   : Thread A (main), yield -> B\n");
            context_switch(addr_of_mut!(CTX_MAIN), addr_of_mut!(CTX_B));
        }
        emit_raw("ctxsw   : 3x A<->B Kontextwechsel ok (B lief ");
        put_dec(*addr_of_mut!(B_RUNS));
        emit_raw("x) -> ALL PASS\n");
    }
}
