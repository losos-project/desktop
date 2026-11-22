# Limine, the boot loader on every PC (docs/boot-loaders.md):
#
#   UEFI  firmware -> ESP: Limine -> chainloads the newest UKI -> systemd-stub
#   BIOS  firmware -> MBR -> Limine's stage in the BIOS boot partition
#         (bios.nix) -> the newest UKI's kernel, initrd and command line
#
# The menu is limine.conf on the ESP, rewritten from the UKI file names by
# losos-limine-menu. The salmon and the palette are the same pictures
# branding builds for the rest of boot. losos-limine-bless settles a UKI's
# tries in its file name once a boot succeeds. losos-limine-trial spends one
# try as soon as userspace can see the ESP, because Limine cannot rename a
# file.
#
# The file Limine is started from is still named grub*.efi. Fedora's
# Microsoft-signed shim loads \grub<arch>.efi from beside itself
# (shim.nix, secondStage). The bytes are Limine.
#
# With LosOS's Secure Boot key (secure-boot.nix), Microsoft-signed shim is
# what firmware starts, and it starts this Limine signed (shim.nix):
#
#   UEFI  firmware -> ESP: shim -> Limine -> chainloads the UKI, through
#         shim's LoadImage, which checks it against the MOK list
#
# NixOS's own GRUB module stays off (boot.nix): it would install GRUB into a
# running system and regenerate a menu on every rebuild, and this OS is an
# image that is never rebuilt in place. The image and the installer put
# Limine on the ESP, and losos-limine-update below replaces it with the one
# in /usr once an update has booted well, as systemd-boot-update does for
# systemd-boot. A disk that still starts the previous loader (GRUB, or
# systemd-boot) is moved over to Limine by that same unit.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos;
  inherit (cfg) arch;
  inherit (config.image.repart.verityStore) partitionIds;
  id = config.system.image.id;
  sb = config.system.build.secureBoot;
  withSbat = import ./limine-efi.nix {
    inherit pkgs;
    efiArch = arch.efi;
  };

  art = import ../branding {
    inherit pkgs;
    name = config.system.nixos.distroName;
  };

  limineMenu = pkgs.writeShellApplication {
    name = "losos-limine-menu";
    runtimeInputs = with pkgs; [
      coreutils
      findutils
      gawk
      gnugrep
      gnused
      jq
      mtools
      util-linux
      config.systemd.package
      limine
    ];
    text = ''
      export LOSOS_ID=${lib.escapeShellArg id}
      export LOSOS_NAME=${lib.escapeShellArg config.system.nixos.distroName}
      export LOSOS_BIOS=${if arch.name == "x86_64" then "1" else "0"}
      export LOSOS_LIMINE=${pkgs.limine}
      export LOSOS_WALLPAPER=${art}/limine/salmon.png
      ${builtins.readFile ./limine-menu.sh}
    '';
  };

  # Limine's EFI program, with the SBAT section above, signed with
  # LosOS's key when there is one so shim will start it.
  efi = pkgs.runCommand "losos-limine-${arch.efi}" { } ''
    mkdir $out
    cp ${if sb.enable then sb.sign "limine${arch.efi}.efi" withSbat else withSbat} $out/limine.efi
  '';

  # A menu the image can boot before losos-limine-menu has rewritten it
  # from the UKIs. The writer replaces it once those are on the disk.
  # No serial: on UEFI that flag skips the graphical terminal. BIOS reads
  # /limine.conf, which the writer gives a serial console.
  stubConf = pkgs.writeText "limine.conf" ''
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
  '';

  # Everything on the ESP that starts Limine: shim and MokManager beside
  # it with Secure Boot's key, at the removable-media path and at a path
  # of its own; and the certificate MokManager enrolls (shim.nix).
  starters =
    sb.removable "${efi}/limine.efi"
    // {
      "/EFI/losos/${sb.secondStage}" = "${efi}/limine.efi";
      "/limine/salmon.png" = "${art}/limine/salmon.png";
      "/limine.conf" = stubConf;
      "/EFI/BOOT/limine.conf" = stubConf;
      "/EFI/losos/limine.conf" = stubConf;
    }
    // lib.optionalAttrs (arch.name == "x86_64") {
      "/limine-bios.sys" = "${pkgs.limine}/share/limine/limine-bios.sys";
    }
    // lib.optionalAttrs sb.enable {
      "/EFI/losos/shim${arch.efi}.efi" = sb.shimFile;
      "/EFI/losos/mm${arch.efi}.efi" = sb.mokManagerFile;
    };

  # This system's UKI, counted or not. The menu names entries "LosOS
  # Desktop <version>", and Limine turns that into LoaderEntrySelected, so
  # the file name is not in the EFI variable. The booted /usr is this
  # version, and sysupdate keeps one UKI of it.
  #
  # A disk still started by systemd-boot reports that in LoaderInfo.
  # systemd-bless-boot owns the count then, and this does nothing.
  ownUki = ''
    info=/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
    if [ -r "$info" ] && tail -c +5 "$info" | tr -d '\0' | grep -q '^systemd-boot'; then
      exit 0
    fi
    esp=$(${config.systemd.package}/bin/bootctl --print-esp-path)
    boot=$(${config.systemd.package}/bin/bootctl --print-boot-path)
    file=
    for dir in "$boot" "$esp"; do
      found=$(find "$dir/EFI/Linux" -maxdepth 1 -name '${id}_${cfg.version}+*.efi' -print -quit 2>/dev/null || true)
      if [ -n "$found" ]; then
        file=$found
        break
      fi
      if [ -f "$dir/EFI/Linux/${id}_${cfg.version}.efi" ]; then
        file=$dir/EFI/Linux/${id}_${cfg.version}.efi
        break
      fi
    done
  '';

  # Spends one try as soon as userspace can see the ESP, then rewrites the
  # menu so the next boot names the file the rename left. A kernel that
  # dies before this runs is started again with the same count: Limine
  # cannot rename a file or write the count itself. A crash after the
  # rename but before the rewrite would leave the menu pointing at a file
  # that is gone, so both happen here.
  trial = pkgs.writeShellApplication {
    name = "losos-limine-trial";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gnugrep
      limineMenu
    ];
    text = ''
      ${ownUki}
      [ -n "$file" ] || exit 0
      entry=$(basename "$file")
      case $entry in
        *+*) ;;
        *)
          losos-limine-menu --esp "$esp"
          exit 0
          ;;
      esac
      base=''${entry%%+*}
      counter=''${entry#*+}
      counter=''${counter%.efi}
      tries=''${counter%%-*}
      made=0
      if [ "$counter" != "$tries" ]; then
        made=''${counter#*-}
      fi
      [ "$tries" -gt 0 ] || exit 0
      dir=$(dirname "$file")
      mv "$file" "$dir/$base+$((tries - 1))-$((made + 1)).efi"
      sync --file-system "$dir"
      losos-limine-menu --esp "$esp"
    '';
  };

  # systemd-bless-boot's job: a UKI that reached boot-complete.target loses
  # its counter. The trial service may already have lowered the count, so
  # this looks up the version rather than the name from boot.
  bless = pkgs.writeShellApplication {
    name = "losos-limine-bless";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gnugrep
      config.systemd.package
      limineMenu
    ];
    text = ''
      ${ownUki}
      [ -n "$file" ] || exit 0
      entry=$(basename "$file")
      case $entry in
        *+*) ;;
        *)
          losos-limine-menu --esp "$esp"
          exit 0
          ;;
      esac
      dir=$(dirname "$file")
      mv "$file" "$dir/${id}_${cfg.version}.efi"
      sync --file-system "$dir"
      echo "Marked ${id}_${cfg.version}.efi as good."
      losos-limine-menu --esp "$esp"
    '';
  };

  # What systemd-boot-update.service does for systemd-boot: the Limine on
  # the ESP is the one the image or the installer put there, and /usr
  # carries the one its version was built with, so after an update this
  # copies the new one over the old. It runs only once losos-limine-bless
  # has run, so a version's Limine reaches the ESP only after that version
  # has started cleanly, and never goes backwards: a rollback to an older
  # version leaves the newer Limine, which reads the same names, in place.
  #
  # It replaces only files that are already LosOS's: Limine, recognised by
  # its base-revision string; the GRUB an older install still starts,
  # recognised by the variable its early config set; or the systemd-boot
  # older installs started with. EFI/BOOT/BOOT<ARCH>.EFI on a disk the
  # Windows installer set up is Windows', and stays so. On an x86_64 disk
  # with a BIOS boot partition it reinstalls Limine's BIOS stage too.
  # EFI/losos/installed records what it installed last, so a boot with
  # nothing new writes nothing.
  update = pkgs.writeShellApplication {
    name = "losos-limine-update";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.diffutils
      pkgs.gnugrep
      pkgs.gnused
      pkgs.jq
      pkgs.util-linux
      config.systemd.package
      pkgs.limine
    ];
    text = ''
      version=${lib.escapeShellArg cfg.version}
      stamp="version=$version efi=${efi} bios=${lib.getExe limineMenu}${lib.optionalString sb.enable " shim=${sb.shimFile}"}"

      esp=$(bootctl --print-esp-path)
      # /efi can be an automount; looking inside mounts it.
      ls "$esp/EFI" > /dev/null
      dir="$esp/EFI/losos"
      record="$dir/installed"
      if [ -f "$record" ]; then
        [ "$(cat "$record")" = "$stamp" ] && exit 0
        installed=$(sed -n 's/^version=\([^ ]*\).*/\1/p' "$record")
        newest=$(printf '%s\n%s\n' "$installed" "$version" | sort -V | tail -n 1)
        if [ "$newest" != "$version" ]; then
          echo "The ESP has Limine from $installed, newer than $version; leaving it."
          exit 0
        fi
      fi
      mkdir -p "$dir"

      # The Windows installer lists what it added to the ESP, so that
      # --uninstall removes exactly that (src/losos-windows-installer); a
      # file this adds goes on its list too. Installs from before Limine
      # kept the list under loader/.
      created() {
        local list
        for list in "$dir/losos-windows-installer.txt" "$esp/loader/losos-windows-installer.txt"; do
          if [ -f "$list" ] && ! grep -qxF "$1" "$list"; then
            printf '%s\n' "$1" >> "$list"
          fi
        done
      }

      # put SRC DEST: replace DEST with SRC, by a rename, unless it already is.
      put() {
        cmp -s "$1" "$2" && return 0
        [ -e "$2" ] || created "''${2#"$esp"/}"
        install -m 0644 "$1" "$2.new"
        sync --data "$2.new"
        mv -f "$2.new" "$2"
        echo "Wrote ''${2#"$esp"/}."
      }
      # Limine, the GRUB an older install still starts (its early config
      # set losos_disk), or the systemd-boot older installs started with;
      # or shim, with Limine or that GRUB beside it.
      ours() {
        [ -f "$1" ] || return 1
        grep -aq -e 'limine: Base revision' -e losos_disk -e '#### LoaderInfo: systemd-boot' "$1" && return 0
        grep -aq 'github.com/rhboot/shim' "$1" \
          && grep -aq -e 'limine: Base revision' -e losos_disk "$(dirname "$1")/${sb.secondStage}" 2> /dev/null
      }

      # What firmware starts under DIR/NAME: shim, and Limine and MokManager
      # beside it, with Secure Boot's key; Limine itself without.
      start_from() {
        ${
          if sb.enable then
            ''
              put ${sb.shimFile} "$1/$2"
              put ${efi}/limine.efi "$1/${sb.secondStage}"
              put ${sb.mokManagerFile} "$1/mm${arch.efi}.efi"
            ''
          else
            ''
              put ${efi}/limine.efi "$1/$2"
            ''
        }
      }

      for file in "BOOT/BOOT${lib.toUpper arch.efi}.EFI" "systemd/systemd-boot${arch.efi}.efi"; do
        if ours "$esp/EFI/$file"; then
          start_from "$(dirname "$esp/EFI/$file")" "$(basename "$file")"
        fi
      done
      put ${efi}/limine.efi "$dir/${sb.secondStage}"
      ${lib.optionalString sb.enable ''
        put ${sb.shimFile} "$dir/shim${arch.efi}.efi"
        put ${sb.mokManagerFile} "$dir/mm${arch.efi}.efi"
        put ${sb.certificateDer} "$esp${sb.enrolPath}"
      ''}
      # GRUB's boot counter. Limine keeps the count in the UKI's file name.
      if [ -f "$dir/grubenv" ]; then
        rm -f "$dir/grubenv"
        echo "Removed EFI/losos/grubenv."
      fi
      ${lib.getExe limineMenu} --esp "$esp"
      ${lib.optionalString (arch.name == "x86_64") ''
        source=$(findmnt --noheadings --raw --output SOURCE --types vfat --mountpoint "$esp")
        disk=/dev/$(lsblk --noheadings --nodeps --output PKNAME "$source")
        idx=$(sfdisk --json "$disk" | jq -r '
          .partitiontable.partitions
          | to_entries[]
          | select(.value.type | ascii_downcase == "21686148-6449-6e6f-744e-656564454649")
          | .key + 1' | head -n 1)
        if [ -n "$idx" ]; then
          put ${pkgs.limine}/share/limine/limine-bios.sys "$esp/limine-bios.sys"
          ${pkgs.limine}/bin/limine bios-install "$disk" "$idx"
          echo "Installed Limine's BIOS boot code on $disk."
        fi
      ''}

      [ -e "$record" ] || created EFI/losos/installed
      printf '%s\n' "$stamp" > "$record.new"
      sync --data "$record.new"
      mv -f "$record.new" "$record"
    '';
  };

  # The firmware entry the Windows installer adds for Limine goes first in
  # the boot order, and Windows' own updates and some firmware put Windows
  # Boot Manager back in front of it (docs/windows-installer.md). Limine's
  # menu offers Windows anyway, so each LosOS boot that gets as far as
  # boot-complete.target puts LosOS's entry first again, whether it names
  # Limine (under shim's second-stage file name) or the shim beside it
  # (shim.nix). A disk the image or the installer laid out starts from the
  # removable-media path and has no entry, and this does nothing there.
  bootOrder = pkgs.writeShellApplication {
    name = "losos-boot-order";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.efibootmgr
      pkgs.gnugrep
      pkgs.gnused
    ];
    text = ''
      [ -d /sys/firmware/efi ] || exit 0
      entries=$(efibootmgr --verbose)
      ours=$(printf '%s\n' "$entries" \
        | grep -iF -e '\EFI\losos\grub' -e '\EFI\losos\shim' \
        | sed -n 's/^Boot\([0-9A-Fa-f]\{4\}\).*/\1/p' \
        | head -n 1 || true)
      [ -n "$ours" ] || exit 0
      order=$(printf '%s\n' "$entries" | sed -n 's/^BootOrder: //p')
      [ "''${order%%,*}" = "$ours" ] && exit 0
      rest=$(printf '%s\n' "$order" | tr , '\n' | grep -vixF "$ours" | paste -sd, || true)
      efibootmgr --quiet --bootorder "$ours''${rest:+,$rest}"
      echo "Put Boot$ours, LosOS, first in the boot order again."
    '';
  };
in
{
  system.build.limine = {
    inherit
      art
      efi
      starters
      update
      bootOrder
      limineMenu
      ;
    inherit (pkgs) efibootmgr;
  };

  # image.nix and the installer run this on the disk after its UKIs are
  # copied. It writes limine.conf, and on x86_64 installs Limine's BIOS
  # boot code. The ESP need not be mounted: the program reads the disk.
  system.build.limineInstall = limineMenu;

  environment.systemPackages = [ limineMenu ];

  # The removable-media path, which firmware boots with no boot entry
  # registered, so this image needs no efibootmgr and no install step; and
  # a copy at the path of its own that the Windows installer registers with
  # the firmware, where Windows' fallback owns the first one.
  image.repart.partitions.${partitionIds.esp}.contents = lib.mapAttrs (_: source: {
    inherit source;
  }) starters;

  # As systemd-bless-boot.service is wired: after boot-complete.target,
  # which systemd-boot-check-no-failures gates (boot.nix), pulled in early
  # so a boot that never gets there leaves the trial running.
  systemd.services.losos-limine-trial = {
    description = "Spend one try of the UKI Limine started";
    wantedBy = [ "basic.target" ];
    after = [ "local-fs.target" ];
    before = [
      "losos-limine-bless.service"
      "boot-complete.target"
    ];
    unitConfig.DefaultDependencies = false;
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = lib.getExe trial;
    };
  };

  systemd.services.losos-limine-menu = {
    description = "Rewrite Limine's menu from the UKIs on the ESP";
    wantedBy = [ "multi-user.target" ];
    after = [
      "local-fs.target"
      "losos-limine-bless.service"
    ];
    before = [ "shutdown.target" ];
    conflicts = [ "shutdown.target" ];
    unitConfig.DefaultDependencies = false;
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = "${lib.getExe limineMenu} --esp /efi";
      ExecStop = "${lib.getExe limineMenu} --esp /efi";
    };
  };

  systemd.services.losos-limine-bless = {
    description = "Mark the UKI Limine started as good";
    wantedBy = [ "basic.target" ];
    requires = [ "boot-complete.target" ];
    after = [
      "local-fs.target"
      "boot-complete.target"
    ];
    conflicts = [ "shutdown.target" ];
    before = [ "shutdown.target" ];
    unitConfig.DefaultDependencies = false;
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = lib.getExe bless;
    };
  };

  systemd.services.losos-limine-update = {
    description = "Install the Limine this version carries";
    wantedBy = [ "basic.target" ];
    requires = [ "boot-complete.target" ];
    after = [
      "local-fs.target"
      "boot-complete.target"
      "losos-limine-bless.service"
    ];
    conflicts = [ "shutdown.target" ];
    before = [ "shutdown.target" ];
    unitConfig.DefaultDependencies = false;
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = lib.getExe update;
    };
  };

  systemd.services.losos-boot-order = {
    description = "Put LosOS first in the firmware's boot order";
    wantedBy = [ "basic.target" ];
    requires = [ "boot-complete.target" ];
    after = [ "boot-complete.target" ];
    conflicts = [ "shutdown.target" ];
    before = [ "shutdown.target" ];
    unitConfig = {
      DefaultDependencies = false;
      ConditionPathExists = "/sys/firmware/efi";
    };
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = lib.getExe bootOrder;
    };
  };
}
