#!/usr/bin/env bash
# Boot the Caprock kernel under QEMU.
#
# Target configuration per the project spec: ARM, 8 cores, 4 GiB RAM.
# `virtualization` is left OFF so QEMU enters the kernel directly at EL1.
# Exit QEMU with Ctrl-A then X.
set -euo pipefail
cd "$(dirname "$0")"

KERNEL="build/target/aarch64-caprock/release/caprock-kernel.elf"
[ -f "$KERNEL" ] || { echo "kernel not built — run ./build.sh first" >&2; exit 1; }

exec qemu-system-aarch64 \
    -machine virt \
    -cpu cortex-a72 \
    -smp 8 \
    -m 4G \
    -nographic \
    -serial mon:stdio \
    -kernel "$KERNEL" \
    "$@"
