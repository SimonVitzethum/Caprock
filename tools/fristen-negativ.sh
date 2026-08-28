#!/usr/bin/env bash
# **Stufe A / A2: die rote Haelfte der Frist.**
#
# `docs/linux-kompatibilitaet-caprock.md` schreibt eine dieser vier woertlich vor:
#
#   *„der Timer entfernt alle Gruende statt des einen -- ein danebenliegender, aus anderem Grund
#    geparkter Thread muss losfallen, und eine zweite Sonde muss das sehen."*
#
# Der zweite Halbsatz ist die eigentliche Arbeit. An einem Thread mit **einem** Grund sind
# „entferne den einen" und „entferne alle" nicht zu unterscheiden -- die Mutation waere folgenlos,
# und ein folgenloser Negativfall belegt gar nichts. Die `uhr`-Zeile faehrt deshalb seit dem
# 2026-08-28 einen dritten Gang, in dem der Kernel den Wartenden **pausiert**, waehrend er wartet.
#
# **Nicht abgedeckt, und das steht hier, damit es niemand fuer abgedeckt haelt:** der Ueberlauf
# von `FRISTEN_JE_TICK` (16). Ihn zu treffen braucht siebzehn Threads mit Fristen im selben Tick;
# die Sonde hat einen. Der Fehler, den er hatte, ist gelesen und behoben, nicht gefahren.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/fristneg.XXXXXX")"
fail=0

DATEIEN=( "crates/caprock-sched/src/lib.rs" "crates/caprock-microkit/src/lib.rs" )
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 900 ./test-qemu-x86.sh > "$1" 2>&1; }

ZEILE="uhr    :"
# **Abgelesen wird NUR auf der eigenen Berichtszeile.** Ein gleichnamiges Konjunkt einer fremden
# Zeile hat in `tools/tls-negativ.sh` schon einmal zwei Laeufe lang eine Mutation falsch rot
# gemeldet.
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
rot()   { pruefe "$1" "$2" "$3" false; }

# **Die Stelligkeitspruefung zaehlt Zeilen, die `sed` DRUCKT -- nicht Fundstellen.** Ein Ersatz mit
# `\n` darin laesst `sed -n p` zwei Zeilen ausgeben und meldet „2 Treffer, erwartet 1". Gekostet hat
# das einen Lauf von M4. Mutationen bleiben deshalb einzeilig; wo das nicht geht, gehoert die
# Zaehlung auf `grep -c` des Musters umgestellt.
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

echo "== Stufe A / A2: Gegenproben zur Frist =="

echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^uhr    : ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot"
    grep -E "^uhr" "$TMP/orig.log" | head -2; exit 1
fi
echo "  PASS: uhr ist vorher gruen"
grep -oE "\| A2:.*" "$TMP/orig.log" | head -1

# --- M1: die Frist nimmt ALLE Gruende ------------------------------------------------------------
#
# **Die vom Dokument vorgeschriebene Gegenprobe.** Der Thread steht in `IPC` UND `PAUSE`; nimmt die
# Frist beide, faellt er los und schreibt sein Ergebnis, obwohl er pausiert ist. Das ist der
# D9-Fehler mit der Uhr als Generalschluessel.
#
# Die beiden Gaenge OHNE zweiten Grund bleiben dabei gruen -- dort sind „einer" und „alle" dasselbe,
# und genau deshalb braucht diese Zeile ihren dritten Gang.
echo "-- M1: fristen_faellig entfernt die ganze Grund-Menge --"
if mutiere crates/caprock-sched/src/lib.rs \
   's|^            self.tcbs\[i\].reasons.remove(grund);$|            self.tcbs[i].reasons = BlockReasons(0);|' M1 1; then
    lauf "$TMP/m1.log"
    rot   M1 "$TMP/m1.log" "zweitgrund-haelt"
    # `lief-nicht` war beim ersten Lauf faelschlich gruen -- fuenf Ticks Zuschlag reichten einem
    # `IDLE_PRIO`-Thread nicht, um ueberhaupt drankommen zu koennen. Das Fenster ist jetzt 20 Ticks.
    rot   M1 "$TMP/m1.log" "lief-nicht"
    rot   M1 "$TMP/m1.log" "nur-pause"
    gruen M1 "$TMP/m1.log" "frist-weckt"
    gruen M1 "$TMP/m1.log" "signal-gewinnt"
    gruen M1 "$TMP/m1.log" "nicht-zu-frueh"
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: die Frist feuert NIE --------------------------------------------------------------------
#
# **Nicht isolierend, und das ist kausal, nicht schlampig:** wer nie geweckt wird, erreicht den
# zweiten Gang nicht. Die Zeile darf hier also mehr als ein Konjunkt verlieren; gepruft wird, dass
# die Nachbarn AUSSERHALB von A2 stehen bleiben -- sonst maesse die Mutation die halbe Sonde.
echo "-- M2: eine faellige Frist gilt als zukuenftig --"
if mutiere crates/caprock-sched/src/lib.rs \
   's|^            if f > self.now {$|            if true {|' M2 1; then
    lauf "$TMP/m2.log"
    rot   M2 "$TMP/m2.log" "frist-weckt"
    gruen M2 "$TMP/m2.log" "rate-plausibel"
    gruen M2 "$TMP/m2.log" "zaehler-waechst"
    gruen M2 "$TMP/m2.log" "passt-zum-tick"
else
    echo "  FAIL: M2 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M3: die Frist feuert SOFORT ------------------------------------------------------------------
#
# Ein Wecker, der sofort feuert, ist kein Warten, sondern ein Rueckgabewert -- und an `frist-weckt`
# allein saehe er richtig aus. Dass dabei auch `aufbau=false` faellt, ist die **Diagnose** und kein
# zweiter Fehlschlag: mit einer 1-Tick-Frist ist der dritte Gang nicht aufzubauen, weil sie feuert,
# bevor die Pause steht. Genau dafuer stehen die Teilfelder in der Zeile.
# **Diese Gegenprobe hat das Konjunkt umgebaut, das sie pruefen sollte.** Beim ersten Lauf blieb
# `nicht-zu-frueh` gruen: es las die Zeitspanne, die der EL0-Thread um sein `WAIT` gelegt hatte --
# also Frist PLUS Einplanung, und die Einplanung ist auf `IDLE_PRIO` der groessere Summand (240 ms
# fuer eine 100-ms-Frist). „Zu frueh" war damit strukturell unsichtbar. Gemessen wird jetzt
# kernelseitig in Ticks.
echo "-- M3: WAIT stellt jede Frist auf 1 Tick --"
if mutiere crates/caprock-microkit/src/lib.rs \
   's|ops.frist_setzen(core, ticks, caprock_sched::BlockReasons::IPC);|ops.frist_setzen(core, 1, caprock_sched::BlockReasons::IPC);|' M3 1; then
    lauf "$TMP/m3.log"
    rot   M3 "$TMP/m3.log" "nicht-zu-frueh"
    gruen M3 "$TMP/m3.log" "frist-weckt"
    gruen M3 "$TMP/m3.log" "rate-plausibel"
else
    echo "  FAIL: M3 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M4: berichtet werden nur die LAUFFAEHIGEN ----------------------------------------------------
#
# **Das ist der Fehler, den der Bau dieser Zeile gefunden hat**, wiederhergestellt. Vor dem
# 2026-08-28 trug `out` nur die Threads, die durch die Frist lauffaehig wurden. Ein Thread mit einem
# zweiten Grund fiel durch beide Maschen: kein `ERR_TIMEOUT` im Frame, keine Abmeldung beim Objekt.
#
# Isolierend, und darin steckt die Aussage: `zweitgrund-haelt` bleibt **gruen** (er laeuft ja
# richtigerweise nicht), und nur `code-trotzdem` faellt. Genau diese Trennung hat gefehlt, solange
# es die zweite Aussage nicht gab.
echo "-- M4: nur lauffaehig Gewordene bekommen einen Code --"
if mutiere crates/caprock-sched/src/lib.rs \
   's|^            out\[n\] = self.id(i);$|            if !self.tcbs[i].reasons.is_empty() { continue; } out[n] = self.id(i);|' M4 1; then
    lauf "$TMP/m4.log"
    rot   M4 "$TMP/m4.log" "code-trotzdem"
    gruen M4 "$TMP/m4.log" "zweitgrund-haelt"
    gruen M4 "$TMP/m4.log" "frist-weckt"
    gruen M4 "$TMP/m4.log" "signal-gewinnt"
else
    echo "  FAIL: M4 -- Mutation nicht anwendbar"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then
    echo "== FRIST-GEGENPROBEN: ALL PASS =="
else
    echo "== FRIST-GEGENPROBEN: FAILURES =="
fi
exit "$fail"
