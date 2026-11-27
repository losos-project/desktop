# Updates replace; they do not patch.
#
# systemd-sysupdate keeps two of each thing it manages: two UKIs on the ESP,
# two /usr slots, two verity hash trees. An update downloads the new version
# into whichever slot is not booted and touches neither the running system
# nor /home. There is no nixos-rebuild here and no Nix on the machine to run
# one with.
#
# Rollback needs no failure detection. Limine offers the newest UKI; a UKI
# installed by sysupdate carries a boot counter in its file name, which
# losos-limine-trial counts down by renaming the file; losos-limine-bless
# settles the name once boot-complete.target is reached; an entry that never
# gets blessed runs out of tries and Limine falls back to the older one by
# itself (limine.nix). The
# older UKI's usrhash still names the older /usr, which is still there. A
# BIOS PC boots the same way, from the kernel and initrd bios.nix's
# transfers put beside each UKI.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos;
  inherit (cfg) arch;
  id = config.system.image.id;

  # Whether sysupdate reads the channel from losos-update-source, which
  # serves GitHub Releases on this machine, rather than from a proxy.
  local = cfg.update.baseUrl == localUrl;
  localUrl = "http://127.0.0.1:${toString config.services.losos-update-source.port}/";

  # The same partition-label contract image.nix writes: "<id>_<version>".
  #
  # The source name carries the partition's UUID (@u), and sysupdate gives the
  # partition it writes that UUID. It has to: the UKI finds /usr by the UUIDs
  # repart derived from usrhash=, so a slot that kept the random UUID repart
  # created it with would hold the right bytes and never be found.
  partition = type: {
    Transfer.ProtectVersion = "%A";
    Source = {
      Type = "url-file";
      Path = cfg.update.baseUrl;
      MatchPattern = "${id}_@v_${type}_@u.raw.xz";
    };
    Target = {
      Type = "partition";
      Path = "auto";
      MatchPattern = "${id}_@v";
      MatchPartitionType = type;
      ReadOnly = true;
      InstancesMax = 2;
    };
  };
in
{
  # sysupdate verifies SHA256SUMS.gpg against this keyring; Verify= is on by
  # default in every transfer below. systemd 261's systemd-pull hands gpg
  # --keyring=/etc/systemd/import-pubring.pgp whether or not that file
  # exists, so the older .gpg name left gpg with no key and every update
  # failed with "Can't check signature: No public key".
  environment.etc."systemd/import-pubring.pgp".source = cfg.update.pubring;

  # systemd-pull runs gpg from PATH to check that signature, and a NixOS
  # unit's PATH holds only coreutils, findutils, grep, sed and systemd, so
  # the timer's updates failed before the check began. nixpkgs gives
  # sysupdated, which runs updatectl's and Settings' updates, this same
  # gnupg already, and only that unit.
  systemd.services.systemd-sysupdate = {
    path = [ pkgs.gnupgMinimal ];

    # A download that breaks off leaves its slot marked partial, and every
    # later `update` then refuses that version until something vacuums it:
    # one dropped connection stopped updates for good. A bare vacuum removes
    # only partial and pending slots and versions beyond InstancesMax, so the
    # version to fall back to stays.
    serviceConfig.ExecStartPre = "${config.systemd.package}/lib/systemd/systemd-sysupdate vacuum";
  };

  services.losos-update-source = lib.mkIf local {
    inherit (cfg.update) index;
    inherit (cfg) channel channels;
  };

  assertions = [
    {
      assertion = local -> cfg.update.index != null;
      message = "losos.update.baseUrl is losos-update-source's, which needs losos.update.index (or losos.docs.url) to serve anything.";
    }
  ];

  systemd.sysupdate = {
    enable = true;

    # The timer is how updates appear without anyone asking. It downloads and
    # installs; it does not reboot, because a desktop that restarts itself
    # under its user loses their work. The next boot picks the new entry.
    reboot.enable = false;

    transfers = {
      # All three belong to one version and sysupdate installs them as a set.
      # A /usr slot no UKI names is inert, so the UKI is the half that decides
      # whether a new version boots at all.
      "10-usr-verity" = partition arch.usrVerity;
      "20-usr" = partition arch.usr;

      "30-uki" = {
        Transfer = {
          ProtectVersion = "%A";
        };
        Source = {
          Type = "url-file";
          Path = cfg.update.baseUrl;
          MatchPattern = "${id}_@v_${arch.name}.efi";
        };
        Target = {
          Type = "regular-file";
          Path = "/EFI/Linux";
          # $BOOT: the XBOOTLDR partition where there is one, the ESP
          # otherwise. A disk of LosOS's own has only the ESP; one shared
          # with Windows keeps the UKIs on an XBOOTLDR partition, because
          # Windows' ESP has no room for two (windows-installer.nix).
          PathRelativeTo = "boot";
          # Every name a UKI of this OS can have on the ESP: the image's own
          # (uncounted), a fresh install (+tries left), one mid-assessment
          # (+left-done), and one already blessed (counter removed again).
          MatchPattern = [
            "${id}_@v+@l-@d.efi"
            "${id}_@v+@l.efi"
            "${id}_@v.efi"
          ];
          Mode = "0444";
          TriesLeft = 3;
          TriesDone = 0;
          # Two kernels kept: the one running and the one to fall back to.
          InstancesMax = 2;
        };
      };
    };
  };
}
