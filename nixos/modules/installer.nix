# The installer: a small live system on its own ISO, which partitions a disk
# with systemd-repart and fills it with systemd-sysupdate.
#
# It used to be the image itself, booted from a stick with a second UKI whose
# command line said `losos.install`, copying its own /usr onto the target with
# CopyBlocks=. That medium was the whole desktop, and it installed whatever
# version the stick happened to be written with. It is not coming back:
# installing is now an update into an empty slot, from the channel, from GHCR
# or from a release copy on the ISO when the build had the update signing key
# (image.nix, installer-embedded-release.nix).
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

  # The ISO's initrd finds the medium by this label, both for the embedded
  # release and for the raw desktop disk a Try boot snapshots.
  volumeID = "losos-${cfg.version}-${pkgs.stdenv.hostPlatform.parsed.cpu.name}";

  tryRaw = "${config.system.build.disk}/${config.image.baseName}.raw";

  # The desktop UKI's own command line carries the usrhash of the /usr in the
  # raw disk. Read it from the finished UKI rather than the configuration, as
  # uki-files.nix does, so a Try boot starts the same version it ships.
  tryUki = "${config.system.build.uki}/${config.system.boot.loader.ukiFile}";
  tryCmdline =
    pkgs.runCommand "losos-try-cmdline"
      {
        nativeBuildInputs = with pkgs.buildPackages; [
          jq
          systemdUkify
          coreutils
          util-linux
        ];
      }
      ''
        cmdline=$(ukify --json=short inspect ${tryUki} \
          | jq -er '.".cmdline".text' | tr -d '\0\n')
        case " $cmdline " in
          *" usrhash="*" root=gpt-auto "* | *" root=gpt-auto "*" usrhash="*) ;;
          *) echo "unexpected UKI command line: $cmdline" >&2; exit 1 ;;
        esac
        # The snapshot disk is not the medium the firmware booted from, so
        # gpt-auto cannot name its root. uki-files.nix's udev rule links the
        # root partition of the disk holding the ESP named by losos.esp; it
        # compares partition UUIDs, so pass the snapshot ESP's PARTUUID and
        # not its device path.
        esp=$(sfdisk --dump ${tryRaw} \
          | sed -n 's/.*uuid=\([^,]*\), name="esp".*/\1/p' \
          | tr A-F a-f)
        case "$esp" in
          ????????-????-????-????-????????????) ;;
          *) echo "no ESP partition UUID in the Try disk: '$esp'" >&2; exit 1 ;;
        esac
        cmdline=''${cmdline/root=gpt-auto/root=/dev/disk/by-loader/root losos.esp=$esp}
        printf '%s losos.try=1 losos.iso=${volumeID}\n' "$cmdline" > $out
      '';

  # ISO 9660 level 3 cannot hold a file this large, and a qcow2 would need a
  # userspace NBD daemon to survive switch-root. A squashfs keeps the raw disk
  # compressed and seekable: the initrd loop-mounts the file inside it.
  tryImage =
    pkgs.runCommand "losos-try-image"
      {
        nativeBuildInputs = [ pkgs.buildPackages.squashfsTools ];
      }
      ''
        mksquashfs ${tryRaw} $out -noappend -all-root -no-hardlinks \
          -b 1048576 -comp zstd -Xcompression-level 3 \
          -processors $NIX_BUILD_CORES -no-progress
      '';

  # Where the installer mounts the new ESP while sysupdate writes the UKI.
  esp = "/run/losos-installer/esp";

  repartFormat = pkgs.formats.ini { listsAsDuplicateKeys = true; };
  # nixpkgs' sysupdate module renders transfers in this format, so a transfer
  # here reads the same as its original in /etc/sysupdate.d.
  sysupdateFormat = pkgs.formats.ini { listToValue = toString; };

  # Limine, as limine.nix puts it on the image's ESP, with shim, MokManager
  # and the certificate when the image is signed for Secure Boot, and on
  # x86_64 Limine's BIOS stage file: the BIOS boot code itself lives in the
  # BIOS boot partition.
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
    # The BIOS boot partition Limine's stage goes in on a legacy BIOS PC, laid
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

  # The installed system's own definitions, all of them, for an install
  # that encrypts root (src/losos-installer's encrypt.rs): it then makes
  # root, /home and swap itself, as first boot would, and only this drop-in
  # differs, making root LUKS2 locked to the TPM. That slot is the
  # installer's to replace with one that also asks for the person's PIN.
  # First boot leaves the volume as it finds it: repart's Encrypt= applies
  # to partitions it creates.
  rootDefinitions = pkgs.linkFarm "losos-installer-root-repart.d" (
    lib.mapAttrsToList (name: settings: {
      name = "${name}.conf";
      path = repartFormat.generate "${name}.conf" { Partition = settings; };
    }) config.systemd.repart.partitions
    ++ [
      {
        name = "30-root.conf.d/encrypt.conf";
        path = repartFormat.generate "encrypt.conf" { Partition.Encrypt = "tpm2"; };
      }
    ]
  );

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
      # The image's pkgs already carries nvidia.nix's allowUnfreePredicate;
      # eval-config forbids nixpkgs.config when pkgs is injected.
      { nixpkgs.pkgs = pkgs; }
    ];
    specialArgs.losos = {
      inherit (cfg) version channel;
      embeddedRelease = config.system.build.installerEmbeddedRelease;
      inherit tryCmdline tryImage;
      tryLinux = config.boot.uki.settings.UKI.Linux;
      tryInitrd = config.boot.uki.settings.UKI.Initrd;
      releaseRepository = cfg.release.repository;
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
      inherit
        esp
        repartDefinitions
        rootDefinitions
        sysupdateTemplates
        ;
      # The swap definition for the PC's RAM, as first boot writes it
      # (disk.nix), for an install that makes swap itself.
      swap = "${pkgs.losos-swap}/lib/losos/losos-swap";
      # The key the medium's Limine and UKI are signed with, as the
      # image's are (secure-boot.nix).
      secureBoot = { inherit (cfg.secureBoot) privateKey certificate; };
      # Run on the disk once it is installed, so a legacy BIOS PC starts it
      # too; null where there is no BIOS (aarch64).
      biosBoot =
        if config.system.build ? limineInstall then lib.getExe config.system.build.limineInstall else null;
    };
  };

  installerLiveBundle = pkgs.callPackage ../pkgs/installer-live-bundle.nix {
    inherit (cfg) version channel;
    installerToplevel = installer.config.system.build.toplevel;
    derisk = pkgs.derisk;
    backend = installer.config.losos.installer.livepatch.backend;
  };
in
{
  system.build = {
    installerSystem = installer;
    installerIso = installer.config.system.build.isoImage;
    inherit installerLiveBundle;
  };
}
