#!/usr/bin/env bash
# Caprock -- GNATprove ueber der SPARK-Portierung des Cap-Space (Experiment).
#
# Beantwortet: findet GNATprove auf Silver Level (Abwesenheit von Laufzeitfehlern)
# am Cap-Space etwas, das das Verus-Modell nicht sieht?
#
# Das Skript ist ein GATTER, keine Anzeige:
#   * die Zahl der unbewiesenen Laufzeitpruefungen ist eine RATSCHE (darf nur fallen);
#   * die Gegenprobe zu [F13] muss GENAU drei unterscheidbare Ausgaenge liefern --
#     faellt einer davon weg, ist die Probe nicht mehr sprechfaehig und das Skript
#     bricht ab, statt gruen zu melden.
#
# Werkzeugkette (benutzerlokal, kein root):
#   alr install gnatprove   ->  ~/.alire/bin/gnatprove
#   alr install gnat_native ->  ~/.alire/bin/gnatmake
set -euo pipefail

HIER="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPARK="$HIER/spark"
export PATH="$HOME/.alire/bin:$PATH"

# Die Zahl, die stehen bleiben soll. Jede Erhoehung ist ein neuer unbewiesener
# Laufzeitcheck und muss einzeln begruendet werden.
ERWARTET_UNBEWIESEN=15

rot() { printf '\033[31m%s\033[0m\n' "$*"; }
gruen() { printf '\033[32m%s\033[0m\n' "$*"; }

command -v gnatprove >/dev/null || { rot "gnatprove fehlt (alr install gnatprove)"; exit 2; }
command -v gnatmake  >/dev/null || { rot "gnatmake fehlt (alr install gnat_native)"; exit 2; }

echo "== gnatprove: $(gnatprove --version | head -1) =="

# ---------------------------------------------------------------- Beweislauf --
cd "$SPARK"
mkdir -p obj obj-demo
gnatprove -P caprock.gpr --level=3 --report=all -j0 --timeout=30 > /tmp/spark-beweis.log 2>&1 || true

BILANZ="$SPARK/obj/gnatprove/gnatprove.out"
[[ -f "$BILANZ" ]] || { rot "keine Bilanz erzeugt -- der Lauf ist ausgefallen"; exit 1; }

# Nur die Zeile "Run-time Checks" zaehlt fuer Silver Level.
ZEILE=$(grep -E '^Run-time Checks' "$BILANZ" || true)
[[ -n "$ZEILE" ]] || { rot "Bilanz ohne Zeile 'Run-time Checks' -- Format geaendert?"; exit 1; }
GESAMT=$(awk '{print $3}' <<<"$ZEILE")
UNBEWIESEN=$(awk '{print $NF}' <<<"$ZEILE")

# Sprechprobe: eine Bilanz mit 0 Pruefungen waere kein bestandener Lauf, sondern ein
# ausgefallener. Genau die Form, die dieses Projekt schon zweimal bezahlt hat.
if [[ "$GESAMT" -lt 50 ]]; then
   rot "nur $GESAMT Laufzeitpruefungen -- der Lauf hat nichts gesehen, kein Urteil"
   exit 1
fi

echo "Laufzeitpruefungen: $GESAMT gesamt, $UNBEWIESEN unbewiesen (erwartet: $ERWARTET_UNBEWIESEN)"

if [[ "$UNBEWIESEN" -gt "$ERWARTET_UNBEWIESEN" ]]; then
   rot "RATSCHE GERISSEN: $UNBEWIESEN > $ERWARTET_UNBEWIESEN"
   grep -E '^ (high|medium|low):' -A2 /tmp/spark-beweis.log | head -80
   exit 1
fi
if [[ "$UNBEWIESEN" -lt "$ERWARTET_UNBEWIESEN" ]]; then
   rot "Weniger unbewiesen als erwartet ($UNBEWIESEN < $ERWARTET_UNBEWIESEN)."
   rot "Das ist gute Nachricht UND ein Fehler: setze ERWARTET_UNBEWIESEN herunter."
   exit 1
fi

# Der ganze Portierungsumfang muss unter SPARK_Mode => On stehen -- sonst sagt die
# Zahl oben nichts ueber das Modul, sondern nur ueber den betrachteten Ausschnitt.
ANALYSIERT=$(grep -oE '[0-9]+ subprograms and packages out of [0-9]+ analyzed' "$BILANZ" | head -1)
[[ -n "$ANALYSIERT" ]] || { rot "Bilanz nennt keinen Analyseumfang"; exit 1; }
#  "<A> subprograms and packages out of <B> analyzed"  ->  Felder 1 und 7.
A=$(awk '{print $1}' <<<"$ANALYSIERT"); B=$(awk '{print $7}' <<<"$ANALYSIERT")
if [[ -z "$A" || -z "$B" ]]; then
   rot "Analyseumfang nicht lesbar ('$ANALYSIERT') -- Bilanzformat geaendert?"
   exit 1
fi
if [[ "$A" != "$B" ]]; then
   rot "nur $A von $B Unterprogrammen analysiert -- Teile liegen in SPARK_Mode => Off"
   exit 1
fi
echo "SPARK-Abdeckung: $A von $B Unterprogrammen (kein SPARK_Mode => Off)"

# ------------------------------------------------------------- Gegenprobe F13 --
gnatmake -q -gnat2022 -gnato -gnatVa -Isrc -Idemo -D obj-demo \
         -o obj-demo/f13_gegenprobe demo/f13_gegenprobe.adb
AUS=$(./obj-demo/f13_gegenprobe)

# Drei unterscheidbare Ausgaenge. Fehlt einer, ist die Probe stumm.
grep -q '1 Positivkontrolle.*audit_cdt =  0'   <<<"$AUS" || { rot "F13: Positivkontrolle nicht bestanden -- die Probe ist nicht sprechfaehig"; echo "$AUS"; exit 1; }
grep -q '2 Kontrolle.*audit_cdt =  6'          <<<"$AUS" || { rot "F13: Kontrolle meldet nicht Code 6"; echo "$AUS"; exit 1; }
grep -q '3 Fund.*CONSTRAINT_ERROR'             <<<"$AUS" || { rot "F13: der Fund reproduziert NICHT mehr -- behoben? dann Probe anpassen"; echo "$AUS"; exit 1; }

echo "$AUS"
gruen "== SPARK-BEWEIS: ALL PASS =="
