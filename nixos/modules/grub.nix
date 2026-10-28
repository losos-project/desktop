# GRUB, the boot loader on every PC (docs/boot-loaders.md):
#
#   UEFI  firmware -> ESP: GRUB, menu and theme inside -> chainloads the
#         newest UKI -> systemd-stub -> kernel
#   BIOS  firmware -> MBR -> GRUB's core in the BIOS boot partition
#         (bios.nix) -> the newest UKI's kernel, initrd and command line
#
# Both are the same menu (grub.cfg) and the same theme (nixos/branding), and
# count a new UKI's tries the same way, in grubenv on the ESP, which
# losos-grub-bless settles in the UKIs' file names once a boot succeeds.
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

  menu = pkgs.replaceVars ./grub.cfg { inherit id; };

  # bios.nix's installer of GRUB's BIOS boot code, on x86_64 only.
  biosInstall =
    if config.system.build ? grubBiosInstall then
      lib.getExe config.system.build.grubBiosInstall
    else
      null;

  # The menu's EFI half: the disk to look for UKIs on is the one GRUB was
  # started from, "hd0" of "hd0,gpt1". bli is the Boot Loader Interface,
  # the EFI variables systemd-boot used to set: LoaderDevicePartUUID, which
  # systemd-gpt-auto-generator finds root and the ESP by, and LoaderInfo,
  # which says GRUB started this boot. It sets them when it loads, as every
  # module built into the image does when GRUB starts.
  unsigned = import ./grub-image.nix {
    inherit pkgs menu art;
    format = "efi";
    output = "grub.efi";
    early = ''
      regexp --set=1:losos_disk '^([^,]+)' "$root"
      normal (memdisk)/grub.cfg
    '';
    modules = [
      "bli"
      "efifwsetup"
      "true"
      "loadenv"
      "reboot"
      "halt"
      "sleep"
    ]
    # The recovery shell's boot, which starts the kernel bios.nix puts beside
    # the UKI as a BIOS boot does (grub.cfg's losos_extra). There is no such
    # kernel on aarch64.
    ++ lib.optionals (arch.name == "x86_64") [
      "linux"
      "probe"
    ];
  };

  # The same GRUB signed with LosOS's key, for shim to start, when there is
  # a key; signed or not, it is $efi/grub.efi.
  efi =
    if sb.enable then
      pkgs.runCommand "losos-grub-${arch.efi}-signed" { } ''
        mkdir $out
        cp ${sb.sign "grub${arch.efi}.efi" "${unsigned}/grub.efi"} $out/grub.efi
      ''
    else
      unsigned;

  # Everything on the ESP that starts GRUB: shim and MokManager beside it
  # with Secure Boot's key, at the removable-media path and at a path of
  # its own; and the certificate MokManager enrolls (shim.nix).
  starters =
    sb.removable "${efi}/grub.efi"
    // {
      "/EFI/losos/grub${arch.efi}.efi" = "${efi}/grub.efi";
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

  # systemd-bless-boot's job for a UKI GRUB started: grub.cfg leaves the
  # trial in grubenv, and this settles it in the file name, where sysupdate
  # reads it.
  bless = pkgs.writeShellApplication {
    name = "losos-grub-bless";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnused
      config.systemd.package
      grubenv
    ];
    text = ''
      # systemd-boot counted this boot itself, and systemd-bless-boot
      # settles it; grubenv, if it holds anything, is from a GRUB boot of
      # the same disk on another machine, and is left for that one.
      if losos-grubenv systemd-boot; then
        exit 0
      fi

      esp=$(bootctl --print-esp-path)
      boot=$(bootctl --print-boot-path)
      env="$esp/EFI/losos/grubenv"
      [ -f "$env" ] || exit 0

      get() { sed -n "s/^$1=//p" "$env" | head -n 1; }
      entry=$(get losos_entry)
      left=$(get losos_left)
      booted=$(get losos_booted)
      [ -n "$entry" ] || exit 0

      file=
      for dir in "$boot" "$esp"; do
        if [ -e "$dir/EFI/Linux/$entry" ]; then
          file="$dir/EFI/Linux/$entry"
          break
        fi
      done

      if [ -n "$file" ]; then
        dir=$(dirname "$file")
        base=''${entry%%+*}
        if [ "$booted" = "$entry" ]; then
          # The UKI on trial is the one that reached boot-complete.target:
          # it loses its counter, as systemd-bless-boot would leave it.
          mv "$file" "$dir/$base.efi"
          echo "Marked $base.efi as good."
        elif [ "$left" = 0 ]; then
          # Its tries ran out and GRUB started an older UKI instead. The name
          # systemd-boot gives such an entry, +0-<tries made>, keeps it last
          # in the menu however grubenv changes.
          counter=''${entry#*+}
          counter=''${counter%.efi}
          tries=''${counter%%-*}
          made=0
          if [ "$counter" != "$tries" ]; then
            made=''${counter#*-}
          fi
          mv "$file" "$dir/$base+0-$((tries + made)).efi"
          echo "Marked $base as failed after $((tries + made)) tries."
        else
          # An older UKI picked by hand while the newer one is on trial:
          # the trial goes on at the next boot.
          exit 0
        fi
        sync --file-system "$dir"
      fi

      # The trial is settled, or its UKI is gone, so grubenv goes back to
      # empty.
      losos-grubenv reset
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
      # LosOS's GRUB or the systemd-boot older installs started with; or
      # shim, with LosOS's GRUB beside it to start (shim.nix). shim's own
      # SBAT line names its repository.
      ours() {
        [ -f "$1" ] || return 1
        grep -aq -e losos_disk -e '#### LoaderInfo: systemd-boot' "$1" && return 0
        grep -aq 'github.com/rhboot/shim' "$1" \
          && grep -aq losos_disk "$(dirname "$1")/grub${arch.efi}.efi" 2> /dev/null
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
      ${lib.optionalString (biosInstall != null) ''
        source=$(findmnt --noheadings --raw --output SOURCE --types vfat --mountpoint "$esp")
        disk=/dev/$(lsblk --noheadings --nodeps --output PKNAME "$source")
        if sfdisk --json "$disk" | jq -e '.partitiontable.partitions[] | select(.type == "21686148-6449-6E6F-744E-656564454649")' > /dev/null \
          && head -c 440 "$disk" | grep -a 'GRUB ' > /dev/null; then
          ${biosInstall} "$disk"
          echo "Installed GRUB's BIOS boot code on $disk."
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
      ;
    inherit (pkgs) efibootmgr;
  };

  environment.systemPackages = [ grubenv ];

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
