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

# **Zwei Melder, und der Rueckgabewert ist der fuehrende.** Manche Suiten liefern 0 und melden
# trotzdem `== FAILURES ==`; umgekehrt ist ein rc != 0 auch ohne diese Zeile ein Ausfall.
#
# Die erste Fassung verlangte zusaetzlich das Wort „ALL PASS" in der Schlusszeile -- und hielt
# damit die Protokolle der WAECHTER fest, die gruen sind und anders schliessen
# („== Kerngrenze eingehalten ==", „== Identitaet: ... =="). Ein Sammler, der Erfolge als
# Ausfaelle ablegt, macht sein eigenes Verzeichnis unlesbar; geprueft wird deshalb auf das
# **Fehlerwort**, nicht auf ein bestimmtes Erfolgswort.
# **Eine LEERE Schlusszeile ist kein Erfolg.** Ein Lauf, der mitten in der Ausgabe endet (SIGKILL,
# Zeitlimit, abgeschnittene Pipe), hat keine Schlusszeile -- und ein Praedikat, das nur auf das
# Fehlerwort prueft, liest daraus „kein Fehler". Genau die Form, vor der der Kopf dieses Projekts
# warnt: Schweigen als Erfolg. Selbst beobachtet, eine Stunde nach dem Bau dieser Datei.
if [ "$rc" -eq 0 ] && [ -n "$LETZTE" ] \
   && ! grep -qE "FAILURES|VERLETZT|NICHT SPRECHFAEHIG|BEFUND" <<<"$LETZTE"; then
    echo "$LETZTE"
    rm -f "$ZIEL"
    exit 0
fi
echo "$LETZTE"
echo "  (volle Ausgabe: $ZIEL)"
# Die durchgefallenen Pruefzeilen gleich mitzeigen -- sie sind der Grund, warum diese Datei
# ueberhaupt existiert.
grep -E "^  FAIL|weicht vom ersten ab|WATCHDOG|KEIN OUTPUT" "$ZIEL" | head -8 | sed 's/^/  /'
exit "$rc"
