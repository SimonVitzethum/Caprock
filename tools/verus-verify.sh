#!/usr/bin/env bash
# SEL4Lake — Formale Verifikation Tier 2: Verus (deduktive funktionale Verifikation).
#
# Beweist, dass dokumentierte Kernel-Invarianten (`cap_audit_cdt`, `dma_audit`, `loader_audit`, …)
# von den jeweiligen Operationen ERHALTEN werden — statisch + fuer ALLE Zustaende, nicht nur an den
# Audit-Punkten wie zur Laufzeit.
#
# Voraussetzung: Verus-Binary (+ Z3, mitgeliefert) unter $VERUS bzw. ~/.verus/verus.
#   Installation (einmalig): Release von https://github.com/verus-lang/verus/releases/latest
#   nach ~/.verus entpacken; die von Verus geforderte Rust-Toolchain via `rustup toolchain install`.
#
# Aufruf:  tools/verus-verify.sh            # alle Beweise + Modell-Treue-Waechter
#          tools/verus-verify.sh --selftest # nur: kann dieses Skript ueberhaupt fehlschlagen?
#
# ------------------------------------------------------------------------------------------------
# **Warum die Dateiliste nicht mehr aus zwei Globs kommt.** (B-7.3, 2026-08-03)
#
# Bis hierher sammelte dieses Skript `verus/*.rs` + `Verification/*/proofs/*.rs`. Eine Beweisdatei
# irgendwo sonst — `Verification/x/y.rs`, ein Unterverzeichnis unter `proofs/`, eine neue Ablage —
# lief damit **nirgends**, und zwar lautlos: das Skript meldete weiter `0 errors`, weil es die Datei
# gar nicht kannte. Das ist dieselbe Form wie „ein Test, der nirgends laeuft, ist kein Test"
# (CLAUDE.md), nur eine Ebene tiefer: hier fehlt nicht der Testlauf, sondern das Einsammeln.
#
# Jetzt entscheidet der **Inhalt**: jede `.rs`-Datei im Baum mit einem `verus!`-Block wird gefahren.
# Wer eine Beweisdatei anlegt, muss dafuer nichts konfigurieren — und kann sie auch nicht mehr aus
# Versehen an der Suite vorbeilegen.
# ------------------------------------------------------------------------------------------------
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERUS="${VERUS:-$(command -v verus || true)}"
[ -x "$VERUS" ] || VERUS="$HOME/.verus/verus"
if [ ! -x "$VERUS" ]; then
    echo "Verus nicht gefunden. Setze \$VERUS oder installiere nach ~/.verus (s. Kopf dieses Skripts)."
    exit 127
fi

# Alle Beweisdateien im Baum finden (inhaltsbasiert, s. o.). Ausgeschlossen sind nur Orte, die
# keine Quelle sind: Bauverzeichnisse, .git und die Wegwerf-Worktrees der Agenten (dort liegen
# ALTE Kopien derselben Dateien -- die zu verifizieren waere die Kopie-mit-Zertifikat-Falle).
beweisdateien() {
    find "$ROOT" \
        \( -name target -o -name build -o -name .git -o -name .claude -o -name node_modules \) -prune -o \
        -name '*.rs' -print 2>/dev/null \
    | sort \
    | while IFS= read -r f; do grep -lq 'verus!' "$f" 2>/dev/null && echo "$f"; done
}

lauf() {   # lauf  -> setzt $rc; gibt die Ergebniszeilen aus
    local rc=0 n=0 f
    echo "-- Verus $("$VERUS" --version 2>/dev/null | sed -n 's/^  Version: //p') --"
    while IFS= read -r f; do
        n=$((n+1))
        echo "== Verus: ${f#$ROOT/} =="
        "$VERUS" --crate-type=lib "$f" || rc=1
    done < <(beweisdateien)
    # Ein leerer Lauf ist kein Testergebnis: faende `find`/`grep` nichts, meldete dieses Skript
    # sonst Erfolg, ohne einen einzigen Beweis gefahren zu haben.
    if [ "$n" -eq 0 ]; then
        echo "FEHLER: KEINE Beweisdatei gefunden. Ein leerer Lauf ist kein bestandener Test." >&2
        return 1
    fi
    echo "-- $n Beweisdateien gefahren --"
    return "$rc"
}

# **Kann dieses Skript ueberhaupt fehlschlagen?** Wir schieben eine Beweisdatei unter, die
# (a) garantiert nicht durchgeht und (b) an einem Ort liegt, den die alten zwei Globs NICHT
# getroffen haetten. Findet das Skript sie nicht, ist genau die Luecke wieder da.
selbsttest() {
    local P="$ROOT/Verification/capability-system/sel4lake_waechterprobe.rs"
    trap 'rm -f "$P"' RETURN
    cat > "$P" <<'EOF'
// Wegwerfdatei des Selbsttests von tools/verus-verify.sh. Sie MUSS fehlschlagen.
use vstd::prelude::*;
verus! {
pub proof fn absichtlich_falsch(x: nat)
    ensures x < x,
{
}
fn main() {}
}
EOF
    # Achtung: NICHT `beweisdateien | grep -q` -- `grep -q` schliesst die Pipe frueh, der Erzeuger
    # stirbt an SIGPIPE, und `pipefail` macht daraus einen Fehlschlag der ganzen Pipeline. Der
    # Selbsttest haette dann „wird nicht eingesammelt" gemeldet, obwohl sie eingesammelt wird.
    local liste; liste="$(beweisdateien)"
    if printf '%s\n' "$liste" | grep -q 'sel4lake_waechterprobe.rs'; then
        echo "  Selbsttest 1/2: die untergeschobene Datei wird eingesammelt (auch ausserhalb von proofs/)"
    else
        echo "  FEHLER: die untergeschobene Beweisdatei wird NICHT eingesammelt." >&2
        return 1
    fi
    if lauf >/dev/null 2>&1; then
        echo "  FEHLER: der Lauf meldet Erfolg, obwohl ein Beweis nachweislich falsch ist." >&2
        return 1
    fi
    echo "  Selbsttest 2/2: ein falscher Beweis laesst den Lauf fehlschlagen"
    return 0
}

if [ "${1:-}" = "--selftest" ]; then selbsttest; exit $?; fi

rc=0
lauf || rc=1

echo
echo "== Modell-Treue (Verus-Modell gegen den echten Quelltext) =="
bash "$ROOT/tools/verus-modelltreue.sh" || rc=1

exit "$rc"
