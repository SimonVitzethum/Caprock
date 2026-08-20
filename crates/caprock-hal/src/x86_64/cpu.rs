//! CPU-Register und Low-Level-Instruktionen (x86_64).
//!
//! API-gleich zum aarch64-Pendant; jede Funktion kapselt genau einen Registerzugriff bzw.
//! eine Instruktion — eine ausdrücklich erlaubte `unsafe`-Domäne.

use core::arch::asm;

// --- Port-I/O (x86-spezifisch, von `console`/`intc`/`power` genutzt) -----------------------

/// Ein Byte aus dem I/O-Port `port` lesen.
///
/// # Safety
/// Port-I/O wirkt auf Geräte. Der Aufrufer muss sicherstellen, dass `port` das gemeinte
/// Gerät adressiert und der Zugriff dessen Zustand nicht unzulässig verändert.
#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

/// Ein Byte in den I/O-Port `port` schreiben.
///
/// # Safety
/// Wie [`inb`].
#[inline(always)]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags)) };
}

/// Ein u32 in den I/O-Port `port` schreiben.
///
/// # Safety
/// Wie [`inb`].
#[inline(always)]
pub unsafe fn outl(port: u16, val: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags)) };
}

// --- MSR ----------------------------------------------------------------------------------

/// Model-Specific Register lesen.
///
/// # Safety
/// Der Aufrufer muss sicherstellen, dass `msr` auf dieser CPU existiert (sonst #GP).
#[inline(always)]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (hi, lo): (u32, u32);
    unsafe { asm!("rdmsr", in("ecx") msr, out("edx") hi, out("eax") lo, options(nomem, nostack, preserves_flags)) };
    ((hi as u64) << 32) | lo as u64
}

/// Model-Specific Register schreiben.
///
/// # Safety
/// Wie [`rdmsr`]; zusätzlich verändert ein MSR-Schreibzugriff CPU-Verhalten (Paging,
/// Syscall-Einstiegspunkte, APIC).
#[inline(always)]
pub unsafe fn wrmsr(msr: u32, val: u64) {
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("edx") (val >> 32) as u32, in("eax") val as u32,
             options(nomem, nostack, preserves_flags))
    };
}

/// `CPUID`-Blatt `leaf` (Unterblatt 0) abfragen -> `(eax, ebx, ecx, edx)`.
///
/// Bewusst über die `core`-Intrinsic statt über handgeschriebenes `asm!`: `CPUID` schreibt
/// **RBX**, das LLVM auf x86_64 reserviert — die naheliegende Rettung per `push rbx`/`pop rbx`
/// im selben `asm!`-Block lieferte hier stillschweigend falsche Werte (die Kern-ID kam als 2
/// statt 0 heraus, worauf der Kernel einen Scheduler ohne Tabellen ansprach). Die Intrinsic
/// kennt die Registerbeschränkung und ist die einzige zuverlässige Form.
pub fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    // SAFETY: `cpuid` ist auf jeder x86_64-CPU verfügbar, hat keine Speicherwirkung und
    // serialisiert lediglich.
    let r = unsafe { core::arch::x86_64::__cpuid(leaf) };
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// `CPUID`-Blatt `leaf`, **Unterblatt** `sub` -> `(eax, ebx, ecx, edx)`.
///
/// Blatt 4 (Cache-Parameter) zählt über den Unterblattindex auf; ohne diese Form ist nur
/// die erste Cache-Ebene sichtbar. Dieselbe RBX-Begründung wie bei [`cpuid`].
pub fn cpuid_count(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    // SAFETY: wie [`cpuid`]; `__cpuid_count` kennt dieselbe Registerbeschränkung.
    let r = unsafe { core::arch::x86_64::__cpuid_count(leaf, sub) };
    (r.eax, r.ebx, r.ecx, r.edx)
}

// --- Identität / Privilegstufe -------------------------------------------------------------

/// Aktuelle Privilegstufe (CPL) — das x86-Gegenstück zum aarch64-Exception-Level.
///
/// Damit die arch-neutrale Bedeutung erhalten bleibt („0 = User, höher = privilegierter"
/// auf ARM; auf x86 ist es genau umgekehrt), meldet diese Funktion **wie auf ARM**:
/// Ring 0 -> `1` (Kernel), Ring 3 -> `0` (User). Sie wird nur für Diagnoseausgaben benutzt.
pub fn current_el() -> u8 {
    let cs: u16;
    // SAFETY: reines Lesen des CS-Selektors, keine Speicherwirkung.
    unsafe { asm!("mov {0:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
    if cs & 0b11 == 3 {
        0
    } else {
        1
    }
}

/// Logische Kern-ID = **LAPIC-ID** (`CPUID.1:EBX[31:24]`).
///
/// Auf QEMU `pc`/`q35` sind die LAPIC-IDs dicht ab 0 vergeben, entsprechen also direkt dem
/// Kernindex (wie MPIDR.Aff0 auf QEMU `virt`). Für Plattformen mit dünn besetzten APIC-IDs
/// wäre hier eine ACPI-MADT-gestützte Abbildung nötig (s. `power::cpu_on`).
pub fn core_id() -> usize {
    let (_, ebx, _, _) = cpuid(1);
    (ebx >> 24) as usize
}

/// Vollständige Kern-Identität (aarch64: MPIDR-Affinität) — hier die LAPIC-ID.
pub fn mpidr_affinity() -> u64 {
    core_id() as u64
}

/// **Read the SMT topology** (Z6 stage 0) from `CPUID.1Fh`, falling back to `CPUID.0Bh`.
///
/// `1Fh` first because it is the V2 enumeration and supersedes `0Bh` where both exist (it adds
/// die/module levels); the SMT level itself is identical in both, so the fallback is exact rather
/// than approximate.
///
/// **Not `CPUID.4:EAX[25:14]`**, although that value is already sitting in a register inside
/// `cache::for_each_level` and would have been free: leaf 4 is Intel's *deterministic cache
/// parameters* leaf, and AMD carries topology in an entirely different place. `0Bh`/`1Fh` is
/// architectural on both vendors — and the likely deployment target is EPYC, i.e. the vendor where
/// the cheap route is the wrong one.
///
/// Anything unreadable ends as `Unknown`, never as `Single`. The caller treats that as worst case.
pub fn smt_topology() -> crate::smt::SmtTopology {
    // Acht Unterblaetter sind mehr als jede real gemeldete Topologie (SMT/Core/Module/Tile/Die);
    // der Abbruch bei Ebenentyp 0 ist der eigentliche Terminator, die Schranke nur das Netz.
    const MAX_SUB: usize = 8;
    let max_leaf = cpuid(0).0;
    for leaf in [0x1F_u32, 0x0B_u32] {
        if max_leaf < leaf {
            continue;
        }
        let mut subs = [(0u32, 0u32, 0u32, 0u32); MAX_SUB];
        let mut n = 0usize;
        while n < MAX_SUB {
            let r = cpuid_count(leaf, n as u32);
            subs[n] = r;
            n += 1;
            if (r.2 >> 8) & 0xFF == 0 {
                break; // Ebenentyp 0 -> Ende der Aufzaehlung
            }
        }
        let t = crate::smt::decode_topology_leaf(&subs[..n]);
        if t != crate::smt::SmtTopology::Unknown {
            return t;
        }
    }
    crate::smt::SmtTopology::Unknown
}

// --- Interrupt-Maske ------------------------------------------------------------------------

/// RFLAGS lesen.
#[inline(always)]
fn read_rflags() -> u64 {
    let f: u64;
    // SAFETY: `pushfq`+`pop` liest nur das Flags-Register (Stack wird ausgeglichen).
    unsafe { asm!("pushfq", "pop {}", out(reg) f, options(nomem, preserves_flags)) };
    f
}

/// RFLAGS.IF (Interrupt-Freigabe).
const RFLAGS_IF: u64 = 1 << 9;

/// Interrupt-Zustand **sichern + Interrupts maskieren**; gibt den vorherigen Zustand für
/// [`local_irq_restore`] zurück. Gegenstück zu DAIF auf aarch64.
pub fn local_irq_save() -> u64 {
    let f = read_rflags();
    // SAFETY: `cli` maskiert Interrupts am eigenen Kern; bewusste Low-Level-Operation.
    unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };
    f & RFLAGS_IF
}

/// Sind die Interrupts am eigenen Kern gerade **freigegeben** (RFLAGS.IF)?
///
/// **Wofuer das gebraucht wird** (C9b): wer mit maskierten IRQs hereinkommt, darf nicht darauf
/// warten, dass ein Halter auf DEMSELBEN Kern fertig wird — der laeuft erst weiter, wenn wir
/// zurueckkehren. Genau der reentrante Ticket-Deadlock, gegen den [`local_irq_save`] in
/// `SpinLock::lock` steht. Wer die Frage nicht stellen kann, muss unbedingt maskieren; wer sie
/// stellen kann, darf im harmlosen Fall warten statt Ausgabe zu verwuerfeln.
#[inline(always)]
pub fn irqs_freigegeben() -> bool {
    read_rflags() & RFLAGS_IF != 0
}

/// Den von [`local_irq_save`] gesicherten Zustand wiederherstellen.
pub fn local_irq_restore(state: u64) {
    if state & RFLAGS_IF != 0 {
        local_irq_enable();
    }
}

/// Interrupts am aktuellen Kern freigeben.
pub fn local_irq_enable() {
    // SAFETY: erlaubt asynchrone Interrupt-Auslieferung; bewusste Low-Level-Operation.
    unsafe { asm!("sti", options(nomem, nostack, preserves_flags)) };
}

/// Interrupts am aktuellen Kern maskieren.
pub fn local_irq_disable() {
    // SAFETY: maskiert Interrupts; bewusste Low-Level-Operation.
    unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };
}

// --- Barrieren / Cache ----------------------------------------------------------------------

/// Instruction Synchronization Barrier (aarch64 `isb`).
///
/// x86 hat keine explizite ISB; nach Änderungen an CR0/CR3/CR4 oder MSRs wirkt ein
/// **serialisierender** Befehl. `cpuid` ist der portable serialisierende Befehl.
pub fn isb() {
    let _ = cpuid(0);
}

/// Data Synchronization Barrier (aarch64 `dsb sy`).
pub fn dsb_sy() {
    // SAFETY: reine Speicherbarriere.
    unsafe { asm!("mfence", options(nostack, preserves_flags)) };
}

/// Frisch geschriebenen **Code** kohärent zur Ausführung machen.
///
/// Auf x86 ist der Instruktions-Cache gegenüber Datenschreibzugriffen **hardware-kohärent**
/// (selbstmodifizierender Code braucht nur einen serialisierenden Befehl vor der Ausführung,
/// den ohnehin jeder Sprung in neuen Code über `iretq`/`sysret` mitbringt). Anders als auf
/// aarch64 sind hier also **keine** Cache-Wartungsinstruktionen nötig.
pub fn sync_code_range(_base: usize, _len: usize) {
    isb();
}

// --- Spekulations-Härtung (ext-29) ----------------------------------------------------------

/// **CSDB**-Äquivalent: `lfence` sperrt die spekulative Weiterverwendung noch nicht
/// aufgelöster Ladeergebnisse — auf x86 die etablierte Spectre-v1-Barriere.
pub fn csdb() {
    // SAFETY: reine Barriere ohne Speicherwirkung.
    unsafe { asm!("lfence", options(nostack, preserves_flags)) };
}

/// Spekulationssicherer Array-Index (arithmetische Maske + [`csdb`]) — identisch zur
/// aarch64-Fassung; der Aufrufer prüft die Grenze weiterhin architektonisch.
#[inline(always)]
pub fn array_index_nospec(index: usize, len: usize) -> usize {
    let mask = (((index as u64).wrapping_sub(len as u64) as i64) >> 63) as u64;
    csdb();
    index & (mask as usize)
}

/// Spekulationsbarriere an einer Vertrauensgrenze (Adressraumwechsel). Auf x86 ist ein
/// `mov cr3` bereits serialisierend; `lfence` schließt zusätzlich offene Spekulation ab.
pub fn speculation_barrier() {
    csdb();
}

/// FEAT_SB-Äquivalent: `lfence` gibt es auf jeder x86_64-CPU.
pub fn sb_supported() -> bool {
    true
}

/// `IA32_ARCH_CAPABILITIES` (MSR 0x10A) — meldet HW-seitige Immunitäten.
const MSR_ARCH_CAPABILITIES: u32 = 0x0000_010A;
/// Bit 0: RDCL_NO — die HW ist gegen Rogue-Data-Cache-Load (Meltdown) immun.
const ARCH_CAP_RDCL_NO: u64 = 1 << 0;
/// Bit 1: IBRS_ALL — verbesserte IBRS (Branch-Predictor kontext-isoliert).
const ARCH_CAP_IBRS_ALL: u64 = 1 << 1;

/// Meldet das MSR `IA32_ARCH_CAPABILITIES` (`CPUID.7.0:EDX[29]`), sonst `0`.
fn arch_capabilities() -> u64 {
    let (_, _, _, edx) = cpuid(7);
    if edx & (1 << 29) == 0 {
        return 0;
    }
    // SAFETY: das MSR existiert genau dann, wenn CPUID.7.0:EDX[29] gesetzt ist (eben geprüft).
    unsafe { rdmsr(MSR_ARCH_CAPABILITIES) }
}

/// aarch64-`CSV2`-Äquivalent: Branch-Predictor über Kontexte nicht ausnutzbar (IBRS_ALL).
pub fn csv2() -> u8 {
    u8::from(arch_capabilities() & ARCH_CAP_IBRS_ALL != 0)
}

/// aarch64-`CSV3`-Äquivalent: kein spekulativer Seitenkanal auf unzugänglichen Speicher
/// (Meltdown-immun, `RDCL_NO`).
pub fn csv3() -> u8 {
    u8::from(arch_capabilities() & ARCH_CAP_RDCL_NO != 0)
}

// --- Warten / Anhalten ------------------------------------------------------------------------

/// Auf einen Interrupt warten (Low-Power) — aarch64 `wfi`.
pub fn wfi() {
    // SAFETY: `hlt` hält bis zum nächsten Interrupt an.
    unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
}

/// Kern für immer anhalten.
pub fn halt() -> ! {
    loop {
        local_irq_disable();
        wfi();
    }
}

/// **Läuft dieser Kernel unter einem Hypervisor?** (`CPUID.1:ECX[31]`)
///
/// Architektonisch reserviert und von jedem verbreiteten Hypervisor gesetzt; auf echter Hardware
/// ist das Bit 0. Der Wert wird gecacht — `cpuid` ist unter KVM ein bedingungsloser VM-Exit und
/// hat in einem heißen Pfad schon einmal 3556 statt 51 Zyklen gekostet.
///
/// **Wofür das gebraucht wird — und wofür nicht.** Nicht, um Verhalten umzuschalten. Sondern für
/// eine Aussage, die ein Gast *prinzipiell nicht treffen kann*: Seitenfärbung wirkt über die
/// **physische** Adresse. Unter einem Hypervisor färbt der Gast gastphysische Adressen, und die
/// zweite Übersetzungsstufe bildet jede 4-KiB-Seite auf eine beliebige Wirtsseite ab — genau die
/// Bits oberhalb des Seitenoffsets, aus denen die Farbe besteht, überleben das nicht. Ein
/// Prime+Probe im Gast misst deshalb nicht die eigene Färbung (s. `kernel/src/colors.rs`, B-4.5).
pub fn hypervisor_present() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    static CACHED: AtomicU8 = AtomicU8::new(0); // 0 = unbekannt, 1 = nein, 2 = ja
    match CACHED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let v = cpuid(1).2 & (1 << 31) != 0;
            CACHED.store(if v { 2 } else { 1 }, Ordering::Relaxed);
            v
        }
    }
}
