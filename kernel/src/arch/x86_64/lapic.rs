//! Local APIC + periodischer LAPIC-Timer (Branch arch/x86_64, Stufe 2b).
//!
//! Der LAPIC (MMIO @ 0xFEE0_0000, in `paging` als cache-disabled 2-MiB-Seite gemappt) liefert
//! lokale Timer-Interrupts direkt an den Kern — kein IOAPIC nötig. Wir maskieren den Legacy-8259-PIC,
//! aktivieren den LAPIC (SVR) und armieren den Timer im **periodischen** Modus auf IDT-Vektor 32.
//! Der IRQ-Handler (`idt::x86_isr_handler`, Vektor 32) zählt Ticks und sendet EOI. Pendant zu
//! `sel4lake-hal::{gic,timer}` (aarch64 GICv2 + Generic Timer).

use core::sync::atomic::{AtomicU64, Ordering};

const LAPIC: u64 = 0xFEE0_0000;
// LAPIC-Registeroffsets.
const SVR: u64 = 0x0F0; //   Spurious Interrupt Vector Register
const EOI_REG: u64 = 0x0B0; // End Of Interrupt
const LVT_TIMER: u64 = 0x320; // LVT Timer Entry
const TIMER_INIT: u64 = 0x380; // Initial Count
const TIMER_DIV: u64 = 0x3E0; //  Divide Configuration

/// IDT-Vektor des LAPIC-Timers (>= 32, oberhalb der CPU-Exceptions).
pub const TIMER_VECTOR: u32 = 32;

static TICKS: AtomicU64 = AtomicU64::new(0);

#[inline]
unsafe fn rd(off: u64) -> u32 {
    // SAFETY: gemapptes LAPIC-MMIO (cache-disabled), 32-bit-ausgerichtet.
    core::ptr::read_volatile((LAPIC + off) as *const u32)
}
#[inline]
unsafe fn wr(off: u64, v: u32) {
    // SAFETY: wie `rd`.
    core::ptr::write_volatile((LAPIC + off) as *mut u32, v);
}

/// Legacy-8259-PIC vollständig maskieren (nur der LAPIC-Timer soll Interrupts liefern).
fn mask_pic() {
    // SAFETY: Port-I/O auf die PIC-Datenports (OCW1 = alle IRQ-Leitungen maskieren).
    unsafe {
        super::outb(0x21, 0xFF); // Master-PIC
        super::outb(0xA1, 0xFF); // Slave-PIC
    }
}

/// LAPIC aktivieren + periodischen Timer (Vektor 32) armieren.
pub fn init() {
    mask_pic();
    // SAFETY: LAPIC-MMIO ist gemappt; Standard-Aktivierungs-/Timer-Sequenz.
    unsafe {
        wr(SVR, 0x1FF); // LAPIC enable (Bit 8) + Spurious-Vektor 0xFF
        wr(TIMER_DIV, 0x3); // Divide /16
        wr(LVT_TIMER, TIMER_VECTOR | (1 << 17)); // Vektor 32 + periodic (Bit 17)
        wr(TIMER_INIT, 1_000_000); // Initial Count -> Tick-Rate (grob; QEMU-Bus-Takt)
    }
}

/// End-of-Interrupt an den LAPIC (im IRQ-Handler nach jedem behandelten Interrupt).
pub fn eoi() {
    // SAFETY: Schreiben von 0 ins EOI-Register quittiert den aktuellen Interrupt.
    unsafe { wr(EOI_REG, 0) }
}

/// Vom Timer-IRQ aufgerufen: Tick zählen.
pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Bisher gezählte Timer-Ticks.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
