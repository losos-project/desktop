# The eID machine (docs/tokens.md, "Signing in with the eID"): where the
# Ministry of the Interior's closed PKCS#11 module reads the eID card for
# the host's login screen, so no closed code runs on the host. The host
# reaches it over vsock (eid-pkcs11.nix) and nothing else does: it has no
# network card.
#
# A service machine (service.nix): booted from the host's store and part
# of the image. The module is not: the host downloads the eID klient when
# a person turns eID sign-in on, and shares its copy here read-only, as
# `losos-eid` (modules/eid-signin.nix). The card is read through the
# host's pcscd, as in any machine, which keeps the readers on the host.
{
  lib,
  pkgs,
  ...
}:

{
  imports = [ ./eid-pkcs11.nix ];

  # The host's copy of the eID klient, unpacked (losos-eid-fetch).
  fileSystems."/run/losos-eid" = {
    device = "losos-eid";
    fsType = "9p";
    options = [
      "ro"
      "trans=virtio"
      "version=9p2000.L"
      "msize=1048576"
      "cache=loose"
    ];
  };
  boot.initrd.availableKernelModules = [ "vmw_vsock_virtio_transport" ];

  # The host's readers at /run/pcscd/pcscd.comm, and no copy of its
  # security keys, which nothing here signs in with.
  systemd.services.losos-tokens = {
    description = "Use the host's smart card readers";
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-tokens} guest --cards-only";
      Restart = "always";
      RestartSec = 1;
      ProtectSystem = "strict";
      RuntimeDirectory = "pcscd";
      RuntimeDirectoryPreserve = true;
      PrivateTmp = true;
      PrivateDevices = true;
      RestrictAddressFamilies = [
        "AF_UNIX"
        "AF_VSOCK"
      ];
      NoNewPrivileges = true;
      CapabilityBoundingSet = "";
    };
  };
}
