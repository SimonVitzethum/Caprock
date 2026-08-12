#!/usr/bin/env bash
# ================================================================================================
# DIE GEGENPROBE ZUR SPERRHALTEDAUER-MARKE (C9) -- die `while let`-Falle, absichtlich gebaut
# ================================================================================================
#
# **Warum es diesen eigenen Lauf gibt.** Die Marke behauptet, sie faenge einen Guard, der ueber den
# Rumpf einer `while let`-Schleife lebt. Eine Behauptung ueber einen Fehler, den man nie ausgeloest
# hat, ist keine Messung -- dieselbe Einordnung wie bei der #DF-Sonde: ein Mechanismus, der nie
# benutzt wurde, ist von einem falsch aufgesetzten nicht zu unterscheiden.
#
# Gefahren wird deshalb `--features selftest,sperrmark-gegenprobe`, und darin steht in
# `kernel/src/sperrmark.rs` woertlich der C8-Befund:
#
#     while let Some(x) = PROBE.lock().entnehmen() { warten(dauer); }     // FALLE
#     while let Some(x) = naechster()              { warten(dauer); }     // richtig
#
# **Die beiden Zweige unterscheiden sich in NICHTS ausser der Lebensdauer des Guards** -- dieselbe
# Schlange, dieselbe Sperre, dieselbe Arbeit, dieselbe Anzahl Durchlaeufe. Eine Gegenprobe, die
# zwei Dinge zugleich aendert, misst die Reihenfolge der Pruefungen und nicht die Eigenschaft
# (Fallenliste, D9).
#
# **Warum nicht ueber `test-qemu-x86.sh`:** die Suite baut selbst, fest mit `--features selftest`,
# und ueberschriebe das Sondenabbild. Genau das ist bei der ersten Fassung dieser Gegenprobe
# passiert -- sie meldete "kein Unterschied", und der Unterschied war nie im Abbild. Ein
# Gegenprobenlauf, dessen Bau von jemand anderem ueberschrieben wird, belegt gar nichts.
#
# **Drei Aussagen, und die dritte ist die eigentliche:**
#
#   1. RICHTIG    -- mit Funktionsgrenze bleibt der bereinigte Hoechststand klein und die Zeile
#                    steht auf ALL PASS. Das ist die Positivkontrolle: ohne sie belegte ein rotes
#                    Ergebnis nur, dass die Zeile ueberhaupt rot werden kann.
#   2. FALLE      -- mit der `while let`-Fassung springt derselbe Hoechststand ueber die Schwelle,
#                    die Zeile faellt durch, und sie NENNT Zahl UND Ort.
#   3. DER ORT    -- die genannte Stelle ist die Zeile des `while let` in `sperrmark.rs`. Eine
#                    Zahl ohne Adresse waere ein Alarm, den niemand aufloesen kann.
#
# Der Lauf laesst den Baum am Ende in der regulaeren Konfiguration zurueck (`--features selftest`),
# damit niemand versehentlich mit der Sonde weitermisst.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
mkdir -p build/diag

bauen() {   # $1 = Featureliste
    echo "== baue mit --features $1 =="
    ./build-x86.sh --features "$1" >build/diag/sperr-gegenprobe-bau.log 2>&1
    local rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "  FAIL: Bau mit '$1' fehlgeschlagen (rc=$rc)"
        tail -20 build/diag/sperr-gegenprobe-bau.log | sed 's/^/        /'
        return 1
    fi
    return 0
}

fahren() {  # $1 = Ausgabedatei
    SEK=120 OUT="$1" ./tools/boot-x86-log.sh
}

# Den bereinigten Hoechststand (Zyklen), seinen Ort und das Urteil aus einem Protokoll ziehen.
# **`grep ... <<<"$X"` statt `echo "$X" | grep`** -- eine Pipeline meldet unter `pipefail` bei
# grosser Ausgabe "nicht gefunden", sobald `grep -q` beim ersten Treffer aussteigt (Fallenliste).
bereinigt()  { grep -oE "ohne die erklaerten Langhalter: [0-9]+" "$1" | grep -oE "[0-9]+$" | head -1; }
ort()        { grep -oE "ohne die erklaerten Langhalter: [0-9]+ Zyklen @ [^ ]+" "$1" | grep -oE "@ .*" | head -1; }
urteil()     { grep -oE "^sperre  : (ALL PASS|FAILURES)" "$1" | head -1; }
schwelle()   { grep -oE "Schwelle [0-9]+ Zyklen" "$1" | grep -oE "[0-9]+" | head -1; }

RICHTIG=build/diag/sperr-gegenprobe-richtig.log
FALLE=build/diag/sperr-gegenprobe-falle.log

# ------------------------------------------------------------------------------------------------
# 1. POSITIVKONTROLLE: mit Funktionsgrenze
# ------------------------------------------------------------------------------------------------
bauen "selftest" || exit 1
fahren "$RICHTIG"
R_BER=$(bereinigt "$RICHTIG"); R_ORT=$(ort "$RICHTIG"); R_URT=$(urteil "$RICHTIG")
SCHW=$(schwelle "$RICHTIG")
echo "== 1. RICHTIG (Funktionsgrenze) =="
echo "   bereinigter Hoechststand: ${R_BER:-?} Zyklen ${R_ORT:-?}"
echo "   Urteil: ${R_URT:-(keins)}   Schwelle: ${SCHW:-?} Zyklen"
if [ -z "${R_BER:-}" ] || [ -z "${SCHW:-}" ]; then
    echo "  FAIL: der Positivlauf hat gar nichts gemessen -- ein leerer Lauf ist kein Testergebnis"
    fail=1
elif [ "$R_BER" -ge "$SCHW" ]; then
    echo "  FAIL: schon OHNE die Falle liegt der Hoechststand ueber der Schwelle -- dann sagt ein"
    echo "        rotes Ergebnis der Falle nichts ueber die Falle"
    fail=1
elif ! grep -q "^sperre  : ALL PASS" "$RICHTIG"; then
    echo "  FAIL: die Zeile steht ohne Falle nicht auf ALL PASS -- keine brauchbare Grundlinie"
    fail=1
else
    echo "  PASS: ohne die Falle traegt die Zeile (Grundlinie steht)"
fi

# ------------------------------------------------------------------------------------------------
# 2. DIE FALLE: `while let Some(x) = LOCK.lock().entnehmen()`
# ------------------------------------------------------------------------------------------------
bauen "selftest,sperrmark-gegenprobe" || exit 1
fahren "$FALLE"
F_BER=$(bereinigt "$FALLE"); F_ORT=$(ort "$FALLE"); F_URT=$(urteil "$FALLE")
echo '== 2. FALLE (der `while let`-Guard lebt ueber den Rumpf) =='
echo "   bereinigter Hoechststand: ${F_BER:-?} Zyklen ${F_ORT:-?}"
echo "   Urteil: ${F_URT:-(keins)}"
if [ -z "${F_BER:-}" ]; then
    echo "  FAIL: der Fallenlauf hat nichts gemessen"
    fail=1
else
    if [ "$F_BER" -gt "$SCHW" ]; then
        echo "  PASS: die Falle reisst die Schwelle ($F_BER > $SCHW Zyklen)"
    else
        echo "  FAIL: die Falle blieb unter der Schwelle ($F_BER <= $SCHW) -- die Zeile sieht sie nicht"
        fail=1
    fi
    if grep -q "^sperre  : FAILURES" "$FALLE"; then
        echo "  PASS: die Zeile faellt durch (nicht nur eine groessere Zahl -- das URTEIL kippt)"
    else
        echo "  FAIL: die Zeile bleibt gruen, obwohl die Falle steht -- sie gattert nicht"
        fail=1
    fi
    # 3. Der ORT. Ohne ihn ist die Zahl ein Alarm ohne Adresse.
    if grep -qE "ohne die erklaerten Langhalter: [0-9]+ Zyklen @ kernel/src/sperrmark\.rs:[0-9]+" "$FALLE"; then
        echo "  PASS: die Zeile NENNT den Ort ($F_ORT) -- Datei und Zeile des 'while let'"
    else
        echo "  FAIL: die Zeile nennt den Ort nicht (gefunden: ${F_ORT:-nichts})"
        fail=1
    fi
    # Und der Sprung muss deutlich sein, nicht im Rauschen liegen.
    if [ -n "${R_BER:-}" ] && [ "$R_BER" -gt 0 ]; then
        echo "  Faktor gegenueber der Grundlinie: $((F_BER / R_BER))x  ($R_BER -> $F_BER Zyklen)"
    fi
fi

# ------------------------------------------------------------------------------------------------
# Rueckbau -- niemand soll versehentlich mit der Sonde weitermessen.
# ------------------------------------------------------------------------------------------------
bauen "selftest" || exit 1
echo "== Rueckbau auf --features selftest =="

if [ "$fail" -eq 0 ]; then
    echo "== SPERR-GEGENPROBE: ALL PASS =="
else
    echo "== SPERR-GEGENPROBE: FAILURES =="
fi
exit "$fail"
