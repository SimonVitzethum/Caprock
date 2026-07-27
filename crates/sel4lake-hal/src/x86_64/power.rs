//! Power-/SMP-Schnittstelle (x86_64) — API-gleich zu PSCI auf aarch64.

/// Erfolgsstatus (aarch64: `PSCI_SUCCESS`).
pub const SUCCESS: i64 = 0;
/// „Nicht unterstützt" (aarch64: `PSCI_NOT_SUPPORTED`).
pub const NOT_SUPPORTED: i64 = -1;

/// System abschalten.
///
/// QEMU (`pc`/`q35`) hört auf den ACPI-Power-Management-Port: ein Wort `0x2000` auf `0x604`
/// löst „soft off" aus. Auf echter Hardware wäre der Port aus der ACPI-FADT zu lesen; für
/// den QEMU-Testlauf ist der feste Port die Entsprechung zu PSCI `SYSTEM_OFF`.
pub fn system_off() -> ! {
    // SAFETY: Port-I/O auf den ACPI-PM1a-Control-Port von QEMU; ein Schreibzugriff schaltet ab.
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") 0x604u16, in("ax") 0x2000u16,
                         options(nomem, nostack, preserves_flags));
        // Ältere QEMU-Versionen (isa-debug-exit / Bochs): 0xB004.
        core::arch::asm!("out dx, ax", in("dx") 0xB004u16, in("ax") 0x2000u16,
                         options(nomem, nostack, preserves_flags));
    }
    super::cpu::halt()
}

/// Einen weiteren Kern starten (aarch64: PSCI `CPU_ON`).
///
/// Auf x86 ist das die INIT-SIPI-SIPI-Sequenz über das LAPIC-ICR **plus** ein
/// 16-bit-Realmode-Trampolin unter 1 MiB, das den Kern in den Long Mode bringt. Der
/// Bring-up der Sekundärkerne ist noch nicht portiert — der Kernel läuft auf x86 derzeit
/// einkernig (s. `todo.md`).
pub fn cpu_on(_target: u64, _entry_point: u64, _context_id: u64) -> i64 {
    NOT_SUPPORTED
}
