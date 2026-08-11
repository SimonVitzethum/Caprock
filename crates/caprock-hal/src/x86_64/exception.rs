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
//! Die ABI (`caprock-abi`) spricht von `x0..x6`. Auf x86_64 bilden wir sie auf die
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
    /// `ist` = **einsbasierter** IST-Index (`0` = kein Stackwechsel, s. [`ist_fuer_vektor`]).
    fn gate(handler: u64, dpl: u8, ist: u8) -> IdtEntry {
        IdtEntry {
            offset_low: handler as u16,
            selector: KERNEL_CS as u16,
            ist,
            // present(1) | dpl | 0 | Typ 0xE (64-bit Interrupt-Gate: löscht IF beim Eintritt)
            type_attr: 0x8E | (dpl << 5),
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            zero: 0,
        }
    }
}

/// **Welcher Vektor bekommt einen eigenen Stack — und welcher ausdrücklich keinen.**
///
/// Die drei mit IST sind die, deren Handler laufen muss, **auch wenn der aktuelle Stack kaputt
/// ist**:
///
/// * **`#DF` (8)** — die Wache gegen den Kernel-Stack-Überlauf. Eine Guard-Page macht aus dem
///   Überlauf einen `#PF`; dessen Handler pusht auf denselben übergelaufenen Stack, und daraus
///   wird der `#DF`. Ohne eigenen Stack pusht auch der dorthin -> **Triple Fault, keine Ausgabe**.
/// * **`NMI` (2)** — kommt asynchron und ist nicht maskierbar. Er trifft jeden Stackzustand,
///   auch den mitten in einem Stackwechsel.
/// * **`#MC` (18)** — Machine Check, ebenfalls asynchron und ebenfalls nicht auf einen gesunden
///   Stack angewiesen.
///
/// **Jeder von ihnen bekommt einen EIGENEN Stack, nicht einen gemeinsamen.** Teilten sich zwei
/// Vektoren einen, wäre der eine im anderen nicht mehr diagnostizierbar: ein NMI, der einen
/// laufenden `#DF`-Handler unterbricht, setzte den Stackzeiger zurück an den Anfang derselben
/// Region und überschriebe dessen Frame — der `#DF` wäre danach spurlos.
///
/// # `#PF` (14) bekommt AUSDRÜCKLICH KEINEN IST — und das ist der wichtigste Eintrag hier
///
/// Ein IST-Gate lädt seinen Stackzeiger **bedingungslos**, also auch dann, wenn schon ein Handler
/// desselben Vektors auf genau diesem Stack läuft. `#PF` ist aber **wiedereintrittsfähig zu
/// sein gezwungen**: er ist der normale Betriebsfall (Demand Paging, EL0-Zugriffsfehler, jeder
/// Isolationstest dieses Projekts löst ihn absichtlich aus), und ein zweiter `#PF` während der
/// Behandlung des ersten ist ein alltäglicher Vorgang. Mit IST schriebe der zweite den Frame des
/// ersten nieder, und der Rücksprung ginge ins Leere — aus einem behandelbaren Fault würde ein
/// stiller Datenverlust.
///
/// Der **Stacküberlauf** wird deshalb nicht über `#PF` gefangen, sondern über `#DF`: genau dafür
/// ist dessen IST da, und genau deshalb darf `#DF` niemals wiedereintrittsfähig sein müssen (er
/// ist ein Abort — es gibt keine Rückkehr).
const fn ist_fuer_vektor(v: u32) -> u8 {
    match v {
        // Die Gegenprobe (`kein-df-ist`) nimmt AUSSCHLIESSLICH dem `#DF` seinen Stack — sie
        // isoliert damit genau eine Grösse. Eine Mutation, die zwei Dinge zugleich kaputtmacht,
        // beweist nichts über das gemeinte.
        #[cfg(not(feature = "kein-df-ist"))]
        8 => super::gdt::IST_DF,
        2 => super::gdt::IST_NMI,
        18 => super::gdt::IST_MC,
        _ => 0,
    }
}

/// Der IST-Index, der im IDT-Gate von `vector` **wirklich steht** — zurückgelesen, nicht
/// nachgerechnet. Für die `ist`-Prüfzeile des Kernels.
pub fn idt_ist(vector: usize) -> u8 {
    // SAFETY: reines Lesen eines statischen Tabelleneintrags; `IDT` wird nur in `init`
    // beschrieben (vor der Interrupt-Freigabe).
    unsafe { core::ptr::addr_of!((*core::ptr::addr_of!(IDT))[vector.min(255)].ist).read_volatile() }
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
            *entry = IdtEntry::gate(table[v], dpl, ist_fuer_vektor(v as u32));
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

// ================================================================================================
// #DF: DIE LAUTE MELDUNG (2026-08-10)
// ================================================================================================

/// **Wasserstand einer Stackregion** — wie viele Bytes am Fuss das Füllmuster NICHT mehr tragen.
///
/// Das Verfahren gehört dem Kernel (`kernel/src/kstackmark.rs`); die HAL darf es nicht kennen
/// (Kerngrenze). Sie reicht deshalb nur die Region herüber und bekommt eine Zahl zurück.
///
/// **Fail-closed:** ist nichts installiert oder wurde nie gefüllt, kommt „voll benutzt" heraus —
/// nicht „viel Luft". Ein Ausfall der Messung sieht damit aus wie der schlimmste Messwert.
pub type WasserstandHook = fn(base: u64, len: u64) -> u64;

static WASSERSTAND_HOOK: AtomicHook<WasserstandHook> = AtomicHook::new();

pub fn set_wasserstand_hook(hook: WasserstandHook) {
    WASSERSTAND_HOOK.store(hook);
}

/// **Wer auf diesem Kern zuletzt Ring-3-Kontext hatte** — SPERRFREI, fuer den `#DF`-Bericht.
///
/// Der Bericht identifiziert bisher den betroffenen **Stack** und nicht den **Thread**, weil
/// Thread-Slot und -Id in `SCHEDS`/`KSTACKS` stehen und beide einen Spinlock brauchen. Ein `#DF`
/// kann genau den Kontext unterbrochen haben, der ihn haelt: der Handler bliebe stehen, und
/// heraus kaeme **kein Output** — also exakt das Bild, gegen das die laute Meldung gebaut ist.
///
/// Der Kernel hat die Angabe aber sperrfrei vorliegen (`FP_OWNER`, ein `AtomicU64`). Er reicht
/// sie hier herein; die HAL kennt die Struktur dahinter nicht (Kerngrenze). `None` heisst
/// **„kein Ring-3-Kontext auf diesem Kern"** und nicht „unbekannt" — die beiden gehoeren im
/// Bericht auseinander, sonst liest sich ein leerer Kern wie ein verlorener Thread.
pub type Ring3KontextHook = fn(core: usize) -> Option<u64>;

static RING3_KONTEXT_HOOK: AtomicHook<Ring3KontextHook> = AtomicHook::new();

pub fn set_ring3_kontext_hook(hook: Ring3KontextHook) {
    RING3_KONTEXT_HOOK.store(hook);
}

/// Benutzte Tiefe einer Region, oder `None`, wenn niemand messen kann.
fn wasserstand(base: u64, len: u64) -> Option<u64> {
    WASSERSTAND_HOOK.load().map(|h| h(base, len))
}

/// **Die IST-Sonde** (nur `selftest`): welcher Stackzeiger kam bei einem IST-Vektor heraus?
///
/// Warum das überhaupt gemessen wird: ein IST-Eintrag, der **nie benutzt wurde**, ist von einem
/// falsch aufgesetzten nicht zu unterscheiden. Ein Off-by-one im Gate-Index (`ist = 2` statt `1`)
/// lädt einfach den Stack des Nachbarvektors — der Handler läuft, druckt, und alles sieht gesund
/// aus, bis die beiden Vektoren einmal zusammentreffen.
///
/// Geprüft wird deshalb an der **Wirkung**: die Sonde löst `int 2` bzw. `int 18` aus und liest
/// hinterher die Frame-Adresse zurück, die der Handler bekommen hat. Liegt sie in der Region, die
/// für genau diesen Vektor gedacht ist, hat der Mechanismus gegriffen **und** der Index stimmt.
///
/// Für `#DF` (8) geht das **nicht** über `int 8`: bei einem Software-Interrupt schiebt die CPU
/// keinen Fehlercode ein, der Stub für Vektor 8 erwartet aber einen (`ISR_ERR`) — das Frame-Layout
/// wäre um 8 Byte verschoben. Der `#DF` wird deshalb in `tools/df-sonde.sh` **echt** ausgelöst.
/// Der Zustand ist **je Kern**, nicht global: jeder Kern misst seine EIGENEN IST-Stacks. Eine
/// gemeinsame Zelle hiesse, dass drei Kerne die Aussage eines vierten erben — dieselbe Form wie
/// „eine Ablage je Rolle", die dieses Projekt schon zweimal bezahlt hat.
#[cfg(feature = "selftest")]
#[allow(clippy::declare_interior_mutable_const)]
static IST_SONDE_ERWARTET: [AtomicU64; super::gdt::MAX_TSS_CORES] =
    [const { AtomicU64::new(u64::MAX) }; super::gdt::MAX_TSS_CORES];
#[cfg(feature = "selftest")]
#[allow(clippy::declare_interior_mutable_const)]
static IST_SONDE_RSP: [AtomicU64; super::gdt::MAX_TSS_CORES] =
    [const { AtomicU64::new(0) }; super::gdt::MAX_TSS_CORES];

/// Die Sonde des **aufrufenden** Kerns auf `vector` scharf stellen (nur 2 und 18, s. o.).
#[cfg(feature = "selftest")]
pub fn ist_sonde_armieren(vector: u64) -> bool {
    if vector != 2 && vector != 18 {
        return false;
    }
    let Some(c) = super::gdt::core_from_tr() else {
        return false;
    };
    IST_SONDE_RSP[c].store(0, Ordering::Relaxed);
    IST_SONDE_ERWARTET[c].store(vector, Ordering::Release);
    true
}

/// Frame-Adresse, die der Handler beim letzten Sondenschuss von Kern `core` bekommen hat
/// (`0` = kein Schuss).
#[cfg(feature = "selftest")]
pub fn ist_sonde_rsp(core: usize) -> u64 {
    IST_SONDE_RSP.get(core).map_or(0, |c| c.load(Ordering::Acquire))
}

/// Steht die Sonde von Kern `core` noch scharf? (`true` heisst: der Schuss ist **nicht**
/// angekommen — genau der Fall, den eine Sprechprobe von „gemessen" unterscheiden muss.)
#[cfg(feature = "selftest")]
pub fn ist_sonde_scharf(core: usize) -> bool {
    IST_SONDE_ERWARTET
        .get(core)
        .is_none_or(|c| c.load(Ordering::Acquire) != u64::MAX)
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

    // --- #DF: IMMER tödlich, und zwar VOR jeder anderen Verzweigung -----------------------------
    //
    // **Das ist eine Verhaltensänderung, und sie behebt einen Fehler.** Bis hierher lief ein `#DF`
    // durch dieselbe Kette wie jeder andere Fault — und `frame_from_el0` entscheidet dort nach dem
    // gesicherten `CS`. Bei genau dem Bild, gegen das die Guard-Page gebaut wird (ein Ring-3-Thread
    // trappt, `RSP0` zeigt auf einen übergelaufenen Kernel-Stack, die Frame-Ablage faultet zweimal),
    // trägt der gesicherte Frame **User-CS**. Der `#DF` wäre also im `FAULT_HOOK` gelandet, der
    // Kernel hätte den User-Thread beendet und wäre weitergelaufen — auf einem Kernel-Stack, dessen
    // Ende nachweislich überschrieben ist. Ein Abort, der als „unartiger User-Thread" verbucht wird,
    // ist die schlechtestmögliche Form von „Schweigen als Erfolg".
    //
    // Ein `#DF` ist ein **Abort**: `RIP` im Frame ist nicht als Wiedereinstiegspunkt zugesichert,
    // es gibt keine Rückkehr. Der einzige richtige Ausgang ist: laut reden, dann anhalten.
    if vector == 8 {
        df_fatal(frame);
    }

    // --- Die IST-Sonde (nur `selftest`) ---------------------------------------------------------
    //
    // Steht **hinter** dem `#DF`-Zweig, damit sie einen echten Double Fault unter keinen Umständen
    // verschlucken kann, und sie nimmt ausschliesslich die beiden Vektoren, auf die sie armiert
    // werden darf.
    #[cfg(feature = "selftest")]
    if vector == 2 || vector == 18 {
        if let Some(c) = super::gdt::core_from_tr() {
            if IST_SONDE_ERWARTET[c].load(Ordering::Acquire) == vector {
                IST_SONDE_RSP[c].store(frame as u64, Ordering::Relaxed);
                IST_SONDE_ERWARTET[c].store(u64::MAX, Ordering::Release);
                return resume(frame);
            }
        }
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

/// **Der Double Fault — die laute Meldung.**
///
/// Ein `#DF`, der nur „#DF" sagt, kostet denselben halben Tag wie ein leeres Protokoll: man weiss
/// dann, dass etwas Schlimmes passiert ist, aber nicht **wo**. Gedruckt wird deshalb alles, was
/// ohne eine einzige Sperre und ohne einen einzigen möglicherweise faultenden Zugriff erreichbar
/// ist:
///
/// * **Kern** (aus `TR`, nicht aus `cpuid` — s. `gdt::core_from_tr`),
/// * **`RIP`/`CS`/`RSP`/`RFLAGS`** des unterbrochenen Kontexts aus dem Frame; `CS & 3` sagt, ob
///   der Überlauf einen Kernel- oder einen Ring-3-Kontext getroffen hat,
/// * **`CR2`** — die Adresse, an der der *ursprüngliche* `#PF` scheiterte. Sie steht noch im
///   Register und ist bei einem Stacküberlauf genau die Guard-Page,
/// * **der IST-Stack, auf dem wir stehen** samt seinem eigenen Wasserstand: das ist zugleich der
///   Beleg, dass der IST-Mechanismus überhaupt gegriffen hat,
/// * **der betroffene EL0-Kernel-Stack** (aus `TSS.rsp0` dieses Kerns) samt Wasserstand.
///
/// **Warum hier keine Sperre genommen wird.** Der laufende Thread-Slot steht in `SCHEDS`/`KSTACKS`
/// und wäre nur unter einem Spinlock lesbar. Ein `#DF` kann aber einen Kontext unterbrochen haben,
/// der genau diese Sperre hält — der Handler bliebe darin stehen, und heraus käme **kein Output**,
/// also exakt das Bild, gegen das diese Funktion gebaut ist. Der Thread wird deshalb über seinen
/// **Stack** identifiziert (lock-frei aus `RSP0`) und nicht über seine Slot-Nummer. Die offene
/// Lücke steht ausdrücklich in der Ausgabe.
/// **Was `CR2` in einem `#DF`-Bericht ueberhaupt bedeutet -- und wann NICHT.**
///
/// `CR2` traegt die Adresse des letzten `#PF`. Bei einem Stackueberlauf ist das genau die
/// Guard-Page, und die Zeile ist die wertvollste des Berichts. **Ein `#DF` entsteht aber auch
/// anders** -- etwa aus einem `#GP` bei der Zustellung einer Ausnahme, oder aus einem Fehler beim
/// Zugriff auf die IDT selbst. Dann steht in `CR2` irgendein alter Wert, und die Beschriftung
/// „hier scheiterte der urspruengliche Fault" schickt den Leser an eine Adresse, die mit dem
/// Vorfall nichts zu tun hat. Die Architektur sagt uns den Vektor des Erstfaults nicht.
///
/// **Der naheliegende Weg traegt nicht:** ein „`#PF` gerade in Arbeit"-Bit im `#PF`-Handler waere
/// fuer genau unseren Fall FALSCH -- beim Stackueberlauf scheitert die CPU schon beim Ablegen des
/// `#PF`-Frames, der Handler wird nie betreten. Also wird nicht behauptet, sondern **geprueft**:
/// liegt `CR2` auf der Seite unmittelbar unterhalb des EL1-Stacks, den dieser Kern zuletzt scharf
/// gemacht hat, dann ist es die Wache dieses Stacks -- eine nachrechenbare Aussage. Sonst sagt die
/// Zeile ausdruecklich, dass der Wert veraltet sein kann.
fn cr2_deutung(kern: Option<usize>, cr2: u64) -> &'static str {
    // **Zuerst die allgemeine, gelesene Antwort.** Liegt `CR2` auf einer stehenden Wache, war der
    // Erstfault ein Stackueberlauf -- gleichgueltig, zu welchem Thread der Stack gehoert.
    if super::mmu::ist_wache(cr2 & !0xFFF) {
        // Und dann die schaerfere, falls sie zutrifft: gehoert die Wache zu dem Stack, den DIESER
        // Kern zuletzt scharf gemacht hat? Nicht immer -- `TSS.rsp0` kann auf einen toten Stack
        // zeigen (gemessen 2026-08-11).
        if let Some(c) = kern {
            if let Some((kb, _)) = super::gdt::kstack_basis_von_rsp0(super::gdt::tss_rsp0(c)) {
                if kb >= 4096 && cr2 & !0xFFF == kb - 4096 {
                    return "<- die GUARD-PAGE des EL1-Stacks DIESES Kerns: Stackueberlauf, CR2 gueltig";
                }
            }
        }
        return "<- eine stehende GUARD-PAGE (nicht die aus TSS.rsp0 dieses Kerns -- rsp0 kann \
                auf einen toten Stack zeigen): Stackueberlauf, CR2 gueltig";
    }
    "<- nur gueltig, WENN der Erstfault ein #PF war; die Adresse liegt auf KEINER stehenden \
     Wache, kann also ein alter Wert sein"
}

fn df_fatal(frame: *mut TrapFrame) -> ! {
    // SAFETY: gültiger, vom Stub angelegter Frame.
    let f = unsafe { &*frame };
    let (error, rip, cs, rflags, rsp) = (f.error, f.rip, f.cs, f.rflags, f.rsp);
    let cr2 = read_cr2();
    let kern = super::gdt::core_from_tr();

    console::emit_raw("\n[#DF] DOUBLE FAULT -- Abort, keine Rueckkehr moeglich.\n");
    console::emit_fmt(format_args!(
        "  kern={} (TR-Selektor {:#06x})\n  \
         rip={:#018x} cs={:#06x} ({})\n  \
         rsp={:#018x} rflags={:#010x} error={:#x}\n  \
         cr2={:#018x}  {}\n",
        match kern {
            Some(c) => c as i64,
            None => -1,
        },
        kern.map_or(0, super::gdt::tss_selector),
        rip,
        cs,
        if cs & 3 == 3 { "Ring 3 unterbrochen" } else { "Ring 0 unterbrochen" },
        rsp,
        rflags,
        error,
        cr2,
        cr2_deutung(kern, cr2),
    ));

    if let Some(c) = kern {
        // (a) Der Stack, auf dem WIR stehen. `frame` liegt darin — das ist der Beleg, dass die
        //     CPU auf den IST-Stack umgeschaltet hat und nicht auf dem kaputten weitergemacht hat.
        let (ib, il) = super::gdt::ist_region(c, super::gdt::ISTF_DF);
        let drin = (frame as u64) >= ib && (frame as u64) < ib + il;
        console::emit_fmt(format_args!(
            "  IST[#DF] = [{:#x},{:#x}) {} B · frame={:#x} liegt {}\n",
            ib,
            ib + il,
            il,
            frame as u64,
            if drin { "DARIN (IST hat gegriffen)" } else { "NICHT darin -- IST-Aufbau falsch!" },
        ));
        match wasserstand(ib, il) {
            Some(b) => console::emit_fmt(format_args!(
                "  IST[#DF] Wasserstand: {} von {} B benutzt{}\n",
                b,
                il,
                if b >= il { "  <- AUFGEBRAUCHT oder nie gefuellt" } else { "" },
            )),
            None => console::emit_raw("  IST[#DF] Wasserstand: kein Messhaken installiert\n"),
        }

        // (a2) **Wer**, soweit sperrfrei feststellbar. Steht vor dem Stack, weil ein Name die
        //      erste Frage beantwortet, die jemand vor diesem Bericht hat.
        match RING3_KONTEXT_HOOK.load().and_then(|h| h(c)) {
            Some(t) => console::emit_fmt(format_args!(
                "  letzter Ring-3-Kontext auf diesem Kern: Thread {t:#x} (sperrfrei aus dem \
                 FP-Besitzregister -- ein #DF darf nichts sperren)\n"
            )),
            None => console::emit_raw(
                "  letzter Ring-3-Kontext auf diesem Kern: KEINER (nicht: unbekannt)\n",
            ),
        }

        // (b) Der Stack, der vermutlich uebergelaufen ist: der EL0-Kernel-Stack, den dieser Kern
        //     zuletzt fuer Ring 3 scharf gemacht hat.
        let rsp0 = super::gdt::tss_rsp0(c);
        match super::gdt::kstack_basis_von_rsp0(rsp0) {
            Some((kb, kl)) => {
                console::emit_fmt(format_args!(
                    "  betroffener EL1-Stack (aus TSS.rsp0={:#x}): [{:#x},{:#x}) {} B\n",
                    rsp0,
                    kb,
                    kb + kl,
                    kl,
                ));
                match wasserstand(kb, kl) {
                    Some(b) => console::emit_fmt(format_args!(
                        "  betroffener EL1-Stack Wasserstand: {} von {} B benutzt ({} B frei){}\n",
                        b,
                        kl,
                        kl - b.min(kl),
                        if b >= kl { "  <- UEBERGELAUFEN" } else { "" },
                    )),
                    None => console::emit_raw(
                        "  betroffener EL1-Stack Wasserstand: kein Messhaken installiert\n",
                    ),
                }
            }
            None => console::emit_fmt(format_args!(
                "  betroffener EL1-Stack: aus TSS.rsp0={rsp0:#x} nicht bestimmbar \
                 (Kstack-Geometrie nicht gemeldet)\n"
            )),
        }
    }
    console::emit_raw(
        "  thread-slot: nicht gedruckt -- er steht in SCHEDS/KSTACKS und braucht eine Sperre.\n  \
         Ein Spinlock im #DF-Pfad kann genau die Stille erzeugen, gegen die diese Meldung gebaut\n  \
         ist; identifiziert wird der Thread deshalb ueber SEINEN STACK (Zeile darueber).\n",
    );
    // **Der ENDSTAND, und er ist der Messwert, aus dem `IST_STACK_BYTES` hergeleitet ist.**
    //
    // Die Zeile weiter oben liest den Wasserstand MITTEN im Bericht — sie kann die Tiefe der
    // danach folgenden `emit_fmt`-Aufrufe strukturell nicht kennen und wäre als Grundlage für die
    // Stackgrösse also systematisch zu klein. Gemessen wird deshalb noch einmal ganz am Schluss,
    // wenn der tiefste Rahmen dieses Pfads sicher gelaufen ist.
    if let Some(c) = kern {
        let (ib, il) = super::gdt::ist_region(c, super::gdt::ISTF_DF);
        if let Some(b) = wasserstand(ib, il) {
            console::emit_fmt(format_args!(
                "  IST[#DF] ENDSTAND: {b} von {il} B benutzt -- DAS ist die Zahl, gegen die \
                 IST_STACK_BYTES bemessen ist\n"
            ));
        }
    }
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
        2 => "NMI",
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
        18 => "#MC machine check",
        19 => "#XM simd fp",
        _ => "(sonstiger Vektor)",
    }
}
