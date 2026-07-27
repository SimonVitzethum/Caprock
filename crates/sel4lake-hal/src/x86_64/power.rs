//! Power- und **SMP**-Schnittstelle (x86_64) — API-gleich zu PSCI auf aarch64.
//!
//! ## Warum ein Trampolin nötig ist
//!
//! Auf ARM startet ein Sekundärkern per PSCI-Firmwareaufruf **direkt** in EL1/64 Bit an einer
//! beliebigen Adresse — `cpu_on(target, entry, stack)` genügt. Auf x86 gibt es keine solche
//! Firmware-Schnittstelle: ein Application Processor (AP) startet nach `INIT`-`SIPI`-`SIPI` im
//! **16-bit-Real-Mode** an `CS = Vektor << 8, IP = 0`, also unterhalb von 1 MiB. Der Weg in den
//! Long Mode muss deshalb als Code **dort unten** liegen:
//!
//! ```text
//!   SIPI(0x08) -> 0x8000  16 Bit: GDT laden, CR0.PE            -> 32 Bit
//!                         32 Bit: CR3 (BSP-Tabellen), CR4.PAE,
//!                                 EFER.LME|NXE, CR0.PG          -> 64 Bit
//!                         64 Bit: eigenen Stack laden, in den Rust-Einstieg springen
//! ```
//!
//! Das Trampolin ist auf eine **feste Basis** ([`TRAMPOLINE_BASE`]) assembliert: alle absoluten
//! Sprungziele und GDT-Zeiger sind Konstanten `BASE + (Label - Start)`. Der BSP kopiert den
//! Block dorthin und trägt vorher CR3, Stack und Einstiegspunkt in den Parameterblock ein.

use super::cpu::{inb, outb};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Erfolgsstatus (aarch64: `PSCI_SUCCESS`).
pub const SUCCESS: i64 = 0;
/// Der Kern hat sich nicht gemeldet / SMP nicht möglich.
pub const NOT_SUPPORTED: i64 = -1;

/// Ladeadresse des AP-Trampolins. Muss < 1 MiB und **4-KiB-ausgerichtet** sein: die SIPI
/// überträgt nur die Seitennummer als Vektor (`0x8000 >> 12 == 0x08`).
pub const TRAMPOLINE_BASE: u64 = 0x8000;
const SIPI_VECTOR: u32 = (TRAMPOLINE_BASE >> 12) as u32;

core::arch::global_asm!(
    r#"
.section .aptramp, "ax"
.balign 16
.globl __ap_tramp_start
.globl __ap_tramp_end
.globl __ap_param_cr3
.globl __ap_param_stack
.globl __ap_param_entry

/* Der Linker legt diesen Block an AP_BASE (s. kernel/x86_64-link.ld) -> alle Labels sind
   bereits absolute Laufadressen. Nur der 16-bit-Teil rechnet segmentrelativ (ds = cs). */
.set AP_BASE, 0x8000

__ap_tramp_start:
.code16
    cli
    cld
    mov ax, cs                       /* SIPI setzt CS = Vektor<<8, IP = 0 -> ds = cs */
    mov ds, ax
    lgdt [gdt32_ptr - AP_BASE]   /* ds = cs -> Offset innerhalb des Segments */
    mov eax, cr0
    or  eax, 1                       /* CR0.PE: Protected Mode */
    mov cr0, eax
    /* Far Jump nach 32 Bit, als Bytes kodiert (0x66 = 32-bit-Operand): LLVMs Intel-Syntax
       hat für `ljmp seg:off` keine verlässliche Schreibweise. */
    .byte 0x66, 0xEA
    .long ap_prot32
    .word 0x08

.code32
ap_prot32:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax

    mov eax, [__ap_param_cr3]
    mov cr3, eax                     /* dieselben Seitentabellen wie der BSP */
    mov eax, cr4
    or  eax, 1 << 5                  /* CR4.PAE */
    mov cr4, eax
    mov ecx, 0xC0000080              /* EFER */
    rdmsr
    or  eax, (1 << 8) | (1 << 11)    /* LME (Long Mode) + NXE (NX-Bit gültig) */
    wrmsr
    mov eax, cr0
    or  eax, 1 << 31                 /* CR0.PG -> Long Mode aktiv */
    mov cr0, eax

    lgdt [gdt64_ptr]
    /* Bootstrap-Stack: der Far Return unten PUSHT — ohne gesetztes ESP landete das an einer
       zufälligen Adresse (nach dem Reset ist ESP = 0, der Push liefe über die 4-GiB-Grenze).
       Der eigentliche Kernel-Stack kommt erst im 64-bit-Teil aus dem Parameterblock. */
    lea esp, [ap_boot_stack_top]
    /* 32 -> 64 Bit per Far Return (wie im BSP-Trampolin; ein `EA`-Far-Jump ist ab hier
       ungültig, weil die CPU bereits im Compatibility-Submodus des Long Mode ist). */
    push 0x08
    lea eax, [ap_long64]
    push eax
    retf

.code64
ap_long64:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax
    mov rsp, [__ap_param_stack]
    mov rax, [__ap_param_entry]
    jmp rax

/* ---- GDTs im Trampolin: die des Kernels liegen oberhalb von 1 MiB und sind vor dem
   Paging-Start für den AP nicht adressierbar. ---- */
.balign 8
gdt32:
    .quad 0x0000000000000000
    .quad 0x00CF9A000000FFFF         /* 32-bit Code, Basis 0, Limit 4 GiB */
    .quad 0x00CF92000000FFFF         /* 32-bit Data */
gdt32_ptr:
    .word 23
    .long gdt32

.balign 8
gdt64:
    .quad 0x0000000000000000
    .quad 0x00209A0000000000         /* 64-bit Code (L-Bit) */
    .quad 0x0000920000000000         /* Data */
gdt64_ptr:
    .word 23
    .quad gdt64

__ap_tramp_end:

/* ---- Parameterblock: vom BSP vor dem Start gefüllt. EIGENE Seite (RW, NX) — die Codeseite
   oben bleibt R-X. Sonst müsste die Trampolin-Seite zugleich schreibbar und ausführbar sein
   und bräche W^X. ---- */
.section .aptramp.data, "aw"
.balign 8
.globl __ap_data_start
.globl __ap_data_end
__ap_data_start:
/* Bootstrap-Stack für den Far Return in den Long Mode (s. o.). */
.balign 16
ap_boot_stack: .skip 64
ap_boot_stack_top:
__ap_param_cr3:   .quad 0
__ap_param_stack: .quad 0
__ap_param_entry: .quad 0
__ap_data_end:
"#
);

extern "C" {
    /// Ladeadressen (LMA) der beiden Trampolin-Blöcke im Kernel-Image — von dort kopiert der
    /// BSP sie einmalig an ihre Laufadressen (s. `kernel/x86_64-link.ld`).
    static __aptramp_lma: u8;
    static __aptramp_data_lma: u8;
    static __ap_tramp_start: u8;
    static __ap_tramp_end: u8;
    static __ap_data_start: u8;
    static __ap_data_end: u8;
    static __ap_param_cr3: u8;
    static __ap_param_stack: u8;
    static __ap_param_entry: u8;
}

/// Zähler der Kerne, die den Rust-Einstieg erreicht haben.
static ONLINE: AtomicUsize = AtomicUsize::new(0);
/// Meldet, dass dieser Kern den Rust-Einstieg erreicht hat (vom AP-Einstieg zu rufen).
pub fn ap_report_online() {
    ONLINE.fetch_add(1, Ordering::Release);
}

/// Anzahl der Kerne, die sich gemeldet haben.
pub fn online_count() -> usize {
    ONLINE.load(Ordering::Acquire)
}

/// Grobe Verzögerung über den (immer vorhandenen) PIT-Kanal 2 — für die von der Spezifikation
/// geforderten Wartezeiten zwischen INIT und SIPI. Ohne Interrupts, rein pollend.
fn delay_us(us: u32) {
    const PIT_HZ: u64 = 1_193_182;
    let ticks = ((PIT_HZ * us as u64) / 1_000_000).clamp(1, 0xFFFF) as u16;
    // SAFETY: Port-I/O auf die architektonisch festen PIT-/Gate-Ports.
    unsafe {
        outb(0x61, (inb(0x61) & !0x02) | 0x01); // Gate an, Lautsprecher aus
        outb(0x43, 0xB0); // Kanal 2, lobyte/hibyte, Modus 0 (one-shot)
        outb(0x42, ticks as u8);
        outb(0x42, (ticks >> 8) as u8);
        // Begrenzt: ein Hardware-Poll darf im Kernel nie unbegrenzt laufen — sonst haengt der
        // Boot bei abweichender Chipsatz-Emulation still (genau das ist hier passiert).
        let mut guard = 0u32;
        while inb(0x61) & 0x20 == 0 {
            guard += 1;
            if guard > 20_000_000 {
                break;
            }
            core::hint::spin_loop();
        }
    }
}

/// Wurde das Trampolin schon an seine Laufadresse kopiert?
static TRAMPOLINE_READY: AtomicUsize = AtomicUsize::new(0);

/// Das Trampolin von seiner Lade- an seine Laufadresse kopieren (einmalig).
///
/// Der Block ist auf [`TRAMPOLINE_BASE`] **assembliert** (alle Labels darin sind absolute
/// Laufadressen), liegt im Image aber an einer anderen Stelle — sonst zwänge ein eigenes
/// Ladesegment unter 1 MiB den Multiboot-Header aus den ersten 8 KiB der Datei.
fn install_trampoline() -> bool {
    // SAFETY: reine Adressabfragen von Linker-Symbolen.
    let (vma, lma, end) = unsafe {
        (
            core::ptr::addr_of!(__ap_tramp_start) as u64,
            core::ptr::addr_of!(__aptramp_lma),
            core::ptr::addr_of!(__ap_tramp_end) as u64,
        )
    };
    if vma != TRAMPOLINE_BASE {
        return false; // Linker-Skript und SIPI-Vektor passen nicht zusammen
    }
    if TRAMPOLINE_READY.swap(1, Ordering::AcqRel) == 0 {
        // SAFETY: Quellen sind die zusammenhängenden Blöcke im Kernel-Image (LMA), Ziele die
        // festen, identity-gemappten Low-Memory-Seiten. Sie liegen unterhalb des Kernel-Images
        // und werden vom Allokator nie vergeben (der beginnt bei 16 MiB).
        // Die Codeseite ist im Kernel-Mapping RW+NX (sie liegt unterhalb des Images). Zum
        // Kopieren ist das richtig; danach wird sie R-X, damit der AP dort nach `CR0.PG`
        // weiterlaufen kann, ohne dass die Seite zugleich schreibbar wäre (W^X).
        unsafe {
            core::ptr::copy_nonoverlapping(lma, TRAMPOLINE_BASE as *mut u8, (end - vma) as usize);
            let dstart = core::ptr::addr_of!(__ap_data_start) as u64;
            let dend = core::ptr::addr_of!(__ap_data_end) as u64;
            core::ptr::copy_nonoverlapping(
                core::ptr::addr_of!(__aptramp_data_lma),
                dstart as *mut u8,
                (dend - dstart) as usize,
            );
        }
        super::mmu::protect_page(TRAMPOLINE_BASE, super::mmu::Perm::Rx);
    }
    true
}

/// Einen Parameter im Trampolin-Block setzen (der BSP schreibt, der AP liest ihn nach dem Start).
fn set_param(sym: *const u8, value: u64) {
    // SAFETY: `sym` ist ein Symbol im `.aptramp`-Block — identity-gemappt und beschreibbar
    // (Sektion `awx`, unterhalb des Kernel-Images, vom Allokator nie vergeben).
    unsafe { core::ptr::write_volatile(sym as *mut u64, value) };
}

/// Einen weiteren Kern starten (aarch64: PSCI `CPU_ON`).
///
/// `target` ist die **LAPIC-ID** des Zielkerns, `entry_point` eine 64-bit-`extern "C" fn() -> !`,
/// `context_id` der Stack-Top dieses Kerns (gleiche Rollenverteilung wie auf ARM).
///
/// Ablauf nach Intel-Spezifikation: `INIT`, warten, dann **zwei** `SIPI` — die Wiederholung ist
/// vorgesehen für Kerne, die die erste verpassen.
pub fn cpu_on(target: u64, entry_point: u64, context_id: u64) -> i64 {
    if !install_trampoline() {
        return NOT_SUPPORTED;
    }
    let cr3: u64;
    // SAFETY: reines Lesen von CR3 (die Tabellen des BSP, die der AP übernimmt).
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags))
    };
    set_param(core::ptr::addr_of!(__ap_param_cr3), cr3);
    set_param(core::ptr::addr_of!(__ap_param_stack), context_id);
    set_param(core::ptr::addr_of!(__ap_param_entry), entry_point);

    let before = online_count();
    let apic_id = target as u32;
    super::intc::send_init_ipi(apic_id);
    delay_us(10_000);
    super::intc::send_startup_ipi(apic_id, SIPI_VECTOR);
    delay_us(200);
    super::intc::send_startup_ipi(apic_id, SIPI_VECTOR);

    // Auf die Meldung des Kerns warten — begrenzt, damit ein nicht vorhandener Kern den Boot
    // nicht aufhält.
    for _ in 0..200 {
        if online_count() > before {
            return SUCCESS;
        }
        delay_us(1_000);
    }
    NOT_SUPPORTED
}

/// System abschalten.
///
/// QEMU (`pc`/`q35`) hört auf den ACPI-Power-Management-Port: ein Wort `0x2000` auf `0x604`
/// löst „soft off" aus. Auf echter Hardware wäre der Port aus der ACPI-FADT zu lesen; für den
/// QEMU-Testlauf ist der feste Port die Entsprechung zu PSCI `SYSTEM_OFF`.
pub fn system_off() -> ! {
    // SAFETY: Port-I/O auf den ACPI-PM1a-Control-Port von QEMU; ein Schreibzugriff schaltet ab.
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") 0x604u16, in("ax") 0x2000u16,
                         options(nomem, nostack, preserves_flags));
        core::arch::asm!("out dx, ax", in("dx") 0xB004u16, in("ax") 0x2000u16,
                         options(nomem, nostack, preserves_flags));
    }
    super::cpu::halt()
}
