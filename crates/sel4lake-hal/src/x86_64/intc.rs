//! Interrupt-Controller (x86_64: **Local APIC**) — API-gleich zum GICv2-Modul auf aarch64.
//!
//! Namensabbildung: was auf ARM eine *INTID* ist, ist hier ein **Vektor** (0..255) — beide
//! identifizieren die Interrupt-Quelle gegenüber dem Dispatch, deshalb bleibt die API gleich.
//! Ein `SGI` (Software Generated Interrupt) ist hier ein **IPI** über das ICR.

use super::cpu::{outb, rdmsr, wrmsr};
use core::sync::atomic::{AtomicU64, Ordering};

/// MSR mit LAPIC-Basisadresse + Enable-Bit.
const MSR_APIC_BASE: u32 = 0x1B;
const APIC_BASE_ENABLE: u64 = 1 << 11;

/// Standard-MMIO-Fenster des LAPIC (xAPIC). Wird identity-gemappt (uncacheable).
pub const LAPIC_BASE: usize = 0xFEE0_0000;

// LAPIC-Register (Byte-Offsets).
const REG_ID: usize = 0x020;
const REG_EOI: usize = 0x0B0;
/// Spurious Interrupt Vector Register (Bit 8 = APIC Software Enable).
const REG_SVR: usize = 0x0F0;
const REG_ICR_LOW: usize = 0x300;
const REG_ICR_HIGH: usize = 0x310;

/// Vektor des Reschedule-IPI (Cross-Core-Wecken) — Gegenstück zu SGI 0 auf ARM.
pub const IPI_RESCHED_INTID: u32 = super::exception::IPI_RESCHED_VECTOR as u32;
/// Vektor für „spurious" Interrupts (Bit 0..3 müssen bei manchen CPUs gesetzt sein).
const SPURIOUS_VECTOR: u32 = 0xFF;

/// Ist der LAPIC bereits initialisiert? (Damit `mmio` nicht vor `init_cpu` benutzt wird.)
static READY: AtomicU64 = AtomicU64::new(0);

fn reg_ptr(off: usize) -> *mut u32 {
    (LAPIC_BASE + off) as *mut u32
}

fn read(off: usize) -> u32 {
    // SAFETY: `LAPIC_BASE+off` ist ein architektonisch festgelegtes, identity-gemapptes
    // LAPIC-Register (uncacheable); volatile MMIO aliast keinen Rust-Speicher.
    unsafe { core::ptr::read_volatile(reg_ptr(off)) }
}

fn write(off: usize, val: u32) {
    // SAFETY: wie `read`.
    unsafe { core::ptr::write_volatile(reg_ptr(off), val) }
}

/// **Globale** Interrupt-Controller-Initialisierung (aarch64: GIC-Distributor).
///
/// Auf x86 heißt das: den alten 8259-PIC vollständig **maskieren**, damit er keine Vektoren
/// mehr einstreut (er ist beim Boot aktiv und würde mit den CPU-Exception-Vektoren 8..15
/// kollidieren). Der LAPIC selbst wird pro Kern in [`init_cpu`] aktiviert.
pub fn init_dist() {
    // SAFETY: Port-I/O auf die architektonisch festen 8259-Ports; wir maskieren nur.
    unsafe {
        // ICW1..ICW4: beide PICs auf die Vektoren 0x20/0x28 umleiten (weg von den
        // CPU-Exceptions), danach alle Leitungen maskieren.
        outb(0x20, 0x11);
        outb(0xA0, 0x11);
        outb(0x21, 0x20);
        outb(0xA1, 0x28);
        outb(0x21, 0x04);
        outb(0xA1, 0x02);
        outb(0x21, 0x01);
        outb(0xA1, 0x01);
        outb(0x21, 0xFF); // alle Master-Leitungen maskiert
        outb(0xA1, 0xFF); // alle Slave-Leitungen maskiert
    }
}

/// Pro-Kern-Initialisierung: LAPIC im MSR **und** im SVR freischalten.
pub fn init_cpu() {
    // SAFETY: `IA32_APIC_BASE` existiert auf jeder x86_64-CPU mit LAPIC (alle unterstützten).
    unsafe {
        let base = rdmsr(MSR_APIC_BASE);
        wrmsr(MSR_APIC_BASE, base | APIC_BASE_ENABLE);
    }
    write(REG_SVR, SPURIOUS_VECTOR | (1 << 8));
    READY.store(1, Ordering::Release);
}

/// LAPIC-ID des aufrufenden Kerns (erst nach [`init_cpu`] gültig).
pub fn lapic_id() -> u32 {
    read(REG_ID) >> 24
}

/// **End Of Interrupt** an den LAPIC. Auf ARM steckt das im GIC-`EOIR`-Schreibzugriff des
/// Dispatch; hier ruft es [`super::exception::handle_exception`] für jeden Vektor >= 32.
pub fn eoi() {
    if READY.load(Ordering::Acquire) != 0 {
        write(REG_EOI, 0);
    }
}

/// Einen Interrupt freigeben. Auf x86 gibt es keine per-INTID-Freigabe im LAPIC —
/// Geräte-Interrupts werden am **IOAPIC** geroutet (noch nicht portiert, s. Modul-Doku der
/// Crate). Für LAPIC-lokale Quellen (Timer) ist die Freigabe Teil ihrer Konfiguration.
pub fn enable_intid(_intid: u32) {}

/// Gegenstück zu [`enable_intid`] (s. dort).
pub fn mask_intid(_intid: u32) {}

/// Einen Geräte-Interrupt an einen Kern routen (IOAPIC) — noch nicht portiert.
pub fn route_spi(_intid: u32, _target_core: usize) {}

/// **IPI** an `target_core` mit Vektor `intid` schicken (aarch64: `send_sgi`).
pub fn send_sgi(target_core: usize, intid: u32) {
    if READY.load(Ordering::Acquire) == 0 {
        return;
    }
    // Ziel-LAPIC-ID in ICR_HIGH[31:24], dann ICR_LOW schreiben (löst den IPI aus).
    write(REG_ICR_HIGH, (target_core as u32) << 24);
    // Delivery Mode 000 (Fixed), Physical, Edge, kein Shorthand.
    write(REG_ICR_LOW, intid & 0xFF);
}

/// **INIT-IPI** an `apic_id` (Teil der AP-Startsequenz, s. `power::cpu_on`).
pub fn send_init_ipi(apic_id: u32) {
    write(REG_ICR_HIGH, apic_id << 24);
    // Delivery Mode 101 (INIT), Level Assert, Edge.
    write(REG_ICR_LOW, 0x4500);
    wait_ipi_delivered();
}

/// **STARTUP-IPI** (SIPI) an `apic_id` mit der Trampolin-Seitennummer als Vektor.
pub fn send_startup_ipi(apic_id: u32, vector: u32) {
    write(REG_ICR_HIGH, apic_id << 24);
    // Delivery Mode 110 (Startup), Level Assert.
    write(REG_ICR_LOW, 0x4600 | (vector & 0xFF));
    wait_ipi_delivered();
}

/// Warten, bis der LAPIC den IPI abgesetzt hat (ICR_LOW Bit 12 = Delivery Status).
fn wait_ipi_delivered() {
    for _ in 0..100_000 {
        if read(REG_ICR_LOW) & (1 << 12) == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Auf ARM liest der Dispatch die aktive INTID aus dem GIC. Auf x86 **ist** der Vektor die
/// Identität (er steht im Trap-Frame) — es gibt nichts nachzuschlagen.
pub fn handle_irq() -> Option<u32> {
    None
}
