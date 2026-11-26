# Schizo mode

Schizo mode sends every connection the PC and its machines make through Tor.
It works like Whonix: a small gateway machine runs Tor, everything else can
reach the internet only through it, and if the gateway is down nothing goes out.

It is off until you turn it on, at the top of the **Network** page in Settings
or from a terminal:

```sh
losos-schizo on       # or off; polkit asks for an administrator's password
losos-schizo          # prints on or off
losos-schizo status   # mode=, gateway= and tor= lines
```

The setting is the file `/var/lib/losos/schizo`, so it survives restarts and
updates. The host side is `nixos/modules/schizo.nix` and the gateway is
`nixos/guest/tor-gateway.nix`.

> [!WARNING]
> Schizo mode hides where your connections come from. It doesn't make a browser
> or app anonymous: a site you sign in to still knows who you are, and an
> ordinary browser can still be fingerprinted. Use Tor Browser in a machine for
> browsing you need to keep apart from yourself.

## What changes when it is on

- A kill switch loads before the network. An nftables table drops everything
  the host sends except to the gateway, its own machines and the network's
  DHCP server. It loads before NetworkManager brings up any link, so nothing
  leaks during boot. Programs that bind to a network card, and routes added by
  hand, still reach nothing.
- The host's connections, and those of every machine on libvirt's networks,
  are routed to the gateway's inside address, `10.152.152.10`.
- Every DNS query goes to the gateway, where Tor resolves it. With the
  [ad blocker](adblock.md) on, its forwarder still answers blocked names itself
  and sends the rest to the gateway, never to its encrypted DNS resolvers,
  whatever that setting says.
- NetworkManager gives every card a random MAC address and sends no host name
  to DHCP. It reconnects each link when the mode changes, so the new address
  applies at once.
- The gateway starts. Its uplink is the only traffic the host still forwards to
  the internet.

UDP other than DNS, and all IPv6, can't go through Tor, so they are dropped.
The local network is out of reach too: printers, shares, other computers and
LosOS boxes found over mDNS. The clock isn't set over NTP while the mode is on.

## The gateway

The gateway is a NixOS machine whose only job is Tor. It copies Whonix's
Gateway design, including its internal address.

| Card | Network | Address | What it does |
| --- | --- | --- | --- |
| uplink | `losos-torext` | `10.152.192.2/24`, router `.1` | Reaches the internet through the host's NAT. Only Tor's own user may send anything out of it. |
| inside | `losos-tor` | `10.152.152.10/18` | Receives everything the host routes to it. The host is `.11`, where Whonix puts its first Workstation. |

On the inside card, Tor takes everything:

| Traffic | Where it goes |
| --- | --- |
| TCP to any address | Tor's TransPort, 9040, which opens the same connection through Tor |
| DNS, UDP port 53 | Tor's DNSPort, 5300 |
| SOCKS on `10.152.152.10:9050` | Tor's SocksPort, for programs that ask for Tor themselves |
| Anything else | Dropped |

Every port isolates streams as Whonix's do: different clients, destinations and
SOCKS usernames get separate circuits. A `.onion` name looked up through the
DNSPort gets an address the TransPort maps back to that onion, so onion
services work for any program, not only a browser. The gateway forwards
nothing, has no login account and no resolver of its own.

Its state is a 256 MiB disk under
`/var/lib/losos-service-machines/losos-tor-gateway`. It keeps Tor's guards,
which Tor picks once and keeps for months, so you aren't handed a new, possibly
hostile, entry relay on every boot.

## Through an edge

When the PC has a LosOS edge, schizo mode sends connections out from that edge
instead of a Tor exit, so sites see the edge's address. The edge is chosen as
everywhere else in LosOS and reached at its onion address, so your network
still sees only Tor and the edge never sees your address.

```
apps → gateway → Tor → the edge's onion service → edge → the internet
```

The edge's onion address and credentials come with the PC's enrolment, so the
PC never asks the edge for them in the clear. While an edge is set, the host
keeps them in `/run/losos-schizo/edge/edge.json`, readable only by root. The
gateway sees that directory read-only at `/run/losos-edge` and checks every two
seconds whether the file exists:

- If it does, every TCP connection from the inside goes to `losos-edge-chain`
  on `10.152.152.10:9041` instead of the TransPort. The chain dials Tor's SOCKS
  port, reaches the edge's onion, and asks the edge to open the connection the
  program wanted.
- If it doesn't, TCP goes to the TransPort as before.

The chain reads the file for every connection. Connections to onions always
stay on the TransPort. The chain gives Tor a SOCKS username per client and
destination port, so programs and services don't share circuits.

It fails closed like the rest of the mode. Once an edge has been set, or found
on the local network while the mode was on, the file stays even when the edge
is unreachable, so connections fail instead of leaving through a Tor exit. Only
a PC that never had an edge goes straight out through Tor.
`losos-schizo status` prints `edge=set` or `edge=none`. Names are still
resolved by Tor's exits through the DNSPort, not by the edge.

## Service machines

The gateway is a service machine (`nixos/modules/service-machines.nix`): a
machine the OS runs part of itself in, not one you create. It has no disk
image. The host boots it from its own store, the kernel and initrd by path and
the store shared read-only over 9p, as NixOS's test VMs boot. It uses the
image's kernel, so it costs `/usr` only what it doesn't share with the host, is
covered by the verity hash, and gets updated the next time it starts. Its root
is a tmpfs, and anything it keeps lives on its state disk.

Each one is a libvirt domain, redefined from the image every time its unit,
`losos-service-machine-<name>.service`, starts. It runs on KVM when available
and emulated otherwise. Its pm metadata marks it as a service, so the Machines
app, palette, overview and top bar hide it, though `virsh` and virt-manager
show it. `virsh console losos-tor-gateway` shows the gateway's journal,
including Tor's bootstrap, and the last output is kept in
`/var/log/libvirt/qemu/losos-tor-gateway-console.log` for a gateway that died.

Every NixOS service machine gets Kicksecure's hardening
(`nixos/guest/kicksecure.nix`). Kicksecure is the Debian derivative Whonix is
built on. The machine takes the settings from its security-misc package, not
its Debian, so it stays part of the verified image:

- Kicksecure's kernel command line: no merged slab caches, memory zeroed on
  allocation and free, a random kernel stack offset, no vsyscalls or 32-bit
  system calls, and every CPU mitigation with SMT off.
- Its sysctls. Kernel pointers and logs are hidden. Unprivileged BPF, io_uring,
  kexec, SysRq and ptrace are off. There are no core dumps. Pings, redirects,
  source routes and router adverts get no answer.
- Its disabled kernel modules: rare network protocols and file systems, network
  file systems, FireWire, Thunderbolt and GPS. A machine that needs one lists it
  in `losos.service.kicksecure.allowModules`.
- No SUID programs except `unix_chkpwd`, which PAM needs to start a session, no
  sudo, and a locked root. A machine that needs another SUID program lists it in
  `losos.service.kicksecure.wrappers`.

So the gateway is what Whonix's Gateway is: Kicksecure with Tor. The settings
are data in `nixos/guest/kicksecure-settings.nix`, so a BusyBox service machine
can boot with the same command line and sysctls.

Service machines are lazy. The gateway starts when schizo mode turns on and
stops when it turns off, so a PC that never uses the mode never runs it. Its
state disk is created on first start. Its balloon reports freed pages and the
host takes them back, so an idle gateway holds little more memory than Tor
uses.

## How it fails

The kill switch guarantees that schizo mode fails closed. The routing only
makes things work:

- If the gateway is stopped, crashed or starting, the route leads nowhere and
  every other way out is dropped.
- If Tor hasn't built a circuit yet, the gateway accepts connections and closes
  them.
- If libvirt's networks are gone, so is the route, and the kill switch still
  drops everything else.

Settings shows which applies. Turning the mode off removes the kill switch,
routes and DNS change, stops the gateway, and reconnects each link with its
real MAC address.

## The network machine

When the [network machine](network-machine.md) holds the network cards, it
routes for the host and every machine. It is then also the gateway's router on
`losos-torext` and the host's way to it on `losos-tor`, and it handles routing,
DNS and MAC addresses itself while schizo mode is on. The host's two bridges
then carry no host address, and its kill switch lets out only traffic to the
network machine. `schizo.nix` reads `losos.machines.network.enable` to tell
which case applies.

## Testing

`nix build .#checks.x86_64-linux.schizo` runs `nixos/tests/schizo.nix`. The
test's internet runs its own Tor network: a directory authority and two relays
on Tor's testing settings, as chutney builds one. The gateway is the image's,
pointed at that authority with an `%include`. The test checks that:

- With the mode off, the web server sees the host.
- With it on, the kill switch drops traffic sent straight out of the card, and
  the card has a random MAC address.
- The web server sees a Tor exit for the host's connections, its DNS lookups,
  and a machine on libvirt's network.
- With the gateway stopped, nothing gets out.
- The mode survives a restart, with the kill switch up before NetworkManager.
- Turned off, everything is back as it was.
