//! **Per-Kern-TSS und IST-Stacks** — die Kernelseite: Füllung, Messhaken und die Prüfzeile `ist`.
//!
//! # Wozu das gebaut ist
//!
//! Der tiefste Kernelpfad dieses Systems (`SYS_LOAD` -> Ed25519 + SHA-2 **im Kernel**) benutzt
//! 12008 der 16384 Byte eines EL0-Kernel-Stacks — 73 %, Reserve 4376 Byte, und ohne Guard-Page
//! schreibt ein Überlauf **still** in fremden Kernelspeicher. Das ist keine Diagnosefrage,
//! sondern eine aus Ring 3 erreichbare Privilegieneskalation.
//!
//! Die Guard-Page macht aus dem Überlauf einen `#PF`. Deren Handler pusht seinen Frame auf
//! **denselben** übergelaufenen Stack -> zweiter Fault -> `#DF`. Ohne IST-Stack pusht der
//! `#DF`-Handler dorthin ebenfalls -> **Triple Fault, leere Logdatei**. Die Guard-Page allein
//! macht das Bild also *schlechter*: aus einer stillen Korruption würde ein stiller Neustart.
//! Erst mit einem eigenen Stack für `#DF` wird daraus eine Meldung.
//!
//! # Was diese Zeile misst — und warum an der Wirkung
//!
//! **Ein IST-Eintrag, der nie benutzt wurde, ist von einem falsch aufgesetzten nicht zu
//! unterscheiden.** Der Gate-Index ist einsbasiert (`ist = 1` lädt `TSS.ist[0]`); ein Off-by-one
//! lädt einfach den Stack des Nachbarvektors, und alles sieht gesund aus, bis die beiden einmal
//! zusammentreffen. Deshalb wird nicht die Konfiguration behauptet, sondern **ausgelöst**: jeder
//! Kern schiesst ein `int 2` und ein `int 18` und liest hinterher zurück, auf welchem Stack der
//! Handler stand.
//!
//! Für `#DF` (8) geht das über `int 8` **nicht**: bei einem Software-Interrupt schiebt die CPU
//! keinen Fehlercode ein, der Stub für Vektor 8 erwartet aber einen — das Frame-Layout wäre um
//! 8 Byte verschoben, und die Sonde produzierte einen echten Kernelfehler statt einer Messung.
//! Der `#DF` wird deshalb in `tools/df-sonde.sh` **echt** ausgelöst, in einem eigenen QEMU-Lauf,
//! dessen Ausgabe der Beleg ist. Hier steht für Vektor 8 die **strukturelle** Aussage
//! (Gate-Index, Stackzeiger in der TSS, Region), zurückgelesen aus IDT und TSS statt
//! nachgerechnet.
//!
//! # Jeder Kern misst seine eigenen Stacks
//!
//! Die Sekundärkerne messen in `ap_entry`, **bevor** sie sich als online melden — der Bericht des
//! BSP kann damit keine Aussage lesen, die noch gar nicht entstanden ist. Eine Messung nur auf
//! dem Bootkern wäre genau der Fehler, den dieses Modul beseitigt: eine Wache auf einem Kern.

use caprock_hal::{self as hal, print, println};
use core::sync::atomic::{AtomicU64, Ordering};

/// **Der Messhaken für die HAL.** Die HAL darf `kstackmark` nicht kennen (Kerngrenze), also
/// reicht sie nur die Region herüber und bekommt die benutzte Tiefe zurück.
///
/// Fail-closed: wurde nie gefüllt, trägt schon das unterste Wort kein Muster, `unberuehrt` gibt
/// `0` und die benutzte Tiefe ist die **volle** Länge. Ein Ausfall der Messung sieht damit aus
/// wie der schlimmste Messwert und nicht wie der beste.
fn wasserstand(base: u64, len: u64) -> u64 {
    if base == 0 || len < 8 {
        return len;
    }
    // SAFETY: `base`/`len` kommen aus `gdt::ist_region` bzw. aus `TSS.rsp0` + der vom Kernel
    // selbst gemeldeten Stackgeometrie — beides identity-gemappte, 8-Byte-ausgerichtete
    // Kernelregionen. Gelesen wird nur.
    let frei = unsafe { crate::kstackmark::unberuehrt(base as usize, len as usize) } as u64;
    len - frei.min(len)
}

/// Die IST-Stacks **dieses** Kerns mit dem Wasserstandsmuster füllen.
///
/// Direkt nach `gdt::init`/`gdt::init_ap` zu rufen, also bevor die CPU je auf einen dieser Stacks
/// umschalten kann.
///
/// **Bewusst NICHT hinter `selftest`**: der Wasserstand steht im `#DF`-Bericht, und der ist im
/// Auslieferungskernel genauso wichtig wie in der Testkonfiguration — dort sogar mehr, weil dort
/// niemand danebensteht. Die Kosten sind 12 KiB `memset` je Kern, einmal beim Hochlauf.
pub fn stacks_fuellen() {
    let Some(core) = hal::gdt::core_from_tr() else {
        return;
    };
    for i in 0..hal::gdt::IST_ANZ {
        let (base, len) = hal::gdt::ist_region(core, i);
        if base == 0 {
            continue;
        }
        let p = base as *mut u64;
        for w in 0..(len as usize / 8) {
            // SAFETY: `w < len/8`, also innerhalb der von `ist_region` zugesicherten, exklusiv
            // diesem Kern gehörenden Region. `write_volatile`, damit die Schleife nicht
            // wegoptimiert wird — für den Compiler ist ein nie gelesener Stack toter Speicher.
            unsafe { p.add(w).write_volatile(crate::kstackmark::MUSTER) };
        }
    }
}

/// Einmal auf dem BSP: der HAL sagen, wie sie messen kann und wie gross ein EL0-Kernel-Stack ist.
pub fn haken_installieren() {
    hal::exception::set_wasserstand_hook(wasserstand);
    // **Wer**, sperrfrei. Die Luecke stand als offener Punkt im Bericht dieses Moduls: der
    // `#DF`-Text nennt den betroffenen STACK, aber nicht den THREAD, weil Slot und Id in
    // gesperrten Strukturen stehen und ein Double Fault nichts sperren darf. `FP_OWNER` traegt
    // die Angabe atomar -- eine Zeile hier, eine dort, und die erste Frage vor dem Bericht ist
    // beantwortet.
    hal::exception::set_ring3_kontext_hook(crate::system::ring3_kontext_von_kern);
    hal::gdt::set_kstack_geometrie(crate::system::USER_KSTACK_SIZE as u64);
}

// ================================================================================================
// DIE MESSUNG
// ================================================================================================

/// Bit 0: `int 2` kam an **und** landete in der NMI-Region **dieses** Kerns.
const B_NMI: u64 = 1;
/// Bit 1: dasselbe für `int 18` und die `#MC`-Region.
const B_MC: u64 = 2;
/// Bit 2: die drei IST-Zeiger dieses Kerns stehen auf der **Obergrenze** ihrer eigenen Region und
/// sind paarweise verschieden.
const B_ZEIGER: u64 = 4;
/// Bit 3: die Messung ist auf diesem Kern überhaupt gelaufen (**Sprechprobe**).
const B_GELAUFEN: u64 = 8;
/// Bit 4: die Stacks dieses Kerns tragen das Füllmuster (sonst misst der `#DF`-Bericht nichts).
const B_GEFUELLT: u64 = 16;

const B_ALLE: u64 = B_NMI | B_MC | B_ZEIGER | B_GELAUFEN | B_GEFUELLT;

#[cfg(feature = "selftest")]
#[allow(clippy::declare_interior_mutable_const)]
static ERGEBNIS: [AtomicU64; hal::gdt::MAX_TSS_CORES] =
    [const { AtomicU64::new(0) }; hal::gdt::MAX_TSS_CORES];

/// Liegt `addr` in `[base, base+len)`?
fn drin(addr: u64, base: u64, len: u64) -> bool {
    base != 0 && addr >= base && addr < base + len
}

/// Einen IST-Vektor **auslösen** und prüfen, wo der Handler stand.
///
/// # Safety
/// `vector` muss 2 oder 18 sein — beide legen keinen CPU-Fehlercode ab, das Frame-Layout des
/// Stubs stimmt also. Für Vektoren MIT Fehlercode (insbesondere 8) wäre der Frame verschoben.
#[cfg(feature = "selftest")]
unsafe fn schuss(core: usize, vector: u64, feld: usize) -> bool {
    if !hal::exception::ist_sonde_armieren(vector) {
        return false;
    }
    // SAFETY: Zusicherung des Aufrufers; der Handler erkennt die scharfe Sonde und kehrt per
    // `iretq` zurück, ohne Kernelzustand anzufassen.
    unsafe {
        match vector {
            2 => core::arch::asm!("int 2", options(nostack)),
            _ => core::arch::asm!("int 18", options(nostack)),
        }
    }
    // **Zwei getrennte Aussagen, und beide werden gebraucht.** „Nicht mehr scharf" heisst, der
    // Schuss ist angekommen (ohne das wäre eine 0 unten von „nie gelaufen" nicht zu
    // unterscheiden); die Lage sagt, ob der RICHTIGE Stack geladen wurde.
    if hal::exception::ist_sonde_scharf(core) {
        return false;
    }
    let (b, l) = hal::gdt::ist_region(core, feld);
    drin(hal::exception::ist_sonde_rsp(core), b, l)
}

/// Die Messung **dieses** Kerns. Aus `kernel_main` (BSP) bzw. `ap_entry` (AP) zu rufen, jeweils
/// direkt nachdem die eigene TSS steht.
#[cfg(feature = "selftest")]
pub fn messen() {
    let Some(core) = hal::gdt::core_from_tr() else {
        return; // ohne TSS läuft dieser Kern ohnehin nicht weiter (s. `ap_entry`)
    };
    let mut bits = B_GELAUFEN;

    // (1) Die Zeiger: **zurückgelesen** aus der echten TSS, nicht nachgerechnet. Ein Prüfer, der
    //     die geprüfte Grösse nachrechnet, prüft eine zweite Wirklichkeit.
    let mut zeiger_ok = true;
    let mut tops = [0u64; hal::gdt::IST_ANZ];
    for i in 0..hal::gdt::IST_ANZ {
        let (b, l) = hal::gdt::ist_region(core, i);
        let z = hal::gdt::tss_ist(core, i);
        // `b + l`, nicht `b`: der Stack wächst nach unten, in der TSS steht die OBERGRENZE.
        zeiger_ok &= b != 0 && z == b + l;
        tops[i] = z;
    }
    // paarweise verschieden — teilten sich zwei Vektoren einen Stack, wäre der eine im anderen
    // nicht mehr diagnostizierbar.
    for i in 0..hal::gdt::IST_ANZ {
        for j in (i + 1)..hal::gdt::IST_ANZ {
            zeiger_ok &= tops[i] != tops[j];
        }
    }
    if zeiger_ok {
        bits |= B_ZEIGER;
    }

    // (2) Die Füllung: ohne sie misst der `#DF`-Bericht den Wasserstand nicht.
    let (db, dl) = hal::gdt::ist_region(core, hal::gdt::ISTF_DF);
    // SAFETY: eigene, identity-gemappte IST-Region.
    if unsafe { crate::kstackmark::unberuehrt(db as usize, dl as usize) } as u64 == dl {
        bits |= B_GEFUELLT;
    }

    // (3) Die Wirkung: auslösen und nachsehen, wo der Handler stand.
    // SAFETY: 2 und 18 legen keinen CPU-Fehlercode ab (s. `schuss`).
    if unsafe { schuss(core, 2, hal::gdt::ISTF_NMI) } {
        bits |= B_NMI;
    }
    // SAFETY: dito.
    if unsafe { schuss(core, 18, hal::gdt::ISTF_MC) } {
        bits |= B_MC;
    }

    ERGEBNIS[core].store(bits, Ordering::Release);
}

#[cfg(not(feature = "selftest"))]
pub fn messen() {}

/// Die Gate-Indizes, **zurückgelesen aus der IDT**. `#PF` MUSS 0 tragen (s. `ist_fuer_vektor`).
#[cfg(feature = "selftest")]
fn gates_ok() -> bool {
    hal::exception::idt_ist(8) == hal::gdt::IST_DF
        && hal::exception::idt_ist(2) == hal::gdt::IST_NMI
        && hal::exception::idt_ist(18) == hal::gdt::IST_MC
        && hal::exception::idt_ist(14) == 0
}

/// **Das Urteil der `ist`-Zeile** — an EINER Stelle, damit `all_done()` und der Bericht dieselbe
/// Wirklichkeit lesen. Reine Funktion über die laufenden Zähler, darf also gepollt werden.
#[cfg(feature = "selftest")]
pub fn urteil() -> bool {
    let n = crate::system::num_cores();
    gates_ok()
        && hal::gdt::kapazitaet() >= n
        && hal::gdt::tr_unbekannt() == 0
        && hal::gdt::ohne_tss() == 0
        && n > 0
        && (0..n).all(|c| ERGEBNIS.get(c).is_some_and(|e| e.load(Ordering::Acquire) == B_ALLE))
}

#[cfg(not(feature = "selftest"))]
pub fn urteil() -> bool {
    true
}

/// Die Berichtszeilen.
#[cfg(feature = "selftest")]
pub fn bericht() {
    let n = crate::system::num_cores();
    println!(
        "ist     : per-Kern-TSS -- Kapazitaet {} Kerne, benutzt {} · je Kern {} IST-Stacks zu \
         {} B · TR-unbekannt={} ohne-TSS={}",
        hal::gdt::kapazitaet(),
        n,
        hal::gdt::IST_ANZ,
        hal::gdt::IST_STACK_BYTES,
        hal::gdt::tr_unbekannt(),
        hal::gdt::ohne_tss(),
    );
    println!(
        "ist     : IDT-Gates (zurueckgelesen) -- #DF(8)={} NMI(2)={} #MC(18)={} · #PF(14)={} \
         (MUSS 0 sein: ein IST-Gate laedt bedingungslos und macht den #PF-Handler \
         nicht-wiedereintrittsfaehig)",
        hal::exception::idt_ist(8),
        hal::exception::idt_ist(2),
        hal::exception::idt_ist(18),
        hal::exception::idt_ist(14),
    );
    print!("ist     : Wirkung je Kern (int 2 / int 18 landeten auf dem EIGENEN Stack):");
    for c in 0..n.min(hal::gdt::MAX_TSS_CORES) {
        let e = ERGEBNIS[c].load(Ordering::Acquire);
        print!(
            " k{c}[nmi={} mc={} zeiger={} gefuellt={} gelaufen={}]",
            u8::from(e & B_NMI != 0),
            u8::from(e & B_MC != 0),
            u8::from(e & B_ZEIGER != 0),
            u8::from(e & B_GEFUELLT != 0),
            u8::from(e & B_GELAUFEN != 0),
        );
    }
    println!();
    // Der Wasserstand der IST-Stacks: in einem gesunden Lauf sind #DF und #MC unberuehrt, der
    // NMI-Stack traegt die Spur der Sonde. Genau daran ist ablesbar, dass hier ueberhaupt etwas
    // benutzt wurde -- und die Zahl ist die Grundlage fuer `IST_STACK_BYTES`.
    print!("ist     : IST-Wasserstand Kern 0 --");
    for (i, name) in ["#DF", "NMI", "#MC"].iter().enumerate() {
        let (b, l) = hal::gdt::ist_region(0, i);
        print!(" {name}={}/{} B", wasserstand(b, l), l);
    }
    println!(
        " (die Sonde beruehrt NMI und #MC; #DF bleibt hier unberuehrt und wird in \
         tools/df-sonde.sh echt gemessen)"
    );
    // **Die Antwort auf „laufen heute Ring-3-Threads auf Sekundaerkernen?"** -- gezaehlt wird die
    // GELEGENHEIT (jede vorbereitete Rueckkehr nach Ring 3), nicht das Unglueck. Ein Melder, der
    // nur beim Zusammenstoss spricht, waere in jedem gesunden Lauf stumm.
    print!("ist     : Ring-3-Rueckkehr je Kern --");
    for c in 0..n.min(hal::gdt::MAX_TSS_CORES) {
        print!(" k{c}={}", hal::gdt::ring3_rueckkehr(c));
    }
    println!(
        " (steht bei einem Sekundaerkern etwas anderes als 0, waere EINE gemeinsame TSS bereits \
         heute ein Riss und nicht erst nach der naechsten Aenderung)"
    );
    println!(
        "ist     : {} (C4/#DF: gemessen wird die WIRKUNG -- der Vektor wird ausgeloest und die \
         Frame-Adresse zurueckgelesen; ein IST-Eintrag, der nie benutzt wurde, ist von einem \
         falsch aufgesetzten nicht zu unterscheiden)",
        if urteil() { "ALL PASS" } else { "FAILURES" }
    );
}

#[cfg(not(feature = "selftest"))]
pub fn bericht() {}

// ================================================================================================
// DIE #DF-SONDE — ein ECHTER Double Fault, in einem EIGENEN Lauf
// ================================================================================================

/// Einen echten `#DF` provozieren: den Stackzeiger auf eine **nicht abgebildete** Adresse setzen
/// und pushen.
///
/// Ablauf: der `push` faultet (`#PF`, `CR2` = die kaputte Adresse). Die CPU will den `#PF`-Frame
/// ablegen — auf denselben kaputten Stack, denn `#PF` hat **keinen** IST (und darf keinen haben).
/// Der zweite Fault während der Auslieferung des ersten ist per Definition ein `#DF`, und dessen
/// Gate trägt IST 1. Das ist Zeichen für Zeichen das Bild eines Kernel-Stack-Überlaufs mit
/// Guard-Page, nur ohne auf die Guard-Page warten zu müssen.
///
/// **Hinter einem eigenen Feature**, nicht hinter `selftest`: der reguläre Lauf soll grün enden,
/// und ein Kernel, der sich absichtlich zerlegt, tut das nicht. Gefahren wird sie von
/// `tools/df-sonde.sh` in einem eigenen QEMU-Aufruf.
///
/// Die Adresse liegt kanonisch (48 Bit) und weit über jedem RAM, den der Speicherplan abdeckt —
/// damit gibt es einen `#PF` und nicht einen `#GP` wegen Nicht-Kanonizität. Der Unterschied ist
/// nicht kosmetisch: nur der `#PF`-Weg ist derselbe wie beim Stacküberlauf.
#[cfg(feature = "dfprobe")]
pub fn df_sonde_ausloesen() -> ! {
    println!(
        "dfsonde : loese jetzt einen ECHTEN #DF aus (RSP auf {:#x}, nicht abgebildet). \
         Ohne IST-Stack folgte hier ein Triple Fault OHNE JEDE AUSGABE.",
        DF_SONDE_RSP
    );
    // SAFETY(-Absicht): Genau dieser Zugriff SOLL fehlschlagen. Ab hier gibt es keine Rückkehr —
    // die Funktion divergiert, entweder über `df_fatal` (mit IST) oder über den Reset (ohne).
    unsafe {
        core::arch::asm!(
            "mov rsp, {0}",
            "push rax",
            in(reg) DF_SONDE_RSP,
            options(noreturn),
        )
    }
}

/// Kanonisch, 16-Byte-ausgerichtet, weit oberhalb jedes vom Speicherplan gedeckten RAM.
#[cfg(feature = "dfprobe")]
const DF_SONDE_RSP: u64 = 0x0000_3000_0000_0000;

/// **Die stärkere Sonde: ein Überlauf über die ECHTE Guard-Page**, nicht über irgendeine
/// unabgebildete Adresse.
///
/// [`df_sonde_ausloesen`] belegt den Mechanismus (`#PF` beim Frame-Ablegen → `#DF` → IST). Sie
/// belegt aber **nicht die Kette**, um die es geht: dass die Wache eines echten EL1-Stacks
/// getroffen wird, dass `CR2` dann auf genau diese Wache zeigt, und dass der Bericht das
/// **nachrechnet** statt es zu behaupten. Genau diese Nachrechnung ist seit heute im Bericht
/// (`cr2_deutung`), und ein Zweig, den nie ein Lauf betritt, ist eine Behauptung.
///
/// Der Weg: `RSP` auf den Fuss des EL1-Stacks setzen, den dieser Kern zuletzt scharf gemacht hat,
/// und zweimal ablegen. Das erste `push` schreibt noch in den Stack, das zweite eine Seite
/// darunter — in die Wache.
///
/// **Fail-closed:** ist kein Stack ermittelbar, wird das gesagt und die Sonde bricht ab, statt
/// auf eine erratene Adresse zu schreiben. Eine Sonde, die im Zweifel irgendwohin greift, belegt
/// im Erfolgsfall nicht, was sie zu belegen vorgibt.
#[cfg(feature = "dfprobe-wache")]
pub fn df_wache_ausloesen() -> ! {
    // **Die Wache wird GELESEN, nicht aus `TSS.rsp0` gerechnet.** Die erste Fassung tat das --
    // und traf einen Stack, dessen Thread laengst tot und dessen Wache damit wieder eingehaengt
    // war: beide `push` gingen durch, der Kernel lief in ein `#UD` statt in einen `#DF`. Ein
    // gerechneter Aufbau ist dieselbe Falle wie ein Pruefer, der die gepruefte Groesse
    // nachrechnet. Nebenbefund, der bleibt: `TSS.rsp0` kann auf einen TOTEN Stack zeigen.
    let Some(wache) = hal::mmu::erste_lebende_wache() else {
        println!(
            "dfsonde : KEINE stehende Wache gefunden -- Sonde bricht ab. Das ist ein \
             Aufbaufehler und kein Messergebnis: ohne Wache kann diese Sonde nichts belegen."
        );
        crate::arch::x86_64::system_off();
    };
    let kb = wache + 4096; // der Stackfuss liegt eine Seite ueber seiner Wache
    println!(
        "dfsonde : Ueberlauf ueber eine NACHWEISLICH stehende Wache bei {wache:#x} \
         (Stackfuss {kb:#x}). RSP auf den Stackfuss, zweimal ablegen -- das zweite trifft sie."
    );
    // SAFETY(-Absicht): das zweite `push` SOLL fehlschlagen. Ab hier keine Rueckkehr.
    unsafe {
        core::arch::asm!(
            "mov rsp, {0}",
            "push rax",
            "push rax",
            in(reg) kb + 8,
            options(noreturn),
        )
    }
}
