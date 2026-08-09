#!/usr/bin/env bash
# **Bisect-Orakel fuer die drei roten Zeilen `blkdev` / `part` / `drv`** (Punkt 2).
#
# Vorbedingung, ohne die ein Bisect Zeit verbrennt: die Suite darf nicht flattern. Gemessen am
# 2026-08-09 mit `tools/lade-haemmern.sh`: **12 Laeufe, EIN Binary (Fingerprint 7aae3ed5531e),
# 12x rot, verhaltensgleich** -- der einzige Unterschied zwischen zwei Protokollen war eine
# Thread-Nummer in einer PASS-Zeile. Damit ist sie als Orakel brauchbar.
#
# **Das Orakel urteilt ueber DIESE DREI ZEILEN, nicht ueber die Schlusszeile.** Aeltere Staende
# haben andere Pruefzeilen und koennen aus voellig anderen Gruenden rot sein; wer auf `== ALL PASS ==`
# bisectet, bisectet dann die Geschichte der Suite und nicht den Fehler.
#
# Exit: 0 = gut (alle drei ALL PASS), 1 = schlecht, 125 = ueberspringen (Zeile fehlt oder Bau
# scheitert -- ein Stand, in dem die Zeile gar nicht existiert, kann die Frage nicht beantworten).
set -uo pipefail
cd "$(dirname "$0")/.." || exit 125
L="$(mktemp)"
timeout 900 ./test-qemu-x86-load.sh > "$L" 2>&1
gut=0
for z in blkdev part drv; do
    zeile="$(grep -m1 -E "^$z +: (ALL PASS|FAILURES)" "$L")"
    if [ -z "$zeile" ]; then
        echo "SKIP: Zeile '$z' existiert in diesem Stand nicht"
        cp "$L" "build/diag/bisect-skip-$(git rev-parse --short HEAD).log" 2>/dev/null
        rm -f "$L"; exit 125
    fi
    case "$zeile" in *FAILURES*) gut=1; echo "  rot: ${zeile:0:60}";; esac
done
cp "$L" "build/diag/bisect-$(git rev-parse --short HEAD)-$gut.log" 2>/dev/null
rm -f "$L"
[ "$gut" = 0 ] && echo "  GUT" || echo "  SCHLECHT"
exit "$gut"
