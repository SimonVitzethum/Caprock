//! `intruder-t` — adversarialer TrustedSAS-Testdienst (ext-27 T3, ADR 0012).
//!
//! **Staerkste Isolations-Aussage.** Auch ein als **TrustedSAS** geladener Dienst laeuft (wie alle
//! geladenen Prozesse) **EL0-isoliert** und kann Kernel-/Fremdspeicher **nicht** lesen — die
//! Trust-Stufe befreit **nicht** von der hardware-erzwungenen Adressraumtrennung. Der Dienst meldet
//! ein **PRE**-Badge (Slot 0) und liest dann Kernel-RAM aus EL0 → **Translation-Fault** → der
//! Kernel terminiert ihn und laeuft weiter.

#![no_std]
#![no_main]

use libcaprock::{exit, signal};

/// PRE-Badge "INRT" (muss zum Kernel-Test `INTRT_PRE` passen).
pub const PRE_BADGE: u64 = 0x494E_5254;

const REPORT: u64 = 0; // Slot 0: Report-Notification (WRITE-only, Badge PRE_BADGE)

/// Kernel-RAM (Beginn des physischen RAM, Kernel-Image). Nicht in der isolierten VSpace gemappt.
const KERNEL_RAM: usize = 0x4000_0000;

#[no_mangle]
pub extern "C" fn _start(_arg: usize) -> ! {
    signal(REPORT, PRE_BADGE); // lief = eigene VSpace ok
    // SAFETY: BEWUSST illegaler Zugriff (Testzweck, Isolationsnachweis). Kernel-RAM ist nicht in der
    // isolierten VSpace dieses TrustedSAS-EL0-Dienstes gemappt -> Translation-Fault -> der Kernel
    // terminiert diesen Thread; kehrt nie zurueck.
    let _v = unsafe { core::ptr::read_volatile(KERNEL_RAM as *const u64) };
    exit(); // unerreichbar
}
