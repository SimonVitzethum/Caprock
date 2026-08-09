//! **LAZY** FP/SIMD-Kontextverwaltung (aarch64) — und das ist die **andere Hälfte einer
//! begründeten Divergenz**, nicht der Rest eines halben Umbaus.
//!
//! Auf x86_64 ist die Politik seit dem 2026-08-09 **eager**, weil `CR0.TS` für **jede**
//! Privilegstufe gilt und Lazy-FP dort CVE-2018-3665 (LazyFP) ist: spekulative Ausführung kann
//! die FP-Register des vorigen Besitzers lesen, bevor das `#NM` zugestellt ist.
//!
//! Hier gilt das nicht. `CPACR_EL1.FPEN = 0b01` trappt **präzise** und **nur an EL0**; eine
//! LazyFP-Entsprechung ist nicht veröffentlicht. Die Trap-Reichweiten sind verschieden, und die
//! Exponierung ist es auch — deshalb bleibt aarch64 lazy.
//!
//! **Wer das ändert, ändert beide Seiten.** Die vorige Fassung sah auf einer Architektur eager und
//! auf der anderen lazy aus, **ohne** dass irgendwo stand warum — und war damals tatsächlich ein
//! Versehen. Dieser Absatz existiert, damit der nächste Leser die Divergenz nicht wieder für eines
//! hält.
//!
//! Lazy-FP/SIMD-Kontextverwaltung (aarch64).
//!
//! Der Microkernel selbst ist **soft-float** (Target ohne NEON, soft-float-ABI):
//! kein EL1-Code berührt die FP/SIMD-Register. Damit gehören die FP-Register
//! ausschließlich den User-Threads (EL0) und können **lazy** verwaltet werden:
//!
//!   * `CPACR_EL1.FPEN = 0b01` trappt FP/SIMD **nur an EL0**, nie an EL1 (der
//!     Kernel nutzt ohnehin kein FP). Der ext-3-Hang — Kernel-NEON trappt sich
//!     selbst — ist damit ausgeschlossen (das brauchte die EL0/EL1-Trennung).
//!   * Pro Kern besitzt höchstens **ein** Thread die FP-Register (der „FP-Owner").
//!     Beim Kontextwechsel werden die Register *nicht* gesichert; stattdessen
//!     trappt der erste FP-Zugriff eines Nicht-Owners (EC 0x07) und löst den
//!     Owner-Wechsel aus (alte Register sichern, neue laden).
//!
//! Save/Restore steht in einem eigenen `global_asm!`-Block mit `.arch armv8-a`,
//! damit die q-Register-Instruktionen trotz `-neon`-Target assemblieren. Asm und
//! Registerzugriffe sind erlaubte `unsafe`-Domänen.

use core::arch::{asm, global_asm};

/// Gesicherter FP/SIMD-Kontext eines Threads: q0..q31 plus FPSR/FPCR.
#[repr(C, align(16))]
pub struct FpState {
    /// q0..q31 (volle 128 Bit je Register).  Offset 0..512
    pub q: [u128; 32],
    /// `FPSR`.  Offset 512
    pub fpsr: u64,
    /// `FPCR`.  Offset 520 -> 528
    pub fpcr: u64,
}

impl FpState {
    /// Frischer (genullter) FP-Kontext — Startzustand eines neuen Threads.
    pub const fn new() -> Self {
        FpState {
            q: [0; 32],
            fpsr: 0,
            fpcr: 0,
        }
    }
}

impl Default for FpState {
    fn default() -> Self {
        Self::new()
    }
}

global_asm!(
    r#"
.arch armv8-a
.section .text
.globl __fp_save
// __fp_save(x0 = *mut FpState): aktuelle FP/SIMD-Register in den Puffer sichern.
__fp_save:
    stp     q0,  q1,  [x0, #0]
    stp     q2,  q3,  [x0, #32]
    stp     q4,  q5,  [x0, #64]
    stp     q6,  q7,  [x0, #96]
    stp     q8,  q9,  [x0, #128]
    stp     q10, q11, [x0, #160]
    stp     q12, q13, [x0, #192]
    stp     q14, q15, [x0, #224]
    stp     q16, q17, [x0, #256]
    stp     q18, q19, [x0, #288]
    stp     q20, q21, [x0, #320]
    stp     q22, q23, [x0, #352]
    stp     q24, q25, [x0, #384]
    stp     q26, q27, [x0, #416]
    stp     q28, q29, [x0, #448]
    stp     q30, q31, [x0, #480]
    mrs     x1, fpsr
    mrs     x2, fpcr
    str     x1, [x0, #512]
    str     x2, [x0, #520]
    ret

.globl __fp_restore
// __fp_restore(x0 = *const FpState): FP/SIMD-Register aus dem Puffer laden.
__fp_restore:
    ldr     x1, [x0, #512]
    ldr     x2, [x0, #520]
    msr     fpsr, x1
    msr     fpcr, x2
    ldp     q0,  q1,  [x0, #0]
    ldp     q2,  q3,  [x0, #32]
    ldp     q4,  q5,  [x0, #64]
    ldp     q6,  q7,  [x0, #96]
    ldp     q8,  q9,  [x0, #128]
    ldp     q10, q11, [x0, #160]
    ldp     q12, q13, [x0, #192]
    ldp     q14, q15, [x0, #224]
    ldp     q16, q17, [x0, #256]
    ldp     q18, q19, [x0, #288]
    ldp     q20, q21, [x0, #320]
    ldp     q22, q23, [x0, #352]
    ldp     q24, q25, [x0, #384]
    ldp     q26, q27, [x0, #416]
    ldp     q28, q29, [x0, #448]
    ldp     q30, q31, [x0, #480]
    ret
"#
);

extern "C" {
    fn __fp_save(state: *mut FpState);
    fn __fp_restore(state: *const FpState);
}

/// Aktuelle FP/SIMD-Register in `state` sichern.
pub fn save(state: &mut FpState) {
    // SAFETY: `state` ist eine gültige, 16-ausgerichtete FpState; die Routine
    // schreibt genau ihre 528 Byte. Registerzugriff = erlaubte Low-Level-Domäne.
    unsafe { __fp_save(state as *mut FpState) }
}

/// FP/SIMD-Register aus `state` wiederherstellen.
pub fn restore(state: &FpState) {
    // SAFETY: `state` ist eine gültige, 16-ausgerichtete FpState; die Routine
    // liest genau ihre 528 Byte.
    unsafe { __fp_restore(state as *const FpState) }
}

/// `CPACR_EL1.FPEN` setzen: `trap = true` -> FP/SIMD **nur an EL0** trappen
/// (`0b01`); `trap = false` -> nicht trappen (`0b11`). EL1 trappt nie (Kernel ist
/// soft-float). Pro Kontextwechsel passend zum FP-Owner des Kerns zu setzen.
pub fn set_el0_trap(trap: bool) {
    let fpen: u64 = if trap { 0b01 } else { 0b11 } << 20;
    // SAFETY: Schreiben von CPACR_EL1 (FP-Trap-Konfiguration), erlaubte Domäne.
    // `isb` stellt sicher, dass die neue Trap-Konfiguration vor dem `eret` greift.
    unsafe {
        asm!("msr cpacr_el1, {v}", "isb", v = in(reg) fpen, options(nomem, nostack, preserves_flags));
    }
}
