//! Exception-Vektoren und Trap-Dispatch (aarch64, EL1).
//!
//! Eine 2-KiB-ausgerichtete Vektortabelle mit 16 Einträgen (4 Quellen × 4
//! Typen). Jeder Eintrag sichert den vollen GP-Registerkontext in einen
//! [`TrapFrame`] auf dem Stack, ruft [`handle_exception`] und stellt den Kontext
//! per `eret` wieder her. Assembler ist eine erlaubte `unsafe`-Domäne.

use crate::cpu;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Auf dem Stack gesicherter Registerkontext einer Exception.
///
/// Das Layout entspricht exakt der Speichersequenz im Vektor-Assembler.
/// Der FP/SIMD-Zustand wird **eager** gesichert (volle q0..q31), damit Threads
/// Fließkomma/SIMD über Preemption hinweg nutzen können. (Lazy-FP — Sichern erst
/// bei Bedarf — wäre eine spätere Optimierung; ADR 0005.)
#[repr(C, align(16))]
pub struct TrapFrame {
    /// x0..x30 (x30 = Link Register).  Offset 0..248
    pub gpr: [u64; 31],
    /// `ELR_EL1` — Rücksprungadresse (unterbrochener PC).  Offset 248
    pub elr: u64,
    /// `SPSR_EL1` — gesicherter Prozessorstatus.  Offset 256
    pub spsr: u64,
    _pad: u64, // 264 -> 272
    /// q0..q31 (FP/SIMD).  Offset 272..784
    pub fpregs: [u128; 32],
    /// `FPSR`.  Offset 784
    pub fpsr: u64,
    /// `FPCR`.  Offset 792 -> 800
    pub fpcr: u64,
}

global_asm!(
    r#"
.macro VENTRY id
.balign 0x80
    sub     sp, sp, #800
    stp     x0,  x1,  [sp, #0]
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    mrs     x9,  ELR_EL1
    mrs     x10, SPSR_EL1
    stp     x30, x9,  [sp, #240]
    str     x10, [sp, #256]
    mov     x1,  #\id
    b       __trap_dispatch
.endm

.section .text
.balign 0x800
.globl __exception_vectors
__exception_vectors:
    VENTRY 0    // Current EL, SP0:  Sync / IRQ / FIQ / SError
    VENTRY 1
    VENTRY 2
    VENTRY 3
    VENTRY 4    // Current EL, SPx:  Sync / IRQ / FIQ / SError  (Kernel läuft hier)
    VENTRY 5
    VENTRY 6
    VENTRY 7
    VENTRY 8    // Lower EL, aarch64
    VENTRY 9
    VENTRY 10
    VENTRY 11
    VENTRY 12   // Lower EL, aarch32
    VENTRY 13
    VENTRY 14
    VENTRY 15

__trap_dispatch:
    // FP/SIMD-Kontext sichern (q0..q31 @272, FPSR/FPCR @784). x1 (vector id)
    // und x0 bleiben unberührt; x9/x10 sind bereits im GP-Bereich gesichert.
    stp     q0,  q1,  [sp, #272]
    stp     q2,  q3,  [sp, #304]
    stp     q4,  q5,  [sp, #336]
    stp     q6,  q7,  [sp, #368]
    stp     q8,  q9,  [sp, #400]
    stp     q10, q11, [sp, #432]
    stp     q12, q13, [sp, #464]
    stp     q14, q15, [sp, #496]
    stp     q16, q17, [sp, #528]
    stp     q18, q19, [sp, #560]
    stp     q20, q21, [sp, #592]
    stp     q22, q23, [sp, #624]
    stp     q24, q25, [sp, #656]
    stp     q26, q27, [sp, #688]
    stp     q28, q29, [sp, #720]
    stp     q30, q31, [sp, #752]
    mrs     x9,  FPSR
    mrs     x10, FPCR
    add     x11, sp, #784         // GP-stp-Offset reicht nur bis 504 -> Adresse bilden
    stp     x9,  x10, [x11]

    mov     x0, sp                // x0 = &TrapFrame ; x1 = vector id
    bl      handle_exception
    mov     sp, x0                // x0 = wiederherzustellender Frame (ggf. anderer Thread)

    // FP/SIMD-Kontext wiederherstellen (vor dem GP-Restore, der x9/x10/x11 setzt).
    add     x11, sp, #784
    ldp     x9,  x10, [x11]
    msr     FPSR, x9
    msr     FPCR, x10
    ldp     q0,  q1,  [sp, #272]
    ldp     q2,  q3,  [sp, #304]
    ldp     q4,  q5,  [sp, #336]
    ldp     q6,  q7,  [sp, #368]
    ldp     q8,  q9,  [sp, #400]
    ldp     q10, q11, [sp, #432]
    ldp     q12, q13, [sp, #464]
    ldp     q14, q15, [sp, #496]
    ldp     q16, q17, [sp, #528]
    ldp     q18, q19, [sp, #560]
    ldp     q20, q21, [sp, #592]
    ldp     q22, q23, [sp, #624]
    ldp     q24, q25, [sp, #656]
    ldp     q26, q27, [sp, #688]
    ldp     q28, q29, [sp, #720]
    ldp     q30, q31, [sp, #752]

    // GP-Kontext wiederherstellen.
    ldr     x10, [sp, #256]
    msr     SPSR_EL1, x10
    ldp     x30, x9,  [sp, #240]
    msr     ELR_EL1, x9
    ldp     x28, x29, [sp, #224]
    ldp     x26, x27, [sp, #208]
    ldp     x24, x25, [sp, #192]
    ldp     x22, x23, [sp, #176]
    ldp     x20, x21, [sp, #160]
    ldp     x18, x19, [sp, #144]
    ldp     x16, x17, [sp, #128]
    ldp     x14, x15, [sp, #112]
    ldp     x12, x13, [sp, #96]
    ldp     x10, x11, [sp, #80]
    ldp     x8,  x9,  [sp, #64]
    ldp     x6,  x7,  [sp, #48]
    ldp     x4,  x5,  [sp, #32]
    ldp     x2,  x3,  [sp, #16]
    ldp     x0,  x1,  [sp, #0]
    add     sp, sp, #800
    eret
"#
);

/// `VBAR_EL1` auf die Vektortabelle setzen. Pro Kern aufzurufen.
pub fn init() {
    extern "C" {
        static __exception_vectors: u8;
    }
    // SAFETY: Setzen von `VBAR_EL1` auf die statische, 2-KiB-ausgerichtete
    // Vektortabelle des Kernels. Interrupt-/Exception-Init ist eine erlaubte
    // `unsafe`-Domäne.
    unsafe {
        let vbar = core::ptr::addr_of!(__exception_vectors) as u64;
        asm!("msr VBAR_EL1, {}", in(reg) vbar, options(nomem, nostack, preserves_flags));
    }
    cpu::isb();
}

/// True, wenn `kind` einen IRQ-Eintrag bezeichnet (Typ 1 in jeder 4er-Gruppe).
fn is_irq(kind: u64) -> bool {
    kind % 4 == 1
}

/// Optionaler Reschedule-Hook (vom Scheduler registriert). Als `usize`
/// (Funktionszeiger) gespeichert; `0` = nicht gesetzt. Wird einmalig beim Boot
/// gesetzt und danach nur gelesen.
static RESCHED_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Signatur des Reschedule-Hooks: bekommt den aktuellen TrapFrame, liefert den
/// wiederherzustellenden (ggf. den eines anderen Threads).
pub type RescheduleHook = fn(*mut TrapFrame) -> *mut TrapFrame;

/// Reschedule-Hook registrieren (vor dem Aktivieren von IRQs aufzurufen).
pub fn set_reschedule_hook(hook: RescheduleHook) {
    RESCHED_HOOK.store(hook as usize, Ordering::Release);
}

fn reschedule(frame: *mut TrapFrame) -> *mut TrapFrame {
    let h = RESCHED_HOOK.load(Ordering::Acquire);
    if h == 0 {
        return frame;
    }
    // SAFETY: `h` wurde ausschließlich über `set_reschedule_hook` mit einem
    // gültigen Funktionszeiger genau dieses Typs gesetzt. Funktionszeiger und
    // `usize` sind auf aarch64 größengleich. (Trap-Dispatch-Plumbing, erlaubte
    // Low-Level-Domäne.)
    let hook: RescheduleHook = unsafe { core::mem::transmute(h) };
    hook(frame)
}

/// Optionaler Syscall-Hook (vom IPC-System registriert). Wie der Reschedule-Hook
/// als `usize` gespeichert; `0` = nicht gesetzt.
static SYSCALL_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Signatur des Syscall-Hooks (Argumente/Rückgabe im TrapFrame).
pub type SyscallHook = fn(*mut TrapFrame) -> *mut TrapFrame;

/// Syscall-Hook registrieren (vor dem Aktivieren von IRQs/Threads aufzurufen).
pub fn set_syscall_hook(hook: SyscallHook) {
    SYSCALL_HOOK.store(hook as usize, Ordering::Release);
}

fn syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    let h = SYSCALL_HOOK.load(Ordering::Acquire);
    if h == 0 {
        return frame;
    }
    // SAFETY: wie `reschedule` — nur über `set_syscall_hook` gesetzt.
    let hook: SyscallHook = unsafe { core::mem::transmute(h) };
    hook(frame)
}

/// Register `xidx` eines (gesicherten) TrapFrames lesen. Für den
/// Nachrichtentransfer zwischen Threads (IPC).
pub fn frame_reg(frame: usize, idx: usize) -> u64 {
    // SAFETY: `frame` ist ein gültiger, vom Trap-Pfad angelegter TrapFrame-Zeiger
    // (Kontext-/Trap-Domäne). `idx` < 31 wird vom Aufrufer (ABI) eingehalten.
    unsafe { (*(frame as *const TrapFrame)).gpr[idx] }
}

/// Register `xidx` eines (gesicherten) TrapFrames schreiben (IPC-Transfer).
pub fn frame_set_reg(frame: usize, idx: usize, val: u64) {
    // SAFETY: wie `frame_reg`; schreibender Zugriff auf den gesicherten Kontext.
    unsafe {
        (*(frame as *mut TrapFrame)).gpr[idx] = val;
    }
}

/// Einen initialen TrapFrame am oberen Ende eines frischen Thread-Stacks
/// anlegen, sodass der Trap-Restore-Epilog per `eret` in `entry(arg)` springt.
/// Gibt den Frame-Zeiger (initialer gespeicherter SP des Threads) zurück.
pub fn init_thread_frame(stack_top: usize, entry: usize, arg: usize) -> usize {
    let frame_addr = stack_top - core::mem::size_of::<TrapFrame>();
    // SAFETY: `frame_addr` liegt in einem frisch allozierten, exklusiv besessenen
    // Thread-Stack (RW, identity-gemappt). Wir initialisieren genau einen
    // TrapFrame, den der Restore-Epilog konsumiert. Thread-Kontext-Setup ist eine
    // erlaubte unsafe-Domäne.
    unsafe {
        let f = frame_addr as *mut TrapFrame;
        (*f).gpr = [0; 31];
        (*f).gpr[0] = arg as u64; // x0 = Argument
        (*f).elr = entry as u64; // Resume-PC
        (*f).spsr = 0x5; // EL1h, DAIF=0 -> IRQs frei (preemptierbar)
        (*f)._pad = 0;
        (*f).fpregs = [0; 32]; // frischer FP/SIMD-Zustand
        (*f).fpsr = 0;
        (*f).fpcr = 0;
    }
    frame_addr
}

/// Zentraler Trap-Handler (aus dem Assembler gerufen). Rückgabe: der
/// wiederherzustellende TrapFrame (normalerweise `frame`, bei einem
/// Scheduler-Switch der eines anderen Threads).
#[no_mangle]
pub extern "C" fn handle_exception(frame: *mut TrapFrame, kind: u64) -> *mut TrapFrame {
    if is_irq(kind) {
        let intid = crate::gic::handle_irq();
        if intid == Some(crate::timer::TIMER_INTID) {
            return reschedule(frame);
        }
        return frame;
    }

    // Nicht-IRQ: ESR auswerten.
    let esr: u64;
    // SAFETY: read-only Systemregister, keine Seiteneffekte.
    unsafe {
        asm!("mrs {}, ESR_EL1", out(reg) esr, options(nomem, nostack, preserves_flags));
    }
    let ec = (esr >> 26) & 0x3f;
    if ec == 0x15 {
        // SVC (AArch64) -> Syscall. ELR zeigt bereits hinter das `svc`.
        return syscall(frame);
    }

    // Echter Fault: diagnostizieren + anhalten.
    let far: u64;
    // SAFETY: read-only Systemregister.
    unsafe {
        asm!("mrs {}, FAR_EL1", out(reg) far, options(nomem, nostack, preserves_flags));
    }
    // SAFETY: `frame` zeigt auf den gültigen, vom Vektor angelegten Stack-Frame.
    let frame = unsafe { &*frame };

    crate::console::emit_raw("\n[EXCEPTION] unerwarteter Trap\n");
    crate::console::emit_fmt(format_args!(
        "  kind={} ({})\n  ESR={:#018x} (EC={:#04x})\n  ELR={:#018x}\n  FAR={:#018x}\n  SPSR={:#018x}\n",
        kind,
        kind_name(kind),
        esr,
        (esr >> 26) & 0x3f,
        frame.elr,
        far,
        frame.spsr,
    ));
    cpu::halt();
}

fn kind_name(kind: u64) -> &'static str {
    match kind {
        0..=3 => "Current EL SP0",
        4..=7 => "Current EL SPx",
        8..=11 => "Lower EL aarch64",
        _ => "Lower EL aarch32",
    }
}
