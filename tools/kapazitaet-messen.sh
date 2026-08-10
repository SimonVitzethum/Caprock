#!/usr/bin/env bash
# **Wo bricht es WIRKLICH?** -- die Kapazitaetskurve ueber wachsendes N und wachsendes RAM.
#
# Die Kapazitaetszahlen dieses Projekts sind alle STATISCH (PD-Tabelle, Thread-Slots,
# Cap-Slots). Die Schranke, an der es zuletzt tatsaechlich gescheitert ist, war ein DYNAMISCHER
# Vorrat: `SYS_LOAD` gab `NoResources`, und die Ressource war Speicher fuer eine Seitentabelle --
# bei SECHS Programmen. In keiner Kapazitaetstabelle kommt dieser Topf vor.
#
# Deshalb misst dieses Werkzeug nicht "geht N", sondern den FUELLSTAND ueber wachsendes N und
# laesst den Kernel die Ressource BENENNEN, an der es endet. Ein einzelner gruener Lauf bei einer
# Zahl belegt die Tabellengroessen und verfehlt die reale Schranke.
#
# Der Parameter geht ueber die Umgebung in den Bau (`option_env!`), und `kernel/build.rs` meldet
# ihn als Bau-Eingabe -- ohne das misst man beim naechsten Drehen den Vorgaengerstand.
set -uo pipefail
cd "$(dirname "$0")/.."

ZIELE="${ZIELE:-1000 5000 10000}"
RAMS="${RAMS:-512M 3G 6G}"
SEK="${SEK:-150}"
mkdir -p build/diag

echo "== Kapazitaetskurve: Ziele [$ZIELE] x RAM [$RAMS] =="
for ziel in $ZIELE; do
    echo "-- Bau mit CAPROCK_SCALE_TARGET=$ziel --"
    CAPROCK_SCALE_TARGET="$ziel" ./build-x86.sh --features selftest >/dev/null 2>&1 || {
        echo "  BUILD FAILED bei Ziel $ziel"; continue; }
    for ram in $RAMS; do
        LOG="build/diag/kurve-$ziel-$ram.log"
        OUT="$LOG" SEK="$SEK" RAM="$ram" ./tools/boot-x86-log.sh >/dev/null 2>&1
        # Nur die Kurvenzeilen und die Schlusszeile -- und ausdruecklich auch dann etwas
        # ausgeben, wenn NICHTS kam: ein leerer Lauf ist kein Messergebnis.
        if grep -aq "^kurve   : ENDE" "$LOG"; then
            grep -a "^kurve   :" "$LOG" | sed "s/^/  [$ziel@$ram] /"
        else
            echo "  [$ziel@$ram] KEINE KURVENZEILE -- Lauf unvollstaendig ($(wc -l <"$LOG") Zeilen)."
            echo "  [$ziel@$ram]   letzte Zeile: $(tail -1 "$LOG" | tr -d '\r')"
        fi
        grep -a "^vorrat  : Fuellstaende" "$LOG" | sed "s/^/  [$ziel@$ram] /"
    done
done
echo "== Ende. Protokolle in build/diag/kurve-*.log =="
