//! Binary-Loader-Parser (ext-26) — die **reine**, bounds-geprüfte Lese-Logik des Loaders.
//!
//! Hier liegt **kein** `unsafe` (`#![forbid(unsafe_code)]`) und keine Kernel-/Hardware-Abhängigkeit:
//! der Code arbeitet ausschließlich auf `&[u8]`-Slices. Damit ist er per Host-`cargo test`
//! vollständig verifizier- und fuzzbar (ADR 0011, Verfeinerung 2). Der **privilegierte** Teil
//! (validierte Segmente in Regionen kopieren, W^X mappen, VSpace/PD anlegen, Caps endowen, spawnen)
//! lebt getrennt im Kernel-Glue (`kernel/src/loader.rs`).
//!
//! **Quellen-agnostisch (ADR 0011, Verfeinerung 3):** der Loader-Kern arbeitet mit dem
//! [`Program`]-Deskriptor (stabile Metadaten + Roh-Image-Bytes). Das **Boot-Archiv**
//! ([`mod archive`](archive)) ist nur **eine** Quelle; künftige Quellen (Dateisystem, Flash,
//! Netzwerk) liefern denselben [`Program`] — die Loader-API (`loader::load_image`) bleibt unverändert.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod archive;
pub mod cert;
pub mod elf;

/// Zieldomäne eines Programms (Manifest/Quellen-Feld). Bewusst kernel-agnostisch (u32); der
/// Kernel-Glue bildet das auf `microkit::Domain` ab.
pub const DOMAIN_TRUSTED: u32 = 0;
pub const DOMAIN_HARDWARE: u32 = 1;
pub const DOMAIN_USERLAND: u32 = 2;

/// Parse-Fehler. Jeder Pfad, der eine fehlerhafte Eingabe erkennt, endet hier — **nie** in einem
/// Out-of-Bounds-Zugriff oder Panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoaderError {
    /// Datenpuffer kürzer als der Header / ein Eintrag.
    TooSmall,
    /// Falsche Magic (kein/kaputtes Archiv).
    BadMagic,
    /// Nicht unterstützte Version.
    BadVersion,
    /// Ein Offset/Länge liegt außerhalb der Quelle.
    OutOfBounds,
    /// Unplausible Eintragszahl (Tabelle passt nicht in `total_len`).
    BadCount,
    /// Ungültiges/nicht unterstütztes ELF-Image (Magic/Klasse/Maschine/Typ falsch, `memsz<filesz`,
    /// unerwartete Program-Header-Größe).
    BadElf,
    /// Domäne (noch) nicht über diesen Pfad ladbar (z. B. HardwareLand braucht eine vor-erstellte
    /// Backend-PD; TrustedSAS/EL1 ist signatur-gegatet).
    UnsupportedDomain,
    /// Image nicht verifiziert: TrustedSAS-Code ohne gültiges Zertifikat (ext-28, ADR 0014):
    /// fehlendes/abgelehntes Zertifikat, ungültige Signatur, unbekannte Key-ID, Hash-Mismatch
    /// oder Downgrade. UserLand/HardwareLand sind hiervon **nicht** betroffen.
    Unverified,
    /// Zertifikat-Parse-Fehler (falsche Länge/Magic/Formatversion) — siehe [`cert`].
    BadCert,
    /// Kernel-Ressourcen erschöpft (VSpace/ASID/RAM/TCB/PD) beim Laden.
    NoResources,
}

/// Ein **quellen-agnostischer** ladbarer Programm-Deskriptor: stabile Metadaten + die Roh-Bytes des
/// Programm-Images (ELF) und des Manifests (bereits bounds-validiert, gefahrlos lesbar). Heute vom
/// Boot-Archiv produziert; jede künftige Quelle liefert denselben `Program`.
#[derive(Clone, Copy)]
pub struct Program<'a> {
    /// **Stabile** numerische ID — überdauert Namensänderungen (für Hot-Reload, Logs, Debugging).
    pub program_id: u32,
    name: &'a [u8],
    /// Programm-Version (monoton; für Versionsverwaltung / Hot-Reload).
    pub version: u32,
    /// Zieldomäne (`DOMAIN_*`).
    pub domain: u32,
    /// Image-Hash — reserviert für die spätere Signatur-/Integritätsprüfung (ADR 0011 §7).
    pub hash: [u8; 32],
    /// Das Programm-Image (ELF64).
    pub elf: &'a [u8],
    /// Das Manifest (Cap-Endowment etc.; ab L2 interpretiert).
    pub manifest: &'a [u8],
    /// Das TrustedSAS-Zertifikat (ext-28, ADR 0014) — leer (`&[]`), falls keines vorliegt. Nur für
    /// `DOMAIN_TRUSTED` erforderlich + geprüft; UserLand/HardwareLand ignorieren es.
    pub cert: &'a [u8],
}

impl<'a> Program<'a> {
    /// Aus beliebiger Quelle konstruieren (Archiv, Dateisystem, …).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        program_id: u32,
        name: &'a [u8],
        version: u32,
        domain: u32,
        hash: [u8; 32],
        elf: &'a [u8],
        manifest: &'a [u8],
        cert: &'a [u8],
    ) -> Self {
        Self { program_id, name, version, domain, hash, elf, manifest, cert }
    }

    /// Der Name als `&str` (bis zum ersten NUL bzw. Ende), nicht-UTF8 → `"?"`.
    pub fn name(&self) -> &str {
        let end = self.name.iter().position(|&c| c == 0).unwrap_or(self.name.len());
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

/// Ein `[off, off+len)`-Sub-Slice von `data`, bounds-geprüft (Overflow-sicher). `len==0` → leerer
/// Slice (gültig). Von den Quellen-Parsern (z. B. [`archive`]) genutzt.
pub(crate) fn slice_within(data: &[u8], off: usize, len: usize) -> Result<&[u8], LoaderError> {
    let end = off.checked_add(len).ok_or(LoaderError::OutOfBounds)?;
    data.get(off..end).ok_or(LoaderError::OutOfBounds)
}
