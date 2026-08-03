//! **virtio-net**: eine Netzkarte mit zwei Warteschlangen (A-5.2).
//!
//! ## Was hier zum ersten Mal geprueft wird
//!
//! `rng` hatte eine Queue, `blk` eine Queue mit einer Kette. `net` hat **zwei** Queues mit
//! entgegengesetzter Richtung — Queue 0 empfaengt, Queue 1 sendet — und das ist keine
//! Verdopplung, sondern ein eigener Fehlerfall: `queue_notify_off` ist **je Queue** verschieden,
//! und ein Treiber, der die Notify-Adresse der ersten Queue fuer beide benutzt, weckt das Geraet
//! auf der falschen Seite. Das faellt bei einem Einqueue-Geraet strukturell nicht auf.
//!
//! ## Wie ein Empfang belegt wird, ohne einen zweiten Rechner
//!
//! Gesendet wird eine **ARP-Anfrage**, und geprueft wird, ob die **ARP-Antwort** ankommt. Das ist
//! bewusst gewaehlt: ARP ist zustandslos, braucht keinen Handshake und keine Zeitgeber, und die
//! Antwort traegt einen Inhalt, den man **nachrechnen** kann (Opcode 2, Absender-IP == die
//! angefragte). Ein Empfangspuffer, der sich bloss fuellt, waere kein Beleg — er koennte
//! Restspeicher sein. Ein Puffer, in dem die Antwort auf die eigene Frage steht, ist einer.
//!
//! Das Gegenstueck stellt die Testumgebung (bei QEMU `-netdev user`: der eingebaute
//! Gateway antwortet auf ARP fuer seine eigene Adresse). Welche Adressen das sind, weiss diese
//! Crate **nicht** — sie werden hereingereicht. Ein Treiber, der die Adressen seiner Testumgebung
//! kennt, ist kein Treiber mehr.

use crate::{Transport, F_ACCESS_PLATFORM, F_VERSION_1, VIRTQ_DESC_F_WRITE};

/// `VIRTIO_NET_F_MAC` — das Geraet meldet seine MAC im Konfigurationsraum.
///
/// Ohne dieses Bit **darf** der Konfigurationsraum keine gueltige MAC enthalten; der Treiber
/// muesste sich dann selbst eine ausdenken. Es wird deshalb verlangt und nicht bloss gehofft.
const F_MAC: u64 = 1 << 5;

/// Groesse des virtio-net-Kopfes. Unter `VIRTIO_F_VERSION_1` **immer** 12 Byte
/// (`virtio_net_hdr_mrg_rxbuf`), auch ohne ausgehandeltes `VIRTIO_NET_F_MRG_RXBUF` — das
/// `num_buffers`-Feld ist dann vorhanden und traegt 1. Die 10-Byte-Fassung gehoert zum
/// Legacy-Layout, das dieser Treiber nicht spricht. Wer hier 10 einsetzt, verschiebt jeden
/// empfangenen Rahmen um zwei Byte und findet den Ethertype an der falschen Stelle.
pub const HDR_LEN: u64 = 12;

// Layout in der DMA-Region (Offsets ab Regionsbasis).
/// Empfangs-Virtqueue (Queue 0).
pub const OFF_RXQ: u64 = 0x0000;
/// Sende-Virtqueue (Queue 1).
pub const OFF_TXQ: u64 = 0x0400;
/// Empfangspuffer.
pub const OFF_RXBUF: u64 = 0x0800;
/// Sendepuffer (Kopf + Rahmen).
pub const OFF_TXBUF: u64 = 0x1000;
/// Groesse des Empfangspuffers. Mit einem Puffer unterhalb der MTU wuerde das Geraet den Rahmen
/// verwerfen, statt ihn abzulegen — ein Fehlschlag, der wie "nichts empfangen" aussieht.
pub const RXBUF_LEN: u32 = 2048;
/// Mindestgroesse der DMA-Region.
pub const REGION_BYTES: u64 = 0x2000;

/// Laenge des gesendeten Rahmens. 42 Byte ARP, auf die Ethernet-Mindestlaenge von 60 Byte
/// aufgefuellt (ohne FCS) — kuerzere Rahmen duerfen unterwegs verworfen werden.
const FRAME_LEN: u32 = 60;

const ETHERTYPE_ARP: u16 = 0x0806;
const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;

/// Ergebnis des ARP-Austauschs. Wieder einzeln pruefbar statt als Sammel-`bool`: "gesendet, aber
/// nichts empfangen" und "gar nicht erst gesendet" sind verschiedene Befunde, und der zweite
/// zeigt auf den Treiber, der erste auf das Gegenueber.
#[derive(Clone, Copy, Default)]
pub struct NetResult {
    /// Caps gefunden, `VIRTIO_F_ACCESS_PLATFORM` + `VIRTIO_NET_F_MAC` angeboten und angenommen.
    pub features_ok: bool,
    /// MAC laut geraetespezifischem Konfigurationsraum.
    pub mac: [u8; 6],
    /// Hat das Geraet den Sendepuffer abgeholt? Das ist der Beleg, dass es unseren Speicher
    /// **liest** — die Richtung, die `rng` nicht zeigt.
    pub tx_used: bool,
    /// Hat das Geraet einen Rahmen abgelegt?
    pub rx_used: bool,
    /// Laenge des empfangenen Rahmens **einschliesslich** virtio-Kopf.
    pub rx_len: u32,
    /// Ist der empfangene Rahmen die ARP-Antwort auf unsere Anfrage?
    pub arp_reply: bool,
    /// Absender-IP der Antwort — muss die angefragte Adresse sein.
    pub sender_ip: [u8; 4],
}

/// **virtio-net**: Netzkarte.
pub struct VirtioNet {
    t: Transport,
}

impl VirtioNet {
    /// Aus einem bereits aufgeloesten Transport bauen.
    pub const fn from_transport(t: Transport) -> Self {
        Self { t }
    }

    /// Handshake, **eine ARP-Anfrage senden und die Antwort empfangen**.
    ///
    /// `src_ip`/`target_ip` kommen vom Aufrufer (s. Modul-Doku). `cpu_base`/`dev_base` sind die
    /// beiden Sichten der DMA-Region, getrennt aus demselben Grund wie ueberall sonst.
    ///
    /// # Safety
    /// Die MMIO-Adressen des Transports muessen zum Geraet gehoeren; `[cpu_base, cpu_base +
    /// REGION_BYTES)` muss beschreibbarer Speicher sein, der dem Aufrufer allein gehoert.
    pub unsafe fn arp_probe(
        &self,
        cpu_base: u64,
        dev_base: u64,
        src_ip: [u8; 4],
        target_ip: [u8; 4],
        max_poll: u64,
    ) -> NetResult {
        // SAFETY: die Zusagen des Aufrufers gelten unveraendert; der Empfangspuffer liegt in der
        // eigenen Region, also im Normalfall.
        unsafe { self.arp_probe_rx_at(cpu_base, dev_base, dev_base + OFF_RXBUF, src_ip, target_ip, max_poll) }
    }

    /// Wie [`Self::arp_probe`], aber der **Empfangspuffer** wird dem Geraet unter `rx_buf_dev`
    /// genannt statt unter der eigenen Regionsadresse (A-5.4).
    ///
    /// ## Wozu dieser Knopf da ist
    ///
    /// Er dient **einem** Zweck: nachzuweisen, dass ein Geraet nicht in die DMA-Region eines
    /// ANDEREN Treibers schreiben kann. Genau **eine** Adresse wandert — Virtqueues und
    /// Sendepuffer bleiben in der eigenen Region. Das ist der Punkt: das Geraet muss dabei
    /// **laufen** koennen. Wuerden auch die Ringe verschoben, faende es nicht einmal die
    /// Deskriptoren, der Versuch scheiterte an der falschen Stelle, und „nichts kam an" bewiese
    /// nur, dass nichts lief.
    ///
    /// Dass eine fremde Adresse hier ueberhaupt eintragbar ist, ist kein Loch: eine IOVA zu
    /// **kennen** hilft nicht, wenn der Uebersetzungskontext des Geraets sie nicht aufloest. Genau
    /// diese Aussage soll gemessen werden — und sie ist nur messbar, wenn der Versuch stattfindet.
    ///
    /// # Safety
    /// Wie [`Self::arp_probe`]. `rx_buf_dev` darf eine **fremde** Gerätesicht sein — der Aufrufer
    /// sagt damit zu, dass er genau das prüfen will, nicht dass die Adresse ihm gehört.
    pub unsafe fn arp_probe_rx_at(
        &self,
        cpu_base: u64,
        dev_base: u64,
        rx_buf_dev: u64,
        src_ip: [u8; 4],
        target_ip: [u8; 4],
        max_poll: u64,
    ) -> NetResult {
        let mut r = NetResult::default();

        self.t.reset();
        let offered = self.t.offered();
        if offered & F_ACCESS_PLATFORM == 0 || offered & F_MAC == 0 {
            return r;
        }
        // Nur die drei Bits, die gebraucht werden. Insbesondere KEIN MRG_RXBUF, kein CTRL_VQ und
        // keine Offloads: jedes ausgehandelte Bit ist eine Zusage, die der Treiber einhalten
        // muesste, und ein Empfangspfad, der Pruefsummen-Offload zusagt und dann nicht auswertet,
        // liest Rahmen falsch statt gar nicht.
        if !self.t.negotiate(F_VERSION_1 | F_ACCESS_PLATFORM | F_MAC) {
            return r;
        }
        // `VIRTIO_NET_F_MAC` sagt zu, dass eine MAC im Konfigurationsraum STEHT -- nicht, dass wir
        // ihn gefunden haben. Fehlt die Capability, liefert `cfg8` Nullen, und der Rahmen ginge
        // mit der Absenderadresse 00:00:00:00:00:00 hinaus. Die Antwort bliebe aus, und der
        // Befund zeigte auf das Gegenueber statt auf die Enumeration.
        if !self.t.has_device_cfg() {
            return r;
        }
        r.features_ok = true;
        for i in 0..6 {
            r.mac[i] = self.t.cfg8(i as u64);
        }

        let Some((rxq, rx_notify)) =
            self.t.queue_setup(0, cpu_base + OFF_RXQ, dev_base + OFF_RXQ, 8)
        else {
            return r;
        };
        let Some((txq, tx_notify)) =
            self.t.queue_setup(1, cpu_base + OFF_TXQ, dev_base + OFF_TXQ, 8)
        else {
            return r;
        };
        self.t.driver_ok();

        // **Die beiden Puffer werden EINMAL herausgeschnitten** (todo E, Descriptor-Typestate).
        // Monoton: Empfangspuffer (0x800, 2048 Byte) direkt gefolgt vom Sendepuffer (0x1000).
        let mut region = crate::Region::from_raw(cpu_base, dev_base, REGION_BYTES);
        let (Some(mut rxbuf), Some(mut txbuf)) = (
            region.carve(OFF_RXBUF, RXBUF_LEN),
            region.carve(OFF_TXBUF, HDR_LEN as u32 + FRAME_LEN),
        ) else {
            return r;
        };

        // Empfangspuffer **zuerst** einhaengen, dann senden. Umgekehrt haette das Geraet die
        // Antwort schon in der Hand, bevor irgendwo Platz dafuer ist — und wuerfe sie weg. Der
        // Fehler saehe aus wie "das Gegenueber antwortet nicht".
        let rx_used0 = rxq.used_idx();
        // **Den Empfangspuffer wirklich leeren, nicht nur sein erstes Wort** (A-5.4).
        //
        // Vorher wurden hier 8 Byte genullt. Das reichte, solange nur EINE Probe je Lauf lief:
        // ein frischer Puffer ist ohnehin leer. Bei zwei Proben hintereinander -- und genau das
        // tut der Kreuz-DMA-Nachweis -- liest die zweite die Antwort der ERSTEN: `ethertype`
        // steht bei +12, `oper` bei +20, die Absender-IP bei +28, und nichts davon lag in den
        // acht genullten Bytes. Gemessen: der Fremdversuch meldete `arp_reply = true`, obwohl
        // VT-d die Schreibung nachweislich blockiert hatte (ein Fault gezaehlt).
        //
        // Dieselbe Fehlerform, die dieses Projekt schon zweimal bezahlt hat: ein Puffer mit den
        // richtigen Bytes darin ist von einem beschriebenen Puffer nicht zu unterscheiden,
        // solange niemand vorher aufraeumt.
        rxbuf.zero(0, HDR_LEN + FRAME_LEN as u64);
        // Nur die **Geraetesicht** wandert (A-5.4): der Treiber liest weiter seinen eigenen
        // Puffer, dem Geraet wird eine fremde IOVA genannt. Genau eine Achse, und sie ist im Typ
        // als solche benannt — nicht ein zweites `u64` neben dem ersten.
        rxbuf.retarget_device_view(rx_buf_dev);
        let armed_rx = rxq.arm(0, rxbuf, VIRTQ_DESC_F_WRITE, 0);
        rxq.publish(0, self.t.fence);
        self.t.kick(rx_notify, 0);

        // Sendepuffer: virtio-Kopf (12 Byte Null: kein Offload, kein GSO) + ARP-Anfrage.
        txbuf.zero(0, HDR_LEN);
        self.build_arp_request(&mut txbuf, HDR_LEN, &r.mac, src_ip, target_ip);
        self.t.fence();

        let tx_used0 = txq.used_idx();
        let armed_tx = txq.arm(0, txbuf, 0, 0);
        txq.publish(0, self.t.fence);
        self.t.kick(tx_notify, 1);

        match txq.poll_used(tx_used0, max_poll) {
            Some(done) => {
                r.tx_used = true;
                let _ = txq.reclaim(armed_tx, &done);
            }
            // Kein Beleg: das Geraet koennte den Sendepuffer noch lesen. Er wird hier auch nicht
            // gelesen — aber er wird benannt zurueckgeholt statt stumm liegengelassen.
            None => {
                let _ = txq.reclaim_unproven(armed_tx);
            }
        }
        match rxq.poll_used(rx_used0, max_poll) {
            Some(done) => {
                r.rx_used = true;
                r.rx_len = done.len();
                // **Erst der Beleg, dann der Zugriff.** Vorher stand hier ein Lesen des
                // Empfangspuffers, waehrend er formal noch in der Queue hing; dass es gutging, lag
                // an der Reihenfolge im Kopf des Autors, nicht an einer Regel.
                let rxbuf = rxq.reclaim(armed_rx, &done);
                self.t.fence();
                let f = HDR_LEN;
                let ethertype = u16::from_be_bytes([rxbuf.rd8(f + 12), rxbuf.rd8(f + 13)]);
                let oper = u16::from_be_bytes([rxbuf.rd8(f + 20), rxbuf.rd8(f + 21)]);
                for i in 0..4 {
                    r.sender_ip[i] = rxbuf.rd8(f + 28 + i as u64);
                }
                r.arp_reply = done.len() as u64 >= HDR_LEN + 42
                    && ethertype == ETHERTYPE_ARP
                    && oper == ARP_REPLY
                    && r.sender_ip == target_ip;
            }
            None => {
                let _ = rxq.reclaim_unproven(armed_rx);
            }
        }
        r
    }

    /// Eine ARP-Anfrage nach `target_ip` ab `at` in `buf` schreiben (42 Byte, Rest bis 60 genullt).
    ///
    /// Nimmt den Puffer als `&mut Owned<Driver>` statt als rohe Adresse: damit ist an der Signatur
    /// abzulesen, dass diese Funktion nur auf einem Puffer laufen darf, der **nicht** armiert ist.
    /// Vorher war das eine Zusage im Kopf des Aufrufers.
    ///
    /// # Safety
    /// `[at, at + FRAME_LEN)` muss innerhalb von `buf` liegen und beschreibbar sein.
    unsafe fn build_arp_request(
        &self,
        buf: &mut crate::Owned<crate::Driver>,
        at: u64,
        mac: &[u8; 6],
        src_ip: [u8; 4],
        target_ip: [u8; 4],
    ) {
        buf.zero(at, FRAME_LEN as u64);
        for i in 0..6u64 {
            buf.wr8(at + i, 0xff); // Ethernet-Ziel: Broadcast
            buf.wr8(at + 6 + i, mac[i as usize]); // Ethernet-Quelle
            buf.wr8(at + 22 + i, mac[i as usize]); // ARP sender hardware address
        }
        // Ethertype, htype (Ethernet), ptype (IPv4), hlen, plen, oper — alles Big-Endian.
        buf.wr8(at + 12, (ETHERTYPE_ARP >> 8) as u8);
        buf.wr8(at + 13, ETHERTYPE_ARP as u8);
        buf.wr8(at + 15, 1); // htype = 1
        buf.wr8(at + 16, 0x08); // ptype = 0x0800
        buf.wr8(at + 18, 6); // hlen
        buf.wr8(at + 19, 4); // plen
        buf.wr8(at + 21, ARP_REQUEST as u8);
        for i in 0..4u64 {
            buf.wr8(at + 28 + i, src_ip[i as usize]); // sender protocol address
            buf.wr8(at + 38 + i, target_ip[i as usize]); // target protocol address
        }
        // target hardware address (Offset 32..38) bleibt null — das ist die Frage.
    }
}
