//! Kernel-Glue des **generischen Binary-Loaders** (ext-26, [ADR 0011](../../docs/adr/0011-binary-loader.md)).
//!
//! Die **reine**, bounds-geprüfte Parse-Logik (Boot-Archiv, ab L1 Minimal-ELF64) liegt im Crate
//! `sel4lake-loader` (0 `unsafe`, host-getestet). Hier liegt der **privilegierte** Teil, der RAM
//! liest und (ab L1) Segmente in Regionen kopiert, W^X mappt, VSpace/PD anlegt, Caps endowt und
//! Threads spawnt — alles über die bestehenden `system::`-Primitive (keine neuen Sonderrechte).
//!
//! **L0:** das Boot-Archiv aus dem reservierten RAM-Fenster lesen + die Module melden.

use sel4lake_cap::CapPtr;
use sel4lake_hal::{print, println};
use sel4lake_loader::archive::Archive;
use sel4lake_loader::elf::ElfImage;
use sel4lake_loader::{LoaderError, Program, DOMAIN_TRUSTED, DOMAIN_USERLAND};
use sel4lake_microkit::Domain;
use sel4lake_sched::ThreadId;

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

/// **Die öffentliche Loader-API** (ADR 0011, Verfeinerung 1): ein quellen-agnostisches `Program`
/// laden + starten. Parst das ELF (Safe Rust, [`ElfImage::parse`]), bildet die Domäne ab und
/// delegiert den privilegierten Teil (Segmente kopieren/mappen, VSpace/PD, Spawn) an
/// [`crate::system::load_elf`]. `endow` = initiale Caps für die neue PD (`(Slot, Cap)`), in L2 aus
/// dem Manifest abgeleitet. Gibt `(ThreadId, pd)` der neuen, laufbereiten PD.
///
/// L1: nur **isolierte EL0-Domänen** (UserLand/HardwareLand). TrustedSAS (EL1) ist signatur-gegatet
/// (L3) und wird hier abgelehnt (`UnsupportedDomain`).
pub fn load_image(prog: &Program, endow: &[(usize, CapPtr)]) -> Result<(ThreadId, usize), LoaderError> {
    if !verify_image(prog) {
        return Err(LoaderError::Unverified); // EL1/TrustedSAS ohne Signatur -> abgelehnt
    }
    let domain = match prog.domain {
        DOMAIN_USERLAND => Domain::UserLand,
        // HardwareLand braucht eine vor-erstellte Backend-PD (Partner-Bindung + Kanal) ->
        // ueber `load_program_into_pd`, NICHT hier (eine bare HardwareLand-PD bricht domain_audit).
        _ => return Err(LoaderError::UnsupportedDomain),
    };
    let img = ElfImage::parse(prog.elf)?; // Safe-Rust-Validierung; unsafe erst im Kopier-Glue
    crate::system::load_elf(&img, domain, endow).ok_or(LoaderError::NoResources)
}

/// Ein Programm in eine **vor-erstellte** PD laden (ext-26, L3) — fuer HardwareLand-Backends
/// (Partner-Bindung + Kanal vom Aufrufer aufgesetzt) und kuenftige spezialisierte PDs. Die
/// Domaenen-Policy traegt die PD selbst (`install_cap_checked` + `domain_audit`). Trust-Gate
/// ([`verify_image`]) gilt auch hier.
pub fn load_program_into_pd(
    prog: &Program,
    pd: usize,
    endow: &[(usize, CapPtr)],
) -> Result<ThreadId, LoaderError> {
    if !verify_image(prog) {
        return Err(LoaderError::Unverified);
    }
    let img = ElfImage::parse(prog.elf)?;
    crate::system::load_into_pd(&img, pd, endow).ok_or(LoaderError::NoResources)
}

/// **Trust-/Signatur-Gate** (ADR 0011 §7): darf dieses Image geladen werden? **EL1/TrustedSAS** ist
/// privilegierter Code in der globalen SAS — extern geladen unterlaeuft er das SIP-Modell und ist
/// daher **nur signiert** ladbar. Die Signaturpruefung ist noch nicht implementiert; bis dahin wird
/// EL1-Laden **abgelehnt** (`prog.hash` ist der vorbereitete Hook). **EL0** (UserLand/HardwareLand)
/// ist hardware-isoliert (ein fehlerhaftes/boesartiges Image faultet nur sich selbst) -> ohne
/// Signatur ladbar.
fn verify_image(prog: &Program) -> bool {
    prog.domain != DOMAIN_TRUSTED
}

/// `SYS_LOAD`-Callback (ext-26, L2): das Programm mit Index `index` aus dem Boot-Archiv laden +
/// die `endow`-Caps (vom Dispatch aus dem Aufrufer-Cspace delegiert) in die neue PD endowen. Gibt
/// die neue PD-Id. Der Dispatch hat die `Loader`-Cap-Autoritaet bereits geprueft.
pub fn load_by_index(index: u32, endow: &[(usize, CapPtr)]) -> Option<usize> {
    let archive = read_archive()?;
    let prog = archive.program(index as usize)?;
    load_image(&prog, endow).ok().map(|(_, pd)| pd)
}
