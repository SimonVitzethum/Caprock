#!/usr/bin/env python3
"""Caprock Z7 — die SW-PCR-Messkette OFFLINE nachrechnen (kein Boot, kein Schreiben).

Was das Werkzeug prueft (gegen `kernel/src/loader.rs` und `programs/attest` gelesen):

* `demo` — die Formel an sich: Extend(`prev || pid(le) || dom(le) || bild`) ueber zwei
  Stufen gegen Goldwerte (derselben, die `programs/attest` faehrt). Faellt, sobald die
  Formel hier von der im Kernel abweicht.
* `kette` — einen PD-Bericht (JSON) nachrechnen: jedes Glied, Kopf gegen Bericht,
  Anzahl gegen Liste, Nonce gegen Erwartung (Replay), Modell-Signatur gegen Key.
* `log` — die `messkette:`-Zeilen eines Kernel-Logs auf Konsistenz lesen (Anzahl,
  `verworfen == 0`, ALL PASS/FAILURES-Wort). Die Log-Praefixe (je 4 Byte) tragen KEINE
  vollstaendige Verifikation — dafuer braucht es den JSON-Bericht der PD (`kette`).

Was es NICHT prueft (ehrlich benannt): ohne TPM gibt es keinen Hardware-Anker —
bestanden heisst „die Kette rechnet sich", nicht „die Maschine ist echt". Die
Ed25519-Produktionssignatur der PD prueft der Tenant gegen den Manifest-Pubkey
(s. `tools/lx_attest.md`); was hier als `--modell-key` laeuft, ist die TEST-MAC aus
`programs/attest` (`TestSchluessel`), kein Ed25519.

JSON-Format (`kette`):

    {"genesis": "<64 hex>",
     "eintraege": [{"program_id": 7, "domain": 2, "image_hash": "<64 hex>"}, ...],
     "bericht": {"pcr": "<64 hex>", "anzahl": 1, "nonce": 41},
     "nonce_erwartet": 41,
     "modell_key": "<64 hex>",   # optional; ohne ihn entfaellt die Signaturpruefung
     "signatur": "<128 hex>"}    # Pflicht, sobald modell_key steht

Aufruf:
  tools/lx_attest_check.py demo
  tools/lx_attest_check.py kette bericht.json
  tools/lx_attest_check.py log build/diag/lauf.log
Rueckgabe 0 = bestanden, 1 = gebrochen/abgelehnt, 2 = Bedienfehler.
"""
import hashlib
import json
import re
import sys

# Goldwerte (72-Byte-Verkettung, s. Modul-Doku): GENESIS = Bytes 0..31,
# P1 = extend(GENESIS, 7, 2, [0x11]*32), P2 = extend(P1, 8, 1, [0x22]*32).
_DEMO_GENESIS = bytes(range(32))
_DEMO_P1 = "bd33b6a220a722068ed27ff968111c607725f77bb5e8e4d6ebcb03bcf2df2f68"
_DEMO_P2 = "b76c12229b0ba4c157c400867286f388db63f95f9ac39f0891144417fb97ab65"


def extend(prev: bytes, pid: int, dom: int, bild: bytes) -> bytes:
    """Ein Extend-Schritt (Spiegel von `loader::messkette_verlaengern`)."""
    assert len(prev) == 32 and len(bild) == 32
    buf = bytes(prev) + pid.to_bytes(4, "little") + dom.to_bytes(4, "little") + bytes(bild)
    assert len(buf) == 72
    return hashlib.sha256(buf).digest()


def modell_signatur(key: bytes, nachricht: bytes) -> bytes:
    """Modell-MAC aus `programs/attest` (`TestSchluessel`) — TEST, kein Ed25519."""
    return hashlib.sha256(key + nachricht).digest() + hashlib.sha256(nachricht + key).digest()


def bericht_bytes(pcr: bytes, anzahl: int, nonce: int) -> bytes:
    """Berichts-Codec aus `programs/attest` (48 Byte, fest)."""
    return bytes(pcr) + anzahl.to_bytes(8, "little") + nonce.to_bytes(8, "little")


def _hex(name, wert, laenge):
    try:
        b = bytes.fromhex(wert)
    except (ValueError, TypeError):
        raise SystemExit(f"lx_attest: {name} ist kein Hex ({wert!r})")
    if len(b) != laenge:
        raise SystemExit(f"lx_attest: {name} ist {len(b)} Byte, verlangt {laenge}")
    return b


def cmd_demo() -> int:
    p1 = extend(_DEMO_GENESIS, 7, 2, bytes([0x11] * 32))
    p2 = extend(p1, 8, 1, bytes([0x22] * 32))
    ok1 = p1.hex() == _DEMO_P1
    ok2 = p2.hex() == _DEMO_P2
    print(f"lx_attest: demo Stufe 1 {'OK' if ok1 else 'ABWEICHUNG'} (P1={p1.hex()[:16]}..)")
    print(f"lx_attest: demo Stufe 2 {'OK' if ok2 else 'ABWEICHUNG'} (P2={p2.hex()[:16]}..)")
    print(f"lx_attest: demo {'ALL PASS' if ok1 and ok2 else 'FAILURES'} "
          f"(Formel gegen Goldwerte, nicht gegen sich selbst)")
    return 0 if ok1 and ok2 else 1


def cmd_kette(pfad: str) -> int:
    with open(pfad, encoding="utf-8") as f:
        try:
            d = json.load(f)
        except json.JSONDecodeError as e:
            raise SystemExit(f"lx_attest: kein JSON ({e})")
    genesis = _hex("genesis", d.get("genesis"), 32)
    eintraege = d.get("eintraege")
    if not isinstance(eintraege, list):
        raise SystemExit("lx_attest: `eintraege` fehlt oder ist keine Liste")
    bericht = d.get("bericht")
    if not isinstance(bericht, dict):
        raise SystemExit("lx_attest: `bericht` fehlt")
    # 3. Kette — erst die Glieder (Bruch mit Index, wie `kette_pruefen`).
    prev = genesis
    for i, e in enumerate(eintraege):
        pcr = extend(prev, int(e["program_id"]), int(e["domain"]),
                     _hex(f"eintraege[{i}].image_hash", e.get("image_hash"), 32))
        prev = pcr
    kopf = prev
    # Bericht halten: Kopf, Anzahl, Nonce.
    will_pcr = _hex("bericht.pcr", bericht.get("pcr"), 32)
    will_anzahl = int(bericht.get("anzahl", -1))
    will_nonce = int(bericht.get("nonce", -1))
    if kopf != will_pcr:
        print(f"lx_attest: KETTE GEBROCHEN (Kopfweiche: gerechnet {kopf.hex()[:16]}.., "
              f"berichtet {will_pcr.hex()[:16]}..)")
        return 1
    if will_anzahl != len(eintraege):
        print(f"lx_attest: KETTE GEBROCHEN (Anzahlweiche: berichtet {will_anzahl}, "
              f"Liste {len(eintraege)})")
        return 1
    print(f"lx_attest: Kette OK ({len(eintraege)} Glieder, Kopf {kopf.hex()[:16]}..)")
    # 2. Nonce — der Replay-Fall ist ketten-gueltig und trotzdem abzulehnen.
    erwartet = d.get("nonce_erwartet")
    if erwartet is None:
        raise SystemExit("lx_attest: `nonce_erwartet` fehlt (ohne Erwartung kein Replay-Urteil)")
    if will_nonce != int(erwartet):
        print(f"lx_attest: REPLAY ABGELEHNT (erwartet {erwartet}, erhalten {will_nonce})")
        return 1
    print(f"lx_attest: Nonce OK ({will_nonce})")
    # 1. Signatur — im Bericht verstanden: sie steht in der Pruefreihenfolge VOR Nonce
    # und Kette (`programs/attest`); hier laeuft sie zuletzt, weil Kette+Nonce ohne Key
    # pruefbar sind und ein fehlender Key dann kein Fehlschlag ist, sondern ein Weglassen
    # mit Ansage. Mit Key gilt die Reihenfolge des Protokolls (Signatur zuerst).
    key_hex = d.get("modell_key")
    if key_hex is None:
        print("lx_attest: ohne modell_key keine Signaturpruefung (Kette + Nonce belegt) "
              "— kein Ed25519-Urteil")
        print("lx_attest: ALL PASS (Kette + Nonce; Signatur WEGGELASSEN)")
        return 0
    key = _hex("modell_key", key_hex, 32)
    sig = _hex("signatur", d.get("signatur"), 64)
    soll = modell_signatur(key, bericht_bytes(will_pcr, will_anzahl, will_nonce))
    if soll != sig:
        print("lx_attest: SIGNATUR UNGUELTIG (Modell-MAC bricht — falscher Key oder Faelschung)")
        return 1
    print("lx_attest: Signatur OK (Modell-MAC — TEST, kein Ed25519-Urteil)")
    print("lx_attest: ALL PASS (Kette + Nonce + Modell-Signatur)")
    return 0


def cmd_log(pfad: str) -> int:
    with open(pfad, encoding="utf-8", errors="replace") as f:
        text = f.read()
    anker = re.findall(r"messkette: Anker Kernel-Code-Hash ([0-9a-f]{8})\.\., (\d+) Eintrag.*?, (\d+) verworfen", text)
    glieder = re.findall(r"messkette:   \[(\d+)\] dom=(\d+) bild=([0-9a-f]{8})\.\. pcr=([0-9a-f]{8})\.\.", text)
    bilanz = re.findall(r"messkette: (ALL PASS|FAILURES)", text)
    if not anker:
        print("lx_attest: keine messkette-Ankerzeile im Log (Kernel ohne Z7 oder alter Bau)")
        return 1
    a_hash, a_n, a_verworfen = anker[-1]
    ok = True
    if int(a_verworfen) != 0:
        print(f"lx_attest: {a_verworfen} Messungen VERWORFEN — die Kette ist unvollstaendig")
        ok = False
    if int(a_n) != len(glieder):
        print(f"lx_attest: Anzahlweiche (Anker {a_n}, Zeilen {len(glieder)})")
        ok = False
    print(f"lx_attest: Log Anker {a_hash}.., {a_n} Eintraege, {a_verworfen} verworfen, "
          f"Bilanz {bilanz[-1] if bilanz else 'keine'}")
    print("lx_attest: Praefixe (je 4 Byte) sind Diagnose, kein Beleg — vollstaendig prueft `kette`")
    wort_ok = bilanz and bilanz[-1] == "ALL PASS"
    print(f"lx_attest: {'ALL PASS' if ok and wort_ok else 'FAILURES'} (Log-Konsistenz)")
    return 0 if ok and wort_ok else 1


def haupt(argv) -> int:
    if len(argv) < 2 or argv[1] in ("-h", "--help", "help"):
        print("Caprock Z7 — SW-PCR-Messkette offline nachrechnen (kein Boot, kein Schreiben).")
        print("\n".join([l for l in __doc__.strip().splitlines() if l.startswith(("  tools/", "  lx_attest", "Rueckgabe"))]))
        return 2 if len(argv) < 2 else 0
    if argv[1] == "demo":
        return cmd_demo()
    if argv[1] == "kette" and len(argv) == 3:
        return cmd_kette(argv[2])
    if argv[1] == "log" and len(argv) == 3:
        return cmd_log(argv[2])
    raise SystemExit(f"lx_attest: unbekannter Aufruf ({' '.join(argv[1:])}) — --help")


if __name__ == "__main__":
    sys.exit(haupt(sys.argv))
