//! Debug-Konsole über die 16550-UART (COM1, Port-I/O) — x86_64.
//!
//! API-gleich zur aarch64-Konsole (PL011): [`emit_raw`] lock-/atomic-frei für den frühen
//! Bring-up und den Panic-Handler, [`_print`] SMP-serialisiert für `println!`.
//!
//! Reine *Kernel-Debug*-Einrichtung (vgl. seL4-Debug-`printf`); produktives Logging ist ein
//! Userland-Dienst (ADR 0006).

use super::cpu::{inb, outb};
use core::fmt::{self, Write};
use sel4lake_sync::SpinLock;

/// I/O-Port-Basis der ersten seriellen Schnittstelle (COM1).
pub const COM1: u16 = 0x3F8;
const REG_DATA: u16 = 0;
const REG_IER: u16 = 1;
const REG_FCR: u16 = 2;
const REG_LCR: u16 = 3;
const REG_MCR: u16 = 4;
const REG_LSR: u16 = 5;
/// Line Status: Transmitter Holding Register leer.
const LSR_THRE: u8 = 1 << 5;

/// Zustandsloser UART-Sender (alle Register direkt per Port-I/O).
struct Uart16550;

impl Uart16550 {
    fn put_byte(b: u8) {
        // SAFETY: COM1 ist die feste, architektonisch reservierte I/O-Port-Adresse der
        // seriellen Schnittstelle; Port-I/O aliast keinen Rust-Speicher. Geräteregister-
        // zugriff ist eine erlaubte `unsafe`-Domäne.
        unsafe {
            while inb(COM1 + REG_LSR) & LSR_THRE == 0 {
                core::hint::spin_loop();
            }
            outb(COM1 + REG_DATA, b);
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

impl Write for Uart16550 {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        Self::write_str_raw(s);
        Ok(())
    }
}

/// UART initialisieren: 115200 8N1, FIFOs an, Interrupts aus (wir pollen `LSR.THRE`).
pub fn init() {
    // SAFETY: Port-I/O auf die feste COM1-Registerdatei (s. `put_byte`).
    unsafe {
        outb(COM1 + REG_IER, 0x00); // keine UART-Interrupts
        outb(COM1 + REG_LCR, 0x80); // DLAB: Teiler-Latch sichtbar
        outb(COM1 + REG_DATA, 0x01); // Teiler = 1 -> 115200 Baud
        outb(COM1 + REG_IER, 0x00); // Teiler-High = 0
        outb(COM1 + REG_LCR, 0x03); // 8N1, DLAB aus
        outb(COM1 + REG_FCR, 0xC7); // FIFOs an + leeren, 14-Byte-Trigger
        outb(COM1 + REG_MCR, 0x03); // DTR + RTS
    }
}

/// Lock-freie Direktausgabe. Für den frühen Bring-up und den Panic-Handler.
pub fn emit_raw(s: &str) {
    Uart16550::write_str_raw(s);
}

/// Lock-freie formatierte Ausgabe (Panic-Handler / frühe Diagnose).
pub fn emit_fmt(args: fmt::Arguments) {
    let _ = Uart16550.write_fmt(args);
}

/// Lock, der die Ausgabe zwischen Kernen serialisiert (der UART selbst ist zustandslos).
static CONSOLE: SpinLock<()> = SpinLock::new(());

/// Backing für [`crate::print!`] / [`crate::println!`] (SMP-serialisiert).
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let _guard = CONSOLE.lock();
    let _ = Uart16550.write_fmt(args);
}
