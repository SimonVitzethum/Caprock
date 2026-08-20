#!/usr/bin/env bash
# **Z6b: die rote Haelfte.** Gruene Konjunkte sind die eine Haelfte des Beweises; die andere ist,
# dass die Zeile ueberhaupt rot werden KANN — und zwar an der Stelle, die man gemeint hat.
#
# Jede Mutation stellt genau einen Befund wieder her. Geprueft wird nicht „irgendetwas ist rot",
# sondern:
#
#   1. die Mutation hat GEGRIFFEN   (ein sed-Muster, das nach einer Umbenennung nicht mehr passt,
#                                    waere ein lautlos abgeschalteter Negativfall),
#   2. das GEMEINTE Konjunkt ist gefallen,
#   3. bei M3 zusaetzlich: das ANDERE ist gruen geblieben — die Asymmetrie IST die Aussage.
#
# Punkt 2 ist der, um den es geht. Eine mutierte Datei faellt auch durch einen Uebersetzungsfehler,
# durch einen anderen Test oder durch einen Tippfehler im Muster; wer jeden Fehlschlag als Beleg
# nimmt, hat einen Pruefer, der gruen ist, sobald irgendetwas kaputt ist. Die erste D9-Gegenprobe
# hat genau das getan — sie machte zwei Dinge zugleich kaputt und fiel an der ERSTEN Aussage
# durch, nicht an der gemeinten.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/dbgneg.XXXXXX")"
fail=0

# **Sicherung ohne `git stash`.** `refs/stash` ist ueber alle Arbeitsbaeume geteilt; ein Werkzeug,
# das ihn benutzt, kann die Arbeit eines parallel laufenden Agenten mitnehmen.
DATEIEN=(
  "kernel/src/loader.rs"
  "kernel/src/system.rs"
  "crates/caprock-cap/src/space.rs"
  "crates/caprock-sched/src/lib.rs"
  "crates/caprock-hal/src/x86_64/mmu.rs"
  "crates/caprock-sched/src/redirect.rs"
  "crates/caprock-hal/src/x86_64/exception.rs"
  "kernel/src/arch/x86_64/bringup.rs"
)
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { # $1 = Logdatei
    timeout 900 ./test-qemu-x86.sh > "$1" 2>&1
}

# Ein Konjunkt aus der `dbg`/`dbgmem`-Zeile lesen. Gibt "true"/"false"/"" zurueck.
konjunkt() { grep -oE "$2=(true|false)" "$1" | head -1 | cut -d= -f2; }

echo "== Z6b: Gegenproben =="

# --- Positivkontrolle ---------------------------------------------------------------------------
#
# Ohne sie belegen die Mutationen nichts: ist der Ausgangszustand schon rot, ist er es mutiert
# auch, und jede Mutation saehe wie ein Beleg aus.
echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^dbg     : ALL PASS" "$TMP/orig.log" || ! grep -q "^dbgmem  : ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot -- die Gegenproben koennen nichts aussagen"
    grep -E "^dbg( |mem)" "$TMP/orig.log" | head -6
    exit 1
fi
echo "  PASS: dbg und dbgmem sind vorher gruen"

pruefe() { # $1 Name  $2 Log  $3 Konjunkt  $4 erwartet(false)
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- Konjunkt '$3' kommt im Protokoll gar nicht vor (Mutation hat den Lauf anders zerstoert als gemeint)"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' ist '$got', erwartet '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', wie gemeint"
    fi
}

mutiere() { # $1 Datei  $2 sed-Ausdruck  $3 Name
    local vorher nachher
    vorher="$(md5sum "$1" | cut -d' ' -f1)"
    sed -i "$2" "$1"
    nachher="$(md5sum "$1" | cut -d' ' -f1)"
    if [ "$vorher" = "$nachher" ]; then
        echo "  FAIL: $3 -- das sed-Muster hat NICHTS getroffen (lautlos abgeschalteter Negativfall)"
        fail=1; return 1
    fi
    return 0
}

# --- M1: die PD IST debuggbar, bevor gemessen wird ----------------------------------------------
#
# Die Gegenprobe zur tragenden Zusage. **Die erste Fassung mutierte `debuggable_praegen` im Lader
# und fiel durch** -- zu Recht: die Ziel-PD der Sonde entsteht ueber `create_pd()` im Hochlauf und
# geht nie durch den Lader, die Mutation konnte sie also gar nicht erreichen. Ein Negativfall, der
# den geprueften Pfad verfehlt, belegt nichts; er sah nur so aus.
#
# Gemeint ist: **existiert Autoritaet ueber diese PD, muss die Zeile fallen.** Also wird die
# Praegung VOR die Messung gezogen.
echo "-- M1: Autoritaet existiert schon bei der Messung --"
if python3 tools/_dbgneg_m1.py; then
    lauf "$TMP/m1.log"; pruefe M1 "$TMP/m1.log" "vorher-undebuggbar" false
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: die Freigabe faellt ganz weg -----------------------------------------------------------
# Ein Revoke, das sein Ziel unbrauchbar macht, ist schlimmer als kein Revoke.
echo "-- M2: keine Freigabe in der Finalisierung --"
if mutiere kernel/src/system.rs 's/^    release_finalized_debug(&rf);/    \/\/ M2: entfernt/' M2; then
    lauf "$TMP/m2.log"; pruefe M2 "$TMP/m2.log" "revoke-bricht-nicht" false
fi
restore

# --- M3: Freigabe NUR beim Revoke ---------------------------------------------------------------
# **Die wichtigste.** Eine sterbende Debugger-PD nimmt den `cap_delete`-Weg. Wer die Regel nur in
# `cap_revoke` unterbringt, hat den Fall gedeckt, an den beim Testen jeder zuerst denkt -- und den
# nicht, der im Betrieb vorkommt. Deshalb muessen hier ZWEI Dinge gelten: die Absturz-Zeile faellt,
# und die Revoke-Zeile bleibt gruen. Faellt beides, hat die Mutation etwas anderes zerstoert.
echo "-- M3: Freigabe nur beim Revoke (die Asymmetrie IST die Aussage) --"
if mutiere kernel/src/system.rs \
   's/^pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {/pub fn cap_delete(ptr: CapPtr) -> Result<(), CapError> {\n    let _m3 = ();/' M3
then
  # Die eigentliche Mutation: im `cap_delete`-Pfad die Debug-Liste NICHT abarbeiten.
  python3 - <<'PY'
import pathlib, re
p = pathlib.Path("kernel/src/system.rs"); s = p.read_text()
i = s.index("pub fn cap_delete(ptr: CapPtr)")
j = s.index("pub fn cap_revoke(ptr: CapPtr)")
teil = s[i:j].replace("    release_finalized_debug(&rf);\n", "", 1)
p.write_text(s[:i] + teil + s[j:])
PY
  lauf "$TMP/m3.log"
  pruefe M3a "$TMP/m3.log" "laeuft-wieder-nach-destroy_pd" false
  pruefe M3b "$TMP/m3.log" "revoke-bricht-nicht" true
fi
restore

# --- M4: der zweite Halter wird nicht abgewiesen -------------------------------------------------
echo "-- M4: zweiter Halter darf mitstoppen --"
if python3 tools/_dbgneg_m4.py; then
    lauf "$TMP/m4.log"; pruefe M4 "$TMP/m4.log" "zweiter-Halter-BUSY" false
else
    echo "  FAIL: M4 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M5: das Ringwort -- ZWEI Gatter, also zwei Mutationen ---------------------------------------
#
# `cs`/`ss` tragen den RING. Wer sie schreiben darf, befoerdert sein Ziel nach Ring 0.
#
# **Die erste Fassung dieser Gegenprobe schlug das Politik-Gatter aus und blieb gruen** -- und das
# war kein Skriptfehler, sondern der Befund: die Regel steht an zwei UNABHAENGIGEN Stellen.
# `redirect::writeback_erlaubt` sagt „diese Stufe darf nicht so weit", `hal::frame_wort_setzen`
# sagt „dieses Wort ist von ausserhalb des Kernels nie schreibbar". Das ist Tiefe, keine Kopie:
# die zweite Aussage haengt an der Architektur und nicht an einer Autoritaet und muss auch dann
# gelten, wenn jemand eine vierte Stufe einfuehrt und die Tabelle dort vergisst.
#
# Deshalb **M5a** (nur Politik weg -> muss GRUEN bleiben; das belegt die Tiefe) und **M5b**
# (beide weg -> muss fallen; das belegt, dass ueberhaupt etwas gattert). Ohne M5a saehe die Tiefe
# wie ein toter Zweig aus, ohne M5b waere sie unbelegt.
#
# **Und M5b hat die Sonde geschaerft, bevor es die Sonde bestaetigt hat.** Beim ersten Lauf griff
# die Mutation und die Zeile blieb trotzdem gruen -- weil es ein DRITTES Gatter gibt: ein Index,
# den `writeback_erlaubt` keiner schreibbaren Klasse zuordnet, wird per Vorgabe abgewiesen
# (`UeberDerStufe`). Die Sonde las `== Err(ERR_RIGHTS)` und konnte „abgewiesen weil Ringwort" von
# „abgewiesen weil unklassifiziert" nicht unterscheiden. Sie liest jetzt den GRUND
# (`DEBUG_WR_RING`), also die Groesse, die sich tatsaechlich aendert -- dieselbe Berichtigung wie
# bei der `park`-Zeile, die `is_parked` statt `blocked` las.
echo "-- M5a: nur das Politik-Gatter weg (Tiefe: muss GRUEN bleiben) --"
if mutiere crates/caprock-sched/src/redirect.rs 's/    if ix.ring.contains(&i) {/    if false {/' M5a; then
    lauf "$TMP/m5a.log"
    # **Beide Haelften, und sie zeigen in verschiedene Richtungen** -- das ist die Aussage:
    # der Schreibversuch scheitert weiterhin (Tiefe), aber nicht mehr AUS DIESEM GRUND (Schaerfe).
    pruefe M5a-Tiefe   "$TMP/m5a.log" "Ringwort-abgewiesen"  true
    pruefe M5a-Schaerfe "$TMP/m5a.log" "Ringwort-Grund-RING" false
fi
# **Zwischendurch zuruecksetzen, und das ist keine Formalie.** Ohne diese Zeile trug `redirect.rs`
# beim Start von M5b noch M5as Mutation, und M5bs Anker-Pruefung schlug fehl -- gemeldet als
# „Mutation nicht anwendbar", also genau so, wie sie soll. Ein Negativtest, dessen Mutationen sich
# gegenseitig ueberlagern, misst die Reihenfolge und nicht die Eigenschaft; dieselbe Lehre wie die
# erste D9-Gegenprobe, die zwei Dinge zugleich kaputtmachte.
restore

echo "-- M5b: BEIDE Gatter weg (muss fallen) --"
if python3 tools/_dbgneg_m5b.py; then
    lauf "$TMP/m5b.log"
    # **`Ringwort-abgewiesen` bleibt hier WAHR, und das ist der Befund** (gemessen 2026-08-20):
    # zwei ausgeschaltete Schichten reichen nicht. Die dritte ist die Vorgabe-Absage --
    # `writeback_erlaubt` ordnet Wort 18 keiner schreibbaren Klasse zu und weist es als
    # `UeberDerStufe` ab. Erwartet wird deshalb `true`, nicht `false`; alles andere waere ein
    # Skript, das seine eigene Erwartung ueber die Messung stellt.
    pruefe M5b-Tiefe   "$TMP/m5b.log" "Ringwort-abgewiesen"  true
    pruefe M5b-Schaerfe "$TMP/m5b.log" "Ringwort-Grund-RING" false
else
    echo "  FAIL: M5b -- Mutation nicht anwendbar"; fail=1
fi
restore

echo "-- M5c: ALLE DREI Schichten weg (erst jetzt darf der Ausgang fallen) --"
# **Ohne diese Mutation waere `Ringwort-abgewiesen` eine Zeile, von der niemand weiss, ob sie
# ueberhaupt fallen KANN** -- und genau das ist die Frage, die dieses Projekt vor jeder Aenderung an
# einem Pruefpfad stellt: „Kann dieser Test noch fehlschlagen, wenn die gepruefte Sache kaputt ist?"
#
# M5a und M5b belegen die Tiefe (eine bzw. zwei Schichten reichen nicht). M5c belegt, dass es
# ueberhaupt Schichten sind und nicht ein unerreichbarer Zweig.
if python3 tools/_dbgneg_m5c.py; then
    lauf "$TMP/m5c.log"
    pruefe M5c-Tiefe   "$TMP/m5c.log" "Ringwort-abgewiesen"  false
    pruefe M5c-Schaerfe "$TMP/m5c.log" "Ringwort-Grund-RING" false
else
    echo "  FAIL: M5c -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M6: `vspace_resolve` ohne Rechtepruefung ----------------------------------------------------
# Die Luecke, die die Sonde am 2026-08-20 gefunden hat: ohne `US` loest der Debugger auch die
# geteilten KERNEL-Eintraege der isolierten VSpace auf und liest Kernelspeicher.
echo "-- M6: vspace_resolve ohne US-Pruefung --"
if mutiere crates/caprock-hal/src/x86_64/mmu.rs 's/ || e \& US == 0//g; s/ || q \& US == 0//g' M6; then
    lauf "$TMP/m6.log"; pruefe M6 "$TMP/m6.log" "luecke-abgewiesen" false
fi
restore

echo
if [ "$fail" -eq 0 ]; then
    echo "== Z6b-GEGENPROBEN: ALL PASS =="
else
    echo "== Z6b-GEGENPROBEN: FAILURES =="
fi
exit "$fail"
