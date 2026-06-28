#!/usr/bin/env bash
# SEL4Lake — Concurrency-Verifikation: Loom (exhaustive Interleaving-Exploration).
#
# Verifiziert die Synchronisationsprimitive aus sel4lake-sync (writer-bevorzugender RwSpinLock +
# Ticket-SpinLock) ueber ALLE Thread-Interleavings: gegenseitiger Ausschluss, kein Lost-Update,
# kein torn read, korrekter fetch_and(!WRITER)-Release. Die Harnesses sind GETREUE Kopien der
# Lock-Logik mit loom-Atomics (analog zu den Kani-unsafe-safety-Kopien).
#
# WARUM eigenstaendige Kopie nach $TMPDIR: der Workspace .cargo/config erzwingt Custom-Target +
# build-std (bare-metal Kernel). Loom braucht Host-std + crates.io (loom-Crate) -> wir kopieren das
# Artefakt nach $TMPDIR (ohne .cargo/config) und lassen loom dort laufen.
#
# Voraussetzung: cargo + Netzzugang fuer den einmaligen loom-Fetch.
# Aufruf:  tools/loom-verify.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
TMP="${TMPDIR:-/tmp}"
SA="$TMP/sel4lake_loom"; rm -rf "$SA"; mkdir -p "$SA/src"
cp "$ROOT/Verification/concurrency/loom/src/"*.rs "$SA/src/"
cp "$ROOT/Verification/concurrency/loom/Cargo.toml" "$SA/Cargo.toml"
echo "== Loom: sel4lake-sync RwSpinLock + Ticket-SpinLock (alle Interleavings) =="
( cd "$SA" && RUSTFLAGS="--cfg loom" cargo test --release "$@" )
