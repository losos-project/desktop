# LosOS boxes

A [LosOS](https://losos.dasmat.us) box is the project's home server. It is a
mini-PC running Nextcloud and Forgejo, with no SSH and no shell, managed from
its admin pages in a browser or from `losos`, its terminal client. LosOS
Desktop finds the boxes on the local network by itself and offers each one on
the desktop.

On a PC all of that happens in one [machine](hypervisor.md), the **LosOS
boxes machine**, and not on the host. That machine finds the boxes, holds
`losos` with its keys and tokens, and opens the admin pages, and the other
machines reach a box through it. The host only lends it its resolver. A phone
has no machines, so there the session does the work itself, as described
[below](#on-a-phone).

## The LosOS boxes machine

The computer makes it at its first boot, as `boxes`, framed in green, with
1.5 GB of memory. It does not start then. The first time a box answers on
one of the computer's networks, `losos-boxes-wake`, a service on the host,
starts it, and stops watching. A machine you stop after that stays stopped
until the next boot, and you can start it in the Machines app at any time.
If you remove it, the computer does not make it again. Make it again in the
Machines app from the **losos-boxes** template, or with

```sh
pm machine create boxes --image /run/current-system/sw/share/losos/machines/losos-boxes.qcow2 --label green
pm machine start boxes
```

Its template is `nixos/guest/boxes.nix`. That is the LosOS guest's machine,
`nixos/guest/common.nix`, described in [the guest
template](hypervisor.md#the-guest-template), without Flatpak and with `losos`,
`losos-boxes`, Danube, GNOME Keyring and the GitHub CLI, `gh`. `losos ci
setup` uses `gh` to give a repository's Actions a key for a box.

Each box the machine finds becomes one of its apps on the desktop. The app is
named after the box, such as `mattbox`, and described as "LosOS v0.4.0 box at
mattbox.local". The desktop shows it as "mattbox in boxes", framed in the
machine's colour. Opening it opens the box in `losos`, in a derisk-terminal
window in the machine. If `losos hosts` does not list the box yet, it signs
in first, as described [below](#the-terminal-client). If anything fails, the
window stays open until Enter is pressed, so what `losos` said can be read.
The app's one action, **Open the admin pages**, opens `http://mattbox.local/`
in Danube, also in the machine.

## Finding boxes

`losos-boxes watch --terminal`, a service of the machine's user, keeps those
entries up to date. It browses two DNS-SD types through systemd-resolved's
Varlink interface, `io.systemd.Resolve.BrowseServices`:

- `_losos._tcp`, which a box announces on its front port, with its release
  in the TXT record as `version=v0.4.0`.
- `_workstation._tcp`, which every box announces, and so do most Linux
  machines running Avahi. Boxes installed before the `_losos._tcp`
  announcement existed announce only this until they update. So
  `losos-boxes` asks each workstation for `/setup/state.json` on port 80,
  which only a box serves. A box found both ways gets one entry.

It resolves each instance with `ResolveService` and writes
`losos-box-<host>.desktop` into `~/.local/share/applications`, where the
machine's `losos-guest-run --list` finds it for the desktop. A box that sends
mDNS's goodbye, or whose records expire, loses its entry. The service removes
all entries when it stops and again when it starts, so a box that left while
nothing was watching is not offered. Only a host name goes into an entry,
because anything on the network can answer mDNS and the name ends up on an
`Exec` line. `losos-boxes list` prints what it finds within a few seconds,
one box per line, with its address and release.

The resolved it browses through is the host's. The machine does no mDNS of
its own, as [names on the network](hypervisor.md#names-on-the-network)
explains, and its multicast would not get past libvirt's NAT to the LAN
anyway. The host's resolved, on the other hand, does mDNS on every network
the host is on. So `LOSOS_BOXES_RESOLVE` points the machine's `losos-boxes`
at `/run/losos-boxes/resolve`, a socket that `systemd-socket-proxyd` carries
to the host over vsock, port 7351. On the host, `losos-boxes relay`, set up in
`nixos/modules/losos-boxes.nix`, takes the connection and asks libvirt which
machine has the vsock address the kernel stamped on it. It passes the
connection to resolved's Varlink socket only if pm recorded that machine's
template as `losos-boxes`, and refuses any other machine. Varlink's resolve
interface only looks things up, so the machine can browse and resolve through
the host but cannot change anything there.

## From the other machines

The other machines reach a box through the LosOS boxes machine, so it is the
one machine that talks to boxes:

1. `losos-boxes route`, a system service of the LosOS boxes machine, finds
   the boxes the same way. Whenever they change, it sends their names to the
   host's relay on vsock port 7352.
2. The relay writes those names, with the address libvirt's DHCP gave that
   machine, into `/run/losos-boxes/hosts/<machine>`. libvirt's dnsmasq, the
   DNS server of every machine, rereads that directory whenever it changes,
   through the `hostsdir` option that `losos-boxes-dns` adds to the
   `default` network once at boot. So dnsmasq answers `mattbox.local` with the
   LosOS boxes machine's address before it would ask the host's resolved.
3. `route` takes each connection on ports 80 and 443 and reads which name it
   is for from its `Host` header or its TLS ClientHello's server name. It
   passes the connection on unchanged to that box, TLS included, so the
   certificate the other machine sees, and that `losos` pins, is the box's
   own. A name that is not a present box gets nothing.

When the LosOS boxes machine stops, its hosts file goes away, and box names
are answered like any other `.local` name again, by the host with the box's
own address. Only ports 80 and 443 go through the machine, because that is
where a box serves its admin pages, its cloud and its Git. An app's own port
on a box is reached directly. Nothing stops a machine from connecting to a
box address it already knows. Only connections made by name go through the
LosOS boxes machine.

## On a phone

The GSI has no machines, so the session runs `losos-boxes watch` itself and
carries `losos`. Each box is an app of the phone. Opening it opens the admin
pages in the default browser, and its action, **Manage in the terminal**,
opens `losos`.

## In the command palette

Text typed into derisk's command palette is also looked for inside the
files of the boxes `losos` is signed in to: the files in LosOS Cloud and
the default branches of LosOS Git's repositories, as the box's own
palette finds them. A file found that way is a row named after the file,
with the folder or repository it is in and the words around the match,
such as `budget.md  mattbox · LosOS Cloud › Documents · the rent is due`.
Choosing a cloud file opens its folder in LosOS Cloud, and choosing a line
in Git opens the file at that line, both in the default browser.

`losos-boxes search <text>` does the searching. It runs `losos hosts`,
then `losos call GET /api/cloud/text` and `/api/git/text` on up to three
of the boxes it lists, all at once, and prints the rows as JSON. On a PC
it runs in the LosOS boxes machine, where `losos` and its tokens are:
derisk runs `losos-boxes-search` there through the guest agent, which is
`losos-boxes search` as the machine's user, on that user's bus. derisk
asks only while the machine is running and never starts it for a search.
On a phone derisk runs `losos-boxes search` in the session itself.

A search never asks for anything. `losos` keeps its tokens in the
keyring, and asking a locked keyring for them makes it show a password
window, so while the keyring is locked `losos-boxes search` finds nothing.
It also finds nothing in a box that does not answer, or whose search is
off, or that runs no LosOS Git. derisk asks once the typed text has three
characters, and one search at a time.

## The terminal client

`losos` shows the same panes as a box's admin pages, with the same words and
colours. Every request it sends is signed by your GPG key and encrypted to
the box, and the box answers the same way, so it needs no SSH and plain
`http://` is safe for it. The handbook's
[terminal client](https://losos.dasmat.us/manual/terminal/) page is the full
guide. On LosOS Desktop it comes down to:

```sh
losos key import --gpg you@example.com
losos login http://mattbox.local
losos
```

> [!IMPORTANT]
> A box accepts only keys that were on the stick it was installed from, so
> the key has to be one of those, and the token `login` asks for comes from
> **Settings → Access tokens** in the box's admin pages. `login` shows the
> box's key in groups of four to compare with that pane before you accept
> it.

`losos` keeps keys and tokens in the Secret Service through libsecret's
`secret-tool`, and never writes them to a file. GNOME Keyring answers the
Secret Service in the LosOS boxes machine, and in a phone's session. Nobody
signs in to the machine, so no sign-in password unlocks its keyring. The
host keeps a password for it instead, made at random the first time the
machine asks and kept in `/var/lib/losos-boxes/keyrings/`, readable only by
the relay's own account. At every boot the machine asks the relay for it on
vsock port 7353, and its keyring starts unlocked with it, so nothing asks
you for a password. A copy of the machine's disk alone does not open its
keyring. A keyring made before the host kept a password has the password
you chose for it then, and keeps asking for that one. To move it over,
remove the machine and make it again, then sign `losos` in again.
`key import --gpg` reads the key
from gpg. Both tools are on `losos`'s `PATH`, after the user's own, so a gpg
installed with [pm](pm.md) is the one it uses.

## How it is built

`nixos/pkgs/losos.nix` builds the `losos` crate from the LosOS repository,
`github.com/dasmatus/losos` at `admin-ui/tui`, at a pinned commit. It is the
same crate and `cargoHash` as that repository's own `.#losos`. Its build
script reads the admin UI's catalogue, palette, pane keywords and the salmon,
and the crate links the envelope crate in `backend/access`. So a sparse
checkout fetches those directories and nothing else, about 3 MB of a 400 MB
tree. That is why it is a `fetchgit` pin and not a `components/` submodule,
as [trust](trust.md) explains: the flake copies its submodules on every
evaluation.

The commit and its hashes are in `nixos/pkgs/losos-pin.nix`. The LosOS
repository moves them: on every release tag its `desktop-pin` workflow
opens a pull request here that points the file at the tag, with the hashes
of the client's sources, its vendored crates and the box's sources, so
nobody edits them by hand. To pin another commit, run that repository's
`.github/scripts/desktop-pin.sh <tag> <checkout of this repository>`, or put
the commit in `rev` and let a build report each hash. The client's own test
suites, a mock box and edge over TLS and a pseudo terminal, run in the LosOS
repository's CI, not here.

`losos-boxes` lives in `src/losos-boxes`, and `nixos/pkgs/losos-boxes.nix`
builds it with its unit tests. `nixos/tests/losos-boxes.nix` boots two boxes,
as far as mDNS and nginx go, one announcing `_losos._tcp` and one from before
that announcement. It also boots a phone's session with resolved and no
Avahi. The session lists both boxes, writes their entries, and drops the
first box's entry when that box leaves the network.
`nixos/tests/losos-box.nix` boots a real box beside a desktop: the box's
own modules from the pinned commit, with lososd, LosOS Login, LosOS Cloud
and LosOS Git, claimed as the setup wizard claims it. The desktop finds it
by its announcement, signs `losos` in with a token made for the GPG key the
box was installed with and reads the box's state through the encrypted
envelope, then pins its certificate and backs a home up to it
([backups](backup.md)). The pin's pull requests run it, so a change in LosOS
that breaks the desktop shows there before it merges.
`nixos/tests/machines.nix` makes a LosOS boxes machine on a host whose LAN
has a box. It checks that the machine offers the box, that another machine's
request for the box's name goes through it and gets the box's answer, that a
name which is not a box is refused, and that stopping the machine removes its
names.
