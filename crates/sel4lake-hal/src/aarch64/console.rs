//! Debug-Konsole über die PL011-UART (QEMU `virt`).
//!
//! Reine *Kernel-Debug*-Einrichtung (vgl. seL4-Debug-`printf`). Produktives
//! Logging ist ein Userland-Dienst (ADR 0006).
//!
//! Zwei Pfade:
//! * [`emit_raw`] — lock- und atomic-frei; für **vor** der MMU-Aktivierung und
//!   für den Panic-Handler (siehe ADR 0002: Atomics brauchen die MMU).
//! * [`_print`] (über `println!`) — durch einen Spinlock SMP-serialisiert; erst
//!   nach MMU-Aktivierung benutzen.

use core::fmt::{self, Write};
use sel4lake_sync::SpinLock;

/// MMIO-Basis der PL011-UART auf QEMU `virt`.
pub const PL011_BASE: usize = 0x0900_0000;
const UART_FR: usize = 0x18;
const UART_FR_TXFF: u32 = 1 << 5;

/// Zustandsloser UART-Sender (alle Register werden direkt per MMIO bedient).
struct Pl011;

impl Pl011 {
    fn put_byte(b: u8) {
        // SAFETY: `PL011_BASE` ist die feste MMIO-Adresse der QEMU-`virt`-UART;
        // volatile Zugriffe auf Geräteregister aliasen keinen Rust-Speicher.
        // Geräteregisterzugriff ist eine erlaubte `unsafe`-Domäne.
        unsafe {
            let fr = (PL011_BASE + UART_FR) as *const u32;
            while core::ptr::read_volatile(fr) & UART_FR_TXFF != 0 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(PL011_BASE as *mut u32, b as u32);
        }
    }

    fn write_str_raw(s: &str) {
        for b in s.bytes() {
            if b == b'\n' {
                Self::put_byte(b'\r');
            }
            Self::put_byte(b);
        }
    }
}

impl Write for Pl011 {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        Self::write_str_raw(s);
        Ok(())
    }
}

/// Lock, der den UART-Zugriff zwischen Kernen serialisiert. Der UART selbst ist
/// zustandslos; der Lock schützt nur die Ausgabe-Atomarität.
static CONSOLE: SpinLock<()> = SpinLock::new(());

/// Lock-freie Direktausgabe. Nur für Pre-MMU-Bring-up und den Panic-Handler.
pub fn emit_raw(s: &str) {
    Pl011::write_str_raw(s);
}

/// Lock-freie formatierte Ausgabe. Für den Panic-Handler (der den Lock evtl.
/// gerade selbst hält) und für Pre-MMU-Diagnose.
pub fn emit_fmt(args: fmt::Arguments) {
    let _ = Pl011.write_fmt(args);
}

/// Backing für [`crate::print!`] / [`crate::println!`] (SMP-serialisiert).
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let _guard = CONSOLE.lock();
    let _ = Pl011.write_fmt(args);
}
