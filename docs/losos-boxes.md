# LosOS boxes

A [LosOS](https://losos.dasmat.us) box is the project's home server: a mini-PC
running Nextcloud and Forgejo, with no SSH and no shell, managed from its admin
pages in a browser or from `losos`, its terminal client. LosOS Desktop finds
boxes on the local network and offers each one on the desktop.

On a PC this all happens in one [machine](hypervisor.md), the LosOS boxes
machine, not on the host. It finds the boxes, holds `losos` with its keys and
tokens, opens the admin pages, and carries the other machines' connections to
boxes. The host only lends it its resolver. A phone has no machines, so the
session does the work itself ([On a phone](#on-a-phone)).

## The LosOS boxes machine

The computer creates the machine at first boot as `boxes`, framed in green,
with 1.5 GB of memory, but doesn't start it. The first time a box answers on
one of the computer's networks, the host service `losos-boxes-wake` starts it
and stops watching. On later boots, if the machine remembers a box reachable
through its edge, it starts without waiting for LAN discovery. An unpaired
machine still waits for a box on the network. A machine you stop stays stopped
until the next boot, and you can start it from the Machines app at any time. If
you remove it, the computer won't recreate it. Recreate it in the Machines app
from the **losos-boxes** template, or with:

```sh
pm machine create boxes --image /run/current-system/sw/share/losos/machines/losos-boxes.qcow2 --label green
pm machine start boxes
```

Its template, `nixos/guest/boxes.nix`, is the LosOS guest
([The guest template](hypervisor.md#the-guest-template)) without Flatpak and
with `losos`, `losos-boxes`, Danube, GNOME Keyring and the GitHub CLI. `losos ci
setup` uses `gh` to give a repository's Actions a key for a box.

Each box the machine finds becomes one of its apps, named after the box, such
as `mattbox`, and described as "LosOS v0.4.0 box at mattbox.local". The desktop
shows it as "mattbox in boxes", framed in green. Opening it runs `losos` for
that box in a derisk-terminal window in the machine, signing in first if
`losos hosts` doesn't list the box yet ([The terminal client](#the-terminal-client)).
If anything fails, the window stays open until you press Enter, so you can read
what `losos` said. The app's one action, **Open the admin pages**, opens
`http://mattbox.local/` in Danube in the machine.

## Finding boxes

`losos-boxes watch --terminal`, a service of the machine's user, keeps those
entries current. It browses two DNS-SD types through systemd-resolved's Varlink
call `io.systemd.Resolve.BrowseServices`:

- `_losos._tcp`, which a box announces on its front port, with its release in
  the TXT record as `version=v0.4.0`.
- `_workstation._tcp`, which every box announces, and so do most Linux machines
  running Avahi. Boxes installed before `_losos._tcp` existed announce only
  this until they update, so `losos-boxes` asks each workstation for
  `/setup/state.json` on port 80, which only a box serves. A box found both
  ways gets one entry.

It resolves each instance with `ResolveService` and writes
`losos-box-<host>.desktop` into `~/.local/share/applications`, where
`losos-guest-run --list` picks it up for the desktop. A box that sends mDNS's
goodbye, or whose records expire, loses its entry. The service clears all
entries when it stops and again when it starts, so a box that left unobserved
isn't offered. Entries carry only a host name, because anything on the network
can answer mDNS and the name ends up on an `Exec` line. `losos-boxes list`
prints what it finds within a few seconds, one box per line with its address
and release.

The machine does no mDNS of its own
([Names on the network](hypervisor.md#names-on-the-network)), and its multicast
wouldn't pass libvirt's NAT anyway, so it browses through the host's resolved,
which does mDNS on every host network. `LOSOS_BOXES_RESOLVE` points
`losos-boxes` at `/run/losos-boxes/resolve`, a socket that
`systemd-socket-proxyd` carries to the host over vsock port 7351. On the host,
`losos-boxes relay` (`nixos/modules/losos-boxes.nix`) asks libvirt which
machine has the connection's kernel-stamped vsock address. It passes the
connection to resolved's Varlink socket only if pm recorded that machine's
template as `losos-boxes`. Varlink's resolve interface only looks things up, so
the machine can browse and resolve through the host but change nothing there.

## Away from home

A box that reaches the internet through an
[edge](https://losos.dasmat.us/manual/terminal/#away-from-home) tells
`losos login` its address there, and `losos` keeps it with the box's key. When
the box's own address leads nowhere because you are on another network, `losos`
sends its calls to the edge address instead. They are still signed and
encrypted end to end, so the edge can't read them.

`losos-boxes watch` offers such a box even when mDNS can't find it. Once a
minute it reads `losos hosts`, and for each box with an away-from-home address
that isn't on this network it writes an entry described as "LosOS box
mattbox.local, away from home, through mattbox.losos.cfd". Opening it opens the
box in `losos`, as at home. A separate "LosOS Cloud: mattbox" entry opens LosOS
Cloud at the edge address, which the edge serves to anyone who signs in to
LosOS Login. It is a separate entry because the PC's machine launcher doesn't
show desktop actions. The admin pages stay on the box's own network.

When the box is on the network again, its usual entry replaces the away entry
and the Cloud entry goes. `losos logout` removes both. While the keyring is
locked, `watch` reads nothing and leaves the entries as they were, so it never
prompts for the keyring password.

Palette search in the boxes' files ([In the command palette](#in-the-command-palette))
also goes through `losos`, so it finds files on a box away from home too.

## From the other machines

The other machines reach a box through the LosOS boxes machine, so it is the
only machine that talks to boxes:

1. `losos-boxes route`, a system service in the LosOS boxes machine, finds the
   boxes the same way and sends their names to the host's relay on vsock port
   7352 whenever they change.
2. The relay writes those names, with the machine's DHCP address, into
   `/run/losos-boxes/hosts/<machine>`. libvirt's dnsmasq, every machine's DNS
   server, rereads that directory on change through the `hostsdir` option that
   `losos-boxes-dns` adds to the `default` network at boot. So dnsmasq answers
   `mattbox.local` with the LosOS boxes machine's address before asking the
   host's resolved.
3. `route` takes each connection on ports 80 and 443, reads the name from the
   `Host` header or the TLS ClientHello's server name, and passes the
   connection on unchanged to that box, TLS included. The certificate the other
   machine sees, and that `losos` pins, is the box's own. A name that isn't a
   present box gets nothing.

When the LosOS boxes machine stops, its hosts file goes away, and the host
answers box names like any `.local` name, with the box's own address. Only
ports 80 and 443 go through the machine, because that is where a box serves its
admin pages, cloud and Git. An app's own port on a box is reached directly, and
nothing stops a machine from connecting to a box address it already knows.

## On a phone

The GSI has no machines, so the session runs `losos-boxes watch` itself and
carries `losos`. Each box is an app on the phone. Opening it opens the admin
pages in the default browser, and its action, **Manage in the terminal**, opens
`losos`.

## In the command palette

derisk's command palette also searches the files of the boxes `losos` is signed
in to: LosOS Cloud files and the default branches of LosOS Git repositories, as
the box's own palette does. Each match is a row with the file name, its folder
or repository, and the words around the match, such as
`budget.md  mattbox · LosOS Cloud › Documents · the rent is due`. A cloud file
opens its folder in LosOS Cloud, and a Git match opens the file at that line,
both in the default browser.

`losos-boxes search <text>` runs `losos hosts`, then `losos call GET
/api/cloud/text` and `/api/git/text` on up to three boxes in parallel, and
prints the rows as JSON. On a PC it runs in the LosOS boxes machine, where
`losos` and its tokens are: derisk runs `losos-boxes-search` there through the
guest agent, as the machine's user on that user's bus. derisk searches only
while the machine is running and never starts it for a search. On a phone,
derisk runs `losos-boxes search` in the session.

A search never prompts. Asking a locked keyring for tokens would pop up a
password window, so while the keyring is locked the search finds nothing. It
also finds nothing on a box that doesn't answer, has search off, or runs no
LosOS Git. derisk searches once you have typed three characters, one search at
a time.

## The terminal client

`losos` shows the same panes as a box's admin pages, with the same words and
colours. Every request is signed by your GPG key and encrypted to the box, and
the box answers the same way, so it needs no SSH and plain `http://` is safe.
The handbook's [terminal client](https://losos.dasmat.us/manual/terminal/) page
is the full guide. On LosOS Desktop:

```sh
losos key import --gpg you@example.com
losos login http://mattbox.local
losos
```

> [!IMPORTANT]
> A box accepts only keys that were on the stick it was installed from. The
> token `login` asks for comes from **Settings → Access tokens** in the box's
> admin pages. `login` shows the box's key in groups of four. Compare it with
> that pane before you accept.

`losos` keeps keys and tokens in the Secret Service through libsecret's
`secret-tool` and never writes them to a file. GNOME Keyring provides the
Secret Service in the LosOS boxes machine and in a phone's session. Nobody
signs in to the machine, so the host keeps a random keyring password for it in
`/var/lib/losos-boxes/keyrings/`, created the first time the machine asks and
readable only by the relay's account. At every boot the machine fetches it on
vsock port 7353 and its keyring starts unlocked, so nothing prompts you. A copy
of the machine's disk alone can't open the keyring. A keyring created before
the host kept a password still asks for the password you chose then. To switch,
remove and recreate the machine, then sign `losos` in again.

`key import --gpg` reads the key from gpg. Both tools are on `losos`'s `PATH`
after your own entries, so a gpg installed with [pm](pm.md) wins.

## How it is built

`nixos/pkgs/losos.nix` builds the `losos` crate from `github.com/dasmatus/losos`
at `admin-ui/tui`, at a pinned commit, with the same `cargoHash` as that
repository's `.#losos`. Its build script reads the admin UI's catalogue,
palette, pane keywords and the salmon colour, and it links the envelope crate
in `backend/access`. A sparse checkout fetches only those directories, about
3 MB of a 400 MB tree, which is why it is a `fetchgit` pin and not a flake
input ([What this trusts from outside](trust.md)).

The commit and hashes are in `nixos/pkgs/losos-pin.nix`. On every release tag,
the LosOS repository's `desktop-pin` workflow opens a pull request here that
points the file at the tag with all the hashes, so nobody edits them by hand.
To pin another commit, run that repository's
`.github/scripts/desktop-pin.sh <tag> <checkout of this repository>`, or set
`rev` and let a build report each hash. The client's own tests, a mock box and
edge over TLS and a pseudo terminal, run in the LosOS repository's CI.

`losos-boxes` lives in `src/losos-boxes`, and `nixos/pkgs/losos-boxes.nix`
builds it with its unit tests. Three VM tests cover it:

- `nixos/tests/losos-boxes.nix` boots two boxes, as far as mDNS and nginx go,
  one announcing `_losos._tcp` and one older, plus a phone session with
  resolved and no Avahi. The session lists both, writes their entries, and
  drops the first one's entry when it leaves the network.
- `nixos/tests/losos-box.nix` boots a real box from the pinned commit's modules,
  with lososd, LosOS Login, LosOS Cloud and LosOS Git, claimed as the setup
  wizard claims it. The desktop finds it, signs `losos` in with a token for the
  box's install key, reads the box's state through the encrypted envelope, pins
  its certificate, signs backups in through LosOS Login with a browser-approved
  code, and backs a home up to it ([Backups](backup.md)). The pin pull requests
  run it, so a LosOS change that breaks the desktop shows before it merges.
- `nixos/tests/machines.nix` creates a LosOS boxes machine on a host whose LAN
  has a box. It checks that the machine offers the box, that another machine's
  request for the box's name goes through it and gets the box's answer, that a
  non-box name is refused, and that stopping the machine removes its names.
