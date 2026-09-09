#!/usr/bin/env python3
"""lx_symbols.py — coverage of the S0 A-header contract table.

Reads the machine-readable table in lx_schablonen.md and prints the
A-symbol coverage (M1 approximation). With --nm <file>, checks the
undefined symbols of an nm -u listing (or a plain one-symbol-per-line
file) against the table and lists the unknown ones.

Stdlib only. Exit 0 (measurement, not a gate).
"""

import argparse
import re
import sys
from pathlib import Path

TABLE = Path(__file__).with_name("lx_schablonen.md")

NM_RE = re.compile(r"^\s*(?:[0-9a-fA-F]+\s+)?[UuWw]\s+([A-Za-z_][\w.]*)")
PLAIN_RE = re.compile(r"^\s*([A-Za-z_][\w.]*)\s*$")


def parse_table(path):
    """Return list of (symbols, klasse, schema, art, status)."""
    rows = []
    lines = Path(path).read_text(encoding="utf-8").splitlines()
    head = None
    for i, ln in enumerate(lines):
        low = ln.lower()
        if ln.startswith("|") and "linux" in low and "status" in low:
            head = i
            break
    if head is None:
        raise SystemExit("lx_symbols: no contract table found in %s" % path)
    for ln in lines[head + 1:]:
        s = ln.strip()
        if not s.startswith("|"):
            if rows:
                break
            continue
        cells = [c.strip() for c in s.strip("|").split("|")]
        if len(cells) < 5:
            continue
        if all(set(c) <= set("-: ") for c in cells):
            continue  # separator line
        symbols = [x.strip("` ") for x in cells[0].split(",") if x.strip("` ")]
        rows.append((symbols, cells[1].strip().upper()[:1],
                     cells[2].strip(), cells[3].strip(), cells[4].strip()))
    return rows


def coverage(rows):
    """(covered_aliases, total_a_aliases, per_status, b_symbols)."""
    per_status = {}
    covered = total = 0
    b_syms = []
    for symbols, klasse, _schema, _art, status in rows:
        per_status[status] = per_status.get(status, 0) + len(symbols)
        if klasse == "B":
            b_syms.extend(symbols)
            continue
        if klasse != "A":
            continue
        total += len(symbols)
        if status.startswith("vorhanden"):
            covered += len(symbols)
    return covered, total, per_status, b_syms


def read_nm(path):
    syms = []
    for ln in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
        m = NM_RE.match(ln)
        if m:
            syms.append(m.group(1))
            continue
        if ln.strip().startswith(("|", "#")) or not ln.strip():
            continue
        m = PLAIN_RE.match(ln)
        if m:
            syms.append(m.group(1))
    # dedupe, keep order
    return list(dict.fromkeys(syms))


def match(sym, aliases):
    for a in aliases:
        if a.endswith("*"):
            if sym.startswith(a[:-1]):
                return True
        elif sym == a:
            return True
    return False


def main():
    ap = argparse.ArgumentParser(description="S0 contract coverage (M1 approx.)")
    ap.add_argument("--nm", metavar="DATEI",
                    help="nm -u listing or one-symbol-per-line file")
    ap.add_argument("--tabelle", default=str(TABLE))
    args = ap.parse_args()

    rows = parse_table(args.tabelle)
    covered, total, per_status, b_syms = coverage(rows)
    pct = 100.0 * covered / total if total else 0.0
    print("LX A-Abdeckung: %d/%d (%.1f%%) [formell %d, ohne Frist %d, fehlt Kernel %d]"
          % (covered, total, pct,
             per_status.get("vorhanden:formell benannt", 0),
             per_status.get("vorhanden:noch ohne Frist", 0),
             per_status.get("fehlt:Kernel", 0)))
    print("LX B-Symbole (uebersetzt, keine Schablone): %d (%s)"
          % (len(b_syms), ", ".join(b_syms)))

    if args.nm:
        undef = read_nm(args.nm)
        a_aliases, b_aliases = [], []
        for symbols, klasse, _s, _a, status in rows:
            if klasse == "A" and status.startswith("vorhanden"):
                a_aliases.extend(symbols)
            elif klasse == "B":
                b_aliases.extend(symbols)
        hit_a = [s for s in undef if match(s, a_aliases)]
        hit_b = [s for s in undef if not match(s, a_aliases) and match(s, b_aliases)]
        unknown = [s for s in undef if s not in hit_a and s not in hit_b]
        print("LX nm: %d undefiniert — A %d, B %d, unbekannt %d"
              % (len(undef), len(hit_a), len(hit_b), len(unknown)))
        for s in unknown:
            print("LX unbekannt: %s" % s)
    return 0


if __name__ == "__main__":
    sys.exit(main())
