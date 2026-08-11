#!/usr/bin/env python3
"""**Die EINE Zaehlung der Mangel-Meldestellen** -- Verbraucher: `kernel/build.rs` und
`tools/mangel-stellen.sh`.

Vorher fuehrte ein Mensch `pub const MELDESTELLEN` parallel zur Wahrheit, und eine Ratsche hielt
beide gegeneinander. Das hat am 2026-08-11 einen Merge-Fehler gefangen (31 gegen 32) -- gut --,
war aber dieselbe Klasse wie eine nachgerechnete Sektion-Segment-Zuordnung: **zwei Gedaechtnisse
fuer eine Tatsache.** Jetzt wird die Zahl **abgeleitet**: `build.rs` ruft dieses Skript und legt
das Ergebnis als `CAPROCK_MELDESTELLEN` ins Abbild. Nach einem Merge kann sie nicht mehr falsch
sein -- sie wird gelesen, nicht gefuehrt. Die Ratsche wacht seither ueber die ABLEITUNG.

Aufruf:  mangel-zaehlen.py <quelle.rs> [--nur-zahl]
"""
import re, sys

quelle = sys.argv[1]
modus = sys.argv[2] if len(sys.argv) > 2 else ""
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

# **`--nur-zahl` steigt HIER aus** -- vor jeder Pruefung der Konstante. Seit die Zahl aus diesem
# Skript ABGELEITET wird, gibt es die Konstante als Literal gar nicht mehr; wer erst prueft und
# dann ausgibt, kann seinen eigenen Verbraucher nicht bedienen.
if modus == "--nur-zahl":
    print(gezaehlt)
    raise SystemExit(0)
# **Geprueft wird jetzt die ABLEITUNG, nicht eine zweite Zahl.** Bis zum 2026-08-11 stand in
# `system.rs` ein Literal, das ein Mensch parallel zur Wahrheit fuehrte; dieser Waechter hielt
# beide gegeneinander. Das hat einen Merge-Fehler gefangen (31 gegen 32) -- und war doch dieselbe
# Klasse wie eine nachgerechnete Sektion-Segment-Zuordnung: zwei Gedaechtnisse fuer eine Tatsache.
# Seither leitet `kernel/build.rs` die Zahl aus GENAU DIESEM Skript ab. Was hier bleibt, ist die
# Frage, ob die Ableitung ueberhaupt noch verdrahtet ist: faellt sie auf ein Literal zurueck, ist
# das zweite Gedaechtnis wieder da, und niemand merkt es.
quelltext = "\n".join(zeilen)
if 'env!("CAPROCK_MELDESTELLEN")' not in quelltext:
    print("FEHLSCHLAG: `MELDESTELLEN` wird nicht mehr aus CAPROCK_MELDESTELLEN abgeleitet.")
    print("            Damit fuehrt wieder jemand eine Zahl neben der Wahrheit -- genau der")
    print("            Zustand, den die Ableitung beseitigt hat.")
    sys.exit(1)
brs = open(quelle.rsplit("/src/", 1)[0] + "/build.rs", encoding="utf-8").read()
if "CAPROCK_MELDESTELLEN" not in brs or "mangel-zaehlen.py" not in brs:
    print("FEHLSCHLAG: `build.rs` setzt CAPROCK_MELDESTELLEN nicht mehr aus diesem Zaehler.")
    sys.exit(1)

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
print("MELDESTELLEN: abgeleitet ueber CAPROCK_MELDESTELLEN (build.rs ruft diesen Zaehler)")

# Die handgeschriebenen sind die interessanten: nur sie koennen SCHWEIGEN (ein `return None` ohne
# `mangel(..)` daneben). Die Helfer melden bei jedem `None` -- das ist der Unterschied, an dem die
# Abdeckungsangabe des Sweeps haengt.
hand = nach_art.get("mangel", 0)
print("Davon handgeschrieben (koennen schweigen): %d · ueber Helfer (koennen es strukturell nicht): %d"
      % (hand, gezaehlt - hand))

print("OK -- der Nenner der `sweep`-Zeile wird ABGELEITET, nicht gefuehrt (%d Stellen)." % gezaehlt)