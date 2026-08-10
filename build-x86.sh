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
objcopy -I elf64-x86-64 -O elf32-i386 "$ELF" "$ELF.mb32"
echo "x86_64-Kernel: $ELF (ELF64) + $ELF.mb32 (Multiboot-ELF32 fuer 'qemu-system-x86_64 -kernel')"
