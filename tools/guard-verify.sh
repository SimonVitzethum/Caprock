#!/usr/bin/env bash
# Caprock — Prüft, ob der IRQ-Wächter in `caprock-sync` auslösen KANN.
#
# WARUM es dieses Skript gibt. Am 2026-08-01 wurde belegt, dass Kani über die IRQ-Sicherheit
# der SpinLocks **nichts** aussagt: Kani baut für das Host-Ziel, dort greift der dritte
# `cfg`-Zweig mit `IRQ_MASKING_IMPLEMENTED = false`, und `irq_save_disable()` ist ein No-Op.
# Ein grüner Kani-Lauf prüft also eine Fassung, in der die Eigenschaft gar nicht vorkommt
# (s. `docs/kani-lauf.md`).
#
# Was die Eigenschaft tatsächlich trägt, ist allein der Wächter zur Übersetzungszeit:
#
#     #[cfg(target_os = "none")]
#     const _: () = assert!(IRQ_MASKING_IMPLEMENTED, "…");
#
# Ein Wächter, den niemand hat auslösen sehen, ist aber eine Behauptung und kein Schutz —
# genau der Fehler, den dieses Projekt sonst überall vermeidet. Deshalb prüft dieses Skript
# **beide Richtungen**:
#
#   1. unverändert   -> muss übersetzen  (die Zusage gilt für dieses Ziel)
#   2. `cfg` zerstört -> muss scheitern, und zwar mit genau dieser Meldung
#
# Erst der zweite Lauf belegt, dass der Wächter nicht leer ist. Nachgestellt wird dabei exakt
# die Regression aus B-1.1: der x86-Bare-Metal-Zweig fällt weg, der Kernel rutscht in den
# Host-Zweig und bekäme einen SpinLock ohne Maskierung — sporadischer Deadlock statt
# Übersetzungsfehler.
#
# Aufruf:  bash tools/guard-verify.sh [ziel]     (Vorgabe: x86_64-unknown-none)
#
# Gebaut wird in einer Kopie unter $TMPDIR, aus demselben Grund wie bei `kani-verify.sh`:
# der Workspace erzwingt über `.cargo/config.toml` ein Custom-Target; hier wird ein anderes
# gebraucht.

# Riegel gegen die falsche Shell — dieselbe Falle wie bei kani-verify.sh (dort lief unter dash
# ein Ziel von vier durch, mit Rückgabewert 0). Steht bewusst VOR `set -euo pipefail`.
if [ -z "${BASH_VERSION:-}" ]; then
    echo "FEHLER: dieses Skript braucht bash, nicht sh/dash." >&2
    echo "        Aufruf:  bash tools/guard-verify.sh [ziel]" >&2
    exit 2
fi

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QUELLE="$ROOT/crates/caprock-sync"
ZIEL="${1:-x86_64-unknown-none}"
MELDUNG="Bare-Metal-Ziel ohne Interrupt-Maskierung"
ARBEIT="$(mktemp -d)"
trap 'rm -rf "$ARBEIT"' EXIT

[ -f "$QUELLE/src/lib.rs" ] || { echo "FEHLER: $QUELLE/src/lib.rs fehlt." >&2; exit 1; }

vorbereiten() {
    rm -rf "$ARBEIT"/src "$ARBEIT"/Cargo.toml "$ARBEIT"/Cargo.lock
    mkdir -p "$ARBEIT/src"
    cp "$QUELLE/src/"*.rs "$ARBEIT/src/"
    cat > "$ARBEIT/Cargo.toml" <<'ENDE'
[package]
name = "caprock-sync"
version = "0.0.0"
edition = "2021"
[lib]
path = "src/lib.rs"
[workspace]
ENDE
}

# `+nightly` und `-Z build-std`: das Bare-Metal-Ziel ist nicht als fertige Standardbibliothek
# installiert, der Kernel baut es ohnehin aus der Quelle (s. rust-toolchain.toml).
bauen() { ( cd "$ARBEIT" && cargo +nightly build --quiet --target "$ZIEL" -Z build-std=core 2>&1 ); }

echo "== 1/2  unveraendert fuer $ZIEL -- muss uebersetzen =="
vorbereiten
if bauen > "$ARBEIT/log1" 2>&1; then
    echo "   OK -- uebersetzt (Waechter zufrieden: Maskierung ist vorhanden)"
else
    echo "   FEHLER: der unveraenderte Stand uebersetzt nicht fuer $ZIEL." >&2
    tail -20 "$ARBEIT/log1" >&2
    exit 1
fi

echo "== 2/2  cfg zerstoert -- muss scheitern =="
vorbereiten
# BEIDE Bare-Metal-Zweige entschaerfen, nicht nur den x86.
#
# Der erste Entwurf traf nur `all(target_arch = "x86_64", target_os = "none")`. Fuer ein
# aarch64-Ziel blieb damit Zweig 1 gueltig, der Bau lief korrekt durch -- und das Skript
# meldete daraufhin "der Waechter ist leer". Ein Pruefer, der aus dem falschen Grund rot wird,
# ist so schlecht wie einer, der aus dem falschen Grund gruen wird; beim Nachziehen des
# aarch64-Laufs ist genau das passiert.
#
# Fallen beide Zweige weg, greift der dritte (`not(any(...))`) fuer JEDES Ziel, und dort ist
# IRQ_MASKING_IMPLEMENTED false. Auf einem Bare-Metal-Ziel muss der Waechter dann ausloesen.
sed -i \
    -e 's/all(target_arch = "x86_64", target_os = "none")/all(target_arch = "x86_64", target_os = "caprock-regressionstest")/g' \
    -e 's/target_arch = "aarch64"/target_arch = "caprock-regressionstest"/g' \
    "$ARBEIT/src/lib.rs"
treffer="$(grep -c 'caprock-regressionstest' "$ARBEIT/src/lib.rs" || true)"
if [ "$treffer" -lt 2 ]; then
    echo "   FEHLER: die cfg-Bedingungen sehen anders aus als erwartet ($treffer Treffer) --" >&2
    echo "           dieses Skript pruefte damit NICHTS. Bitte an den geaenderten Code anpassen." >&2
    exit 1
fi
echo "   (cfg an $treffer Stellen entschaerft -- beide Bare-Metal-Zweige, Regression aus B-1.1)"

if bauen > "$ARBEIT/log2" 2>&1; then
    echo "   FEHLER: der Bau lief durch, OBWOHL die Maskierung fehlt." >&2
    echo "           Der Waechter ist leer -- er kann nicht ausloesen." >&2
    exit 1
fi
if ! grep -q "$MELDUNG" "$ARBEIT/log2"; then
    echo "   FEHLER: der Bau scheitert, aber NICHT am Waechter. Die Aussage traegt so nicht." >&2
    tail -25 "$ARBEIT/log2" >&2
    exit 1
fi
echo "   OK -- Bau bricht am Waechter ab:"
grep -m1 "$MELDUNG" "$ARBEIT/log2" | cut -c1-140 | sed 's/^/      /'

echo
echo "== Ergebnis: der Waechter loest aus, wenn die Eigenschaft fehlt. Er ist nicht leer. =="
