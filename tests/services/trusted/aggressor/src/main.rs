//! `aggressor-t` — adversarialer TrustedSAS-Testdienst (ext-27 T3, ADR 0012).
//!
//! Beweist **„Trust ≠ Privileg"**. Ein als **TrustedSAS** geladener Dienst laeuft (wie alle
//! geladenen Prozesse) **EL0-isoliert** und besitzt zwar die **hoechste** Cap-Autoritaet seiner
//! Domaene (er DUERFTE PdControl-/Loader-Caps halten), doch seine Operationsmacht ergibt sich
//! **ausschliesslich** aus den Caps, die er **tatsaechlich** haelt — **nicht** aus der Domaene.
//! Da der Kernel-Test ihm **keine** Management-Cap endowt, scheitern **alle** Eskalations-Versuche
//! (PDCTL/LOAD/KILL → `ERR_BADCAP`). Er signalisiert sein Erfolgs-Badge (Slot 0) **genau dann**,
//! wenn der Kernel **jeden** Angriff korrekt abgewiesen hat.
//!
//! Endowment (Kernel-Test): Slot 0 = Report-Notification (WRITE-only, Badge `SUCCESS_BADGE`),
//! Slot 1 = dieselbe Notification (READ-only) fuer die SIGNAL-Rechte-Probe.

#![no_std]
#![no_main]
// ext-28: vollstaendig unsafe-frei -> als TrustedSAS **zertifizierbar** (Signatur-Gate, ADR 0014).
// Der Entry-Point `_start` (mit dem unsafe-Attribut `#[no_mangle]`) kommt aus der auditierten
// SDK-Schicht libsel4lake (Allowlist) via `entry!` — dieser Dienst selbst bleibt forbid-rein.
#![forbid(unsafe_code)]

use libsel4lake::{exit, invoke, result, sys};

/// Erfolgs-Badge "AGRT" (muss zum Kernel-Test `AGGRT_SUCCESS` passen).
pub const SUCCESS_BADGE: u64 = 0x4147_5254;

const REPORT: u64 = 0; // Slot 0: Report-Notification (WRITE-only) — Melde-Kanal + WAIT-Rechte-Probe
const RDONLY: u64 = 1; // Slot 1: dieselbe Notification (READ-only) — SIGNAL-Rechte-Probe
const EMPTY: u64 = 7; // garantiert leerer Cap-Slot

#[inline]
fn expect(nr: u64, cap: u64, want: u64, ok: &mut bool) {
    if invoke(nr, cap, [0; 4], 0).result != want {
        *ok = false;
    }
}

libsel4lake::entry!(run);

fn run(_arg: usize) -> ! {
    let mut ok = true;

    // A1: leerer Slot -> BADCAP. KERNPUNKT (Trust != Privileg): PDCTL/LOAD/KILL scheitern als
    // BADCAP, OBWOHL der Dienst die TrustedSAS-Domaene hat — er haelt schlicht KEINE PdControl-/
    // Loader-/Tcb-Cap. Domaenen-Trust allein gewaehrt KEINE Operationsmacht.
    for &nr in &[
        sys::CALL, sys::RECV, sys::REPLY, sys::SIGNAL, sys::WAIT,
        sys::KILL, sys::MAP, sys::UNMAP, sys::PDCTL, sys::LOAD,
    ] {
        expect(nr, EMPTY, result::ERR_BADCAP, &mut ok);
    }

    // A2: falscher Objekttyp ueber die Report-Notification (Slot 0) -> BADCAP (SIGNAL/WAIT ausgenommen).
    for &nr in &[
        sys::CALL, sys::RECV, sys::REPLY, sys::KILL, sys::MAP, sys::UNMAP, sys::PDCTL, sys::LOAD,
    ] {
        expect(nr, REPORT, result::ERR_BADCAP, &mut ok);
    }

    // A3: falsche Rechte -> SIGNAL auf die READ-only Cap (Slot 1) -> RIGHTS; WAIT auf die WRITE-only
    // Cap (Slot 0) -> RIGHTS (Rechte-Check VOR dem Blockieren).
    expect(sys::SIGNAL, RDONLY, result::ERR_RIGHTS, &mut ok);
    expect(sys::WAIT, REPORT, result::ERR_RIGHTS, &mut ok);

    // A4: unbekannte Syscall-Nr ueber einen gueltigen Cap -> BADSYS.
    expect(999, REPORT, result::ERR_BADSYS, &mut ok);

    // Erfolg melden — NUR bei vollstaendig abgewiesener Batterie.
    if ok {
        libsel4lake::signal(REPORT, SUCCESS_BADGE);
    }
    exit();
}
