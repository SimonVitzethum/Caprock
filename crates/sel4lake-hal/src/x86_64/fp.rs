//! Lazy-FP/SIMD-Kontextverwaltung (x86_64) — API-gleich zur aarch64-Fassung.
//!
//! Der Microkernel selbst ist **soft-float** (Target ohne SSE): kein Kernel-Code berührt die
//! FP/SIMD-Register. Damit gehören sie ausschließlich den User-Threads und können **lazy**
//! verwaltet werden:
//!
//! * `CR0.TS` („Task Switched") lässt den **ersten** FP/SIMD-Zugriff eines Nicht-Owners als
//!   `#NM` (Vektor 7) trappen — das exakte Gegenstück zu `CPACR_EL1.FPEN` auf ARM.
//! * Pro Kern besitzt höchstens ein Thread die Register; erst der Trap löst den Owner-Wechsel
//!   aus (alte sichern, neue laden).
//!
//! Gesichert wird mit `FXSAVE`/`FXRSTOR` (512 Byte, 16-Byte-ausgerichtet) — das deckt x87,
//! MMX und SSE ab. AVX-Zustand (`XSAVE`) käme hinzu, sobald der Kernel AVX für Userland
//! freigibt; solange `CR4.OSXSAVE` aus ist, existiert er architektonisch nicht.

use core::arch::asm;

/// Gesicherter FP/SIMD-Kontext eines Threads (FXSAVE-Bereich).
#[repr(C, align(16))]
pub struct FpState {
    area: [u8; 512],
}

impl FpState {
    /// Frischer (genullter) FP-Kontext — Startzustand eines neuen Threads.
    pub const fn new() -> Self {
        FpState { area: [0; 512] }
    }
}

impl Default for FpState {
    fn default() -> Self {
        Self::new()
    }
}

/// Aktuelle FP/SIMD-Register in `state` sichern.
pub fn save(state: &mut FpState) {
    // SAFETY: `state` ist eine gültige, 16-Byte-ausgerichtete 512-Byte-Region (Typinvariante);
    // `fxsave64` schreibt genau diese. Registerzugriff = erlaubte Low-Level-Domäne.
    unsafe { asm!("fxsave64 [{}]", in(reg) state.area.as_mut_ptr(), options(nostack, preserves_flags)) };
}

/// FP/SIMD-Register aus `state` wiederherstellen.
pub fn restore(state: &FpState) {
    // SAFETY: wie `save`; `fxrstor64` liest genau diese 512 Byte. Ein frisch genullter Bereich
    // ist ein gültiger FXSAVE-Zustand (alle Register 0, FCW/MXCSR 0 -> von der CPU akzeptiert,
    // da wir MXCSR-Bits nicht setzen, die #GP auslösen).
    unsafe { asm!("fxrstor64 [{}]", in(reg) state.area.as_ptr(), options(readonly, nostack, preserves_flags)) };
}

/// FP-Zugriff aus dem User-Modus trappen lassen (`true`) oder freigeben (`false`).
///
/// `CR0.TS` gilt für **jede** Privilegstufe — anders als `CPACR_EL1.FPEN=0b01` auf ARM, das
/// gezielt nur EL0 trappt. Das ist hier unkritisch, weil der Kernel soft-float ist und FP
/// nie anfasst; träfe ihn der Trap doch, meldete ihn `handle_exception` als Kernel-Fault
/// statt ihn stillschweigend zu verschlucken.
pub fn set_el0_trap(trap: bool) {
    // SAFETY: Schreiben von CR0.TS (FP-Trap-Konfiguration) — erlaubte Domäne. Alle übrigen
    // CR0-Bits bleiben unverändert (read-modify-write).
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        const CR0_TS: u64 = 1 << 3;
        cr0 = if trap { cr0 | CR0_TS } else { cr0 & !CR0_TS };
        asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack, preserves_flags));
    }
}

/// `CR0.TS` löschen, ohne den Owner zu wechseln — nach einem `#NM` nötig, bevor `fxrstor64`
/// ausgeführt werden darf (sonst trappt der Restore selbst erneut).
pub fn clear_task_switched() {
    // SAFETY: `clts` löscht genau CR0.TS; Ring-0-Instruktion, erlaubte Domäne.
    unsafe { asm!("clts", options(nomem, nostack, preserves_flags)) };
}
