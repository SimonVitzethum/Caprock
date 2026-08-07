#!/usr/bin/env bash
# **D0-Messung: viele Laeufe, jeder vollstaendig aufgehoben, nur die normalen geloescht.**
#
# ================================================================================================
# WAS GEMESSEN WIRD -- und was der Aufbau dafuer leisten muss
# ================================================================================================
#
# D0 ist ein Haenger ohne bekannte Ursache, zuletzt unter der Messschwelle (obere 95-%-Schranke
# 0,13 % aus 2300 Laeufen). Um darunter zu kommen, braucht es Groessenordnungen mehr Laeufe -- und
# von jedem abweichenden Lauf ein **vollstaendiges Protokoll**, sonst kostet jeder Treffer nur
# Wandzeit. Genau daran ist diese Sitzung dreimal gescheitert (D12).
#
# Drei Eigenschaften, ohne die die Messung nichts sagt:
#
# **(1) EINE Referenzsignatur fuer ALLE Stroeme.** Bis 2026-08-03 verglich jeder Lauf nur gegen
# den ersten Lauf seines EIGENEN Stroms. Fuenf Stroeme mit je in sich stimmiger, untereinander
# aber verschiedener Signatur haetten fuenfmal gruen gemeldet. Hier gibt es genau eine Referenz.
#
# **(2) DREI Melder, und aufgehoben wird bei jedem.** Signaturabweichung, Rueckgabewert != 0
# (inkl. 124 = Zeitlimit) und **leere Ausgabe**. Der dritte ist nicht kosmetisch: ein Lauf, der
# mitten in der Ausgabe endet, hat eine leere Signatur -- und eine leere Signatur ist von einer
# uebereinstimmenden nicht zu unterscheiden, wenn man nur vergleicht. Das war das
# „erfundene Erfolge"-Loch vom 2026-08-05, hier waere es fatal.
#
# **(3) Jeder Lauf braucht seine EIGENE Platte.** Die Hauptsuite SCHREIBT (`OP_WRITE`,
# Sondensektor) -- parallele Laeufe auf einer Datei verseuchen einander, und gemessen waere der
# eigene Aufbau. Geloest ueber `snapshot=on`: jede QEMU-Instanz bekommt ein eigenes
# Copy-on-Write-Overlay, das Master-Abbild bleibt unberuehrt. Kein Kopieren, keine Verseuchung.
#
# Aufruf:
#   tools/d0-messen.sh [ANZAHL] [PARALLEL] [RAM]
#
# Abweichende Laeufe landen unter `build/d0/`. Abbruch mit Ctrl-C oder `touch build/d0/STOP`.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

ANZAHL="${1:-50000}"
# `PARALLEL` ist die OBERGRENZE, nicht die Vorgabe: der Regler faehrt von unten hoch, solange
# CPU und RAM es hergeben (s. `regler`). 0 = nur aus den Ressourcen ableiten.
PARALLEL="${2:-0}"
RAM="${3:-512M}"
ZEITLIMIT=120
# **Ressourcenschranken -- damit die Messung die Maschine nicht mitnimmt.**
# Zielauslastung der CPU in Prozent; darueber wird nicht weiter hochgefahren.
CPU_ZIEL="${CPU_ZIEL:-90}"
# Speicherbudget fuer die Laeufe in MiB: Ziel 8 GiB, harte Decke 10 GB -- und zusaetzlich nie
# mehr als (verfuegbar - 2 GiB), weil eine Messung, die den Wirt ins Swappen treibt, nicht mehr
# misst, was sie messen soll.
RAM_ZIEL_MIB="${RAM_ZIEL_MIB:-8192}"
RAM_DECKE_MIB="${RAM_DECKE_MIB:-10000}"
# Sicherung gegen ein volllaufendes Dateisystem: bricht ab, wenn zu viele Laeufe abweichen. Eine
# Messung, die die Platte fuellt, endet als Aufbauproblem und nicht als Ergebnis.
MAX_ABWEICHUNGEN="${MAX_ABWEICHUNGEN:-2000}"

# **Das Multiboot-ELF32, nicht das ELF64.** QEMUs `-kernel` laedt nur ein 32-Bit-Multiboot-Bild
# ("Cannot load x86-64 image, give a 32bit one") -- der erste Anlauf zeigte das im Referenzlauf,
# und genau dafuer gibt es ihn: eine Messung, die mit einem nicht ladbaren Bild startet, haette
# 50000 leere Protokolle erzeugt.
ELF="build/target/x86_64-unknown-none/release/sel4lake-kernel.mb32"
D0="build/d0"
rm -rf "$D0"; mkdir -p "$D0"

# -- Bauen: EINMAL. -----------------------------------------------------------------------------
echo "== bauen =="
# **`--features selftest`, wie die Suite.** Ohne das Feature gibt es keine Pruefinfrastruktur,
# `all_done()` wird nie wahr, der Gast faehrt nicht herunter -- der Referenzlauf lief ins
# Zeitlimit (rc=124, 8 Signaturzeilen statt 30). Auch das hat die Referenzpruefung gefangen.
./build-x86.sh --features selftest >/dev/null 2>&1 || { echo "BUILD FAILED" >&2; exit 2; }
[ -f "$ELF" ] || { echo "FEHLER: $ELF fehlt." >&2; exit 2; }

# -- Master-Abbild: EINES fuer alle, dank `snapshot=on` nur gelesen. -----------------------------
MASTER="$D0/master.img"
# **Wortgleich zur Hauptsuite.** Zwei Suiten, die dieselbe Platte verschieden aufsetzen, waeren
# derselbe Riss wie zwei, die dasselbe Geraet verschieden aufsetzen (CLAUDE.md) -- und hier waere
# er schlimmer: die Referenzsignatur haenge dann an einer anderen Platte als die Vergleichslaeufe.
python3 tools/mkgpt.py "$MASTER" --sectors 32768 \
    --part 34:20000 --part 20001:32700 --magic-at 20001 \
    --fat16 34:20000 --file "HELLO.TXT=SEL4LAKE-DATEIINHALT" >/dev/null 2>&1 || {
    echo "FEHLER: mkgpt.py hat kein Abbild erzeugt." >&2; exit 2; }
[ -s "$MASTER" ] || { echo "FEHLER: Master-Abbild leer." >&2; exit 2; }

if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL=(-enable-kvm -cpu host,+invtsc,host-cache-info=on)
else
    ACCEL=(-cpu Skylake-Client)
    echo "HINWEIS: kein KVM -- die Messung ist damit eine andere (TCG)." >&2
fi

# Die Signatur: **dieselbe Extraktion wie in `test-qemu-x86.sh`**. Eine zweite Fassung koennte
# still auseinanderlaufen, und dann misst diese Datei etwas anderes als die Suite.
signatur() {
    grep -oE '^[a-z0-9_]+ +: (ALL PASS|FAILURES|SKIP)|^== SELFTEST [A-Z]+( \(watchdog\))?' "$1" \
        | sort
}

ein_lauf() {   # ein_lauf <logdatei>
    local log="$1"
    timeout "$ZEITLIMIT" qemu-system-x86_64 \
        -kernel "$ELF" -m "$RAM" -smp 4 "${ACCEL[@]}" \
        -machine q35,kernel-irqchip=split -device intel-iommu,caching-mode=on,intremap=on \
        -device virtio-rng-pci,disable-legacy=on,iommu_platform=on \
        -drive if=none,id=blk0,format=raw,file="$MASTER",snapshot=on \
        -device virtio-blk-pci,drive=blk0,disable-legacy=on,iommu_platform=on \
        -netdev user,id=net0 \
        -device virtio-net-pci,netdev=net0,disable-legacy=on,iommu_platform=on \
        -nographic -serial file:"$log" -no-reboot \
        </dev/null >/dev/null 2>&1
    return $?
}

# -- Referenzlauf: EINER, fuer alle Stroeme. ----------------------------------------------------
echo "== Referenzlauf =="
REF_LOG="$D0/referenz.log"
ein_lauf "$REF_LOG"; ref_rc=$?
signatur "$REF_LOG" > "$D0/referenz.sig"
if [ ! -s "$D0/referenz.sig" ] || [ "$ref_rc" -ne 0 ]; then
    echo "FEHLER: der Referenzlauf ist selbst nicht sauber (rc=$ref_rc, $(wc -l < "$D0/referenz.sig") Signaturzeilen)." >&2
    echo "        Eine Messung gegen eine kaputte Referenz misst nichts." >&2
    exit 2
fi
echo "  Referenz: $(wc -l < "$D0/referenz.sig") Ergebniszeilen, rc=0"
echo "  $(md5sum < "$D0/referenz.sig" | cut -c1-12)"

# -- Wieviel Speicher kostet EIN Lauf? Gemessen, nicht geschaetzt. ------------------------------
#
# `-m 512M` ist die Zusage an den Gast, nicht der Verbrauch des Wirts: QEMU legt Gastspeicher
# faul an, und der Kernel fasst nur einen Teil davon an. Wer mit 512 MiB je Lauf rechnet, laesst
# die halbe Maschine ungenutzt -- wer mit 50 rechnet, treibt sie ins Swappen. Also messen.
echo "== Speicherbedarf eines Laufs messen =="
# **Ueber die PID, nicht ueber den Prozessnamen.** `ps -C qemu-system-x86_64` greift nicht:
# `comm` ist auf 15 Zeichen gekuerzt, und was der erste Anlauf dann mass, waren 2479 MiB je Lauf
# -- fuer einen 512-MiB-Gast unmoeglich, und die Folge waere gewesen, dass der Regler bei drei
# Arbeitern stehenbleibt. Gemessen ueber die PID: 240 MiB.
# **Ueber den ganzen Prozessbaum, nicht ueber die PID allein.** Das war bis zum 2026-08-07 falsch:
# `$!` ist die Subshell, die `ein_lauf` ausfuehrt -- QEMU ist ihr KIND. Gemessen wurden also ein
# paar MiB Bash, der Wert fiel unter die Untergrenze, und die griff still. Im Protokoll stand dann
# „je Lauf rund 192 MiB" -- das ist exakt `128 * 3/2`, also die Untergrenze und kein Messwert.
#
# Eine Untergrenze, die einspringt, wenn die Messung nichts sieht, ist derselbe Fehler wie ein
# Pruefer, der bei Schweigen Erfolg meldet: sie macht den Ausfall der Messung unsichtbar. Deshalb
# steht darunter jetzt eine **Sprechprobe**.
baum_rss_mib() {
    local wurzel="$1" i=0 k
    local -a pids=("$wurzel")
    while [ "$i" -lt "${#pids[@]}" ]; do
        for k in $(ps -o pid= --ppid "${pids[$i]}" 2>/dev/null); do pids+=("$k"); done
        i=$((i + 1))
    done
    local liste; liste="$(IFS=,; echo "${pids[*]}")"
    ps -o rss= -p "$liste" 2>/dev/null | awk '{s+=$1} END {print int(s/1024)}'
}

ein_lauf "$D0/mess.log" &
MESS_PID=$!
PRO_LAUF_MIB=0
for _ in $(seq 1 60); do
    sleep 0.1
    m="$(baum_rss_mib "$MESS_PID")"
    [ -n "$m" ] && [ "$m" -gt "$PRO_LAUF_MIB" ] 2>/dev/null && PRO_LAUF_MIB=$m
done
wait "$MESS_PID" 2>/dev/null
rm -f "$D0/mess.log"
# **Sprechprobe der Speichermessung.** Ein 512-MiB-Gast unter QEMU/KVM kostet den Wirt zwangslaeufig
# mehr als 64 MiB; sieht die Messung weniger, hat sie den falschen Prozess beobachtet -- und dann
# ist der Regler blind, nicht vorsichtig. Abbruch statt Untergrenze.
if [ "$PRO_LAUF_MIB" -lt 64 ]; then
    echo "FEHLER: die Speichermessung sah nur $PRO_LAUF_MIB MiB je Lauf." >&2
    echo "        Fuer einen 512-MiB-Gast ist das unmoeglich -- vermutlich wurde der falsche" >&2
    echo "        Prozess beobachtet. Ein Regler auf einer kaputten Messung ist gefaehrlicher" >&2
    echo "        als gar keiner; es gibt hier bewusst KEINE Untergrenze, die das verdeckt." >&2
    exit 2
fi
PRO_LAUF_MIB=$(( PRO_LAUF_MIB * 3 / 2 ))
echo "  je Lauf rund $PRO_LAUF_MIB MiB (Hoechststand ueber den Prozessbaum + 50 % Zuschlag)"

VERFUEGBAR_MIB="$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)"
BUDGET_MIB=$(( RAM_ZIEL_MIB < RAM_DECKE_MIB ? RAM_ZIEL_MIB : RAM_DECKE_MIB ))
SICHER_MIB=$(( VERFUEGBAR_MIB - 2048 ))
[ "$SICHER_MIB" -lt "$BUDGET_MIB" ] && BUDGET_MIB="$SICHER_MIB"
[ "$BUDGET_MIB" -lt "$PRO_LAUF_MIB" ] && { echo "FEHLER: zu wenig Speicher frei ($VERFUEGBAR_MIB MiB)." >&2; exit 2; }
MAX_RAM=$(( BUDGET_MIB / PRO_LAUF_MIB ))
[ "$PARALLEL" -gt 0 ] && [ "$PARALLEL" -lt "$MAX_RAM" ] && MAX_RAM="$PARALLEL"
echo "  Budget $BUDGET_MIB MiB (verfuegbar $VERFUEGBAR_MIB) -> hoechstens $MAX_RAM gleichzeitig"

# -- Arbeitsvorrat: EIN Zaehler, von allen Arbeitern unter `flock` geholt. ----------------------
#
# Feste Stroeme mit fester Laufzahl gingen nicht mehr: die Zahl der Arbeiter aendert sich
# waehrend der Messung. Ein gemeinsamer Vorrat ist ausserdem ehrlicher -- ein langsamer Arbeiter
# haelt die Bilanz nicht auf.
echo "$ANZAHL" > "$D0/vorrat"
: > "$D0/sperre"
naechster() {   # gibt 1 aus, wenn noch Arbeit da ist
    (
        flock 9
        local r; r="$(cat "$D0/vorrat")"
        if [ "$r" -le 0 ]; then echo 0; else echo $((r - 1)) > "$D0/vorrat"; echo 1; fi
    ) 9<"$D0/sperre"
}

echo "== $ANZAHL Laeufe, Regler auf CPU ${CPU_ZIEL} % / $BUDGET_MIB MiB (RAM $RAM je Gast) =="
echo "   abweichende Laeufe: $D0/abweichung-*.log   ·   Abbruch: touch $D0/STOP"

strom() {   # strom <nr>
    local nr="$1" sig rc abw=0 ok=0 i=0
    local tmp="$D0/lauf-$nr.tmp"
    while [ "$(naechster)" = "1" ]; do
        [ -e "$D0/STOP" ] && break
        i=$((i + 1))
        rc=0
        ein_lauf "$tmp" || rc=$?
        sig="$D0/sig-$nr.tmp"
        signatur "$tmp" > "$sig"
        # **Drei Melder, aufgehoben wird bei jedem.** Die leere Signatur steht ausdruecklich
        # dabei: ohne sie ginge ein abgebrochener Lauf als uebereinstimmend durch.
        if [ ! -s "$sig" ] || [ "$rc" -ne 0 ] || ! cmp -s "$D0/referenz.sig" "$sig"; then
            abw=$((abw + 1))
            local grund="signatur"
            [ ! -s "$sig" ] && grund="leer"
            [ "$rc" -ne 0 ] && grund="rc$rc"
            cp -f "$tmp" "$D0/abweichung-s${nr}-l${i}-${grund}.log"
            diff "$D0/referenz.sig" "$sig" > "$D0/abweichung-s${nr}-l${i}-${grund}.sigdiff" 2>/dev/null
        else
            ok=$((ok + 1))
        fi
        rm -f "$tmp" "$sig"
        if (( i % 20 == 0 )); then
            echo "$ok $abw" > "$D0/stand-$nr"
        fi
    done
    echo "$ok $abw" > "$D0/stand-$nr"
    rm -f "$D0/lauf-$nr.tmp" "$D0/sig-$nr.tmp"
}

# -- Der Regler: von unten hochfahren, bis CPU oder RAM die Grenze setzen. ----------------------
#
# **Hochfahren, nicht vorgeben.** Eine feste Zahl waere entweder zu klein (Maschine langweilt
# sich) oder zu gross (Swappen, und dann misst die Messung den Wirt). Die CPU-Auslastung kommt
# aus zwei `/proc/stat`-Proben, nicht aus `uptime` -- die Lastmittel dort haengen der Wirklichkeit
# um Minuten hinterher, und der Regler wuerde ueberschwingen.
cpu_last() {
    local a b i1 i2 t1 t2
    a=($(awk '/^cpu /{print}' /proc/stat)); i1=${a[4]}; t1=0
    for v in "${a[@]:1}"; do t1=$((t1 + v)); done
    sleep 3
    b=($(awk '/^cpu /{print}' /proc/stat)); i2=${b[4]}; t2=0
    for v in "${b[@]:1}"; do t2=$((t2 + v)); done
    local dt=$((t2 - t1)) di=$((i2 - i1))
    [ "$dt" -le 0 ] && { echo 0; return; }
    echo $(( 100 - (100 * di / dt) ))
}

# **Regler und Arbeiter laufen in DERSELBEN Shell.** Der erste Anlauf hatte den Regler in einer
# Subshell -- die dort gestarteten Arbeiter waren damit Kinder der Subshell, das `wait` der
# Hauptshell kannte sie nicht, und beim Ende der Subshell wurden sie mitten im Lauf abgeschnitten.
# Sichtbar wurde es an der Bilanz: 195 statt 200 Laeufe. Fuenf Laeufe, die niemand gezaehlt hat --
# und in einer 50000er-Messung waeren das Hunderte, die weder als normal noch als abweichend
# erscheinen. Ein Zaehler, der still verliert, ist schlimmer als einer, der falsch zaehlt.
ARBEITER=0
starte_arbeiter() { ARBEITER=$((ARBEITER + 1)); strom "$ARBEITER" & }
starte_arbeiter; starte_arbeiter

zahl() { local v="${1:-}"; case "$v" in ''|*[!0-9]*) echo 0 ;; *) echo "$v" ;; esac; }

bilanz_lesen() {   # setzt G_OK und G_ABW
    G_OK=0; G_ABW=0
    local f o b
    for f in "$D0"/stand-*; do
        [ -f "$f" ] || continue
        read -r o b < "$f" 2>/dev/null || continue
        G_OK=$((G_OK + $(zahl "$o"))); G_ABW=$((G_ABW + $(zahl "$b")))
    done
}

hoch=1
letzte_meldung=0
while :; do
    [ -e "$D0/STOP" ] && break
    [ "$(zahl "$(cat "$D0/vorrat" 2>/dev/null)")" -le 0 ] && break

    if [ "$hoch" = 1 ] && [ "$ARBEITER" -lt "$MAX_RAM" ]; then
        c="$(cpu_last)"
        v="$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)"
        if [ "$c" -lt "$CPU_ZIEL" ] && [ "$v" -gt 2048 ]; then
            starte_arbeiter
            echo "  [$(date +%H:%M:%S)] CPU ${c} %, frei ${v} MiB -> Arbeiter $ARBEITER"
            continue
        fi
        hoch=0
        echo "  [$(date +%H:%M:%S)] Regler steht bei $ARBEITER Arbeitern (CPU ${c} %, frei ${v} MiB)"
    fi

    sleep 30
    jetzt=$(date +%s)
    if (( jetzt - letzte_meldung >= 60 )); then
        bilanz_lesen
        rest="$(zahl "$(cat "$D0/vorrat" 2>/dev/null)")"
        echo "  [$(date +%H:%M:%S)] $((G_OK + G_ABW)) gefahren, $G_OK normal, $G_ABW abweichend, $rest offen ($ARBEITER Arbeiter)"
        letzte_meldung=$jetzt
        if [ "$G_ABW" -ge "$MAX_ABWEICHUNGEN" ]; then
            echo "  ABBRUCH: $G_ABW Abweichungen (Grenze $MAX_ABWEICHUNGEN) -- die Protokolle wuerden die Platte fuellen." >&2
            touch "$D0/STOP"
            break
        fi
    fi
done

# Auf ALLE Arbeiter warten -- sie sind Kinder dieser Shell.
wait

# -- Bilanz -------------------------------------------------------------------------------------
bilanz_lesen
gesamt_ok=$G_OK; gesamt_abw=$G_ABW
gefahren=$((gesamt_ok + gesamt_abw))
echo "== Bilanz =="
echo "  gefahren   : $gefahren"
echo "  normal     : $gesamt_ok"
echo "  abweichend : $gesamt_abw"
if [ "$gefahren" -gt 0 ] && [ "$gesamt_abw" -eq 0 ]; then
    # Die Dreierregel: obere 95-%-Schranke bei 0 Treffern in n Laeufen ist 3/n.
    echo "  0 Abweichungen in $gefahren Laeufen -> obere 95-%-Schranke $(python3 -c "print(f'{3/$gefahren*100:.4f}')") %"
fi
ls "$D0"/abweichung-*.log >/dev/null 2>&1 && {
    echo "  Protokolle:"; ls -1 "$D0"/abweichung-*.log | head -20 | sed 's/^/    /'
}
rm -f "$D0/master.img"
[ "$gesamt_abw" -eq 0 ]
