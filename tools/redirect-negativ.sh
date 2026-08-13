#!/usr/bin/env bash
# **Kann die Umleitungs-Weiche ueberhaupt anschlagen?** (Z26/A3, 2026-08-10)
#
# `crates/caprock-sched/src/redirect.rs` behauptet drei Dinge:
#
#   * eine bestehende Bindung fuehrt **nie** zum Kernel (fail-closed, keine Rechteausweitung),
#   * eine Handler-Kette schliesst **keinen Kreis** (Z26/Nachtrag 3, BAUPFLICHT),
#   * ein Sidecar-Slot gehoert **genau einem** Gast.
#
# Die Tests in der Datei sehen den GEBAUTEN Zustand -- und ein Test, der nur den gebauten Zustand
# sieht, belegt nichts. Also wird der Fehler wieder eingebaut, EINZELN, und nachgesehen, ob GENAU
# der zustaendige Test faellt. Sieben Mutationen:
#
#   M1  Rueckfall auf den Kernel statt Fault  -> die Rechteausweitung selbst
#   M2  Zyklusgang nur EINEN Schritt tief     -> der laengere Kreis muss fallen
#   M3  Selbstbindung durchgelassen           -> der Sonderfall mit Kettenlaenge 0
#   M4  Schrittschranke weg                   -> der vorbestehende Kreis (Endlosschleife im
#                                                Syscall-Pfad) muss auffallen
#   M5  Slot-Schranke inklusiv (`<=`)         -> der Off-by-one am Fensterende
#   M6  Fenster nur am Slot-ANFANG geprueft   -> der halbe letzte Slot
#   M7  „schon gebunden" nicht geprueft       -> zwei Persoenlichkeiten ueber einem Adressraum
#
# **Warum der Name des Tests geprueft wird und nicht bloss „rot":** eine mutierte Datei faellt auch
# durch einen Uebersetzungsfehler, durch einen anderen Test, durch einen Tippfehler im sed-Muster.
# Wer jeden Fehlschlag als Beleg nimmt, hat einen Pruefer, der gruen ist, sobald irgendetwas kaputt
# ist -- dieselbe Falle wie in `tools/dmar-rmrr-negativ.sh`.
#
# **Und warum geprueft wird, dass die Mutation ueberhaupt greift:** ein sed-Muster, das nach einer
# Umbenennung nicht mehr passt, waere ein lautlos abgeschalteter Negativfall.
#
# **Zusaetzlich, und es ist keine Mutation, sondern ein QUELLTEXT-WAECHTER (Q1):** der Grund
# `BlockReasons::HANDLER` darf an GENAU EINER Stelle entfernt werden. Diese Aussage laesst sich
# nicht mutieren -- sie ist eine Aussage ueber den ganzen Baum, nicht ueber eine Datei. Sie steht
# unten, mit Sprechprobe.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/redirectneg.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
RUSTC="rustup run nightly rustc"
fail=0

QUELLE="$ROOT/crates/caprock-sched/src/redirect.rs"
if [ ! -f "$QUELLE" ]; then
    echo "  FEHLT: $QUELLE ist nicht vorhanden -- Ziel nicht gelaufen (kein Uebersetzungsfehler)"
    exit 1
fi

echo "== Umgeleitete Syscalls: Mutationen (Z26/A3) =="

# --- Positivkontrolle ---------------------------------------------------------------------------
#
# Ohne sie belegen die Mutationen nichts: faellt die Datei schon im Ausgangszustand durch, faellt
# sie auch mutiert, und jede Mutation saehe wie ein Beleg aus.
if ! $RUSTC --test --edition 2021 "$QUELLE" -o "$TMP/orig" 2>"$TMP/orig.err"; then
    echo "  FEHLER: redirect.rs uebersetzt nicht -- der Negativtest kann nichts aussagen"
    grep -E "^error" -A 6 "$TMP/orig.err" | head -20
    exit 1
fi
if "$TMP/orig" >"$TMP/orig.out" 2>&1; then
    echo "  PASS  Positivkontrolle -- unveraendert laufen alle Tests durch"
    grep -E "^test result:" "$TMP/orig.out" | sed 's/^/        /'
else
    echo "  FEHLER Positivkontrolle -- redirect.rs ist schon unmutiert rot:"
    grep -E "^test .* FAILED|^test result:" "$TMP/orig.out" | head -10
    exit 1
fi

# $1 = Kurzname, $2 = erwartet fallender Test, $3 = Python-Ersetzung, $4 = Beschreibung,
# $5 = wie viele Tests fallen DUERFEN (Vorgabe 1).
#
# **Der fuenfte Parameter ist der Punkt.** Eine Mutation, die zwei Aussagen kippt, misst die
# Reihenfolge der Pruefungen und nicht die Eigenschaft (der Befund aus der ersten D9-Gegenprobe).
# Wo die Kopplung STRUKTURELL ist, steht sie hier als Zahl -- also als erklaerte Erwartung, die
# selbst fehlschlagen kann, statt als Hinweiszeile, die man ueberliest.
mutation() {
    local name="$1" erwartet="$2" ersetzung="$3" text="$4" erlaubt="${5:-1}"
    local datei="$TMP/mut_$name.rs"
    ALT="$QUELLE" NEU="$datei" ERS="$ersetzung" python3 - <<'PY'
import os
s = open(os.environ['ALT'], encoding='utf-8').read()
exec(os.environ['ERS'])
open(os.environ['NEU'], 'w', encoding='utf-8').write(s)
PY
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
    # (c) Faellt GENAU der zustaendige Test?
    if grep -qE "^test tests::${erwartet} \.\.\. FAILED" "$TMP/$name.out"; then
        local n
        n="$(grep -cE '\.\.\. FAILED' "$TMP/$name.out")"
        if [ "$n" -eq "$erlaubt" ]; then
            echo "  PASS  $name -- $text: '$erwartet' faellt ($n von $erlaubt erwarteten)"
            [ "$n" -gt 1 ] && grep -E '\.\.\. FAILED' "$TMP/$name.out" | sed 's/^/          /'
        else
            # **Nicht isoliert und nicht erklaert.** Eine Mutation, die mehr kippt als angesagt,
            # belegt nicht mehr, WELCHE Eigenschaft sie widerlegt hat.
            echo "  FEHLER $name -- $n Tests gefallen, erwartet waren $erlaubt:"
            grep -E '\.\.\. FAILED' "$TMP/$name.out" | sed 's/^/          /'
            fail=1
        fi
    else
        echo "  FEHLER $name -- rot, aber NICHT an '$erwartet'. Das belegt nichts:"
        grep -E '\.\.\. FAILED' "$TMP/$name.out" | head -5 | sed 's/^/          /'
        fail=1
    fi
}

# --- M1: der Rueckfall auf die native ABI -------------------------------------------------------
#
# DIE Rechteausweitung. Sie sieht aus wie Robustheit („dann halt wie frueher") und macht aus dem
# Entzug einer Cap eine Befoerderung.
mutation rueckfall weggefallener_handler_faultet_und_faellt_nicht_zurueck \
  's = s.replace("        Some(_) if !handler_lebt => Weiche::Fault(ERR_HANDLER_GONE),\n        Some(b) => Weiche::Handler {\n            ep: b.sys_ep,", "        Some(_) if !handler_lebt => Weiche::Kernel,\n        Some(b) => Weiche::Handler {\n            ep: b.sys_ep,", 1)' \
  "Syscall-Weiche faellt bei totem Handler auf den Kernel zurueck" 2
# **Warum hier ZWEI Tests fallen und das keine fehlende Isolation ist:** die beiden Aussagen sind
# dieselbe, in zwei Koernungen. `weggefallener_handler_...` nennt den EINEN Fall (Handler tot ->
# Fault), `bindung_vorhanden_heisst_niemals_kernel` ist seine Verallgemeinerung ueber das ganze
# Kreuzprodukt. Die zweite ENTHAELT die erste; eine Mutation, die die erste kippt, MUSS die zweite
# kippen, sonst waere die Verallgemeinerung falsch. Das sind nicht zwei Fehler, sondern ein
# Konjunkt mit zwei Beobachtern -- dieselbe Form wie bei der `fp`-Gegenprobe aus Z25.

# --- M2: der Zyklusgang nur EINEN Schritt tief --------------------------------------------------
#
# Die naheliegende Fassung: „ist der Handler direkt mein Gast?". Sie faengt A<->B und laesst
# A->B->C->A durch -- und dann haengt die ganze Kette beim ersten Syscall, ohne Fehlerbild.
# Die erste Fassung dieser Mutation hob die SCHRITTSCHRANKE auf und kippte damit drei Tests --
# sie mass, dass der Gang ueberhaupt laeuft, nicht dass er weit genug laeuft. Diese Fassung
# beschraenkt nur den GAST-TREFFER auf den ersten Schritt: der Zweierzyklus wird weiter gefangen,
# der Dreierzyklus nicht mehr. Genau ein Konjunkt.
mutation kurzer_gang laengerer_zyklus_wird_abgewiesen \
  's = s.replace("        if k == gast_pd {\n            return BindUrteil::Zyklus;\n        }", "        if k == gast_pd && schritte <= 1 {\n            return BindUrteil::Zyklus;\n        }", 1)' \
  "Zyklusgang sieht nur den direkten Partner, nicht die Kette"

# --- M3: Selbstbindung durchgelassen ------------------------------------------------------------
mutation selbst selbstbindung_wird_abgewiesen \
  's = s.replace("    if gast_pd == handler_pd {\n        return BindUrteil::SelbstBindung;\n    }", "    if false {\n        return BindUrteil::SelbstBindung;\n    }", 1)' \
  "eine PD darf ihr eigener Handler sein"

# --- M4: die Schrittschranke weg ----------------------------------------------------------------
#
# Ohne sie laeuft der Gang in einen VORBESTEHENDEN Kreis und kehrt nie zurueck -- eine
# Endlosschleife im Syscall-Pfad, mit gehaltenem CAPS-Lock. Der Test faellt hier durch ein
# anderes Ergebnis (`Ok` statt `KetteZuLang`) und nicht durch Haengen: die Schranke wird auf einen
# sehr grossen Wert gesetzt statt entfernt, damit der Negativtest terminiert.
mutation keine_schranke vorbestehender_zyklus_ist_unterscheidbar \
  's = s.replace("        if schritte > n_knoten {\n            return BindUrteil::KetteZuLang;\n        }", "        if schritte > n_knoten * 1000 {\n            return BindUrteil::Ok;\n        }", 1)' \
  "Schrittschranke erkennt den vorbestehenden Kreis nicht"

# --- M5: die Slot-Schranke inklusiv -------------------------------------------------------------
#
# Ein Slot daneben heisst hier nicht „Absturz", sondern: EIN GAST SCHREIBT IN DEN FRAME EINES
# ANDEREN. Die stummste Fehlerform, die dieses Primitiv haben kann.
mutation slot_off_by_one slot_schranke_ist_exklusiv \
  's = s.replace("pub const fn slot_gueltig(slot: u16, slots: u16) -> bool {\n    slot < slots\n}", "pub const fn slot_gueltig(slot: u16, slots: u16) -> bool {\n    slot <= slots\n}", 1)' \
  "Slot-Schranke laesst einen Slot hinter dem Fenster zu"

# --- M6: das Fenster nur am Slot-ANFANG geprueft ------------------------------------------------
mutation fenster_anfang fenster_deckt_den_letzten_slot_ganz \
  's = s.replace("    (slots as u64) * (SLOT_BYTES as u64) <= len", "    (slots.saturating_sub(1) as u64) * (SLOT_BYTES as u64) <= len", 1)' \
  "Fensterpruefung sieht nur den Anfang des letzten Slots"

# --- M7: „schon gebunden" nicht geprueft --------------------------------------------------------
#
# Zwei Persoenlichkeiten ueber EINEM Adressraum -- zwei Wahrheiten ueber denselben Speicher. Und
# im Graphen zwei ausgehende Kanten von einem Knoten, womit der billige Gang unmoeglich wird.
mutation zweiter_handler zweiter_handler_fuer_dieselbe_pd_wird_abgewiesen \
  's = s.replace("    if let Some(vorhanden) = kante(gast_pd) {\n        if vorhanden != handler_pd {\n            return BindUrteil::FremderHandler;\n        }\n    }", "", 1)' \
  "eine zweite, fremde Handler-Bindung wird durchgelassen"

# ================================================================================================
# DIE NUTZLAST (2026-08-13): das SLOT-FORMAT
# ================================================================================================
#
# Bis heute trug `redirect.rs` nur die Weiche, das Zyklusverbot und die Slot-Arithmetik. Mit der
# Nutzlast kommt die **Vertragsflaeche** dazu: ein versionierter Kopf, eine Architekturkennung,
# reservierte Felder und -- die schaerfste Aussage -- welche Frame-Woerter aus dem Sidecar
# ZURUECK duerfen. Sie sind hier, weil eine Mutation billiger ist als ein QEMU-Lauf und weil sie
# alle mit Literalen ausloesbar sind.

# --- M8: der Rueckschreibe-Filter um EINS zu weit -----------------------------------------------
#
# **Die schaerfste Mutation der Reihe.** Die Quelle ist Speicher einer Persoenlichkeits-PD; was
# von dort zurueckdarf, entscheidet, ob eine Umleitung eine Umleitung bleibt oder eine
# BEFOERDERUNG wird. Ein Wort zu weit ist auf x86 noch `vector` (harmlos aussehend) -- drei weiter
# waere `cs`, und dann setzt ein Handler seinen Gast nach Ring 0. Die Mutation nimmt bewusst den
# harmlos aussehenden Schritt: eine Schranke, die um eins verrutscht, faellt beim Gegenlesen nicht
# auf.
mutation filter_zu_weit nur_die_allzweckregister_kommen_zurueck \
  's = s.replace("pub const fn uebernehmbar(i: usize, n_gpr: usize) -> bool {\n    i < n_gpr\n}", "pub const fn uebernehmbar(i: usize, n_gpr: usize) -> bool {\n    i < n_gpr + 1\n}", 1)' \
  "der Rueckschreibe-Filter laesst ein Wort hinter den GPR zu"

# --- M9: die Versionspruefung weg ---------------------------------------------------------------
#
# Das Sidecar ist die Vertragsflaeche zwischen Kernel und Persoenlichkeit, und die beiden sind per
# Entwurf nicht dieselbe Fassung. Ohne Versionspruefung liest der Kernel die Woerter einer fremden
# Fassung als seine eigenen GPR -- und schreibt sie als REGISTERINHALT in einen laufenden Thread.
# Dieselbe Regel wie bei der Zustandsuebergabe (A-4.3): abweisen, nicht auslegen.
#
# **Warum hier ZWEI Tests fallen und das keine fehlende Isolation ist:** die beiden sind dieselbe
# Aussage in zwei Koernungen -- „eine fremde Version wird benannt abgewiesen" und „die Version
# wird VOR allem anderen geprueft". Ohne die Pruefung gibt es weder das eine noch das andere.
# Dieselbe Form wie bei M1.
mutation keine_version fremde_version_wird_benannt_abgewiesen \
  's = s.replace("    if slot[KOPF_VERSION] != FORMAT_VERSION {\n        return KopfUrteil::FremdeVersion(slot[KOPF_VERSION]);\n    }", "", 1)' \
  "unbekannte Formatversion wird ausgelegt statt abgewiesen" 2

# --- M10: die Gruppenkennung wird ignoriert -----------------------------------------------------
#
# Das Feld ist reserviert und der Mechanismus nicht gebaut. **Nullen heissen „keine Angabe", nicht
# „passt auf alles"** -- dieselbe Regel wie bei den reservierten Manifest-Bytes. Ein Slot, der eine
# Gruppe verlangt, verlangt etwas, das niemand einloest; ihn durchzulassen hiesse, ungeprueft
# Werte anzusammeln, und der Tag der Einloesung waere der Tag, an dem sie alle falsch sind (Z11c).
mutation gruppe_ignoriert eine_gruppe_wird_abgewiesen_und_nicht_ignoriert \
  's = s.replace("    if slot[KOPF_GRUPPE] != 0 {\n        return KopfUrteil::Reserviert;\n    }", "", 1)' \
  "eine gesetzte Gruppenkennung wird still ignoriert"

# --- M11: die reservierten Woerter werden nicht geprueft ----------------------------------------
mutation reserviert_ungeprueft reservierte_woerter_muessen_null_sein \
  's = s.replace("    while i < KOPF_RESERVIERT_N {", "    while i < 0 {", 1)' \
  "reservierte Kopfwoerter werden nicht auf null geprueft"

# ------------------------------------------------------------------------------------------------
# Q1: DER QUELLTEXT-WAECHTER -- „nur EIN Wecker fuer den Handler-Grund"
# ------------------------------------------------------------------------------------------------
#
# Die tragende Aussage von Z26/Nachtrag 3 ist nicht in `redirect.rs` formulierbar: sie ist eine
# Aussage ueber den GANZEN Baum -- naemlich, dass `BlockReasons::HANDLER` an genau einer Stelle
# entfernt wird. Waere es an zweien, koennte ein fremder Wecker den Gast mit halbem Syscall
# loslaufen lassen, und zwar so, wie es die Park-Naht viermal getan hat.
#
# Ein `grep` ist ein schwacher Pruefer, und das gehoert hierhin: er sieht den TEXT, nicht die
# Wirkung. Aber er sieht die Klasse Fehler, gegen die er gebaut ist -- eine zweite Fundstelle --,
# und die Wirkung misst die Pruefzeile `handler` in QEMU.
echo "  -- Q1: der Handler-Grund hat genau EINEN Wecker --"
Q1_MUSTER='reasons\.remove\(BlockReasons::HANDLER\)'
Q1_ERLAUBT='crates/caprock-sched/src/lib.rs'
q1_treffer="$(grep -rnE --include=*.rs "$Q1_MUSTER" "$ROOT/crates" "$ROOT/kernel" 2>/dev/null)"
q1_anzahl="$(printf '%s' "$q1_treffer" | grep -c . )"
# **Sprechprobe zuerst.** Ein Waechter, der 0 Treffer findet, weil sich der Name geaendert hat,
# meldete „in Ordnung" -- Schweigen als Erfolg, im eigenen Werkzeug (Fallenliste, sammellauf.sh).
if [ "$q1_anzahl" -eq 0 ]; then
    echo "  FEHLER Q1 -- KEIN Treffer fuer '$Q1_MUSTER'. Der Waechter ist stumm, nicht zufrieden."
    fail=1
elif [ "$q1_anzahl" -ne 1 ]; then
    echo "  FEHLER Q1 -- $q1_anzahl Stellen entfernen den Handler-Grund; es darf genau EINE geben:"
    printf '%s\n' "$q1_treffer" | sed 's/^/          /'
    fail=1
elif ! printf '%s' "$q1_treffer" | grep -q "$Q1_ERLAUBT"; then
    echo "  FEHLER Q1 -- die eine Stelle liegt nicht in $Q1_ERLAUBT:"
    printf '%s\n' "$q1_treffer" | sed 's/^/          /'
    fail=1
else
    fn="$(printf '%s' "$q1_treffer" | sed 's/:.*//')"
    echo "  PASS  Q1 -- genau eine Stelle, in ${fn#"$ROOT/"} (handler_reply)"
fi
# ... und die Gegenprobe zum Waechter selbst: mit einer eingebauten zweiten Stelle MUSS er
# anschlagen. Ohne diese Zeile waere „genau eine" eine Behauptung ueber ein grep, das vielleicht
# gar nicht sucht.
mkdir -p "$TMP/q1/crates" "$TMP/q1/kernel"
cp -r "$ROOT/crates/caprock-sched" "$TMP/q1/crates/" 2>/dev/null
printf 'fn x(){ self.tcbs[s].reasons.remove(BlockReasons::HANDLER); }\n' > "$TMP/q1/kernel/zweite.rs"
q1_probe="$(grep -rnE --include=*.rs "$Q1_MUSTER" "$TMP/q1/crates" "$TMP/q1/kernel" 2>/dev/null | grep -c .)"
if [ "$q1_probe" -ge 2 ]; then
    echo "  PASS  Q1-Sprechprobe -- mit einer zweiten Stelle findet der Waechter $q1_probe (>=2)"
else
    echo "  FEHLER Q1-Sprechprobe -- der Waechter findet die eingebaute zweite Stelle NICHT ($q1_probe)."
    echo "         Damit sagt sein 'genau eine' nichts."
    fail=1
fi

if [ "$fail" = 0 ]; then
    echo "== REDIRECT-NEGATIV: ALL PASS =="
else
    echo "== REDIRECT-NEGATIV: FAILURES =="
fi
exit "$fail"
