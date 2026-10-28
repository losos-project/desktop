# `losos-portal forward` as a portal backend, `losos` (docs/portal-machine.md):
# on the host (portal-machine.nix) and in every machine (guest/common.nix),
# xdg-desktop-portal hands it the portals whose dialogs run in the portal
# machine, and it hands them to the host's relay. D-Bus starts it at the
# first request, as a unit of the user's manager.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.portalBackend;
  busName = "org.freedesktop.impl.portal.desktop.losos";
  command = "${lib.getExe pkgs.losos-portal} forward${lib.optionalString cfg.inMachine " --machine"}";
  # The portals it answers, for the frontend; which desktop uses it for
  # which is each desktop's portals.conf.
  backend = pkgs.runCommand "losos-portal-backend" { } ''
    mkdir -p $out/share/xdg-desktop-portal/portals $out/share/dbus-1/services
    cat > $out/share/xdg-desktop-portal/portals/losos.portal <<END
    [portal]
    DBusName=${busName}
    Interfaces=org.freedesktop.impl.portal.FileChooser;org.freedesktop.impl.portal.Access;org.freedesktop.impl.portal.Account;org.freedesktop.impl.portal.Screenshot;org.freedesktop.impl.portal.Wallpaper;
    END
    cat > $out/share/dbus-1/services/${busName}.service <<END
    [D-BUS Service]
    Name=${busName}
    Exec=${command}
    SystemdService=losos-portal.service
    END
  '';
in
{
  options.losos.portalBackend = {
    enable = lib.mkEnableOption "the portal machine's backend for xdg-desktop-portal";
    inMachine = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Whether this is a machine, which reaches the relay over vsock and
        copies the files picked for its apps in, rather than the host.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    xdg.portal.extraPortals = [ backend ];
    systemd.user.services.losos-portal = {
      description = "Portal dialogs from the portal machine";
      serviceConfig = {
        Type = "dbus";
        BusName = busName;
        ExecStart = command;
      };
    };
  };
}
