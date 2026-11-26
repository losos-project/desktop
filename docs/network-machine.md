# The network machine

On a PC, the network cards belong to sys-net, a small machine of their own, as
in [Qubes OS](https://www.qubes-os.org). The host reaches the internet only
through it. Card drivers, firmware, Wi-Fi handshakes and DHCP all parse what
strangers send, and on LosOS that code runs in sys-net, not on the host.

sys-net is BusyBox, one program called `losos-netd`, and the host's kernel. It
has nothing to update, no shell to log in to, and nowhere for a compromised
driver to hide: the whole machine is an initrd in the host's verified `/usr`,
and it starts from scratch every time. The Machines app hides it. The
**Network** and **Wi-Fi** pages in Settings, Danube's captive portal sign-in
page and `nmcli` in a terminal work as usual.

## What it does

- Cards. At boot, before NetworkManager starts, every Ethernet and Wi-Fi card
  that the IOMMU can isolate is unbound from the host and handed to sys-net
  through VFIO. `losos-netd` loads the driver and firmware from the host's
  module tree and firmware, shared read-only.
- Wi-Fi and DHCP. `losos-netd` runs `wpa_supplicant` and BusyBox's `udhcpc` for
  each card. It joins the strongest saved network in range, and after a wrong
  password it waits two minutes before trying that network again. Saved
  networks live on the machine's own 16 MB disk. On first start, sys-net
  imports the Wi-Fi networks the host had saved in NetworkManager.
- Routing. The host's link to sys-net is the bridge `losos-up0`, with the host
  at 10.139.0.2 and sys-net at 10.139.0.1 as its router. Everything from the
  host and its machines leaves behind sys-net's address.
- DNS and ad blocking. The [ad blocker](adblock.md)'s forwarder runs in sys-net
  and is the host's only DNS server. The host still fetches and compiles the
  filter lists, and sys-net reads them from a read-only share.
- mDNS. sys-net passes mDNS between the host and its networks, so `.local`
  names and [LosOS boxes](losos-boxes.md) still resolve. It passes nothing from
  one network to another.
- Schizo mode. The host plugs sys-net into the Tor gateway's two networks and
  tells it the mode. sys-net then sends everything from the host to the gateway
  and nothing else out. At boot it forwards nothing until it has applied the
  mode.

## Settings and nmcli

`nmcli` on the host is a wrapper. While sys-net holds the cards, it runs
`losos-netd nmcli` in the machine through `losos-vm-call`. That covers the
parts of nmcli that derisk and Danube use: general state, devices, Wi-Fi scans,
saved connections, connecting with a password, disconnecting, forgetting, and
the Wi-Fi radio. Anything else gets the error nmcli itself would give.

`losos-vm-call` is how the host runs anything in a service machine. A machine
takes calls only from the host's root, which it recognises because only root
can open a vsock connection from a port below 1024. The relay,
`losos-vm-call-relay.service`, is that root, and it checks who asks for each
call. For nmcli it allows root, the `networkmanager` and `wheel` groups, and
the user whose session is in front.

## Without an IOMMU

A PC without an IOMMU, or whose network card shares an IOMMU group with
something else, can't hand the card over safely. The card stays on the host,
and the layout stays the same: NetworkManager runs the card with its routes in
a separate table, and the host routes only sys-net's traffic there, over the
bridge `losos-wan0`. The host's and machines' connections still go through
sys-net, its ad blocker and its firewall. The VM tests run this way.

## Through an edge

A LosOS edge can be the PC's way out to the internet, like a VPN, so sites see
the edge's address instead of your network's. It is off until root, or a user
allowed by polkit, gives the PC an account on an edge:

```sh
losos-edge set <id> [<url>]   # the account's token on stdin
losos-edge                    # current state, as JSON
losos-edge off
```

The account is a tenant the edge's operator rents out
(`losos.edge.tenants.<id>.exit` on the edge; see "The edge as an exit" in the
LosOS handbook). The host keeps it in `/var/lib/losos-edge/account`, readable
only by root, and gives sys-net a copy through its share.

### Which edge

sys-net picks the edge with LosOS's rule, from the crate a LosOS box uses
(`losos-edge-path`): a gateway on the network first, found by DNS-SD
(`_losos-edge._tcp`), otherwise the URL you set. A gateway on your network is
never the exit itself. Only an edge on the internet can be, so through a
gateway the exit is one of the official edges the gateway lists, the one the
account's id weighs heaviest on. sys-net refuses an exit if any address its URL
or WireGuard endpoint resolves to is private or on one of the PC's networks.

### Outside schizo mode

sys-net enrols its WireGuard key at the exit and brings up a tunnel,
`losos-edge`. Traffic from the host and its machines then uses a routing table
whose only way out is the tunnel. The PC's own networks stay reachable, for
printers and LosOS boxes. While the tunnel is down, or the edge refuses the
account, that traffic goes nowhere, like a VPN kill switch, and `losos-edge`
says why. Once the tunnel is up, names go to Quad9 through it. Until then
sys-net asks the network's DNS servers, which is how it finds the edge. The key
never leaves sys-net's state disk.

### In schizo mode

The tunnel is down and Tor carries everything. sys-net enrols through Tor and
gives the host the edge's onion address and the SOCKS account from the
enrolment. The host writes them where the Tor gateway reads them
([Schizo mode](schizo-mode.md), "Through an edge"), and connections leave Tor
at the edge. That file exists from the moment an edge is set: a placeholder
until sys-net has enrolled, then the last account that worked. So connections
fail rather than leave through a Tor exit.

## Through a VPN

With a [VPN](vpn.md) set, sys-net joins sys-vpn's two networks and hands it
everything that would have left through the cards: the host's and machines'
connections, or in schizo mode the Tor gateway's. sys-vpn's uplink then leaves
the way the host's would have, through the edge if one is set. While sys-vpn is
missing, those connections go nowhere.

## Staying up

sys-net starts at boot and never idles, because the host has no other way out.
If it stops, the host restarts it within seconds. Freed memory goes back to the
host through the balloon device's free page reporting.

The `service-machines` VM test (`nix flake check`) stresses it with hundreds of
parallel web requests, downloads checked byte for byte, 2000 simultaneous DNS
queries, 200 nmcli calls, schizo mode toggled with no reachable gateway (which
must cut the host off), and the machine destroyed mid-download.

## The pieces

| Piece | Where |
| --- | --- |
| The machine, the host's links, nmcli, schizo mode | `nixos/modules/network-machine.nix` |
| Handing PCI devices to a machine | `nixos/modules/passthrough.nix`, `losos-passthrough.sh` |
| BusyBox machines and the host's modules in them | `nixos/modules/busybox-guests.nix`, `busybox-host-kernel.nix` |
| Booting and following service machines | `nixos/modules/service-machines.nix` |
| The machine's program | `src/losos-netd` |
| The edge it leaves through, the host's half | `nixos/modules/edge-exit.nix`, `src/losos-netd/src/edge.rs`, `edge_run.rs`, `dnssd.rs` |
| Routing through sys-vpn ([VPN](vpn.md)) | `src/losos-netd/src/vpn.rs`, `nixos/modules/vpn.nix` |
| Host-to-machine calls | `src/losos-vm-call`, `nixos/modules/vm-call.nix` |
