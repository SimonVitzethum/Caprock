#!/usr/bin/env bash
# **Die Zahl der Mangel-Meldestellen ist gezaehlt, nicht geschaetzt** (C7, 2026-08-10).
#
# ================================================================================================
# WARUM ES DAS GIBT
# ================================================================================================
#
# An der `mangel`-Pruefzeile stand „die uebrigen rund zwanzig Stellen sind gegengelesen, nicht
# gemessen". „Rund zwanzig" ist derselbe Nullbefund wie „ich habe das noch nie gesehen": es klingt
# nach einer Groesse und ist eine Erinnerung. Gezaehlt sind es **31**, nicht zwanzig -- und mit
# der falschen Zahl waere die Abdeckungsangabe des Sweeps (`n von m`) von Anfang an falsch
# gewesen, ohne dass es jemandem auffaellt.
#
# Eine Zahl im Kommentar verrottet. Dieses Skript haelt sie gegen den Quelltext:
# `system::MELDESTELLEN` MUSS die Summe der drei Formen sein.
#
#   * handgeschrieben : `mangel(MANGEL_..., <menge>)`
#   * benannt_alloc   : meldet bei jedem `None` -- kann strukturell nicht schweigen
#   * benannt_slot    : dito, fuer Toepfe die Plaetze statt Bytes vergeben
#
# **Nicht mitgezaehlt** wird `mangel_vergiften()`: die Marke MELDET keinen Mangel, sie stellt eine
# Frage. Wer sie mitzaehlt, macht die Sprechprobe zu einer Meldestelle und die Abdeckung um eins
# zu gut.
#
# Aufruf:  tools/mangel-stellen.sh          (prueft)
#          tools/mangel-stellen.sh --liste  (zeigt jede Stelle mit Zeile, Code und Menge)
#
# Rueckgabe: 0 = die Zahl stimmt, 1 = sie stimmt nicht, 2 = Aufbaufehler.
if [ -z "${BASH_VERSION:-}" ]; then echo "FEHLER: braucht bash." >&2; exit 2; fi
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2

QUELLE="kernel/src/system.rs"
[ -r "$QUELLE" ] || { echo "FEHLER: $QUELLE nicht lesbar." >&2; exit 2; }

# Der Zaehler laeuft in Python, damit Kommentarzeilen sicher ausscheiden: ein `grep` auf
# `mangel(MANGEL_` trifft auch die Doku-Zeilen, in denen die Falle vom Vormittag ZITIERT wird --
# und ein Waechter, der Prosa mitzaehlt, meldet Zuwachs, sobald jemand ihn erklaert.
python3 - "$QUELLE" "${1:-}" <<'PY'
import re, sys

quelle, modus = sys.argv[1], sys.argv[2]
zeilen = open(quelle, encoding="utf-8").read().split("\n")

# Die Funktionsnamen mitschleppen -- sie stehen in der Liste und machen den Befund lesbar.
fns, aktuell = [], "?"
def besitzer(nr):
    b = "?"
    for start, name in fns:
        if start <= nr:
            b = name
        else:
            break
    return b

for i, z in enumerate(zeilen):
    m = re.match(r'\s*(pub(\([^)]*\))?\s+)?(unsafe\s+)?(extern\s+"[^"]*"\s+)?fn\s+(\w+)', z)
    if m:
        fns.append((i + 1, m.group(5)))

stellen = []
for i, z in enumerate(zeilen):
    nr, roh = i + 1, z.strip()
    if roh.startswith("//") or roh.startswith("///") or roh.startswith("*"):
        continue  # Prosa, kein Code
    m = re.search(r'(?<![\w_])mangel\((MANGEL_\w+)\s*,\s*([^)]*)\)', z)
    if m and "fn mangel" not in z:
        # Die Marke ist keine Meldestelle -- sie stellt die Frage, sie beantwortet sie nicht.
        if m.group(1) != "MANGEL_VERGIFTET":
            stellen.append((nr, "mangel", m.group(1), m.group(2).strip(), besitzer(nr)))
    for art in ("benannt_alloc", "benannt_slot"):
        if re.search(r'(?<![\w_])%s\(' % art, z) and ("fn %s" % art) not in z:
            m2 = re.search(r'%s\((MANGEL_\w+)\s*,\s*([^,)]*)' % art, z)
            stellen.append((nr, art, m2.group(1) if m2 else "?",
                            (m2.group(2).strip() if m2 else "?"), besitzer(nr)))

gezaehlt = len(stellen)
m = re.search(r'pub const MELDESTELLEN:\s*usize\s*=\s*(\d+)\s*;', "\n".join(zeilen))
if not m:
    print("FEHLER: `pub const MELDESTELLEN` steht nicht in %s." % quelle)
    sys.exit(2)
behauptet = int(m.group(1))

if modus == "--liste":
    print("%6s  %-13s %-28s %-18s %s" % ("Zeile", "Art", "Topf", "Menge", "Funktion"))
    for s in stellen:
        print("%6d  %-13s %-28s %-18s %s" % (s[0], s[1], s[2], s[3][:18], s[4]))
    print()

nach_art = {}
for s in stellen:
    nach_art[s[1]] = nach_art.get(s[1], 0) + 1
print("Meldestellen gezaehlt: %d  (%s)"
      % (gezaehlt, " · ".join("%s %d" % (k, v) for k, v in sorted(nach_art.items()))))
print("MELDESTELLEN im Quelltext: %d" % behauptet)

# Die handgeschriebenen sind die interessanten: nur sie koennen SCHWEIGEN (ein `return None` ohne
# `mangel(..)` daneben). Die Helfer melden bei jedem `None` -- das ist der Unterschied, an dem die
# Abdeckungsangabe des Sweeps haengt.
hand = nach_art.get("mangel", 0)
print("Davon handgeschrieben (koennen schweigen): %d · ueber Helfer (koennen es strukturell nicht): %d"
      % (hand, gezaehlt - hand))

if gezaehlt != behauptet:
    print()
    print("FEHLSCHLAG: die Zahl stimmt nicht mehr. Wer eine Meldestelle hinzufuegt oder entfernt,")
    print("            zieht `pub const MELDESTELLEN` in %s mit -- sonst nennt die" % quelle)
    print("            `sweep`-Zeile eine Abdeckung, deren Nenner falsch ist.")
    sys.exit(1)
print("OK -- die Abdeckungsangabe der `sweep`-Zeile hat einen richtigen Nenner.")
PY
rc=$?

# ------------------------------------------------------------------------------------------------
# Selbsttest: der Waechter muss auch NEIN sagen koennen.
# ------------------------------------------------------------------------------------------------
#
# Ein Zaehler, der nie widerspricht, ist von einem, der immer `true` liefert, nicht zu
# unterscheiden -- dieselbe Frage wie bei jeder Pruefzeile hier. Geprueft wird an einer KOPIE.
if [ "${1:-}" != "--liste" ] && [ "$rc" -eq 0 ]; then
    TMP="$(mktemp -d)" || exit 2
    trap 'rm -rf "$TMP"' EXIT
    mkdir -p "$TMP/kernel/src" "$TMP/tools"
    sed 's/pub const MELDESTELLEN: usize = [0-9]*;/pub const MELDESTELLEN: usize = 999;/' \
        "$QUELLE" >"$TMP/kernel/src/system.rs"
    cp "$0" "$TMP/tools/$(basename "$0")"
    if (cd "$TMP" && bash "tools/$(basename "$0")" >/dev/null 2>&1); then
        echo "SELBSTTEST FEHLGESCHLAGEN: der Waechter nickt eine falsche Zahl (999) ab."
        exit 1
    fi
    echo "Selbsttest: eine falsche Zahl (999) faellt durch -- der Waechter ist sprechfaehig."
fi
exit "$rc"
