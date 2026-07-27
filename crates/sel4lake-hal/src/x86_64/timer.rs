//! Periodischer Zeitscheiben-Timer (x86_64: **LAPIC-Timer**) — API-gleich zu aarch64.

use super::intc::LAPIC_BASE;
use core::sync::atomic::{AtomicU64, Ordering};

/// Vektor des Timer-Interrupts (aarch64: PPI 30).
pub const TIMER_INTID: u32 = super::exception::TIMER_VECTOR as u32;

// LAPIC-Timer-Register.
const REG_LVT_TIMER: usize = 0x320;
const REG_TIMER_INITCNT: usize = 0x380;
const REG_TIMER_CURRCNT: usize = 0x390;
const REG_TIMER_DIV: usize = 0x3E0;
/// LVT-Bit 17: periodischer Modus.
const LVT_PERIODIC: u32 = 1 << 17;

/// Gemessene Taktrate des LAPIC-Timers (Ticks/s), s. [`calibrate`].
static LAPIC_HZ: AtomicU64 = AtomicU64::new(0);

/// Tick-Zähler je Kern (nur der jeweilige Kern schreibt seinen Eintrag).
const MAX_CORES: usize = 256;
static TICKS: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(0) }; MAX_CORES];

fn write(off: usize, val: u32) {
    // SAFETY: architektonisch festes, identity-gemapptes LAPIC-Register (uncacheable).
    unsafe { core::ptr::write_volatile((LAPIC_BASE + off) as *mut u32, val) }
}
fn read(off: usize) -> u32 {
    // SAFETY: wie `write`.
    unsafe { core::ptr::read_volatile((LAPIC_BASE + off) as *const u32) }
}

/// Frequenz der Zeitbasis (aarch64: `CNTFRQ_EL0`).
pub fn freq() -> u64 {
    LAPIC_HZ.load(Ordering::Relaxed)
}

/// Den LAPIC-Timer gegen den **PIT-Kanal 2** kalibrieren (der läuft mit den festen
/// 1.193182 MHz und ist ohne Interrupts pollbar).
///
/// Nötig, weil die LAPIC-Timer-Frequenz vom Bustakt abhängt und **nicht** architektonisch
/// festgelegt ist (anders als `CNTFRQ_EL0` auf ARM).
fn calibrate() -> u64 {
    const PIT_HZ: u64 = 1_193_182;
    const SAMPLE_TICKS: u64 = PIT_HZ / 100; // 10 ms Messfenster
    // SAFETY: Port-I/O auf die architektonisch festen PIT-/Gate-Ports.
    unsafe {
        use super::cpu::{inb, outb};
        // Kanal 2: Gate an, Lautsprecher aus.
        outb(0x61, (inb(0x61) & !0x02) | 0x01);
        outb(0x43, 0xB0); // Kanal 2, lobyte/hibyte, Modus 0 (one-shot)
        outb(0x42, (SAMPLE_TICKS & 0xFF) as u8);
        outb(0x42, (SAMPLE_TICKS >> 8) as u8);

        write(REG_TIMER_DIV, 0b1011); // Teiler 1
        write(REG_TIMER_INITCNT, u32::MAX); // frei laufend abwärts
        // Warten, bis der PIT-Ausgang (Port 0x61 Bit 5) kippt.
        while inb(0x61) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        let elapsed = u32::MAX - read(REG_TIMER_CURRCNT);
        write(REG_TIMER_INITCNT, 0); // anhalten
        (elapsed as u64) * 100 // Ticks in 10 ms -> Ticks/s
    }
}

/// Timer mit `hz` Interrupts/s armieren (pro Kern aufzurufen).
pub fn init(hz: u64) {
    let rate = match LAPIC_HZ.load(Ordering::Relaxed) {
        0 => {
            let r = calibrate().max(1);
            LAPIC_HZ.store(r, Ordering::Relaxed);
            r
        }
        r => r,
    };
    write(REG_TIMER_DIV, 0b1011); // Teiler 1
    write(REG_LVT_TIMER, TIMER_INTID | LVT_PERIODIC);
    write(REG_TIMER_INITCNT, (rate / hz.max(1)).max(1) as u32);
}

/// Vom Interrupt-Dispatch bei jedem Timer-Interrupt zu rufen (aarch64: Comparator neu setzen;
/// im periodischen LAPIC-Modus nur den Zähler führen).
pub fn on_irq() {
    let core = super::cpu::core_id();
    if core < MAX_CORES {
        TICKS[core].fetch_add(1, Ordering::Relaxed);
    }
}

/// Bisherige Timer-Interrupts auf Kern `core`.
pub fn ticks(core: usize) -> u64 {
    TICKS.get(core).map(|t| t.load(Ordering::Relaxed)).unwrap_or(0)
}

// --- Zyklenzähler (Stufe 1) -------------------------------------------------------------------
//
// Die Tick-Uhr löst 10 ms auf. Das reicht, um Zeitscheiben zu verteilen, und **nicht**, um zu
// sagen, wie viel Rechenzeit ein Thread tatsächlich verbraucht hat oder wie lange eine kritische
// Sektion dauert: bei einer Sektion von wenigen Mikrosekunden misst man mit 10-ms-Ticks nichts.
// Deshalb ein zweites, feineres Primitiv — dieselbe API auf beiden Architekturen
// (`CNTPCT_EL0` dort, TSC hier).

/// Kalibrierte TSC-Frequenz (Zyklen/s); `0` = noch nicht gemessen.
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

/// Ist der TSC **invariant** (`CPUID.80000007H:EDX[8]`)?
///
/// Geprüft, nicht angenommen: ein nicht-invarianter TSC ändert seine Rate mit P-States und
/// steht im Deep-Sleep still. Zeitdifferenzen wären dann keine Zeit, sondern eine Funktion des
/// Taktverhaltens — und zwar eine, die im Messlauf plausibel aussieht.
pub fn invariant_tsc() -> bool {
    // SAFETY: `cpuid` ist nebenwirkungsfrei; das Blatt wird zuvor auf Verfügbarkeit geprüft.
    unsafe {
        let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
        if max < 0x8000_0007 {
            return false;
        }
        core::arch::x86_64::__cpuid(0x8000_0007).edx & (1 << 8) != 0
    }
}

/// Ein **serialisierender** Zyklen-Zeitstempel.
///
/// `rdtsc` allein serialisiert nicht: die CPU darf es gegenüber den umgebenden Befehlen
/// verschieben. Bei langen Messfenstern verschwindet das im Rauschen, bei kurzen kritischen
/// Sektionen — genau dem Fall, für den dieses Primitiv gebaut wird — dominiert es. Deshalb
/// `rdtscp` (wartet auf alle vorherigen Befehle) plus `lfence` (verhindert, dass spätere
/// vorgezogen werden). Fehlt `rdtscp`, `lfence` + `rdtsc`.
pub fn cycles() -> u64 {
    // SAFETY: reine Lesebefehle ohne Speicherzugriff.
    unsafe {
        if has_rdtscp() {
            let mut aux = 0u32;
            let t = core::arch::x86_64::__rdtscp(&mut aux);
            core::arch::x86_64::_mm_lfence();
            t
        } else {
            core::arch::x86_64::_mm_lfence();
            let t = core::arch::x86_64::_rdtsc();
            core::arch::x86_64::_mm_lfence();
            t
        }
    }
}

fn has_rdtscp() -> bool {
    // SAFETY: siehe `invariant_tsc`.
    unsafe {
        let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
        max >= 0x8000_0001 && core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 27) != 0
    }
}

/// Zyklen pro Sekunde (gegen den PIT kalibriert, wie der LAPIC-Timer).
///
/// **Auf einem Kern gemessen und nur dort gültig.** Zeitstempel verschiedener Kerne sind erst
/// nach TSC-Synchronisation subtrahierbar; auf Mehrsockel-Systemen ist das nicht selbstver-
/// ständlich. Wer eine Sektion misst, bleibt auf einem Kern — dann stellt sich die Frage nicht.
pub fn cycles_per_sec() -> u64 {
    match TSC_HZ.load(Ordering::Relaxed) {
        0 => {
            let hz = calibrate_tsc().max(1);
            TSC_HZ.store(hz, Ordering::Relaxed);
            hz
        }
        v => v,
    }
}

/// Den TSC über ein 10-ms-PIT-Fenster kalibrieren (gleiche Mechanik wie [`calibrate`]).
fn calibrate_tsc() -> u64 {
    const PIT_HZ: u64 = 1_193_182;
    const SAMPLE_TICKS: u64 = PIT_HZ / 100;
    // SAFETY: PIT-Kanal 2 wird hier exklusiv benutzt (kein Interrupt, nur Gate + Polling) —
    // dieselbe Sequenz wie in `calibrate`.
    unsafe {
        use super::cpu::{inb, outb};
        outb(0x61, (inb(0x61) & !0x02) | 0x01);
        outb(0x43, 0xB0);
        outb(0x42, (SAMPLE_TICKS & 0xFF) as u8);
        outb(0x42, (SAMPLE_TICKS >> 8) as u8);
        let t0 = cycles();
        while inb(0x61) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        let t1 = cycles();
        t1.wrapping_sub(t0) * 100
    }
}
