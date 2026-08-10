#!/usr/bin/env bash
# Build the Caprock kernel image (aarch64, bare metal, no_std).
#
# Uses the rustup `nightly` toolchain (required for `-Z build-std`, configured
# in .cargo/config.toml). Always builds from the workspace root.
set -euo pipefail
cd "$(dirname "$0")"

exec rustup run nightly cargo build --release "$@"
