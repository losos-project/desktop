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

  session = pkgs.writeShellApplication {
    name = "losos-installer-session";
    runtimeInputs = [ pkgs.derisk ];
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
      exec "$derisk" installer -- "$backend" "$@"
    '';
  };
in
{
  options.losos.installer.livepatch.backend = lib.mkOption {
    type = lib.types.str;
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
        ExecStartPre = "${lib.getExe pkgs.coreutils}/bin/rm -f /run/losos-installer/livepatch-offer.json";
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
