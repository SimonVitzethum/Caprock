#!/usr/bin/env bash
# **Ein Lauf, dessen Ausfall lesbar bleibt.** (D12, 2026-08-05.)
#
# ================================================================================================
# WARUM ES DAS GIBT
# ================================================================================================
#
# Am 2026-08-04/05 gingen DREI Fehlschlaege verloren, weil die Sammelschleife darueber nur
# `tail -1` festhielt: aarch64 unter Parallellast, die x86-Lade-Suite bei 512M, und ein
# `RUNS=8`-Ausfall, dessen Kernel-Protokoll am Ende zwar vorlag -- aber die durchgefallene
# PRUEFZEILE steht in der stdout der Suite, nicht im Kernel-Log.
#
# Die Suiten legen inzwischen ihr Kernel-Protokoll ab (`build/diag/`). Was fehlte, war die
# **Urteilsausgabe**. Das ist ein `tee` je Lauf -- und solange es nicht steht, ist jeder weitere
# Sammellauf eine Messung, deren Ausfaelle niemand lesen kann.
#
# Aufruf:
#   tools/sammellauf.sh <name> <befehl...>
#
# Bei Erfolg: letzte Zeile auf stdout, Datei geloescht.
# Bei Fehlschlag: **vollstaendige** Ausgabe unter `build/diag/sammellauf-<name>-<zeit>.log`,
# und der Pfad wird genannt.
#
# Rueckgabe: der des Befehls.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

if [ "$#" -lt 2 ]; then
    echo "Aufruf: tools/sammellauf.sh <name> <befehl...>" >&2
    exit 2
fi
NAME="$1"; shift
mkdir -p build/diag
ZIEL="build/diag/sammellauf-${NAME}-$(date +%Y%m%d-%H%M%S).log"

"$@" >"$ZIEL" 2>&1
rc=$?
LETZTE="$(tail -1 "$ZIEL" 2>/dev/null)"

# ================================================================================================
# ZWEI FESTLEGUNGEN, beide aus eigenen Fehlern dieser Datei
# ================================================================================================
#
# Binnen einer Stunde nach dem Bau hatte sie zwei Loecher. Das zweite war das schlimmere, und
# zwar aus einem Grund, den ich zuerst uebersehen habe: eine **leere** Schlusszeile galt als
# Erfolg. Das hat kein Fehlerbild VERLOREN -- es hat einen **Erfolg ERFUNDEN**: ein Lauf, der
# mitten in der Ausgabe endete, wurde gruen verbucht. Und weil bei Erfolg geloescht wurde, liess
# sich hinterher nicht nachzaehlen, wie oft. Ein Zaehler, der hochzaehlt, wenn nichts geschieht,
# beschaedigt die GRUEN-Bilanz, nicht nur die rote.
#
# **(A) Der Ausgang entscheidet sich am EXIT-CODE, nicht am Text.** Die erste Fassung verglich
# Schlusszeilen -- erst gegen `ALL PASS`, dann zusaetzlich gegen die Formel der Waechter, morgen
# gegen die der naechsten Suite. Das ist derselbe Befund wie beim ersten Identitaets-Waechter:
# ein Pruefer, dessen Schluessel nicht die des Registers sind. Der Schluessel ist `$?`. Gibt eine
# Suite bei Fehlschlag `0` zurueck, ist **das** der Fehler -- einer in der Suite. Der Text wird
# nur GEGENGELESEN und ein Widerspruch gemeldet; er urteilt nicht.
#
# **(B) Vorerst wird AUCH BEI ERFOLG behalten** (`SAMMELLAUF_BEHALTEN=0` schaltet es ab). Solange
# nicht ein paar Dutzend Laeufe hier durchgegangen sind, ist „gruen" eine Aussage dieser Datei
# ueber sich selbst. Speicher ist billiger als eine zweite Runde dieser Erkenntnis.

# Widerspruch Text <-> Exit-Code: ein BEFUND ueber die Suite, kein Urteil dieses Sammlers.
if [ "$rc" -eq 0 ] && grep -qE "FAILURES|VERLETZT|NICHT SPRECHFAEHIG|BEFUND" <<<"$LETZTE"; then
    echo "  BEFUND ueber die SUITE: Exit-Code 0, Schlusszeile meldet einen Fehlschlag" >&2
    echo "    ($NAME: \"$LETZTE\") -- Fehler der Suite, nicht dieses Sammlers." >&2
    rc=1
fi
# Keine Schlusszeile heisst: der Lauf hat nicht zu Ende geschrieben.
if [ "$rc" -eq 0 ] && [ -z "$LETZTE" ]; then
    echo "  BEFUND: Exit-Code 0, aber KEINE Schlusszeile -- der Lauf endete mitten in der" >&2
    echo "    Ausgabe ($NAME). Ein abgebrochener Lauf ist kein bestandener." >&2
    rc=1
fi

if [ "$rc" -eq 0 ]; then
    echo "${LETZTE:-(keine Ausgabe)}"
    if [ "${SAMMELLAUF_BEHALTEN:-1}" = "0" ]; then
        rm -f "$ZIEL"
    else
        echo "  (behalten: $ZIEL -- Festlegung (B))"
    fi
    exit 0
fi
echo "${LETZTE:-(keine Ausgabe)}"
echo "  (volle Ausgabe: $ZIEL)"
# Die durchgefallenen Pruefzeilen gleich mitzeigen -- sie sind der Grund, warum es diese Datei gibt.
grep -E "^  FAIL|weicht vom ersten ab|WATCHDOG|KEIN OUTPUT" "$ZIEL" | head -8 | sed 's/^/  /'
exit "$rc"
