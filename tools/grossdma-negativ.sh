#!/usr/bin/env bash
# **Kann die Klassifikation der grossen DMA ueberhaupt anschlagen?** (Z26 V2, 2026-08-10)
#
# `crates/caprock-dma/src/gross.rs` behauptet: „ging nicht" wird in vier unterscheidbare Befunde
# zerlegt, die Reihenfolge der Pruefungen ist Teil der Aussage, und die Identitaet der beiden
# Achsen wird abgewiesen. Die Tests dort sehen den BEHOBENEN Zustand.
#
# Der interessante Teil sind hier nicht die einzelnen Absagen, sondern die **Reihenfolge**: dieselbe
# Lage kann zwei wahre Namen haben, und der falsche schickt den Leser in die falsche Richtung
# („der groesste Block ist 8 MiB" klingt nach Fragmentierung, wenn jemand 2 GiB wollte). Genau das
# faellt beim Gegenlesen nicht auf und ist mit einer Mutation in Sekunden zu zeigen.
#
#   M1  Identitaetspruefung ausgehaengt -> der Sicherheitsbefund muss fallen
#   M2  „zu gross" nach „erschoepft"    -> die Reihenfolge muss fallen
#   M3  „erschoepft" nach „Liste voll"  -> die andere Reihenfolge muss fallen
#   M4  Ausrichtung aufgerundet         -> die krumme Laenge muss auffallen
#   M5  Suche ohne Ausrichtung          -> die abgerundete Antwort muss auffallen
#
# Geprueft wird der NAME des fallenden Tests, nicht bloss „rot" -- sonst waere ein Tippfehler im
# sed-Muster schon ein Beleg. Und es wird geprueft, dass die Mutation die Datei ueberhaupt
# aendert: ein Muster, das nach einer Umbenennung nicht mehr passt, waere ein lautlos
# abgeschalteter Negativfall.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/grossdma.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
RUSTC="rustup run nightly rustc"
fail=0

LIB="$ROOT/crates/caprock-dma/src/lib.rs"
QUELLE="$ROOT/crates/caprock-dma/src/gross.rs"
if [ ! -f "$QUELLE" ] || [ ! -f "$LIB" ]; then
    echo "  FEHLT: crates/caprock-dma ist unvollstaendig -- Ziel nicht gelaufen"
    exit 1
fi

echo "== GROSSE DMA: Mutationen (Z26 V2) =="

# Die Crate hat zwei Dateien; mutiert wird `gross.rs`, uebersetzt wird ueber `lib.rs`. Deshalb
# wird der ganze `src`-Baum kopiert und dort die eine Datei ersetzt.
baue_und_lauf() { # $1 = Verzeichnis mit src, $2 = Ausgabename
    $RUSTC --test --edition 2021 "$1/lib.rs" -o "$2" 2>"$2.cerr"
}

mkdir -p "$TMP/orig"
cp "$ROOT/crates/caprock-dma/src/"*.rs "$TMP/orig/"

# --- Positivkontrolle ---------------------------------------------------------------------------
if ! baue_und_lauf "$TMP/orig" "$TMP/bin_orig"; then
    echo "  FEHLER: caprock-dma uebersetzt nicht -- der Negativtest kann nichts aussagen"
    grep -E "^error" -A 6 "$TMP/bin_orig.cerr" | head -20
    exit 1
fi
if "$TMP/bin_orig" >"$TMP/orig.out" 2>&1; then
    echo "  PASS  Positivkontrolle -- unveraendert laufen alle Tests durch"
    grep -E "^test result:" "$TMP/orig.out" | sed 's/^/        /'
else
    echo "  FEHLER Positivkontrolle -- caprock-dma ist schon unmutiert rot:"
    grep -E "^test .* FAILED|^test result:" "$TMP/orig.out" | head -10
    exit 1
fi

mutation() { # $1 = Kurzname, $2 = erwarteter Test, $3 = Text, $4.. = sed
    local name="$1" erwartet="$2" text="$3"; shift 3
    local d="$TMP/mut_$name"
    mkdir -p "$d"
    cp "$ROOT/crates/caprock-dma/src/"*.rs "$d/"
    sed "$@" "$QUELLE" > "$d/gross.rs"
    if cmp -s "$QUELLE" "$d/gross.rs"; then
        echo "  FEHLER $name -- die Mutation aendert die Datei NICHT (Muster passt nicht mehr)."
        echo "         Damit ist dieser Negativfall abgeschaltet, nicht bestanden."
        fail=1; return
    fi
    if ! baue_und_lauf "$d" "$TMP/bin_$name"; then
        echo "  FEHLER $name -- mutiert uebersetzt nicht; der Fehlschlag saegt am falschen Ast:"
        grep -E "^error" -A 4 "$TMP/bin_$name.cerr" | head -10
        fail=1; return
    fi
    if "$TMP/bin_$name" >"$TMP/$name.out" 2>&1; then
        echo "  FEHLER $name -- $text: die Suite bleibt GRUEN. '$erwartet' sieht den Fehler nicht."
        fail=1; return
    fi
    if grep -qE "^test (gross::)?tests::${erwartet} \.\.\. FAILED" "$TMP/$name.out"; then
        local n; n="$(grep -cE '\.\.\. FAILED' "$TMP/$name.out")"
        echo "  PASS  $name -- $text: '$erwartet' faellt (insgesamt $n Test(s))"
    else
        echo "  FEHLER $name -- die Suite faellt, aber NICHT ueber '$erwartet':"
        grep -E '\.\.\. FAILED' "$TMP/$name.out" | head -5 | sed 's/^/        /'
        fail=1
    fi
}

# --- M1: die Identitaetspruefung aushaengen ------------------------------------------------------
#
# Durchgelassen liefe der Treiber, solange CPU- und Geraetesicht zufaellig uebereinstimmen -- und
# braeche in dem Augenblick, in dem jemand die Trennung durchsetzt. Genau der Zustand, gegen den
# `DmaRegion::identity` absichtlich entfernt wurde.
mutation identitaet_egal "identitaet_ist_ein_sicherheitsbefund_und_kein_ressourcenproblem" \
    "IOVA == PA wird durchgelassen" \
    -e 's/^    if iova == pa {$/    if false {/'

# --- M2: „zu gross" hinter „erschoepft" ----------------------------------------------------------
#
# Bei einer 2-GiB-Anforderung stuende dann „der groesste Block ist 8 MiB" -- eine Zahl, die nach
# Fragmentierung klingt und den Leser Stunden kostet.
mutation reihenfolge_zu_gross "zu_gross_gewinnt_gegen_alles_andere" \
    "die Zonengrenze wird erst nach der Erschoepfung geprueft" \
    -e 's/^    if len > zone\.groesse() {$/    if false \&\& len > zone.groesse() {/'

# --- M3: „erschoepft" hinter „Liste voll" --------------------------------------------------------
#
# Ist der groesste Block ohnehin zu klein, hilft eine groessere Freiliste NICHT -- `FreilisteVoll`
# schickte den Leser in die falsche Richtung.
mutation reihenfolge_erschoepft "erschoepfung_gewinnt_gegen_volle_freiliste" \
    "die Freilisten-Kapazitaet wird vor der Erschoepfung geprueft" \
    -e 's/^    if lage\.groesster_block < len {$/    if false \&\& lage.groesster_block < len {/'

# --- M4: krumme Laenge aufrunden -----------------------------------------------------------------
#
# Aufgerundet bekaeme der Aufrufer mehr, als er angefordert hat -- und die Buchhaltung beim
# Freigeben stuende auf einer anderen Zahl als die beim Belegen.
mutation krumm_aufgerundet "krumme_laenge_wird_abgewiesen_statt_aufgerundet" \
    "eine krumme Laenge wird stillschweigend angenommen" \
    -e 's/^    if len % SEITE != 0 {$/    if false {/'

# --- M5: die Suche ohne Ausrichtung --------------------------------------------------------------
#
# **Hier stand zuerst eine andere Mutation, und sie hat einen Fehler in MEINEM Code gefunden:**
# der Waechter `if mitte <= lo { break }` liess sich entfernen, ohne dass ein einziger Test fiel.
# Er kann naemlich nie ausloesen -- beide Schranken sind seitenausgerichtet, also ist
# `mitte >= lo + SEITE` immer wahr. Ein Waechter, der nicht ausloesen kann, ist keiner; er ist
# jetzt weg, und die Begruendung steht an der Funktion.
#
# Was die Suche wirklich traegt, ist die AUSRICHTUNG -- sie ist zugleich die
# Terminierungsbedingung. Also wird die mutiert: eine krumme Antwort wuerde als „so viel geht"
# gelesen und beim naechsten Versuch abgewiesen.
mutation suche_ohne_ausrichtung "die_suche_rundet_auf_seiten_ab" \
    "die Suche rundet ihre Antwort nicht mehr auf Seiten" \
    -e 's|^        let mitte = (lo + (hi - lo) / 2) & !(SEITE - 1);$|        let mitte = lo + (hi - lo) / 2;|'

if [ "$fail" = 0 ]; then
    echo "== GROSSDMA-NEGATIV: ALL PASS =="
else
    echo "== GROSSDMA-NEGATIV: FAILURES =="
fi
exit "$fail"
