#!/usr/bin/env bash
# Build the Caprock kernel image (aarch64, bare metal, no_std).
#
# Uses the rustup `nightly` toolchain (required for `-Z build-std`, configured
# in .cargo/config.toml). Always builds from the workspace root.
set -euo pipefail
cd "$(dirname "$0")"

# Dieselbe Falle wie in `build-x86.sh`, nur fuer das aarch64-Ziel: Cargo mischt
# `.cargo/config.toml` aus jedem Vorfahrenverzeichnis und HAENGT Arrays aneinander. In einem
# Agenten-Worktree unter `<repo>/.claude/worktrees/<id>` steht `-Tkernel/linker.ld` deshalb
# zweimal auf der Linkerzeile. Begruendung + Messung: `tools/rustflags-entdoppeln.py`.
ENTDOPPELT="$(python3 tools/rustflags-entdoppeln.py aarch64-caprock)" && RC=0 || RC=$?
if [ "${RC:-0}" = "10" ]; then
    echo "rustflags: DOPPELT geerbt (Worktree liegt im Hauptbaum) -- entdoppelt auf: $ENTDOPPELT"
    # `RUSTFLAGS` ERSETZT die Konfiguration; `CARGO_TARGET_<T>_RUSTFLAGS` wird mit ihr
    # gemischt und macht die Doppelung nur schlimmer (gemessen, s. build-x86.sh).
    export RUSTFLAGS="$ENTDOPPELT"
fi

exec rustup run nightly cargo build --release "$@"
