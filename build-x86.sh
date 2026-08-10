#!/usr/bin/env bash
# Baut den Caprock-Kernel fuer x86_64 (Branch arch/x86_64): eingebauter Bare-Metal-Target
# x86_64-unknown-none, Multiboot1-Image (Linker kernel/x86_64-link.ld). Ergebnis:
#   build/target/x86_64-unknown-none/release/caprock-kernel
set -euo pipefail
cd "$(dirname "$0")"

# ================================================================================================
# DIE LINKERFLAGS DUERFEN NICHT DOPPELT STEHEN -- und in einem Agenten-Worktree TUN SIE ES
# ================================================================================================
#
# Cargo liest `.cargo/config.toml` aus JEDEM Vorfahrenverzeichnis und **haengt Array-Werte
# aneinander**. Ein Worktree unter `<repo>/.claude/worktrees/<id>` liegt innerhalb des Hauptbaums,
# erbt dessen Konfiguration also ein zweites Mal -- `-Tkernel/x86_64-link.ld` steht danach ZWEIMAL
# auf der Linkerzeile, lld wertet den `SECTIONS`-Block zweimal aus, und heraus faellt ein Abbild
# mit sieben leeren Doppel-Sektionen, mit allen Linkersymbolen auf `0x100000` und mit einem
# nullgrossen LOAD-Segment. Das ist die gemessene Ursache des Eintrags „das nullgrosse
# LOAD-Duplikat" (todo.md) -- ein Fehler der BAUUMGEBUNG, nicht der Quelle. Begruendung und
# Beleg stehen in `tools/rustflags-entdoppeln.py`.
#
# Repariert wird **laut**: eine stille Reparatur waere dieselbe Krankheit wie das stille Mischen.
ENTDOPPELT="$(python3 tools/rustflags-entdoppeln.py x86_64-unknown-none)" && RC=0 || RC=$?
if [ "${RC:-0}" = "10" ]; then
    echo "rustflags: DOPPELT geerbt (Worktree liegt im Hauptbaum) -- entdoppelt auf: $ENTDOPPELT"
    echo "rustflags:   ohne diese Zeile linkt lld den SECTIONS-Block ZWEIMAL; s. tools/rustflags-entdoppeln.py"
    # **`RUSTFLAGS` und nicht `CARGO_TARGET_<T>_RUSTFLAGS`** -- gemessen: die zielspezifische
    # Variable wird von Cargo mit der Konfiguration MITGEMISCHT (danach standen die Flags
    # dreifach da, das Abbild wurde schlechter statt besser). `RUSTFLAGS` dagegen ERSETZT
    # `build.rustflags` und `target.*.rustflags` vollstaendig -- das ist der einzige Weg, der
    # das Anhaengen aus den Vorfahren-Konfigurationen wirklich abschaltet.
    export RUSTFLAGS="$ENTDOPPELT"
fi

rustup run nightly cargo build --release --target x86_64-unknown-none -p caprock-kernel "$@"
ELF=build/target/x86_64-unknown-none/release/caprock-kernel
# QEMUs Multiboot1-Loader akzeptiert nur ELF32. Der ELF-Container wird auf ELF32 downgecastet
# (alle Lade-/Entry-Adressen liegen < 4 GiB; der Code bleibt 64-bit, QEMU tritt am 32-bit-`_start`
# ein, das Trampolin schaltet in den Long Mode). -> *.mb32 ist das bootbare QEMU-Image.
objcopy -I elf64-x86-64 -O elf32-i386 "$ELF" "$ELF.mb32"

# ================================================================================================
# DER MULTIBOOT-HEADER MUSS IN DEN ERSTEN 8192 DATEI-BYTES LIEGEN -- und das wird GEPRUEFT
# ================================================================================================
#
# Die Bedingung steht seit jeher im Linkerskript ("in den ersten 8 KiB der Datei -> .multiboot
# zuerst") -- und **nichts hat sie durchgesetzt**. Am 2026-08-10 hat das einen halben Tag gekostet:
# der Baum baute fehlerfrei, `objcopy` meldete nichts, und QEMU sagte nur
#
#     Error loading uncompressed kernel without PVH ELF Note
#
# mit einer 0-Byte-Logdatei. Zwei Agenten haben es unabhaengig als "die Suite laeuft in diesem Baum
# nicht" gemeldet, ich habe es zuerst der falschen Ursache zugeschrieben.
#
# **Die Ursache, gemessen:** `lld` legt ein NULLGROSSES LOAD-Segment an, das die vaddr eines
# anderen dupliziert (`.data`, zweimal). GNU objcopy 2.46.1 bildet es nicht ab und legt `.boot`
# danach ausserhalb jedes Segments -- bei Dateioffset ~740 000 statt 4096. Ob das kippt, haengt an
# den GROESSEN der Sektionen; derselbe Commit kann vorher gebaut und nachher nicht mehr booten.
# Genau deshalb ist eine Pruefung noetig und ein Kommentar nicht: die Bedingung ist nicht stabil
# verletzt, sondern **grenzwertig**.
#
# Ein Bauwerkzeug, das ein unbootbares Abbild ausliefert, ist die Bauzeit-Fassung von "Schweigen
# als Erfolg".
MB_MAGIC_OFF="$(python3 - "$ELF.mb32" <<'PYEOF'
import struct, sys
d = open(sys.argv[1], 'rb').read()
# Multiboot-1-Magic, 4-Byte-ausgerichtet gesucht -- so sucht der Bootloader auch.
for off in range(0, min(len(d), 1 << 20), 4):
    if d[off:off+4] == b'\x02\xb0\xad\x1b':
        print(off); break
else:
    print(-1)
PYEOF
)"
if [ "$MB_MAGIC_OFF" = "-1" ]; then
    echo "BUILD FAILED: im mb32 steht ueberhaupt kein Multiboot-Magic (0x1BADB002)." >&2
    exit 1
fi
if [ "$MB_MAGIC_OFF" -ge 8192 ]; then
    echo "BUILD FAILED: der Multiboot-Header liegt bei Dateioffset $MB_MAGIC_OFF, erlaubt sind < 8192." >&2
    echo "  QEMU laedt dieses Abbild NICHT und meldet nur 'Error loading uncompressed kernel" >&2
    echo "  without PVH ELF Note' bei leerer Logdatei -- ein Aufbaufehler, der wie ein Haenger aussieht." >&2
    echo "  Ursache (2026-08-10 gemessen): ein NULLGROSSES LOAD-Segment von lld, das die vaddr eines" >&2
    echo "  anderen dupliziert; GNU objcopy bildet es nicht ab und schiebt .boot ans Dateiende." >&2
    readelf -lW "$ELF" | grep -E "^  LOAD" | sed 's/^/    /' >&2
    exit 1
fi
echo "multiboot: Header bei Dateioffset $MB_MAGIC_OFF (< 8192 -- QEMU findet ihn)"
echo "x86_64-Kernel: $ELF (ELF64) + $ELF.mb32 (Multiboot-ELF32 fuer 'qemu-system-x86_64 -kernel')"
