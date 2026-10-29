# Peripherals in machines of their own

A USB device runs code someone else wrote, and the host's drivers trust
what it says. A drive's filesystem, a camera's video and a Bluetooth
radio's protocol are all parsed by the kernel that reads them, and a bug
in any of those parsers is a way in. [Qubes OS](https://www.qubes-os.org)
answers this with sys-usb: the USB controllers belong to a machine of
their own, and dom0 never drives a USB device. LosOS Desktop does the
same, and goes further with Bluetooth and with the devices themselves.
Nobody using the computer has to know any of it: the machines involved
are hidden from the Machines app, start only when a device needs them,
and stop again afterwards.

> [!NOTE]
> This is the PC's. A phone's USB and Bluetooth are the vendor's, behind
> Android's HALs ([Halium GSI](halium.md)).

## Where each device goes

| Device | Where it goes |
| --- | --- |
| USB controllers | sys-usb, passed through whole |
| Bluetooth radios | sys-bluetooth, which runs BlueZ |
| Network cards, USB Ethernet and Wi-Fi dongles | sys-net, the network machine |
| Drives, card readers, phones and cameras' photos (PTP, MTP) | sys-files, the files machine, which hands the host their files |
| Cameras, sound cards, printers, anything else | a disposable machine of its own |
| Keyboards, mice, touchpads, game pads, security keys, smart card and eID readers | the host |

The host keeps what it cannot be used without. Sign-in takes a security
key, the desktop takes a keyboard and a pointer, and the disk's password
at boot is typed on that keyboard. Smart card readers (USB class CCID)
stay too: the host's pcscd drives them, and a security key such as a
YubiKey is often HID and a smart card at once. It also keeps any controller that has
the disk it booted from on it.

The router decides by the drivers that would claim a device, not only by
what the device says it is. `modprobe` names the drivers for each of its
interfaces, and where they live in the kernel's tree gives the answer: a
driver under `drivers/net` drives a network card, and one under
`drivers/bluetooth` a radio. Many Wi-Fi dongles, Ethernet adapters and
Bluetooth radios describe themselves only as "vendor specific", and only
the drivers' ID tables know what they are. A device that is several things
goes by what it is for: a headset is sound, even though its volume
buttons are HID.

## The router

`losos-peripherals serve` (`src/losos-peripherals`,
`nixos/modules/peripherals.nix`) runs on the host as
`losos-peripherals.service`. It reads the kernel's device events from
netlink before udev acts on them, and moves each device in one of two
ways.

- **A device on one of the host's own ports** waits unconfigured. The
  router claims every port of every hub the host has
  (`USBDEVFS_CLAIM_PORT`). The kernel still enumerates what is plugged in
  and reads its descriptors, but it configures nothing, and binds no
  driver to a device on a claimed port. The router parses the descriptors
  itself, and either gives the port back to the kernel, for a device the
  host keeps, or has libvirt pass the device to its machine as it is.
- **A device in sys-usb** reaches its machine over USB/IP on vsock. The
  kernel's own `usbip-host` in sys-usb and `vhci-hcd` in the receiving
  machine each take a socket and want only that it is a stream, which a
  vsock socket is. The router connects to both machines and copies
  between the two connections. A device sys-usb holds that the host keeps,
  such as a USB keyboard, comes to the host's own `vhci-hcd` the same way.

Only the host connects. Each machine listens on vsock, which nothing but
the host can reach, and drops a connection that does not come from a port
below 1024, which only the host's root can bind. Someone signed in to the
host cannot ask sys-usb for another person's device, and no machine can
reach another.

What sys-usb says about its devices comes from a machine a device may
have taken over, so the router trusts nothing in a report beyond where
the device should go. A name is only logged, and a port that cannot exist
is dropped.

`/run/losos-peripherals/devices.json` says where every device is now.

## Which controllers stay

`losos-peripherals-keep.service` runs at every boot before any controller
is passed through. It waits for the USB device list to stop growing, then
writes the PCI functions the host keeps to `/run/losos-passthrough/keep`.
Those are every function above a mounted disk, through device-mapper to the
disks under `/usr`'s verity, and above any keyboard or pointer. Input
devices are recognised by udev's own test. The passthrough unit skips those
functions, and refuses any IOMMU group that holds one.

On a laptop the keyboard and touchpad are usually not USB at all, so every
USB controller goes to sys-usb. On a desktop with a USB keyboard, the
controller it is plugged into stays with the host, and the router holds
that controller's other ports as above.

## Lazy machines

None of these machines runs before a device needs it, and each hands back
memory it is not using (`virtio-balloon` with free page reporting).

- **sys-usb** is stopped while nothing is plugged into its controllers.
  Then the controllers are the host's, with every port held by the router.
  The first device plugged into one starts sys-usb. libvirt takes the
  controllers from the host as the machine starts, the device enumerates
  again inside it, and the router routes it from there. A minute after
  the last device is pulled out, the router stops sys-usb and libvirt gives
  the controllers back.
- **sys-bluetooth** runs while Bluetooth is on. Whether it is on is kept in
  `/var/lib/losos-peripherals/bluetooth` across boots. While it is off the
  radio waits where it is.
- **A disposable machine** is made when its device is plugged in and
  destroyed when it is pulled out. It is a transient libvirt domain with no
  disk, so whatever the device did in it is gone with it.

## Sound cards

A sound card is no use with only its driver, so its disposable machine is
not BusyBox but NixOS, a service machine like schizo mode's Tor gateway,
with Kicksecure's hardening (`nixos/guest/sound.nix`). It runs PipeWire
system-wide, since nobody signs in there, with PipeWire's PulseAudio
server, and `losos-peripherals forward` joins each connection the host makes
to its vsock port 3244 to that server's socket.

Once the card is in the machine's PipeWire, the router makes a socket for
the machine, `/run/losos-peripherals/sound/disposable-<cid>`, and removes
it when the card is pulled out. The host's PipeWire, that of whoever is
signed in, watches the directory (`losos-peripherals-sound.path`, a user
unit). For each socket it tunnels a sink to the machine's sound server,
and a source when the card records too, so the card shows in Settings'
Sound page as any other would. It unloads them when the socket goes.

The card's name in Settings comes from the machine. The machine is the
device's, so only letters, digits and a little punctuation of the name
show.

## BusyBox machines

Each of these machines but a sound card's is BusyBox, one program and the host's own kernel,
and nothing else (`nixos/modules/busybox-guests.nix`). There is no
distribution inside to update, no service manager, and no shell anyone logs
in to. A machine's whole system is its initrd, which sits in the host's
verity `/usr`, and it starts over from that at every boot.

`/init` mounts the kernel's filesystems, loads the modules the machine
names, mounts its 9p shares, starts mdev and runs `/etc/init.d/*`. It then
replaces itself with the machine's program, which is PID 1 from then on.

| Machine | PID 1 | What else is in it |
| --- | --- | --- |
| sys-usb | `losos-peripherals usb-machine` | USB controller drivers and `usbip-host`, and no class driver: devices are enumerated and sent on |
| sys-bluetooth | `losos-peripherals bluetooth-machine` | BlueZ and its own system bus, the host's firmware read-only, and its pairings in a share of the host's `/var/lib/losos-peripherals/bluetooth` |
| disposable | `losos-peripherals receiver` | the host's kernel modules and firmware read-only, so whatever driver the device needs loads |

A sound card's disposable machine is the exception, as above.

The kernel is the one the host booted, so a machine runs whatever kernel
the host runs, and an update moves both.

Each one is hardened the way Kicksecure hardens a system, with the same
settings the NixOS service machines take (`nixos/guest/kicksecure-settings.nix`,
[schizo-mode.md](schizo-mode.md#service-machines)). A BusyBox machine has
no sysctl.d and no NixOS, so `busybox-guests.nix` applies them itself:

- Kicksecure's kernel parameters go on the machine's command line. The
  kernel takes the ones it knows and hands the rest to `/init`, which
  ignores them.
- `/init` sets Kicksecure's sysctls with `sysctl -w` before anything else
  runs.
- `/etc/modprobe.d/kicksecure.conf` turns each disabled module into
  `install <module> /bin/false`, which stops it even when it is asked for by
  name. A blacklisted module is a `blacklist` line, which stops mdev loading
  it for a device.

A machine takes a module back with
`losos.busyboxGuests.<name>.kicksecure.allowModules`, and sets a sysctl of
its own with `kicksecure.sysctl`. Three do:

| Machine | What it changes, and why |
| --- | --- |
| sys-net | Answers pings, because the host pings it to see it is up. Uses loose reverse-path filtering, because strict filtering drops the multicast replies that LAN edges answer a DNS-SD browse with. |
| sys-shares | Loads the CIFS and NFS clients, because mounting network shares is its job. The servers, `ksmbd` and `nfsd`, stay off. |
| sys-files | Loads the rare file systems, `cdrom` and `sr_mod`. A Mac's USB stick or a DVD should be read here, away from the host's kernel, since this machine exists to parse drives nobody vouches for. |

## Bluetooth in Settings

Settings' Bluetooth page runs `bluetoothctl`, as it always has. On a PC the
`bluetoothctl` on `PATH` is `losos-peripherals bluetoothctl`, which passes
its arguments to the router over `/run/losos-peripherals/control`. The
router runs BlueZ's own `bluetoothctl` in sys-bluetooth with them.

- Turning Bluetooth on starts sys-bluetooth, and sys-usb too, since a
  laptop's radio sits on its internal USB controller. The router hands the
  radio over, waits for BlueZ to see it, then turns it on.
- Turning it off stops sys-bluetooth.
- While Bluetooth is off, the router answers the page itself. It reports
  the radio, if there is one, as off, and no devices.

Anyone signed in may use the control socket, as BlueZ lets anyone on the
system bus use Bluetooth.
