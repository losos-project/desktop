# Ephemeral try session: QEMU boots the qcow2 grafted onto the ISO with a
# fresh overlay in tmpfs, so nothing is written to any disk on the machine.
{
  lib,
  pkgs,
  hostPlatform,
}:

let
  qemu = pkgs.qemu;
  inherit (pkgs.stdenv.hostPlatform) isx86_64 isAarch64;
  machine =
    if isx86_64 then
      "q35,accel=kvm:tcg"
    else if isAarch64 then
      "virt,accel=kvm:tcg -cpu max"
    else
      null;
  system =
    if isx86_64 then
      "qemu-system-x86_64"
    else if isAarch64 then
      "qemu-system-aarch64"
    else
      throw "losos-installer-try-live: no QEMU target for ${hostPlatform.system}";
  ovmf =
    if isx86_64 then
      {
        code = "${pkgs.OVMF.firmware}/FV/OVMF_CODE.fd";
        vars = pkgs.OVMF.variables;
      }
    else if isAarch64 then
      {
        code = "${pkgs.OVMF.firmware}/FV/AAVMF_CODE.fd";
        vars = pkgs.AAVMF.variables;
      }
    else
      null;
in
assert machine != null;
pkgs.writeShellApplication {
  name = "losos-installer-try-live";
  runtimeInputs = [
    qemu
    pkgs.qemu
    pkgs.coreutils
  ];
  text = ''
    set -euo pipefail

    BASE=/iso/try.qcow2
    WORK=/run/losos-try-live
    DISK="$WORK/ephemeral.qcow2"
    VARS="$WORK/OVMF_VARS.fd"

    for path in "$BASE" ${ovmf.code}; do
      if [ ! -f "$path" ]; then
        echo "Try LosOS is not on this medium ($path is missing)." >&2
        exit 1
      fi
    done

    mkdir -p "$WORK"
    rm -f "$DISK"
    qemu-img create -f qcow2 -F qcow2 -b "$BASE" "$DISK" 48G
    install -m 0644 ${ovmf.vars} "$VARS"

    # Hand the display to QEMU; the installer session is replaced, not nested
    # inside it.
    systemctl stop derisk-installer.service || true
    systemctl stop plymouth-quit.service plymouth-quit-wait.service 2>/dev/null || true

    exec ${qemu}/bin/${system} \
      -machine ${machine} \
      -m 4096 \
      -drive "file=$DISK,if=virtio,format=qcow2" \
      -drive "if=pflash,format=raw,readonly=on,file=${ovmf.code}" \
      -drive "if=pflash,format=raw,file=$VARS" \
      -device virtio-vga \
      -display default,show-cursor=on,full-screen=on \
      -audiodev id=audio,driver=none \
      -serial mon:stdio \
      -no-reboot
  '';
}
