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
