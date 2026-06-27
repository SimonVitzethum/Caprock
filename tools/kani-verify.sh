#!/usr/bin/env bash
# SEL4Lake — Formale Verifikation Tier 1: Kani (bounded Model Checking).
#
# Beweist Panik-/OOB-/Overflow-Freiheit + Struktur-/Bounds-Invarianten der klar abgegrenzten,
# sicherheitskritischen Komponenten. Die Harnesses leben als `#[cfg(kani)]`-Module direkt im jeweiligen
# Crate-Code (im Normal-Build inert).
#
# WARUM eigenständige Kopien: der Workspace `.cargo/config.toml` erzwingt ein Custom-Target + build-std
# (für den bare-metal Kernel). Kani braucht das Host-Target -> wir kopieren die Zielcrate samt ihrer
# workspace-lokalen path-Abhängigkeiten nach $TMPDIR (ohne `.cargo/config`) und lassen Kani dort laufen.
#
# Voraussetzung: `cargo install --locked kani-verifier && cargo kani setup` (einmalig).
# Aufruf:
#   tools/kani-verify.sh                         # ALLE Ziele (loader, region, sync) — so nutzt es die CI
#   tools/kani-verify.sh loader                  # nur ein Ziel
#   tools/kani-verify.sh loader --harness cert::kani_proofs::parse_never_panics
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
TMP="${TMPDIR:-/tmp}"

# Minimal-Cargo.toml einer kopierten Crate schreiben (workspace-Inheritance aufgelöst).
manifest() { # $1=dir $2=crate-name $3=zusatz (z. B. [dependencies]-Block oder [workspace])
    {
        echo "[package]"
        echo "name = \"$2\""
        echo "version = \"0.0.0\""
        echo "edition = \"2021\""
        echo "[lib]"
        echo "path = \"src/lib.rs\""
        printf '%s\n' "$3"
    } > "$1/Cargo.toml"
}

copy_src() { mkdir -p "$2/src"; cp "$ROOT/crates/$1/src/"*.rs "$2/src/"; }

# --- Ziel: loader (abhängigkeitsfrei) ---
setup_loader() {
    local SA="$TMP/kani_loader"; rm -rf "$SA"; copy_src sel4lake-loader "$SA"
    manifest "$SA" sel4lake-loader "[workspace]"
    echo "$SA"
}

# --- Ziel: region (Workspace mit region + mem + sync; region nutzt sel4lake_mem/_sync) ---
setup_region() {
    local SA="$TMP/kani_region"; rm -rf "$SA"; mkdir -p "$SA"
    copy_src sel4lake-mem "$SA/mem"; manifest "$SA/mem" sel4lake-mem ""
    copy_src sel4lake-sync "$SA/sync"; manifest "$SA/sync" sel4lake-sync ""
    copy_src sel4lake-region "$SA/region"
    manifest "$SA/region" sel4lake-region \
        $'[dependencies]\nsel4lake-mem = { path = "../mem" }\nsel4lake-sync = { path = "../sync" }'
    printf '[workspace]\nmembers = ["region", "mem", "sync"]\nresolver = "2"\n' > "$SA/Cargo.toml"
    echo "$SA"
}

# --- Ziel: sync (abhängigkeitsfrei) ---
setup_sync() {
    local SA="$TMP/kani_sync"; rm -rf "$SA"; copy_src sel4lake-sync "$SA"
    manifest "$SA" sel4lake-sync "[workspace]"
    echo "$SA"
}

run_target() { # $1=loader|region|sync ; weitere Args -> cargo kani
    local t="$1"; shift || true
    local dir pkg
    case "$t" in
        loader) dir="$(setup_loader)"; pkg="sel4lake-loader" ;;
        region) dir="$(setup_region)"; pkg="sel4lake-region" ;;
        sync)   dir="$(setup_sync)";   pkg="sel4lake-sync" ;;
        *) echo "unbekanntes Ziel '$t' (loader|region|sync)"; exit 2 ;;
    esac
    echo "== Kani: $pkg =="
    ( cd "$dir" && cargo kani -p "$pkg" "$@" )
}

# Ziele mit Harnesses (wird erweitert, sobald weitere Crates aufgenommen sind).
DEFAULT_TARGETS="loader region sync"

if [ "$#" -eq 0 ]; then
    for t in $DEFAULT_TARGETS; do run_target "$t"; done
else
    run_target "$@"
fi
