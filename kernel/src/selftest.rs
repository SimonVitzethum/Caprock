//! Boot-Selbsttests des Speicher- und Capability-Subsystems.
//!
//! Phase 2 (`memtest`): Allokator (alloc/split/restrict/transfer/free+Coalescing).
//! Phase 3 (`captest`): Capability-Space (install/copy/mint/move/delete/revoke,
//! CDT, Refcount-Finalisierung, Generations-Handles, keine Rechte-Eskalation).

use crate::system as mm;
use sel4lake_cap::CapError;
use sel4lake_hal::println;
use sel4lake_mem::{MemoryCap, Rights, PAGE};

fn check(prefix: &str, cond: bool, name: &str, fail: &mut bool) {
    if cond {
        println!("{prefix}: PASS  {name}");
    } else {
        println!("{prefix}: FAIL  {name}");
        *fail = true;
    }
}

/// Beide Selbsttests ausführen (Primärkern, vor SMP-Start).
pub fn run() {
    memtest();
    captest();
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
    let region_ok = matches!(ri.kind, sel4lake_cap::ObjectKind::Memory(r) if r.len == len);
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
