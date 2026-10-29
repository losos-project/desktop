# The network machine

On a PC, the network cards do not belong to the host. They belong to
sys-net, a small machine of their own, the way
[Qubes OS](https://www.qubes-os.org) gives them to its sys-net. The host
reaches the internet only through it. A network card's driver and firmware,
Wi-Fi's handshakes and the DHCP answers a network sends are code that
parses what strangers send, and here that code runs in sys-net, not in the
host.

sys-net is not a Linux distribution. It is BusyBox, one program called
`losos-netd`, and the host's own kernel. There is nothing in it to update,
no shell to log in to, and nothing a compromised driver could hide in: the
whole machine is an initrd in the host's verified `/usr`, and it starts
from scratch every time. It is hidden from the Machines app. Settings'
Network and Wi-Fi pages, Danube's sign-in page for captive portals, and
`nmcli` in a terminal all keep working as before.

## What it does

- **Cards.** At boot, before NetworkManager starts, every Ethernet and
  Wi-Fi card that the IOMMU can fence off is unbound from the host and
  handed to sys-net through VFIO. `losos-netd` loads the card's driver and
  firmware from the host's own module tree and firmware, which the host
  shares with the machine read-only.
- **Wi-Fi and DHCP.** `losos-netd` runs `wpa_supplicant` and BusyBox's
  `udhcpc` for each card. It joins the strongest saved network in reach by
  itself, and gives up on one whose password is wrong for two minutes
  instead of retrying in a loop. Saved networks are kept on a 16 MB disk of
  the machine's own. The first time sys-net starts, it takes in the Wi-Fi
  networks the host had saved in NetworkManager.
- **Routing.** The host's link to sys-net is the bridge `losos-up0`, with
  the host at 10.139.0.2 and sys-net at 10.139.0.1 as its router.
  Everything the host and its other machines send leaves through sys-net,
  behind sys-net's own address.
- **DNS and ad blocking.** The [ad blocker](adblock.md)'s forwarder runs in
  sys-net and is the host's only DNS server. The host still fetches and
  compiles the filter lists, and sys-net reads them from a read-only share.
- **mDNS.** sys-net passes mDNS between the host and the networks it is on,
  so `.local` names and [LosOS boxes](losos-boxes.md) are still found. It
  passes nothing from one network to another.
- **Schizo mode.** The host plugs sys-net into the Tor gateway's two
  networks and tells it the mode. sys-net then sends everything from the
  host to the gateway and lets nothing else out. Until it has applied the
  mode, which it does before forwarding anything at boot, nothing from the
  host is forwarded at all.

## Settings and nmcli

`nmcli` on the host is a small wrapper. While sys-net holds the cards, it
runs `losos-netd nmcli` in the machine through `losos-vm-call`, which
answers the part of nmcli's language that derisk and Danube use: the
general state, devices, Wi-Fi scans, saved connections, connecting with a
password, disconnecting, forgetting, and the Wi-Fi radio. Anything else
gets the error nmcli itself would give.

`losos-vm-call` is how the host runs anything in a service machine. A
machine only takes calls from the host's root, which it can tell from the
connection: only root can open a vsock connection from a port below 1024.
The relay, `losos-vm-call-relay.service`, is that root. It checks who is
asking for each call. For nmcli, that is root, members of the
`networkmanager` and `wheel` groups, and the person whose session is in
front.

## Without an IOMMU

A PC with no IOMMU, or one whose network card shares an IOMMU group with
something else, cannot hand the card over safely. The card then stays on
the host, and the layout stays the same. NetworkManager on the host runs
the card with its routes in a table of their own, and the host routes only
sys-net's traffic there, over the bridge `losos-wan0`. The host's own
connections and its machines' still go through sys-net, its ad blocker and
its firewall. The VM tests run this way.

## Through an edge

A LosOS edge can be the PC's way out to the internet, the way a VPN is.
Sites then see the edge's address, not the network the PC is on. It is
off until someone gives the PC an account on an edge, as root or through
polkit:

```sh
losos-edge set <id> [<url>]   # the account's token on stdin
losos-edge                    # how things stand, as JSON
losos-edge off
```

The account is a tenant the edge's operator lets out
(`losos.edge.tenants.<id>.exit` on the edge, LosOS's handbook, "The edge
as an exit"). The host keeps it in `/var/lib/losos-edge/account`, root's
alone, and hands sys-net a copy through its share.

**Which edge.** sys-net picks the edge with LosOS's own rule, the crate a
LosOS box goes by (`losos-edge-path`): a gateway on the network first,
found by DNS-SD (`_losos-edge._tcp`), else the URL the person set. A
gateway on the network is never the exit, though. Only an edge out on the
internet may be one, so through a gateway the exit is one of the official
edges the gateway lists, the one the account's id weighs heaviest on.
Every address the exit's URL and its WireGuard endpoint resolve to must be
public and on none of the PC's own networks, or sys-net refuses it.

**Outside schizo mode** sys-net enrols its WireGuard key at the exit and
brings up `losos-edge`, a tunnel to it. The host's and its machines'
traffic then takes a routing table whose only way out is the tunnel. The
PC's own networks stay in reach, for printers and LosOS boxes. While the
tunnel is down, or the edge refuses the account, that traffic goes
nowhere, the way a VPN's kill switch works, and `losos-edge` says why.
Names go to Quad9 through the tunnel once it is up; until then sys-net
asks the network's own servers, which is how it finds the edge at all.
The key never leaves sys-net's state disk.

**In schizo mode** the tunnel is down, and Tor carries everything. sys-net
enrols through Tor and hands the host the edge's onion address and the
SOCKS account the enrolment came with. The host writes them where the Tor
gateway reads them ([Schizo mode](schizo-mode.md), "Through an edge"),
and the edge is then where connections leave Tor. From the moment an edge
is set, the file is there: a placeholder until sys-net has enrolled, then
the last account that worked. So connections fail rather than leave
through a Tor exit.

## Staying up

sys-net starts at boot and is never idle: the host has no other way out.
If it stops for any reason, the host starts it again within seconds.
Memory it frees goes back to the host as it frees it, through the balloon
device's free page reporting.

The VM test `service-machines` (`nix flake check`) stresses it: hundreds of
parallel web requests, downloads checked byte for byte, two thousand DNS
queries at once, two hundred nmcli calls, schizo mode switched on and off
with no gateway to reach (which must cut the host off), and the machine
destroyed in the middle of a download.

## The pieces

| Piece | Where |
| --- | --- |
| The machine, the host's links, nmcli, schizo mode | `nixos/modules/network-machine.nix` |
| Handing PCI devices to a machine | `nixos/modules/passthrough.nix`, `losos-passthrough.sh` |
| BusyBox machines and the host's modules in them | `nixos/modules/busybox-guests.nix`, `busybox-host-kernel.nix` |
| Booting and following service machines | `nixos/modules/service-machines.nix` |
| The machine's program | `src/losos-netd` |
| The edge it leaves through, the host's half | `nixos/modules/edge-exit.nix`, `src/losos-netd/src/edge.rs`, `edge_run.rs`, `dnssd.rs` |
| Host-to-machine calls | `src/losos-vm-call`, `nixos/modules/vm-call.nix` |
