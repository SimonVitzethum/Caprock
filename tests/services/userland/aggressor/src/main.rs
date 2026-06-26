//! `aggressor-u` — adversarialer UserLand-Testdienst (ext-27 T0, ADR 0012).
//!
//! Ein extern gebauter, vom Binary-Loader (ext-26) wie Drittsoftware geladener EL0-Prozess der
//! Domaene **UserLand**. Er greift den Kernel **ausschliesslich** ueber die Syscall-ABI an und
//! prueft fuer **jeden** Angriff, dass der Kernel ihn mit dem **erwarteten** Fehlercode abweist.
//! Er signalisiert sein Erfolgs-Badge [`SUCCESS_BADGE`] (Slot 0) **genau dann**, wenn **alle**
//! Angriffe korrekt abgewiesen wurden — der Dienst ist damit das **Orakel** der Kernel-Korrektheit
//! (laesst der Kernel auch nur einen Angriff durch, bleibt das Signal aus und der Kernel-Test
//! laeuft in den Timeout -> FAIL).
//!
//! Endowment (vom Kernel-Test gesetzt):
//!   * Slot 0 = Report-Notification, gemintet **WRITE-only**, Badge = `SUCCESS_BADGE`.
//!   * Slot 1 = dieselbe Notification, gemintet **READ-only**.
//! Alle uebrigen Angriffe laufen gegen einen **leeren** Slot bzw. den **falschen Objekttyp**.

#![no_std]
#![no_main]

use libsel4lake::{exit, invoke, result, sys};

/// Erfolgs-Badge "AGRU" (muss zum Kernel-Test `AGGRU_SUCCESS` passen).
pub const SUCCESS_BADGE: u64 = 0x4147_5255;

const REPORT: u64 = 0; // Slot 0: Report-Notification (WRITE-only) — Signal-Kanal + WAIT-Rechte-Probe
const RDONLY: u64 = 1; // Slot 1: dieselbe Notification (READ-only) — SIGNAL-Rechte-Probe
const EMPTY: u64 = 7; // garantiert leerer Cap-Slot

/// Einen Angriff fahren und pruefen, dass der Kernel exakt `want` zurueckgibt.
#[inline]
fn expect(nr: u64, cap: u64, msg: [u64; 4], want: u64, ok: &mut bool) {
    if invoke(nr, cap, msg, 0).result != want {
        *ok = false; // Angriff NICHT korrekt abgewiesen -> Kernel-Fehler
    }
}

#[no_mangle]
pub extern "C" fn _start(_arg: usize) -> ! {
    let mut ok = true;

    // ── A1: leerer Slot ── jeder cap-pruefende Syscall MUSS ERR_BADCAP liefern (Cap-Aufloesung
    // scheitert ganz oben im Dispatch, vor jeder Operation; KEINER blockiert).
    for &nr in &[
        sys::CALL, sys::RECV, sys::REPLY, sys::SIGNAL, sys::WAIT,
        sys::KILL, sys::MAP, sys::UNMAP, sys::PDCTL, sys::LOAD,
    ] {
        expect(nr, EMPTY, [0; 4], result::ERR_BADCAP, &mut ok);
    }

    // ── A2: falscher Objekttyp ── Slot 0 ist eine Notification. Jeder Syscall, der einen ANDEREN
    // Objekttyp erwartet (Endpoint/Tcb/Memory/PdControl/Loader), MUSS ERR_BADCAP liefern (Typ-Check
    // vor Rechte-Check). SIGNAL/WAIT sind hier ausgenommen (gegen eine Notification typ-gueltig).
    for &nr in &[
        sys::CALL, sys::RECV, sys::REPLY, sys::KILL, sys::MAP, sys::UNMAP, sys::PDCTL, sys::LOAD,
    ] {
        expect(nr, REPORT, [0; 4], result::ERR_BADCAP, &mut ok);
    }

    // ── A3: falsche Rechte ──
    // SIGNAL braucht WRITE; Slot 1 ist READ-only -> ERR_RIGHTS.
    expect(sys::SIGNAL, RDONLY, [0; 4], result::ERR_RIGHTS, &mut ok);
    // WAIT braucht READ; Slot 0 ist WRITE-only -> ERR_RIGHTS (Rechte-Check VOR dem Blockieren ->
    // kehrt zurueck, blockiert NICHT).
    expect(sys::WAIT, REPORT, [0; 4], result::ERR_RIGHTS, &mut ok);

    // ── B: Autoritaets-Eskalation ── ein UserLand-Prozess haelt KEINE PdControl/Loader/Tcb-Cap.
    // PDCTL/LOAD/KILL ueber Slot 0 (Notification) oder den leeren Slot fallen bereits unter A1/A2
    // (BADCAP) — die Eskalation ist damit nachweislich gescheitert (Trust/Privileg folgt nur aus
    // tatsaechlich gehaltenen Caps, nicht aus der Domaene).

    // ── A4: unbekannte Syscall-Nummer ── ueber einen GUELTIGEN Cap-Slot (sonst gewinnt die
    // Cap-Aufloesung mit BADCAP); die Dispatch-Tabelle MUSS ERR_BADSYS liefern.
    expect(999, REPORT, [0; 4], result::ERR_BADSYS, &mut ok);

    // Erfolg melden — NUR wenn jeder einzelne Angriff korrekt abgewiesen wurde. signal() nutzt den
    // Cap-Badge (SUCCESS_BADGE), das x2-Argument ist irrelevant.
    if ok {
        libsel4lake::signal(REPORT, SUCCESS_BADGE);
    }
    exit();
}
