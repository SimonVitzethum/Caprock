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

/// **Read the SMT topology** (Z6 stage 0) — same signature as the x86 side, because the kernel's
/// admission policy is architecture-neutral and must not grow two spellings.
///
/// On aarch64 the only architectural source in a system register is `MPIDR_EL1.MT`. It answers
/// "are the lowest-affinity PEs multithreaded", and nothing else: the thread *count* lives in the
/// ACPI PPTT / device tree, which this kernel does not read. `MT == 1` therefore decodes to
/// `Unknown` — see `smt::decode_mpidr`, which also records the second reason (with `MT == 1`,
/// [`core_id`] itself collides across cores).
///
/// Under QEMU `virt` with `cortex-a72` the bit is clear, so this reads `Single` and the policy is
/// a no-op there. That is a correct reading, not an absent one — and the difference is exactly
/// what the report line has to make visible.
pub fn smt_topology() -> crate::smt::SmtTopology {
    let mpidr: u64;
    // SAFETY: `MPIDR_EL1` ist read-only und ohne Seiteneffekte.
    unsafe {
        asm!("mrs {}, MPIDR_EL1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    crate::smt::decode_mpidr(mpidr)
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

/// Den IRQ-Maskenzustand (DAIF) **sichern + IRQs maskieren**; gibt den vorherigen Zustand für
/// [`local_irq_restore`] zurück. Für Code, der in **beiden** Kontexten läuft (IRQs an *oder* im
/// Trap maskiert) und den Vorzustand erhalten muss, statt unbedingt freizugeben.
pub fn local_irq_save() -> u64 {
    let daif: u64;
    // SAFETY: reines Lesen + Setzen des DAIF-Systemregisters (I-Bit).
    unsafe {
        asm!("mrs {0}, DAIF", out(reg) daif, options(nomem, nostack, preserves_flags));
        asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }
    daif
}

/// Sind die IRQs am eigenen Kern gerade **freigegeben** (DAIF.I geloescht)?
///
/// Gegenstueck zu `x86_64::cpu::irqs_freigegeben`; wofuer es gebraucht wird, steht dort und in
/// `crate::konsole` (C9b).
#[inline(always)]
pub fn irqs_freigegeben() -> bool {
    let daif: u64;
    // SAFETY: reines Lesen des DAIF-Systemregisters, ohne Seiteneffekte.
    unsafe {
        asm!("mrs {0}, DAIF", out(reg) daif, options(nomem, nostack, preserves_flags));
    }
    // DAIF beim Lesen: D=Bit 9, A=Bit 8, **I=Bit 7**, F=Bit 6. Gesetzt heisst maskiert.
    daif & (1 << 7) == 0
}

/// Den von [`local_irq_save`] gesicherten DAIF-Zustand wiederherstellen.
pub fn local_irq_restore(daif: u64) {
    // SAFETY: schreibt nur den zuvor gelesenen DAIF-Zustand zurück.
    unsafe {
        asm!("msr DAIF, {0}", in(reg) daif, options(nomem, nostack, preserves_flags));
    }
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

/// Frisch geschriebenen **Code** im Bereich `[base, base+len)` kohärent zur
/// Instruktions-Ausführung machen: D-Cache bis PoU säubern (`dc cvau`), dann
/// I-Cache invalidieren (`ic ivau`) + Barrieren. Nötig nach dem Laden von Code in
/// einen Frame, bevor er ausgeführt wird (sonst holt der Kern evtl. veraltete/leere
/// I-Cache-Zeilen). Cache-Wartung ist eine erlaubte Low-Level-Domäne.
pub fn sync_code_range(base: usize, len: usize) {
    const LINE: usize = 64; // konservative Cache-Line-Größe (CTR_EL0 wäre exakter)
    let start = base & !(LINE - 1);
    let end = base + len;
    // SAFETY: Cache-Wartungsinstruktionen auf gültigem, kernel-besessenem RAM.
    unsafe {
        let mut a = start;
        while a < end {
            asm!("dc cvau, {}", in(reg) a, options(nostack, preserves_flags));
            a += LINE;
        }
        asm!("dsb ish", options(nostack, preserves_flags));
        a = start;
        while a < end {
            asm!("ic ivau, {}", in(reg) a, options(nostack, preserves_flags));
            a += LINE;
        }
        asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

// --- Spekulations-Härtung (Spectre-Klasse) ---------------------------------------------
//
// Der Kernel prüft an der EL0-Grenze Indizes (Cap-Slot, Endpoint-/Notification-Id) gegen
// Tabellengrenzen. Ein `if idx < N { tab[idx] }` schützt nur den ARCHITEKTONISCHEN Pfad:
// die Verzweigungsvorhersage kann den Rumpf spekulativ mit einem out-of-bounds `idx`
// ausführen und so einen von EL0 wählbaren Kernel-Offset in den Cache ziehen (Spectre-v1).
// [`array_index_nospec`] macht den Index zusätzlich DATENABHÄNGIG null, sobald er außerhalb
// liegt — das überlebt auch eine falsch vorhergesagte Verzweigung.
//
// NICHT abgedeckt (bewusst, s. `docs/invariants.md`): Cache-/Timing-Seitenkanäle zwischen PDs
// (keine Cache-Partitionierung), und Spectre-v2/BHB auf HW ohne FEAT_CSV2 (dort hilft nur
// Predictor-Invalidierung per Firmware-Call, den QEMU `virt` nicht anbietet). [`csv2`]/[`csv3`]
// melden, was die HW von sich aus garantiert.

/// **CSDB** — Consumption of Speculative Data Barrier (`HINT #20`, auf jeder ARMv8-HW als
/// Hint kodiert, auf älterer HW ein NOP). Verhindert, dass eine spekulativ erzeugte
/// Bedingungsmaske vor ihrer Auflösung weiterverwendet wird.
pub fn csdb() {
    // SAFETY: reine Hint-/Barriere-Instruktion ohne Speichereffekt.
    unsafe { asm!("hint #20", options(nomem, nostack, preserves_flags)) }
}

/// **Spekulationssicherer Array-Index:** liefert `index`, wenn `index < len`, sonst `0` —
/// und zwar **datenabhängig** (arithmetische Maske + [`csdb`]), nicht per Verzweigung. Der
/// Aufrufer muss die Grenze weiterhin architektonisch prüfen; dies härtet nur den
/// spekulativen Pfad (Linux' `array_index_nospec`).
#[inline(always)]
pub fn array_index_nospec(index: usize, len: usize) -> usize {
    // index < len  ->  (index - len) hat gesetztes Vorzeichenbit  ->  Maske = !0
    // index >= len ->  Vorzeichenbit 0                            ->  Maske = 0
    let mask = (((index as u64).wrapping_sub(len as u64) as i64) >> 63) as u64;
    csdb();
    index & (mask as usize)
}

/// **Spekulationsbarriere** an einer Vertrauensgrenze (VSpace-Wechsel): `SB` (FEAT_SB,
/// ARMv8.5), sonst der portable Ersatz `dsb sy; isb`. Nach der Barriere hängt keine
/// Spekulation mehr am Zustand von vor dem Wechsel.
pub fn speculation_barrier() {
    if sb_supported() {
        // SAFETY: `SB` (0xd50330ff) ist eine reine Barriere; nur ausgeführt, wenn
        // ID_AA64ISAR1_EL1.SB sie meldet. Als `.inst` kodiert, damit das Target-Feature
        // (armv8-a) den Assembler nicht ablehnt.
        unsafe { asm!(".inst 0xd50330ff", options(nomem, nostack, preserves_flags)) }
    } else {
        dsb_sy();
        isb();
    }
}

/// `ID_AA64ISAR1_EL1.SB` (Bits [39:36]) != 0 -> FEAT_SB (Instruktion `SB`) vorhanden.
pub fn sb_supported() -> bool {
    let v: u64;
    // SAFETY: read-only ID-Register.
    unsafe {
        asm!("mrs {}, ID_AA64ISAR1_EL1", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    (v >> 36) & 0xf != 0
}

/// `ID_AA64PFR0_EL1.CSV2` (Bits [59:56]): >=1 -> die HW garantiert, dass Branch-Predictor-
/// Einträge nicht über Kontexte hinweg ausgenutzt werden können (Spectre-v2-immun).
pub fn csv2() -> u8 {
    let v: u64;
    // SAFETY: read-only ID-Register.
    unsafe {
        asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    ((v >> 56) & 0xf) as u8
}

/// `ID_AA64PFR0_EL1.CSV3` (Bits [63:60]): >=1 -> die HW garantiert, dass Daten aus nicht
/// zugänglichem Speicher keinen spekulativen Seitenkanal erzeugen (Meltdown-immun).
pub fn csv3() -> u8 {
    let v: u64;
    // SAFETY: read-only ID-Register.
    unsafe {
        asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) v, options(nomem, nostack, preserves_flags));
    }
    ((v >> 60) & 0xf) as u8
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

/// **Läuft dieser Kernel unter einem Hypervisor?** — auf aarch64 aus EL1 **nicht** feststellbar.
///
/// Anders als x86 (`CPUID.1:ECX[31]`) kennt die Architektur kein Bit, das ein Gast lesen dürfte;
/// EL2 ist von EL1 aus per Entwurf unsichtbar. Gemeldet wird deshalb `false` — die
/// **konservative** Antwort: ein davon abhängiger Test urteilt dann so, als liefe er auf echter
/// Hardware, und ein Fehlschlag bleibt ein Fehlschlag statt weggeklärt zu werden. Siehe die
/// x86-Fassung für den Grund, warum das überhaupt jemanden interessiert (B-4.5).
pub fn hypervisor_present() -> bool {
    false
}
