#!/usr/bin/env bash
# **Stufe B: die rote Haelfte der `irqmsi`-Zeile.** Gruene Konjunkte sind die eine Haelfte des
# Beweises; die andere ist, dass die Zeile ueberhaupt rot werden KANN — und zwar an der Stelle, die
# man gemeint hat.
#
# Geprueft wird dreierlei:
#   1. die Mutation hat GEGRIFFEN (ein sed-Muster, das nach einer Umbenennung nicht mehr passt,
#      waere ein lautlos abgeschalteter Negativfall),
#   2. das GEMEINTE Konjunkt ist gefallen — und nicht irgendeines,
#   3. die uebrigen sind gruen geblieben. Eine Mutation, die zwei Dinge zugleich kaputtmacht,
#      beweist ueber keines etwas (erste D9-Gegenprobe).
#
# **Die Suite ist die LADE-Suite**, nicht die Hauptsuite: nur dort gibt es zwei Treiber-PDs mit
# zugeteilten Geraeten, also ueberhaupt eine Interrupt-Vergabe. Eine Gegenprobe gegen die falsche
# Suite bewiese dieselbe Klasse wie `system::alloc` auf `KernelOnly` — gruen bleiben und die
# Aussage verfehlen.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/irqmsineg.XXXXXX")"
fail=0

# **Sicherung ohne `git stash`.** `refs/stash` ist ueber alle Arbeitsbaeume geteilt; ein Werkzeug,
# das ihn benutzt, kann die Arbeit eines parallel laufenden Agenten mitnehmen.
DATEIEN=(
    "kernel/src/system.rs"
    "kernel/src/loader.rs"
    "crates/caprock-hal/src/x86_64/irte.rs"
    "crates/caprock-hal/src/x86_64/pcie.rs"
    "crates/caprock-microkit/src/lib.rs"
    "programs/hardware/virtio-blk/src/main.rs"
)
for f in "${DATEIEN[@]}"; do mkdir -p "$TMP/$(dirname "$f")"; cp "$f" "$TMP/$f"; done
restore() { for f in "${DATEIEN[@]}"; do cp "$TMP/$f" "$ROOT/$f"; done; }
trap 'restore; rm -rf "$TMP"' EXIT

lauf() { timeout 1500 ./test-qemu-x86-load.sh > "$1" 2>&1; }
ZEILE="irqmsi  :"
# **Abgelesen wird NUR auf der eigenen Berichtszeile** -- und das hat eine Runde gekostet.
#
# Die erste Fassung war `grep -oE "name=(true|false)" | head -1`, also dateiweit. `irtevgb`
# (der IRTE-Selbsttest) hat ein gleichnamiges Konjunkt `getrennt=true`, steht weiter oben, und der
# Extraktor nahm dessen Wert: M3 meldete `getrennt=true`, WAEHREND die Zeile korrekt
# `getrennt=false` sagte. Zwei Laeufe lang sah es aus wie ein Fehler in der Mutation.
#
# Das ist die Klasse aus todo D18 im Ableseweg statt im Pruefer: *„trifft genau einmal" schliesst
# nicht aus, dass der Treffer an der falschen Stelle sitzt.*
konjunkt() { grep -E "^${ZEILE}" "$1" | grep -oE "$2=(true|false)" | head -1 | cut -d= -f2; }

pruefe() { # $1 Name  $2 Log  $3 Konjunkt  $4 erwartet
    local got; got="$(konjunkt "$2" "$3")"
    if [ -z "$got" ]; then
        echo "  FAIL: $1 -- Konjunkt '$3' kommt im Protokoll gar nicht vor (die Mutation hat den"
        echo "        Lauf anders zerstoert als gemeint -- z. B. den BAU, und dann misst der"
        echo "        Negativfall den Bau)"; fail=1
    elif [ "$got" != "$4" ]; then
        echo "  FAIL: $1 -- '$3' ist '$got', erwartet '$4'"; fail=1
    else
        echo "  PASS: $1 -- '$3=$got', wie gemeint"
    fi
}
gruen() { pruefe "$1" "$2" "$3" true; }

# **Die Trefferzahl gehoert zur Mutation.** Ein `sed` mit zwei Treffern sieht in der Ergebniszeile
# aus wie eines mit einem — und blendet dann womoeglich die Erhebung UND ihren Pruefer (M7 bei
# `ckptcut`, gefunden erst durch `grep -c`).
mutiere() { # $1 Datei  $2 sed-Ausdruck  $3 Name  $4 erwartete Trefferzahl
    local vorher nachher treffer
    vorher="$(md5sum "$1" | cut -d' ' -f1)"
    treffer="$(sed -n "$2p" "$1" 2>/dev/null | wc -l)"
    sed -i "$2" "$1"
    nachher="$(md5sum "$1" | cut -d' ' -f1)"
    if [ "$vorher" = "$nachher" ]; then
        echo "  FAIL: $3 -- das sed-Muster hat NICHTS getroffen (lautlos abgeschalteter Negativfall)"
        fail=1; return 1
    fi
    if [ -n "${4:-}" ] && [ "$treffer" != "$4" ]; then
        echo "  FAIL: $3 -- das Muster traf $treffer Stelle(n), erwartet $4"
        fail=1; return 1
    fi
    return 0
}

echo "== Stufe B: Gegenproben zur irqmsi-Zeile (B1+B2+B3) =="

# --- Positivkontrolle ---------------------------------------------------------------------------
# Ohne sie belegen die Mutationen nichts: ist der Ausgangszustand schon rot, ist er es mutiert
# auch, und jede Mutation saehe wie ein Beleg aus. Sie hat schon einmal gefangen, dass ein ZWEITER
# Gegenproben-Lauf dieselben Dateien mutierte.
echo "-- Positivkontrolle (unveraendert) --"
lauf "$TMP/orig.log"
if ! grep -q "^irqmsi  : ALL PASS" "$TMP/orig.log"; then
    echo "  FEHLER: der Ausgangszustand ist schon rot -- die Gegenproben koennen nichts aussagen"
    grep -E "^irqmsi" "$TMP/orig.log" | head -3
    exit 1
fi
echo "  PASS: irqmsi ist vorher gruen"

# --- M1: der IRTE wird gar nicht geschrieben ----------------------------------------------------
#
# **Der Baum von gestern.** Ohne diese Mutation belegt die ganze Zeile nur, dass sie laeuft --
# nicht, dass sie den Zustand VOR B1 von dem danach unterscheidet.
echo "-- M1: keine IRTE-Vergabe (der Zustand vor B1) --"
if mutiere kernel/src/system.rs \
   '/^fn msi_grant(dev: &DriverDevice) -> Option<MsiGrant> {$/,/^    let vector/ s/^    if dev.msix_table == 0 || dev.msix_eintraege == 0 {$/    if true {/' M1 1; then
    lauf "$TMP/m1.log"
    # Kein Vektor mehr -> die Sprechprobe faellt, und zwar SICHTBAR: `mit-vektor=0`.
    if grep -q "^irqmsi  : FAILURES" "$TMP/m1.log" && grep -q "mit-vektor=0" "$TMP/m1.log"; then
        echo "  PASS: M1 -- 'irqmsi: FAILURES' mit 'mit-vektor=0', wie gemeint"
    else
        echo "  FAIL: M1 -- erwartet 'irqmsi  : FAILURES' mit 'mit-vektor=0'"
        grep -E "^irqmsi" "$TMP/m1.log" | head -3; fail=1
    fi
    # **Die Isolationsaussage, und sie ist hier die interessantere:** ohne Vektor gibt es auch
    # keine Cap -- `angeboten=>da` MUSS fallen, denn die Geraete bieten MSI-X weiter an. Faellt es
    # nicht, misst dieses Konjunkt nicht, was es behauptet.
    pruefe M1 "$TMP/m1.log" "angeboten-dann-da" false
else
    echo "  FAIL: M1 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M2: SVT aus -------------------------------------------------------------------------------
#
# Der Eintrag stellt weiter zu, die positiven Konjunkte bleiben also gruen — **und genau deshalb
# braucht es diese Gegenprobe**: ohne sie waere `svt` ein Feld, dessen Wirkung niemand geprueft
# hat. Ohne SVT=01 naehme der Eintrag eine MSI von JEDEM Geraet an.
echo "-- M2: SVT_SID aus (der Eintrag prueft die Quelle nicht mehr) --"
if mutiere crates/caprock-hal/src/x86_64/irte.rs \
   's/^    let hi = (sid as u64) | (SQ_ALLE_16 << 16) | (SVT_SID << 18);$/    let hi = (sid as u64) | (SQ_ALLE_16 << 16);/' M2 1; then
    lauf "$TMP/m2.log"
    pruefe M2 "$TMP/m2.log" "svt" false
    # **Die Trennschaerfe:** `praesent` und `vektor-passt` bleiben gruen. Ein Eintrag ohne
    # Quellpruefung ist ein FUNKTIONIERENDER Eintrag -- das ist der Grund, warum die Luecke ohne
    # eigenes Konjunkt unsichtbar waere.
    gruen M2 "$TMP/m2.log" "praesent"
    gruen M2 "$TMP/m2.log" "vektor-passt"
else
    echo "  FAIL: M2 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M3: der Cap-Riegel faellt (G3 wiederhergestellt) -------------------------------------------
#
# Die Mutation, die zeigt, dass B3 ein GATTER ist und keine Bequemlichkeit: ohne die Typpruefung
# bindet eine PD, die **keine** `Irq`-Cap haelt. Die Zustellung bliebe gruen — nur die Autoritaet
# waere weg.
echo "-- M3: BIND_IRQ prueft den Cap-Typ nicht (G3 zurueck) --"
# Die gemeinte Lage ist Fall (b) des Negativfalls in `init`: eine GEHALTENE Cap vom falschen Typ.
# Fall (a) (leerer Slot) faellt weiter in der generischen Aufloesung -- er kann diese Mutation
# nicht sehen, und deshalb steht er nicht allein im Negativfall."
if mutiere crates/caprock-microkit/src/lib.rs \
   '/^        sys::BIND_IRQ => {$/,/^            if !rights.contains(Rights::READ) {$/ s/^            let ObjectKind::Irq { intid } = kind else {$/            let intid = 0x60u32; if false {/' M3 1; then
    lauf "$TMP/m3.log"
    pruefe M3 "$TMP/m3.log" "bind-ohne-cap-abgewiesen" false
    # Die uebrigen Aussagen haengen nicht am Riegel und muessen stehen bleiben.
    gruen M3 "$TMP/m3.log" "svt"
    gruen M3 "$TMP/m3.log" "cap-in-slot7"
else
    echo "  FAIL: M3 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M4: die Cap wird gepraegt, aber nicht ausgeliefert ------------------------------------------
#
# **Der `SYS_SPAWN`-Zustand als Mutation**: etwas ist gebaut, vollstaendig, und hat keinen Halter.
# Ohne `cap-in-slot7` waere das von einer ausgelieferten Cap nicht zu unterscheiden.
#
# **Mutiert wird der LADER, nicht die Praegung** -- und der erste Anlauf hat genau daran gezeigt,
# wozu die Existenzpruefung im `pruefe` da ist: eine Wachbedingung an den `Some`-Arm von
# `msi_grant` zu haengen macht das `match` nicht-erschoepfend (E0004), der Bau bricht, und der
# Negativfall misst dann den BAU. Er meldete `Konjunkt kommt gar nicht vor` -- ohne diese Pruefung
# waeren `false` und `nicht vorhanden` in einem grep-Vergleich dasselbe gewesen.
echo "-- M4: die Irq-Cap erreicht die Treiber-PD nicht --"
if mutiere kernel/src/loader.rs \
   's/^                    out\[7\] = Some((7, irq));$/                    let _ = irq;/' M4 1; then
    lauf "$TMP/m4.log"
    pruefe M4 "$TMP/m4.log" "cap-in-slot7" false
    # Der IRTE steht weiter -- die Vergabe ist von der AUSLIEFERUNG unabhaengig, und genau das
    # macht die beiden zu zwei Konjunkten statt einem.
    gruen M4 "$TMP/m4.log" "svt"
    gruen M4 "$TMP/m4.log" "praesent"
else
    echo "  FAIL: M4 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M5: die Cap nennt einen FREMDEN Vektor -----------------------------------------------------
#
# Eine gueltige, installierte, pruefbare Cap — auf den falschen Interrupt. `cap-in-slot7` bliebe
# gruen; nur `cap-nennt-vektor` sieht es. Dieselbe Form wie „das Geraet ist da" gegen „es ist das
# richtige Geraet" bei A-5.3.
echo "-- M5: die Irq-Cap traegt einen fremden Vektor --"
if mutiere kernel/src/system.rs \
   's/^                let ic = install_irq_cap(g.vector as u32, Rights::READ).ok()?;$/                let ic = install_irq_cap(g.vector as u32 ^ 1, Rights::READ).ok()?;/' M5 1; then
    lauf "$TMP/m5.log"
    pruefe M5 "$TMP/m5.log" "cap-nennt-vektor" false
    gruen M5 "$TMP/m5.log" "cap-in-slot7"
else
    echo "  FAIL: M5 -- Mutation nicht anwendbar"; fail=1
fi
restore


# --- M6: die MSI-X-Zeile bleibt MASKIERT ---------------------------------------------------------
#
# Der Eintrag steht, die Adresse stimmt, das Geraet ist scharf -- und die Zeile ist zu. Ohne
# `zeile-steht` waere das von „der Interrupt kam nicht" nicht zu unterscheiden, und genau diese
# Verwechslung hat die B4-Suche eine Runde gekostet: die erste Hypothese war „ein Geraetereset
# wischt die Zeile", und erst die Ruecklesung hat sie widerlegt.
echo "-- M6: die MSI-X-Zeile bleibt maskiert (Vector Control Bit 0) --"
if mutiere crates/caprock-hal/src/x86_64/pcie.rs \
   's|^        core::ptr::write_volatile((e + 12) as \*mut u32, 0); // Vector Control: Maske loesen$|        core::ptr::write_volatile((e + 12) as *mut u32, 1);|' M6 1; then
    lauf "$TMP/m6.log"
    pruefe M6 "$TMP/m6.log" "zeile-steht" false
    # Die IRTE-Seite ist davon unberuehrt -- das ist die Trennschaerfe zwischen Tabelle und Geraet.
    gruen M6 "$TMP/m6.log" "praesent"
    gruen M6 "$TMP/m6.log" "svt"
    gruen M6 "$TMP/m6.log" "cap-in-slot7"
else
    echo "  FAIL: M6 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M7: der Treiber meldet nicht ----------------------------------------------------------------
#
# Ohne Marke liest der Bericht die vier B4-Zahlen aus **fremden Bytes** -- das ist einmal passiert
# (`weckrufe=9223372037261623427` aus dem Virtqueue-Ring von virtio-net). `melder` ist die
# Sprechprobe dagegen, und sie gattert unbedingt.
echo "-- M7: die Melder-Marke fehlt --"
if mutiere programs/hardware/virtio-blk/src/main.rs \
   's/^const B4_MAGIC: u64 = 0x4234_4D45_4C44_4552;$/const B4_MAGIC: u64 = 0x0000_0000_0000_0001;/' M7 1; then
    lauf "$TMP/m7.log"
    if grep -q "^irqmsi  : FAILURES" "$TMP/m7.log" && grep -q "melder=0" "$TMP/m7.log"; then
        echo "  PASS: M7 -- 'irqmsi: FAILURES' mit 'melder=0', wie gemeint"
    else
        echo "  FAIL: M7 -- erwartet 'irqmsi  : FAILURES' mit 'melder=0'"
        grep -E "^irqmsi" "$TMP/m7.log" | head -3; fail=1
    fi
    gruen M7 "$TMP/m7.log" "svt"
    gruen M7 "$TMP/m7.log" "zeile-steht"
else
    echo "  FAIL: M7 -- Mutation nicht anwendbar"; fail=1
fi
restore

# --- M8: die BEDINGUNG selbst -------------------------------------------------------------------
#
# **Die wichtigste der acht.** `b4-aktiv=false` schaltet die B4-Haelfte ab; eine Praemisse, die nie
# wahr wird, ist eine Konjunkte, die nie urteilt -- *ein Negativtest kann eine Eigenschaft
# absichern, die niemand benutzt.* Gemeldet wird hier `aktiv=1`, OHNE den Warteweg einzuschalten:
# damit greift die Bedingung, `wartende` bleibt 0, und die Zeile muss genau daran fallen.
#
# Den Warteweg wirklich einzuschalten waere die naheliegende Fassung und die schlechtere: der
# Treiber blockierte (Stufe A fehlt), die Suite liefe in den Watchdog, und die Mutation machte
# **zwei** Dinge zugleich -- das beweist ueber keines etwas.
echo "-- M8: b4-aktiv gemeldet, ohne dass gewartet wird --"
if mutiere programs/hardware/virtio-blk/src/main.rs \
   's|^        core::ptr::write_volatile((dma_cpu + OFF_B4_AKTIV) as \*mut u64, u64::from(B4_WARTEN));$|        core::ptr::write_volatile((dma_cpu + OFF_B4_AKTIV) as *mut u64, 1);|' M8 1; then
    lauf "$TMP/m8.log"
    if grep -q "^irqmsi  : FAILURES" "$TMP/m8.log" && grep -q "b4-aktiv=true" "$TMP/m8.log"; then
        echo "  PASS: M8 -- 'irqmsi: FAILURES' bei 'b4-aktiv=true', die Bedingung greift also"
    else
        echo "  FAIL: M8 -- erwartet 'irqmsi  : FAILURES' mit 'b4-aktiv=true'"
        grep -E "^irqmsi" "$TMP/m8.log" | head -3; fail=1
    fi
    # Alles ausserhalb der B4-Haelfte bleibt gruen -- die Bedingung schaltet GENAU sie.
    gruen M8 "$TMP/m8.log" "svt"
    gruen M8 "$TMP/m8.log" "cap-in-slot7"
    gruen M8 "$TMP/m8.log" "bind-ohne-cap-abgewiesen"
else
    echo "  FAIL: M8 -- Mutation nicht anwendbar"; fail=1
fi
restore

echo
if [ "$fail" = 0 ]; then
    echo "== IRQMSI-GEGENPROBEN: ALL PASS =="
else
    echo "== IRQMSI-GEGENPROBEN: FAILURES =="
fi
exit "$fail"
