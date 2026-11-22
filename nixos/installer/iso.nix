# The installer medium: an ISO 9660 filesystem holding the live system's Nix
# store as one squashfs and its UKI, and an appended FAT partition holding
# Limine as the removable-media boot path.
#
# Written to a stick, the ISO reads as a GPT disk with that FAT partition as
# its ESP; burnt to a disc, El Torito points the firmware at the same
# partition. UEFI firmware starts Limine, which shows the installed system's
# salmon and chainloads the UKI from the FAT. nixpkgs' iso-image.nix is not
# used because it is built around a GRUB menu generated per kernel and
# syslinux, and around a Nix that registers the store at boot, none of
# which is here.
#
# On x86_64 a legacy BIOS PC starts it too, with Limine's CD boot image,
# because the OS it installs starts on one (modules/bios.nix). That Limine
# reads limine.conf from the ISO and starts the UKI's kernel and initrd.
#
# With LosOS's Secure Boot key, the FAT partition starts Microsoft-signed
# shim instead, which starts Limine signed with that key (the file is still
# named grub*.efi, which is the name shim looks for), and the UKI is signed
# too (modules/shim.nix).
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

  art = import ../branding {
    inherit pkgs;
    name = config.system.nixos.distroName;
  };
  # UEFI Limine reads this from the FAT, where the UKI is copied so boot():
  # reaches it. BIOS Limine reads the other file from the ISO.
  cmdline = config.boot.uki.settings.UKI.Cmdline;
  # No serial here. On UEFI, serial: yes skips Limine's graphical
  # terminal, so a machine with a serial port would boot in text.
  efiConf = pkgs.writeText "limine.conf" ''
    timeout: 3
    wallpaper: boot():/limine/salmon.png
    wallpaper_style: centered
    backdrop: 0a0e12
    term_background: 800a0e12
    term_foreground: e2e8ee
    term_background_bright: 48b3c0
    term_foreground_bright: 0a0e12
    interface_branding: ${config.system.nixos.distroName}
    interface_branding_colour: 48b3c0
    interface_help_colour: 94a2b0

    /Install LosOS Desktop
        comment: Install LosOS Desktop
        protocol: efi
        path: boot():/EFI/Linux/installer.efi

    /Try LosOS Desktop
        comment: Try LosOS Desktop
        protocol: linux
        path: boot():/boot/linux
        module_path: boot():/boot/initrd
        cmdline: ${cmdline} losos.try=1
  '';
  biosConf = pkgs.writeText "limine-bios.conf" ''
    timeout: 3
    serial: yes
    serial_baudrate: 115200
    wallpaper: boot():/boot/salmon.png
    wallpaper_style: centered
    backdrop: 0a0e12
    term_background: 800a0e12
    term_foreground: e2e8ee
    term_background_bright: 48b3c0
    term_foreground_bright: 0a0e12
    interface_branding: ${config.system.nixos.distroName}
    interface_branding_colour: 48b3c0

    /Install LosOS Desktop
        comment: Install LosOS Desktop
        protocol: linux
        path: boot():/boot/linux
        module_path: boot():/boot/initrd
        cmdline: ${cmdline}

    /Try LosOS Desktop
        comment: Try LosOS Desktop
        protocol: linux
        path: boot():/boot/linux
        module_path: boot():/boot/initrd
        cmdline: ${cmdline} losos.try=1
  '';

  sb = import ../modules/shim.nix {
    inherit pkgs;
    inherit (losos.secureBoot) privateKey certificate;
  };
  # The same SBAT-bearing Limine the installed system starts (limine-efi.nix).
  limineEfi = import ../modules/limine-efi.nix {
    inherit pkgs;
    inherit efiArch;
  };
  limine = if sb.enable then sb.sign "limine${efiArch}.efi" limineEfi else limineEfi;
  # The UKI Limine starts, signed as the installed system's are
  # (modules/secure-boot.nix).
  uki =
    if sb.enable then
      sb.sign "installer.efi" "${config.system.build.uki}/${ukiFile}"
    else
      "${config.system.build.uki}/${ukiFile}";
  starters = sb.removable limine;

  # What starts Limine at the removable-media boot path: Limine alone, or
  # shim with Limine beside it under the name shim loads. The UKI Limine
  # starts is on the ISO 9660 filesystem
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
        size=$(du --block-size=1 --apparent-size --total \
          ${toString (lib.attrValues starters)} \
          ${efiConf} ${art}/limine/salmon.png ${uki} \
          ${config.boot.uki.settings.UKI.Linux} ${config.boot.uki.settings.UKI.Initrd} \
          | tail -n 1 | cut -f1)
        size=$(( (size * 110 / 100 / 1048576 + 1) * 1048576 ))
        truncate --size=$size $out
        mkfs.vfat --invariant -i 4c4f534f -n LOSOS-EFI $out
        mmd -i $out ::/EFI ::/EFI/BOOT ::/EFI/Linux ::/limine ::/boot
        ${lib.concatStrings (
          lib.mapAttrsToList (target: source: ''
            mcopy -i $out ${source} ::${target}
          '') starters
        )}
        mcopy -i $out ${efiConf} ::/limine.conf
        mcopy -i $out ${efiConf} ::/EFI/BOOT/limine.conf
        mcopy -i $out ${art}/limine/salmon.png ::/limine/salmon.png
        mcopy -i $out ${uki} ::/EFI/Linux/installer.efi
        mcopy -i $out ${config.boot.uki.settings.UKI.Linux} ::/boot/linux
        mcopy -i $out ${config.boot.uki.settings.UKI.Initrd} ::/boot/initrd
        fsck.vfat -n $out
      '';

  # Limine's BIOS CD image. xorriso points El Torito at it, and
  # `limine bios-install` finishes the ISO afterwards. The conf and
  # limine-bios.sys are grafted onto the ISO, which is where that stage
  # looks for them.
  bios = lib.optionalAttrs pkgs.stdenv.hostPlatform.isx86_64 {
    cd = "${pkgs.limine}/share/limine/limine-bios-cd.bin";
    sys = "${pkgs.limine}/share/limine/limine-bios.sys";
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
        nativeBuildInputs = [
          pkgs.buildPackages.xorriso
          pkgs.buildPackages.limine
        ];
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
              -b boot/limine-bios-cd.bin \
              -no-emul-boot -boot-load-size 4 -boot-info-table \
              -eltorito-alt-boot \
            ''
          } \
          -e --interval:appended_partition_2:all:: \
          -no-emul-boot \
          -graft-points \
          -output $out \
          nix-store.squashfs=${squashfs} \
          boot/installer.efi=${uki} \
          release.squashfs=${losos.embeddedRelease} \
          try.qcow2=${losos.tryQcow2} \
          ${lib.optionalString (bios != { }) ''
            boot/limine-bios-cd.bin=${bios.cd} \
            boot/limine-bios.sys=${bios.sys} \
            boot/limine.conf=${biosConf} \
            boot/salmon.png=${art}/limine/salmon.png \
            boot/linux=${config.boot.uki.settings.UKI.Linux} \
            boot/initrd=${config.boot.uki.settings.UKI.Initrd}
          ''}
        ${lib.optionalString (bios != { }) ''
          limine bios-install $out
        ''}
      '';
}
