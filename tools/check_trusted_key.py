#!/usr/bin/env python3
"""Prüft, ob der private TrustedSAS-Schlüssel zu dem im Kernel eingebetteten öffentlichen passt.

WARUM es das gibt. `kernel/src/trusted_keys.rs` ist **versioniert** und trägt den öffentlichen
Schlüssel dessen, der ihn zuletzt erzeugt hat. Der private Schlüssel unter `keys/` ist es
**nicht** — zu Recht, er ist ein Geheimnis. Wer also einen fremden `trusted_keys.rs` zieht,
während lokal noch ein eigener privater Schlüssel liegt, hat zwei Hälften, die nicht
zusammengehören.

Die Load-Suite prüfte bisher nur, ob der Schlüssel **existiert**. Der Fehlerfall sah dann so
aus (gemessen am 2026-08-01):

    root    : FAILURES (Rejected(Unverified)) -- kein Root-Task

Das ist eine Zeile, die wie ein Testergebnis aussieht und ein Aufbauproblem ist. Genau die
Verwechslung, die dieses Projekt sonst überall vermeidet: der Prüfer meldet Rot, aber aus dem
falschen Grund, und niemand sieht es ihm an.

Rückgabewert:
  0  passt
  1  passt nicht (Aufrufer sollte neu erzeugen)
  2  lässt sich nicht entscheiden (Datei fehlt, `cryptography` fehlt, Format unerwartet)
     -- ausdrücklich NICHT 0: „konnte nicht prüfen" ist kein „ist in Ordnung".
"""
import re
import sys

SCHLUESSEL = "keys/trusted-test.ed25519"
EINGEBETTET = "kernel/src/trusted_keys.rs"
EINTRAG = "trusted-test"


def main() -> int:
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError:
        print("check_trusted_key: python3-cryptography fehlt -- nicht entscheidbar", file=sys.stderr)
        return 2

    try:
        roh = open(SCHLUESSEL, "rb").read()
    except OSError:
        return 1  # fehlt -> neu erzeugen, das ist der bekannte Fall

    if len(roh) < 32:
        print("check_trusted_key: %s ist zu kurz (%d Byte)" % (SCHLUESSEL, len(roh)), file=sys.stderr)
        return 2

    priv = Ed25519PrivateKey.from_private_bytes(roh[:32])
    oeffentlich = priv.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )

    try:
        quelle = open(EINGEBETTET).read()
    except OSError:
        print("check_trusted_key: %s fehlt" % EINGEBETTET, file=sys.stderr)
        return 2

    # Der Eintrag wird über seinen Kommentar gefunden, nicht über die Position: die Datei ist
    # generiert, und ihre Reihenfolge ist keine Zusage.
    treffer = re.search(
        r"//\s*" + re.escape(EINTRAG) + r"\s*\n\s*TrustedKey \{[^}]*?pubkey: \[([^\]]*)\]",
        quelle,
        re.S,
    )
    if not treffer:
        print("check_trusted_key: Eintrag '%s' in %s nicht gefunden" % (EINTRAG, EINGEBETTET),
              file=sys.stderr)
        return 2

    eingebettet = bytes(int(b, 16) for b in re.findall(r"0x([0-9a-fA-F]{2})", treffer.group(1)))
    if len(eingebettet) != 32:
        print("check_trusted_key: eingebetteter Schluessel hat %d statt 32 Byte" % len(eingebettet),
              file=sys.stderr)
        return 2

    if eingebettet == oeffentlich:
        return 0

    print("check_trusted_key: privater Schluessel passt NICHT zum eingebetteten oeffentlichen",
          file=sys.stderr)
    print("  aus %s abgeleitet : %s" % (SCHLUESSEL, oeffentlich.hex()), file=sys.stderr)
    print("  in %s            : %s" % (EINGEBETTET, eingebettet.hex()), file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
