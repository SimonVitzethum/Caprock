//! `virtio-net` — **der zweite Treiber, und das ist der ganze Zweck** (A-5.4).
//!
//! ## Warum es dieses Programm gibt
//!
//! A-5.3 belegt, dass *eine* Treiber-PD das *benannte* Gerät bekommt. Nicht belegt war, dass zwei
//! zugeteilte Geräte **voneinander** getrennt sind — denn es lief immer nur eine Treiber-PD, und
//! damit kam der Fall, der die Aussage widerlegen könnte, gar nicht vor. Das ist dieselbe Form wie
//! `virtio-rng` vor A-5.2: eine Aussage sieht wahr aus, weil der Gegenbeweis nie läuft.
//!
//! Dieses Programm bringt den fehlenden Fall. Es ist bewusst klein — es fährt kein Netzwerk, es
//! belegt eine Trennung.
//!
//! ## Die zwei Anfragen, und warum es beide braucht
//!
//! | Anfrage | was sie zeigt |
//! |---|---|
//! | [`OP_SELF`] | **Positivkontrolle**: eine echte ARP-Transaktion in der EIGENEN Region. Ohne sie hieße „der Fremdzugriff kam nicht an" nur, dass überhaupt nichts lief. |
//! | [`OP_FOREIGN`] | Dieselbe Transaktion, aber der Empfangspuffer liegt in der DMA-Region des **anderen** Treibers. Genau eine Adresse wandert. |
//!
//! Bei `OP_FOREIGN` bleiben Virtqueues und Sendepuffer in der eigenen Region — das Gerät muss
//! **laufen** können. Würden auch die Ringe verschoben, fände es die Deskriptoren nicht, der
//! Versuch scheiterte an der falschen Stelle, und die Messung wäre wertlos.
//!
//! Dass dieses Programm die fremde IOVA überhaupt genannt bekommt, ist kein Loch, sondern der
//! Kern der Aussage: eine Adresse zu **kennen** hilft nicht, wenn der Übersetzungskontext des
//! Geräts sie nicht auflöst. Ein Angreifer, der die Adresse nicht kennt, würde nur beweisen, dass
//! Raten schwer ist.
//!
//! ## Was dieses Programm hält
//!
//! | Slot | Cap | wofür |
//! |---|---|---|
//! | 1 | Kanal-Notification (Manifest `ntfn`) | „ich bin bereit" |
//! | 2 | Kanal-Endpoint (Manifest `ep`) | die Dienstschnittstelle |
//! | 3 | MMIO: die eigene Konfigurationsraum-**Seite** | das eigene Gerät auflösen |
//! | 4 | MMIO: das Registerfenster (BAR) | das Gerät bedienen |
//! | 5 | DMA-Region, an die eigene RID angehängt | Virtqueues + Puffer |
//!
//! Es hält **keine** Cap auf die Region des anderen Treibers — es bekommt nur deren Zahl.

#![no_std]
#![no_main]

use libcaprock::{exit, map_window, recv, reply, result, signal};
use caprock_virtio::net::VirtioNet;

const NTFN: u64 = 1;
const EP: u64 = 2;
const CFG: u64 = 3;
const BAR: u64 = 4;
const DMA: u64 = 5;

/// Positivkontrolle: ARP mit dem Empfangspuffer in der **eigenen** Region.
pub const OP_SELF: u64 = 1;
/// Der Versuch: derselbe Ablauf, Empfangspuffer unter der in `msg[1]` genannten **fremden** IOVA.
pub const OP_FOREIGN: u64 = 2;

/// Wie lange auf das Gerät gewartet wird. Großzügig — ein zu knappes Limit machte aus „hat nicht
/// geantwortet" ein „war noch nicht fertig", und die beiden sehen im Ergebnis gleich aus.
const MAX_POLL: u64 = 20_000_000;

/// Absender- und Zieladresse der ARP-Anfrage. `10.0.2.2` ist das Gateway von QEMUs
/// User-Mode-Netz; es antwortet, ohne dass ein echtes Netz dahinterstehen muss.
const SRC_IP: [u8; 4] = [10, 0, 2, 15];
const DST_IP: [u8; 4] = [10, 0, 2, 2];

/// Speicherbarriere für Gerätezugriffe.
///
/// **Keine arch-neutrale Barriere.** `core::sync::atomic::fence(SeqCst)` wird auf aarch64 zu
/// `dmb ish` — Device-Memory liegt nicht in dieser Domäne. Der bequeme Weg schwächte die Semantik
/// still ab; das Projekt hat diese Falle beim Entkoppeln von `caprock-virtio` schon einmal
/// gesehen.
#[cfg(target_arch = "x86_64")]
fn device_fence() {
    // SAFETY: `mfence` ist eine reine Ordnungsanweisung ohne Operanden und ohne Speicherzugriff.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) }
}

#[cfg(target_arch = "aarch64")]
fn device_fence() {
    // SAFETY: wie oben.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) }
}

libcaprock::entry!(run);

fn run(_arg: usize) -> ! {
    // 1. Fenster mappen. Jeder Fehlschlag endet **ohne** Bereit-Meldung: ein Treiber, der ohne
    //    Gerät in `recv` ginge, nähme Anfragen an, die er nicht beantworten kann.
    let Some(cfg_win) = map_window(CFG) else { exit() };
    let cfg = cfg_win.base();
    if map_window(BAR).is_none() {
        exit();
    }
    let Some(dma) = map_window(DMA) else { exit() };
    let (dma_cpu, dma_len, dma_dev) = (dma.base(), dma.len(), dma.iova());
    // Die Gerätesicht MUSS eine eigene Achse sein — eine identity-Abbildung soll es nicht geben.
    if dma_dev == 0 || dma_len < caprock_virtio::net::REGION_BYTES {
        exit();
    }

    // 2. Das eigene Gerät auflösen — auf der eigenen Seite, ohne den Kernel.
    // SAFETY: `cfg` ist die gemappte Konfigurationsraum-Seite genau dieser Funktion; das darin
    // genannte BAR ist über Slot 4 in dieser VSpace erreichbar.
    let Some(transport) = (unsafe { caprock_virtio::probe_ecam(cfg, device_fence) }) else {
        exit();
    };
    let net = VirtioNet::from_transport(transport);

    // 3. „Ich bin bereit." Erst **nach** dem Auflösen.
    signal(NTFN, 0);

    // 4. Dienstschleife.
    loop {
        let m = recv(EP);
        if m.result != result::OK {
            exit(); // Endpoint stillgelegt/entzogen -> geordnet enden
        }
        let op = m.msg[0];
        // Bei `OP_SELF` die eigene Pufferadresse, bei `OP_FOREIGN` die genannte fremde.
        let rx_dev = match op {
            OP_SELF => dma_dev + caprock_virtio::net::OFF_RXBUF,
            OP_FOREIGN => m.msg[1],
            _ => {
                reply(EP, [u64::MAX, 0, 0, 0]);
                continue;
            }
        };
        // SAFETY: der Transport gehört zu diesem Gerät, `[dma_cpu, dma_cpu + REGION_BYTES)` gehört
        // dieser PD allein. `rx_dev` darf fremd sein — genau das ist die Messung (s. Moduldoku).
        let r = unsafe {
            net.arp_probe_rx_at(dma_cpu, dma_dev, rx_dev, SRC_IP, DST_IP, MAX_POLL)
        };
        // Einzeln zurückgeben, nicht als Sammel-`bool`: „gesendet, aber nichts empfangen" und „gar
        // nicht erst gesendet" sind verschiedene Befunde, und nur der erste ist die Aussage, um
        // die es hier geht.
        reply(
            EP,
            [
                u64::from(r.features_ok),
                u64::from(r.tx_used),
                u64::from(r.rx_used),
                u64::from(r.arp_reply),
            ],
        );
    }
}
