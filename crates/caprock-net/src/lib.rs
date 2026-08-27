//! **The wire protocol between the network stack and the virtio-net driver — kernel-free, without
//! `unsafe`, without dependencies.**
//!
//! Like `caprock-part` and `caprock-fat`, this crate depends on **nothing**, and unlike them it
//! parses nothing from a disk: it holds no device access, no syscalls, no I/O at all. It is the
//! shared vocabulary of the programs that must not disagree — the driver PD, the stack PD, and the
//! kernel-side probe that drives both. Each of them links this crate, so none of them can hold a
//! private opinion about where a frame begins or what a status word means. Two definitions of one
//! rule are a crack; this repository has already paid for one when two suites set up the same
//! device differently.
//!
//! Because it is pure arithmetic over byte slices, it is checkable on the HOST in seconds:
//!
//! ```text
//! rustc --test --edition 2021 -O crates/caprock-net/src/lib.rs -o /tmp/t && /tmp/t
//! ```
//!
//! ## Two slots, not one
//!
//! The shared area carries **two independent slots** — [`OFF_TX_HDR`] for stack -> driver and
//! [`OFF_RX_HDR`] for driver -> stack — although one slot plus a direction flag would have been
//! smaller. With one slot, a frame the stack has staged for sending is destroyed the moment the
//! driver stages a received frame into the same bytes, and the loss is **silent**: both writers are
//! legitimate, both write a well-formed header, and nothing in the area records that a frame
//! existed. Two slots make the collision unrepresentable instead of unlikely.
//!
//! They are laid out **adjacent**: `OFF_TX_DATA + FRAME_MAX == OFF_RX_HDR`. That is not packing for
//! its own sake — a TX write that runs one byte past its slot lands in the RX **header**, where the
//! magic check refuses it, rather than in RX payload, where it would read as a frame.
//!
//! ## Why [`FRAME_MAX`] is 2048
//!
//! An Ethernet frame with the virtio-net header in front of it fits well under 1600 bytes; 2048 is
//! the next power of two above that and leaves room for a jumbo-ish frame without making the two
//! slots exceed the 8 KiB the kernel hands out (`kernel/src/system.rs`, `SHARED_BYTES = 8 * 1024`).
//! The number that matters is not 2048 but that it is a **named bound with a named overflow**:
//! a frame that does not fit is answered with [`ST_TOOBIG`], never truncated. A silent truncation
//! is the failure mode this repository has paid for repeatedly.
//!
//! ## Why every header carries a sequence number
//!
//! Without [`SlotHeader::seq`], a reader that polls cannot tell "nothing new arrived" from "the
//! same frame is still sitting there". Both look like a valid header with the same length and the
//! same status — the payload bytes are identical too, because they are literally the same bytes.
//! The counter is monotonic **per direction**, so the reader compares it against the last value it
//! consumed and gets one answer instead of a guess. This is the same distinction as `rx_used`
//! against "data actually arrived": that the device acted is not that something new is there.
//!
//! ## Why [`ST_EMPTY`] is its own code
//!
//! An [`OP_RX`] poll that finds nothing waiting is a **normal outcome**, not a failure. Folded into
//! an error code it would make a healthy idle link indistinguishable from a dead one, and a caller
//! that cannot tell them apart cannot poll: it must either treat every empty poll as a fault or
//! treat every fault as an empty poll. Two separate facts get two separate values.
//!
//! ## The magic is checked, never assumed
//!
//! [`SlotHeader::parse`] refuses a header whose first word is not [`MAGIC`], and it refuses one
//! whose length the slot cannot hold. Both refusals mean the same thing — the bytes over there were
//! not written by a peer that speaks this version — and neither is a small error to clamp. That is
//! the A-4.3 rule: what cannot mean the same thing on the other side is **rejected, not read**.
//!
//! ## The one exception to the reply convention, written down so it cannot be lost
//!
//! [`OP_SELF`] and [`OP_FOREIGN`] predate this crate. They are what the suite line `dmaiso : ALL
//! PASS` measures (A-5.4), and their reply is **not** a status word: it is four separate flags
//! (`features_ok`, `tx_used`, `rx_used`, `arp_reply`), deliberately kept apart because "sent but
//! received nothing" and "never sent at all" are different findings. The `ST_*` convention below
//! applies to [`OP_MAC`], [`OP_TX`] and [`OP_RX`] only. Anyone tempted to make the two old ops
//! "consistent" would be changing the measurement, not the formatting.
//!
//! ## What this crate does NOT do
//!
//! Touch a device, issue a syscall, allocate, or know what an Ethernet frame contains. It answers
//! "where do the bytes go and what does the word mean", and nothing else — every question it does
//! not answer is one that cannot be answered wrongly in three places at once.

#![no_std]
#![forbid(unsafe_code)]

// The crate is `no_std` (it is linked into PDs that have no operating system under them). The
// protocol is pure byte arithmetic and therefore checkable on the host -- the test harness needs
// `std` for that, and only for that.
#[cfg(test)]
extern crate std;

// ------------------------------------------------------------------------------------------------
// Operations, in `msg[0]` of a CALL to the driver PD's endpoint.
//
// `u64` and not something narrower: these values live in an IPC message word, and the existing
// driver already declares `pub const OP_SELF: u64 = 1`. A second width for the same number would be
// a cast at every call site, and a cast is where a value quietly changes meaning.
// ------------------------------------------------------------------------------------------------

/// **Positive control**: an ARP transaction with the receive buffer in the driver's OWN region.
///
/// Pre-existing (A-5.4). Without it, "the foreign access did not arrive" would only say that
/// nothing ran at all. Its value and its four-flag reply are frozen — see the module doc.
pub const OP_SELF: u64 = 1;

/// The attempt: the same transaction, with the receive buffer under a FOREIGN IOVA named in
/// `msg[1]`. Exactly one address moves.
///
/// Pre-existing (A-5.4), frozen for the same reason as [`OP_SELF`].
pub const OP_FOREIGN: u64 = 2;

/// Report the negotiated MAC. Reply: `msg[1]` = the six bytes packed by [`mac_to_u64`].
///
/// The stack cannot derive it — the MAC is a property of the device, and the device is behind the
/// driver PD's isolation boundary. Asking is the only honest way to learn it.
pub const OP_MAC: u64 = 3;

/// Send the frame staged in the TX slot. Reply: `msg[1]` = 1 if the device consumed the descriptor.
///
/// "The device consumed the descriptor" is deliberately the only claim: whether the frame reached
/// anything is not observable from here, and a reply that pretended otherwise would be the
/// `rx_used` mistake in the other direction.
pub const OP_TX: u64 = 4;

/// Poll for one received frame and stage it in the RX slot. Reply: `msg[1]` = length in bytes.
///
/// Finding nothing is [`ST_EMPTY`] with length 0 — an outcome, not an error.
pub const OP_RX: u64 = 5;

// ------------------------------------------------------------------------------------------------
// Status codes.
//
// `u32` and not `u64`, although the reply carries them in a message word: the same code also lives
// in a slot header, where the field is 32 bits wide. Of the two conversions only one is lossless,
// so the narrower type is the primary one and the wire path widens (`u64::from(ST_OK)`). Declaring
// them as `u64` would put a truncating `as u32` on every header write -- exactly the place where a
// status would silently become a different status.
// ------------------------------------------------------------------------------------------------

/// The operation did what it says.
pub const ST_OK: u32 = 0;
/// Unknown op in `msg[0]`. Distinct from a failure of a known op: one is a peer that speaks a
/// different protocol, the other is a peer that speaks this one and did not get what it wanted.
pub const ST_BADOP: u32 = 1;
/// The frame is longer than [`FRAME_MAX`], or longer than the slot can stage.
///
/// **This is the named overflow of the capacity this crate introduces.** The caller is refused, not
/// truncated and not left blocked: a bound without a name is not a bound, it is a hole.
pub const ST_TOOBIG: u32 = 2;
/// The device could not be brought up, or the link is down. Nothing was attempted.
pub const ST_NODEV: u32 = 3;
/// [`OP_RX`]: nothing was waiting. **A normal outcome, not an error** — see the module doc.
pub const ST_EMPTY: u32 = 4;

// ------------------------------------------------------------------------------------------------
// The shared transfer area.
// ------------------------------------------------------------------------------------------------

/// `"NET1"` in ASCII, read as a little-endian `u32`.
///
/// It carries the version, not just the identity: a future layout gets `NET2` and is refused here
/// rather than half-understood. A header that does not carry this word was not written by a peer
/// that speaks this protocol, and the only correct thing to do with it is nothing.
pub const MAGIC: u32 = 0x4E45_5431;

/// Size of a slot header. Four little-endian `u32` fields: magic, len, status, seq.
pub const HDR_BYTES: usize = 16;

/// Header of the stack -> driver slot.
pub const OFF_TX_HDR: usize = 0x0000;
/// Payload of the stack -> driver slot.
pub const OFF_TX_DATA: usize = 0x0010;
/// Header of the driver -> stack slot.
pub const OFF_RX_HDR: usize = 0x0810;
/// Payload of the driver -> stack slot.
pub const OFF_RX_DATA: usize = 0x0820;

/// Payload bytes a single slot can stage. See the module doc for why it is 2048.
pub const FRAME_MAX: u32 = 2048;

/// Size of the area the kernel hands both PDs (`kernel/src/system.rs`, `SHARED_BYTES = 8 * 1024`).
///
/// Named here so [`shared_bytes_needed`] can be held against it **at compile time**, on the side
/// that defines the layout. A layout that outgrows the area must break the build of this crate,
/// not produce a fault in a PD at boot.
pub const SHARED_BYTES: usize = 8 * 1024;

// The header sits immediately in front of its payload, in both directions. Written as an assertion
// rather than as a comment because the four offsets are four independent constants: nothing else
// stops someone from moving one and leaving the others.
const _: () = assert!(OFF_TX_DATA - OFF_TX_HDR == HDR_BYTES);
const _: () = assert!(OFF_RX_DATA - OFF_RX_HDR == HDR_BYTES);

// The slots are adjacent, so a TX overrun lands in the RX HEADER, where the magic check refuses it.
// One byte further apart and the same overrun would land in nothing; one byte closer and it would
// land in RX payload, which reads as a frame.
const _: () = assert!(OFF_TX_DATA + FRAME_MAX as usize == OFF_RX_HDR);

// The whole layout fits the area the kernel actually hands out.
const _: () = assert!(shared_bytes_needed() <= SHARED_BYTES);

/// How many bytes of the shared area the layout occupies: `OFF_RX_DATA + FRAME_MAX` = 4128.
///
/// A function and not a literal, so the answer moves with the constants instead of next to them.
pub const fn shared_bytes_needed() -> usize {
    OFF_RESULT + RESULT_BYTES
}

// ------------------------------------------------------------------------------------------------
// The result block -- how the stack PD tells the KERNEL what happened
//
// The kernel cannot ask the stack. `SYS_SIGNAL` ORs the badge of the CAP into `pending` and the
// message word plays no role, so a notification carries "I ran" and nothing else. Numbers therefore
// travel the same way the file system PD's do: written into the shared area, read out of it by the
// kernel through `driver_shared_region` (`kernel/src/arch/x86_64/bringup.rs` does exactly this for
// `fs` at offset 4096 of the block service's area).
//
// **It sits above both frame slots, not at 4096.** `fs` uses 4096 because its area carries no frame
// slots; here 4096 is inside TX payload. The layout assert below is what keeps that from being a
// thing someone has to remember.
//
// **Why the kernel's reading of this is NOT the acceptance.** These are the stack's own numbers --
// a writer confirming its own result. They go into the report so a failure can be read at all
// (a run that says "0 bytes, state=listening" points somewhere completely different from one that
// says "48 bytes, state=closed"), and the judgement is made by `tools/checknet.py` on the host,
// against the frames that actually crossed the link. Both must agree; where they disagree, the
// wire wins and the disagreement is the finding.
// ------------------------------------------------------------------------------------------------

/// Offset of the result block. Above both frame slots -- see the module note.
pub const OFF_RESULT: usize = 0x1800;
/// Bytes of the result block: [`RESULT_WORDS`] little-endian `u64`.
pub const RESULT_BYTES: usize = RESULT_WORDS * 8;
/// How many `u64` the result block carries.
pub const RESULT_WORDS: usize = 16;
/// Magic of the result block ("NETR"), distinct from [`MAGIC`].
///
/// A block that does not carry it has **not been written by a stack that speaks this version** --
/// and, more importantly on the very first boot, has not been written at all. Zeroed memory would
/// otherwise decode as a perfectly well-formed result reporting that nothing happened, which is
/// indistinguishable from a stack that ran and achieved nothing. The kernel must be able to tell
/// "no stack" from "a stack that failed"; those point at different files.
pub const RESULT_MAGIC: u64 = 0x4E45_5452;

/// Word indices in the result block. `W_MAGIC` is word 0 and is written **last**.
pub mod result_word {
    /// [`super::RESULT_MAGIC`] -- written last, so a half-written block never decodes.
    pub const MAGIC: usize = 0;
    /// Connection state: see [`state`](super::state).
    pub const STATE: usize = 1;
    /// Bytes the stack received from its peer.
    pub const RX_BYTES: usize = 2;
    /// Bytes the stack sent to its peer.
    pub const TX_BYTES: usize = 3;
    /// `1` if the received bytes matched the expected pattern, else `0`.
    pub const PATTERN_OK: usize = 4;
    /// Poll cycles executed. **A stack that "initialised" is not a stack that works** -- this is a
    /// number only a running poll loop can produce.
    pub const POLLS: usize = 5;
    /// Frames handed to the driver.
    pub const FRAMES_TX: usize = 6;
    /// Frames taken from the driver.
    pub const FRAMES_RX: usize = 7;
    /// Last non-OK status the driver replied, or `0`.
    pub const LAST_DRIVER_STATUS: usize = 8;
    /// Last refusal reason the driver replied, or `0`.
    pub const LAST_DRIVER_REASON: usize = 9;
    /// `1` once the stack has the driver's MAC.
    pub const HAVE_MAC: usize = 10;
}

/// Values for [`result_word::STATE`].
pub mod state {
    /// The stack has not reached its listen call.
    pub const INIT: u64 = 0;
    /// Listening, no peer yet.
    pub const LISTENING: u64 = 1;
    /// A connection is open.
    pub const ESTABLISHED: u64 = 2;
    /// Closed in an orderly way -- both sides said so.
    pub const CLOSED: u64 = 3;
    /// The connection ended without an orderly close.
    pub const ABORTED: u64 = 4;
}

/// `(offset, end)` of the result block, or `None` if `area` cannot hold it.
pub const fn result_range(area_len: usize) -> Option<(usize, usize)> {
    if area_len < OFF_RESULT + RESULT_BYTES {
        None
    } else {
        Some((OFF_RESULT, OFF_RESULT + RESULT_BYTES))
    }
}

// The result block must not overlap either frame slot, and the whole thing must fit what the kernel
// hands out. Prose would drift; these do not.
const _: () = assert!(OFF_RX_DATA + FRAME_MAX as usize <= OFF_RESULT);
const _: () = assert!(OFF_RESULT + RESULT_BYTES <= SHARED_BYTES);
const _: () = assert!(RESULT_MAGIC != MAGIC as u64);

// ------------------------------------------------------------------------------------------------
// The header.
// ------------------------------------------------------------------------------------------------

/// A slot header, read or written.
///
/// [`MAGIC`] is not a field: it is a **condition**. A value of this type only ever comes out of
/// [`parse`](SlotHeader::parse), which has already checked it, and only ever goes onto the wire
/// through [`write`](SlotHeader::write), which always emits it. The same construction as
/// `GptHeader` in `caprock-part` — the check sequence is carried by the type, not by the caller's
/// discipline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotHeader {
    /// Payload bytes at the matching `OFF_*_DATA`. Never larger than [`FRAME_MAX`] in a parsed
    /// header, because such a header is refused rather than clamped.
    pub len: u32,
    /// One of the `ST_*` codes. Kept as a number and not as an enum: an unknown status from a peer
    /// is **data**, and turning unrecognised data into an unrepresentable value in the reader is
    /// how a parser for foreign bytes acquires its first panic.
    pub status: u32,
    /// Monotonic per direction. Lets a reader tell "nothing new" from "the same frame again" — see
    /// the module doc.
    pub seq: u32,
}

fn rd32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl SlotHeader {
    /// Is `len` one that a slot can actually hold?
    ///
    /// Exposed separately so the caller can tell the two reasons [`write`](SlotHeader::write)
    /// refuses apart **before** it calls: "my buffer is too small" and "my length is impossible"
    /// are different findings, and a single `bool` return would destroy the distinction.
    pub const fn len_ok(&self) -> bool {
        self.len <= FRAME_MAX
    }

    /// Read a header out of `bytes`, which begins at the slot's header offset.
    ///
    /// `None` for a short buffer, for a foreign [`MAGIC`], and for a `len` beyond [`FRAME_MAX`].
    /// The last one is deliberate and is the least obvious of the three: a length the slot cannot
    /// hold is not a small error to clamp, it is a peer speaking a different protocol. Clamping it
    /// would hand the caller a plausible-looking frame assembled from whatever happened to be in
    /// the area — the failure mode where nothing reports an error and the data is wrong.
    pub fn parse(bytes: &[u8]) -> Option<SlotHeader> {
        if bytes.len() < HDR_BYTES {
            return None;
        }
        if rd32(bytes, 0) != MAGIC {
            return None;
        }
        let len = rd32(bytes, 4);
        if len > FRAME_MAX {
            return None;
        }
        Some(SlotHeader { len, status: rd32(bytes, 8), seq: rd32(bytes, 12) })
    }

    /// Write the header into `out`, which begins at the slot's header offset.
    ///
    /// `false` if `out` is shorter than [`HDR_BYTES`], or if `self.len` is one that
    /// [`parse`](SlotHeader::parse) would refuse. The second refusal keeps the two ends symmetric:
    /// a writer that emits a header its own reader rejects has manufactured a peer that speaks a
    /// different protocol, and it would find out about it on the far side, where the only remaining
    /// answer is silence. Use [`len_ok`](SlotHeader::len_ok) to tell the two cases apart.
    ///
    /// Nothing is written when the call refuses — a half-written header is a header whose magic and
    /// length disagree, and that is worse than none.
    pub fn write(&self, out: &mut [u8]) -> bool {
        if out.len() < HDR_BYTES || !self.len_ok() {
            return false;
        }
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&self.len.to_le_bytes());
        out[8..12].copy_from_slice(&self.status.to_le_bytes());
        out[12..16].copy_from_slice(&self.seq.to_le_bytes());
        true
    }
}

// ------------------------------------------------------------------------------------------------
// Bounds.
//
// The point of these two is that a caller never computes an offset itself. An `OFF_* + len` written
// out at the call site is correct until someone passes a length that came off the wire -- and the
// length off the wire is exactly the one an attacker chooses.
// ------------------------------------------------------------------------------------------------

/// `(offset, end)` of the TX payload for `len` bytes, or `None` if `len` exceeds [`FRAME_MAX`].
///
/// `None` is the input for a [`ST_TOOBIG`] reply, not a reason to clamp: the caller must answer,
/// and it must answer with the fact that the frame did not fit.
pub const fn tx_data_range(len: u32) -> Option<(usize, usize)> {
    if len > FRAME_MAX {
        None
    } else {
        Some((OFF_TX_DATA, OFF_TX_DATA + len as usize))
    }
}

/// `(offset, end)` of the RX payload for `len` bytes, or `None` if `len` exceeds [`FRAME_MAX`].
pub const fn rx_data_range(len: u32) -> Option<(usize, usize)> {
    if len > FRAME_MAX {
        None
    } else {
        Some((OFF_RX_DATA, OFF_RX_DATA + len as usize))
    }
}

// ------------------------------------------------------------------------------------------------
// MAC packing for OP_MAC.
//
// Both directions live here so the two ends cannot disagree about the endianness. They would
// otherwise agree on the six bytes and disagree about which end of the word byte 0 sits in -- and a
// reversed MAC is a value that looks entirely well-formed, which is why it survives review.
// ------------------------------------------------------------------------------------------------

/// Pack a MAC into the low 48 bits: byte 0 in bits 0..8, byte 5 in bits 40..48.
///
/// The upper 16 bits are zero and carry no meaning in this version.
pub fn mac_to_u64(mac: [u8; 6]) -> u64 {
    let mut v = 0u64;
    let mut i = 0;
    while i < 6 {
        v |= (mac[i] as u64) << (8 * i);
        i += 1;
    }
    v
}

/// Unpack a MAC from the low 48 bits.
///
/// The upper 16 bits are **ignored**, because the signature has nowhere to report them: the reply
/// word carries one value, and a decoder that returned an `Option` here would push the decision
/// onto a caller that has already committed to a `[u8; 6]`. Where the distinction matters — a peer
/// that set bits this version does not define — use [`mac_from_u64_checked`], which names the case
/// instead of swallowing it.
pub fn mac_from_u64(v: u64) -> [u8; 6] {
    let mut mac = [0u8; 6];
    let mut i = 0;
    while i < 6 {
        mac[i] = (v >> (8 * i)) as u8;
        i += 1;
    }
    mac
}

/// Like [`mac_from_u64`], but `None` if anything is set above bit 47.
///
/// Same rule as the magic check: a word carrying bits this version does not define was not written
/// by a peer that speaks this version, and refusing it is cheaper than deciding later what those
/// bits were supposed to have meant.
pub fn mac_from_u64_checked(v: u64) -> Option<[u8; 6]> {
    if v >> 48 != 0 {
        return None;
    }
    Some(mac_from_u64(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    /// A MAC that is **not** a palindrome, and its packed form written out by hand.
    ///
    /// The choice is the whole point: a palindromic vector round-trips even through an
    /// implementation that reverses the bytes, so it would prove nothing about the one property
    /// these two functions exist to pin down. `52:54:00` is QEMU's prefix, so the vector is also
    /// the one a real run sees.
    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    const MAC_PACKED: u64 = 0x0000_5634_1200_5452;

    /// A fresh slot area, all zero -- which is exactly what a peer that has written nothing leaves
    /// behind, and therefore the state the magic check has to refuse.
    fn area() -> std::vec::Vec<u8> {
        vec![0u8; SHARED_BYTES]
    }

    #[test]
    fn header_round_trips_through_write_and_parse() {
        let h = SlotHeader { len: 1514, status: ST_OK, seq: 7 };
        let mut buf = [0u8; HDR_BYTES];
        assert!(h.write(&mut buf));
        assert_eq!(SlotHeader::parse(&buf), Some(h));
        // The magic is emitted, not assumed to be there already: `write` into a buffer that never
        // held one must still produce a parseable header.
        assert_eq!(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]), MAGIC);
    }

    #[test]
    fn parse_refuses_a_foreign_magic() {
        let h = SlotHeader { len: 64, status: ST_OK, seq: 1 };
        let mut buf = [0u8; HDR_BYTES];
        assert!(h.write(&mut buf));
        // One bit. `NET1` -> something that is not `NET1`; the rest of the header stays valid, so
        // the refusal is attributable to the magic and to nothing else.
        buf[0] ^= 0x01;
        assert_eq!(SlotHeader::parse(&buf), None);
        // And the state a peer that wrote nothing leaves behind.
        assert_eq!(SlotHeader::parse(&area()[OFF_RX_HDR..]), None);
    }

    #[test]
    fn parse_refuses_a_length_the_slot_cannot_hold() {
        // Both sides of the boundary, in one test, because only the pair says where the boundary
        // is. A parser that refused everything would pass the negative half alone.
        let mut buf = [0u8; HDR_BYTES];
        buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());

        buf[4..8].copy_from_slice(&FRAME_MAX.to_le_bytes());
        assert_eq!(SlotHeader::parse(&buf).map(|h| h.len), Some(FRAME_MAX));

        buf[4..8].copy_from_slice(&(FRAME_MAX + 1).to_le_bytes());
        assert_eq!(SlotHeader::parse(&buf), None);

        // The value an attacker actually writes: not one over, but everything.
        buf[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(SlotHeader::parse(&buf), None);
    }

    #[test]
    fn parse_refuses_a_buffer_shorter_than_the_header() {
        let h = SlotHeader { len: 0, status: ST_EMPTY, seq: 3 };
        let mut buf = [0u8; HDR_BYTES];
        assert!(h.write(&mut buf));
        assert_eq!(SlotHeader::parse(&buf[..HDR_BYTES - 1]), None);
        assert_eq!(SlotHeader::parse(&[]), None);
    }

    #[test]
    fn write_refuses_a_short_buffer_and_an_impossible_length_and_distinguishes_them() {
        let good = SlotHeader { len: 60, status: ST_OK, seq: 0 };
        let mut short = [0u8; HDR_BYTES - 1];
        assert!(!good.write(&mut short));
        assert!(good.len_ok(), "the length was fine -- only the buffer was too small");

        let bad = SlotHeader { len: FRAME_MAX + 1, status: ST_OK, seq: 0 };
        let mut buf = [0xAAu8; HDR_BYTES];
        assert!(!bad.write(&mut buf));
        assert!(!bad.len_ok(), "here it is the length, and `len_ok` is what says so");
        // Nothing was written: a half-written header whose magic and length disagree is worse than
        // no header, because the magic alone would make the next reader trust the length.
        assert_eq!(buf, [0xAAu8; HDR_BYTES]);
    }

    #[test]
    fn the_two_payload_ranges_never_overlap_at_any_length() {
        // Every length, not a sample: the interesting one is FRAME_MAX, where the TX slot ends
        // exactly where the RX header begins, and an off-by-one there is invisible at any other
        // length.
        for len in 0..=FRAME_MAX {
            let (t0, t1) = tx_data_range(len).expect("tx range within bounds");
            let (r0, r1) = rx_data_range(len).expect("rx range within bounds");
            assert!(t1 <= r0, "TX payload runs into the RX slot at len={len}");
            assert_eq!(t1 - t0, len as usize);
            assert_eq!(r1 - r0, len as usize);
            // The RX header sits between them, so a TX overrun hits the magic check.
            assert!(t1 <= OFF_RX_HDR && r0 >= OFF_RX_HDR + HDR_BYTES);
        }
    }

    #[test]
    fn both_slots_and_the_result_block_fit_inside_the_shared_area() {
        // 0x1800 + 16 * 8. The literal is here on purpose: `shared_bytes_needed()` is what a PD
        // refuses to start on, so a change to it must be a change someone made, not one that
        // followed from moving a constant.
        assert_eq!(shared_bytes_needed(), 6272);
        assert!(shared_bytes_needed() <= SHARED_BYTES);
        // The bound that actually matters is the largest range any helper can hand out.
        let (_, tx_end) = tx_data_range(FRAME_MAX).unwrap();
        let (_, rx_end) = rx_data_range(FRAME_MAX).unwrap();
        let (res_start, res_end) = result_range(SHARED_BYTES).unwrap();
        assert!(tx_end <= SHARED_BYTES && rx_end <= SHARED_BYTES && res_end <= SHARED_BYTES);
        // The result block sits ABOVE both frame slots. `fs` puts its result at 4096 because its
        // area carries no frame slots; here 4096 is inside the RX payload, and a result written
        // there would be overwritten by the next frame the driver stages. Asserted rather than
        // asserted-in-prose: the first version of this comment said TX, which is off by one slot
        // and would have sent the next reader to the wrong place.
        assert!(rx_end <= res_start);
        assert!(
            (OFF_RX_DATA..rx_end).contains(&4096),
            "4096 really is inside the RX slot -- that is why 0x1800 is used"
        );
        assert_eq!(res_end, shared_bytes_needed());
    }

    #[test]
    fn the_result_block_is_refused_when_the_area_cannot_hold_it() {
        assert!(result_range(SHARED_BYTES).is_some());
        assert!(result_range(OFF_RESULT + RESULT_BYTES).is_some());
        // One byte short is a refusal, not a shortened block: a caller that wrote a truncated
        // result would have the kernel read the tail out of whatever came after it.
        assert!(result_range(OFF_RESULT + RESULT_BYTES - 1).is_none());
        assert!(result_range(0).is_none());
    }

    #[test]
    fn a_zeroed_area_does_not_decode_as_a_result() {
        // The case that matters on the first boot: an area nobody has written yet. Without a magic
        // it would decode as a well-formed result saying nothing happened -- indistinguishable
        // from a stack that ran and achieved nothing, and those point at different files.
        let zeroed = [0u64; RESULT_WORDS];
        assert_ne!(zeroed[result_word::MAGIC], RESULT_MAGIC);
        assert_ne!(RESULT_MAGIC, 0);
        // And the two magics must not be confusable with each other.
        assert_ne!(RESULT_MAGIC, MAGIC as u64);
    }

    #[test]
    fn the_result_word_indices_are_distinct_and_inside_the_block() {
        use result_word::*;
        let all = [
            MAGIC, STATE, RX_BYTES, TX_BYTES, PATTERN_OK, POLLS, FRAMES_TX, FRAMES_RX,
            LAST_DRIVER_STATUS, LAST_DRIVER_REASON, HAVE_MAC,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(*a < RESULT_WORDS, "word index {a} is outside the block");
            for b in &all[i + 1..] {
                assert_ne!(a, b, "two fields share word index {a}");
            }
        }
        // Word 0 is the magic, because it is written last: a block whose magic is present is a
        // block whose other words are already there.
        assert_eq!(MAGIC, 0);
    }

    #[test]
    fn the_states_are_distinct_and_init_is_zero() {
        use state::*;
        let all = [INIT, LISTENING, ESTABLISHED, CLOSED, ABORTED];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // A block that was allocated and never filled reads INIT. That is the honest default:
        // "did not get there" rather than any outcome that could be mistaken for progress.
        assert_eq!(INIT, 0);
    }

    #[test]
    fn the_ranges_refuse_an_oversized_length_instead_of_clamping() {
        for len in [FRAME_MAX + 1, FRAME_MAX * 2, u32::MAX] {
            assert_eq!(tx_data_range(len), None, "len={len}");
            assert_eq!(rx_data_range(len), None, "len={len}");
        }
    }

    #[test]
    fn mac_round_trips_and_the_byte_order_is_pinned() {
        assert_eq!(mac_to_u64(MAC), MAC_PACKED);
        assert_eq!(mac_from_u64(MAC_PACKED), MAC);
        assert_eq!(mac_from_u64(mac_to_u64(MAC)), MAC);
        // Byte 0 is in the LOW byte -- stated as a value, so an implementation that agreed with
        // itself while reversing the order cannot pass.
        assert_eq!(MAC_PACKED & 0xFF, MAC[0] as u64);
        assert_eq!((MAC_PACKED >> 40) & 0xFF, MAC[5] as u64);
        let mut reversed = MAC;
        reversed.reverse();
        assert_ne!(mac_to_u64(reversed), MAC_PACKED, "the test vector must not be a palindrome");
    }

    #[test]
    fn mac_decoding_ignores_undefined_high_bits_and_the_checked_form_names_them() {
        let dirty = MAC_PACKED | 0xFFFF_0000_0000_0000;
        assert_eq!(mac_from_u64(dirty), MAC);
        assert_eq!(mac_from_u64_checked(dirty), None);
        assert_eq!(mac_from_u64_checked(MAC_PACKED), Some(MAC));
        // A MAC with every byte set must still round-trip: it fills the low 48 bits exactly, and an
        // off-by-one in the mask would show up here and nowhere else.
        let broadcast = [0xFFu8; 6];
        assert_eq!(mac_from_u64_checked(mac_to_u64(broadcast)), Some(broadcast));
    }

    #[test]
    fn the_op_and_status_codes_are_distinct_and_the_two_old_ops_are_frozen() {
        // A-5.4 measures OP_SELF and OP_FOREIGN. Renumbering either one would leave the suite line
        // `dmaiso : ALL PASS` measuring a different thing under the same name, and the driver would
        // answer a valid-looking request with its unknown-op branch.
        assert_eq!(OP_SELF, 1);
        assert_eq!(OP_FOREIGN, 2);

        let ops = [OP_SELF, OP_FOREIGN, OP_MAC, OP_TX, OP_RX];
        for (i, a) in ops.iter().enumerate() {
            for b in &ops[i + 1..] {
                assert_ne!(a, b, "two ops share a code");
            }
        }
        let sts = [ST_OK, ST_BADOP, ST_TOOBIG, ST_NODEV, ST_EMPTY];
        for (i, a) in sts.iter().enumerate() {
            for b in &sts[i + 1..] {
                assert_ne!(a, b, "two statuses share a code");
            }
        }
        // ST_EMPTY is not ST_OK. Stated on its own because folding the two is the specific mistake
        // this code exists to prevent: an empty poll would then be indistinguishable from a
        // received frame of length zero.
        assert_ne!(ST_EMPTY, ST_OK);
    }

    #[test]
    fn a_staged_frame_survives_a_reply_in_the_other_direction() {
        // The reason there are two slots, exercised end to end: the stack stages a TX frame, the
        // driver stages an RX frame, and the TX bytes are still there afterwards. With one slot the
        // second write would be equally well-formed and the first frame would be gone without any
        // party noticing.
        let mut a = area();
        let tx = SlotHeader { len: 100, status: ST_OK, seq: 1 };
        assert!(tx.write(&mut a[OFF_TX_HDR..]));
        let (t0, t1) = tx_data_range(tx.len).unwrap();
        a[t0..t1].fill(0xC5);

        let rx = SlotHeader { len: 60, status: ST_OK, seq: 1 };
        assert!(rx.write(&mut a[OFF_RX_HDR..]));
        let (r0, r1) = rx_data_range(rx.len).unwrap();
        a[r0..r1].fill(0x3A);

        assert_eq!(SlotHeader::parse(&a[OFF_TX_HDR..]), Some(tx));
        assert!(a[t0..t1].iter().all(|&b| b == 0xC5));
        assert_eq!(SlotHeader::parse(&a[OFF_RX_HDR..]), Some(rx));
        assert!(a[r0..r1].iter().all(|&b| b == 0x3A));
    }

    #[test]
    fn a_sequence_number_separates_a_new_frame_from_the_same_one_again() {
        // Without `seq` these two states are byte-identical in every field a reader looks at, and
        // "nothing new arrived" reads as "a frame arrived" on every poll.
        let mut a = area();
        let first = SlotHeader { len: 60, status: ST_OK, seq: 41 };
        assert!(first.write(&mut a[OFF_RX_HDR..]));
        let seen = SlotHeader::parse(&a[OFF_RX_HDR..]).unwrap();

        // Poll again with nothing new: the header is untouched.
        let again = SlotHeader::parse(&a[OFF_RX_HDR..]).unwrap();
        assert_eq!(again.seq, seen.seq);
        assert_eq!(again, seen);

        // A genuinely new frame of the same length and status differs only here.
        let next = SlotHeader { len: 60, status: ST_OK, seq: 42 };
        assert!(next.write(&mut a[OFF_RX_HDR..]));
        let fresh = SlotHeader::parse(&a[OFF_RX_HDR..]).unwrap();
        assert_ne!(fresh.seq, seen.seq);
        assert_eq!((fresh.len, fresh.status), (seen.len, seen.status));
    }
}
