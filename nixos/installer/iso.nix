# The installer medium: an ISO 9660 filesystem holding the live system's Nix
# store as one squashfs and its UKI, and an appended FAT partition holding
# GRUB as the removable-media boot path.
#
# Written to a stick, the ISO reads as a GPT disk with that FAT partition as
# its ESP; burnt to a disc, El Torito points the firmware at the same
# partition. UEFI firmware starts GRUB, which shows the installed system's
# theme with one entry and chainloads the UKI from the ISO 9660 filesystem.
# nixpkgs' iso-image.nix is not used because it is built around a GRUB menu
# generated per kernel and syslinux, and around a Nix that registers the
# store at boot, none of which is here.
#
# It used to start the UKI directly, with no boot loader in front. GRUB is
# there now so the medium starts as the installed system does, under the
# same salmon, from the menu on to Plymouth (modules/splash.nix).
#
# On x86_64 a legacy BIOS PC starts it too, with GRUB, because the OS it
# installs starts on one (modules/bios.nix): GRUB's hybrid MBR on a stick,
# its El Torito image on a disc, and either starts the UKI's own kernel and
# initrd with the UKI's command line, kept beside it in boot/.
#
# With LosOS's Secure Boot key, the FAT partition starts Microsoft-signed
# shim instead, which starts GRUB signed with that key, and the UKI is signed
# too (modules/shim.nix). A PC with Secure Boot on that has not enrolled the
# key yet stops in MokManager, where the certificate beside shim enrolls it.
{
  config,
  lib,
  pkgs,
  modulesPath,
  losos,
  ...
}:

let
  inherit (pkgs.stdenv.hostPlatform) efiArch;
  inherit (config.system.boot.loader) ukiFile;
  inherit (config.system.image) version;

  # The label the initrd finds the medium by. ISO 9660 allows 32 characters.
  volumeID = "losos-${version}-${pkgs.stdenv.hostPlatform.parsed.cpu.name}";

  squashfs = pkgs.callPackage "${modulesPath}/../lib/make-squashfs.nix" {
    fileName = "nix-store";
    storeContents = [ config.system.build.toplevel ];
    # zstd decompresses several times faster than xz for a few percent more
    # size, and everything the installer runs is read from here.
    comp = "zstd -Xcompression-level 19";
  };

  # The installer's GRUB menu, the same on both firmwares and drawn with the
  # installed system's theme (nixos/branding, grub-image.nix), which here
  # names it "LosOS Desktop installer". Its one entry finds the medium by
  # its label rather than guessing which drive GRUB was started from, a
  # CD or a disk. UEFI GRUB chainloads the UKI from the medium, so
  # systemd-stub starts it as it would from the ESP; BIOS GRUB starts the
  # UKI's own kernel and initrd with the UKI's command line, kept beside it
  # in boot/.
  art = import ../branding {
    inherit pkgs;
    name = config.system.nixos.distroName;
  };
  menu = pkgs.writeText "losos-installer-grub.cfg" ''
    set timeout=3
    set timeout_style=menu
    search --no-floppy --set=root --label ${volumeID}
    if [ "$grub_platform" = efi ]; then
      menuentry "Install LosOS Desktop" {
        chainloader /boot/installer.efi
      }
    else
      menuentry "Install LosOS Desktop" {
        linux /boot/linux ${config.boot.uki.settings.UKI.Cmdline}
        initrd /boot/initrd
      }
    fi
  '';
  # What the menu uses on either firmware, beside what grub-image.nix gives
  # every image.
  modules = [
    "iso9660"
    "search"
    "search_label"
  ];

  efi = import ../modules/grub-image.nix {
    inherit pkgs menu art;
    format = "efi";
    output = "grub.efi";
    early = "normal (memdisk)/grub.cfg";
    inherit modules;
  };

  # shim, MokManager and the certificate, when there is a key to sign GRUB
  # and the UKI with (modules/shim.nix).
  sb = import ../modules/shim.nix {
    inherit pkgs;
    inherit (losos.secureBoot) privateKey certificate;
  };
  grub = if sb.enable then sb.sign "grub${efiArch}.efi" "${efi}/grub.efi" else "${efi}/grub.efi";
  # The UKI GRUB starts, signed as the installed system's are
  # (modules/secure-boot.nix).
  uki =
    if sb.enable then
      sb.sign "installer.efi" "${config.system.build.uki}/${ukiFile}"
    else
      "${config.system.build.uki}/${ukiFile}";
  starters = sb.removable grub;

  # What starts GRUB at the removable-media boot path: GRUB alone, or shim
  # with GRUB beside it. The UKI GRUB starts is on the ISO 9660 filesystem
  # with the rest of the medium. Sized from its contents as nixpkgs sizes
  # its own EFI image: a tenth more for FAT's own overhead, rounded up to a
  # whole MiB.
  esp =
    pkgs.runCommand "losos-installer-esp.img"
      {
        nativeBuildInputs = with pkgs.buildPackages; [
          dosfstools
          mtools
        ];
      }
      ''
        size=$(du --block-size=1 --apparent-size --total ${toString (lib.attrValues starters)} | tail -n 1 | cut -f1)
        size=$(( (size * 110 / 100 / 1048576 + 1) * 1048576 ))
        truncate --size=$size $out
        mkfs.vfat --invariant -i 4c4f534f -n LOSOS-EFI $out
        mmd -i $out ::/EFI ::/EFI/BOOT
        ${lib.concatStrings (
          lib.mapAttrsToList (target: source: ''
            mcopy -i $out ${source} ::${target}
          '') starters
        )}
        fsck.vfat -n $out
      '';

  # GRUB for a legacy BIOS, as one El Torito image with its modules, menu
  # and theme inside, so it reads nothing of its own from the medium. The
  # same image starts from a disc, and from a stick through boot_hybrid.img
  # in the MBR, which xorriso points at it (--grub2-boot-info).
  bios = lib.optionalAttrs pkgs.stdenv.hostPlatform.isx86_64 {
    image = import ../modules/grub-image.nix {
      inherit pkgs menu art;
      format = "i386-pc-eltorito";
      output = "eltorito.img";
      early = "normal (memdisk)/grub.cfg";
      inherit modules;
    };
    mbr = "${pkgs.grub2}/lib/grub/i386-pc/boot_hybrid.img";
  };
in
{
  assertions = [
    {
      assertion = lib.stringLength volumeID <= 32;
      message = "The installer's volume ID ${volumeID} is longer than ISO 9660's 32 characters.";
    }
  ];

  # The root is a tmpfs; the store is the squashfs with a tmpfs over it, as on
  # nixpkgs' own ISOs. The writable layer is what lets a unit's generated files
  # and the like land in /nix/store at runtime without touching the medium.
  fileSystems = {
    "/" = {
      fsType = "tmpfs";
      options = [ "mode=0755" ];
    };
    "/iso" = {
      device = "/dev/disk/by-label/${volumeID}";
      fsType = "iso9660";
      neededForBoot = true;
      noCheck = true;
    };
    "/nix/.ro-store" = {
      fsType = "squashfs";
      device = "/sysroot/iso/nix-store.squashfs";
      options = [
        "loop"
        "threads=multi"
      ];
      neededForBoot = true;
    };
    "/nix/.rw-store" = {
      fsType = "tmpfs";
      options = [ "mode=0755" ];
      neededForBoot = true;
    };
    "/nix/store" = {
      overlay = {
        lowerdir = [ "/nix/.ro-store" ];
        upperdir = "/nix/.rw-store/store";
        workdir = "/nix/.rw-store/work";
      };
      neededForBoot = true;
    };
  };

  boot.initrd.availableKernelModules = [
    "squashfs"
    "iso9660"
    "sr_mod"
    "usb_storage"
    "uas"
    "overlay"
  ];
  boot.initrd.kernelModules = [
    "loop"
    "overlay"
  ];

  system.build.isoImage =
    pkgs.runCommand "losos-installer_${version}_${pkgs.stdenv.hostPlatform.parsed.cpu.name}.iso"
      {
        nativeBuildInputs = [ pkgs.buildPackages.xorriso ];
      }
      ''
        xorriso -as mkisofs \
          -iso-level 3 \
          -full-iso9660-filenames \
          -joliet -joliet-long \
          -rational-rock \
          -volid ${volumeID} \
          -appid "LosOS Desktop installer" \
          -partition_offset 16 \
          -append_partition 2 C12A7328-F81F-11D2-BA4B-00A0C93EC93B ${esp} \
          -appended_part_as_gpt \
          ${
            lib.optionalString (bios != { }) ''
              -b boot/eltorito.img \
              -no-emul-boot -boot-load-size 4 -boot-info-table \
              --grub2-boot-info --grub2-mbr ${bios.mbr} \
              -eltorito-alt-boot \
            ''
          } \
          -e --interval:appended_partition_2:all:: \
          -no-emul-boot \
          -graft-points \
          -output $out \
          nix-store.squashfs=${squashfs} \
          boot/installer.efi=${uki} \
          ${lib.optionalString (bios != { }) ''
            boot/eltorito.img=${bios.image}/eltorito.img \
            boot/linux=${config.boot.uki.settings.UKI.Linux} \
            boot/initrd=${config.boot.uki.settings.UKI.Initrd}
          ''}
      '';
}
