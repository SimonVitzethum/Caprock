//! Debug-Konsole über die 16550-UART (COM1, Port-I/O) — x86_64.
//!
//! API-gleich zur aarch64-Konsole (PL011): [`emit_raw`] lock-/atomic-frei für den frühen
//! Bring-up und den Panic-Handler, [`_print`] SMP-serialisiert für `println!`.
//!
//! Reine *Kernel-Debug*-Einrichtung (vgl. seL4-Debug-`printf`); produktives Logging ist ein
//! Userland-Dienst (ADR 0006).

use super::cpu::{inb, outb};
use crate::konsole::{self, Schreibordnung};
use core::fmt::{self, Write};

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

    /// Nimmt der Port jetzt ein Byte an? (`LSR.THRE`.)
    ///
    /// **Getrennt von [`Self::put_byte`], weil das Warten OHNE Maske laufen muss** (C9b): steht
    /// der Sende-FIFO voll oder haengt das Backend, dauert es Millisekunden -- unter einer Sperre
    /// waere das genau der Praemptionsverlust, um den es geht.
    fn bereit() -> bool {
        // SAFETY: Port-I/O auf die feste COM1-Registerdatei (s. `put_byte`).
        unsafe { inb(COM1 + REG_LSR) & LSR_THRE != 0 }
    }

    /// Die CR/LF-Regel steht arch-neutral in [`crate::konsole`] — hier nur der Treiber.
    fn write_str_raw(s: &str) {
        konsole::roh(TREIBER, s);
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

/// Die Schreibordnung dieser Konsole (C9b): Besitzrecht ueber die Nachricht, Portsperre je
/// Block. Warum so und nicht als **eine** Haltung ueber das ganze `write_fmt` — mit den Zahlen,
/// die den naheliegenden Weg ausgeschlossen haben — steht in [`crate::konsole`].
/// Der Treiber als Wertepaar: warten und schreiben sind getrennt (s. `konsole::Treiber`).
static TREIBER: konsole::Treiber = konsole::Treiber { bereit: Uart16550::bereit, senden: Uart16550::put_byte };

static ORDNUNG: Schreibordnung = Schreibordnung::neu();

/// Backing für [`crate::print!`] / [`crate::println!`] (SMP-serialisiert).
///
/// **Die Kernnummer kommt aus `TR` und nicht aus `cpuid`**: `cpuid` ist unter KVM ein
/// bedingungsloser VM-Exit (Fallenliste; in `cycles()` hat er einmal 3556 statt 51 Zyklen
/// gekostet). Vor dem ersten `ltr` gibt es sie noch nicht — dann laeuft aber auch nur der
/// Startkern, und `0` ist die Wahrheit und keine Notloesung.
#[doc(hidden)]
#[track_caller]
pub fn _print(args: fmt::Arguments) {
    let kern = super::gdt::core_from_tr().unwrap_or(0);
    ORDNUNG.drucken(kern, super::cpu::irqs_freigegeben(), TREIBER, args);
}

/// Was die Schreibordnung gesehen hat — fuer die `konsole`-Berichtszeile.
pub fn schreibstand() -> konsole::Stand {
    ORDNUNG.stand()
}
