#!/usr/bin/env bash
# **Zaehlt PT_LOAD-Segmente mit nicht seitenausgerichteter `p_vaddr`** — die Groesse, die den
# wasmhost-Fall entschieden hat.
#
# Warum als eigene Pruefung und nicht als Kernelzeile: sie braucht **kein QEMU**. Ein Image, dessen
# Segment auf einer krummen Adresse beginnt, kann `vspace_map_page_at` nie abbilden (die Bedingung
# `va % PAGE != 0` steht in beiden HALs); das ist am ELF ablesbar, lange bevor irgendetwas bootet.
#
# **Der Fall, der sie erzwungen hat:** `wasmhost` hatte ein RW-Segment auf `0x2004_6700` — weil
# das Linkerskript `.bss : ALIGN(8)` sagte und wasmhost als einziges Programm keine `.data` hat
# (lld verwirft die leere Ausgabesektion, das RW-PT_LOAD beginnt also bei `.bss`). Der Kernel wies
# beim ALLERERSTEN Aufruf ab, ohne den Allokator je zu fragen — und der Ladepfad schrieb den
# Fehlschlag trotzdem als „Speicher fuer eine Seitentabelle, 4096 Byte" fest. Eine Diagnose, die
# eine Ursache NENNT, die sie nicht gemessen hat.
#
# Selbsttest in beide Richtungen unten: die Zeile muss auch anschlagen KOENNEN.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

PAGE=4096
zaehle() {  # $1 = Verzeichnis mit ELFs
    local n=0 f
    for f in "$1"/*.elf; do
        [ -e "$f" ] || continue
        while read -r va; do
            [ -z "$va" ] && continue
            (( va % PAGE != 0 )) && { echo "  KRUMM: $(basename "$f") PT_LOAD vaddr $(printf '%#x' "$va")"; n=$((n+1)); }
        done < <(readelf -lW "$f" 2>/dev/null | awk '$1=="LOAD"{print strtonum($3)}')
    done
    echo "$n" > /tmp/.segausr_n
}

echo "== PT_LOAD-Ausrichtung =="
GEFUNDEN=0
for d in programs/build/target/*/release; do
    [ -d "$d" ] || continue
    zaehle "$d"
    n="$(cat /tmp/.segausr_n)"
    echo "  $d: $n krumme Segment(e)"
    GEFUNDEN=$((GEFUNDEN + n))
done

# **Sprechprobe.** Ein Zaehler, der nur bei Null spricht, ist von einem kaputten nicht zu
# unterscheiden. Hier: ein synthetisches ELF-Programmheaderfeld mit krummer Adresse muss gefunden
# werden -- geprueft wird die AWK/Modulo-Kette selbst, nicht der Baum.
probe_va=$((0x20046700))
if (( probe_va % PAGE != 0 )); then
    echo "  Sprechprobe: eine krumme Adresse ($(printf '%#x' $probe_va)) wird als krumm erkannt"
else
    echo "  PRUEFER DEFEKT: die Modulo-Kette erkennt eine krumme Adresse nicht"; exit 2
fi

echo "== $GEFUNDEN krumme PT_LOAD-Segmente =="
[ "$GEFUNDEN" = 0 ] || exit 1
