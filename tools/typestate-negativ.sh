#!/usr/bin/env bash
# **Der Descriptor-Typestate muss WIRKEN, nicht dastehen** (todo E).
#
# `Owned<Driver>` / `Owned<Device>` in `crates/sel4lake-virtio/src/owned.rs` behaupten: ein Puffer,
# der dem Geraet uebergeben wurde, ist im Treibercode nicht mehr adressierbar, bis das Geraet ihn
# zurueckgibt. Eine Behauptung ueber den Uebersetzer ist so lange wertlos, wie niemand den
# Uebersetzer gefragt hat.
#
# Also fragen wir ihn. Drei Faelle, jeder mit einer ERWARTETEN Diagnose:
#
#   1. POSITIVKONTROLLE  -- der richtige Ablauf uebersetzt.
#   2. armiert + weitergeschrieben -- der alte Name ist weg (E0382, use after move).
#   3. `Owned<Device>` beschrieben  -- diesen Weg gibt es nicht (E0599, kein solcher Name).
#   4. Deskriptor an `arm` vorbei    -- `set_desc` ist privat (E0624).
#
# **Warum die Fehlercodes und nicht bloss "hat nicht uebersetzt":** eine Negativdatei scheitert auch
# an einem Tippfehler, an einem fehlenden `use`, an einer geaenderten Signatur. Ein Test, der jeden
# Fehlschlag als Beleg nimmt, belegt nichts — er ist gruen, sobald irgendetwas kaputt ist. Genau
# dieselbe Falle wie die leere Event-Queue: eine Aussage sieht wahr aus, weil der Fall, der sie
# widerlegen koennte, nie laeuft.
#
# **Warum kein `trybuild`:** das waere eine Abhaengigkeit, und `sel4lake-virtio` hat KEINE. Das ist
# Absicht (A-5.1: die Crate wird von einer Userland-Treiber-PD gelinkt), nicht Sparsamkeit.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/typestate.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
RUSTC="rustup run nightly rustc"
fail=0

QUELLE="$ROOT/crates/sel4lake-virtio/src/lib.rs"
if [ ! -f "$QUELLE" ]; then
    echo "  FEHLT: $QUELLE ist nicht vorhanden -- Ziel nicht gelaufen (kein Uebersetzungsfehler)"
    exit 1
fi

# Die Crate einmal als rlib bauen. `no_std` als rlib braucht keinen Panic-Handler.
if ! $RUSTC --edition 2021 --crate-type rlib --crate-name sel4lake_virtio \
        "$QUELLE" -o "$TMP/libsel4lake_virtio.rlib" 2>"$TMP/lib.err"; then
    echo "  FEHLER: sel4lake-virtio uebersetzt selbst nicht -- der Negativtest kann nichts aussagen"
    head -20 "$TMP/lib.err"
    exit 1
fi

# Gemeinsamer Rumpf: eine Region, ein Puffer, eine Queue gibt es hier nicht (die entsteht nur am
# Geraet). Geprueft wird deshalb an `Owned` selbst plus an der Sichtbarkeit von `set_desc`.
kopf() {
    cat <<'EOF'
#![no_std]
extern crate sel4lake_virtio as v;
use v::{Owned, Driver, Device, Region};

/// Ein Stellvertreter fuer `Queue::arm`: er nimmt den Puffer `by value` und gibt ihn als
/// `Owned<Device>` zurueck -- genau die Signatur, um die es geht. Die echte Queue laesst sich hier
/// nicht bauen (sie entsteht in `queue_setup` am Geraet), die Eigentumsuebergabe schon.
fn armieren(b: Owned<Driver>) -> Owned<Device> {
    // Der echte Weg ist `Queue::arm`; hier genuegt, dass der Puffer verbraucht wird.
    unsafe { core::mem::transmute::<Owned<Driver>, Owned<Device>>(b) }
}
EOF
}

pruefe() { # $1 = Name, $2 = erwarteter Fehlercode ("" = muss uebersetzen), $3 = Datei
    local name="$1" code="$2" datei="$3"
    if $RUSTC --edition 2021 --crate-type rlib --extern sel4lake_virtio="$TMP/libsel4lake_virtio.rlib" \
            "$datei" -o "$TMP/out.rlib" >"$TMP/o.err" 2>&1; then
        if [ -z "$code" ]; then
            echo "  PASS  $name -- uebersetzt (Positivkontrolle)"
        else
            echo "  FEHLER $name -- uebersetzt, erwartet war $code"
            fail=1
        fi
        return
    fi
    if [ -z "$code" ]; then
        echo "  FEHLER $name -- Positivkontrolle uebersetzt NICHT; ohne sie belegen die"
        echo "         Negativfaelle nichts (sie koennten an irgendetwas scheitern)"
        grep -E "^error" -A 4 "$TMP/o.err" | head -20
        fail=1
        return
    fi
    if grep -q "\[$code\]" "$TMP/o.err"; then
        echo "  PASS  $name -- abgewiesen mit $code (der erwartete Grund, nicht irgendeiner)"
    else
        echo "  FEHLER $name -- abgewiesen, aber NICHT mit $code:"
        grep -E "^error" "$TMP/o.err" | head -5
        fail=1
    fi
}

echo "== Descriptor-Typestate: der Uebersetzer als Pruefer (todo E) =="

# --- 1. Positivkontrolle: der richtige Ablauf ---------------------------------------------------
{ kopf; cat <<'EOF'

pub unsafe fn richtig(cpu: u64, dev: u64) -> u64 {
    let mut region = Region::from_raw(cpu, dev, 0x2000);
    let mut buf = match region.carve(0x800, 64) { Some(b) => b, None => return 0 };
    buf.wr64(0, 0xdead_beef);      // VOR dem Armieren: erlaubt
    let armed = armieren(buf);      // ab hier gehoert er dem Geraet
    let dev_addr = armed.dev_addr(); // eine ZAHL ablesen ist kein Zugriff
    let back: Owned<Driver> = core::mem::transmute(armed); // (steht fuer `Queue::reclaim`)
    let _ = back.rd64(0);           // NACH dem Zurueckholen: wieder erlaubt
    dev_addr
}
EOF
} > "$TMP/positiv.rs"
pruefe "Positivkontrolle (schreiben, armieren, zurueckholen, lesen)" "" "$TMP/positiv.rs"

# --- 2. Der Fehler, um den es geht: armiert und trotzdem weitergeschrieben -----------------------
{ kopf; cat <<'EOF'

pub unsafe fn armiert_und_weiter_beschrieben(cpu: u64, dev: u64) {
    let mut region = Region::from_raw(cpu, dev, 0x2000);
    let mut buf = match region.carve(0x800, 64) { Some(b) => b, None => return };
    let _armed = armieren(buf);
    buf.wr64(0, 0x1234);   // <-- der Puffer steht in der Queue und wird weiter beschrieben
}
EOF
} > "$TMP/neg_move.rs"
pruefe "armierter Puffer wird weiter beschrieben" "E0382" "$TMP/neg_move.rs"

# --- 3. Zugriff auf den armierten Puffer selbst --------------------------------------------------
{ kopf; cat <<'EOF'

pub unsafe fn zugriff_auf_armierten(cpu: u64, dev: u64) {
    let mut region = Region::from_raw(cpu, dev, 0x2000);
    let buf = match region.carve(0x800, 64) { Some(b) => b, None => return };
    let mut armed = armieren(buf);
    armed.wr64(0, 0x1234); // <-- `Owned<Device>` hat keinen Zugriffsweg, auch keinen unsafe
}
EOF
} > "$TMP/neg_device.rs"
pruefe "Schreibzugriff auf Owned<Device>" "E0599" "$TMP/neg_device.rs"

# --- 4. Am Typestate vorbei: Deskriptor von Hand setzen ------------------------------------------
#
# Ohne diesen Fall waere die Zusicherung eine Empfehlung: wer `set_desc` selbst rufen darf, kann
# einen Puffer armieren, ohne ihn herzugeben.
{ kopf; cat <<'EOF'

pub unsafe fn deskriptor_von_hand(q: &v::Queue) {
    q.set_desc(0, 0x7000_0000, 64, 2, 0); // <-- privat, es gibt nur `Queue::arm`
}
EOF
} > "$TMP/neg_setdesc.rs"
pruefe "Deskriptor an arm() vorbei setzen" "E0624" "$TMP/neg_setdesc.rs"

if [ "$fail" = 0 ]; then
    echo "== TYPESTATE: ALL PASS =="
else
    echo "== TYPESTATE: FAILURES =="
fi
exit "$fail"
