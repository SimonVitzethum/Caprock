//! `intruder-h` — adversarialer HardwareLand-Testdienst (ext-27 T2, ADR 0012).
//!
//! **Speicher-Isolation aus einem HardwareLand-Backend.** Beweist, dass die hardware-erzwungene
//! Adressraumtrennung **domaenen-unabhaengig** ist: auch ein „Hardware"-Backend laeuft EL0-isoliert
//! und kann Kernel-/Fremdspeicher **nicht** lesen — der Zugriff faultet, der Kernel terminiert das
//! Backend und laeuft weiter. Meldet ein **PRE**-Badge ueber den eigenen Kanal (Slot 0), dann der
//! fatale Zugriff.

#![no_std]
#![no_main]

use libsel4lake::{exit, signal};

/// PRE-Badge "INRH" (muss zum Kernel-Test `INTRH_PRE` passen).
pub const PRE_BADGE: u64 = 0x494E_5248;

const CHAN: u64 = 0; // Slot 0: eigene Kanal-Notification (WRITE-only, Badge PRE_BADGE)

/// Kernel-RAM (Beginn des physischen RAM, Kernel-Image). Nicht in der isolierten VSpace gemappt.
const KERNEL_RAM: usize = 0x4000_0000;

#[no_mangle]
pub extern "C" fn _start(_arg: usize) -> ! {
    signal(CHAN, PRE_BADGE); // ueber den eigenen Kanal melden (lief = eigene VSpace ok)
    // SAFETY: BEWUSST illegaler Zugriff (Testzweck, Isolationsnachweis). Kernel-RAM ist nicht in der
    // isolierten VSpace gemappt -> Translation-Fault -> der Kernel terminiert diesen Thread; kehrt
    // nie zurueck.
    let _v = unsafe { core::ptr::read_volatile(KERNEL_RAM as *const u64) };
    exit(); // unerreichbar
}
