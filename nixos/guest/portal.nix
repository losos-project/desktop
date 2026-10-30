# The portal machine, sys-portal (docs/portal-machine.md): where the
# dialogs of the desktop's portals run, picking a file, saying yes to an
# app, framing a screenshot, for the host's apps and every machine's. GTK's
# file chooser and derisk's portal parse what is in a person's files,
# names, folders and pictures, and here that happens in a machine with no
# network card and nothing that lasts past its next start.
#
# A service machine (service.nix), booted from the host's store. It runs
# for the person whose session is in front: the host's relay
# (losos-portal relay) shares their home and their drives' mount point
# into it over virtiofs, at the same paths as on the host, so a file
# picked here is the file the host has, and says who they are on a 9p
# share, `person`, before it starts it. The machine makes the account to
# match, with the same uid, and keeps a session for it, whose user
# manager runs:
#
#  - waypipe, which carries the dialogs' windows to the desktop over vsock,
#    as every machine's;
#  - `losos-portal serve`, which takes the relay's requests and puts them
#    to GTK's and derisk's portal backends on the session bus;
#  - `losos-portal agent`, the one thing of the derisk session's agent
#    socket derisk's portal asks for, a screenshot, through the relay.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  person = "/run/losos-portal-person";

  # The account, made from the host's line `name:uid:gid:gecos:home` once
  # sysusers has written the machine's own, before logind starts and
  # starts the person's user manager, which the linger file asks for.
  makePerson = pkgs.writeShellScript "losos-portal-person" ''
    set -eu
    PATH=${
      lib.makeBinPath [
        pkgs.coreutils
        pkgs.util-linux
        pkgs.gnugrep
      ]
    }
    mkdir -p ${person}
    mount -t 9p -o ro,trans=virtio,version=9p2000.L person ${person}
    IFS=: read -r name uid gid gecos home < ${person}/person
    # The host wrote it, but nothing here takes a name or path on trust.
    echo "$name" | grep -Eq '^[a-z_][a-z0-9_-]{0,31}$'
    echo "$uid$gid" | grep -Eq '^[0-9]+$'
    [ "$uid" -ge 1000 ] && [ "$uid" -lt 4294967294 ]
    case "$home" in /home/*) ;; *) exit 1 ;; esac
    grep -q "^[^:]*:[^:]*:$gid:" /etc/group || echo "$name:x:$gid:" >> /etc/group
    echo "$name:x:$uid:$gid:$gecos:$home:/run/current-system/sw/bin/nologin" >> /etc/passwd
    echo "$name:!*:1::::::" >> /etc/shadow
    mkdir -p "$home" "/run/media/$name"
    mount -t virtiofs home "$home"
    mount -t virtiofs media "/run/media/$name"
    mkdir -p /var/lib/systemd/linger
    touch "/var/lib/systemd/linger/$name"
  '';
in
{
  boot.kernelModules = [
    "virtiofs"
    "vmw_vsock_virtio_transport"
  ];

  systemd.services.losos-portal-person = {
    description = "Make the account of the person the portals are for";
    wantedBy = [ "sysinit.target" ];
    after = [
      "systemd-sysusers.service"
      "systemd-modules-load.service"
      "local-fs.target"
    ];
    before = [
      "systemd-logind.service"
      "sysinit.target"
    ];
    unitConfig.DefaultDependencies = false;
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = makePerson;
    };
  };

  services.dbus.enable = true;
  # The backends the requests go to, which D-Bus starts on the first one.
  services.dbus.packages = [
    pkgs.xdg-desktop-portal-gtk
    pkgs.derisk
  ];
  systemd.packages = [
    pkgs.xdg-desktop-portal-gtk
    pkgs.derisk
  ];
  environment.systemPackages = [
    pkgs.derisk
    pkgs.losos-portal
    pkgs.papirus-icon-theme
    pkgs.adwaita-icon-theme
  ];
  # GTK's settings, such as the icon theme derisk writes there, from the
  # person's home (desktop.nix).
  programs.dconf.enable = true;
  # Mesa's software renderers: derisk's dialogs draw with Vulkan, and the
  # machine has no GPU.
  hardware.graphics.enable = true;
  fonts.enableDefaultPackages = true;
  fonts.packages = [
    pkgs.inter
    pkgs.nerd-fonts.hack
  ];
  fonts.fontconfig.defaultFonts.sansSerif = [ "Inter" ];

  systemd.user.settings.Manager.DefaultEnvironment =
    "WAYLAND_DISPLAY=wayland-0 XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=derisk";

  # The dialogs' windows, to the desktop, as in every machine
  # (common.nix).
  systemd.user.services.waypipe = {
    description = "Show the portals' dialogs on the host's desktop";
    wantedBy = [ "default.target" ];
    serviceConfig = {
      ExecStart = lib.escapeShellArgs [
        (lib.getExe pkgs.waypipe)
        "--vsock"
        "--socket"
        "2:7350"
        "--no-gpu"
        "--display"
        "wayland-0"
        "server"
        "--"
        "${pkgs.coreutils}/bin/sleep"
        "infinity"
      ];
      Restart = "always";
    };
  };

  systemd.user.services.losos-portal-serve = {
    description = "Take the portal requests the host's relay passes on";
    wantedBy = [ "default.target" ];
    after = [ "waypipe.service" ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-portal} serve";
      Restart = "always";
    };
  };

  systemd.user.services.losos-portal-agent = {
    description = "Screenshots from the host's session for derisk's portal";
    wantedBy = [ "default.target" ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-portal} agent";
      Restart = "always";
    };
  };
}
