# How it boots. Every stage is systemd, as in docs/boot.md:
#
#   firmware -> GRUB (grub.nix) -> UKI (systemd-stub) -> kernel
#     (a legacy BIOS: MBR -> GRUB -> the UKI's kernel and initrd, bios.nix)
#     -> systemd in the initrd, under Plymouth's splash (splash.nix):
#        repart, veritysetup, gpt-auto root
#     -> switch-root -> systemd: sysusers, tmpfiles, homed, NetworkManager ...
#
# What NixOS changes is where the operating system lives. In the pm tree it was
# the root partition, replaced wholesale by sysupdate. Here it is the Nix store,
# on a dm-verity protected /usr partition, and the root partition holds only
# state. That turns out to be the shape docs/factory-reset.md asked for and
# could not reach: see disk.nix.
{
  config,
  lib,
  pkgs,
  ...
}:

{
  boot = {
    # There is no bootloader to install. GRUB is copied into the ESP, and on
    # a BIOS PC into the BIOS boot partition, when the image is assembled
    # (grub.nix, bios.nix), and losos-grub-update copies a newer one over it
    # after an update, as systemd-boot-update would. Its menu is the same
    # for every version and never generated. So NixOS's own GRUB module,
    # which installs GRUB and rewrites its menu on every rebuild, stays off.
    loader.grub.enable = false;

    # systemd in the initrd is the only stage 1 nixpkgs has, so there is
    # no enable to set.
    initrd.systemd = {
      # Find the root filesystem by GPT partition type rather than by name.
      # systemd-gpt-auto-generator matches the type UUID, which is why there is
      # still no root= device on the command line and no fstab entry for /.
      root = "gpt-auto";
    };

    # The console is the only thing a VM test or a broken boot can be read
    # from; `quiet` keeps the kernel's own chatter off it on real hardware.
    kernelParams = [
      "quiet"
      "console=tty0"
    ];

    # Boot assessment is on for every UKI sysupdate installs (update.nix) and
    # deliberately off for the one the image ships with: the first kernel is
    # trusted by construction, and a counter on it would retire the only entry
    # there is.
    uki.tries = null;
  };

  # losos-grub-bless (grub.nix) marks a counted entry good once
  # boot-complete.target is reached, and boot-check-no-failures is what makes
  # reaching it mean something. (systemd-bless-boot does the same for a disk
  # that still starts systemd-boot: systemd-bless-boot-generator pulls it in
  # whenever the boot loader reports a boot counter, which GRUB never does.)
  # The check has to be named. Upstream's unit is RequiredBy= the target, and
  # NixOS ignores [Install] sections, so that is set here too.
  systemd.additionalUpstreamSystemUnits = [
    "systemd-boot-check-no-failures.service"
  ];
  systemd.services.systemd-boot-check-no-failures.requiredBy = [ "boot-complete.target" ];

  # nix.enable, the /etc overlay and sysusers are in base.nix: a Halium build
  # is the same kind of image and shares them.
}
