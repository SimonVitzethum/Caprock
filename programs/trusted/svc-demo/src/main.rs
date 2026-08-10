//! `svc-demo` — signiertes TrustedSAS-Demoprogramm (ext-28, ADR 0014).
//!
//! Vollstaendig **ohne `unsafe`** (`#![forbid(unsafe_code)]`): das eigentliche Programm enthaelt
//! keinerlei `unsafe`; die einzige zugelassene Ausnahme im gesamten Dependency-Baum ist die explizit
//! auditierte Syscall-ABI-Schicht `libcaprock` (Allowlist). Genau das bezeugt das Zertifikat. Der
//! Kernel laedt dieses Binary nur, wenn ein gueltiges, auf genau dieses Binary gebundenes Zertifikat
//! vorliegt. Verhalten: die endowte Notification (Slot 0) signalisieren, dann beenden.

#![no_std]
#![no_main]
#![forbid(unsafe_code)]

/// Charakteristisches Badge ("TSVC"), das der Kernel-Test erwartet.
pub const SVC_BADGE: u64 = 0x5453_5643;

const NTFN_SLOT: u64 = 0;

// Der Entry-Point `_start` (mit dem unsafe-Attribut `#[no_mangle]`) wird von der auditierten
// SDK-Schicht libcaprock (Allowlist) erzeugt -- dieses Programm bleibt vollstaendig forbid-sauber.
libcaprock::entry!(run);

/// Programmlogik: die vom Loader endowte Notification (Slot 0) signalisieren, dann beenden.
fn run(_arg: usize) -> ! {
    libcaprock::signal(NTFN_SLOT, SVC_BADGE);
    libcaprock::exit();
}
