#!/usr/bin/env bash
# **Haelt die VA==PA-Annahmen strukturell zusammen.** (Nachgang zu E-Rest 3d.)
#
# ================================================================================================
# WAS SICH GEGENUEBER DER ERSTEN FASSUNG GEAENDERT HAT -- und warum
# ================================================================================================
#
# Die erste Fassung war eine **Liste im Skript**, gegen die die Aufrufstellen gehalten wurden. Sie
# hat innerhalb eines Tages zwei eigene Loecher gezeigt:
#
#   * `hal::mmu::vspace_map_dma` stand in ihrer Funktionsliste **gar nicht** -- der DMA-Pfad einer
#     Treiber-PD bildet identisch ab, und der Waechter sah ihn nie. Eine Textflaeche ueber einem
#     Loch.
#   * der Grundtext zu `SYS_MAP` war **falsch**: er sagte „das ist die ABI". `sel4lake_abi::sys::MAP`
#     traegt aber kein Adressargument -- der Aufrufer nennt eine **Cap**, und die Basis kommt aus
#     `ObjectKind::Memory(r).base`, also aus der Cap-Aufloesung IM KERNEL. Die Identitaet liegt
#     damit in einer Entscheidung des Kernels, nicht in der Schnittstelle, und ist behebbar, ohne
#     die ABI anzufassen. Ein Grund, den niemand widerlegen kann, ueberlebt seinen Autor auch
#     dann, wenn er falsch ist.
#
# Deshalb liegt die Liste jetzt **im Quelltext**: `addr::IdentityReason` ist ein geschlossenes
# Enum, und `addr::Va::identity(reason, pa)` ist die **einzige** Umwandlung `Pa -> Va`. `Va` hat
# keinen Konstruktor aus `u64` und keinen aus `Pa`. Eine neue identische Abbildung braucht eine
# neue Variante -- und die schreibt man nicht versehentlich.
#
# Dieses Skript prueft daher nur noch das, was ein Typ nicht kann:
#   1. dass es **keinen zweiten Weg** in einen `Va` gibt (kein `From<u64>`, kein `Va::new`),
#   2. dass **jede** Enum-Variante einen Grundtext traegt,
#   3. dass die identisch abbildenden HAL-Funktionen nur aus den benannten Engstellen gerufen
#      werden -- die Liste der Funktionen kommt dabei aus der **HAL selbst**, nicht aus diesem
#      Skript (sonst waere das erste Loch sofort wieder da),
#   4. **Falsifikatoren**: wo ein Grund widerlegbar ist, wird er widerlegt versucht. Bisher einer
#      -- s. `SyscallMapByCap` unten.
#
# Aufruf:
#   tools/identitaet.sh              # pruefen + Selbsttest
#   tools/identitaet.sh --nur-pruefen
#
# Rueckgabe: 0 = in Ordnung · 1 = Befund · 2 = Werkzeugfehler.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

fehler=0
n=0
ok()   { n=$((n+1)); echo "  ok      : $1"; }
nok()  { n=$((n+1)); echo "  BEFUND  : $1" >&2; fehler=1; }

echo "== Identitaets-Annahmen (VA == PA): der Typ, nicht die Textflaeche =="

# -- 1. Gibt es einen zweiten Weg in einen `Va`? --------------------------------------------------
#
# Der ganze Umbau haengt daran. Solange `Va::new(x)` oder `impl From<u64> for Va` existiert, ist
# `IdentityReason` eine Bitte und keine Bedingung.
ADDR="kernel/src/addr.rs"
[ -f "$ADDR" ] || { echo "FEHLER: $ADDR fehlt -- der Waechter liest ins Leere." >&2; exit 2; }
if grep -qE "impl +From<u64> +for +Va|impl +From<Pa> +for +Va|pub +const +fn +new" <<<"$(sed -n '/impl Va {/,/^}/p' "$ADDR")"; then
    nok "es gibt einen allgemeinen Konstruktor fuer \`Va\` -- die Bindung Stelle<->Grund waere damit unverbindlich."
else
    ok "\`Va\` hat keinen allgemeinen Konstruktor aus \`u64\`/\`Pa\`"
fi
# **Kein WAEHLBARER Grund mehr.** `Va::identity(reason, pa)` war ein freies Argument: nichts
# hinderte `Va::identity(Mmio, dma_pa)`, und der Waechter haette einen gueltigen Grund gesehen und
# geschwiegen. Es gibt jetzt einen Konstruktor JE STELLE.
if grep -q "pub const fn identity" "$ADDR"; then
    nok "\`Va::identity(reason, pa)\` existiert wieder -- ein waehlbarer Grund ist ein Warnschild, keine Bindung."
else
    ok "kein waehlbarer Grund: es gibt einen Konstruktor je Stelle (\`Va::for_*\`), kein Grund-Argument"
fi
KONSTRUKTOREN="$(grep -oE 'pub const fn for_[a-z_]+' "$ADDR" | sed 's/pub const fn //' | sort -u)"
[ -n "$KONSTRUKTOREN" ] || nok "keine \`Va::for_*\`-Konstruktoren gefunden -- der Umbau ist nicht da, wo der Waechter ihn sucht."
for f in "pub const fn window" "pub const fn link"; do
    grep -q "$f" "$ADDR" || nok "der benannte Weg \`$f\` fehlt -- \`Va\` waere anders gebaut als angenommen."
done
# **Die Zuordnung Konstruktor -> AUFRUFENDE FUNKTION, als Menge.**
#
# Die erste Fassung zaehlte nur: „ein- oder zweimal". Das ist dieselbe Form, gegen die dieser
# ganze Umbau geht -- **eine Kardinalzahl steht, wo eine Menge gemeint ist**. Zwei Aufrufe aus dem
# FALSCHEN Paar sind von Abbilden+Gegenstueck nicht zu unterscheiden, solange nur gezaehlt wird;
# und die Lockerung von „hoechstens einmal" auf „zwei" hat genau das Loch aufgemacht, das sie
# schliessen sollte.
#
# **Warum das ein Skript tut und nicht rustc.** Die saubere Fassung waere
# `pub(in crate::system) const fn for_mmio_window(..)` -- aber `Va` liegt in `crate::addr`, und
# Rusts `pub(in path)` verlangt einen VORFAHREN des Elements. Ein Modul, das nicht ueber `addr`
# liegt, ist nicht ausdrueckbar. Der strukturelle Weg waere, die Engstellen in ein privates
# Untermodul zu ziehen, das seine Zeugen-Typen selbst besitzt; das steht als eigener Punkt in
# `todo.md`. Bis dahin haelt diese Tabelle die Bindung -- und sie haelt NAMEN, keine Zahlen.
declare -A BINDUNG=(
    [for_syscall_map]="map_frame unmap_frame"
    [for_kernel_setup]="map_into_thread unmap_into_thread"
    [for_mmio_window]="map_region_into_thread unmap_window"
    [for_dma_window]="map_region_into_thread unmap_dma_from_thread"
    [for_kernel_global_window]="pcie_find_virtio run"
)
schief=""
for k in $KONSTRUKTOREN; do
    erwartet="$(tr ' ' '\n' <<<"${BINDUNG[$k]:-}" | grep -v '^$' | sort -u)"
    if [ -z "$erwartet" ]; then
        schief="$schief\n    $k: kein Eintrag in der Bindungstabelle"
        continue
    fi
    gefunden="$(grep -rn "Va::$k" kernel/src --include=*.rs 2>/dev/null | while IFS= read -r z; do
        d="$(cut -d: -f1 <<<"$z")"; n="$(cut -d: -f2 <<<"$z")"
        head -n "$n" "$d" | grep -oE '^[a-z ]*fn [a-z_0-9]+' | tail -1 | sed 's/.*fn //'
    done | sort -u)"
    if [ "$erwartet" != "$gefunden" ]; then
        schief="$schief\n    $k: erwartet [$(tr '\n' ' ' <<<"$erwartet")] gefunden [$(tr '\n' ' ' <<<"$gefunden")]"
    fi
done
if [ -n "$schief" ]; then
    nok "die Bindung Konstruktor<->aufrufende Funktion stimmt nicht:$(printf '%b' "$schief")"
else
    ok "jeder der $(echo "$KONSTRUKTOREN" | wc -w) Stellen-Konstruktoren wird von GENAU den eingetragenen Funktionen gerufen (Namen, nicht Anzahl)"
fi

# -- 2. Traegt jede Variante einen Grund? ---------------------------------------------------------
#
# „Grund" heisst hier: ein Doku-Kommentar unmittelbar davor. Eine Variante ohne ist genau der
# Fall, den die erste Fassung nicht verhindern konnte.
VARIANTEN="$(sed -n '/pub enum IdentityReason {/,/^}/p' "$ADDR" | grep -oE '^ {4}[A-Z][A-Za-z]+,' | tr -d ' ,')"
if [ -z "$VARIANTEN" ]; then
    echo "FEHLER: keine Varianten von IdentityReason gefunden -- Enum umbenannt?" >&2
    exit 2
fi
ohne_grund=""
for v in $VARIANTEN; do
    # Die Zeile davor muss ein Doku-Kommentar sein.
    zeile="$(grep -n "^    $v,$" "$ADDR" | head -1 | cut -d: -f1)"
    vor="$(sed -n "$((zeile-1))p" "$ADDR")"
    case "$vor" in
        *"///"*) ;;
        *) ohne_grund="$ohne_grund $v" ;;
    esac
done
if [ -n "$ohne_grund" ]; then
    nok "Variante(n) ohne Grundtext:$ohne_grund"
else
    ok "alle $(echo "$VARIANTEN" | wc -w) Varianten von \`IdentityReason\` tragen einen Grund"
fi
if true; then :
fi

# -- 1b. Die SCHULD-Ratsche ----------------------------------------------------------------------
#
# `IdentityReason` faltete anfangs zwei Dinge in einen Begriff: „die Identitaet IST hier die
# Zusicherung" und „die Identitaet ist eine Entscheidung, behebbar". Damit waere die Schuld
# unsichtbar geworden -- wer die Liste liest, saehe ueberall einen Grund und schloesse, alles sei
# nach Absicht. Jetzt traegt jede Variante eine **Klasse**, und die Zahl der Schulden darf nur
# fallen.
# Die Schuld-Varianten aus `class()` -- und die eingetragene Menge aus `IDENTITY_DEBTS`.
IST="$(sed -n '/pub const fn class(self)/,/^    }/p' "$ADDR" \
       | grep -E 'IdentityReason::[A-Za-z]+ *=> *IdentityClass::Debt' \
       | grep -oE 'IdentityReason::[A-Za-z]+' | sed 's/IdentityReason:://' | sort -u)"
SOLL="$(sed -n '/pub const IDENTITY_DEBTS/,/\];/p' "$ADDR" | grep -oE '"[A-Za-z]+"' | tr -d '"' | sort -u)"
if [ -z "$SOLL" ]; then
    nok "die Ratsche \`IDENTITY_DEBTS\` fehlt oder ist leer -- die Schuldmenge waere unbeschraenkt."
elif [ "$IST" != "$SOLL" ]; then
    nok "die Schuld-MENGE weicht ab -- eingetragen [$(tr '\n' ' ' <<<"$SOLL")], im Code [$(tr '\n' ' ' <<<"$IST")]. Eine Ratsche ueber einer ZAHL haette einen Austausch durchgelassen."
else
    ok "die Schuld-Menge stimmt ueberein: $(tr '\n' ' ' <<<"$IST")($(echo "$IST" | wc -w) von $(echo "$VARIANTEN" | wc -w))"
fi

# -- 3. Rufen nur die Engstellen die identisch abbildenden HAL-Funktionen? ------------------------
#
# **Die Funktionsliste kommt aus der HAL**, nicht von Hand: `vspace_map_dma` hat in der ersten
# Fassung genau deshalb gefehlt. Ausgenommen sind die beiden, die VA und PA GETRENNT nehmen
# (`_page_at`, `_user_window`) -- sie sind das Gegenteil einer Identitaetsannahme.
HAL="crates/sel4lake-hal/src/x86_64/mmu.rs"
FUNKTIONEN="$(grep -oE '^pub fn (vspace_map[a-z_]*|vspace_unmap[a-z_]*|map_device[a-z_]*)\(' "$HAL" \
              | sed 's/^pub fn //; s/($//; s/(//' \
              | grep -vE '^(vspace_map_page_at|vspace_map_user_window)$' | sort -u)"
[ -n "$FUNKTIONEN" ] || { echo "FEHLER: keine HAL-Abbildungsfunktionen gefunden." >&2; exit 2; }
# Die Engstellen: Funktionen des Kernels, in denen ein `Va::identity(..)` unmittelbar dabeisteht.
ENGSTELLEN="vspace_map_masked vspace_unmap map_region_into_thread"
pruefe_aufrufer() {
    local wurzel="$1" f datei zeile
    ausserhalb=()
    for f in $FUNKTIONEN; do
        while IFS= read -r zeile; do
            [ -n "$zeile" ] || continue
            datei="$(cut -d: -f1 <<<"$zeile")"
            nr="$(cut -d: -f2 <<<"$zeile")"
            # In welcher Funktion steht der Aufruf? Die naechste `fn ` darueber.
            umgebung="$( (cd "$wurzel" && head -n "$nr" "$datei") | grep -oE '^[a-z ]*fn [a-z_0-9]+' | tail -1 | sed 's/.*fn //')"
            case " $ENGSTELLEN " in
                *" $umgebung "*) ;;
                *)
                    # Erlaubt bleibt, was unmittelbar von einem Stellen-Konstruktor (`Va::for_*`)
                    # begleitet wird -- die beiden globalen Kernel-Fenster und der DMA-Abbau
                    # stehen so da.
                    von=$(( nr > 9 ? nr - 9 : 1 ))
                    fenster="$( (cd "$wurzel" && sed -n "${von},${nr}p" "$datei") )"
                    grep -qE "Va::for_[a-z_]+" <<<"$fenster" || ausserhalb+=("$datei:$nr ($f, in fn $umgebung)")
                    ;;
            esac
        done < <(cd "$wurzel" && grep -rn "hal::mmu::$f(" kernel/src --include=*.rs 2>/dev/null | cut -d: -f1,2)
    done
}
pruefe_aufrufer "$ROOT"
if [ "${#ausserhalb[@]}" -gt 0 ]; then
    nok "identisch abbildende HAL-Aufrufe ausserhalb der Engstellen und ohne \`Va::for_*\`:"
    printf '            %s\n' "${ausserhalb[@]}" >&2
else
    ok "jeder Aufruf einer identisch abbildenden HAL-Funktion steht in einer Engstelle oder bei einem benannten Grund ($(echo "$FUNKTIONEN" | wc -w) Funktionen aus der HAL gelesen)"
fi

# -- 4. Falsifikator zu `SyscallMapByCap` --------------------------------------------------------
#
# Der Grund behauptet: **die ABI traegt kein Adressargument**, der Aufrufer nennt eine Cap. Waere
# das falsch, laege die Identitaet wirklich in der Schnittstelle und der Punkt waere unbehebbar.
# Also wird versucht, ihn zu widerlegen: im `SYS_MAP`-Zweig darf die Basis NICHT aus dem Frame
# gelesen werden, sondern muss aus der aufgeloesten Cap stammen.
MK="crates/sel4lake-microkit/src/lib.rs"
if [ ! -f "$MK" ]; then
    nok "$MK fehlt -- der Falsifikator kann nicht laufen."
elif grep -q 'ObjectKind::Memory(r) => (r.base, r.len' "$MK"; then
    # Gegenprobe zur Gegenprobe: kaeme die Basis aus einem Register, stuende hier ein
    # `frame_reg(frame, reg::MSG..)`-Ausdruck in derselben Zuweisung.
    if sed -n '/let (base, len, dma, ro_kind) = match kind {/,/};/p' "$MK" | grep -q "frame_reg"; then
        nok "SyscallMapByCap WIDERLEGT: die Basis kommt (auch) aus einem Frame-Register -- die ABI traegt doch eine Adresse."
    else
        ok "Falsifikator SyscallMapByCap: die Basis stammt aus der aufgeloesten Cap, nicht aus einem Register -- der Grund haelt"
    fi
else
    nok "Falsifikator SyscallMapByCap kann nicht urteilen: der Anker (\`ObjectKind::Memory(r) => (r.base\`) fehlt. Ein Falsifikator, der ins Leere liest, ist keiner."
fi

# -- Selbsttest ----------------------------------------------------------------------------------
if [ "${1:-}" != "--nur-pruefen" ]; then
    echo "-- Selbsttest --"
    W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
    mkdir -p "$W/kernel/src"; cp -r kernel/src/. "$W/kernel/src/"
    cat > "$W/kernel/src/untergeschoben.rs" <<'RS'
// Eingeschleust: eine identisch abbildende Stelle ausserhalb jeder Engstelle, ohne Grund.
fn schmuggel(l2: u64, phys: u64) -> bool {
    hal::mmu::vspace_map_dma(l2, phys, 4096, true, &mut || None)
}
RS
    pruefe_aufrufer "$W"
    if [ "${#ausserhalb[@]}" -gt 0 ]; then
        echo "  ok      : eine untergeschobene Stelle wird erkannt -- auch \`vspace_map_dma\`, das der ersten Fassung fehlte"
    else
        nok "der Selbsttest hat nichts erkannt. Der Waechter kann nicht fehlschlagen, also sagt sein Schweigen nichts."
    fi
    rm -f "$W/kernel/src/untergeschoben.rs"
    pruefe_aufrufer "$W"
    if [ "${#ausserhalb[@]}" -eq 0 ]; then
        echo "  ok      : ohne sie schweigt er wieder -- er schlaegt nicht grundlos an"
    else
        nok "der Waechter schlaegt auch ohne die eingeschleuste Stelle an."
        printf '            %s\n' "${ausserhalb[@]}" >&2
    fi
fi

echo "  $n Pruefung(en)"
if [ "$fehler" -eq 0 ]; then
    echo "== Identitaet: der Typ traegt die Liste, die Gruende sind vollstaendig, einer ist falsifiziert =="
    exit 0
fi
echo "== IDENTITAET: BEFUND ==" >&2
exit 1
