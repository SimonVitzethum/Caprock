//! x86_64 `syscall`/`sysret` + Ring-3-Eintritt (Branch arch/x86_64, Stufe 3a).
//!
//! Demonstriert die User/Kernel-Privilegtrennung: `enter_ring3` springt per `iretq` mit den
//! User-Segmenten (RPL 3) + IF=0 nach `user_entry` (in `.user_text`, U/S=1). Dort führt Ring-3-Code
//! `syscall` aus → der CPU lädt CS/SS aus STAR + RIP aus LSTAR (`syscall_entry`), wir wechseln auf
//! einen Kernel-Stack, rufen [`rust_syscall`] und kehren per `sysretq` nach Ring 3 zurück. Pendant
//! zu `sel4lake-hal::syscall` (aarch64 SVC/EL0). TSS/Interrupts-aus-Ring-3 folgen in Stufe 4.

use super::{emit_raw, halt, put_hex};
use core::arch::{asm, global_asm};
use core::ptr::addr_of;

const MSR_EFER: u32 = 0xC000_0080;
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;

/// Demo-Syscall-Nummer „Prozessende" (wie Linux `exit`).
const SYS_EXIT: u64 = 0x3C;

/// Ring-3-User-Stack (in `.user_data` -> U/S=1, RW, NX).
#[link_section = ".user_data"]
static mut USER_STACK: [u8; 4096] = [0; 4096];

global_asm!(
    r#"
/* ---- Ring-3-Code (.user_text, U/S=1, ausführbar): CS lesen (CPL-Beweis) + 2x syscall ---- */
.section .user_text, "ax"
.globl user_entry
user_entry:
    xor rdi, rdi
    mov di, cs                       /* rdi = CS-Selektor (User-Code|RPL3 = 0x23); low 2 Bits = CPL */
    syscall                          /* -> ring0 syscall_entry; sysret kehrt hierher zurück */
    mov rcx, 200000000               /* Spin -> der LAPIC-Timer (IF=1) präemptiert Ring 3 via TSS.RSP0 */
2:  dec rcx
    jnz 2b
    mov rdi, 0x3C                    /* SYS_EXIT */
    syscall                          /* -> Handler meldet Präemption + system_off */
1:  jmp 1b

/* ---- Syscall-Entrypoint (LSTAR): syscall liefert user RIP in rcx, RFLAGS in r11 ---- */
.section .text
.globl syscall_entry
syscall_entry:
    mov [rip + user_rsp_slot], rsp   /* User-RSP retten (syscall wechselt den Stack NICHT) */
    lea rsp, [rip + syscall_kstack_top]
    push rcx                         /* user RIP (für sysret) */
    push r11                         /* user RFLAGS (für sysret) */
    call rust_syscall                /* rdi = Argument (von syscall unverändert) */
    pop r11
    pop rcx
    mov rsp, [rip + user_rsp_slot]
    sysretq                          /* -> Ring 3: RIP=rcx, RFLAGS=r11, CS/SS aus STAR */

/* ---- Kernel-Syscall-Stack + User-RSP-Slot (.bss, supervisor, vom Trampolin genullt) ----
   NUR SINGLE-CORE: ein globaler Stack/Slot genügt, weil nur der Primärkern läuft und der
   Handler mit IF=0 (SFMASK) nicht verschachtelt. SMP braucht per-CPU-Slots via
   swapgs/KERNEL_GS_BASE (Stufe 4+), sonst korrumpieren sich Kerne gegenseitig den RSP. */
.section .bss
.align 16
syscall_kstack:
    .skip 4096
syscall_kstack_top:
user_rsp_slot:
    .skip 8
.section .text
"#
);

extern "C" {
    fn syscall_entry();
    fn user_entry();
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack));
    ((hi as u64) << 32) | lo as u64
}
unsafe fn wrmsr(msr: u32, val: u64) {
    asm!("wrmsr", in("ecx") msr, in("eax") val as u32, in("edx") (val >> 32) as u32,
         options(nomem, nostack));
}

/// `syscall`/`sysret` aktivieren: EFER.SCE, STAR (Selektor-Basen), LSTAR (Entry), SFMASK (IF aus).
pub fn init() {
    // SAFETY: MSR-Setup für die Syscall-Schnittstelle (Firmware-/CPU-Konfiguration).
    unsafe {
        wrmsr(MSR_EFER, rdmsr(MSR_EFER) | 1); // SCE (System Call Enable)
        // STAR[47:32] = syscall-Basis 0x08 (CS=0x08, SS=0x10); STAR[63:48] = sysret-Basis 0x10
        // (CS=0x10+16=0x20|3, SS=0x10+8=0x18|3). Passt zur GDT (gdt.rs).
        wrmsr(MSR_STAR, (0x10u64 << 48) | (0x08u64 << 32));
        wrmsr(MSR_LSTAR, syscall_entry as u64);
        wrmsr(MSR_SFMASK, 0x200); // IF beim Syscall-Eintritt löschen (Handler läuft mit IRQs aus)
    }
}

/// Vom Ring-3-Code via `syscall` gerufen (Argument in rdi). Demo: Argument dumpen; bei `SYS_EXIT`
/// den Round-Trip als bestanden melden und anhalten.
#[no_mangle]
extern "C" fn rust_syscall(arg: u64) {
    emit_raw("syscall : ring3 -> ring0, arg=");
    put_hex(arg);
    if arg & 3 == 3 {
        emit_raw("  (CPL=3 bestaetigt: Aufruf kam aus Ring 3)");
    }
    emit_raw("\n");
    if arg == SYS_EXIT {
        emit_raw("syscall : SYS_EXIT -> ring3-Round-Trip OK (iretq->ring3 -> syscall -> sysret -> syscall) -> ALL PASS\n");
        emit_raw("x86_64 Stufe 0-3a: ALL PASS (+ ring3 + syscall/sysret)\n");
        halt();
    }
}

/// Nach Ring 3 wechseln und `user_entry` ausführen (per `iretq`, IF=0). Kehrt nicht zurück; die Demo
/// endet im `SYS_EXIT`-Zweig von [`rust_syscall`].
pub fn enter_ring3() -> ! {
    let ustack_top = unsafe { addr_of!(USER_STACK) as u64 + USER_STACK.len() as u64 };
    let entry = user_entry as u64;
    // SAFETY: kontrollierter Privilegwechsel via iretq mit gültigen User-Segmenten (RPL3, GDT) +
    // U/S-gemappten Code-/Stack-Seiten (.user_text/.user_data, s. paging.rs). IF=0 -> keine IRQs.
    unsafe {
        asm!(
            "push {ss}",      // SS  = User-Daten | RPL3
            "push {rsp}",     // RSP = User-Stack
            "push 0x2",       // RFLAGS (nur reservedes Bit 1; IF=0)
            "push {cs}",      // CS  = User-Code | RPL3
            "push {rip}",     // RIP = user_entry
            "iretq",
            ss = in(reg) super::gdt::UDATA3 as u64,
            rsp = in(reg) ustack_top,
            cs = in(reg) super::gdt::UCODE3 as u64,
            rip = in(reg) entry,
            options(noreturn),
        );
    }
}
