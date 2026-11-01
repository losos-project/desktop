# The addresses schizo mode's two networks use, shared by the host's half
# (schizo.nix) and the gateway's (nixos/guest/tor-gateway.nix), so the two
# cannot disagree.
{
  # Whonix's internal network: the gateway at .10, as in Whonix, and the
  # host where Whonix puts its first Workstation, .11.
  inside = {
    network = "10.152.152.0";
    prefix = 18;
    gateway = "10.152.152.10";
    host = "10.152.152.11";
    bridge = "losos-tor";
  };
  # The gateway's way out, through the host's NAT. Not Whonix's
  # 10.0.2.0/24, which is also QEMU's user networking: a LosOS that is
  # itself a VM on it would have its own uplink there.
  uplink = {
    network = "10.152.192.0";
    prefix = 24;
    host = "10.152.192.1";
    gateway = "10.152.192.2";
    bridge = "losos-torext";
  };
  gateway = {
    uplinkMac = "52:54:00:4c:0f:01";
    insideMac = "52:54:00:4c:0f:02";
  };
  # Tor's ports on the inside address. 53 is redirected to `dns` there.
  ports = {
    trans = 9040;
    dns = 5300;
    socks = 9050;
    # losos-edge-chain, which takes the inside's TCP instead of the
    # TransPort while an edge is set. It dials Tor's SOCKS on the
    # gateway's loopback, at the same port as `socks`.
    edge = 9041;
  };
  # The edge schizo mode leaves Tor through, when one is set: what the host
  # writes, and where the gateway reads it over a read-only 9p share.
  edge = {
    hostDir = "/run/losos-schizo/edge";
    share = "losos-edge";
    gatewayDir = "/run/losos-edge";
    file = "edge.json";
  };
}
