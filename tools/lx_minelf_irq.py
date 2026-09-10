#!/usr/bin/env python3
"""MinELF-IRQ-Generator fuer die E2E-IRQ-Fahrt (Strang 4, Mitteilung 17).

Erzeugt ein GUELTIGES ET_EXEC x86-64 ELF (ein PT_LOAD R+X bei VA 0x20000000,
wie tools/lx_minelf.py), dessen Nutzlast statt `EB FE` den IRQ-Warteweg faehrt:

    BIND_IRQ (26) auf Slot 7/8, Badge 0x1  ->  Ergebnis nach r12
    WAIT (9) auf Slot 8, Frist 200 Ticks    ->  Ergebnis nach r13, Badge nach r14
    kombinierter Seitenfehler: lese [r12*0x1000 + r13]  ->  #PF, FAR traegt BEIDE Codes

Warum ein Fehler als Melder: Eine MinELF-PD hat keinen Kanal (kein Endpoint,
kein Log) — ihr einziges Fenster nach aussen ist, WIE sie stirbt. `el0-trap`
meldet `EC` + `FAR`; EC=0x0e (#PF) sagt "die IRQ-Sonde meldet", FAR decodiert:

    FAR & 0xfff          = WAIT-Ergebnis  (0=OK=geweckt, 25=ERR_TIMEOUT, 1=ERR_BADCAP)
    (FAR >> 12) & 0xfff  = BIND-Ergebnis  (0=OK, 1=ERR_BADCAP, 3=ERR_RIGHTS, 23=ERR_IRQ_FULL)

Beide Adressen liegen in den low 16 MiB — `vspace_map_page_at` schuetzt sie per
FINE_BLOCKS (crates/caprock-hal/src/x86_64/mmu.rs), sie sind in keiner User-VSpace
gemappt. Der Fehler ist damit garantiert, kein Zufall. Faellt das Lesen doch je
durch (unerwartet), landet der Thread in `EB FE` wie Fahrt 5 — "gestartet ohne
Trap" ist dann der Befund, nicht Stille.

Registerlage x86_64 (programs/libcaprock/src/lib.rs `invoke`: rax=Nr, rdi=Cap,
rsi/rdx/r10/r8=MSG0..3, r9=Tag; Rueckgabe rax=Ergebnis, rdi=Badge). r12-r14 sind
keine ABI-Register — der Kernel restauriert sie aus dem Trap-Frame. Kein Stack,
kein Schreiben in die eigene Seite (R+X genuegt).

Aufruf:  python3 tools/lx_minelf_irq.py --out build/diag/lx-minelf-irq.img [--frist TICKS]
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

SYS_BIND_IRQ = 26
SYS_WAIT = 9
IRQ_SLOT = 7
IRQ_NTFN_SLOT = 8
IRQ_BADGE = 0x1


def payload(frist: int) -> bytes:
    p = bytearray()
    # BIND_IRQ: rax=26, rdi=7 (Irq-Cap), rsi=8 (Notification), rdx=1 (Badge).
    p += bytes([0xB8]) + struct.pack("<I", SYS_BIND_IRQ)   # mov eax, 26
    p += bytes([0xBF]) + struct.pack("<I", IRQ_SLOT)       # mov edi, 7
    p += bytes([0xBE]) + struct.pack("<I", IRQ_NTFN_SLOT)  # mov esi, 8
    p += bytes([0xBA]) + struct.pack("<I", IRQ_BADGE)      # mov edx, 1
    p += bytes([0x45, 0x31, 0xD2])  # xor r10d, r10d
    p += bytes([0x45, 0x31, 0xC0])  # xor r8d, r8d
    p += bytes([0x45, 0x31, 0xC9])  # xor r9d, r9d (Tag = 0)
    p += bytes([0xCD, 0x80])        # int 0x80
    p += bytes([0x49, 0x89, 0xC4])  # mov r12, rax (BIND-Ergebnis sichern)
    # WAIT mit Frist: rax=9, rdi=8, rsi=frist (0 = ewig — hier nie 0, s. unten).
    p += bytes([0xB8]) + struct.pack("<I", SYS_WAIT)       # mov eax, 9
    p += bytes([0xBF]) + struct.pack("<I", IRQ_NTFN_SLOT)  # mov edi, 8
    p += bytes([0xBE]) + struct.pack("<I", frist)          # mov esi, frist
    p += bytes([0x31, 0xD2])        # xor edx, edx
    p += bytes([0x45, 0x31, 0xD2])  # xor r10d, r10d
    p += bytes([0x45, 0x31, 0xC0])  # xor r8d, r8d
    p += bytes([0xCD, 0x80])        # int 0x80
    p += bytes([0x49, 0x89, 0xC5])  # mov r13, rax (WAIT-Ergebnis sichern)
    p += bytes([0x49, 0x89, 0xFE])  # mov r14, rdi (WAIT-Badge sichern)
    # Kombinierter Melder: lese [r12*0x1000 + r13] -> #PF mit FAR = beide Codes.
    p += bytes([0x49, 0x8B, 0xC4])  # mov rax, r12
    p += bytes([0x48, 0xC1, 0xE0, 0x0C])  # shl rax, 12
    p += bytes([0x4C, 0x01, 0xE8])  # add rax, r13
    p += bytes([0x0F, 0xB6, 0x00])  # movzx eax, byte [rax]
    p += bytes([0xEB, 0xFE])        # jmp $ (Rueckfall, sollte nie erreicht werden)
    return bytes(p)


def build(frist: int) -> bytes:
    code = payload(frist)
    assert len(code) < PAGE, "Nutzlast passt nicht in eine Seite"
    ehsize = 64
    phentsize = 56
    phnum = 1
    phoff = ehsize
    table_end = phoff + phnum * phentsize  # 120
    p_offset = PAGE
    filesz = PAGE
    total = p_offset + filesz

    v = bytearray(total)
    v[0:4] = b"\x7fELF"
    v[4] = 2  # ELFCLASS64
    v[5] = 1  # ELFDATA2LSB
    v[6] = 1  # EI_VERSION
    struct.pack_into("<H", v, 16, ET_EXEC)
    struct.pack_into("<H", v, 18, EM_X86_64)
    struct.pack_into("<I", v, 20, 1)  # e_version
    struct.pack_into("<Q", v, 24, VA)  # e_entry = Segmentanfang
    struct.pack_into("<Q", v, 32, phoff)
    struct.pack_into("<Q", v, 40, 0)  # e_shoff
    struct.pack_into("<I", v, 48, 0)  # e_flags
    struct.pack_into("<H", v, 52, ehsize)
    struct.pack_into("<H", v, 54, phentsize)
    struct.pack_into("<H", v, 56, phnum)
    struct.pack_into("<H", v, 58, 0)
    struct.pack_into("<H", v, 60, 0)
    struct.pack_into("<H", v, 62, 0)
    base = phoff
    struct.pack_into("<I", v, base + 0, 1)  # PT_LOAD
    struct.pack_into("<I", v, base + 4, 5)  # PF_R|PF_X
    struct.pack_into("<Q", v, base + 8, p_offset)
    struct.pack_into("<Q", v, base + 16, VA)
    struct.pack_into("<Q", v, base + 24, VA)
    struct.pack_into("<Q", v, base + 32, filesz)
    struct.pack_into("<Q", v, base + 40, filesz)
    struct.pack_into("<Q", v, base + 48, PAGE)
    v[p_offset:p_offset + len(code)] = code
    for i in range(p_offset + len(code), p_offset + filesz):
        v[i] = 0x90  # NOP-Padding wie Fahrt 5
    assert table_end <= p_offset
    return bytes(v)


# Erwartete Nutzlast bei Frist 200 (0xC8) — der Host-Selbstcheck vergleicht exakt.
ERWARTET_FRIST200 = bytes.fromhex(
    "b81a000000" "bf07000000" "be08000000" "ba01000000"
    "4531d2" "4531c0" "4531c9" "cd80" "4989c4"
    "b809000000" "bf08000000" "bec8000000"
    "31d2" "4531d2" "4531c0" "cd80" "4989c5" "4989fe"
    "498bc4" "48c1e00c" "4c01e8" "0fb600" "ebfe"
)


def selbstcheck(img: bytes, frist: int) -> None:
    assert img[:4] == b"\x7fELF", "kein ELF"
    assert struct.unpack_from("<H", img, 16)[0] == ET_EXEC, "kein ET_EXEC"
    assert struct.unpack_from("<H", img, 18)[0] == EM_X86_64, "kein EM_X86_64"
    assert struct.unpack_from("<Q", img, 24)[0] == VA, "entry != VA"
    assert struct.unpack_from("<Q", img, 64 + 16)[0] == VA, "PT_LOAD VA falsch"
    code = img[PAGE:PAGE + len(ERWARTET_FRIST200)]
    if frist == 200:
        assert code == ERWARTET_FRIST200, (
            "Nutzlast weicht ab (Assembler-Fehler): "
            + code.hex() + " != " + ERWARTET_FRIST200.hex()
        )
    else:
        assert code.endswith(bytes([0xEB, 0xFE])), "kein EB FE am Nutzlastende"
        assert b"\xcd\x80" in code, "kein int 0x80 in der Nutzlast"


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="MinELF-IRQ-Generator (E2E-IRQ-Fahrt)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--frist", type=int, default=200,
                    help="WAIT-Frist in Timer-Ticks (nie 0: ohne Frist hinge die Sonde)")
    a = ap.parse_args(argv)
    if not (0 < a.frist <= 0xFFFFFFFF):
        print("minelf-irq: Frist muss 1..2^32-1 sein (0 hinge ewig)", file=sys.stderr)
        return 2
    img = build(a.frist)
    selbstcheck(img, a.frist)
    with open(a.out, "wb") as f:
        f.write(img)
    print(f"minelf-irq: {a.out} ({len(img)} B, VA 0x{VA:08x}, "
          f"BIND_IRQ(7,8,1) + WAIT(8,frist={a.frist}) + #PF-Melder)")
    print(f"minelf-irq: sha256={hashlib.sha256(img).hexdigest()}")
    print("minelf-irq: Host-Selbstcheck ok "
          f"(ET_EXEC x86-64, entry=VA, {len(payload(a.frist))} B Nutzlast exakt)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
