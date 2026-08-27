#!/usr/bin/env python3
"""checknet -- the witness from OUTSIDE the VM for the TCP path.

This is the network analogue of `tools/checkfat.py`, and it exists for the same reason that one
does:

    A writer that confirms its own result confirms nothing.

The network stack inside the guest reports what it believes happened. That report is the guest's
claim, and a stack that is broken in an interesting way will claim success just as loudly as one
that works. So the judgement is made here instead, by a process that runs on the host, speaks TCP
through a foreign kernel, and reads the wire independently.

Two independent statements, deliberately not one:

  peer  -- a real TCP peer. It connects through QEMU's hostfwd into the guest, sends a pattern and
           reads one back. A three-way handshake with the host kernel is not something a guest can
           fake: the SYN-ACK it must produce is checked by an implementation nobody in this
           repository wrote.

  pcap  -- an independent reader of the frames that actually crossed the link, captured by QEMU's
           own `-object filter-dump`. It counts flags and payload bytes with the parser written
           out HERE rather than imported from anywhere the guest also uses. If the guest's claim
           and the wire disagree, the wire wins and the disagreement is the finding.

Neither alone is enough. `peer` proves a connection was served but says nothing about which frames
carried it; `pcap` proves frames crossed but a recorded handshake with no payload is not a working
socket. **Both must pass, and the green line spells the two-source case differently from the
one-source case** -- `ALL PASS` against `SINGLE-SOURCE PASS`. A `-object filter-dump` that
silently fails to attach degrades the run to half a witness, and half a witness that still prints
the acceptance string is worse than no witness at all.

The pattern is written out in both directions and is NOT derived from anything the guest has. That
is the checkfat rule: a pattern that both sides import is a pattern that a broken copy still
matches.

WHAT MAKES THE WIRE A SECOND SOURCE IS THE CONNECTION, NOT THE PORT NUMBER
--------------------------------------------------------------------------------------------
An earlier version of this file decided direction by TCP port alone (`to_guest = dport == port`).
Three captures passed it that no working stack could have produced: one in which every frame
carried the same source and destination MAC and src IP == dst IP (nothing ever left the guest);
one built from three unrelated connections of which not a single one completed; and a verbatim
replay of an earlier run. Under a port-only rule, one flag byte on one frame with the right port
"is" the SYN-ACK -- exactly the half the docstring claimed a guest could not fake.

So the wire half LEARNS the connection instead of assuming it:

  * the SYN to the guest's port names the client side (MAC, IP, port) and the guest side;
  * the answer must MIRROR that SYN -- reversed MAC pair, reversed IP pair, reversed ports -- and
    must **acknowledge it**: `ack == syn.seq + 1`. A fabricated SYN-ACK with a made-up sequence
    number is not an answer to this SYN;
  * the two ends must actually BE two ends: identical MACs or identical IPs mean nothing crossed
    a link, and that is a named failure rather than a passing capture;
  * only frames carrying that 6-tuple (or its exact reverse) are counted. Everything else is
    reported as off-connection and counted nowhere;
  * payload is reassembled BY SEQUENCE NUMBER per direction before the pattern is searched, so a
    stack that pushes its answer in three small segments passes -- `run_peer` already tolerates
    that, and a wire half that did not would have invented a failure for a legal stack;
  * when the peer half ran, the connection on the wire must be THE ONE IT OPENED: same client
    port (slirp's hostfwd preserves it; a tap bridge does too). That is the binder that reads
    the capture's CONTENT; two more read the capture FILE and the record stamps against each
    other, and the section below says why the record stamps are never read against the host's
    clock. The port binder can be waived with `--allow-port-rewrite` for a backend that does
    rewrite ports; the run then says so in a line of its own, and the file binder is what is
    left holding a replay out.

What this does NOT establish, written down so nobody reads more into it: a guest that emits
BOTH directions on its own link -- forging frames that carry the host's MAC and IP -- produces a
capture the wire half cannot tell from a real exchange, because classic pcap records both
directions of a netdev without a direction marker. That is not a hole to be patched here; it is
the reason `peer` exists and the reason a one-source run is spelled differently. The host kernel
either completed a handshake or it did not, and no frame the guest writes changes that.

Checksums are deliberately NOT verified: virtio checksum offload legitimately leaves them unset in
a capture taken on the guest link, so a checksum gate would fail working runs. The identity above
is bound by endpoints and sequence numbers instead, which offload does not touch.

THE CAPTURE'S CLOCK IS NOT THE HOST'S CLOCK -- MEASURED, TWICE
--------------------------------------------------------------------------------------------
An earlier version of this file bound the capture to this run by comparing pcap RECORD stamps
against `time.time()` read in this process. It rejected a capture that was FRESH, and because a
failed binding correctly suppresses the guest thresholds, the one run in which the guest really
did complete a handshake, exchange the pattern in BOTH directions and close reported nothing at
all. An invented failure costs no finding -- it drowns one.

The two clocks were measured here, on this machine, with QEMU 11.1.0, twice:

  the archived load run          its first record is stamped 3594.78 s BEHIND the wall clock at
  (2026-08-27 14:51)             which that run began -- and the run began before QEMU did, so
                                 that number is a lower bound on the gap
  an independent 40 s probe      its first record is stamped 3597.62 s BEHIND `time.time()` read
  (2026-08-27 15:10, PXE guest,  immediately before QEMU was started -- again a lower bound, for
  same QEMU, same host)          the same reason

Both gaps land within seconds of one hour, which is this host's DST offset, and the exact shape
of the bug does not matter: even if it were exactly 3600 s, "add an hour" is a correction that
holds for this timezone, this QEMU build and this side of the next DST transition, and goes
wrong silently everywhere else. `filter-dump` does not stamp with `time(NULL)` at write time.

The offset is not even the whole of it. In the archived capture the host's 24 B and the host's
FIN sit 12 us apart (frames 11 and 12) -- and the peer that produced them waited 10 s of host
wall time between the `send` and the `shutdown`. slirp had buffered both and released them the
instant the guest finished its handshake, so a record stamp does not date the host action behind
it at all. Comparing a record stamp with a host stamp is comparing across a meaning change, and
the fix for that is not a better constant -- it is to stop comparing them.

What binds a capture to THIS run instead: three statements, each of which stays on ONE clock.

  file     the capture FILE's mtime against the moment this run began. Both are `time.time()`
           on the host. The suite deletes the file before the boot, so a file last written
           before this run began is a leftover from an earlier one -- and an archived capture
           handed to a later run is exactly that. This is the binder that fails a replay.
  content  the client port the host kernel chose for THIS connection must be the source port of
           the SYN. A replay carries the ephemeral port of the run it came from.
  self     the record stamps are compared only WITH EACH OTHER. One filter-dump appending to one
           file stamps monotonically, so a backward jump means two captures in one file: an old
           one spliced in front of or behind a fresh one, which is the replay the file's mtime
           cannot see.

Each of the three is a statement about the CAPTURE, never about the guest, so a failure is
spelled `binding:` and suppresses the guest thresholds exactly as `identity:` does.

A FAILED MEASUREMENT IS NEVER SPELLABLE AS A FAILED GUEST
--------------------------------------------------------------------------------------------
A capture that was cut short, or snapped by a short snaplen, under-counts everything derived from
it. The old parser stopped at a truncated record with the comment "say so by stopping" and then
said nothing -- two truncated captures read green, and a third, cut earlier, failed at six
thresholds that all pointed at the guest and none at the capture. Truncation and snapping are now
NAMED outcomes, they fail the run by themselves, and they SUPPRESS the derived guest thresholds:
if the measurement did not happen, the guest has not been judged.

The one thing that must NOT become a failure is the opposite mistake. filter-dump appends a record
header and its frame in two separate writes, and this tool reads the file while QEMU is still
running -- so a short tail can simply mean the reader was early. A cut capture and a capture still
being written are separated by measuring (does the file grow?) rather than by assuming; see
`_read_capture`.
"""

import argparse
import base64
import os
import socket
import struct
import sys
import tempfile
import time
from collections import namedtuple

# --------------------------------------------------------------------------------------------
# THE THRESHOLDS STAND BEFORE THE MEASUREMENT -- and every one of them has a lower bound
# --------------------------------------------------------------------------------------------
#
# A one-sided comparison is green the moment the measurement fails: `x < limit` passes for x = 0,
# and 0 is what a broken capture, a missing file or an unparsed frame all produce. This repository
# has paid for that shape once already (`NOSEL_TEXT` stood at 0 because the build was broken, and
# the check reported PASS for "0 < 0x62000").
#
# So each of these is a MINIMUM, and zero can never satisfy one.
#
# There is deliberately no MIN_SYN and no MIN_SYNACK any more. Counting them was the weaker
# statement: a count of 1 is satisfied by any frame that happens to carry the flag byte and the
# port. The SYN and the answer that mirrors AND acknowledges it are now a precondition of reading
# the capture at all -- the identity check below -- which implies both counts and cannot be
# satisfied by a single fabricated frame.

MIN_PAYLOAD_TO_GUEST = 16  # host -> guest, in bytes of TCP payload
MIN_PAYLOAD_TO_HOST = 16  # guest -> host. A separate statement: "sent" is not "received".
MIN_FIN = 1  # per direction -- "closed cleanly" is not "stopped talking"
MIN_FRAMES = 8  # a capture below this is a failed capture, not a quiet link
MIN_CONN_FRAMES = 6  # ...and of those, this many must belong to the ONE identified connection

# --------------------------------------------------------------------------------------------
# BINDING A CAPTURE TO THIS RUN -- three constants, and not one of them crosses a clock
# --------------------------------------------------------------------------------------------
#
# `FRESH_SLACK_S` used to stand here with the comment "filter-dump stamps with the same wall
# clock we read". It does not; the measurement is in the docstring. What is left are two
# tolerances, each inside ONE clock.

# The capture FILE's mtime against `not_before`, both of them host wall clock. The slack covers
# mtime granularity and the moment between "this run began" and QEMU's first write -- not a
# second clock.
MTIME_SLACK_S = 5.0

# How far a record may be stamped BEFORE its predecessor before the file reads as two captures
# in one. Zero would be the honest bound for a single appending writer, but two netdev queues can
# be stamped in one order and written in the other, so a second of slack costs nothing: the gap
# between two runs of this suite is minutes.
MAX_CLOCK_REGRESSION_S = 1.0

# --------------------------------------------------------------------------------------------
# THE PEER'S PATIENCE IS A PARAMETER, BECAUSE 10 s WAS A THRESHOLD ON THE EMULATOR
# --------------------------------------------------------------------------------------------
#
# `connected` means slirp's hostfwd listener accepted -- NOT that the guest did. slirp accepts on
# the host side immediately and only then starts sending SYNs into the guest, so everything the
# peer writes sits in slirp's buffer until the guest completes the handshake. In the run this
# file was fixed for, the guest completed it 17.96 s of CAPTURE time after the first SYN, and by
# then the peer had long given up: its 24 B and its FIN reached the guest 12 us apart, both of
# them buffered while the 10 s ran out. The guest's own answer followed 0.42 s later.
#
# How much wall time those 17.96 s of capture clock are cannot be read off the archived file --
# macOS `cp` clones on APFS, so the copy's birth came from the original and its mtime from the
# copy, and the two do not bound anything. What IS knowable is the boot's own budget: the guest
# may answer at any moment until QEMU is killed, so a patience below that budget measures the
# emulator and not the stack. Hence the default rises to the connect deadline -- which the suite
# already derives from the boot budget -- and the constant below is only the floor for a caller
# that gives no deadline at all.
PEER_PATIENCE_S = 300.0

# A stack that answers only after seeing EOF is legal, and the capture that prompted this shows
# the host's FIN arriving 12 us behind the host's data -- the guest saw both at once. So the
# half-close is no longer a give-up reflex (see `run_peer`): once the patience is spent it is
# sent DELIBERATELY, as a probe, with its own short window. An answer that arrives only in that
# window is a different finding from an answer that never came, and it is reported as one.
PEER_EOF_GRACE_S = 15.0

# Waiting for the guest's FIN after our own. Its own name and its own budget, because "the guest
# never closed" and "the guest never spoke" are two findings.
PEER_CLOSE_PATIENCE_S = 30.0

# A send that blocks for ever is not a measurement either.
PEER_SEND_TIMEOUT_S = 30.0

# Payload is placed by sequence number, so a hostile or broken capture could name an offset far
# from the ISN. The window is a capacity, so it gets a NAME and a counter: segments outside it are
# not silently dropped, they are reported and they fail the run.
REASSEMBLY_WINDOW = 1 << 20

# The pattern. Written out here, byte for byte, and never imported. Non-palindromic on purpose:
# a palindrome round-trips even through a reversing bug.
PATTERN_TO_GUEST = b"CAPROCK-HOST-TO-GUEST-01"
PATTERN_TO_HOST = b"CAPROCK-GUEST-TO-HOST-01"

F_FIN, F_SYN, F_RST, F_PSH, F_ACK = 0x01, 0x02, 0x04, 0x08, 0x10


def note(msg):
    print(f"checknet: {msg}", flush=True)


def _mac(b):
    return ":".join(f"{x:02x}" for x in b)


def _ip(b):
    return ".".join(str(x) for x in b)


# --------------------------------------------------------------------------------------------
# peer -- a real TCP client on the host, talking to the stack inside the guest
# --------------------------------------------------------------------------------------------


def peer_patience_for(deadline_s, explicit=None):
    """How long the peer waits for the guest's answer. One place, so it can be gated.

    An explicit value wins. Otherwise `PEER_PATIENCE_S` is the floor and the connect deadline
    raises it: the deadline is the budget of the boot being watched, the guest may answer at any
    moment until QEMU is killed, and a patience below that budget gives up on a guest that is
    still allowed to run. That was the shape of the fixed 10 s.
    """
    if explicit is not None:
        return explicit
    return max(PEER_PATIENCE_S, deadline_s)


def _silence_reason(timed_out, closed, patience_s, half_closed, eof_grace_s, close_patience_s):
    """Why no bytes came, in the report's own words -- and never a claim that did not happen.

    Measured: with 300 s of patience and a guest that died two seconds in, this used to print
    "the guest sent nothing within 300 s plus 45 s after our FIN" for a run that had waited
    2.09 s. Naming a threshold that was never reached is the same family of defect as an invented
    failure -- it sends the reader after the wrong question.
    """
    if timed_out:
        waited = f"{patience_s:.0f} s"
        if half_closed:
            waited += f" plus {eof_grace_s + close_patience_s:.0f} s after our FIN"
        return f"connected, but the guest sent nothing within {waited}"
    if closed:
        return "connected, and the guest closed without sending anything -- the patience was"\
               " not spent"
    return "connected, and the read ended without bytes and without spending the patience"


def _read_answer(sock, have, end_monotonic):
    """Read toward the expected pattern until it is complete, the peer closes, or the deadline.

    Four separate facts, four separate return values: `(buf, closed, timed_out, error)`. "the
    guest closed", "I ran out of patience" and "the read itself failed" are three different
    findings, and a caller that got one boolean would have to guess which.

    The deadline is a TOTAL, not a per-recv timeout. A per-recv timeout is a threshold on the gap
    between bytes: a stack that dribbles one byte just inside every window holds the witness for
    ever, and a stack that answers in one burst after a long silence is failed. The inner
    `settimeout` is only a polling interval.
    """
    buf = have
    while len(buf) < len(PATTERN_TO_HOST):
        left = end_monotonic - time.monotonic()
        if left <= 0:
            return buf, False, True, ""
        sock.settimeout(min(left, 5.0))
        try:
            chunk = sock.recv(4096)
        except socket.timeout:
            continue  # nothing yet; only `end_monotonic` decides when patience is spent
        except OSError as e:
            return buf, False, False, f"receive failed after {len(buf)} B: {e}"
        if not chunk:
            return buf, True, False, ""
        buf += chunk
    return buf, False, False, ""


def run_peer(host, port, deadline_s, settle_s, patience_s=PEER_PATIENCE_S,
             eof_grace_s=PEER_EOF_GRACE_S, close_patience_s=PEER_CLOSE_PATIENCE_S):
    """Connect into the guest, exchange the pattern in both directions, close cleanly.

    Returns a dict of findings. Every failure mode gets its OWN name: "never reachable",
    "connected but silent", "answered only after our FIN" and "answered with the wrong bytes" are
    four different defects, only two of them are about the stack, and the first is an
    infrastructure problem.

    `local_port` and `started_at` are not diagnostics -- they are what binds the CAPTURE to this
    run: the port binds its contents, `started_at` binds the capture FILE's mtime. Without them a
    capture of any earlier run satisfies the wire half.

    `patience_s` is how long the guest may take to answer, and it is a PARAMETER because the old
    fixed 10 s was a threshold on how fast QEMU boots -- see `PEER_PATIENCE_S`.
    """
    out = {
        "connected": False,
        "sent": 0,
        "received": 0,
        "received_after_fin": 0,
        "answer_prefix_ok": False,
        "timed_out": False,
        "answered_only_after_fin": False,
        "half_closed": False,
        "pattern_ok": False,
        "clean_close": False,
        "attempts": 0,
        "local_port": None,
        "started_at": time.time(),
        "patience_s": patience_s,
        "reason": "",
    }

    # The guest boots while we are already trying. Retrying is not politeness -- without it the
    # measurement would be a race between QEMU's startup and ours, and a lost race looks exactly
    # like a broken stack.
    end = time.monotonic() + deadline_s
    sock = None
    while time.monotonic() < end:
        out["attempts"] += 1
        try:
            sock = socket.create_connection((host, port), timeout=2.0)
            out["connected"] = True
            break
        except OSError:
            time.sleep(settle_s)
    if not out["connected"]:
        out["reason"] = f"never reachable at {host}:{port} after {out['attempts']} attempts"
        return out

    # The port the host kernel chose for THIS connection. Under QEMU's slirp hostfwd the guest
    # sees the connection arrive from 10.0.2.2 with this very source port (slirp rewrites the
    # loopback address and leaves the port alone), and a tap bridge does not rewrite either.
    try:
        out["local_port"] = sock.getsockname()[1]
    except OSError:
        out["local_port"] = None

    try:
        try:
            sock.settimeout(PEER_SEND_TIMEOUT_S)
            sock.sendall(PATTERN_TO_GUEST)
            out["sent"] = len(PATTERN_TO_GUEST)
        except OSError as e:
            # A send that fails leaves `sent` short, and `sent` is judged. Without this the
            # exception would escape run_peer and the run would end with no verdict line at all.
            out["reason"] = f"send failed after {out['sent']} B: {e}"
            return out

        buf, closed, timed_out, err = _read_answer(sock, b"", time.monotonic() + patience_s)
        if err:
            out["reason"] = err
        before_fin = None  # how much had arrived when we half-closed, set at that moment

        # THE HALF-CLOSE IS NO LONGER A GIVE-UP REFLEX, and the capture is why. The old order
        # sent our FIN the moment the read timed out; on the wire that FIN was queued behind our
        # data in slirp and reached the guest 12 us after it, so the guest saw "here is my
        # pattern, and I am done" in one breath -- the host telling the guest to hurry, in the
        # very run that was meant to show the guest answering.
        #
        # So the order is now: data, then WAIT, and the FIN goes out either after the answer or
        # deliberately, once, when the patience is already spent. A stack that answers only on
        # EOF is legal and must not be failed by that change, hence the grace window -- and an
        # answer that arrives only inside it is its OWN finding, not a plain pass.
        if not buf and not closed and timed_out and not err:
            try:
                sock.shutdown(socket.SHUT_WR)
                out["half_closed"] = True
            except OSError as e:
                out["reason"] = out["reason"] or f"half-close failed: {e}"
            if out["half_closed"]:
                before_fin = len(buf)
                buf, closed, timed_out, err2 = _read_answer(
                    sock, buf, time.monotonic() + eof_grace_s
                )
                if err2:
                    out["reason"] = err2

        # Close from our side and give the guest room to send its FIN. `clean_close` is about the
        # shutdown handshake, not about the socket object going away.
        if not out["half_closed"] and not err:
            try:
                sock.shutdown(socket.SHUT_WR)
                out["half_closed"] = True
                before_fin = len(buf)
            except OSError:
                pass

        # WHATEVER ARRIVES NOW IS STILL THE GUEST'S ANSWER, and it is judged as one. The old
        # shape read once more for the FIN and, if bytes came instead, added them to `received`
        # while leaving `pattern_ok` at the verdict computed on the empty buffer -- so an answer
        # that arrived a moment late was reported as "the guest answered with the wrong bytes".
        # That is the worst kind of invented finding: it names the stack for the witness's own
        # impatience. Measured with a guest that answers at 20 s and a patience of 5 s.
        if not closed and not err:
            end_close = time.monotonic() + close_patience_s
            buf, closed, _spent, err_c = _read_answer(sock, buf, end_close)
            if err_c and not out["reason"]:
                out["reason"] = err_c
            if not closed and not err_c:
                try:
                    # The pattern is complete, so this read is only about the FIN -- and about
                    # anything the guest sent BEYOND the pattern, which is not a clean close.
                    sock.settimeout(max(0.05, end_close - time.monotonic()))
                    rest = sock.recv(4096)
                    if rest:
                        buf += rest
                    else:
                        closed = True
                except OSError:
                    pass
        out["clean_close"] = closed

        # ONE place where the answer is judged, and it runs after ALL the reading is done. Two
        # separate facts stay separate: `received`/`pattern_ok` say what came, `timed_out` says
        # whether patience ran out, and `answered_only_after_fin` says the answer needed our EOF.
        out["received"] = len(buf)
        out["timed_out"] = timed_out and not buf
        out["pattern_ok"] = buf[: len(PATTERN_TO_HOST)] == PATTERN_TO_HOST
        # "right bytes, too few of them" and "the wrong bytes" are two findings, and a truncated
        # answer used to be reported as the second. A stack that gets its first segment out and
        # then stops is a different defect from one that echoes rubbish.
        out["answer_prefix_ok"] = PATTERN_TO_HOST.startswith(buf) if buf else False
        if before_fin is not None:
            out["received_after_fin"] = len(buf) - before_fin
            out["answered_only_after_fin"] = before_fin == 0 and len(buf) > 0
        if buf and not out["pattern_ok"]:
            out["reason"] = (
                f"the guest's answer matches as far as it goes and then stops after"
                f" {len(buf)} of {len(PATTERN_TO_HOST)} B"
                if out["answer_prefix_ok"]
                else f"guest answered with unexpected bytes: {buf[:40]!r}"
            )
        elif not buf and not err:
            out["reason"] = _silence_reason(
                out["timed_out"], closed, patience_s, out["half_closed"],
                eof_grace_s, close_patience_s,
            )
    finally:
        try:
            sock.close()
        except OSError:
            pass
    return out



# --------------------------------------------------------------------------------------------
# pcap -- the wire, read independently
# --------------------------------------------------------------------------------------------
#
# QEMU's filter-dump writes classic libpcap. The parser is deliberately written out rather than
# taken from a library: a dependency the guest also uses is not a second source, and a library
# that silently skips a malformed frame would hide exactly the frame worth seeing.

PCAP_MAGIC_LE = 0xA1B2C3D4
PCAP_MAGIC_BE = 0xD4C3B2A1

# `ts` is what the capturing process stamped ON ITS OWN CLOCK -- measured here about an hour
# behind this host's wall clock, on two separate runs -- so it is a wall clock only in the sense
# that somebody's wall carries it. It is compared with other `ts` values and with nothing else.
# `caplen`/`origlen` are kept APART because
# their difference is the whole snaplen finding: a frame stored short still carries a truthful
# `origlen`, and comparing the two is the only way to notice that the payload counts are a lower
# bound.
Frame = namedtuple("Frame", "ts caplen origlen data")

# One decoded TCP segment. Every field the identity check needs is here; nothing is recomputed
# later from the raw bytes, so there is one reading of a frame and not two.
Seg = namedtuple("Seg", "ts src_mac dst_mac src_ip dst_ip sport dport seq ack flags payload")

# The connection, learned from the wire. Two ENDS, named separately -- the whole point is that
# they are different ends.
Identity = namedtuple(
    "Identity",
    "client_mac guest_mac client_ip guest_ip client_port guest_port client_isn guest_isn syn_ts",
)


def parse_pcap(path):
    """Read a classic libpcap file. Returns `(capture, error)`.

    Two separate facts, two separate places:

      `error`                       -- this file is not a capture I can read AT ALL.
      `capture["measurement_error"]` -- I read it, and what I read is INCOMPLETE.

    The second one used to be a bare `break` with the comment "the capture was cut, say so by
    stopping" -- and then it said nothing, returning the frames it had as if they were all of
    them. A capture cut after the handshake read green; a capture cut earlier failed at six
    thresholds that all named the guest. Both are the same defect in the measurement, and neither
    is a statement about the stack.
    """
    try:
        with open(path, "rb") as f:
            data = f.read()
            # `fstat` on the OPEN handle, not `os.stat` on the name: the mtime that binds this
            # capture to this run has to be the mtime of the bytes just read.
            mtime = os.fstat(f.fileno()).st_mtime
    except OSError as e:
        # A missing --pcap used to raise FileNotFoundError out of main: non-zero exit, but the one
        # failure path in the whole tool that printed no verdict line at all.
        return None, f"cannot read {path}: {e.strerror or e}"
    if len(data) < 24:
        return None, f"pcap too short ({len(data)} B) -- the capture did not start"
    (magic,) = struct.unpack("<I", data[:4])
    if magic == PCAP_MAGIC_LE:
        end = "<"
    elif magic == PCAP_MAGIC_BE:
        end = ">"
    else:
        return None, f"not a pcap: magic {magic:#010x}"
    _, _, _, _, snaplen, link = struct.unpack(end + "HHiIII", data[4:24])
    if link != 1:
        return None, f"link type {link} is not Ethernet -- refusing to interpret"

    frames = []
    off = 24
    truncated = None
    snapped = 0
    worst = None
    while off < len(data):
        if off + 16 > len(data):
            truncated = (
                f"the file ends {len(data) - off} B into a 16 B record header,"
                f" after {len(frames)} whole frame(s)"
            )
            break
        ts_s, ts_us, caplen, origlen = struct.unpack(end + "IIII", data[off : off + 16])
        off += 16
        if caplen > len(data) - off:
            truncated = (
                f"frame {len(frames)} claims {caplen} B and the file holds {len(data) - off}"
                f" -- the capture was cut mid-frame"
            )
            break
        if origlen != caplen:
            snapped += 1
            if worst is None or (origlen - caplen) > (worst[1] - worst[0]):
                worst = (caplen, origlen)
        frames.append(Frame(ts_s + ts_us / 1e6, caplen, origlen, data[off : off + caplen]))
        off += caplen

    problems = []
    if truncated is not None:
        problems.append("truncated -- " + truncated)
    if snapped:
        problems.append(
            f"snapped -- {snapped} frame(s) were stored shorter than they crossed the link"
            f" (worst {worst[0]} of {worst[1]} B, header snaplen {snaplen}); every payload count"
            f" below would be a LOWER bound, and a lower bound reads as a stack that sent too"
            f" little"
        )
    # The capture's own span, and the worst BACKWARD step in it. Both are quantities on the
    # capture's own clock and are never compared with ours -- that is the whole point of them.
    # One filter-dump appending to one file stamps monotonically, so a backward step is not a
    # slow guest, it is two captures in one file.
    regression = None
    for i in range(1, len(frames)):
        back = frames[i - 1].ts - frames[i].ts
        if back > 0 and (regression is None or back > regression[1]):
            regression = (i, back)
    cap = {
        "frames": frames,
        "snaplen": snaplen,
        "snapped": snapped,
        "truncated": truncated,
        "mtime": mtime,
        "first_ts": frames[0].ts if frames else None,
        "last_ts": frames[-1].ts if frames else None,
        "span": (frames[-1].ts - frames[0].ts) if frames else None,
        "regression": regression,
        "measurement_error": "; ".join(problems) if problems else None,
    }
    return cap, None


def check_binding(cap, not_before):
    """Bind this capture to THIS run. Returns `(error, applied)` -- two separate facts.

    `error`    a binder RAN and FAILED: this capture is not of this run, so nothing in it is
               evidence about the guest.
    `applied`  which binders ran at all. "bound and passed" and "never bound" are not the same
               state, and a report that cannot tell them apart lets a missing binder read as a
               satisfied one.

    Nothing here compares a pcap record stamp with the host's clock; the docstring measures what
    that cost. The file's mtime and `not_before` are both `time.time()` on the host, and the
    record stamps are only ever compared with each other.
    """
    applied = []
    if not_before is not None and cap.get("mtime") is not None:
        applied.append("file")
        if cap["mtime"] < not_before - MTIME_SLACK_S:
            return (
                f"the capture file was last written at {cap['mtime']:.3f}, before this run began"
                f" ({not_before:.3f}, slack {MTIME_SLACK_S:.1f} s) -- this run never wrote it, so"
                f" what is in it is an earlier run's capture",
                applied,
            )
    if cap.get("frames"):
        applied.append("self")
        reg = cap.get("regression")
        if reg is not None and reg[1] > MAX_CLOCK_REGRESSION_S:
            return (
                f"record {reg[0]} is stamped {reg[1]:.3f} s BEFORE its predecessor (tolerated:"
                f" {MAX_CLOCK_REGRESSION_S:.1f} s) -- one filter-dump appending to one file stamps"
                f" monotonically, so this file holds two captures spliced together, not one",
                applied,
            )
    return None, applied


def _read_capture(path, settle_s, attempts=3, sleep=time.sleep):
    """Read the capture, and tell a CUT capture apart from one that is still being WRITTEN.

    QEMU's filter-dump appends the record header and the frame body in two separate `write()`
    calls, and `checknet` reads the file while QEMU is still running -- the peer half needs a live
    guest. A reader that arrives between those two writes sees exactly the byte pattern of a
    truncated file. Failing on that would be an INVENTED failure, and this repository names that
    the worse direction: an invented failure costs no finding, it drowns one.

    So the two cases are separated by MEASUREMENT rather than by a guess: a file that is still
    being written GROWS. If the tail is still short after a settle and the file has not grown, the
    capture really was cut, and that is a named failure. The retry cannot hide a real cut -- a cut
    file has a stable size, so the very first re-read confirms it.
    """
    cap, err = parse_pcap(path)
    last_size = None
    for _ in range(attempts):
        if err or cap["truncated"] is None or settle_s <= 0:
            return cap, err
        size = os.path.getsize(path)
        if last_size is not None and size == last_size:
            return cap, err  # not growing and still short: cut, not racing
        note(
            f"capture ends mid-record at {size} B -- re-reading after {settle_s:.2f} s to tell a"
            " cut file from a writer that is still running"
        )
        sleep(settle_s)
        last_size = size
        cap, err = parse_pcap(path)
    return cap, err


def _decode(fr):
    """One frame -> one `Seg`, or None if it is not IPv4/TCP or is too short to read.

    Refuses IP fragments other than the first: a later fragment carries no TCP header, and
    reading its payload bytes as one would invent ports out of user data.
    """
    d = fr.data
    if len(d) < 14:
        return None
    if struct.unpack(">H", d[12:14])[0] != 0x0800:
        return None
    ip = d[14:]
    if len(ip) < 20:
        return None
    ihl = (ip[0] & 0x0F) * 4
    if ihl < 20 or len(ip) < ihl:
        return None
    if ip[9] != 6:  # not TCP
        return None
    if struct.unpack(">H", ip[6:8])[0] & 0x1FFF:  # a non-first fragment
        return None
    total_len = struct.unpack(">H", ip[2:4])[0]
    tcp = ip[ihl:total_len] if total_len and total_len <= len(ip) else ip[ihl:]
    if len(tcp) < 20:
        return None
    doff = (tcp[12] >> 4) * 4
    if doff < 20 or len(tcp) < doff:
        return None
    sport, dport = struct.unpack(">HH", tcp[0:4])
    seq, ack = struct.unpack(">II", tcp[4:12])
    return Seg(
        ts=fr.ts,
        src_mac=d[6:12],
        dst_mac=d[0:6],
        src_ip=ip[12:16],
        dst_ip=ip[16:20],
        sport=sport,
        dport=dport,
        seq=seq,
        ack=ack,
        flags=tcp[13],
        payload=tcp[doff:],
    )


def _identify(segs, port, peer_client_port):
    """Learn the ONE connection this capture is allowed to be about. Returns (Identity, error).

    Every rejection carries the numbers that produced it. `error` is a MEASUREMENT statement, not
    a statement about the stack: if no connection can be named, nothing that follows is evidence
    about the guest, and the thresholds are not evaluated at all.
    """
    whys = []
    for i, s in enumerate(segs):
        if not (s.flags & F_SYN) or (s.flags & F_ACK) or s.dport != port:
            continue

        # Two ends that are the same end never crossed a link. This is the capture in which only
        # the port fields alternate -- it satisfied every threshold of the port-only rule.
        if s.src_mac == s.dst_mac or s.src_ip == s.dst_ip or s.sport == s.dport:
            whys.append(
                f"the SYN to port {port} has identical endpoints"
                f" (mac {_mac(s.src_mac)} -> {_mac(s.dst_mac)},"
                f" ip {_ip(s.src_ip)} -> {_ip(s.dst_ip)}, port {s.sport} -> {s.dport})"
                f" -- nothing crossed a link"
            )
            continue

        # Bind the capture's CONTENT to THIS run: the ephemeral port the host kernel chose is
        # not something a replay of an earlier run carries, unless the same port comes up again.
        # The other two binders are in `check_binding`, because they are statements about the
        # FILE and not about a connection.
        #
        # What is NOT here any more is `s.ts < not_before`. That compared a record stamp with the
        # host's wall clock; the two were measured at least 3594.78 s apart on the archived run
        # and at least 3597.62 s apart on an independent probe an hour later -- both within
        # seconds of this host's DST offset. It rejected a capture that was FRESH, and the
        # rejection suppressed the guest thresholds, so the one run that had a complete exchange
        # in it reported nothing at all.
        if peer_client_port is not None and s.sport != peer_client_port:
            whys.append(
                f"the SYN to port {port} comes from client port {s.sport}, but the peer this run"
                f" opened used {peer_client_port} -- this is not the connection that was measured"
            )
            continue

        # The answer must MIRROR the SYN and must acknowledge THIS SYN. Reversed MACs, reversed
        # IPs, reversed ports and `ack == seq + 1`: that combination is what the host kernel's
        # three-way handshake actually forces out of the guest, and it is the half a guest cannot
        # fake with one flag byte.
        want = (s.dst_mac, s.src_mac, s.dst_ip, s.src_ip, s.dport, s.sport)
        mirrored = False
        for t in segs[i + 1 :]:
            if not (t.flags & F_SYN and t.flags & F_ACK):
                continue
            if (t.src_mac, t.dst_mac, t.src_ip, t.dst_ip, t.sport, t.dport) != want:
                continue
            mirrored = True
            if t.ack != (s.seq + 1) & 0xFFFFFFFF:
                whys.append(
                    f"a SYN-ACK mirrors the SYN but acknowledges {t.ack} where this SYN's"
                    f" sequence number {s.seq} demands {(s.seq + 1) & 0xFFFFFFFF}"
                    f" -- it is not an answer to this SYN"
                )
                continue
            return (
                Identity(
                    client_mac=s.src_mac,
                    guest_mac=s.dst_mac,
                    client_ip=s.src_ip,
                    guest_ip=s.dst_ip,
                    client_port=s.sport,
                    guest_port=s.dport,
                    client_isn=s.seq,
                    guest_isn=t.seq,
                    syn_ts=s.ts,
                ),
                None,
            )
        if not mirrored:
            whys.append(
                f"no SYN-ACK mirrors the SYN {_ip(s.src_ip)}:{s.sport} -> {_ip(s.dst_ip)}:{s.dport}"
                f" (wanted mac {_mac(s.dst_mac)} -> {_mac(s.src_mac)},"
                f" ip {_ip(s.dst_ip)} -> {_ip(s.src_ip)}, port {s.dport} -> {s.sport})"
                f" -- this is the half the guest cannot fake"
            )
    if not whys:
        whys.append(
            f"no SYN to port {port} anywhere in the capture ({len(segs)} TCP segment(s) read)"
        )
    return None, "; ".join(dict.fromkeys(whys))


def _flag_letters(flags):
    """The flag byte as letters, so a census line can be read without a decoder."""
    return "".join(
        letter
        for bit, letter in ((F_SYN, "S"), (F_ACK, "A"), (F_PSH, "P"), (F_FIN, "F"), (F_RST, "R"))
        if flags & bit
    ) or "-"


def _census(segs, limit=6):
    """What IS in this capture, in the reader's terms, for when no connection could be named.

    "Nothing happened" is a finding about the GUEST. "Something happened and I could not name it"
    is a finding about the WITNESS. The post-identity view spells the second as the first -- it
    prints `on-conn=0 syn=0` for a capture that may hold a complete exchange -- so the numbers
    that are NOT filtered by the identity get printed too, and they are counted here.
    """
    tally = {}
    for s in segs:
        key = (_ip(s.src_ip), s.sport, _ip(s.dst_ip), s.dport)
        e = tally.setdefault(key, {"frames": 0, "flags": 0, "payload": 0})
        e["frames"] += 1
        e["flags"] |= s.flags
        e["payload"] += len(s.payload)
    ranked = sorted(tally.items(), key=lambda kv: (-kv[1]["frames"], kv[0]))
    lines = [
        f"{si}:{sp} -> {di}:{dp}  {e['frames']} frame(s),"
        f" flags {_flag_letters(e['flags'])}, {e['payload']} B payload"
        for (si, sp, di, dp), e in ranked[:limit]
    ]
    if len(ranked) > limit:
        lines.append(f"... and {len(ranked) - limit} further endpoint pair(s)")
    return lines


class Stream:
    """One direction's payload, placed BY SEQUENCE NUMBER rather than by arrival.

    `PATTERN in payload` per frame measured segment size, not the property: a stack that pushes
    its 24-byte answer in three segments is legal -- `run_peer` says so in as many words -- and
    would have failed the wire half while passing the peer half. Placing bytes by sequence also
    makes a retransmission harmless instead of a double count.
    """

    def __init__(self, isn):
        self.isn = isn
        self.at = {}
        self.raw = 0  # every payload byte seen, retransmissions included
        self.out_of_window = 0

    def add(self, seq, data):
        self.raw += len(data)
        rel = (seq - self.isn - 1) & 0xFFFFFFFF
        if rel > 0x7FFFFFFF:
            return  # before the ISN: an ACK of old data, not this stream's bytes
        if rel + len(data) > REASSEMBLY_WINDOW:
            self.out_of_window += 1
            return
        for k, b in enumerate(data):
            self.at[rel + k] = b

    def assembled(self):
        """The contiguous run from the first payload byte. A hole stops it, and that is the
        honest answer: bytes behind a hole did not arrive as a stream."""
        out = bytearray()
        k = 0
        while k in self.at:
            out.append(self.at[k])
            k += 1
        return bytes(out)


def tcp_stats(frames, port, *, peer_client_port=None):
    """Count what crossed the link ON ONE NAMED CONNECTION. Every field is a separate statement.

    Direction is decided by the learned 6-tuple, not by the TCP port: filter-dump records both
    directions on one link, and a port-only split calls any frame with the right port number an
    answer from the guest -- including a frame the guest never sent.

    `arp`, `ipv4` and `tcp` are totals of the whole capture. They are printed and they gate
    NOTHING on purpose: a link may legitimately carry no ARP at all, so a threshold on them would
    measure the environment. Everything that IS judged is counted per connection.
    """
    s = {
        "frames": len(frames),
        "ipv4": 0,
        "arp": 0,
        "tcp": 0,
        "conn_frames": 0,
        "off_conn": 0,
        "syn": 0,
        "synack": 0,
        "fin_to_guest": 0,
        "fin_to_host": 0,
        "rst": 0,
        "payload_to_guest": 0,
        "payload_to_host": 0,
        "raw_to_guest": 0,
        "raw_to_host": 0,
        "out_of_window": 0,
        "pattern_to_guest_seen": False,
        "pattern_to_host_seen": False,
        "identity": None,
        "identity_error": None,
        # Totals of the whole capture, kept OUTSIDE the identity so they survive its failure.
        # They gate nothing -- they are what lets a reader tell an empty link from an unnamed one.
        "census": [],
        "endpoint_pairs": 0,
    }
    segs = []
    for fr in frames:
        if len(fr.data) < 14:
            continue
        ethertype = struct.unpack(">H", fr.data[12:14])[0]
        if ethertype == 0x0806:
            s["arp"] += 1
            continue
        if ethertype != 0x0800:
            continue
        s["ipv4"] += 1
        seg = _decode(fr)
        if seg is None:
            continue
        s["tcp"] += 1
        segs.append(seg)

    s["census"] = _census(segs)
    s["endpoint_pairs"] = len({(g.src_ip, g.sport, g.dst_ip, g.dport) for g in segs})

    idty, err = _identify(segs, port, peer_client_port)
    if err is not None:
        s["identity_error"] = err
        return s
    s["identity"] = idty

    fwd = (idty.client_mac, idty.guest_mac, idty.client_ip, idty.guest_ip,
           idty.client_port, idty.guest_port)
    rev = (idty.guest_mac, idty.client_mac, idty.guest_ip, idty.client_ip,
           idty.guest_port, idty.client_port)
    to_guest = Stream(idty.client_isn)
    to_host = Stream(idty.guest_isn)

    for seg in segs:
        key = (seg.src_mac, seg.dst_mac, seg.src_ip, seg.dst_ip, seg.sport, seg.dport)
        if key == fwd:
            forward = True
        elif key == rev:
            forward = False
        else:
            # Carries one of the two ports but belongs to another connection, another station, or
            # another run. Reported, never counted -- this is the frame the port-only rule
            # credited to the guest.
            if port in (seg.sport, seg.dport):
                s["off_conn"] += 1
            continue
        s["conn_frames"] += 1
        if seg.flags & F_SYN and not seg.flags & F_ACK:
            s["syn"] += 1
        if seg.flags & F_SYN and seg.flags & F_ACK:
            s["synack"] += 1
        if seg.flags & F_RST:
            s["rst"] += 1
        if seg.flags & F_FIN:
            s["fin_to_guest" if forward else "fin_to_host"] += 1
        if seg.payload:
            (to_guest if forward else to_host).add(seg.seq, seg.payload)

    asm_g, asm_h = to_guest.assembled(), to_host.assembled()
    s["payload_to_guest"] = len(asm_g)
    s["payload_to_host"] = len(asm_h)
    s["raw_to_guest"] = to_guest.raw
    s["raw_to_host"] = to_host.raw
    s["out_of_window"] = to_guest.out_of_window + to_host.out_of_window
    s["pattern_to_guest_seen"] = PATTERN_TO_GUEST in asm_g
    s["pattern_to_host_seen"] = PATTERN_TO_HOST in asm_h
    return s


# --------------------------------------------------------------------------------------------
# the verdict
# --------------------------------------------------------------------------------------------


def judge(peer, wire):
    """Return (ok, [failure lines]). Each threshold is named with the number that missed it.

    A MEASUREMENT fault comes first and SUPPRESSES the wire thresholds. That order is the point:
    six lines that all name the guest, produced by a capture that was cut in half, are six
    invented failures -- and invented failures do not cost a finding, they drown it.
    """
    blocking = []
    bad = []

    if peer is not None:
        if not peer["connected"]:
            bad.append(f"peer: {peer['reason']}")
        else:
            if peer["sent"] != len(PATTERN_TO_GUEST):
                bad.append(
                    f"peer: sent {peer['sent']} B of the {len(PATTERN_TO_GUEST)} B pattern"
                    + (f" ({peer['reason']})" if peer["reason"] else "")
                )
            # SILENCE AND WRONG BYTES ARE TWO FINDINGS, and only the second is about the stack.
            # The old shape spent three lines on one fact: a guest that had not got there yet
            # produced "0 B received", "not the expected pattern" AND "no orderly FIN" -- and the
            # last two are verdicts on bytes that do not exist. The threshold below is still a
            # MINIMUM and zero still fails it; what stops is deriving further guest findings from
            # a measurement that did not happen.
            if peer["received"] < MIN_PAYLOAD_TO_HOST:
                if peer["received"]:
                    line = f"peer: guest sent {peer['received']} B"
                elif peer["timed_out"]:
                    # The patience is named because it is the threshold that was applied, and
                    # because a run that missed it wants to know whether it missed the stack or
                    # the emulator.
                    line = (
                        f"peer: the guest sent nothing within {peer.get('patience_s', 0.0):.0f} s"
                        " -- SILENCE, which is a different finding from wrong bytes"
                    )
                elif peer["clean_close"]:
                    line = "peer: the guest closed the connection without sending anything"
                else:
                    line = "peer: no bytes arrived, and the read did not run out of patience"
                bad.append(
                    line + f"; required >= {MIN_PAYLOAD_TO_HOST} B"
                    + (f" ({peer['reason']})" if peer["reason"] else "")
                )
            if peer["received"] and not peer["pattern_ok"]:
                bad.append(
                    "peer: the guest's answer is not the pattern -- "
                    + (
                        "it matches as far as it goes and then stops"
                        if peer.get("answer_prefix_ok")
                        else "the bytes differ"
                    )
                )
            if peer["received"] and not peer["clean_close"]:
                bad.append("peer: the connection did not close cleanly (no orderly FIN from guest)")

    if wire is not None:
        if wire.get("measurement_error"):
            blocking.append(
                f"capture: {wire['measurement_error']}"
                " -- the wire was NOT measured, and a failed measurement is not a failed guest"
            )
        elif wire.get("binding_error"):
            # Between "I could not read it" and "I read it and could not name a connection in it"
            # sits "I read it and it is not this run's". Its own name, because the reader's next
            # move is different: a stale capture is a question about the harness, an unnamed one
            # a question about the link.
            blocking.append(
                f"binding: {wire['binding_error']}"
                " -- this capture is not bound to this run, so nothing in it is evidence about"
                " the guest"
            )
        elif wire.get("identity_error"):
            blocking.append(
                f"identity: {wire['identity_error']}"
                " -- no connection could be named, so nothing in this capture is evidence"
                " about the guest"
            )
        else:
            if wire["frames"] < MIN_FRAMES:
                bad.append(f"wire: {wire['frames']} frames captured, required >= {MIN_FRAMES}")
            if wire["conn_frames"] < MIN_CONN_FRAMES:
                bad.append(
                    f"wire: {wire['conn_frames']} frames on the identified connection,"
                    f" required >= {MIN_CONN_FRAMES} ({wire['off_conn']} carried the port but"
                    f" belonged to something else)"
                )
            if wire["payload_to_guest"] < MIN_PAYLOAD_TO_GUEST:
                bad.append(
                    f"wire: {wire['payload_to_guest']} B host->guest,"
                    f" required >= {MIN_PAYLOAD_TO_GUEST}"
                )
            if wire["payload_to_host"] < MIN_PAYLOAD_TO_HOST:
                bad.append(
                    f"wire: {wire['payload_to_host']} B guest->host,"
                    f" required >= {MIN_PAYLOAD_TO_HOST}"
                )
            if not wire["pattern_to_guest_seen"]:
                bad.append("wire: the host's pattern never appeared on the link")
            if not wire["pattern_to_host_seen"]:
                bad.append("wire: the guest's pattern never appeared on the link")
            if wire["fin_to_guest"] < MIN_FIN or wire["fin_to_host"] < MIN_FIN:
                bad.append(
                    f"wire: FIN host->guest {wire['fin_to_guest']}, guest->host"
                    f" {wire['fin_to_host']}, required >= {MIN_FIN} each"
                )
            if wire["rst"]:
                # A connection that ends in a reset was not closed, it was torn down. The peer
                # half cannot see this: its own socket was already shut when the RST arrived.
                bad.append(
                    f"wire: {wire['rst']} RST on the connection -- it did not end, it was torn down"
                )
            if wire["out_of_window"]:
                bad.append(
                    f"wire: {wire['out_of_window']} segment(s) outside the"
                    f" {REASSEMBLY_WINDOW} B reassembly window -- their bytes were not counted"
                )

    return (not blocking and not bad), blocking + bad


def verdict_line(peer, wire):
    """The green line, spelled by the NUMBER of sources that stood behind it.

    `ALL PASS` means two. One source prints `SINGLE-SOURCE PASS`, which deliberately does not
    contain the string `ALL PASS`: a suite that greps for the acceptance must not be satisfiable
    by half a witness, and a `-object filter-dump` that fails to attach is exactly how a run
    silently becomes half a witness.
    """
    if peer is not None and wire is not None:
        return (
            "checknet: ALL PASS (two independent sources: a real TCP peer on the host,"
            " and the captured link parsed independently)"
        )
    only = (
        "a real TCP peer on the host"
        if peer is not None
        else "the captured link, parsed independently"
    )
    return f"checknet: SINGLE-SOURCE PASS ({only} -- ONE source, deliberately not an acceptance)"


# --------------------------------------------------------------------------------------------
# the speech test -- a checker that judges ABSENCE must prove it can speak
# --------------------------------------------------------------------------------------------
#
# `checknet` decides that something did NOT happen. That verdict is worthless unless the parser
# can be shown to find the thing when it IS there -- an empty capture and a parser that reads
# nothing produce the same silence, and silence must never read as success.
#
# So the self-test runs in BOTH directions: a synthetic capture that satisfies every threshold
# must pass, and the same capture with exactly one property removed must fail AT THAT PROPERTY.
# One-directional self-tests prove only that the checker can say yes.
#
# The positive control itself is the part that failed before. The old `_synth_frame` gave EVERY
# frame -- the SYN-ACK included -- one MAC pair and `10.0.2.2 -> 10.0.2.15`. That capture cannot
# occur on a link: the guest's answer would have to leave the guest addressed FROM the host. It
# passed, and a positive control that is physically impossible proves nothing about the checker.
# The builder below gives each direction its own MAC pair, its own IP pair and its own sequence
# space, and the SYN-ACK acknowledges the SYN it answers.

_HOST_MAC = bytes.fromhex("52550a000202")  # QEMU's gateway side of the link
_GUEST_MAC = bytes.fromhex("525400123456")  # the NIC inside the guest
_OTHER_MAC = bytes.fromhex("525400aabbcc")  # a third station on the same link
_HOST_IP = bytes([10, 0, 2, 2])
_GUEST_IP = bytes([10, 0, 2, 15])
_T0 = 1700000000.0
_CLIENT_PORT = 40000

# --------------------------------------------------------------------------------------------
# THE POSITIVE CONTROL THAT IS NOT SYNTHETIC -- the archived capture of a REAL run
# --------------------------------------------------------------------------------------------
#
# 1444 bytes, recorded byte for byte out of
# `build/diag/load-netwitness-20260827-150027-512M.pcap`: 18 frames, 9 ARP and 9 TCP, a complete
# three-way handshake 10.0.2.2:50133 <-> 10.0.2.15:7777, 24 B of payload in EACH direction, and a
# FIN from each side -- everything the feature exists to show. It is in here because the binding
# this file used to carry REJECTED it: the
# record stamps sit 3594.78 s behind the host clock of the very run that produced them, and that
# rejection then suppressed every guest threshold, so the one run that had the whole exchange in
# it reported nothing at all.
#
# A binding that cannot be shown to ACCEPT a real run is not a binding, it is an outage. The
# bytes travel with the test rather than being read from build/diag, so the case cannot quietly
# turn into a skip when that directory is cleaned.
REAL_CAPTURE_B64 = (
    "1MOyoQIABAAAAAAAAAAAAAAAAQABAAAAzCSQagTqDgA8AAAAPAAAAP///////1JVCgACAggGAAEIAAYEAAFSVQoAAgIK"
    "AAICAAAAAAAACgACDwAAAAAAAAAAAAAAAAAAAAAAAM0kkGp+6gcAPAAAADwAAAD///////9SVAASNFYIBgABCAAGBAAB"
    "UlQAEjRWCgACDwAAAAAAAAoAAgIAAAAAAAAAAAAAAAAAAAAAAADNJJBqieoHAEAAAABAAAAAUlQAEjRWUlUKAAICCAYA"
    "AQgABgQAAlJVCgACAgoAAgJSVAASNFYKAAIPAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM0kkGqU6gcAPAAAADwAAABSVAAS"
    "NFZSVQoAAgIIAEUAACwAAAAAQAZivAoAAgIKAAIPw9UeYQAA+gEAAAAAYAL//6PdAAACBAW0AADSJJBqHQYIADwAAAA8"
    "AAAAUlQAEjRWUlUKAAICCABFAAAsAAEAAEAGYrsKAAICCgACD8PVHmEAAPoBAAAAAGAC//+j3QAAAgQFtAAA3iSQaqUc"
    "CAA8AAAAPAAAAFJUABI0VlJVCgACAggARQAALAACAABABmK6CgACAgoAAg/D1R5hAAD6AQAAAABgAv//o90AAAIEBbQA"
    "AN4kkGpsLAsAPAAAADwAAAD///////9SVAASNFYIBgABCAAGBAABUlQAEjRWCgACD////////woAAgIAAAAAAAAAAAAA"
    "AAAAAAAAAADeJJBqfiwLAEAAAABAAAAAUlQAEjRWUlUKAAICCAYAAQgABgQAAlJVCgACAgoAAgJSVAASNFYKAAIPAAAA"
    "AAAAAAAAAAAAAAAAAAAAAAAAAN4kkGpdYA4APAAAADwAAABSVQoAAgJSVAASNFYIAEUAACwAAEAAQAYivAoAAg8KAAIC"
    "HmHD1aoc6q0AAPoCYBIFvglEAAACBAW0AADeJJBqbGAOADwAAAA8AAAAUlQAEjRWUlUKAAICCABFAAAoAAMAAEAGYr0K"
    "AAICCgACD8PVHmEAAPoCqhzqrlAQ//8mvwAAAAAAAAAA3iSQaoFgDgBOAAAATgAAAFJUABI0VlJVCgACAggARQAAQAAE"
    "AABABmKkCgACAgoAAg/D1R5hAAD6Aqoc6q5QGP//z24AAENBUFJPQ0stSE9TVC1UTy1HVUVTVC0wMd4kkGqNYA4APAAA"
    "ADwAAABSVAASNFZSVQoAAgIIAEUAACgABQAAQAZiuwoAAgIKAAIPw9UeYQAA+hqqHOquUBH//yamAAAAAAAAAADfJJBq"
    "C4kFAE4AAABOAAAAUlUKAAICUlQAEjRWCABFAABAAABAAEAGIqgKAAIPCgACAh5hw9WqHOquAAD6G1ARBcK8pwAAQ0FQ"
    "Uk9DSy1HVUVTVC1UTy1IT1NULTAx3ySQajWJBQA8AAAAPAAAAFJUABI0VlJVCgACAggARQAAKAAGAABABmK6CgACAgoA"
    "Ag/D1R5hAAD6G6oc6sdQEP//Jo0AAAAAAAAAAOEkkGrEBg8APAAAADwAAAD///////9SVAASNFYIBgABCAAGBAABUlQA"
    "EjRWCgACDwAAAAAAAAoAAgIAAAAAAAAAAAAAAAAAAAAAAADhJJBq0AYPAEAAAABAAAAAUlQAEjRWUlUKAAICCAYAAQgA"
    "BgQAAlJVCgACAgoAAgJSVAASNFYKAAIPAAAAAAAAAAAAAAAAAAAAAAAAAAAAAOIkkGoD1QAAPAAAADwAAAD///////9S"
    "VAASNFYIBgABCAAGBAABUlQAEjRWCgACDwAAAAAAAAoAAgIAAAAAAAAAAAAAAAAAAAAAAADiJJBqENUAAEAAAABAAAAA"
    "UlQAEjRWUlUKAAICCAYAAQgABgQAAlJVCgACAgoAAgJSVAASNFYKAAIPAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="
)
# The three numbers that run itself produced -- two from the witness's own log, one from the
# capture file's mtime. All three are host wall clock; the record stamps inside the file are not.
REAL_RUN_STARTED_AT = 1787835095.754  # `time.time()` in the witness when the peer half began
REAL_CAPTURE_MTIME = 1787835627.828  # its last write -- here the suite's archiving `cp`,
#                                      on the LIVE file QEMU's last frame; either is after
#                                      the run began, which is all the binder asks
REAL_CLIENT_PORT = 50133  # the ephemeral port the host kernel chose for that connection


def _synth_frame(src_mac, dst_mac, src_ip, dst_ip, sport, dport, seq, ack, flags, payload=b""):
    """One Ethernet/IPv4/TCP frame, built by hand. Nothing here is shared with the parser."""
    tcp = struct.pack(">HHIIBBHHH", sport, dport, seq, ack, (5 << 4), flags, 8192, 0, 0) + payload
    ip = struct.pack(">BBHHHBBH", 0x45, 0, 20 + len(tcp), 1, 0, 64, 6, 0) + src_ip + dst_ip
    return dst_mac + src_mac + b"\x08\x00" + ip + tcp


class _Link:
    """A capture of ONE connection with two DIFFERENT ends and real sequence arithmetic."""

    def __init__(
        self,
        guest_port,
        *,
        client_port=_CLIENT_PORT,
        t0=_T0,
        client_mac=_HOST_MAC,
        guest_mac=_GUEST_MAC,
        client_ip=_HOST_IP,
        guest_ip=_GUEST_IP,
        client_isn=0x11110000,
        guest_isn=0x22220000,
    ):
        self.guest_port = guest_port
        self.client_port = client_port
        self.client_mac, self.guest_mac = client_mac, guest_mac
        self.client_ip, self.guest_ip = client_ip, guest_ip
        self.client_isn, self.guest_isn = client_isn, guest_isn
        self.c_seq, self.g_seq = client_isn, guest_isn
        self.t = t0
        self.frames = []

    def _emit(self, src_mac, dst_mac, src_ip, dst_ip, sport, dport, seq, ack, flags, payload):
        self.t += 0.001
        data = _synth_frame(
            src_mac, dst_mac, src_ip, dst_ip, sport, dport,
            seq & 0xFFFFFFFF, ack & 0xFFFFFFFF, flags, payload,
        )
        self.frames.append(Frame(self.t, len(data), len(data), data))

    def to_guest(self, flags, payload=b"", *, src_mac=None, sport=None, seq=None, advance=True):
        seq = self.c_seq if seq is None else seq
        self._emit(
            src_mac or self.client_mac, self.guest_mac, self.client_ip, self.guest_ip,
            self.client_port if sport is None else sport, self.guest_port,
            seq, self.g_seq, flags, payload,
        )
        if advance:
            self.c_seq += len(payload) + (1 if flags & (F_SYN | F_FIN) else 0)

    def to_host(self, flags, payload=b"", *, src_mac=None, dport=None, seq=None, ack=None,
                advance=True):
        seq = self.g_seq if seq is None else seq
        self._emit(
            src_mac or self.guest_mac, self.client_mac, self.guest_ip, self.client_ip,
            self.guest_port, self.client_port if dport is None else dport,
            seq, self.c_seq if ack is None else ack, flags, payload,
        )
        if advance:
            self.g_seq += len(payload) + (1 if flags & (F_SYN | F_FIN) else 0)


def _play(
    link,
    *,
    synack=True,
    fin=True,
    rst=False,
    pad=0,
    host_payload=PATTERN_TO_GUEST,
    guest_payload=PATTERN_TO_HOST,
    segment=0,
    retransmit=False,
    synack_ack=None,
    synack_src_mac=None,
    guest_data_src_mac=None,
):
    """Play a whole exchange onto a link. Every knob removes or bends exactly ONE property."""
    link.to_guest(F_SYN)
    if synack:
        link.to_host(F_SYN | F_ACK, ack=synack_ack, src_mac=synack_src_mac)
    link.to_guest(F_ACK)
    if host_payload:
        link.to_guest(F_PSH | F_ACK, host_payload)
    if guest_payload:
        if segment:
            for k in range(0, len(guest_payload), segment):
                link.to_host(F_PSH | F_ACK, guest_payload[k : k + segment],
                             src_mac=guest_data_src_mac)
        else:
            link.to_host(F_PSH | F_ACK, guest_payload, src_mac=guest_data_src_mac)
        if retransmit:
            link.to_host(F_PSH | F_ACK, guest_payload, seq=link.guest_isn + 1, advance=False)
    link.to_guest(F_ACK)
    for _ in range(pad):
        link.to_guest(F_ACK)
    if fin:
        link.to_guest(F_FIN | F_ACK)
        link.to_host(F_FIN | F_ACK)
        link.to_guest(F_ACK)
    if rst:
        link.to_host(F_RST | F_ACK)
    return link.frames


def _pcap_bytes(frames, endian="<"):
    # The magic is one VALUE written in the file's own byte order -- `PCAP_MAGIC_BE` is what that
    # value looks like when a little-endian reader reads a big-endian file, not what a big-endian
    # writer stores. Writing the swapped constant here produced a file whose header parsed as
    # little-endian and whose records did not; the case below caught it.
    hdr = struct.pack(endian + "IHHiIII", PCAP_MAGIC_LE, 2, 4, 0, 0, 65535, 1)
    body = b""
    for f in frames:
        sec = int(f.ts)
        usec = int(round((f.ts - sec) * 1e6))
        body += struct.pack(endian + "IIII", sec, usec, f.caplen, f.origlen) + f.data
    return hdr + body


def _pad(frames, index, to=60):
    """Model the Ethernet minimum: the NIC pads a short frame, and those bytes are NOT payload.

    They land exactly where the TCP payload would, so a parser that ignored the IP total length
    would count them -- and six bytes of zeroes placed at the start of the guest's stream do not
    fail loudly, they shift the pattern.
    """
    out = list(frames)
    f = out[index]
    if len(f.data) >= to:
        return out
    data = f.data + b"\x00" * (to - len(f.data))
    out[index] = Frame(f.ts, len(data), len(data), data)
    return out


def _snap(frames, index, keep):
    """Model a short snaplen: the record stores `keep` bytes and still reports the true length."""
    out = list(frames)
    f = out[index]
    out[index] = Frame(f.ts, keep, f.origlen, f.data[:keep])
    return out


def _green_peer():
    return {
        "connected": True,
        "sent": len(PATTERN_TO_GUEST),
        "received": len(PATTERN_TO_HOST),
        "received_after_fin": 0,
        "answer_prefix_ok": True,
        "timed_out": False,
        "answered_only_after_fin": False,
        "half_closed": True,
        "pattern_ok": True,
        "clean_close": True,
        "attempts": 1,
        "local_port": _CLIENT_PORT,
        "started_at": _T0 - 1.0,
        "patience_s": PEER_PATIENCE_S,
        "reason": "",
    }


def run_selftest():
    port = 7777
    cases = []

    def record(name, ok, why=""):
        cases.append((name, ok, why))

    def cap_of(raw, mtime=None):
        """Parse REAL bytes out of a REAL file, with a settable mtime.

        The mtime is written on the file rather than handed to the parser because that is what
        the binding reads: the capture's provenance is a property of the file, not of a number
        somebody passed alongside it.
        """
        fd, path = tempfile.mkstemp(suffix=".pcap")
        os.write(fd, raw)
        os.close(fd)
        if mtime is not None:
            os.utime(path, (mtime, mtime))
        try:
            return parse_pcap(path)
        finally:
            os.unlink(path)

    def wire_of(frames, *, raw=None, client_port=_CLIENT_PORT, not_before=None, mtime=None):
        """Go through the REAL parser and a REAL file: the bytes on disk are what gets judged."""
        blob = _pcap_bytes(frames) if raw is None else raw
        cap, err = cap_of(blob, mtime)
        if err:
            return None, err
        wire = tcp_stats(cap["frames"], port, peer_client_port=client_port)
        wire["measurement_error"] = cap["measurement_error"]
        wire["binding_error"], _applied = check_binding(cap, not_before)
        return wire, None

    def case(name, frames, expect_ok, expect_miss=None, *, only_prefix=None, raw=None,
             client_port=_CLIENT_PORT, not_before=None, mtime=None):
        wire, err = wire_of(frames, raw=raw, client_port=client_port, not_before=not_before,
                            mtime=mtime)
        if err:
            record(name, False, f"parser refused a capture it built: {err}")
            return
        ok, bad = judge(None, wire)
        if ok != expect_ok:
            record(name, False, f"expected ok={expect_ok}, got ok={ok} ({bad})")
            return
        if expect_miss is not None and not any(expect_miss in b for b in bad):
            record(name, False, f"failed for the wrong reason: {bad} (wanted {expect_miss!r})")
            return
        if only_prefix is not None and not (bad and all(b.startswith(only_prefix) for b in bad)):
            record(name, False, f"wanted every miss to start with {only_prefix!r}, got {bad}")
            return
        record(name, True)

    def expect(name, cond, why=""):
        record(name, bool(cond), why)

    good = _play(_Link(port))

    # ---- positive controls: what a working stack is allowed to look like -------------------
    # The frames carry two DIFFERENT ends and a SYN-ACK that acknowledges the SYN. The old
    # positive control had one MAC pair and one IP pair for every frame and still passed.
    case("a plausible two-ended exchange passes", good, True)
    # A stack that pushes its answer in three segments is LEGAL -- run_peer says so itself. The
    # per-frame `PATTERN in payload` search failed it while the peer half passed it.
    case("a guest answer split into 8 B segments passes", _play(_Link(port), segment=8), True)
    # A retransmission is not a second 24 bytes. Placing payload by sequence number makes that
    # true by construction rather than by luck.
    case("a retransmitted guest segment passes", _play(_Link(port), retransmit=True), True)
    # Sequence numbers are 32 bit and a connection may open just below the wrap. The offsets are
    # computed modulo 2^32 for exactly this; without the mask the guest's 24 bytes would land at
    # offset ~4 billion and read as "the guest sent nothing".
    case(
        "an exchange whose sequence numbers wrap passes",
        _play(_Link(port, client_isn=0xFFFFFFF0, guest_isn=0xFFFFFFF5)),
        True,
    )
    # The NIC pads a bare ACK to 60 B. Those bytes sit where payload would; the IP total length
    # is what separates them, and this case is what keeps that reading honest.
    case("a padded minimum-size frame passes", _pad(_pad(good, 2), 5), True)
    # A big-endian capture is a second parser path through the same file format.
    case("a big-endian capture passes", None, True, raw=_pcap_bytes(good, endian=">"))

    # ---- identity: the three fabricated captures that used to read ALL PASS -----------------
    # 1. Nothing ever left the guest: one MAC pair, src IP == dst IP, only the ports alternate.
    case(
        "a capture whose two ends are one end fails at identity",
        _play(_Link(port, client_mac=_HOST_MAC, guest_mac=_HOST_MAC,
                    client_ip=_HOST_IP, guest_ip=_HOST_IP)),
        False,
        "identical endpoints",
        only_prefix="identity:",
    )
    # 2. Three different connections, none of which completes.
    lnk_a = _Link(port, client_port=40000)
    lnk_a.to_guest(F_SYN)
    lnk_b = _Link(port, client_port=40001, t0=_T0 + 1.0)
    lnk_b.to_host(F_SYN | F_ACK)
    lnk_c = _Link(port, client_port=40002, t0=_T0 + 2.0)
    lnk_c.to_host(F_PSH | F_ACK, PATTERN_TO_HOST)
    three = lnk_a.frames + lnk_b.frames + lnk_c.frames
    case(
        "frames from three unrelated connections fail at identity",
        three,
        False,
        "the half the guest cannot fake",
        only_prefix="identity:",
    )
    # 3. A verbatim replay of an earlier run. Two independent binders catch it, so each is
    #    proved on its own: the capture FILE's mtime, and the client port this run's peer used.
    case(
        "a capture FILE older than this run fails at the binding",
        good,
        False,
        "was last written",
        only_prefix="binding:",
        not_before=_T0 + 100.0,
        mtime=_T0,
    )
    # ...and the other direction, which is the one that cost a run: the RECORDS may be stamped
    # long before this run began -- QEMU's clock is not ours -- and that alone must decide
    # nothing at all as long as the file itself was written by this run.
    case(
        "a capture whose RECORDS predate the run but whose FILE is fresh passes",
        good,
        True,
        not_before=_T0 + 100.0,
        mtime=_T0 + 200.0,
    )
    # Two captures in one file: the mtime is fresh and the client port matches, so both other
    # binders pass. This is the one that reads the record stamps against EACH OTHER.
    case(
        "an old capture spliced onto a fresh one fails at the binding",
        _play(_Link(port, t0=_T0 + 1000.0)) + _play(_Link(port)),
        False,
        "spliced",
        only_prefix="binding:",
        not_before=_T0 + 900.0,
        mtime=_T0 + 2000.0,
    )
    case(
        "a capture of another client's connection fails at identity",
        _play(_Link(port, client_port=40007)),
        False,
        "client port 40007",
        only_prefix="identity:",
    )

    # ---- identity: the SYN-ACK is checked, not counted --------------------------------------
    case(
        "a SYN-ACK that acknowledges nothing fails at identity",
        _play(_Link(port), synack_ack=0x99999999),
        False,
        "acknowledges",
        only_prefix="identity:",
    )
    case(
        "a SYN-ACK from another station fails at identity",
        _play(_Link(port), synack_src_mac=_OTHER_MAC),
        False,
        "no SYN-ACK mirrors",
        only_prefix="identity:",
    )
    case(
        "no SYN-ACK at all fails at the half the guest cannot fake",
        _play(_Link(port), synack=False),
        False,
        "the half the guest cannot fake",
        only_prefix="identity:",
    )
    # An empty capture must NOT pass. This is the shape the whole file exists to prevent.
    case("an empty capture fails at identity", [], False, "no SYN to port", only_prefix="identity:")
    # A frame with the right port and the wrong sender is not the guest's answer.
    case(
        "guest payload sent by another station is not counted",
        _play(_Link(port), guest_data_src_mac=_OTHER_MAC),
        False,
        "guest->host",
    )

    # ---- the REAL run: it must pass, and its replay must not --------------------------------
    # Both cases below run on the SAME bytes and differ in exactly ONE fact -- when the file was
    # last written. A binding that only ever fails is an outage and one that only ever passes is
    # decoration; this pair is what says which of the two is standing here.
    real = base64.b64decode(REAL_CAPTURE_B64)
    cap_real, err_real = cap_of(real, REAL_CAPTURE_MTIME)
    expect(
        "the archived capture of a real run parses: 18 frames",
        not err_real and cap_real and len(cap_real["frames"]) == 18,
        f"{err_real} {cap_real and len(cap_real['frames'])}",
    )
    # The measurement that decided the whole binding, kept where it can be re-read. If anyone
    # rebuilds a rule that compares record stamps with the host's clock, this gap is what it will
    # trip over -- and this line says the gap is in the data, not in a story about the data.
    expect(
        "a REAL capture's records sit nowhere near the host clock of its own run",
        cap_real and REAL_RUN_STARTED_AT - cap_real["first_ts"] > 3000.0,
        f"gap {cap_real and REAL_RUN_STARTED_AT - cap_real['first_ts']:.3f} s",
    )
    err_bind, applied = check_binding(cap_real, None)
    expect(
        "without a run start the FILE binder does not run -- and the report can say so",
        err_bind is None and "file" not in applied and "self" in applied,
        f"{err_bind} {applied}",
    )
    err_bind, applied = check_binding(cap_real, REAL_RUN_STARTED_AT)
    expect(
        "with a run start the file binder runs and PASSES on the real capture",
        err_bind is None and "file" in applied,
        f"{err_bind} {applied}",
    )
    case(
        "the archived capture of a REAL run passes, record clock an hour adrift and all",
        None,
        True,
        raw=real,
        client_port=REAL_CLIENT_PORT,
        not_before=REAL_RUN_STARTED_AT,
        mtime=REAL_CAPTURE_MTIME,
    )
    case(
        "the same real bytes replayed into a LATER run fail at the binding",
        None,
        False,
        "before this run began",
        only_prefix="binding:",
        raw=real,
        client_port=REAL_CLIENT_PORT,
        not_before=REAL_CAPTURE_MTIME + 60.0,
        mtime=REAL_CAPTURE_MTIME,
    )
    case(
        "the real capture under another run's client port fails at identity",
        None,
        False,
        "client port",
        only_prefix="identity:",
        raw=real,
        client_port=REAL_CLIENT_PORT + 1,
        not_before=REAL_RUN_STARTED_AT,
        mtime=REAL_CAPTURE_MTIME,
    )
    # An identity failure must leave the reader able to tell "nothing happened" from "something
    # happened and I could not name it". The run this file was fixed for printed `on-conn=0
    # syn=0` for a capture holding a complete exchange.
    unnamed, _ = wire_of(None, raw=real, client_port=REAL_CLIENT_PORT + 1,
                         not_before=REAL_RUN_STARTED_AT, mtime=REAL_CAPTURE_MTIME)
    expect(
        "a capture that could not be named still reports what IS in it",
        unnamed["identity_error"]
        and unnamed["frames"] == 18
        and unnamed["tcp"] == 9
        and unnamed["endpoint_pairs"] == 2
        and any(f"10.0.2.15:{port}" in line for line in unnamed["census"]),
        f"{unnamed['frames']} frames, {unnamed['tcp']} tcp, census {unnamed['census']}",
    )

    # A capacity gets a NAME and a counter, and the counter gets a case: without one, the window
    # would be a silent discard -- bytes that vanish from the count and read as a quiet stack.
    oow = _Link(port)
    oow_frames = _play(oow)
    oow.to_host(F_PSH | F_ACK, b"Z" * 8, seq=oow.guest_isn + 1 + REASSEMBLY_WINDOW, advance=False)
    case("a segment outside the reassembly window is named", oow_frames, False, "reassembly window")

    # ---- the capture itself: a failed measurement is never spelled as a failed guest --------
    case(
        "a capture cut mid-frame fails at the capture",
        None,
        False,
        "cut mid-frame",
        only_prefix="capture:",
        raw=_pcap_bytes(good)[:-5],
    )
    case(
        "a capture cut inside a record header fails at the capture",
        None,
        False,
        "record header",
        only_prefix="capture:",
        raw=_pcap_bytes(good) + b"\x00" * 8,
    )
    case(
        "a snapped capture fails at the capture",
        _snap(good, 4, 20),
        False,
        "snapped",
        only_prefix="capture:",
    )

    # ---- the guest thresholds: each mutation removes exactly ONE property -------------------
    case("no guest payload fails at guest->host", _play(_Link(port), guest_payload=None), False,
         "guest->host")
    case("no host payload fails at host->guest", _play(_Link(port), host_payload=None), False,
         "host->guest")
    case("a garbled host payload fails at the host's pattern",
         _play(_Link(port), host_payload=b"X" * len(PATTERN_TO_GUEST)), False,
         "the host's pattern")
    case("a garbled guest payload fails at the guest's pattern",
         _play(_Link(port), guest_payload=b"Y" * len(PATTERN_TO_HOST)), False,
         "the guest's pattern")
    # `pad` keeps the frame count above MIN_FRAMES so this mutation breaks ONE thing.
    case("no FIN fails at FIN", _play(_Link(port), fin=False, pad=3), False, "FIN")
    case("a RST after the exchange fails at RST", _play(_Link(port), rst=True), False, "RST")

    # ---- a cut capture and a capture still being written are NOT the same finding ----------
    # The `sleep` is injected, so this is deterministic and instant: the injected sleep is where
    # the writer gets to run, which is exactly what it is in the real thing.
    whole = _pcap_bytes(good)
    fd, path = tempfile.mkstemp(suffix=".pcap")
    os.write(fd, whole[:-5])
    os.close(fd)
    try:
        cap_cut, err_cut = _read_capture(path, 0.01, sleep=lambda _s: None)
        expect(
            "a capture that stays short is still named cut",
            not err_cut and cap_cut["truncated"] is not None,
            f"{err_cut} {cap_cut and cap_cut['truncated']}",
        )

        def _writer_finishes(_s, _p=path):
            with open(_p, "ab") as f:
                f.write(whole[-5:])

        os.truncate(path, len(whole) - 5)
        cap_race, err_race = _read_capture(path, 0.01, sleep=_writer_finishes)
        wire_race = tcp_stats(cap_race["frames"], port, peer_client_port=_CLIENT_PORT)
        wire_race["measurement_error"] = cap_race["measurement_error"]
        wire_race["binding_error"], _ = check_binding(cap_race, None)
        ok_race, bad_race = judge(None, wire_race)
        expect(
            "a capture still being written is re-read, not failed",
            ok_race and not err_race,
            f"{err_race} {bad_race}",
        )
    finally:
        os.unlink(path)

    # ---- the peer half ---------------------------------------------------------------------
    short = _green_peer()
    short["sent"] = 8
    ok, bad = judge(short, None)
    expect("a short send fails at the peer's sent bytes",
           not ok and any("sent 8 B" in b for b in bad), f"got {bad}")
    unreachable = _green_peer()
    unreachable.update(connected=False,
                       reason="never reachable at 127.0.0.1:17777 after 3 attempts")
    ok, bad = judge(unreachable, None)
    expect("an unreachable guest fails at the peer",
           not ok and any("never reachable" in b for b in bad), f"got {bad}")

    # SILENCE and WRONG BYTES are two findings and only one of them is about the stack. The run
    # this file was fixed for printed three lines for one fact -- 0 B, "not the pattern", "no
    # orderly FIN" -- of which the last two judged bytes that did not exist.
    silent = _green_peer()
    silent.update(received=0, pattern_ok=False, clean_close=False, timed_out=True,
                  reason="connected, but the guest sent nothing within 300 s")
    ok, bad = judge(silent, None)
    expect(
        "a silent guest is ONE finding, and it names the patience that was applied",
        not ok and len(bad) == 1 and "sent nothing within" in bad[0] and "300 s" in bad[0],
        f"got {bad}",
    )
    wrong = _green_peer()
    wrong.update(pattern_ok=False, answer_prefix_ok=False,
                 reason="guest answered with unexpected bytes: b'XXXX'")
    ok, bad = judge(wrong, None)
    expect(
        "a guest that answers with the WRONG BYTES fails at the pattern, not at silence",
        not ok
        and any("the bytes differ" in b for b in bad)
        and not any("sent nothing" in b for b in bad),
        f"got {bad}",
    )
    # ...and an answer that is RIGHT but short is neither of the two: the bytes are not wrong,
    # there are not enough of them, and a report that says "wrong bytes" sends the reader after
    # a defect that is not there.
    short_answer = _green_peer()
    short_answer.update(received=8, pattern_ok=False, answer_prefix_ok=True,
                        reason="the guest's answer matches as far as it goes and then stops"
                               " after 8 of 24 B")
    ok, bad = judge(short_answer, None)
    expect(
        "a TRUNCATED answer is named as truncated, not as wrong bytes",
        not ok
        and any("guest sent 8 B" in b for b in bad)
        and any("matches as far as it goes" in b for b in bad)
        and not any("the bytes differ" in b for b in bad),
        f"got {bad}",
    )

    # A report line must not name a threshold it never reached. This one did: 300 s of patience,
    # a guest that died after two seconds, and a line that said the peer had waited 345 s.
    spent = _silence_reason(True, False, 300.0, True, 15.0, 30.0)
    unspent = _silence_reason(False, True, 300.0, False, 15.0, 30.0)
    expect(
        "the silence line names the patience only when the patience was actually spent",
        "300 s" in spent and "45 s" in spent and "300" not in unspent and "closed" in unspent,
        f"{spent!r} / {unspent!r}",
    )

    # The patience DEFAULT, gated. Without this line the constant could go back to 10 s and
    # every case above would still pass -- they read the constant rather than judging it. 120 s
    # is the smallest boot budget this witness is ever run under (`SECONDS_RUN` in
    # `test-qemu-x86-load.sh` defaults to 120), and a witness that gives up before the boot it
    # watches is measuring the emulator.
    expect(
        "the patience floor covers a whole emulated boot, not a fraction of one",
        peer_patience_for(10.0) >= 120.0,
        f"a 10 s deadline yields {peer_patience_for(10.0):.0f} s of patience",
    )
    expect(
        "a longer boot budget raises the patience, and an explicit value wins over both",
        peer_patience_for(900.0) >= 900.0 and peer_patience_for(900.0, 7.0) == 7.0,
        f"{peer_patience_for(900.0)} / {peer_patience_for(900.0, 7.0)}",
    )

    # The patience MECHANISM, on a real socket pair: it is a TOTAL, and the three outcomes it can
    # end in are told apart by three separate return values rather than by one boolean.
    sa, sb = socket.socketpair()
    try:
        t_start = time.monotonic()
        buf_t, closed_t, out_of_time, err_t = _read_answer(sa, b"", time.monotonic() + 0.2)
        spent = time.monotonic() - t_start
        expect(
            "the read patience is a TOTAL deadline, not a per-recv timeout",
            out_of_time and not closed_t and not err_t and buf_t == b"" and spent < 5.0,
            f"timed_out={out_of_time} closed={closed_t} err={err_t!r} after {spent:.2f} s",
        )
        sb.sendall(PATTERN_TO_HOST)
        buf_t, closed_t, out_of_time, err_t = _read_answer(sa, b"", time.monotonic() + 10.0)
        expect(
            "the reader returns the moment the answer is complete",
            buf_t == PATTERN_TO_HOST and not out_of_time and not err_t,
            f"{buf_t!r} timed_out={out_of_time} err={err_t!r}",
        )
        sb.close()
        buf_t, closed_t, out_of_time, err_t = _read_answer(sa, b"", time.monotonic() + 10.0)
        expect(
            "a peer that CLOSED is not a peer that ran out of patience",
            closed_t and not out_of_time and not err_t,
            f"closed={closed_t} timed_out={out_of_time} err={err_t!r}",
        )
    finally:
        sa.close()
        try:
            sb.close()
        except OSError:
            pass

    # ---- the verdict is spelled by the NUMBER of sources ------------------------------------
    wire_green, err = wire_of(good)
    ok, bad = judge(_green_peer(), wire_green)
    expect("a green peer and a green wire judge green", ok and not err, f"{err} {bad}")
    both = verdict_line(_green_peer(), wire_green)
    expect("two sources spell ALL PASS", both.startswith("checknet: ALL PASS"), both)
    peer_only = verdict_line(_green_peer(), None)
    expect("a peer-only pass does NOT spell ALL PASS",
           "ALL PASS" not in peer_only and "SINGLE-SOURCE PASS" in peer_only, peer_only)
    pcap_only = verdict_line(None, wire_green)
    expect("a pcap-only pass does NOT spell ALL PASS",
           "ALL PASS" not in pcap_only and "SINGLE-SOURCE PASS" in pcap_only, pcap_only)

    passed = sum(1 for _, ok, _ in cases if ok)
    for name, ok, why in cases:
        note(f"selftest {'PASS' if ok else 'FAIL'}: {name}{'' if ok else ' -- ' + why}")
    if passed == len(cases):
        print(f"checknet: SELFTEST ALL PASS ({passed}/{len(cases)}, both directions)")
        return 0
    print(f"checknet: SELFTEST FAILURES ({passed}/{len(cases)})")
    return 1


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=17777, help="host-side port of the hostfwd rule")
    ap.add_argument("--guest-port", type=int, default=7777, help="the port the stack listens on")
    ap.add_argument("--pcap", help="the file QEMU's -object filter-dump wrote")
    ap.add_argument("--deadline", type=float, default=90.0, help="seconds to keep retrying connect")
    ap.add_argument(
        "--peer-patience",
        type=float,
        help="seconds the peer waits for the guest's answer. Default: the larger of"
        f" {PEER_PATIENCE_S:.0f} s and --deadline, because the deadline is the budget of the boot"
        " being watched and the guest cannot answer after QEMU is gone. This is patience, not a"
        " property of the stack: slirp accepts the host connection before the guest does, so a"
        " short value measures how fast the emulator boots",
    )
    ap.add_argument("--settle", type=float, default=1.0, help="seconds between connect attempts")
    ap.add_argument("--peer-only", action="store_true")
    ap.add_argument("--pcap-only", action="store_true")
    ap.add_argument(
        "--not-before",
        type=float,
        help="epoch seconds: refuse a capture FILE last written before this. Host wall clock on"
        " both sides -- it is NOT compared with the pcap's record stamps, which QEMU writes on a"
        " different clock, measured about an hour behind this one on two runs). When the peer half runs"
        " this is set from its own clock, so it only needs giving for --pcap-only",
    )
    ap.add_argument(
        "--capture-settle",
        type=float,
        default=1.0,
        help="seconds to wait before re-reading a capture whose tail is short. filter-dump writes"
        " a record header and its frame separately, so a reader can arrive between them; 0"
        " disables the re-read and makes that race a failure",
    )
    ap.add_argument(
        "--allow-port-rewrite",
        action="store_true",
        help="stop requiring the captured connection to carry the source port this run's peer"
        " opened. slirp's hostfwd and a tap bridge both preserve it; a NAT that rewrites ports"
        " would not. Waiving it removes the binder that reads the capture's CONTENT and leaves"
        " only the file's mtime",
    )
    ap.add_argument(
        "--selftest",
        action="store_true",
        help="prove the parser finds the properties when they ARE there, and misses them at the"
        " right place when they are not. Needs no QEMU.",
    )
    args = ap.parse_args()

    if args.selftest:
        return run_selftest()

    # Every way of ending with fewer than two sources is decided HERE, before 90 s of retries --
    # and none of them can end in the acceptance string.
    if args.peer_only and args.pcap_only:
        print("checknet: FAILURES (--peer-only and --pcap-only together measure nothing)")
        return 1
    if args.pcap_only and not args.pcap:
        print("checknet: FAILURES (--pcap-only without --pcap -- there is no capture to read)")
        return 1
    if not args.peer_only and not args.pcap:
        # This used to print a warning and carry on to a green ALL PASS from a single source.
        print(
            "checknet: FAILURES (no --pcap and no --peer-only: one source cannot spell an"
            " acceptance -- give --pcap, or ask for the half witness with --peer-only)"
        )
        return 1

    peer = None
    wire = None

    if not args.pcap_only:
        # The patience follows the boot budget when the caller gave one: a witness that gives up
        # while the guest it watches is still allowed to run measures its own impatience.
        patience = peer_patience_for(args.deadline, args.peer_patience)
        peer = run_peer(args.host, args.port, args.deadline, args.settle, patience_s=patience)
        note(
            f"peer connected={peer['connected']} attempts={peer['attempts']}"
            f" local_port={peer['local_port']}"
            f" sent={peer['sent']} received={peer['received']}"
            f" pattern={peer['pattern_ok']} clean_close={peer['clean_close']}"
            f" patience={peer['patience_s']:.0f}s timed_out={peer['timed_out']}"
            f" answered_only_after_fin={peer['answered_only_after_fin']}"
            " [connected means slirp accepted, NOT that the guest did]"
        )
        if peer["answered_only_after_fin"]:
            note(
                f"peer: the guest's {peer['received_after_fin']} B arrived only AFTER our"
                " half-close -- the answer came, but this stack answers on EOF rather than on"
                " the request. That is a finding of its own, not a failure"
            )

    if not args.peer_only:
        cap, err = _read_capture(args.pcap, args.capture_settle)
        if err:
            note(f"pcap unreadable: {err}")
            # An unreadable capture is a failed measurement, not an absent one. Say so and
            # fail: silence must never read as success.
            print("checknet: FAILURES (capture unreadable)")
            return 1

        client_port = None if args.allow_port_rewrite else (peer or {}).get("local_port")
        if args.allow_port_rewrite and peer is not None:
            note(
                "WARNING --allow-port-rewrite: the capture is no longer bound to the connection"
                " this peer opened; the capture FILE's mtime is what is left separating it from"
                " a replay, and that is one binder rather than two"
            )
        not_before = args.not_before
        if not_before is None and peer is not None:
            # The peer's own start, on the host's clock -- the same clock the file's mtime is on.
            not_before = peer["started_at"]

        wire = tcp_stats(cap["frames"], args.guest_port, peer_client_port=client_port)
        wire["measurement_error"] = cap["measurement_error"]
        binding_error, binders = check_binding(cap, not_before)
        if client_port is not None:
            binders.append("content")
        wire["binding_error"] = binding_error
        note(
            "binding: "
            + (" + ".join(binders) if binders else "NONE -- this capture is bound to nothing")
            + f" [file mtime={cap['mtime']:.3f},"
            + (f" run began {not_before:.3f}," if not_before is not None else " no run start,")
            + (f" client port {client_port}," if client_port is not None else " port waived,")
            + (
                f" records {cap['first_ts']:.3f}..{cap['last_ts']:.3f} spanning {cap['span']:.3f} s"
                if cap["frames"]
                else " no records"
            )
            + " -- mtime and run start are host wall clock, the record stamps are QEMU's own"
            " clock and are never compared with either]"
        )
        idty = wire["identity"]
        note(
            "wire conn="
            + (
                "none identified"
                if idty is None
                else f"{_ip(idty.client_ip)}:{idty.client_port} ({_mac(idty.client_mac)})"
                f" <-> {_ip(idty.guest_ip)}:{idty.guest_port} ({_mac(idty.guest_mac)})"
                f" syn_at={idty.syn_ts:.3f}"
            )
        )
        if idty is None:
            # An identity failure suppresses every number derived from the identity -- correctly,
            # because an unnamed capture has judged nothing. But then the only numbers left in the
            # report were the derived ones, and `on-conn=0 off-conn=0 syn=0` reads as "almost
            # nothing crossed the link" for a file that may hold a complete exchange. So the
            # undivided totals get their own lines: "nothing happened" is a finding about the
            # guest, "something happened and I could not name it" one about this witness.
            note(
                f"wire inventory, BEFORE any identity filtered it: {wire['frames']} frame(s) in"
                f" the file, {wire['arp']} ARP, {wire['ipv4']} IPv4, {wire['tcp']} TCP segment(s)"
                f" over {wire['endpoint_pairs']} endpoint pair(s)"
            )
            for line in wire["census"] or ["(not one TCP segment in the whole capture)"]:
                note(f"wire saw {line}")
        note(
            f"wire frames={wire['frames']} on-conn={wire['conn_frames']}"
            f" off-conn={wire['off_conn']} arp={wire['arp']} ipv4={wire['ipv4']}"
            f" tcp={wire['tcp']} syn={wire['syn']} synack={wire['synack']} rst={wire['rst']}"
            f" fin(h->g)={wire['fin_to_guest']} fin(g->h)={wire['fin_to_host']}"
            f" bytes(h->g)={wire['payload_to_guest']}/{wire['raw_to_guest']}"
            f" bytes(g->h)={wire['payload_to_host']}/{wire['raw_to_host']}"
            f" pattern(h->g)={wire['pattern_to_guest_seen']}"
            f" pattern(g->h)={wire['pattern_to_host_seen']}"
            " [bytes: assembled/raw; arp, ipv4, tcp are capture totals and syn, synack are"
            " implied by the identity above -- none of those four is a threshold]"
        )

    if peer is None and wire is None:
        print("checknet: FAILURES (nothing was measured -- that is not a pass)")
        return 1

    ok, bad = judge(peer, wire)
    for line in bad:
        note(f"MISS {line}")
    if ok:
        print(verdict_line(peer, wire))
        return 0
    print(f"checknet: FAILURES ({len(bad)} threshold(s) missed)")
    return 1


if __name__ == "__main__":
    sys.exit(main())
