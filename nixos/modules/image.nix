# The image: an ESP and one dm-verity protected /usr slot, built by
# systemd-repart. Nothing else is on it. First boot grows it into a whole disk
# (disk.nix), and every later version arrives as a new /usr slot and a new UKI
# (update.nix).
#
# nixpkgs' image/repart.nix builds it as it is, under `unshare
# --map-root-user`: CI's runner allows the build user namespaces (ci.yml's
# AppArmor sysctl), so nixpkgs' module is no longer patched to go without.
#
# What comes out of one configuration:
#
#   system.build.image             the disk as systemd-repart makes it
#   system.build.disk              the OS as a disk: write it to a disk and
#                                  boot, on UEFI or (x86_64) a legacy BIOS
#   system.build.releaseArtifacts  what systemd-sysupdate downloads, plus the
#                                  disk image, the installer ISO
#                                  (installer.nix) and, on x86_64, the
#                                  Windows installer (windows-installer.nix),
#                                  with SHA256SUMS
#
# The installer used to be this image again, with a second UKI that booted
# into an install. installer.nix says why it is a separate live system now.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  cfg = config.losos;
  inherit (cfg) arch version;
  inherit (config.image.repart.verityStore) partitionIds;
  inherit (config.system.boot.loader) ukiFile;

  id = config.system.image.id;
in
{
  imports = [ "${modulesPath}/image/repart.nix" ];

  system.image = {
    id = "losos-desktop";
    inherit version;
  };

  image.repart = {
    # nixpkgs unstable gates the whole repart image module on this switch.
    # Without it image.repart.image is never defined and every output built
    # from the disk fails to evaluate.
    enable = true;

    name = id;

    # /usr is the Nix store on erofs, with a dm-verity hash tree beside it
    # whose root hash is baked into the UKI's command line as usrhash=. That
    # one value is what makes /usr immutable: a changed block fails
    # verification, and a different /usr needs a different UKI.
    verityStore = {
      enable = true;
      ukiPath = "/EFI/Linux/${ukiFile}";
    };

    # systemd 261's repart hands mkfs.erofs the image's 512-byte sector size
    # as the filesystem block size. libblkid cannot identify an erofs with
    # 512-byte blocks, so udev never learns /dev/mapper/usr holds a
    # filesystem, marks it SYSTEMD_READY=0, and the initrd waits out the
    # device with /usr unmounted. A later -b wins over repart's own.
    #
    # Compressed, because every release has to fit the fixed update slot an
    # installed disk already has (losos.usrSize, 8G; releaseArtifacts
    # refuses a /usr that does not). LZ4HC did until the store grew past it:
    # the 23 October 2026 nightly's /usr was 8.24G, 11.7G uncompressed.
    # The same store as Zstandard level 3 in 128K physical clusters (-C) is
    # 7.32G. On a 3G part of it, level 3 took 41s where LZ4HC 12 took 112s,
    # level 9 saved 2% more in four times as long, and libdeflate 9 saved
    # less than level 3. A third of the store is the machine templates (machines.nix),
    # compressed qcow2 that nothing here shrinks. The kernel's erofs reads
    # Zstandard (CONFIG_EROFS_FS_ZIP_ZSTD, on in nixpkgs from 6.10); it has
    # no LZMA.
    mkfsOptions.erofs = [
      "-b4096"
      "-zzstd,level=3"
      "-C131072"
    ];

    partitions = {
      # GRUB and grubenv go in from grub.nix, and the UKI from nixpkgs'
      # verity-store module.
      ${partitionIds.esp} = {
        repartConfig = {
          Type = "esp";
          Format = "vfat";
          SizeMinBytes = "512M";
          SizeMaxBytes = "512M";
        };
      };

      # Fixed at 128M rather than minimised: the hash tree of an 8G /usr is
      # about 64M, and slot A's verity partition cannot grow later because
      # slot A's data partition sits directly behind it.
      ${partitionIds.store-verity}.repartConfig = {
        Label = "${id}_${version}";
        # repart defaults verity blocks to the image's 512-byte sector size,
        # which makes the hash tree a sixteenth of the data: 332M for a 4.8G
        # store, measured, and it did not fit. At 4K it is 1/128.
        VerityDataBlockSizeBytes = 4096;
        VerityHashBlockSizeBytes = 4096;
        Minimize = lib.mkForce "off";
        SizeMinBytes = "128M";
        SizeMaxBytes = "128M";
      };

      # The label carries the version because that is where systemd-sysupdate
      # reads a partition's version from. The data partition is minimised here
      # and grown to its slot size by the first boot's repart run.
      ${partitionIds.store}.repartConfig = {
        Label = "${id}_${version}";
        VerityDataBlockSizeBytes = 4096;
        VerityHashBlockSizeBytes = 4096;
      };
    };
  };

  # What a release uploads. The file names are the contract with update.nix:
  # sysupdate matches them with MatchPattern, so the two are written from the
  # same arch table and cannot drift.
  #
  # The /usr halves are cut out of the finished disk image using the offsets
  # repart itself reported, rather than built a second time, so the bytes
  # sysupdate installs are the bytes the image boots. Their names carry the
  # partition UUIDs repart derived from the verity root hash, because that is
  # how the initrd finds /usr from usrhash= on the command line, and
  # sysupdate gives a partition the UUID its source's name carries (@u).
  # The image with what a BIOS boot needs added (bios.nix): GRUB in the
  # MBR and the BIOS boot partition, and the image's own version's kernel,
  # initrd and command line on the ESP beside its UKI. Added afterwards
  # rather than given to repart, because the command line carries the
  # usrhash= repart only reports once /usr is built, and the ESP is made
  # in that same run; nixpkgs' verity-store module gets the UKI onto the
  # ESP the same way. Elsewhere this is the repart image as it is.
  system.build.disk =
    let
      image = config.system.build.image;
      raw = "${config.image.baseName}.raw";
    in
    if config.system.build ? biosBootFiles then
      pkgs.runCommand "${id}_${version}-disk"
        {
          nativeBuildInputs = with pkgs.buildPackages; [
            jq
            mtools
          ];
        }
        ''
          mkdir $out
          cp ${image}/repart-output.json $out/
          cp --sparse=always ${image}/${raw} $out/${raw}
          chmod u+w $out/${raw}
          esp=$(jq -er '.[] | select(.type=="esp") | .offset' $out/repart-output.json)
          MTOOLS_SKIP_CHECK=1 mcopy -i "$out/${raw}@@$esp" \
            ${config.system.build.biosBootFiles}/* ::/EFI/Linux/
          ${lib.getExe config.system.build.grubBiosInstall} $out/${raw}
        ''
    else
      image;

  system.build.releaseArtifacts =
    let
      image = config.system.build.disk;
      raw = "${config.image.baseName}.raw";
      prefix = "${id}_${version}";
    in
    pkgs.runCommand "${prefix}-release"
      {
        nativeBuildInputs = with pkgs.buildPackages; [
          jq
          xz
        ];
      }
      ''
        mkdir -p $out
        cd $out

        extract() {
          local type=$1 offset size uuid
          field() {
            jq -er --arg t "$type" ".[] | select(.type==\$t) | .$1" ${image}/repart-output.json
          }
          offset=$(field offset)
          size=$(field raw_size)
          uuid=$(field uuid)
          dd if=${image}/${raw} of="${prefix}_''${type}_$uuid.raw" bs=1M iflag=skip_bytes,count_bytes \
            skip="$offset" count="$size" status=none
          xz --threads=$NIX_BUILD_CORES "${prefix}_''${type}_$uuid.raw"
        }

        # Every installed disk has /usr slots of losos.usrSize, which cannot
        # grow, so a /usr larger than that is a release nothing can install
        # or update to. The nightlies from 22 October 2026 were: their
        # store passed 8G, and installs stopped at 97% with "File too
        # large".
        usr=$(jq -er '.[] | select(.type=="${arch.usr}") | .raw_size' ${image}/repart-output.json)
        slot=$(numfmt --from=iec ${cfg.usrSize})
        if [ "$usr" -gt "$slot" ]; then
          echo "/usr is $(numfmt --to=iec "$usr"), larger than its ${cfg.usrSize} update slot (losos.usrSize)" >&2
          exit 1
        fi
        echo "/usr is $(numfmt --to=iec "$usr") of its ${cfg.usrSize} slot"

        cp ${config.system.build.uki}/${ukiFile} ${prefix}_${arch.name}.efi
        ${lib.optionalString (config.system.build ? biosBootFiles) ''
          # What GRUB starts this version from on a BIOS PC (bios.nix),
          # named as sysupdate installs them.
          cp ${config.system.build.biosBootFiles}/* .
        ''}
        extract ${arch.usr}
        extract ${arch.usrVerity}

        xz --threads=$NIX_BUILD_CORES -c ${image}/${raw} > ${prefix}_${arch.name}.raw.xz
        cp ${config.system.build.installerIso} ${prefix}_${arch.name}-installer.iso
        ${lib.optionalString (config.system.build ? windowsInstaller) ''
          cp ${config.system.build.windowsInstaller}/bin/losos-windows-installer.exe \
            ${prefix}_${arch.name}-windows-installer.exe
        ''}

        sha256sum -- * > SHA256SUMS
      '';

  system.build.qcow2 =
    pkgs.runCommand "${id}_${version}.qcow2" { nativeBuildInputs = [ pkgs.buildPackages.qemu-utils ]; }
      ''
        qemu-img convert -f raw -O qcow2 \
          ${config.system.build.disk}/${config.image.baseName}.raw $out
      '';
}
