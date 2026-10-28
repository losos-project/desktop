# Company accounts: people in a LosOS box's LosOS Login log in here with the
# same name and password (docs/company.md).
#
# A company keeps its people in LosOS Login, the box's Authelia, on a box
# behind the edge gateway on its network. An administrator joins a computer
# to that box once, `losos-login-bridge join`, through the join service
# below, which asks polkit for an administrator of this computer and then
# signs in to LosOS Login as one of the box's. From then on the PAM module
# goes before pam_systemd_home at both logins, derisk's and the console's:
# LosOS Login accepting a password makes or updates the person's homed
# record with it, LosOS Login refusing keeps them out, and LosOS Login out
# of reach leaves the decision to homed, whose record holds the password
# LosOS Login last accepted. So a company laptop logs in offline, and a
# person the company removed cannot log in while the box can be asked.
#
# Nothing here is on until someone joins: with no
# /var/lib/losos-login-bridge/joined.json the module steps aside and the
# stacks are the ones sign-in.nix describes.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  bridge = pkgs.losos-login-bridge;
  # Before systemd_home-early (11600), the first module that reads the
  # password, so the password the person typed reaches LosOS Login first and
  # the record exists by the time homed is asked. requisite: a refusal ends
  # the login, and PAM_IGNORE (not joined, a local account) leaves the stack
  # as it was.
  rule = {
    order = 11500;
    control = "requisite";
    modulePath = "${bridge}/lib/security/pam_losos_login.so";
    # A login screen's PATH is its display manager's, so the module is told
    # where homectl and userdbctl are.
    args = [ "bin=${config.systemd.package}/bin" ];
  };
in
{
  # The command, for Settings and a terminal, and the polkit action the
  # join service asks about (share/polkit-1/actions).
  environment.systemPackages = [ bridge ];

  security.pam.services = {
    derisk-login.rules.auth.losos_login = rule;
    login.rules.auth.losos_login = rule;
  };

  # The joined box: readable by the logins, written by the join service.
  systemd.tmpfiles.settings."10-losos-login-bridge"."/var/lib/losos-login-bridge".d = {
    mode = "0755";
    user = "root";
    group = "root";
  };

  # The join service: a process per connection, as root, which asks polkit
  # about the process at the other end before it reads anything. The socket
  # is open to everyone because polkit is the gate, as with homed's own.
  systemd.sockets.losos-login-bridge = {
    description = "Joining this computer to a company's LosOS Login";
    wantedBy = [ "sockets.target" ];
    socketConfig = {
      ListenStream = "/run/losos-login-bridge/join.sock";
      SocketMode = "0666";
      Accept = true;
    };
  };

  # Between logins: read the box's people list and lock the accounts of
  # people it removed or disabled, so a laptop does not keep logging them
  # in offline (src/losos-login-bridge/src/sync.rs). Only a computer joined
  # to a box that gave it a token has anything to do.
  systemd.services.losos-login-bridge-sync = {
    description = "Locking company accounts the company removed";
    unitConfig.ConditionPathExists = "/var/lib/losos-login-bridge/directory-token";
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    path = [ config.systemd.package ];
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${lib.getExe bridge} sync";
      ProtectHome = true;
      PrivateTmp = true;
      NoNewPrivileges = true;
      RestrictAddressFamilies = [
        "AF_UNIX"
        "AF_INET"
        "AF_INET6"
      ];
    };
  };
  systemd.timers.losos-login-bridge-sync = {
    wantedBy = [ "timers.target" ];
    timerConfig = {
      OnBootSec = "2min";
      # Often enough that someone let go is locked out within minutes of a
      # laptop seeing the office network.
      OnUnitActiveSec = "5min";
      RandomizedDelaySec = "1min";
    };
  };

  systemd.services."losos-login-bridge@" = {
    description = "Joining this computer to a company's LosOS Login";
    # pkcheck, which asks polkit about the caller.
    path = [ (lib.getBin config.security.polkit.package) ];
    serviceConfig = {
      ExecStart = "${lib.getExe bridge} serve";
      StandardInput = "socket";
      StandardOutput = "socket";
      StandardError = "journal";
      # It reads the box over the network and writes one file.
      ProtectSystem = "strict";
      ReadWritePaths = [ "/var/lib/losos-login-bridge" ];
      ProtectHome = true;
      PrivateTmp = true;
      NoNewPrivileges = true;
      ProtectKernelTunables = true;
      ProtectKernelModules = true;
      ProtectControlGroups = true;
      RestrictAddressFamilies = [
        "AF_UNIX"
        "AF_INET"
        "AF_INET6"
      ];
      SystemCallArchitectures = "native";
      LockPersonality = true;
      MemoryDenyWriteExecute = true;
      RuntimeMaxSec = "5min";
    };
  };
}
