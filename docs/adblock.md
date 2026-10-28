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
passes every other query on, byte for byte, to the servers the network gave.
It asks NetworkManager for those with `nmcli`, and asks again whenever none
of them answers.
While `/run/losos-adblock/upstreams` exists, its `nameserver` lines are the
only servers the forwarder uses. [Schizo mode](schizo-mode.md) writes the Tor
gateway there.

This half covers Flatpaks, the Android layer and anything else that resolves
names, but it can only block whole domains. A local network's names still
resolve through its server. A bare name such as `printer` no longer gets the
network's search domain appended, though. resolved's DNS over TLS is off,
because the forwarder speaks plain DNS. The forwarder's own queries are plain
DNS too, which is what resolved's "opportunistic" mode fell back to on most
networks anyway.

**Content blockers, in the browser.** Some rules need a URL or a page, such
as a path like `/banner/ad_` or an element to hide like
`example.org##.sponsored`. Brave's `adblock` crate compiles those into WebKit
content blocker rule sets, and [Danube](danube.md) loads them into every
page. Rules the DNS half already applies are left out, so WebKit does not
compile them twice.

![A daily timer compiles the filter lists into a domain list and WebKit rule sets; every app's lookups go through systemd-resolved to the forwarder, which answers 0.0.0.0 or :: for a blocked name and passes the rest to the network's DNS; Danube loads the rule sets for paths and page elements](images/adblock.svg)

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
| `/var/lib/losos-adblock/lists/` | The fetched copies |
| `/var/lib/losos-adblock/dns.list` | The compiled domain list the forwarder reads |
| `/var/lib/losos-adblock/webkit/` | WebKit rule sets and their `index` |

With the four default lists, the domain list has about 195,000 names and the
WebKit sets come to about 5 MB of JSON. WebKit compiles them once and keeps
the result in `~/.cache/danube/content-filters`.
