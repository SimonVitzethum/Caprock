#!/usr/bin/env bash
# ================================================================================================
# DIE #DF-SONDE -- ein ECHTER Double Fault, und die Gegenprobe dazu
# ================================================================================================
#
# **Warum es diesen eigenen Lauf gibt.** Ein IST-Eintrag, der nie benutzt wurde, ist von einem
# falsch aufgesetzten nicht zu unterscheiden. Die `ist`-Zeile der Hauptsuite loest deshalb NMI und
# #MC wirklich aus -- fuer #DF (Vektor 8) geht das ueber `int 8` aber nicht: bei einem
# Software-Interrupt schiebt die CPU keinen Fehlercode ein, der Stub fuer Vektor 8 erwartet aber
# einen, und das Frame-Layout waere um 8 Byte verschoben. Ein echter #DF haelt den Kernel ausserdem
# an (er IST ein Abort) -- in der regulaeren Suite waere das ein roter Lauf.
#
# Deshalb: eigener Bau (`--features selftest,dfprobe`), eigener QEMU-Aufruf, eigenes Urteil.
#
# **Drei Aussagen, und die dritte ist die eigentliche:**
#
#   1. POSITIV   -- der #DF spricht: Kern, RIP/CS/RSP, CR2, IST-Region, Wasserstand.
#   2. LAGE      -- die Frame-Adresse liegt IN der #DF-IST-Region. Das ist der Beleg, dass der
#                   IST-Mechanismus gegriffen hat und dass der Gate-Index stimmt (ein Off-by-one
#                   laedt den Nachbarstack und faellt hier auf).
#   3. GEGENPROBE-- derselbe Lauf OHNE IST am #DF-Gate (`dfprobe-kein-ist`) muss STUMM bleiben:
#                   Triple Fault, keine `[#DF]`-Zeile. Ohne diese dritte Aussage belegte der Lauf
#                   nur, dass ein #DF irgendwie spricht -- nicht, dass es am IST-Stack HAENGT.
#
# Der Lauf laesst den Baum am Ende in der regulaeren Konfiguration zurueck (`--features selftest`),
# damit niemand versehentlich mit der Sonde weitermisst.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
mkdir -p build/diag

bauen() {   # $1 = Featureliste
    echo "== baue mit --features $1 =="
    ./build-x86.sh --features "$1" >build/diag/df-sonde-bau.log 2>&1
    local rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "  FAIL: Bau mit '$1' fehlgeschlagen (rc=$rc)"
        tail -20 build/diag/df-sonde-bau.log | sed 's/^/        /'
        return 1
    fi
    grep -E "^rustflags|^multiboot" build/diag/df-sonde-bau.log | sed 's/^/        /'
    return 0
}

fahren() {  # $1 = Ausgabedatei
    SEK=30 OUT="$1" ./tools/boot-x86-log.sh
}

POS=build/diag/df-sonde-positiv.log
NEG=build/diag/df-sonde-gegenprobe.log

# ------------------------------------------------------------------------------------------------
# 1. POSITIV: mit IST am #DF-Gate
# ------------------------------------------------------------------------------------------------
if ! bauen "selftest,dfprobe"; then
    exit 1
fi
fahren "$POS"

echo "== Positivfall: der #DF spricht =="
if grep -q "\[#DF\] DOUBLE FAULT" "$POS"; then
    echo "  PASS: der Double Fault MELDET SICH -- ohne IST-Stack stuende hier ein Triple Fault"
    sed -n '/\[#DF\] DOUBLE FAULT/,/ENDSTAND/p' "$POS" | sed 's/^/        /'
else
    echo "  FAIL: keine [#DF]-Meldung im Protokoll -- der Lauf ist stumm geblieben"
    tail -5 "$POS" | sed 's/^/        /'
    fail=1
fi

# Die entscheidende Zeile: liegt der Frame IN der #DF-Region?
if grep -q "DARIN (IST hat gegriffen)" "$POS"; then
    echo "  PASS: die Frame-Adresse liegt IN der #DF-IST-Region -- der Mechanismus hat gegriffen"
    echo "        und der Gate-Index stimmt (ein Off-by-one laedt den Nachbarstack und faellt hier auf)"
elif grep -q "NICHT darin -- IST-Aufbau falsch" "$POS"; then
    echo "  FAIL: der Handler lief NICHT auf dem #DF-IST-Stack -- IST-Aufbau falsch"
    fail=1
else
    echo "  FAIL: die Lage-Aussage fehlt ganz (Meldung unvollstaendig?)"
    fail=1
fi

# Die Sonde SOLL den Ueberlauf simulieren: CR2 muss die verbogene Adresse tragen, nicht 0.
if grep -qE "cr2=0x0000300000000000|cr2=0x00002fff" "$POS"; then
    echo "  PASS: CR2 traegt die Adresse, an der der URSPRUENGLICHE Fault scheiterte -- genau die"
    echo "        Groesse, die bei einem echten Stackueberlauf auf die Guard-Page zeigt"
else
    echo "  FAIL: CR2 zeigt nicht auf die Sondenadresse:"
    grep -m1 "cr2=" "$POS" | sed 's/^/        /'
    fail=1
fi

# **Die Zahl, aus der `IST_STACK_BYTES` hergeleitet ist.** Sie steht in der Ausgabe und nicht nur
# in einem Kommentar -- eine Zahl, die nur im Kommentar steht, veraltet still.
#
# **Gelesen wird der ENDSTAND, nicht der Zwischenstand.** Die Zeile mitten im Bericht kennt die
# Tiefe der danach folgenden Ausgaben strukturell nicht und waere als Grundlage systematisch zu
# klein -- dieselbe Form wie ein Beobachtungsfenster, das vor dem gemessenen Ereignis endet.
BEN="$(grep -m1 "IST\[#DF\] ENDSTAND" "$POS" | grep -oE "[0-9]+ von [0-9]+" | head -1)"
if [ -n "$BEN" ]; then
    U="${BEN%% *}"; G="${BEN##* }"
    echo "  MESSUNG: der #DF-Pfad hat $U von $G B seines IST-Stacks benutzt (Rahmen + Diagnoseausgabe)"
    if [ "$U" -ge "$G" ]; then
        echo "  FAIL: der IST-Stack ist AUFGEBRAUCHT -- IST_STACK_BYTES ist zu klein"
        fail=1
    elif [ "$((U * 2))" -ge "$G" ]; then
        echo "  FAIL: ueber die Haelfte des IST-Stacks benutzt -- unter Faktor 2 ist die Zahl"
        echo "        flatterhaft; ein IST-Stack hat selbst KEINE Guard-Page"
        fail=1
    else
        echo "  PASS: Reserve Faktor $(( G / (U>0 ? U : 1) )) -- und der Wasserstand ist GEMESSEN,"
        echo "        nicht abgezaehlt (core::fmt laesst sich nicht abzaehlen)"
    fi
else
    echo "  FAIL: kein Wasserstand im Protokoll -- die Fuellung oder der Messhaken fehlt"
    fail=1
fi

# Der betroffene EL1-Stack: die Sonde laeuft im Idle-Kontext, aber `TSS.rsp0` traegt den
# Kernel-Stack des zuletzt fuer Ring 3 vorbereiteten Threads -- genau der, der bei einem echten
# Ueberlauf betroffen waere.
if grep -q "betroffener EL1-Stack Wasserstand" "$POS"; then
    echo "  PASS: der Bericht nennt AUCH den betroffenen EL1-Stack samt Wasserstand"
    grep -m2 "betroffener EL1-Stack" "$POS" | sed 's/^/        /'
else
    echo "  FAIL: der betroffene EL1-Stack fehlt im Bericht"
    grep -m1 "betroffener EL1-Stack" "$POS" | sed 's/^/        /'
    fail=1
fi

# ------------------------------------------------------------------------------------------------
# 2. GEGENPROBE: dasselbe OHNE IST am #DF-Gate
# ------------------------------------------------------------------------------------------------
#
# **Die Mutation isoliert genau eine Groesse** (`ist_fuer_vektor` gibt fuer 8 eine 0). NMI und #MC
# behalten ihre Stacks -- eine Mutation, die zwei Dinge zugleich kaputtmacht, beweist nichts ueber
# das gemeinte.
echo
if ! bauen "selftest,dfprobe,dfprobe-kein-ist"; then
    exit 1
fi
fahren "$NEG"

echo "== Gegenprobe: dasselbe OHNE IST am #DF-Gate =="
if grep -q "\[#DF\] DOUBLE FAULT" "$NEG"; then
    echo "  FAIL: der Lauf OHNE IST hat trotzdem gesprochen -- dann haengt die Meldung nicht am"
    echo "        IST-Stack, und der Positivfall belegt nichts ueber ihn"
    fail=1
else
    echo "  PASS: OHNE IST bleibt der Lauf STUMM (Triple Fault) -- damit ist belegt, dass die"
    echo "        laute Meldung im Positivfall AM IST-STACK haengt und nicht daran, dass ein #DF"
    echo "        ohnehin irgendwie spricht"
fi
# Sprechprobe der Gegenprobe: der Lauf muss ueberhaupt bis zur Sonde gekommen sein, sonst waere
# „stumm" auch die Antwort eines Kernels, der gar nicht erst gebootet hat.
if grep -q "dfsonde : loese jetzt einen ECHTEN #DF aus" "$NEG"; then
    echo "  PASS: Sprechprobe -- der Lauf ist bis zur Sonde gekommen (die Ankuendigungszeile steht"
    echo "        im Protokoll); 'stumm' ist also das Ergebnis des #DF und nicht eines toten Boots"
else
    echo "  FAIL: die Gegenprobe kam gar nicht bis zur Sonde -- 'keine #DF-Zeile' sagt dann nichts"
    tail -5 "$NEG" | sed 's/^/        /'
    fail=1
fi

# ------------------------------------------------------------------------------------------------
# 3. Baum zurueck in die regulaere Konfiguration
# ------------------------------------------------------------------------------------------------
echo
bauen "selftest" >/dev/null || echo "  WARNUNG: Rueckbau auf 'selftest' fehlgeschlagen"

echo
if [ "$fail" -eq 0 ]; then
    echo "== DF-SONDE: ALL PASS =="
else
    echo "== DF-SONDE: FAILURES =="
fi
exit "$fail"
