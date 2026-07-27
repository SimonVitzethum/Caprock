//! **x86_64-Implementierung** der HAL (QEMU `pc`/`q35`, LAPIC, Multiboot).
//!
//! Spiegelbild zu `../aarch64/`: dieselbe öffentliche API, andere Hardware. Was die
//! Architekturen bewusst **gleich** halten, steht in den Modulen selbst — vor allem das
//! Trap-Modell (`exception`), das den arch-neutralen Scheduler überhaupt erst möglich macht.
//!
//! Nicht vorhanden (bewusst, s. Crate-Doku): `pcie`/`smmu`/`virtio` — deren x86-Entsprechungen
//! (PCI-ECAM über ACPI-MCFG, VT-d/AMD-Vi) sind ein eigener Portierungsschritt.

pub mod console;
pub mod cpu;
pub mod exception;
pub mod fp;
pub mod gdt;
pub mod intc;
pub mod mmu;
pub mod power;
pub mod syscall;
pub mod timer;
