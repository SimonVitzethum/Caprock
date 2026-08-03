#!/usr/bin/env bash
# **Kann der RMRR-Gruppentest ueberhaupt anschlagen?** (E-Rest 2, 2026-08-03)
#
# `crates/sel4lake-hal/src/x86_64/dmar.rs` behauptet seit dem 2026-08-03: eine RMRR schliesst die
# ganze ACS-Gruppe aus, nicht nur die einzelne Funktion. Die Tests in der Datei sehen den
# BEHOBENEN Zustand -- und ein Test, der nur den behobenen Zustand sieht, belegt nichts. Auf q35
# ist der Fall ausserdem unsichtbar (0 RMRRs), die QEMU-Suiten koennen hier also nicht mitreden.
#
# Also wird der Fehler wieder eingebaut, einzeln, und nachgesehen, ob GENAU der zustaendige Test
# faellt. Vier Mutationen:
#
#   M1  Gruppen-Faerbung ausgehaengt  -> der Fehlerfall muss fallen (der Fehler selbst)
#   M2  Faerbung ohne Bedingung       -> die POSITIVKONTROLLE muss fallen (nicht ueberfaerben)
#   M3  audit() blind fuer Code 4     -> der Audit-Test muss fallen (der Pruefer selbst)
#   M4  unaufloesbare RMRR verschluckt-> Code 5 muss fallen
#
# **Warum der Name des Tests geprueft wird und nicht bloss „rot":** eine mutierte Datei faellt auch
# durch einen Uebersetzungsfehler, durch einen anderen Test, durch einen Tippfehler im sed-Muster.
# Wer jeden Fehlschlag als Beleg nimmt, hat einen Pruefer, der gruen ist, sobald irgendetwas kaputt
# ist -- dieselbe Falle wie die erwarteten Fehlercodes in `tools/typestate-negativ.sh`.
#
# **Und warum geprueft wird, dass die Mutation ueberhaupt greift:** beim Descriptor-Typestate war
# die erste Mutation wirkungslos (`#[derive(Copy)]` auf einem unbewohnten Marker ist ein No-op) und
# meldete faelschlich gruen. Ein sed-Muster, das nach einer Umbenennung nicht mehr passt, waere
# genau dasselbe -- lautlos abgeschaltet. Greift eine Mutation nicht, ist das ein FEHLER.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/dmarrmrr.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
RUSTC="rustup run nightly rustc"
fail=0

QUELLE="$ROOT/crates/sel4lake-hal/src/x86_64/dmar.rs"
if [ ! -f "$QUELLE" ]; then
    echo "  FEHLT: $QUELLE ist nicht vorhanden -- Ziel nicht gelaufen (kein Uebersetzungsfehler)"
    exit 1
fi

echo "== RMRR faerbt die ACS-GRUPPE: Mutationen (E-Rest 2) =="

# --- Positivkontrolle: die unveraenderte Datei muss gruen sein -----------------------------------
#
# Ohne sie belegen die Mutationen nichts: faellt die Datei schon im Ausgangszustand durch, faellt
# sie auch mutiert, und jede Mutation saehe wie ein Beleg aus.
if ! $RUSTC --test --edition 2021 "$QUELLE" -o "$TMP/orig" 2>"$TMP/orig.err"; then
    echo "  FEHLER: dmar.rs uebersetzt nicht -- der Negativtest kann nichts aussagen"
    grep -E "^error" -A 6 "$TMP/orig.err" | head -20
    exit 1
fi
if "$TMP/orig" >"$TMP/orig.out" 2>&1; then
    echo "  PASS  Positivkontrolle -- unveraendert laufen alle Tests durch"
    grep -E "^test result:" "$TMP/orig.out" | sed 's/^/        /'
else
    echo "  FEHLER Positivkontrolle -- dmar.rs ist schon unmutiert rot:"
    grep -E "^test .* FAILED|^test result:" "$TMP/orig.out" | head -10
    exit 1
fi

# $1 = Kurzname, $2 = erwartet fallender Test, $3 = sed-Ausdruck, $4 = Beschreibung
mutation() {
    local name="$1" erwartet="$2" ausdruck="$3" text="$4"
    local datei="$TMP/mut_$name.rs"
    sed "$ausdruck" "$QUELLE" > "$datei"
    # (a) Greift die Mutation ueberhaupt? Ein nicht passendes Muster waere ein lautlos
    #     abgeschalteter Negativfall.
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

# --- M1: die Faerbung der Gruppe wieder aushaengen (der Zustand vor dem 2026-08-03) --------------
mutation gruppe_ungefaerbt "rmrr_ohne_acs_faerbt_die_ganze_gruppe" \
    's/let hat_rmrr = (0..n)\.any(/let hat_rmrr = false \&\& (0..n).any(/' \
    "RMRR faerbt wieder nur die Funktion"

# --- M2: zu breit faerben -- jede Gruppe, sobald irgendwo eine RMRR steht ------------------------
#
# Der Fall, den die Positivkontrolle abdeckt: „alles ausschliessen" bestuende M1, waere aber
# genauso falsch -- am Ende haette die Maschine keine zuteilbaren Geraete mehr.
mutation zu_breit "rmrr_mit_acs_faerbt_nur_das_geraet" \
    's/let hat_rmrr = (0..n)\.any(/let hat_rmrr = info.n_rmrr > 0 || (0..n).any(/' \
    "Faerbung greift ueber die Gruppengrenze hinaus"

# --- M3: den Pruefer blenden ---------------------------------------------------------------------
mutation audit_blind "audit_meldet_die_ungefaerbte_gruppe" \
    's/^            return 4;$/            \{\}/' \
    "audit() meldet Code 4 nicht mehr"

# --- M4: die unaufloesbare RMRR verschlucken -----------------------------------------------------
mutation unaufloesbar_still "unaufloesbare_rmrr_wird_gemeldet_statt_verschluckt" \
    's/            g\.rmrr_unresolved += 1;/            \{\}/' \
    "ein RMRR-Scope ohne Ziel wird still uebergangen"

if [ "$fail" = 0 ]; then
    echo "== DMAR-RMRR-NEGATIV: ALL PASS =="
else
    echo "== DMAR-RMRR-NEGATIV: FAILURES =="
fi
exit "$fail"
