//! **LXPD-Boot-Transport** (Strang Boot): Treiber-Images, die der Bootloader als
//! Multiboot-Module liefert, beim Hochlauf prüfen und zuordnen.
//!
//! ## Was hier liegt — und was nicht
//!
//! Diese Datei prüft die **Hülle**: Magic, Zähler, Stub-Inhalte, Trailer, Längen. Jeder
//! Fehler ist benannt ([`LxpdBootError`]), kein Pfad panikt, kein Pfad liest ausserhalb
//! des übergebenen Bereichs (nur `.get()`, nur geprüfte Arithmetik).
//!
//! Was hier **nicht** liegt, und warum:
//!
//! * **Das Format gehört dem Format-Strang** (`crates/caprock-lxpd`, dazu `tools/lx_driver*`).
//!   Die Konstanten unten sind daraus **gelesen** (Werte und Bedeutung identisch, Namen
//!   angeglichen) — fliesst das Format weiter, fliesst die Änderung von dort hierher,
//!   niemals umgekehrt. Eine zweite Wahrheit über dieselben Bytes wäre genau die Form,
//!   an der dieses Projekt schon mehrfach Stunden verloren hat.
//! * **Die Manifest-Seite** (`BadManifest`/`BadSignature`/`BadGrants` dort) liegt hier nicht:
//!   Der v1-Container enthält **kein** Manifest — die JSON-Bytes kommen separat als Datei
//!   herein und werden vom Format-Strang geprüft. Was der Boot über einen Treiber weiss
//!   (welches Modul, welcher Hash, welche Politik), steht im **System-Manifest** und wird
//!   in `loader.rs` aufgelöst, nicht hier.
//! * **Die ELF-Regeln** liegen in `caprock-loader` (`ElfImage::parse`). Ein als ELF
//!   erkanntes Modul wird an ihnen gemessen, nicht an dieser Hülle — deshalb meldet diese
//!   Datei für ELF-Eingaben [`LxpdBootError::NotLxpd`] statt „kaputtes LXPD".
//!
//! ## Die drei Auflösungsfragen (Antworten trägt der Hook in `loader.rs`)
//!
//! * **Welches Modul?** Modul 0 ist das Boot-Archiv (Verabredung mit dem Bootloader, s.
//!   `set_archive_span`). Die Module dahinter meldet der Bring-up über
//!   [`set_lxpd_module_span`] an (derselbe Mechanismus, eine Tabelle weiter). Der Join-Key
//!   zwischen Manifest-Eintrag und Modul ist der **Hash** (s. unten) — nicht die
//!   Position: Der Bootloader sagt über die Reihenfolge nichts zu.
//! * **Hash prüfen womit?** `sha256(Modulbytes) == Eintrag.sha256` — dieselbe Bindung wie
//!   beim Root-Task (`start_root_task`, Schritt 3): Das Manifest sagt nicht nur, *dass*
//!   etwas geladen wird, sondern *was*.
//! * **Signatur gegen welche Keys?** Gegen [`crate::manifest_keys::MANIFEST_KEYS`]. Der Hook
//!   liest ausschliesslich über `read_manifest()` — also erst nach Algorithmus, Key-DB,
//!   Signatur über die gesamte Nachricht, Kernel-Bindung und Anti-Downgrade. Es gibt keinen
//!   zweiten Weg zu einem Eintrag.
//!
//! ## Abhängigkeitsfrei mit Absicht
//!
//! Nur `core` — kein `caprock-lxpd`, kein `alloc`. Damit lässt sich diese Datei per
//! `rustc --test` auf dem Host prüfen (Parser gegen Byte-Literale, gut/böse Container),
//! ohne den Kernel zu bauen:
//!
//! ```sh
//! rustc --test --edition 2021 -O kernel/src/lxpd_boot.rs -o /tmp/lxpd_boot_test && /tmp/lxpd_boot_test
//! ```

use core::sync::atomic::{AtomicU64, Ordering};

// --- Hüllen-Konstanten (aus `crates/caprock-lxpd` GELESEN, s. Kopf) -------------------------------

/// Magic am Anfang eines LXPD-v1-Containers (4 Bytes `"LXPD"`).
pub const LXPD_MAGIC: &[u8; 4] = b"LXPD";
/// Magic am Anfang jedes 32-Byte-Trampolin-Stubs (`"LXTR"`).
pub const LXTR_MAGIC: &[u8; 4] = b"LXTR";
/// End-Marker am Schluss des Containers (`"LXEND"`).
pub const LXEND_MARKER: &[u8; 5] = b"LXEND";
/// ELF-Magic (`0x7F 'E' 'L' 'F'`) — entscheidet, ob der ELF-Pfad greift.
pub const ELF_MAGIC: &[u8; 4] = &[0x7F, b'E', b'L', b'F'];
/// Breite eines Trampolin-Stubs in Bytes (`LXTR` + id + 2×FNV32 + NOP-Padding).
pub const STUB_LEN: usize = 32;
/// Kopfbreite: Magic(4) + tramp_count(4) + rewired_count(4) + section_size(8).
pub const HEADER_LEN: usize = 20;
/// Harte Schranke der Stubzahl — eine Schranke hier ist billiger als eine Schleife über
/// eine fremde `u32`.
pub const MAX_TRAMPS: usize = 256;
/// NOP-Padding der Stub-Hinterhälfte (`[16..32]`).
const NOP_PAD: [u8; 16] = [0x90; 16];

/// **Multiboot-Module neben dem Archiv.** Der Bootloader liefert höchstens 8 Module
/// (`arch::x86_64::multiboot::MAX_MODULES`, dort gezählt, nicht hier); Modul 0 ist das
/// Boot-Archiv. Übrig bleiben sieben Plätze — die Schranke ist hergeleitet, nicht erfunden.
pub const LXPD_MAX_MODULES: usize = 7;

// --- Fehler --------------------------------------------------------------------------------------------------------

/// Jeder Pfad, der eine fehlerhafte Eingabe erkennt, endet hier — **nie** in einem
/// Out-of-Bounds-Zugriff oder Panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LxpdBootError {
    /// Bereich kürzer als die angekündigte Struktur.
    TooSmall,
    /// Keine `LXPD`-Magic am Anfang.
    BadMagic,
    /// Zähler unplausibel (über [`MAX_TRAMPS`], `section_size != count*32`, Überlänge)
    /// oder Stub-Index ausserhalb der Zahl.
    BadCounts,
    /// Korrupter Stub — trägt die beanstandete Stub-ID (bzw. den Index, falls die Magic
    /// schon fehlt und keine ID lesbar ist).
    BadStub(u32),
    /// `LXEND`-Trailer fehlt oder steht falsch.
    BadEnd,
    /// **Kein LXPD-Container, sondern ein ELF.** Kein Fehler im Sinne von „kaputt", sondern
    /// die Weiche: Diese Eingabe gehört auf den ELF-Pfad (`bind_elf`-Form, ladbar über die
    /// Loader-Regeln), nicht auf die Hüllenprüfung.
    NotLxpd,
}

impl LxpdBootError {
    /// Fester Fehlertext (für `BadStub` s. [`core::fmt::Display`]).
    pub fn as_str(&self) -> &'static str {
        match self {
            LxpdBootError::TooSmall => "lxpd-boot: image ends before the announced structure",
            LxpdBootError::BadMagic => "lxpd-boot: missing LXPD magic",
            LxpdBootError::BadCounts => "lxpd-boot: trampoline counts do not match the image",
            LxpdBootError::BadStub(_) => "lxpd-boot: corrupt trampoline stub",
            LxpdBootError::BadEnd => "lxpd-boot: missing LXEND trailer",
            LxpdBootError::NotLxpd => "lxpd-boot: ELF image, not an LXPD container (ELF path)",
        }
    }
}

impl core::fmt::Display for LxpdBootError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LxpdBootError::BadStub(id) => write!(f, "{}: stub {}", self.as_str(), id),
            _ => f.write_str(self.as_str()),
        }
    }
}

// --- Kopf und Bild -------------------------------------------------------------------------------------------------

/// Geparster Container-Kopf — Zähler ohne Speicher (kein `alloc`, kein Kopieren).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LxpdHeader {
    /// Zahl der 32-Byte-Trampolin-Stubs.
    pub tramp_count: u32,
    /// Zahl der umverdrahteten Einträge (Zähler ohne eigene Bytes im Layout).
    pub rewired_count: u32,
    /// Angekündigte Stubbereichsgrösse — muss `tramp_count * 32` sein.
    pub section_size: u64,
}

fn rd_u32(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn rd_u64(d: &[u8], off: usize) -> Option<u64> {
    let b = d.get(off..off + 8)?;
    Some(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

/// Den Kopf zählen/validieren, ohne etwas zu speichern: Magic, Längen, Zählerkonsistenz.
/// Die Stub-Inhalte prüft [`LxpdBild::parse`].
pub fn parse_header(data: &[u8]) -> Result<LxpdHeader, LxpdBootError> {
    if data.len() < LXPD_MAGIC.len() {
        return Err(LxpdBootError::TooSmall);
    }
    if is_elf(data) {
        return Err(LxpdBootError::NotLxpd);
    }
    if data.get(..LXPD_MAGIC.len()) != Some(&LXPD_MAGIC[..]) {
        return Err(LxpdBootError::BadMagic);
    }
    if data.len() < HEADER_LEN {
        return Err(LxpdBootError::TooSmall);
    }
    let tramp_count = rd_u32(data, 4).ok_or(LxpdBootError::TooSmall)?;
    let rewired_count = rd_u32(data, 8).ok_or(LxpdBootError::TooSmall)?;
    let section_size = rd_u64(data, 12).ok_or(LxpdBootError::TooSmall)?;
    if tramp_count as usize > MAX_TRAMPS {
        return Err(LxpdBootError::BadCounts);
    }
    let want_section =
        (tramp_count as u64).checked_mul(STUB_LEN as u64).ok_or(LxpdBootError::BadCounts)?;
    if section_size != want_section {
        return Err(LxpdBootError::BadCounts);
    }
    Ok(LxpdHeader { tramp_count, rewired_count, section_size })
}

/// Ein validierter LXPD-v1-Container: der geprüfte Kopf. Hält keine Bytes — der Aufrufer
/// besitzt die Eingabe ohnehin; zurückgegeben wird der Stempel, dass sie vollständig
/// validiert wurde (Kopf, exakte Gesamtlänge, je Stub Magic + ID-Folge + NOP-Padding,
/// `LXEND`-Trailer).
#[derive(Clone, Copy, Debug)]
pub struct LxpdBild {
    header: LxpdHeader,
}

impl LxpdBild {
    /// Vollständig validieren: Kopf, exakte Gesamtlänge (`20 + n*32 + 5`, kein Byte mehr
    /// oder weniger), je Stub Magic + ID-Folge + NOP-Padding, `LXEND`-Trailer.
    pub fn parse(data: &[u8]) -> Result<Self, LxpdBootError> {
        let header = parse_header(data)?;
        let n = header.tramp_count as usize;
        let stubs_len = n.checked_mul(STUB_LEN).ok_or(LxpdBootError::BadCounts)?;
        let total = HEADER_LEN
            .checked_add(stubs_len)
            .and_then(|t| t.checked_add(LXEND_MARKER.len()))
            .ok_or(LxpdBootError::BadCounts)?;
        if data.len() < total {
            return Err(LxpdBootError::TooSmall);
        }
        if data.len() > total {
            // Anhängsel hinter dem Trailer sind kein „Straffungsspielraum", sondern Bytes,
            // über die niemand etwas zusagt — fail-closed.
            return Err(LxpdBootError::BadCounts);
        }
        for i in 0..n {
            let base = HEADER_LEN + i * STUB_LEN; // kein Overflow: `i <= n <= 256`
            let stub = data.get(base..base + STUB_LEN).ok_or(LxpdBootError::TooSmall)?;
            if stub.get(..LXTR_MAGIC.len()) != Some(&LXTR_MAGIC[..]) {
                return Err(LxpdBootError::BadStub(i as u32));
            }
            let id = rd_u32(stub, 4).ok_or(LxpdBootError::TooSmall)?;
            if id as usize != i {
                return Err(LxpdBootError::BadStub(id));
            }
            if stub.get(16..STUB_LEN) != Some(&NOP_PAD[..]) {
                return Err(LxpdBootError::BadStub(id));
            }
        }
        let tail = HEADER_LEN + stubs_len;
        if data.get(tail..tail + LXEND_MARKER.len()) != Some(&LXEND_MARKER[..]) {
            return Err(LxpdBootError::BadEnd);
        }
        Ok(LxpdBild { header })
    }

    /// Zahl der Trampolin-Stubs.
    pub fn tramp_count(&self) -> usize {
        self.header.tramp_count as usize
    }

    /// Zahl der umverdrahteten Einträge (Zähler ohne eigene Bytes).
    pub fn rewired_count(&self) -> u32 {
        self.header.rewired_count
    }
}

/// Trägt `data` die LXPD-Magic?
pub fn is_lxpd(data: &[u8]) -> bool {
    data.get(..LXPD_MAGIC.len()) == Some(&LXPD_MAGIC[..])
}

/// Trägt `data` die ELF-Magic?
pub fn is_elf(data: &[u8]) -> bool {
    data.get(..ELF_MAGIC.len()) == Some(&ELF_MAGIC[..])
}

// --- Modul-Spannen (Multiboot-Module 1.., derselbe Mechanismus wie `set_archive_span`) ------------------------------

/// Basen der gemeldeten LXPD-Module (`0` = nicht gemeldet).
static LXPD_BASE: [AtomicU64; LXPD_MAX_MODULES] =
    [const { AtomicU64::new(0) }; LXPD_MAX_MODULES];
/// Längen dazu (`0` = nicht gemeldet).
static LXPD_LEN: [AtomicU64; LXPD_MAX_MODULES] = [const { AtomicU64::new(0) }; LXPD_MAX_MODULES];

/// Ein Bootloader-Modul als LXPD-Quelle melden. Muss **vor** der ersten Benutzung laufen und
/// beschreibt einen Bereich, den der `PhysAllocator` nicht vergeben darf (der Bring-up
/// schneidet alle Module vor der ersten Allokation aus der Freiliste aus).
///
/// `idx` zählt ab dem **ersten Modul hinter dem Archiv** (Multiboot-Modul 1 → `idx` 0).
/// Gibt `false`, wenn `idx` ausserhalb der Tabelle liegt — eine Absage am Aufruf, kein
/// stilles Verwerfen.
///
/// Noch ohne Aufrufer: Die Bring-up-Verdrahtung (B-Seite, `bringup.rs`) folgt als Nächstes
/// und gibt diese Funktion per `pub use` in `loader.rs` frei — bis dahin das `allow` unten,
/// nicht ein synthetischer Aufruf.
#[allow(dead_code)]
pub fn set_lxpd_module_span(idx: usize, base: u64, len: u64) -> bool {
    let (Some(b), Some(l)) = (LXPD_BASE.get(idx), LXPD_LEN.get(idx)) else {
        return false;
    };
    b.store(base, Ordering::Relaxed);
    l.store(len, Ordering::Relaxed);
    true
}

/// Die gemeldete Lage des Moduls `idx` (`None` = nicht gemeldet).
pub fn lxpd_module_span(idx: usize) -> Option<(u64, u64)> {
    let base = LXPD_BASE.get(idx)?.load(Ordering::Relaxed);
    let len = LXPD_LEN.get(idx)?.load(Ordering::Relaxed);
    (base != 0 && len != 0).then_some((base, len))
}

/// Die Bytes des Moduls `idx` (`None` = nicht gemeldet).
pub fn lxpd_module_bytes(idx: usize) -> Option<&'static [u8]> {
    let (base, len) = lxpd_module_span(idx)?;
    // SAFETY: `[base, base+len)` ist ein vom Bootloader gemeldeter Modulbereich, der vor der
    // ersten Allokation aus der Freiliste ausgeschnitten wurde (derselbe Vertrag wie bei
    // `set_archive_span`/`read_archive` in `loader.rs`). Nur **lesender** Zugriff; der Parser
    // oben ist vollständig bounds-geprüft und panik-frei.
    let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, len as usize) };
    Some(bytes)
}

/// Wie viele LXPD-Modul-Spannen gemeldet sind (Bericht, keine Autorität).
pub fn lxpd_module_gemeldet() -> usize {
    (0..LXPD_MAX_MODULES).filter(|&i| lxpd_module_span(i).is_some()).count()
}

// --- Host-Tests (Byte-Literale, gut/böse) ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv32(s: &str) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for b in s.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    /// Ein Stub exakt nach `lx-rewrite::stub_for`: `LXTR` + id + FNV32(family) +
    /// FNV32(source) + `0x90`-Padding.
    fn stub(id: u32, family: &str, source: &str) -> [u8; STUB_LEN] {
        let mut s = [0x90u8; STUB_LEN];
        s[0..4].copy_from_slice(b"LXTR");
        s[4..8].copy_from_slice(&id.to_le_bytes());
        s[8..12].copy_from_slice(&fnv32(family).to_le_bytes());
        s[12..16].copy_from_slice(&fnv32(source).to_le_bytes());
        s
    }

    /// Gültiger Container mit 2 Stubs: 20 + 64 + 5 = 89 Bytes.
    fn gut() -> [u8; 89] {
        let mut v = [0u8; 89];
        v[0..4].copy_from_slice(b"LXPD");
        v[4..8].copy_from_slice(&2u32.to_le_bytes());
        v[8..12].copy_from_slice(&0u32.to_le_bytes()); // rewired_count
        v[12..20].copy_from_slice(&64u64.to_le_bytes());
        v[20..52].copy_from_slice(&stub(0, "dma", "dma_map_single"));
        v[52..84].copy_from_slice(&stub(1, "spin", "spin_lock"));
        v[84..89].copy_from_slice(b"LXEND");
        v
    }

    #[test]
    fn gut_vollstaendig() {
        let raw = gut();
        let h = parse_header(&raw).unwrap();
        assert_eq!(h.tramp_count, 2);
        assert_eq!(h.rewired_count, 0);
        assert_eq!(h.section_size, 64);
        let img = LxpdBild::parse(&raw).unwrap();
        assert_eq!(img.tramp_count(), 2);
        assert_eq!(img.rewired_count(), 0);
        assert!(is_lxpd(&raw));
        assert!(!is_elf(&raw));
    }

    #[test]
    fn boese_magic() {
        let mut raw = gut();
        raw[0] = b'X';
        assert_eq!(LxpdBild::parse(&raw).unwrap_err(), LxpdBootError::BadMagic);
        assert_eq!(parse_header(&[]).unwrap_err(), LxpdBootError::TooSmall);
        assert_eq!(parse_header(b"LXP").unwrap_err(), LxpdBootError::TooSmall);
        assert!(!is_lxpd(&raw));
    }

    #[test]
    fn elf_ist_kein_lxpd() {
        // ELF-Magic → Weiche, nicht „kaputtes LXPD".
        let mut raw = [0u8; 64];
        raw[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
        assert!(is_elf(&raw));
        assert!(!is_lxpd(&raw));
        assert_eq!(parse_header(&raw).unwrap_err(), LxpdBootError::NotLxpd);
        assert_eq!(LxpdBild::parse(&raw).unwrap_err(), LxpdBootError::NotLxpd);
        assert_eq!(LxpdBootError::NotLxpd.as_str(), "lxpd-boot: ELF image, not an LXPD container (ELF path)");
    }

    #[test]
    fn boese_zaehler() {
        let raw = gut();
        // section_size lügt (63 statt 64).
        let mut a = raw;
        a[12..20].copy_from_slice(&63u64.to_le_bytes());
        assert_eq!(LxpdBild::parse(&a).unwrap_err(), LxpdBootError::BadCounts);
        // Zähler lügt (3 angekündigt + Sektion passend dazu, 2 vorhanden) → zu kurz.
        let mut b = raw;
        b[4..8].copy_from_slice(&3u32.to_le_bytes());
        b[12..20].copy_from_slice(&96u64.to_le_bytes());
        assert_eq!(LxpdBild::parse(&b).unwrap_err(), LxpdBootError::TooSmall);
        // Absurde Zahl über der Schranke (Stub-Zahl ≤ MAX).
        let mut d = raw;
        d[4..8].copy_from_slice(&10_000u32.to_le_bytes());
        assert_eq!(LxpdBild::parse(&d).unwrap_err(), LxpdBootError::BadCounts);
        assert_eq!(parse_header(&d).unwrap_err(), LxpdBootError::BadCounts);
        // Abgeschnittener Kopf.
        assert_eq!(LxpdBild::parse(&raw[..10]).unwrap_err(), LxpdBootError::TooSmall);
    }

    #[test]
    fn boese_ueberlaenge() {
        // Anhängsel hinter dem Trailer → zu lang (fail-closed, kein Straffungsspielraum).
        let mut v = [0u8; 90];
        v[..89].copy_from_slice(&gut());
        v[89] = 0x00;
        assert_eq!(LxpdBild::parse(&v).unwrap_err(), LxpdBootError::BadCounts);
    }

    #[test]
    fn boese_stub_magic() {
        let mut raw = gut();
        raw[20 + 32] = b'X'; // zweiter Stub, Magic kaputt
        assert_eq!(LxpdBild::parse(&raw).unwrap_err(), LxpdBootError::BadStub(1));
    }

    #[test]
    fn boese_stub_folge() {
        // IDs vertauscht: (1, 0) statt (0, 1).
        let mut raw = gut();
        raw[20..52].copy_from_slice(&stub(1, "dma", "dma_map_single"));
        raw[52..84].copy_from_slice(&stub(0, "spin", "spin_lock"));
        assert_eq!(LxpdBild::parse(&raw).unwrap_err(), LxpdBootError::BadStub(1));
        // Padding angenagt.
        let mut raw2 = gut();
        raw2[20 + 16] = 0x00;
        assert_eq!(LxpdBild::parse(&raw2).unwrap_err(), LxpdBootError::BadStub(0));
    }

    #[test]
    fn boese_ende() {
        let mut raw = gut();
        raw[84..89].copy_from_slice(b"LXENX");
        assert_eq!(LxpdBild::parse(&raw).unwrap_err(), LxpdBootError::BadEnd);
    }

    #[test]
    fn fehlertexte_benannt() {
        // Jeder Fehler hat einen festen Text — eine Absage ohne Namen ist keine Diagnose.
        assert_eq!(LxpdBootError::TooSmall.as_str(), "lxpd-boot: image ends before the announced structure");
        assert_eq!(LxpdBootError::BadMagic.as_str(), "lxpd-boot: missing LXPD magic");
        assert_eq!(LxpdBootError::BadCounts.as_str(), "lxpd-boot: trampoline counts do not match the image");
        assert_eq!(LxpdBootError::BadEnd.as_str(), "lxpd-boot: missing LXEND trailer");
        let s = alloc_fmt(LxpdBootError::BadStub(7));
        assert!(s.contains('7'));
    }

    /// `Display` ohne `alloc` prüfen: feste Ausgabe in einen Stapelpuffer.
    fn alloc_fmt(e: LxpdBootError) -> ArrayString {
        use core::fmt::Write;
        let mut s = ArrayString::new();
        let _ = write!(s, "{e}");
        s
    }

    struct ArrayString {
        buf: [u8; 64],
        len: usize,
    }

    impl ArrayString {
        fn new() -> Self {
            ArrayString { buf: [0u8; 64], len: 0 }
        }
        fn contains(&self, c: char) -> bool {
            self.buf[..self.len].contains(&(c as u8))
        }
    }

    impl core::fmt::Write for ArrayString {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                *self.buf.get_mut(self.len).ok_or(core::fmt::Error)? = b;
                self.len += 1;
            }
            Ok(())
        }
    }
}
