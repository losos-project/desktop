# derisk, as a tree of systemd user units.
#
# derisk (github.com/dasmatus/derisk) replaces GNOME. It launches every app as
# a transient unit in app-graphical.slice, binds its own derisk-session.target
# to graphical-session.target, and serves its agent socket from a
# socket-activated user unit, so `systemctl --user status` still describes
# the desktop and systemd-oomd can still act on one application's cgroup.
#
# derisk drives the display itself: with no Wayland or X11 session to nest in,
# its compositor takes the seat's GPU and input devices from logind and
# scans out through DRM/KMS, for the login screen and the session alike.
{
  lib,
  pkgs,
  ...
}:

let
  # cage is gone. It ran the greeter and the session while derisk could only
  # draw nested in another compositor's window, and needed a wrapper to retry
  # on wlroots' CPU renderer when a GPU had no GL driver. derisk now scans out
  # on its own, and Mesa's software rasterizer covers a GPU without GL, so
  # there is nothing left for a kiosk compositor to do underneath it.
  derisk = lib.getExe pkgs.derisk;
in
{
  # derisk is the display manager too: `derisk display-manager` runs as root
  # from VT1 and does only PAM and session starts, drawing nothing. It runs
  # derisk's lock screen as the login screen (`derisk greeter`), so logging in
  # and unlocking are the same screen, and the greeter runs as its own
  # unprivileged user and talks to it over a socket only that user can open.
  # After a login it opens the user's session through the derisk-login PAM
  # service and runs the session command as them. Several people can be
  # logged in at once, each session on a free VT of its own; the greeter
  # comes back when the session on the screen ends or asks to switch user
  # (docs/users.md).
  #
  # This replaced gdm, a GNOME Shell process with its own accounts daemon and
  # session chooser, all to offer one session. greetd would also have done:
  # the greeter speaks its protocol and runs under it unchanged. But it is one
  # more daemon between derisk and PAM doing the same small job, and without
  # it nothing on the login path is anything but derisk and systemd.
  #
  # --execute makes app launches and lock/suspend/reboot real rather than
  # simulated. There is one kind of session, so no session chooser and no
  # wayland-sessions entry: the greeter is handed the command outright.
  #
  # Boot to graphical.target, which is what pulls the display manager in.
  # NixOS' display-manager module used to set this for gdm; with gdm gone the
  # default fell back to multi-user.target and VT1 stayed dark.
  systemd.defaultUnit = "graphical.target";
  systemd.services.derisk-display-manager = {
    description = "derisk display manager";
    aliases = [ "display-manager.service" ];
    wantedBy = [ "graphical.target" ];
    # VT1 is the display manager's first, as it was gdm's: no getty there.
    # Further sessions take the VTs after it that nothing has open; the
    # display manager holds one open while it starts a login screen there, so
    # logind's autovt getty never starts on it.
    conflicts = [ "getty@tty1.service" ];
    # After Plymouth has let go of the display (splash.nix), which leaves
    # its last frame there for the greeter to draw over.
    after = [
      "systemd-user-sessions.service"
      "getty@tty1.service"
      "systemd-logind.service"
      "plymouth-quit.service"
    ];
    wants = [ "systemd-user-sessions.service" ];
    serviceConfig = {
      ExecStart = lib.escapeShellArgs [
        derisk
        "display-manager"
        "--vt"
        "1"
        "--"
        derisk
        "greeter"
        "--"
        derisk
        "session"
        "--execute"
      ];
      Type = "notify";
      Restart = "always";
      # Sessions live in their own logind scopes; stopping the display
      # manager should not take a logged-in desktop down with it.
      KillMode = "process";
      # Its own runtime directory, for the greeter's socket and switch.sock,
      # where a session asks for the login screen (Switch user).
      RuntimeDirectory = "derisk-dm";
      RuntimeDirectoryMode = "0711";
    };
  };

  # The greeter's user: a system user with no home and no login, whose only
  # power is asking the display manager to check a password.
  users.users.derisk-greeter = {
    isSystemUser = true;
    group = "derisk-greeter";
    description = "derisk login screen";
  };
  users.groups.derisk-greeter = { };

  # derisk-login is a full login: startSession puts pam_systemd (the logind
  # session) in it, and homed adds pam_systemd_home. derisk-greeter is the
  # greeter's own session, which only needs pam_systemd, for the logind
  # session derisk takes the display and input devices through. Its auth stack goes unused: the
  # display manager never authenticates the greeter or sets its credentials.
  security.pam.services.derisk-login.startSession = true;
  security.pam.services.derisk-greeter.startSession = true;

  # derisk-session.target, derisk-agent.socket and derisk-agent.service,
  # and derisk-index's socket and service. The index starts with the
  # graphical session, so home is indexed by the time the palette asks;
  # the units' [Install] sections mean nothing to NixOS, so the wants are
  # here. Not derisk-session.target: nothing requires it, so it stops
  # itself as soon as it is reached and would take the index with it.
  systemd.packages = [
    pkgs.derisk
    pkgs.derisk.index
  ];
  systemd.user.services.derisk-index.wantedBy = [ "graphical-session.target" ];

  # Generate the PAM stack used by derisk's lock screen. With homed enabled,
  # NixOS includes pam_systemd_home in this service.
  security.pam.services.derisk = { };

  # Locking on idle and before sleep is derisk's: it runs swayidle, which
  # holds logind's sleep delay inhibitor until derisk says its lock screen
  # is up (derisk's README, "Idle"). A system oneshot running `loginctl
  # lock-sessions` Before=sleep.target could not do that, since logind's
  # Lock signal returns before anything is drawn.

  # derisk's Files, Settings, Text Editor, System Monitor and Calculator are
  # built into the derisk binary, which replaces GNOME's Files. Its terminal,
  # derisk-terminal, is a package of its own. It replaced foot, which the
  # image shipped only because derisk had no terminal of its own.
  # derisk draws every icon in its chrome and apps from the icon theme the
  # derisk theme names, tinted grey; its built-in themes name Papirus-Dark
  # and Papirus. A theme that lacks an icon falls back through its Inherits=
  # chain and then Papirus, so Papirus stays in the image whatever theme is
  # set; without any of them derisk falls back to glyphs.
  environment.systemPackages = [
    pkgs.derisk
    pkgs.derisk.terminal
    # `derisk-index search ...` asks the index from a terminal.
    pkgs.derisk.index
    pkgs.papirus-icon-theme
    # derisk starts it to learn when nobody uses the session (above).
    pkgs.swayidle
  ];

  # derisk-terminal draws with the theme's monospace family through its Nerd
  # Font build, the same letters with the icons prompts and TUIs print. Hack
  # is the theme's default; the symbols-only Nerd Font gives the icons to a
  # theme that names another family.
  #
  # Inter is the theme's interface font, the one the LosOS web interface is
  # drawn in. That interface asks for the system UI font, and fontconfig
  # answers sans-serif, so making Inter sans-serif here also gives it to
  # what does not read derisk's theme: Danube's pages, Flatpak apps and
  # every `system-ui` stylesheet. derisk's egui apps load it themselves
  # through mcsapi. Before it, the theme named Ubuntu, which only egui's
  # built-in copy drew, and sans-serif was fontconfig's own DejaVu Sans, so
  # the rest of the desktop was lettered in a third face.
  fonts.packages = [
    pkgs.inter
    pkgs.nerd-fonts.hack
    pkgs.nerd-fonts.symbols-only
  ];
  fonts.fontconfig.defaultFonts.sansSerif = [ "Inter" ];

  # What GNOME's module used to switch on, kept where something in the session
  # still uses it. Audio for every app. udisks2 and upower are bus-activated
  # and back removable drives and the overview's battery widget. The keyring
  # is the Secret Service every app that stores a password asks for.
  #
  # Portals are derisk's and GTK's. derisk's backend answers what only the
  # session knows: the appearance from its theme, screenshots, the wallpaper
  # and which apps have windows. GTK's does the rest, the file chooser and
  # the access dialog derisk's own portals ask through among them. Which
  # backend gets which portal is derisk's derisk-portals.conf, taken from the
  # package rather than restated here. Screen sharing is
  # xdg-desktop-portal-wlr's, over the compositor's capture protocols; it
  # asks which screen or window through `derisk choose`, a dialog in the
  # session, named by the config below for XDG_CURRENT_DESKTOP=derisk.
  #
  # Bluetooth and power profiles are derisk Settings' Bluetooth page and its
  # Power mode row, which run bluetoothctl and powerprofilesctl; both
  # modules put those on PATH. The command palette's "Power Mode" rows run
  # the same powerprofilesctl. bluetooth.service starts only where
  # /sys/class/bluetooth exists, so a machine without an adapter runs
  # nothing. geoclue stays off: nothing in derisk asks for a location.
  hardware.bluetooth.enable = true;
  services.power-profiles-daemon.enable = true;
  services.pipewire = {
    enable = true;
    pulse.enable = true;
  };
  services.udisks2.enable = true;
  services.upower.enable = true;
  # A battery that reaches critical powers the machine off. NixOS's default
  # asks for hybrid sleep, which needs hibernation, and nothing running this
  # OS hibernates (docs/hibernation.md).
  services.upower.criticalPowerAction = "PowerOff";
  # GSettings' dconf backend. GTK takes the icon theme from GSettings before
  # derisk's settings.ini, and the GTK portal backend serves it to Flatpak
  # apps, so derisk writes the theme's icon theme there; without dconf every
  # app sees GSettings' default, Adwaita (docs/nixos.md, "One icon theme").
  programs.dconf.enable = true;
  services.gnome.gnome-keyring.enable = true;
  xdg.portal = {
    enable = true;
    extraPortals = [
      pkgs.derisk
      pkgs.xdg-desktop-portal-gtk
      pkgs.xdg-desktop-portal-wlr
    ];
    configPackages = [ pkgs.derisk ];
  };
  # The store path, not a bare name: the portal is a user service started by
  # D-Bus, and its PATH is not the session's.
  environment.etc."xdg/xdg-desktop-portal-wlr/derisk".text = ''
    [screencast]
    chooser_type=dmenu
    chooser_cmd=${pkgs.derisk}/bin/derisk choose
  '';

  # X11 apps, through xwayland-satellite: Xwayland as an ordinary Wayland
  # client of derisk, turning each X11 window into an xdg_toplevel, so the
  # compositor needs nothing beyond xdg-shell and wp_viewporter, both of
  # which it has. It starts with the session, on the lowest display number
  # nobody else's session holds (sessions share /tmp/.X11-unix), and once
  # Xwayland is up it puts DISPLAY into the user manager's environment and
  # D-Bus activation's. derisk starts every app as a transient unit of that
  # manager, so apps started after it reach Xwayland, and derisk's own
  # DISPLAY= at session start stays the value until then.
  systemd.user.services.xwayland-satellite =
    let
      start = pkgs.writeShellScript "xwayland-satellite-start" ''
        n=0
        while [ -e "/tmp/.X11-unix/X$n" ] || [ -e "/tmp/.X$n-lock" ]; do
          n=$((n + 1))
        done
        echo ":$n" > "$RUNTIME_DIRECTORY/display"
        exec ${lib.getExe pkgs.xwayland-satellite} ":$n"
      '';
      export = pkgs.writeShellScript "xwayland-satellite-export" ''
        display=$(cat "$RUNTIME_DIRECTORY/display")
        ${pkgs.systemd}/bin/systemctl --user set-environment "DISPLAY=$display"
        ${pkgs.dbus}/bin/dbus-update-activation-environment --systemd "DISPLAY=$display"
      '';
    in
    {
      description = "Xwayland for X11 apps";
      bindsTo = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      requisite = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        # satellite says READY once Xwayland has its display.
        Type = "notify";
        NotifyAccess = "all";
        RuntimeDirectory = "xwayland-satellite";
        ExecStart = start;
        ExecStartPost = export;
        ExecStopPost = "${pkgs.systemd}/bin/systemctl --user unset-environment DISPLAY";
        Restart = "on-failure";
      };
    };

  # Let systemd-oomd act on a desktop session before the kernel OOM killer
  # does. oomd watches cgroup pressure and kills the worst-behaved application
  # cgroup while the machine is still responsive, but only in slices that opt
  # in; this opts in every user's. NixOS sets the limit at 80% pressure on
  # user.slice. 50% is Fedora's for a desktop, where a session stalling for
  # half of every interval is already unusable.
  systemd.oomd = {
    enable = true;
    enableUserSlices = true;
  };
  systemd.slices.user.sliceConfig.ManagedOOMMemoryPressureLimit = "50%";

  # Firmware updates. fwupd itself is bus-activated; the metadata refresh
  # timer is what makes updates appear without anyone asking.
  services.fwupd.enable = true;

  # Mesa, for derisk's GLES renderer and its GBM scanout buffers.
  hardware.graphics.enable = true;

  # Nothing is needed for FIDO2. The pm tree had to add CONFIG_HIDRAW to its
  # kernel fragment because arm64's defconfig leaves it off; nixpkgs'
  # common-config.nix sets it for every architecture, and systemd's fido_id and
  # uaccess rules do the rest. `homectl update --fido2-device=auto` works as-is.
}
