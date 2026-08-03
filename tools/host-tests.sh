#!/usr/bin/env bash
# **Die Host-Tests der reinen Crates — an einem Ort** (B-5.5-Nachtrag, 2026-08-02).
#
# Warum es dieses Skript gibt: mehrere Crates tragen `#[cfg(test)]`-Module mit echter Deckung, und
# ein Teil davon lief **nirgends**. `sel4lake-cap` ist das deutlichste Beispiel — sechs Tests der
# CDT-Laufgrenzen, die kein Skript und keine CI anfasste. Ein Test, der nirgends laeuft, ist kein
# Test, sondern eine Absichtserklaerung.
#
# Der Grund war mechanisch: `cargo test -p <crate>` scheitert am erzwungenen Custom-Target aus
# `.cargo/config.toml` (`build-std`). Der Ausweg steht in AGENTS.md und ist derselbe, den
# `tools/kani-verify.sh` schon nimmt — die Crate ausserhalb des Workspace mit einem
# Minimal-Manifest bauen:
#
#   * **abhaengigkeitsfreie** Crates direkt ueber `rustc --test` (Sekunden, kein Cargo noetig);
#   * Crates **mit** Abhaengigkeiten ueber ein Wegwerf-Cargo-Projekt im TMPDIR.
#
# Aufruf:
#   tools/host-tests.sh            # alles
#   tools/host-tests.sh cap        # nur ein Ziel
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="${TMPDIR:-/tmp}"
RUSTC="rustup run nightly rustc"
CARGO="rustup run nightly cargo"
fail=0

# --- Abhaengigkeitsfreie Crates: ein Aufruf, kein Cargo ------------------------------------------
einzeln() { # $1 = Anzeigename, $2 = Pfad zur Quelldatei
    local name="$1" src="$2" bin="$TMP/hosttest_$1"
    echo "== Host-Tests: $name =="
    # **Fehlend und kaputt duerfen nicht gleich aussehen** (Befund aus B-5.1).
    #
    # Vorher meldete jede Quelle, die nicht vorlag, "liess sich nicht uebersetzen" -- derselbe
    # Wortlaut wie ein echter Compilerfehler. In einem unvollstaendigen Baum (anderer Zweig,
    # frischer Klon, Agenten-Worktree) las sich das wie ein kaputtes Projekt, und umgekehrt haette
    # sich ein wirklich kaputtes Ziel als "ist halt nicht da" abtun lassen.
    #
    # Dieselbe Form wie die stummen VT-d-Einheiten (B-3.3) und wie `CycleStats::measurable()`:
    # "nicht gemessen" ist keine Messung. Ein fehlendes Ziel ist deshalb ein eigener Ausgang --
    # er faellt DURCH (ein Ziel, das hier steht, soll existieren), sagt aber, warum.
    if [ ! -f "$src" ]; then
        echo "  FEHLT: $src ist nicht vorhanden -- Ziel nicht gelaufen (kein Uebersetzungsfehler)"
        fail=1; return
    fi
    rm -f "$bin"
    if ! $RUSTC --test --edition 2021 -O "$src" -o "$bin" 2>&1 | grep -E "^error" -A 6; then :; fi
    if [ ! -x "$bin" ]; then
        echo "  FEHLER: $name liess sich nicht uebersetzen"; fail=1; return
    fi
    "$bin" || fail=1
    rm -f "$bin"
}

# --- Crates mit Abhaengigkeiten: Wegwerf-Projekt ausserhalb des Workspace ------------------------
#
# Die Abhaengigkeiten werden **mitkopiert**, nicht per Pfad in den Workspace gezeigt: ein
# `path = "../.."` zoege `.cargo/config.toml` und damit das Custom-Target wieder herein, und wir
# waeren zurueck beim `build-std`-Umweg (16 Minuten, s. AGENTS.md).
mit_deps() { # $1 = Name, $2 = Crate-Verzeichnis, $3.. = Abhaengigkeiten (Verzeichnisname)
    local name="$1" dir="$2"; shift 2
    local SA="$TMP/hosttest_${name}_proj"
    echo "== Host-Tests: $name =="
    rm -rf "$SA"; mkdir -p "$SA/src"
    cp -r "$ROOT/crates/$dir/src/." "$SA/src/"
    {
        printf '[package]\nname="%s"\nversion="0.0.0"\nedition="2021"\n[workspace]\n[lib]\npath="src/lib.rs"\n[dependencies]\n' "$dir"
        for d in "$@"; do
            mkdir -p "$SA/$d/src"
            cp -r "$ROOT/crates/$d/src/." "$SA/$d/src/"
            printf '[package]\nname="%s"\nversion="0.0.0"\nedition="2021"\n[lib]\npath="src/lib.rs"\n' "$d" > "$SA/$d/Cargo.toml"
            printf '%s = { path = "%s" }\n' "$d" "$d"
        done
        printf '[lints.rust]\nunexpected_cfgs = { level = "warn", check-cfg = ['"'"'cfg(kani)'"'"', '"'"'cfg(loom)'"'"'] }\n'
    } > "$SA/Cargo.toml"
    ( cd "$SA" && $CARGO test --release ) || fail=1
    rm -rf "$SA"
}

ZIELE="${*:-mem part fat cycles loader cap ipctreue}"
for z in $ZIELE; do
    case "$z" in
        mem)  einzeln mem  "$ROOT/crates/sel4lake-mem/src/lib.rs" ;;
        part) einzeln part "$ROOT/crates/sel4lake-part/src/lib.rs" ;;
        fat)  einzeln fat  "$ROOT/crates/sel4lake-fat/src/lib.rs" ;;
        # `sel4lake-sched` als Ganzes haengt an `sel4lake-hal` (arch-Asm) und wird auf dem Host nie
        # bauen. Die Zyklenarithmetik (B-5.1) liegt deshalb abhaengigkeitsfrei in einem eigenen
        # Modul und wird als **Datei** geprueft -- die Fallen dort sind reine u64-Rechnung und
        # brauchen keine Maschine, sondern Literale.
        cycles) einzeln cycles "$ROOT/crates/sel4lake-sched/src/cycles.rs" ;;
        # `sel4lake-loader` ist abhaengigkeitsfrei und traegt die Parser fuer Boot-Archiv, ELF64
        # und **System-Manifest**. Es gibt kein `.github/workflows/` in diesem Baum -- die Tests
        # liefen also nirgends, genau wie die von `sel4lake-cap` vor B-5.5. Kani prueft Beweise,
        # keine `#[test]`s; das ist nicht dasselbe.
        loader) einzeln loader "$ROOT/crates/sel4lake-loader/src/lib.rs" ;;
        # `sel4lake-cap` haengt an `sel4lake-mem` und `sel4lake-slab`. Bis B-5.5 liefen seine Tests
        # deshalb nirgends -- die Huerde war das Manifest, nicht der Code.
        cap)  mit_deps cap sel4lake-cap sel4lake-mem sel4lake-slab ;;
        # `sel4lake-ipc` haengt an `sel4lake-hal` (arch-Asm) und wird auf dem Host nie bauen -- wie
        # `sel4lake-sched`. Anders als bei `cycles` liegt die Logik aber NICHT abhaengigkeitsfrei in
        # einem eigenen Modul, sondern mitten im Endpoint. Der Ausweg sind Stellvertreter fuer
        # HAL/Scheduler/ABI; weil derselbe Aufbau zugleich das Verus-Modell gegen den echten Code
        # faehrt, liegt er in `tools/verus-modelltreue-ipc.sh` und wird hier nur gerufen.
        ipctreue)
            echo "== Host-Tests: ipctreue (Endpoint gegen das Verus-Modell) =="
            if [ ! -x "$ROOT/tools/verus-modelltreue-ipc.sh" ] && [ ! -f "$ROOT/tools/verus-modelltreue-ipc.sh" ]; then
                echo "  FEHLT: tools/verus-modelltreue-ipc.sh ist nicht vorhanden -- Ziel nicht gelaufen"
                fail=1
            else
                bash "$ROOT/tools/verus-modelltreue-ipc.sh" || fail=1
            fi ;;
        *)    echo "  FEHLER: unbekanntes Ziel '$z' (bekannt: mem part fat cycles loader cap ipctreue)"; fail=1 ;;
    esac
done

if [ "$fail" = 0 ]; then echo "== HOST-TESTS: ALL PASS =="; else echo "== HOST-TESTS: FAILURES =="; fi
exit "$fail"
