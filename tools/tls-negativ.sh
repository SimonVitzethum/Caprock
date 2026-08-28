#!/usr/bin/env bash
# **TLS: die rote Haelfte der `tls`-Zeile.** Gruene Konjunkte sind die eine Haelfte des Beweises;
# die andere ist, dass die Zeile ueberhaupt rot werden KANN -- und zwar an der Stelle, die man
# gemeint hat.
#
# Nach todo D18 ist eine Zeile ohne Gegenprobe nicht neutral, sondern die Vorstufe der fuenf
# dort aufgezaehlten Faelle: ein Pruefer, der nicht scheitern kann, wird mit jedem gruenen Lauf
# glaubwuerdiger.
#
# **Gefahren wird die HAUPTSUITE**, nicht die Lade-Suite: `tls` misst Kernelmechanik (Register,
# Wechsel, Syscall) und braucht kein Boot-Archiv. Der Treiberteil (`treiber-*`) ist dort
# `false` und gattert bedingt -- das ist Absicht und in der Zeile ausgeschrieben.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/tlsneg.XXXXXX")"
fail=0

# **Sicherung ohne `git stash`** -- `refs/stash` ist ueber alle Arbeitsbaeume geteilt.
DATEIEN=( "kernel/src/system.rs" "kernel/src/tlsprobe.rs" "crates/caprock-microkit/src/lib.rs" )
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 900 ./test-qemu-x86.sh > "$1" 2>&1; }
ZEILE="tls    :"
# **Abgelesen wird NUR auf der eigenen Berichtszeile** -- und das hat eine Runde gekostet.
#
# Die erste Fassung war `grep -oE "name=(true|false)" | head -1`, also dateiweit. `irtevgb`
# (der IRTE-Selbsttest) hat ein gleichnamiges Konjunkt `getrennt=true`, steht weiter oben, und der
# Extraktor nahm dessen Wert: M3 meldete `getrennt=true`, WAEHREND die Zeile korrekt
# `getrennt=false` sagte. Zwei Laeufe lang sah es aus wie ein Fehler in der Mutation.
#
# Das ist die Klasse aus todo D18 im Ableseweg statt im Pruefer: *„trifft genau einmal" schliesst
# nicht aus, dass der Treffer an der falschen Stelle sitzt.*
konjunkt() { grep -E "^${ZEILE}" "$1" | grep -oE "$2=(true|false)" | head -1 | cut -d= -f2; }

pruefe() { # $1 Name  $2 Log  $3 Konjunkt  $4 erwartet
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- Konjunkt '$3' kommt im Protokoll gar nicht vor (die Mutation hat den"
        echo "        Lauf anders zerstoert als gemeint -- z. B. den BAU, und dann misst der"
        echo "        Negativfall den Bau)"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' ist '$got', erwartet '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', wie gemeint"
    fi
}
gruen() { pruefe "$1" "$2" "$3" true; }

# **Die Trefferzahl gehoert zur Mutation** -- ein `sed` mit zwei Treffern sieht in der
# Ergebniszeile aus wie eines mit einem (M7 bei `ckptcut`).
mutiere() { # $1 Datei  $2 sed  $3 Name  $4 erwartete Treffer
    local vorher nachher treffer
    vorher="$(md5sum "$1" | cut -d' ' -f1)"
    treffer="$(sed -n "$2p" "$1" 2>/dev/null | wc -l)"
    sed -i "$2" "$1"
    nachher="$(md5sum "$1" | cut -d' ' -f1)"
    if [ "$vorher" = "$nachher" ]; then
        echo "  FAIL: $3 -- das sed-Muster hat NICHTS getroffen (lautlos abgeschalteter Negativfall)"
        fail=1; return 1
    fi
    if [ -n "${4:-}" ] && [ "$treffer" != "$4" ]; then
        echo "  FAIL: $3 -- das Muster traf $treffer Stelle(n), erwartet $4"; fail=1; return 1
    fi
    return 0
}

echo "== TLS: Gegenproben zur tls-Zeile (T1..T5) =="

# --- Positivkontrolle ---------------------------------------------------------------------------
echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^tls    : ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot -- die Gegenproben koennen nichts aussagen"
    grep -E "^tls" "$TMP/orig.log" | head -3
    exit 1
fi
echo "  PASS: tls ist vorher gruen"

# --- M1: der Zustand VOR T1 ---------------------------------------------------------------------
#
# `sync_tls` schreibt nichts. Auf x86 faultet das Kind dann an `fs:[0]` (FS_BASE ist 0, also
# lineare Adresse 0) -- das ist die richtige Folge und keine zweite Sache: ohne Register gibt es
# keinen Selbstzeiger zu lesen. Gemessen wird `tp-gesetzt`, und `spawn-maske` belegt, dass die
# Kinder ueberhaupt entstanden sind.
echo "-- M1: sync_tls schreibt das Register nicht (der Zustand vor T1) --"
if mutiere kernel/src/system.rs \
   's|^        hal::cpu::set_thread_pointer(want);$|        let _ = want;|' M1 1; then
    lauf "$TMP/m1.log"
    pruefe M1 "$TMP/m1.log" "tp-gesetzt" false
    if grep -q "spawn-maske=0x3" "$TMP/m1.log"; then
        echo "  PASS: M1 -- 'spawn-maske=0x3', die Kinder sind entstanden (Sprechprobe)"
    else
        echo "  FAIL: M1 -- erwartet 'spawn-maske=0x3'"; fail=1
    fi
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: gesetzt, aber nicht GEHALTEN -----------------------------------------------------------
#
# **Zwei `sed`, EINE semantische Aenderung**: das Register wird beim `SETTLS`-Syscall geschrieben
# und beim Wechsel nicht mehr gespiegelt. Getrennt waere keines der beiden eine sinnvolle Lage --
# ohne das erste gaebe es gar kein Setzen, ohne das zweite kein Nicht-Halten.
#
# Genau diese Mutation war der ECHTE Fehler beim Bau von T1: `sync_tls` landete an einer von vier
# Spiegelstellen, die Kinder setzten korrekt und verloren den Wert beim ersten `YIELD`. `sofort`
# blieb dabei gruen -- ohne `ueberlebt-wechsel` waere die Zeile durchgegangen.
echo "-- M2: gesetzt im Syscall, nicht gespiegelt beim Wechsel --"
if mutiere kernel/src/system.rs \
   's|^    sync_tls(core, sched);$|    let _ = sync_tls;|' M2a 1 \
   && mutiere kernel/src/system.rs \
      's|^        with_owner(tid, \|s, _\| s.set_tls(tid, va).then_some(())).is_some()$|        { hal::cpu::set_thread_pointer(va as u64); with_owner(tid, \|s, _\| s.set_tls(tid, va).then_some(())).is_some() }|' M2b 1; then
    lauf "$TMP/m2.log"
    pruefe M2 "$TMP/m2.log" "ueberlebt-wechsel" false
    # **Die Trennschaerfe:** unmittelbar nach dem Syscall stimmt das Register noch.
    gruen M2 "$TMP/m2.log" "tp-gesetzt"
else
    echo "  FAIL: M2 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M3: beide Kinder bekommen denselben Puffer -------------------------------------------------
#
# **Ein Fehler im PROGRAMM, nicht im Kernel** -- und ohne diese Gegenprobe waere `getrennt` auch in
# einem Baum wahr, in dem der Kernel stimmt und der Aufrufer nicht. Dieselbe Bewegung wie M8 bei
# `irqmsi`: die Praemisse pruefen, nicht den Schluss (todo D18).
echo "-- M3: beide Kinder teilen ein Fenster --"
if mutiere kernel/src/tlsprobe.rs \
   's|^        let basis = arena + off \* PAGE;$|        let basis = arena;|' M3 1; then
    lauf "$TMP/m3.log"
    pruefe M3 "$TMP/m3.log" "getrennt" false
    if grep -q "spawn-maske=0x3" "$TMP/m3.log"; then
        echo "  PASS: M3 -- 'spawn-maske=0x3', beide Kinder entstanden trotzdem"
    else
        echo "  FAIL: M3 -- erwartet 'spawn-maske=0x3'"; fail=1
    fi
else
    echo "  FAIL: M3 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M4: die Schranke faellt --------------------------------------------------------------------
#
# `SETTLS` wirkt **ohne Cap**; was ihn traegt, ist allein `USER_VA_TOP`. Ohne diese Gegenprobe
# waere die Schranke eine Behauptung -- und sie schuetzt nicht den Aufrufer, sondern den KERNEL
# (ein `WRMSR` mit nicht-kanonischem Wert faultet in Ring 0).
#
# Der Probewert der Sonde ist **kanonisch** und liegt nur ausserhalb des Benutzerbereichs: die
# Mutation laesst ihn also durch, ohne die Maschine umzulegen. Eine Gegenprobe, die abstuerzt
# statt ein Konjunkt fallen zu lassen, waere keine Messung.
echo "-- M4: SETTLS prueft die Adresse nicht mehr --"
if mutiere crates/caprock-microkit/src/lib.rs \
   's|^        if va >= caprock_abi::USER_VA_TOP {$|        if false {|' M4 1; then
    lauf "$TMP/m4.log"
    pruefe M4 "$TMP/m4.log" "schranke-beisst" false
    gruen M4 "$TMP/m4.log" "tp-gesetzt"
    gruen M4 "$TMP/m4.log" "getrennt"
else
    echo "  FAIL: M4 -- Mutation nicht anwendbar"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then
    echo "== TLS-GEGENPROBEN: ALL PASS =="
else
    echo "== TLS-GEGENPROBEN: FAILURES =="
fi
exit "$fail"
