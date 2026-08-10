//! `aggressor-h` — adversarialer HardwareLand-Testdienst (ext-27 T2, ADR 0012).
//!
//! Ein extern geladenes **HardwareLand-Backend**. Es haelt **nur** eine Cap auf seinen eigenen
//! Partner-Kanal (Slot 0) und beweist: trotz der „Hardware"-Domaene besitzt es **keine**
//! Management-Autoritaet und kann **nichts** ausserhalb seines Kanals erreichen — jeder Angriff
//! (Cap-Confusion + Autoritaets-Eskalation) wird vom Kernel korrekt abgewiesen. Es signalisiert
//! sein Erfolgs-Badge ueber den **eigenen Kanal** (die einzige legitime Operation) **genau dann**,
//! wenn **alle** Angriffe abgewiesen wurden ("der Dienst ist sein eigener Richter").
//!
//! Endowment (Kernel-Test): Slot 0 = Kanal-Notification, gemintet **WRITE-only**, Badge
//! `SUCCESS_BADGE`. (Die HardwareLand-Cap-Policy erlaubt einem Backend NUR Caps des eigenen Kanals.)

#![no_std]
#![no_main]

use libcaprock::{exit, invoke, result, sys};

/// Erfolgs-Badge "AGRH" (muss zum Kernel-Test `AGGRH_SUCCESS` passen).
pub const SUCCESS_BADGE: u64 = 0x4147_5248;

const CHAN: u64 = 0; // Slot 0: eigene Kanal-Notification (WRITE-only) — Melde-Kanal + WAIT-Rechte-Probe
const EMPTY: u64 = 7; // garantiert leerer Cap-Slot

#[inline]
fn expect(nr: u64, cap: u64, want: u64, ok: &mut bool) {
    if invoke(nr, cap, [0; 4], 0).result != want {
        *ok = false;
    }
}

#[no_mangle]
pub extern "C" fn _start(_arg: usize) -> ! {
    let mut ok = true;

    // A1: leerer Slot -> BADCAP. Schliesst die Autoritaets-Eskalation ein: PDCTL/LOAD/KILL ohne
    // gating-Cap -> BADCAP (ein HardwareLand-Backend hat KEINE Management-Autoritaet).
    for &nr in &[
        sys::CALL, sys::RECV, sys::REPLY, sys::SIGNAL, sys::WAIT,
        sys::KILL, sys::MAP, sys::UNMAP, sys::PDCTL, sys::LOAD,
    ] {
        expect(nr, EMPTY, result::ERR_BADCAP, &mut ok);
    }

    // A2: falscher Objekttyp ueber die Kanal-Notification (Slot 0) -> BADCAP (SIGNAL/WAIT ausgenommen,
    // typ-gueltig gegen eine Notification).
    for &nr in &[
        sys::CALL, sys::RECV, sys::REPLY, sys::KILL, sys::MAP, sys::UNMAP, sys::PDCTL, sys::LOAD,
    ] {
        expect(nr, CHAN, result::ERR_BADCAP, &mut ok);
    }

    // A3: falsche Rechte -> WAIT braucht READ; die Kanal-Cap ist WRITE-only -> ERR_RIGHTS (Rechte-
    // Check VOR dem Blockieren).
    expect(sys::WAIT, CHAN, result::ERR_RIGHTS, &mut ok);

    // A4: unbekannte Syscall-Nr ueber einen gueltigen Cap -> ERR_BADSYS.
    expect(999, CHAN, result::ERR_BADSYS, &mut ok);

    // Erfolg ueber den EIGENEN Kanal melden — NUR bei vollstaendig abgewiesener Batterie.
    if ok {
        libcaprock::signal(CHAN, SUCCESS_BADGE);
    }
    exit();
}
