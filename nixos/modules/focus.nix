# Focus mode (docs/focus-mode.md): while a game, or a program for CAD,
# 3D, video or music, is open, the work the desktop does in the background
# waits, and carries on once the last of them has closed.
#
# GameMode is the switch, because games already ask it for exactly this
# (Steam's `gamemoderun %command%`, Lutris, Heroic, Feral's ports), and
# derisk asks it on behalf of every program whose desktop entry says it is
# one of those. Its custom start and end scripts run `losos-focus start`
# and `stop` (src/losos-focus), which freeze the person's background.slice
# and start losos-focus@<user>.service, which freezes the system's
# losos-background.slice; both stop the timers that would start more, and
# the system's half stops Ollama, whose model holds gigabytes of memory a
# game wants. Frozen, not stopped: an update or a backup halfway through
# resumes where it was instead of starting again.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  focus = lib.getExe pkgs.losos-focus;
  # The person's half: systemd's own background.slice, for work nobody is
  # waiting on. derisk's file index answers the palette on a thread of its
  # own, so the palette lists files by name alone until it thaws.
  # losos-ping and losos-boxes, which not every build has, are moved by
  # the modules that make them.
  userUnits = [
    "derisk-index"
    "losos-backup"
    "losos-backup-now"
    "danube-captive-watch"
    "app-windows-downloads"
  ];
  background.serviceConfig.Slice = "losos-background.slice";
  # Stopped for the duration and started again after: the timers of the
  # work above, since systemd refuses to start a unit in a frozen slice
  # and a run due mid-game would fail, and Ollama, whose model holds
  # gigabytes of memory a game wants. A timer started again fires at once
  # when it counts from its last run or is Persistent, as the updates',
  # backups' and ad-block lists' are. One a build lacks is not running,
  # and so is left alone.
  systemStopped = [
    "systemd-sysupdate.timer"
    "systemd-sysupdate-update.timer"
    "losos-adblock-update.timer"
    "fwupd-refresh.timer"
    "logrotate.timer"
    "fstrim.timer"
    "podman-prune.timer"
  ]
  ++ lib.optional config.services.ollama.enable "ollama.service";
  userStopped = [
    "losos-backup.timer"
    "losos-ping.timer"
    # systemd's own, whose service is in background.slice already.
    "systemd-tmpfiles-clean.timer"
  ];
in
{
  environment.systemPackages = [ pkgs.losos-focus ];

  programs.gamemode = {
    enable = true;
    settings = {
      # Whether the screen may blank or lock stays the program's to say,
      # through Wayland's idle inhibitor, which derisk honours: a CAD
      # session left for lunch still locks. GameMode would ask
      # org.freedesktop.ScreenSaver, which nothing here serves.
      general.inhibit_screensaver = 0;
      custom = {
        start = "${focus} start ${toString userStopped}";
        end = "${focus} stop";
      };
      # GPU clocks stay as the driver sets them (gpu-helper is not
      # granted below either): overclocking is the person's to choose.
    };
  };

  systemd.slices.losos-background = {
    description = "Background work focus mode holds";
  };

  systemd.services = lib.mkMerge [
    {
      losos-adblock-update = background;
      fwupd-refresh = lib.mkIf config.services.fwupd.enable background;
      logrotate = lib.mkIf config.services.logrotate.enable background;
      fstrim = lib.mkIf config.services.fstrim.enable background;
      podman-prune = lib.mkIf config.virtualisation.podman.autoPrune.enable background;
    }
    # The PC's updates. A download frozen long enough for the server to
    # hang up leaves a partial slot, which update.nix's vacuum clears
    # before the next try.
    (lib.mkIf config.systemd.sysupdate.enable {
      systemd-sysupdate = background;
      systemd-sysupdate-update = background;
    })
    {
      "losos-focus@" = {
        description = "Hold background work while %i is in focus mode";
        serviceConfig = {
          Type = "exec";
          ExecStartPre = "${focus} system pause ${toString systemStopped}";
          # Until %i stops it, or logs out without GameMode running its end
          # script, so the system's updates never stay frozen until the next
          # boot.
          ExecStart = "${focus} system hold %i";
          # Also after a failed start, so nothing stays half frozen. It
          # leaves everything as it is while someone else's focus holds.
          ExecStopPost = "${focus} system resume";
        };
      };
    }
  ];

  systemd.user.services = lib.genAttrs userUnits (_: {
    serviceConfig.Slice = "background.slice";
  });

  # Before pm.nix's last rule, which refuses every systemd action to anyone
  # but pm. A person on this seat may start and stop their own focus unit
  # and nothing else; it only pauses background work and takes no
  # argument. GameMode's helpers set the processor's governor and turn the
  # split-lock slowdown off for a game, which its own rule grants to a
  # `gamemode` group that homed accounts are not in.
  security.polkit.extraConfig = ''
    polkit.addRule(function (action, subject) {
      if (!subject.local || !subject.active) {
        return polkit.Result.NOT_HANDLED;
      }
      if (
        action.id === "org.freedesktop.systemd1.manage-units" &&
        action.lookup("unit") === "losos-focus@" + subject.user + ".service" &&
        (action.lookup("verb") === "start" || action.lookup("verb") === "stop")
      ) {
        return polkit.Result.YES;
      }
      if (
        action.id === "com.feralinteractive.GameMode.governor-helper" ||
        action.id === "com.feralinteractive.GameMode.procsys-helper"
      ) {
        return polkit.Result.YES;
      }
      return polkit.Result.NOT_HANDLED;
    });
  '';
}
