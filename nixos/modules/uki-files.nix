# Files cut from each UKI, which sysupdate installs beside it in
# $BOOT/EFI/Linux (docs/boot-loaders.md), for the boots where GRUB does not
# simply chainload the UKI:
#
#   .grub    its command line, as a line of GRUB script, on every PC
#   .linux   its kernel, x86_64 only
#   .initrd  its initrd, x86_64 only
#
# An x86_64 BIOS cannot run a UKI, which is an EFI program, so there GRUB
# starts the kernel and initrd with that command line (bios.nix). On UEFI,
# a boot with losos_extra set, the recovery shell's (docs/troubleshooting.md),
# needs the command line too: a UKI takes kernel arguments only in place of
# its whole command line. x86_64 starts the kernel beside the UKI for it, as
# its BIOS boot does. aarch64 has no BIOS, and its kernel is an uncompressed
# Image of some 70 MiB, which two versions of beside two UKIs would nearly
# fill the 512M ESP with, so its GRUB chainloads the UKI with the command
# line and losos_extra as the UKI's arguments instead. systemd-stub takes
# them while Secure Boot is off, and ignores them while it is on.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos;
  inherit (cfg) arch version;
  id = config.system.image.id;
  inherit (config.system.boot.loader) ukiFile;

  suffixes = [
    "grub"
  ]
  ++ lib.optionals (arch.name == "x86_64") [
    "linux"
    "initrd"
  ];

  # The root partition on the disk whose ESP GRUB started the kernel from,
  # which the initrd's udev rule below links here.
  loaderRoot = "/dev/disk/by-loader/root";

  partx = "${pkgs.util-linux}/bin/partx";

  # Succeeds when the disk $1 has a partition whose UUID is $2: the udev
  # rule's test that a root partition sits on the same disk as the ESP GRUB
  # named. It reads the disk's partition table itself rather than asking
  # udev, which may not have seen the ESP yet.
  onLoaderDisk = pkgs.writeScript "losos-on-loader-disk" ''
    #!/bin/sh
    tr=${pkgs.coreutils}/bin/tr
    want=$(printf '%s' "$2" | $tr A-F a-f)
    for uuid in $(${partx} --show --noheadings --output UUID "$1" | $tr A-F a-f); do
      [ "$uuid" = "$want" ] && exit 0
    done
    exit 1
  '';

  # Two lines, because udev sorts a line's keys by kind before it runs them:
  # an ENV match and PROGRAM come before IMPORT{cmdline}, so on one line they
  # saw losos.esp still unset, the link was never made, and every BIOS boot
  # since the rule waited out root and stopped in emergency mode. $parent is
  # the disk's name without /dev/, which partx cannot open from udev's /.
  loaderRootRule = ''
    SUBSYSTEM=="block", ENV{DEVTYPE}=="partition", ENV{ID_PART_ENTRY_NAME}=="${arch.root}", IMPORT{cmdline}="losos.esp"
    SUBSYSTEM=="block", ENV{DEVTYPE}=="partition", ENV{ID_PART_ENTRY_NAME}=="${arch.root}", ENV{losos.esp}=="?*", PROGRAM="${onLoaderDisk} /dev/$parent $env{losos.esp}", SYMLINK+="${lib.removePrefix "/dev/" loaderRoot}"
  '';

  # Cut from the finished UKI rather than taken from the configuration,
  # because only the finished UKI's command line carries the usrhash= of the
  # /usr it belongs to.
  #
  # root=gpt-auto is replaced: systemd finds the root partition by type on
  # the disk the boot loader names in LoaderDevicePartUUID, an EFI variable,
  # and a BIOS has none to set. In its place GRUB names the ESP it started
  # the kernel from, as losos_esp, and the initrd's rule below finds root on
  # that ESP's disk. A GRUB from before losos_esp sets nothing, and the line
  # falls back to the root partition's label, which repart gives it from its
  # type (disk.nix gives it none of its own): right with one LosOS disk in
  # the machine, a coin toss with two. A UEFI boot with the line goes the
  # same way, so every boot that uses it finds root alike.
  files =
    pkgs.runCommand "${id}_${version}-uki-files"
      {
        nativeBuildInputs = with pkgs.buildPackages; [
          jq
          systemdUkify
        ];
      }
      ''
        cmdline=$(ukify --json=short inspect ${config.system.build.uki}/${ukiFile} \
          | jq -er '.".cmdline".text' | tr -d '\0\n')
        case " $cmdline " in
          *" usrhash="*" root=gpt-auto "* | *" root=gpt-auto "*" usrhash="*) ;;
          *) echo "unexpected UKI command line: $cmdline" >&2; exit 1 ;;
        esac
        # It goes into a double-quoted GRUB string, where these would mean
        # something.
        case "$cmdline" in
          *[\"\$\\\']*) echo "UKI command line needs quoting: $cmdline" >&2; exit 1 ;;
        esac
        cmdline=''${cmdline/root=gpt-auto/root=\$losos_root}

        mkdir $out
        {
          echo 'if [ -n "$losos_esp" ]; then'
          echo '  set losos_root="${loaderRoot} losos.esp=$losos_esp"'
          echo 'else'
          echo '  set losos_root="PARTLABEL=${arch.root}"'
          echo 'fi'
          printf 'set losos_cmdline="%s"\n' "$cmdline"
        } > $out/${id}_${version}.grub
        ${lib.optionalString (arch.name == "x86_64") ''
          cp ${config.boot.uki.settings.UKI.Linux} $out/${id}_${version}.linux
          cp ${config.boot.uki.settings.UKI.Initrd} $out/${id}_${version}.initrd
        ''}
      '';

  # The release's name for each of them. Both architectures' files go up to
  # one GitHub release, where every name has to be its own, so aarch64's
  # carry the architecture. x86_64's keep the names they had while only
  # x86_64 had them, because the x86_64 machines already installed look for
  # those.
  releaseName =
    suffix: if arch.name == "x86_64" then "${id}_@v.${suffix}" else "${id}_@v_${arch.name}.${suffix}";

  releaseFiles = pkgs.runCommand "${id}_${version}-uki-files-release" { } ''
    mkdir $out
    for suffix in ${toString suffixes}; do
      name=${lib.escapeShellArg (releaseName "@suffix@")}
      name=''${name//@v/${version}}
      cp ${files}/${id}_${version}.$suffix "$out/''${name//@suffix@/$suffix}"
    done
  '';

  # Each of them as a sysupdate transfer, installed beside the UKI in
  # $BOOT/EFI/Linux, where grub.cfg reads only *.efi names and finds these
  # by the UKI's.
  # They belong to the same version set as the UKI and the /usr slot
  # (update.nix), so a version is installed with all of them or not at all.
  transfer = suffix: {
    Transfer.ProtectVersion = "%A";
    Source = {
      Type = "url-file";
      Path = cfg.update.baseUrl;
      MatchPattern = releaseName suffix;
    };
    Target = {
      Type = "regular-file";
      Path = "/EFI/Linux";
      PathRelativeTo = "boot";
      MatchPattern = "${id}_@v.${suffix}";
      Mode = "0444";
      InstancesMax = 2;
    };
  };
in
{
  system.build = {
    ukiFiles = files;
    ukiReleaseFiles = releaseFiles;
  };

  systemd.sysupdate.transfers = {
    "42-uki-grub" = transfer "grub";
  }
  // lib.optionalAttrs (arch.name == "x86_64") {
    "40-uki-linux" = transfer "linux";
    "41-uki-initrd" = transfer "initrd";
  };

  # What LoaderDevicePartUUID gives gpt-auto under a UKI GRUB chainloads
  # with nothing added: root on the disk the boot came from, so another
  # LosOS disk in the same machine is never mounted in its place. losos.esp=
  # is on the command line only when GRUB put the .grub file's line there.
  # The rule is in the running system too, so the link the command line
  # names is still there after the switch to it.
  boot.initrd.services.udev.rules = loaderRootRule;
  services.udev.extraRules = loaderRootRule;
  boot.initrd.systemd = {
    # The script, and the store paths it names: coreutils is in every
    # initrd, partx is not.
    storePaths = [ onLoaderDisk ];
    extraBin.partx = partx;
  };
}
