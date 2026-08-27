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
//!
//! ## The generic path, beside the probe and not in place of it
//!
//! [`VirtioNet::arp_probe`] answers exactly one question — goes a frame out, and does the answer
//! to it come back — and the suite measures it by name (`vnet`, and `dmaiso` through
//! [`VirtioNet::arp_probe_rx_at`]). It sends once and polls once. That is all a one-shot proof
//! needs, and it is useless for a protocol stack: after that single poll nothing is armed any
//! more, so a second frame has nowhere to land.
//!
//! [`VirtioNet::link_up`], [`VirtioNet::tx`] and [`VirtioNet::rx`] are the second path. They were
//! added **beside** the probe and share no code with it, deliberately: a shared helper would make
//! the probe's behaviour depend on edits made for the stack, and a measuring line whose behaviour
//! moves without its name moving is one nobody re-reads.
//!
//! The one thing that decides whether a stack works here is the **re-arming**: [`VirtioNet::rx`]
//! gives the receive descriptor back to the device after every frame. Whoever forgets it sees
//! exactly one frame and then silence — the same picture as a reused queue region that was never
//! initialised, where the device starts over at 0 while the driver waits for progress that has
//! already happened.
//!
//! ## One frame at a time on the send side, and the ceiling is NAMED
//!
//! There is exactly **one** send buffer in this layout ([`OFF_TXBUF`], [`TXBUF_LEN`]), so there
//! can be exactly one send outstanding. Every descriptor [`VirtioNet::tx`] arms points at that
//! same buffer; a second descriptor would not get separate memory, it would get the same bytes
//! under a different index. Writing it while the device still holds the first descriptor puts a
//! frame on the wire that is half send *n* and half send *n+1* — the failure `owned` was built
//! against, arriving through the back door.
//!
//! So `tx` **refuses** while a descriptor is outstanding ([`TxOutcome::Busy`]) instead of
//! proceeding and counting afterwards. The refusal is a return value, not a `false` shared with
//! three other outcomes, and it does not block the caller: the completion is reclaimed at the top
//! of the next `tx`, and the send after that goes through. A capacity of one with no name would be
//! the D11 shape; a capacity of one with a name is a queue depth.

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

/// Largest Ethernet frame the generic path carries, **without** the 12-byte virtio header and
/// without the FCS (the device appends that): 14 byte Ethernet header + 1500 byte payload.
pub const MAX_FRAME: usize = 1514;

/// Smallest frame that may go on the wire. Anything shorter is zero-padded up to this length
/// **before** the descriptor is armed, and the descriptor carries the padded length.
///
/// This is not politeness: a bare TCP ACK is 54 byte, perfectly legal for the layer above and
/// illegal on the wire, and a switch that drops it produces a retransmission timeout — a symptom
/// pointing at the peer instead of at the sender. Deliberately a **second** constant next to
/// `FRAME_LEN`: that one is the length of the single fixed ARP frame the probe sends, this one is
/// a floor for arbitrary frames. One constant for both meanings is the shape this repository has
/// already paid for.
pub const MIN_FRAME: usize = 60;

/// Bytes the generic send buffer occupies (virtio header + [`MAX_FRAME`]).
pub const TXBUF_LEN: u32 = HDR_LEN as u32 + MAX_FRAME as u32;

// **The layout is asserted, not remembered.** `programs/hardware/virtio-net` checks
// `dma_len < REGION_BYTES` before it hands the region over, so REGION_BYTES must not grow —
// which means every buffer added here has to fit into what is already there. A wrong constant
// would not fail loudly: the send buffer would run into the region end, or the receive buffer
// into the send buffer, and the result reads as a device that corrupts frames under load.
const _: () = assert!(OFF_RXQ + crate::Queue::BYTES <= OFF_TXQ);
const _: () = assert!(OFF_TXQ + crate::Queue::BYTES <= OFF_RXBUF);
const _: () = assert!(OFF_RXBUF + RXBUF_LEN as u64 <= OFF_TXBUF);
const _: () = assert!(OFF_TXBUF + TXBUF_LEN as u64 <= REGION_BYTES);

/// [`VirtioNet::rx`]: a frame arrived and did **not** fit into `out`. It is dropped and the
/// descriptor is re-armed, so one oversized frame cannot wedge the queue — but the caller is told
/// that a frame existed, which is a different fact from `0` ("nothing was waiting").
pub const RX_TOO_LARGE: usize = usize::MAX;

/// [`VirtioNet::rx`]: the call named a region other than the one [`VirtioNet::link_up`] was given.
///
/// The two address axes are the whole point of this crate, and a call that swaps `cpu` and `dev`
/// would otherwise behave exactly like a link that receives nothing. Nothing was touched; the
/// device was not even looked at.
pub const RX_WRONG_LINK: usize = usize::MAX - 1;

/// [`VirtioNet::rx`]: no receive descriptor is armed, so nothing **can** land. Distinct from `0`
/// for the same reason `rx_used` is distinct from "data arrived": one says the device had no
/// opportunity, the other that it had one and did not use it.
pub const RX_UNARMED: usize = usize::MAX - 2;

/// [`VirtioNet::rx`]: the device completed a descriptor that carried **nothing but the 12-byte
/// header**. The completion was consumed, the cursor advanced, the descriptor was re-armed — and
/// there is no frame to hand up.
///
/// This used to be reported as `0`, i.e. as "nothing was waiting". The two look alike from the
/// outside and are opposite diagnoses: `0` says the device did not move, `RX_RUNT` says it moved
/// and delivered an empty completion. A caller that treats `rx(..) == 0` as "the used ring did not
/// advance" was simply wrong in that case. Same distinction as `rx_used` against "data arrived",
/// one level down.
pub const RX_RUNT: usize = usize::MAX - 3;

// **A sentinel that a real length can reach is not a sentinel.** The four are distinct by
// construction above, and the assert keeps them that way when a fifth is added at the wrong end of
// the range; the second line is the one that matters, and it is the one nobody would think to
// write: `rx` can return at most `RXBUF_LEN - HDR_LEN` bytes, so every sentinel must sit above
// that. A collision would not fail loudly — a frame of exactly the wrong length would read as
// "wrong link", and the caller would tear down a link that was working.
const _: () = assert!(RX_RUNT < RX_UNARMED && RX_UNARMED < RX_WRONG_LINK);
const _: () = assert!(RX_WRONG_LINK < RX_TOO_LARGE);
const _: () = assert!((RXBUF_LEN as usize - HDR_LEN as usize) < RX_RUNT);

/// The **one** send descriptor.
///
/// One, because there is one send buffer: [`VirtioNet::tx`] refuses while a descriptor is
/// outstanding, so a second index would never be armed at the same time as the first and would
/// only make the reader believe two frames can be in flight. Index 0 exists in every queue —
/// `queue_setup` clamps the requested size to the device's, which is at least 1 or the queue does
/// not exist at all.
///
/// It also settles the available ring: with at most one published-but-unconsumed entry, the ring
/// of 8 cannot wrap onto an entry the device has not read.
const TX_DESC: u16 = 0;

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

/// Calls that never reached the device, counted per link.
///
/// Every one of these is **also** a named return value ([`TxOutcome`], [`RX_TOO_LARGE`],
/// [`RX_WRONG_LINK`], [`RX_RUNT`]) — the counter is the *history*, not the second channel. That
/// split matters: a caller sees one call at a time and answers it, a report line sees the link's
/// whole life and diagnoses it. A silent link with a rising `rx_runt` and a silent link without
/// one are different findings, and no single return value can say which one you have.
///
/// Reachable as a whole through [`NetLink::stats`]; the field order on the wire is fixed by
/// [`LinkStats::to_words`].
///
/// Counters saturate rather than wrap: a counter that wraps to 0 reads as "never happened".
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LinkRefusals {
    /// `cpu`/`dev` did not match what [`VirtioNet::link_up`] was given — an axis mix-up.
    /// ([`TxOutcome::WrongLink`] / [`RX_WRONG_LINK`].)
    pub wrong_addresses: u32,
    /// A frame handed to [`VirtioNet::tx`] was empty or longer than [`MAX_FRAME`].
    /// ([`TxOutcome::BadLength`].)
    pub tx_len_refused: u32,
    /// [`VirtioNet::tx`] was called while the previous descriptor was still the device's, and
    /// refused ([`TxOutcome::Busy`]). Nothing was written and nothing was published.
    ///
    /// A link whose `tx_busy` climbs is being polled faster than its device drains, or is being
    /// given a `max_poll` too short for it — the same diagnosis `late_sends` carries, seen from
    /// the caller's side instead of the device's.
    pub tx_busy: u32,
    /// A received frame did not fit into the caller's `out` ([`RX_TOO_LARGE`]).
    pub rx_too_large: u32,
    /// The device completed a receive descriptor with no frame in it (it reported at most the
    /// 12 byte header) — [`RX_RUNT`].
    pub rx_runt: u32,
}

/// What one [`VirtioNet::tx`] call did. **Five outcomes, five values.**
///
/// This was a `bool` until the send path grew a capacity. The `bool` said "the device consumed the
/// descriptor within `max_poll`", and three refusals said `false` alongside it — "the device
/// stayed silent" and "the caller handed in a 9000-byte frame" point at different people, and the
/// caller could not tell them apart at the point where it has to decide what to do next.
///
/// Only [`Self::Sent`] means the frame is gone. Everything else means it is not, and each one
/// names a different reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxOutcome {
    /// The device consumed the descriptor within `max_poll`. The buffer is back in driver hands.
    Sent,
    /// The descriptor was armed, published and kicked, and the device did **not** complete it
    /// within `max_poll`. It is still the device's, and the buffer with it.
    ///
    /// Not a failure of the caller and not proof of a dead device — only proof that the poll
    /// budget ran out first. The next [`VirtioNet::tx`] reclaims the completion if it has turned
    /// up ([`NetLink::late_sends`]) and refuses with [`Self::Busy`] if it has not.
    Outstanding,
    /// **Refused**: the descriptor from an earlier send is still outstanding. Nothing was written,
    /// nothing was published, the device was not kicked.
    ///
    /// There is one send buffer, so a second frame would be written over the bytes the live
    /// descriptor points at. The caller is not blocked by the refusal — it may poll
    /// [`VirtioNet::rx`], do other work, and call `tx` again; the completion is picked up at the
    /// top of that call. A device that never completes leaves the send side refused for good, and
    /// the way out of that is a fresh [`VirtioNet::link_up`], which resets the device.
    Busy,
    /// **Refused**: the frame was empty or longer than [`MAX_FRAME`]. Truncating it would produce
    /// a frame the peer answers wrongly, which is worse than one it never sees.
    BadLength,
    /// **Refused**: `cpu`/`dev` are not the ones the link was brought up on. The device was not
    /// even looked at.
    WrongLink,
}

impl TxOutcome {
    /// Did the frame go out and get confirmed? True for [`Self::Sent`] and nothing else.
    ///
    /// Here so a caller that only wants the old `bool` writes `.sent()` instead of comparing
    /// against a variant it might pick wrongly — `Outstanding` in particular is *not* success.
    pub fn sent(self) -> bool {
        matches!(self, TxOutcome::Sent)
    }

    /// Was this call turned away before anything was written or published?
    ///
    /// True for the three refusals; false for [`Self::Sent`] and [`Self::Outstanding`], both of
    /// which put a descriptor into the device's hands. The distinction is what a caller needs to
    /// decide whether the frame may be re-staged.
    pub fn refused(self) -> bool {
        matches!(self, TxOutcome::Busy | TxOutcome::BadLength | TxOutcome::WrongLink)
    }
}

/// Everything one link has counted, in a single `Copy` value.
///
/// It exists because the individual getters were **unreachable in practice**: they were public,
/// and nothing outside this file read them, so `rx_runt` — the only thing that distinguished
/// "nothing waiting" from "runt consumed" before [`RX_RUNT`] existed — was a fact nobody could
/// observe. A named overflow whose name nobody can reach is the D11 shape one level up.
///
/// One call, fixed field order, no allocation: a driver PD can put this straight into a reply.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LinkStats {
    /// Frames the device confirmed it consumed ([`TxOutcome::Sent`]).
    pub frames_sent: u32,
    /// Frames handed up to a caller of [`VirtioNet::rx`] — not completions.
    pub frames_received: u32,
    /// Completions of an earlier send that turned up during a later [`VirtioNet::tx`].
    pub late_sends: u32,
    /// Times the used ring moved while **no** descriptor was outstanding, so the link's cursor was
    /// resynchronised to it. The used ring is device-written memory; a device is free to advance
    /// it whenever it likes, and a driver that kept its stale cursor would read the next
    /// `poll_used` as an instant success for a frame the device never saw.
    pub tx_resyncs: u32,
    /// Calls that never reached the device.
    pub refusals: LinkRefusals,
}

impl LinkStats {
    /// Slots in [`Self::to_words`].
    pub const WORDS: usize = 9;

    /// The counters as one fixed-width word array — the shape that fits in an IPC reply.
    ///
    /// The order is this function, and the destructuring below is exhaustive **on purpose**: a
    /// counter added to [`LinkStats`] or [`LinkRefusals`] without a slot here is a compile error,
    /// not a number that quietly stops being reported. Getters could be forgotten one at a time,
    /// and were — all five of them, for the whole life of the generic path.
    pub fn to_words(&self) -> [u32; Self::WORDS] {
        let LinkStats { frames_sent, frames_received, late_sends, tx_resyncs, refusals } = *self;
        let LinkRefusals { wrong_addresses, tx_len_refused, tx_busy, rx_too_large, rx_runt } =
            refusals;
        [
            frames_sent,
            frames_received,
            late_sends,
            tx_resyncs,
            wrong_addresses,
            tx_len_refused,
            tx_busy,
            rx_too_large,
            rx_runt,
        ]
    }

    /// The inverse of [`Self::to_words`], for whoever reads the reply on the other side.
    ///
    /// Here rather than at the reader, so the two halves of the encoding cannot drift apart: a
    /// round trip is checkable in this crate's own tests, a reader in another crate is not.
    pub fn from_words(w: [u32; Self::WORDS]) -> Self {
        LinkStats {
            frames_sent: w[0],
            frames_received: w[1],
            late_sends: w[2],
            tx_resyncs: w[3],
            refusals: LinkRefusals {
                wrong_addresses: w[4],
                tx_len_refused: w[5],
                tx_busy: w[6],
                rx_too_large: w[7],
                rx_runt: w[8],
            },
        }
    }
}

/// **A brought-up link**: both queues, their notify offsets, the used-ring cursors, and the
/// receive buffer that is currently in the device's hands.
///
/// Not `Copy`, and that is the mechanism rather than an oversight: the link **owns** the armed
/// receive buffer as an `Owned<Device>`, and that type has no access path at all (s.
/// [`crate::owned`]). While the buffer sits in here there is no way to read the bytes the device
/// is writing; the read path opens only against a used-ring completion. A copyable link would
/// hand out a second name for that one buffer.
///
/// Dropping a link does **not** stop the device — the descriptor stays armed and points into the
/// DMA region. Whoever wants the region back resets the transport (another [`VirtioNet::link_up`],
/// which begins with `reset`), and the old link is stale from that moment on.
pub struct NetLink {
    rxq: crate::Queue,
    txq: crate::Queue,
    rx_notify: u16,
    tx_notify: u16,
    /// How far this driver has consumed each used ring. Kept per link rather than re-read before
    /// every wait: `poll_used` judges "did the ring move" against a starting point, and a starting
    /// point read fresh after a send that timed out would already include that send's late
    /// completion.
    rx_seen: u16,
    tx_seen: u16,
    /// The two views of the region this link was brought up on. They are stored to be **checked**,
    /// not to be used — every call names them again, and a call that names different ones is a
    /// driver bug that would otherwise look like a dead device.
    cpu_base: u64,
    dev_base: u64,
    /// The receive buffer, armed. `None` only between reclaim and re-arm inside [`VirtioNet::rx`],
    /// which cannot be observed from outside (no unwinding here — `panic = abort`).
    rx: Option<crate::Owned<crate::Device>>,
    /// The send buffer while the device holds it, i.e. after a [`TxOutcome::Outstanding`].
    ///
    /// `Some` **is** the capacity: [`VirtioNet::tx`] refuses while this is occupied, because the
    /// next send would write over the bytes that descriptor points at. Holding the
    /// `Owned<Device>` rather than dropping it through `reclaim_unproven` is the honest form —
    /// while the device owns the buffer there is no `Owned<Driver>` naming it, so the typestate
    /// says what the code does. It goes back to the driver side only against a completion.
    tx_inflight: Option<crate::Owned<crate::Device>>,
    refusals: LinkRefusals,
    tx_frames: u32,
    rx_frames: u32,
    late_sends: u32,
    tx_resyncs: u32,
}

impl NetLink {
    /// Calls that never reached the device, by cause.
    pub fn refusals(&self) -> LinkRefusals {
        self.refusals
    }
    /// Frames the device confirmed it consumed (used ring advanced within `max_poll`).
    pub fn frames_sent(&self) -> u32 {
        self.tx_frames
    }
    /// Frames handed up to the caller — **not** completions: a runt or an oversized frame advances
    /// the used ring and is not counted here.
    pub fn frames_received(&self) -> u32 {
        self.rx_frames
    }
    /// Completions of an earlier send that only turned up during a later [`VirtioNet::tx`].
    ///
    /// Each one means some previous `tx` returned [`TxOutcome::Outstanding`] while the device was
    /// merely slow. The buffer was **not** rewritten in the meantime — that is what
    /// [`TxOutcome::Busy`] is for — so a late send costs latency and nothing else. A link with
    /// `late_sends > 0` has been given a `max_poll` too short for its device.
    pub fn late_sends(&self) -> u32 {
        self.late_sends
    }
    /// Is the send buffer still in the device's hands?
    ///
    /// While this is true the next [`VirtioNet::tx`] is refused with [`TxOutcome::Busy`] unless
    /// the completion has turned up by then. Reads a field, touches no device memory — a polling
    /// stack can ask before it stages a frame it would only have to stage again.
    pub fn tx_pending(&self) -> bool {
        self.tx_inflight.is_some()
    }
    /// **Everything this link counted, in one value.** See [`LinkStats`].
    ///
    /// The one call a report line needs. The getters above stay because they read well at a call
    /// site that wants exactly one number; this is what a driver PD puts into its reply.
    pub fn stats(&self) -> LinkStats {
        LinkStats {
            frames_sent: self.tx_frames,
            frames_received: self.rx_frames,
            late_sends: self.late_sends,
            tx_resyncs: self.tx_resyncs,
            refusals: self.refusals,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// **The decisions of the generic path, as pure functions.**
//
// Everything below is arithmetic over numbers the caller and the device hand in. It sits outside
// `VirtioNet` on purpose: `tx` and `rx` are `unsafe fn`s over MMIO and raw addresses and cannot be
// exercised without a device, so any judgement left inside them is a judgement no test can reach —
// and this file had zero tests for the generic path while carrying its whole return table in
// prose. Split out, the tables are literals against literals.
// ---------------------------------------------------------------------------------------------

/// What [`VirtioNet::tx`] may do next, from the two facts it can read before touching anything:
/// whether a descriptor of ours is outstanding, and whether the device's used ring has moved since
/// this link last looked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TxGate {
    /// Nothing outstanding, ring where we left it: send.
    Send,
    /// Our descriptor completed after its poll budget: take the buffer back against the
    /// completion, count it late, then send.
    Reclaim,
    /// The ring moved with nothing of ours in it. Follow it and count — a stale cursor turns the
    /// next `poll_used` into an instant false success.
    Resync,
    /// Our descriptor is still the device's: refuse, write nothing.
    Busy,
}

/// The gate, spelled out. All four combinations are named; none falls through to "send anyway".
const fn tx_gate(outstanding: bool, ring_moved: bool) -> TxGate {
    match (outstanding, ring_moved) {
        (false, false) => TxGate::Send,
        (true, true) => TxGate::Reclaim,
        (false, true) => TxGate::Resync,
        (true, false) => TxGate::Busy,
    }
}

/// The length that goes on the wire for a frame of `frame_len` bytes — `None` if it may not go out
/// at all.
///
/// Padding up to [`MIN_FRAME`] is the whole content of this function, and it is a decision, not
/// politeness: a bare TCP ACK is 54 byte and a switch may drop it, which reads as a retransmission
/// timeout at the peer. Refusing above [`MAX_FRAME`] is the other half — truncating would put a
/// frame on the wire that the peer answers wrongly.
const fn tx_wire_len(frame_len: usize) -> Option<usize> {
    if frame_len == 0 || frame_len > MAX_FRAME {
        return None;
    }
    Some(if frame_len < MIN_FRAME { MIN_FRAME } else { frame_len })
}

/// What [`VirtioNet::rx`] returns once a completion has been consumed: a payload length,
/// [`RX_RUNT`] or [`RX_TOO_LARGE`].
///
/// `reported_len` is the device's own statement of how many bytes it wrote and is clamped to
/// [`RXBUF_LEN`] before anything is derived from it — an unclamped device number is the difference
/// between a 2048-byte buffer and a read far past the region.
///
/// Never returns `0`: "nothing was waiting" is decided before a completion exists, and merging the
/// two was the reason a caller could not tell an empty ring from a consumed runt.
fn rx_payload(reported_len: u32, out_len: usize) -> usize {
    let written = if reported_len > RXBUF_LEN { RXBUF_LEN as u64 } else { reported_len as u64 };
    let n = written.saturating_sub(HDR_LEN) as usize;
    if n == 0 {
        RX_RUNT
    } else if n > out_len {
        RX_TOO_LARGE
    } else {
        n
    }
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

    /// **Bring both queues up once and arm the receive queue.** `None` if the handshake failed.
    ///
    /// The handshake is the same one [`Self::arp_probe_rx_at`] performs, written out a second time
    /// rather than factored out of it: the probe is a measured line, and a shared helper would let
    /// an edit made for the stack change what that line reports without changing its name.
    ///
    /// What is genuinely different is the last step. The probe arms one receive buffer for its one
    /// expected answer; here the buffer stays armed and is handed back to the device after every
    /// frame ([`Self::rx`]). The whole queue is initialised by `queue_setup` — driver half **and**
    /// device half — which matters because a driver PD may inherit the region of its predecessor:
    /// the device restarts its used index at 0 after the reset while the old end value still sits
    /// in memory, and a driver that took that value as its starting point would wait for progress
    /// that has already happened.
    ///
    /// Calling this again resets the device; any earlier [`NetLink`] is stale from that point on
    /// and must not be used.
    ///
    /// # Safety
    /// The transport's MMIO addresses must belong to this device; `[cpu, cpu + REGION_BYTES)` must
    /// be writable memory owned by the caller alone, and `dev` must be the **device's** view of
    /// exactly that memory.
    pub unsafe fn link_up(&self, cpu: u64, dev: u64) -> Option<NetLink> {
        self.t.reset();
        let offered = self.t.offered();
        if offered & F_ACCESS_PLATFORM == 0 || offered & F_MAC == 0 {
            return None;
        }
        // The same three bits as the probe, for the same reason: every negotiated feature is a
        // promise the driver would have to keep. A receive path that agrees to checksum offload
        // and then does not evaluate it reads frames wrongly instead of not at all.
        if !self.t.negotiate(F_VERSION_1 | F_ACCESS_PLATFORM | F_MAC) {
            return None;
        }
        // `VIRTIO_NET_F_MAC` promises a MAC **exists** in config space, not that we found the
        // capability. Without it `cfg8` returns zeros and [`Self::mac`] would hand out
        // 00:00:00:00:00:00 — an address the caller cannot tell from a real one.
        if !self.t.has_device_cfg() {
            return None;
        }

        let (rxq, rx_notify) = self.t.queue_setup(0, cpu + OFF_RXQ, dev + OFF_RXQ, 8)?;
        // Queue 1, and with **its own** notify offset: `queue_notify_off` is per queue, and a
        // driver that reuses the first queue's notify address wakes the device on the wrong side.
        let (txq, tx_notify) = self.t.queue_setup(1, cpu + OFF_TXQ, dev + OFF_TXQ, 8)?;
        self.t.driver_ok();

        // The receive buffer is carved once and stays carved for the life of the link. The send
        // buffer is not: [`Self::tx`] carves it per call from a fresh `Region`, so its descriptor
        // length is the padded frame length and never a stale one. The two areas are disjoint by
        // the layout asserts above, so the two `Region`s cannot hand out overlapping buffers.
        let mut region = crate::Region::from_raw(cpu, dev, REGION_BYTES);
        let mut rxbuf = region.carve(OFF_RXBUF, RXBUF_LEN)?;
        // Zero the **whole** buffer once, not its first word (A-5.4): a buffer holding the right
        // bytes is indistinguishable from a written one until somebody clears it first.
        rxbuf.zero(0, RXBUF_LEN as u64);

        // Cursors **before** arming. Taken afterwards they could already contain the completion of
        // the first frame, and that frame would then never be handed up.
        let rx_seen = rxq.used_idx();
        let tx_seen = txq.used_idx();
        let armed = rxq.arm(0, rxbuf, VIRTQ_DESC_F_WRITE, 0);
        rxq.publish(0, self.t.fence);
        self.t.kick(rx_notify, 0);

        Some(NetLink {
            rxq,
            txq,
            rx_notify,
            tx_notify,
            rx_seen,
            tx_seen,
            cpu_base: cpu,
            dev_base: dev,
            rx: Some(armed),
            tx_inflight: None,
            refusals: LinkRefusals::default(),
            tx_frames: 0,
            rx_frames: 0,
            late_sends: 0,
            tx_resyncs: 0,
        })
    }

    /// **The MAC from device config space.** `None` if `VIRTIO_NET_F_MAC` is absent.
    ///
    /// The requirement is not a second one: it is the same `F_MAC` the handshake already demands,
    /// asked again here because this function is reachable without a [`NetLink`]. Made-up
    /// addresses are the alternative, and a frame that leaves with sender 00:00:00:00:00:00 gets
    /// no answer — a finding that points at the peer instead of at the enumeration.
    ///
    /// Reading the offered features after `DRIVER_OK` is harmless: `device_feature_select` only
    /// chooses which half of the **device's** feature word is visible, while the negotiated set was
    /// latched at `FEATURES_OK` and is not touched here.
    ///
    /// # Safety
    /// The transport's MMIO addresses must belong to this device.
    pub unsafe fn mac(&self) -> Option<[u8; 6]> {
        if !self.t.has_device_cfg() || self.t.offered() & F_MAC == 0 {
            return None;
        }
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = self.t.cfg8(i as u64);
        }
        Some(mac)
    }

    /// **Send one Ethernet frame.** `frame` carries no virtio header — this prepends the 12 bytes.
    ///
    /// | return | meaning |
    /// |---|---|
    /// | [`TxOutcome::Sent`] | the device consumed the descriptor within `max_poll` |
    /// | [`TxOutcome::Outstanding`] | armed and kicked, no completion within `max_poll` — the descriptor is still the device's |
    /// | [`TxOutcome::Busy`] | **refused**: the previous descriptor is still outstanding. Nothing written, nothing published |
    /// | [`TxOutcome::BadLength`] | **refused**: `frame` was empty or longer than [`MAX_FRAME`] |
    /// | [`TxOutcome::WrongLink`] | **refused**: `cpu`/`dev` are not the ones the link was brought up on |
    ///
    /// Every one of them is also counted, s. [`NetLink::stats`] — the return value answers *this*
    /// call, the counters describe the link.
    ///
    /// Frames shorter than [`MIN_FRAME`] are zero-padded, frames longer than [`MAX_FRAME`] are
    /// refused rather than truncated: a truncated frame is a frame the peer answers wrongly, which
    /// is worse than one it never sees.
    ///
    /// **Only one send at a time**, s. the module doc: there is one send buffer, every descriptor
    /// points at it, and rewriting it under a live descriptor puts half of one frame and half of
    /// the next on the wire. So the refusal comes *before* the write, not a counter after it.
    ///
    /// # Safety
    /// As [`Self::link_up`], and `l` must be a link brought up on exactly `(cpu, dev)`.
    #[must_use]
    pub unsafe fn tx(
        &self,
        l: &mut NetLink,
        cpu: u64,
        dev: u64,
        frame: &[u8],
        max_poll: u64,
    ) -> TxOutcome {
        if cpu != l.cpu_base || dev != l.dev_base {
            l.refusals.wrong_addresses = l.refusals.wrong_addresses.saturating_add(1);
            return TxOutcome::WrongLink;
        }

        // **Settle the previous send before touching anything.** Two facts decide it, and both are
        // read here: whether a descriptor of ours is outstanding, and whether the device's used
        // ring has moved since this link last looked. The gate is a pure function so the four
        // combinations are checkable without a device (`tx_gate_covers_all_four`).
        let seen = l.txq.used_idx();
        match tx_gate(l.tx_inflight.is_some(), seen != l.tx_seen) {
            TxGate::Send => {}
            TxGate::Reclaim => {
                // The completion turned up after its poll budget. Take the buffer back **against
                // the completion**, not through `reclaim_unproven`: the proof exists now.
                let done = l.txq.used_entry(l.tx_seen);
                if let Some(armed) = l.tx_inflight.take() {
                    let _ = l.txq.reclaim(armed, &done);
                }
                l.tx_seen = l.tx_seen.wrapping_add(1);
                l.late_sends = l.late_sends.saturating_add(1);
            }
            TxGate::Resync => {
                // The used ring moved and nothing of ours was in it. That is device-written
                // memory, so a device may do this whenever it likes; the damage would be a stale
                // cursor, against which the next `poll_used` returns instantly and reports a frame
                // as consumed that the device never saw. Follow the ring and name it.
                l.tx_seen = seen;
                l.tx_resyncs = l.tx_resyncs.saturating_add(1);
            }
            TxGate::Busy => {
                l.refusals.tx_busy = l.refusals.tx_busy.saturating_add(1);
                return TxOutcome::Busy;
            }
        }

        let Some(wire) = tx_wire_len(frame.len()) else {
            l.refusals.tx_len_refused = l.refusals.tx_len_refused.saturating_add(1);
            return TxOutcome::BadLength;
        };
        // Carved per call with the padded length, because `arm` takes the descriptor length from
        // the buffer: a length that outlives the frame it was cut for would hand the device bytes
        // of the previous frame.
        let mut region = crate::Region::from_raw(cpu, dev, REGION_BYTES);
        let Some(mut buf) = region.carve(OFF_TXBUF, HDR_LEN as u32 + wire as u32) else {
            // Unreachable — `tx_wire_len` bounds `wire` by `MAX_FRAME`, and the layout assert
            // above proves `OFF_TXBUF + HDR_LEN + MAX_FRAME` fits. Fail closed anyway, and charge
            // it to the length: the only way here is a length the region does not hold.
            l.refusals.tx_len_refused = l.refusals.tx_len_refused.saturating_add(1);
            return TxOutcome::BadLength;
        };
        // 12 zero bytes: no offload, no GSO. Anything else would be a promise about the frame
        // that the layer above never made.
        buf.zero(0, HDR_LEN);
        for (i, b) in frame.iter().enumerate() {
            buf.wr8(HDR_LEN + i as u64, *b);
        }
        if wire > frame.len() {
            buf.zero(HDR_LEN + frame.len() as u64, (wire - frame.len()) as u64);
        }
        self.t.fence();

        let from = l.tx_seen;
        let armed = l.txq.arm(TX_DESC, buf, 0, 0);
        l.txq.publish(TX_DESC, self.t.fence); // the transport's barrier, never a generic one
        self.t.kick(l.tx_notify, 1);

        match l.txq.poll_used(from, max_poll) {
            Some(done) => {
                let _ = l.txq.reclaim(armed, &done);
                l.tx_seen = from.wrapping_add(1);
                l.tx_frames = l.tx_frames.saturating_add(1);
                TxOutcome::Sent
            }
            // No proof: the device may still be reading the buffer. It **keeps** it — the
            // `Owned<Device>` stays in the link, so there is no driver-side name for those bytes
            // until a completion says there may be one. The cursor stays put as well, so the
            // completion is recognised as late on the next call instead of being counted as this
            // frame's.
            None => {
                l.tx_inflight = Some(armed);
                TxOutcome::Outstanding
            }
        }
    }

    /// **Take one received frame, without blocking.** Returns its length in `out`, `0` if the used
    /// ring has not moved.
    ///
    /// | return | meaning | used ring moved? |
    /// |---|---|---|
    /// | `1..=(RXBUF_LEN - HDR_LEN)` | that many bytes of Ethernet frame are in `out` (virtio header stripped). Not capped at [`MAX_FRAME`]: what the device delivered is reported, not what this driver would have sent | yes |
    /// | `0` | nothing was waiting — the normal outcome of a poll, not a failure | **no** |
    /// | [`RX_RUNT`] | a completion carrying nothing but the 12-byte header was consumed and the descriptor re-armed | yes |
    /// | [`RX_TOO_LARGE`] | a frame arrived and did not fit into `out`; it was dropped | yes |
    /// | [`RX_WRONG_LINK`] | `cpu`/`dev` are not the ones the link was brought up on | not looked at |
    /// | [`RX_UNARMED`] | no descriptor is armed, so nothing can land | not looked at |
    ///
    /// The third column is the reason [`RX_RUNT`] exists: it used to be `0`, and a caller that read
    /// the table and treated `0` as "the ring did not advance" was wrong in exactly that row —
    /// the descriptor **was** consumed, the cursor advanced and the buffer was re-armed. Two facts,
    /// two values.
    ///
    /// The descriptor is re-armed in **every** case in which a completion was consumed — including
    /// the dropped ones. Otherwise a single oversized frame would wedge the queue for good, and the
    /// symptom would be a link that goes quiet after one bad packet.
    ///
    /// # Safety
    /// As [`Self::link_up`], and `l` must be a link brought up on exactly `(cpu, dev)`.
    #[must_use]
    pub unsafe fn rx(&self, l: &mut NetLink, cpu: u64, dev: u64, out: &mut [u8]) -> usize {
        if cpu != l.cpu_base || dev != l.dev_base {
            l.refusals.wrong_addresses = l.refusals.wrong_addresses.saturating_add(1);
            return RX_WRONG_LINK;
        }
        let Some(armed) = l.rx.take() else {
            return RX_UNARMED;
        };
        if l.rxq.used_idx() == l.rx_seen {
            l.rx = Some(armed); // untouched — the device still owns it
            return 0;
        }

        // **The proof first, then the access.** The completion is what "the device is done with
        // this buffer" means; without it the read would race the device's write, and that it went
        // well would depend on the order in the author's head.
        let done = l.rxq.used_entry(l.rx_seen);
        let buf = l.rxq.reclaim(armed, &done);
        self.t.fence();

        // **The whole return table for a consumed completion, in one pure function** — including
        // the clamp, because the length comes from the device: a buffer of 2048 byte and a
        // reported length of 60 000 differ by whatever the driver would have read past its own
        // region. Pure so the table can be checked against literals instead of against a device.
        let result = rx_payload(done.len(), out.len());
        match result {
            // A completion with nothing but the header in it. Nothing to hand up — and since
            // `RX_RUNT` it is no longer indistinguishable from an empty ring at the call site.
            RX_RUNT => l.refusals.rx_runt = l.refusals.rx_runt.saturating_add(1),
            RX_TOO_LARGE => l.refusals.rx_too_large = l.refusals.rx_too_large.saturating_add(1),
            n => {
                for (i, b) in out.iter_mut().take(n).enumerate() {
                    *b = buf.rd8(HDR_LEN + i as u64);
                }
                l.rx_frames = l.rx_frames.saturating_add(1);
            }
        }

        // **Give the descriptor back.** This is the whole difference from the probe: without it
        // the second frame has nowhere to land, and the link looks like a peer that stopped
        // talking. The buffer is not re-zeroed — the only bytes ever read out of it are the ones
        // `rx_payload` allowed, and that number is the used ring's, i.e. the device's own
        // statement that it wrote them (clamped to the buffer, s. there).
        l.rx_seen = l.rx_seen.wrapping_add(1);
        let armed = l.rxq.arm(0, buf, VIRTQ_DESC_F_WRITE, 0);
        l.rxq.publish(0, self.t.fence);
        self.t.kick(l.rx_notify, 0);
        l.rx = Some(armed);
        result
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

// ---------------------------------------------------------------------------------------------
// **Host tests for the generic path** (`tools/host-tests.sh virtio`, which builds `lib.rs` and
// therefore this module — `pub mod net;` is what makes them reachable).
//
// Until now this file had none: `owned.rs` carried all three, and everything the generic path
// decides lived inside two `unsafe fn`s over MMIO, i.e. inside functions no host test can call.
// Every test below drives a pure function with literals. None of them touches a device, and that
// is not a limitation of the tests but the point — the faults these guard against (a sentinel that
// a length can reach, a return value that means two things, a send that overwrites a live
// descriptor) are all decisions about numbers.
// ---------------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// The four `rx` sentinels are distinct **and** unreachable as a payload length.
    ///
    /// The second half is the load-bearing one: `rx` may return up to `RXBUF_LEN - HDR_LEN`, and a
    /// sentinel inside that range would turn one particular frame length into "wrong link".
    #[test]
    fn rx_sentinels_are_distinct_and_out_of_range() {
        let all = [RX_TOO_LARGE, RX_WRONG_LINK, RX_UNARMED, RX_RUNT];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b, "sentinels {i} and {j} collide");
            }
            // `0` is a value of its own ("nothing was waiting") and must not be a sentinel.
            assert_ne!(*a, 0);
            assert!(*a > (RXBUF_LEN as usize - HDR_LEN as usize));
        }
    }

    /// `rx_payload` IS the return table of `rx` for a consumed completion — checked row by row.
    ///
    /// The regression this guards against has a name: before `RX_RUNT`, the first three rows all
    /// returned `0`, which is also what `rx` returns when the used ring never moved. A caller
    /// following the table treated a consumed, re-armed descriptor as "the device did not move".
    #[test]
    fn rx_payload_covers_the_return_table() {
        // A completion carrying at most the header: consumed, nothing to hand up.
        assert_eq!(rx_payload(0, 2048), RX_RUNT);
        assert_eq!(rx_payload(HDR_LEN as u32 - 1, 2048), RX_RUNT);
        assert_eq!(rx_payload(HDR_LEN as u32, 2048), RX_RUNT);
        // ... and it is NOT the same answer as an empty ring.
        assert_ne!(rx_payload(HDR_LEN as u32, 2048), 0);

        // One byte past the header is one byte of frame.
        assert_eq!(rx_payload(HDR_LEN as u32 + 1, 2048), 1);
        assert_eq!(rx_payload(HDR_LEN as u32 + MAX_FRAME as u32, 2048), MAX_FRAME);

        // The device's number is clamped to the buffer before anything is derived from it.
        assert_eq!(rx_payload(60_000, 4096), RXBUF_LEN as usize - HDR_LEN as usize);
        assert_eq!(rx_payload(u32::MAX, 4096), RXBUF_LEN as usize - HDR_LEN as usize);

        // Does not fit the caller's buffer: refused, never clamped into it.
        assert_eq!(rx_payload(HDR_LEN as u32 + 100, 99), RX_TOO_LARGE);
        assert_eq!(rx_payload(HDR_LEN as u32 + 100, 100), 100);
        assert_eq!(rx_payload(HDR_LEN as u32 + 1, 0), RX_TOO_LARGE);
    }

    /// Padding up to `MIN_FRAME`, refusal above `MAX_FRAME`, and neither at the wrong boundary.
    #[test]
    fn tx_wire_len_pads_and_refuses() {
        assert_eq!(tx_wire_len(0), None); // nothing to send is not a frame
        assert_eq!(tx_wire_len(1), Some(MIN_FRAME));
        assert_eq!(tx_wire_len(54), Some(MIN_FRAME)); // the bare TCP ACK this exists for
        assert_eq!(tx_wire_len(MIN_FRAME - 1), Some(MIN_FRAME));
        assert_eq!(tx_wire_len(MIN_FRAME), Some(MIN_FRAME)); // padded, not padded again
        assert_eq!(tx_wire_len(MIN_FRAME + 1), Some(MIN_FRAME + 1));
        assert_eq!(tx_wire_len(MAX_FRAME), Some(MAX_FRAME)); // the largest frame still goes
        assert_eq!(tx_wire_len(MAX_FRAME + 1), None); // refused, never truncated
        assert_eq!(tx_wire_len(9000), None); // a jumbo frame is somebody's mistake, not ours
    }

    /// The arithmetic that ties the padding to the layout: the largest buffer `tx` can ever carve
    /// is exactly `TXBUF_LEN`, and it fits the region.
    ///
    /// `tx` carves `HDR_LEN + wire` per call. If `tx_wire_len` ever allowed more than `MAX_FRAME`,
    /// the carve would run past `REGION_BYTES` and fail closed — no send, silently, for big frames
    /// only. Checked here rather than trusted, because the two constants live far apart.
    #[test]
    fn the_largest_send_fits_the_layout() {
        let widest = tx_wire_len(MAX_FRAME).expect("MAX_FRAME must be sendable");
        assert_eq!(HDR_LEN as u32 + widest as u32, TXBUF_LEN);
        assert!(OFF_TXBUF + TXBUF_LEN as u64 <= REGION_BYTES);
        // The receive buffer holds anything the device may deliver into it, header included.
        assert!(RXBUF_LEN as usize >= HDR_LEN as usize + MAX_FRAME);
        assert!(OFF_RXBUF + RXBUF_LEN as u64 <= OFF_TXBUF);
        assert!(MIN_FRAME <= MAX_FRAME);
        // The single send descriptor exists in the smallest queue a device can hand back.
        assert_eq!(TX_DESC, 0);
    }

    /// All four states of the send gate, and the one that refuses is the one that must.
    ///
    /// This is the fault of finding 5 in one line: with a descriptor outstanding and no completion
    /// in the ring, the old code went on to rewrite the send buffer — the bytes that live
    /// descriptor points at. `Busy` is the only answer that does not write.
    #[test]
    fn tx_gate_covers_all_four() {
        assert_eq!(tx_gate(false, false), TxGate::Send);
        assert_eq!(tx_gate(true, true), TxGate::Reclaim);
        assert_eq!(tx_gate(false, true), TxGate::Resync);
        assert_eq!(tx_gate(true, false), TxGate::Busy);
        // Outstanding and no completion is the fault case, and it must never be a send.
        assert_ne!(tx_gate(true, false), TxGate::Send);
        assert_ne!(tx_gate(true, false), TxGate::Reclaim);
    }

    /// `Sent` is success, `Outstanding` is not, and the three refusals stay apart from both.
    ///
    /// `Outstanding` reads like a failure and is not one: a descriptor is in the device's hands,
    /// so the frame must not be re-staged. That is exactly the distinction a `bool` could not
    /// carry.
    #[test]
    fn tx_outcome_keeps_success_refusal_and_in_flight_apart() {
        assert!(TxOutcome::Sent.sent());
        for o in [TxOutcome::Outstanding, TxOutcome::Busy, TxOutcome::BadLength, TxOutcome::WrongLink] {
            assert!(!o.sent(), "{o:?} must not read as sent");
        }
        for o in [TxOutcome::Busy, TxOutcome::BadLength, TxOutcome::WrongLink] {
            assert!(o.refused(), "{o:?} must read as refused");
        }
        // Nothing was written for these two, and a descriptor was armed for those two.
        assert!(!TxOutcome::Sent.refused());
        assert!(!TxOutcome::Outstanding.refused());
        // The refusal that finding 5 introduced is not the timeout it used to be folded into.
        assert_ne!(TxOutcome::Busy, TxOutcome::Outstanding);
    }

    /// Every counter survives the trip through `to_words` in its own slot.
    ///
    /// Distinct values on purpose: two counters sharing a slot, or swapped, is the failure this
    /// catches — and a report line built on a swapped pair diagnoses the wrong end of the link.
    /// The exhaustive destructuring inside `to_words` catches the other half (a counter with no
    /// slot at all) at compile time.
    #[test]
    fn link_stats_round_trip_keeps_every_counter_apart() {
        let s = LinkStats {
            frames_sent: 1,
            frames_received: 2,
            late_sends: 3,
            tx_resyncs: 4,
            refusals: LinkRefusals {
                wrong_addresses: 5,
                tx_len_refused: 6,
                tx_busy: 7,
                rx_too_large: 8,
                rx_runt: 9,
            },
        };
        let w = s.to_words();
        assert_eq!(w, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(w.len(), LinkStats::WORDS);
        assert_eq!(LinkStats::from_words(w), s);
        // An untouched link reports nothing, not "never measured" — every slot is a zero.
        assert_eq!(LinkStats::default().to_words(), [0u32; LinkStats::WORDS]);
    }
}
