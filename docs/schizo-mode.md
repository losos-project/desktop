# Schizo mode

Schizo mode sends every connection the PC makes through Tor, and every
connection its machines make too. It works the way Whonix does: a small
gateway machine runs Tor, and everything else can only reach the internet
through that gateway. If the gateway is down, nothing goes out at all.

It is off until someone turns it on. Turn it on or off at the top of
Settings' Network page, or from a terminal:

```sh
losos-schizo on       # or off; polkit asks for an administrator's password
losos-schizo          # prints on or off
losos-schizo status   # mode=, gateway= and tor= lines
```

The choice is the file `/var/lib/losos/schizo`, so it stays as it was left
across restarts and updates. The host is in `nixos/modules/schizo.nix` and
the gateway in `nixos/guest/tor-gateway.nix`.

> [!WARNING]
> Schizo mode hides where your connections come from. It does not make a
> browser or an app anonymous by itself: a site you sign in to still knows
> who you are, and an ordinary browser can still be fingerprinted. Use Tor
> Browser in a machine for browsing you need to keep apart from you.

## What changes when it is on

- **A kill switch, before the network.** An nftables table drops everything
  the host would send anywhere except to the gateway, to its own machines
  and to the network's DHCP server. It loads before NetworkManager brings
  up a single link, so nothing leaks while the PC starts. A program that
  binds to a network card still reaches nothing, and so does a route
  someone adds by hand.
- **Routing to the gateway.** The host's own connections, and those of
  every machine on libvirt's networks, are routed to the gateway's inside
  address, `10.152.152.10`.
- **Names through Tor.** Every DNS query goes to the gateway, where Tor
  resolves it. With the [ad blocker](adblock.md) on, its forwarder still
  answers blocked names itself and sends the rest to the gateway.
- **No identity on the network.** NetworkManager gives every network card a
  random MAC address and sends no host name to the DHCP server. It
  reconnects each link when the mode changes, so the new address takes
  effect at once.
- **The gateway starts.** Its own uplink is the only traffic the host still
  forwards to the internet.

UDP other than DNS and all of IPv6 cannot go through Tor, so they are
dropped. The local network is out of reach too: printers, shares, other
computers and LosOS boxes found over mDNS. Tor's exits could not reach
them anyway. The clock is not set over NTP while the mode is on.

## The gateway

The gateway is a NixOS machine whose only job is Tor. It copies the design
of Whonix's Gateway, including Whonix's internal address for it.

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

Every port isolates streams the way Whonix's do. Different clients,
destinations and SOCKS usernames get circuits of their own. A `.onion` name
looked up through the DNSPort gets an address that the TransPort takes as
that onion, so onion services work for any program, not only a browser.
The gateway forwards nothing, has no account anyone can sign in to and no
resolver of its own.

The gateway's state is a 256 MiB disk under
`/var/lib/losos-service-machines/losos-tor-gateway`. It keeps Tor's guards,
which Tor picks once and keeps for months, so a person is not handed a new,
maybe hostile, entry relay on every boot.

## Service machines

The gateway is a service machine (`nixos/modules/service-machines.nix`): a
machine the OS runs part of itself in, rather than one a person makes. A
service machine has no disk image. The host boots it from its own store:
the kernel and initrd by path, and the store shared read-only over 9p, the
way NixOS's test VMs boot. It uses the image's own kernel, so it costs
`/usr` only what it does not share with the host. It is covered by the
verity hash like the rest of the OS, and an update brings a new one the
next time it starts. Its root is a tmpfs, and anything it keeps is on its
state disk.

Each one is a libvirt domain, defined again from the image every time its
unit, `losos-service-machine-<name>.service`, starts. It runs on KVM when
the host has it and emulated otherwise. Its pm metadata marks it as a
service, so the Machines app, the palette, the overview and the top bar do
not show it. `virsh` and virt-manager still do, and `virsh console
losos-tor-gateway` shows the gateway's journal, including Tor's bootstrap.
What it printed last is kept in
`/var/log/libvirt/qemu/losos-tor-gateway-console.log`, so a gateway that
died can still be read.

Every service machine that runs NixOS is hardened the way Kicksecure, the
Debian derivative Whonix is built on, hardens itself
(`nixos/guest/kicksecure.nix`). It takes Kicksecure's settings from its
security-misc package but not its Debian, so the machine stays part of the
verified image:

- Kicksecure's kernel command line: no merged slab caches, memory zeroed
  when it is allocated and when it is freed, a random kernel stack offset,
  no vsyscalls or 32-bit system calls, and every CPU mitigation with SMT
  off.
- Its sysctls. Kernel pointers and logs are hidden. Unprivileged BPF,
  io_uring, kexec, SysRq and ptrace are off. There are no core dumps. Pings,
  redirects, source routes and router adverts get no answer.
- Its disabled kernel modules: rare network protocols and file systems,
  network file systems, FireWire, Thunderbolt and GPS. A machine that needs
  one back lists it in `losos.service.kicksecure.allowModules`.
- No SUID programs but `unix_chkpwd`, which PAM needs to start a person's
  session, no sudo, and root is locked. A machine that needs another SUID
  program lists it in `losos.service.kicksecure.wrappers`.

The gateway is therefore what Whonix's Gateway is: Kicksecure with Tor.
The settings are kept as data in `nixos/guest/kicksecure-settings.nix`, so
a BusyBox service machine can boot with the same command line and set the
same sysctls.

Service machines are lazy. Nothing starts one until it is needed: the
gateway starts when schizo mode is turned on and stops when it is turned
off, so a PC that never uses the mode never runs it. Its state disk is
made the first time it starts. While it runs, its balloon reports the
pages it frees, and the host takes them back, so an idle gateway holds
little more memory than Tor uses.

## How it fails

Schizo mode is built to fail closed. The kill switch is what guarantees it,
and the routing is only what makes things work:

- If the gateway is stopped, crashed or still starting, the route to it
  leads nowhere and every other way out is dropped.
- If Tor has not built a circuit yet, the gateway accepts connections and
  closes them.
- If libvirt's networks are gone, the route is gone too, and the kill
  switch still drops everything else.

Settings says which of these applies. Turning the mode off removes the
kill switch, the routes and the DNS change, stops the gateway, and
reconnects each link with its own MAC address.

## The network machine

When the network machine holds the PC's network cards, it routes for the
host and every machine. In that case it is also the gateway's router on
`losos-torext` and the host's way to it on `losos-tor`, and it does the
routing, the DNS and the MAC addresses itself while schizo mode is on. The
host's two bridges then carry no address of the host's own. Its kill switch
lets out only what goes to the network machine. `schizo.nix` reads
`losos.machines.network.enable` to tell which case applies.

## Testing

`nix build .#checks.x86_64-linux.schizo` runs `nixos/tests/schizo.nix`. The
test's internet runs a Tor network of its own: a directory authority and
two relays on Tor's testing settings, the way chutney builds one. The
gateway is the image's, told of that authority with an `%include`. The test
checks these things:

- With the mode off, the web server sees the host.
- With it on, the kill switch drops traffic straight out of the network
  card, and the card has a random MAC address.
- The web server sees a Tor exit for the host's connections, for names it
  looks up, and for a machine on libvirt's network.
- With the gateway stopped, nothing gets out.
- The mode survives a restart, with the kill switch up before
  NetworkManager starts.
- Turned off, everything is back as it was.
