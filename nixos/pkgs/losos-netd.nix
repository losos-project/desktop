# The network machine's one program (docs/network-machine.md): its PID 1,
# the daemon that runs the cards, Wi-Fi, DHCP, the routing and schizo
# mode, and the nmcli the host's Settings talk to.
#
# Built from src/losos-netd. Cargo.lock pins every crate with its SHA-256,
# and importCargoLock turns each pin into a fixed-output fetch.
{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "losos-netd";
  version = "0.1.0";

  src = ../../src/losos-netd;

  cargoLock = {
    lockFile = ../../src/losos-netd/Cargo.lock;
    # LosOS's edge path rule (src/losos-netd/src/edge.rs), from git.
    outputHashes."losos-edge-path-0.1.0" = "sha256-cDHVnD8JdAODLlzhR/4l3OEFtY6VawyEH52spAhrhS8=";
  };

  # nmcli's language, the saved networks and wpa_supplicant's answers.
  doCheck = true;

  meta = {
    description = "The network machine's init, network daemon and nmcli";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "losos-netd";
  };
}
