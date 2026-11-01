# The eID card's PKCS#11 module, served to the host for signing in to the
# computer (docs/tokens.md, "Signing in with the eID"). Part of the eID
# machine (eid-machine.nix); modules/eid-signin.nix is the host's half,
# and nixos/tests/eid-signin.nix runs both on one kernel with SoftHSM in
# the module's place.
#
# Each connection on vsock port 7373 is its own `p11-kit remote`, which
# loads the module for that conversation and exits with it. A module that
# stays loaded keeps a PC/SC context open, and the host lends its readers
# to one machine at a time (tokens.nix): the eID machine holds them only
# while the host is reading the card.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.eidPkcs11;
in
{
  options.losos.eidPkcs11 = {
    module = lib.mkOption {
      type = lib.types.str;
      default = "/run/losos-eid/app/lib/libpkcs11_x64.so";
      description = ''
        The PKCS#11 module served: the eID klient's, from the copy the host
        downloaded and shares with the machine.
      '';
    };
    hostCid = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 2;
      description = ''
        The vsock address connections must come from: the host's, or this
        kernel's own where a test runs the host's half beside this one.
      '';
    };
  };

  config = {
    systemd.sockets.losos-eid-module = {
      description = "The eID card's PKCS#11 module, for the host";
      wantedBy = [ "sockets.target" ];
      socketConfig = {
        ListenStream = "vsock::7373";
        Accept = true;
      };
    };

    systemd.services."losos-eid-module@" = {
      description = "Serve the eID card's PKCS#11 module to the host";
      # The module is built for Debian and links, beside the Botan and the
      # card library it carries next to itself, the C++ runtime and
      # libpcsclite, which reaches the host's readers at
      # /run/pcscd/pcscd.comm.
      environment.LD_LIBRARY_PATH = lib.makeLibraryPath [
        pkgs.pcsclite
        pkgs.stdenv.cc.cc.lib
      ];
      serviceConfig = {
        # losos-tokens checks that the host's relay made the connection,
        # then becomes p11-kit remote, which speaks PKCS#11's RPC on it.
        ExecStart = lib.escapeShellArgs [
          (lib.getExe pkgs.losos-tokens)
          "pkcs11-serve"
          "--host-cid"
          (toString cfg.hostCid)
          "--"
          "${lib.getBin pkgs.p11-kit}/bin/p11-kit"
          "remote"
          cfg.module
        ];
        StandardInput = "socket";
        StandardOutput = "socket";
        # Not the socket: the RPC is all that may be written there.
        StandardError = "journal";
        # The closed module runs as nobody in particular, with nothing to
        # write and no network of its own; it needs only the readers'
        # socket, which every user may reach.
        DynamicUser = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateNetwork = true;
        PrivateDevices = true;
        RestrictAddressFamilies = [ "AF_UNIX" ];
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        SystemCallArchitectures = "native";
      };
    };
  };
}
