//! `lxpdrv` — **Loader-Dienst-PD fuer den BETRIEB im Gast** (LXPD-End-to-End).
//!
//! Laedt ein Treiber-Image von Platte ueber den Blockdienst (fremde PD, virtio-blk-Protokoll),
//! verifiziert es mit `lxpd_runtime::LadeDienst` (GPT → Verzeichnis → Eintrag → Bild → Manifest,
//! SHA-256-Bindung, Zeugen, Namensbindung — niemals laden ohne Pruefung) und stoesst die
//! Instanziierung per `SYS_LOAD_IMAGE = 36` an (`KernelAnstoss`).
//!
//! ## Slots (aus dem EIGENEN Manifest-Eintrag, s. `tools/lxpd-e2e.sh`)
//!
//! | Slot | Cap | wofuer |
//! |---|---|---|
//! | 0 | Loader (nur mit `loader`-Bit; sonst die delegierte Notification von init) | der Anstoss |
//! | 1 | Notification (Manifest `ntfn`) | der Meldekanal |
//! | 2 | Endpoint (Manifest `ep`, Dienst des Blocktreibers) | INFO/READ an den Blockdienst |
//! | 6 | Shared-Fenster (Manifest `shared`, Uebertragungsflaeche des Blockdienstes) | Transfer UND Bild-Uebergabe |
//!
//! Das Shared-Fenster traegt zwei Rollen nacheinander (nie gleichzeitig): erst die
//! Transferflaeche der Block-Reads (der Treiber legt gelesene Sektoren ab Fensterbasis ab),
//! danach — nach bestandener Pruefung — das gepruefte Bild ab Basis fuer `LOAD_IMAGE`
//! (`bild_slot = 6`: eine `Memory`-Cap mit READ, wie der Dispatch sie verlangt). Dazwischen
//! liegt die Pruefung auf PD-eigenen Puffern (`LadeDienst`), nicht im Fenster.
//!
//! ## Meldungen (lesbar an `root : Notification-Badge`, ohne Kernel-Aenderung)
//!
//! Ein `signal` traegt kein Wort — das Badge steckt in der Cap. Deshalb badgt dieser Dienst
//! je Aussage eine eigene Kopie (`ccopy`) und signalisiert sie:
//!
//! * `GEPRUEFT` (Bit 40): Platte gelesen, Bild verifiziert, `LOAD_IMAGE` wird jetzt gerufen.
//!   Steht das Bit und sonst nichts, hat der Kernel den Anstoss abgewiesen (keine Loader-Cap,
//!   kein Manifest-Eintrag, falscher Hash — der Grund steht im PD-Rueckgabewert, nicht im Log).
//! * `GELADEN` (Bit 41): `LOAD_IMAGE` meldete `OK` — die Treiber-PD existiert
//!   (`lxpdimg : [pid]... gestartet` steht dann im Kernel-Log).
//!
//! Schweigen heisst: vor der Pruefung gescheitert (kein Geraet, keine LXPD-Partition,
//! Verzeichnis/Herkunft/Hash/Zeuge/Name abgelehnt). Danach `exit()` — wie `init` kein
//! Dauer-Park: Stack und TCB gehen zurueck.

#![no_std]
#![no_main]
#![forbid(unsafe_code)]

use libcaprock::{call, exit, map_window, result, signal, Window};
use lxpd_runtime::{
    AnstossKontext, BlockFehler, BlockInfo, BlockQuelle, KernelAnstoss, LadeDienst, SEKTOR,
};

/// Slot der Loader-Cap (Manifest `loader`, wie `init`: Slot-Konvention des Glue).
const LOADER_SLOT: u64 = 0;
/// Slot der eigenen Notification (Manifest `ntfn`) — der Meldekanal.
const NTFN: u64 = 1;
/// Slot des Kanals zum Blockdienst (Manifest `ep`; die Instanz teilt der Kernel zu).
const BLK: u64 = 2;
/// Slot der geteilten Uebertragungsflaeche (Manifest `shared`).
const SHARED: u64 = 6;
/// Freie Slots fuer eigen gebadgte Kopien (Aussage je Bit, s. Modul-Doku).
const MELDE_GEPRUEFT_SLOT: u64 = 7;
const MELDE_GELADEN_SLOT: u64 = 5;

/// Block-Protokoll (Teilmenge von `virtio-blk`, s. `lxpd_runtime::protokoll`).
const OP_INFO: u64 = 0;
const OP_READ: u64 = 1;
const ST_OK: u64 = 0;
const ST_RANGE: u64 = 3;

/// Hoechstzahl Sektoren je Anfrage (muss zum Blockdienst passen).
const MAX_SEKTOREN: u32 = 8;

/// Programm-ID des Laufzeit-Treibers im Boot-Manifest (daran bindet das Kernel-Vertrauen).
const ZIEL_PID: u64 = 8;

/// Manifest-Schluessel (LXPD-Konvention) und Root-Pubkey (Eintrags-Zeuge): Endowment dieser
/// PD, nie Draht. Werte wie in den Dienst-Tests (`deadbeef`/`[0x42; 32]`) — die Platten-Dokumente
/// des E2E-Aufbaus (`tools/lxpd-e2e-platte.py`) sind mit genau ihnen gebaut und signiert.
const MANIFEST_SCHLUESSEL: &[u8] = b"deadbeef";
const PUBKEY: [u8; 32] = [0x42; 32];

/// „Platte gelesen + geprueft, Anstoss laeuft" (Bit 40 im Root-Badge).
const GEPRUEFT_BADGE: u64 = 1 << 40;
/// „LOAD_IMAGE meldete OK" (Bit 41 im Root-Badge).
const GELADEN_BADGE: u64 = 1 << 41;

/// Rechte-Maske fuer die Melde-Kopien (R+W+X; geschnitten mit dem Original).
const RWX: u64 = 7;

/// Blockquelle ueber IPC: Worte an den Blockdienst, Bytes aus dem Shared-Fenster.
/// Entspricht dem Fake der Host-Tests (`BlockQuelle`), nur dass dahinter `call` + Fenster
/// stehen statt eines Plattenabbilds.
struct IpcQuelle<'a> {
    shared: &'a Window,
    max_je_anfrage: u32,
}

impl BlockQuelle for IpcQuelle<'_> {
    fn info(&mut self) -> Result<BlockInfo, BlockFehler> {
        let r = call(BLK, [OP_INFO, 0, 0, 0]);
        if r.result != result::OK || r.msg[0] != ST_OK {
            return Err(BlockFehler::Geraet);
        }
        if r.msg[1] == 0 || r.msg[3] != SEKTOR as u64 {
            return Err(BlockFehler::Geraet);
        }
        let max = (r.msg[2] as u32).min(MAX_SEKTOREN);
        if max == 0 {
            return Err(BlockFehler::Geraet);
        }
        self.max_je_anfrage = max;
        Ok(BlockInfo { kapazitaet: r.msg[1], max_je_anfrage: max })
    }

    fn lesen(&mut self, lba: u64, sektoren: u32, puffer: &mut [u8]) -> Result<u32, BlockFehler> {
        if (sektoren as usize) * SEKTOR > puffer.len() {
            return Err(BlockFehler::Bereich);
        }
        let mut rest = sektoren;
        let mut sektor = lba;
        let mut off = 0usize;
        while rest > 0 {
            let n = rest.min(self.max_je_anfrage);
            let r = call(BLK, [OP_READ, sektor, u64::from(n), 0]);
            if r.result != result::OK {
                return Err(BlockFehler::Geraet);
            }
            if r.msg[0] == ST_RANGE {
                return Err(BlockFehler::Bereich);
            }
            if r.msg[0] != ST_OK {
                return Err(BlockFehler::Geraet);
            }
            // Der Treiber meldet die Sektorzahl in `msg[2]` (`msg[1]` sind Datums-Bytes des
            // Sektors, keine Zaehler — s. `BlkResult::first_word`). Kuerzer als angefragt ist
            // abgebrochen, nicht teilweise gut.
            if r.msg[2] != u64::from(n) {
                return Err(BlockFehler::Abgebrochen);
            }
            let nbytes = (n as usize) * SEKTOR;
            let chunk = self.shared.bytes(0, nbytes as u64).ok_or(BlockFehler::Geraet)?;
            let ziel = puffer.get_mut(off..off + nbytes).ok_or(BlockFehler::Bereich)?;
            ziel.copy_from_slice(chunk);
            off += nbytes;
            sektor += u64::from(n);
            rest -= n;
        }
        Ok(sektoren)
    }
}

libcaprock::entry!(run);

/// Eigen gebadgte Kopie melden: `ccopy` praegt, `signal` verodert das Badge ins Pending der
/// Root-Notification (dasselbe Objekt wie Slot 0 — s. Modul-Doku). Gibt an, ob gemeldet wurde.
fn melden(quell_slot: u64, ziel_slot: u64, badge: u64) -> bool {
    if libcaprock::ccopy(quell_slot, ziel_slot, RWX, badge) != result::OK {
        return false;
    }
    signal(ziel_slot, 0);
    true
}

fn run(_arg: usize) -> ! {
    let Some(shared) = map_window(SHARED) else { exit() };
    let mut quelle = IpcQuelle { shared: &shared, max_je_anfrage: MAX_SEKTOREN };
    let mut dienst = LadeDienst::neu(true);
    // `true` mit Absicht: Diese PD ist FUER ein Manifest mit `loader`-Bit gebaut (Run 1 der
    // E2E-Reihe). Fehlt das Bit im Manifest (Run 2), faellt der Anstoss client-seitig mit
    // `KeineLoaderCap` — und genau das trennt „PD duerfte nicht" von „Kernel wies ab".
    // (Der Host-Test `ohne_loader_cap_kein_syscall_kein_anstoss` belegt das Gatter; hier zaehlt,
    // was der Kernel antwortet.)
    let bild_len = match dienst.suchen(&mut quelle, 0) {
        Ok(_) => match dienst.bild_lesen(&mut quelle) {
            Ok(()) => match dienst.pruefen(MANIFEST_SCHLUESSEL, &PUBKEY) {
                Ok(summe) => summe.bild_len,
                Err(_) => exit(),
            },
            Err(_) => exit(),
        },
        Err(_) => exit(),
    };
    // Geprueft — das Bit steht BEVOR der Anstoss laeuft: Bloeckte der Kernel danach, stuende
    // trotzdem fest, dass Lesen + Pruefen durch waren.
    melden(LOADER_SLOT, MELDE_GEPRUEFT_SLOT, GEPRUEFT_BADGE);
    // Gepruefte Bytes ins Fenster ab Basis kopieren — DORT liest der Kernel (`bild_slot`).
    // Eigener Block, damit die Leihe der Vorlage vor dem Anstoss endet.
    {
        let Some(vorlage) = dienst.geprueftes_bild() else { exit() };
        if vorlage.len() != bild_len || (vorlage.len() as u64) > shared.len() {
            exit();
        }
        let mut i = 0usize;
        while i < vorlage.len() {
            if shared.write_u8(i as u64, vorlage[i]).is_none() {
                exit();
            }
            i += 1;
        }
    }
    let ctx = AnstossKontext {
        loader_slot: LOADER_SLOT,
        bild_slot: SHARED,
        programm_id: ZIEL_PID,
        deleg_liste: 0,
        deleg_anzahl: 0,
        extras: 0,
    };
    let mut anstoss = KernelAnstoss;
    match dienst.anstossen(&mut anstoss, &ctx) {
        Ok(_) => {
            melden(LOADER_SLOT, MELDE_GELADEN_SLOT, GELADEN_BADGE);
            signal(NTFN, 0);
            exit();
        }
        Err(_) => exit(),
    }
}
