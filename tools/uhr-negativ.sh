#!/usr/bin/env bash
# **Stufe A / A1: die rote Haelfte der `uhr`-Zeile.**
#
# `docs/linux-kompatibilitaet-caprock.md` §5 schreibt die erste dieser drei woertlich vor: *„eine um
# 10 % verfaelschte Rate melden -- der Vergleich muss fallen."* Ohne sie waere `passt-zum-tick` eine
# Zeile, von der niemand weiss, ob sie eine falsche Eichung ueberhaupt sieht (todo D18).
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/uhrneg.XXXXXX")"
fail=0

DATEIEN=( "kernel/src/system.rs" )
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 900 ./test-qemu-x86.sh > "$1" 2>&1; }

ZEILE="uhr    :"
# **Abgelesen wird NUR auf der eigenen Berichtszeile** -- s. `tools/tls-negativ.sh` fuer den
# Vorfall, der das gekostet hat (ein gleichnamiges Konjunkt einer fremden Zeile).
konjunkt() { grep -E "^${ZEILE}" "$1" | grep -oE "$2=(true|false)" | head -1 | cut -d= -f2; }

pruefe() {
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- Konjunkt '$3' kommt im Protokoll gar nicht vor"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' ist '$got', erwartet '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', wie gemeint"
    fi
}
gruen() { pruefe "$1" "$2" "$3" true; }

mutiere() {
    local vorher nachher treffer
    vorher="$(md5sum "$1" | cut -d' ' -f1)"
    treffer="$(sed -n "$2p" "$1" 2>/dev/null | wc -l)"
    sed -i "$2" "$1"
    nachher="$(md5sum "$1" | cut -d' ' -f1)"
    if [ "$vorher" = "$nachher" ]; then
        echo "  FAIL: $3 -- das sed-Muster hat NICHTS getroffen"; fail=1; return 1
    fi
    if [ -n "${4:-}" ] && [ "$treffer" != "$4" ]; then
        echo "  FAIL: $3 -- das Muster traf $treffer Stelle(n), erwartet $4"; fail=1; return 1
    fi
    return 0
}

echo "== Stufe A / A1: Gegenproben zur uhr-Zeile =="

echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^uhr    : ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot"
    grep -E "^uhr" "$TMP/orig.log" | head -2; exit 1
fi
echo "  PASS: uhr ist vorher gruen"

# --- M1: die Rate ist um 10 % verfaelscht -------------------------------------------------------
#
# **Die vom Dokument vorgeschriebene Gegenprobe.** Sie faellt nur, wenn die Toleranz SCHAERFER ist
# als die Verfaelschung -- mit den urspruenglichen 12 % waere sie durchgegangen, und die Zeile
# haette nie belegt, dass sie eine falsche Eichung sieht.
echo "-- M1: SYS_CLOCK meldet die Rate 10 % zu hoch --"
if mutiere kernel/src/system.rs \
   's|^        hal::timer::cycles_per_sec()$|        hal::timer::cycles_per_sec() * 110 / 100|' M1 1; then
    lauf "$TMP/m1.log"
    pruefe M1 "$TMP/m1.log" "passt-zum-tick" false
    # Die Rate bleibt plausibel -- eine um 10 % falsche Zahl sieht wie eine richtige aus, und
    # genau deshalb reicht `rate-plausibel` allein nicht.
    gruen M1 "$TMP/m1.log" "rate-plausibel"
    gruen M1 "$TMP/m1.log" "zaehler-waechst"
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: die Rate ist NULL ----------------------------------------------------------------------
#
# *Null ist ein Befund, kein Messwert.* Ohne die **Untergrenze** in `rate-plausibel` waere eine
# ausgefallene Eichung von einer richtigen nicht zu unterscheiden -- dieselbe Falle wie `NOSEL_TEXT`
# in der F1-Zeile.
echo "-- M2: SYS_CLOCK meldet 0 Hz --"
if mutiere kernel/src/system.rs \
   's|^        hal::timer::cycles_per_sec()$|        0|' M2 1; then
    lauf "$TMP/m2.log"
    pruefe M2 "$TMP/m2.log" "rate-plausibel" false
    gruen M2 "$TMP/m2.log" "zaehler-waechst"
else
    echo "  FAIL: M2 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M3: der Zaehler steht ----------------------------------------------------------------------
#
# Eine stehende Uhr mit richtiger Rate: `rate-plausibel` bleibt gruen, und nur `zaehler-waechst`
# sieht es. Die Mutation trifft die EL0-Sicht -- der Kernel liest seinen Zaehler weiter selbst,
# also faellt genau das Konjunkt, das die ABI-Seite misst.
echo "-- M3: SYS_CLOCK meldet einen stehenden Zaehler --"
if mutiere kernel/src/system.rs \
   's|^        hal::timer::cycles()$|        0x1234_5678|' M3 1; then
    lauf "$TMP/m3.log"
    pruefe M3 "$TMP/m3.log" "zaehler-waechst" false
    gruen M3 "$TMP/m3.log" "rate-plausibel"
    gruen M3 "$TMP/m3.log" "passt-zum-tick"
else
    echo "  FAIL: M3 -- Mutation nicht anwendbar"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then
    echo "== UHR-GEGENPROBEN: ALL PASS =="
else
    echo "== UHR-GEGENPROBEN: FAILURES =="
fi
exit "$fail"
