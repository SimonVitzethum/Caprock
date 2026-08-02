//! **virtio-pci: das Auffinden der Strukturen** — die Treiberlogik liegt in `sel4lake-virtio`.
//!
//! Was hier bleibt, ist **Bus-Enumeration**: welches Geraet gibt es, und auf welcher
//! Konfigurationsraum-Seite liegt es. Das gehoert in den Kern, weil ein Lauf ueber alle Busse
//! jedes Geraet der Maschine sieht.
//!
//! Seit A-5.1 ist der **Capability-Lauf** selbst nicht mehr hier: er liegt in
//! `sel4lake_virtio::probe_ecam` und arbeitet auf **einer** Seite. Der alte Einwand
//! ("Konfigurationsraum ist geraeteweit") gilt fuer das ECAM-Fenster als Ganzes, nicht fuer eine
//! einzelne Funktion — ECAM bildet jede Funktion auf genau 4 KiB ab. Diese Funktion hier ruft
//! also dieselbe Routine auf wie die Treiber-PD, nur mit einer Seite, die sie selbst gefunden hat.
//!
//! Was gegangen ist, ist das **Protokoll** (Handshake, Virtqueue, used-Ring): es haengt an nichts
//! Privilegiertem und gehoert in eine Userland-Treiber-PD (todo A-5.1). Gesetzte Regel (Simon,
//! 2026-08-01): keine Treiber im Mikrokern. Der Schnitt liegt genau dort, wo die Autoritaet
//! aufhoert -- nicht dort, wo es beim Aufteilen bequem war.

use crate::cpu;
use crate::pcie::{self, PciDevice};
pub use sel4lake_virtio::{
    blk, blk::VirtioBlk, net, net::VirtioNet, Transport, VirtioRng, DATA_LEN_BYTES, DATA_OFFSET,
};

/// Die virtio-Capabilities des Geraets parsen und `common_cfg`, Notify und (falls vorhanden) den
/// geraetespezifischen Konfigurationsraum lokalisieren. `None`, wenn die noetigen Caps fehlen oder
/// ihr BAR nicht zugewiesen ist.
///
/// Das Ergebnis traegt **keine Autoritaet**: wer es hat, kann genau dieses Geraet bedienen und
/// nichts finden, was er nicht schon hatte. Genau deshalb darf es einer Treiber-PD gereicht werden
/// -- der Weg, der es erzeugt hat (ein Lauf ueber alle Busse), aber nicht.
pub fn probe_transport(dev: &PciDevice) -> Option<Transport> {
    let page = pcie::cfg_page(dev);
    if page == 0 {
        return None;
    }
    // **Derselbe** Lauf, den auch die Treiber-PD fährt (`sel4lake_virtio::probe_ecam`) — nicht
    // eine zweite Fassung davon. Zwei Fassungen desselben Capability-Laufs hätten genau die
    // Eigenschaft, die dieses Projekt schon einmal bezahlt hat: die eine wird gepflegt, die
    // andere bleibt still zurück, und welche von beiden im Ernstfall lief, weiß hinterher
    // niemand.
    //
    // Was hier bleibt, ist das **Auffinden der Seite** — Enumeration, also Kernaufgabe.
    //
    // Die Barriere wird HEREINGEREICHT (s. Crate-Doku von `sel4lake-virtio`): auf aarch64 ist
    // `dsb sy` noetig, weil Device-Memory nicht in der inner-shareable Domaene liegt. Eine
    // "arch-neutrale" Barriere haette die Semantik still abgeschwaecht.
    //
    // SAFETY: `page` ist die Konfigurationsraum-Seite genau dieser Funktion; das ECAM-Fenster ist
    // global EL1-/Kernel-Device-gemappt, die BARs ebenso.
    unsafe { sel4lake_virtio::probe_ecam(page, cpu::dsb_sy) }
}

/// Wie [`probe_transport`], aber gleich als RNG-Treiber (der bisherige Aufrufweg).
pub fn probe(dev: &PciDevice) -> Option<VirtioRng> {
    probe_transport(dev).map(VirtioRng::from_transport)
}

/// Wie [`probe_transport`], aber gleich als Blockgeraet-Treiber (A-5.2).
pub fn probe_blk(dev: &PciDevice) -> Option<VirtioBlk> {
    probe_transport(dev).map(VirtioBlk::from_transport)
}

/// Wie [`probe_transport`], aber gleich als Netzkarten-Treiber (A-5.2).
pub fn probe_net(dev: &PciDevice) -> Option<VirtioNet> {
    probe_transport(dev).map(VirtioNet::from_transport)
}
