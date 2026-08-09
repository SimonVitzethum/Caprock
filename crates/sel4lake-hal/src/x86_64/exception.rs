//! Interrupt-/Exception-Dispatch (x86_64, Long Mode) — API-gleich zum aarch64-Pendant.
//!
//! ## Dasselbe Trap-Modell wie auf ARM
//!
//! Der Scheduler des Kernels ist architekturneutral, **weil** beide Architekturen dasselbe
//! Modell benutzen: ein Trap sichert den vollen Registersatz in einen [`TrapFrame`] **auf
//! dem Stack**, ruft [`handle_exception`] mit dessen Adresse, und der Rückgabewert ist der
//! wiederherzustellende Frame — **möglicherweise der eines anderen Threads**. Der
//! Kontextwechsel ist damit auch hier ein reiner Stackzeiger-Tausch:
//!
//! ```text
//!   ISR-Stub:  GPRs pushen -> rdi = rsp -> call handle_exception
//!              -> rsp = rax (evtl. FREMDER Frame) -> GPRs poppen -> iretq
//! ```
//!
//! `iretq` lädt im Long Mode **immer** SS:RSP (anders als 32-bit) — der Wechsel zwischen
//! Kernel- und User-Stack und zwischen Threads fällt damit ohne Sonderfall an.
//!
//! ## Registerabbildung der Syscall-ABI
//!
//! Die ABI (`sel4lake-abi`) spricht von `x0..x6`. Auf x86_64 bilden wir sie auf die
//! Linux-nahe Syscall-Konvention ab (`RAX` = Nummer/Ergebnis, dann `RDI, RSI, RDX, R10, R8,
//! R9`) — [`ABI_TO_GPR`] ist die einzige Stelle, die das festlegt.

use super::{console, cpu, intc};
use crate::hook::AtomicHook;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

/// Auf dem Stack gesicherter Registerkontext eines Traps.
///
/// Das Layout entspricht **exakt** der Push-Reihenfolge im Stub plus dem, was die CPU beim
/// Interrupt selbst ablegt (`rip`,`cs`,`rflags`,`rsp`,`ss` — und bei einigen Vektoren einen
/// Fehlercode, für den die übrigen Stubs eine Null einschieben, damit das Layout **einheitlich**
/// bleibt).
///
/// Der FP/SIMD-Zustand liegt **nicht** im Frame: er wird lazy verwaltet (siehe [`super::fp`]).
#[repr(C)]
pub struct TrapFrame {
    /// Allzweckregister in der Reihenfolge, in der der Stub sie ablegt (aufsteigende
    /// Adressen): rax, rbx, rcx, rdx, rsi, rdi, rbp, r8..r15.
    pub gpr: [u64; 15],
    /// Interrupt-/Exception-Vektor (vom Stub eingeschoben).
    pub vector: u64,
    /// Fehlercode (von der CPU oder als Null vom Stub eingeschoben).
    pub error: u64,
    /// Unterbrochener Befehlszeiger.
    pub rip: u64,
    /// Code-Segment-Selektor (RPL = Ring des unterbrochenen Kontexts).
    pub cs: u64,
    pub rflags: u64,
    /// Stackzeiger des unterbrochenen Kontexts (`iretq` lädt ihn zurück).
    pub rsp: u64,
    pub ss: u64,
}

/// Indizes in [`TrapFrame::gpr`].
const GPR_RAX: usize = 0;
const GPR_RDX: usize = 3;
const GPR_RSI: usize = 4;
const GPR_RDI: usize = 5;
const GPR_R8: usize = 7;
const GPR_R9: usize = 8;
const GPR_R10: usize = 9;

/// Abbildung der ABI-Register `x0..x6` auf [`TrapFrame::gpr`]-Indizes.
/// `x0` = Syscall-Nummer/Ergebnis, `x1` = Cap/Badge, `x2..x5` = Nachricht, `x6` = Tag.
const ABI_TO_GPR: [usize; 7] = [
    GPR_RAX, // x0
    GPR_RDI, // x1
    GPR_RSI, // x2
    GPR_RDX, // x3
    GPR_R10, // x4
    GPR_R8,  // x5
    GPR_R9,  // x6
];

/// Kernel-Segmentselektoren (GDT-Einträge 1 und 2, s. [`super::gdt`]).
pub const KERNEL_CS: u64 = 0x08;
pub const KERNEL_DS: u64 = 0x10;
/// User-Segmentselektoren (GDT 3 und 4, RPL 3).
pub const USER_DS: u64 = 0x18 | 3;
pub const USER_CS: u64 = 0x20 | 3;

/// RFLAGS beim Thread-Start: reserviertes Bit 1 + IF (Interrupts frei).
const RFLAGS_START: u64 = 0x202;

/// Vektor der **Syscall-Software-Interrupts** aus Kernel-Threads (`int 0x80`).
///
/// Kernel-Threads laufen bereits in Ring 0; ein `syscall` von dort wäre umständlich (kein
/// Stackwechsel, RCX/R11 werden zerstört). Ein Software-Interrupt legt dagegen **genau
/// denselben** Frame an wie jeder andere Trap — der Dispatch bleibt einheitlich.
pub const SYSCALL_VECTOR: u64 = 0x80;

/// Vektoren, bei denen die CPU **selbst** einen Fehlercode ablegt.
const fn has_error_code(v: u32) -> bool {
    matches!(v, 8 | 10 | 11 | 12 | 13 | 14 | 17 | 21 | 29 | 30)
}

// --- ISR-Stubs ----------------------------------------------------------------------------

global_asm!(
    r#"
.section .text

/* Gemeinsamer Teil: GPRs sichern, Handler rufen, (evtl. anderen) Frame wiederherstellen. */
__isr_common:
    push r15
    push r14
    push r13
    push r12
    push r11
    push r10
    push r9
    push r8
    push rbp
    push rdi
    push rsi
    push rdx
    push rcx
    push rbx
    push rax
    mov  rdi, rsp                 /* rdi = &TrapFrame (SysV: 1. Argument) */
    call handle_exception
    mov  rsp, rax                 /* rax = wiederherzustellender Frame (ggf. anderer Thread) */
    pop  rax
    pop  rbx
    pop  rcx
    pop  rdx
    pop  rsi
    pop  rdi
    pop  rbp
    pop  r8
    pop  r9
    pop  r10
    pop  r11
    pop  r12
    pop  r13
    pop  r14
    pop  r15
    add  rsp, 16                  /* Vektor + Fehlercode verwerfen */
    iretq

/* Stub ohne CPU-Fehlercode: Null einschieben, damit das Frame-Layout einheitlich bleibt. */
.macro ISR_NOERR num
.globl __isr_\num
__isr_\num:
    push 0
    push \num
    jmp  __isr_common
.endm

/* Stub mit CPU-Fehlercode: nur den Vektor einschieben. */
.macro ISR_ERR num
.globl __isr_\num
__isr_\num:
    push \num
    jmp  __isr_common
.endm
"#
);

// Die 256 Stubs + eine Tabelle ihrer Adressen erzeugt der Assembler selbst (256 einzelne
// `extern`-Symbole in Rust wären reine Boilerplate). `.altmacro` erlaubt es, den Schleifen-
// zähler als Makro-Argument zu übergeben (`%i` wertet ihn aus).
global_asm!(
    r#"
.altmacro
.section .text
.set i, 0
.rept 256
.if (i==8)||(i==10)||(i==11)||(i==12)||(i==13)||(i==14)||(i==17)||(i==21)||(i==29)||(i==30)
    ISR_ERR %i
.else
    ISR_NOERR %i
.endif
.set i, i+1
.endr

/* Tabelle der 256 Stub-Adressen — der Rust-Code liest sie und baut daraus die IDT.
   `%i` wird nur als MAKRO-ARGUMENT ersetzt (altmacro), daher auch hier über ein Makro. */
.macro ISR_ENTRY num
    .quad __isr_\num
.endm
.section .rodata
.balign 8
.globl __isr_table
__isr_table:
.set i, 0
.rept 256
    ISR_ENTRY %i
    .set i, i+1
.endr
"#
);

extern "C" {
    /// 256 Stub-Adressen (vom Assembler erzeugt, s. o.).
    static __isr_table: [u64; 256];
}

// --- IDT ------------------------------------------------------------------------------------

/// Ein 16-Byte-IDT-Eintrag (Interrupt-Gate, Long Mode).
#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const EMPTY: IdtEntry = IdtEntry {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        zero: 0,
    };

    /// `dpl` = niedrigste Ring-Stufe, die diesen Vektor per `int n` auslösen darf.
    fn gate(handler: u64, dpl: u8) -> IdtEntry {
        IdtEntry {
            offset_low: handler as u16,
            selector: KERNEL_CS as u16,
            ist: 0,
            // present(1) | dpl | 0 | Typ 0xE (64-bit Interrupt-Gate: löscht IF beim Eintritt)
            type_attr: 0x8E | (dpl << 5),
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            zero: 0,
        }
    }
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::EMPTY; 256];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// IDT aufbauen + laden. Pro Kern aufzurufen (die Tabelle selbst ist global).
pub fn init() {
    // SAFETY: `IDT` ist eine statische Tabelle, die ausschließlich hier (beim Boot, vor dem
    // Freigeben von Interrupts) beschrieben wird; `__isr_table` liefert die vom Assembler
    // erzeugten Stub-Adressen. Das Laden von IDTR ist eine erlaubte Low-Level-Domäne.
    unsafe {
        let table = &*core::ptr::addr_of!(__isr_table);
        let idt = &mut *core::ptr::addr_of_mut!(IDT);
        for (v, entry) in idt.iter_mut().enumerate() {
            // Nur der Syscall-Vektor darf aus Ring 3 per `int` ausgelöst werden.
            let dpl = if v as u64 == SYSCALL_VECTOR { 3 } else { 0 };
            *entry = IdtEntry::gate(table[v], dpl);
        }
        let ptr = DescriptorTablePointer {
            limit: (core::mem::size_of_val(idt) - 1) as u16,
            base: idt.as_ptr() as u64,
        };
        asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
    }
}

// --- Hooks (identisch zur aarch64-Fassung) ---------------------------------------------------

pub type RescheduleHook = fn(*mut TrapFrame) -> *mut TrapFrame;
pub type SyscallHook = fn(*mut TrapFrame) -> *mut TrapFrame;
pub type FaultHook = fn(*mut TrapFrame, u64, u64) -> *mut TrapFrame;
pub type FpHook = fn(*mut TrapFrame) -> *mut TrapFrame;
pub type IrqHook = fn(u32) -> bool;

static RESCHED_HOOK: AtomicHook<RescheduleHook> = AtomicHook::new();
static SYSCALL_HOOK: AtomicHook<SyscallHook> = AtomicHook::new();
static FAULT_HOOK: AtomicHook<FaultHook> = AtomicHook::new();
static FP_HOOK: AtomicHook<FpHook> = AtomicHook::new();
static IRQ_HOOK: AtomicHook<IrqHook> = AtomicHook::new();

/// **Das Vektor-Inventar** (Z25): ein Zähler je CPU-Ausnahmevektor.
///
/// **Warum nur Vektor 0..31:** das sind die Ausnahmen, von denen die meisten *nie* vorkommen
/// dürfen — und genau die verschwinden sonst spurlos, weil niemand nach ihnen sucht. Die heissen
/// Vektoren (Syscall, Timer) sind bewusst **nicht** dabei: dort kostete ein zusätzliches Atomic
/// je Eintritt etwas auf dem heissesten Pfad des Systems, und beide werden ohnehin an anderer
/// Stelle gezählt (`USER_SYSCALLS`, Ticks).
///
/// Der Anlass war `#NM` (Vektor 7): unter eager darf er nicht auftreten, und ein Melder, der nur
/// beim Unglück spricht, wäre in jedem gesunden Lauf stumm — man wüsste nie, ob er sprechfähig
/// ist. Ein Inventar dagegen ist in **jedem** Lauf ablesbar.
#[allow(clippy::declare_interior_mutable_const)]
static VECTOR_HITS: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];

/// Wie oft Ausnahmevektor `v` (0..31) genommen wurde.
pub fn vector_hits(v: usize) -> u64 {
    VECTOR_HITS.get(v).map_or(0, |c| c.load(Ordering::Relaxed))
}

/// Klartextname eines Ausnahmevektors — für das Inventar.
pub fn vector_label(v: u64) -> &'static str {
    vector_name(v)
}

pub fn set_reschedule_hook(hook: RescheduleHook) {
    RESCHED_HOOK.store(hook);
}
pub fn set_syscall_hook(hook: SyscallHook) {
    SYSCALL_HOOK.store(hook);
}
pub fn set_fault_hook(hook: FaultHook) {
    FAULT_HOOK.store(hook);
}
pub fn set_fp_hook(hook: FpHook) {
    FP_HOOK.store(hook);
}
pub fn set_irq_hook(hook: IrqHook) {
    IRQ_HOOK.store(hook);
}

// --- Frame-Zugriff (ABI) ----------------------------------------------------------------------

/// ABI-Register `xidx` eines gesicherten Frames lesen.
pub fn frame_reg(frame: usize, idx: usize) -> u64 {
    let slot = ABI_TO_GPR[idx.min(ABI_TO_GPR.len() - 1)];
    // SAFETY: `frame` ist ein gültiger, vom Trap-Pfad angelegter TrapFrame-Zeiger
    // (Kontext-/Trap-Domäne); `slot` ist per Konstruktion < 15.
    unsafe { (*(frame as *const TrapFrame)).gpr[slot] }
}

/// ABI-Register `xidx` eines gesicherten Frames schreiben (IPC-Transfer).
pub fn frame_set_reg(frame: usize, idx: usize, val: u64) {
    let slot = ABI_TO_GPR[idx.min(ABI_TO_GPR.len() - 1)];
    // SAFETY: wie `frame_reg`; schreibender Zugriff auf den gesicherten Kontext.
    unsafe {
        (*(frame as *mut TrapFrame)).gpr[slot] = val;
    }
}

/// Gesicherter Prozessorstatus (aarch64 `SPSR`) — hier RFLAGS.
pub fn frame_spsr(frame: usize) -> u64 {
    // SAFETY: wie `frame_reg`.
    unsafe { (*(frame as *const TrapFrame)).rflags }
}

/// Kam der Trap aus dem **User-Modus** (aarch64: EL0)? Auf x86: RPL des gesicherten CS = 3.
pub fn frame_from_el0(frame: usize) -> bool {
    // SAFETY: wie `frame_reg`.
    unsafe { (*(frame as *const TrapFrame)).cs & 3 == 3 }
}

/// Initialen Kontext eines neuen Threads am Stack-Top anlegen; gibt den zu sichernden
/// Stackzeiger (= Frame-Adresse) zurück.
///
/// `user = false`: Kernel-Thread (Ring 0) mit `stack_top` als eigenem Stack.
/// `user = true`: Ring-3-Thread; `user_sp` ist sein User-Stackzeiger, `stack_top` das obere
/// Ende seines **Kernel**-Stacks (dort liegt der Frame).
pub fn init_thread_frame(
    stack_top: usize,
    entry: usize,
    arg: usize,
    user: bool,
    user_sp: usize,
) -> usize {
    let frame_addr = (stack_top - core::mem::size_of::<TrapFrame>()) & !0xf;
    // SysV erwartet beim Funktionseintritt `rsp % 16 == 8` (so, als hätte ein `call` gerade
    // die Rücksprungadresse abgelegt). Ohne das dürfte der Compiler 16-Byte-ausgerichtete
    // Spills setzen und würde faulten.
    let sp = if user {
        ((user_sp & !0xf) - 8) as u64
    } else {
        ((frame_addr & !0xf) - 8) as u64
    };
    // SAFETY: `[stack_top - size_of::<TrapFrame>(), stack_top)` liegt im frisch allozierten,
    // exklusiv gehaltenen Stack des neuen Threads; wir initialisieren ihn vollständig.
    unsafe {
        let f = frame_addr as *mut TrapFrame;
        (*f).gpr = [0; 15];
        (*f).gpr[GPR_RDI] = arg as u64; // SysV: 1. Argument
        (*f).vector = 0;
        (*f).error = 0;
        (*f).rip = entry as u64;
        (*f).cs = if user { USER_CS } else { KERNEL_CS };
        (*f).rflags = RFLAGS_START;
        (*f).rsp = sp;
        (*f).ss = if user { USER_DS } else { KERNEL_DS };
    }
    frame_addr
}

// --- Dispatch -----------------------------------------------------------------------------

/// Vektor des LAPIC-Timers (periodische Zeitscheibe).
pub const TIMER_VECTOR: u64 = 32;
/// Vektor des Reschedule-IPI (Cross-Core-Wecken).
pub const IPI_RESCHED_VECTOR: u64 = 33;

/// Zentraler Trap-Handler (aus dem Assembler-Stub gerufen). Rückgabe: der
/// wiederherzustellende TrapFrame (normalerweise `frame`, bei einem Scheduler-Switch der
/// eines anderen Threads).
/// Den Frame zurückgeben und dabei — falls wir nach **Ring 3** zurückkehren — `TSS.RSP0` auf
/// den Kernel-Stack **genau dieses** Threads setzen.
///
/// Das ist der x86-Ersatz für `SP_EL1` auf ARM: dort hat jede Privilegstufe ihr eigenes
/// Stackregister, hier schaltet die CPU beim Trap aus Ring 3 auf `TSS.RSP0` um. Zeigte der
/// weiter auf den Stack des vorigen Threads, überschriebe der nächste Trap dessen Frame.
/// Der Frame eines Ring-3-Threads liegt am oberen Ende seines Kernel-Stacks — die Adresse
/// direkt darüber ist damit genau der richtige `RSP0`.
fn resume(frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: gültiger, vom Trap-Pfad angelegter bzw. vom Scheduler gelieferter Frame.
    if unsafe { (*frame).cs } & 3 == 3 {
        super::gdt::set_kernel_stack(frame as u64 + core::mem::size_of::<TrapFrame>() as u64);
    }
    frame
}

#[no_mangle]
pub extern "C" fn handle_exception(frame: *mut TrapFrame) -> *mut TrapFrame {
    // SAFETY: der Stub übergibt den eben angelegten, gültigen Frame.
    let vector = unsafe { (*frame).vector };
    // Vektor-Inventar (Z25): nur die CPU-Ausnahmen, nicht der heisse Syscall-/Timer-Pfad.
    if vector < 32 {
        VECTOR_HITS[vector as usize].fetch_add(1, Ordering::Relaxed);
    }

    // --- Syscall (`int 0x80`) ---
    if vector == SYSCALL_VECTOR {
        return resume(match SYSCALL_HOOK.load() {
            Some(hook) => hook(frame),
            None => frame,
        });
    }

    // --- Externe Interrupts (LAPIC) ---
    if vector >= 32 {
        intc::eoi();
        if vector == TIMER_VECTOR {
            super::timer::on_irq();
        }
        if vector == TIMER_VECTOR || vector == IPI_RESCHED_VECTOR {
            return resume(match RESCHED_HOOK.load() {
                Some(hook) => hook(frame),
                None => frame,
            });
        }
        // Geräte-IRQ: dem Kernel melden (pending + maskieren); war er registriert, fährt der
        // Dispatch einen Reschedule (der Drain stellt ihn außerhalb des IRQ-Kontexts zu).
        let handled = match IRQ_HOOK.load() {
            Some(hook) => hook(vector as u32),
            None => false,
        };
        if handled {
            return resume(match RESCHED_HOOK.load() {
                Some(hook) => hook(frame),
                None => frame,
            });
        }
        return resume(frame);
    }

    // --- #NM (Device Not Available): Lazy-FP-Owner-Wechsel ---
    if vector == 7 {
        return resume(match FP_HOOK.load() {
            Some(hook) => hook(frame),
            // Kein Lazy-FP-Subsystem: unerwartet -> wie ein Fault behandeln.
            None => fatal(frame),
        });
    }

    // --- Echte Faults ---
    // Aus Ring 3: den fehlerhaften Thread isolieren (beenden), Kernel läuft weiter.
    if frame_from_el0(frame as usize) {
        if let Some(hook) = FAULT_HOOK.load() {
            // ESR-Äquivalent: Vektor + Fehlercode; FAR-Äquivalent: CR2 (Fault-Adresse).
            // SAFETY: gültiger Frame (s. o.).
            let err = unsafe { (*frame).error };
            let esr = (vector << 32) | err;
            return resume(hook(frame, esr, read_cr2()));
        }
    }
    fatal(frame)
}

/// Nicht behandelbarer Trap (Kernel-Bug): diagnostizieren + anhalten.
fn fatal(frame: *mut TrapFrame) -> ! {
    // SAFETY: gültiger, vom Stub angelegter Frame.
    let f = unsafe { &*frame };
    let (vector, error, rip, cs, rflags, rsp) = (f.vector, f.error, f.rip, f.cs, f.rflags, f.rsp);
    console::emit_raw("\n[EXCEPTION] unerwarteter Trap\n");
    console::emit_fmt(format_args!(
        "  vector={} ({})\n  error={:#018x}\n  rip={:#018x}\n  cs={:#06x} rflags={:#010x}\n  rsp={:#018x}\n  cr2={:#018x}\n",
        vector,
        vector_name(vector),
        error,
        rip,
        cs,
        rflags,
        rsp,
        read_cr2(),
    ));
    cpu::halt();
}

/// `CR2` — bei einem #PF die fehlerhafte lineare Adresse (aarch64-`FAR`-Äquivalent).
fn read_cr2() -> u64 {
    let v: u64;
    // SAFETY: reines Lesen eines Steuerregisters, keine Speicherwirkung.
    unsafe { asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

fn vector_name(v: u64) -> &'static str {
    match v {
        0 => "#DE divide error",
        3 => "#BP breakpoint",
        6 => "#UD invalid opcode",
        7 => "#NM device not available",
        8 => "#DF double fault",
        11 => "#NP segment not present",
        12 => "#SS stack fault",
        13 => "#GP general protection",
        14 => "#PF page fault",
        16 => "#MF x87 fp",
        17 => "#AC alignment check",
        19 => "#XM simd fp",
        _ => "(sonstiger Vektor)",
    }
}
