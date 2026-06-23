//! CPU-Register und Low-Level-Instruktionen (aarch64).
//!
//! Alle Funktionen kapseln genau einen Registerzugriff bzw. eine
//! Hint-Instruktion — eine ausdrücklich erlaubte `unsafe`-Domäne.

use core::arch::asm;

/// Aktuelles Exception-Level (0–3) aus `CurrentEL`.
pub fn current_el() -> u8 {
    let el: u64;
    // SAFETY: `CurrentEL` ist read-only und ohne Seiteneffekte.
    unsafe {
        asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags));
    }
    ((el >> 2) & 0b11) as u8
}

/// Logische Kern-ID = MPIDR_EL1 Aff0 (auf QEMU `virt` 0..7, ein Cluster).
pub fn core_id() -> usize {
    let mpidr: u64;
    // SAFETY: `MPIDR_EL1` ist read-only und ohne Seiteneffekte.
    unsafe {
        asm!("mrs {}, MPIDR_EL1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    (mpidr & 0xff) as usize
}

/// Vollständige MPIDR-Affinität (Aff0..Aff3), wie sie PSCI als Ziel-CPU erwartet.
pub fn mpidr_affinity() -> u64 {
    let mpidr: u64;
    // SAFETY: read-only Register.
    unsafe {
        asm!("mrs {}, MPIDR_EL1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    // Aff3[39:32] | Aff2[23:16] | Aff1[15:8] | Aff0[7:0]
    mpidr & 0xff_00ff_ffff
}

/// IRQs am aktuellen Kern freigeben (DAIF.I löschen).
pub fn local_irq_enable() {
    // SAFETY: erlaubt asynchrone IRQ-Auslieferung; bewusste Low-Level-Operation.
    unsafe {
        asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
    }
}

/// IRQs am aktuellen Kern maskieren (DAIF.I setzen).
pub fn local_irq_disable() {
    // SAFETY: maskiert IRQs; bewusste Low-Level-Operation.
    unsafe {
        asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }
}

/// Instruction Synchronization Barrier.
pub fn isb() {
    // SAFETY: reine Barriere ohne Speichereffekt.
    unsafe { asm!("isb", options(nomem, nostack, preserves_flags)) }
}

/// Data Synchronization Barrier (full system).
pub fn dsb_sy() {
    // SAFETY: reine Barriere.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) }
}

/// Auf ein Ereignis warten (Low-Power).
pub fn wfi() {
    // SAFETY: Hint-Instruktion ohne Speichereffekt.
    unsafe { asm!("wfi", options(nomem, nostack, preserves_flags)) }
}

/// Kern für immer anhalten.
pub fn halt() -> ! {
    loop {
        // SAFETY: Hint-Instruktion.
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) }
    }
}
