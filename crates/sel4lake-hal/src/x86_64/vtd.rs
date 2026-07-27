//! **Intel VT-d** (DMA Remapping) — das x86-Gegenstück zum SMMUv3-Treiber auf ARM.
//!
//! Zweck ist derselbe: ein bus-masterndes Gerät greift **an der CPU-MMU vorbei** direkt auf
//! physischen Speicher zu. Die IOMMU stellt eine zweite Übersetzungsstufe davor, sodass ein
//! Gerät nur erreicht, was ihm der Kernel ausdrücklich zugeteilt hat.
//!
//! ## Was hier steht — und was (noch) nicht
//!
//! Implementiert ist der **Bring-up mit Default-Block**: Registerbasis aus der ACPI-DMAR,
//! Root-Tabelle anlegen (alle Einträge „not present"), `RTADDR` setzen, `SRTP`, dann `TE` —
//! ab da ist die Übersetzung aktiv und **jede** nicht ausdrücklich erlaubte DMA-Anforderung
//! wird geblockt. Das ist exakt die Aussage, die der `smmu`-Test auf ARM prüft
//! („Default-Abort, CR0-Enable, CMD_SYNC-Round-Trip"), und es ist der sicherheitsrelevante
//! Teil: **ohne Zuteilung geht nichts**.
//!
//! **Nicht** implementiert sind die per-Gerät-Zuteilungen (Kontext-Einträge + Second-Level-
//! Tabellen je Domäne). `attach` meldet deshalb ehrlich `false`: auf x86 lässt sich derzeit
//! kein DMA-Puffer an ein Gerät binden — statt eine Isolation vorzutäuschen, die es nicht gibt.
//! Der Zustand ist trotzdem **sicher**: geblockt statt ungeschützt.

use core::sync::atomic::{AtomicU64, Ordering};

// Registeroffsets (Intel VT-d Spezifikation, Kapitel 10).
const REG_VER: usize = 0x000;
const REG_CAP: usize = 0x008;
const REG_ECAP: usize = 0x010;
const REG_GCMD: usize = 0x018;
const REG_GSTS: usize = 0x01C;
const REG_RTADDR: usize = 0x020;
const REG_CCMD: usize = 0x028;

/// Global Command/Status: Übersetzung aktivieren bzw. aktiv.
const GCMD_TE: u32 = 1 << 31;
const GSTS_TES: u32 = 1 << 31;
/// Set Root Table Pointer.
const GCMD_SRTP: u32 = 1 << 30;
const GSTS_RTPS: u32 = 1 << 30;
/// Context-Cache: globale Invalidierung anfordern / Fertigmeldung.
const CCMD_ICC: u64 = 1 << 63;
const CCMD_CIRG_GLOBAL: u64 = 1 << 61;

/// Registerbasis der Remapping-Einheit (`0` = keine gefunden).
static UNIT_BASE: AtomicU64 = AtomicU64::new(0);
/// Physische Adresse der Root-Tabelle (`0` = noch keine).
static ROOT_TABLE: AtomicU64 = AtomicU64::new(0);

fn read32(off: usize) -> u32 {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: `b` stammt aus der ACPI-DMAR und liegt im identity-gemappten, uncacheable
    // MMIO-Bereich; volatile Registerzugriffe aliasen keinen Rust-Speicher.
    unsafe { core::ptr::read_volatile((b + off as u64) as *const u32) }
}
fn write32(off: usize, v: u32) {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: wie `read32`.
    unsafe { core::ptr::write_volatile((b + off as u64) as *mut u32, v) };
}
fn read64(off: usize) -> u64 {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: wie `read32`.
    unsafe { core::ptr::read_volatile((b + off as u64) as *const u64) }
}
fn write64(off: usize, v: u64) {
    let b = UNIT_BASE.load(Ordering::Acquire);
    // SAFETY: wie `read32`.
    unsafe { core::ptr::write_volatile((b + off as u64) as *mut u64, v) };
}

/// Ist eine Remapping-Einheit vorhanden (ACPI-DMAR gefunden)?
pub fn present() -> bool {
    UNIT_BASE.load(Ordering::Acquire) != 0
}

/// Versionsregister (Diagnose/Test).
pub fn version() -> u32 {
    if present() {
        read32(REG_VER)
    } else {
        0
    }
}

/// Capability-Register (Diagnose/Test).
pub fn cap() -> u64 {
    if present() {
        read64(REG_CAP)
    } else {
        0
    }
}

/// Extended-Capability-Register (Diagnose/Test).
pub fn ecap() -> u64 {
    if present() {
        read64(REG_ECAP)
    } else {
        0
    }
}

/// Ist die Übersetzung aktiv (`GSTS.TES`)? Genau dann blockt die Einheit alles, was nicht
/// ausdrücklich zugeteilt ist.
pub fn enabled() -> bool {
    present() && read32(REG_GSTS) & GSTS_TES != 0
}

/// Registerbasis aus der **ACPI-DMAR** übernehmen (idempotent).
pub fn discover() -> bool {
    if present() {
        return true;
    }
    let Some(base) = super::acpi::dmar_unit_base() else {
        return false;
    };
    UNIT_BASE.store(base, Ordering::Release);
    true
}

/// Auf ein Statusbit warten (begrenzt — ein Hardware-Poll darf im Kernel nie unbegrenzt laufen).
fn wait_status(mask: u32, want_set: bool) -> bool {
    for _ in 0..1_000_000 {
        let set = read32(REG_GSTS) & mask != 0;
        if set == want_set {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// **Bring-up mit Default-Block.**
///
/// `root_table` ist ein vom Kernel geliefertes, **genulltes** 4-KiB-Frame: eine Root-Tabelle,
/// deren 256 Einträge alle „not present" sind. Genau das ist die Default-Block-Politik — jede
/// DMA-Anforderung eines Geräts läuft in einen nicht vorhandenen Kontext und wird abgewiesen.
///
/// Gibt `true` zurück, wenn die Übersetzung danach aktiv ist (`GSTS.TES`).
pub fn init(root_table: u64) -> bool {
    if !discover() {
        return false;
    }
    ROOT_TABLE.store(root_table, Ordering::Release);
    // Root-Table-Pointer setzen (Bits 63:12; Translation Table Mode = Legacy).
    write64(REG_RTADDR, root_table & !0xfff);
    write32(REG_GCMD, GCMD_SRTP);
    if !wait_status(GSTS_RTPS, true) {
        return false;
    }
    invalidate_context_cache();
    // Übersetzung aktivieren.
    write32(REG_GCMD, GCMD_TE);
    wait_status(GSTS_TES, true)
}

/// Globale Invalidierung des Kontext-Caches (Gegenstück zum `CMD_SYNC`-Round-Trip auf ARM):
/// die Einheit muss die Anforderung quittieren, indem sie `ICC` wieder löscht.
pub fn invalidate_context_cache() -> bool {
    if !present() {
        return false;
    }
    write64(REG_CCMD, CCMD_ICC | CCMD_CIRG_GLOBAL);
    for _ in 0..1_000_000 {
        if read64(REG_CCMD) & CCMD_ICC == 0 {
            return true; // quittiert
        }
        core::hint::spin_loop();
    }
    false
}
