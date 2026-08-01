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

# --- Riegel gegen die falsche Shell -------------------------------------------------------
# Dieses Skript ist bash-spezifisch (`local`, `$'...'`, `set -o pipefail`). Am 2026-07-31 wurde
# es versehentlich als `sh tools/kani-verify.sh` gefahren: dabei lief GENAU EIN Ziel von vier
# -- und der Rueckgabewert war trotzdem 0. Ein Lauf, der drei Viertel auslaesst und Erfolg
# meldet, ist die gefaehrlichste Sorte gruen, weil niemand ihn nachprueft.
#
# Die Anleitung sagte das bereits (docs/kani-lauf.md). Eine Anleitung ist aber keine Sperre:
# sie wirkt nur auf den, der sie liest, und der Fehler passiert dem, der es eilig hat. Deshalb
# steht der Riegel VOR `set -euo pipefail` -- unter dash scheitert diese Zeile selbst.
if [ -z "${BASH_VERSION:-}" ]; then
    echo "FEHLER: dieses Skript braucht bash, nicht sh/dash." >&2
    echo "        Aufruf:  bash tools/kani-verify.sh [ziel ...]" >&2
    exit 2
fi

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

# --- Ziel: unsafe-safety (eigenständiges Artefakt: getreue Kopien der Kernel-unsafe-Glue,
#     Memory-Safety der Kategorie-A-`unsafe`-Stellen; abhängigkeitsfrei) ---
setup_unsafe() {
    local SA="$TMP/kani_unsafe"; rm -rf "$SA"; mkdir -p "$SA/src"
    cp "$ROOT/Verification/unsafe-safety/kani/src/"*.rs "$SA/src/"
    manifest "$SA" sel4lake-unsafe-safety "[workspace]"
    echo "$SA"
}

run_target() { # $1=loader|region|sync ; weitere Args -> cargo kani
    local t="$1"; shift || true
    local dir pkg
    case "$t" in
        loader) dir="$(setup_loader)"; pkg="sel4lake-loader" ;;
        region) dir="$(setup_region)"; pkg="sel4lake-region" ;;
        sync)   dir="$(setup_sync)";   pkg="sel4lake-sync" ;;
        unsafe) dir="$(setup_unsafe)"; pkg="sel4lake-unsafe-safety" ;;
        *) echo "unbekanntes Ziel '$t' (loader|region|sync|unsafe)"; exit 2 ;;
    esac
    echo "== Kani: $pkg =="
    ( cd "$dir" && cargo kani -p "$pkg" "$@" )
    GELAUFEN=$((GELAUFEN + 1))
}

# Ziele mit Harnesses (wird erweitert, sobald weitere Crates aufgenommen sind).
DEFAULT_TARGETS="loader region sync unsafe"

# Mitzaehlen, wie viele Ziele tatsaechlich durchliefen. Der Riegel oben faengt die falsche
# Shell; diese Zaehlung faengt alles andere, was einen Durchlauf still verkuerzen koennte --
# ein veraendertes DEFAULT_TARGETS, ein `break` in einer kuenftigen Fassung, eine Schleife, die
# aus einem Grund abbricht, den heute niemand vorhersieht.
GELAUFEN=0

if [ "$#" -eq 0 ]; then
    ERWARTET=0
    for t in $DEFAULT_TARGETS; do ERWARTET=$((ERWARTET + 1)); done
    for t in $DEFAULT_TARGETS; do run_target "$t"; done
else
    ERWARTET=1
    run_target "$@"
fi

if [ "$GELAUFEN" -ne "$ERWARTET" ]; then
    echo "FEHLER: $GELAUFEN von $ERWARTET Zielen gelaufen -- das ist KEIN Beweisergebnis." >&2
    exit 1
fi
echo "== Kani: $GELAUFEN von $ERWARTET Zielen durchlaufen =="
