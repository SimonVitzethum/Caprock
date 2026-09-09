#!/bin/sh
# lx_messen.sh — M1/M2-Stand aus der S0-Vertragstabelle (Naeherung).
# M1 (transitive Huelle) und M2 (Massenbau) brauchen den Linux-Baum;
# bis dahin zaehlt die Tabelle: A-Symbole abgedeckt / gesamt.
# Mit --nm <datei> zusaetzlich nm-Abgleich (wird durchgereicht).
# Immer Exit 0: Messung, kein Gatter.
set -u
D=$(dirname "$0")
OUT=$(python3 "$D/lx_symbols.py" "$@")
FIRST=$(printf '%s\n' "$OUT" | head -n 1)
echo "M1/M2-Stand (Tabellen-Naeherung): $FIRST"
if [ $# -gt 0 ]; then
    printf '%s\n' "$OUT"
fi
exit 0
