//! Architektur-spezifischer Boot-Glue.
//!
//! Bleibt im Kernel-Binary (referenziert Linker-Symbole und `kernel_main`).
//! Alles übrige CPU-Spezifische liegt in `sel4lake-hal`.

#[cfg(target_arch = "aarch64")]
mod aarch64;
