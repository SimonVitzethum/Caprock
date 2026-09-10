#!/usr/bin/env python3
"""MinELF-Generator fuer Fahrt 5 (LXPD-Kernelpfad-Nachweis).

Erzeugt ein minimales, GUELTIGES ET_EXEC x86-64 ELF mit genau einem
PT_LOAD-Segment bei VA 0x20000000 (also oberhalb der low 16 MiB, die
`vspace_map_page_at` per FINE_BLOCKS=8 schuetzt — s.
crates/caprock-hal/src/x86_64/mmu.rs:1204-1210), Entry = Segmentanfang.

Segmentinhalt: `EB FE` (jmp $ — Endlosschleife, beweist Ausfuehrung ohne
Syscalls/Geraete), Rest der Seite NOP (`0x90`, falls je ein Schritt
daneben geht). Alles seitenausgerichtet (VA, Offset, Align 0x1000),
p_memsz == p_filesz (kein .bss), e_machine = EM_X86_64 (0x3E) — der
caprock-loader-Parser (crates/caprock-loader/src/elf.rs) verlangt
ET_EXEC + EXPECTED_MACHINE + seitenausgerichtete p_vaddr.

Aufruf:  python3 tools/lx_minelf.py --out build/diag/lx-minelf.img
Stdout:  Groesse + SHA-256 (wird Manifest-Hash von Eintrag 7).
"""
import argparse
import hashlib
import struct
import sys

VA = 0x20000000
PAGE = 0x1000
EM_X86_64 = 0x3E
ET_EXEC = 2


def build() -> bytes:
    ehsize = 64
    phentsize = 56
    phnum = 1
    phoff = ehsize
    table_end = phoff + phnum * phentsize  # 120
    p_offset = PAGE  # 0x1000, seitenausgerichtet
    filesz = PAGE  # eine Seite Code
    total = p_offset + filesz  # 0x2000

    v = bytearray(total)
    # e_ident
    v[0:4] = b"\x7fELF"
    v[4] = 2  # ELFCLASS64
    v[5] = 1  # ELFDATA2LSB
    v[6] = 1  # EI_VERSION
    # ELF64-Header (little-endian)
    struct.pack_into("<H", v, 16, ET_EXEC)
    struct.pack_into("<H", v, 18, EM_X86_64)
    struct.pack_into("<I", v, 20, 1)  # e_version
    struct.pack_into("<Q", v, 24, VA)  # e_entry = Segmentanfang
    struct.pack_into("<Q", v, 32, phoff)  # e_phoff
    struct.pack_into("<Q", v, 40, 0)  # e_shoff
    struct.pack_into("<I", v, 48, 0)  # e_flags
    struct.pack_into("<H", v, 52, ehsize)
    struct.pack_into("<H", v, 54, phentsize)
    struct.pack_into("<H", v, 56, phnum)
    struct.pack_into("<H", v, 58, 0)  # e_shentsize
    struct.pack_into("<H", v, 60, 0)  # e_shnum
    struct.pack_into("<H", v, 62, 0)  # e_shstrndx
    # Program-Header: ein PT_LOAD, R+X
    base = phoff
    struct.pack_into("<I", v, base + 0, 1)  # p_type = PT_LOAD
    struct.pack_into("<I", v, base + 4, 5)  # p_flags = PF_R|PF_X
    struct.pack_into("<Q", v, base + 8, p_offset)
    struct.pack_into("<Q", v, base + 16, VA)  # p_vaddr
    struct.pack_into("<Q", v, base + 24, VA)  # p_paddr
    struct.pack_into("<Q", v, base + 32, filesz)
    struct.pack_into("<Q", v, base + 40, filesz)  # p_memsz == filesz
    struct.pack_into("<Q", v, base + 48, PAGE)  # p_align
    # Segmentinhalt: EB FE + NOP-Padding
    v[p_offset] = 0xEB
    v[p_offset + 1] = 0xFE
    for i in range(p_offset + 2, p_offset + filesz):
        v[i] = 0x90
    assert table_end <= p_offset  # Header-Tabelle ueberschneidet kein Segment
    return bytes(v)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="MinELF-Generator (Fahrt 5)")
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    img = build()
    with open(a.out, "wb") as f:
        f.write(img)
    print(f"minelf: {a.out} ({len(img)} B, VA 0x{VA:08x}, entry 0x{VA:08x}, 1x PT_LOAD R+X)")
    print(f"minelf: sha256={hashlib.sha256(img).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
