#!/usr/bin/env bash
# SEL4Lake — Formale Verifikation Tier 2 (Pilot): Verus (deduktive funktionale Verifikation).
#
# Beweist, dass eine bereits dokumentierte Kernel-Invariante (`cap_audit_cdt`, Refcount-Anteil) von
# den Capability-Operationen ERHALTEN wird — statisch + fuer ALLE Zustaende (nicht nur an den
# Audit-Punkten wie zur Laufzeit). Erster kleiner Pilot; bewusst KEIN komplexer Bereich (Scheduler/
# IPC), sondern ein in sich geschlossenes, dokumentiertes Invariant.
#
# Voraussetzung: Verus-Binary (+ Z3, mitgeliefert) unter $VERUS bzw. ~/.verus/verus.
#   Installation (einmalig): Release von https://github.com/verus-lang/verus/releases/latest
#   nach ~/.verus entpacken; die von Verus geforderte Rust-Toolchain via `rustup toolchain install`.
# Aufruf:  tools/verus-verify.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERUS="${VERUS:-$(command -v verus || true)}"
[ -x "$VERUS" ] || VERUS="$HOME/.verus/verus"
if [ ! -x "$VERUS" ]; then
    echo "Verus nicht gefunden. Setze \$VERUS oder installiere nach ~/.verus (s. Kopf dieses Skripts)."
    exit 127
fi

# Alle Verus-Pilotdateien verifizieren (jede ist eigenstaendig, --crate-type=lib).
rc=0
for f in "$ROOT"/verus/*.rs; do
    echo "== Verus: $(basename "$f") =="
    "$VERUS" --crate-type=lib "$f" || rc=1
done
exit "$rc"
