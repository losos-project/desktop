# VPN

A VPN sends every connection the PC makes, and every connection its
machines make, through one VPN provider's WireGuard tunnel. The tunnel runs
in sys-vpn, a small machine of its own. The PC's own networks, its printers
and LosOS boxes stay in reach. While the tunnel is down, everything else
goes nowhere.

It is off until someone sets a provider, as root or through polkit:

```sh
losos-vpn providers               # the providers it takes, with their owners
losos-vpn set mullvad < wg.conf   # the provider's WireGuard file on stdin
losos-vpn                         # vpn=, endpoint= and machine= lines
losos-vpn off
```

The host keeps the choice in `/var/lib/losos-vpn`, root's alone, so it
stays as it was left across restarts and updates. The host's half is
`nixos/modules/vpn.nix`, the machine `nixos/guest/vpn.nix`, and the program
on both `src/losos-vpn`.

> [!WARNING]
> A VPN moves trust from your network to the provider. The provider sees
> every connection's destination and can link it to your account. It
> hides your address from sites, not who you are from the provider.

## How it works

sys-vpn sits between the [network machine](network-machine.md) and the
internet:

- **sys-net hands it everything.** While a VPN is set, the network machine
  routes the host's and the other machines' connections to sys-vpn's
  inside address, `10.154.0.10`, instead of out through its cards. Its own
  networks stay reachable directly. The ad blocker's forwarder asks the
  provider's DNS servers, from the file's `DNS` line, through the tunnel;
  a file without one gets Quad9's, still through the tunnel.
- **sys-vpn sends it into the tunnel.** Its inside reaches the tunnel and
  nothing else. Its uplink, with sys-net as its router, carries only the
  tunnel's own packets and one DNS lookup, the provider's endpoint.
- **It fails closed.** While sys-vpn's link is not there, sys-net's table
  for it leads nowhere. While the tunnel is down, sys-vpn's does too. If
  the last handshake is more than three minutes old, sys-vpn looks the
  endpoint up again and makes a new tunnel.
- **It is disposable.** sys-vpn has no disk. Every start is a new machine
  from the image, with the tunnel file the host hands it read-only, and
  setting a provider starts a new one. Like the Tor gateway, it is
  Kicksecure-hardened NixOS ([Schizo mode](schizo-mode.md), "Service machines"),
  and it is hidden from the Machines app.

**With an edge.** If a [LosOS edge](network-machine.md) is set as well,
sys-vpn's uplink leaves through the edge's tunnel, so the VPN rides inside
the edge's.

**In schizo mode** it is the Tor gateway's uplink that sys-net hands to
sys-vpn, so the network sees a VPN and the VPN sees only Tor.

## The WireGuard file

Every provider below gives out WireGuard files in the form `wg-quick`
reads. `losos-vpn set` checks the file on the host before it keeps it, and
sys-vpn checks it again before the tunnel goes up. It takes:

- one `[Interface]` with a `PrivateKey` and at least one IPv4 `Address`;
- one `[Peer]` with a `PublicKey`, an `Endpoint` and `AllowedIPs` that
  include `0.0.0.0/0`, so everything goes through it.

It refuses `PreUp`, `PostUp`, `PreDown`, `PostDown`, `SaveConfig`,
`Table` and `FwMark`. The first four are commands `wg-quick` runs as root,
and a downloaded file is not a script to run. The others would change how
the routes are made. IPv6 addresses and servers are left out, because sys-net
routes IPv4 only.

## Which providers

`losos-vpn` takes the providers in its `Provider` enum,
`src/losos-vpn/src/provider.rs`. Each is popular, gives out WireGuard
files, and is not owned by an advertising company. Each needs a paid
account to make a WireGuard file.

| Provider | Run by | Owned by | WireGuard file |
| --- | --- | --- | --- |
| Mullvad VPN | Mullvad VPN AB, Sweden | Amagicom AB, which its founders Fredrik Strömberg and Daniel Berntsson own outright ([About](https://mullvad.net/en/about), [2020 restructuring](https://mullvad.net/en/blog/2020/2/3/doing-corporate-shuffle)) | [mullvad.net](https://mullvad.net/en/account/wireguard-config) |
| Proton VPN | Proton AG, Switzerland | The non-profit Proton Foundation as its largest shareholder, endowed by the founders in 2024 ([Proton](https://proton.me/blog/proton-non-profit-foundation)) | [account.protonvpn.com](https://account.protonvpn.com/downloads) |
| IVPN | IVPN Limited, formerly Privatus Limited, Gibraltar | Reported to be its founder Nicholas Pestell; the company says the 2023 rename changed no owners ([IVPN](https://www.ivpn.net/blog/change-company-name-privatus-ivpn-limited/), [Privacy Guides forum](https://discuss.privacyguides.net/t/ivpn-dual-jurisdiction/36230)) | [ivpn.net](https://www.ivpn.net/account/wireguard-config) |
| Windscribe | Windscribe Limited, Canada | Its founders Yegor Sak, Alex Paguis and Mark Ulicki and its employees, with no outside investors ([Windscribe](https://windscribe.com/knowledge-base/articles/who-owns-windscribe)) | [windscribe.com](https://windscribe.com/getconfig/wireguard) |
| AirVPN | Air di Paolo Brini, Italy | Paolo Brini, for the activists who started it in 2010 ([Top10VPN](https://www.top10vpn.com/reviews/airvpn/), [TorrentFreak](https://torrentfreak.com/?p=246572)) | [airvpn.org](https://airvpn.org/generator/) |

Most of the ownership above is the company's own word. No public register
was checked: Sweden's, Switzerland's and Gibraltar's need an account or a
fee. The IVPN owner comes from a forum post citing the company, and AirVPN
names no team at all, only Paolo Brini as its contact.

**Left out: Kape Technologies.** ExpressVPN, Private Internet Access,
CyberGhost and ZenMate belong to Kape. Kape was Crossrider, an ad-injection
platform that Malwarebytes flagged as adware. It renamed itself in 2018 and
bought those four between 2017 and 2021 ([The Register](https://www.theregister.com/2021/09/14/expressvpn_bought_kape/)).
It also owns VPN review sites that rank its own services
([CyberInsider](https://cyberinsider.com/kape-technologies-owns-expressvpn-cyberghost-pia-zenmate-vpn-review-sites/)).

## The pieces

| Piece | Where |
| --- | --- |
| The command, the provider list, the file check, the tunnel | `src/losos-vpn` |
| The host's half: the networks, the units, sys-net's link | `nixos/modules/vpn.nix`, `vpn-addresses.nix` |
| The machine | `nixos/guest/vpn.nix` |
| sys-net's routing to it | `src/losos-netd/src/vpn.rs`, `daemon.rs` |
