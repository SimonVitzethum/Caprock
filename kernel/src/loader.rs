//! Kernel-Glue des **generischen Binary-Loaders** (ext-26, [ADR 0011](../../docs/adr/0011-binary-loader.md)).
//!
//! Die **reine**, bounds-geprüfte Parse-Logik (Boot-Archiv, ab L1 Minimal-ELF64) liegt im Crate
//! `sel4lake-loader` (0 `unsafe`, host-getestet). Hier liegt der **privilegierte** Teil, der RAM
//! liest und (ab L1) Segmente in Regionen kopiert, W^X mappt, VSpace/PD anlegt, Caps endowt und
//! Threads spawnt — alles über die bestehenden `system::`-Primitive (keine neuen Sonderrechte).
//!
//! **L0:** das Boot-Archiv aus dem reservierten RAM-Fenster lesen + die Module melden.

use sel4lake_hal::{print, println};
use sel4lake_loader::archive::Archive;

/// Größe des reservierten RAM-Fensters für das Boot-Archiv (oben in RAM, vom `PhysAllocator`
/// ausgenommen — siehe `init_mem`-Aufruf in `main.rs`). QEMU legt das Archiv per
/// `-device loader,addr=MOD_BASE` hierher; der Wert MUSS zu `test-qemu.sh` passen.
pub const MOD_WINDOW: u64 = 0x0100_0000; // 16 MiB

/// Basis des Archiv-Fensters = `ram_end - MOD_WINDOW` für die QEMU-`virt`-Maschine mit 4 GiB RAM
/// (RAM `0x4000_0000` + 4 GiB = `0x1_4000_0000`). Liegt in der statisch identity-gemappten
/// Normal-WB-Region (GiB 2..9), also EL1-lesbar.
pub const MOD_BASE: u64 = 0x0001_4000_0000 - MOD_WINDOW; // 0x1_3F00_0000

/// Das Boot-Archiv aus dem reservierten Fenster lesen + **vollständig validieren**. `None`, wenn
/// kein gültiges Archiv vorliegt (fehlend/beschädigt) — der Kernel läuft dann ohne externe Module.
pub fn read_archive() -> Option<Archive<'static>> {
    // SAFETY: `[MOD_BASE, MOD_BASE+MOD_WINDOW)` ist reserviertes, statisch identity-gemapptes
    // Normal-RAM (vom PhysAllocator ausgenommen, s. `system::init_mem`-Aufruf). Nur **lesender**
    // Zugriff; der Parser (`sel4lake-loader`) ist vollständig bounds-geprüft und panik-frei.
    let bytes = unsafe { core::slice::from_raw_parts(MOD_BASE as *const u8, MOD_WINDOW as usize) };
    Archive::parse(bytes).ok()
}

/// **L0-Selbsttest/Telemetrie:** das Archiv lesen + die gefundenen Module melden. Greift NICHT in
/// den Boot-Ablauf ein (reines Lesen); das eigentliche Laden folgt ab L1.
pub fn probe() {
    match read_archive() {
        Some(a) => {
            print!("archive : {} Modul(e):", a.count());
            for p in a.iter() {
                // Stabile program_id (Verfeinerung 4) + Name + Version melden.
                print!(" [{}]{}@v{}", p.program_id, p.name(), p.version);
            }
            println!(" -> ALL PASS");
        }
        None => println!("archive : kein gueltiges Boot-Archiv (0 Module, FAILURES)"),
    }
}
