#![no_std]
//! `sel4lake-hal` — **Hardware-Abstraktion**, architekturselektiv.
//!
//! Kapselt alles Architekturabhängige (CPU-Register, MMU, Exceptions, Interrupt-Controller,
//! Timer, Power/SMP). Sämtliches `unsafe` liegt hier in den von der Projektregel erlaubten
//! Domänen und ist einzeln begründet.
//!
//! ## Struktur (ext-31)
//!
//! Bis ext-30 war diese Crate **aarch64-only**: die Module lagen direkt in `src/` und
//! benutzten aarch64-Assembler. Für den x86_64-Port sind sie nach `src/aarch64/` gewandert
//! und werden per `cfg(target_arch)` ausgewählt; daneben steht `src/x86_64/` mit derselben
//! **öffentlichen API**. Der Kernel-Kern (Caps, Scheduler, IPC, PDs) enthält deshalb
//! **keinerlei** `cfg(target_arch)` — er sieht nur `hal::…`.
//!
//! ```text
//!   sel4lake-hal
//!    ├── aarch64/   console cpu exception fp intc(GICv2) mmu power(PSCI) timer syscall
//!    │              + pcie smmu virtio   (ARM-/QEMU-`virt`-spezifisch, ext-22..24)
//!    └── x86_64/    console cpu exception fp intc(LAPIC)  mmu power       timer syscall
//! ```
//!
//! ### Namen
//!
//! Die Modulnamen sind **arch-neutral**: `intc` (Interrupt-Controller: GICv2 bzw. LAPIC),
//! `power` (PSCI bzw. ACPI-/QEMU-Abschaltung + SMP-Start). Auf aarch64 bleiben `gic`/`psci`
//! zusätzlich als Alias sichtbar, damit ARM-spezifischer Gerätecode unverändert bleibt.
//!
//! ### Was der x86_64-Port (noch) nicht hat
//!
//! `pcie`/`smmu`/`virtio` sind ARM-/QEMU-`virt`-spezifisch (SMMUv3, ECAM-Fenster des
//! `virt`-Boards). Ihre x86-Entsprechungen (PCI-ECAM über ACPI-MCFG, VT-d/AMD-Vi) sind ein
//! eigener Schritt; bis dahin sind sie **aarch64-only** und der Kernel gated die zugehörigen
//! Subsysteme.

pub(crate) mod hook;

// --- Architekturauswahl -------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
#[path = "aarch64/mod.rs"]
mod imp;

#[cfg(target_arch = "x86_64")]
#[path = "x86_64/mod.rs"]
mod imp;

// Gemeinsame API-Fläche beider Architekturen.
pub use imp::{console, cpu, exception, fp, intc, mmu, power, syscall, timer};

// ARM-/QEMU-`virt`-spezifische Geräte (noch ohne x86-Entsprechung, s. Modul-Doku).
#[cfg(target_arch = "aarch64")]
pub use imp::{gic, pcie, psci, smmu, virtio};

// x86-spezifisch: Segmentierung existiert auf ARM nicht (dort gibt es keine GDT/TSS).
#[cfg(target_arch = "x86_64")]
pub use imp::{acpi, gdt, pcie, vtd};

/// Formatierte Ausgabe auf der Debug-Konsole (gesperrt, SMP-sicher).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

/// Wie [`print!`], mit Zeilenumbruch.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
