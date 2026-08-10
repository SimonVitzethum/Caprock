#!/usr/bin/env python3
"""**Cargo mischt `.cargo/config.toml` aus JEDEM Vorfahrenverzeichnis — und haengt Arrays an.**

Ein Agenten-Worktree liegt unter `<repo>/.claude/worktrees/<id>`, also **innerhalb** des
Hauptbaums. Cargo laeuft beim Start das Verzeichnis nach oben und findet die Konfiguration
deshalb **zweimal**: die des Worktrees und die des Hauptbaums. Fuer `rustflags` heisst „mischen"
bei Cargo **aneinanderhaengen**, nicht „ersetzen" — die Liste steht danach doppelt da:

    target.x86_64-unknown-none.rustflags = ["-C", "link-arg=-Tkernel/x86_64-link.ld",
                                            "-C", "relocation-model=static",
                                            "-C", "link-arg=-Tkernel/x86_64-link.ld",
                                            "-C", "relocation-model=static"]

Damit bekommt `lld` das Linkerskript **zweimal** und wertet den ganzen `SECTIONS`-Block zweimal
aus. Gemessen am 2026-08-10, und die Folgen sind genau die, an denen todo.md („das nullgrosse
LOAD-Duplikat") drei Behebungsversuche verbraucht hat:

* **sieben leere Doppel-Sektionen** — genau die Ausgabesektionen, die ausser
  Eingabe-Beschreibungen noch etwas enthalten (`. = ALIGN(..)`, Symbolzuweisung, feste Adresse).
  Rein aus Eingabesektionen bestehende (`.boot`, `.data`, `.boot_bss`) verwirft lld, wenn sie
  leer sind — deshalb fehlen genau diese drei in der Duplikatliste.
* **alle Linkersymbole auf dem Wert des ZWEITEN Durchlaufs**: `__text_start = 0x100000`,
  `__bss_start = 0x101000`, `__aptramp_lma = 0x100000`. Der zweite Durchlauf faengt wieder bei
  `. = 1M` an und sieht lauter leere Sektionen.
* daraus ein **nullgrosses LOAD-Segment**, an dem GNU objcopy scheitert und `.boot` ans Dateiende
  schiebt — der sichtbare Fehler, aber nur das letzte Glied.

**Das ist ein Fehler der BAUUMGEBUNG, nicht der Quelle.** Deshalb hat der Gegenversuch aus
todo.md (die verdaechtige Aenderung im selben Worktree zurueckgenommen) ebenfalls fehlerhaft
gebaut, und deshalb baut derselbe Commit im Hauptbaum gesund: dort gibt es nur eine
Konfiguration. Die drei Versuche an `x86_64-link.ld` haben ein Symptom bearbeitet.

Dieses Skript gibt die **entdoppelte** Flagliste aus (Reihenfolge erhalten, exakt gleiche
`-C x=y`-Paare nur einmal). Der Aufrufer setzt sie als `CARGO_TARGET_<TRIPLE>_RUSTFLAGS` —
Umgebungsvariablen ERSETZEN die Konfiguration, sie mischen nicht.

Exit-Code 0 = nichts zu tun, 10 = entdoppelt (der Aufrufer meldet das laut; eine stille
Reparatur waere dieselbe Krankheit wie das stille Mischen).
"""

import json
import subprocess
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print("Aufruf: rustflags-entdoppeln.py <ziel-triple>", file=sys.stderr)
        return 2
    triple = sys.argv[1]
    key = f"target.{triple}.rustflags"
    try:
        raw = subprocess.run(
            ["rustup", "run", "nightly", "cargo", "-Zunstable-options",
             "config", "get", "--format=json-value", key],
            capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        # Kein Wert konfiguriert (oder cargo kann es nicht): NICHTS ausgeben und nichts
        # behaupten. Eine erfundene Flagliste waere schlimmer als keine.
        return 0
    try:
        flags = json.loads(raw)
    except json.JSONDecodeError:
        return 0
    if not isinstance(flags, list) or not flags:
        return 0

    # Paarweise entdoppeln: die Flags stehen als ["-C", "wert", "-C", "wert", ...] da.
    # Ein alleinstehendes Flag (ungerade Laenge) wird unveraendert durchgereicht — raten
    # waere hier schlimmer als nichts tun.
    out: list[str] = []
    gesehen: set[tuple[str, ...]] = set()
    i = 0
    while i < len(flags):
        if flags[i] in ("-C", "-Z", "-L", "-l") and i + 1 < len(flags):
            paar = (flags[i], flags[i + 1])
            i += 2
        else:
            paar = (flags[i],)
            i += 1
        if paar in gesehen:
            continue
        gesehen.add(paar)
        out.extend(paar)

    print(" ".join(out))
    return 10 if len(out) != len(flags) else 0


if __name__ == "__main__":
    sys.exit(main())
