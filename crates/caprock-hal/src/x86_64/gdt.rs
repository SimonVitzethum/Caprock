//! GDT + **per-Kern-TSS mit IST-Stacks** (x86_64).
//!
//! Im Long Mode ist Segmentierung weitgehend abgeschaltet — die GDT wird trotzdem gebraucht:
//! sie liefert die **Selektoren** für Ring 0 und Ring 3 (Privilegwechsel per `iretq`/`syscall`
//! laufen über CS/SS) und die **TSS** mit `RSP0`: dorthin schaltet die CPU den Stack um, wenn
//! ein Interrupt aus Ring 3 eintrifft. Ohne gültiges `RSP0` würde jeder Trap aus dem User-Modus
//! auf dem User-Stack landen — ein direkter Privilegienbruch.
//!
//! # Warum die TSS per Kern ist (2026-08-10)
//!
//! Bis hierher gab es **genau eine** TSS, und nur der BSP führte `ltr` aus; `init_ap` lud
//! bewusst keine. Zwei Dinge stehen in der TSS, und beide sind kernlokal:
//!
//! * **`RSP0`** — der Kernel-Stack, auf den ein Trap aus Ring 3 umschaltet. Schrieben zwei Kerne
//!   dasselbe Feld, bekäme der Trap des einen den Kernel-Stack des Threads, der zuletzt auf dem
//!   *anderen* Kern lief. Das ist keine Diagnosefrage, sondern Speicherkorruption über
//!   Kerngrenzen.
//! * **`IST[0..7]`** — die Stackzeiger, auf die die CPU bei einem Vektor mit IST-Index
//!   **bedingungslos** umschaltet. Eine einzige TSS hiesse: eine einzige Wache, auf einem Kern.
//!
//! # Der Grund, aus dem das jetzt gebaut wird: `#DF` braucht einen eigenen Stack
//!
//! Eine Guard-Page unter dem Kernel-Stack macht aus einem Überlauf einen `#PF`. Dessen Handler
//! versucht, seinen Frame auf **denselben** übergelaufenen Stack zu pushen — das faultet erneut,
//! und aus zwei Faults wird ein `#DF`. Ohne IST-Stack pusht auch der `#DF`-Handler dorthin, und
//! daraus wird ein **Triple Fault ohne jede Ausgabe**: der Rechner startet neu, die Logdatei ist
//! leer, und im Protokoll steht nichts, woraus sich die Ursache ablesen liesse. Genau dieses Bild
//! (`KEIN OUTPUT`) ist der teuerste Fehlerzustand dieses Projekts.
//!
//! # Die Falle, gegen die hier ausdrücklich gebaut wird
//!
//! **Der IST-Index im IDT-Gate ist EINSBASIERT.** `ist = 1` lädt `TSS.ist[0]`, `ist = 0` heisst
//! „kein IST". Ein Off-by-one ist **lautlos**, bis es zählt: der Handler läuft dann auf dem
//! Stack eines *anderen* Vektors und sieht völlig gesund aus, solange die beiden nie
//! zusammentreffen. Deshalb trägt jeder Vektor hier ein Paar aus Gate-Index ([`IST_DF`] …) und
//! Feldindex ([`ISTF_DF`] …), und die `ist`-Prüfzeile im Kernel liest die Zuordnung **aus der
//! echten TSS und der echten IDT zurück**, statt sie nachzurechnen.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

// ================================================================================================
// Kapazität, Grössen, Indizes
// ================================================================================================

/// Wie viele Kerne dieser Kernel mit **eigener TSS** versorgen kann.
///
/// Die Zahl ist eine **statische** Grenze, weil die TSS und ihre IST-Stacks stehen müssen, bevor
/// es einen Speicherallokator gibt: `gdt::init()` läuft auf dem BSP **vor** `mmu::init_primary`.
/// Sie ist damit ein bewusster Kompromiss und keine Architekturgrenze — [`init_ap`] weist einen
/// Kern darüber **ab** (fail-closed), statt ihn ohne TSS laufen zu lassen. Der Bericht druckt
/// Kapazität und Verbrauch nebeneinander, damit „passt gerade so" nicht unsichtbar bleibt.
///
/// 16 ist gewählt, nicht geraten: beide QEMU-Suiten fahren `-smp 4`, das ist Faktor 4 Luft; die
/// Kosten sind `16 * 3 * IST_STACK_BYTES` BSS (s. [`IST_STACK_BYTES`]).
pub const MAX_TSS_CORES: usize = 16;

/// Anzahl der IST-Stacks je Kern (`#DF`, `NMI`, `#MC`).
pub const IST_ANZ: usize = 3;

/// **IST-Index im IDT-Gate** für `#DF` (Vektor 8) — die CPU lädt damit `TSS.ist[0]`.
pub const IST_DF: u8 = 1;
/// IST-Index im IDT-Gate für `NMI` (Vektor 2) — lädt `TSS.ist[1]`.
pub const IST_NMI: u8 = 2;
/// IST-Index im IDT-Gate für `#MC` (Vektor 18) — lädt `TSS.ist[2]`.
pub const IST_MC: u8 = 3;

/// Feldindex des `#DF`-Stacks in `TSS.ist` (= [`IST_DF`] − 1).
pub const ISTF_DF: usize = 0;
/// Feldindex des `NMI`-Stacks in `TSS.ist`.
pub const ISTF_NMI: usize = 1;
/// Feldindex des `#MC`-Stacks in `TSS.ist`.
pub const ISTF_MC: usize = 2;

/// Grösse **eines** IST-Stacks.
///
/// # Die Zahl ist gemessen, nicht gegriffen
///
/// Ein `#DF`-Handler auf diesem Kernel tut genau drei Dinge, und alle drei sind zählbar:
///
/// | Posten | Bytes | Herkunft |
/// |---|---|---|
/// | CPU-Frame + Stub-Pushes (`TrapFrame`) | 176 | `size_of::<TrapFrame>()`, 22 × 8 |
/// | `handle_exception` → `df_fatal` | wenige 100 | Rahmen der beiden Funktionen |
/// | `console::emit_fmt(format_args!(..))` | der Löwenanteil | `core::fmt` mit 10 Argumenten |
///
/// Der letzte Posten ist der einzige, den man nicht abzählen kann — `core::fmt` legt je Argument
/// einen `Formatter` samt Zwischenpuffer an. Er ist deshalb **gemessen** worden, mit demselben
/// Verfahren wie die EL0-Kernel-Stacks (`kernel/src/kstackmark.rs`): der IST-Stack wird beim
/// Hochlauf mit einem Muster gefüllt, ein echter `#DF` wird provoziert, und danach wird von unten
/// gezählt, wie viele Worte das Muster noch tragen. Das Ergebnis steht in der `ist`-Berichtszeile
/// jedes Laufs, nicht nur in diesem Kommentar — eine Zahl, die nur im Kommentar steht, veraltet
/// still.
///
/// **Gemessen am 2026-08-10** (`tools/df-sonde.sh`, echter `#DF` aus einem verbogenen `RSP`,
/// abgelesen als `IST[#DF] ENDSTAND` **nach** der vollständigen Diagnoseausgabe):
/// **816 von 4096 B** (19,9 %). 4 KiB sind damit Faktor 5,0 — knapper wäre flatterhaft, weiter
/// wäre BSS ohne Aussage. Zum Vergleich: der `TrapFrame` allein ist 176 B, der ganze Rest sind
/// die Rahmen von `df_fatal` und `core::fmt`.
///
/// **Der Endstand und nicht der Zwischenstand:** eine Messung mitten im Bericht kennt die Tiefe
/// der danach folgenden `emit_fmt`-Aufrufe strukturell nicht und wäre systematisch zu klein —
/// dieselbe Form wie ein Beobachtungsfenster, das vor dem gemessenen Ereignis endet.
///
/// **Wer die Meldung erweitert, muss neu messen.** Genau deshalb druckt jeder `#DF` seinen
/// Endstand selbst, statt sich auf diesen Kommentar zu verlassen, und `tools/df-sonde.sh` lässt
/// den Lauf durchfallen, sobald mehr als die **Hälfte** verbraucht ist.
///
/// **Und warum es trotzdem keine „sichere" Zahl ist:** ein `#DF`-Stack hat selbst **keine**
/// Guard-Page. Läuft er über, schreibt er in den Stack des Nachbarvektors. Dagegen steht die
/// Wasserstandsmessung: `unberuehrt == 0` heisst „aufgebraucht **oder** nie gefüllt", und beide
/// lassen die `ist`-Zeile durchfallen.
pub const IST_STACK_BYTES: usize = 4096;

// ================================================================================================
// TSS + GDT
// ================================================================================================

/// **Die x86-TSS ist von Hardware wegen unausgerichtet**: `rsp0` liegt auf Offset 4, jedes `u64`
/// darin also auf einer 4-mod-8-Adresse. Deshalb `packed` und deshalb `read_volatile`/
/// `write_volatile` statt gewöhnlicher Feldzugriffe — die Form ist von der Architektur vorgegeben
/// und nicht wählbar.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Tss {
    _res0: u32,
    /// Stackzeiger, auf den bei einem Trap **aus Ring 3** umgeschaltet wird.
    rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    _res1: u64,
    /// `ist[n]` wird von einem IDT-Gate mit IST-Index `n + 1` geladen (**einsbasiert**, s. Modul).
    ist: [u64; 7],
    _res2: u64,
    _res3: u16,
    iomap_base: u16,
}

impl Tss {
    const LEER: Tss = Tss {
        _res0: 0,
        rsp0: 0,
        rsp1: 0,
        rsp2: 0,
        _res1: 0,
        ist: [0; 7],
        _res2: 0,
        _res3: 0,
        iomap_base: core::mem::size_of::<Tss>() as u16, // kein I/O-Bitmap -> Ring 3 kein Port-I/O
    };
}

static mut TSS: [Tss; MAX_TSS_CORES] = [Tss::LEER; MAX_TSS_CORES];

/// Die IST-Stacks **eines** Kerns. `align(16)`, weil die CPU beim Eintritt über ein IST-Gate den
/// Stackzeiger auf 16 Byte ausrichtet — eine unausgerichtete Obergrenze verschenkte sonst still
/// bis zu 15 Byte und machte die Wasserstandsrechnung um denselben Betrag falsch.
#[repr(C, align(16))]
struct IstStacks([[u8; IST_STACK_BYTES]; IST_ANZ]);

static mut IST: [IstStacks; MAX_TSS_CORES] =
    [const { IstStacks([[0u8; IST_STACK_BYTES]; IST_ANZ]) }; MAX_TSS_CORES];

/// Ohne diese Zusicherung wäre die flache Adressarithmetik in [`ist_region`] falsch, sobald
/// jemand `IST_STACK_BYTES` auf etwas setzt, das kein Vielfaches der Ausrichtung ist.
const _: () = assert!(core::mem::size_of::<IstStacks>() == IST_ANZ * IST_STACK_BYTES);
const _: () = assert!(IST_STACK_BYTES % 16 == 0);
/// Die drei Gate-Indizes müssen zu den drei Feldindizes passen — das ist die Off-by-one-Klammer.
const _: () = assert!(IST_DF as usize == ISTF_DF + 1);
const _: () = assert!(IST_NMI as usize == ISTF_NMI + 1);
const _: () = assert!(IST_MC as usize == ISTF_MC + 1);
const _: () = assert!(IST_ANZ == 3);

/// GDT-Slot des **ersten** TSS-Deskriptors. Davor: null, kcode, kdata, udata, ucode.
const TSS_SLOT0: usize = 5;
/// Länge der GDT: 5 feste Einträge + **zwei** Slots je TSS (ein 64-bit-TSS-Deskriptor ist
/// 16 Byte gross und belegt damit zwei GDT-Einträge).
const GDT_LEN: usize = TSS_SLOT0 + 2 * MAX_TSS_CORES;

static mut GDT: [u64; GDT_LEN] = [0; GDT_LEN];

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

/// Code-/Datensegment-Deskriptor im Long Mode: nur die Flags zählen (Basis/Limit ignoriert).
const fn seg(code: bool, dpl: u64) -> u64 {
    let mut d = (1 << 44) | (1 << 47) | (dpl << 45); // S=1 (nicht-System), P=1, DPL
    d |= 1 << 41; // RW (Daten schreibbar / Code lesbar)
    if code {
        d |= (1 << 43) | (1 << 53); // Executable + L (64-bit)
    }
    d
}

/// Selektor des TSS-Deskriptors von Kern `core`.
pub const fn tss_selector(core: usize) -> u16 {
    ((TSS_SLOT0 + 2 * core) * 8) as u16
}

// ================================================================================================
// Telemetrie — GEZÄHLT WIRD DIE GELEGENHEIT, NICHT DAS UNGLÜCK
// ================================================================================================

/// Wie oft ein Kern `RSP0` gesetzt hat, also eine Rückkehr **nach Ring 3** vorbereitet hat.
///
/// **Das ist die Messgrösse zu der Frage „laufen Ring-3-Threads auf Sekundärkernen?".** Ein
/// Melder, der erst beim Unglück spricht (zwei Kerne schreiben dasselbe `RSP0`), wäre in jedem
/// gesunden Lauf stumm, und niemand wüsste, ob er sprechfähig ist. Die **Gelegenheit** dagegen ist
/// in jedem Lauf zählbar: steht hier für einen Sekundärkern etwas anderes als 0, hat dort ein
/// Ring-3-Thread gelegen — und dann wäre die eine gemeinsame TSS bereits heute ein Riss und nicht
/// erst nach der nächsten Änderung.
#[cfg(feature = "selftest")]
#[allow(clippy::declare_interior_mutable_const)]
static RING3_RSP0: [AtomicU64; MAX_TSS_CORES] =
    [const { AtomicU64::new(0) }; MAX_TSS_CORES];

/// Wie oft [`set_kernel_stack`] die eigene Kern-Identität **nicht** bestimmen konnte.
///
/// Muss 0 bleiben. Der einzige Weg dorthin ist ein Kern, der `RSP0` setzt, ohne dass `ltr` je
/// gelaufen wäre — dann ist das TR-Register keiner unserer Selektoren. Fail-closed: es wird in
/// diesem Fall **nichts** geschrieben (in die TSS eines fremden Kerns zu schreiben wäre genau der
/// Fehler, den dieses Modul beseitigt), und der Zähler macht das Schweigen sichtbar.
static TR_UNBEKANNT: AtomicU64 = AtomicU64::new(0);

/// Kerne, denen [`init_ap`] die TSS **verweigert** hat (Index ≥ [`MAX_TSS_CORES`]).
static OHNE_TSS: AtomicU64 = AtomicU64::new(0);

/// Grösse eines EL0-Kernel-Stacks, vom Kernel gemeldet (`0` = unbekannt).
///
/// Die HAL kennt die Zahl nicht und darf sie nicht erfinden — sie gehört dem Kernel
/// (`system::USER_KSTACK_SIZE`). Gebraucht wird sie **nur** im `#DF`-Bericht, um aus `RSP0` die
/// Basis des betroffenen Stacks zu rechnen.
static KSTACK_GROESSE: AtomicU64 = AtomicU64::new(0);

/// Der Kernel meldet die Grösse eines EL0-Kernel-Stacks (muss eine Zweierpotenz sein — die
/// Stacks sind auf ihre eigene Grösse ausgerichtet, darauf beruht die Basisrechnung).
pub fn set_kstack_geometrie(groesse: u64) {
    KSTACK_GROESSE.store(groesse, Ordering::Release);
}

/// Wie oft Kern `core` eine Rückkehr nach Ring 3 vorbereitet hat.
#[cfg(feature = "selftest")]
pub fn ring3_rueckkehr(core: usize) -> u64 {
    RING3_RSP0
        .get(core)
        .map_or(0, |c| c.load(Ordering::Relaxed))
}

/// Zähler „TR gehörte keinem bekannten Kern" (muss 0 sein).
pub fn tr_unbekannt() -> u64 {
    TR_UNBEKANNT.load(Ordering::Relaxed)
}

/// Zähler „Kern ohne TSS abgewiesen" (muss 0 sein).
pub fn ohne_tss() -> u64 {
    OHNE_TSS.load(Ordering::Relaxed)
}

// ================================================================================================
// Zugriff auf die Tabellen (für die `ist`-Prüfzeile und den `#DF`-Bericht)
// ================================================================================================

fn tss_ptr(core: usize) -> *mut Tss {
    // SAFETY: `core < MAX_TSS_CORES` wird von jedem Aufrufer geprüft; `addr_of_mut!` erzeugt
    // keinen Zwischenverweis auf das `static mut`.
    unsafe { core::ptr::addr_of_mut!(TSS).cast::<Tss>().add(core) }
}

/// `[base, base+len)` des IST-Stacks `idx` von Kern `core`, oder `(0, 0)`.
pub fn ist_region(core: usize, idx: usize) -> (u64, u64) {
    if core >= MAX_TSS_CORES || idx >= IST_ANZ {
        return (0, 0);
    }
    // SAFETY: reine Adressarithmetik innerhalb des statischen Feldes; die Grössenzusicherung
    // oben (`size_of::<IstStacks>() == IST_ANZ * IST_STACK_BYTES`) macht die flache Rechnung
    // exakt. Es wird nichts dereferenziert.
    let base = unsafe {
        core::ptr::addr_of!(IST)
            .cast::<u8>()
            .add(core * IST_ANZ * IST_STACK_BYTES + idx * IST_STACK_BYTES) as u64
    };
    (base, IST_STACK_BYTES as u64)
}

/// Der Wert, der in `TSS[core].ist[idx]` **wirklich steht** — zurückgelesen, nicht nachgerechnet.
///
/// Der Unterschied ist der ganze Zweck: ein Prüfer, der die Zuordnung nachrechnet, prüft eine
/// zweite Wirklichkeit (dieselbe Falle wie `iova_window_clear_of_msi`).
pub fn tss_ist(core: usize, idx: usize) -> u64 {
    if core >= MAX_TSS_CORES || idx >= 7 {
        return 0;
    }
    // SAFETY: gültiger Index; `read_volatile` auf ein Feld, das sonst nur `init`/`init_ap`
    // schreiben.
    unsafe { core::ptr::addr_of!((*tss_ptr(core)).ist[idx]).read_volatile() }
}

/// Der Wert, der in `TSS[core].rsp0` steht (Kernel-Stack-Top des zuletzt für Ring 3 vorbereiteten
/// Threads dieses Kerns; `0` = nie gesetzt).
pub fn tss_rsp0(core: usize) -> u64 {
    if core >= MAX_TSS_CORES {
        return 0;
    }
    // SAFETY: gültiger Index.
    unsafe { core::ptr::addr_of!((*tss_ptr(core)).rsp0).read_volatile() }
}

/// Kern-Index aus dem **geladenen Task-Register** — der billige, von Ring 3 nicht manipulierbare
/// Weg an die eigene Identität.
///
/// **Warum nicht `cpu::core_id()`:** das ist ein `cpuid` und damit unter KVM ein bedingungsloser
/// VM-Exit. [`set_kernel_stack`] steht auf dem heissesten Pfad des Systems (jede Rückkehr nach
/// Ring 3); dort hat ein `cpuid` in diesem Projekt schon einmal 3556 statt 51 Zyklen gekostet.
///
/// **Warum nicht `GS_BASE`:** ein Ring-3-Thread darf `mov gs, ax` ausführen und setzt damit die
/// GS-Basis auf 0. Ohne `swapgs` an jeder Ein-/Austrittsstelle wäre die Kern-Identität also von
/// User-Code steuerbar — und der Kernel schriebe `RSP0` nach Adresse 4. `TR` kann Ring 3 nicht
/// anfassen (`ltr` ist Ring-0-only).
///
/// Gibt `None`, wenn `TR` keiner unserer TSS-Selektoren ist (dann ist auf diesem Kern nie `ltr`
/// gelaufen).
pub fn core_from_tr() -> Option<usize> {
    let sel: u16;
    // SAFETY: `str` liest nur den Selektorteil des Task-Registers; keine Speicherwirkung.
    unsafe { asm!("str {0:x}", out(reg) sel, options(nomem, nostack, preserves_flags)) };
    let idx = (sel & !0b111) as usize / 8;
    if idx < TSS_SLOT0 || (idx - TSS_SLOT0) % 2 != 0 {
        return None;
    }
    let c = (idx - TSS_SLOT0) / 2;
    (c < MAX_TSS_CORES).then_some(c)
}

/// Wie viele Kerne dieser Kernel mit eigener TSS versorgen kann.
pub const fn kapazitaet() -> usize {
    MAX_TSS_CORES
}

// ================================================================================================
// Aufbau + Laden
// ================================================================================================

/// Alle GDT-Einträge und **alle** TSS aufbauen (nur der BSP; die Tabelle ist global) und auf dem
/// eigenen Kern laden.
pub fn init() {
    // SAFETY: statische Tabellen, ausschließlich hier beim Boot beschrieben; das Laden von
    // GDTR/TR und der Segmentregister ist eine erlaubte Low-Level-Domäne.
    unsafe {
        let gdt = &mut *core::ptr::addr_of_mut!(GDT);
        gdt[0] = 0;
        gdt[1] = seg(true, 0); // 0x08 kernel code
        gdt[2] = seg(false, 0); // 0x10 kernel data
        gdt[3] = seg(false, 3); // 0x18 user data
        gdt[4] = seg(true, 3); //  0x20 user code

        // **Je Kern ein Deskriptor, und je Kern eine TSS.** Der BSP baut alle auf — die GDT ist
        // global, ein AP lädt sie nur noch und führt sein eigenes `ltr` aus. Andersherum (jeder
        // Kern schreibt seinen Eintrag selbst) gäbe es ein Fenster, in dem ein Kern eine GDT
        // liest, die ein anderer gerade beschreibt.
        for c in 0..MAX_TSS_CORES {
            let t = tss_ptr(c);
            (*t) = Tss::LEER;
            for i in 0..IST_ANZ {
                // **`base + len`, nicht `base`**: der Stack wächst nach unten, der Zeiger in der
                // TSS ist die OBERGRENZE. Ein `base` hier wäre der klassische Fehler — der erste
                // Push liefe unter die Region.
                let (b, l) = ist_region(c, i);
                (*t).ist[i] = b + l;
            }
            let tss_addr = t as u64;
            let limit = (core::mem::size_of::<Tss>() - 1) as u64;
            gdt[TSS_SLOT0 + 2 * c] = limit
                | ((tss_addr & 0xFF_FFFF) << 16)
                | (0x9 << 40) // Typ: 64-bit TSS available
                | (1 << 47) // present
                | (((tss_addr >> 24) & 0xFF) << 56);
            gdt[TSS_SLOT0 + 2 * c + 1] = tss_addr >> 32;
        }

        lade_gdt_und_segmente();
    }
    // TR laden: der BSP nimmt seinen eigenen Selektor. Schlägt das fehl (Kernindex über der
    // Kapazität), läuft dieser Kernel gar nicht erst an — der BSP ist der Kern, der bootet.
    let core = super::cpu::core_id();
    if core >= MAX_TSS_CORES {
        OHNE_TSS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    lade_tr(core);
}

/// GDT auf einem **Application Processor** laden und die **eigene** TSS scharf machen.
///
/// Gibt `false` zurück, wenn dieser Kern über [`MAX_TSS_CORES`] liegt. Der Aufrufer MUSS den Kern
/// dann anhalten, statt ihn Threads einplanen zu lassen: ein Kern ohne TSS hat kein `RSP0`, und
/// der erste Trap eines Ring-3-Threads auf ihm landete auf dem User-Stack — ein direkter
/// Privilegienbruch. **Fail-closed heisst hier: gar nicht laufen.**
#[must_use]
pub fn init_ap() -> bool {
    let core = super::cpu::core_id();
    if core >= MAX_TSS_CORES {
        OHNE_TSS.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    // SAFETY: dieselbe statische, vom BSP fertig aufgebaute GDT laden + Segmente neu laden.
    unsafe { lade_gdt_und_segmente() };
    // Der eigene TSS-Deskriptor: **jeder Kern hat einen eigenen**, deshalb ist das Busy-Bit, das
    // `ltr` setzt, hier kein Problem mehr. Genau daran scheiterte die frühere Fassung — ein
    // zweites `ltr` auf DASSELBE TSS wäre ein #GP.
    lade_tr(core);
    true
}

/// # Safety
/// Nur beim Hochlauf eines Kerns aufzurufen; lädt GDTR und alle Segmentregister neu.
unsafe fn lade_gdt_und_segmente() {
    // SAFETY: Zusicherung des Aufrufers.
    unsafe {
        let gdt = &*core::ptr::addr_of!(GDT);
        let ptr = DescriptorTablePointer {
            limit: (core::mem::size_of_val(gdt) - 1) as u16,
            base: gdt.as_ptr() as u64,
        };
        asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
        // CS lässt sich nur per Far-Return neu laden.
        asm!(
            "push 0x08",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            tmp = lateout(reg) _,
            options(preserves_flags)
        );
        asm!(
            "mov ax, 0x10", "mov ds, ax", "mov es, ax", "mov ss, ax",
            "mov ax, 0", "mov fs, ax", "mov gs, ax",
            out("ax") _, options(nostack, preserves_flags)
        );
    }
}

fn lade_tr(core: usize) {
    let sel = tss_selector(core);
    // SAFETY: `sel` zeigt auf einen vom BSP aufgebauten, präsenten 64-bit-TSS-Deskriptor, den
    // auf diesem Kern noch niemand geladen hat (Busy-Bit frei).
    unsafe { asm!("ltr {0:x}", in(reg) sel, options(nostack, preserves_flags)) };
}

/// Kernel-Stackzeiger setzen, auf den ein Trap **aus Ring 3** umschaltet — in der TSS **dieses**
/// Kerns.
///
/// Bei jedem Wechsel zu einem Ring-3-Thread zu setzen (dessen eigener Kernel-Stack), sonst
/// liefe der nächste Trap dieses Threads auf dem Stack des vorigen.
pub fn set_kernel_stack(top: u64) {
    let Some(core) = core_from_tr() else {
        // **Nicht raten.** In die TSS von Kern 0 zu schreiben, weil die eigene unbekannt ist,
        // wäre genau der Fehler, gegen den dieses Modul gebaut ist — nur mit einem Zähler
        // darüber. Erreichbar ist der Zweig nur ohne `ltr`, und dann hat die CPU ohnehin keine
        // TSS, aus der sie `RSP0` läse.
        TR_UNBEKANNT.fetch_add(1, Ordering::Relaxed);
        return;
    };
    #[cfg(feature = "selftest")]
    RING3_RSP0[core].fetch_add(1, Ordering::Relaxed);
    // SAFETY: `core < MAX_TSS_CORES` (aus `core_from_tr`); nur `rsp0` wird geschrieben (die CPU
    // liest es beim Privilegwechsel).
    unsafe {
        core::ptr::addr_of_mut!((*tss_ptr(core)).rsp0).write_volatile(top);
    }
}

/// Basis des EL0-Kernel-Stacks, auf dessen Obergrenze `rsp0` zeigt — oder `None`, wenn die
/// Geometrie nicht gemeldet wurde.
///
/// Die Stacks sind auf ihre eigene Grösse ausgerichtet (`claim_user_kstack_masked`), deshalb ist
/// `(rsp0 - 1) & !(groesse - 1)` die Basis. `rsp0 - 1`, weil `rsp0` die **Obergrenze** ist und
/// damit bereits im nächsten Block läge, wenn der Frame exakt am Top begänne.
pub fn kstack_basis_von_rsp0(rsp0: u64) -> Option<(u64, u64)> {
    let g = KSTACK_GROESSE.load(Ordering::Acquire);
    if g == 0 || !g.is_power_of_two() || rsp0 < g {
        return None;
    }
    Some(((rsp0 - 1) & !(g - 1), g))
}
