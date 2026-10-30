# The host's security keys and smart card readers, in a machine
# (docs/tokens.md). nixos/modules/tokens.nix is the host's half.
#
# `losos-tokens guest` makes a uhid copy of every FIDO key the host has, so
# a browser, libfido2 or ssh here finds a key on USB, and listens where
# programs look for pcscd, passing each connection to the host's. The guest
# runs no pcscd of its own: it has no USB controller to find a reader on.
# It also tells the host while the eID klient listens on localhost here, so
# a sign-in page on the host can hand the card's work to it.
{
  lib,
  pkgs,
  ...
}:

{
  # /dev/uhid, which the copies are made through, and the HID driver that
  # gives each copy its hidraw node. udev loads hid-generic for the first
  # HID device it sees by its modalias, but not always before a key the
  # host already had is copied at boot, and a copy no driver took is
  # invisible to every program.
  boot.kernelModules = [
    "uhid"
    "hid_generic"
  ];

  systemd.services.losos-tokens = {
    description = "Use the host's security keys and smart card readers";
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      # The eID client's port (BSI TR-03124), which the host forwards here
      # while the eID klient listens on it (eid.nix).
      ExecStart = "${lib.getExe pkgs.losos-tokens} guest --serve 24727";
      # Root, for /dev/uhid and /run/pcscd. It keeps trying the host, and
      # the copies go whenever the host does.
      Restart = "always";
      RestartSec = 1;
      ProtectSystem = "strict";
      RuntimeDirectory = "pcscd";
      RuntimeDirectoryPreserve = true;
      ProtectHome = true;
      PrivateTmp = true;
      DevicePolicy = "closed";
      DeviceAllow = [ "/dev/uhid rw" ];
      RestrictAddressFamilies = [
        "AF_UNIX"
        "AF_VSOCK"
        "AF_INET"
      ];
      NoNewPrivileges = true;
    };
  };

  # Nobody logs in here, so uaccess gives the machine's user no key: its
  # apps run under a lingering manager with no seat. The copies are its
  # instead, through the group every normal account is in.
  services.udev.extraRules = ''
    SUBSYSTEM=="hidraw", ENV{ID_FIDO_TOKEN}=="1", GROUP="users", MODE="0660"
  '';

  # libpcsclite for the programs that ask for readers, which finds the
  # relay at /run/pcscd/pcscd.comm, and the CLI tools that show what the
  # machine sees.
  environment.systemPackages = [
    pkgs.pcsclite
    pkgs.libfido2
  ];
}
