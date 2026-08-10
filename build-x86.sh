#!/usr/bin/env bash
# Baut den Caprock-Kernel fuer x86_64 (Branch arch/x86_64): eingebauter Bare-Metal-Target
# x86_64-unknown-none, Multiboot1-Image (Linker kernel/x86_64-link.ld). Ergebnis:
#   build/target/x86_64-unknown-none/release/caprock-kernel
set -euo pipefail
cd "$(dirname "$0")"
rustup run nightly cargo build --release --target x86_64-unknown-none -p caprock-kernel "$@"
ELF=build/target/x86_64-unknown-none/release/caprock-kernel
# QEMUs Multiboot1-Loader akzeptiert nur ELF32. Der ELF-Container wird auf ELF32 downgecastet
# (alle Lade-/Entry-Adressen liegen < 4 GiB; der Code bleibt 64-bit, QEMU tritt am 32-bit-`_start`
# ein, das Trampolin schaltet in den Long Mode). -> *.mb32 ist das bootbare QEMU-Image.
#
# ================================================================================================
# OFFEN, und es blockiert JEDE x86-QEMU-Messung: binutils 2.46.1 erzeugt hier ein Image, das
# QEMU nicht laedt. Gemessen am 2026-08-10, auf dem UNVERAENDERTEN Stand 86ad033.
# ================================================================================================
#
# Fehlerbild:
#     qemu-system-x86_64: Error loading uncompressed kernel without PVH ELF Note
#     -> Logdatei 0 Byte, die Suite meldet korrekt „KEIN OUTPUT ... das ist KEIN Testergebnis".
#
# Diagnose (gemessen, nicht vermutet):
#   * Der ELF64 ist in Ordnung: Multiboot-Magic bei Dateioffset **4096**, also innerhalb der
#     8 KiB, die QEMU absucht.
#   * Der ELF32 aus `objcopy` hat es bei **741376** -- weit ausserhalb. QEMU findet keinen
#     Multiboot-Header, faellt auf den Linux-/PVH-Pfad zurueck und bricht mit obiger Zeile ab.
#   * Der Grund steht in `objcopy`s eigener Meldung:
#         section `.user_data' can't be allocated in segment 5
#         warning: allocated section `.boot' not in segment
#     Segment 5 des ELF64 (`readelf -l`) ist ein LOAD mit FileSiz 0 ueber die NULLGROSSEN
#     Duplikate `.text .rodata .user_text .user_data .aptramp_data`, die lld zusaetzlich anlegt.
#     2.46.1 weigert sich, die abzubilden, und legt danach `.boot` ausserhalb jedes Segments ab.
#   * `llvm-objcopy` legt das Magic korrekt auf 4096, und QEMU laedt das Image dann auch (der
#     Banner erscheint) -- der Kernel bleibt danach aber sofort stehen. **Also kein Ersatz**,
#     sondern nur der Beleg, dass die Ursache in diesem Schritt sitzt und nicht in QEMU.
#
# Die Behebung gehoert in `kernel/x86_64-link.ld` (die nullgrossen Duplikate / Segment 5
# loswerden), nicht hierher -- und sie braucht eine eigene Messung. Bis dahin ist die x86-Suite
# auf dieser Werkzeugkette nicht lauffaehig; das ist ein AUFBAU-Problem, kein Kernelbefund.
objcopy -I elf64-x86-64 -O elf32-i386 "$ELF" "$ELF.mb32"
echo "x86_64-Kernel: $ELF (ELF64) + $ELF.mb32 (Multiboot-ELF32 fuer 'qemu-system-x86_64 -kernel')"
