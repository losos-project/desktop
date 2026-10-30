# sys-vpn's two networks (docs/vpn.md), shared by the host's half
# (vpn.nix), the network machine's (network-machine.nix) and the machine's
# own (nixos/guest/vpn.nix), so they cannot disagree. Laid out as schizo
# mode's are (schizo-addresses.nix): an inside, where the network machine
# hands sys-vpn every connection, and an uplink, sys-vpn's only way out,
# with the network machine as its router.
{
  inside = {
    network = "10.154.0.0";
    prefix = 24;
    vpn = "10.154.0.10";
    net = "10.154.0.11";
    bridge = "losos-vpn";
  };
  uplink = {
    network = "10.154.1.0";
    prefix = 24;
    net = "10.154.1.1";
    vpn = "10.154.1.2";
    bridge = "losos-vpnext";
  };
  machine = {
    uplinkMac = "52:54:00:4c:2f:01";
    insideMac = "52:54:00:4c:2f:02";
  };
  # The host's copy of the tunnel file, read-only in sys-vpn over 9p.
  share = {
    hostDir = "/run/losos-vpn";
    tag = "losos-vpn";
  };
}
