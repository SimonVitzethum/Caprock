#!/usr/bin/env bash
# SEL4Lake — Formale Verifikation Tier 1: Kani (bounded Model Checking) auf die Loader-Parser.
#
# Beweist Panik-/OOB-Freiheit (und strukturelle Korrektheit) der TrustedSAS-Zertifikats- + Boot-
# Archiv-Parser auf BELIEBIGER Eingabe — hebt die bisher nur gefuzzte Aussage auf einen (bounded)
# Beweis. Die Harnesses leben als `#[cfg(kani)]`-Module in crates/sel4lake-loader/src/{cert,archive}.rs
# (im Normal-Build inert).
#
# WARUM eine eigenständige Kopie: der Workspace `.cargo/config.toml` erzwingt ein Custom-Target +
# build-std (für den bare-metal Kernel). Kani braucht das Host-Target -> wir kopieren die Loader-Crate
# (0 Abhängigkeiten) nach $TMPDIR ohne `.cargo/config` und lassen Kani dort laufen.
#
# Voraussetzung: `cargo install --locked kani-verifier && cargo kani setup` (einmalig).
# Aufruf:  tools/kani-verify.sh [weitere cargo-kani-Argumente, z. B. --harness parse_never_panics]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SA="${TMPDIR:-/tmp}/kani_loader"
rm -rf "$SA"; mkdir -p "$SA/src"
cp "$ROOT"/crates/sel4lake-loader/src/*.rs "$SA/src/"
cat > "$SA/Cargo.toml" <<'EOF'
[package]
name = "sel4lake-loader"
version = "0.0.0"
edition = "2021"
[workspace]
[lib]
path = "src/lib.rs"
EOF
export PATH="$HOME/.cargo/bin:$PATH"
cd "$SA"
echo "== Kani auf sel4lake-loader (Cert- + Archiv-Parser) =="
cargo kani "$@"
