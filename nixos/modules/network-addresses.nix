# The network machine's links and addresses, shared by the host's half
# and the machine's, both in network-machine.nix, so they cannot disagree. 10.139/16 is the range Qubes gives its own sys-net
# links; nothing on a home network is likely to use it.
{
  # The host's way out: the host at .2, the network machine its router at
  # .1, and the host's DNS server there too.
  up = {
    network = "10.139.0.0";
    prefix = 24;
    machine = "10.139.0.1";
    host = "10.139.0.2";
    bridge = "losos-up0";
    libvirt = "losos-up";
    mac = "52:54:00:4c:1e:01";
    link = "up0";
  };
  # The network machine's way out when the host had to keep the network
  # cards (no IOMMU to fence them): the host at .1 routes it to them.
  wan = {
    network = "10.139.1.0";
    prefix = 24;
    host = "10.139.1.1";
    machine = "10.139.1.2";
    bridge = "losos-wan0";
    libvirt = "losos-wan";
    mac = "52:54:00:4c:1e:02";
    link = "wan0";
    # The routing table the host's own network cards' routes go in, so the
    # host's connections take the network machine and only its are routed
    # to the cards.
    table = 100;
  };
  # Schizo mode's two networks (schizo-addresses.nix), on which the network
  # machine is the host's stand-in: Whonix's first Workstation's address on
  # the inside, and the gateway's router on its uplink.
  tor = {
    mac = "52:54:00:4c:1e:03";
    link = "tor0";
  };
  torext = {
    mac = "52:54:00:4c:1e:04";
    link = "torext0";
  };
  # The network shares machine (shares-machine.nix), on the host's link
  # with the network machine as its router, as the host is.
  shares = {
    mac = "52:54:00:4c:1e:05";
    address = "10.139.0.3";
  };
  # Its vsock address, and the calls it takes there (losos-vm-call).
  cid = 4100;
  ports = {
    nmcli = 5000;
    apply = 5001;
  };
}
