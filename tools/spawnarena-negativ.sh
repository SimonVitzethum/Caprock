#!/usr/bin/env bash
# **K1b: the red half of "several stacks out of one Cap".** Green conjuncts are one half of the
# proof; the other is that the line can go red AT ALL — and at the place one meant.
#
# Three things are checked per mutation, and the second is the point:
#   1. the mutation BIT (a sed pattern that no longer matches after a rename would be a silently
#      disabled counter-proof) — **and how often it bit**: a sed with two hits looks in the result
#      line exactly like one with a single hit, and it may blind the measurement and its checker at
#      once (the M7 lesson from `ckptcut-negativ.sh`),
#   2. the INTENDED conjunct fell — not some other one,
#   3. what the statement rests on: the conjuncts that had to stay green did.
#
# **Where a mutation makes MORE than one conjunct fall, that is written down here in advance.**
# Some of these changes have a structural second consequence (removing the offset does not only
# collide the windows, it makes the sibling check refuse the later spawns), and a counter-proof
# that discovered its own fallout afterwards would be a counter-proof fitted to its result.
#
# The most important mutation is M1: it removes `stack_sibling_overlaps`, and that IS the tree as
# it stood before 2026-08-26. `pd_mapping_overlaps` reads `KSTACKS.ubase_of`, and
# `spawn_with_stack_parked` deliberately writes nothing there — so the `Overlaps` refusal was
# **structurally unreachable** for exactly the threads `SYS_SPAWN` creates. It never fired, and
# nothing said so.
if [ -z "${BASH_VERSION:-}" ]; then echo "ERROR: needs bash, not sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/arenaneg.XXXXXX")"
fail=0

# **Backup without `git stash`.** `refs/stash` is shared across all worktrees; a tool that uses it
# can carry off the work of an agent running in parallel.
DATEIEN=( "crates/caprock-cap/src/spawncheck.rs" "kernel/src/system.rs" "kernel/src/spawnarena.rs" )
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 900 ./test-qemu-x86.sh > "$1" 2>&1; }

# **Aus der `arena`-ZEILE lesen, nicht aus dem Protokoll.** `threads=` und `slots=` stehen auch in
# anderen Zeilen; ein ungeankerter Greifer beantwortete die Frage eines anderen Tests.
zeile() { grep -m1 '^arena  :' "$1"; }
konjunkt() { zeile "$1" | grep -oE "(^| )$2=(true|false|[0-9]+)" | head -1 | cut -d= -f2; }

pruefe() { # $1 name  $2 log  $3 conjunct  $4 expected
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- Konjunkt '$3' kommt in der arena-Zeile gar nicht vor (die Mutation hat"
        echo "        den Lauf anders zerstoert als gemeint -- oder es gibt keine Zeile)"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' ist '$got', erwartet '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', wie beabsichtigt"
    fi
}
gruen() { pruefe "$1" "$2" "$3" true; }

# **`mutiere` zaehlt die GEAENDERTEN ZEILEN, nicht nur ob sich etwas geaendert hat.**
#
# „Hat das Muster getroffen?" ist die halbe Frage; die andere ist „wie oft?". Ein `sed` mit zwei
# Treffern sieht in der Ergebniszeile aus wie eines mit einem — und kann die Messung UND ihren
# Pruefer zugleich blenden (die M7-Lehre aus `ckptcut-negativ.sh`). Gezaehlt wird gegen die
# Sicherungskopie, also die WIRKUNG der Mutation und nicht die Zahl der Musterstellen; ein
# bereichsgebundenes `sed` laesst sich mit `grep -c` gar nicht ehrlich zaehlen.
mutiere() { # $1 file  $2 sed  $3 name  $4 erwartete Zahl geaenderter Zeilen (Vorgabe 1)
    local erwartet="${4:-1}" geaendert
    sed -i "$2" "$1"
    geaendert="$(diff "$TMP/$1" "$1" | grep -c '^>')"
    if [ "$geaendert" = 0 ]; then
        echo "  FAIL: $3 -- das sed-Muster traf NICHTS (stillgelegte Gegenprobe)"; fail=1; return 1
    fi
    if [ "$geaendert" != "$erwartet" ]; then
        echo "  FAIL: $3 -- ${geaendert} Zeilen geaendert, erwartet ${erwartet}. Eine Mutation, die"
        echo "        mehr trifft als gemeint, macht zwei Dinge zugleich kaputt und beweist ueber"
        echo "        keines etwas."
        fail=1; return 1
    fi
    return 0
}

echo "== K1b: Gegenproben zu mehreren Stapeln aus EINER Cap =="

# --- Positivkontrolle ---------------------------------------------------------------------------
# Ohne sie beweisen die Mutationen nichts: ist der Ausgangszustand schon rot, ist er es mutiert
# auch, und jede Mutation saehe wie ein Beleg aus.
echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^arena  : ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot -- die Gegenproben koennen nichts sagen"
    zeile "$TMP/orig.log" | head -3
    exit 1
fi
echo "  PASS: arena ist vorher gruen"

# --- M1: die Geschwisterpruefung gibt es nicht --------------------------------------------------
# **Das ist der Baum von gestern.** Und genau deshalb ist es die wichtigste Mutation der Datei:
# ohne sie waere „ueberlappung-abgewiesen=true" eine Zeile, von der niemand weiss, ob sie fallen
# kann -- der Zustand, in dem sie ein Jahr lang war.
echo "-- M1: stack_sibling_overlaps sieht nie einen Nachbarstapel --"
# Bereichsgebunden auf `stack_sibling_overlaps` -- `let t = STACK_CAP_OF.lock();` steht auch in
# `stack_cap_in_use` (das ist M5), und eine Mutation, die beide traefe, schaltete die
# Ueberlappungspruefung UND den ERR_INUSE-Schutz zugleich ab.
if mutiere kernel/src/system.rs \
   '/^fn stack_sibling_overlaps/,/^}$/ s/^    let t = STACK_CAP_OF.lock();$/    return false;\n    #[allow(unreachable_code)] let t = STACK_CAP_OF.lock();/' M1 2; then
    lauf "$TMP/m1.log"
    pruefe M1 "$TMP/m1.log" "ueberlappung-abgewiesen" false
    # Die vier Fenster sind wirklich disjunkt -- an ihnen aendert die blinde Pruefung nichts.
    # Faellt hier etwas mit, hat die Mutation mehr abgeschaltet als die eine Frage.
    gruen M1 "$TMP/m1.log" "disjunkt"
    gruen M1 "$TMP/m1.log" "vier-fenster"
    gruen M1 "$TMP/m1.log" "ausserhalb-abgewiesen"
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: die Obergrenze der Cap wird nicht geprueft ----------------------------------------------
# Ein Fenster, das ueber das Ende hinausragt, wird zum Stapel. Der Aufrufer bekaeme Speicher, den
# er nicht haelt -- und mit `offset` aus einem USER-Register ist das die Angriffsform, nicht der
# Tippfehler.
echo "-- M2: sub_region prueft die Obergrenze nicht --"
if mutiere crates/caprock-cap/src/spawncheck.rs \
   's/^    if end > cap_end {$/    if false {/' M2; then
    lauf "$TMP/m2.log"
    pruefe M2 "$TMP/m2.log" "ausserhalb-abgewiesen" false
    gruen M2 "$TMP/m2.log" "vier-fenster"
    gruen M2 "$TMP/m2.log" "disjunkt"
    gruen M2 "$TMP/m2.log" "ueberlappung-abgewiesen"
else
    echo "  FAIL: M2 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M3: die Sprechprobe des KOLLISIONSMELDERS ---------------------------------------------------
# Diese Mutation trifft die SONDE, nicht den Kernel, und das ist Absicht: sie beantwortet die
# Frage „wuerde `disjunkt` eine Kollision ueberhaupt bemerken?". Alle vier Kinder bekommen dieselbe
# Zieladresse; die Fenster bleiben korrekt getrennt, die Spawns gelingen unveraendert.
#
# Ohne sie waere `disjunkt=true` moeglicherweise ein Praedikat, das gar nicht fallen kann -- und
# ein Praedikat, das nicht durchfallen kann, ist so wenig eine Pruefung wie eines, das nicht
# bestehen kann.
echo "-- M3: alle Kinder schreiben an DIESELBE Adresse (Sprechprobe des Melders) --"
if mutiere kernel/src/spawnarena.rs \
   's/^        let basis = arena + off \* PAGE;$/        let basis = arena;/' M3; then
    lauf "$TMP/m3.log"
    pruefe M3 "$TMP/m3.log" "disjunkt" false
    # Die Spawns selbst sind unveraendert: dieselben Fenster, dieselben Caps, dieselben Threads.
    gruen M3 "$TMP/m3.log" "vier-fenster"
    pruefe M3 "$TMP/m3.log" "threads" 6
    pruefe M3 "$TMP/m3.log" "slots" 2
    gruen M3 "$TMP/m3.log" "ganze-region"
else
    echo "  FAIL: M3 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M4: der Versatz wird ignoriert ---------------------------------------------------------------
# Die ehrliche Fassung von „was, wenn der Kernel das Fenster gar nicht beachtet".
#
# **Die Folge ist zweiteilig, und sie steht hier VOR dem Lauf:** alle vier Fenster liegen dann an
# der Cap-Basis, also gelingt der erste Spawn und die drei folgenden werden von der (intakten)
# Geschwisterpruefung abgewiesen. Es faellt deshalb `vier-fenster` UND `disjunkt` UND `lebendig`.
# Die Isolation liegt woanders: beide Absagen und der `x1 == 0`-Weg bleiben gruen -- die Mechanik
# ist nicht kaputt, sie ist wirkungslos.
echo "-- M4: sub_region legt jedes Fenster an die Cap-Basis --"
if mutiere crates/caprock-cap/src/spawncheck.rs \
   's/^    let base = cap_base.checked_add(off).ok_or(StackRefusal::OutsideCap)?;$/    let base = cap_base; let _ = off;/' M4; then
    lauf "$TMP/m4.log"
    pruefe M4 "$TMP/m4.log" "vier-fenster" false
    pruefe M4 "$TMP/m4.log" "disjunkt" false
    gruen M4 "$TMP/m4.log" "ganze-region"
    gruen M4 "$TMP/m4.log" "ueberlappung-abgewiesen"
else
    echo "  FAIL: M4 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M5: die Stack-Cap laesst sich loeschen ------------------------------------------------------
# Die K1a-Zusage, jetzt unter einer Cap, die VIER Stapel traegt. Ihre Finalisierung gibt die Region
# an den Allokator zurueck -- unter vier laufenden Stapeln.
#
# **Zweite Folge, ebenfalls vorher notiert:** gelingt das Loeschen, sinkt die Slotzahl der PD von
# 2 auf 1, also faellt `slots` mit. Isolation ist `disjunkt`: die Fenster selbst bleiben getrennt.
echo "-- M5: stack_cap_in_use findet die Cap nicht mehr --"
if mutiere kernel/src/system.rs \
   '/^fn stack_cap_in_use/,/^}$/ s/^    let t = STACK_CAP_OF.lock();$/    return false;\n    #[allow(unreachable_code)] let t = STACK_CAP_OF.lock();/' M5 2; then
    lauf "$TMP/m5.log"
    pruefe M5 "$TMP/m5.log" "cap-gesperrt" false
    pruefe M5 "$TMP/m5.log" "slots" 1
    gruen M5 "$TMP/m5.log" "disjunkt"
    gruen M5 "$TMP/m5.log" "vier-fenster"
else
    echo "  FAIL: M5 -- Mutation nicht anwendbar"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then
    echo "== SPAWNARENA-NEGATIV: ALL PASS =="
else
    echo "== SPAWNARENA-NEGATIV: FAILURES =="
fi
exit "$fail"
