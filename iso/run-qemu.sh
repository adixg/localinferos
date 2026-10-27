#!/usr/bin/env bash
set -euo pipefail

# Boot the USB image in QEMU the way a PC would: UEFI firmware (OVMF), image
# attached as a USB stick. Writes go to the image, like a real stick.
# Override defaults with e.g.: RAM=8G CPUS=8 iso/run-qemu.sh

cd "$(dirname "$0")/.."

IMG=dist/LocalInferOS-v0.1.img
OVMF=/usr/share/ovmf/OVMF.fd

if [ ! -f "$IMG" ]; then
    echo "Image not found: run iso/build.sh first"
    exit 1
fi

if [ ! -f "$OVMF" ]; then
    echo "UEFI firmware not found: sudo apt install ovmf"
    exit 1
fi

# -cpu host: llama.cpp needs AVX2, which QEMU's default CPU lacks
exec qemu-system-x86_64 \
    -enable-kvm -cpu host \
    -smp "${CPUS:-4}" -m "${RAM:-4G}" \
    -bios "$OVMF" \
    -device qemu-xhci \
    -drive if=none,id=stick,file="$IMG",format=raw \
    -device usb-storage,drive=stick,removable=on \
    -netdev user,id=n0 -device e1000,netdev=n0
