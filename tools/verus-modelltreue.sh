#!/usr/bin/env bash
# **Haelt das Verus-Modell an den echten Quelltext.** (B-7.3)
#
# Der Verus-Beweis in `Verification/capability-system/proofs/cap_space.rs` gilt an einem MODELL,
# nicht am Code: Verus braucht seinen eigenen Dialekt, also ist die Beziehung zwischen
# `crates/caprock-cap/src/space.rs::unlink` und den Spezifikationen `unlink1`/`unlink2`/
# `unlink_slots` zwangslaeufig eine **Uebertragung**.
#
# Bis hierher war diese Uebertragung eine BEHAUPTUNG (README §11/§12: „Modell-Treue ... eine
# dokumentierte Annahme"). Das ist genau die Form, die B-7.2 teuer bezahlt hat: eine Kopie, die
# autoritativ aussieht, aber nichts mehr mit dem Original zu tun hat. Ein gruener Beweis sagt
# darueber NICHTS aus -- er kann per Konstruktion nicht bemerken, dass sich der Code unter ihm
# bewegt hat.
#
# Dieser Waechter vergleicht deshalb beide Seiten **strukturell**: er reduziert jede auf dieselbe
# normalisierte Folge aus Verzweigungen und Feldzuweisungen und verlangt Gleichheit. Er beweist
# nichts; er sorgt dafuer, dass eine Aenderung an einer Seite nicht stillschweigend an der anderen
# vorbeigeht.
#
# Aufruf:
#   tools/verus-modelltreue.sh              # pruefen + Selbsttest
#   tools/verus-modelltreue.sh --nur-pruefen
#   tools/verus-modelltreue.sh --selftest
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash, nicht sh/dash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

CODE_STD="$ROOT/crates/caprock-cap/src/space.rs"
MODELL_STD="$ROOT/Verification/capability-system/proofs/cap_space.rs"

# ------------------------------------------------------------------------------------------------
# Der Normalisierer. Beide Seiten werden auf dieselbe Ereignisfolge abgebildet:
#
#   BRANCH <feld>            -- verzweigt auf `Option`-Feld <feld> des zu loeschenden Knotens
#   ARM some <feld>          -- der Zweig „ist Some" dazu (Binder wird auf <feld> gebunden)
#   ARM none <feld>          -- der Zweig „ist None" dazu
#   WRITE <ziel>.<f> := <v>  -- Schreibzugriff: Slot am Binder <ziel>, Feld <f>, Wert <v>
#   WRITE self.* := EMPTY    -- der geloeschte Slot selbst wird geleert
#
# `match X { Some(p) => A, None => B }` und `if X is Some { A } else { B }` sind damit dieselbe
# Folge -- der Dialektunterschied faellt weg, die Struktur bleibt stehen.
# ------------------------------------------------------------------------------------------------
normalisieren() {   # normalisieren <seite: code|modell> <datei>
    python3 - "$1" "$2" <<'PY'
import io, re, sys

seite, pfad = sys.argv[1], sys.argv[2]
text = io.open(pfad, encoding='utf-8').read()

FELD = {'prev_sibling': 'prev', 'next_sibling': 'next',
        'first_child': 'first_child', 'parent': 'parent',
        'prev': 'prev', 'next': 'next'}

def feld(n):
    if n not in FELD:
        sys.exit("FEHLER: unbekanntes MDB-Feld %r in %s -- der Normalisierer kennt es nicht.\n"
                 "        Entweder ist es neu (dann gehoert es hier hinein UND ins Modell),\n"
                 "        oder der Waechter liest die falsche Stelle." % (n, pfad))
    return FELD[n]

def rumpf(text, startmuster):
    """Rumpf ab der ersten Zeile, die `startmuster` trifft, bis die Klammertiefe wieder 0 ist."""
    zeilen = text.splitlines()
    for k, z in enumerate(zeilen):
        if re.search(startmuster, z):
            raus, tiefe, begonnen = [], 0, False
            for z2 in zeilen[k:]:
                ohne = re.sub(r'//.*$', '', z2)
                raus.append(ohne)
                tiefe += ohne.count('{') - ohne.count('}')
                if ohne.count('{'):
                    begonnen = True
                if begonnen and tiefe <= 0:
                    return raus
            sys.exit("FEHLER: Rumpf zu %r in %s nicht geschlossen." % (startmuster, pfad))
    sys.exit("FEHLER: %r in %s NICHT GEFUNDEN. Der Waechter liest ins Leere -- das ist kein\n"
             "        bestandener Test, sondern ein kaputter Waechter." % (startmuster, pfad))

ereignisse = []
binder = {}          # Variablenname -> MDB-Feld, aus dem er stammt
letzte_verzweigung = [None]

def branch(f):
    ereignisse.append("BRANCH %s" % f); letzte_verzweigung[0] = f
def arm(art, f):
    ereignisse.append("ARM %s %s" % (art, f))
def write(ziel, f, v):
    ereignisse.append("WRITE %s.%s := %s" % (ziel, f, v))

if seite == 'code':
    # --- die echte Implementierung: `fn unlink(&mut self, slot: usize)`
    zeilen = rumpf(text, r'fn\s+unlink\s*\(\s*&mut\s+self\s*,\s*(\w+)\s*:')
    m = re.search(r'fn\s+unlink\s*\(\s*&mut\s+self\s*,\s*(\w+)\s*:', zeilen[0])
    slotparam = m.group(1)
    PAT = re.compile(r'''
        (?P<iflet>if\s+let\s+Some\(\s*(?P<iv>\w+)\s*\)\s*=\s*mdb\.(?P<if_>\w+))
      | (?P<mat>match\s+mdb\.(?P<mf>\w+)\s*\{)
      | (?P<some>Some\(\s*(?P<sv>\w+)\s*\)\s*=>)
      | (?P<none>None\s*=>)
      | (?P<clear>self\.slots\[\s*(?P<ct>\w+)\s*\]\.mdb\s*=\s*Mdb::EMPTY)
      | (?P<setf>self\.slots\[\s*(?P<t>\w+)\s*\]\.mdb\.(?P<wf>\w+)\s*=\s*mdb\.(?P<sf>\w+))
    ''', re.X)
    for z in zeilen:
        for g in PAT.finditer(z):
            if g.group('iflet'):
                f = feld(g.group('if_')); binder[g.group('iv')] = f; branch(f); arm('some', f)
            elif g.group('mat'):
                branch(feld(g.group('mf')))
            elif g.group('some'):
                f = letzte_verzweigung[0]; binder[g.group('sv')] = f; arm('some', f)
            elif g.group('none'):
                arm('none', letzte_verzweigung[0])
            elif g.group('clear'):
                z_ = 'self' if g.group('ct') == slotparam else binder.get(g.group('ct'), g.group('ct'))
                write(z_, '*', 'EMPTY')
            elif g.group('setf'):
                t = binder.get(g.group('t'))
                if t is None:
                    sys.exit("FEHLER: Schreibzugriff auf slots[%s] -- der Index stammt aus keinem\n"
                             "        erkannten Option-Zweig. Der Waechter kann die Struktur nicht\n"
                             "        beurteilen." % g.group('t'))
                write(t, feld(g.group('wf')), feld(g.group('sf')))
    # Zusatz: `delete_leaf` muss unlink VOR release_slot und danach den Refcount senken.
    dl = "\n".join(rumpf(text, r'fn\s+delete_leaf\s*\('))
    folge = re.findall(r'self\.unlink\(|self\.release_slot\(|self\.objects\[\w+\]\.refcount\s*-=', dl)
    ereignisse.append("DELETE_LEAF " + " ".join(
        {'self.unlink(': 'unlink', 'self.release_slot(': 'release_slot'}.get(x, 'refcount--')
        for x in folge))
else:
    # --- das Verus-Modell: unlink1 / unlink2 / unlink_slots
    zeilen = []
    for start in (r'spec\s+fn\s+unlink1\s*\(', r'spec\s+fn\s+unlink2\s*\(', r'spec\s+fn\s+unlink_slots\s*\('):
        zeilen += rumpf(text, start)
    PAT = re.compile(r'''
        (?P<elseif>\}\s*else\s+if\s+nd\.(?P<ef>\w+)\s+is\s+Some)
      | (?P<iff>if\s+nd\.(?P<jf>\w+)\s+is\s+Some)
      | (?P<bind>let\s+(?P<bv>\w+)\s*=\s*nd\.(?P<bf>\w+)->Some_0)
      | (?P<clear>\.update\(\s*i\s+as\s+int\s*,\s*dead_slot\(\)\s*\))
      | (?P<upd>\.update\(\s*(?P<t>\w+)\s+as\s+int\s*,\s*Slot\s*\{\s*(?P<wf>\w+)\s*:\s*nd\.(?P<sf>\w+))
    ''', re.X)
    for z in zeilen:
        for g in PAT.finditer(z):
            if g.group('elseif'):
                arm('none', letzte_verzweigung[0])
                f = feld(g.group('ef')); branch(f); arm('some', f)
            elif g.group('iff'):
                f = feld(g.group('jf')); branch(f); arm('some', f)
            elif g.group('bind'):
                binder[g.group('bv')] = feld(g.group('bf'))
            elif g.group('clear'):
                write('self', '*', 'EMPTY')
            elif g.group('upd'):
                t = binder.get(g.group('t'))
                if t is None:
                    sys.exit("FEHLER: Modell schreibt an Index %r ohne erkannte Option-Bindung."
                             % g.group('t'))
                write(t, feld(g.group('wf')), feld(g.group('sf')))
    # Das Modell faltet `unlink` + `release_slot` in einen Schritt und senkt den Refcount in
    # `delete`; die Reihenfolge ist damit festgelegt.
    ereignisse.append("DELETE_LEAF unlink release_slot refcount--")

if not ereignisse:
    sys.exit("FEHLER: leere Ereignisfolge fuer %s -- ein leerer Lauf ist kein Ergebnis." % pfad)
print("\n".join(ereignisse))
PY
}

pruefen() {   # pruefen <space.rs> <cap_space.rs> [--leise]
    local code="$1" modell="$2" leise="${3:-}"
    local a b
    # stderr mitfangen: sonst leckt eine Fehlermeldung des Normalisierers auch im leisen Lauf
    # (des Selbsttests) auf das Terminal und sieht aus wie ein Defekt des Waechters.
    a="$(normalisieren code "$code" 2>&1)"     || { [ -n "$leise" ] || echo "$a" >&2; return 2; }
    b="$(normalisieren modell "$modell" 2>&1)" || { [ -n "$leise" ] || echo "$b" >&2; return 2; }
    if [ "$a" = "$b" ]; then
        if [ -z "$leise" ]; then
            echo "  Code   : $(basename "$code")::unlink/delete_leaf"
            echo "  Modell : $(basename "$modell")::unlink1/unlink2/unlink_slots"
            echo "$a" | sed 's/^/    /'
            echo "  -> deckungsgleich ($(echo "$a" | wc -l) Ereignisse)"
        fi
        return 0
    fi
    if [ -z "$leise" ]; then
        echo "  ABWEICHUNG zwischen Code und Modell:" >&2
        diff <(echo "$a") <(echo "$b") | sed 's/^/    /' >&2
        echo "    (< Code $code)" >&2
        echo "    (> Modell $modell)" >&2
    fi
    return 1
}

# ------------------------------------------------------------------------------------------------
# Selbsttest: kann dieser Waechter ueberhaupt ausloesen -- und haelt er still, wenn nichts ist?
# Beide Haelften sind noetig. Ein Waechter, der immer schreit, wird abgeschaltet; einer, der nie
# schreit, ist eine Kopie mit Zertifikat.
# ------------------------------------------------------------------------------------------------
selbsttest() {
    local W; W="$(mktemp -d)"; trap 'rm -rf "$W"' RETURN
    local fehler=0 n=0
    mutieren() {   # mutieren <ziel: code|modell> <python-ersetzung>
        cp "$CODE_STD" "$W/space.rs"; cp "$MODELL_STD" "$W/cap_space.rs"
        local datei="$W/space.rs"; [ "$1" = "modell" ] && datei="$W/cap_space.rs"
        python3 - "$datei" "$2" <<'PY'
import io, sys
p, prog = sys.argv[1], sys.argv[2]
s = io.open(p, encoding='utf-8').read()
ns = {'s': s}
exec(prog, ns)
io.open(p, 'w', encoding='utf-8').write(ns['s'])
PY
    }
    erwarte() {   # erwarte <kracht|still> <name>
        n=$((n+1))
        pruefen "$W/space.rs" "$W/cap_space.rs" --leise
        local rc=$?
        if [ "$1" = "kracht" ]; then
            if [ "$rc" -eq 0 ]; then echo "  FEHLER: '$2' -- der Waechter schweigt." >&2; fehler=1
            else echo "  erkannt : $2"; fi
        else
            if [ "$rc" -ne 0 ]; then echo "  FEHLER: '$2' -- der Waechter schlaegt grundlos an." >&2; fehler=1
            else echo "  still   : $2"; fi
        fi
    }

    # -- Mutationen am ECHTEN Code --------------------------------------------------------------
    mutieren code 's = s.replace("Some(p) => self.slots[p].mdb.next_sibling = mdb.next_sibling,",
                                 "Some(p) => self.slots[p].mdb.next_sibling = mdb.prev_sibling,", 1)'
    erwarte kracht "Code: next[p] bekommt prev statt next"

    mutieren code 's = s.replace("""                if let Some(par) = mdb.parent {
                    self.slots[par].mdb.first_child = mdb.next_sibling;
                }""", "", 1)'
    erwarte kracht "Code: die first_child-Fortschreibung faellt weg"

    mutieren code 's = s.replace("""        if let Some(n) = mdb.next_sibling {
            self.slots[n].mdb.prev_sibling = mdb.prev_sibling;
        }
        self.slots[slot].mdb = Mdb::EMPTY;""",
    """        self.slots[slot].mdb = Mdb::EMPTY;
        if let Some(n) = mdb.next_sibling {
            self.slots[n].mdb.prev_sibling = mdb.prev_sibling;
        }""", 1)'
    erwarte kracht "Code: der Slot wird zuerst geleert statt zuletzt"

    mutieren code 's = s.replace("        self.unlink(slot);\n        self.release_slot(slot);",
                                 "        self.release_slot(slot);\n        self.unlink(slot);", 1)'
    erwarte kracht "Code: release_slot vor unlink"

    # -- Mutationen am MODELL -------------------------------------------------------------------
    mutieren modell 's = s.replace("s1.update(nx as int, Slot { prev: nd.prev,",
                                   "s1.update(nx as int, Slot { prev: nd.next,", 1)'
    erwarte kracht "Modell: prev[nx] bekommt next statt prev"

    mutieren modell 's = s.replace("""    } else if nd.parent is Some {
        let par = nd.parent->Some_0;
        slots.update(par as int, Slot { first_child: nd.next, ..slots[par as int] })
""", "", 1)'
    erwarte kracht "Modell: der first_child-Zweig faellt weg"

    mutieren modell 's = s.replace("pub open spec fn unlink1", "pub open spec fn unlink1_umbenannt", 1)'
    erwarte kracht "Modell: unlink1 gibt es nicht mehr (Waechter liest nicht ins Leere)"

    # -- Und die Gegenprobe: Kosmetik auf BEIDEN Seiten darf NICHT ausloesen ---------------------
    mutieren code 's = s.replace("if let Some(par) = mdb.parent", "if let Some(elter) = mdb.parent", 1)
s = s.replace("self.slots[par].mdb.first_child", "self.slots[elter].mdb.first_child", 1)
s = s.replace("    fn unlink(&mut self, slot: usize) {",
              "    // Kommentar des Selbsttests.\n\n    fn unlink(&mut self, slot: usize) {", 1)'
    python3 - "$W/cap_space.rs" <<'PY'
import io, sys
p = sys.argv[1]; s = io.open(p, encoding='utf-8').read()
s = s.replace("let nx = nd.next->Some_0;", "let nachfolger = nd.next->Some_0;   // umbenannt", 1)
s = s.replace("s1.update(nx as int,", "s1.update(nachfolger as int,", 1)
s = s.replace("..s1[nx as int] })", "..s1[nachfolger as int] })", 1)
io.open(p, 'w', encoding='utf-8').write(s)
PY
    erwarte still "Kosmetik beidseitig (Binder umbenannt, Kommentare, Leerzeilen)"

    echo "  Selbsttest: $n Faelle"
    return "$fehler"
}

MODUS="${1:-alles}"
case "$MODUS" in
    --nur-pruefen)
        pruefen "$CODE_STD" "$MODELL_STD" --leise; exit $? ;;
    --selftest)
        selbsttest; exit $? ;;
    alles|"")
        echo "== Modell-Treue: Verus-cap_space gegen caprock-cap::space =="
        pruefen "$CODE_STD" "$MODELL_STD" || { echo "== MODELL-TREUE VERLETZT ==" >&2; exit 1; }
        echo "-- Selbsttest --"
        selbsttest || { echo "== WAECHTER NICHT SPRECHFAEHIG ==" >&2; exit 1; }
        echo "== Modell und Code sind strukturell deckungsgleich =="
        ;;
    *) echo "Aufruf: $0 [--nur-pruefen|--selftest]" >&2; exit 2 ;;
esac
