//! ARM Generic Timer (EL1 physical timer) als periodische Tick-Quelle.
//!
//! Der nicht-sichere EL1-Physical-Timer signalisiert über PPI INTID 30. Wir
//! programmieren ein festes Intervall und laden es bei jedem Tick neu. Pro Kern
//! wird ein Tick-Zähler geführt (Verifikation des Multicore-Timings, P1e).
//!
//! `unsafe` nur für Timer-Systemregisterzugriffe — erlaubte Domäne.

use super::{cpu, gic};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// PPI des nicht-sicheren EL1-Physical-Timers (QEMU `virt`).
pub const TIMER_INTID: u32 = 30;

/// Compile-Zeit-Obergrenze der Kernzahl (nur diese Telemetrie-Tabelle; die tatsächliche
/// Kernzahl ermittelt der Kernel beim Boot).
const MAX_CORES: usize = 256;
static TICKS: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(0) }; MAX_CORES];
/// Tick-Intervall in Timer-Zählern (für alle Kerne identisch).
static INTERVAL: AtomicU64 = AtomicU64::new(0);

// CNTP_CTL_EL0-Bits.
const CTL_ENABLE: u64 = 1 << 0;
// (IMASK = 1<<1 bleibt 0 -> Interrupt nicht maskiert.)

/// Timer-Frequenz (`CNTFRQ_EL0`) in Hz.
pub fn freq() -> u64 {
    let f: u64;
    // SAFETY: read-only Systemregister.
    unsafe { asm!("mrs {}, CNTFRQ_EL0", out(reg) f, options(nomem, nostack, preserves_flags)) }
    f
}

fn set_tval(ticks: u64) {
    // SAFETY: Timer-Registerzugriff (erlaubte Low-Level-Domäne).
    unsafe { asm!("msr CNTP_TVAL_EL0, {}", in(reg) ticks, options(nomem, nostack, preserves_flags)) }
}

fn set_ctl(val: u64) {
    // SAFETY: Timer-Registerzugriff.
    unsafe { asm!("msr CNTP_CTL_EL0, {}", in(reg) val, options(nomem, nostack, preserves_flags)) }
}

/// Periodischen Tick mit `hz` Hertz am aktuellen Kern starten.
///
/// Setzt das gemeinsame Intervall (idempotent), gibt die Timer-PPI am GIC frei
/// und armiert den Timer. IRQs müssen separat freigegeben werden
/// ([`cpu::local_irq_enable`]).
pub fn init(hz: u64) {
    let interval = freq() / hz;
    INTERVAL.store(interval, Ordering::Relaxed);
    gic::enable_intid(TIMER_INTID);
    set_tval(interval);
    set_ctl(CTL_ENABLE);
}

/// Vom IRQ-Dispatch bei einem Timer-Interrupt aufgerufen: neu armieren + zählen.
/// (Die eigentliche Reschedule-Entscheidung trifft der Hook in `exception`.)
pub fn on_irq() {
    set_tval(INTERVAL.load(Ordering::Relaxed));
    let core = cpu::core_id();
    TICKS[core].fetch_add(1, Ordering::Relaxed);
}

/// Tick-Zähler eines Kerns.
pub fn ticks(core: usize) -> u64 {
    TICKS[core].load(Ordering::Relaxed)
}
