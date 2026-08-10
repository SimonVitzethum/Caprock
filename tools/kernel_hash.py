#!/usr/bin/env python3
"""Caprock — den **Kernel-Code-Hash** aus einem ELF berechnen (A-1.3).

Der Hash bindet das System-Manifest an genau diesen Kernel: `sign_manifest.py` schreibt ihn in die
signierte Nachricht, der Kernel berechnet ihn beim Hochlauf aus seinem eigenen Speicher und
vergleicht. Ein Manifest, das fuer einen anderen Kernel ausgestellt wurde, wird damit abgewiesen --
sonst koennte man Kernel und Manifest getrennt tauschen, und die Zuteilung von Autoritaet haenge an
zwei Dateien statt an einer.

**Definition (eingefroren):**  SHA-256 ueber `[__text_start, __rodata_end)`.

Warum genau diese Spanne, und nicht "das ganze Image":

* Sie ist zur Laufzeit **unveraenderlich**. `.text` ist R-X, `.rodata` R--, beides unter CR0.WP
  bzw. dem ARM-Pendant. Ein Hash ueber `.data`/`.bss` waere schon nach dem ersten Boot-Schritt ein
  anderer -- der Kernel koennte ihn gar nicht reproduzieren.
* Sie ist auf beiden Architekturen **zusammenhaengend** (die Linker-Skripte legen sie so; auf
  aarch64 liegt `.user_text` dazwischen, auf x86 dahinter -- beides innerhalb bzw. ausserhalb der
  Spanne, aber in sich geschlossen).
* Sie ist im ELF **byte-identisch** zu dem, was spaeter im Speicher steht (identity-geladen, keine
  Relokation): das Werkzeug hier und der Kernel sehen dieselben Bytes.

Was sie NICHT abdeckt: das Boot-Trampolin (`.boot`), Daten und BSS. Das ist bewusst und muss beim
Lesen des Ergebnisses mitgedacht werden -- der Hash bezeugt den Kernel-CODE, nicht das Boot-Image.

Aufruf:  tools/kernel_hash.py <kernel.elf>        -> Hex-Digest auf stdout
"""
import hashlib
import sys


def _u(b, off, n):
    return int.from_bytes(b[off:off + n], "little")


def elf_symbols(data):
    """{name: value} aller Symbole eines ELF64 (little-endian). Minimal, ohne Fremdbibliothek."""
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise SystemExit("kernel_hash: kein little-endian ELF64")
    e_shoff = _u(data, 0x28, 8)
    e_shentsize = _u(data, 0x3A, 2)
    e_shnum = _u(data, 0x3C, 2)
    secs = []
    for i in range(e_shnum):
        o = e_shoff + i * e_shentsize
        secs.append({
            "type": _u(data, o + 4, 4),
            "offset": _u(data, o + 0x18, 8),
            "size": _u(data, o + 0x20, 8),
            "link": _u(data, o + 0x28, 4),
            "entsize": _u(data, o + 0x38, 8),
        })
    syms = {}
    for s in secs:
        if s["type"] != 2 or s["entsize"] == 0:  # SHT_SYMTAB
            continue
        stro = secs[s["link"]]["offset"]
        for k in range(s["size"] // s["entsize"]):
            o = s["offset"] + k * s["entsize"]
            name_off = _u(data, o, 4)
            value = _u(data, o + 8, 8)
            end = data.index(b"\0", stro + name_off)
            name = data[stro + name_off:end].decode("utf-8", "replace")
            if name:
                syms[name] = value
    return syms


def vaddr_to_bytes(data, start, end):
    """Die Bytes im virtuellen Bereich `[start, end)` aus den PT_LOAD-Segmenten zusammensetzen."""
    e_phoff = _u(data, 0x20, 8)
    e_phentsize = _u(data, 0x36, 2)
    e_phnum = _u(data, 0x38, 2)
    out = bytearray(end - start)
    covered = bytearray(end - start)
    for i in range(e_phnum):
        o = e_phoff + i * e_phentsize
        if _u(data, o, 4) != 1:  # PT_LOAD
            continue
        p_offset = _u(data, o + 0x08, 8)
        p_vaddr = _u(data, o + 0x10, 8)
        p_filesz = _u(data, o + 0x20, 8)
        lo, hi = max(start, p_vaddr), min(end, p_vaddr + p_filesz)
        if hi <= lo:
            continue
        src = p_offset + (lo - p_vaddr)
        out[lo - start:hi - start] = data[src:src + (hi - lo)]
        for j in range(lo - start, hi - start):
            covered[j] = 1
    if not all(covered):
        raise SystemExit(
            "kernel_hash: die Spanne [__text_start, __rodata_end) liegt nicht vollstaendig in "
            "PT_LOAD-Segmenten mit Dateiinhalt -- das Linker-Skript passt nicht zur Definition"
        )
    return bytes(out)


def kernel_code_hash(path):
    with open(path, "rb") as f:
        data = f.read()
    syms = elf_symbols(data)
    try:
        start, end = syms["__text_start"], syms["__rodata_end"]
    except KeyError as e:
        raise SystemExit(f"kernel_hash: Symbol {e} fehlt im ELF")
    if end <= start:
        raise SystemExit("kernel_hash: __rodata_end <= __text_start")
    return hashlib.sha256(vaddr_to_bytes(data, start, end)).digest()


def main(argv):
    if len(argv) != 2:
        sys.stderr.write("usage: kernel_hash.py <kernel.elf>\n")
        return 2
    print(kernel_code_hash(argv[1]).hex())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
