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

use crate::fault::{FaultKind, FaultRecord};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// Registeroffsets (Intel VT-d Spezifikation, Kapitel 10).
const REG_VER: usize = 0x000;
const REG_CAP: usize = 0x008;
const REG_ECAP: usize = 0x010;
const REG_GCMD: usize = 0x018;
const REG_GSTS: usize = 0x01C;
const REG_RTADDR: usize = 0x020;
const REG_CCMD: usize = 0x028;
const REG_FSTS: usize = 0x034;

/// `FSTS.PFO` — **Primary Fault Overflow**: die Fault-Recording-Register sind voll, weitere
/// Faults werden **verworfen**, bis das Bit gelöscht ist.
///
/// Das ist die x86-Entsprechung zu der Lücke, die auf ARM schon einmal zugeschlagen hat: ein
/// Oracle der Form „keine weiteren Faults" bedeutet nach einem Fault-Sturm gar nichts, wenn die
/// Aufzeichnung übergelaufen ist. Der Zähler muss `PFO` mitführen (s. `fault_overflow`).
const FSTS_PFO: u32 = 1 << 0;

/// Bits in `GCMD`, die den **Zustand** tragen und bei jedem Kommando mitgeschrieben werden
/// müssen. `GCMD` ist kein Read-Modify-Write-Register: es wird als Ganzes geschrieben, und ein
/// nicht mitgeschriebenes Zustandsbit wird **gelöscht**.
///
/// Genau daran hängt eine Reihenfolge-Falle: `TE = 0` heißt **nicht** „blockiert", sondern
/// „keine Übersetzung" — also freier DMA. Wer den Root-Table-Pointer wechselt, indem er `SRTP`
/// allein nach `GCMD` schreibt, während `TE` gesetzt ist, öffnet für die Dauer des Wechsels
/// genau das Fenster, gegen das der ganze Mechanismus gebaut ist. Und es sähe im Test wie ein
/// Erfolg aus. Deshalb geht **jedes** Kommando über [`gcmd_issue`], das die Zustandsbits aus
/// `GSTS` übernimmt.
const GCMD_STATE_MASK: u32 = GCMD_TE | (1 << 25) | (1 << 26); // TE, IRE, QIE

/// Global Command/Status: Übersetzung aktivieren bzw. aktiv.
const GCMD_TE: u32 = 1 << 31;
const GSTS_TES: u32 = 1 << 31;
/// Set Root Table Pointer.
const GCMD_SRTP: u32 = 1 << 30;
const GSTS_RTPS: u32 = 1 << 30;
/// Context-Cache: globale Invalidierung anfordern / Fertigmeldung.
// --- Queued Invalidation (B-3.1) -------------------------------------------------------------
//
// **Vorbedingung, keine Alternative.** Der Registerpfad (`CCMD`/`IOTLB`) kann Kontext-Cache und
// IOTLB invalidieren -- den **Interrupt-Entry-Cache** nicht: fuer den existiert ueberhaupt nur ein
// QI-Deskriptor (Typ 0x4). Ohne QI ist Interrupt Remapping (B-3.2) also nicht bloss unbequem,
// sondern nicht sicher abschaltbar/aenderbar, weil eine geaenderte IRTE nie wirksam invalidiert
// werden koennte.
//
// **Und der Umstieg ist vollstaendig, nicht additiv.** Die Architektur verbietet den
// Registerpfad, sobald `GSTS.QIES` steht (VT-d 6.5.2): beides parallel zu halten waere kein
// Rueckfallnetz, sondern ein Fehler. Deshalb schalten die Invalidierungen unten hart um, statt
// eine Wahl anzubieten.
const REG_IQH: usize = 0x080; // Queue Head  (nur lesend)
const REG_IQT: usize = 0x088; // Queue Tail
const REG_IQA: usize = 0x090; // Queue Address + Groesse
const REG_ICS: usize = 0x09C; // Invalidation Completion Status

const GCMD_QIE: u32 = 1 << 26;
const GSTS_QIES: u32 = 1 << 26;

/// Deskriptoren sind 16 B; eine 4-KiB-Seite fasst 256 davon (IQA-Groessenfeld 0).
const QI_DESCS: u64 = 256;
const QI_DESC_BYTES: u64 = 16;

/// Deskriptortypen (unteres Nibble von Wort 0).
const QI_CC_INV: u64 = 0x1; // Context Cache
const QI_IOTLB_INV: u64 = 0x2; // IOTLB
const QI_IEC_INV: u64 = 0x4; // Interrupt Entry Cache -- gibt es NUR hier
const QI_WAIT: u64 = 0x5; // Invalidation Wait

/// Basisadresse der Warteschlange je Einheit (0 = QI nicht aktiv).
static QI_QUEUE: [AtomicU64; super::dmar::MAX_UNITS] =
    [const { AtomicU64::new(0) }; super::dmar::MAX_UNITS];
/// Statuswort, in das der Wait-Deskriptor schreibt. Eigene Zeile, damit die Einheit nicht in
/// fremde Daten schreibt; identity-gemappt wie alles andere hier.
static QI_STATUS: [AtomicU64; super::dmar::MAX_UNITS] =
    [const { AtomicU64::new(0) }; super::dmar::MAX_UNITS];
/// Naechster freier Deskriptorplatz (in Deskriptoren, nicht Bytes).
static QI_TAIL: [AtomicU64; super::dmar::MAX_UNITS] =
    [const { AtomicU64::new(0) }; super::dmar::MAX_UNITS];

/// Laeuft die Queued Invalidation auf Einheit 0?
pub fn qi_active() -> bool {
    QI_QUEUE[0].load(Ordering::Acquire) != 0
}

// --- Interrupt Remapping (B-3.2) --------------------------------------------------------------
//
// Ohne IR kann ein durchgereichtes Geraet **beliebige** Interrupt-Nachrichten erzeugen: eine
// MSI-Schreibung an die APIC-Adresse ist eine gewoehnliche DMA-Schreibung, und die
// Adressuebersetzung sieht sie sich nicht an -- der Bereich 0xFEEx_xxxx ist fuer die Einheit
// ausdruecklich KEIN uebersetzbarer Adressraum, sondern eine Interrupt-Nachricht (deshalb ist er
// auch als IOVA unbrauchbar, s. B-3.4). Ein Tenant-Geraet koennte damit einen beliebigen Vektor
// auf einem beliebigen Kern ausloesen, ganz ohne Zutun der Uebersetzungstabellen.
//
// **Und IR mit weiter erlaubtem CFI ist eine offene Tuer an der Seite.** Das
// Compatibility-Format ist der alte, nicht-remappte Nachrichtenpfad; laesst man ihn zu, kann ein
// Geraet die gesamte Remapping-Tabelle einfach umgehen. `GCMD.CFI = 0` ist deshalb Teil des
// Anschaltens, nicht eine Verschaerfung danach.
const REG_IRTA: usize = 0x0B8;

const GCMD_SIRTP: u32 = 1 << 24;
const GSTS_IRTPS: u32 = 1 << 24;
const GCMD_IRE: u32 = 1 << 25;
const GSTS_IRES: u32 = 1 << 25;
/// `GCMD.CFI` / `GSTS.CFIS` -- Compatibility Format Interrupts. **Wird nie gesetzt.**
const GSTS_CFIS: u32 = 1 << 23;

/// Eintraege der Interrupt-Remapping-Tabelle. 256 x 16 B = eine 4-KiB-Seite; das Groessenfeld in
/// `IRTA` ist `log2(n) - 1`.
///
/// **Beide Zahlen kommen seit dem 2026-08-10 aus `irte.rs`** -- dort sitzt der Zuteiler, und ein
/// Zuteiler, der ueber eine andere Tabellengroesse vergibt als die, die der Einheit gemeldet
/// wird, schreibt an Eintraegen vorbei oder ueber das Ende hinaus. Zwei Zahlen fuer dieselbe
/// Sache sind die `iova_window_clear_of_msi`-Falle; `irte.rs` haelt sie mit einem Host-Test
/// aneinander (`groessenfeld_passt_zur_tabellengroesse`).
const IRT_ENTRIES: u64 = super::irte::IRT_EINTRAEGE as u64;
const IRTA_SIZE_FIELD: u64 = super::irte::IRTA_GROESSENFELD;

/// Basisadresse der Interrupt-Remapping-Tabelle (0 = IR nicht aktiv).
static IRT: [AtomicU64; super::dmar::MAX_UNITS] =
    [const { AtomicU64::new(0) }; super::dmar::MAX_UNITS];

/// Laeuft Interrupt Remapping auf Einheit 0?
pub fn ir_active() -> bool {
    IRT[0].load(Ordering::Acquire) != 0 && read32(REG_GSTS) & GSTS_IRES != 0
}

/// Sind Compatibility-Format-Interrupts abgeschaltet? (Muss bei aktivem IR **immer** `true` sein.)
pub fn cfi_blocked() -> bool {
    read32(REG_GSTS) & GSTS_CFIS == 0
}

const CCMD_ICC: u64 = 1 << 63;
const CCMD_CIRG_GLOBAL: u64 = 1 << 61;

/// Registerbasen **aller** Remapping-Einheiten (`0` = Steckplatz leer).
///
/// Vorher war das eine einzige Basis. Bei mehreren Einheiten hat jede eigene Root-Table-Pointer,
/// eigene Invalidierungsqueue und **eigene Fault-Register** — ein Zähler, der nur Einheit 0
/// liest, ist für alles blind, was nicht an ihr hängt. Genau die Sorte Blindheit, die auf der
/// ARM-Seite zweimal aufgetreten ist.
static UNIT_BASES: [AtomicU64; super::dmar::MAX_UNITS] =
    [const { AtomicU64::new(0) }; super::dmar::MAX_UNITS];
static N_UNITS: AtomicU32 = AtomicU32::new(0);
/// Physische Adresse der Root-Tabelle je Einheit (`0` = noch keine).
static ROOT_TABLE: [AtomicU64; super::dmar::MAX_UNITS] =
    [const { AtomicU64::new(0) }; super::dmar::MAX_UNITS];

/// Anzahl gefundener Einheiten.
pub fn unit_count() -> usize {
    N_UNITS.load(Ordering::Acquire) as usize
}

fn base_of(unit: usize) -> u64 {
    UNIT_BASES
        .get(unit)
        .map(|b| b.load(Ordering::Acquire))
        .unwrap_or(0)
}

fn ru32(unit: usize, off: usize) -> u32 {
    let b = base_of(unit);
    if b == 0 {
        return 0;
    }
    // SAFETY: `b` stammt aus der ACPI-DMAR und liegt im identity-gemappten, uncacheable
    // MMIO-Bereich; volatile Registerzugriffe aliasen keinen Rust-Speicher.
    unsafe { core::ptr::read_volatile((b + off as u64) as *const u32) }
}
fn wu32(unit: usize, off: usize, v: u32) {
    let b = base_of(unit);
    if b == 0 {
        return;
    }
    // SAFETY: wie `ru32`.
    unsafe { core::ptr::write_volatile((b + off as u64) as *mut u32, v) };
}
fn ru64(unit: usize, off: usize) -> u64 {
    let b = base_of(unit);
    if b == 0 {
        return 0;
    }
    // SAFETY: wie `ru32`.
    unsafe { core::ptr::read_volatile((b + off as u64) as *const u64) }
}
fn wu64(unit: usize, off: usize, v: u64) {
    let b = base_of(unit);
    if b == 0 {
        return;
    }
    // SAFETY: wie `ru32`.
    unsafe { core::ptr::write_volatile((b + off as u64) as *mut u64, v) };
}

// Kurzformen für **Einheit 0**. Sie sind nach B-3.3 kein Standardweg mehr, sondern nur noch für
// die Pfade da, die tatsächlich einheitsgebunden sind: QI und IR werden heute ausschliesslich auf
// Einheit 0 aufgesetzt (s. `init`), und die Diagnosefunktionen `version`/`cap`/`ecap` melden
// bewusst eine einzelne Einheit. Alles, was eine Aussage über *die IOMMU* trifft, geht über
// `ru32`/`wu32` mit ausdrücklicher Einheit.
fn read32(off: usize) -> u32 {
    ru32(0, off)
}
fn read64(off: usize) -> u64 {
    ru64(0, off)
}
fn write64(off: usize, v: u64) {
    wu64(0, off, v)
}

/// Ein **Ein-Schritt-Kommando** an `unit` absetzen, ohne die Zustandsbits zu verlieren.
///
/// `one_shot` ist das Kommandobit (`SRTP`, `SIRTP`, …); alle Zustandsbits werden aus `GSTS`
/// übernommen. `want_mask`/`want_set` beschreiben, worauf gewartet wird.
fn gcmd_issue_on(unit: usize, one_shot: u32, want_mask: u32, want_set: bool) -> bool {
    let state = ru32(unit, REG_GSTS) & GCMD_STATE_MASK;
    wu32(unit, REG_GCMD, state | one_shot);
    wait_status_on(unit, want_mask, want_set)
}

fn gcmd_issue(one_shot: u32, want_mask: u32, want_set: bool) -> bool {
    gcmd_issue_on(0, one_shot, want_mask, want_set)
}

/// Ein **Zustandsbit** an `unit` setzen oder löschen (ebenfalls unter Erhalt der übrigen).
fn gcmd_set_state_on(unit: usize, bit: u32, on: bool) -> bool {
    let mut state = ru32(unit, REG_GSTS) & GCMD_STATE_MASK;
    if on {
        state |= bit;
    } else {
        state &= !bit;
    }
    wu32(unit, REG_GCMD, state);
    wait_status_on(unit, bit, on)
}

fn gcmd_set_state(bit: u32, on: bool) -> bool {
    gcmd_set_state_on(0, bit, on)
}

// --- Sprechfähigkeit einzelner Einheiten (B-3.3) ----------------------------------------------
//
// **Das Kernproblem der Aggregation.** Sobald über mehrere Einheiten geschleift wird, bedeutet ein
// `false` aus `ru32(u, …)` zweierlei: „die Einheit meldet nichts" oder „die Einheit antwortet gar
// nicht". `ru32` liefert bei fehlender Basis `0` — eine stumme Einheit trägt damit exakt so viel
// zu `faults_empty()` bei wie eine fehlerfreie. Genau die Form, gegen die dieses Projekt schon
// zweimal angetreten ist (leere SMMU-Event-Queue ohne `CD.R`, `virtio-rng` ohne Leserichtung):
// eine Aussage sieht wahr aus, weil der Fall, der sie widerlegen könnte, gar nicht auftreten kann.
//
// Deshalb gibt es hier eine ausdrückliche Sprechprobe, und die Abwesenheitsaussagen unten hängen
// an ihr statt an einer Null.

/// Antwortet die deklarierte Einheit `unit` überhaupt?
///
/// Drei Stufen, und alle drei sind nötig: Die Basis muss gesetzt sein (eine DRHD ohne
/// Registerbasis ist Firmware-Unsinn); `CAP` darf nicht `0` sein (ein nicht abgebildeter Bereich
/// liest sich unter TCG als Null); und `CAP` darf nicht `!0` sein (ein nicht dekodierter
/// MMIO-Bereich liest sich auf Blech als lauter Einsen). Nur wer alle drei besteht, darf mit
/// seinem Schweigen als Beleg für Fehlerfreiheit gelten.
pub fn unit_speaking(unit: usize) -> bool {
    if base_of(unit) == 0 {
        return false;
    }
    let cap = ru64(unit, REG_CAP);
    cap != 0 && cap != u64::MAX
}

/// Anzahl der Einheiten, die antworten (≤ [`unit_count`]).
pub fn units_speaking() -> usize {
    (0..unit_count()).filter(|&u| unit_speaking(u)).count()
}

/// Antworten **alle** deklarierten Einheiten? Nur dann trägt eine Abwesenheitsaussage.
pub fn all_units_speaking() -> bool {
    unit_count() > 0 && units_speaking() == unit_count()
}

/// Die DMAR deklarierte **mehr** Einheiten, als [`super::dmar::MAX_UNITS`] fasst.
///
/// Das ist kein Kapazitätsdetail, sondern ein Loch im Oracle: von den überzähligen Einheiten
/// kennen wir nicht einmal die Registerbasis, können also weder ihre Fähigkeiten mitteln noch
/// ihre Faults lesen. Ein Gerät hinter einer solchen Einheit wäre unbemerkt unübersetzt.
static UNITS_TRUNCATED: AtomicU32 = AtomicU32::new(0);

/// Wurden bei der DMAR-Auswertung Einheiten abgeschnitten?
pub fn units_truncated() -> bool {
    UNITS_TRUNCATED.load(Ordering::Acquire) != 0
}

/// **Fähigkeiten der Einheit**, einmal beim Hochlauf gelesen (Schritt 1 des VT-d-Aufbaus).
///
/// Der Grund für diesen Aufsatz ist eine Lektion von der ARM-Seite: `STE.S1STALLD` unbedingt zu
/// setzen war ein Bit, das nur unter einer Capability-Bedingung zulässig ist — die Einheit
/// antwortete mit `C_BAD_STE`, der Stream übersetzte **gar nicht**, und nichts sagte es laut.
/// VT-d hat ein Dutzend solcher Bedingungen. Sie werden deshalb **hier** einmal gelesen,
/// protokolliert und zu benannten Werten abgeleitet; keine spätere Stelle entscheidet selbst.
#[derive(Clone, Copy, Debug)]
pub struct VtdCaps {
    pub raw_cap: u64,
    pub raw_ecap: u64,
    /// Anzahl Domain-IDs (`CAP.ND`) — das x86-Gegenstück zu `NDMA_CTX`. Die Erschöpfung braucht
    /// denselben sauberen Fehlschlag wie das IOVA-Fenster.
    pub num_domains: u32,
    /// **Caching Mode** (`CAP.CM`): sind auch **nicht-präsente** Einträge cachebar?
    ///
    /// Bei `CM = 1` (typisch für QEMU) ist eine Invalidierung **nach dem Anlegen** einer
    /// Übersetzung zwingend, nicht nur nach dem Entfernen. Der Kernel fährt die Invalidierung
    /// unbedingt und protokolliert `CM` nur: sie ist die konservative Variante, die überall
    /// hält. Die umgekehrte Schlussfolgerung („unter QEMU nötig, auf Blech nicht") wäre die
    /// falsche Richtung — und auf Blech fiele das Fehlen nicht auf.
    pub caching_mode: bool,
    /// `CAP.RWBF`: nach Tabellenänderungen muss der Write-Buffer explizit geflusht werden.
    pub rwbf: bool,
    /// `CAP.SAGAW` roh, und die daraus **gewählte** Adressbreite.
    pub sagaw: u32,
    /// Gewählte Eingangsbreite in Bits (39 = 3 Level, 48 = 4 Level, 57 = 5 Level). `0`, wenn die
    /// Einheit keine der unterstützten Stufen anbietet.
    pub agaw_bits: u32,
    /// Seitentabellen-Level zur gewählten Breite.
    pub agaw_levels: u32,
    /// `CAP.MGAW + 1` — harte Obergrenze der Eingangsadresse (x86-Gegenstück zu `CD.T0SZ`).
    pub mgaw_bits: u32,
    /// Anzahl Fault-Recording-Register (`CAP.NFR + 1`) und deren Offset (`CAP.FRO`).
    pub num_fault_regs: u32,
    pub fault_reg_offset: u64,
    /// `ECAP.C` — **Page-Walk-Kohärenz**. Ist sie 0, sieht die Einheit die Schreibvorgänge auf
    /// Root-/Context-/Second-Level-Einträge nicht ohne Cache-Clean. Das betrifft die *Tabellen*,
    /// nicht die DMA-Puffer — ein Pfad, den `dma_granule()` gar nicht abdeckt.
    pub coherent_walk: bool,
    /// `ECAP.QI` — Queued Invalidation (das `CMDQ`-Analogon). Ohne sie bleibt der
    /// registerbasierte Pfad; beides parallel soll es nicht geben.
    pub queued_invalidation: bool,
    /// `ECAP.IR` — Interrupt Remapping vorhanden (eigener Schalter, eigene Baustelle).
    pub interrupt_remapping: bool,
    /// `ECAP.SC` — **Snoop Control**: erlaubt, No-Snoop-Transaktionen per `SNP`-Bit im SLPTE zu
    /// überstimmen. Ohne SC hängt die Kohärenzannahme daran, dass das Gerät kein No-Snoop
    /// verwendet — GPUs tun es. „x86 ist kohärent" ist also eine Aussage unter einer Bedingung,
    /// nicht absolut.
    pub snoop_control: bool,
    /// `ECAP.SMTS` — Scalable Mode. Der Kernel fährt **Legacy**; das ist eine Wahl, keine
    /// Annahme, und sie steht hier, damit sie im Log sichtbar ist.
    pub scalable_mode: bool,
}

impl VtdCaps {
    /// Die Fähigkeiten der Einheit lesen. `None`, wenn keine Einheit gefunden wurde.
    pub fn read() -> Option<VtdCaps> {
        VtdCaps::read_unit(0)
    }

    /// Die Fähigkeiten **einer bestimmten** Einheit lesen.
    pub fn read_unit(unit: usize) -> Option<VtdCaps> {
        if base_of(unit) == 0 {
            return None;
        }
        let cap = ru64(unit, REG_CAP);
        let ecap = ru64(unit, REG_ECAP);
        let nd = (cap & 0b111) as u32;
        let sagaw = ((cap >> 8) & 0x1f) as u32;
        // Bevorzugt 39 Bit (3 Level) — dieselbe Eingangsbreite wie die ARM-Seite, damit die
        // Fensterarithmetik nicht zweimal existiert. Bietet die Einheit sie nicht an, wird
        // aufgestiegen; sie ist NICHT garantiert, manche Implementierungen bieten nur 48.
        let (agaw_bits, agaw_levels) = agaw_from_sagaw(sagaw);
        Some(VtdCaps {
            raw_cap: cap,
            raw_ecap: ecap,
            num_domains: 1u32 << (4 + 2 * nd),
            caching_mode: cap & (1 << 7) != 0,
            rwbf: cap & (1 << 4) != 0,
            sagaw,
            agaw_bits,
            agaw_levels,
            mgaw_bits: (((cap >> 16) & 0x3f) as u32) + 1,
            num_fault_regs: (((cap >> 40) & 0xff) as u32) + 1,
            fault_reg_offset: ((cap >> 24) & 0x3ff) * 16,
            coherent_walk: ecap & (1 << 0) != 0,
            queued_invalidation: ecap & (1 << 1) != 0,
            interrupt_remapping: ecap & (1 << 3) != 0,
            snoop_control: ecap & (1 << 7) != 0,
            scalable_mode: ecap & (1 << 43) != 0,
        })
    }

    /// Kann diese Einheit überhaupt eine Zuteilung tragen, wie der Kernel sie baut?
    ///
    /// Heute die einzige harte Bedingung: eine unterstützte AGAW-Stufe, und die Eingangsbreite
    /// muss auch von `MGAW` gedeckt sein. Alles andere ist Verhalten, nicht Machbarkeit.
    pub fn usable(&self) -> bool {
        self.agaw_bits != 0 && self.mgaw_bits >= self.agaw_bits
    }

    /// Obergrenze der Eingangsadresse, gegen die eine IOVA geprüft werden muss.
    pub fn input_limit(&self) -> u64 {
        let bits = self.agaw_bits.min(self.mgaw_bits);
        if bits >= 64 { u64::MAX } else { 1u64 << bits }
    }
}

/// Ist die Fault-Aufzeichnung übergelaufen (`FSTS.PFO`)? Danach sind weitere Faults **verworfen**
/// — jede Aussage der Form „keine weiteren Faults" ist dann bedeutungslos.
pub fn fault_overflow() -> bool {
    (0..unit_count()).any(|u| ru32(u, REG_FSTS) & FSTS_PFO != 0)
}

/// `FSTS.PPF` — mindestens ein Fault-Recording-Register ist besetzt.
const FSTS_PPF: u32 = 1 << 1;

/// Die Bits, mit denen die Einheit sich ueber **uns** beschwert — im Gegensatz zu `PPF`, das ein
/// gewoehnlicher Befund ueber ein *Geraet* ist.
///
/// | Bit | | Bedeutung |
/// |---|---|---|
/// | 0 | `PFO` | Fault-Aufzeichnung uebergelaufen |
/// | 2 | `AFO` | Advanced-Fault-Log uebergelaufen |
/// | 3 | `APF` | Advanced Pending Fault |
/// | 4 | `IQE` | **Invalidation Queue Error** — ein Deskriptor war fehlerhaft |
/// | 5 | `ICE` | **Invalidation Completion Error** |
/// | 6 | `ITE` | **Invalidation Time-out Error** |
/// | 7 | `PRO` | Page-Request-Queue uebergelaufen |
///
/// **Befund vom 2026-08-17: `IQE`/`ICE`/`ITE` hat bis heute NIEMAND gelesen.** `REG_FSTS` kam im
/// ganzen Modul nur mit `PFO` und `PPF` vor. Das ist teuer, und zwar aus einem Grund, den man dem
/// Code nicht ansieht: **die drei Bits sind sticky (RW1C), und solange `IQE` steht, verarbeitet
/// die Einheit die Invalidierungs-Warteschlange NICHT weiter** (VT-d 6.5.2.9) — `IQH` bleibt
/// stehen. Danach laeuft *jedes* [`qi_submit`] in seine Poll-Schranke und gibt `false` zurueck.
///
/// Das Fehlerbild ist damit „die Invalidierung schlaegt fehl", die Ursache ein einzelnes gesetztes
/// Bit von vorhin, und **nichts im Baum nennt sie**. Genau die Form aus der Fallenliste: ein
/// Zustand, nach dem jede weitere Beobachtung bedeutungslos ist, ohne dass ein Pruefer ihn liest —
/// wie die leere Event-Queue ohne `CD.R`, nur auf der x86-Seite.
const FSTS_UNIT_ERRORS: u32 = (1 << 0) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7);

/// Die Fehlerbits der Einheit selbst, ODER-verknuepft ueber **alle** Einheiten.
///
/// Das x86-Gegenstueck zu `GERROR` auf aarch64. `0` heisst „die Einheit beschwert sich nicht";
/// alles andere ist nie hinnehmbar, und die Bittabelle steht bei [`FSTS_UNIT_ERRORS`].
///
/// `PPF` ist ausdruecklich **nicht** enthalten: ein aufgezeichneter Fault ist eine Aussage ueber
/// ein Geraet und wird von [`faults_empty`] getragen. Beides in eine Zahl zu werfen hiesse, einen
/// erwarteten Negativtest-Treffer von einem Defekt der Einheit nicht mehr unterscheiden zu koennen.
pub fn fault_status() -> u32 {
    if !present() {
        return 0;
    }
    (0..unit_count()).fold(0, |acc, u| acc | (ru32(u, REG_FSTS) & FSTS_UNIT_ERRORS))
}

/// Beobachtungsunabhängiger Zähler: Konfigurationsfehler **und** Aufzeichnungs-Überläufe, über
/// **alle** Einheiten.
static CFG_ERRORS: AtomicU32 = AtomicU32::new(0);

/// Welche stummen Einheiten wurden bereits in [`CFG_ERRORS`] gezählt (Bitmaske)?
/// Stummheit ist ein Dauerzustand — ohne diese Latch wäre der Zähler eine Funktion der
/// Aufrufhäufigkeit von [`drain_faults`] statt der Anzahl Befunde.
static MUTE_REPORTED: AtomicU32 = AtomicU32::new(0);

pub fn config_errors() -> u32 {
    CFG_ERRORS.load(Ordering::Relaxed)
}

/// Offset des `i`-ten Fault-Recording-Registers dieser Einheit.
fn frr_off(unit: usize, i: u32) -> Option<usize> {
    let c = VtdCaps::read_unit(unit)?;
    if i >= c.num_fault_regs {
        return None;
    }
    Some((c.fault_reg_offset + (i as u64) * 16) as usize)
}

/// Ein Fault-Recording-Register in die gemeinsame Form übersetzen. `None`, wenn es leer ist.
fn read_frr(unit: usize, i: u32) -> Option<FaultRecord> {
    let off = frr_off(unit, i)?;
    let lo = ru64(unit, off);
    let hi = ru64(unit, off + 8);
    if hi >> 63 == 0 {
        return None; // F-Bit nicht gesetzt
    }
    let reason = ((hi >> 32) & 0xff) as u8;
    let kind = match reason {
        // Nicht vorhandene Übersetzung — inklusive „gar nicht zugeteilt" (Root/Kontext fehlen),
        // was der Default-Block-Zustand ist und kein Kernel-Fehler.
        0x01 | 0x02 | 0x07 => FaultKind::Translation,
        0x05 | 0x06 => FaultKind::Permission,
        0x04 => FaultKind::AddressSize,
        // **Die Einheit lehnt die Tabellen ab** — reservierte Bits, ungültige Tabellenadressen.
        // Genau die Klasse, die auf ARM `C_BAD_STE`/`C_BAD_CD` heißt.
        0x03 | 0x08 | 0x09 | 0x0a | 0x0b | 0x0c => FaultKind::Config,
        other => FaultKind::Other(other),
    };
    Some(FaultRecord {
        kind,
        requester: (hi & 0xffff) as u32,
        input_addr: lo & !0xfff,
        raw: reason,
    })
}

/// Keine aufgezeichneten Fehler in irgendeiner Einheit?
///
/// **Das ist eine Abwesenheitsaussage, und sie hängt an der Sprechfähigkeit.** Eine deklarierte,
/// aber stumme Einheit liefert aus `ru32` eine `0` — sie trüge sonst genauso zum „leer" bei wie
/// eine fehlerfreie. Genau deshalb ist eine unvollständige Sprechprobe hier ein **`false`**: wir
/// wissen es nicht, und „wir wissen es nicht" darf nicht als „nichts passiert" durchgehen.
/// Dasselbe gilt für abgeschnittene Einheiten — von denen kennen wir nicht einmal die Basis.
///
/// **Nicht** betroffen ist der Fall „gar keine Einheit deklariert" (`unit_count() == 0`): dort gibt
/// es keine IOMMU und damit auch keine IOMMU-Faults; die Aussage ist leer, aber nicht falsch, und
/// der Aufrufer gattert ohnehin über [`present`]. Der gefährliche Fall ist der andere — eine
/// Einheit ist **deklariert** und antwortet trotzdem nicht.
pub fn faults_empty() -> bool {
    let n = unit_count();
    if n > 0 && (units_speaking() != n || units_truncated()) {
        return false;
    }
    !(0..n).any(|u| ru32(u, REG_FSTS) & FSTS_PPF != 0)
}

/// Den ältesten Eintrag lesen, ohne ihn zu verbrauchen.
///
/// `FSTS.FRI` zeigt auf das Register mit dem **ersten** aufgezeichneten Fault.
pub fn peek_fault() -> Option<FaultRecord> {
    for u in 0..unit_count() {
        let fsts = ru32(u, REG_FSTS);
        if fsts & FSTS_PPF == 0 {
            continue;
        }
        let fri = (fsts >> 8) & 0xff;
        if let Some(r) = read_frr(u, fri) {
            return Some(r);
        }
        // FRI kann veraltet sein -> alle durchsehen.
        let n = VtdCaps::read_unit(u).map(|c| c.num_fault_regs).unwrap_or(0);
        for i in 0..n {
            if let Some(r) = read_frr(u, i) {
                return Some(r);
            }
        }
    }
    None
}

/// Alle Fault-Recording-Register aller Einheiten räumen.
///
/// **Räumen ist hier Pflicht, nicht Kosmetik.** Mit `CAP.NFR == 1` — dem Wert dieser Plattform —
/// setzt der *zweite* Fault `FSTS.PFO`, und ab da wird verworfen, bis sowohl das `F`-Bit im
/// Register als auch `PFO` gelöscht sind. Ein Negativtest, der zwischen Fault und
/// Positivkontrolle nicht räumt, bekäme eine Positivkontrolle, die besteht, **weil nichts mehr
/// aufgezeichnet wird** — wieder ein Grün ohne Bedeutung.
pub fn drain_faults() -> u32 {
    let mut n = 0;
    // **Stumme und abgeschnittene Einheiten zählen als Konfigurationsfehler** (B-3.3). Sie sind
    // der Fall, in dem das Räumen selbst nicht stattfinden kann: die Register sind nicht lesbar,
    // also ist auch jede spätere Aussage „keine Faults" über sie bedeutungslos. Der Zähler ist
    // beobachtungsunabhängig und wird vom Kernel ohnehin geprüft (Audit-Code 6) — die Blindheit
    // wird damit sichtbar, ohne dass ein neuer Meldeweg nötig wäre.
    //
    // Die Abschneidung ist ein **Zustand**, kein Ereignis: sie würde sonst bei jedem Aufruf erneut
    // zählen und den Zähler zu einer Funktion der Aufrufhäufigkeit machen. Deshalb genau einmal
    // (1 = festgestellt, 2 = gezählt); `units_truncated()` bleibt in beiden Fällen wahr.
    if UNITS_TRUNCATED
        .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        CFG_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    for u in 0..unit_count() {
        if !unit_speaking(u) {
            // Ebenfalls ein Zustand, nicht ein Ereignis -> je Einheit genau einmal zählen.
            let bit = 1u32 << (u % 32);
            if MUTE_REPORTED.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
                CFG_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        }
        let nfr = VtdCaps::read_unit(u).map(|c| c.num_fault_regs).unwrap_or(0);
        for i in 0..nfr {
            let Some(off) = frr_off(u, i) else { continue };
            if let Some(r) = read_frr(u, i) {
                if r.kind == FaultKind::Config {
                    CFG_ERRORS.fetch_add(1, Ordering::Relaxed);
                }
                n += 1;
            }
            // F-Bit ist RW1C: das obere Wort mit gesetztem Bit 63 zurückschreiben.
            wu64(u, off + 8, 1u64 << 63);
        }
        // Überlauf zählt in denselben Zähler: danach war die Aufzeichnung blind, und jede
        // spätere Abwesenheitsaussage wäre bedeutungslos.
        let fsts = ru32(u, REG_FSTS);
        if fsts & FSTS_PFO != 0 {
            CFG_ERRORS.fetch_add(1, Ordering::Relaxed);
            wu32(u, REG_FSTS, FSTS_PFO); // RW1C
        }
    }
    n
}

/// Ist eine Remapping-Einheit vorhanden (ACPI-DMAR gefunden)?
pub fn present() -> bool {
    base_of(0) != 0
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

/// Ist die Übersetzung aktiv (`GSTS.TES`) — auf **jeder** deklarierten Einheit?
///
/// Genau dann blockt die IOMMU alles, was nicht ausdrücklich zugeteilt ist. Über alle Einheiten,
/// nicht nur Einheit 0: eine einzige Einheit mit `TE = 0` genügt, damit jedes Gerät hinter ihr
/// **unübersetzt** auf den ganzen Speicher zugreift. Eine stumme Einheit zählt als nicht aktiv —
/// wir können ihren Zustand nicht feststellen und dürfen ihn deshalb nicht behaupten.
pub fn enabled() -> bool {
    present()
        && unit_count() > 0
        && (0..unit_count()).all(|u| unit_speaking(u) && ru32(u, REG_GSTS) & GSTS_TES != 0)
}

/// Registerbasis aus der **ACPI-DMAR** übernehmen (idempotent).
pub fn discover() -> bool {
    if present() {
        return true;
    }
    // **Alle** DRHD-Einheiten übernehmen, nicht nur die erste.
    let Some(tbl) = super::acpi::dmar_table() else {
        return false;
    };
    let info = super::dmar::parse(tbl);
    if info.n_units == 0 {
        return false;
    }
    let n = info.n_units.min(super::dmar::MAX_UNITS);
    for u in 0..n {
        UNIT_BASES[u].store(info.units[u].reg_base, Ordering::Release);
    }
    N_UNITS.store(n as u32, Ordering::Release);
    // Abschnittene Einheiten sind ein Loch im Oracle, kein Kapazitätsdetail (s. `units_truncated`).
    if info.truncated || info.n_units > super::dmar::MAX_UNITS {
        UNITS_TRUNCATED.store(1, Ordering::Release);
    }
    // Den Fähigkeits-Cache **verwerfen**, nicht hier neu füllen: `discover` hat bisher keine
    // Register gelesen, und diese Reihenfolgeannahme (MMIO schon abgebildet) soll sie auch nicht
    // bekommen. Die Neuberechnung passiert beim ersten echten Bedarf — und das ist frühestens im
    // Tabellenaufbau von `init`, wo die Register nachweislich erreichbar sind.
    AGG_COHERENT.store(0, Ordering::Release);
    AGG_SNOOP.store(0, Ordering::Release);
    true
}

// --- Gemittelte Fähigkeiten im heissen Pfad (B-3.3) --------------------------------------------
//
// `flush_entry` und `slpt_map` treffen **Politik**: ob geflusht wird und ob `SNP` gesetzt werden
// darf. Beides hing bisher an `VtdCaps::read()`, also an **Einheit 0** — und beides ist genau in
// der Richtung falsch, die nicht auffällt:
//
// * `ECAP.C` (kohärenter Page-Walk) von Einheit 0 übernommen: ist Einheit 1 inkohärent, entfällt
//   der `clflush`, und sie sieht den Tabelleneintrag **nie**. Die Übersetzung, die dort entstehen
//   sollte, entsteht nicht — und der Test sieht einen erfolgreichen `slpt_map`.
// * `ECAP.SC` (Snoop Control) von Einheit 0 übernommen: fehlt sie Einheit 1, ist `SNP` dort ein
//   **reserviertes** Bit. Reservierte Bits faulten — wörtlich derselbe Mechanismus wie
//   `STE.S1STALLD` ohne `IDR0.STALL_MODEL`, an dem dieses Projekt schon einmal hing.
//
// Das Minimum ist bei beiden die sichere Richtung: lieber einmal zu viel flushen, lieber `SNP`
// weglassen. Zwischengespeichert wird, weil der Pfad je Tabelleneintrag läuft — `caps_common()`
// dort aufzurufen hiesse, je 4-KiB-Seite `n_units` MMIO-Lesungen zu machen.
//
// Tri-State, damit ein kalter Cache nicht als „nichts nötig" durchgeht: 0 = unbekannt,
// 1 = ja, 2 = nein. Bei `unbekannt` gilt die **konservative** Annahme (flushen, kein `SNP`).
static AGG_COHERENT: AtomicU32 = AtomicU32::new(0);
static AGG_SNOOP: AtomicU32 = AtomicU32::new(0);

fn refresh_aggregate() {
    let (coh, snp) = match caps_common() {
        Some(c) => (c.coherent_walk, c.snoop_control),
        None => (false, false), // konservativ: flushen, kein SNP
    };
    AGG_COHERENT.store(if coh { 1 } else { 2 }, Ordering::Release);
    AGG_SNOOP.store(if snp { 1 } else { 2 }, Ordering::Release);
}

/// Ist der Page-Walk **aller** Einheiten kohärent? Unbekannt -> `false` (also flushen).
fn agg_coherent_walk() -> bool {
    match AGG_COHERENT.load(Ordering::Acquire) {
        1 => true,
        2 => false,
        _ => {
            refresh_aggregate();
            AGG_COHERENT.load(Ordering::Acquire) == 1
        }
    }
}

/// Beherrschen **alle** Einheiten Snoop Control? Unbekannt -> `false` (also kein `SNP`).
fn agg_snoop_control() -> bool {
    match AGG_SNOOP.load(Ordering::Acquire) {
        1 => true,
        2 => false,
        _ => {
            refresh_aggregate();
            AGG_SNOOP.load(Ordering::Acquire) == 1
        }
    }
}

/// Die **gemeinsame** Fähigkeitsbasis aller Einheiten.
///
/// Die AGAW-Wahl muss von **jeder** Einheit getragen werden, die eine zuteilbare Gruppe scopet —
/// sonst gäbe es die Fensterarithmetik zweimal, genau das, was die Wahl von 39 Bit vermeiden
/// sollte. Kann eine Einheit die gewählte Breite nicht, ist das ein **lauter** Fehlschlag für die
/// dahinterliegenden Gruppen (`usable() == false`), keine stille Anpassung.
/// **`raw_cap`/`raw_ecap` sind die Rohwerte von Einheit 0** und dürfen nicht als gemittelt gelesen
/// werden. Wer ein Bit braucht, das hier nicht als benanntes Feld auftaucht, muss es über alle
/// Einheiten selbst mitteln (Vorbild: `invalidate_iotlb_global`, das `IRO` je Einheit liest).
pub fn caps_common() -> Option<VtdCaps> {
    caps_for_units(u32::MAX)
}

/// Wie [`caps_common`], aber nur über die Einheiten, deren Bit in `mask` gesetzt ist.
///
/// Gedacht für `dmar::units_scoping_allocatable`: das Minimum über **genau die** Einheiten, die
/// eine zuteilbare Gruppe scopen. Eine Einheit, hinter der nur ausgeschlossene Geräte hängen,
/// beschneidet damit nicht mehr die Zusicherung für alle anderen.
///
/// `None`, wenn keine Einheit übrig bleibt oder eine ausgewählte Einheit **stumm** ist — deren
/// Fähigkeiten sind unbekannt, und ein Minimum über unbekannte Werte wäre eine erfundene Zusage.
pub fn caps_for_units(mask: u32) -> Option<VtdCaps> {
    let n = unit_count();
    if n == 0 {
        return None;
    }
    let sel = |u: usize| u < 32 && mask & (1u32 << u) != 0;
    let first = (0..n).find(|&u| sel(u))?;
    // Eine stumme Einheit liefert `CAP == 0` bzw. `!0`; daraus abgeleitete „Fähigkeiten" wären
    // frei erfunden (SAGAW 0, MGAW 1 -- oder umgekehrt alles). Deshalb hier ein hartes `None`
    // statt eines Minimums ueber Werte, die niemand gelesen hat.
    if !unit_speaking(first) {
        return None;
    }
    let mut acc = VtdCaps::read_unit(first)?;
    for u in (first + 1)..n {
        if !sel(u) {
            continue;
        }
        if !unit_speaking(u) {
            return None;
        }
        let c = VtdCaps::read_unit(u)?;
        // Konservativ mischen: die schwächste Zusicherung gewinnt, die strengste Pflicht auch.
        acc.sagaw &= c.sagaw;
        acc.num_domains = acc.num_domains.min(c.num_domains);
        acc.mgaw_bits = acc.mgaw_bits.min(c.mgaw_bits);
        acc.num_fault_regs = acc.num_fault_regs.min(c.num_fault_regs);
        acc.caching_mode |= c.caching_mode; //   eine Einheit mit CM=1 zwingt den strengen Pfad
        acc.rwbf |= c.rwbf; //                   ebenso
        acc.coherent_walk &= c.coherent_walk; // eine inkohärente Einheit zwingt zum Flush
        acc.queued_invalidation &= c.queued_invalidation;
        acc.interrupt_remapping &= c.interrupt_remapping;
        acc.snoop_control &= c.snoop_control;
        acc.scalable_mode |= c.scalable_mode;
    }
    let (bits, levels) = agaw_from_sagaw(acc.sagaw);
    acc.agaw_bits = bits;
    acc.agaw_levels = levels;
    Some(acc)
}

fn agaw_from_sagaw(sagaw: u32) -> (u32, u32) {
    if sagaw & (1 << 1) != 0 {
        (39, 3)
    } else if sagaw & (1 << 2) != 0 {
        (48, 4)
    } else if sagaw & (1 << 3) != 0 {
        (57, 5)
    } else {
        (0, 0)
    }
}

/// Auf ein Statusbit warten (begrenzt — ein Hardware-Poll darf im Kernel nie unbegrenzt laufen).
fn wait_status_on(unit: usize, mask: u32, want_set: bool) -> bool {
    for _ in 0..1_000_000 {
        let set = ru32(unit, REG_GSTS) & mask != 0;
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
pub fn init(root_table: u64, alloc: &mut dyn FnMut() -> Option<u64>) -> bool {
    if !discover() {
        return false;
    }
    // **Kontext-Tabellen einmal beim Hochlauf**, eine je Bus (256 x 4 KiB = 1 MiB), statt sie
    // beim ersten `attach` nachzuziehen. Drei Gründe, und der dritte ist der eigentliche:
    // sie sind eine **Bus**-Struktur und dürfen beim Abbau eines einzelnen Kontexts gar nicht
    // freigegeben werden (andere Geräte desselben Busses hängen daran); die Symmetrie zur
    // ARM-Seite, wo die lineare Stream-Tabelle ebenfalls einmal beim Bring-up entsteht; und
    // dass der DMA-Pfad damit **ohne Allokation** auskommt — eine lazy angelegte, nie
    // freigegebene Tabelle sähe in jeder Ressourcenbilanz wie ein Leck aus.
    for bus in 0..256u64 {
        let Some(ctx) = alloc() else { return false };
        // SAFETY: `root_table` ist ein frisch genulltes, identity-gemapptes 4-KiB-Frame.
        unsafe { write_entry(root_table + bus * 16, (ctx & ADDR_MASK) | 1) };
    }
    // **Die Root-Tabelle wird von allen Einheiten GETEILT** (B-3.3).
    //
    // Vorher bekam nur Einheit 0 einen Root-Table-Pointer und nur sie `TE` — die übrigen blieben
    // mit `TE = 0` stehen, und das heisst nicht „blockiert", sondern **keine Übersetzung**, also
    // freier DMA für jedes Gerät hinter ihnen. Ein Aggregations-Todo hätte das nicht abgedeckt;
    // es ist der Unterschied zwischen „das Oracle ist blind" und „es gibt dort keinen Schutz".
    //
    // Geteilt statt je Einheit eigen, weil die Root-Tabelle nur Bus -> Kontext-Tabelle abbildet
    // und die Kontexteinträge ohnehin nach Requester-ID getrennt sind: eine zweite Tabelle wäre
    // dieselbe Abbildung ein zweites Mal, plus 1 MiB Kontext-Tabellen je Einheit. Die Einheiten
    // scopen disjunkte Gerätemengen, sie können sich also nicht gegenseitig etwas freischalten.
    for u in 0..unit_count() {
        ROOT_TABLE[u].store(root_table, Ordering::Release);
    }
    let mut all_ok = true;
    for u in 0..unit_count() {
        if !unit_speaking(u) {
            // Eine deklarierte, aber stumme Einheit lässt sich nicht scharfstellen. Das ist ein
            // Fehlschlag: Geräte hinter ihr wären unübersetzt, und das darf nicht wie Erfolg
            // aussehen.
            all_ok = false;
            continue;
        }
        // Root-Table-Pointer setzen (Bits 63:12; Translation Table Mode = Legacy).
        wu64(u, REG_RTADDR, root_table & !0xfff);
        // **Nie** `TE` löschen, um umzukonfigurieren: `TE = 0` heißt freier DMA, nicht Blockade.
        // `gcmd_issue_on` übernimmt die Zustandsbits aus `GSTS`, der Wechsel läuft also auch dann
        // ohne Lücke, wenn die Übersetzung bereits aktiv ist.
        if !gcmd_issue_on(u, GCMD_SRTP, GSTS_RTPS, true) {
            all_ok = false;
        }
    }
    if !all_ok {
        return false;
    }
    // QI VOR der ersten Invalidierung aufsetzen (B-3.1): danach laeuft sie ueber die
    // Warteschlange. Scheitert es, bleibt der Registerpfad gueltig -- `qi_active()` sagt dann
    // `false`, und niemand behauptet etwas anderes. Interrupt Remapping (B-3.2) ist in diesem
    // Fall NICHT zulaessig, weil der Interrupt-Entry-Cache dann nicht invalidierbar waere.
    //
    // **Beides heute nur auf Einheit 0** — beide brauchen je Einheit eigene Frames (2 Seiten QI,
    // 1 Seite IRT), und die Warteschlangen-Statics sind je Einheit angelegt, aber nur an Index 0
    // befüllt. Für die Uebersetzung ist das folgenlos (die uebrigen Einheiten fahren den
    // Registerpfad, s. `invalidate_context_cache`), fuer Interrupt Remapping NICHT: hinter
    // Einheit 1..n bleibt der nicht-remappte Nachrichtenpfad offen. Das steht als Rest von B-3.2
    // offen und ist hier vermerkt, damit es nicht in der Aggregation verschwindet.
    qi_enable(alloc);
    ir_enable(alloc);
    // **Der einzige Punkt im Hochlauf, an dem die IRTE-Vergabe pruefbar ist** (Z22 P1): die
    // Tabelle steht frisch, QI laeuft, und noch kein Geraet haelt einen Vektor. Nur mit
    // `selftest`; der Release-Bau enthaelt das nicht.
    #[cfg(feature = "selftest")]
    {
        let b = irte_selbsttest();
        if !b.sprechfaehig {
            // **SKIP mit Grund, nicht PASS.** Ein leerer Lauf ist kein Testergebnis -- und die
            // vier Zahlen sagen, WELCHE Vorbedingung fehlt.
            crate::println!(
                "irtevgb : SKIP  (IR nicht scharf: tabelle={} ire={} qi={} einheiten={})",
                IRT[0].load(Ordering::Acquire) != 0,
                ir_active(),
                qi_active(),
                unit_count()
            );
        } else {
            crate::println!(
                "irtevgb : {}  vergabe={} eintrag-steht={} quellpruefung={} getrennt={} \
                 adresse-remappable={} fail-closed={} erschoepfung-benannt={} einzug={} \
                 belegt-am-ende={} hoechststand={}",
                if b.ok() { "ALL PASS" } else { "FAILURES" },
                b.vergabe,
                b.eintrag_steht,
                b.quellpruefung,
                b.getrennt,
                b.adresse_remappable,
                b.fail_closed,
                b.erschoepfung_benannt,
                b.einzug,
                b.belegt_am_ende,
                b.hoechststand
            );
        }
    }
    invalidate_context_cache();
    // Übersetzung auf **allen** Einheiten aktivieren.
    for u in 0..unit_count() {
        if !gcmd_set_state_on(u, GCMD_TE, true) {
            all_ok = false;
        }
    }
    all_ok
}



/// **Interrupt Remapping aufsetzen** (B-3.2). `alloc` liefert ein genulltes 4-KiB-Frame.
///
/// Die Tabelle wird mit lauter **nicht vorhandenen** Eintraegen aufgesetzt -- dieselbe
/// Default-Block-Politik wie bei der Root-Tabelle: eine Interrupt-Nachricht, fuer die kein
/// Eintrag existiert, wird abgewiesen statt durchgelassen. Erst wer eine IRTE bekommt, darf
/// ueberhaupt einen Interrupt ausloesen.
///
/// Reihenfolge, und sie ist nicht beliebig:
/// 1. `IRTA` setzen (Adresse + Groessenfeld), dann `SIRTP` -- die Einheit uebernimmt den Zeiger.
/// 2. **Interrupt-Entry-Cache invalidieren.** Ohne das koennte die Einheit Eintraege einer
///    frueheren Tabelle weiterverwenden. Das geht nur ueber QI (B-3.1) -- deshalb schlaegt diese
///    Funktion fehl, wenn QI nicht laeuft, statt IR ohne Invalidierungsmoeglichkeit anzuschalten.
/// 3. `IRE` setzen. `CFI` bleibt dabei **aus**: `gcmd_set_state` schreibt nur die Bits aus
///    [`GCMD_STATE_MASK`], und Bit 23 steht bewusst nicht darin.
///
/// `false`, wenn kein Speicher da ist, QI fehlt, oder die Einheit einen der Schritte nicht
/// bestaetigt. Der Aufrufer darf dann **nicht** so tun, als sei IR aktiv.
fn ir_enable(alloc: &mut dyn FnMut() -> Option<u64>) -> bool {
    // Ohne QI ist der Interrupt-Entry-Cache nicht invalidierbar -- IR waere dann eine Zusage
    // ohne Durchsetzungsmittel (B-3.1 ist Vorbedingung, nicht Alternative).
    if !qi_active() {
        return false;
    }
    let Some(table) = alloc() else { return false };
    // Groessenfeld in den unteren Bits, Adresse in 63:12. EIME (Bit 11) bleibt aus: es gilt nur
    // im x2APIC-Modus, und den meldet diese Plattform nicht zwingend -- ein gesetztes EIME ohne
    // x2APIC waere ein reserviertes Bit.
    write64(REG_IRTA, (table & !0xfff) | IRTA_SIZE_FIELD);
    if !gcmd_issue(GCMD_SIRTP, GSTS_IRTPS, true) {
        return false;
    }
    IRT[0].store(table, Ordering::Release);
    if !invalidate_iec_global() {
        IRT[0].store(0, Ordering::Release);
        return false;
    }
    if !gcmd_set_state(GCMD_IRE, true) {
        IRT[0].store(0, Ordering::Release);
        return false;
    }
    // Gegenprobe statt Annahme: CFI MUSS aus sein, sonst ist die Tabelle umgehbar.
    cfi_blocked()
}

/// **Queued Invalidation aufsetzen** (B-3.1). `alloc` liefert genullte, identity-gemappte 4-KiB-Frames.
///
/// Zwei Seiten: die Deskriptor-Warteschlange (256 x 16 B) und eine Seite fuer das Statuswort, in
/// das der Wait-Deskriptor schreibt. Das Statuswort bekommt eine **eigene** Seite, weil die
/// Einheit dorthin per DMA schreibt -- in einer geteilten Zeile wuerde sie fremde Daten
/// ueberschreiben, und der Fehler traete weit entfernt von hier auf.
///
/// `false`, wenn kein Speicher da ist oder die Einheit `QIES` nicht bestaetigt. Der Aufrufer darf
/// dann **nicht** so tun, als liefe QI: der Registerpfad bleibt gueltig, solange QIES aus ist.
fn qi_enable(alloc: &mut dyn FnMut() -> Option<u64>) -> bool {
    let Some(queue) = alloc() else { return false };
    let Some(status) = alloc() else { return false };
    // IQA: Bits 63:12 Adresse, Bits 2:0 Groesse (0 = 256 Deskriptoren), Bit 11 DW (128-Bit) = 0.
    write64(REG_IQA, queue & !0xfff);
    write64(REG_IQT, 0);
    QI_QUEUE[0].store(queue, Ordering::Release);
    QI_STATUS[0].store(status, Ordering::Release);
    QI_TAIL[0].store(0, Ordering::Release);
    if !gcmd_set_state(GCMD_QIE, true) {
        // Nicht bestaetigt -> Zustand zuruecknehmen, sonst hielte `qi_active()` eine Unwahrheit.
        QI_QUEUE[0].store(0, Ordering::Release);
        return false;
    }
    true
}

/// Einen Deskriptor abschicken und auf seine Abarbeitung warten.
///
/// Angehaengt wird immer ein **Wait-Deskriptor** mit Statusschreibung: erst wenn die Einheit ihn
/// abgearbeitet hat, sind alle davor liegenden Deskriptoren wirksam. Ohne ihn waere `IQT` nur die
/// Aussage "eingereiht", nicht "durchgefuehrt" -- und eine Invalidierung, deren Wirksamkeit man
/// nicht abwartet, ist genau die Sorte Zusage, die dieses Projekt nicht ausspricht.
fn qi_submit(w0: u64, w1: u64) -> bool {
    let queue = QI_QUEUE[0].load(Ordering::Acquire);
    let status = QI_STATUS[0].load(Ordering::Acquire);
    if queue == 0 {
        return false;
    }
    // Statuswort auf 0, Zielwert 1 -- die Einheit schreibt ihn beim Abarbeiten des Wait-Deskriptors.
    // SAFETY: `status` ist ein frisch alloziertes, identity-gemapptes, genulltes 4-KiB-Frame.
    unsafe { write_entry(status, 0) };

    let mut tail = QI_TAIL[0].load(Ordering::Acquire);
    let mut put = |a: u64, b: u64, tail: &mut u64| {
        let off = queue + (*tail % QI_DESCS) * QI_DESC_BYTES;
        // SAFETY: `off` liegt in der allozierten Warteschlangenseite (Modulo ueber QI_DESCS).
        unsafe {
            write_entry(off, a);
            write_entry(off + 8, b);
        }
        *tail += 1;
    };
    put(w0, w1, &mut tail);
    // Wait: SW (Bit 5) = Statuswort schreiben, FN (Bit 6) = erst nach allen vorherigen.
    put(QI_WAIT | (1 << 5) | (1 << 6) | (1u64 << 32), status, &mut tail);
    QI_TAIL[0].store(tail, Ordering::Release);
    write64(REG_IQT, (tail % QI_DESCS) * QI_DESC_BYTES);

    for _ in 0..1_000_000 {
        // SAFETY: identity-gemapptes Frame, von der Einheit per DMA beschrieben.
        if unsafe { core::ptr::read_volatile(status as *const u64) } == 1 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// **Interrupt-Entry-Cache invalidieren** (global). Gibt es **nur** als QI-Deskriptor -- das ist
/// der Grund, warum B-3.1 Vorbedingung von B-3.2 ist und nicht Geschmackssache.
pub fn invalidate_iec_global() -> bool {
    if !qi_active() {
        return false;
    }
    qi_submit(QI_IEC_INV, 0)
}

/// **Interrupt-Entry-Cache fuer GENAU EINEN Index invalidieren.**
///
/// Deskriptorfelder (VT-d, `iec_inv_dsc`): Typ in 3:0, `G` (Granularitaet) in Bit 4
/// (`1` = index-selektiv), `IM` in 31:27, `IIDX` in 47:32. `IM = 0` heisst „genau dieser eine
/// Index" -- damit gibt es keine Ausrichtungsbedingung an einen Block, und eine Bedingung, die
/// nicht gilt, kann auch niemand vergessen.
///
/// **Warum nicht global.** Global ist bequem und richtig, wirft aber bei JEDER Vergabe die
/// Eintraege ALLER Geraete weg. Auf einer Maschine, die im Betrieb Treiber nachlaedt, zwingt
/// damit jedes neu geladene Geraet jedes andere zu einem Tabellendurchlauf. Der Preis ist ein
/// Deskriptor je Eintrag statt einer je Block, und das laeuft einmal beim Aufsetzen.
fn invalidate_iec_index(index: u16) -> bool {
    if !qi_active() {
        return false;
    }
    qi_submit(QI_IEC_INV | (1 << 4) | ((index as u64) << 32), 0)
}

// --- Die IRTE-Vergabe (Z22 P1, zweiter Teil -- 2026-08-10) ---------------------------------------
//
// Der Zuteiler selbst (Bitfeld, Formbedingungen, benannte Absagen, Reihenfolge) und die LOGIK des
// Selbsttests liegen in `irte.rs` und sind dort mit Literalen host-geprueft. Hier steht nur, was
// zwingend Hardware ist: WO die Tabelle liegt, wie ein Eintrag hineinkommt, wie er
// zurueckgelesen wird und wie die Einheit zur Kenntnisnahme gezwungen wird.
//
// **Die Trennlinie ist nicht Bequemlichkeit.** Ein Bit an der falschen Stelle in einer IRTE
// aeussert sich als „das Geraet unterbricht einfach nicht" -- ohne Fault, ohne Meldung. Was in
// QEMU Geraet, Treiber und Glueck braucht, ist als reine Funktion in Sekunden pruefbar; hier
// bleibt so wenig wie moeglich.

/// Der Index-Allokator der Tabelle von **Einheit 0**.
///
/// Nur Einheit 0: `ir_enable` setzt Interrupt Remapping ausschliesslich dort auf (s. dort; der
/// Rest von B-3.2 steht als offener Punkt). Ein Geraet hinter Einheit 1..n bekaeme hier einen
/// Handle, den seine Einheit gar nicht nachschlaegt -- deshalb verlangt [`irte_vergabe_moeglich`]
/// ausdruecklich `unit_count() == 1`, statt still zu vergeben.
static IRTE_ALLOC: caprock_sync::SpinLock<super::irte::IrteAllocator> =
    caprock_sync::SpinLock::new(super::irte::IrteAllocator::new());

/// Zugriff auf die echte Tabelle im Speicher + die echte Invalidierung.
struct IrtHardware {
    tabelle: u64,
}

impl super::irte::IrtZugriff for IrtHardware {
    fn schreibe_hi(&mut self, index: u16, hi: u64) {
        // SAFETY: `tabelle` ist das in `ir_enable` allozierte, identity-gemappte 4-KiB-Frame;
        // `index < IRT_ENTRIES` ist durch den Allokator garantiert (256 * 16 B == 4096 B).
        unsafe { write_entry(self.tabelle + index as u64 * 16 + 8, hi) };
    }
    fn schreibe_lo(&mut self, index: u16, lo: u64) {
        // SAFETY: wie oben.
        unsafe { write_entry(self.tabelle + index as u64 * 16, lo) };
    }
    fn invalidiere(&mut self, index: u16) -> bool {
        invalidate_iec_index(index)
    }
    fn lies(&self, index: u16) -> (u64, u64) {
        // SAFETY: wie oben.
        unsafe {
            let a = self.tabelle + index as u64 * 16;
            (
                core::ptr::read_volatile(a as *const u64),
                core::ptr::read_volatile((a + 8) as *const u64),
            )
        }
    }
}

/// **Kann die Vergabe ueberhaupt etwas eintragen?** — die Sprechprobe.
///
/// Ein Pruefer, der ueber Abwesenheit entscheidet („kein Geraet hat einen Vektor bekommen"),
/// muss belegen koennen, dass er sprechfaehig ist. Diese Funktion sagt, ob die Vorbedingungen
/// stehen, **ohne** etwas zu vergeben — damit ist „0 Vektoren vergeben" von „konnte gar nicht"
/// unterscheidbar.
///
/// Alle vier Bedingungen sind noetig: ohne Tabelle gibt es nichts zu schreiben, ohne `IRE`
/// schaut die Einheit nicht hinein, ohne QI ist der Eintrag nicht invalidierbar, und bei mehr
/// als einer Einheit weiss diese Datei nicht, hinter welcher das Geraet haengt.
pub fn irte_vergabe_moeglich() -> bool {
    IRT[0].load(Ordering::Acquire) != 0 && ir_active() && qi_active() && unit_count() == 1
}

/// **Einen MSI-/MSI-X-Vektorblock an ein Geraet vergeben.**
///
/// Das ist die Stelle, an der aus „dieses Geraet gehoert dieser PD" wird: „dieses Geraet darf
/// diesen Vektor auf diesem Kern ausloesen, und kein anderes". Getragen wird das von `SVT = 01`
/// und `SID` im Eintrag -- die PD schreibt ihre MSI-X-Tabelle selbst (sie besitzt das Fenster)
/// und koennte den Handle einer FREMDEN IRTE eintragen; die Quellpruefung der Einheit weist das
/// ab.
///
/// `rid` ist die Requester-ID des Geraets, also `(bus << 8) | (dev << 3) | func` -- genau das
/// Format einer `SID`.
///
/// **Geraeteneutral.** Hier kommt keine Geraeteklasse vor: NVMe, Netzkarte, USB-Controller und
/// GPU laufen durch dieselbe Vergabe. Was sich unterscheidet, ist [`super::irte::Vektorform`] --
/// und die **nennt der Aufrufer**, statt dass sie hier geraten wird.
pub fn irte_vergib(
    rid: u32,
    basis_vektor: u8,
    apic_id: u32,
    form: super::irte::Vektorform,
    anzahl: usize,
) -> Result<super::irte::MsiZiel, super::irte::VergabeFehler> {
    let tabelle = IRT[0].load(Ordering::Acquire);
    let aktiv = irte_vergabe_moeglich();
    let w = super::irte::Vektorwunsch {
        basis_vektor,
        apic_id,
        sid: (rid & 0xFFFF) as u16,
        form,
        anzahl,
    };
    let mut hw = IrtHardware { tabelle };
    let mut a = IRTE_ALLOC.lock();
    super::irte::vergib(&mut hw, &mut a, aktiv, &w)
}

/// **Einen Eintrag ZURUECKLESEN** — `(lo, hi)` roh, so wie er in der Tabelle steht.
///
/// Fuer den Pruefer, und die Betonung liegt auf *zurueck*: *ein Pruefer, der die gepruefte Groesse
/// NACHRECHNET statt sie zu lesen, prueft eine zweite Wirklichkeit* (`iova_window_clear_of_msi`).
/// Wer belegen will, dass `SVT/SID` gesetzt ist, muss die Bytes lesen, die die Einheit liest — die
/// Kodierungsfunktion noch einmal aufzurufen belegte nur, dass sie deterministisch ist.
///
/// `None` = es gibt gar keine Tabelle (Remapping nicht scharf). Das ist von „Eintrag steht auf
/// nicht vorhanden" zu unterscheiden, und deshalb ein `Option` und kein Nullpaar.
pub fn irte_lesen(index: u16) -> Option<(u64, u64)> {
    let tabelle = IRT[0].load(Ordering::Acquire);
    if tabelle == 0 {
        return None;
    }
    use super::irte::IrtZugriff;
    Some(IrtHardware { tabelle }.lies(index))
}

/// **Was in einem Eintrag WIRKLICH steht** — das Ergebnis von [`irte_lesen`], entschluesselt.
///
/// Die Entschluesselung liegt hier und nicht beim Aufrufer: die Bitlage steht in `irte_build`, und
/// ein Pruefer, der sie ein zweites Mal hinschreibt, prueft seine eigene Kopie. Wandert ein Feld,
/// wandert diese Funktion mit — der Kernel sieht Konjunkte, keine Schiebeoperationen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IrteBefund {
    /// Gibt es ueberhaupt eine Tabelle? (`false` = Remapping nicht scharf — nicht dasselbe wie
    /// „Eintrag nicht vorhanden", und der Unterschied ist die halbe Diagnose.)
    pub tabelle_da: bool,
    /// `P` — der Eintrag ist vorhanden.
    pub praesent: bool,
    /// Der eingetragene CPU-Vektor ist der erwartete.
    pub vektor_passt: bool,
    /// `SVT == 01` — **die Sicherheitsaussage**: die Einheit prueft die Quelle ueberhaupt.
    pub svt_gesetzt: bool,
    /// Die eingetragene `SID` ist die dieses Geraets — sie prueft die **richtige** Quelle.
    pub sid_passt: bool,
}

/// **Einen Eintrag gegen die Erwartung pruefen** — gelesen, nicht nachgerechnet.
///
/// `rid` ist die Requester-ID des Geraets; die `SID` wird daraus **mit derselben Regel** abgeleitet
/// wie in [`irte_vergib`] (untere 16 Bit). Das ist Absicht und nicht der `iova_window_clear_of_msi`
/// -Fehler: Zuteiler und Pruefer brauchen EINE Quelle fuer die Ableitung — was der Pruefer
/// unabhaengig beibringt, ist der **Tabelleninhalt**, und der ist die Groesse, um die es geht.
pub fn irte_pruefen(index: u16, erwarteter_vektor: u8, rid: u32) -> IrteBefund {
    let Some((lo, hi)) = irte_lesen(index) else {
        return IrteBefund::default();
    };
    IrteBefund {
        tabelle_da: true,
        praesent: lo & 1 == 1,
        vektor_passt: ((lo >> 16) & 0xFF) as u8 == erwarteter_vektor,
        svt_gesetzt: (hi >> 18) & 0b11 == super::irte::SVT_SID,
        sid_passt: hi & 0xFFFF == (rid & 0xFFFF) as u64,
    }
}

/// **Einen vergebenen Mehrvektor-Block gegen die Erwartung prüfen** — gelesen, nicht
/// nachgerechnet (s. [`irte_lesen`]).
///
/// Nach einem Mehrvektor-Grant (`anzahl > 1`) sagt ein einzelnes [`irte_pruefen`] nichts
/// über die übrigen Zeilen: ein Block, dessen dritte Zeile ohne `SID` stünde, wäre auf
/// diesem Vektor von jedem Gerät benutzbar — und ein Prüfer, der nur Zeile 0 liest, sähe
/// das nie (dieselbe Form wie `jeder_eintrag_des_blocks_traegt_die_quellpruefung` auf der
/// Host-Seite).
///
/// Geprüft wird jede Zeile des Tickets (`handle + i` gegen `basis_vektor + i`, dieselbe
/// `rid`): `geprueft` zählt die gelesenen Zeilen, `ok` heisst „alle tragen Präsenz, Vektor
/// und Quellprüfung", `erster_fehler` nennt die erste Zeile, die es nicht tut (`None` bei
/// `ok`). Ein Block, dessen Vektorfolge nicht darstellbar wäre, ist kein Prüf-, sondern
/// ein Vergabefehler (`vergib` weist ihn vorher ab) — läuft die Folge hier trotzdem über,
/// zählt die Zeile als Fehler, statt umzulaufen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrteBlockBefund {
    /// Wie viele Zeilen gelesen wurden.
    pub geprueft: usize,
    /// Alle gelesenen Zeilen tragen Präsenz, Vektor und Quellprüfung.
    pub ok: bool,
    /// Erste fehlerhafte Zeile (`None` bei `ok`).
    pub erster_fehler: Option<u16>,
}

pub fn irte_block_pruefen(ziel: &super::irte::MsiZiel, rid: u32) -> IrteBlockBefund {
    let mut b = IrteBlockBefund { geprueft: 0, ok: true, erster_fehler: None };
    for i in 0..ziel.anzahl() {
        let index = ziel.handle().wrapping_add(i as u16);
        let Some(vektor) = ziel.basis_vektor().checked_add(i as u8) else {
            b.ok = false;
            if b.erster_fehler.is_none() {
                b.erster_fehler = Some(index);
            }
            continue;
        };
        let f = irte_pruefen(index, vektor, rid);
        b.geprueft += 1;
        if !(f.tabelle_da && f.praesent && f.vektor_passt && f.svt_gesetzt && f.sid_passt) {
            b.ok = false;
            if b.erster_fehler.is_none() {
                b.erster_fehler = Some(index);
            }
        }
    }
    b
}

/// Einen vergebenen Block wieder einziehen (Teardown, Hot-Reload).
pub fn irte_zieh_ein(ziel: &super::irte::MsiZiel) -> Result<(), super::irte::VergabeFehler> {
    let tabelle = IRT[0].load(Ordering::Acquire);
    if tabelle == 0 {
        return Err(super::irte::VergabeFehler::TabelleNichtAktiv);
    }
    let mut hw = IrtHardware { tabelle };
    let mut a = IRTE_ALLOC.lock();
    super::irte::zieh_ein(&mut hw, &mut a, ziel)
}

/// Belegung der Remapping-Tabelle: `(belegt, frei, hoechststand)` — Telemetrie fuer den Bericht.
/// Die Zahl, mit der sich eine Tabellengroesse begruenden laesst, statt sie zu raten.
pub fn irte_belegung() -> (usize, usize, usize) {
    let a = IRTE_ALLOC.lock();
    (a.belegt_anzahl(), a.frei_anzahl(), a.hoechststand())
}

// --- Selbsttest der Vergabe (nur mit `selftest`) -------------------------------------------------
//
// **Die LOGIK steht in `irte.rs`, nicht hier.** Ein Selbsttest, der nur auf einer bootenden
// Maschine laeuft, ist so viel wert, wie diese Maschine bootet -- und ein Bauwerkzeug kann das
// jederzeit kippen (in dieser Sitzung: `objcopy` 2.46.1, s. Bericht). Hardware ist nur, WO die
// Tabelle liegt; alles andere ist gegen einen speicherbasierten Stellvertreter host-geprueft,
// mit Positivkontrolle und drei Gegenproben.

/// Der Bericht — der Typ liegt bei der Logik.
#[cfg(feature = "selftest")]
pub use super::irte::VergabeBericht as IrteVergabeBericht;
/// Die Vektorform, die [`irte_vergib`] als Argument nimmt — **hier weitergereicht, damit der
/// Kernel mit der IOMMU-Fassade spricht und nicht mit dem Kodierungsmodul.** `irte` ist am
/// Crate-Kopf nicht exportiert, und das soll so bleiben: wer die Kodierung braucht, arbeitet an
/// der HAL, nicht am Kernel.
pub use super::irte::Vektorform;
/// Das Ticket, das [`irte_vergib`] zurueckgibt und [`irte_zieh_ein`] als einziges annimmt —
/// **aus demselben Grund hier weitergereicht wie [`Vektorform`]**: wer die Vergabe zuruecknehmen
/// koennen will, muss das Ticket AUFBEWAHREN, und dazu muss er seinen Typ nennen koennen. Nur den
/// Handle zu behalten liesse den Eintrag praesent stehen, ohne dass ihn noch jemand einziehen
/// kann -- genau das, wogegen das `#[must_use]` am Typ steht.
pub use super::irte::MsiZiel;
/// Der Mehrvektor-Satz (`irte.rs`, Abschnitt „Mehrere Vektoren je Gerät") — **aus demselben
/// Grund hier weitergereicht wie [`MsiZiel`]**: der Kernel schreibt je Zeile
/// (`MsixZeile`/`MsixZeilen`), schützt je Vektor (`ReTriggerSchutz`/`MeldeUrteil`) und
/// entscheidet gerätelokal (`geraet_vektoren`/`GeraeteVerdikt` gegen
/// `VEKTOREN_JE_GERAET_MAX`), und dazu muss er die Typen nennen können. `irte` selbst
/// bleibt am Crate-Kopf unerreicht — die Fassade ist diese Datei, nicht das Kodierungsmodul.
pub use super::irte::{
    geraet_vektoren, GeraeteVerdikt, MeldeUrteil, MsixZeile, MsixZeilen, ReTriggerSchutz,
    VEKTOREN_JE_GERAET_MAX,
};
/// Der Befund der Block-Rücklesung ([`irte_block_pruefen`]) — gelesen, nicht nachgerechnet.

/// **Der Selbsttest der IRTE-Vergabe** — laeuft in [`init`], unmittelbar nachdem Interrupt
/// Remapping aufgesetzt ist.
///
/// Warum genau dort: `crate::selftest::run()` laeuft im x86-Hochlauf **vor**
/// `dma_enforcer_init()`; dort ist die Tabelle noch nicht da, und ein Pruefer, der strukturell
/// nichts sehen kann, ist kein Pruefer. Spaeter im Hochlauf naehme er einem Treiber Eintraege weg.
#[cfg(feature = "selftest")]
pub fn irte_selbsttest() -> IrteVergabeBericht {
    let tabelle = IRT[0].load(Ordering::Acquire);
    let aktiv = irte_vergabe_moeglich();
    // Zwei synthetische Quellen: die Aussage ist „zwei QUELLEN werden getrennt", und die Einheit
    // prueft die SID im Eintrag -- nicht, ob dort ein Geraet steckt. Ein echtes Geraet zu nehmen
    // hiesse, ihm im Hochlauf eine IRTE wegzunehmen.
    let (Some(sid_a), Some(sid_b)) = (
        super::irte::sid_from_bdf(0, 0x1e, 0),
        super::irte::sid_from_bdf(0, 0x1e, 1),
    ) else {
        return IrteVergabeBericht::default();
    };
    let mut hw = IrtHardware { tabelle };
    let mut a = IRTE_ALLOC.lock();
    super::irte::selbsttest(&mut hw, &mut a, aktiv, sid_a, sid_b, 0x70)
}

/// Globale Invalidierung des Kontext-Caches (Gegenstück zum `CMD_SYNC`-Round-Trip auf ARM):
/// die Einheit muss die Anforderung quittieren, indem sie `ICC` wieder löscht.
///
/// **Über alle Einheiten** (B-3.3). Vorher lief das über die Kurzformen `write64`/`read64`, also
/// ausschliesslich auf Einheit 0 — die übrigen behielten ihre Kontext-Cache-Einträge. Nach einem
/// `context_clear` hiesse das: das Gerät ist in der Tabelle entwaffnet, die Einheit übersetzt es
/// aber weiter aus dem Cache. Ein Teardown, der genau die Eigenschaft nicht herstellt, für die er
/// da ist.
pub fn invalidate_context_cache() -> bool {
    if !present() {
        return false;
    }
    let mut all_ok = true;
    for u in 0..unit_count() {
        if !unit_speaking(u) {
            // Eine stumme Einheit kann nicht quittieren — das ist ein Fehlschlag, keine
            // erledigte Invalidierung. Sonst hinge ein `true` an einer Einheit, die schweigt.
            all_ok = false;
            continue;
        }
        // Sobald QI laeuft, ist der Registerpfad architektonisch VERBOTEN (VT-d 6.5.2) -- nicht
        // bloss unnoetig. Deshalb umschalten statt beides halten (B-3.1). QI wird heute nur auf
        // Einheit 0 aufgesetzt (s. `qi_enable`); die uebrigen fahren den Registerpfad, der fuer
        // sie der spezifikationsgemaesse ist, solange ihr `GSTS.QIES` aus ist.
        if u == 0 && qi_active() {
            // Granularitaet global: Bits 5:4 = 01.
            if !qi_submit(QI_CC_INV | (1 << 4), 0) {
                all_ok = false;
            }
            continue;
        }
        wu64(u, REG_CCMD, CCMD_ICC | CCMD_CIRG_GLOBAL);
        let mut ok = false;
        for _ in 0..1_000_000 {
            if ru64(u, REG_CCMD) & CCMD_ICC == 0 {
                ok = true; // quittiert
                break;
            }
            core::hint::spin_loop();
        }
        all_ok &= ok;
    }
    all_ok
}

// --- Übersetzungstabellen (Schritt 3) ---------------------------------------------------------
//
// Root-Tabelle (256 Einträge à 16 Byte, indiziert mit dem Bus) -> Kontext-Tabelle je Bus
// (256 Einträge à 16 Byte, indiziert mit `dev<<3 | func`) -> Second-Level-Pagetable.
//
// Zwei Eigenschaften, die aus der Fähigkeitslesung folgen und nicht aus Gewohnheit:
//
// * **`ECAP.C == 0`** — die Einheit ist *nicht* page-walk-kohärent und sieht die Schreibvorgänge
//   auf Root-, Kontext- und SLPT-Einträge nicht durch den Cache. Jeder Eintrag braucht deshalb
//   einen `clflush` **vor** der zugehörigen Invalidierung, und zwar auch auf den Zwischenebenen
//   beim Aufbau eines neuen Zweigs, nicht nur auf dem Blatt. Damit das keine Disziplinfrage
//   bleibt, geht **jeder** Tabellenschreibzugriff durch [`write_entry`], das nie ohne Flush
//   zurückkehrt. Auf kohärenten Einheiten wird der Flush zum No-Op — die Stelle bleibt sichtbar.
// * **`ECAP.SC == 0`** — keine Snoop Control. Das `SNP`-Bit im SLPTE ist dann **reserviert**, und
//   reservierte Bits faulten; es darf also nicht gesetzt werden. Folge: No-Snoop-DMA bleibt
//   unkohärent, und `dma_granule() == 1` auf x86 gilt unter der Bedingung, dass die zugeteilten
//   Geräte kein No-Snoop verwenden (s. `docs/invariants.md` §2a).

/// SLPTE/Kontext-Bits.
const SL_READ: u64 = 1 << 0;
const SL_WRITE: u64 = 1 << 1;
/// `SNP` (Snoop) — **nur** zulässig, wenn `ECAP.SC` gesetzt ist.
const SL_SNP: u64 = 1 << 11;
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

/// Kontext-Eintrag: `P` (Present) und `FPD` (Fault Processing Disable).
const CTX_PRESENT: u64 = 1 << 0;
/// `FPD` ist **wörtlich `CD.R` noch einmal**: gesetzt, werden Faults dieses Geräts nicht
/// aufgezeichnet — und ein Negativtest bestünde wieder an einer strukturell leeren Beobachtung.
/// Die Konstante existiert, damit die Entscheidung *dagegen* im Code steht und nicht in einer
/// vergessenen Null.
#[allow(dead_code)]
const CTX_FPD: u64 = 1 << 1;

/// Einen Tabelleneintrag schreiben — **nie ohne Flush**.
///
/// # Safety
/// `addr` muss eine gültige, identity-gemappte Tabellenadresse sein.
unsafe fn write_entry(addr: u64, v: u64) {
    unsafe { core::ptr::write_volatile(addr as *mut u64, v) };
    flush_entry(addr);
}

/// Cache-Line der Adresse zurückschreiben, falls **irgendeine** Einheit nicht page-walk-kohärent
/// ist.
///
/// Gemittelt über alle Einheiten, nicht von Einheit 0 übernommen: es genügt eine inkohärente
/// Einheit, damit der Eintrag geflusht werden muss (s. `AGG_COHERENT`).
fn flush_entry(addr: u64) {
    if agg_coherent_walk() {
        return; // jede Einheit sieht den Schreibzugriff ohnehin
    }
    // SAFETY: `clflush` auf eine gemappte Adresse ist nebenwirkungsfrei; `sfence` ordnet.
    unsafe {
        core::arch::asm!("clflush [{a}]", "sfence", a = in(reg) addr, options(nostack, preserves_flags));
    }
}

/// Eine Region `[iova, iova+len)` -> `[pa, ...)` in die **Second-Level-Pagetable** `slpt`
/// einhängen. `levels` = 3 (39 Bit) oder 4 (48 Bit).
///
/// **Richtung fällt hier heraus statt hinzuzukommen:** Präsenz *ist* `R|W`, ein eigenes P-Bit
/// gibt es nicht. `ToDevice` (das Gerät liest) wird damit zu „nur R", und ein solcher Puffer ist
/// gegen das Gerät schreibgeschützt — dieselbe Eigenschaft wie über die SMMU-Rechte, ohne
/// Zusatzarbeit.
///
/// # Safety
/// `slpt` ist eine gültige, identity-gemappte SLPT-Wurzel; `alloc` liefert genullte 4-KiB-Frames.
pub unsafe fn slpt_map(
    slpt: u64,
    levels: u32,
    iova: u64,
    pa: u64,
    len: u64,
    readable: bool,
    writable: bool,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    if len == 0 || iova % 4096 != 0 || pa % 4096 != 0 || len % 4096 != 0 {
        return false;
    }
    // Gemittelt über alle Einheiten: fehlt EINER die Snoop Control, ist `SNP` für sie ein
    // reserviertes Bit und würde faulten. Das Minimum ist hier die einzige sichere Richtung.
    let snp = if agg_snoop_control() {
        SL_SNP
    } else {
        0 // ohne ECAP.SC ist das Bit reserviert -> setzen würde faulten
    };
    let leaf_bits = snp
        | if readable { SL_READ } else { 0 }
        | if writable { SL_WRITE } else { 0 };
    if leaf_bits & (SL_READ | SL_WRITE) == 0 {
        return false; // ohne R und W wäre der Eintrag nicht präsent
    }
    let mut v = iova;
    let mut p = pa;
    while v < iova + len {
        let mut tbl = slpt;
        // Von der Wurzel abwärts; die unterste Ebene trägt das Blatt.
        for lvl in (1..levels).rev() {
            let shift = 12 + 9 * lvl;
            let idx = ((v >> shift) & 0x1ff) as u64;
            let e = tbl + idx * 8;
            // SAFETY: `tbl` ist ein gültiger Tabellen-Frame, `idx` < 512.
            let cur = unsafe { core::ptr::read_volatile(e as *const u64) };
            tbl = if cur & (SL_READ | SL_WRITE) != 0 {
                cur & ADDR_MASK
            } else {
                let Some(next) = alloc() else { return false };
                // Zwischenebenen sind ebenfalls flush-pflichtig.
                unsafe { write_entry(e, (next & ADDR_MASK) | SL_READ | SL_WRITE) };
                next
            };
        }
        let idx = ((v >> 12) & 0x1ff) as u64;
        // SAFETY: `tbl` gültig, `idx` < 512.
        unsafe { write_entry(tbl + idx * 8, (p & ADDR_MASK) | leaf_bits) };
        v += 4096;
        p += 4096;
    }
    true
}

/// Eine Region wieder aus der SLPT entfernen (Blätter auf 0). Die Tabellen-Frames bleiben.
///
/// # Safety
/// wie [`slpt_map`].
pub unsafe fn slpt_unmap(slpt: u64, levels: u32, iova: u64, len: u64) {
    let mut v = iova;
    while v < iova + len {
        let mut tbl = slpt;
        let mut ok = true;
        for lvl in (1..levels).rev() {
            let shift = 12 + 9 * lvl;
            let idx = ((v >> shift) & 0x1ff) as u64;
            // SAFETY: gültiger Tabellen-Frame.
            let cur = unsafe { core::ptr::read_volatile((tbl + idx * 8) as *const u64) };
            if cur & (SL_READ | SL_WRITE) == 0 {
                ok = false;
                break;
            }
            tbl = cur & ADDR_MASK;
        }
        if ok {
            let idx = ((v >> 12) & 0x1ff) as u64;
            // SAFETY: gültiger Tabellen-Frame.
            unsafe { write_entry(tbl + idx * 8, 0) };
        }
        v += 4096;
    }
}

/// Alle Frames einer SLPT einsammeln (Blätterebenen zuerst) und über `free` zurückgeben.
///
/// # Safety
/// `slpt` ist eine gültige SLPT-Wurzel mit `levels` Ebenen.
pub unsafe fn slpt_free(slpt: u64, levels: u32, free: &mut dyn FnMut(u64)) {
    // SAFETY: read-only-Scan über gültige Tabellen-Frames.
    unsafe fn walk(tbl: u64, lvl: u32, free: &mut dyn FnMut(u64)) {
        if lvl > 1 {
            for i in 0..512u64 {
                let e = unsafe { core::ptr::read_volatile((tbl + i * 8) as *const u64) };
                if e & (SL_READ | SL_WRITE) != 0 {
                    unsafe { walk(e & ADDR_MASK, lvl - 1, free) };
                }
            }
        }
        free(tbl);
    }
    unsafe { walk(slpt, levels, free) };
}

/// Einen **Kontext-Eintrag** für `rid` schreiben: zeigt auf `slpt`, Domain `did`, Adressbreite
/// aus `levels`. `root` ist die Root-Tabelle; `alloc` liefert bei Bedarf eine Kontext-Tabelle.
///
/// `FPD` bleibt **aus** — Faults dieses Geräts sollen aufgezeichnet werden.
///
/// # Safety
/// `root` ist eine gültige, identity-gemappte Root-Tabelle.
pub unsafe fn context_set(
    root: u64,
    rid: u32,
    slpt: u64,
    did: u16,
    levels: u32,
    alloc: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    let bus = ((rid >> 8) & 0xff) as u64;
    let devfn = (rid & 0xff) as u64;
    let re = root + bus * 16;
    // SAFETY: `root` gültig, `bus` < 256.
    let cur = unsafe { core::ptr::read_volatile(re as *const u64) };
    let ctx = if cur & 1 != 0 {
        cur & ADDR_MASK
    } else {
        let Some(t) = alloc() else { return false };
        unsafe { write_entry(re, (t & ADDR_MASK) | 1) };
        t
    };
    // AW-Kodierung: 1 = 39 Bit (3 Level), 2 = 48 Bit (4 Level).
    let aw = (levels as u64).saturating_sub(2);
    let lo = (slpt & ADDR_MASK) | CTX_PRESENT; // T = 00 (nur untranslated Requests)
    let hi = aw | ((did as u64) << 8);
    // SAFETY: `ctx` gültig, `devfn` < 256.
    unsafe {
        write_entry(ctx + devfn * 16, lo);
        write_entry(ctx + devfn * 16 + 8, hi);
    }
    true
}

/// Einen Kontext-Eintrag entfernen (nicht mehr präsent -> Default-Block).
///
/// # Safety
/// wie [`context_set`].
pub unsafe fn context_clear(root: u64, rid: u32) {
    let bus = ((rid >> 8) & 0xff) as u64;
    let devfn = (rid & 0xff) as u64;
    // SAFETY: `root` gültig.
    let cur = unsafe { core::ptr::read_volatile((root + bus * 16) as *const u64) };
    if cur & 1 == 0 {
        return;
    }
    let ctx = cur & ADDR_MASK;
    // SAFETY: `ctx` gültig, `devfn` < 256.
    unsafe {
        write_entry(ctx + devfn * 16, 0);
        write_entry(ctx + devfn * 16 + 8, 0);
    }
}

/// Globale **IOTLB**-Invalidierung (Registerpfad).
///
/// Der Kernel fährt bewusst **nur** den Registerpfad, nicht zusätzlich Queued Invalidation:
/// `ECAP.QI` ist zwar vorhanden, aber beide Wege parallel zu betreiben wäre zwei Mechanismen für
/// dieselbe Aufgabe. QI bleibt als Optimierung vorgemerkt; solange `GCMD.QIE` aus ist, ist der
/// Registerpfad der spezifikationsgemäße.
pub fn invalidate_iotlb_global() -> bool {
    let mut all_ok = true;
    for u in 0..unit_count() {
        // Einheit 0 laeuft ueber die Warteschlange, sobald QI steht -- der Registerpfad ist dann
        // architektonisch verboten (B-3.1). **Die uebrigen Einheiten nicht:** QI wird heute nur
        // auf Einheit 0 aufgesetzt (je Einheit eigene Frames noetig, s. `init`); fuer sie ist der
        // Registerpfad der spezifikationsgemaesse, solange ihr `GSTS.QIES` aus ist.
        if u == 0 && qi_active() {
            // Granularitaet global: Bits 5:4 = 01.
            if !qi_submit(QI_IOTLB_INV | (1 << 4), 0) {
                all_ok = false;
            }
            continue;
        }
        // Eine stumme Einheit wurde hier frueher per `else { continue }` uebersprungen -- und der
        // Rueckgabewert blieb `true`. Eine nicht durchgefuehrte Invalidierung, die als erfolgreich
        // gemeldet wird, ist genau die Zusage ohne Deckung, gegen die B-3.3 antritt.
        let Some(c) = VtdCaps::read_unit(u).filter(|_| unit_speaking(u)) else {
            all_ok = false;
            continue;
        };
        let iro = (((c.raw_ecap >> 8) & 0x3ff) * 16) as usize;
        let iotlb = iro + 8;
        const IVT: u64 = 1 << 63;
        const IIRG_GLOBAL: u64 = 1 << 60;
        wu64(u, iotlb, IVT | IIRG_GLOBAL);
        let mut ok = false;
        for _ in 0..1_000_000 {
            if ru64(u, iotlb) & IVT == 0 {
                ok = true;
                break;
            }
            core::hint::spin_loop();
        }
        all_ok &= ok;
    }
    all_ok
}

/// **Nach jeder Tabellenänderung**: erst Kontext-Cache, dann IOTLB.
///
/// Die Reihenfolge ist nicht beliebig: umgekehrt könnte ein noch gecachter Kontexteintrag
/// zwischen den beiden Schritten neue IOTLB-Einträge erzeugen, und die IOTLB-Invalidierung
/// liefe ins Leere.
///
/// Mit `CAP.CM == 1` gilt das **auch nach dem Anlegen** einer Übersetzung, nicht nur nach dem
/// Entfernen — dort sind nicht-präsente Einträge cachebar. Der Kernel invalidiert deshalb
/// unbedingt und protokolliert `CM` nur: das ist die konservative Variante, die überall hält.
pub fn sync_tables() -> bool {
    let a = invalidate_context_cache();
    let b = invalidate_iotlb_global();
    a && b
}

/// Die Root-Tabelle der Einheit 0 (`0` = noch keine).
pub fn root_table() -> u64 {
    ROOT_TABLE[0].load(Ordering::Acquire)
}
