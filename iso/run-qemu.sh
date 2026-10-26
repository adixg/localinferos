#!/usr/bin/env bash
set -euo pipefail

# Boot the ISO in QEMU with KVM. Creates the disk images on first run.
# Override defaults with e.g.: RAM=8G CPUS=8 iso/run-qemu.sh

cd "$(dirname "$0")/.."

ISO=dist/LocalInferOS-v0.1.iso
DUMMY=dist/dummy.img
DATA=dist/data.img

if [ ! -f "$ISO" ]; then
    echo "ISO not found: run iso/build.sh first"
    exit 1
fi

# init mounts /dev/sdb, so a placeholder disk takes /dev/sda
[ -f "$DUMMY" ] || truncate -s 64M "$DUMMY"

# Persistent data disk: profile, downloaded models. Sparse, so it only
# uses real space as it fills up.
if [ ! -f "$DATA" ]; then
    truncate -s "${DATA_SIZE:-16G}" "$DATA"
    mkfs.ext4 -q -F "$DATA"
fi

# -cpu host: llama.cpp is built for this CPU's instruction set
exec qemu-system-x86_64 \
    -enable-kvm -cpu host \
    -smp "${CPUS:-4}" -m "${RAM:-4G}" \
    -cdrom "$ISO" -boot d \
    -device ahci,id=ahci \
    -drive file="$DUMMY",if=none,id=d0,format=raw -device ide-hd,drive=d0,bus=ahci.0 \
    -drive file="$DATA",if=none,id=d1,format=raw -device ide-hd,drive=d1,bus=ahci.1 \
    -netdev user,id=n0 -device e1000,netdev=n0
