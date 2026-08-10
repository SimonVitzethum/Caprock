#!/usr/bin/env bash
# **Kann der IRTE-Vergabe-Test ueberhaupt anschlagen?** (Z22 P1, 2026-08-10)
#
# `crates/caprock-hal/src/x86_64/irte.rs` behauptet seit dem 2026-08-10: die Vergabe schreibt `hi`
# vor `lo`, benennt jede Absage, gibt bei Fehlschlag den Tabellenplatz zurueck und laesst keinen
# praesenten Eintrag stehen. Die Tests in der Datei sehen den BEHOBENEN Zustand -- und ein Test,
# der nur den behobenen Zustand sieht, belegt nichts.
#
# In QEMU ist der Fall ausserdem kaum ausloesbar: ein Bit an der falschen Stelle in einer IRTE
# aeussert sich als „das Geraet unterbricht einfach nicht" -- ohne Fault, ohne Meldung. Man
# braeuchte Geraet, Treiber und Glueck. Also wird der Fehler hier wieder eingebaut, einzeln, und
# nachgesehen, ob GENAU der zustaendige Test faellt.
#
#   M1  SVT aus dem Eintrag genommen   -> die Quellpruefung muss fallen (die Sicherheitsaussage)
#   M2  `lo` vor `hi` geschrieben      -> die Reihenfolgezusicherung muss fallen
#   M3  Ruecknahme gibt nichts zurueck -> das Tabellenplatz-Leck muss auffallen (der ECHTE Fehler
#                                         der ersten Fassung, s. `ruecknahme`)
#   M4  MSI ohne Zweierpotenz-Bedingung-> die Formunterscheidung muss fallen
#   M5  Freigabe ohne Besitzpruefung   -> die doppelte Freigabe muss auffallen
#   M6  Einzug trotz Invalidierungs-
#       fehlschlag                     -> die Sperre des Index muss fallen
#   M7  Kodierpruefung uebersprungen   -> der CPU-Ausnahmevektor muss auffallen
#
# **Warum der NAME des Tests geprueft wird und nicht bloss „rot":** eine mutierte Datei faellt auch
# durch einen Uebersetzungsfehler, durch einen anderen Test, durch einen Tippfehler im sed-Muster.
# Wer jeden Fehlschlag als Beleg nimmt, hat einen Pruefer, der gruen ist, sobald irgendetwas kaputt
# ist -- dieselbe Falle wie in `tools/dmar-rmrr-negativ.sh`.
#
# **Und warum geprueft wird, dass die Mutation ueberhaupt greift:** ein sed-Muster, das nach einer
# Umbenennung nicht mehr passt, waere ein lautlos abgeschalteter Negativfall. Greift eine Mutation
# nicht, ist das ein FEHLER.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/irtevgb.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
RUSTC="rustup run nightly rustc"
fail=0

QUELLE="$ROOT/crates/caprock-hal/src/x86_64/irte.rs"
if [ ! -f "$QUELLE" ]; then
    echo "  FEHLT: $QUELLE ist nicht vorhanden -- Ziel nicht gelaufen (kein Uebersetzungsfehler)"
    exit 1
fi

echo "== IRTE-VERGABE: Mutationen (Z22 P1) =="

# --- Positivkontrolle ---------------------------------------------------------------------------
#
# Ohne sie belegen die Mutationen nichts: faellt die Datei schon im Ausgangszustand durch, faellt
# sie auch mutiert, und jede Mutation saehe wie ein Beleg aus.
if ! $RUSTC --test --edition 2021 "$QUELLE" -o "$TMP/orig" 2>"$TMP/orig.err"; then
    echo "  FEHLER: irte.rs uebersetzt nicht -- der Negativtest kann nichts aussagen"
    grep -E "^error" -A 6 "$TMP/orig.err" | head -20
    exit 1
fi
if "$TMP/orig" >"$TMP/orig.out" 2>&1; then
    echo "  PASS  Positivkontrolle -- unveraendert laufen alle Tests durch"
    grep -E "^test result:" "$TMP/orig.out" | sed 's/^/        /'
else
    echo "  FEHLER Positivkontrolle -- irte.rs ist schon unmutiert rot:"
    grep -E "^test .* FAILED|^test result:" "$TMP/orig.out" | head -10
    exit 1
fi

# $1 = Kurzname, $2 = erwartet fallender Test, $3 = Beschreibung, $4.. = sed-Argumente
mutation() {
    local name="$1" erwartet="$2" text="$3"; shift 3
    local datei="$TMP/mut_$name.rs"
    sed "$@" "$QUELLE" > "$datei"
    # (a) Greift die Mutation ueberhaupt?
    if cmp -s "$QUELLE" "$datei"; then
        echo "  FEHLER $name -- die Mutation aendert die Datei NICHT (Muster passt nicht mehr)."
        echo "         Damit ist dieser Negativfall abgeschaltet, nicht bestanden."
        fail=1
        return
    fi
    # (b) Uebersetzt sie noch? Sonst faellt sie am Compiler statt an der Zusicherung.
    if ! $RUSTC --test --edition 2021 "$datei" -o "$TMP/bin_$name" 2>"$TMP/$name.cerr"; then
        echo "  FEHLER $name -- mutiert uebersetzt nicht; der Fehlschlag saegt am falschen Ast:"
        grep -E "^error" -A 4 "$TMP/$name.cerr" | head -10
        fail=1
        return
    fi
    if "$TMP/bin_$name" >"$TMP/$name.out" 2>&1; then
        echo "  FEHLER $name -- $text: die Suite bleibt GRUEN. '$erwartet' sieht den Fehler nicht."
        fail=1
        return
    fi
    # (c) Faellt GENAU der zustaendige Test -- nicht irgendeiner?
    if grep -qE "^test tests::${erwartet} \.\.\. FAILED" "$TMP/$name.out"; then
        local n
        n="$(grep -cE '\.\.\. FAILED' "$TMP/$name.out")"
        echo "  PASS  $name -- $text: '$erwartet' faellt (insgesamt $n Test(s))"
    else
        echo "  FEHLER $name -- die Suite faellt, aber NICHT ueber '$erwartet':"
        grep -E '\.\.\. FAILED' "$TMP/$name.out" | head -5 | sed 's/^/        /'
        fail=1
    fi
}

# --- M1: die Quellpruefung aus dem Eintrag nehmen ------------------------------------------------
#
# Ohne SVT=01 duerfte JEDES Geraet JEDEN Handle benutzen -- und die Treiber-PD schreibt ihre
# MSI-X-Tabelle selbst. Das ist die Sicherheitsaussage der ganzen Datei.
mutation svt_aus "quellpruefung_ist_eingeschaltet" \
    "SVT nicht mehr im Eintrag" \
    -e 's/(SVT_SID << 18)/(0u64 << 18)/'

# --- M2: `lo` vor `hi` schreiben -----------------------------------------------------------------
#
# Der Eintrag wird gueltig, BEVOR die Quellpruefung darinsteht -- fuer dieses Fenster darf jedes
# Geraet den Handle benutzen. Getauscht wird ueber den Hold-Space, damit genau die beiden Zeilen
# ihre Plaetze tauschen und sonst nichts.
mutation lo_vor_hi "reihenfolge_hi_vor_lo_je_eintrag" \
    "der Eintrag wird praesent, bevor die Quellpruefung darinsteht" \
    -e '/z\.schreibe_hi(index, e\.hi);/{h;d}' \
    -e '/z\.schreibe_lo(index, e\.lo);/{G}'

# --- M3: die Ruecknahme gibt den Block nicht zurueck ---------------------------------------------
#
# **Der Fehler, den die erste Fassung dieser Datei wirklich hatte.** Nach ein paar
# fehlgeschlagenen Vergaben waere die Tabelle voll gewesen, ohne dass ein einziges Geraet einen
# Vektor haelt.
mutation ruecknahme_leckt "fehlgeschlagene_vergabe_leckt_keinen_tabellenplatz" \
    "eine fehlgeschlagene Vergabe behaelt ihren Tabellenplatz" \
    -e 's/^    let _ = a\.gib_frei(handle, reserviert);$/    let _ = (handle, reserviert);/'

# --- M4: MSI ohne Zweierpotenz-Bedingung ---------------------------------------------------------
#
# Die Stelle, an der die GERAETEART die Bedingung bestimmt (`MME` kennt nur 1/2/4/8/16/32).
mutation msi_form_egal "msi_verlangt_eine_zweierpotenz_msix_nicht" \
    "MSI nimmt jede Anzahl" \
    -e 's/^            if !w\.anzahl\.is_power_of_two() {$/            if false {/'

# --- M5: Freigabe ohne Besitzpruefung ------------------------------------------------------------
#
# Eine doppelte Freigabe gaebe einen Index frei, den ein anderes Geraet haelt -- dessen Interrupt
# landete danach beim falschen Treiber.
mutation freigabe_blind "doppelte_freigabe_wird_abgewiesen" \
    "Freigabe prueft den Besitz nicht mehr" \
    -e 's/^        if !(start\.\.start + anzahl)\.all(|i| self\.bit(i)) {$/        if false {/'

# --- M6: Einzug trotz fehlgeschlagener Invalidierung ---------------------------------------------
#
# Der Index waere wieder vergebbar, waehrend die Einheit den alten Eintrag noch
# zwischengespeichert hat. Ein verlorener Tabelleneintrag ist das kleinere Uebel.
mutation einzug_ohne_bestaetigung "einziehen_ohne_bestaetigte_invalidierung_sperrt_den_index" \
    "der Index wird trotz unbestaetigter Invalidierung freigegeben" \
    -e 's/^    if !ok {$/    if false {/'

# --- M7: die Kodierpruefung ueberspringen --------------------------------------------------------
#
# **Ein Befund, den erst diese Mutation gezeigt hat.** Erwartet war, dass
# `cpu_ausnahmevektoren_werden_auch_in_der_vergabe_abgewiesen` faellt -- er faellt NICHT. Der
# Einzelvektor ist doppelt bewacht: faellt `pruefe_kodierbar` weg, weist `irte_build` in der
# Schleife den Vektor 14 immer noch ab, und `vergib` nimmt zurueck und benennt. Das ist Tiefe,
# kein Fehler.
#
# Was `pruefe_kodierbar` ALLEIN traegt, ist die Aussage ueber den BLOCK: „vorher pruefen, nicht
# unterwegs". Ohne sie steht ein Block, dessen dritter Eintrag nicht kodierbar ist, mit zwei
# geschriebenen Eintraegen da -- und GENAU das prueft die Zeile unten. Die erste Fassung dieses
# Negativfalls hat also den falschen Zeugen benannt; die Mutation war richtig, die Erwartung
# nicht.
mutation kodierpruefung_weg "ein_block_der_ueber_vektor_255_liefe_wird_ganz_abgewiesen" \
    "der Vektorbereich wird nicht mehr VORHER geprueft (halb geschriebener Block)" \
    -e 's/^    pruefe_kodierbar(w)?;$/    let _ = pruefe_kodierbar(w);/'

if [ "$fail" = 0 ]; then
    echo "== IRTE-VERGABE-NEGATIV: ALL PASS =="
else
    echo "== IRTE-VERGABE-NEGATIV: FAILURES =="
fi
exit "$fail"
