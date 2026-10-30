# Security keys and smart card readers stay plugged into the host, and the
# machines use them through it (docs/tokens.md). The guest's half is
# nixos/guest/tokens.nix.
#
# A key or a reader is something a person signs in with, so it stays where
# the login screen can reach it, and no USB passthrough may hand it to a
# machine for good. A machine gets a copy of each key, which
# `losos-tokens host` feeds from the real one, and its programs' PC/SC
# connections reach the host's pcscd. Neither ever leaves the host, so no
# machine can keep a key from the host or from the others, and pulling a key
# out takes it from every machine at once.
{
  config,
  lib,
  pkgs,
  ...
}:

{
  # pcscd and the CCID driver, which covers every reader on USB and the
  # card side of keys such as a YubiKey. NixOS starts it on the first
  # connection to /run/pcscd/pcscd.comm, which polkit then decides on below.
  services.pcscd.enable = true;

  systemd.services.losos-tokens = {
    description = "Lend the host's security keys and smart card readers to the machines";
    wantedBy = [ "multi-user.target" ];
    after = [ "libvirtd.service" ];
    # `virsh -r` names the machine at a connection's vsock address.
    path = [ config.virtualisation.libvirtd.package ];
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig = {
      # 24727 is where an eID client listens for the page that hands it a
      # sign-in (BSI TR-03124), as slovensko.sk does: Danube on the host
      # reaches the eID klient in whichever machine runs it
      # (docs/tokens.md, "Hand-offs between apps").
      ExecStart = "${lib.getExe pkgs.losos-tokens} host --forward 24727";
      # A user of its own: the udev rule below lets it open FIDO keys, the
      # polkit rule lets it reach pcscd, and libvirt's read-only socket is
      # open to every account. A vsock port above 1023 needs nothing.
      User = "losos-tokens";
      Group = "losos-tokens";
      Restart = "always";
      RestartSec = 1;
      ProtectSystem = "strict";
      ProtectHome = true;
      PrivateTmp = true;
      DevicePolicy = "closed";
      DeviceAllow = [ "char-hidraw rw" ];
      RestrictAddressFamilies = [
        "AF_UNIX"
        "AF_VSOCK"
        # The forwarded localhost ports.
        "AF_INET"
      ];
      NoNewPrivileges = true;
      CapabilityBoundingSet = "";
    };
  };
  users.users.losos-tokens = {
    isSystemUser = true;
    group = "losos-tokens";
  };
  users.groups.losos-tokens = { };

  # FIDO keys' hidraw nodes, which systemd's 60-fido-id.rules marks: the
  # relay's group may read and write them. The person at the seat keeps the
  # access uaccess gives them, so the host's own sign-in still reaches the
  # key beside the relay.
  services.udev.extraRules = ''
    SUBSYSTEM=="hidraw", ENV{ID_FIDO_TOKEN}=="1", GROUP="losos-tokens", MODE="0660"
  '';

  # pcscd asks polkit about every client, and allows only the person at the
  # seat. The relay is nobody's session, and decides itself which machine
  # may have the readers (docs/tokens.md, "Smart cards").
  security.polkit.extraConfig = ''
    polkit.addRule(function (action, subject) {
      if ((action.id == "org.debian.pcsc-lite.access_pcsc" ||
           action.id == "org.debian.pcsc-lite.access_card") &&
          subject.user == "losos-tokens") {
        return polkit.Result.YES;
      }
    });
  '';
}
