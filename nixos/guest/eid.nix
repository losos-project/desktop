# The Slovak eID klient in a machine (docs/tokens.md, "The Slovak eID"),
# which signs in to slovensko.sk and other Slovak public services with the
# eID card in the host's reader. The Ministry of the Interior builds it
# only for x86_64, and its licence lets nobody pass it on, so the image
# carries only `losos-eid`, which downloads it on its first start, and the
# system around it.
#
# Its card goes through the host's pcscd (tokens.nix), its windows through
# X11 (common.nix), and the host forwards the port a sign-in page hands it
# the card's work on (BSI TR-03124's 24727) while it listens there.
{
  lib,
  pkgs,
  ...
}:

let
  # What the eID klient links from the system, beside the Qt, OpenSSL and
  # ICU it carries: X11's client libraries, fonts, the C++ runtime and
  # libpcsclite, which finds the host's readers at /run/pcscd/pcscd.comm.
  # An FHS environment, since it is built for Debian's file layout.
  env = pkgs.buildFHSEnv {
    name = "losos-eid-env";
    targetPkgs =
      p: with p; [
        libice
        libsm
        libx11
        libxcb
        fontconfig
        freetype
        stdenv.cc.cc.lib
        libgpg-error
        libselinux
        pcsclite
        zlib
        # It opens the page a sign-in ends on through xdg-open.
        xdg-utils
      ];
    runScript = pkgs.writeShellScript "losos-eid-run" ''exec "$@"'';
  };

  launcher = pkgs.writeShellApplication {
    name = "losos-eid";
    runtimeInputs = [
      env
      pkgs.curl
      pkgs.gnutar
      pkgs.gzip
      pkgs.squashfsTools
      pkgs.binutils
      pkgs.gawk
      pkgs.findutils
      pkgs.coreutils
    ];
    text = builtins.readFile ./losos-eid.sh;
  };
in
lib.mkIf pkgs.stdenv.hostPlatform.isx86_64 {
  environment.systemPackages = [
    launcher
    (pkgs.makeDesktopItem {
      name = "losos-eid";
      desktopName = "eID klient";
      genericName = "Slovak eID card";
      comment = "Sign in to slovensko.sk and other Slovak public services with your eID card";
      exec = "losos-eid";
      icon = "auth-smartcard";
      categories = [ "Utility" ];
    })
  ];
}
