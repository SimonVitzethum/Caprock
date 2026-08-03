#!/usr/bin/env bash
# **Prueft, dass die CI-Beschreibungen ueberhaupt ladbar sind.** (2026-08-03)
#
# Anlass: `.gitea/workflows/kani.yml` war seit seiner Anlage ungueltiges YAML. Die Zeile
#
#     - name: Kani-Beweise ausfuehren (alle Ziele: loader, region, sync, unsafe-safety)
#
# enthaelt ein `: ` mitten im Skalar; YAML liest das als verschachtelte Abbildung und bricht ab.
# Die Datei haette also auch auf einem Gitea-Server NIE geladen -- zusaetzlich dazu, dass gar kein
# Gitea-Server existiert. Gemerkt hat es niemand, weil eine CI-Datei nur dann Rueckmeldung gibt,
# wenn sie auch gefahren wird.
#
# Das ist dieselbe Form wie die leere Ereigniswarteschlange ohne `CD.R`: etwas schweigt, und das
# Schweigen sieht aus wie Ordnung. Ein Syntaxpruefer ist die billigste Stelle, an der man das
# unterbricht -- er braucht weder Server noch Runner noch Netz.
#
# ACHTUNG, GRENZE: dieser Pruefer sagt „ladbar", nicht „richtig". Ob ein Job das Gemeinte tut,
# faellt hier nicht auf; dafuer muss er laufen.
#
# Aufruf:
#   tools/ci-yaml.sh              # pruefen + Selbsttest
#   tools/ci-yaml.sh --nur-pruefen
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

pruefe() {   # pruefe <datei>...
    python3 - "$@" <<'PY'
import sys, glob
try:
    import yaml
except ImportError:
    print("FEHLER: PyYAML fehlt -- der Pruefer kann nichts sagen und meldet das, statt zu schweigen.", file=sys.stderr)
    sys.exit(3)

dateien = sys.argv[1:]
if not dateien:
    # Ein leerer Lauf ist kein Testergebnis.
    print("FEHLER: keine CI-Datei gefunden -- entweder wurde umbenannt oder der Pfad stimmt nicht.", file=sys.stderr)
    sys.exit(2)

schlecht = 0
for d in dateien:
    try:
        with open(d, encoding='utf-8') as f:
            yaml.safe_load(f)
        print("  ok      %s" % d)
    except Exception as e:
        erste = str(e).splitlines()
        print("  KAPUTT  %s -- %s" % (d, " | ".join(erste[:2])))
        schlecht += 1
sys.exit(1 if schlecht else 0)
PY
}

sammeln() {
    local -a f=()
    [ -f .gitlab-ci.yml ] && f+=(.gitlab-ci.yml)
    local g
    for g in .gitea/workflows/*.yml .gitea/workflows/*.yaml \
             .github/workflows/*.yml .github/workflows/*.yaml; do
        [ -f "$g" ] && f+=("$g")
    done
    printf '%s\n' "${f[@]:-}"
}

mapfile -t DATEIEN < <(sammeln)
DATEIEN=("${DATEIEN[@]:-}")
if [ -z "${DATEIEN[0]:-}" ]; then
    echo "FEHLER: keine CI-Beschreibung gefunden. Leeres Ergebnis ist kein Erfolg." >&2
    exit 2
fi

echo "== CI-YAML (${#DATEIEN[@]} Dateien) =="
pruefe "${DATEIEN[@]}"
RC=$?

[ "${1:-}" = "--nur-pruefen" ] && exit $RC
[ $RC -ne 0 ] && exit $RC

# ---------------------------------------------------------------------------------------------
# Selbsttest: kann dieser Pruefer ueberhaupt anschlagen? Zwei Faelle -- einer, der anschlagen MUSS
# (genau der echte Fehler von damals), und eine NEGATIVKONTROLLE, die schweigen muss. Ohne die
# zweite wuesste man nur, dass der Pruefer laut ist, nicht dass er unterscheidet.
# ---------------------------------------------------------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

printf 'jobs:\n  x:\n    steps:\n      - name: Ziele: a, b\n        run: true\n' > "$TMP/kaputt.yml"
printf 'jobs:\n  x:\n    steps:\n      - name: "Ziele: a, b"\n        run: true\n' > "$TMP/heil.yml"

FEHLER=0
if pruefe "$TMP/kaputt.yml" >/dev/null 2>&1; then
    echo "SELBSTTEST FEHLGESCHLAGEN: der echte Fehler von 2026-06 (unquotiertes ': ' im Namen)" >&2
    echo "                           wird NICHT erkannt -- der Pruefer ist blind." >&2
    FEHLER=1
fi
if ! pruefe "$TMP/heil.yml" >/dev/null 2>&1; then
    echo "SELBSTTEST FEHLGESCHLAGEN: Negativkontrolle schlaegt an -- der Pruefer meldet alles," >&2
    echo "                           also unterscheidet er nichts." >&2
    FEHLER=1
fi
[ $FEHLER -ne 0 ] && exit 1

echo "  Selbsttest: der unquotierte Doppelpunkt wird erkannt, die heile Fassung nicht -- sprechfaehig"
echo "== CI-YAML: alle ladbar =="
