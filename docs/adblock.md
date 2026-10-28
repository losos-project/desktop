# System-wide ad blocking

`losos-adblock` blocks ads and trackers for every app on the OS, from the same
EasyList-style filter lists. Its code is in `src/losos-adblock` and its module
in `nixos/modules/adblock.nix`. It works in two halves.

**DNS, for every app.** resolved sends every name to losos-adblock's
forwarder on `169.254.153.53:53`. NetworkManager gives resolved a dummy
link, `adblock0`, whose one DNS server is the forwarder. That link handles
every domain and comes ahead of each network's own servers, so those servers
no longer get queries directly. For a blocked domain or any of its
subdomains, the forwarder answers `0.0.0.0` for A, `::` for AAAA and no
records for other types, so a program fails at once instead of retrying. It
sends every other query on [encrypted](#encrypted-dns), to a DNS over HTTPS
or DNS over TLS server. With encrypted DNS off, it passes them on byte for
byte to the servers the network gave instead. It asks NetworkManager for
those with `nmcli`, and asks again whenever none of them answers.

This half covers Flatpaks, the Android layer and anything else that resolves
names, but it can only block whole domains. A local network's names still
resolve through its server. A bare name such as `printer` no longer gets the
network's search domain appended, though. resolved's own DNS over TLS is off,
because resolved talks to the forwarder, which encrypts on the way out
instead.

**Content blockers, in the browser.** Some rules need a URL or a page, such
as a path like `/banner/ad_` or an element to hide like
`example.org##.sponsored`. Brave's `adblock` crate compiles those into WebKit
content blocker rule sets, and [Danube](danube.md) loads them into every
page. Rules the DNS half already applies are left out, so WebKit does not
compile them twice.

![A daily timer compiles the filter lists into a domain list and WebKit rule sets; every app's lookups go through systemd-resolved to the forwarder, which answers 0.0.0.0 or :: for a blocked name and passes the rest to the network's DNS; Danube loads the rule sets for paths and page elements](images/adblock.svg)

## Encrypted DNS

Plain DNS shows every name the machine looks up to whoever carries its
traffic, such as the Wi-Fi's owner or the ISP, and lets them answer in the
server's place. So the forwarder asks its upstream servers over an encrypted
connection, which those carriers see only as a connection to a resolver.
It speaks both protocols:

- **DNS over HTTPS** (RFC 8484), one POST per query on port 443. To the
  network it looks like any web page.
- **DNS over TLS** (RFC 7858), DNS's own framing inside TLS on port 853.
  Being on a port of its own, it is easy for a network to block or single
  out.

`losos.adblock.encryptedDns.servers` lists the servers, tried in order. By
default these are Quad9 and Mullvad over HTTPS, then the same two over
TLS. Both operators are non-profit or keep no logs, and neither filters
anything, since the blocking is the machine's own. A server is written as
its URL, `https://host/path` or `tls://host`, followed by the addresses to
reach it at:

```
https://dns.quad9.net/dns-query 9.9.9.9 149.112.112.112 2620:fe::fe 2620:fe::9
tls://dns.mullvad.net 194.242.2.2 2a07:e340::2
```

The forwarder never looks those addresses up. That lookup would be one
plain query, and its answer would decide which server is trusted. The
certificate is still checked against the host in the URL, from the
system's CA store. A server that failed is tried after the others for a
minute, so a network that blocks port 853 costs one timeout, not one per
query. Connections stay open between queries.

Some names still go to the network's own server in plain DNS: names under
`home.arpa`, `lan`, `home`, `internal`, `localdomain` and `intranet`, and the
reverse lookups of private addresses. A resolver on the internet has no
answer for these. The network sees them anyway once the connection is made.

Encrypted DNS has three modes:

| Mode | When no encrypted server answers |
| --- | --- |
| `on`, the default | The query fails. The exception is a captive portal: names go to the network's server until the portal lets encrypted DNS through |
| `strict` | The query fails, captive portal or not |
| `off` | Encrypted DNS is not used at all, and every query goes to the network's server |

The forwarder never falls back to plain DNS just because the encrypted
servers stopped answering. Otherwise anyone who could block ports 443 and
853 would get every name in plain text.

Captive portals are handled the way Android's Private DNS handles them.
When every encrypted server fails, the forwarder runs the check Danube's
[sign-in window](danube.md#captive-portals) uses. It looks
`nmcheck.gnome.org` up through the network's server and fetches GNOME's page
over plain HTTP. If something other than GNOME answers, that is a portal,
and names go to the network's server for 30 seconds before encrypted DNS is
tried again. If GNOME answers, the network is open and simply blocks
encrypted DNS, so the query fails. Outside `strict` mode, the check's own
name always goes to the network's server, so Danube can find a portal that
lets DNS through.

> [!WARNING]
> Whoever runs the network can fake a captive portal and read names in
> plain text for as long as they keep it up. Use `strict` mode if that
> matters more than signing in to hotel Wi-Fi, and turn it off only while
> signing in.

While `/run/losos-adblock/upstreams-forced` exists, encrypted DNS stands
aside, and the forwarder asks only the servers `/run/losos-adblock/upstreams`
names. Schizo mode sets both to send every name to its Tor gateway, whose DNS port puts each
destination's lookups on a circuit of their own and resolves `.onion`
names, neither of which one resolver behind one encrypted connection
would do. Both files live in `/run`, so a machine that stops with them in
place comes back without them, and the person's own mode is never touched.

`losos-adblock encrypted-dns on|strict|off|status` changes or shows the
mode. `losos.adblock.encryptedDns.mode` sets the OS's default, and
`/var/lib/losos-adblock/encrypted-dns` records the person's choice: the
mode on its first line, and optionally servers of their own after it, one
per line, which replace the OS's. An administrator changes the mode
without a root shell by starting `losos-adblock-encrypted-dns@on.service`,
`@strict` or `@off`, and polkit asks for their password. The running
forwarder notices within a second.

## Lists

`losos.adblock.lists` names the lists by https URL or absolute path. The
defaults are EasyList, EasyPrivacy, AdGuard's DNS filter and Peter Lowe's
list. Adblock Plus syntax, hosts files and plain domain lists all work. A
user can add more, one per line, in `/var/lib/losos-adblock/lists`.

`losos-adblock-update.timer` fetches the lists two minutes after boot and
then daily, and compiles both halves. It sends `If-None-Match`, so an
unchanged list is not downloaded again. If a list fails to download, it keeps
the last good copy.

> [!TIP]
> `losos-adblock update` does the same by hand, and
> `losos-adblock check NAME...` says whether a name is blocked and by which
> rule.

## Exceptions

`losos.adblock.allow` and `/var/lib/losos-adblock/allow` list domains that
are never blocked, along with their subdomains, whatever the lists say. In
the browser, nothing on their pages is blocked either. The lists' own `@@`
exceptions apply in both halves too.

## Files

| Path | What |
| --- | --- |
| `/etc/losos-adblock/lists`, `allow` | From the module |
| `/var/lib/losos-adblock/lists`, `allow` | The user's additions |
| `/etc/losos-adblock/encrypted-dns`, `encrypted-dns-default` | Encrypted DNS's servers and default mode, from the module |
| `/var/lib/losos-adblock/encrypted-dns` | The person's encrypted DNS mode, and servers of their own |
| `/var/lib/losos-adblock/lists/` | The fetched copies |
| `/var/lib/losos-adblock/dns.list` | The compiled domain list the forwarder reads |
| `/var/lib/losos-adblock/webkit/` | WebKit rule sets and their `index` |

With the four default lists, the domain list has about 195,000 names and the
WebKit sets come to about 5 MB of JSON. WebKit compiles them once and keeps
the result in `~/.cache/danube/content-filters`.
