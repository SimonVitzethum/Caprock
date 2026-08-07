#!/usr/bin/env bash
# **Der Zeuge `Parked` und die benannten Spaetbindungen.** (D0-Verschaerfung, 2026-08-07.)
#
# ================================================================================================
# WARUM ES DAS GIBT
# ================================================================================================
#
# D0 war: ein Thread lief, bevor er seine PD hatte. Die erste Behebung trennte `spawn_parked` von
# `admit` und zaehlte mit einem Waechter (`pdbind`), ob eine PD zu spaet gebunden wurde.
#
# Das schliesst die Luecke NICHT, und zwar aus zwei Gruenden:
#
#   * `spaet == 0` zaehlt SPAETE BINDUNGEN, nicht AUSBLEIBENDE ZULASSUNGEN. Eine Aufrufstelle, die
#     `spawn_parked` ruft und `admit` vergisst, ist an dem Zaehler nicht zu sehen -- der Thread
#     laeuft nie, und der Waechter schweigt.
#   * Vier Stellen, an denen Autoritaet NACH der Zulassung vergeben wurde, fand ein Gegenlesen.
#     Auffindbarkeit ist nicht Unmoeglichkeit. Der Typ hat danach eine FUENFTE gefunden, die das
#     Gegenlesen uebersehen hatte (`map_region_into_thread` an drei Geraete-Backends) -- mein Scan
#     suchte nach `map_into_thread` und `install_pd_cap`, diese Variante kam darin nicht vor.
#
# Deshalb `Parked`: ein Zeuge ohne oeffentlichen Weg an die `ThreadId`. Wer sie braucht, ruft
# `admit`, und das verbraucht ihn.
#
# **rustc prueft HERSTELLBARKEIT, nicht NICHT-WEITERGABE** -- derselbe Befund wie beim
# Identitaets-Waechter. Ein `pub fn tid()`, ein `#[derive(Copy)]` oder ein oeffentliches Feld gaebe
# die Bindung wieder frei, und der Typ saehe unveraendert aus. Was der Typ nicht abdeckt, prueft
# dieses Skript.
#
# Aufruf:  tools/zulassung.sh             # pruefen
#          tools/zulassung.sh --selftest  # kann dieses Skript ueberhaupt fehlschlagen?
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

SYS="kernel/src/system.rs"
befunde=0
melde() { echo "  BEFUND: $*"; befunde=$((befunde + 1)); }

pruefe() {
    local sys="$1"

    # ------------------------------------------------------------------------------------------
    # [1] Der Zeuge selbst
    # ------------------------------------------------------------------------------------------
    if ! grep -qE '^pub struct Parked\(ThreadId\);' "$sys"; then
        melde "\`pub struct Parked(ThreadId);\` nicht gefunden -- entweder umbenannt, oder das Feld"
        echo "          ist nicht mehr privat. Ein oeffentliches Feld gibt die \`ThreadId\` frei,"
        echo "          ohne dass \`admit\` je gerufen wird."
    fi
    # Kein Drop -- mit einem Drop liesse sich das Feld in `admit` nicht mehr herausbewegen, und
    # der Typ verlore genau die Eigenschaft, um die es geht. (Ein Drop waere hier also KEIN
    # Fortschritt, sondern das Ende der Konstruktion.)
    if grep -qE 'impl +Drop +for +Parked' "$sys"; then
        melde "\`impl Drop for Parked\` -- damit ist \`Parked\` nicht mehr konsumierbar, und \`admit\`"
        echo "          kann die \`ThreadId\` nicht mehr herausbewegen."
    fi
    # Kein Copy/Clone/Default: ein kopierbarer Zeuge ist keiner.
    if grep -B 3 -E '^pub struct Parked\(' "$sys" | grep -qE '#\[derive\(.*(Copy|Clone|Default)'; then
        melde "\`Parked\` leitet Copy/Clone/Default ab -- ein kopierbarer Zeuge bindet nichts."
    fi
    # `#[must_use]` -- ein fallengelassener Parked ist kein Uebersetzungsfehler, aber er muss
    # wenigstens warnen.
    if ! grep -B 6 -E '^pub struct Parked\(' "$sys" | grep -q 'must_use'; then
        melde "\`Parked\` ohne \`#[must_use]\` -- ein weggeworfener Zeuge waere lautlos, und der"
        echo "          Thread liefe nie."
    fi
    # Kein oeffentlicher Ausgang: in `impl Parked` darf keine `pub fn` stehen.
    local impl_start
    impl_start="$(grep -n '^impl Parked {' "$sys" | head -1 | cut -d: -f1)"
    if [ -n "$impl_start" ]; then
        local impl_ende
        impl_ende="$(awk -v s="$impl_start" 'NR>s && /^}/ {print NR; exit}' "$sys")"
        if sed -n "${impl_start},${impl_ende}p" "$sys" | grep -qE '^\s*pub fn '; then
            melde "\`impl Parked\` hat eine \`pub fn\` -- das ist der oeffentliche Ausgang, gegen den"
            echo "          der ganze Typ gebaut ist."
        fi
    fi

    # ------------------------------------------------------------------------------------------
    # [2] `admit` verbraucht den Zeugen (nimmt ihn per Wert, nicht per Referenz)
    # ------------------------------------------------------------------------------------------
    if ! grep -qE '^pub fn admit\(p: Parked\) -> Option<ThreadId>' "$sys"; then
        melde "\`admit\` nimmt \`Parked\` nicht per Wert oder gibt keine \`ThreadId\` zurueck."
        echo "          Per Referenz waere der Zeuge nach der Zulassung noch da."
    fi

    # ------------------------------------------------------------------------------------------
    # [3] Ankertest der benannten Spaetbindungen
    # ------------------------------------------------------------------------------------------
    #
    # Dieselbe Form wie `IDENTITY_DEBTS`: eine MENGE von Namen, kein Zaehler -- und jeder Name muss
    # eine echte Variante bezeichnen. Ohne Ankertest liest die Liste ins Leere, sobald jemand eine
    # Variante umbenennt, und der Waechter meldet weiter Ordnung.
    local varianten liste
    varianten="$(awk '/^pub enum SpaetbindungsGrund \{/,/^\}/' "$sys" \
                 | grep -oE '^\s{4}[A-Z][A-Za-z0-9]*,' | tr -d ' ,' | sort)"
    liste="$(grep -oE 'pub const ERLAUBTE_SPAETBINDUNGEN: \[&str; [0-9]+\] = \[[^]]*\]' "$sys" \
             | grep -oE '"[A-Za-z0-9]+"' | tr -d '"' | sort)"
    if [ -z "$varianten" ]; then
        melde "keine Varianten von \`SpaetbindungsGrund\` gefunden -- der Ankertest liest ins Leere."
    fi
    if [ -z "$liste" ]; then
        melde "\`ERLAUBTE_SPAETBINDUNGEN\` ist leer oder nicht gefunden."
    fi
    local fehlend
    fehlend="$(comm -13 <(echo "$varianten") <(echo "$liste"))"
    if [ -n "$fehlend" ]; then
        melde "in ERLAUBTE_SPAETBINDUNGEN stehen Namen, die KEINE Variante sind: $(echo "$fehlend" | tr '\n' ' ')"
        echo "          Ein Name ohne Anker ist eine Zeile, die nichts festhaelt."
    fi
    fehlend="$(comm -23 <(echo "$varianten") <(echo "$liste"))"
    if [ -n "$fehlend" ]; then
        melde "es gibt Varianten, die NICHT in ERLAUBTE_SPAETBINDUNGEN stehen: $(echo "$fehlend" | tr '\n' ' ')"
        echo "          Eine Ausnahme ohne Eintrag waechst unsichtbar -- und die vorhandene"
        echo "          legitimiert sie."
    fi
    # Die deklarierte Laenge muss zur Menge passen (sonst faellt ein Name beim Kuerzen raus).
    local n_dekl n_ist
    n_dekl="$(grep -oE 'ERLAUBTE_SPAETBINDUNGEN: \[&str; [0-9]+\]' "$sys" | grep -oE '[0-9]+')"
    n_ist="$(echo "$liste" | grep -c .)"
    if [ -n "$n_dekl" ] && [ "$n_dekl" != "$n_ist" ]; then
        melde "ERLAUBTE_SPAETBINDUNGEN deklariert $n_dekl Eintraege, enthaelt aber $n_ist."
    fi
}

# ================================================================================================
# SELBSTTEST -- in BEIDE Richtungen
# ================================================================================================
if [ "${1:-}" = "--selftest" ]; then
    echo "== Selbsttest: kann dieses Skript ueberhaupt fehlschlagen? =="
    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT
    faelle=0
    gefangen=0
    probe() { # $1 = Name, $2 = sed-Ausdruck, der die Eigenschaft kaputtmacht
        faelle=$((faelle + 1))
        cp "$SYS" "$TMP/s.rs"
        sed -i "$2" "$TMP/s.rs"
        if ! cmp -s "$SYS" "$TMP/s.rs"; then
            befunde=0
            pruefe "$TMP/s.rs" >/dev/null 2>&1
            if [ "$befunde" -gt 0 ]; then
                echo "  gemeldet: $1"
                gefangen=$((gefangen + 1))
            else
                echo "  STILL   : $1  <-- der Waechter sieht das nicht"
            fi
        else
            echo "  UNWIRKSAM: $1 (die Mutation hat nichts geaendert)"
        fi
    }
    probe "oeffentliches Feld"        's/^pub struct Parked(ThreadId);/pub struct Parked(pub ThreadId);/'
    probe "Copy-Ableitung"            's/^pub struct Parked(ThreadId);/#[derive(Clone, Copy)]\npub struct Parked(ThreadId);/'
    probe "oeffentlicher Ausgang"     's/^    fn tid(\&self) -> ThreadId {/    pub fn tid(\&self) -> ThreadId {/'
    probe "admit nimmt per Referenz"  's/^pub fn admit(p: Parked) -> Option<ThreadId>/pub fn admit(p: \&Parked) -> Option<ThreadId>/'
    probe "Name ohne Anker"           's/pub const ERLAUBTE_SPAETBINDUNGEN: \[&str; 1\] = \["CheckpointSubjektNachtraeglich"\]/pub const ERLAUBTE_SPAETBINDUNGEN: [\&str; 1] = ["GibtEsNicht"]/'
    probe "Variante ohne Eintrag"     's/^    CheckpointSubjektNachtraeglich,/    CheckpointSubjektNachtraeglich,\n    ZweiteAusnahme,/'
    # Und die Gegenrichtung: eine harmlose Aenderung darf NICHT melden.
    faelle=$((faelle + 1))
    cp "$SYS" "$TMP/s.rs"
    sed -i 's/^\/\/\/ Einen geparkten Thread zulassen\./\/\/\/ Einen geparkten Thread zulassen (Kommentar geaendert)./' "$TMP/s.rs"
    befunde=0
    pruefe "$TMP/s.rs" >/dev/null 2>&1
    if [ "$befunde" -eq 0 ]; then
        echo "  still   : Kommentaraenderung (richtig -- der Waechter prueft Struktur, nicht Text)"
        gefangen=$((gefangen + 1))
    else
        echo "  FALSCHALARM: Kommentaraenderung gemeldet"
    fi
    echo "  Selbsttest: $gefangen von $faelle"
    [ "$gefangen" -eq "$faelle" ] || { echo "== SELBSTTEST FEHLGESCHLAGEN =="; exit 1; }
    echo "== Selbsttest bestanden =="
    exit 0
fi

echo "== Zulassung: der Zeuge \`Parked\` und die benannten Spaetbindungen =="
pruefe "$SYS"
if [ "$befunde" -gt 0 ]; then
    echo "== ZULASSUNG VERLETZT ($befunde Befund/e) =="
    exit 1
fi
echo "== Zulassung: der Zeuge ist nicht weitergebbar, die Ausnahmen sind benannt und verankert =="
