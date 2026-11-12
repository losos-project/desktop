#!/usr/bin/env bash
# Boot the installer ISO in QEMU with a visible GTK window, wait for the UI,
# then grab PNGs with ImageMagick. Usage: installer-proof-screenshots.sh <iso> <outdir>
set -euo pipefail

iso=${1:?installer ISO path}
outdir=${2:-docs/images/installer-liveproof}
mkdir -p "$outdir"

disk=$(mktemp --suffix=.qcow2)
cleanup() { rm -f "$disk"; }
trap cleanup EXIT

qemu-img create -f qcow2 "$disk" 8G >/dev/null

readarray -t ovmf < <(nix build --no-link --print-out-paths nixpkgs#legacyPackages.x86_64-linux.OVMF.fd nixpkgs#legacyPackages.x86_64-linux.OVMF.Variables.fd 2>/dev/null || nix eval --raw --impure --expr 'let pkgs = import <nixpkgs> {}; in pkgs.OVMF.fd + "\n" + pkgs.OVMF.Variables.fd')
ovmf_code=${ovmf[0]}
ovmf_vars=${ovmf[1]}

qemu-system-x86_64 \
  -machine q35,accel=kvm:tcg \
  -cpu max \
  -m 4096 \
  -smp 2 \
  -drive if=pflash,format=raw,unit=0,readonly=on,file="$ovmf_code" \
  -drive if=pflash,format=raw,unit=1,file="$ovmf_vars" \
  -cdrom "$iso" \
  -boot order=d \
  -drive "file=$disk,if=virtio,format=qcow2" \
  -device virtio-vga \
  -display gtk,window-close-action=hide,show-cursor=on \
  -name "LosOS installer proof" \
  -serial none \
  &
qemu_pid=$!
trap 'kill "$qemu_pid" 2>/dev/null || true; cleanup' EXIT

shot() {
  local name=$1
  local delay=$2
  sleep "$delay"
  if command -v import >/dev/null; then
    import -window "LosOS installer proof" "$outdir/$name" 2>/dev/null \
      || import -window root "$outdir/$name-fullscreen.png"
  elif command -v scrot >/dev/null; then
    scrot -u "$outdir/$name"
  else
    echo "install imagemagick or scrot to capture $name" >&2
    return 1
  fi
  echo "wrote $outdir/$name"
}

echo "QEMU pid $qemu_pid — waiting for installer UI"
shot "01-after-boot.png" 75
shot "02-installer-ui.png" 30

kill "$qemu_pid" 2>/dev/null || true
wait "$qemu_pid" 2>/dev/null || true

sha256sum "$iso" > "$outdir/iso.sha256"
cp -f "$iso" "$outdir/" 2>/dev/null || ln -f "$iso" "$outdir/$(basename "$iso")"
