# Signing in to the computer with the Slovak eID card (docs/tokens.md,
# "Signing in with the eID"). homed already logs a person in with a
# PKCS#11 token whose certificate it enrolled: it encrypts the home's
# secret to the certificate's key, and at login the card decrypts it once
# the person types the card's PIN. The eID card speaks PKCS#11 only
# through the Ministry of the Interior's closed module, so that module
# runs in a machine of its own, the eID machine (nixos/guest/eid-machine.nix),
# and the host reaches it with p11-kit's remote mode:
#
# - homed's and homectl's p11-kit load the `losos-eid` module, which is
#   p11-kit's RPC to /run/losos-eid/pkcs11.sock rather than a library;
# - systemd hands each connection there to `losos-tokens pkcs11-relay`,
#   which lets in root (homed) and the person at the seat, and passes it
#   to the eID machine over vsock from a privileged port;
# - the eID machine runs `p11-kit remote` with the module for each one.
#
# The image carries no part of the eID klient, whose licence lets nobody
# pass it on: `losos-eid-signin` has the host download it when a person
# turns eID sign-in on, and shares the copy with the machine.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.eidSignin;
  name = "losos-eid";
  # losos-eid-fetch.service's StateDirectory under DynamicUser.
  share = "/var/lib/private/losos-eid";
  socket = "/run/losos-eid/pkcs11.sock";

  signin = pkgs.writeShellApplication {
    name = "losos-eid-signin";
    runtimeInputs = [
      pkgs.gnutls
      pkgs.openssl
      pkgs.gawk
      pkgs.gnugrep
      pkgs.gnused
      pkgs.coreutils
      config.systemd.package
    ];
    runtimeEnv = {
      P11_KIT_CLIENT = "${pkgs.p11-kit}/lib/pkcs11/p11-kit-client.so";
      P11_KIT_SERVER_ADDRESS = "unix:path=${socket}";
    };
    text = builtins.readFile ./losos-eid-signin.sh;
  };
in
{
  options.losos.eidSignin = {
    enable = lib.mkOption {
      type = lib.types.bool;
      # The eID klient is built for x86_64 alone.
      default = pkgs.stdenv.hostPlatform.isx86_64;
      description = "Whether a person can sign in with the Slovak eID card.";
    };
    machine = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Whether the eID machine is part of the image. Off only where
        nixos/tests/eid-signin.nix runs the machine's half on the host's
        own kernel.
      '';
    };
    machineCid = lib.mkOption {
      type = lib.types.ints.unsigned;
      # Fixed, so the relay knows where to go without asking libvirt;
      # libvirt hands out the low ones to people's machines.
      default = 7373;
      description = "The eID machine's vsock address.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        # homed's helper reads the card at login, and homed and homectl
        # when a person enrols it. No other program sees the card this way;
        # Danube and the machines use the readers as they always have.
        environment.etc."pkcs11/modules/${name}.module".text = ''
          remote: unix:path=${socket}
          enable-in: systemd-homed, systemd-homework, homectl
        '';

        systemd.sockets.losos-eid-pkcs11 = {
          description = "The eID card's PKCS#11 module, from the eID machine";
          wantedBy = [ "sockets.target" ];
          socketConfig = {
            ListenStream = socket;
            Accept = true;
            # Every program may knock; the relay decides who is let in.
            SocketMode = "0666";
          };
        };

        systemd.services."losos-eid-pkcs11@" = {
          description = "Pass a PKCS#11 client to the eID machine";
          serviceConfig = {
            ExecStart = "${lib.getExe pkgs.losos-tokens} pkcs11-relay --machine-cid ${toString cfg.machineCid}";
            StandardInput = "socket";
            StandardOutput = "socket";
            # Not the socket: only the RPC may be written there.
            StandardError = "journal";
            DynamicUser = true;
            # A privileged vsock port is the eID machine's proof that a
            # connection is the relay's, so the relay alone may bind one.
            AmbientCapabilities = [ "CAP_NET_BIND_SERVICE" ];
            CapabilityBoundingSet = [ "CAP_NET_BIND_SERVICE" ];
            ProtectSystem = "strict";
            ProtectHome = true;
            PrivateTmp = true;
            PrivateDevices = true;
            RestrictAddressFamilies = [
              "AF_UNIX"
              "AF_VSOCK"
            ];
            NoNewPrivileges = true;
            SystemCallArchitectures = "native";
          };
        };

        environment.systemPackages = [ signin ];
      }

      (lib.mkIf cfg.machine {
        losos.serviceMachines.${name} = {
          module = ../guest/eid-machine.nix;
          title = "The eID card's sign-in machine";
          cid = cfg.machineCid;
          extraDevices = ''
            <filesystem type='mount' accessmode='squash'>
              <source dir='${share}'/>
              <target dir='losos-eid'/>
              <readonly/>
            </filesystem>
          '';
        };

        # The eID machine runs from the first boot that has the eID klient
        # downloaded, which is when someone turned eID sign-in on: at the
        # first login screen after it, the card is ready to be asked.
        systemd.paths.losos-eid-machine = {
          description = "Start the eID machine once the eID klient is here";
          wantedBy = [ "multi-user.target" ];
          pathConfig = {
            PathExists = "${share}/ready";
            Unit = "losos-service-machine-${name}.service";
          };
        };

        # Downloads the eID klient for the machine, when losos-eid-signin
        # starts it (an administrator's to allow, as polkit asks for any unit
        # started by hand) and weekly after, for the Ministry's updates.
        # As nobody in particular: the download is unpacked, never run,
        # here.
        systemd.services.losos-eid-fetch = {
          description = "Download the eID klient for the eID machine";
          wants = [ "network-online.target" ];
          after = [ "network-online.target" ];
          serviceConfig = {
            Type = "oneshot";
            DynamicUser = true;
            StateDirectory = name;
            ExecStart = pkgs.writeShellScript "losos-eid-fetch-for-machine" ''
              ${lib.getExe pkgs.losos-eid-fetch} "$STATE_DIRECTORY"
              touch "$STATE_DIRECTORY/ready"
            '';
            ProtectSystem = "strict";
            ProtectHome = true;
            PrivateTmp = true;
            PrivateDevices = true;
            RestrictAddressFamilies = [
              "AF_UNIX"
              "AF_INET"
              "AF_INET6"
            ];
            NoNewPrivileges = true;
            CapabilityBoundingSet = "";
          };
        };
        systemd.timers.losos-eid-fetch = {
          description = "Look for a new eID klient weekly";
          wantedBy = [ "timers.target" ];
          unitConfig.ConditionPathExists = "${share}/ready";
          timerConfig = {
            OnCalendar = "weekly";
            Persistent = true;
            RandomizedDelaySec = "6h";
          };
        };
      })
    ]
  );
}
