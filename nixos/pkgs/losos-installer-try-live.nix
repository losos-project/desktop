# Kexec the desktop kernel and initrd that ship with this image, so Try boots
# the machine's own hardware in the same Wayland/KMS session as an installed
# system. The desktop initrd snapshots the raw disk in /iso/try.squashfs onto
# tmpfs (modules/try.nix),
# so nothing on the machine's disks is written.
{
  pkgs,
  tryLinux,
  tryInitrd,
  tryCmdline,
}:

pkgs.writeShellApplication {
  name = "losos-installer-try-live";
  runtimeInputs = with pkgs; [
    coreutils
    kexec-tools
  ];
  text = ''
    set -euo pipefail

    for path in /iso/try.squashfs ${tryLinux} ${tryInitrd} ${tryCmdline}; do
      if [ ! -f "$path" ]; then
        echo "Try LosOS is not on this medium ($path is missing)." >&2
        exit 1
      fi
    done

    cmdline=$(< ${tryCmdline})
    echo "Try LosOS Desktop: kexec into the desktop on this medium" >&2
    kexec -l ${tryLinux} --initrd=${tryInitrd} --command-line="$cmdline"
    kexec -e
  '';
}
