//! Boot-Selbsttests des Speicher- und Capability-Subsystems.
//!
//! Phase 2 (`memtest`): Allokator (alloc/split/restrict/transfer/free+Coalescing).
//! Phase 3 (`captest`): Capability-Space (install/copy/mint/move/delete/revoke,
//! CDT, Refcount-Finalisierung, Generations-Handles, keine Rechte-Eskalation).

use crate::system as mm;
use caprock_cap::CapError;
use caprock_hal::println;
use caprock_mem::{MemoryCap, Rights, PAGE};

fn check(prefix: &str, cond: bool, name: &str, fail: &mut bool) {
    if cond {
        println!("{prefix}: PASS  {name}");
    } else {
        println!("{prefix}: FAIL  {name}");
        *fail = true;
    }
}

/// Alle Selbsttests ausführen (Primärkern, vor SMP-Start).
pub fn run() {
    memtest();
    zerotest();
    captest();
    budgettest();
    dmaaligntest();
}

/// **Granularitäts-Bedingung der DMA-Cap** (ext-35): ein Puffer, dessen Anfang oder Länge nicht
/// auf dem Cache-Writeback-Granule liegt, darf gar nicht erst zu einer Cap werden.
///
/// Grund: `dc civac` (Invalidate nach einem Geräte-Write) verwirft **ganze** Cache-Zeilen. Liegt
/// in einer angebrochenen Randzeile fremder Speicher, verliert der seine noch nicht
/// zurückgeschriebenen Daten — ein Schaden **außerhalb** des Puffers, den weder die
/// Bounds-Prüfung noch die IOMMU sieht (beide betrachten den Puffer, nicht seine Nachbarschaft).
///
/// Auf kohärenten Architekturen (x86: kein Cache-Maintenance, Granule 1) gibt es die Bedingung
/// nicht — der Test meldet dort `SKIP` statt eine Eigenschaft zu behaupten, die es nicht gibt.
fn dmaaligntest() {
    let p = "dmaalign";
    let g = caprock_hal::mmu::dma_granule();
    if g <= 1 {
        println!("{p}: SKIP  (kohaerente Architektur, Granule {g} -> keine Bedingung)");
        println!("dmaalign: ALL PASS");
        return;
    }
    let mut fail = false;
    let region = mm::alloc(2 * PAGE, PAGE).expect("dmaalign region");
    let (base, len) = (region.base(), region.len());

    // Ausgerichtet -> muss angenommen werden.
    match mm::install_dma_cap(base, len, Rights::RW) {
        Ok(cap) => {
            check(p, true, "ausgerichtete Region wird angenommen", &mut fail);
            // Die Cap besitzt die Region jetzt; Loeschen gibt sie an den Allokator zurueck.
            let _ = mm::cap_delete(cap);
        }
        Err(_) => check(p, false, "ausgerichtete Region wird angenommen", &mut fail),
    }

    // Verschobener Anfang -> muss abgelehnt werden (angebrochene erste Zeile).
    let region2 = mm::alloc(2 * PAGE, PAGE).expect("dmaalign region2");
    let (b2, l2) = (region2.base(), region2.len());
    check(
        p,
        mm::install_dma_cap(b2 + 1, l2 - 1, Rights::RW) == Err(CapError::Unaligned),
        "unausgerichteter Anfang -> Unaligned",
        &mut fail,
    );
    // Angebrochene Laenge -> ebenfalls abgelehnt (angebrochene letzte Zeile).
    check(
        p,
        mm::install_dma_cap(b2, l2 - 1, Rights::RW) == Err(CapError::Unaligned),
        "angebrochene Laenge -> Unaligned",
        &mut fail,
    );
    // Nichts davon darf eine Cap erzeugt haben -> Region gehoert weiter uns.
    mm::free(region2);
    check(p, mm::cap_audit_cdt() == 0, "CDT konsistent (keine Cap aus Fehlschlaegen)", &mut fail);

    println!("dmaalign: {}", if fail { "FAILURES" } else { "ALL PASS" });
}

/// **Datenremanenz-Test** (ext-29): frisch allozierter Speicher ist IMMER genullt.
///
/// Der Allokator vergibt Regionen wieder, die zuvor einem anderen Subjekt gehörten. Ohne
/// Nullung könnte der neue Eigentümer die Restdaten des alten lesen — eine Vertraulichkeits-
/// lücke über Vertrauensgrenzen hinweg. Der Test beschreibt eine Region mit einem Muster,
/// gibt sie zurück, alloziert dieselbe Größe erneut und prüft, dass **kein einziges Byte**
/// des Musters überlebt hat.
fn zerotest() {
    let p = "zerotest";
    let mut fail = false;

    let size = 4 * PAGE;
    let a = mm::alloc(size, PAGE).expect("alloc a");
    let base = a.base();

    // Frische Allokation ist bereits genullt (auch fabrikfrisches RAM/Firmware-Reste).
    check(p, region_is_zero(base, size), "frische Allokation ist genullt", &mut fail);

    // Muster schreiben, Region zurückgeben.
    fill_region(base, size, 0xA5A5_5A5A_DEAD_BEEF);
    check(p, !region_is_zero(base, size), "Muster geschrieben (Kontrolle)", &mut fail);
    mm::free(a);

    // Erneut allozieren: derselbe Bereich, aber keine Restdaten mehr.
    let b = mm::alloc(size, PAGE).expect("alloc b");
    check(p, b.base() == base, "Allokator gibt dieselbe Region erneut aus", &mut fail);
    check(
        p,
        region_is_zero(b.base(), b.len()),
        "wiederverwendete Region ist genullt (keine Datenremanenz)",
        &mut fail,
    );
    mm::free(b);

    println!("zerotest: {}", if fail { "FAILURES" } else { "ALL PASS" });
}

/// Region wortweise mit `pat` beschreiben (Testmuster).
fn fill_region(base: u64, len: u64, pat: u64) {
    let mut off = 0;
    while off + 8 <= len {
        // SAFETY: `[base, base+len)` gehört uns exklusiv (eben alloziert), liegt im
        // identity-gemappten Normal-RAM und ist 8-Byte-ausgerichtet zugreifbar.
        unsafe { core::ptr::write_volatile((base + off) as *mut u64, pat) };
        off += 8;
    }
}

/// Ist die Region vollständig genullt?
fn region_is_zero(base: u64, len: u64) -> bool {
    let mut off = 0;
    while off + 8 <= len {
        // SAFETY: wie `fill_region` — exklusiv gehaltene, gemappte Region.
        if unsafe { core::ptr::read_volatile((base + off) as *const u64) } != 0 {
            return false;
        }
        off += 8;
    }
    true
}

/// **Cap-Budget-Test** (ext-29): eine PD kann die systemweit geteilte Cap-Tabelle nicht
/// monopolisieren. `install_cap_checked` weist jede Installation über
/// [`CAP_BUDGET_PER_PD`](caprock_microkit::CAP_BUDGET_PER_PD) hinaus ab; ein Überschreiben
/// eines schon belegten Slots bleibt erlaubt (kein zusätzlicher Verbrauch).
fn budgettest() {
    let p = "budget";
    let mut fail = false;
    const BUDGET: usize = caprock_microkit::CAP_BUDGET_PER_PD;

    let free_before = mm::total_free();
    let pd = mm::create_pd().expect("budget pd");
    let root = mm::cap_install(mm::alloc(PAGE, PAGE).expect("budget mem")).expect("budget root");

    // Bis zum Budget füllen: jede Installation muss gelingen.
    let mut all_ok = true;
    for slot in 0..BUDGET {
        let c = mm::cap_copy(root, Rights::RW).expect("budget copy");
        all_ok &= mm::install_pd_cap(pd, slot, c);
    }
    check(p, all_ok, "Installationen bis zum Budget gelingen", &mut fail);

    // Ein weiterer, bisher LEERER Slot muss abgewiesen werden.
    let over = mm::cap_copy(root, Rights::RW).expect("budget over-copy");
    check(
        p,
        !mm::install_pd_cap(pd, BUDGET, over),
        "Installation ueber das Budget hinaus wird abgewiesen",
        &mut fail,
    );

    // Ein bereits belegter Slot darf weiterhin ERSETZT werden (kein Mehrverbrauch).
    let repl = mm::cap_copy(root, Rights::RW).expect("budget replace-copy");
    check(
        p,
        mm::install_pd_cap(pd, 0, repl),
        "Ersetzen eines belegten Slots bleibt erlaubt",
        &mut fail,
    );

    // Teardown: PD abbauen, Wurzel löschen -> alles zurück auf die Baseline.
    mm::destroy_pd(pd);
    let _ = mm::cap_delete(over);
    let _ = mm::cap_revoke(root);
    let _ = mm::cap_delete(root);
    check(p, mm::cap_audit_cdt() == 0, "CDT konsistent nach Teardown", &mut fail);
    check(p, mm::total_free() == free_before, "Speicher zurueckgegeben", &mut fail);

    println!("budget  : {}", if fail { "FAILURES" } else { "ALL PASS" });
}

/// Transfer-Demonstration: Cap per Move durchreichen (Ownership = Capability).
fn transfer(cap: MemoryCap) -> MemoryCap {
    cap
}

fn memtest() {
    let p = "memtest";
    let mut fail = false;
    let free0 = mm::total_free();
    let frags0 = mm::fragments();
    println!("memtest : freies RAM = {} MiB ({frags0} Fragmente)", free0 >> 20);

    let a = mm::alloc(PAGE, PAGE).expect("alloc a");
    let b = mm::alloc(4 * PAGE, 2 * PAGE).expect("alloc b");
    check(p, a.len() == PAGE, "alloc 1 Seite", &mut fail);
    check(
        p,
        b.len() == 4 * PAGE && b.base() % (2 * PAGE) == 0,
        "alloc 4 Seiten, 8-KiB-aligned",
        &mut fail,
    );
    check(p, a.base() != b.base(), "disjunkte Allokationen", &mut fail);
    check(
        p,
        mm::total_free() == free0 - 5 * PAGE,
        "freies RAM um 5 Seiten reduziert",
        &mut fail,
    );

    let bbase = b.base();
    let (b1, b2) = b.split(PAGE).expect("split b");
    check(
        p,
        b1.len() == PAGE && b2.len() == 3 * PAGE,
        "split 4 -> 1 + 3 Seiten",
        &mut fail,
    );
    check(
        p,
        b1.base() == bbase && b2.base() == bbase + PAGE,
        "split-Adressen zusammenhaengend",
        &mut fail,
    );

    let b1 = b1.restrict(Rights::READ);
    check(p, b1.rights() == Rights::READ, "restrict RW -> R", &mut fail);

    let a = transfer(a);
    check(p, a.len() == PAGE, "transfer per Move erhaelt Cap", &mut fail);

    mm::free(a);
    mm::free(b1);
    mm::free(b2);
    check(
        p,
        mm::total_free() == free0 && mm::fragments() == frags0,
        "free + Coalescing stellt RAM wieder her",
        &mut fail,
    );

    println!("memtest : {}", if fail { "FAILURES" } else { "ALL PASS" });
}

fn captest() {
    let p = "captest";
    let mut fail = false;
    let free_before = mm::total_free();

    // Wurzel-Cap aus 16 Seiten RAM.
    let mc = mm::alloc(16 * PAGE, PAGE).expect("alloc root");
    let len = mc.len();
    let after_alloc = mm::total_free();
    let root = mm::cap_install(mc).expect("install");
    check(
        p,
        mm::total_free() == after_alloc,
        "install belegt keinen zusaetzlichen Speicher",
        &mut fail,
    );
    let ri = mm::cap_inspect(root).expect("inspect root");
    let region_ok = matches!(ri.kind, caprock_cap::ObjectKind::Memory(r) if r.len == len);
    check(
        p,
        region_ok && ri.rights == Rights::RW && ri.refcount == 1,
        "Wurzel-Cap: RW, refcount 1",
        &mut fail,
    );

    // Ableitungen: copy, mint, grandchild.
    let c1 = mm::cap_copy(root, Rights::RW).expect("copy c1");
    let c2 = mm::cap_mint(root, Rights::READ, 0xBEEF).expect("mint c2");
    let _g1 = mm::cap_copy(c1, Rights::RW).expect("copy g1");

    let ri = mm::cap_inspect(root).expect("inspect");
    check(p, ri.refcount == 4, "refcount 4 nach copy/mint/copy", &mut fail);
    check(p, ri.child_count == 2, "Wurzel hat 2 Kinder", &mut fail);
    // CDT-/Refcount-Property bei voll abgeleitetem Baum (deterministisch).
    check(p, mm::cap_audit_cdt() == 0, "CDT konsistent (abgeleiteter Baum)", &mut fail);

    let c2i = mm::cap_inspect(c2).expect("inspect c2");
    check(
        p,
        c2i.rights == Rights::READ && c2i.badge == 0xBEEF,
        "mint: Rechte READ + Badge gesetzt",
        &mut fail,
    );

    // Keine Privilege-Escalation: copy von c2(READ) mit RWX bleibt READ.
    let c3 = mm::cap_copy(c2, Rights::RWX).expect("copy c3");
    check(
        p,
        mm::cap_inspect(c3).expect("inspect c3").rights == Rights::READ,
        "keine Rechte-Eskalation bei copy",
        &mut fail,
    );

    // move: altes Handle ungueltig, neues gueltig.
    let c1b = mm::cap_move(c1).expect("move c1");
    check(p, mm::cap_inspect(c1).is_none(), "altes Handle nach move ungueltig", &mut fail);
    check(p, mm::cap_inspect(c1b).is_some(), "neues Handle nach move gueltig", &mut fail);

    // delete mit Kindern -> HasChildren.
    check(
        p,
        mm::cap_delete(root) == Err(CapError::HasChildren),
        "delete mit Kindern -> HasChildren",
        &mut fail,
    );

    // revoke: alle Abkoemmlinge entfernen, Wurzel bleibt.
    mm::cap_revoke(root).expect("revoke root");
    let ri = mm::cap_inspect(root).expect("inspect after revoke");
    check(
        p,
        ri.refcount == 1 && ri.child_count == 0,
        "revoke entfernt alle Abkoemmlinge (refcount 1)",
        &mut fail,
    );
    check(
        p,
        mm::cap_inspect(c2).is_none() && mm::cap_inspect(c3).is_none(),
        "Abkoemmling-Handles ungueltig nach revoke",
        &mut fail,
    );
    check(
        p,
        mm::total_free() == after_alloc,
        "Speicher noch gehalten (Wurzel lebt)",
        &mut fail,
    );

    // CDT konsistent nach revoke (nur noch die Wurzel + ihr Objekt).
    check(p, mm::cap_audit_cdt() == 0, "CDT konsistent nach revoke", &mut fail);

    // delete der Wurzel -> Finalisierung -> Speicherrueckgabe.
    mm::cap_delete(root).expect("delete root");
    check(p, mm::cap_inspect(root).is_none(), "Wurzel-Handle nach delete ungueltig", &mut fail);
    // CDT konsistent nach vollstaendigem Teardown (leer).
    check(p, mm::cap_audit_cdt() == 0, "CDT konsistent nach Teardown", &mut fail);
    check(
        p,
        mm::total_free() == free_before,
        "Finalisierung gibt Speicher zurueck",
        &mut fail,
    );

    println!("captest : {}", if fail { "FAILURES" } else { "ALL PASS" });
}
