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

use crate::cpu;

pub const SMMU_BASE: u64 = 0x0905_0000;

// --- Register-Offsets (Page 0) ---
const IDR0: u64 = 0x000;
const IDR1: u64 = 0x004;
const IDR5: u64 = 0x014;
const CR0: u64 = 0x020;
const CR0ACK: u64 = 0x024;
const CR1: u64 = 0x028;
const CR2: u64 = 0x02c;
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
pub const LOG2_STRTAB: u32 = 8; // 256 StreamIDs (deckt PCI-RIDs auf Bus 0 ab)
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
