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
/// `CR0.TS` löschen. **Muss vor jedem `fxsave64`/`fxrstor64` geschehen** — s. [`save`].
#[inline]
fn ts_loeschen() {
    // SAFETY: Read-Modify-Write auf CR0, ausschliesslich Bit 3 (TS).
    unsafe {
        let mut cr0: u64;
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 &= !(1u64 << 3);
        asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack, preserves_flags));
    }
}

pub fn save(state: &mut FpState) {
    // SAFETY: `state` ist eine gültige, 16-Byte-ausgerichtete 512-Byte-Region (Typinvariante);
    // `fxsave64` schreibt genau diese. Registerzugriff = erlaubte Low-Level-Domäne.
    //
    // **`CR0.TS` zuerst loeschen, und zwar HIER statt beim Aufrufer** (2026-08-09). `FXSAVE` und
    // `FXRSTOR` sind selbst FP-Instruktionen: mit gesetztem `TS` loesen sie ein `#NM` aus -- also
    // genau den Trap, aus dem heraus sie gerufen werden. Das Ergebnis ist ein rekursiver Trap und
    // ein Haenger.
    //
    // **Warum das nie aufgefallen ist:** `CR0.TS` gilt fuer JEDE Privilegstufe, `CPACR_EL1.FPEN`
    // auf ARM dagegen nur fuer EL0 -- dort darf der Kernel immer rechnen, und derselbe
    // Aufruferkode ist korrekt. Der x86-Pfad war bis heute tot: mit `CR0.EM = 1` war SSE ein
    // `#UD`, x87 benutzt ein soft-float-Kernel nicht, also feuerte `#NM` nie.
    //
    // In der HAL und nicht beim Aufrufer, damit die naechste Aufrufstelle es nicht vergessen kann.
    ts_loeschen();
    unsafe { asm!("fxsave64 [{}]", in(reg) state.area.as_mut_ptr(), options(nostack, preserves_flags)) };
}

/// FP/SIMD-Register aus `state` wiederherstellen.
pub fn restore(state: &FpState) {
    ts_loeschen(); // s. `save` -- `fxrstor64` ist selbst eine FP-Instruktion
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

/// **Den SSE-Befehlssatz freischalten** (Z19/A4, 2026-08-09). `false`, wenn die CPU FXSR oder SSE
/// nicht meldet — dann wird **nichts** gesetzt.
///
/// # Vier Bits, nicht eines
///
/// | Bit | wofuer |
/// |---|---|
/// | `CR0.EM` = **0** | solange gesetzt, faultet jede FP-Instruktion — **unabhaengig** von OSFXSR |
/// | `CR0.MP` = **1** | laesst `WAIT`/`FWAIT` zusammen mit `TS` richtig trappen |
/// | `CR4.OSFXSR` = 1 | schaltet `FXSAVE`/`FXRSTOR` **und** SSE frei |
/// | `CR4.OSXMMEXCPT` = 1 | meldet unmaskierte SIMD-Ausnahmen als `#XM` statt `#UD` |
///
/// Die vier sind **nicht** gleichwertig: nur `OSFXSR` (und `EM`) verschieben etwas an der
/// Fault-Leitung; `OSXMMEXCPT` wirkt erst, wenn schon SSE-Code mit unmaskierten Ausnahmebits
/// laeuft. Eine frueher hier stehende Behauptung „alle vier haben dieselbe Wirkung" war eine
/// Verallgemeinerung ueber Messungen, die es nicht gab — gemessen waren drei KOMBINATIONEN, keine
/// Einzelbits.
///
/// # Auf JEDEM Kern, und NACH `init_core()`
///
/// `CR0`/`CR4` sind kernlokal. Dieselbe Funktion fuer BSP und AP, damit die zwei Stellen nicht
/// auseinanderlaufen; der Zaehler beim Aufrufer muss am Ende `num_cores()` sein.
pub fn enable_sse() -> bool {
    let edx: u32;
    // SAFETY: `cpuid` Blatt 1, nebenwirkungsfrei. KEIN `nostack` -- der Block pusht `rbx`.
    unsafe {
        core::arch::asm!(
            "push rbx", "mov eax, 1", "cpuid", "pop rbx",
            out("edx") edx, out("eax") _, out("ecx") _,
            options(preserves_flags)
        );
    }
    if edx & (1 << 24) == 0 || edx & (1 << 25) == 0 {
        return false;
    }
    // SAFETY: Read-Modify-Write auf CR0/CR4, nur die vier benannten Bits.
    unsafe {
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 &= !(1u64 << 2);
        cr0 |= 1u64 << 1;
        core::arch::asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack, preserves_flags));
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        cr4 |= (1u64 << 9) | (1u64 << 10);
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack, preserves_flags));
    }
    true
}

/// `CR0.TS` löschen, ohne den Owner zu wechseln — nach einem `#NM` nötig, bevor `fxrstor64`
/// ausgeführt werden darf (sonst trappt der Restore selbst erneut).
pub fn clear_task_switched() {
    // SAFETY: `clts` löscht genau CR0.TS; Ring-0-Instruktion, erlaubte Domäne.
    unsafe { asm!("clts", options(nomem, nostack, preserves_flags)) };
}
