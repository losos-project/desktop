# pm stays the system manager. This image comes from nixpkgs, but software a
# user builds or runs on the machine goes through pm: signed recipes built into
# .cpkg archives, their entrypoints run in pm's sandbox, and pmd as its worker.
#
# Nothing extra is needed for that sandbox. It is unprivileged user namespaces
# plus landlock, both of which NixOS's kernel allows by default.
#
# pm also administers the machine, as a user of its own. `pm system` asks
# `pmd --system`, which runs as `pm` and asks polkit about each caller, and an
# administrator authenticates as themselves before pm starts, stops or
# restarts a unit or updates the system. That is all it does: it runs no
# command as root, which is why sudo, su and run0 are gone and not replaced
# (accounts.nix). docs/pm.md has the commands.
{ lib, pkgs, ... }:

#
# pm's plugins are shipped under /run/current-system/sw/share/pm/plugins and
# not installed into anyone's config: pm runs a plugin only when it is signed
# by a key its user trusts, and that trust is the user's to give. Where a user
# runs Home Manager, the flake's homeModules.pm (nixos/home/pm.nix) installs
# and signs them from their own configuration.
{
  environment.systemPackages = [
    pkgs.pm
    pkgs.pm-plugins
  ];
  environment.pathsToLink = [ "/share/pm" ];

  # The identity the system trusts to manage units. A system user with no
  # shell and no password: nobody logs in as pm, only the daemon runs as it.
  users.users.pm = {
    isSystemUser = true;
    group = "pm";
    description = "pm, the system manager";
    home = "/var/lib/pm";
  };
  users.groups.pm = { };

  # The D-Bus policy (only pm owns org.pm1.System) and the activation file,
  # from pm's data/; its polkit actions arrive through the system profile.
  services.dbus.packages = [ pkgs.pm ];

  # Started by the bus on the first call and gone after a minute idle, so it
  # costs nothing on a machine nobody administers.
  systemd.services.pm-system = {
    description = "pm's system service";
    documentation = [ "https://github.com/losos-project/pm#administering-the-machine" ];
    serviceConfig = {
      Type = "dbus";
      BusName = "org.pm1.System";
      ExecStart = "${pkgs.pm}/bin/pmd --system";
      User = "pm";
      Group = "pm";
      StateDirectory = "pm";
      # Everything the daemon does, it does over the system bus, as pm: it
      # needs no capability, no network, no devices and nothing of the file
      # system.
      CapabilityBoundingSet = "";
      PrivateDevices = true;
      NoNewPrivileges = true;
      ProtectSystem = "strict";
      ProtectHome = true;
      PrivateTmp = true;
      PrivateNetwork = true;
      ProtectKernelTunables = true;
      ProtectKernelModules = true;
      ProtectKernelLogs = true;
      ProtectControlGroups = true;
      ProtectClock = true;
      ProtectHostname = true;
      RestrictAddressFamilies = [ "AF_UNIX" ];
      RestrictNamespaces = true;
      RestrictRealtime = true;
      RestrictSUIDSGID = true;
      LockPersonality = true;
      MemoryDenyWriteExecute = true;
      SystemCallArchitectures = "native";
      SystemCallFilter = [ "@system-service" ];
      UMask = "0077";
    };
  };

  # The Machines app will not make a machine until this account's copy of
  # the libvirt plugin is signed by a key the account trusts. derisk offers
  # a button for that, and a fresh account never finds it: the button sits
  # under a card and New machine stays quiet. Doing the same copy and
  # `pm sign` at login makes the key on this computer, for this account,
  # rather than shipping a signature in the image. The other plugins stay
  # unsigned until the person signs them.
  systemd.user.services.losos-pm-machines = {
    description = "Sign pm's libvirt plugin so this account can make machines";
    wantedBy = [ "graphical-session-pre.target" ];
    before = [ "graphical-session.target" ];
    unitConfig.ConditionUser = "!@system";
    path = [
      pkgs.pm
      pkgs.coreutils
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    script = ''
      shipped=/run/current-system/sw/share/pm/plugins/libvirt.wasm
      if [ ! -f "$shipped" ]; then
        exit 0
      fi
      config=''${XDG_CONFIG_HOME:-$HOME/.config}
      plugins="$config/pm/plugins"
      mkdir -p "$plugins"
      installed="$plugins/libvirt.wasm"
      signature="$installed.sig"
      if [ ! -f "$installed" ] || ! cmp -s "$shipped" "$installed"; then
        install -m 0644 "$shipped" "$installed"
      fi
      # pm sign makes a key and trusts it the first time. A signature older
      # than the plugin is what the Machines app calls unsigned.
      if [ ! -f "$signature" ] || [ "$installed" -nt "$signature" ]; then
        pm sign "$installed"
      fi
    '';
  };

  security.polkit.extraConfig = lib.mkMerge [
    ''
      // pm manages units without asking anyone: pmd --system has already
      // asked polkit about its caller (org.pm1.system.*) before it does.
      // All but the debug shell, a root shell on tty9 with no password,
      // which would make starting a unit a way to root. The recovery shell
      // (docs/troubleshooting.md) starts it from the kernel's command line.
      polkit.addRule(function (action, subject) {
        if (
          action.id === "org.freedesktop.systemd1.manage-units" &&
          subject.user === "pm" &&
          action.lookup("unit") !== "debug-shell.service"
        ) {
          return polkit.Result.YES;
        }
        return polkit.Result.NOT_HANDLED;
      });
    ''
    # Last, so a narrower rule before it (disk.nix lets an administrator ask
    # for a factory reset) still answers first. Without it an administrator
    # could still drive systemd directly, and run0 or `systemd-run` would hand
    # them root past pm; with it, `systemctl restart` and run0 are refused and
    # pm is the one way in. Root is never asked: systemd lets it through
    # without polkit.
    (lib.mkAfter ''
      polkit.addRule(function (action, subject) {
        var systemd = [
          "org.freedesktop.systemd1.manage-units",
          "org.freedesktop.systemd1.manage-unit-files",
          "org.freedesktop.systemd1.reload-daemon",
          "org.freedesktop.systemd1.set-environment",
        ];
        if (systemd.indexOf(action.id) >= 0 && subject.user !== "pm") {
          return polkit.Result.NO;
        }
        return polkit.Result.NOT_HANDLED;
      });
    '')
  ];
}
