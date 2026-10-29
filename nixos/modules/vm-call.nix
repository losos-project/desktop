# The host's half of losos-vm-call (a machine runs `losos-vm-call serve` for
# each call it takes): a relay, running as root, with a socket per call in
# /run/losos-vm-call, which `losos-vm-call call <name>` connects to. It asks
# the kernel who connected, lets through whom the call allows, and opens
# the call to its machine from a vsock port below 1024, which only root can
# use; the machine refuses any other. So a program on the host that is not
# root reaches a service machine only through this, and only for its calls.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  calls = config.losos.vmCalls;
in
{
  options.losos.vmCalls = lib.mkOption {
    default = { };
    description = ''
      Commands the host runs in its service machines, by name: each one the
      machine at `cid` serves on `port`.
    '';
    type = lib.types.attrsOf (
      lib.types.submodule {
        options = {
          cid = lib.mkOption {
            type = lib.types.ints.unsigned;
            description = "The machine's vsock address (losos.serviceMachines.<machine>.cid).";
          };
          port = lib.mkOption {
            type = lib.types.ints.unsigned;
            description = "The port the machine takes it on.";
          };
          allow = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [ ];
            example = [
              "group:wheel"
              "active-session"
            ];
            description = ''
              Who besides root may call it: `group:<name>` for a group's
              members, `active-session` for whoever is signed in at the
              screen.
            '';
          };
        };
      }
    );
  };

  config = lib.mkIf (calls != { }) {
    environment.systemPackages = [ pkgs.losos-vm-call ];

    environment.etc."losos-vm-call/calls".text = lib.concatLines (
      lib.mapAttrsToList (
        name: call:
        lib.concatStringsSep " " (
          [
            name
            (toString call.cid)
            (toString call.port)
          ]
          ++ call.allow
        )
      ) calls
    );

    systemd.services.losos-vm-call-relay = {
      description = "Let calls through to the service machines";
      wantedBy = [ "multi-user.target" ];
      restartTriggers = [ config.environment.etc."losos-vm-call/calls".source ];
      serviceConfig = {
        ExecStart = "${lib.getExe pkgs.losos-vm-call} relay /etc/losos-vm-call/calls";
        Restart = "always";
        RestartSec = 1;
        RuntimeDirectory = "losos-vm-call";
        RuntimeDirectoryMode = "0755";
        # Root for the ports below 1024 and nothing else it needs: no files
        # to write but its sockets, no devices, no other address families
        # than its two sockets' and NSS's.
        CapabilityBoundingSet = [ "CAP_NET_BIND_SERVICE" ];
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_VSOCK"
        ];
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" ];
      };
    };
  };
}
