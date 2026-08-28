#!/usr/bin/env bash
# **Z4d stage 1: the red half of the cut rule.** Green conjuncts are one half of the proof; the
# other is that the line can go red AT ALL — and at the place one meant.
#
# Three things are checked per mutation, and the second is the point:
#   1. the mutation BIT (a sed pattern that no longer matches after a rename would be a silently
#      disabled counter-proof),
#   2. the INTENDED conjunct fell — not some other one,
#   3. where the statement rests on it: the others stayed green. A mutation that breaks two things
#      at once proves nothing about the one meant (the first D9 counter-proof).
#
# The mutation that matters most is M2. It disables the direction that had no name until
# 2026-08-25: the endpoint migrates, the peer blocked at it stays behind. Before this strand there
# was nothing to disable — `Scope::endpoints` was believed, not checked — so a run with M2 applied
# is, quite literally, the state of the tree before Z4d stage 1.
if [ -z "${BASH_VERSION:-}" ]; then echo "ERROR: needs bash, not sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/ckptcutneg.XXXXXX")"
fail=0

# **Backup without `git stash`.** `refs/stash` is shared across all worktrees; a tool that uses it
# can carry off the work of an agent running in parallel.
DATEIEN=( "crates/caprock-cap/src/checkpoint.rs" "crates/caprock-ipc/src/lib.rs" "kernel/src/system.rs" )
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 900 ./test-qemu-x86.sh > "$1" 2>&1; }
konjunkt() { grep -oE "$2=(true|false)" "$1" | head -1 | cut -d= -f2; }

pruefe() { # $1 name  $2 log  $3 conjunct  $4 expected
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- conjunct '$3' does not appear in the log at all (the mutation wrecked"
        echo "        the run differently than intended)"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' is '$got', expected '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', as intended"
    fi
}

# A conjunct that must have stayed GREEN -- the isolation statement.
gruen() { pruefe "$1" "$2" "$3" true; }

mutiere() { # $1 file  $2 sed expression  $3 name
    local vorher nachher
    vorher="$(md5sum "$1" | cut -d' ' -f1)"
    sed -i "$2" "$1"
    nachher="$(md5sum "$1" | cut -d' ' -f1)"
    if [ "$vorher" = "$nachher" ]; then
        echo "  FAIL: $3 -- the sed pattern matched NOTHING (silently disabled counter-proof)"
        fail=1; return 1
    fi
    return 0
}

echo "== Z4d stage 1: counter-proofs for the checkpoint cut =="

# --- positive control ---------------------------------------------------------------------------
# Without it the mutations prove nothing: if the starting state is already red, it is red mutated
# too, and every mutation would look like evidence.
echo "-- positive control (unchanged) --"
lauf "$TMP/orig.log"
if ! grep -q "^ckptcut : ALL PASS" "$TMP/orig.log"; then
    echo "  ERROR: the starting state is already red -- the counter-proofs cannot say anything"
    grep -E "^ckptcut" "$TMP/orig.log" | head -3
    exit 1
fi
echo "  PASS: ckptcut is green beforehand"

# --- M1: the rule is blind to "participant migrates, channel stays" -----------------------------
# Z4d verbatim: a thread with an open CALL leaves a waiting server behind. This is the half
# `freeze_thread` also covers -- but only for a caller who asked it and honoured the answer, which
# is call discipline and not structure. Here the artifact itself goes blind.
echo "-- M1: an open CALL travels without its endpoint --"
if mutiere crates/caprock-cap/src/checkpoint.rs \
   's/        (false, true) => Some(CutRefusal::ChannelNotInScope),/        (false, true) => None,/' M1; then
    lauf "$TMP/m1.log"
    pruefe M1 "$TMP/m1.log" "offener-ruf-abgewiesen" false
    # The other direction is a DIFFERENT decision and must survive -- otherwise the mutation
    # merely switched the rule off wholesale and says nothing about this half.
    gruen M1 "$TMP/m1.log" "fremder-partner-abgewiesen"
else
    echo "  FAIL: M1 -- mutation not applicable"; fail=1
fi
restore

# --- M2: the rule is blind to "channel migrates, peer stays" ------------------------------------
# **The half that had no name.** This mutation reproduces the tree as it stood before 2026-08-25:
# an endpoint named in the scope was portable, full stop, and the server blocked at it was never
# looked at. Note which conjunct stays green -- the old half was there all along, which is exactly
# why the gap looked closed.
echo "-- M2: the endpoint travels and the peer blocked at it is not looked at --"
if mutiere crates/caprock-cap/src/checkpoint.rs \
   's/        (true, false) => Some(CutRefusal::PeerNotInScope),/        (true, false) => None,/' M2; then
    lauf "$TMP/m2.log"
    pruefe M2 "$TMP/m2.log" "fremder-partner-abgewiesen" false
    gruen M2 "$TMP/m2.log" "offener-ruf-abgewiesen"
else
    echo "  FAIL: M2 -- mutation not applicable"; fail=1
fi
restore

# --- M3: the gate is not in the artifact --------------------------------------------------------
# `Image::build` stops asking. That is the state in which the stage-1 promise rests entirely on
# whoever called `freeze_thread` first -- the `ep_inv` shape: green, and holding by call discipline.
echo "-- M3: Image::build does not check the cut at all --"
if mutiere crates/caprock-cap/src/checkpoint.rs \
   's/        classify_cut(subject, edges, scope).map_err(|(i, r)| BuildRefusal::Cut(i, r))?;/        let _ = (subject, edges);/' M3; then
    lauf "$TMP/m3.log"
    pruefe M3 "$TMP/m3.log" "offener-ruf-abgewiesen" false
    pruefe M3 "$TMP/m3.log" "fremder-partner-abgewiesen" false
    # **The cap gate is a SEPARATE gate and must still bite.** If it fell with them, `build` would
    # simply be broken and the two refusals would prove nothing about the cut.
    gruen M3 "$TMP/m3.log" "cap-grund-getrennt"
else
    echo "  FAIL: M3 -- mutation not applicable"; fail=1
fi
restore

# --- M4: the enumeration direction does not exist at all ----------------------------------------
# `quiescence_of` needs a thread you can already name; the dangerous participant is by construction
# one the checkpoint never listed. With `Endpoint::occupants` reporting nobody, the rule is intact
# and simply never sees the edge -- a guard judging a measurement that was never taken.
#
# **The first version of this mutation replaced the two `occupants` CALL SITES in `cut_edges` with
# `Ok(0)` and did not compile** (`E0282`: with both arms of the `if` gone, nothing pinned the error
# type). The run then had no `ckptcut` line at all, and the third check above -- "does the conjunct
# appear?" -- is what said so instead of the file reporting a passing counter-proof. A mutation that
# breaks the BUILD measures the build.
echo "-- M4: Endpoint::occupants reports nobody --"
if mutiere crates/caprock-ipc/src/lib.rs \
   '/pub fn occupants(&self, out: &mut \[(ThreadId, Role)\]) -> Result<usize, usize> {/,/^    }$/ s/^        if !self.used {$/        if true {/' M4; then
    lauf "$TMP/m4.log"
    pruefe M4 "$TMP/m4.log" "fremde-kante-gefunden" false
    pruefe M4 "$TMP/m4.log" "fremder-partner-abgewiesen" false
    # Pass 2 asks the other question and is untouched -- the subject's own edge is still found.
    gruen M4 "$TMP/m4.log" "ruf-kante-gefunden"
    # And notifications are a separate object with a separate enumeration: if this fell too, the
    # mutation would have switched off more than the endpoint direction.
    gruen M4 "$TMP/m4.log" "ntfn-kante-gefunden"
else
    echo "  FAIL: M4 -- mutation not applicable"; fail=1
fi
restore

# --- M5: the enumeration omits exactly the participant that would be left behind ------------------
# One role dropped from `occupants`, and it is the one that owes a reply. The list is not shorter,
# it is wrong -- and a shortened participant list IS a participant left behind.
echo "-- M5: occupants does not report the reply owner --"
if mutiere crates/caprock-ipc/src/lib.rs \
   's/^                push(o, Role::ReplyOwner);$/                let _ = o;/' M5; then
    lauf "$TMP/m5.log"
    pruefe M5 "$TMP/m5.log" "fremde-kante-gefunden" false
    pruefe M5 "$TMP/m5.log" "fremder-partner-abgewiesen" false
    gruen M5 "$TMP/m5.log" "ruf-kante-gefunden"
else
    echo "  FAIL: M5 -- mutation not applicable"; fail=1
fi
restore

# --- M6: the rule refuses everything ------------------------------------------------------------
# The mutation that keeps both refusals green. Without the positive control below it, a rule that
# says no to every checkpoint would pass this whole file -- and it would be useless, because no
# thread could ever migrate. `geschlossene-beziehung-geht` is the only line that separates
# "refuses the right thing" from "refuses".
echo "-- M6: even a relationship wholly inside the cut is refused --"
if mutiere crates/caprock-cap/src/checkpoint.rs \
   's/        (true, true) | (false, false) => None,/        (false, false) => None,\n        (true, true) => Some(CutRefusal::PeerNotInScope),/' M6; then
    lauf "$TMP/m6.log"
    pruefe M6 "$TMP/m6.log" "geschlossene-beziehung-geht" false
    gruen M6 "$TMP/m6.log" "offener-ruf-abgewiesen"
else
    echo "  FAIL: M6 -- mutation not applicable"; fail=1
fi
restore

# --- M7: notifications are not a channel ---------------------------------------------------------
# Two kinds of channel, and only one of them is exercised by the endpoint tests. A thread blocked in
# WAIT is just as stranded by a move as one blocked in RECV; if the survey skips notifications, the
# rule is fine and the finding is empty.
#
# **The sed is RANGE-BOUND to `cut_edges`, and that is not tidiness.** The bare pattern
# `for i in 0..ntfns().len() {` matches TWICE in `system.rs` -- the second occurrence is inside
# `thread_quiescence`, which the probe's own speaking probes are built on. Unbounded, this mutation
# would blind the survey AND the probe that says the survey has something to find: two things broken
# at once, which proves nothing about either (the first D9 counter-proof, verbatim). It looked clean
# because the isolation conjunct checked here (`ruf-kante-gefunden`) survives both. `warter-steht`
# is the one that does not, and it is checked below for exactly that reason.
echo "-- M7: the survey skips notifications --"
if mutiere kernel/src/system.rs \
   '/^pub fn cut_edges(/,/^}$/ s/^    for i in 0..ntfns().len() {$/    for i in 0..0 {/' M7; then
    lauf "$TMP/m7.log"
    pruefe M7 "$TMP/m7.log" "ntfn-kante-gefunden" false
    pruefe M7 "$TMP/m7.log" "ntfn-abgewiesen" false
    # The endpoint half is a separate traversal and stays green.
    gruen M7 "$TMP/m7.log" "ruf-kante-gefunden"
    # **And the speaking probe must survive.** It reads `thread_quiescence`, not the survey; if it
    # fell too, the mutation reached past the thing it claims to switch off.
    gruen M7 "$TMP/m7.log" "warter-steht"
else
    echo "  FAIL: M7 -- mutation not applicable"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then
    echo "== Z4d stage 1: every counter-proof bit, and each at its own conjunct =="
else
    echo "== Z4d stage 1: COUNTER-PROOFS FAILED -- a green line here proves nothing =="
fi
exit "$fail"
