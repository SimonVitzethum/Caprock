#!/usr/bin/env bash
# **Haelt die Menge der VA==PA-Annahmen aufgezaehlt.** (Nachgang zu E-Rest 3d.)
#
# ================================================================================================
# WORUM ES GEHT
# ================================================================================================
#
# Der Kernel bildet an manchen Stellen **identisch** ab: die virtuelle Adresse, die ein Subjekt
# sieht, IST die physische Adresse. Das ist bequem und an einigen Stellen sogar die ABI -- aber
# jede solche Stelle traegt eine stillschweigende Annahme, und die Annahmen sehen einander alle
# gleich. Genau daran hing der GiB-0-Deckel: die private Region einer isolierten PD war an GiB 0
# gebunden, weil `vspace_map_block` den Tabellenindex aus der PHYSadresse ableitet.
#
# Zwei Fehler dieses Projekts hatten dieselbe Form, und beide waren unsichtbar, **solange die
# beiden Zahlen zufaellig gleich waren**:
#
#   * `Scheduler::spawn_user` nahm EINEN Wert fuer den EL0-Stackzeiger UND die Reap-Region, die
#     beim Thread-Tod an den Allokator zurueckgeht. Beim Umbau auf ein VA-Fenster wurde daraus
#     ein `#PF cr2=0x80_0000_0000` im Kernel -- der Reap-Pfad gab eine VA als PA frei. Die
#     Funktion ist deshalb **geloescht**, nicht repariert.
#   * `spawn_isolated_native` nahm die Physadresse des Code-Frames als **Einsprungadresse**.
#
# Dieser Waechter sorgt dafuer, dass die verbliebenen Stellen eine **Liste** sind und keine
# Gewohnheit: jede identisch abbildende HAL-Funktion darf nur dort gerufen werden, wo dieser Kopf
# es mit einem Grund vermerkt. Kommt eine Stelle dazu, schlaegt er an -- und der Grund muss
# hingeschrieben werden, bevor sie durchgeht.
#
# **Was er NICHT kann:** er sieht Aufrufe, keine Absichten. Dass eine erlaubte Stelle ihre
# Identitaet weiterhin zu Recht annimmt, prueft er nicht -- dafuer stehen die Gruende hier, und
# `isohigh` in beiden Suiten misst den Fall, der frueher strukturell unmoeglich war.
#
# Aufruf:
#   tools/identitaet.sh              # pruefen + Selbsttest
#   tools/identitaet.sh --selftest
#
# Rueckgabe: 0 = Liste stimmt · 1 = unbenannte Stelle · 2 = Werkzeugfehler.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

# Die identisch abbildenden Funktionen der HAL. Wer eine neue hinzufuegt, muss sie hier eintragen
# -- sonst prueft der Waechter an ihr vorbei. Der Selbsttest deckt genau das ab.
FUNKTIONEN=(
    vspace_map_block
    vspace_map_code_block
    vspace_map_page
    vspace_unmap_block
    vspace_unmap_page
    vspace_map_device
    map_device_window_global
    map_device_block_global
)

# ================================================================================================
# DIE LISTE. Jede Zeile: <datei>:<funktion>|<Grund>
# ================================================================================================
#
# Der Grund ist keine Zierde. Er beantwortet: **warum darf hier VA == PA sein, und was waere die
# Folge, wenn es nicht mehr gaelte?**
ERLAUBT=(
"kernel/src/system.rs:vspace_map_block|SYS_MAP/map_into_thread: der Aufrufer nennt eine Memory-Cap, also eine PHYSadresse, und bekommt sie unter derselben Zahl in seine VSpace. Das ist die ABI (sel4lake_abi::sys::MAP) und keine Bequemlichkeit -- sie zu aendern hiesse, dass ein Subjekt eine VA nennen muesste, die es nicht kennt."
"kernel/src/system.rs:vspace_map_code_block|dito, RX-Fall desselben Pfades (W^X)."
"kernel/src/system.rs:vspace_map_page|dito, 4-KiB-Granularitaet."
"kernel/src/system.rs:vspace_unmap_block|Gegenstueck zu SYS_UNMAP -- muss dieselbe Achse benutzen wie das Mappen, sonst raeumt es an der falschen Stelle ab."
"kernel/src/system.rs:vspace_unmap_page|dito."
"kernel/src/system.rs:vspace_map_device|Geraetefenster (ext-22): eine PD sieht ein MMIO-Register unter seiner PHYSadresse. Hier ist die Identitaet die Zusicherung selbst -- der Treiber rechnet mit Adressen aus der PCI-Enumeration, und die sind physisch."
"kernel/src/system.rs:map_device_block_global|ECAM-Fenster global abbilden -- Kernelsicht, kein Subjekt beteiligt."
"kernel/src/arch/x86_64/bringup.rs:map_device_window_global|BAR-Fenster global abbilden, damit der Kernel enumerieren kann -- Kernelsicht, kein Subjekt beteiligt."
)

fehler=0
gefunden=0
unbenannt=()

# `pruefen <wurzel>` sucht **relativ** in `<wurzel>/kernel/src`, damit die gemeldeten Pfade genau
# die Schluessel der Liste sind. Absolut zu suchen war der erste Versuch -- dann trug jede Zeile
# das Temporaerverzeichnis im Namen, passte auf keinen Eintrag, und der Selbsttest meldete seine
# eigene Mechanik als Befund. (Er hat damit funktioniert: er schlug an, wo nichts war.)
pruefen() {
    local wurzel="$1"
    gefunden=0
    unbenannt=()
    local f datei zeile eintrag ok
    for f in "${FUNKTIONEN[@]}"; do
        while IFS= read -r zeile; do
            [ -n "$zeile" ] || continue
            datei="${zeile%%:*}"
            gefunden=$((gefunden + 1))
            ok=0
            for eintrag in "${ERLAUBT[@]}"; do
                if [ "${eintrag%%|*}" = "$datei:$f" ]; then ok=1; break; fi
            done
            [ "$ok" = 1 ] || unbenannt+=("$datei:$f  ($zeile)")
        done < <(cd "$wurzel" && grep -rn "hal::mmu::$f(" kernel/src --include=*.rs 2>/dev/null | cut -d: -f1,2)
    done
}

echo "== Identitaets-Annahmen (VA == PA): die Liste gegen den Quelltext =="
pruefen "$ROOT"
if [ "$gefunden" -eq 0 ]; then
    echo "  FEHLER: KEIN einziger Aufruf gefunden -- der Waechter liest ins Leere." >&2
    echo "          Entweder heissen die HAL-Funktionen anders, oder der Pfad stimmt nicht." >&2
    exit 2
fi
echo "  gefunden: $gefunden Aufrufstelle(n) in $(printf '%s\n' "${FUNKTIONEN[@]}" | wc -l) beobachteten Funktionen"
if [ "${#unbenannt[@]}" -gt 0 ]; then
    echo "  FEHLER: identisch abbildende Aufrufe OHNE Eintrag in der Liste:" >&2
    printf '    %s\n' "${unbenannt[@]}" >&2
    echo "  Jede solche Stelle traegt eine stillschweigende VA==PA-Annahme. Sie gehoert in den" >&2
    echo "  Kopf dieser Datei -- mit dem Grund, warum die Identitaet dort gilt und was die Folge" >&2
    echo "  waere, wenn sie faellt." >&2
    fehler=1
else
    echo "  jede Aufrufstelle steht mit Grund in der Liste"
fi

# -- Gegenprobe: eine EINGESCHLEUSTE Stelle muss auffallen ---------------------------------------
#
# Ohne sie waere „keine unbenannte Stelle" auch mit einem kaputten grep wahr. Dieselbe Form wie
# der Selbsttest von `tools/kernel-grenze.sh`.
if [ "${1:-}" != "--nur-pruefen" ]; then
    echo "-- Selbsttest --"
    W="$(mktemp -d)"
    trap 'rm -rf "$W"' EXIT
    mkdir -p "$W/kernel/src"
    cp -r kernel/src/. "$W/kernel/src/" 2>/dev/null
    cat > "$W/kernel/src/untergeschoben.rs" <<'RS'
// Vom Selbsttest eingeschleust: eine identisch abbildende Stelle ohne Eintrag in der Liste.
fn schmuggel(l2: u64, phys: u64) -> bool {
    hal::mmu::vspace_map_block(l2, phys)
}
RS
    pruefen "$W"
    if [ "${#unbenannt[@]}" -gt 0 ]; then
        echo "  Selbsttest: eine untergeschobene Stelle wird erkannt -- der Waechter ist sprechfaehig"
    else
        echo "  FEHLER: der Selbsttest hat NICHTS erkannt. Der Waechter kann nicht fehlschlagen," >&2
        echo "          also sagt sein Schweigen nichts." >&2
        fehler=1
    fi
    # Und die Gegenrichtung: ohne die eingeschleuste Datei muss er schweigen.
    rm -f "$W/kernel/src/untergeschoben.rs"
    pruefen "$W"
    if [ "${#unbenannt[@]}" -eq 0 ]; then
        echo "  Selbsttest: ohne sie schweigt er wieder -- er schlaegt nicht grundlos an"
    else
        echo "  FEHLER: der Waechter schlaegt auch ohne die eingeschleuste Stelle an." >&2
        fehler=1
    fi
fi

if [ "$fehler" -eq 0 ]; then
    echo "== Identitaets-Annahmen: aufgezaehlt und begruendet =="
    exit 0
fi
echo "== IDENTITAETS-LISTE VERLETZT ==" >&2
exit 1
