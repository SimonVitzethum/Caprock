//! `netstack` — **the TCP/IP stack as a protection domain of its own.**
//!
//! ================================================================================================
//! WHY THIS IS A PD AND NOT A LIBRARY IN THE DRIVER
//! ================================================================================================
//!
//! The tempting shape is one PD: the virtio-net driver already owns the DMA region the frames land
//! in, so putting smoltcp beside it would save an IPC round trip per frame. **That shape does not
//! exist in this system, and the reason is a rule, not a preference.**
//!
//! `cap_allowed` (`crates/caprock-microkit/src/lib.rs:219-241`) refuses an Endpoint or Notification
//! cap to a `HardwareLand` PD unless it names that PD's **own** channel, and the domain oracle
//! beside it calls a hardware cap (MMIO/IRQ/DMA) in a non-`HardwareLand` PD anomaly code 1. The two
//! halves close the door from both sides: a PD that owns packet buffers is `HardwareLand`, and a
//! `HardwareLand` PD cannot hold a foreign endpoint. A stack fused to the driver could therefore
//! never be **called** by anybody — it would be a network service with no way to serve.
//!
//! So this PD holds no DMA and no MMIO. It holds a channel to the driver and a window of ordinary
//! RAM, and every byte that crosses the wire crosses that window. What it buys is the property the
//! whole exercise is for: **the code that parses bytes an attacker chose does not own the device.**
//! A smoltcp bug here cannot reprogram a descriptor ring, because there is no ring in this address
//! space to reprogram.
//!
//! The second half of the argument is the one `wasmhost` already made: smoltcp is third-party code,
//! and it links **here**. For every mandant who does not speak TCP the TCB grows by zero.
//!
//! ================================================================================================
//! WHAT THIS PD HOLDS
//! ================================================================================================
//!
//! | Slot | Cap | for |
//! |---|---|---|
//! | 1 | Notification (manifest `ntfn`), CLIENT badge | „I am done" |
//! | 2 | Endpoint to the virtio-net driver PD (manifest `ep` + `service_id` 5) | every frame, both directions |
//! | 6 | the driver's 8 KiB shared transfer area | where the frames actually sit |
//!
//! Nothing else. There is no `map` of a device window in this file and there is no `unsafe` block
//! that touches one.
//!
//! ================================================================================================
//! THE PATTERN IS WRITTEN OUT HERE, ON PURPOSE
//! ================================================================================================
//!
//! [`PATTERN_FROM_HOST`] and [`PATTERN_TO_HOST`] are literals in this file. They are **not** derived
//! from anything `tools/checknet.py` also has, and they are not imported from a shared crate. That
//! is the `checkfat` rule: a pattern both sides import is a pattern a broken copy still matches, and
//! then the comparison measures the import rather than the wire. The host witness writes the same
//! twenty-four bytes out in its own file, in its own language, and the two spellings agreeing is the
//! statement.
//!
//! ================================================================================================
//! THE CLOCK — NOMINAL, NOT CALIBRATED, AND THE CONSEQUENCE IS NAMED
//! ================================================================================================
//!
//! **There is no time syscall.** `programs/libcaprock/src/lib.rs` lists the whole ABI and it has no
//! clock; there is no vDSO in this tree, and `boot_arg` is explicitly reserved for the minimum,
//! because „ein Boot-Info-Block, den jedes Programm ungefragt lesen kann, waere eine
//! Autoritaetsquelle neben dem Manifest" (`kernel/src/loader.rs`).
//!
//! `CR4.TSD` is never set, so `rdtsc` works from ring 3 — but a PD does not know its **rate**. The
//! kernel has a PIT-calibrated `TSC_HZ` (`crates/caprock-hal/src/x86_64/timer.rs`); this PD has no
//! way to ask for it. So [`TSC_HZ_NOMINAL`] is a **nominal** number, not a measured one, and every
//! TCP timer constant inside smoltcp is therefore scaled by `TSC_actual / TSC_nominal`.
//!
//! The nominal value is deliberately a **lower bound** on any x86-64 TSC rate, and that choice fixes
//! the direction of the error: with `TSC_actual >= TSC_nominal`, this PD's clock runs **fast**, so
//! every timer fires **early**. Early retransmission costs a duplicate frame; late retransmission
//! costs a stall that looks exactly like a dead peer. Of the two directions, only one is debuggable
//! from the outside, and this is it. It is also why [`DEADLINE_MICROS`] can be trusted as an upper
//! bound in real time: a fast clock reaches the deadline sooner than it claims, never later, which
//! is the direction that keeps a run out of the watchdog.
//!
//! Under QEMU against a peer one hop away no retransmission timer needs to fire at all, which is
//! why a nominal rate is survivable for a first version. It is written down rather than buried
//! because the day it stops being survivable is the day someone puts a real link behind this.
//!
//! On aarch64 there is no readable counter at all: `CNTKCTL_EL1.EL0VCTEN` is set nowhere in this
//! kernel (measured — zero hits for `CNTKCTL` across `kernel/` and `crates/`), so `mrs cntvct_el0`
//! from EL0 traps. The aarch64 clock is therefore a **step counter**: monotonic, which is all
//! smoltcp requires of it, and bearing no relation to wall time, which is stated rather than
//! implied. This PD is loaded by the x86 load suite; the aarch64 build exists so the workspace
//! builds, and its clock has never run.
//!
//! ================================================================================================
//! THE POLL LOOP HAS NO CLIENT YET, AND THAT IS SAID OUT LOUD
//! ================================================================================================
//!
//! The design is that a client PD's `CALL` is the pump: a client asks for bytes, the stack pumps the
//! link far enough to answer, and nothing spins. There is no client PD in this round, so this PD
//! runs the pump itself. Both are the same [`poll_once`] called from somewhere; today the caller is
//! the loop in [`run`], and that is the only difference.
//!
//! **The loop is bounded twice, because neither bound is trustworthy alone.** [`POLL_BUDGET`] is a
//! property of this program and holds even if the clock is stuck; [`DEADLINE_MICROS`] is a property
//! of a clock whose rate is nominal and holds even if a poll turns out to be far cheaper than
//! assumed. Whichever fires first ends the loop, **and the result block is written either way** —
//! a run that gives up reports the state it actually reached and the polls it actually did, which
//! is what separates it from a run that never started (state `INIT`, or no `RESULT_MAGIC` at all).
//!
//! ================================================================================================
//! THE ORDER INSIDE ONE TURN — MEASURED ON THE WIRE, NOT CHOSEN FOR TIDINESS
//! ================================================================================================
//!
//! `Interface::poll` means „take in every frame, then send everything". Running the application
//! **after** it — the obvious shape, and the one this file had — has a consequence that no line of
//! it stated: bytes handed to a socket cannot leave until the NEXT poll, and if that next poll is
//! also the one that takes in the peer's FIN, then the answer and this side's FIN are decided
//! between the same two egresses. TCP then does the correct thing and puts them in **one segment**.
//!
//! That is not a theory. The capture of the run this file was fixed against shows frame 11 as the
//! host's 24 bytes, frame 12 as the host's FIN, and frame 13 as the guest's 24 bytes riding out on
//! the guest's own FIN — one `FIN,ACK` segment of 78 bytes, 420 ms later. A stack that answers only
//! while shutting down and a stack that answers promptly are indistinguishable in that picture.
//!
//! [`pump`] therefore spells the turn out instead of borrowing it, in the decomposition smoltcp
//! documents for exactly this purpose (`poll_maintenance` + `poll_ingress_single` + `poll_egress`):
//! **application, then an egress round, then ONE frame in, and round again.** Two things follow,
//! and both are visible on the wire rather than in a comment: the peer's data frame is answered
//! before the peer's FIN frame is even looked at, and this side's FIN is a segment of its own
//! because [`close_allowed`] refuses to send it until the answer has had an egress round to itself.
//!
//! ================================================================================================
//! „ON THE WIRE" AND „ACKNOWLEDGED" ARE TWO FACTS, AND THEY FAIL DIFFERENTLY
//! ================================================================================================
//!
//! The block used to carry one word for both, holding the stricter of the two: `TX_BYTES` was
//! `enqueued - send_queue()`, which is what the peer has **acknowledged**. The reasoning was sound
//! — only a running peer produces ACKs, the same distinction as `rx_used` against „data arrived" —
//! and it produced `tx=0` for a run whose own capture shows 24 bytes crossing the link, because the
//! run stopped before the acknowledgement was readable. One word cannot carry „we put bytes on the
//! wire" and „the peer confirmed them": a stack that never sends and a stack whose peer vanished
//! look identical in the second fact and are told apart by the first.
//!
//! So there are two. `TX_BYTES` is measured out of the frames this PD actually handed the driver
//! ([`tcp_payload_len`], counted only on the reply that says `ST_OK`) — the honest reading of the
//! word the shared crate calls „bytes the stack sent to its peer". [`RW_TX_ACKED`] carries the
//! acknowledgement, and reads `0` when the run ended before one could be read. `TX_BYTES > 0` with
//! `TX_ACKED == 0` is a statement, not a gap: **the answer left, nobody confirmed it.**
//!
//! ================================================================================================
//! WHAT THIS PD CANNOT REPORT, WRITTEN DOWN RATHER THAN LEFT TO BE FOUND
//! ================================================================================================
//!
//! `caprock-net` defines eleven words of the block and none of them is „the driver channel stopped
//! answering". A `CALL` the kernel could not deliver is **not** a driver status — the driver said
//! nothing — so putting a fabricated `ST_NODEV` into `LAST_DRIVER_STATUS` would be inventing a reply
//! that never happened, which is the one thing a report must never do. The loop stops on that
//! outcome and the result carries the state reached; a word of its own would separate it from an
//! expired deadline, and `caprock-net` is pinned for this round.
//!
//! One word **is** written past those eleven: [`RW_TX_ACKED`] takes index 11 locally, because the
//! twelfth fact of this round could not be folded into an existing word without one of the two
//! becoming a lie. The debt that comes with it is written at the constant: the kernel's `netcon`
//! line decodes the eleven the crate names, so word 11 reaches a dump of the shared area and no
//! report line, and the next time `caprock-net` is opened it belongs there — with the conjunct in
//! `netcon`'s verdict that cannot exist today.
//!
//! Two internal counters ([`Link::rx_stale`], [`Link::tx_dropped`]) have the same shape: they are
//! named, they are counted, and no word of the block carries them. Where a dropped send was the
//! driver's decision it names itself through `LAST_DRIVER_STATUS`/`LAST_DRIVER_REASON`; where it was
//! this PD's, it does not, and that is the limit.

#![no_std]
#![no_main]

use caprock_net::{
    mac_from_u64_checked, result_range, result_word, rx_data_range, state, tx_data_range,
    SlotHeader, FRAME_MAX, HDR_BYTES, OFF_RX_HDR, OFF_TX_HDR, OP_MAC, OP_RX, OP_TX, RESULT_MAGIC,
    RESULT_WORDS, ST_EMPTY, ST_OK,
};
use libcaprock::{call, exit, map_window, park, result, signal, Window};
use smoltcp::iface::{
    Config, Interface, PollIngressSingleResult, PollResult, SocketHandle, SocketSet, SocketStorage,
};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address};

// ------------------------------------------------------------------------------------------------
// Cap slots. The loader endows by fixed index (`kernel/src/loader.rs`): 1 = notification from the
// manifest, 2 = endpoint, 6 = the shared transfer area. Slots 3..5 are the device caps a driver PD
// gets, and this PD deliberately has none of them — see the module doc.
// ------------------------------------------------------------------------------------------------

/// The notification this PD signals when it is done. Badged by the loader; the badge is a property
/// of the **cap**, not of the message (`libcaprock::signal`), so the message word is 0.
const NTFN: u64 = 1;
/// The channel to the virtio-net driver PD.
const EP: u64 = 2;
/// The driver's 8 KiB shared transfer area — the only memory both PDs can see.
const SHARED: u64 = 6;

// ------------------------------------------------------------------------------------------------
// The network setup. Decided and fixed; see `tools/checknet.py` for the other end of it.
//
// QEMU runs `-netdev user,id=n0,restrict=on`. `restrict=on` isolates the guest from the host but
// leaves explicit forwarding rules untouched, so the guest cannot dial out and the host CAN dial in.
// The direction follows from that and is not a preference: the stack LISTENS, the host connects.
// ------------------------------------------------------------------------------------------------

/// The guest address. The same one `arp_probe` already uses in the driver PD — one address for one
/// interface, so a capture cannot show two.
const GUEST_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
/// Prefix length of [`GUEST_IP`].
const GUEST_PREFIX: u8 = 24;
/// QEMU's user-mode gateway, and the address the host's connection appears to come from.
const GATEWAY_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
/// The port the stack listens on. The host reaches it through a hostfwd rule on 127.0.0.1:17777.
const LISTEN_PORT: u16 = 7777;

/// What the peer sends. **A literal, not an import** — see the module doc.
const PATTERN_FROM_HOST: &[u8] = b"CAPROCK-HOST-TO-GUEST-01";
/// What this stack sends back. Likewise a literal.
const PATTERN_TO_HOST: &[u8] = b"CAPROCK-GUEST-TO-HOST-01";

/// How many bytes of [`PATTERN_FROM_HOST`] have to arrive before the answer goes out.
const PATTERN_LEN: usize = PATTERN_FROM_HOST.len();

// The two patterns must not be each other: a stack that echoed its input would satisfy a comparison
// against one pattern and prove nothing about the other direction. Asserted rather than assumed,
// because both literals live three lines apart and a copy-paste is exactly how they would converge.
const _: () = assert!(PATTERN_FROM_HOST.len() == PATTERN_TO_HOST.len());
const _: () = {
    let (a, b) = (PATTERN_FROM_HOST, PATTERN_TO_HOST);
    let mut i = 0;
    let mut differs = false;
    while i < a.len() {
        if a[i] != b[i] {
            differs = true;
        }
        i += 1;
    }
    assert!(differs, "the two patterns must not be the same bytes");
};

// ------------------------------------------------------------------------------------------------
// The two bounds of the poll loop, and their named overflows.
// ------------------------------------------------------------------------------------------------

/// **The named overflow of the poll capacity.** An unbounded loop hangs the run, and a run that
/// hangs reports as a dead network no matter what the stack achieved.
///
/// It is a property of *this program* and therefore holds even when the clock does not advance —
/// which is the one failure [`DEADLINE_MICROS`] cannot survive. On expiry the result block is
/// written with the state actually reached and `POLLS == POLL_BUDGET`, so „gave up counting" is
/// readable in the block itself.
const POLL_BUDGET: u64 = 20_000_000;

/// **The named overflow of the waiting time**, in microseconds of the nominal clock.
///
/// It is a property of the *clock* and therefore holds even when a poll turns out to be far cheaper
/// than assumed — which is the one failure [`POLL_BUDGET`] cannot survive. Because
/// [`TSC_HZ_NOMINAL`] is a lower bound on the real rate, this deadline can only arrive **earlier**
/// in real time than the number says: with a real TSC between 1 and 4 GHz, 35 nominal seconds are
/// somewhere between 9 and 35 real ones.
///
/// The number is chosen against the run it lives in. `bringup.rs` pulls the emergency brake at
/// `sekunden > 60` (100 Hz ticks), and this PD starts several seconds into that window, so an upper
/// bound of 35 real seconds leaves the watchdog room to be the *second* thing that fires rather than
/// the first — a bound that fires after the watchdog is not a bound, it is decoration.
///
/// **What is left over, said plainly:** if the clock were stuck *and* a poll were expensive, neither
/// bound would land before the watchdog. That case is not survivable here, and it does not have to
/// be silent: the watchdog names `netcon`, which is a readable outcome and strictly better than a
/// hang with no name.
const DEADLINE_MICROS: i64 = 35_000_000;

/// How often one staged frame is offered to the driver before it is given up on.
///
/// The driver refuses a send it cannot take right now (`RSN_TX_BUSY`, its one-send-buffer capacity)
/// **without booking the sequence number**, so re-offering the same bytes under the same number is
/// exactly what its own rule allows. The pinned status vocabulary of `caprock-net` cannot tell a
/// transient refusal from a permanent one — `ST_REFUSED` is declared in the driver, not in the
/// shared crate, and holding a private opinion about a number that crate does not define is the one
/// thing that crate exists to prevent. So every non-OK status is retried this many times and then
/// the frame is dropped, with the driver's own last word left in the result block.
const TX_ATTEMPTS: u32 = 4;

/// **The named overflow of one turn's ingress.** How many received frames one turn of [`pump`]
/// takes in before it hands control back to the loop in [`run`].
///
/// `Interface::poll` carries a DoS warning in its own documentation — it processes *every* frame
/// the device has, which is unbounded work when frames arrive faster than they are handled, and
/// this PD has no preemption to fall back on: the two bounds that end the run ([`POLL_BUDGET`],
/// [`DEADLINE_MICROS`]) are only checked between turns, so a turn that never ends is a run that
/// never ends. Hitting this bound **drops nothing**: the remaining frames are still in the driver
/// and the next turn takes them.
const INGRESS_PER_TURN: u32 = 8;

/// **The named overflow of one turn's egress**, for the same reason as [`INGRESS_PER_TURN`] and
/// with the same consequence: what is still queued goes out on the next turn.
const EGRESS_PER_TURN: u32 = 8;

/// The MTU this device reports. An Ethernet frame including its 14-byte header and excluding the
/// FCS — the same 1514 `caprock_virtio::net::MAX_FRAME` refuses to exceed.
const MTU: usize = 1514;

/// **Word 11 of the result block, claimed HERE and not in `caprock-net` — and that is a debt, not
/// a design.**
///
/// `caprock-net` declares `RESULT_WORDS = 16` and names eleven of them (`MAGIC` .. `HAVE_MAC`).
/// The twelfth fact this stack now has — *how many of the bytes it put on the wire the peer
/// acknowledged* — needs a word of its own, and the crate is pinned for this round, so the index is
/// taken here. Two consequences, written down rather than left to be found:
///
/// * **The kernel does not print it.** `netcon` in `kernel/src/arch/x86_64/bringup.rs` decodes the
///   eleven words the crate names, so this one reaches a memory dump of the shared area and no
///   report line. It is written anyway, because a fact that has nowhere to go is a fact the next
///   run loses again.
/// * **It belongs in `caprock-net`.** The next time that crate is opened, this constant becomes
///   `result_word::TX_ACKED` there, this one is deleted, and the `netcon` verdict gains the
///   conjunct it cannot have today.
///
/// The asserts below are the guard against the collision that would otherwise happen silently: the
/// day the crate names word 11 itself, `HAVE_MAC` is no longer the last defined index, and this
/// build stops.
const RW_TX_ACKED: usize = 11;
const _: () = assert!(
    RW_TX_ACKED > result_word::HAVE_MAC,
    "caprock-net has grown past word 10 -- move TX_ACKED into the crate instead of claiming an \
     index it now defines"
);
const _: () = assert!(RW_TX_ACKED < RESULT_WORDS);

// A frame the device accepts must fit the staging slot. Pinned in this direction only: a slot wider
// than the MTU costs unused bytes, a slot narrower than the MTU makes `TxToken::consume` unable to
// hand out the buffer smoltcp asked for.
const _: () = assert!(MTU <= FRAME_MAX as usize);

// ------------------------------------------------------------------------------------------------
// The buffers, in `.bss`.
//
// A loaded program gets 16 KiB of EL0 stack (`LOADED_STACK_BYTES`). Four kilobyte-scale buffers do
// not go on it: the failure mode of a stack overflow here is a fault at an address nobody connects
// to a buffer size, and `panic = abort` means the PD disappears without a word. The loader lays the
// `.bss` out as a segment, exactly as it does for `wasmhost`'s arena.
// ------------------------------------------------------------------------------------------------

/// Where one received frame is staged on its way from the shared area into smoltcp.
static mut RX_STAGE: [u8; FRAME_MAX as usize] = [0; FRAME_MAX as usize];
/// Where smoltcp builds one frame on its way into the shared area.
static mut TX_STAGE: [u8; FRAME_MAX as usize] = [0; FRAME_MAX as usize];
/// The TCP socket's receive window. 4 KiB is far more than the 24 bytes this round moves; it is
/// sized so the buffer is never the thing that limits the window, because a window limited by two
/// different things at once cannot be reasoned about from a capture.
static mut SOCK_RX: [u8; 4096] = [0; 4096];
/// The TCP socket's send queue.
static mut SOCK_TX: [u8; 4096] = [0; 4096];

// ------------------------------------------------------------------------------------------------
// The clock.
// ------------------------------------------------------------------------------------------------

/// **A nominal TSC rate — 1 GHz, and it is a LOWER BOUND, not a measurement.**
///
/// Nothing in ring 3 can measure it (see the module doc). The value is chosen below every x86-64
/// part that can run this system, so `TSC_actual / TSC_nominal >= 1` and the clock runs fast rather
/// than slow. Calling it „calibrated" anywhere would be false.
const TSC_HZ_NOMINAL: u64 = 1_000_000_000;

/// Nominal ticks per microsecond. A `const` division of two nominal numbers is still nominal.
const TICKS_PER_MICRO: u64 = TSC_HZ_NOMINAL / 1_000_000;

/// How far the aarch64 step clock advances per read, in nominal ticks — 100 µs.
///
/// It is a step, not a measurement: on aarch64 nothing readable from EL0 counts time in this kernel
/// (see the module doc). smoltcp requires a monotonic `Instant` and nothing more, and a step counter
/// is monotonic. What it is not is a clock, and no comment here says otherwise.
#[cfg(target_arch = "aarch64")]
const TICKS_PER_STEP: u64 = 100 * TICKS_PER_MICRO;

/// One raw tick reading. `prev` is the previous raw value; the x86 implementation ignores it, the
/// aarch64 one *is* it plus a step. Threading the previous value through the signature is what lets
/// both architectures share one field instead of a `cfg`-ed struct.
#[cfg(target_arch = "x86_64")]
fn raw_ticks(_prev: u64) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: `rdtsc` is a pure read of the time-stamp counter. It touches no memory, no flags and
    // no register other than `eax`/`edx`, both of which are declared as outputs here. It is legal
    // from ring 3 because `CR4.TSD` gates it and this kernel never sets that bit (measured: zero
    // hits for `TSD` outside a comment in `crates/caprock-sync`).
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// See the x86 implementation and the module doc: this one counts steps, not time.
#[cfg(target_arch = "aarch64")]
fn raw_ticks(prev: u64) -> u64 {
    prev.wrapping_add(TICKS_PER_STEP)
}

/// The nominal clock, with the one property smoltcp actually requires made structural.
struct NominalClock {
    /// The reading `run` started at, so `Instant` counts from this PD's own start.
    origin: u64,
    /// The last raw reading, which is the aarch64 step counter's state.
    raw: u64,
    /// The last microsecond value handed out. Never decreases.
    last_micros: i64,
}

impl NominalClock {
    fn new() -> Self {
        let r = raw_ticks(0);
        NominalClock { origin: r, raw: r, last_micros: 0 }
    }

    /// The current instant, **monotonic by construction**.
    ///
    /// A TSC read on a core this thread has just migrated to can come back slightly behind the last
    /// one. `wrapping_sub` on a time difference is the expensive convenient line this project has
    /// already paid for — a counter that jumps back by 100 ticks would become roughly `2^64` — so a
    /// backwards reading makes time **stand still** here instead of running backwards or wrapping.
    /// Standing still is a thing smoltcp survives; going backwards is not.
    fn now(&mut self) -> Instant {
        self.raw = raw_ticks(self.raw);
        let elapsed = self.raw.saturating_sub(self.origin) / TICKS_PER_MICRO;
        let micros = i64::try_from(elapsed).unwrap_or(i64::MAX);
        if micros > self.last_micros {
            self.last_micros = micros;
        }
        Instant::from_micros(self.last_micros)
    }

    /// The raw reading this PD started at.
    ///
    /// It is handed to smoltcp as `Config::random_seed`, which decides the initial TCP sequence
    /// number. On x86 it is a `rdtsc` value and therefore different on every boot, which is the
    /// property that field asks for. It is **not** a random number and nothing here calls it one;
    /// on aarch64 it is a constant, because that architecture has no counter to read (see the
    /// module doc), and a constant ISN is one more thing the aarch64 build has never run with.
    fn seed(&self) -> u64 {
        self.origin
    }
}

// ------------------------------------------------------------------------------------------------
// The barrier.
// ------------------------------------------------------------------------------------------------

/// Ordering barrier for bytes another protection domain reads.
///
/// **Not an arch-neutral fence.** `core::sync::atomic::fence(SeqCst)` becomes `dmb ish` on aarch64,
/// and this project has already paid once for the convenient version silently weakening the
/// semantics. Both spellings here are the strongest the architecture has; strengthening a barrier is
/// always allowed, weakening one is the trap.
#[cfg(target_arch = "x86_64")]
fn publish_fence() {
    // SAFETY: `mfence` is a pure ordering instruction — no operands, no memory access, no flags.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) }
}

/// See the x86 version.
#[cfg(target_arch = "aarch64")]
fn publish_fence() {
    // SAFETY: as above.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) }
}

// ------------------------------------------------------------------------------------------------
// Talking to the driver PD.
// ------------------------------------------------------------------------------------------------

/// What came back from one `CALL` to the driver.
///
/// **Two separate facts, two separate values.** „The kernel could not deliver the call" and „the
/// driver refused the operation" are different findings and point at different files: the first says
/// the endpoint is gone or the driver died, the second says the driver is alive and answered. A
/// single status word would fold them together, and a caller that cannot tell them apart cannot
/// decide whether retrying is pointless.
enum DrvReply {
    /// The kernel did not deliver the call — **there is no driver answer at all.**
    ///
    /// It carries no value, and that is deliberate rather than lazy: the kernel's result code would
    /// be the only thing known about the failure, and there is nowhere in the result block to put
    /// it (see the module doc). A field carried through three call sites and read by none is a claim
    /// the code does not make good on.
    NoAnswer,
    /// The driver answered `[status, payload, refusal_reason, seq]`.
    ///
    /// The refusal reason is **not** a field here. [`Link::ask`] is the one place that has the
    /// `&mut self` needed to record it, so it records it there and the callers never have to carry
    /// it — a value that travels only to be dropped is how a report loses one.
    Answer { status: u64, payload: u64 },
}

/// **The link to the driver PD: the channel, the shared window, and everything counted about them.**
///
/// It is a struct of its own and not a handful of locals in [`run`] because `Device::receive` has to
/// hand out two tokens that borrow the device at the same time. One token needs the received bytes
/// and the other needs everything else; splitting the state along exactly that line is what makes
/// the borrow check pass without a cell, a lock or an `unsafe`.
struct Link {
    /// The shared transfer area. `Window` bounds-checks every access, so nothing in this file holds
    /// a raw slice into memory another PD writes.
    shared: Window,
    /// Where smoltcp builds one outgoing frame.
    tx_stage: &'static mut [u8],
    /// The sequence number of the frame last staged for the driver. `0` is reserved for „not
    /// sequenced", which is why [`Link::next_tx_seq`] never hands it out after the first send.
    tx_seq: u32,
    /// The sequence number of the frame last taken out of the RX slot. This is what makes „nothing
    /// new arrived" distinguishable from „the same frame is still sitting there".
    rx_seq: u32,
    /// Frames the driver **accepted** — `ST_OK`, whether or not the device had confirmed the
    /// descriptor yet. A frame the driver refused is not counted here; it lands in
    /// [`Link::tx_dropped`] and the driver's own word lands in [`Link::last_status`]. Counting
    /// refusals as sends is how a link that carries nothing reports a healthy frame rate.
    frames_tx: u64,
    /// Frames taken out of the RX slot — a number only a device that received something can move.
    frames_rx: u64,
    /// **TCP payload bytes in the frames the driver ACCEPTED** — measured out of the frames
    /// themselves, in [`Link::publish`], and only on the reply that says `ST_OK`.
    ///
    /// This is „the bytes left this PD", and it is a different fact from „the peer acknowledged
    /// them": a stack that never sends and a stack whose peer vanished look identical in an
    /// acknowledgement count and are told apart here. It is deliberately **not** derived from what
    /// was handed to the socket — smoltcp queues, coalesces, retransmits and drops, so „I called
    /// `send_slice`" says nothing about the wire. What is counted is what this PD actually gave the
    /// driver, parsed by [`tcp_payload_len`] out of the very bytes it gave it.
    ///
    /// A retransmission is counted again, because it **is** more bytes on the wire; the number is
    /// therefore an upper bound on what a peer can have received and a lower bound on what crossed
    /// the link, and both directions are stated rather than implied.
    wire_tx_payload: u64,
    /// The driver's last non-OK status, `0` if it has never given one.
    last_status: u64,
    /// The refusal reason that came with it.
    last_reason: u64,
    /// **Named, counted, and not reportable**: an `OP_RX` that answered `ST_OK` over a slot whose
    /// header repeats the sequence number already consumed. It means the driver believes it staged
    /// something and this PD has already taken it — a disagreement, not an idle link.
    rx_stale: u64,
    /// **Named, counted, and not reportable**: frames this PD built and never got onto the wire.
    /// Where the driver refused, `last_status`/`last_reason` carry its word; where the refusal was
    /// local, nothing in the result block does. See the module doc.
    tx_dropped: u64,
    /// `false` once a `CALL` came back without a driver answer.
    ///
    /// The poll loop stops on it, and [`poll_once`] checks it **before** it counts. Without both
    /// halves a stack polling an endpoint nobody serves would report a rising `POLLS` count against
    /// no work at all — the shape „a capacity curve that counts return values rises just as nicely
    /// while everything dies" describes exactly.
    channel_ok: bool,
}

impl Link {
    fn new(shared: Window, tx_stage: &'static mut [u8]) -> Self {
        Link {
            shared,
            tx_stage,
            tx_seq: 0,
            rx_seq: 0,
            frames_tx: 0,
            frames_rx: 0,
            wire_tx_payload: 0,
            last_status: 0,
            last_reason: 0,
            rx_stale: 0,
            tx_dropped: 0,
            channel_ok: true,
        }
    }

    /// One request to the driver.
    ///
    /// `ST_EMPTY` is **not** recorded as the last non-OK status. `caprock-net` fixes it as a normal
    /// outcome, and an idle link produces it on nearly every poll — recording it would bury the one
    /// status worth reading (a send the driver refused) under thousands of copies of „nothing was
    /// waiting". That is the same failure as inventing failures: it does not lose a finding, it
    /// drowns one.
    fn ask(&mut self, op: u64, arg: u64) -> DrvReply {
        let m = call(EP, [op, arg, 0, 0]);
        if m.result != result::OK {
            self.channel_ok = false;
            return DrvReply::NoAnswer;
        }
        let (status, payload, reason) = (m.msg[0], m.msg[1], m.msg[2]);
        if status != u64::from(ST_OK) && status != u64::from(ST_EMPTY) {
            self.last_status = status;
            self.last_reason = reason;
        }
        DrvReply::Answer { status, payload }
    }

    /// The negotiated MAC — and the proof that the channel works before anything else is attempted.
    ///
    /// `mac_from_u64_checked` and not `mac_from_u64`: a reply word with bits set above 47 was not
    /// written by a peer that speaks this version, and this call happens exactly once, at startup,
    /// where refusing costs nothing and guessing would put a fabricated address on every frame.
    fn mac(&mut self) -> Option<[u8; 6]> {
        match self.ask(OP_MAC, 0) {
            DrvReply::Answer { status, payload } if status == u64::from(ST_OK) => {
                mac_from_u64_checked(payload)
            }
            _ => None,
        }
    }

    /// Poll the driver once and copy whatever it staged into `out`. `None` means „no frame for you",
    /// which covers an empty link, a refusal, and a header this PD will not read.
    fn fetch(&mut self, out: &mut [u8]) -> Option<usize> {
        let DrvReply::Answer { status, .. } = self.ask(OP_RX, 0) else {
            return None;
        };
        if status != u64::from(ST_OK) {
            return None;
        }
        // The header is the authority, not `msg[1]`: `SlotHeader::parse` refuses a foreign magic and
        // a length the slot cannot hold, and those two refusals are the whole point of the type.
        // Trusting the reply word instead would read a length off the wire without the check that
        // exists for lengths off the wire.
        let hdr = {
            let w = self.shared;
            SlotHeader::parse(w.bytes(OFF_RX_HDR as u64, HDR_BYTES as u64)?)?
        };
        if hdr.seq == self.rx_seq {
            // The driver says `ST_OK` and the slot still carries the frame already consumed. Not an
            // idle link and not a fault — a disagreement, and taking the bytes again would hand
            // smoltcp a duplicate it would answer twice.
            self.rx_stale += 1;
            return None;
        }
        let (off, end) = rx_data_range(hdr.len)?;
        let n = end - off;
        if n == 0 {
            return None;
        }
        {
            let w = self.shared;
            let src = w.bytes(off as u64, n as u64)?;
            out.get_mut(..n)?.copy_from_slice(src);
        }
        self.rx_seq = hdr.seq;
        self.frames_rx += 1;
        Some(n)
    }

    /// The next TX sequence number.
    fn next_tx_seq(&mut self) -> u32 {
        self.tx_seq = bump_seq(self.tx_seq);
        self.tx_seq
    }

    /// Build one frame in the staging buffer and hand it to the driver.
    ///
    /// This is `TxToken::consume` with the token unwrapped. `consume` must produce an `R` and only
    /// `f` can make one, so there is no path here that refuses to call the closure — a frame that
    /// cannot be sent is dropped **after** it has been built, not instead of being built.
    fn emit<R, F: FnOnce(&mut [u8]) -> R>(&mut self, len: usize, f: F) -> R {
        let Some(out) = self.tx_stage.get_mut(..len) else {
            // Unreachable: `capabilities()` reports `MTU`, smoltcp builds nothing larger, and the
            // compile-time assert above pins `MTU <= FRAME_MAX`. Handled anyway, because the
            // alternative is a slice index, and `panic = abort` here means the PD disappears
            // without a word. The closure gets the whole staging buffer and the frame is dropped.
            self.tx_dropped += 1;
            return f(self.tx_stage);
        };
        // Zeroed first: a field smoltcp leaves untouched would otherwise carry bytes of the previous
        // frame onto the wire. That is a leak out of this address space, and it costs a memset.
        out.fill(0);
        let r = f(out);
        self.publish(len);
        r
    }

    /// Stage the frame that is already in [`Link::tx_stage`] and send it.
    fn publish(&mut self, len: usize) {
        let Ok(n) = u32::try_from(len) else {
            self.tx_dropped += 1;
            return;
        };
        let Some((off, _)) = tx_data_range(n) else {
            // Refused, never clamped: a truncated frame looks whole to whoever reads it next.
            self.tx_dropped += 1;
            return;
        };
        // **Payload first, header second.** The header is what the driver validates; publishing it
        // over bytes that are not there yet lets the driver see `MAGIC` and a length across stale
        // data. The IPC that follows orders both against the driver's read, but the order in which
        // they are written here is this PD's own discipline and does not depend on that.
        if stage(&self.shared, off as u64, &self.tx_stage[..len]).is_none() {
            self.tx_dropped += 1;
            return;
        }
        let seq = self.next_tx_seq();
        let h = SlotHeader { len: n, status: ST_OK, seq };
        if put_header(&self.shared, OFF_TX_HDR as u64, &h).is_none() {
            // Nothing published, and the number is spent. That is harmless and deliberate: the
            // driver's repeat check compares against numbers it has actually **booked**, and it
            // books nothing for a frame it never received, so a gap in the sequence costs nothing.
            // Skipping is preferred over reusing because it keeps the counter monotonic — then „the
            // same number twice" means one thing here and one thing over there.
            self.tx_dropped += 1;
            return;
        }
        publish_fence();
        // **Measured before the attempt, booked only after the driver takes it.** The bytes are in
        // hand right now; after a successful `OP_TX` the slot belongs to the driver again. A frame
        // that does not parse as IPv4/TCP contributes zero — which is the truth for the ARP frames
        // this stack also sends, and errs downward for anything else. Downward is the direction
        // that cannot manufacture a green report out of a broken parser.
        let payload = match self.tx_stage.get(..len) {
            Some(frame) => tcp_payload_len(frame).unwrap_or(0) as u64,
            None => 0,
        };
        for _ in 0..TX_ATTEMPTS {
            match self.ask(OP_TX, 0) {
                // `ST_OK` with `tx_used = 1` is „the device took the descriptor"; with `tx_used = 0`
                // it is „published and kicked, not yet confirmed". Both are frames **handed to the
                // driver**, which is what `FRAMES_TX` says, and neither is „the peer got it" — that
                // fact does not exist on this side of the link at all.
                DrvReply::Answer { status, .. } if status == u64::from(ST_OK) => {
                    self.frames_tx += 1;
                    self.wire_tx_payload += payload;
                    return;
                }
                // A refusal did not book the sequence number on the driver's side, so the same bytes
                // under the same number are what it expects to see again. See [`TX_ATTEMPTS`] for
                // why a permanent refusal is retried too.
                DrvReply::Answer { .. } => {}
                DrvReply::NoAnswer => {
                    self.tx_dropped += 1;
                    return;
                }
            }
        }
        self.tx_dropped += 1;
    }
}

/// The successor of a TX sequence number. `0` stays reserved for „not sequenced", so a wrap skips
/// it — otherwise the frame after the wrap would look unsequenced to the driver and its repeat check
/// would stop applying to it.
///
/// Pure, so the three cases that matter are pinned **in the build** rather than in a test this
/// `no_main` binary has no harness for. The wrap case is the one that is never exercised in a run
/// and would therefore never be exercised anywhere else either.
const fn bump_seq(prev: u32) -> u32 {
    let s = prev.wrapping_add(1);
    if s == 0 {
        1
    } else {
        s
    }
}

const _: () = assert!(bump_seq(0) == 1);
const _: () = assert!(bump_seq(1) == 2);
const _: () = assert!(bump_seq(u32::MAX) == 1, "a wrap must skip the reserved 0");

// ------------------------------------------------------------------------------------------------
// How many bytes of a frame this PD built are TCP payload.
//
// **This is the measurement `TX_BYTES` is made of.** Without it the block can only report what was
// handed to the socket (which smoltcp may still be holding) or what the peer acknowledged (which
// needs a living peer). Neither of those is „the bytes left this PD", and that is the fact a
// capture can be held against: the answer in the run's own capture is a 78-byte frame, which is
// 14 + 20 + 20 + 24.
//
// The parser reads bytes **this PD wrote itself** one instruction earlier, not bytes an attacker
// chose — but it is written fail-closed anyway, because that is the cheaper half of the two: every
// refusal returns `None`, `None` contributes zero, and zero cannot make a run look better than it
// was.
// ------------------------------------------------------------------------------------------------

/// Bytes of an Ethernet header: two addresses and the type.
const ETH_HDR: usize = 14;
/// Smallest IPv4 header, and smallest TCP header. The same number twice, by coincidence of the two
/// protocols; written as two constants because they are two facts.
const IP_HDR_MIN: usize = 20;
/// See [`IP_HDR_MIN`].
const TCP_HDR_MIN: usize = 20;

/// TCP payload bytes in `frame`, or `None` if `frame` is not an IPv4/TCP frame this PD can read.
///
/// `None` is not an error path with a second meaning: an ARP frame is a legitimate thing for this
/// stack to send and carries no TCP payload, so „not TCP" and „no payload" both come out as zero at
/// the call site. What `None` buys is that a **malformed** frame — one whose own IP length claims
/// more than the frame holds — is refused rather than measured, so a length that would read past
/// the buffer is never used.
///
/// Pure, and therefore pinned **in the build** rather than in a test this `no_main` binary has no
/// harness for. The cases below are the ones that separate a real measurement from a plausible
/// one: options in either header (a fixed 40 would be wrong), a frame shorter than its own claim,
/// and a non-TCP frame.
const fn tcp_payload_len(frame: &[u8]) -> Option<usize> {
    if frame.len() < ETH_HDR + IP_HDR_MIN + TCP_HDR_MIN {
        return None;
    }
    // EtherType, big-endian on the wire. 0x0800 = IPv4; 0x0806 (ARP) lands here too and leaves.
    if frame[12] != 0x08 || frame[13] != 0x00 {
        return None;
    }
    let v = frame[ETH_HDR];
    if v >> 4 != 4 {
        return None;
    }
    // IHL is in 32-bit words and is the ONLY authority on where the TCP header starts. A fixed 20
    // is right for every frame smoltcp builds today and wrong for the first one that carries an
    // option.
    let ihl = ((v & 0x0f) as usize) * 4;
    if ihl < IP_HDR_MIN {
        return None;
    }
    // Protocol 6 = TCP. UDP, ICMP and everything else are refused rather than counted: they carry
    // no bytes of the answer, and counting their payload would put foreign bytes into `TX_BYTES`.
    if frame[ETH_HDR + 9] != 6 {
        return None;
    }
    // The IP total length, not `frame.len()`: an Ethernet frame may be PADDED to the 60-byte
    // minimum, so the frame can be longer than the datagram in it — never shorter.
    let total = ((frame[ETH_HDR + 2] as usize) << 8) | (frame[ETH_HDR + 3] as usize);
    if frame.len() < ETH_HDR + total {
        return None;
    }
    let tcp = ETH_HDR + ihl;
    if frame.len() < tcp + TCP_HDR_MIN {
        return None;
    }
    let doff = ((frame[tcp + 12] >> 4) as usize) * 4;
    if doff < TCP_HDR_MIN {
        return None;
    }
    if total < ihl + doff {
        return None;
    }
    Some(total - ihl - doff)
}

/// Build one frame of exactly `N` bytes for the compile-time checks below.
///
/// A builder and not six hand-written byte arrays: the arrays would have to be edited in six places
/// the day a field moves, and five of them would be edited correctly.
const fn synth_frame<const N: usize>(
    ethertype_lo: u8,
    ihl_words: u8,
    doff_words: u8,
    proto: u8,
    total_len: u16,
) -> [u8; N] {
    let mut f = [0u8; N];
    if N >= ETH_HDR {
        f[12] = 0x08;
        f[13] = ethertype_lo;
    }
    if N >= ETH_HDR + IP_HDR_MIN {
        f[ETH_HDR] = 0x40 | ihl_words;
        f[ETH_HDR + 2] = (total_len >> 8) as u8;
        f[ETH_HDR + 3] = total_len as u8;
        f[ETH_HDR + 9] = proto;
        let tcp = ETH_HDR + (ihl_words as usize) * 4;
        if tcp + TCP_HDR_MIN <= N {
            f[tcp + 12] = doff_words << 4;
        }
    }
    f
}

/// The answer itself: 14 + 20 + 20 + 24 = 78 bytes, the size the run's capture shows for it.
const F_TCP24: [u8; 78] = synth_frame::<78>(0x00, 5, 5, 6, 64);
const _: () = assert!(matches!(tcp_payload_len(&F_TCP24), Some(24)));

/// A bare ACK. **Zero is a measurement here, not a refusal** — and it must not be confused with
/// one, or every acknowledgement would look like an unreadable frame.
const F_ACK: [u8; 54] = synth_frame::<54>(0x00, 5, 5, 6, 40);
const _: () = assert!(matches!(tcp_payload_len(&F_ACK), Some(0)));

/// Options in BOTH headers: 4 payload bytes behind 24 + 24 of header. A parser that subtracted a
/// fixed 40 would answer 12 here and would have been green on every frame of the run.
const F_OPTS: [u8; 66] = synth_frame::<66>(0x00, 6, 6, 6, 52);
const _: () = assert!(matches!(tcp_payload_len(&F_OPTS), Some(4)));

/// Patch one byte of a frame, so the checks below can each be aimed at a single field.
const fn with_byte<const N: usize>(mut f: [u8; N], i: usize, v: u8) -> [u8; N] {
    f[i] = v;
    f
}

/// ARP — a frame this stack really does send. Refused, and therefore worth zero.
///
/// **It is refused by the LENGTH check, not by the ethertype check**, and saying so is the point:
/// a real ARP frame is 42 bytes and an IPv4/TCP frame needs 54, so this case is green whether or
/// not the ethertype is ever looked at. That is a test passing for a reason that has nothing to do
/// with the property it appears to cover, which is why [`F_NOT_IPV4`] stands beside it.
const F_ARP: [u8; 42] = synth_frame::<42>(0x06, 5, 5, 6, 28);
const _: () = assert!(matches!(tcp_payload_len(&F_ARP), None));

/// A frame that is a well-formed IPv4/TCP frame in **every** field except its ethertype. The only
/// check that can refuse it is the ethertype one — measured: with that check removed this assert is
/// the one that fires, and with [`F_ARP`] alone nothing fires at all.
const F_NOT_IPV4: [u8; 78] = synth_frame::<78>(0x06, 5, 5, 6, 64);
const _: () = assert!(matches!(tcp_payload_len(&F_NOT_IPV4), None));

/// IP version 6 in an IPv4-shaped header. Aimed at the version nibble alone.
const F_BAD_VER: [u8; 78] = with_byte(F_TCP24, ETH_HDR, 0x65);
const _: () = assert!(matches!(tcp_payload_len(&F_BAD_VER), None));

/// A TCP data offset of 16 bytes — smaller than a TCP header can be. Refused rather than believed:
/// believing it would add four bytes of header to the payload count.
const F_BAD_DOFF: [u8; 78] = with_byte(F_TCP24, ETH_HDR + 20 + 12, 0x40);
const _: () = assert!(matches!(tcp_payload_len(&F_BAD_DOFF), None));

/// IPv4, but not TCP. Its payload is not this connection's payload.
const F_UDP: [u8; 78] = synth_frame::<78>(0x00, 5, 5, 17, 64);
const _: () = assert!(matches!(tcp_payload_len(&F_UDP), None));

/// A frame whose own IP length claims 64 bytes inside 46. Refused, because the alternative is a
/// length off a header used to index a buffer that does not hold it.
const F_SHORT: [u8; 60] = synth_frame::<60>(0x00, 5, 5, 6, 64);
const _: () = assert!(matches!(tcp_payload_len(&F_SHORT), None));

/// A frame whose IP length is smaller than the headers it declares. Refused rather than clamped:
/// `total - ihl - doff` would underflow, and in a release build that is a wrapped `usize`.
const F_LIAR: [u8; 78] = synth_frame::<78>(0x00, 5, 5, 6, 30);
const _: () = assert!(matches!(tcp_payload_len(&F_LIAR), None));

/// Too short to be anything — five bytes.
///
/// This one pins the **first** check, and it pins it in the only way a length check can be pinned:
/// the parser reads `frame[12]` immediately afterwards, so a build in which that check is gone does
/// not produce a wrong answer here, it fails const evaluation with an index out of bounds. A
/// 20-byte frame stood here first and was useless — it was refused by the version nibble, which
/// `synth_frame` had never written, so the length check could have been deleted without a sound.
const F_TINY: [u8; 5] = [0; 5];
const _: () = assert!(matches!(tcp_payload_len(&F_TINY), None));

/// **How a finished connection is judged**: orderly only when *both* sides said so.
///
/// Pure and asserted for the same reason as [`bump_seq`], and this one carries more: it is the
/// judgement the whole run is about, and three of its four cases are the failure cases — a version
/// that reported `CLOSED` whenever the socket reached `Closed` would turn every reset into a clean
/// exchange, and the report would be green for the one outcome it exists to catch.
const fn close_verdict(peer_fin: bool, we_closed: bool) -> u64 {
    if peer_fin && we_closed {
        state::CLOSED
    } else {
        state::ABORTED
    }
}

const _: () = assert!(close_verdict(true, true) == state::CLOSED);
const _: () = assert!(close_verdict(true, false) == state::ABORTED);
const _: () = assert!(close_verdict(false, true) == state::ABORTED);
const _: () = assert!(close_verdict(false, false) == state::ABORTED);

/// **May this side send its FIN yet?**
///
/// The rule this function exists for is the last clause: an answer that has been handed to the
/// socket but has not yet had an **egress round of its own** must not be joined by a FIN. Enqueuing
/// is not sending; if both decisions are taken between the same two egresses, TCP does the obvious
/// thing and puts the data and the FIN in one segment. That is exactly what the run's capture shows
/// — frame 13, `FIN,ACK` with 78 bytes — and from the outside it is indistinguishable from a stack
/// that only ever answers while shutting down.
///
/// `answer_owed` and `answer_flushed` are two inputs and not one „ready" flag on purpose: a run in
/// which no answer was ever owed must still be able to close, and a run in which one was owed and
/// never got out must not.
///
/// Pure and asserted for the same reason as [`close_verdict`], and the second assert is the
/// regression guard for this round's finding: if someone folds the flush condition back out of the
/// close decision, the build stops here rather than the next capture showing it.
const fn close_allowed(
    peer_fin: bool,
    we_closed: bool,
    answer_owed: bool,
    answer_flushed: bool,
) -> bool {
    peer_fin && !we_closed && (!answer_owed || answer_flushed)
}

const _: () = assert!(close_allowed(true, false, true, true));
const _: () = assert!(
    !close_allowed(true, false, true, false),
    "the FIN must not leave in the same egress as the answer"
);
const _: () = assert!(close_allowed(true, false, false, false), "nothing owed, nothing to wait");
const _: () = assert!(!close_allowed(false, false, false, false), "the peer has not closed");
const _: () = assert!(!close_allowed(true, true, true, true), "closed once, not twice");

/// **What the peer has acknowledged — a measurement, and therefore not part of [`service`].**
///
/// `send_queue()` is what is still unsent or unacked, so `enqueued - send_queue()` is bytes the
/// peer's TCP has confirmed: work only a running peer can do. A monotonic maximum, because the
/// number may only ever be joined by more of the same fact.
///
/// **The refusal on `Closed` is the limit of what can be read, and it is not a bug to be routed
/// around.** A socket reaches `Closed` two ways: the peer acknowledged our FIN, in which case TCP
/// guarantees everything before it was acknowledged too — or `reset()` ran (an `RST`, an abort, the
/// interface losing its address), which **clears the transmit buffer without any acknowledgement at
/// all**. Both leave `send_queue() == 0`, and from outside the socket they are the same reading. So
/// a measurement taken there would produce a full acknowledgement for a connection the peer tore
/// down, which is the one thing a report must never do. The consequence is stated instead: a run
/// that stops before the acknowledgement is readable reports `TX_ACKED = 0` next to a non-zero
/// `TX_BYTES`, and that pair means „the answer left, nobody confirmed it".
fn measure_acked(sock: &tcp::Socket, app: &mut App) {
    if app.enqueued == 0 || sock.state() == tcp::State::Closed {
        return;
    }
    let acked = app.enqueued.saturating_sub(sock.send_queue() as u64);
    if acked > app.tx_acked {
        app.tx_acked = acked;
    }
}

/// Copy `src` into the shared window at `off`, byte by byte. `None` if it does not fit.
///
/// Byte-wise on purpose, exactly as in the driver PD: `Window::write_u64` is a single volatile store
/// and needs an 8-byte-aligned target. `OFF_TX_DATA` happens to be 8-aligned today, but that is a
/// property of two constants in another crate, not of this copy — and a correctness that follows
/// from a size relation instead of from structure disappears at the next measurement.
fn stage(w: &Window, off: u64, src: &[u8]) -> Option<()> {
    for (i, b) in src.iter().enumerate() {
        w.write_u8(off + i as u64, *b)?;
    }
    Some(())
}

/// Publish one slot header. `None` if `caprock-net` refused to emit it or it does not fit.
///
/// The detour through a stack buffer is not laziness: `SlotHeader::write` needs a `&mut [u8]` and a
/// `Window` never hands one out. Sixteen bytes of stack are the price of not holding a raw mutable
/// slice into shared memory in this file.
fn put_header(w: &Window, off: u64, h: &SlotHeader) -> Option<()> {
    let mut raw = [0u8; HDR_BYTES];
    if !h.write(&mut raw) {
        return None;
    }
    stage(w, off, &raw)
}

// ------------------------------------------------------------------------------------------------
// The smoltcp device.
// ------------------------------------------------------------------------------------------------

/// The link, seen as a `smoltcp::phy::Device`.
///
/// The frames do not live in this address space and never will (see the module doc), so both
/// directions are IPC: `receive` **is** an `OP_RX` request, and consuming a `TxToken` **is** an
/// `OP_TX` request.
///
/// That is why „drain the driver, then poll, then push what came out" has no separate spelling in
/// [`poll_once`]. `Interface::poll` calls `receive` until it returns `None` and then `transmit`
/// until there is nothing left — the drain and the push, in the one order that matters: the reply to
/// frame *n* is emitted through the `TxToken` handed out **with** frame *n*, so it goes out before
/// frame *n + 1* is fetched. A hand-written drain-then-poll loop would fetch the whole burst first
/// and answer it afterwards, which is a deeper queue than this link has.
struct Nic {
    /// Where one received frame is staged. Split out of [`Link`] so that `receive` can hand out a
    /// token borrowing the bytes and a token borrowing the link at the same time.
    rx_stage: &'static mut [u8],
    link: Link,
}

/// A received frame, already fetched. Holding bytes rather than a promise is what makes `consume`
/// infallible: by the time smoltcp sees this token the IPC has happened.
struct DrvRx<'a> {
    bytes: &'a [u8],
}

/// Permission to build one frame and send it.
struct DrvTx<'a> {
    link: &'a mut Link,
}

impl RxToken for DrvRx<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.bytes)
    }
}

impl TxToken for DrvTx<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.link.emit(len, f)
    }
}

impl Device for Nic {
    type RxToken<'a>
        = DrvRx<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = DrvTx<'a>
    where
        Self: 'a;

    fn receive(&mut self, _t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let Nic { rx_stage, link } = self;
        let n = link.fetch(rx_stage)?;
        Some((DrvRx { bytes: &rx_stage[..n] }, DrvTx { link }))
    }

    fn transmit(&mut self, _t: Instant) -> Option<Self::TxToken<'_>> {
        // Always `Some`. Whether the driver will take the frame is not knowable before the frame
        // exists, and answering `None` here would make smoltcp postpone work for a reason this PD
        // cannot check — a guess dressed as a capability.
        Some(DrvTx { link: &mut self.link })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = MTU;
        // **One, and it is measured against the driver, not chosen for taste.** smoltcp clamps the
        // advertised TCP window to `max_burst_size * MSS`; the driver arms exactly one receive
        // descriptor (`caprock_virtio::net::OFF_RXBUF`, one buffer) and one send buffer, so a peer
        // allowed a larger window would push segments this link cannot hold and lose them. A window
        // that promises more than the link can take is the politest possible way to cause packet
        // loss.
        c.max_burst_size = Some(1);
        // Checksums are left to smoltcp on purpose. `caprock-virtio` negotiates exactly
        // `VIRTIO_F_ACCESS_PLATFORM` and `VIRTIO_NET_F_MAC` — no checksum offload — so the device
        // neither verifies nor generates them, and claiming otherwise here would put frames on the
        // wire that the host kernel silently discards.
        c
    }
}

// ------------------------------------------------------------------------------------------------
// The application on top of the socket.
// ------------------------------------------------------------------------------------------------

/// Everything the result block reports, **maintained while it happens**.
///
/// Not computed at the end: a judgement that first comes into being in the report cannot trigger the
/// report, so the run walks into the watchdog and prints the result anyway. In a log that reads as
/// „green, but hung", and this repository has paid for it twice in one day.
struct App {
    /// One of `caprock_net::state::*`.
    state: u64,
    rx_bytes: u64,
    /// Bytes of the answer the peer has **acknowledged**. Reported separately from the bytes that
    /// went out — see [`RW_TX_ACKED`] and the module doc.
    tx_acked: u64,
    pattern_ok: bool,
    polls: u64,
    have_mac: bool,
    /// How many bytes of [`PATTERN_FROM_HOST`] have arrived **in the right place with the right
    /// value**. Compared position by position as the bytes come in, so a stack that received the
    /// right count of wrong bytes cannot pass.
    matched: usize,
    /// Bytes handed to the socket's send queue. **Not** what the block reports as `TX_BYTES`:
    /// enqueued is neither sent nor acknowledged, and this round exists because those three were
    /// treated as one number.
    enqueued: u64,
    /// The answer has been fully enqueued. **Enqueued, not sent** — the two are one poll apart, and
    /// this round is being fixed because they were treated as one thing.
    answered: bool,
    /// The value [`App::egress_rounds`] had when the answer was fully enqueued.
    ///
    /// It is what makes „the answer has had an egress of its own" a *checkable* statement instead
    /// of an assumption about the loop's shape. See [`close_allowed`].
    answered_at_egress: u64,
    /// How many egress rounds [`pump`] has run. Bytes handed to a socket reach the wire in an
    /// egress round and nowhere else, so this counter — not the poll count, not wall time — is the
    /// unit „the answer went out before the FIN" is measured in.
    egress_rounds: u64,
    /// A connection was open at some point. Without it, the `Closed` a socket has *before* it ever
    /// listened would read as a connection that ended.
    established: bool,
    /// The peer's FIN has been seen.
    peer_fin: bool,
    /// This side's FIN has been requested.
    we_closed: bool,
}

impl App {
    const fn new() -> Self {
        App {
            state: state::INIT,
            rx_bytes: 0,
            tx_acked: 0,
            pattern_ok: false,
            polls: 0,
            have_mac: false,
            matched: 0,
            enqueued: 0,
            answered: false,
            answered_at_egress: 0,
            egress_rounds: 0,
            established: false,
            peer_fin: false,
            we_closed: false,
        }
    }

    /// Account for bytes that arrived, and decide the pattern question **here**, while the bytes are
    /// in hand.
    fn absorb(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let pos = self.rx_bytes;
            self.rx_bytes += 1;
            let Ok(i) = usize::try_from(pos) else { continue };
            if i < PATTERN_LEN && b == PATTERN_FROM_HOST[i] {
                self.matched += 1;
            }
        }
        if self.matched == PATTERN_LEN {
            self.pattern_ok = true;
        }
    }
}

/// One turn of the socket, after `Interface::poll` has moved whatever it could.
///
/// Returns `true` when the connection has reached a terminal state and there is nothing left to do.
fn service(sock: &mut tcp::Socket, app: &mut App) -> bool {
    use tcp::State;

    // **Every state that can only be reached through a completed handshake**, not just
    // `Established`. One `Interface::poll` drains the link until it is empty, so a peer that sends
    // its ACK, its data and its FIN back to back leaves the socket in `CloseWait` before this
    // function ever runs — and a check for `Established` alone would then never record that a
    // connection existed, which would take the terminal test below (guarded on `established`) with
    // it and burn the whole poll budget on a finished exchange.
    if !app.established
        && matches!(
            sock.state(),
            State::Established
                | State::FinWait1
                | State::FinWait2
                | State::CloseWait
                | State::Closing
                | State::LastAck
                | State::TimeWait
        )
    {
        app.established = true;
        app.state = state::ESTABLISHED;
    }

    // A listening socket that is reset before any connection exists does **not** go back to
    // listening: `Socket::reset` clears `listen_endpoint` outright, so the socket is simply closed
    // and every later `SYN` is answered with nothing. The host witness makes up to three connection
    // attempts, so a stack that gave up after a stray first one would fail a run in which nothing
    // was wrong. Re-arming costs one call and is only reachable before a connection has existed.
    if !app.established && sock.state() == State::Closed {
        let _ = sock.listen(LISTEN_PORT);
    }

    // 1. Read whatever arrived. Sixty-four bytes at a time: the whole exchange is twenty-four, and a
    //    buffer sized for a case that does not occur is a buffer nobody has ever filled.
    while sock.can_recv() {
        let mut buf = [0u8; 64];
        match sock.recv_slice(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => app.absorb(&buf[..n]),
        }
    }

    // 2. Answer once, as soon as a full pattern's worth of bytes has arrived — **whether or not it
    //    matched**. Withholding the answer on a mismatch would fold „the pattern was wrong" and „the
    //    stack never answered" into the same silence at the host end, and only one of those two is a
    //    finding about this stack. `PATTERN_OK` carries the other fact, separately.
    if !app.answered && app.rx_bytes >= PATTERN_LEN as u64 && sock.can_send() {
        if let Ok(n) = sock.send_slice(PATTERN_TO_HOST) {
            app.enqueued += n as u64;
            if app.enqueued >= PATTERN_TO_HOST.len() as u64 {
                app.answered = true;
                // **The egress round the answer has not had yet.** `send_slice` moved bytes into a
                // buffer; nothing is on the wire until [`pump`] runs an egress. Recording the
                // count here is what lets step 4 below refuse to close the connection in the same
                // turn — which is the whole of finding 2: the answer and the FIN left together
                // because both decisions were taken between two egresses instead of across one.
                app.answered_at_egress = app.egress_rounds;
            }
        }
    }

    // 3. Has the peer closed? These four states are the ones only reachable after its FIN arrived.
    //    `FinWait2` is deliberately not among them: it means the peer acknowledged **our** FIN, not
    //    that it sent one.
    if matches!(
        sock.state(),
        State::CloseWait | State::Closing | State::LastAck | State::TimeWait
    ) {
        app.peer_fin = true;
    }

    // 4. Close in return — once the answer has had an egress of its own, or once there is no
    //    answer to give. If the answer was wanted and never got enqueued, this side deliberately
    //    does **not** close: the run then ends on its bound with `state = ESTABLISHED`, which says
    //    what happened, instead of on a tidy `CLOSED` that would claim an exchange took place.
    if close_allowed(
        app.peer_fin,
        app.we_closed,
        app.rx_bytes >= PATTERN_LEN as u64,
        app.answered && app.egress_rounds > app.answered_at_egress,
    ) {
        sock.close();
        app.we_closed = true;
    }

    // 5. Terminal. `TimeWait` counts as done: it is the state after both FINs have been exchanged
    //    and acknowledged, and waiting for it to expire would burn the whole poll budget on a
    //    connection that is already over. A socket that reached `Closed` without both FINs was reset
    //    or torn down, and that is `ABORTED` — a different finding from an orderly close, reported
    //    as one.
    if app.established && matches!(sock.state(), State::Closed | State::TimeWait) {
        app.state = close_verdict(app.peer_fin, app.we_closed);
        return true;
    }
    false
}

/// Push out what the sockets have queued — up to [`EGRESS_PER_TURN`] segments — and count that the
/// round happened. What does not fit goes out on the next round; nothing is dropped here.
///
/// One egress round is the unit in which bytes handed to a socket become bytes handed to the
/// driver. It is counted **unconditionally**, including the rounds that find nothing to send: the
/// statement [`close_allowed`] needs is „an egress ran, so anything that was queued has had its
/// chance", and that statement is true of an empty round too. Counting only the productive rounds
/// would make the counter a measure of traffic instead of a measure of opportunity — the same
/// mistake as a watchdog that counts hits rather than occasions.
///
/// An empty round is also nearly free: `poll_egress` reaches `Device::transmit` only when a socket
/// actually dispatches, so a round with nothing to send makes **no IPC at all**.
///
/// **The counter lives here and nowhere else, on purpose.** [`App::egress_rounds`] is the only
/// evidence [`close_allowed`] has that the answer was given a chance to leave; a counter that is
/// bumped *next to* the call it stands for is a counter that survives the call being moved, removed
/// or replaced by something that does not send. Taking `&mut App` makes the round and its count one
/// act.
fn drain_egress(
    iface: &mut Interface,
    nic: &mut Nic,
    sockets: &mut SocketSet<'_>,
    app: &mut App,
    now: Instant,
) {
    for _ in 0..EGRESS_PER_TURN {
        if iface.poll_egress(now, nic, sockets) == PollResult::None {
            break;
        }
        if !nic.link.channel_ok {
            break;
        }
    }
    app.egress_rounds += 1;
}

/// **One turn of the whole machine — and the order inside it is the finding of this round.**
///
/// `Interface::poll` is „drain every frame, then send everything". Called with the application
/// *after* it, as this loop used to do, it has a consequence nothing in the code said out loud:
/// whatever the application hands to a socket cannot leave until the NEXT poll — and if that next
/// poll is also the one that processes the peer's FIN, the answer and the FIN are decided between
/// the same two egresses and TCP puts them in one segment. The run's capture shows exactly that:
/// frame 11 is the host's 24 bytes, frame 12 is the host's FIN, and frame 13 is the guest's 24
/// bytes riding out **on** its own FIN, one segment, 420 ms later.
///
/// So the turn is spelled out instead of borrowed. smoltcp documents this decomposition itself
/// (`poll_maintenance` + `poll_ingress_single` + `poll_egress`, „this allows you to insert yields or
/// process other events between processing individual ingress packets"), and the order here is:
///
/// 1. the application, on everything that has arrived so far;
/// 2. **an egress round, in the same turn** — so what the application just produced is on its way
///    before anything else happens;
/// 3. exactly ONE frame in, and round again.
///
/// The effect on the wire is the point: the peer's data frame is answered before the peer's FIN
/// frame is even looked at, and the FIN this side sends is a segment of its own because
/// [`close_allowed`] refuses to send it until the answer has had step 2.
///
/// Both loops are bounded ([`INGRESS_PER_TURN`], [`EGRESS_PER_TURN`]) because the two bounds that
/// end the run are only checked between turns.
///
/// Returns `true` when the connection is finished.
fn pump(
    iface: &mut Interface,
    nic: &mut Nic,
    sockets: &mut SocketSet<'_>,
    handle: SocketHandle,
    app: &mut App,
    now: Instant,
) -> bool {
    // The part of a poll that touches no frames, exactly where `Interface::poll` puts it.
    iface.poll_maintenance(now);
    let mut frames = 0u32;
    loop {
        let terminal = service(sockets.get_mut::<tcp::Socket>(handle), app);
        drain_egress(iface, nic, sockets, app, now);
        if !nic.link.channel_ok {
            return false;
        }
        // Read after the egress, so a peer that acknowledged during this turn is seen in this turn.
        measure_acked(sockets.get_mut::<tcp::Socket>(handle), app);
        if terminal {
            // **The stack stops here, and it does not wait for the peer's last ACK.** The
            // acknowledgement of our FIN is the peer's work, it may never come, and — see
            // [`measure_acked`] — a socket that has reached `Closed` can no longer be asked about
            // it honestly anyway. What the stack does owe is its own last segment, and it has just
            // been flushed by the egress round above. Everything that was decided has left; nothing
            // that is outstanding is this side's to produce.
            return true;
        }
        if frames >= INGRESS_PER_TURN {
            return false;
        }
        if iface.poll_ingress_single(now, nic, sockets) == PollIngressSingleResult::None {
            return false;
        }
        frames += 1;
    }
}

/// **One counted turn.** Today the caller is the loop in [`run`]. The design is that a client PD's
/// `CALL` calls exactly this function instead, and nothing else about it changes — which is why it
/// is a function and not the body of the loop.
///
/// Returns `true` when the loop should stop.
fn poll_once(
    iface: &mut Interface,
    nic: &mut Nic,
    sockets: &mut SocketSet<'_>,
    handle: SocketHandle,
    app: &mut App,
    now: Instant,
) -> bool {
    let finished = pump(iface, nic, sockets, handle, app, now);
    // **The channel check comes before the count, and the order is the point.** A turn is counted
    // only when every `CALL` it made was delivered, so `POLLS` measures work done against a living
    // driver and not turns of this loop. A number that only proved this loop was spinning would
    // rise just as nicely with the driver dead, which is exactly the shape a capacity curve that
    // counts return values has.
    //
    // **Every turn that does not end the run makes at least one `OP_RX`**, and that is where the
    // number gets its meaning: [`pump`] leaves its loop either through `poll_ingress_single`
    // answering „nothing" (one request), through the ingress bound (eight of them), or through the
    // terminal state. Only the last of those can make no call at all — and it is also the last turn
    // there is, so it cannot inflate a curve.
    //
    // The consequence is a sharper reading, not a softer one: `POLLS == 0` with `state = LISTENING`
    // says the very first request was never delivered.
    if !nic.link.channel_ok {
        return true;
    }
    app.polls += 1;
    finished
}

// ------------------------------------------------------------------------------------------------
// The report.
// ------------------------------------------------------------------------------------------------

/// Write the result block, **magic last**.
///
/// The magic is word 0 and is written after every other word and after a barrier, so a block that
/// decodes is a block whose other words are already there. A zeroed area would otherwise read as a
/// perfectly well-formed result saying nothing happened — indistinguishable from a stack that ran
/// and achieved nothing, and those two point at different files.
fn write_result(shared: &Window, link: &Link, app: &App) -> bool {
    let Some((off, _)) = result_range(shared.len() as usize) else {
        return false;
    };
    let mut w = [0u64; RESULT_WORDS];
    w[result_word::STATE] = app.state;
    w[result_word::RX_BYTES] = app.rx_bytes;
    // **The bytes that left this PD**, measured out of the frames the driver accepted — the honest
    // reading of the word `caprock-net` calls „bytes the stack sent to its peer". „Sent" is what
    // this side can know; „arrived" is not, and „acknowledged" is a different fact with its own
    // word below. Folding the two into this one is what made the run report `tx=0` while its own
    // capture showed 24 bytes on the wire.
    w[result_word::TX_BYTES] = link.wire_tx_payload;
    // The second fact. Zero here next to a non-zero `TX_BYTES` reads „the answer left, nobody
    // confirmed it" — a stack that never sends and a stack whose peer vanished are now two lines,
    // not one. See [`RW_TX_ACKED`]: the index is claimed locally and belongs in the shared crate.
    w[RW_TX_ACKED] = app.tx_acked;
    w[result_word::PATTERN_OK] = u64::from(app.pattern_ok);
    w[result_word::POLLS] = app.polls;
    w[result_word::FRAMES_TX] = link.frames_tx;
    w[result_word::FRAMES_RX] = link.frames_rx;
    w[result_word::LAST_DRIVER_STATUS] = link.last_status;
    w[result_word::LAST_DRIVER_REASON] = link.last_reason;
    w[result_word::HAVE_MAC] = u64::from(app.have_mac);

    for (i, v) in w.iter().enumerate().skip(1) {
        if shared.write_u64((off + i * 8) as u64, *v).is_none() {
            return false;
        }
    }
    publish_fence();
    shared.write_u64(off as u64, RESULT_MAGIC).is_some()
}

/// Write the result, say so, and stop.
///
/// **`park` and not `exit`, for two reasons that are not the obvious one.** The obvious one would be
/// „so the bytes survive", and it is false: the result block lives in the *driver's* shared area
/// (`system::driver_shared_region`), which outlives this PD either way.
///
/// The reasons that hold are these. First, this is a **service**, and the loop above is the
/// temporary half of it — the design is that a client PD's `CALL` drives [`poll_once`], and a PD
/// that has exited cannot serve one; parking is the state a service is in between measurements, not
/// a way of tidying up after one. Second, a thread that exited and a thread that faulted are the
/// same absence from outside, while a parked thread is present and blocked and the scheduler audit
/// can say so. Ending in a way that is indistinguishable from dying is not an ending this repository
/// accepts anywhere else either.
fn finish(shared: &Window, link: &Link, app: &App) -> ! {
    let _ = write_result(shared, link, app);
    signal(NTFN, 0);
    park()
}

/// **End without ever writing a result.** Only the two startup failures that make a result
/// impossible come here.
///
/// The function exists for its comment, because the failure it describes is **silent**. The result
/// block lives *inside* the shared area: if slot 6 is missing or too small there is nowhere to put a
/// report, and every other channel this PD has says less. `exit()` carries no code — `SYS_EXIT`
/// takes none. A badge is a property of the cap, not of the message: `SYS_SIGNAL` ORs the badge of
/// the cap it was invoked on into `pending` and ignores the message word, and this PD holds one
/// notification cap with one badge, which already means „I am done". Signalling it here would be a
/// lie that reads as a completed run with an empty result block — worse than silence, because the
/// kernel would then decode `RESULT_MAGIC`'s absence as a stack that never started while a
/// notification claimed one had finished.
///
/// So it dies quietly, and the run reports a stack that never wrote anything. What would make this
/// audible is kernel-side and not this program's to add: a reason word on `SYS_EXIT`, or a second
/// badge meaning „started and gave up".
fn exit_without_result() -> ! {
    exit()
}

// ------------------------------------------------------------------------------------------------

libcaprock::entry!(run);

fn run(_arg: usize) -> ! {
    // 1. The shared area. Refused rather than survived if it is absent or short: a stack that ran
    //    without one could neither move a frame nor say so afterwards.
    let Some(shared) = map_window(SHARED) else {
        exit_without_result()
    };
    if (shared.len() as usize) < caprock_net::shared_bytes_needed() {
        exit_without_result()
    }

    // SAFETY: these four statics are taken exactly once, here, before anything else in this PD can
    // reach them, and the references are moved into `Nic`/`tcp::Socket` and never duplicated. This
    // program is single-threaded — a loaded PD gets one thread (`kernel/src/loader.rs`) and this
    // file creates none (`system::load_into_pd` returns exactly one `ThreadId`, and the SDK has no
    // syscall that makes another) — so there is no second holder and no interrupt handler in this
    // address space. `addr_of_mut!` and not `&mut STATIC`, so the reference is formed from a pointer rather
    // than from a place expression: the latter is what `static_mut_refs` refuses in edition 2024,
    // and this crate should not depend on the edition to stay sound.
    let (rx_stage, tx_stage, sock_rx, sock_tx): (
        &'static mut [u8],
        &'static mut [u8],
        &'static mut [u8],
        &'static mut [u8],
    ) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(RX_STAGE),
            &mut *core::ptr::addr_of_mut!(TX_STAGE),
            &mut *core::ptr::addr_of_mut!(SOCK_RX),
            &mut *core::ptr::addr_of_mut!(SOCK_TX),
        )
    };

    let mut clock = NominalClock::new();
    let mut app = App::new();
    let mut nic = Nic { rx_stage, link: Link::new(shared, tx_stage) };

    // 2. The MAC. This is also the proof that the channel works, and it is asked **first**, before
    //    an interface exists — a stack that built its whole state and only then discovered it cannot
    //    reach the driver would report a great deal of machinery and no link.
    let Some(mac) = nic.link.mac() else {
        // `state` stays `INIT` and `HAVE_MAC` stays 0, and the third word separates the two ways of
        // getting here: a driver that answered and refused leaves its own code in
        // `LAST_DRIVER_STATUS`, a driver that never answered leaves it at 0. So
        // `(INIT, HAVE_MAC = 0, LAST_DRIVER_STATUS = 0)` is „there is nothing on the other end of
        // that endpoint" and `(INIT, HAVE_MAC = 0, LAST_DRIVER_STATUS = 3)` is „the device has no
        // MAC to give". Those point at different files.
        finish(&shared, &nic.link, &app)
    };
    app.have_mac = true;

    // 3. The interface. The seed comes from the raw counter rather than from a constant: smoltcp
    //    uses it for the initial sequence number, and an ISN that is the same on every boot is the
    //    collision the field exists to avoid. It is not a random number and nothing here claims it
    //    is one.
    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    cfg.random_seed = clock.seed();
    let now = clock.now();
    let mut iface = Interface::new(cfg, &mut nic, now);

    let mut addr_ok = false;
    iface.update_ip_addrs(|a| {
        addr_ok = a.push(IpCidr::new(GUEST_IP.into(), GUEST_PREFIX)).is_ok();
    });
    if !addr_ok {
        // An interface with no address answers nothing. Reported as `INIT` with the MAC in hand,
        // which points at this line and at no other.
        finish(&shared, &nic.link, &app)
    }
    // The default route is not load-bearing for the run this PD is built for — the host's connection
    // arrives from 10.0.2.2, which is inside 10.0.2.15/24 and therefore on-link. It is installed
    // anyway because the failure it removes is silent: a peer one hop further away would produce a
    // stack that receives the SYN, has nowhere to send the answer, and looks from outside exactly
    // like a stack that never woke up.
    //
    // The result is dropped rather than checked, and that is not the usual laziness: the table is
    // empty at this point and has room, so the only way to fail is a smoltcp change — and the
    // outcome of failing is exactly the state this stack would be in without the line at all, in
    // which every peer this round actually has still works. There is nothing to report that is not
    // already true of the version before this line existed.
    let _ = iface.routes_mut().add_default_ipv4_route(GATEWAY_IP);

    // 4. One socket, one listener. `SocketStorage` is small and lives on the stack; the buffers it
    //    points at do not.
    let socket = tcp::Socket::new(
        tcp::SocketBuffer::new(sock_rx),
        tcp::SocketBuffer::new(sock_tx),
    );
    let mut storage: [SocketStorage; 1] = Default::default();
    let mut sockets = SocketSet::new(&mut storage[..]);
    let handle = sockets.add(socket);

    if sockets.get_mut::<tcp::Socket>(handle).listen(LISTEN_PORT).is_err() {
        finish(&shared, &nic.link, &app)
    }
    app.state = state::LISTENING;

    // 5. Pump. Bounded twice — see [`POLL_BUDGET`] and [`DEADLINE_MICROS`] for why one bound would
    //    not do. The loop leaves through exactly one door, and the result block is written on the
    //    other side of it whichever door it was.
    while app.polls < POLL_BUDGET {
        let now = clock.now();
        if now.total_micros() >= DEADLINE_MICROS {
            break;
        }
        if poll_once(&mut iface, &mut nic, &mut sockets, handle, &mut app, now) {
            break;
        }
    }

    finish(&shared, &nic.link, &app)
}
