# The portal machine, sys-portal (docs/portal-machine.md): the dialogs of
# the desktop's portals, which every app asks for to open a file, to be
# let do something or to take a screenshot, run in a machine of their own
# (nixos/guest/portal.nix), for the host's apps and every machine's alike,
# as Qubes keeps its file dialogs out of dom0.
#
#  - losos-portal-relay.service, the host's relay, is the one way to it: it
#    takes requests from the host's apps on a socket and from machines over
#    vsock, knows which machine asks from its vsock address, starts the
#    portal machine for the person in front, and passes on the files
#    picked for a machine, only those.
#  - `losos-portal forward`, on the session bus here and in every machine
#    (guest/common.nix), is xdg-desktop-portal's backend for those portals,
#    `losos`, and hands each request to the relay.
#
# It is lazy: the relay starts it at the first request and stops it once
# none has come for a while (losos.machines.portal.idle). The portals with
# no dialog (the theme, which apps run, screen sharing's chooser) stay on
# the host, so an app starting does not start it.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  name = "sys-portal";
  cid = 4104;
  cfg = config.losos.machines.portal;
in
{
  options.losos.machines.portal = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether the portals' dialogs run in the portal machine.";
    };
    idle = lib.mkOption {
      type = lib.types.ints.positive;
      default = 600;
      description = "Seconds the portal machine runs with nothing asked of it before it is stopped.";
    };
    boot = lib.mkOption {
      type = lib.types.ints.positive;
      default = 120;
      description = "Seconds a request waits for the portal machine to boot.";
    };
  };

  config = lib.mkIf cfg.enable {
    losos.serviceMachines.${name} = {
      module = ../guest/portal.nix;
      title = "The portal machine";
      # GTK's dialogs and derisk's, drawn in software; the balloon gives
      # back what they do not use.
      memory = 768;
      cpus = 2;
      inherit cid;
      # Nothing it does needs a network, so it has none.
      interfaces = [ ];
      # The relay stops it when idle and starts it when asked.
      whenStopped = "stop";
      # The person's home and drives come over virtiofs.
      sharedMemory = true;
    };
    virtualisation.libvirtd.qemu.vhostUserPackages = [ pkgs.virtiofsd ];

    systemd.services.losos-portal-relay = {
      description = "Pass portal requests to the portal machine";
      wantedBy = [ "multi-user.target" ];
      requires = [ "libvirtd.service" ];
      after = [ "libvirtd.service" ];
      path = [
        config.virtualisation.libvirtd.package
        config.systemd.package
        pkgs.gnutar
      ];
      environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
      serviceConfig = {
        ExecStart = "${lib.getExe pkgs.losos-portal} relay --machine ${name} --idle ${toString cfg.idle} --boot ${toString cfg.boot}";
        Restart = "on-failure";
        RuntimeDirectory = "losos-portal";
        RuntimeDirectoryMode = "0755";
        # The grants outlive a restart of the relay, not of the host.
        RuntimeDirectoryPreserve = true;
      };
    };

    # The host's apps reach it through the backend here.
    losos.portalBackend.enable = true;
    xdg.portal.config.derisk = {
      # derisk's own derisk-portals.conf, but for the portals whose
      # dialogs are the machine's.
      default = [ "gtk" ];
      "org.freedesktop.impl.portal.Settings" = [
        "derisk"
        "gtk"
      ];
      "org.freedesktop.impl.portal.Background" = [ "derisk" ];
      "org.freedesktop.impl.portal.ScreenCast" = [ "wlr" ];
      "org.freedesktop.impl.portal.FileChooser" = [ "losos" ];
      "org.freedesktop.impl.portal.Access" = [ "losos" ];
      "org.freedesktop.impl.portal.Account" = [ "losos" ];
      "org.freedesktop.impl.portal.Screenshot" = [ "losos" ];
      "org.freedesktop.impl.portal.Wallpaper" = [ "losos" ];
    };
  };
}
