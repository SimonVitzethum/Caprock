//! `hello` — minimales extern geladenes EL0-UserLand-Programm (ext-26, L1).
//!
//! Beweist, dass der generische Binary-Loader extern gebauten Code lädt + ausführt: signalisiert
//! die vom Loader endowte Notification in **Cap-Slot 0** mit einem charakteristischen Badge, dann
//! Selbst-Park. Der Kernel wartet auf genau dieses Badge und bestätigt damit die Ausführung.

#![no_std]
#![no_main]

/// Charakteristisches Badge ("HELO"), das der Kernel-Test erwartet.
pub const HELLO_BADGE: u64 = 0x4845_4C4F;

/// Cap-Slot der vom Loader endowten Notification (Konvention für `hello`).
const NTFN_SLOT: u64 = 0;

/// Entry-Point (vom Linker als ELF-Entry gesetzt; der Kernel startet hier mit gesetztem SP).
/// `_arg` = x0 (Boot-Info-Zeiger; in L1 ungenutzt).
#[no_mangle]
pub extern "C" fn _start(_arg: usize) -> ! {
    libsel4lake::signal(NTFN_SLOT, HELLO_BADGE);
    libsel4lake::park();
}
