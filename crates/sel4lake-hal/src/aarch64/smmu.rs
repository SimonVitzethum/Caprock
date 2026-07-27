//! Minimaler **ARM SMMUv3**-Register-/Queue-Layer (ext-23, D2/D3) für QEMU `virt`.
//!
//! Kapselt das gesamte SMMU-spezifische Wissen (Register-Offsets, Command-/Event-Queue,
//! Stream-Table-Einträge, Stage-1-Deskriptoren). Der Kernel (`SmmuV3Enforcer`) ruft diese
//! Funktionen über die generische `DmaEnforcer`-Schnittstelle auf; **kein** anderer Teil des
//! Kernels kennt SMMU-Details. Reine Register-/Speicher-Operationen, keine Allokation (die
//! Queue-/Tabellen-RAM-Frames reicht der Aufrufer als Physadressen herein).
//!
//! Modell: **Stage-1, Default-Abort.** Eine lineare Stream-Tabelle (alle STEs zunächst V=0 ->
//! jeder Stream abortet); je gebundenem Gerät wird genau eine STE -> CD -> Stage-1-Tabelle
//! installiert, die ausschließlich die DMA-Region abbildet (D3). Geräte-DMA außerhalb ->
//! Translation-Fault -> Event-Queue.
//!
//! SMMUv3 `@0x0905_0000` (QEMU virt). Register-Offsets nach ARM IHI 0070.

use super::cpu;

pub const SMMU_BASE: u64 = 0x0905_0000;

// --- Register-Offsets (Page 0) ---
const IDR0: u64 = 0x000;
const IDR1: u64 = 0x004;
const IDR5: u64 = 0x014;
const CR0: u64 = 0x020;
const CR0ACK: u64 = 0x024;
const CR1: u64 = 0x028;
const CR2: u64 = 0x02c;
#[allow(dead_code)] // bewusst behalten (HW-Register/API-Vollstaendigkeit bzw. nur unter cfg(kani)/Feature genutzt)
const GBPA: u64 = 0x044;
const IRQ_CTRL: u64 = 0x050;
const GERROR: u64 = 0x060;
const GERRORN: u64 = 0x064;
const STRTAB_BASE: u64 = 0x080;
const STRTAB_BASE_CFG: u64 = 0x088;
const CMDQ_BASE: u64 = 0x090;
const CMDQ_PROD: u64 = 0x098;
const CMDQ_CONS: u64 = 0x09c;
const EVENTQ_BASE: u64 = 0x0a0;
// EVENTQ_PROD/CONS liegen in **Page 1** (Basis + 0x10000) — so im SMMUv3-Modell von QEMU.
const EVENTQ_PROD: u64 = 0x100a8;
const EVENTQ_CONS: u64 = 0x100ac;

// --- CR0-Bits ---
const CR0_SMMUEN: u32 = 1 << 0;
const CR0_EVENTQEN: u32 = 1 << 2;
const CR0_CMDQEN: u32 = 1 << 3;

// --- CMD-Opcodes (Word0[7:0]) ---
const CMD_CFGI_STE: u64 = 0x03;
const CMD_TLBI_NSNH_ALL: u64 = 0x30;
const CMD_SYNC: u64 = 0x46;

/// Größe (log2) der linearen Stream-Tabelle / Command-/Event-Queue (Einträge).
pub const LOG2_STRTAB: u32 = 9; // 512 StreamIDs (deckt RIDs hinter Root-Ports ab, z.B. Bus 1 = 0x100)
pub const LOG2_CMDQ: u32 = 7; //  128 Commands
pub const LOG2_EVENTQ: u32 = 7; // 128 Events
pub const STE_BYTES: u64 = 64; // 8 x u64
pub const CD_BYTES: u64 = 64;
const CMD_BYTES: u64 = 16; // 2 x u64
const EVT_BYTES: u64 = 32; // 4 x u64

#[inline]
fn r32(off: u64) -> u32 {
    // SAFETY: SMMU-Register sind global EL1-Device-gemappt (GiB 0).
    unsafe { core::ptr::read_volatile((SMMU_BASE + off) as *const u32) }
}
#[inline]
fn w32(off: u64, val: u32) {
    // SAFETY: s.o.; Device-Schreibzugriff auf ein SMMU-Register.
    unsafe { core::ptr::write_volatile((SMMU_BASE + off) as *mut u32, val) }
}
#[inline]
/// 64-bit-Register lesen (SMMU-MMIO).
fn r64(off: u64) -> u64 {
    // SAFETY: feste, EL1-Device-gemappte SMMU-Registerdatei; volatile MMIO.
    unsafe { core::ptr::read_volatile((SMMU_BASE + off) as *const u64) }
}

fn w64(off: u64, val: u64) {
    // SAFETY: s.o.; 64-bit-Register (BASE-Register) als ein Zugriff.
    unsafe { core::ptr::write_volatile((SMMU_BASE + off) as *mut u64, val) }
}

/// Rohe IDR-Werte (für Diagnose/Plausibilität).
pub fn idr0() -> u32 {
    r32(IDR0)
}
pub fn idr1() -> u32 {
    r32(IDR1)
}
pub fn idr5() -> u32 {
    r32(IDR5)
}
/// #StreamID-Bits aus IDR1.SIDSIZE[5:0].
pub fn sid_bits() -> u32 {
    idr1() & 0x3f
}
/// Ist eine SMMUv3 plausibel vorhanden? (IDR0 weder 0 noch all-1s.)
pub fn present() -> bool {
    let v = idr0();
    v != 0 && v != 0xffff_ffff
}

/// Ein 16-Byte-Command in die Command-Queue schreiben (Speicher, ohne PROD zu bumpen).
fn write_cmd(cmdq_phys: u64, index: u32, word0: u64, word1: u64) {
    let slot = cmdq_phys + (index as u64) * CMD_BYTES;
    // SAFETY: `cmdq_phys` ist kernel-allozierter, identity-gemappter RAM für die Command-Queue;
    // `index` < Queue-Größe (vom Aufrufer maskiert).
    unsafe {
        core::ptr::write_volatile(slot as *mut u64, word0);
        core::ptr::write_volatile((slot + 8) as *mut u64, word1);
    }
}

/// CR0-Bits setzen und auf CR0ACK warten (Spiegelregister). `true`, wenn die Bits innerhalb
/// des Poll-Limits gespiegelt werden.
fn set_cr0(bits: u32) -> bool {
    let cur = r32(CR0);
    w32(CR0, cur | bits);
    for _ in 0..100_000 {
        if r32(CR0ACK) & bits == bits {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// **SMMU-Bring-up** (D2): Command-/Event-Queue + lineare Stream-Tabelle programmieren, CR0
/// (CMDQEN|EVENTQEN, dann SMMUEN) aktivieren. Der Aufrufer übergibt die (genullten,
/// kontiguierlichen) RAM-Frames als Physadressen. Default-Abort: die Stream-Tabelle ist genullt
/// (alle STEs V=0) -> jeder Stream abortet, bis eine STE installiert wird (D3). Gibt `false`,
/// wenn keine SMMU vorhanden ist oder CR0ACK nicht spiegelt.
pub fn bringup(strtab_phys: u64, cmdq_phys: u64, eventq_phys: u64) -> bool {
    if !present() {
        return false;
    }
    // Sicherstellen, dass die Anfangsregister-Schreibvorgänge an die Queue-/Tabellen-RAM
    // (vom Aufrufer genullt) sichtbar sind.
    cpu::dsb_sy();

    // Lineare Stream-Tabelle: FMT=0 (linear), LOG2SIZE.
    w64(STRTAB_BASE, strtab_phys & 0x000f_ffff_ffff_ffc0);
    w32(STRTAB_BASE_CFG, LOG2_STRTAB); // FMT=0 in Bits[17:16]=0

    // Command-Queue: BASE | LOG2SIZE (untere 5 Bit), PROD=CONS=0.
    w64(CMDQ_BASE, (cmdq_phys & 0x000f_ffff_ffff_ffe0) | LOG2_CMDQ as u64);
    w32(CMDQ_PROD, 0);
    w32(CMDQ_CONS, 0);

    // Event-Queue: BASE | LOG2SIZE, PROD=CONS=0 (Page 1).
    w64(EVENTQ_BASE, (eventq_phys & 0x000f_ffff_ffff_ffe0) | LOG2_EVENTQ as u64);
    w32(EVENTQ_PROD, 0);
    w32(EVENTQ_CONS, 0);

    // CR1: Tabellen-/Queue-Speicher Inner-WB, Inner-Shareable (QEMU ignoriert Attrs funktional;
    // korrekt für reale HW). IRQ_CTRL = 0 (kein MSI; wir pollen).
    w32(CR1, (0b01 << 0) | (0b01 << 2) | (0b10 << 4) | (0b01 << 6) | (0b01 << 8) | (0b10 << 10));
    w32(CR2, 0);
    w32(IRQ_CTRL, 0);

    // Erst Queues aktivieren, dann die Übersetzung.
    if !set_cr0(CR0_CMDQEN | CR0_EVENTQEN) {
        return false;
    }
    if !set_cr0(CR0_SMMUEN) {
        return false;
    }
    true
}

/// Aktuelle CR0/CR0ACK-Werte (Diagnose).
pub fn cr0() -> u32 {
    r32(CR0)
}
pub fn cr0ack() -> u32 {
    r32(CR0ACK)
}
/// Ist die Übersetzung aktiv (CR0ACK.SMMUEN)?
pub fn enabled() -> bool {
    r32(CR0ACK) & CR0_SMMUEN != 0
}
/// GERROR-Status (0 = keine globalen Fehler).
pub fn gerror() -> u32 {
    r32(GERROR) ^ r32(GERRORN)
}

/// Ein Command in die Queue legen + PROD bumpen. `*cons_shadow`/Queue-Index wird vom Aufrufer
/// gehalten (`prod`). Gibt den neuen PROD-Index zurück (mit Wrap-Bit).
fn issue(cmdq_phys: u64, prod: u32, word0: u64, word1: u64) -> u32 {
    let mask = (1u32 << LOG2_CMDQ) - 1;
    let idx = prod & mask;
    write_cmd(cmdq_phys, idx, word0, word1);
    cpu::dsb_sy(); // Command sichtbar machen, bevor PROD geschrieben wird
    // PROD inkrementieren (Index + Wrap-Bit bei Überlauf).
    let wrap = prod & (1 << LOG2_CMDQ);
    let next_idx = (idx + 1) & mask;
    let next = if next_idx == 0 { (wrap ^ (1 << LOG2_CMDQ)) | next_idx } else { wrap | next_idx };
    w32(CMDQ_PROD, next);
    next
}

/// Auf das Leerlaufen der Command-Queue warten (CONS == PROD). `true` bei Erfolg.
fn wait_drained(prod: u32) -> bool {
    for _ in 0..1_000_000 {
        if r32(CMDQ_CONS) == prod {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// **CMD_SYNC-Round-Trip** (D2-Spike): einen SYNC-Command absetzen und auf das Einholen von
/// CONS warten — beweist, dass die Command-Queue-Mechanik funktioniert. `prod` ist der aktuelle
/// PROD-Index; gibt `(neuer_prod, ok)` zurück.
pub fn cmd_sync(cmdq_phys: u64, prod: u32) -> (u32, bool) {
    let next = issue(cmdq_phys, prod, CMD_SYNC, 0);
    (next, wait_drained(next))
}

/// Eine STE (8 x u64) in die Stream-Tabelle schreiben + `CMD_CFGI_STE`(sid) + `CMD_TLBI` +
/// `CMD_SYNC`, damit der SMMU die neue Konfiguration übernimmt (D3). Gibt `(neuer_prod, ok)`.
pub fn write_ste_and_sync(
    strtab_phys: u64,
    cmdq_phys: u64,
    mut prod: u32,
    sid: u32,
    ste: &[u64; 8],
) -> (u32, bool) {
    let slot = strtab_phys + (sid as u64) * STE_BYTES;
    // SAFETY: `strtab_phys` ist die kernel-allozierte lineare Stream-Tabelle; `sid` <
    // 2^LOG2_STRTAB (vom Aufrufer geprüft). Genau 8 u64 = ein STE-Eintrag.
    unsafe {
        for (i, &w) in ste.iter().enumerate() {
            core::ptr::write_volatile((slot + (i as u64) * 8) as *mut u64, w);
        }
    }
    cpu::dsb_sy();
    // CFGI_STE: Word0[7:0]=opcode, Word0[63:32]=StreamID. (Leaf=1 -> Bit32 im Word1? Im
    // SMMUv3 ist StreamID in Word0[63:32]; SSec/Leaf in Word1. Wir invalidieren die ganze STE.)
    prod = issue(cmdq_phys, prod, CMD_CFGI_STE | ((sid as u64) << 32), 0);
    prod = issue(cmdq_phys, prod, CMD_TLBI_NSNH_ALL, 0);
    prod = issue(cmdq_phys, prod, CMD_SYNC, 0);
    (prod, wait_drained(prod))
}

/// Eine STE invalidieren (auf V=0 zurücksetzen) + CFGI/TLBI/SYNC (D3, `disable_dma`).
pub fn clear_ste_and_sync(strtab_phys: u64, cmdq_phys: u64, prod: u32, sid: u32) -> (u32, bool) {
    write_ste_and_sync(strtab_phys, cmdq_phys, prod, sid, &[0u64; 8])
}

/// Nur **TLBI + SYNC** absetzen (ext-24): nach dem additiven Ein-/Aushängen einer Region in
/// einen bestehenden Kontext (die STE bleibt, nur die Stage-1-Tabelle änderte sich). Gibt
/// `(neuer_prod, ok)`.
pub fn tlbi_sync(cmdq_phys: u64, mut prod: u32) -> (u32, bool) {
    prod = issue(cmdq_phys, prod, CMD_TLBI_NSNH_ALL, 0);
    prod = issue(cmdq_phys, prod, CMD_SYNC, 0);
    (prod, wait_drained(prod))
}

// --- Stage-1-Übersetzung (D3): STE -> CD -> Stage-1-Pagetable ------------------------------
//
// Eine STE (Config = Stage-1) verweist auf einen **Context Descriptor** (CD); der CD enthält
// TTB0 (Basis der Stage-1-Pagetable), TCR (T0SZ/TG0/...) und MAIR. Die Stage-1-Tabelle bildet
// IOVA->PA ab. Die IOVA wird vom Kernel aus dem Fenster des Übersetzungskontexts vergeben (seit
// ext-36 **nicht mehr** identisch zur PA); alles andere bleibt ungemappt -> Translation-Fault
// (Event-Queue). Format = VMSAv8-64, 4-KiB-Granule, T0SZ=25
// (39-bit, wie die CPU-MMU).

// Stage-1-Deskriptor-Bits (AArch64), lokal (entkoppelt von mmu.rs).
const S1_TABLE: u64 = 0b11;
const S1_PAGE: u64 = 0b11;
const S1_AF: u64 = 1 << 10;
const S1_SH_INNER: u64 = 0b11 << 8;
const S1_AP_RW: u64 = 0b01 << 6; //   RW für EL0+EL1 (Geräte-Zugriff erlaubt)
const S1_AP_RO: u64 = 0b11 << 6; //   RO für EL0+EL1 (ext-24: Device-Read-Puffer schreibgeschützt)
const S1_ATTR_NORMAL: u64 = 1 << 2; //    MAIR-Index 1 = Normal WB (Coherent)
const S1_ATTR_NORMAL_NC: u64 = 2 << 2; // MAIR-Index 2 = Normal NC (NonCoherent)
const S1_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
const ONE_GIB: u64 = 1 << 30;
const TWO_MIB: u64 = 2 * 1024 * 1024;
const PAGE: u64 = 4096;

/// Bits eines Stage-1-Leaf-Deskriptors für `ro` (read-only = Device-Read, schreibgeschützt) +
/// `cacheable` (Coherent = Normal-WB, sonst Normal-NC).
fn s1_leaf_bits(ro: bool, cacheable: bool) -> u64 {
    let ap = if ro { S1_AP_RO } else { S1_AP_RW };
    let attr = if cacheable { S1_ATTR_NORMAL } else { S1_ATTR_NORMAL_NC };
    S1_AF | S1_SH_INNER | attr | ap | S1_PAGE
}

/// Eine **leere** Stage-1-Pagetable (nur die L1-Wurzel, alles ungemappt -> Fault) anlegen
/// (ext-24, DmaContext). `alloc` liefert einen genullten 4-KiB-Frame. Regionen werden mit
/// [`stage1_map_region`] additiv eingehängt (mehrere Regionen je Kontext).
pub fn stage1_create(alloc: &mut dyn FnMut() -> Option<u64>) -> Option<u64> {
    alloc() // genullter L1 = alle Einträge ungültig
}

/// Eine Region `[base, base+len)` (4-KiB-granular, GiB 0..511) **additiv** in die Stage-1-
/// Tabelle `l1` einhängen (identitäts, IOVA=PA). `ro` = Device-Read (schreibgeschützt),
/// `cacheable` = Coherent (Normal-WB) sonst NonCoherent (Normal-NC). `alloc` liefert L2/L3-
/// Frames. `false` bei ungültiger Region oder fehlgeschlagener Allokation.
pub fn stage1_map_region(
    l1: u64,
    iova: u64,
    pa: u64,
    len: u64,
    ro: bool,
    cacheable: bool,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    if len == 0 || iova % PAGE != 0 || pa % PAGE != 0 || len % PAGE != 0 {
        return false;
    }
    // Eingangsbreite der Stage-1 (T0SZ=25 -> 39 Bit). Eine IOVA darüber wäre nicht übersetzbar.
    if iova.checked_add(len).is_none_or(|e| e > (1u64 << 39)) {
        return false;
    }
    let leaf = s1_leaf_bits(ro, cacheable);
    let mut p = iova;
    let mut phys = pa;
    while p < iova + len {
        let i1 = (p / ONE_GIB) as usize;
        let Some(l2) = walk_or_alloc(l1, i1, alloc) else {
            return false;
        };
        let i2 = ((p % ONE_GIB) / TWO_MIB) as usize;
        let Some(l3) = walk_or_alloc(l2, i2, alloc) else {
            return false;
        };
        let i3 = ((p % TWO_MIB) / PAGE) as usize;
        // SAFETY: `l3` ist ein gültiger, identity-gemappter Tabellen-Frame; `i3` < 512.
        // **Der Blatt-Eintrag trägt die PA, der Index die IOVA** — das ist die Übersetzung.
        unsafe {
            core::ptr::write_volatile((l3 + (i3 as u64) * 8) as *mut u64, phys | leaf);
        }
        p += PAGE;
        phys += PAGE;
    }
    cpu::dsb_sy();
    true
}

/// Eine zuvor gemappte Region wieder aus der Stage-1-Tabelle entfernen (Leaves auf ungültig).
/// Die Tabellen-Frames bleiben (werden erst von [`free_stage1`] beim Kontext-Abbau freigegeben).
pub fn stage1_unmap_region(l1: u64, base: u64, len: u64) {
    let mut p = base;
    while p < base + len {
        // SAFETY: read-only-Walk + ggf. ein Leaf-Write; Tabellen-Frames gültig.
        unsafe {
            let i1 = (p / ONE_GIB) as usize;
            let e1 = core::ptr::read_volatile((l1 + (i1 as u64) * 8) as *const u64);
            if e1 & 0b11 == S1_TABLE {
                let l2 = e1 & S1_ADDR_MASK;
                let i2 = ((p % ONE_GIB) / TWO_MIB) as usize;
                let e2 = core::ptr::read_volatile((l2 + (i2 as u64) * 8) as *const u64);
                if e2 & 0b11 == S1_TABLE {
                    let l3 = e2 & S1_ADDR_MASK;
                    let i3 = ((p % TWO_MIB) / PAGE) as usize;
                    core::ptr::write_volatile((l3 + (i3 as u64) * 8) as *mut u64, 0);
                }
            }
        }
        p += PAGE;
    }
    cpu::dsb_sy();
}

/// Den Leaf-Deskriptor für IOVA `iova` aus der Stage-1-Tabelle `l1` zurücklesen (für den
/// strukturellen Test: AP-/Attr-Bits prüfen). `0`, wenn ungemappt.
pub fn stage1_read_leaf(l1: u64, iova: u64) -> u64 {
    // SAFETY: read-only-Walk über gültige Tabellen-Frames.
    unsafe {
        let i1 = (iova / ONE_GIB) as usize;
        let e1 = core::ptr::read_volatile((l1 + (i1 as u64) * 8) as *const u64);
        if e1 & 0b11 != S1_TABLE {
            return 0;
        }
        let i2 = ((iova % ONE_GIB) / TWO_MIB) as usize;
        let e2 = core::ptr::read_volatile(((e1 & S1_ADDR_MASK) + (i2 as u64) * 8) as *const u64);
        if e2 & 0b11 != S1_TABLE {
            return 0;
        }
        let i3 = ((iova % TWO_MIB) / PAGE) as usize;
        core::ptr::read_volatile(((e2 & S1_ADDR_MASK) + (i3 as u64) * 8) as *const u64)
    }
}

/// Ist ein Stage-1-Leaf read-only (Device-Read, schreibgeschützt)? (Test-Helfer.)
pub fn leaf_is_ro(leaf: u64) -> bool {
    leaf & (0b11 << 6) == S1_AP_RO
}
/// Ist ein Stage-1-Leaf Normal-Cacheable (Coherent)? (Test-Helfer.)
pub fn leaf_is_cacheable(leaf: u64) -> bool {
    leaf & (0b111 << 2) == S1_ATTR_NORMAL
}

/// Tabelleneintrag `idx` in der Tabelle `table_phys` auflösen; existiert noch keine nächste
/// Ebene, eine neue (genullte) anlegen und einhängen. Gibt die Physadresse der nächsten Ebene.
fn walk_or_alloc(table_phys: u64, idx: usize, alloc: &mut dyn FnMut() -> Option<u64>) -> Option<u64> {
    // SAFETY: `table_phys` ist ein gültiger Tabellen-Frame; `idx` < 512.
    unsafe {
        let e = (table_phys + (idx as u64) * 8) as *mut u64;
        let cur = core::ptr::read_volatile(e);
        if cur & 0b11 == S1_TABLE {
            return Some(cur & S1_ADDR_MASK);
        }
        let next = alloc()?;
        core::ptr::write_volatile(e, next | S1_TABLE);
        Some(next)
    }
}

/// Einen **Context Descriptor** (8 x u64) in den Frame `cd_phys` schreiben: Stage-1, 4-KiB-
/// Granule, T0SZ=25 (39-bit), IPS=40-bit, TTB0 = `stage1_l1`, MAIR wie die CPU-MMU
/// (attr0 Device, attr1 Normal-WB, attr2 Normal-NC). `cd_phys` muss 64-Byte-ausgerichtet sein.
pub fn write_cd(cd_phys: u64, stage1_l1: u64) {
    // word[0]: T0SZ=25, TG0=0, IR0=OR0=WB(0b01), SH0=inner(0b11), EPD1=1, V=1.
    let w0: u64 = 25 | (0b01 << 8) | (0b01 << 10) | (0b11 << 12) | (1 << 30) | (1 << 31);
    // word[1]: IPS=0b010 (40-bit), AA64=1 (Bit 9), R=1 (Bit 13, Faults **aufzeichnen**),
    // A=1 (Bit 14, Terminate-Modell: ein Fault bricht die Transaktion ab, statt sie als
    // RAZ/WI durchzulassen), ASET=1 (Bit 15).
    //
    // `A` ist sicherheitsrelevant und war schlicht vergessen: ohne das Bit ist das Verhalten bei
    // einem Übersetzungsfehler nicht „abbrechen", und `R` ist der Grund, warum überhaupt ein
    // Event in der Queue landet — ohne es prüft der Negativtest eine Queue, die per Konfiguration
    // leer bleibt. Eine Einheit, die die Kombination validiert, antwortet mit `C_BAD_CD`.
    let w1: u64 = 0b010 | (1 << 9) | (1 << 13) | (1 << 14) | (1 << 15);
    let cd0 = w0 | (w1 << 32);
    let cd1 = stage1_l1 & S1_ADDR_MASK; // TTB0
    let mair0: u64 = 0x00 | (0xFF << 8) | (0x44 << 16);
    let cd3 = mair0; // MAIR0 (word[6]); MAIR1 (word[7]) = 0
    let cd = [cd0, cd1, 0u64, cd3, 0, 0, 0, 0];
    // SAFETY: `cd_phys` ist ein genullter, identity-gemappter 64-Byte-Bereich.
    unsafe {
        for (i, &w) in cd.iter().enumerate() {
            core::ptr::write_volatile((cd_phys + (i as u64) * 8) as *mut u64, w);
        }
    }
    cpu::dsb_sy();
}

/// Alle Tabellen-Frames einer per [`build_stage1_identity`] gebauten Stage-1-Tabelle einsammeln
/// (L3s -> L2s -> L1) und über `free` zurückgeben (Teardown beim `disable_dma`). Mappt nur GiB
/// 0..511, L1->L2->L3 wie der Builder.
pub fn free_stage1(l1: u64, free: &mut dyn FnMut(u64)) {
    // SAFETY: `l1` ist eine gültige, identity-gemappte Stage-1-L1-Tabelle (read-only Scan).
    unsafe {
        for i1 in 0..512usize {
            let e1 = core::ptr::read_volatile((l1 + (i1 as u64) * 8) as *const u64);
            if e1 & 0b11 != S1_TABLE {
                continue;
            }
            let l2 = e1 & S1_ADDR_MASK;
            for i2 in 0..512usize {
                let e2 = core::ptr::read_volatile((l2 + (i2 as u64) * 8) as *const u64);
                if e2 & 0b11 == S1_TABLE {
                    free(e2 & S1_ADDR_MASK); // L3
                }
            }
            free(l2);
        }
    }
    free(l1);
}

/// Eine **STE** (8 x u64) für Stage-1-Übersetzung bauen, die auf den CD bei `cd_phys` zeigt.
pub fn build_ste_stage1(cd_phys: u64) -> [u64; 8] {
    // STE[0]: V=1, Config=0b101 (Stage-1), S1Fmt=0 (linear, 1 CD), S1ContextPtr = cd_phys[51:6].
    let ste0 = (cd_phys & 0x000f_ffff_ffff_ffc0) | (0b101 << 1) | 1;
    // STE[1]: S1CIR=WB(0b01), S1COR=WB(0b01), S1CSH=inner(0b11).
    //
    // `S1STALLD` (Bit 27) heißt „Stalling für diesen Stream **verbieten**" und ist nur dann eine
    // zulässige Wahl, wenn die Einheit beide Modelle beherrscht (`IDR0.STALL_MODEL == 0b10`).
    // Meldet sie 0b00 („Faults terminieren immer"), ist das Bit gegenstandslos, und bei 0b01
    // („Stall erzwungen") ist es schlicht verboten — eine Einheit, die das prüft, antwortet mit
    // `C_BAD_STE`, und der Stream übersetzt **gar nicht**. Genau das tat QEMU, blieb aber
    // unbemerkt, solange das Gerät die IOMMU ohnehin umging (kein `iommu_platform=on`) und
    // IOVA == PA war: beide Fehler zusammen sahen aus wie ein funktionierender Aufbau.
    // Ohne das Bit terminieren Faults über `CD.S == 0` — dasselbe gewünschte Verhalten.
    let stall_model = (idr0() >> 24) & 0b11;
    let stalld = if stall_model == 0b10 { 1u64 << 27 } else { 0 };
    let ste1: u64 = (0b01 << 2) | (0b01 << 4) | (0b11 << 6) | stalld;
    [ste0, ste1, 0, 0, 0, 0, 0, 0]
}

/// Ist die Event-Queue leer (PROD == CONS)? (Keine Translation-Faults aufgezeichnet.)
pub fn eventq_empty() -> bool {
    r32(EVENTQ_PROD) == r32(EVENTQ_CONS)
}
/// Anzahl Einträge in der Event-Queue (Diagnose).
pub fn eventq_count() -> u32 {
    let mask = (1u32 << LOG2_EVENTQ) - 1;
    let p = r32(EVENTQ_PROD) & mask;
    let c = r32(EVENTQ_CONS) & mask;
    p.wrapping_sub(c) & mask
}
/// Die Event-Queue leeren (CONS := PROD) — nach einem **bewusst** provozierten Fault (Kronjuwel-
/// Sensitivität), damit nachfolgende Audits die Queue wieder leer sehen. Gibt zurück, wie viele
/// Events verworfen wurden.
pub fn drain_eventq() -> u32 {
    let n = eventq_count();
    w32(EVENTQ_CONS, r32(EVENTQ_PROD));
    cpu::dsb_sy();
    n
}
/// Ein Eintrag der Event-Queue, so weit hier gebraucht.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventRecord {
    /// Ereignistyp (`EVT_*`), Bits [7:0] von Wort 0.
    pub kind: u8,
    /// StreamID der verursachenden Transaktion (Wort 0, Bits [63:32]).
    pub stream_id: u32,
    /// **Eingangs**adresse der fehlgeschlagenen Übersetzung — die IOVA, die das Gerät
    /// angefordert hat (Wort 2 bei den Translation-Fault-Typen).
    pub input_addr: u64,
}

/// `F_TRANSLATION` — die Übersetzung schlug fehl, weil kein gültiger Eintrag existiert.
/// Genau der Typ, den eine Anforderung auf eine **nicht zugeteilte** Adresse erzeugt.
pub const EVT_F_TRANSLATION: u8 = 0x10;

/// Den **ältesten** Eintrag der Event-Queue lesen, ohne ihn zu verbrauchen.
///
/// Für Tests, die einen Fault nicht nur *zählen*, sondern belegen müssen, dass er der erwartete
/// ist: ein bloßer Zähler bestünde auch bei einem Fault aus ganz anderem Grund.
pub fn eventq_peek() -> Option<EventRecord> {
    if eventq_empty() {
        return None;
    }
    let mask = (1u64 << LOG2_EVENTQ) - 1;
    let idx = (r32(EVENTQ_CONS) as u64) & mask;
    let base = r64(EVENTQ_BASE) & 0x000f_ffff_ffff_ffe0;
    let rec = base + idx * EVT_BYTES;
    // SAFETY: `rec` liegt in der vom Kernel allozierten, identity-gemappten Event-Queue; gelesen
    // werden genau die beiden Worte des Eintrags.
    unsafe {
        let w0 = core::ptr::read_volatile(rec as *const u64);
        let w2 = core::ptr::read_volatile((rec + 16) as *const u64);
        Some(EventRecord {
            kind: (w0 & 0xff) as u8,
            stream_id: (w0 >> 32) as u32,
            input_addr: w2,
        })
    }
}

/// Byte-Größen der drei Strukturen (für die RAM-Allokation durch den Aufrufer).
pub fn strtab_bytes() -> u64 {
    (1u64 << LOG2_STRTAB) * STE_BYTES
}
pub fn cmdq_bytes() -> u64 {
    (1u64 << LOG2_CMDQ) * CMD_BYTES
}
pub fn eventq_bytes() -> u64 {
    (1u64 << LOG2_EVENTQ) * EVT_BYTES
}
