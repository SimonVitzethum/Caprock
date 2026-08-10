#!/usr/bin/env bash
# **Die Abnahme faehrt die RAM-Reihe -- oder sie ist keine Abnahme.** (2026-08-10.)
#
# ================================================================================================
# WARUM ES DAS GIBT
# ================================================================================================
#
# Am 2026-08-10 fiel `grossdma : FAILURES` bei `-m 3G` durch, weil die Abnahme eines Merges nur
# 512M gefahren hatte. Der Fehler war **nicht Unwissen**: dass die Lade-Suite bei 512M · 3G · 6G
# gruen sein MUSS, steht seit dem 2026-08-04 als gemessener Stand in `CLAUDE.md`. Eine Reihe, die
# bekannt ist und trotzdem nicht gefahren wird, ist ein fehlender Mechanismus und keine
# Sorgfaltsfrage -- dieselbe Einordnung wie beim `local_irq_disable()`, das an EINER von 53 Stellen
# per Hand stand.
#
# Deshalb ist die Reihe hier ein Werkzeug und nicht ein Merkzettel. Ein Merkzettel wird gelesen,
# wenn man ohnehin schon daran denkt.
#
# **Warum ausgerechnet diese RAM-Groessen** (jede steht fuer einen Zweig, den die anderen nicht
# nehmen):
#   512M  -- kein Speicher oberhalb 4 GiB; `Zone::Anywhere` weicht ueberall aus.
#   2560M -- unterer Bereich knapp, oberer noch leer.
#   3G    -- oben 1024 MiB gegen unten 2032 MiB: die Groessenrelation KEHRT SICH UM, und genau
#            daran fiel E-Rest 3b auf ("unten zuerst" war ein Zufall der Groessenrelation).
#   4G    -- die Kante selbst.
#   6G    -- oben groesser als unten, der bequeme Fall.
# Die Lade-Suite faehrt 512M · 3G · 6G, weil nur sie einen Ladepfad hat -- und der ist die Haelfte,
# die die Hauptsuite strukturell nicht prueft (gemessen: `system::alloc` auf `KernelOnly` gestellt
# reisst die Lade-Suite bei 3G, waehrend die Hauptsuite gruen bleibt).
#
# **Der Schluessel des Registers ist der Exit-Code, nicht der Text.** Erfolg ueber einen
# Textvergleich der Schlusszeile heisst: mit jeder neuen Suite waechst eine Formel, und jede Formel
# ist ein Loch (die gruenen Waechter schliessen anders als die QEMU-Suiten). Gibt eine Suite bei
# Fehlschlag 0 zurueck, ist DAS der Fehler -- einer in der Suite.
#
# Aufruf:
#   tools/abnahme.sh            # die volle Reihe
#   tools/abnahme.sh --schnell  # nur 512M je Suite + die Waechter (fuer den Zwischenstand)
#
# Rueckgabe: 0 nur, wenn JEDER Punkt der Reihe 0 zurueckgegeben hat.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

SCHNELL=0
[ "${1:-}" = "--schnell" ] && SCHNELL=1

# Sekunden je QEMU-Lauf. Die Suiten deckeln sich selbst; der Wert ist die Obergrenze.
SEK="${ABNAHME_SEK:-120}"

# Die Reihe. Ein Eintrag ist `Name|Befehl...`.
PUNKTE=()
PUNKTE+=("kernel-grenze|./tools/kernel-grenze.sh")
PUNKTE+=("host-tests|./tools/host-tests.sh")
PUNKTE+=("mangel-stellen|./tools/mangel-stellen.sh")
if [ "$SCHNELL" = 1 ]; then
    PUNKTE+=("haupt-512M|./test-qemu-x86.sh $SEK 512M")
    PUNKTE+=("lade-512M|./test-qemu-x86-load.sh $SEK 512M")
else
    for r in 512M 2560M 3G 4G 6G; do
        PUNKTE+=("haupt-$r|./test-qemu-x86.sh $SEK $r")
    done
    for r in 512M 3G 6G; do
        PUNKTE+=("lade-$r|./test-qemu-x86-load.sh $SEK $r")
    done
fi

echo "== Abnahme: ${#PUNKTE[@]} Punkte =="
[ "$SCHNELL" = 1 ] && echo "   (--schnell: die RAM-Reihe ist NICHT gefahren -- das ist ein Zwischenstand, keine Abnahme)"
echo

NAMEN=(); CODES=(); ZEITEN=(); SCHLUSS=()
GESAMT0=$SECONDS
for eintrag in "${PUNKTE[@]}"; do
    name="${eintrag%%|*}"
    befehl="${eintrag#*|}"
    printf '%-16s ... ' "$name"
    t0=$SECONDS
    # Ueber `sammellauf.sh`, damit ein Fehlschlag sein VOLLSTAENDIGES Protokoll behaelt. Wer nur
    # die letzte Zeile festhaelt, verliert genau die Ausfaelle, die selten sind.
    #
    # **Die Schlusszeile wird MITGENOMMEN, nicht weggeworfen.** Die erste Fassung schickte die
    # Ausgabe nach /dev/null: bei Erfolg loescht `sammellauf.sh` sein Protokoll, und danach war
    # von einem gruenen Lauf nichts mehr da als ein Exit-Code. Eine Abnahme, die ihre eigenen
    # Schlusszeilen nicht vorzeigen kann, ist eine Behauptung. Geurteilt wird trotzdem allein nach
    # `$?` -- der Text wird gegengelesen, nicht befragt.
    # **Erst den Exit-Code sichern, dann den Text ansehen** -- und die beiden nicht in EINER
    # Pipeline. `zeile="$(cmd | head -1)"` sieht richtig aus und ist es nicht: die Pipeline laeuft
    # in der Kommandosubstitution, `PIPESTATUS` der aeusseren Shell wird davon nicht gesetzt, und
    # `rc` waere der von `head` -- also **immer 0**. Das ist „Schweigen als Erfolg" im eigenen
    # Werkzeug, genau die Form, gegen die `sammellauf.sh` Festlegung (A) getroffen hat.
    ausgabe="$(./tools/sammellauf.sh "abnahme-$name" bash -c "$befehl" 2>/dev/null)"
    rc=$?
    # `head -1` und nicht `tail -1`: `sammellauf.sh` druckt die Schlusszeile ZUERST und danach
    # seine eigene Notiz (Festlegung (B): auch bei Erfolg behalten, mit Rotation). Ein `tail -1`
    # holt den Ablagepfad statt des Urteils -- selbst gebaut und im ersten Lauf gesehen.
    # Herestring statt Pipe: `echo "$X" | head` bricht unter `pipefail` an SIGPIPE, sobald die
    # Ausgabe den Pipe-Puffer ueberschreitet (66..70 KiB, in diesem Projekt schon bezahlt).
    zeile="$(head -1 <<<"$ausgabe")"
    t=$((SECONDS - t0))
    NAMEN+=("$name"); CODES+=("$rc"); ZEITEN+=("$t"); SCHLUSS+=("$zeile")
    if [ "$rc" -eq 0 ]; then printf 'OK   (%3ds)\n' "$t"; else printf 'ROT  (%3ds, rc=%d)\n' "$t" "$rc"; fi
done
GESAMT=$((SECONDS - GESAMT0))

echo
echo "== Schlusszeilen (gegengelesen, nicht befragt) =="
for i in "${!NAMEN[@]}"; do
    printf '  %-16s %s\n' "${NAMEN[$i]}" "${SCHLUSS[$i]:-<keine Ausgabe>}"
done

echo
echo "== Bilanz =="
rot=0
for i in "${!NAMEN[@]}"; do
    if [ "${CODES[$i]}" -ne 0 ]; then
        rot=$((rot + 1))
        echo "  ROT: ${NAMEN[$i]} (rc=${CODES[$i]}) -- volles Protokoll unter build/diag/sammellauf-abnahme-${NAMEN[$i]}-*.log"
    fi
done
[ "$rot" -eq 0 ] && echo "  (kein roter Punkt)"
echo "  ${#NAMEN[@]} Punkte, $rot rot, Wandzeit ${GESAMT}s"
if [ "$SCHNELL" = 1 ]; then
    echo
    echo "== ABNAHME UNVOLLSTAENDIG (--schnell) =="
    echo "   Die RAM-Reihe ist der Punkt dieser Datei. Ohne sie ist das ein Zwischenstand."
    exit $((rot > 0))
fi
if [ "$rot" -eq 0 ]; then
    echo "== ABNAHME: ALL PASS =="
else
    echo "== ABNAHME: FAILURES =="
fi
exit $((rot > 0))
