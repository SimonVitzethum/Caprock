#!/usr/bin/env bash
# **Z23/S3: die rote Haelfte des Gruppenschnitts.** Gruene Konjunkte sind die eine Haelfte des
# Beweises; die andere ist, dass die Zeile ueberhaupt rot werden KANN — und zwar an der Stelle, die
# man gemeint hat.
#
# Geprueft wird dreierlei, und der zweite Punkt ist der, um den es geht:
#   1. die Mutation hat GEGRIFFEN (ein sed-Muster, das nach einer Umbenennung nicht mehr passt,
#      waere ein lautlos abgeschalteter Negativfall),
#   2. das GEMEINTE Konjunkt ist gefallen — und nicht irgendeines,
#   3. wo es die Aussage traegt: die uebrigen sind gruen geblieben. Eine Mutation, die zwei Dinge
#      zugleich kaputtmacht, beweist nichts ueber das gemeinte (erste D9-Gegenprobe).
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pdfneg.XXXXXX")"
fail=0

# **Sicherung ohne `git stash`.** `refs/stash` ist ueber alle Arbeitsbaeume geteilt; ein Werkzeug,
# das ihn benutzt, kann die Arbeit eines parallel laufenden Agenten mitnehmen.
DATEIEN=( "kernel/src/system.rs" "crates/caprock-sched/src/lib.rs" )
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 900 ./test-qemu-x86.sh > "$1" 2>&1; }
konjunkt() { grep -oE "$2=(true|false)" "$1" | head -1 | cut -d= -f2; }

pruefe() { # $1 Name  $2 Log  $3 Konjunkt  $4 erwartet
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- Konjunkt '$3' kommt im Protokoll gar nicht vor (die Mutation hat den"
        echo "        Lauf anders zerstoert als gemeint)"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' ist '$got', erwartet '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', wie gemeint"
    fi
}

# Ein Konjunkt, das GRUEN geblieben sein muss — die Isolationsaussage.
gruen() { pruefe "$1" "$2" "$3" true; }

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

echo "== Z23/S3: Gegenproben zum Gruppenschnitt =="

# --- Positivkontrolle ---------------------------------------------------------------------------
# Ohne sie belegen die Mutationen nichts: ist der Ausgangszustand schon rot, ist er es mutiert
# auch, und jede Mutation saehe wie ein Beleg aus.
echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^pdfreeze: ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot -- die Gegenproben koennen nichts aussagen"
    grep -E "^pdfreeze" "$TMP/orig.log" | head -3
    exit 1
fi
echo "  PASS: pdfreeze ist vorher gruen"

# --- M1: der Schnitt nimmt PAUSE statt eines EIGENEN Grundes ------------------------------------
#
# Die naheliegende Fassung, und ihre Doku nennt den Gruppenschnitt sogar als kuenftigen Nutzer.
# Sie traegt nicht: mit `PAUSE` genuegt EIN `RESUME` auf EINEN Teilnehmer, um ihn aus der Gruppe
# herauszuloesen -- die PD waere halb eingefroren, und **jeder Pruefer meldete Ordnung**.
# Das ist die D11-Form, und `resume-wirkt-nicht` ist die einzige Zeile, die sie misst.
echo "-- M1: der Gruppengrund ist PAUSE (ein RESUME loest einen Teilnehmer heraus) --"
if mutiere crates/caprock-sched/src/lib.rs \
   '/pub fn freeze_group/,/^    }$/ s/BlockReasons::FREEZE/BlockReasons::PAUSE/g' M1; then
    lauf "$TMP/m1.log"
    pruefe M1 "$TMP/m1.log" "resume-wirkt-nicht" false
    gruen M1 "$TMP/m1.log" "steht"
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: eine INTERNE Beziehung gilt als extern -------------------------------------------------
#
# Die Kernaussage des Strangs. Ohne sie ist ein Prozess-Freeze strukturell unmoeglich, und die
# Zeile muss genau daran fallen: `schnitt-code=9` ist [`PdFreeze::Deadline`].
echo "-- M2: interne Beziehung zaehlt nicht als intern --"
if mutiere kernel/src/system.rs \
   's/^                Some(p) if pd_of_thread(p) == Some(pd) => continue,/                Some(_p) if false => continue,/' M2; then
    lauf "$TMP/m2.log"
    if grep -q "^pdfreeze: FAILURES" "$TMP/m2.log" && grep -q "schnitt-code=9" "$TMP/m2.log"; then
        echo "  PASS: M2 -- der Schnitt scheitert mit 'schnitt-code=9' (Frist), wie gemeint"
    else
        echo "  FAIL: M2 -- erwartet 'pdfreeze: FAILURES' mit 'schnitt-code=9'"
        grep -E "^pdfreeze" "$TMP/m2.log" | head -2; fail=1
    fi
    # Die Isolationsaussage: die EINZEL-Absage stand vorher schon und steht weiter.
    gruen M2 "$TMP/m2.log" "einzeln-unfrierbar"
else
    echo "  FAIL: M2 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M3: kein Tor nach innen (S1b) --------------------------------------------------------------
# Ohne den Riegel bekaeme ein fremder Aufrufer waehrend des Freeze unbegrenztes Blockieren statt
# eines Codes, der „kommt gleich wieder" heisst -- der Freeze reichte den Deadlock an Unbeteiligte
# weiter.
echo "-- M3: der Kanal wird nicht nach innen zugesperrt --"
if mutiere kernel/src/system.rs \
   's/^                ep_gesperrt\[i\] = e.begin_quiesce();/                ep_gesperrt[i] = false;/' M3; then
    lauf "$TMP/m3.log"
    pruefe M3 "$TMP/m3.log" "kanal-zu" false
    # **Die Asymmetrie IST die Aussage:** das Zurueckziehen ist eine ANDERE Zusage und bleibt gruen.
    gruen M3 "$TMP/m3.log" "empfaenger-gezogen"
else
    echo "  FAIL: M3 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M4: der Empfaenger wird nicht zurueckgezogen ------------------------------------------------
# Bliebe er in der Schlange, koennte ein Rendezvous waehrend des Schnitts eine NEUE offene
# Transaktion erzeugen -- die Menge der offenen Transaktionen wuechse, waehrend die PD steht.
echo "-- M4: der Empfaenger bleibt in der Schlange --"
if mutiere kernel/src/system.rs \
   's/^                eps()\[recv_ep\[i\] as usize\].lock().retire_receiver(tids\[i\]);/                let _ = recv_ep[i];/' M4; then
    lauf "$TMP/m4.log"
    pruefe M4 "$TMP/m4.log" "empfaenger-gezogen" false
    gruen M4 "$TMP/m4.log" "kanal-zu"
else
    echo "  FAIL: M4 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M5: die Frist nennt den Partner nicht (S2) --------------------------------------------------
# „Nicht einfrierbar" ist als Diagnose wertlos; ohne den Namen ist eine dauerhafte Absage von einem
# Haenger nicht zu unterscheiden.
echo "-- M5: die Absage nennt keinen Partner --"
if mutiere kernel/src/system.rs \
   '/^fn thread_partner(tid: ThreadId) -> Option<ThreadId> {/,/^}$/ s/^    for i in 0..eps().len() {/    for i in 0..0 {/' M5; then
    lauf "$TMP/m5.log"
    pruefe M5 "$TMP/m5.log" "frist-nennt-partner" false
    # Der Schnitt selbst muss weiter gelingen -- interne Beziehungen brauchen den Partner nicht,
    # solange `pd_of_thread` ihn nicht liefert... **und genau das ist hier NICHT der Fall**: ohne
    # Partner ist eine interne Beziehung nicht mehr als intern erkennbar. Die Zeile faellt also an
    # ZWEI Stellen, und das wird hier ausgesprochen statt verschwiegen.
    if grep -q "schnitt-code=9" "$TMP/m5.log"; then
        echo "  HINWEIS: M5 reisst zusaetzlich den Schnitt (schnitt-code=9) -- die Partnerauskunft"
        echo "           traegt BEIDE Aussagen; die Mutation isoliert hier nicht, und das steht so"
        echo "           im Skript, statt als Beleg fuer die gemeinte Aussage durchzugehen."
    fi
else
    echo "  FAIL: M5 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M6: das Auftauen reiht den Empfaenger nicht wieder ein (S6) ----------------------------------
# „Auftauen ist nicht die Umkehrung" -- der Thread bliebe IPC-blockiert an einem Kanal, an dem ihn
# niemand mehr findet.
echo "-- M6: das Auftauen laesst den Empfaenger draussen --"
if mutiere kernel/src/system.rs \
   's/^            eps()\[z.recv_ep\[i\] as usize\].lock().bind_receiver(z.tids\[i\]);/            let _ = z.recv_ep[i];/' M6; then
    lauf "$TMP/m6.log"
    pruefe M6 "$TMP/m6.log" "lauscher-zurueck" false
    gruen M6 "$TMP/m6.log" "laeuft-danach"
else
    echo "  FAIL: M6 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M7: das Auftauen entfernt ALLE Gruende ------------------------------------------------------
# Der D9-Fehler woertlich, von der anderen Seite: ein Wecker, der eine fremde Entscheidung mitnimmt.
# Der Aufrufer haengt in IPC; nach dem Thaw duerfte er NICHT laufen.
echo "-- M7: thaw_group entfernt jeden Grund --"
if mutiere crates/caprock-sched/src/lib.rs \
   '/pub fn thaw_group/,/^    }$/ s/self.tcbs\[s\].reasons.remove(BlockReasons::FREEZE);/self.tcbs[s].reasons = BlockReasons::NONE;/' M7; then
    lauf "$TMP/m7.log"
    pruefe M7 "$TMP/m7.log" "klient-laeuft-nicht" false
    gruen M7 "$TMP/m7.log" "laeuft-danach"
else
    echo "  FAIL: M7 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M8: die DMA-Absage faellt weg (S5) ----------------------------------------------------------
# Eine eingefrorene Treiber-PD, deren Geraet gerade in ihre Region schreibt, ist NICHT eingefroren --
# der Deskriptorring laeuft weiter. Bis der Domaenen-Schwenk gebaut ist, ist die Absage die einzige
# Zusage, die haelt.
echo "-- M8: eine PD mit DMA-Cap wird trotzdem eingefroren --"
if mutiere kernel/src/system.rs \
   's/^    if pd_haelt_dma(pd) {/    if false \&\& pd_haelt_dma(pd) {/' M8; then
    lauf "$TMP/m8.log"
    pruefe M8 "$TMP/m8.log" "dma-abgewiesen" false
    # **Die Positivkontrolle bleibt gruen** -- dieselbe PD ohne Cap ging vorher durch und geht
    # weiter durch. Faellt sie mit, hat die Mutation etwas anderes zerstoert als die Absage.
    gruen M8 "$TMP/m8.log" "dma-ohne-cap-geht"
else
    echo "  FAIL: M8 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M9: die Weckmarke ueberlebt den Schnitt nicht (S6) -------------------------------------------
# Ein Thread, der WAEHREND des Schnitts geweckt wurde, muss nach dem Thaw sofort laufen. Geht die
# Marke unterwegs verloren, schlaeft er weiter -- ein verlorenes Wecken, exakt das, wogegen Z22 P4
# gebaut ist, nur ueber den Freeze hinweg.
echo "-- M9: das unpark waehrend des Schnitts verpufft --"
if mutiere kernel/src/system.rs \
   '/^pub fn unpark_thread(tid: ThreadId) -> bool {/,/^}$/ s/^    if let Some((_, c)) = with_owner(tid, |s, _| s.unpark(tid).then_some(())) {/    if let Some((_, c)) = None::<((), usize)> {/' M9; then
    lauf "$TMP/m9.log"
    pruefe M9 "$TMP/m9.log" "park-marke-ueberlebt" false
    # **Die andere Richtung bleibt gruen:** ohne Marke schlaeft er ohnehin -- das aendert die
    # Mutation nicht, und genau daran ist zu sehen, dass sie die MARKE trifft und nicht den Schlaf.
    gruen M9 "$TMP/m9.log" "park-ohne-marke-bleibt"
    gruen M9 "$TMP/m9.log" "park-stumm-im-schnitt"
else
    echo "  FAIL: M9 -- Mutation nicht anwendbar"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then echo "== Z23/S3-GEGENPROBEN: ALL PASS =="; else echo "== Z23/S3-GEGENPROBEN: FAILURES =="; fi
exit "$fail"
