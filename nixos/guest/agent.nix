# The Agent's machine (docs/agent.md, "Its own machine"): derisk's Agent,
# with the Claude Code it runs and everything that runs, in a machine of
# each person's own rather than in their session. modules/agent.nix makes
# one per person, `agent-<user>`, when they first open the Agent.
#
# A service machine (service.nix): booted from the host's store, part of
# the image and updated with it, and hidden from Machines. Unlike the
# others it has a screen's worth of userland, the Agent's window drawn in
# software and carried to the desktop by waypipe as any machine's are
# (common.nix, "The guest's display"), and a home of its own, on the
# encrypted disk agent.nix gives each person's machine. Nothing of the
# host's is shared with it: the person's files, their drives' and their
# network shares' come as they come to every machine, a copy of what they
# pick in the portal machine's dialog, saved back when the Agent saves it
# (docs/portal-machine.md). What it has of the desktop beyond its window
# and those dialogs is losos-agent-machine's tools channel, which takes
# derisk's desktop tools and web links to open, and refuses the rest.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  # The one account, whose home is the machine's disk. Nobody signs
  # in as it: its systemd manager runs from boot (linger), and the
  # Agent's window, its chats and routines all run there.
  user = "user";

  channels = lib.getExe pkgs.losos-agent-machine;

  # Links the Agent opens, such as Claude Code's sign-in page, open in the
  # person's browser on the desktop; files leave it only through the portal.
  xdgOpen = pkgs.writeShellScriptBin "xdg-open" ''
    exec ${channels} open-uri "$@"
  '';
in
{
  # The portals' dialogs, from the portal machine.
  imports = [ ../modules/portal-backend.nix ];

  # Its root is a tmpfs as every service machine's, here as large as half
  # its memory: /tmp, and what the Agent's previews build there.
  fileSystems."/".options = lib.mkForce [
    "mode=0755"
    "size=50%"
  ];
  # The home, the disk agent.nix gives the machine, made the first time it
  # boots there, and handed back to the host as the Agent frees it.
  losos.service.state = "/home/${user}";
  fileSystems."/home/${user}".options = [ "discard" ];
  systemd.tmpfiles.rules = [ "d /home/${user} 0700 ${user} users -" ];
  # vsock, for the window, the launch port and the tools channel.
  boot.kernelModules = [ "vmw_vsock_virtio_transport" ];

  # A system account to sysusers, which makes no others, with the uid and
  # group a person's first account has elsewhere.
  users.users.${user} = {
    isSystemUser = true;
    uid = 1000;
    group = "users";
    home = "/home/${user}";
    shell = pkgs.bashInteractive;
    linger = true;
    # The disk is the home; tmpfiles above gives it to this user.
    createHome = false;
  };

  # One network card on libvirt's `default` network, which reaches out
  # through the host's NAT and so through the network machine when there is
  # one, as every machine does. networkd, as the other service machines
  # have: one card, one DHCP lease, nothing a person configures.
  networking = {
    useNetworkd = true;
    useDHCP = true;
  };
  services.resolved.enable = true;

  # The guest's display, as common.nix's: a waypipe server whose socket
  # every app here takes for a compositor, each connection a vsock stream to
  # the host's desktop, which frames the Agent's window as this machine's.
  systemd.user.services.waypipe = {
    description = "Show this machine's windows on the host's desktop";
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
  systemd.user.settings.Manager.DefaultEnvironment =
    "WAYLAND_DISPLAY=wayland-0 XDG_SESSION_TYPE=wayland";

  # Portals, as common.nix's. Opening and saving a file, and picking a
  # folder for a project, are the portal machine's dialogs, through the
  # host's relay, which hands this machine a copy of what the person
  # picked and takes it back when the Agent saves it, and only while the
  # person whose machine this is (agent.nix's <pm:person>) is in front.
  # Nothing else here is a portal's: the Agent's links go through
  # xdg-open above, and it reads the desktop's theme itself.
  losos.portalBackend = {
    enable = true;
    inMachine = true;
  };
  # What xdg.portal.enable would install, without the xdg-utils it adds:
  # its scripts are Perl, which service.nix's perlless profile refuses, and
  # xdg-open here is the one above.
  services.dbus.packages = [ pkgs.xdg-desktop-portal ];
  systemd.packages = [ pkgs.xdg-desktop-portal ];
  programs.fuse.enable = true;
  environment.pathsToLink = [ "/share/xdg-desktop-portal" ];
  environment.etc."xdg/xdg-desktop-portal/portals.conf".text = lib.generators.toINI { } {
    preferred = {
      default = "none";
      "org.freedesktop.impl.portal.FileChooser" = "losos";
      "org.freedesktop.impl.portal.Access" = "losos";
      "org.freedesktop.impl.portal.Account" = "losos";
      "org.freedesktop.impl.portal.Screenshot" = "losos";
      "org.freedesktop.impl.portal.Wallpaper" = "losos";
    };
  };

  # derisk's agent socket, where `derisk mcp` and the Agent look for it,
  # carried to the host's tools channel.
  systemd.user.services.losos-agent-relay = {
    description = "Carry derisk's desktop tools to the host";
    wantedBy = [ "default.target" ];
    serviceConfig = {
      ExecStart = "${channels} relay";
      Restart = "always";
    };
  };

  # Opens the window, or hands a link to the one open: what the host's
  # desktop entry asks for, through its root end, which alone may connect.
  systemd.user.sockets.losos-agent-launch = {
    description = "Open the Agent when the desktop asks";
    wantedBy = [ "sockets.target" ];
    socketConfig = {
      ListenStream = "vsock::7365";
      Accept = true;
    };
  };
  systemd.user.services."losos-agent-launch@" = {
    description = "Open the Agent";
    # systemd-run finds the Agent on the unit's PATH, which NixOS keeps to
    # a handful of tools.
    path = [ pkgs.derisk.agent ];
    serviceConfig = {
      ExecStart = "${channels} launch";
      StandardInput = "socket";
      StandardError = "journal";
    };
  };

  # Lazy: the machine powers off once nothing has needed it for ten
  # minutes. It is needed while the Agent's window or a routine's run is
  # open, which are both derisk-agent, and while a routine is scheduled,
  # since its timer is this machine's: a person with routines keeps it.
  # agent.nix starts it again on the next open.
  systemd.services.losos-agent-idle = {
    description = "Power off the Agent's machine when nothing needs it";
    path = [
      pkgs.procps
      pkgs.coreutils
      pkgs.systemd
    ];
    script = ''
      mark=/run/losos-agent-idle
      if pgrep -u ${user} -x derisk-agent >/dev/null ||
        compgen -G '/home/${user}/.config/systemd/user/timers.target.wants/derisk-agent-routine-*.timer' >/dev/null; then
        rm -f "$mark"
      elif [ ! -e "$mark" ]; then
        touch "$mark"
      elif [ -n "$(find "$mark" -mmin +10)" ]; then
        systemctl poweroff
      fi
    '';
    serviceConfig.Type = "oneshot";
  };
  systemd.timers.losos-agent-idle = {
    wantedBy = [ "timers.target" ];
    timerConfig = {
      OnBootSec = "1min";
      OnUnitActiveSec = "1min";
    };
  };

  # perlless.nix, through service.nix, refuses Perl anywhere in the
  # closure. Here it comes with `derisk mcp`: derisk links libvirt for its
  # machines, and libvirt's Xen support carries syslinux's Perl scripts.
  # Nothing in the machine runs them, its boot is still the profile's,
  # Perl-free, and the machine runs from the host's store, which holds
  # derisk and all of that already.
  system.forbiddenDependenciesRegexes = lib.mkForce [ ];

  # Mesa's software renderers, as in common.nix: no GPU here, and the
  # Agent draws with egui.
  hardware.graphics.enable = true;
  # service.nix's minimal profile goes without fonts' configuration and
  # the icon and MIME databases, which a window needs.
  fonts.fontconfig.enable = true;
  xdg.icons.enable = true;
  xdg.mime.enable = true;
  fonts.enableDefaultPackages = true;
  fonts.packages = [
    pkgs.inter
    pkgs.nerd-fonts.hack
  ];
  fonts.fontconfig.defaultFonts.sansSerif = [ "Inter" ];

  # Claude Code is not in the image (docs/agent.md, "Where it ships"); a
  # person who installs it here with Anthropic's installer gets it in
  # ~/.local/bin, on PATH, and its glibc binary finds a loader through
  # nix-ld. It stays on the machine's disk, so it outlasts each boot.
  environment.localBinInPath = true;
  programs.nix-ld.enable = true;

  environment.systemPackages = [
    pkgs.xdg-desktop-portal
    pkgs.derisk.agent
    # `derisk mcp`, the desktop's tools for every chat, which the Agent
    # runs from PATH and which reach the desktop through the relay above.
    pkgs.derisk
    # Without Perl, which the perlless profile refuses: no send-email or
    # git-svn, which an agent has no use for.
    pkgs.gitMinimal
    pkgs.adwaita-icon-theme
    (lib.hiPrio xdgOpen)
  ]
  # The portal backend's definition, which xdg.portal would have linked.
  ++ config.xdg.portal.extraPortals;
}
