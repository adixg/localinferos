#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

ROOT="$(pwd)"
WORK="$ROOT/build/minimal-os"
ROOTFS="$WORK/rootfs"
ISOROOT="$WORK/iso"

rm -rf "$WORK"
mkdir -p "$ROOTFS" "$ISOROOT/boot/grub" dist

# Minimal filesystem
mkdir -p "$ROOTFS"/{bin,sbin,usr/bin,lib,lib64}
mkdir -p "$ROOTFS"/{dev,proc,sys,tmp,run,root,models,etc}
mkdir -p "$ROOTFS/dev/pts" "$ROOTFS/dev/shm"

# BusyBox: shell + core utilities
cp /bin/busybox "$ROOTFS/bin/busybox"

for cmd in sh mount mkdir ip clear wget sleep setsid df; do
    ln -s busybox "$ROOTFS/bin/$cmd"
done

# Build Rust executable
cargo build --release
UI_BIN=target/release/localinferos

# Copy application binaries
cp "$UI_BIN" "$ROOTFS/usr/bin/localinferos"

cp build/llama-cpu/bin/llama-server \
    "$ROOTFS/usr/bin/llama-server"

cp build/llama-cpu/bin/llama-bench \
    "$ROOTFS/usr/bin/llama-bench"

# Shared library required by llama-bench
mkdir -p "$ROOTFS/usr/lib"

cp -L build/llama-cpu/bin/libllama-bench-impl.so \
    "$ROOTFS/usr/lib/"

chmod +x "$ROOTFS/usr/bin/llama-bench"

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

# Bundled model. llama-server names a model after its folder in --models-dir,
# so this one is listed as "default".
mkdir -p "$ISOROOT/models/default"
cp models/Qwen3-1.7B-Q4_K_M.gguf "$ISOROOT/models/default/"

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

KDIR="$WORK/kernel"
mkdir -p "$KDIR"

(
    cd "$KDIR"
    apt download "$KERNEL_PKG"
    dpkg-deb -x ./*.deb extracted
)

cp "$KDIR"/extracted/boot/vmlinuz-* \
    "$ISOROOT/boot/vmlinuz"

# Extract ISO9660 filesystem module matching our kernel
KVER="${KERNEL_PKG#linux-image-}"

mkdir -p "$KDIR/modules"

(
    cd "$KDIR"
    apt download "linux-modules-$KVER"
)

MOD_PKG=$(find "$KDIR" -maxdepth 1 \
    -name "linux-modules-${KVER}_*.deb" \
    -print -quit)

dpkg-deb -x "$MOD_PKG" "$KDIR/modules"

ISOFS=$(find "$KDIR/modules" \
    -name 'isofs.ko*' -print -quit)

if [ -z "$ISOFS" ]; then
    echo "ERROR: isofs kernel module not found"
    exit 1
fi

mkdir -p "$ROOTFS/lib/modules"

case "$ISOFS" in
    *.zst)
        zstd -dc "$ISOFS" > "$ROOTFS/lib/modules/isofs.ko"
        ;;
    *.ko)
        cp "$ISOFS" "$ROOTFS/lib/modules/isofs.ko"
        ;;
    *)
        echo "Unsupported module format: $ISOFS"
        exit 1
        ;;
esac

# VirtualBox Intel PRO/1000 Ethernet driver
E1000=$(find "$KDIR/modules" -name 'e1000.ko*' -print -quit)

if [ -z "$E1000" ]; then
    echo "ERROR: e1000 module not found"
    exit 1
fi

mkdir -p "$ROOTFS/lib/modules"

case "$E1000" in
    *.zst)
        zstd -dc "$E1000" > "$ROOTFS/lib/modules/e1000.ko"
        ;;
    *.ko)
        cp "$E1000" "$ROOTFS/lib/modules/e1000.ko"
        ;;
    *)
        echo "Unsupported e1000 module format"
        exit 1
        ;;
esac

# SATA support for VirtualBox persistent disk
MODULE_DIR="$KDIR/modules/lib/modules/$KVER/kernel/drivers/ata"

for mod in libahci ahci; do
    zstd -dc "$MODULE_DIR/$mod.ko.zst" \
        > "$ROOTFS/lib/modules/$mod.ko"
done

mkdir -p "$ROOTFS/etc"

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

# HTTPS downloads
mkdir -p "$ROOTFS/usr/bin" "$ROOTFS/etc/ssl/certs"

cp /usr/bin/curl "$ROOTFS/usr/bin/"
cp /etc/ssl/certs/ca-certificates.crt \
    "$ROOTFS/etc/ssl/certs/"

# Copy curl's shared-library dependencies
ldd /usr/bin/curl | awk '
    $3 ~ /^\// { print $3 }
    $1 ~ /^\// { print $1 }
' | while read -r lib; do
    mkdir -p "$ROOTFS$(dirname "$lib")"
    cp -L "$lib" "$ROOTFS$lib"
done


# Package root filesystem as initramfs
(
    cd "$ROOTFS"
    find . -print0 |
        cpio --null -o --format=newc 2>/dev/null |
        gzip -9 > "$ISOROOT/boot/initramfs.gz"
)

# GRUB configuration
cat > "$ISOROOT/boot/grub/grub.cfg" <<'GRUB'
set timeout=2
set default=0

menuentry "LocalInferOS" {
    linux /boot/vmlinuz console=tty1 quiet
    initrd /boot/initramfs.gz
}
GRUB

# Generate bootable ISO
grub-mkrescue \
    -o "$ROOT/dist/LocalInferOS-v0.1.iso" \
    "$ISOROOT"

echo
echo "ISO created:"
ls -lh "$ROOT/dist/LocalInferOS-v0.1.iso"
