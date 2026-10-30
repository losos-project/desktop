# The installer: a small live system on its own ISO, which partitions a disk
# with systemd-repart and fills it with systemd-sysupdate.
#
# It used to be the image itself, booted from a stick with a second UKI whose
# command line said `losos.install`, copying its own /usr onto the target with
# CopyBlocks=. That medium was the whole desktop, and it installed whatever
# version the stick happened to be written with. It is not coming back:
# installing is now an update into an empty slot, downloaded from the ISO's
# configured update channel, and the medium carries no copy of the OS.
#
# This module is the OS's half, the half that decides what is written. The
# repart definitions are disk.nix's own ESP and /usr slots, and the transfers are
# update.nix's own, aimed at a disk that is not the one running. So the
# installer cannot lay out a disk differently from how first boot expects to
# find it, nor fetch anything the OS's own updates would not. The live system
# that runs them is nixos/installer/, evaluated here as a NixOS system of its
# own that shares the package set with the image and nothing else.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  cfg = config.losos;
  inherit (config.image.repart.verityStore) partitionIds;

  # Where the installer mounts the new ESP while sysupdate writes the UKI.
  esp = "/run/losos-installer/esp";

  repartFormat = pkgs.formats.ini { listsAsDuplicateKeys = true; };
  # nixpkgs' sysupdate module renders transfers in this format, so a transfer
  # here reads the same as its original in /etc/sysupdate.d.
  sysupdateFormat = pkgs.formats.ini { listToValue = toString; };

  # GRUB and grubenv, as grub.nix puts them on the image's ESP, with shim,
  # MokManager and the certificate when the image is signed for Secure Boot,
  # and on x86_64 nothing more: BIOS GRUB lives in the BIOS boot partition.
  # The UKI is not in this set (repart-verity-store.nix adds it later), which
  # is as it should be: sysupdate installs the UKI, counted, like any update.
  espFiles = lib.mapAttrsToList (
    target: file: "${file.source}:${target}"
  ) config.image.repart.partitions.${partitionIds.esp}.contents;

  # The ESP and both /usr slots, and nothing else. Root, /home and swap are
  # made by the installed system's first boot, from disk.nix, as they are for
  # an image written with dd. Both slots are created at their full size and
  # labelled `_empty`, the label sysupdate looks for when it needs a partition
  # to write a version into. Slot B has to be here too, though nothing is
  # written to it yet: sysupdate refuses a disk with fewer than two partitions
  # of a type it updates ("less than two partition slots"), and its own
  # definitions are the ones first boot would make it from, so first boot finds
  # it already there.
  repartPartitions =
    let
      inherit (config.systemd.repart) partitions;
    in
    {
      # disk.nix only matches an ESP; the installer makes this one, at the
      # size image.nix gives its own.
      "10-esp" = partitions."10-esp" // {
        SizeMinBytes = "512M";
        SizeMaxBytes = "512M";
        CopyFiles = espFiles;
      };
      "20-usr-verity-a" = partitions."20-usr-verity-a" // {
        Label = "_empty";
      };
      "21-usr-a" = partitions."21-usr-a" // {
        Label = "_empty";
      };
      inherit (partitions) "22-usr-verity-b" "23-usr-b";
    }
    # The BIOS boot partition GRUB's core goes in on a legacy BIOS PC, laid
    # out as image.nix lays out the image's (bios.nix). First boot has no
    # definition of it, and leaves it be as it leaves any partition it does
    # not know.
    // lib.optionalAttrs (config.image.repart.partitions ? "05-bios-boot") {
      "05-bios-boot" = config.image.repart.partitions."05-bios-boot".repartConfig;
    };

  repartDefinitions = pkgs.linkFarm "losos-installer-repart.d" (
    lib.mapAttrsToList (name: settings: {
      name = "${name}.conf";
      path = repartFormat.generate "${name}.conf" { Partition = settings; };
    }) repartPartitions
  );

  # update.nix's transfers with their targets moved. A partition target names
  # the disk being installed, which only the person at the keyboard knows, so
  # it is left as @TARGET@ for the installer to fill in. A file target is
  # placed under the ESP the installer mounts, rather than whichever ESP the
  # running system has (it has none). ProtectVersion= goes: it protects the
  # running system's version, and the installer is not running from the disk
  # being written.
  retarget =
    name: transfer:
    let
      target = transfer.Target;
      moved =
        if target.Type == "partition" then
          { Path = "@TARGET@"; }
        # $BOOT is the ESP on a disk the installer lays out, which has no
        # XBOOTLDR partition.
        else if
          lib.elem (target.PathRelativeTo or "root") [
            "esp"
            "boot"
          ]
        then
          {
            Path = "${esp}${target.Path}";
            PathRelativeTo = "root";
          }
        else
          throw "installer.nix: transfer ${name} writes somewhere the installer does not mount";
    in
    transfer
    // {
      Transfer = lib.removeAttrs (transfer.Transfer or { }) [ "ProtectVersion" ];
      Target = target // moved;
    };

  sysupdateTemplates = pkgs.linkFarm "losos-installer-sysupdate.d" (
    lib.mapAttrsToList (name: transfer: {
      name = "${name}.transfer";
      path = sysupdateFormat.generate "${name}.transfer" (retarget name transfer);
    }) config.systemd.sysupdate.transfers
  );

  installer = import "${modulesPath}/../lib/eval-config.nix" {
    system = null;
    modules = [
      ../installer
      # The image's own package set, overlay and all, so the installer's
      # systemd is the image's systemd and nothing is evaluated twice.
      { nixpkgs.pkgs = pkgs; }
    ];
    specialArgs.losos = {
      inherit (cfg) version channel;
      inherit (cfg.update) baseUrl pubring;
      # Where the backend reads which assets make up the channel's files,
      # when baseUrl is losos-update-source's address (update-source.nix).
      updateIndex = config.services.losos-update-source.defaultIndex;
      # Do not pass a pinned release: the installer always installs the
      # channel's newest complete release, regardless of the ISO's build tag.
      releaseRepo = null;
      docsUrl = cfg.docs.url;
      # Where a failed install is reported, and the GitHub App it signs in
      # with; no report is offered without both.
      reportRepo = cfg.report.repository;
      reportClientId = cfg.report.clientId;
      inherit esp repartDefinitions sysupdateTemplates;
      # The key the medium's GRUB and UKI are signed with, as the
      # image's are (secure-boot.nix).
      secureBoot = { inherit (cfg.secureBoot) privateKey certificate; };
      # Run on the disk once it is installed, so a legacy BIOS PC starts it
      # too; null where there is no BIOS (aarch64).
      biosBoot =
        if config.system.build ? grubBiosInstall then
          lib.getExe config.system.build.grubBiosInstall
        else
          null;
    };
  };
in
{
  system.build = {
    installerSystem = installer;
    installerIso = installer.config.system.build.isoImage;
  };
}
