#!/usr/bin/env bash
# **Die Zahl der Mangel-Meldestellen ist gezaehlt, nicht geschaetzt** (C7, 2026-08-10).
#
# ================================================================================================
# WARUM ES DAS GIBT
# ================================================================================================
#
# An der `mangel`-Pruefzeile stand „die uebrigen rund zwanzig Stellen sind gegengelesen, nicht
# gemessen". „Rund zwanzig" ist derselbe Nullbefund wie „ich habe das noch nie gesehen": es klingt
# nach einer Groesse und ist eine Erinnerung. Gezaehlt sind es **31**, nicht zwanzig -- und mit
# der falschen Zahl waere die Abdeckungsangabe des Sweeps (`n von m`) von Anfang an falsch
# gewesen, ohne dass es jemandem auffaellt.
#
# Eine Zahl im Kommentar verrottet. Dieses Skript haelt sie gegen den Quelltext:
# `system::MELDESTELLEN` MUSS die Summe der drei Formen sein.
#
#   * handgeschrieben : `mangel(MANGEL_..., <menge>)`
#   * benannt_alloc   : meldet bei jedem `None` -- kann strukturell nicht schweigen
#   * benannt_slot    : dito, fuer Toepfe die Plaetze statt Bytes vergeben
#
# **Nicht mitgezaehlt** wird `mangel_vergiften()`: die Marke MELDET keinen Mangel, sie stellt eine
# Frage. Wer sie mitzaehlt, macht die Sprechprobe zu einer Meldestelle und die Abdeckung um eins
# zu gut.
#
# Aufruf:  tools/mangel-stellen.sh          (prueft)
#          tools/mangel-stellen.sh --liste  (zeigt jede Stelle mit Zeile, Code und Menge)
#
# Rueckgabe: 0 = die Zahl stimmt, 1 = sie stimmt nicht, 2 = Aufbaufehler.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

QUELLE="kernel/src/system.rs"
[ -r "$QUELLE" ] || { echo "FEHLER: $QUELLE nicht lesbar." >&2; exit 2; }

# Der Zaehler laeuft in Python, damit Kommentarzeilen sicher ausscheiden: ein `grep` auf
# `mangel(MANGEL_` trifft auch die Doku-Zeilen, in denen die Falle vom Vormittag ZITIERT wird --
# und ein Waechter, der Prosa mitzaehlt, meldet Zuwachs, sobald jemand ihn erklaert.
# **EINE Zaehlung, zwei Verbraucher.** Der Zaehler ist nach `tools/mangel-zaehlen.py` gewandert,
# weil `kernel/build.rs` ihn ebenfalls ruft: die Konstante wird seit dem 2026-08-11 ABGELEITET
# statt gefuehrt. Zwei Kopien derselben Zaehllogik waeren genau der Fehler, den die Ableitung
# beseitigt hat.
python3 "$ROOT/tools/mangel-zaehlen.py" "$QUELLE" "${1:-}"
rc=$?
rc=$?

# ------------------------------------------------------------------------------------------------
# Selbsttest: zaehlt der Zaehler WIRKLICH, und faellt die Ableitung auf, wenn sie wegfaellt?
# ------------------------------------------------------------------------------------------------
#
# Der alte Selbsttest verbog die Konstante auf 999 -- die gibt es nicht mehr, seit die Zahl
# ABGELEITET wird. Er waere damit stumm geworden, ohne dass jemand es merkt: genau die Form, gegen
# die dieses Projekt seine Sprechproben schreibt. Zwei Richtungen, beide an einer KOPIE:
#
#   1. eine Meldestelle DAZU -> die Zahl muss um genau 1 steigen (der Zaehler zaehlt wirklich),
#   2. die Ableitung entfernt -> der Waechter muss NEIN sagen (sonst kehrt die zweite Zahl zurueck).
if [ "${1:-}" != "--liste" ] && [ "$rc" -eq 0 ]; then
    TMP="$(mktemp -d)" || exit 2
    trap 'rm -rf "$TMP"' EXIT
    mkdir -p "$TMP/kernel/src"
    cp "$ROOT/kernel/build.rs" "$TMP/kernel/build.rs"

    N0="$(python3 "$ROOT/tools/mangel-zaehlen.py" "$QUELLE" --nur-zahl)"
    { cat "$QUELLE"; echo 'fn __selbsttest() { mangel(MANGEL_KEINER, 4711); }'; } \
        >"$TMP/kernel/src/system.rs"
    N1="$(python3 "$ROOT/tools/mangel-zaehlen.py" "$TMP/kernel/src/system.rs" --nur-zahl)"
    if [ "$N1" != "$((N0 + 1))" ]; then
        echo "SELBSTTEST FEHLGESCHLAGEN: eine zusaetzliche Meldestelle aendert die Zahl nicht"
        echo "                           ($N0 -> $N1). Der Zaehler zaehlt nicht, was er zaehlen soll."
        exit 1
    fi

    sed 's/env!("CAPROCK_MELDESTELLEN")/"32"/' "$QUELLE" >"$TMP/kernel/src/system.rs"
    if python3 "$ROOT/tools/mangel-zaehlen.py" "$TMP/kernel/src/system.rs" >/dev/null 2>&1; then
        echo "SELBSTTEST FEHLGESCHLAGEN: eine wieder von Hand gefuehrte Zahl wird abgenickt."
        exit 1
    fi
    echo "Selbsttest: +1 Meldestelle -> +1 gezaehlt ($N0 -> $N1); und eine von Hand gefuehrte"
    echo "            Zahl faellt durch -- der Waechter ist in beide Richtungen sprechfaehig."
fi
exit "$rc"
