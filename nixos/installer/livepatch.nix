# Livepatch the installer's derisk and backend from the channel's newest
# installer-live bundle on GHCR, while the medium is running.
{
  config,
  lib,
  pkgs,
  losos,
  ...
}:

let
  inherit (pkgs.stdenv.hostPlatform.parsed.cpu) name;

  repository =
    let
      repo = lib.toLower (losos.releaseRepository or "losos-project/desktop");
    in
    "ghcr.io/${repo}/installer-live";

  configJson = pkgs.writeText "losos-installer-live.json" (
    builtins.toJSON {
      inherit repository;
      inherit (losos) version channel;
      arch = name;
    }
  );

  livepatch = pkgs.callPackage ../pkgs/losos-installer-livepatch.nix {
    inherit configJson;
  };

  x11Fallback = pkgs.callPackage ../pkgs/losos-installer-x11.nix { };

  session = pkgs.writeShellApplication {
    name = "losos-installer-session";
    runtimeInputs = [
      pkgs.derisk
      pkgs.coreutils
    ];
    text = ''
      set -euo pipefail
      default_derisk=${lib.getExe pkgs.derisk}
      default_backend=${lib.escapeShellArg config.losos.installer.livepatch.backend}
      if [ -f /run/losos-installer/session.env ]; then
        # shellcheck disable=SC1091
        . /run/losos-installer/session.env
      fi
      derisk=''${DERISK:-$default_derisk}
      backend=''${BACKEND:-$default_backend}

      # The video devices virt-manager offers by default cannot draw the
      # compositor's GBM buffers: QXL has no GBM scanout, and virtio-vga
      # without 3D acceleration has no virgl renderer. Both still make a
      # working X display, so run the same installer through winit there.
      # The driver can be forced with LOSOS_INSTALLER_X11_DRIVER for other
      # virtual video devices.
      virtual_gpu_driver() {
        local vendor device card
        for card in /sys/class/drm/card*/device; do
          [ -r "$card/vendor" ] || continue
          read -r vendor < "$card/vendor" || continue
          device=$(cat "$card/device" 2>/dev/null || true)
          case "$vendor:$device" in
            0x1b36:0x0001) echo qxl; return 0 ;; # QXL
            0x1af4:0x1042|0x1af4:0x1050) echo fbdev; return 0 ;; # virtio GPU
          esac
        done
        return 1
      }

      driver=''${LOSOS_INSTALLER_X11_DRIVER:-}
      if [ -z "$driver" ]; then
        driver=$(virtual_gpu_driver || true)
      fi
      if [ -n "$driver" ]; then
        export LOSOS_INSTALLER_X11_DRIVER="$driver"
        exec ${lib.getExe x11Fallback} "$derisk" "$driver" "$backend" "$@"
      fi

      exec "$derisk" installer -- "$backend" "$@"
    '';
  };
in
{
  options.losos.installer.livepatch.backend = lib.mkOption {
    type = lib.types.path;
    internal = true;
    description = "The losos-installer serve wrapper the session starts.";
  };

  config = {
    fileSystems."/nix/.rw-store".options = lib.mkAfter [ "size=4G" ];

    environment.etc."losos/installer-live.json".source = configJson;
    environment.systemPackages = [ livepatch ];

    system.build.installerSession = lib.getExe session;

    systemd.services.losos-installer-livepatch-check = {
      description = "Check for a newer installer on the channel";
      before = [ "derisk-installer.service" ];
      wantedBy = [ "derisk-installer.service" ];
      after = [
        "network.target"
        "NetworkManager.service"
      ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStartPre = "${lib.getExe' pkgs.coreutils "rm"} -f /run/losos-installer/livepatch-offer.json";
        ExecStart = "${lib.getExe livepatch} check";
      };
    };

    systemd.services.losos-installer-livepatch-apply = {
      description = "Update the installer from the channel";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${lib.getExe livepatch} apply";
      };
    };
  };
}
