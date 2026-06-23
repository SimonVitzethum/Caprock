//! Syscall-Eintritt (Aufrufer-/Thread-Seite): `SVC #0` mit Register-Marshalling
//! gemäß `sel4lake-abi`. Der Trap landet in `exception::handle_exception`, das
//! den registrierten Syscall-Hook aufruft. Inline-Assembler für die
//! Syscall-Schranke ist eine erlaubte Low-Level-Domäne.

use core::arch::asm;

/// Rückgabe eines Syscalls (Register beim Austritt).
pub struct Ret {
    pub result: u64,
    pub badge: u64,
    pub msg: [u64; 4],
    pub tag: u64,
}

/// Einen Syscall ausführen (blockiert ggf., bis der Kernel den Thread fortsetzt).
pub fn invoke(nr: u64, ep: u64, msg: [u64; 4], tag: u64) -> Ret {
    let (r0, r1, r2, r3, r4, r5, r6);
    // SAFETY: `svc #0` wechselt nach EL1 in den Trap-Handler, der die Register
    // gemäß ABI als Argumente liest und die Rückgaben hineinschreibt; x0..x6
    // sind daher inout, übrige Register bleiben erhalten (voller Frame-Save).
    unsafe {
        asm!(
            "svc #0",
            inout("x0") nr => r0,
            inout("x1") ep => r1,
            inout("x2") msg[0] => r2,
            inout("x3") msg[1] => r3,
            inout("x4") msg[2] => r4,
            inout("x5") msg[3] => r5,
            inout("x6") tag => r6,
        );
    }
    Ret {
        result: r0,
        badge: r1,
        msg: [r2, r3, r4, r5],
        tag: r6,
    }
}
