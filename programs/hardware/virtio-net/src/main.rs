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
//! | 6 | geteilte Übertragungsfläche (8 KiB, normales RAM) | Rahmen zwischen Stack-PD und Treiber |
//!
//! Es hält **keine** Cap auf die Region des anderen Treibers — es bekommt nur deren Zahl.
//!
//! ## The three ops that turn the measurement into a service
//!
//! [`OP_SELF`] and [`OP_FOREIGN`] are unchanged, down to their reply shape: four separate values
//! (`features_ok`, `tx_used`, `rx_used`, `arp_reply`), because a single combined `bool` would
//! merge "sent but received nothing" with "never sent at all", and only the first of those is the
//! A-5.4 finding. Three further ops carry actual frames:
//!
//! | op | what it answers |
//! |---|---|
//! | [`OP_MAC`] | the negotiated MAC — a stack cannot build an Ethernet header without it |
//! | [`OP_TX`] | send the frame staged in the shared area |
//! | [`OP_RX`] | stage one received frame there, or say that none was waiting |
//!
//! ### `ST_EMPTY` is a normal outcome, not a failure
//!
//! A poller that cannot tell "nothing arrived" from "something went wrong" guesses, and it guesses
//! wrong in exactly the case that matters: an idle link reads as a broken one and a broken one as
//! idle. `ST_EMPTY` is therefore its own code, and [`OP_RX`] returns immediately instead of
//! waiting for a frame — this loop serves every client of this PD, and a driver that blocks on the
//! network blocks all of them.
//!
//! ### `OP_RX` says WHY it was empty
//!
//! `VirtioNet::rx` has two ways of handing up no frame: `0`, the used ring did not move, and
//! [`RX_RUNT`], a completion carrying nothing but the 12-byte virtio header was consumed and the
//! descriptor re-armed. Both mean "no frame was staged for you", so both are `ST_EMPTY` — a poller
//! must not read either as a fault. Which of the two it was goes into `msg[2]` ([`RSN_RUNT`]),
//! because a link whose runts climb is delivering empty completions, and a link that is genuinely
//! idle is not. The counter behind it (`NetLink::stats().refusals.rx_runt`) lives inside this PD
//! and no caller can read it; a named overflow nobody can reach is the D11 shape one level up.
//!
//! ### The cached link is dropped BEFORE every probe
//!
//! Two reasons, and the order is the second one.
//!
//! `arp_probe_rx_at` resets the transport and re-negotiates. A [`NetLink`] taken before that call
//! describes queues the device has since forgotten, so [`OP_SELF`]/[`OP_FOREIGN`] set the cached
//! link back to `None`. Reusing a stale handle would address descriptors that no longer exist, and
//! the symptom — polls that never complete — is indistinguishable from a dead device.
//!
//! The probe also carves `OFF_RXBUF` into a fresh `Owned<Driver>`, while a cached link still holds
//! an `Owned<Device>` over exactly those bytes. Dropping the link **after** the probe left two
//! owners of one buffer for the length of the call, and the only thing making that harmless was
//! that the probe happens to `reset()` first — care at the call site, which is precisely what
//! `Region::carve` exists to make unnecessary: „die Zusicherung ‚genau ein Besitzer' haengt nicht
//! an der Sorgfalt an der Aufrufstelle".
//!
//! ### The slot is wider than the link
//!
//! A slot holds `caprock_net::FRAME_MAX` = 2048 bytes, `caprock-virtio` sends at most
//! [`MAX_FRAME`] = 1514. Something staged in between fits the area and is still not an Ethernet
//! frame, so [`OP_TX`] refuses it **here**, with [`RSN_OVER_MTU`]. Handing it down instead would
//! come back as `tx_used = 0` — the same answer the driver gives when the device was merely slow,
//! and the caller would spend its time blaming the peer for a frame that never left this PD.
//!
//! ### Headers are checked, never assumed — and the refusal is not `ST_BADOP`
//!
//! A slot header that does not carry `caprock_net::MAGIC` is refused, not interpreted: that is the
//! A-4.3 rule — what cannot mean the same thing over there is rejected rather than read.
//!
//! It is answered with [`ST_REFUSED`] and a **named** reason in `msg[2]` (see [`RSN_NONE`] and its
//! neighbours), so "you sent me an op I do not know" stays distinguishable from "the area you
//! pointed me at is not written in this protocol". `ST_BADOP` is not that answer: `caprock-net`
//! fixes it as "unknown op in `msg[0]` … **distinct from a failure of a known op**", and a known op
//! over an unreadable staging area is exactly such a failure. Using it here would have made this
//! PD hold a private opinion about a word the shared crate defines — the one thing that crate
//! exists to prevent.
//!
//! ### `OP_TX` honours the sequence number
//!
//! Every slot header carries a `seq` so a reader can tell "nothing new" from "the same frame still
//! sitting there". The RX direction maintained one and the TX direction only echoed it back, so two
//! identical [`OP_TX`] calls put the same frame on the wire twice — the very case `seq` exists to
//! make distinguishable. A repeat is now a named refusal ([`RSN_REPEAT`]), never a silent resend:
//! whether the frame *arrived* is not a fact this PD can establish, so "send it again" is not a
//! decision it may take on the peer's behalf.
//!
//! `seq == 0` means **not sequenced** — the same reservation the RX direction already makes — and
//! such a send always goes ahead. A peer that never fills the field keeps working; a peer that
//! wants the guarantee starts at 1.
//!
//! The check is carried by types, not by care ([`txseq`]): [`send`] takes a `Fresh` that only
//! `TxSeq::accept` can produce, and hands back an `Unbooked` that only `TxSeq::book` can open. On
//! that path, dropping the question or dropping the bookkeeping does not compile. It does not bind
//! a *future* op that calls `VirtioNet::tx` for itself — no type in this file can — and the claim
//! is deliberately no wider than that.

#![no_std]
#![no_main]

use libcaprock::{exit, map_window, recv, reply, result, signal, Window};
use caprock_virtio::net::{
    NetLink, TxOutcome, VirtioNet, MAX_FRAME, RX_RUNT, RX_TOO_LARGE, RX_UNARMED, RX_WRONG_LINK,
};
use caprock_net::{SlotHeader, HDR_BYTES, ST_BADOP, ST_EMPTY, ST_NODEV, ST_OK, ST_TOOBIG};
use txseq::{Fresh, TxSeq, Unbooked};

const NTFN: u64 = 1;
const EP: u64 = 2;
const CFG: u64 = 3;
const BAR: u64 = 4;
const DMA: u64 = 5;
/// The shared transfer area. The loader fills this slot for **every** driver PD out of
/// `assign_driver_device`, so a missing slot 6 is a startup failure and not a configuration this
/// program should survive: a driver that answered every framing op with "no staging area" for the
/// rest of its life would be a service that cannot do its job, announcing itself as ready.
///
/// What such a failure looks like from outside — and why this PD cannot make it look like
/// anything better — is written out at [`exit_before_ready`].
const SHARED: u64 = 6;

/// Positivkontrolle: ARP mit dem Empfangspuffer in der **eigenen** Region.
pub const OP_SELF: u64 = 1;
/// Der Versuch: derselbe Ablauf, Empfangspuffer unter der in `msg[1]` genannten **fremden** IOVA.
pub const OP_FOREIGN: u64 = 2;

/// Report the negotiated MAC. Reply `msg[1]` carries the six bytes little-endian in the low 48
/// bits (byte 0 in bits 0..8).
pub const OP_MAC: u64 = caprock_net::OP_MAC;
/// Send the frame staged at `OFF_TX_DATA`. Reply `msg[1]` is 1 when the device consumed the
/// descriptor, 0 when it did not — that is `tx_used`, not "the peer got it".
pub const OP_TX: u64 = caprock_net::OP_TX;
/// Poll for one received frame and stage it at `OFF_RX_DATA`. Reply `msg[1]` is its length.
pub const OP_RX: u64 = caprock_net::OP_RX;

// The protocol numbers belong to `caprock-net`. These two are still written out because they
// predate that crate and because `dmaiso` measures them by value; the assertion keeps the two
// spellings from drifting apart. A number that exists twice and agrees only by habit is the same
// failure mode as a guard that greps for an identifier somebody renamed.
const _: () = assert!(OP_SELF == caprock_net::OP_SELF && OP_FOREIGN == caprock_net::OP_FOREIGN);

// The staging slot must be able to hold anything the link can carry. The inequality is pinned in
// this direction only: a slot larger than the MTU costs unused bytes, a slot smaller than the MTU
// silently truncates traffic that arrived intact.
const _: () = assert!(MAX_FRAME <= caprock_net::FRAME_MAX as usize);

/// The two header offsets, cast to the width this program does its window arithmetic in. The
/// values themselves stay in `caprock-net`; this is a cast, not a second copy of the layout.
const OFF_TX_HDR: u64 = caprock_net::OFF_TX_HDR as u64;
const OFF_RX_HDR: u64 = caprock_net::OFF_RX_HDR as u64;

/// **Refused before anything left this PD — what the caller staged is untouched.** The reason is
/// in `msg[2]`.
///
/// ## Why this code is here and not in `caprock-net`
///
/// The pinned vocabulary has five words, and none of them says this. `ST_BADOP` is fixed as
/// "unknown op in `msg[0]` … distinct from a failure of a known op", so a known op with an
/// unreadable slot header is precisely what it is *not*. `ST_TOOBIG` is fixed as "the named
/// overflow of the capacity **this crate** introduces", i.e. the slot size — a header claiming
/// length 0 is the opposite direction, and the one-send-buffer capacity of `caprock-virtio` is a
/// different crate's. `ST_NODEV` claims something about the device that is not true here, and
/// `ST_EMPTY` is documented as a **normal outcome**: a poller that reads "your header is not
/// written in this protocol" as "nothing arrived yet" retries forever on its own bug.
///
/// Both previous spellings were in use and both were wrong — the `RSN_*` word kept the cases apart
/// operationally, which is exactly why nobody noticed that the status word had stopped meaning
/// what its own crate says.
///
/// `caprock-net` is not editable in this change, so the code is declared here, **derived** from the
/// pinned set rather than picked: one past the highest one it defines. That is the whole reason for
/// the two assertions below — a sixth pinned code takes this value, and the build has to stop
/// rather than let two meanings share a number across a PD boundary. It belongs in `caprock-net`
/// the next time that crate is opened; until then a reader that does not know it sees an unknown
/// status, which that crate already calls **data** and requires its readers to survive.
pub const ST_REFUSED: u32 = ST_EMPTY + 1;

// The premise of the derivation, written down because the derivation is worthless without it: the
// value is "one past the highest", so `ST_EMPTY` has to *be* the highest. Renumbering anything in
// `caprock-net`, or adding a sixth code, breaks the build here instead of silently making
// `ST_REFUSED` a synonym for it.
const _: () = assert!(
    ST_EMPTY > ST_OK && ST_EMPTY > ST_BADOP && ST_EMPTY > ST_TOOBIG && ST_EMPTY > ST_NODEV
);
const _: () = assert!(
    ST_REFUSED != ST_OK
        && ST_REFUSED != ST_BADOP
        && ST_REFUSED != ST_TOOBIG
        && ST_REFUSED != ST_NODEV
        && ST_REFUSED != ST_EMPTY
);

// Named reasons, reported in `msg[2]` next to the status. The pinned status vocabulary has one
// code for "the staged length is not a length this slot can carry"; without a second field, a
// caller that staged nothing and a caller that staged 9000 bytes would get the same answer and
// could not tell which mistake it made.
//
// Most of them accompany a refusal. Three do not — [`RSN_RUNT`] and [`RSN_TX_OUTSTANDING`] name a
// fact beside a non-refusal status, and [`RSN_NONE`] names its absence. That widening is
// deliberate: the alternative is a counter inside `NetLink` that no caller can reach, which is the
// shape this round is fixing, not one to repeat.
/// No refusal and nothing to add — the status word says everything there is to say.
pub const RSN_NONE: u64 = 0;
/// The header in the shared area did not carry `MAGIC`; it was refused, not interpreted.
pub const RSN_NO_MAGIC: u64 = 1;
/// The header parsed and claims length 0 — nothing is staged, so there is nothing to send.
pub const RSN_NO_FRAME: u64 = 2;
/// The claimed length does not fit the slot. **Refused, never clamped**: a truncated frame looks
/// like a whole one to whoever reads it next.
pub const RSN_OVER_SLOT: u64 = 3;
/// The device handed back more bytes than the staging buffer holds ([`RX_TOO_LARGE`]). Distinct
/// from [`RSN_OVER_SLOT`] on purpose — this one points at the link, that one at the caller.
pub const RSN_OVER_LINK: u64 = 4;
/// The link refused because the addresses did not match the ones it was brought up on
/// ([`RX_WRONG_LINK`], [`TxOutcome::WrongLink`]). Unreachable from here — the same pair goes in on
/// every call — and answered anyway: a case that cannot happen and is silently folded into another
/// one is how the next refactoring loses it.
pub const RSN_WRONG_LINK: u64 = 5;
/// No receive descriptor is armed ([`RX_UNARMED`]), so nothing **can** land. Not the same fact as
/// `ST_EMPTY`: one says the device had no opportunity, the other that it had one and did not use
/// it.
pub const RSN_UNARMED: u64 = 6;
/// The staged frame is longer than the **link** can send. The slot holds `FRAME_MAX` = 2048 bytes,
/// but `caprock-virtio` refuses anything above [`MAX_FRAME`] = 1514 — an Ethernet frame, not a
/// buffer. Without its own reason the caller would read `tx_used = 0` and blame the peer for a
/// frame that never left this PD.
pub const RSN_OVER_MTU: u64 = 7;
/// The TX header repeats the sequence number of the frame this driver last handed to the device.
/// **Refused, never resent** — see the module doc. Nothing was written and nothing was published,
/// so the caller may stage a new frame under a new number and try again.
pub const RSN_REPEAT: u64 = 8;
/// [`OP_RX`]: a completion was consumed and it carried nothing but the 12-byte virtio header
/// ([`RX_RUNT`]). The status is still `ST_EMPTY` — no frame was staged for the caller — but the
/// used ring **did** move, which is the opposite diagnosis from an idle link.
pub const RSN_RUNT: u64 = 9;
/// [`OP_TX`]: the send buffer is still the device's from an earlier send ([`TxOutcome::Busy`]).
///
/// This is the **named overflow** of the one-send-buffer capacity `caprock-virtio` introduces, and
/// the caller is refused rather than blocked: nothing was written over the bytes the live
/// descriptor points at, the staged frame is untouched, and this loop goes straight back to `recv`
/// for its other clients. A driver that waited here would make one slow device stop the service.
pub const RSN_TX_BUSY: u64 = 10;
/// [`OP_TX`]: armed, published and kicked, and the device did not complete it within [`MAX_POLL`]
/// ([`TxOutcome::Outstanding`]). The status is `ST_OK` with `tx_used = 0`.
///
/// **Not a refusal, and the difference decides what the caller may do next**: the descriptor is the
/// device's, so the staged bytes must not be rewritten. Re-staging the same frame under a new `seq`
/// would put a second copy of it on the wire; the next [`OP_TX`] picks the completion up, or
/// answers [`RSN_TX_BUSY`].
pub const RSN_TX_OUTSTANDING: u64 = 11;
/// [`OP_TX`]: the link refused the frame's length ([`TxOutcome::BadLength`]). Unreachable from here
/// — length 0 and anything above [`MAX_FRAME`] are already refused above with their own reasons —
/// and answered anyway, for the same cause as [`RSN_WRONG_LINK`].
pub const RSN_TX_BADLEN: u64 = 12;

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

/// Copy `src` into the window at `off`, byte by byte. `None` if it does not fit — the `Window`
/// type checks every access, so a short window refuses instead of writing past its end.
///
/// Byte-wise on purpose. `Window::write_u64` is a single volatile store and therefore needs an
/// 8-byte-aligned target; `OFF_RX_DATA` happens to be 8-aligned today, but that is a property of
/// two constants in another crate, not of this copy. A correctness that follows from a size
/// relation instead of from structure disappears at the next measurement — this project has paid
/// for that shape more than once.
fn stage(w: &Window, off: u64, src: &[u8]) -> Option<()> {
    for (i, b) in src.iter().enumerate() {
        w.write_u8(off + i as u64, *b)?;
    }
    Some(())
}

/// Read and validate one slot header. `None` means "not written by a peer that speaks this
/// version" — `SlotHeader::parse` refuses anything that does not carry `MAGIC`.
///
/// No barrier is needed before this read: the bytes were published by the caller before its
/// `CALL`, and the IPC path through the kernel orders that against this PD's `RECV`.
fn read_header(w: &Window, off: u64) -> Option<SlotHeader> {
    SlotHeader::parse(w.bytes(off, HDR_BYTES as u64)?)
}

/// Publish one slot header. `None` if `caprock-net` refused to emit it, or if it does not fit the
/// window.
///
/// The detour through a stack buffer is not laziness: `SlotHeader::write` needs a `&mut [u8]`, and
/// a `Window` never hands one out — every write through it is bounds-checked individually, which is
/// exactly the property a PD that stages foreign-facing bytes wants to keep. Sixteen bytes on the
/// stack are the price of not having a raw mutable slice into shared memory in this file.
fn put_header(w: &Window, off: u64, h: &SlotHeader) -> Option<()> {
    let mut raw = [0u8; HDR_BYTES];
    if !h.write(&mut raw) {
        return None;
    }
    stage(w, off, &raw)
}

/// The staged TX payload, bounded by `caprock-net` rather than by arithmetic here. `None` when the
/// claimed length does not fit the slot.
fn tx_frame(w: &Window, len: u32) -> Option<&[u8]> {
    let (off, end) = caprock_net::tx_data_range(len)?;
    w.bytes(off as u64, (end - off) as u64)
}

/// Where a received frame of `len` bytes goes. Same source of truth as [`tx_frame`]; `None` when
/// `len` is one the slot cannot hold, and that is a refusal, never a clamp.
fn rx_offset(len: usize) -> Option<u64> {
    let n = u32::try_from(len).ok()?;
    caprock_net::rx_data_range(n).map(|(off, _)| off as u64)
}

/// Bring the link up unless it already is. `None` means the handshake failed, and the caller turns
/// that into `ST_NODEV` — a fact of its own, kept apart from "the frame did not fit".
fn up<'a>(
    net: &VirtioNet,
    link: &'a mut Option<NetLink>,
    cpu: u64,
    dev: u64,
) -> Option<&'a mut NetLink> {
    if link.is_none() {
        // SAFETY: the transport belongs to this device (resolved from the config-space page in
        // slot 3), and `[cpu, cpu + REGION_BYTES)` is the DMA region of slot 5, which this PD owns
        // alone. `dev` is that region's device view, checked non-zero at startup.
        *link = unsafe { net.link_up(cpu, dev) };
    }
    link.as_mut()
}

/// **The TX sequence state — in a module of its own, so that the witness is a real one.**
///
/// `rustc` checks **constructibility**, not non-forwarding, and a private field is private to its
/// *module*: a witness declared beside its user is constructible beside its user, and then it
/// proves nothing. This repository has written that sentence down after paying for it once. Hence
/// a module boundary — inside it, [`TxSeq::accept`] is the only thing that makes a [`Fresh`] and
/// [`TxSeq::book`] the only thing that opens an [`Unbooked`]; outside it, nothing does.
///
/// What that buys is one property, and it is the one that was missing: **no frame reaches the
/// device without the sequence number having been asked about, and no outcome reaches the reply
/// without the number having been booked.** Neither is a rule a reader has to remember; both are
/// compile errors.
mod txseq {
    use caprock_virtio::net::TxOutcome;

    /// Permission to send under one sequence number. The number is inside, and nothing outside
    /// this module can read it, write it, or invent one.
    pub struct Fresh(u32);

    /// What a send did, **not yet booked**.
    ///
    /// `#[must_use]`, and [`TxSeq::book`] is the only way to get the [`TxOutcome`] out of it: a
    /// path that only wanted the answer cannot skip the bookkeeping and leave the next repeat
    /// undetected — which is exactly how the check was missing in the first place.
    #[must_use]
    pub struct Unbooked(u32, TxOutcome);

    impl Fresh {
        /// Pair the permission with what the send did. Consumes the permission, so one `accept`
        /// cannot cover two sends.
        pub fn done(self, outcome: TxOutcome) -> Unbooked {
            Unbooked(self.0, outcome)
        }
    }

    /// The sequence number of the frame this driver last **handed to the device**.
    ///
    /// Handed to, not confirmed by: those are two facts, and the second one is the `tx_used` word
    /// of the reply. A frame whose descriptor is still outstanding is in the device's hands —
    /// sending it again would put a second copy of the same bytes on the wire.
    pub struct TxSeq(Option<u32>);

    /// Is `seq` a number this driver has not already sent under?
    const fn is_new(last: Option<u32>, seq: u32) -> bool {
        // 0 is reserved: it means the peer does not sequence its sends, exactly as it means
        // "nothing has ever been staged here" in the RX direction. Without this line the first
        // `seq = 0` would make every later one a repeat, and a peer that simply never fills the
        // field could send exactly one frame in its life.
        if seq == 0 {
            return true;
        }
        match last {
            Some(prev) => prev != seq,
            None => true,
        }
    }

    /// Does a send under `seq` consume the number?
    const fn consumes(seq: u32, refused: bool) -> bool {
        // A refused send wrote nothing and published nothing — the frame never reached the device,
        // and the peer must be able to retry it under the same number. Booking it would turn one
        // `Busy` into a permanent refusal of that frame, which is the fix being worse than the
        // fault.
        seq != 0 && !refused
    }

    // **The two decisions, pinned in the build.** Both functions are pure, so their cases are
    // checkable without a machine and without a test harness this `no_main` binary does not have.
    // The second line is the fault this module exists for: a sequence number that was read, echoed
    // and never compared against anything.
    const _: () = assert!(is_new(None, 7));
    const _: () = assert!(!is_new(Some(7), 7));
    const _: () = assert!(is_new(Some(7), 8));
    const _: () = assert!(is_new(Some(7), 0));
    const _: () = assert!(is_new(None, 0));
    const _: () = assert!(consumes(7, false));
    const _: () = assert!(!consumes(7, true));
    const _: () = assert!(!consumes(0, false));

    impl TxSeq {
        /// Nothing sent yet.
        pub const fn new() -> Self {
            TxSeq(None)
        }

        /// `None` when `seq` repeats the last booked one — the caller turns that into a named
        /// refusal instead of sending the same frame a second time.
        ///
        /// Nothing is recorded here. The number is booked in [`Self::book`], **after** the outcome
        /// is known, because only a send that actually reached the device may consume it.
        pub fn accept(&mut self, seq: u32) -> Option<Fresh> {
            if is_new(self.0, seq) {
                Some(Fresh(seq))
            } else {
                None
            }
        }

        /// Book the send and hand its outcome on.
        pub fn book(&mut self, u: Unbooked) -> TxOutcome {
            let Unbooked(seq, outcome) = u;
            if consumes(seq, outcome.refused()) {
                self.0 = Some(seq);
            }
            outcome
        }
    }
}

/// **The one place in this program that hands a frame to the device.**
///
/// It takes [`Fresh`] by value and returns [`Unbooked`], and that is the mechanism rather than
/// decoration: neither type can be built outside [`txseq`], so a send that never asked about the
/// sequence number — or an outcome that never got booked — is a compile error instead of a frame
/// on the wire twice. It cannot stop a *future* op from calling [`VirtioNet::tx`] directly; nothing
/// in this file can. It does stop this path from losing the check the way it was lost the first
/// time, which is the failure that actually happened.
///
/// # Safety
/// As [`VirtioNet::tx`]: the transport's MMIO addresses must belong to this device, and `l` must be
/// a link brought up on exactly `(cpu, dev)`.
unsafe fn send(
    net: &VirtioNet,
    l: &mut NetLink,
    cpu: u64,
    dev: u64,
    frame: &[u8],
    fresh: Fresh,
) -> Unbooked {
    // SAFETY: forwarded unchanged from this function's own contract — the caller resolved the
    // transport from the config-space page in slot 3 and brought `l` up on exactly this pair.
    // `frame` points into the shared area, which is plain RAM this PD has mapped; `tx` copies out
    // of it and keeps no reference to it.
    fresh.done(unsafe { net.tx(l, cpu, dev, frame, MAX_POLL) })
}

/// [`OP_TX`]: send whatever the peer staged in the TX slot.
///
/// The TX header is read and never written. It belongs to the other side; writing a status back
/// into it would give up the one property the two-slot layout exists for — that a frame staged by
/// the stack cannot be overwritten by traffic coming the other way.
///
/// `msg[3]` echoes the staged sequence number, `msg[1]` is `tx_used`, and `msg[2]` names which of
/// the five outcomes of [`VirtioNet::tx`] happened. Five outcomes, five answers: "the device is
/// still chewing on the last one" and "the frame you staged is not one I can send" tell the caller
/// to do opposite things.
fn op_tx(
    net: &VirtioNet,
    link: &mut Option<NetLink>,
    shared: &Window,
    cpu: u64,
    dev: u64,
    tx_seq: &mut TxSeq,
) -> [u64; 4] {
    let Some(hdr) = read_header(shared, OFF_TX_HDR) else {
        // Not written by a peer that speaks this version. `msg[3]` is 0 because no sequence number
        // was read — not because the peer staged one and it was 0.
        return [ST_REFUSED as u64, 0, RSN_NO_MAGIC, 0];
    };
    let seq = u64::from(hdr.seq);
    if hdr.len == 0 {
        // A well-formed header staging nothing. **Not `ST_TOOBIG`**: that code is the named
        // overflow of the capacity `caprock-net` introduces, and this is the opposite direction.
        return [ST_REFUSED as u64, 0, RSN_NO_FRAME, seq];
    }
    let Some(frame) = tx_frame(shared, hdr.len) else {
        return [ST_TOOBIG as u64, 0, RSN_OVER_SLOT, seq];
    };
    if frame.len() > MAX_FRAME {
        // Fits the slot, does not fit an Ethernet frame. `VirtioNet::tx` would refuse it too, with
        // `TxOutcome::BadLength` — but it is refused **here** so it never reaches the ring, and so
        // the caller reads a reason that points at the frame rather than at the link.
        return [ST_TOOBIG as u64, 0, RSN_OVER_MTU, seq];
    }
    let Some(l) = up(net, link, cpu, dev) else {
        return [ST_NODEV as u64, 0, RSN_NONE, seq];
    };
    // **The sequence number is consulted here** — after the link is up and immediately before the
    // send. A call that failed at `up` never reached the device, and booking its number would turn
    // a transient `ST_NODEV` into a permanent refusal of that one frame.
    let Some(fresh) = tx_seq.accept(hdr.seq) else {
        // Already handed to the device under this number. Refused, not resent: `tx_used` on the
        // earlier call already said whether the device took it, and whether it ARRIVED is not a
        // fact this PD can establish — so "send it again" is not a decision it may take on the
        // peer's behalf. Nothing was written and nothing was published; a new frame under a new
        // number goes through.
        return [ST_REFUSED as u64, 0, RSN_REPEAT, seq];
    };
    // SAFETY: as in `up` — transport, DMA region and device view all belong to this PD, and `l`
    // was brought up on exactly this `(cpu, dev)` pair.
    let outcome = tx_seq.book(unsafe { send(net, l, cpu, dev, frame, fresh) });
    // `Sent` says the device took the descriptor, NOT that anything arrived anywhere. The two are
    // reported separately everywhere in this driver for the same reason `rx_used` and `arp_reply`
    // are separate fields of `NetResult`.
    match outcome {
        TxOutcome::Sent => [ST_OK as u64, 1, RSN_NONE, seq],
        // Published and kicked, not confirmed. `ST_OK` because the op did what it says, `tx_used`
        // 0 because the device has not answered — and the reason word so the caller knows the
        // bytes are the device's and must not be rewritten.
        TxOutcome::Outstanding => [ST_OK as u64, 0, RSN_TX_OUTSTANDING, seq],
        // The named overflow of the one-send-buffer capacity. Refused **without blocking**: this
        // loop serves every client of this PD, and waiting here would let one slow device stop the
        // service (D11, in this program's own vocabulary).
        TxOutcome::Busy => [ST_REFUSED as u64, 0, RSN_TX_BUSY, seq],
        // The two the link can refuse and this function cannot produce — the length is bounded
        // above and the address pair is the same one `up` brought the link up on. Answered anyway:
        // a case that cannot happen and is silently folded into another one is how the next
        // refactoring loses it.
        TxOutcome::BadLength => [ST_REFUSED as u64, 0, RSN_TX_BADLEN, seq],
        TxOutcome::WrongLink => [ST_REFUSED as u64, 0, RSN_WRONG_LINK, seq],
    }
}

/// [`OP_RX`]: poll once and stage what came in.
///
/// `msg[3]` of **every** reply is the sequence number of the frame currently in the RX slot, `0`
/// when nothing has ever been staged there. An empty poll used to answer `0` regardless, which
/// reads as "this slot has never held anything" — false as soon as one frame has been staged, and
/// the precise claim `seq` exists to make.
fn op_rx(
    net: &VirtioNet,
    link: &mut Option<NetLink>,
    shared: &Window,
    cpu: u64,
    dev: u64,
    buf: &mut [u8],
    seq: &mut u32,
) -> [u64; 4] {
    // What is in the slot right now, read before anything can change it.
    let staged = u64::from(*seq);
    let Some(l) = up(net, link, cpu, dev) else {
        return [ST_NODEV as u64, 0, RSN_NONE, staged];
    };
    // SAFETY: as in `up`. `buf` is this function's own storage; `rx` only writes into it.
    let n = unsafe { net.rx(l, cpu, dev, buf) };
    match n {
        // Nothing waiting — and the RX slot is deliberately left ALONE. Its header describes the
        // last frame that was staged there, not the outcome of this poll; overwriting it with an
        // empty one would destroy a frame a slower reader has not picked up yet.
        0 => return [ST_EMPTY as u64, 0, RSN_NONE, staged],
        // A completion carrying nothing but the 12-byte virtio header. Also `ST_EMPTY` — no frame
        // was staged for the caller, and a poller must not read either case as a fault — but the
        // used ring **did** move, which is the opposite diagnosis from an idle link. Until
        // `RX_RUNT` existed these two were one answer, and the counter that told them apart sits
        // inside `NetLink` where no caller can read it.
        RX_RUNT => return [ST_EMPTY as u64, 0, RSN_RUNT, staged],
        // A frame arrived and was dropped because it did not fit. Refused, not clamped, and
        // charged to the link rather than to the caller: the caller did nothing wrong here.
        RX_TOO_LARGE => return [ST_TOOBIG as u64, 0, RSN_OVER_LINK, staged],
        // Both of these say the link is no longer usable as it stands, so it is dropped and the
        // next poll brings it up again. Leaving it cached would repeat the same refusal forever,
        // and a queue that never re-arms reads from outside as a peer that stopped talking.
        RX_WRONG_LINK => {
            *link = None;
            return [ST_NODEV as u64, 0, RSN_WRONG_LINK, staged];
        }
        RX_UNARMED => {
            *link = None;
            return [ST_NODEV as u64, 0, RSN_UNARMED, staged];
        }
        _ => {}
    }
    let Some(frame) = buf.get(..n) else {
        // Unreachable — `rx` reports `RX_TOO_LARGE` rather than a length past `out`. Answered
        // anyway, because a slice index that is only in range by someone else's invariant is a
        // panic, and `panic = abort` here means the driver disappears.
        return [ST_TOOBIG as u64, 0, RSN_OVER_LINK, staged];
    };
    let Some(off) = rx_offset(n) else {
        return [ST_TOOBIG as u64, 0, RSN_OVER_SLOT, staged];
    };
    if stage(shared, off, frame).is_none() {
        return [ST_TOOBIG as u64, 0, RSN_OVER_SLOT, staged];
    }
    // Payload first, header second — the header is what a reader validates, so publishing it over
    // bytes that are not there yet would let a reader see `MAGIC` and a length across stale data.
    // The barrier is `device_fence` although the ordering here is between two PDs over ordinary
    // RAM, where `dmb ish` would already suffice: it is the only barrier this file defines, and it
    // is strictly stronger. Strengthening a barrier is always allowed; weakening one is the trap.
    device_fence();
    let next = {
        let mut s = seq.wrapping_add(1);
        if s == 0 {
            // 0 stays reserved for "nothing has ever been staged here", so a reader can tell an
            // empty slot from one that wrapped around to the same number it saw before.
            s = 1;
        }
        s
    };
    // `MAGIC` is not passed in — it is a condition of the type, and `write` always emits it. The
    // only way to produce a header this driver's own reader would reject is a length `write`
    // refuses, and that path is already covered above.
    let h = SlotHeader { len: n as u32, status: ST_OK, seq: next };
    if put_header(shared, OFF_RX_HDR, &h).is_none() {
        // The header did not go out, so the slot still describes the PREVIOUS frame — and the
        // counter is not advanced either. A number handed out for a header that was never written
        // would leave a reader waiting for a frame that is not there, which is the same mistake as
        // reporting a length nobody staged.
        return [ST_TOOBIG as u64, 0, RSN_OVER_SLOT, staged];
    }
    *seq = next;
    [ST_OK as u64, n as u64, RSN_NONE, u64::from(next)]
}

/// **End without ever signalling readiness.** Every startup failure in [`run`] goes through here.
///
/// The function exists for its comment, and the comment exists because the failure it describes is
/// **silent**. A PD that dies here leaves the kernel's `drv_service_step` waiting on a notification
/// that never comes; the run walks into the watchdog, and the report prints
/// `dmaiso : SKIP (kein zweiter Treiber ...)` — a line that points at the manifest when the cause
/// was, say, a shared window one byte too small. Slot 6 is provisioned today, so nothing here fires
/// today. A silent exit is a failure shape this repository has already paid for, so it is written
/// down rather than left to be rediscovered.
///
/// **Why this PD cannot say anything better — checked against the kernel, not assumed:**
///
/// * `exit()` carries no code. `SYS_EXIT` takes none, so there is no value to put in one.
/// * A badge is a property of the **cap**, not of the message: `SYS_SIGNAL` ORs the badge of the
///   cap it was invoked on into `pending` and ignores the message word (see
///   `system::install_notification_cap_badged`). This PD holds exactly one notification cap and the
///   loader badged it `DRIVER_NTFN_BADGE`. There is no second badge available to mean anything
///   else.
/// * Signalling the one badge it has would be a lie **and would not even change the report**:
///   `drv_service_step` reads that badge as "the device is resolved", installs an endpoint cap and
///   spawns its client, whose `CALL` then parks on an endpoint with no receiver. `DMAISO_STATE`
///   stays 0 either way, so the same `SKIP` line is printed — the wait merely moves from step 3 to
///   step 4, now behind a green ready-signal. That is worse, not better.
///
/// So it dies quietly. What would make it audible is kernel-side and not this program's to add: a
/// reason word on `SYS_EXIT`, or a second badge meaning "started and gave up".
fn exit_before_ready() -> ! {
    exit()
}

libcaprock::entry!(run);

fn run(_arg: usize) -> ! {
    // 1. Fenster mappen. Jeder Fehlschlag endet **ohne** Bereit-Meldung: ein Treiber, der ohne
    //    Gerät in `recv` ginge, nähme Anfragen an, die er nicht beantworten kann. Was das von
    //    aussen aussieht — und warum diese PD es nicht besser aussehen lassen kann — steht bei
    //    [`exit_before_ready`].
    let Some(cfg_win) = map_window(CFG) else { exit_before_ready() };
    let cfg = cfg_win.base();
    if map_window(BAR).is_none() {
        exit_before_ready();
    }
    let Some(dma) = map_window(DMA) else { exit_before_ready() };
    let (dma_cpu, dma_len, dma_dev) = (dma.base(), dma.len(), dma.iova());
    // Die Gerätesicht MUSS eine eigene Achse sein — eine identity-Abbildung soll es nicht geben.
    if dma_dev == 0 || dma_len < caprock_virtio::net::REGION_BYTES {
        exit_before_ready();
    }
    // The shared transfer area, same failure discipline as the windows above. Its size is checked
    // against the layout's own bound rather than against a number repeated here: two places
    // computing the same limit are two memories for one fact, and the second one goes stale in
    // silence. The other half of that bound — that the layout still fits the 8 KiB the kernel hands
    // out — is a compile-time assert in `caprock-net` and is deliberately not repeated here.
    let Some(shared) = map_window(SHARED) else { exit_before_ready() };
    if shared.len() < caprock_net::shared_bytes_needed() as u64 {
        exit_before_ready();
    }

    // 2. Das eigene Gerät auflösen — auf der eigenen Seite, ohne den Kernel.
    // SAFETY: `cfg` ist die gemappte Konfigurationsraum-Seite genau dieser Funktion; das darin
    // genannte BAR ist über Slot 4 in dieser VSpace erreichbar.
    let Some(transport) = (unsafe { caprock_virtio::probe_ecam(cfg, device_fence) }) else {
        exit_before_ready();
    };
    let net = VirtioNet::from_transport(transport);

    // 3. „Ich bin bereit." Erst **nach** dem Auflösen.
    signal(NTFN, 0);

    // Link state survives across calls: bringing the queues up costs a full handshake, and doing
    // it per request would make every `OP_RX` throw away the descriptor the previous one armed.
    let mut link: Option<NetLink> = None;
    // Per-direction sequence number for the RX slot. Monotonic, so a reader can tell "nothing new"
    // from "the same frame again" — two different facts about an unchanged slot.
    let mut rx_seq: u32 = 0;
    // The other direction, and it is a **different** kind of state: the RX number is one this
    // driver hands out, the TX number is one the peer hands in and this driver has to compare
    // against something. Without it two identical `OP_TX` calls sent the same frame twice — the
    // exact case `seq` exists to make distinguishable, in the direction that had no memory. See
    // [`txseq`].
    let mut tx_seq = TxSeq::new();
    // The staging buffer lives outside the loop so this service's stack footprint is the same on
    // every path instead of depending on which op ran last: 2 KiB against the 16 KiB EL0 stack a
    // loaded program gets (`LOADED_STACK_BYTES`).
    let mut rx_buf = [0u8; caprock_net::FRAME_MAX as usize];

    // 4. Dienstschleife.
    loop {
        let m = recv(EP);
        if m.result != result::OK {
            exit(); // Endpoint stillgelegt/entzogen -> geordnet enden
        }
        let op = m.msg[0];
        // Every arm yields a reply word and the reply is sent once, below. Written as an
        // expression on purpose: an arm that fell through without answering would leave the caller
        // parked in `CALL` forever, and a blocked caller is indistinguishable from a dead device
        // seen from outside. The compiler now enforces what a `continue` could quietly skip.
        let answer: [u64; 4] = match op {
            OP_SELF | OP_FOREIGN => {
                // **Before the probe, not after.** The probe resets the transport and
                // re-negotiates, so any cached link names queues the device has forgotten — that
                // was always the reason, and it did not need the ordering. This does: the probe
                // carves `OFF_RXBUF` into a fresh `Owned<Driver>` while a cached link still holds
                // an `Owned<Device>` over exactly those bytes, so dropping it afterwards left two
                // owners of one buffer for the length of the call. Harmless only because the probe
                // happens to `reset()` first — care at the call site, which is what
                // `Region::carve` exists to make unnecessary. Moving the line costs nothing and
                // removes the overlap instead of relying on it being survivable.
                link = None;
                // Bei `OP_SELF` die eigene Pufferadresse, bei `OP_FOREIGN` die genannte fremde.
                let rx_dev = if op == OP_SELF {
                    dma_dev + caprock_virtio::net::OFF_RXBUF
                } else {
                    m.msg[1]
                };
                // SAFETY: der Transport gehört zu diesem Gerät, `[dma_cpu, dma_cpu + REGION_BYTES)`
                // gehört dieser PD allein. `rx_dev` darf fremd sein — genau das ist die Messung
                // (s. Moduldoku).
                let r = unsafe {
                    net.arp_probe_rx_at(dma_cpu, dma_dev, rx_dev, SRC_IP, DST_IP, MAX_POLL)
                };
                // Einzeln zurückgeben, nicht als Sammel-`bool`: „gesendet, aber nichts empfangen"
                // und „gar nicht erst gesendet" sind verschiedene Befunde, und nur der erste ist
                // die Aussage, um die es hier geht.
                [
                    u64::from(r.features_ok),
                    u64::from(r.tx_used),
                    u64::from(r.rx_used),
                    u64::from(r.arp_reply),
                ]
            }
            OP_MAC => {
                // Deliberately does NOT bring the link up. `VirtioNet::mac` checks the offered
                // feature bit and the device-config capability itself, so it needs no queues — and
                // an op that merely *reports* a property must not reset the device as a side
                // effect. It would throw away the descriptor an earlier `OP_RX` armed, and the
                // frame that was already sitting in it would vanish because somebody asked for
                // their own address.
                //
                // SAFETY: the transport was resolved from the config-space page in slot 3 and its
                // BAR is mapped through slot 4; `mac` only reads device config space.
                match unsafe { net.mac() } {
                    // Packed by `caprock-net`, not here. Both ends would otherwise agree on the
                    // six bytes and disagree about which end of the word byte 0 sits in — and a
                    // reversed MAC is a well-formed value, which is why that mistake survives
                    // review.
                    Some(mac) => [ST_OK as u64, caprock_net::mac_to_u64(mac), RSN_NONE, 0],
                    // `VIRTIO_NET_F_MAC` was not offered, or there is no device-config capability.
                    // Answering six zero bytes instead would send that address out on the wire and
                    // make a missing feature look like a silent peer.
                    None => [ST_NODEV as u64, 0, RSN_NONE, 0],
                }
            }
            OP_TX => op_tx(&net, &mut link, &shared, dma_cpu, dma_dev, &mut tx_seq),
            OP_RX => op_rx(
                &net,
                &mut link,
                &shared,
                dma_cpu,
                dma_dev,
                &mut rx_buf,
                &mut rx_seq,
            ),
            // Unknown op. Answered with the protocol's own vocabulary rather than with a sentinel
            // of its own, so a caller needs one table to read every reply this PD can produce.
            // Nothing greps for the previous `u64::MAX` — checked against `kernel/src` and both
            // QEMU suites before changing it.
            _ => [ST_BADOP as u64, 0, RSN_NONE, 0],
        };
        reply(EP, answer);
    }
}
