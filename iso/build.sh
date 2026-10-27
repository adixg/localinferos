#!/usr/bin/env bash
set -euo pipefail

# Builds a bootable USB image (UEFI) for 16 GB sticks:
#   partition 1  LIO_BOOT  FAT32  EFI/BOOT/BOOTX64.EFI (GRUB), boot/vmlinuz, boot/initramfs.gz
#   partition 2  LIO_DATA  ext4   models/, config/, hf-cache/  (the rest of the stick)

cd "$(dirname "$0")/.."

ROOT="$(pwd)"
WORK="$ROOT/build/minimal-os"
ROOTFS="$WORK/rootfs"
BOOTDIR="$WORK/boot"   # contents of LIO_BOOT
DATADIR="$WORK/data"   # contents of LIO_DATA
IMG="$ROOT/dist/LocalInferOS-v0.1.img"

# "16 GB" sticks hold 14.4-15.6 GiB, so 14 GiB fits all of them
IMG_MB=14336
BOOT_MB=128
DATA_MB=$((IMG_MB - BOOT_MB - 2)) # 1 MiB before the partitions, 1 MiB for the GPT backup

rm -rf "$WORK"
mkdir -p "$ROOTFS" "$BOOTDIR/EFI/BOOT" "$BOOTDIR/boot" "$DATADIR" dist

# Minimal filesystem
mkdir -p "$ROOTFS"/{bin,sbin,usr/bin,usr/lib,lib,lib64}
mkdir -p "$ROOTFS"/{dev,proc,sys,tmp,run,root,data,etc}
mkdir -p "$ROOTFS/dev/pts" "$ROOTFS/dev/shm"

# BusyBox: shell + core utilities
cp /bin/busybox "$ROOTFS/bin/busybox"

for cmd in sh mount mkdir ip clear wget sleep setsid df modprobe findfs udhcpc; do
    ln -s busybox "$ROOTFS/bin/$cmd"
done

# Build Rust executable
cargo build --release
UI_BIN=target/release/localinferos

# Copy application binaries
cp "$UI_BIN" "$ROOTFS/usr/bin/localinferos"
cp build/llama-cpu/bin/llama-server "$ROOTFS/usr/bin/llama-server"
cp build/llama-cpu/bin/llama-bench "$ROOTFS/usr/bin/llama-bench"

# Shared library required by llama-bench
cp -L build/llama-cpu/bin/libllama-bench-impl.so "$ROOTFS/usr/lib/"

# Copy required shared libraries
copy_libs() {
    local binary="$1"

    if ldd "$binary" | grep -q 'not found'; then
        echo "Missing dependencies for $binary"
        ldd "$binary"
        exit 1
    fi

    while read -r lib; do
        [ -f "$lib" ] || continue

        mkdir -p "$ROOTFS$(dirname "$lib")"
        cp -L "$lib" "$ROOTFS$lib"
    done < <(
        ldd "$binary" | awk '
            $2 == "=>" && $3 ~ /^\// {print $3}
            $1 ~ /^\// {print $1}
        '
    )
}

copy_libs "$UI_BIN"
copy_libs build/llama-cpu/bin/llama-server

# PID 1 startup script
cp iso/init "$ROOTFS/init"
chmod +x "$ROOTFS/init"

# Kernel requires an initial console
sudo mknod -m 600 "$ROOTFS/dev/console" c 5 1
sudo mknod -m 666 "$ROOTFS/dev/null" c 1 3

# Network: DHCP client script, CA certificates for HTTPS downloads
cat > "$ROOTFS/etc/udhcpc.script" <<'EOF'
#!/bin/sh

case "$1" in
    bound|renew)
        ifconfig "$interface" "$ip" \
            netmask "${subnet:-255.255.255.0}" up

        ip route replace default via "${router%% *}"

        : > /etc/resolv.conf
        for ns in $dns; do
            echo "nameserver $ns" >> /etc/resolv.conf
        done
        ;;
esac
EOF

chmod +x "$ROOTFS/etc/udhcpc.script"

mkdir -p "$ROOTFS/etc/ssl/certs"
cp /etc/ssl/certs/ca-certificates.crt "$ROOTFS/etc/ssl/certs/"

# Kernel and modules from Ubuntu
KERNEL_PKG=$(apt-cache depends linux-image-generic |
    awk '$1 == "Depends:" && $2 ~ /^linux-image-[0-9]/ {
        print $2
        exit
    }')

if [ -z "$KERNEL_PKG" ]; then
    echo "Could not resolve Ubuntu kernel package"
    exit 1
fi

echo "Using kernel: $KERNEL_PKG"

KVER="${KERNEL_PKG#linux-image-}"
KDIR="$WORK/kernel"
mkdir -p "$KDIR"

(
    cd "$KDIR"
    apt download "$KERNEL_PKG" "linux-modules-$KVER"
    dpkg-deb -x "$(ls ${KERNEL_PKG}_*.deb)" extracted
    dpkg-deb -x "$(ls linux-modules-${KVER}_*.deb)" modules
)

cp "$KDIR"/extracted/boot/vmlinuz-* "$BOOTDIR/boot/vmlinuz"

# Drivers loaded by init. modprobe resolves their dependencies (e.g. uas
# needs usb-storage), so list only what init asks for.
#   xhci-pci  USB 3 controllers (USB 2 drivers are built into the kernel)
#   uas       USB sticks (with usb-storage)
#   ahci      SATA, for VMs
#   e1000     Intel network card, as emulated by QEMU and VirtualBox
MODULES="xhci-pci uas ahci e1000"

depmod -b "$KDIR/modules" "$KVER"
MODDIR="$ROOTFS/lib/modules/$KVER"
mkdir -p "$MODDIR"
for f in modules.builtin modules.builtin.modinfo modules.order; do
    cp "$KDIR/modules/lib/modules/$KVER/$f" "$MODDIR/"
done

for mod in $MODULES; do
    modprobe -d "$KDIR/modules" -S "$KVER" --show-depends "$mod" |
        awk '$1 == "insmod" {print $2}'
done | sort -u | while read -r path; do
    src="$path"
    [ -f "$src" ] || src="$KDIR/modules$path"
    rel="${path#*/lib/modules/$KVER/}"
    mkdir -p "$MODDIR/$(dirname "$rel")"
    # BusyBox can't load compressed modules
    case "$src" in
        *.zst) zstd -qdc "$src" > "$MODDIR/${rel%.zst}" ;;
        *)     cp "$src" "$MODDIR/$rel" ;;
    esac
done

depmod -b "$ROOTFS" "$KVER"

# Package root filesystem as initramfs
(
    cd "$ROOTFS"
    find . -print0 |
        cpio --null -o --format=newc 2>/dev/null |
        gzip -9 > "$BOOTDIR/boot/initramfs.gz"
)

# GRUB as a single EFI file, with its config built in
cat > "$WORK/grub.cfg" <<'GRUB'
# A standalone GRUB doesn't load these by itself; without part_gpt it sees
# the stick as one unpartitioned disk
insmod part_gpt
insmod fat
search --no-floppy --file --set=root /boot/vmlinuz
insmod all_video
set timeout=2
set default=0

menuentry "LocalInferOS" {
    linux /boot/vmlinuz console=tty1 quiet
    initrd /boot/initramfs.gz
}
GRUB

grub-mkstandalone -O x86_64-efi \
    -o "$BOOTDIR/EFI/BOOT/BOOTX64.EFI" \
    "boot/grub/grub.cfg=$WORK/grub.cfg"

# Data partition contents. llama-server names a model after its folder in
# --models-dir, so the bundled model is listed as "default".
mkdir -p "$DATADIR/models/default" "$DATADIR/config" "$DATADIR/hf-cache"
cp models/Qwen3-1.7B-Q4_K_M.gguf "$DATADIR/models/default/"

# Build each partition as a file (no root or loop devices needed)
BOOT_PART="$WORK/boot.part"
DATA_PART="$WORK/data.part"

truncate -s "${BOOT_MB}M" "$BOOT_PART"
mformat -i "$BOOT_PART" -F -v LIO_BOOT ::
mcopy -i "$BOOT_PART" -s "$BOOTDIR/EFI" "$BOOTDIR/boot" ::/

truncate -s "${DATA_MB}M" "$DATA_PART"
mkfs.ext4 -q -F -L LIO_DATA -d "$DATADIR" "$DATA_PART"

# Assemble the disk image: partition table, then each partition at its offset
rm -f "$IMG"
truncate -s "${IMG_MB}M" "$IMG"

sfdisk --quiet "$IMG" <<EOF
label: gpt
start=1MiB, size=${BOOT_MB}MiB, type=uefi, name="LIO_BOOT"
start=$((BOOT_MB + 1))MiB, size=${DATA_MB}MiB, type=linux, name="LIO_DATA"
EOF

dd if="$BOOT_PART" of="$IMG" bs=1M seek=1 conv=notrunc,sparse status=none
dd if="$DATA_PART" of="$IMG" bs=1M seek=$((BOOT_MB + 1)) conv=notrunc,sparse status=none
rm -f "$BOOT_PART" "$DATA_PART"

echo
echo "USB image created (sparse; real size shown second):"
ls -lh "$IMG"
du -h "$IMG"
