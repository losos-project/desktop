# LosOS Desktop as a NixOS module, for UEFI PCs and arm64 machines. Importing
# this is the whole OS; a host configuration adds nothing but its architecture
# and version (see flake.nix).
#
# docs/nixos.md maps each file here to the piece of the pm tree it replaces.
{
  imports = [
    ./base.nix
    ./boot.nix
    ./grub.nix
    ./bios.nix
    ./splash.nix
    ./boot-sound.nix
    ./hardware.nix
    ./nvidia.nix
    ./libvirt.nix
    ./machines.nix
    ./service-machines.nix
    ./schizo.nix
    ./busybox-guests.nix
    ./busybox-host-kernel.nix
    ./passthrough.nix
    ./gaming.nix
    ./vm-call.nix
    ./network-machine.nix
    ./edge-exit.nix
    ./vpn.nix
    ./files-machine.nix
    ./shares-machine.nix
    ./peripherals.nix
    ./portal-backend.nix
    ./portal-machine.nix
    ./windows-apps.nix
    ./macos-apps.nix
    ./android-apps.nix
    ./flatpak-machine.nix
    # Security keys and smart card readers, lent to the machines.
    ./tokens.nix
    # Signing in with the Slovak eID card, through the eID machine.
    ./eid-signin.nix
    ./allocator.nix
    ./image.nix
    ./disk.nix
    # hibernate.nix is gone: the kernel is locked down (kernel-hardening.nix),
    # and a locked-down kernel refuses to hibernate. It sealed the swap's key
    # to the TPM so a laptop could write its memory there and resume from it
    # (docs/hibernation.md).
    ./update.nix
    ./update-source.nix
    ./installer.nix
    ./windows-installer.nix
    ./agent.nix
    # testing.nix is gone: its losos-selftest unit sat in every boot's
    # transaction and sometimes kept losos-grub-bless from running, and its
    # losos-ota-test could install an unverified release. Tests wait for the
    # bless themselves and rewrite sysupdate's transfers from the driver.
  ];
}
