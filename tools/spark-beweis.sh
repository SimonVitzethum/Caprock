#!/usr/bin/env bash
# Caprock -- GNATprove ueber den SPARK-Portierungen (Experimente S1 und S2).
#
# Beantwortet zwei Fragen:
#   S1  Cap-Space  -- findet GNATprove auf Silver Level (Abwesenheit von Laufzeitfehlern)
#                     etwas, das das Verus-Modell nicht sieht?
#   S2  Scheduler  -- WIEVIEL des Scheduler-Kerns kommt ueberhaupt unter `SPARK_Mode => On`,
#                     und was findet der Beweiser dort?
#
# Das Skript ist ein GATTER, keine Anzeige:
#   * die Zahl der unbewiesenen Laufzeitpruefungen ist je Modul eine RATSCHE (darf nur
#     fallen) -- und die Bilanzen sind GETRENNT. Eine Summe ueber beide Module waere eine
#     Zahl, die kippt, sobald jemand am ANDEREN Modul etwas aendert;
#   * jede Gegenprobe muss ihre unterscheidbaren Ausgaenge liefern -- faellt einer weg, ist
#     die Probe nicht mehr sprechfaehig und das Skript bricht ab, statt gruen zu melden;
#   * eine Bilanz mit zu wenigen Pruefungen ist ein AUSGEFALLENER Lauf, kein bestandener.
#
# Werkzeugkette (benutzerlokal, kein root):
#   alr install gnatprove   ->  ~/.alire/bin/gnatprove
#   alr install gnat_native ->  ~/.alire/bin/gnatmake
set -euo pipefail

HIER="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPARK="$HIER/spark"
export PATH="$HOME/.alire/bin:$PATH"

# Die Zahlen, die stehen bleiben sollen. Jede Erhoehung ist ein neuer unbewiesener
# Laufzeitcheck und muss einzeln begruendet werden.
ERWARTET_S1=15          # Cap-Space, Laufzeitpruefungen
ERWARTET_S2=50          # Scheduler, Laufzeitpruefungen
ERWARTET_S2_ASSERT=8    # Scheduler, Zusicherungen: die `debug_assert_eq!(core, self.core)`
ERWARTET_S2_TERM=1      # Scheduler, Terminierung: [S2-T1] `migration_candidate`

# Untergrenzen fuer die Sprechprobe. Sie liegen deutlich unter dem Ist-Stand und fangen
# ausschliesslich den Fall ab, dass der Lauf gar nichts angesehen hat.
MINDESTENS_S1=50
MINDESTENS_S2=80

rot() { printf '\033[31m%s\033[0m\n' "$*"; }
gruen() { printf '\033[32m%s\033[0m\n' "$*"; }

command -v gnatprove >/dev/null || { rot "gnatprove fehlt (alr install gnatprove)"; exit 2; }
command -v gnatmake  >/dev/null || { rot "gnatmake fehlt (alr install gnat_native)"; exit 2; }

echo "== gnatprove: $(gnatprove --version | head -1) =="

# ---------------------------------------------------------------- ein Beweislauf --
#
# $1 Projektdatei   $2 Objektverzeichnis   $3 Klartextname
# $4 erwartete unbewiesene Laufzeitpruefungen   $5 Untergrenze der Sprechprobe
# $6 erwartete unbewiesene Zusicherungen   $7 erwartete unbewiesene Terminierungschecks
beweislauf() {
   local GPR="$1" OBJ="$2" NAME="$3" ERW="$4" MIN="$5" ERW_A="$6" ERW_T="$7" EINHEIT="$8"
   local LOG="/tmp/spark-beweis-$OBJ.log"

   cd "$SPARK"
   # **Das Objektverzeichnis wird GELEERT, und das ist keine Bequemlichkeit.**
   # gnatprove sammelt seine Ergebnisse in der Sitzungsablage AUF: aendert sich die
   # Quelldateiliste eines Projekts, stehen die alten Ergebnisse weiter in `gnatprove.out`,
   # und die Bilanz ist die VEREINIGUNG. Genau das ist beim Aufteilen von `caprock.gpr` in
   # zwei Projekte passiert -- S1 meldete 232 Laufzeitpruefungen statt 99, also die Summe
   # beider Module. Diesmal fiel es auf, weil die Ratsche riss; in der anderen Richtung
   # (eine Datei faellt weg) haette dieselbe Mechanik still MEHR bewiesen gemeldet, als
   # gerechnet wurde. Eine Bilanz muss zu den Quellen gehoeren, die sie zu beschreiben
   # behauptet -- und das kostet hier ein paar Minuten Beweiszeit.
   rm -rf "$OBJ"
   mkdir -p "$OBJ"
   gnatprove -P "$GPR" --level=3 --report=all -j0 --timeout=30 > "$LOG" 2>&1 || true

   local BILANZ="$SPARK/$OBJ/gnatprove/gnatprove.out"
   [[ -f "$BILANZ" ]] || { rot "$NAME: keine Bilanz erzeugt -- der Lauf ist ausgefallen"; exit 1; }

   # Zweite Sicherung derselben Frage, unabhaengig vom Leeren: die Bilanz muss GENAU EINE
   # Einheit nennen, und zwar die erwartete.
   local EINHEITEN
   EINHEITEN=$(grep -oE '^in unit [a-z_0-9]+' "$BILANZ" | awk '{print $3}' | sort -u || true)
   if [[ "$EINHEITEN" != "$EINHEIT" ]]; then
      rot "$NAME: die Bilanz nennt die Einheit(en) '$EINHEITEN', erwartet war genau '$EINHEIT'"
      rot "(eine Bilanz ueber mehr als ein Modul ist eine Summe, keine Kennzahl)"
      exit 1
   fi

   # Eine Fehlermeldung des Werkzeugs darf nicht als „nichts gefunden" durchgehen.
   if grep -qE '^ *error:' "$LOG"; then
      rot "$NAME: gnatprove meldet einen Fehler -- kein Urteil"
      grep -E '^ *error:' "$LOG" | head -10
      exit 1
   fi

   local ZEILE GESAMT UNBEWIESEN
   ZEILE=$(grep -E '^Run-time Checks' "$BILANZ" || true)
   [[ -n "$ZEILE" ]] || { rot "$NAME: Bilanz ohne Zeile 'Run-time Checks' -- Format geaendert?"; exit 1; }
   GESAMT=$(awk '{print $3}' <<<"$ZEILE")
   UNBEWIESEN=$(awk '{print $NF}' <<<"$ZEILE")

   # Sprechprobe: eine Bilanz mit zu wenigen Pruefungen waere kein bestandener Lauf,
   # sondern ein ausgefallener. Genau die Form, die dieses Projekt schon zweimal bezahlt hat.
   if [[ "$GESAMT" -lt "$MIN" ]]; then
      rot "$NAME: nur $GESAMT Laufzeitpruefungen (erwartet >= $MIN) -- der Lauf hat nichts gesehen"
      exit 1
   fi

   echo "$NAME -- Laufzeitpruefungen: $GESAMT gesamt, $UNBEWIESEN unbewiesen (erwartet: $ERW)"

   if [[ "$UNBEWIESEN" -gt "$ERW" ]]; then
      rot "$NAME: RATSCHE GERISSEN: $UNBEWIESEN > $ERW"
      grep -E '^ (high|medium|low):' -A2 "$LOG" | head -80
      exit 1
   fi
   if [[ "$UNBEWIESEN" -lt "$ERW" ]]; then
      rot "$NAME: weniger unbewiesen als erwartet ($UNBEWIESEN < $ERW)."
      rot "Das ist gute Nachricht UND ein Fehler: setze die Erwartung herunter."
      exit 1
   fi

   # Zusicherungen und Terminierung sind eigene Zeilen -- fuer den Scheduler tragen sie
   # zwei Befundklassen, die es im Cap-Space gar nicht gab (die weggekompilierten
   # `debug_assert_eq!` und die unbegrenzte Kettenwanderung). Wer nur „Run-time Checks"
   # liest, sieht beide nicht.
   ratsche_zeile "$BILANZ" "$NAME" "Assertions" "$ERW_A"
   ratsche_zeile "$BILANZ" "$NAME" "Termination" "$ERW_T"

   # Der ganze Portierungsumfang muss unter SPARK_Mode => On stehen -- sonst sagt die Zahl
   # oben nichts ueber das Modul, sondern nur ueber den betrachteten Ausschnitt.
   local ANALYSIERT A B
   ANALYSIERT=$(grep -oE '[0-9]+ subprograms and packages out of [0-9]+ analyzed' "$BILANZ" | head -1)
   [[ -n "$ANALYSIERT" ]] || { rot "$NAME: Bilanz nennt keinen Analyseumfang"; exit 1; }
   A=$(awk '{print $1}' <<<"$ANALYSIERT"); B=$(awk '{print $7}' <<<"$ANALYSIERT")
   if [[ -z "$A" || -z "$B" ]]; then
      rot "$NAME: Analyseumfang nicht lesbar ('$ANALYSIERT') -- Bilanzformat geaendert?"
      exit 1
   fi

   # **Die Ausnahmen sind eine MENGE VON NAMEN, keine Zahl.** Eine Ratsche ueber „hoechstens
   # drei uebersprungen" greift gegen Zuwachs, nicht gegen AUSTAUSCH -- und Austausch fuehlt
   # sich beim Umbauen wie Fortschritt an (dieselbe Lehre wie `IDENTITY_DEBTS`).
   #
   # Erlaubt ist ausschliesslich, was in DIESER Quelle gar keinen Rumpf hat: die zwei
   # importierten HAL-Aufrufe und der generische Ada-Freigeber. Kein Unterprogramm der
   # portierten Scheduler-Logik steht darunter.
   local UEBERSPRUNGEN ERLAUBT
   UEBERSPRUNGEN=$(grep 'skipped; body is SPARK_Mode => Off' "$BILANZ" \
                   | sed -E 's/^ *([^ ]+) at .*/\1/' | sort || true)
   case "$NAME" in
      *Scheduler*) ERLAUBT=$(printf '%s\n' \
                     "Caprock_Sched.Free_Parkedgp4873.Free_Parked" \
                     "Caprock_Sched.Init_Thread_Frame" \
                     "Caprock_Sched.Stapeladresse" | sort) ;;
      *)           ERLAUBT="" ;;
   esac
   # Der generische Instanzname traegt eine vom Compiler vergebene Nummer -- sie darf sich
   # aendern, ohne dass sich etwas geaendert hat.
   local U_NORM E_NORM
   U_NORM=$(sed -E 's/gp[0-9]+/gpN/' <<<"$UEBERSPRUNGEN")
   E_NORM=$(sed -E 's/gp[0-9]+/gpN/' <<<"$ERLAUBT")
   if [[ "$U_NORM" != "$E_NORM" ]]; then
      rot "$NAME: die uebersprungenen Ruempfe weichen von der erklaerten Menge ab."
      rot "erlaubt:"; printf '  %s\n' $E_NORM
      rot "gefunden:"; printf '  %s\n' $U_NORM
      exit 1
   fi
   echo "$NAME -- SPARK-Abdeckung: $A von $B Unterprogrammen; uebersprungene Ruempfe: $(wc -w <<<"$E_NORM") (benannt)"
}

# Eine weitere Bilanzzeile als Ratsche. Fehlt die Zeile ganz, sind es 0 -- und das ist
# ein gueltiger Wert, kein Ausfall (der Cap-Space hat keine Terminierungsfunde).
ratsche_zeile() {
   local BILANZ="$1" NAME="$2" ZEILE="$3" ERW="$4"
   local IST
   IST=$(grep -E "^$ZEILE " "$BILANZ" | awk '{print $NF}' || true)
   [[ -n "$IST" ]] || IST=0
   [[ "$IST" == "." ]] && IST=0
   if [[ "$IST" != "$ERW" ]]; then
      rot "$NAME: $ZEILE unbewiesen = $IST, erwartet $ERW"
      rot "(hoeher = neue Schuld; niedriger = gute Nachricht, dann Erwartung herunter)"
      exit 1
   fi
   echo "$NAME -- $ZEILE: $IST unbewiesen (erwartet: $ERW)"
}

# --------------------------------------------------------------- die zwei Laeufe --
beweislauf caprock.gpr       obj       "S1 Cap-Space" "$ERWARTET_S1" "$MINDESTENS_S1" 0 0 \
           caprock_cap
beweislauf caprock_sched.gpr obj-sched "S2 Scheduler" "$ERWARTET_S2" "$MINDESTENS_S2" \
           "$ERWARTET_S2_ASSERT" "$ERWARTET_S2_TERM" caprock_sched

# ------------------------------------------------------------- Gegenprobe S1/F13 --
cd "$SPARK"
mkdir -p obj-demo
gnatmake -q -gnat2022 -gnato -gnatVa -Isrc -Idemo -D obj-demo \
         -o obj-demo/f13_gegenprobe demo/f13_gegenprobe.adb
AUS=$(./obj-demo/f13_gegenprobe)

# Drei unterscheidbare Ausgaenge. Fehlt einer, ist die Probe stumm.
grep -q '1 Positivkontrolle.*audit_cdt =  0'   <<<"$AUS" || { rot "F13: Positivkontrolle nicht bestanden -- die Probe ist nicht sprechfaehig"; echo "$AUS"; exit 1; }
grep -q '2 Kontrolle.*audit_cdt =  6'          <<<"$AUS" || { rot "F13: Kontrolle meldet nicht Code 6"; echo "$AUS"; exit 1; }
grep -q '3 Fund.*CONSTRAINT_ERROR'             <<<"$AUS" || { rot "F13: der Fund reproduziert NICHT mehr -- behoben? dann Probe anpassen"; echo "$AUS"; exit 1; }
echo "$AUS"

# ---------------------------------------------------------- Gegenprobe S2/F1+F8 --
#
# `priority` ist ein `u8`, `queues` hat NPRIO = 8 Faecher. Zwischen Ausgang 2 und 3
# wandert AUSSCHLIESSLICH die Prioritaet -- damit isoliert die Probe den Wertebereich
# von allem anderen.
gnatmake -q -gnat2022 -gnato -gnatVa -Isrc -Idemo -D obj-demo \
         -o obj-demo/s2_prio_gegenprobe demo/s2_prio_gegenprobe.adb
AUS2=$(./obj-demo/s2_prio_gegenprobe)

grep -q '1 Positivkontrolle (prio 0): zugelassen=TRUE' <<<"$AUS2" || { rot "S2-F1: Positivkontrolle nicht bestanden -- die Probe ist nicht sprechfaehig"; echo "$AUS2"; exit 1; }
grep -q '2 Kontrolle        (prio 7): zugelassen=TRUE' <<<"$AUS2" || { rot "S2-F1: prio 7 (letzte gueltige Stufe) faellt durch -- die Probe misst etwas anderes"; echo "$AUS2"; exit 1; }
grep -q '3 Fund             (prio 8): CONSTRAINT_ERROR' <<<"$AUS2" || { rot "S2-F1: der Fund reproduziert NICHT mehr -- geklemmt? dann Probe anpassen"; echo "$AUS2"; exit 1; }
echo "$AUS2"

# ------------------------------------------------- Gegenprobe S2/Parked (linear) --
#
# Diese faellt GNATprove selbst, nicht ein Programm: zwei Unterprogramme, die sich in
# GENAU EINER Zeile unterscheiden (`Admit` vorhanden / nicht). Erwartet:
#   * die Positivkontrolle BEWEIST Leckfreiheit  -> die Probe ist sprechfaehig
#   * der Fund meldet GENAU EIN Leck             -> Rusts `#[must_use]` ist hier ein Fehler
mkdir -p obj-gegenprobe
gnatprove -P gegenprobe.gpr -u s2_parked_probe --level=1 --report=all -j0 --timeout=10 \
   > /tmp/spark-beweis-parked.log 2>&1 || true

if grep -qE '^ *error:' /tmp/spark-beweis-parked.log; then
   rot "S2-Parked: gnatprove meldet einen Fehler -- kein Urteil"
   grep -E '^ *error:' /tmp/spark-beweis-parked.log | head -10
   exit 1
fi

LECK_OK=$(grep -c 'info: absence of resource or memory leak at end of scope proved' /tmp/spark-beweis-parked.log || true)
LECK_FUND=$(grep -c 'medium: resource or memory leak might occur at end of scope' /tmp/spark-beweis-parked.log || true)

if [[ "$LECK_OK" -lt 1 ]]; then
   rot "S2-Parked: die Positivkontrolle beweist KEINE Leckfreiheit -- die Probe ist nicht sprechfaehig"
   rot "(ohne sie waere 'genau ein Leck' auch dann wahr, wenn die Eigentumspruefung gar nicht laeuft)"
   exit 1
fi
if [[ "$LECK_FUND" -ne 1 ]]; then
   rot "S2-Parked: $LECK_FUND Lecks statt genau 1 -- der Fund reproduziert nicht mehr"
   grep -E 'resource or memory leak' /tmp/spark-beweis-parked.log | head -10
   exit 1
fi
echo "S2-Parked: Positivkontrolle leckfrei bewiesen ($LECK_OK), fallengelassener Parked = 1 Leck"

gruen "== SPARK-BEWEIS: ALL PASS =="
