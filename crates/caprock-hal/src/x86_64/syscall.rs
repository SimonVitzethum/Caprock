//! Syscall-Aufruf aus **Kernel-Threads** (x86_64) — API-gleich zu `svc #0` auf aarch64.
//!
//! Kernel-Threads laufen bereits in Ring 0, ein `syscall` von dort wäre umständlich (kein
//! Stackwechsel, RCX/R11 zerstört). Ein **Software-Interrupt** (`int 0x80`) legt dagegen
//! genau denselben Trap-Frame an wie jeder andere Interrupt — der Dispatch bleibt einheitlich.
//! Die Registerabbildung der ABI steht in `exception::ABI_TO_GPR`.

/// Ergebnis eines Syscalls: Ergebniscode + Badge + Nachrichtenwörter.
#[derive(Clone, Copy, Debug)]
pub struct Ret {
    pub result: u64,
    pub badge: u64,
    pub msg: [u64; 4],
}

/// Syscall auslösen (`x0`=Nummer, `x1`=Cap-Index, `x2..x5`=Nachricht, `x6`=Tag).
pub fn invoke(nr: u64, ep: u64, msg: [u64; 4], tag: u64) -> Ret {
    let (mut r0, mut r1, mut r2, mut r3, mut r4, mut r5);
    // SAFETY: `int 0x80` ist der registrierte Syscall-Vektor (IDT-Gate mit DPL 3). Der Kernel
    // liest/schreibt ausschließlich die hier angegebenen Register des Trap-Frames.
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inout("rax") nr => r0,
            inout("rdi") ep => r1,
            inout("rsi") msg[0] => r2,
            inout("rdx") msg[1] => r3,
            inout("r10") msg[2] => r4,
            inout("r8")  msg[3] => r5,
            in("r9") tag,
            clobber_abi("sysv64"),
        );
    }
    Ret {
        result: r0,
        badge: r1,
        msg: [r2, r3, r4, r5],
    }
}
