#![no_std]
//! `sel4lake-hal` — Hardware-Abstraktion für aarch64 / QEMU `virt`.
//!
//! Kapselt die architekturabhängigen Low-Level-Routinen (CPU-Register, MMU,
//! Exceptions, GIC, Timer, PSCI). Sämtliches `unsafe` hier liegt in den von der
//! Projektregel erlaubten Domänen und ist einzeln begründet.

pub mod console;
pub mod cpu;
pub mod exception;
pub mod fp;
pub mod gic;
pub mod mmu;
pub mod psci;
pub mod syscall;
pub mod timer;

/// Formatierte Ausgabe auf der Debug-Konsole (gesperrt, SMP-sicher).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

/// Wie [`print!`], mit Zeilenumbruch.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
