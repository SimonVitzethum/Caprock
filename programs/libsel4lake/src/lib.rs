//! Minimal-Userland-SDK für extern geladene SEL4Lake-EL0-Programme (ext-26).
//!
//! Bietet die **Syscall-Stubs** (SVC-`#0`-ABI: `x0`=nr, `x1`=cap-Index, `x2..x5`=msg, `x6`=tag;
//! Rückgabe `x0`=result, `x1`=badge, `x2..x5`=reply, `x6`=tag) + einen **Panik-Handler**. Bewusst
//! ohne Abhängigkeit vom Kernel-Workspace (die ABI-Konstanten sind dupliziert) — ein extern
//! gebautes Programm hängt nur von diesem SDK ab.

#![no_std]

use core::arch::asm;

/// Syscall-Nummern (Spiegel von `sel4lake_abi::sys`).
pub mod sys {
    pub const YIELD: u64 = 0;
    pub const CALL: u64 = 1;
    pub const RECV: u64 = 2;
    pub const REPLY: u64 = 3;
    pub const PARK: u64 = 5;
    pub const EXIT: u64 = 6;
    pub const KILL: u64 = 7;
    pub const SIGNAL: u64 = 8;
    pub const WAIT: u64 = 9;
    pub const MAP: u64 = 10;
    pub const UNMAP: u64 = 11;
    pub const PDCTL: u64 = 12;
    pub const LOAD: u64 = 13;
}

/// Ergebniscodes (Register `x0` beim Austritt; Spiegel von `sel4lake_abi::result`).
pub mod result {
    pub const OK: u64 = 0;
    /// Kein gültiger Capability an der Stelle / falscher Objekttyp.
    pub const ERR_BADCAP: u64 = 1;
    /// Unbekannte Syscall-Nummer.
    pub const ERR_BADSYS: u64 = 2;
    /// Capability hat nicht die nötigen Rechte.
    pub const ERR_RIGHTS: u64 = 3;
    /// Aufrufer gehört zu keiner Protection Domain.
    pub const ERR_NOPD: u64 = 4;
    /// Antwort-seitiger Liveness-Fehler (Reply-Owner verschwunden).
    pub const ERR_SERVER_GONE: u64 = 5;
}

/// Sub-Operationen für [`sys::PDCTL`] (Register `x2`; Spiegel von `sel4lake_abi::pdctl`).
pub mod pdctl {
    pub const START: u64 = 0;
    pub const STOP: u64 = 1;
    pub const PAUSE: u64 = 2;
    pub const RESUME: u64 = 3;
    pub const ASSIGN_BUDGET: u64 = 4;
}

/// Syscall-Ergebnis (Register `x0..x6` nach `eret`).
#[derive(Clone, Copy)]
pub struct Ret {
    pub result: u64,
    pub badge: u64,
    pub msg: [u64; 4],
    pub tag: u64,
}

/// Roh-Syscall (`svc #0`). `cap` = lokaler Cap-Index der eigenen PD.
#[inline]
pub fn invoke(nr: u64, cap: u64, msg: [u64; 4], tag: u64) -> Ret {
    let (r0, r1, r2, r3, r4, r5, r6);
    // SAFETY: reiner Supervisor-Call; der Kernel restauriert beim `eret` alle Register aus dem
    // Frame (nur x0..x6 tragen das Ergebnis). Kein Speicher-/Stack-Effekt im User-Kontext.
    unsafe {
        asm!(
            "svc #0",
            inout("x0") nr => r0,
            inout("x1") cap => r1,
            inout("x2") msg[0] => r2,
            inout("x3") msg[1] => r3,
            inout("x4") msg[2] => r4,
            inout("x5") msg[3] => r5,
            inout("x6") tag => r6,
            options(nostack),
        );
    }
    Ret { result: r0, badge: r1, msg: [r2, r3, r4, r5], tag: r6 }
}

/// Notification (Cap `cap`) mit `badge` signalisieren (asynchron, nicht blockierend).
pub fn signal(cap: u64, badge: u64) {
    let _ = invoke(sys::SIGNAL, cap, [badge, 0, 0, 0], 0);
}
/// Auf eine Notification (Cap `cap`) warten; gibt das akkumulierte Badge zurück.
pub fn wait(cap: u64) -> u64 {
    invoke(sys::WAIT, cap, [0; 4], 0).badge
}
/// Synchroner Aufruf (Endpoint-Cap `cap`): senden + auf Antwort warten.
pub fn call(cap: u64, msg: [u64; 4]) -> Ret {
    invoke(sys::CALL, cap, msg, 0)
}
/// Auf einen Aufruf warten (Server, Endpoint-Cap `cap`).
pub fn recv(cap: u64) -> Ret {
    invoke(sys::RECV, cap, [0; 4], 0)
}
/// Den letzten Aufrufer beantworten (Endpoint-Cap `cap`).
pub fn reply(cap: u64, msg: [u64; 4]) {
    let _ = invoke(sys::REPLY, cap, msg, 0);
}
/// Frame (Memory-Cap `cap`) in die eigene VSpace mappen; gibt den Ergebniscode zurück.
pub fn map(cap: u64) -> u64 {
    invoke(sys::MAP, cap, [0; 4], 0).result
}
/// Frame (Memory-Cap `cap`) wieder entfernen; gibt den Ergebniscode zurück.
pub fn unmap(cap: u64) -> u64 {
    invoke(sys::UNMAP, cap, [0; 4], 0).result
}
/// Ziel-PD steuern (PdControl-Cap `cap`, Sub-Op `subop`); gibt den Ergebniscode zurück.
pub fn pdctl(cap: u64, subop: u64) -> u64 {
    invoke(sys::PDCTL, cap, [subop, 0, 0, 0], 0).result
}
/// Programm `index` laden (Loader-Cap `cap`, delegierter Cap-Slot `delegate`, `u64::MAX`=keiner);
/// gibt den Ergebniscode zurück.
pub fn load(cap: u64, index: u64, delegate: u64) -> u64 {
    invoke(sys::LOAD, cap, [index, delegate, 0, 0], 0).result
}
/// Thread (Tcb-Cap `cap`) beenden; gibt den Ergebniscode zurück.
pub fn kill(cap: u64) -> u64 {
    invoke(sys::KILL, cap, [0; 4], 0).result
}
/// Freiwilliger Zeitscheibenabtritt.
pub fn yield_now() {
    let _ = invoke(sys::YIELD, 0, [0; 4], 0);
}
/// Sich selbst dauerhaft blockieren (kein Cap nötig).
pub fn park() -> ! {
    loop {
        let _ = invoke(sys::PARK, 0, [0; 4], 0);
    }
}
/// Sich selbst beenden (Stack/TCB werden zurückgewonnen).
pub fn exit() -> ! {
    let _ = invoke(sys::EXIT, 0, [0; 4], 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Definiert den ELF-Entry-Point `_start` eines extern geladenen Programms.
///
/// Die `#[no_mangle]`-Glue (in aktuellem Rust ein *unsafe* Attribut) gehoert zur auditierten
/// SDK-Schicht (= Allowlist), damit das eigentliche Programm `#![forbid(unsafe_code)]` bleiben und
/// damit zertifiziert werden kann. `$main` ist eine **sichere** `fn(usize) -> !` des Programms (x0 =
/// Boot-Arg). Verwendung:  `libsel4lake::entry!(run);`  mit  `fn run(_arg: usize) -> ! { ... }`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub extern "C" fn _start(arg: usize) -> ! {
            $main(arg)
        }
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Kein Heap/Konsole im EL0-Programm verfügbar -> still parken; der Kernel beobachtet, dass
    // kein erwartetes Signal kam (bzw. ein Fault terminiert den Thread regulär).
    park()
}
