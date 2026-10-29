# Security keys and smart cards

A security key, such as a YubiKey, and a smart card reader, such as the one
a Slovak eID card goes into, stay plugged into the host. The machines use
them through it. A person signs in to the host with them, so the login
screen must always reach them, and pulling one out must take it from every
machine at once. `losos-tokens`, built from `src/losos-tokens`, does the
relaying. `nixos/modules/tokens.nix` runs its host half and
`nixos/guest/tokens.nix` runs its machine half in every
[machine template](hypervisor.md#the-guest-template).

## Security keys

A machine gets a copy of each FIDO key the host has. `losos-tokens guest`
makes the copy with uhid, the kernel's way to create a HID device from a
program. A browser, libfido2 or ssh in the machine sees a key on USB with
the real key's name, vendor and product. Everything a program writes to the
copy goes to the host over vsock port 7370, and `losos-tokens host` writes
it to the real key's hidraw device. The key's answers come back the same
way. The person touches the key and types its PIN as they would on the
host.

The host half watches `/sys/class/hidraw` for devices whose report
descriptor declares the FIDO usage page, 0xF1D0, and tells every connected
machine when one appears or goes. It skips the copies, which name
`losos-tokens` as their physical path, so a host that is itself a machine
never hands a key back.

Several machines and the host can use a key at once. Each program opens a
channel on the key with CTAPHID's INIT command and a nonce. The key answers
on the broadcast channel with the same nonce and the new channel's number.
The relay reads those two reports and nothing else, and remembers which
machine opened which channel. A machine then gets only the answers on its
own channels, and may write only on those or send INIT. The host's own
programs, such as derisk's login, open their channels through hidraw beside
the relay, so no machine sees them. The host's journal says which machine
started using which key.

In a machine nobody logs in, so systemd's uaccess rule gives its user no
access to the copy. A udev rule gives the copies to the `users` group
instead, which the machine's user is in. On the host, a udev rule gives FIDO
keys to the relay's own account, `losos-tokens`, and the person at the seat
keeps the access uaccess gives them.

## Smart cards

The host runs pcscd with the CCID driver, which covers every USB smart card
reader and the card side of keys such as a YubiKey. A machine runs no pcscd
of its own. `losos-tokens guest` listens at `/run/pcscd/pcscd.comm` instead,
where libpcsclite looks for it, and passes each connection over vsock port
7371 to the host half, which passes it on to the host's pcscd. pcscd asks
polkit about each client and allows only the person at the seat, so a
polkit rule lets the relay's account in too. The relay decides which machine
gets the readers.

One machine holds the readers at a time. A card keeps what a PIN unlocked
until it is reset or pulled out. If two machines shared a reader, the second
could sign with a card the first had unlocked. The first machine to open a
connection holds the readers until its last connection closes. Until then
the host refuses every other machine's connections, and its journal says
which machine holds them. The host's own programs are not limited this way.

## Hand-offs between apps

Some sites hand work to an app on the person's own computer through
`localhost`. slovensko.sk signs a person in with their eID card this way.
The page sends the browser to `http://127.0.0.1:24727/`, where an eID
client listens (BSI TR-03124), and the eID client reads the card and sends
the browser back. With the browser on the host and the eID klient in a
machine, the two localhosts are different places.

`losos-tokens guest` looks at what listens in its machine every two
seconds, on the ports it serves (24727 in the image), and tells the host
when something starts or stops listening on one. `losos-tokens host`
listens on the same ports of the host's localhost and passes each
connection to the machine that serves the port, over vsock port 7372. If
several machines serve it, the one that started last gets it, since that
is the one the person just opened the app in. If no machine serves it, the
host closes the connection, as a computer with nothing listening would. A
machine reaches only the ports it serves, and only from the host.

The hand-off works the other way without help. Danube in a machine reaches
the eID klient on that machine's own localhost.

## The Slovak eID

The Ministry of the Interior's eID klient signs in to slovensko.sk and
other Slovak public services with the eID card. Every x86_64 machine has
it as an app, "eID klient". Its licence lets nobody pass it on, so the
image carries only `losos-eid`, from `nixos/guest/eid.nix`. The first time
the app is started, `losos-eid` downloads the eID klient from
`eidas.minv.sk` into `~/.local/share/losos-eid` in the machine. Each later
start asks the server for the file's size and downloads it again only when
the size has changed, because the server gives no date or version for the
file. Without the network, the copy already there runs.

The download is an AppImage. `losos-eid` unpacks it without running
anything from it, and starts the eID klient in an FHS environment with the
few libraries it does not carry. The eID klient is built only for X11, so
every machine runs xwayland-satellite, an X server that turns each X11
window into a Wayland one for waypipe to carry. The card goes through the
host's reader as described in [Smart cards](#smart-cards), and the eID
klient asks for its PIN in a window of its own. The host forwards
slovensko.sk's hand-off to it while it is open, as
[Hand-offs between apps](#hand-offs-between-apps) describes.

> [!IMPORTANT]
> Open the eID klient in a machine before signing in on the host. The
> host reaches only an eID klient that is running.

## Signing in with the eID

A person can sign in to the computer itself with their eID card, as they
can with a security key. homed does the work, as it does for a key. It
encrypts a secret for the person's account to a certificate on a PKCS#11
token, and at login the token decrypts it once the person types the
token's PIN. The eID card's certificate for encryption is the one used,
because only its key may decrypt.

The card speaks PKCS#11 only through the Ministry's closed module, which
comes in the eID klient download. So the module runs in a machine of its
own, the eID machine (`nixos/guest/eid-machine.nix`), and no closed code
runs on the host. It is one of the machines the OS runs parts of itself
in (`nixos/modules/service-machines.nix`). It boots from the host's own
store, the Machines app does not show it, and it has no network card. `nixos/modules/eid-signin.nix` is the host's side:

- homed loads a PKCS#11 module called `losos-eid`. It is p11-kit's remote
  mode, not a library: every call goes as p11-kit's RPC to
  `/run/losos-eid/pkcs11.sock`. Only homed, its helper and homectl load it.
  Danube and the machines still use the readers through pcscd.
- systemd hands each connection on that socket to
  `losos-tokens pkcs11-relay`. The relay lets in root, which is homed, and
  the person at the seat, and refuses everyone else. It then connects to
  the eID machine's vsock port 7373 from a port below 1024, which only a
  program with `CAP_NET_BIND_SERVICE` can bind.
- In the eID machine, systemd hands each connection to
  `losos-tokens pkcs11-serve`. It checks that the connection comes from
  the host and from a port below 1024, and then runs `p11-kit remote` with
  the module on it. Nothing in the machine or on the host can skip the
  relay.
- The module reads the card through the host's pcscd, as any machine
  does. Each connection loads the module for one conversation and exits
  with it, so the eID machine holds the readers only while the host is
  reading the card. A machine running the eID klient for slovensko.sk is
  refused the readers for those few seconds.

The image carries no part of the eID klient. `losos-eid-signin` turns
eID sign-in on for the person who runs it, with the card in a reader. The
first time, it starts `losos-eid-fetch.service`, and polkit asks an
administrator to allow it. That service downloads and unpacks the eID
klient as nobody in particular, the way `losos-eid` does in a machine, and
shares the copy with the eID machine read-only. A path unit starts the eID
machine from then on, at every boot, and a timer looks for a new eID klient
every week. Then `losos-eid-signin` finds the card's certificates with
p11tool, through the same relay. It picks the one whose key may encipher
and is not an authority's, and enrols it by its ID with
`homectl update --pkcs11-token-uri`. homed decrypts with the private key
that shares that ID. homectl asks for the card's PIN, to
find the key beside the certificate, and for the person's password. If the card has more than one such
certificate, it lists them and takes the one given on its command line.
At the login screen, pam_systemd_home asks for the token's PIN, and derisk
shows the prompt.

> [!IMPORTANT]
> Nobody has signed in with a real eID card yet. If the Ministry's module
> insists on its own PIN window rather than taking the PIN homed passes
> it, that window opens in the eID machine, which has no screen, and
> signing in fails. The password keeps working either way.

## Which machine is asking

The host knows which machine a connection comes from by the vsock address
the kernel stamps on it. It never trusts anything the machine says about
itself. It asks libvirt's read-only connection which running machine has
that address, and refuses an address no running machine has.

## Tests

`nixos/tests/tokens.nix` runs both halves on one machine over vsock's
loopback. The host half there takes connections from that kernel as coming
from a machine called `work`. A Python program makes a FIDO2 key with uhid
that answers INIT and CTAP2's getInfo. The machine half makes its copy,
`fido2-token -I` reads the key's AAGUID through it, and stopping the key
takes the copy away. vsmartcard's virtual reader in the host's pcscd is
listed through the machine's socket, and so is the virtual card put into it.
A web server on the machine's localhost stands in for the eID klient. The
host's localhost reaches it while it listens, and nothing does once it
stops. The machine's localhost is 127.0.0.2 in that test, because the host
half listens on 127.0.0.1 of the same kernel.

`nixos/tests/eid-signin.nix` runs the host's side of eID sign-in and the
eID machine's half on one machine, with SoftHSM serving a token in place of
the Ministry's module. The token holds an encryption certificate and a
signing one. `losos-eid-signin --print` picks the encryption one, homed
enrols it for a person, and the person logs in on the console with the
token's PIN and no password. A program run by `nobody` gets no token from
the relay. A connection to the eID machine's port from an ordinary port is
refused.

Nothing tests the eID klient itself, which needs a real card and the
Ministry's servers.
