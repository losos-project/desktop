# Limine, the boot loader on every PC (docs/boot-loaders.md):
#
#   UEFI  firmware -> ESP: Limine -> chainloads the newest UKI -> systemd-stub
#   BIOS  firmware -> MBR -> Limine's stage in the BIOS boot partition
#         (bios.nix) -> the newest UKI's kernel, initrd and command line
#
# The menu is limine.conf on the ESP, rewritten from the UKI file names by
# losos-limine-menu. The salmon and the palette are the same pictures
# branding builds for the rest of boot. losos-grub-bless still settles a
# UKI's tries in its file name once a boot succeeds. The file Limine starts
# from is still named grub*.efi: shim looks for that name beside itself.
#
# UEFI used to boot with systemd-boot, which draws a text menu in the
# firmware's console font and nothing else. GRUB draws the salmon behind
# the menu as the rest of the boot does, from GRUB to Plymouth (splash.nix)
# to the login screen. It is not coming back for that reason, and because
# two loaders that count tries in two places were one more thing to keep in
# step. What systemd-boot gave the rest of the system, GRUB gives too: the
# bli module sets LoaderDevicePartUUID, so systemd-gpt-auto-generator finds
# the root disk and the ESP as before, and chainloading leaves systemd-stub
# to measure the UKI into the TPM as before.
#
# With LosOS's Secure Boot key (secure-boot.nix), Microsoft-signed shim is
# what firmware starts, and it starts this GRUB signed (shim.nix):
#
#   UEFI  firmware -> ESP: shim -> GRUB -> chainloads the UKI, through
#         shim's LoadImage, which checks it against the MOK list
#
# NixOS's own GRUB module stays off (boot.nix): it installs GRUB into a
# running system and regenerates its menu on every rebuild, and this OS is
# an image that is never rebuilt in place. The image and the installer put
# this GRUB on the ESP, and losos-grub-update below replaces it with the one
# in /usr once an update has booted well, as systemd-boot-update does for
# systemd-boot.
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

  art = import ../branding {
    inherit pkgs;
    name = config.system.nixos.distroName;
  };

  # Kept so a derivation that still asks for the old menu path evaluates.
  # Nothing boots it. The menu Limine reads is written by limineMenu.
  menu = pkgs.writeText "limine-unused.conf" ''
    # The menu is losos-limine-menu's limine.conf, not this file.
  '';

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

  # What image.nix and the installer run on a disk once its UKIs are in
  # place: the menu, and on x86_64 Limine's BIOS boot code.
  biosInstall = lib.getExe limineMenu;

  # Limine's EFI program, signed with LosOS's key when there is one, so
  # shim will start it. The name on the ESP stays grub.efi because that is
  # the file shim loads from beside itself.
  efi = pkgs.runCommand "losos-limine-${arch.efi}" { } (
    if sb.enable then
      ''
        mkdir $out
        cp ${sb.sign "grub${arch.efi}.efi" "${pkgs.limine}/share/limine/BOOT${lib.toUpper arch.efi}.EFI"} $out/grub.efi
      ''
    else
      ''
        mkdir $out
        cp ${pkgs.limine}/share/limine/BOOT${lib.toUpper arch.efi}.EFI $out/grub.efi
      ''
  );

  # Everything on the ESP that starts GRUB: shim and MokManager beside it
  # with Secure Boot's key, at the removable-media path and at a path of
  # its own; and the certificate MokManager enrolls (shim.nix).
  # A menu the image can boot before losos-limine-menu has rewritten it.
  # The writer replaces it once the UKIs are on the disk.
  stubConf = pkgs.writeText "limine.conf" ''
    timeout: 3
    serial: yes
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

  starters =
    sb.removable "${efi}/grub.efi"
    // {
      "/EFI/losos/grub${arch.efi}.efi" = "${efi}/grub.efi";
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

  # grubenv as grub-editenv creates it: a signature line padded with # to
  # 1024 bytes. GRUB writes it in place and never creates it, so the ESP has
  # to start with one.
  env = pkgs.runCommand "losos-grubenv" { } ''
    {
      printf '# GRUB Environment Block\n'
      head -c 1024 /dev/zero | tr '\0' '#'
    } | head -c 1024 > $out
  '';

  # grubenv from Linux, as GRUB's save_env writes it: the same 1024 bytes
  # rewritten where they are, since GRUB can only write into blocks the file
  # already has. grub-editenv would do the same, and bring all of GRUB into
  # the image for it.
  grubenv = pkgs.writeShellApplication {
    name = "losos-grubenv";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      config.systemd.package
    ];
    text = ''
      # losos-grubenv set NAME VALUE | unset NAME | reset | systemd-boot
      #
      # set, unset and reset edit the ESP's grubenv; reset empties it.
      # systemd-boot succeeds when systemd-boot started this boot rather
      # than GRUB: a disk installed before GRUB replaced it, whose ESP still
      # starts systemd-boot, and whose boots systemd-bless-boot settles.
      info=/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
      if [ "''${1-}" = systemd-boot ]; then
        [ -r "$info" ] || exit 1
        tail -c +5 "$info" | tr -d '\0' | grep -q '^systemd-boot'
        exit
      fi

      file="$(bootctl --print-esp-path)/EFI/losos/grubenv"
      [ -f "$file" ] || { echo "losos-grubenv: no $file" >&2; exit 1; }
      new=$(mktemp)
      trap 'rm -f "$new"' EXIT
      case "''${1-}" in
        set | unset)
          name=''${2:?a variable name}
          {
            printf '# GRUB Environment Block\n'
            grep -v -e '^#' -e "^$name=" "$file" || true
            if [ "$1" = set ]; then
              printf '%s=%s\n' "$name" "''${3?a value}"
            fi
          } > "$new"
          ;;
        reset) printf '# GRUB Environment Block\n' > "$new" ;;
        *)
          echo "usage: losos-grubenv set NAME VALUE | unset NAME | reset | systemd-boot" >&2
          exit 2
          ;;
      esac
      size=$(stat -c %s "$new")
      if [ "$size" -gt 1024 ]; then
        echo "losos-grubenv: $file would outgrow its 1024 bytes" >&2
        exit 1
      fi
      head -c $((1024 - size)) /dev/zero | tr '\0' '#' >> "$new"
      dd if="$new" of="$file" bs=1024 count=1 conv=notrunc status=none
      sync --file-system "$file"
    '';
  };

  # The UKI this boot started, as Limine recorded it. UEFI sets
  # LoaderEntrySelected to the entry name, which is the file name. A BIOS
  # boot puts the same name on the kernel command line as losos.uki=.
  bootedUki = ''
    entry=
    info=/sys/firmware/efi/efivars/LoaderInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
    selected=/sys/firmware/efi/efivars/LoaderEntrySelected-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
    if [ -r "$info" ] && tail -c +5 "$info" | tr -d '\0' | grep -q '^systemd-boot'; then
      exit 0
    fi
    if [ -r "$selected" ]; then
      entry=$(tail -c +5 "$selected" | tr -d '\0')
    fi
    if [ -z "$entry" ]; then
      entry=$(grep -o 'losos\.uki=[^ ]*' /proc/cmdline | head -n 1 | cut -d= -f2- || true)
    fi
  '';

  # Spends one try as soon as userspace can see the ESP. A kernel that dies
  # before this runs is started again with the same count: Limine cannot
  # rename a file or write the count itself.
  trial = pkgs.writeShellApplication {
    name = "losos-limine-trial";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      ${bootedUki}
      [ -n "$entry" ] || exit 0
      case $entry in
        *+*) ;;
        *) exit 0 ;;
      esac
      esp=$(${config.systemd.package}/bin/bootctl --print-esp-path)
      boot=$(${config.systemd.package}/bin/bootctl --print-boot-path)
      file=
      for dir in "$boot" "$esp"; do
        if [ -e "$dir/EFI/Linux/$entry" ]; then
          file="$dir/EFI/Linux/$entry"
          break
        fi
      done
      [ -n "$file" ] || exit 0
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
    '';
  };

  # systemd-bless-boot's job: a UKI that reached boot-complete.target loses
  # its counter. The trial service may already have lowered the count, so
  # this looks up the version rather than the name from boot.
  bless = pkgs.writeShellApplication {
    name = "losos-grub-bless";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gnugrep
      config.systemd.package
      grubenv
    ];
    text = ''
      ${bootedUki}
      [ -n "$entry" ] || exit 0
      case $entry in
        *+*) ;;
        *) exit 0 ;;
      esac
      esp=$(bootctl --print-esp-path)
      boot=$(bootctl --print-boot-path)
      base=''${entry%%+*}
      version=''${base#${id}_}
      version=''${version%.efi}
      file=
      for dir in "$boot" "$esp"; do
        found=$(find "$dir/EFI/Linux" -maxdepth 1 -name "${id}_$version*.efi" 2>/dev/null | head -n 1 || true)
        if [ -n "$found" ]; then
          file=$found
          break
        fi
      done
      [ -n "$file" ] || exit 0
      dir=$(dirname "$file")
      mv "$file" "$dir/${id}_$version.efi"
      sync --file-system "$dir"
      echo "Marked ${id}_$version.efi as good."
      if [ -f "$esp/EFI/losos/grubenv" ]; then
        losos-grubenv reset || true
      fi
    '';
  };

  # What systemd-boot-update.service does for systemd-boot: the GRUB on the
  # ESP is the one the image or the installer put there, and /usr carries
  # the one its version was built with, so after an update this copies the
  # new one over the old. It runs only once losos-grub-bless has run, so a
  # version's GRUB reaches the ESP only after that version has started
  # cleanly, and never goes backwards: a rollback to an older version leaves
  # the newer GRUB, which reads the same names and grubenv, in place.
  #
  # It replaces only files that are already LosOS's: GRUB, recognised by
  # the variable its early config sets, or the systemd-boot older installs
  # started with, which moves them to GRUB. EFI/BOOT/BOOT<ARCH>.EFI on a
  # disk the Windows installer set up is Windows', and stays so. Under a
  # BIOS-capable disk it rewrites GRUB's boot code in the MBR and the BIOS
  # boot partition too, with bios.nix's losos-grub-bios-install, when the
  # MBR already holds GRUB's. EFI/losos/installed records what it installed
  # last, so a boot with nothing new writes nothing.
  update = pkgs.writeShellApplication {
    name = "losos-grub-update";
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
      stamp="version=$version efi=${efi} bios=${toString biosInstall}${lib.optionalString sb.enable " shim=${sb.shimFile}"}"

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
          echo "The ESP has GRUB from $installed, newer than $version; leaving it."
          exit 0
        fi
      fi
      mkdir -p "$dir"

      # The Windows installer lists what it added to the ESP, so that
      # --uninstall removes exactly that (src/losos-windows-installer); a
      # file this adds goes on its list too. Installs from before GRUB kept
      # the list under loader/.
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
      # Limine, the GRUB an older install still starts, or the systemd-boot
      # older installs started with; or shim, with Limine beside it.
      ours() {
        [ -f "$1" ] || return 1
        grep -aq -e 'limine: Base revision' -e losos_disk -e '#### LoaderInfo: systemd-boot' "$1" && return 0
        grep -aq 'github.com/rhboot/shim' "$1" \
          && grep -aq -e 'limine: Base revision' -e losos_disk "$(dirname "$1")/grub${arch.efi}.efi" 2> /dev/null
      }

      # What firmware starts under DIR/NAME: shim, and GRUB and MokManager
      # beside it, with Secure Boot's key; GRUB itself without.
      start_from() {
        ${
          if sb.enable then
            ''
              put ${sb.shimFile} "$1/$2"
              put ${efi}/grub.efi "$1/grub${arch.efi}.efi"
              put ${sb.mokManagerFile} "$1/mm${arch.efi}.efi"
            ''
          else
            ''
              put ${efi}/grub.efi "$1/$2"
            ''
        }
      }

      for file in "BOOT/BOOT${lib.toUpper arch.efi}.EFI" "systemd/systemd-boot${arch.efi}.efi"; do
        if ours "$esp/EFI/$file"; then
          start_from "$(dirname "$esp/EFI/$file")" "$(basename "$file")"
        fi
      done
      put ${efi}/grub.efi "$dir/grub${arch.efi}.efi"
      ${lib.optionalString sb.enable ''
        put ${sb.shimFile} "$dir/shim${arch.efi}.efi"
        put ${sb.mokManagerFile} "$dir/mm${arch.efi}.efi"
        put ${sb.certificateDer} "$esp${sb.enrolPath}"
      ''}
      [ -f "$dir/grubenv" ] || put ${env} "$dir/grubenv"
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

  # The firmware entry the Windows installer adds for GRUB goes first in
  # the boot order, and Windows' own updates and some firmware put Windows
  # Boot Manager back in front of it (docs/windows-installer.md). GRUB's
  # menu offers Windows anyway, so each LosOS boot that gets as far as
  # boot-complete.target puts LosOS's entry first again, whether it names
  # GRUB or the shim beside it (shim.nix). A disk the image or the
  # installer laid out starts from the removable-media path and has no
  # entry, and this does nothing there.
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
      echo "Put Boot$ours, LosOS's GRUB, first in the boot order again."
    '';
  };
in
{
  system.build.grub = {
    inherit
      art
      menu
      efi
      starters
      env
      grubenv
      update
      bootOrder
      limineMenu
      ;
    inherit (pkgs) efibootmgr;
  };

  # image.nix and the installer run this on the disk after its UKIs are
  # copied, while the ESP is not mounted.
  system.build.grubBiosInstall = limineMenu;

  environment.systemPackages = [
    grubenv
    limineMenu
  ];

  # The removable-media path, which firmware boots with no boot entry
  # registered, so this image needs no efibootmgr and no install step; and
  # a copy at the path of its own that the Windows installer registers with
  # the firmware, where Windows' fallback owns the first one.
  image.repart.partitions.${partitionIds.esp}.contents =
    lib.mapAttrs (_: source: { inherit source; }) starters
    // {
      "/EFI/losos/grubenv".source = env;
    };

  # As systemd-bless-boot.service is wired: after boot-complete.target,
  # which systemd-boot-check-no-failures gates (boot.nix), pulled in early
  # so a boot that never gets there leaves the trial running.
  systemd.services.losos-limine-trial = {
    description = "Spend one try of the UKI Limine started";
    wantedBy = [ "basic.target" ];
    after = [ "local-fs.target" ];
    before = [
      "losos-grub-bless.service"
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
      "losos-grub-bless.service"
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

  systemd.services.losos-grub-bless = {
    description = "Mark the UKI GRUB started as good";
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

  systemd.services.losos-grub-update = {
    description = "Install the GRUB this version carries";
    wantedBy = [ "basic.target" ];
    requires = [ "boot-complete.target" ];
    after = [
      "local-fs.target"
      "boot-complete.target"
      "losos-grub-bless.service"
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
