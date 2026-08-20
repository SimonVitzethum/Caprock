#!/usr/bin/env bash
# **Jede Berichtszeile gattert, oder sie ist ausdruecklich als Auskunft benannt.**
#
# Beim dritten Mal ist es eine Form, keine Panne. Dreimal hat dieses Projekt dieselbe Sache
# bezahlt:
#
#   * A-6.1 (2026-08-02, zweimal an einem Tag) — ein Urteil, das erst im BERICHT entsteht, kann den
#     Bericht nicht ausloesen: der Lauf faellt in den Watchdog und druckt das Ergebnis trotzdem.
#     Im Protokoll sieht das aus wie „gruen, aber gehangen".
#   * `pdbind` (2026-08-07) — `pdbind : FAILURES` bei `== ALL PASS ==`: `all_done()` baute eine
#     Liste fuer den Bericht und gab eine getrennte `&&`-Kette zurueck, 21 Glieder gegen 24
#     Eintraege.
#   * `dbg` (2026-08-20) — dieselbe Falle, drittes Mal: die Zeile stand in `all_done()` UND wurde
#     im Bericht gesetzt, also `bringup : offen waren: ... dbg` bei `dbg : ALL PASS` darueber.
#
# **Der vierte Watchdog-Tod kostet mehr als dieser Waechter.** Geprueft wird, dass jede Zeile mit
# einer `ALL PASS|FAILURES`-Signatur an genau einer der beiden Stellen haengt:
#
#   (a) als Konjunkt in `all_done()` — dann darf sie NICHT im Bericht gesetzt werden, oder
#   (b) als `check`-Zeile in der zugehoerigen Suite.
#
# Wer weder noch hat, druckt nur. Wer beides hat, hat die A-6.1-Falle.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
fail=0

# Zeilenpraefixe, die eine Signatur drucken. Aus dem Quelltext gelesen, nicht gepflegt --
# **eine Liste im Skript waere eine zweite Wirklichkeit**, und genau davon handelt dieser Waechter.
mapfile -t PRAEFIXE < <(
  grep -rhoE '"[a-z_]{2,8} +: (ALL PASS|FAILURES)' --include=*.rs kernel/ 2>/dev/null \
    | sed -E 's/^"([a-z_]+) +:.*/\1/' | sort -u
  grep -rhoE '\{\} \(|"[a-z_]{2,8} +: \{\}' --include=*.rs kernel/ 2>/dev/null \
    | grep -oE '"[a-z_]{2,8}' | tr -d '"' | sort -u
)
mapfile -t PRAEFIXE < <(printf '%s\n' "${PRAEFIXE[@]}" | sort -u | grep -vE '^$')

if [ "${#PRAEFIXE[@]}" -lt 5 ]; then
    echo "  FEHLER: nur ${#PRAEFIXE[@]} Berichtszeilen gefunden -- das Muster passt nicht mehr."
    echo "  **Eine Sprechprobe, die nichts findet, sieht aus wie ein bestandener Test.**"
    exit 1
fi
echo "== Berichtsgatter: ${#PRAEFIXE[@]} Zeilen mit Signatur gefunden =="

SUITEN="test-qemu-x86.sh test-qemu.sh test-qemu-x86-load.sh"
ohne=0
for p in "${PRAEFIXE[@]}"; do
    in_alldone=0; in_suite=0
    grep -rqE "\(\"$p\"," --include=*.rs kernel/ && in_alldone=1
    for s in $SUITEN; do
        [ -f "$s" ] || continue
        grep -qE "check \"$p +:" "$s" && in_suite=1
    done

    # **Je ARCHITEKTUR, nicht global** -- und das ist eine am 2026-08-20 bezahlte Verschaerfung.
    #
    # Die erste Fassung fragte „wird die Zeile in IRGENDEINER Suite gelesen". Als die `dbg`-Sonde
    # arch-neutral wurde, druckte sie ploetzlich auch auf aarch64 -- gegattert war sie aber nur in
    # `test-qemu-x86.sh`. Ergebnis: `dbg : FAILURES` bei `== ALL PASS ==`, woertlich die
    # pdbind-Form, und dieser Waechter sagte nichts, weil die Zeile ja „irgendwo" gattert.
    #
    # Eine Zeile, die von einem **arch-neutralen** Modul kommt, wird von BEIDEN Suiten gedruckt und
    # muss von beiden gelesen werden.
    if grep -rqE "\"$p +: " --include=*.rs kernel/src/dbgmem.rs kernel/src/dbgprobe.rs kernel/src/dmatests.rs 2>/dev/null; then
        for s in test-qemu-x86.sh test-qemu.sh; do
            [ -f "$s" ] || continue
            if ! grep -qE "check \"$p +:" "$s"; then
                echo "  OFFEN (arch): '$p' kommt aus einem ARCH-NEUTRALEN Modul, wird also von beiden Zweigen gedruckt -- $s liest sie nicht"
                ohne=$((ohne+1))
            fi
        done
    fi
    if [ "$in_alldone" -eq 0 ] && [ "$in_suite" -eq 0 ]; then
        echo "  OFFEN: '$p' druckt eine Signatur, wird aber weder in all_done() gegattert noch von einer Suite gelesen"
        ohne=$((ohne+1))
    fi
done

# **Keine Ratsche auf 0.** Es gibt legitime Auskunftszeilen, und ein Waechter, der sofort rot ist,
# wird abgeschaltet. Was hier gattert, ist der ZUWACHS: die Zahl darf nicht steigen.
GRENZE="${BERICHTSGATTER_GRENZE:-5}"
echo "  ungegatterte Signaturzeilen: $ohne (Grenze $GRENZE)"
if [ "$ohne" -gt "$GRENZE" ]; then
    echo "  FAIL: mehr ungegatterte Zeilen als erlaubt -- eine neue Zeile, die nur druckt"
    fail=1
fi

# **Ein Hinweis fuer „steht in all_done() UND in der Suite" stand hier und ist wieder weg.**
#
# Er feuerte auf rund dreissig Zeilen — also auf den NORMALFALL: beides zu haben ist Tiefe, kein
# Fehler. Die A-6.1-Falle ist enger (ein Konjunkt, dessen Wert erst IM Bericht entsteht) und aus
# den Namen nicht mechanisch zu entscheiden. Ein Warnschild, das bei jedem Vorbeigehen leuchtet,
# wird abgeschaltet — und danach schweigt es auch beim echten Fall. Dieselbe Lehre wie die
# B2-Veraltungsmeldung im Scheduler-Waechter.
#
# Was bleibt, ist die Haelfte, die entscheidbar IST: eine Zeile, die WEDER gattert NOCH gelesen
# wird, druckt nur.

if [ "$fail" -eq 0 ]; then echo "== BERICHTSGATTER: ALL PASS =="; else echo "== BERICHTSGATTER: FAILURES =="; fi
exit "$fail"
