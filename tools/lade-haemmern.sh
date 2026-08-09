#!/usr/bin/env bash
# **Die Lade-Suite haemmern** -- weil "Flattern ODER veralteter Build" keine Diagnose ist.
#
# Ein einzelner roter Lauf am 2026-08-09 war nicht zuzuordnen; zwei gruene danach reichten nicht,
# denn genau diese Baseline ist anderswo zwanzigmal gehaemmert worden. Bevor die Lade-Suite als
# ORAKEL fuer einen Bisect taugt, muss feststehen, ob sie flattert -- eine flatternde Suite kann
# kein Orakel sein.
#
# Festgehalten wird JE LAUF: Schlusszeile, Fingerprint und die Kernzeilen (fs/drv/blkdev). Bei
# Abweichung bleibt das volle Log liegen -- ein Fehlschlag ohne Protokoll ist ein verlorener
# Fehlschlag.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
N="${RUNS:-20}"
OUT=build/diag/haemmern; mkdir -p "$OUT"
gruen=0; rot=0
echo "== Lade-Suite $N mal =="
for i in $(seq 1 "$N"); do
    L="$OUT/lauf-$i.log"
    timeout 600 ./test-qemu-x86-load.sh > "$L" 2>&1
    rc=$?
    schluss="$(grep -E '^== (ALL PASS|FAILURES) ==' "$L" | tail -1)"
    fp="$(grep -m1 '^fingerprint:' "$L" | cut -d' ' -f2)"
    fs="$(grep -m1 '^fs      : \(ALL PASS\|FAILURES\)' "$L" | cut -c1-22)"
    printf '  [%2d] rc=%-3s %-16s fp=%-12s %s\n' "$i" "$rc" "${schluss:-KEINE-SCHLUSSZEILE}" "${fp:-?}" "$fs"
    if [ "$rc" = 0 ] && [ "$schluss" = "== ALL PASS ==" ]; then
        gruen=$((gruen+1)); rm -f "$L"
    else
        rot=$((rot+1))
    fi
done
echo "== Bilanz =="
echo "  gruen $gruen / rot $rot von $N   (Protokolle der roten: $OUT/)"
# **Die Aussage ist die Streuung, nicht die Zahl.** Alles rot = stabil kaputt, bisectbar.
# Alles gruen = der eine rote Lauf war ein Ausreisser und die Suite ist als Orakel brauchbar.
# Gemischt = sie FLATTERT und taugt als Orakel nicht, bevor das behoben ist.
if [ "$rot" = 0 ];      then echo "  Befund: stabil GRUEN -- als Bisect-Orakel brauchbar"
elif [ "$gruen" = 0 ];  then echo "  Befund: stabil ROT -- als Bisect-Orakel brauchbar"
else echo "  Befund: FLATTERT ($gruen/$rot) -- KEIN Orakel, bevor das behoben ist. Eigener Befund erster Ordnung."
fi
