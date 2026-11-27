# Peripherals in machines of their own

The host's kernel parses everything a USB device sends: a drive's filesystem, a
camera's video, a Bluetooth radio's protocol. A bug in any of those parsers is
a way in. [Qubes OS](https://www.qubes-os.org) handles this with sys-usb, a
machine that owns the USB controllers so dom0 never drives a USB device. LosOS
Desktop does the same and extends it to Bluetooth and to the devices
themselves. You never see these machines: the Machines app hides them, they
start when a device needs them, and they stop afterwards.

> [!NOTE]
> This applies to PCs. On a phone, USB and Bluetooth belong to the vendor,
> behind Android's HALs ([Halium GSI](halium.md)).

## Where each device goes

| Device | Where it goes |
| --- | --- |
| USB controllers | sys-usb, passed through whole |
| Bluetooth radios | sys-bluetooth, which runs BlueZ |
| Network cards, USB Ethernet and Wi-Fi dongles | sys-net, the network machine |
| Drives, card readers, phones and cameras' photos (PTP, MTP) | sys-files, the files machine, which hands the host their files |
| Sound cards, printers and cameras | a Kicksecure machine of its own, which the host plays to, prints to and films from |
| Anything else | a disposable machine of its own |
| Keyboards, mice, touchpads, game pads, security keys, smart card and eID readers | the host |

The host keeps what it can't work without. Sign-in takes a security key, the
desktop needs a keyboard and pointer, and you type the disk password on that
keyboard at boot. Smart card readers (USB class CCID) stay too, because the
host's pcscd drives them and a YubiKey is often HID and a smart card at once.
The host also keeps any controller with its boot disk on it.

The router decides by the drivers that would claim a device, not by what the
device claims to be. `modprobe` names the drivers for each interface, and
their place in the kernel tree gives the answer: a driver under `drivers/net`
means a network card, one under `drivers/bluetooth` a radio. Many Wi-Fi
dongles, Ethernet adapters and Bluetooth radios describe themselves only as
"vendor specific", and only the drivers' ID tables know what they are. A
device that is several things goes by its purpose: a headset is sound, even
though its volume buttons are HID.

## The router

`losos-peripherals serve` (`src/losos-peripherals`,
`nixos/modules/peripherals.nix`) runs on the host as
`losos-peripherals.service`. It reads the kernel's device events from netlink
before udev acts on them, and moves each device one of two ways:

- A device on one of the host's own ports waits unconfigured. The router claims
  every port of every host hub (`USBDEVFS_CLAIM_PORT`), so the kernel
  enumerates the device and reads its descriptors but binds no driver. The
  router parses the descriptors itself. Then it either gives the port back to
  the kernel, for a device the host keeps, or has libvirt pass the device to
  its machine.
- A device in sys-usb reaches its machine over USB/IP on vsock. The kernel's
  `usbip-host` in sys-usb and `vhci-hcd` in the receiving machine each only
  need a stream socket, which vsock is. The router connects to both and copies
  between them. A device in sys-usb that the host keeps, such as a USB
  keyboard, reaches the host's own `vhci-hcd` the same way.

Only the host connects. Each machine listens on vsock, which only the host can
reach, and drops connections that don't come from a port below 1024, which only
the host's root can bind. A signed-in user can't ask sys-usb for someone else's
device, and no machine can reach another.

A device may have taken over sys-usb, so the router trusts nothing in its
reports beyond where a device should go. Names are only logged, and impossible
ports are dropped.

`/run/losos-peripherals/devices.json` lists where every device is now.

## Which controllers stay

`losos-peripherals-keep.service` runs at every boot before any controller is
passed through. It waits for the USB device list to settle, then writes the PCI
functions the host keeps to `/run/losos-passthrough/keep`: every function
above a mounted disk (following device-mapper down to the disks under `/usr`'s
verity) and above any keyboard or pointer, as udev's own test identifies them.
The passthrough unit skips those functions and refuses any IOMMU group that
holds one.

On a laptop the keyboard and touchpad are usually not USB, so every USB
controller goes to sys-usb. On a desktop with a USB keyboard, its controller
stays with the host, and the router holds that controller's other ports.

## Lazy machines

None of these machines runs before a device needs it, and each returns unused
memory (`virtio-balloon` with free page reporting).

- sys-usb is stopped while nothing is plugged into its controllers. The host
  owns them then, with the router holding every port. The first device plugged
  in starts sys-usb, libvirt takes the controllers, the device enumerates again
  inside, and the router routes it. A minute after the last device is
  unplugged, the router stops sys-usb and libvirt returns the controllers.
- sys-bluetooth runs while Bluetooth is on. The on or off state survives
  reboots in `/var/lib/losos-peripherals/bluetooth`. While it is off, the radio
  stays where it is.
- A disposable machine is created when its device is plugged in and destroyed
  when it is unplugged. It is a transient libvirt domain with no disk, so
  whatever the device did inside is gone.

## Sound cards

A sound card needs more than a driver, so its disposable machine is NixOS with
Kicksecure's hardening, a service machine like schizo mode's Tor gateway
(`nixos/guest/sound.nix`). It runs PipeWire system-wide with PipeWire's
PulseAudio server, because nobody signs in there. `losos-peripherals forward`
joins each host connection on vsock port 3244 to that server's socket.

Once the card is in the machine's PipeWire, the router creates
`/run/losos-peripherals/sound/disposable-<cid>` and removes it when the card is
unplugged. The signed-in user's PipeWire watches that directory
(`losos-peripherals-sound.path`, a user unit). For each socket it tunnels a
sink to the machine, plus a source if the card records, so the card shows on
the **Sound** page in Settings like any other.

The machine belongs to the device, so only letters, digits and a little
punctuation of the card's name reach Settings.

## Printers

A printer's disposable machine is NixOS with Kicksecure too
(`nixos/guest/printer.nix`). It runs CUPS, because converting a document to a
printer's language takes a driver and Ghostscript. It creates one queue:

- A printer that speaks IPP over USB, like most made since 2015, is reached
  driverless through `ipp-usb`.
- Any other uses CUPS's USB backend with a driver matching its IEEE 1284 device
  ID: Gutenprint's, CUPS's own, or CUPS's generic PostScript or PCL driver if
  the ID says it takes either. A printer that matches nothing gets no queue,
  and the machine's journal says so.

`losos-peripherals forward` joins each host connection on vsock port 3245 to
CUPS's socket, and the router creates
`/run/losos-peripherals/printer/disposable-<cid>` once the queue exists.
`losos-peripherals-printers.path` then adds a queue of the same name to the
host's CUPS, pointing IPP Everywhere at the machine's queue, so print dialogs
list it. Unplugging the printer removes the queue.

The host's CUPS has no printer drivers and does no network discovery. It only
turns what you print into PDF or raster for the machine, so the printer and its
driver never touch the host. A printer's card slot and scanner go to the
machine with it, where nothing uses them yet.

## Cameras

The host decodes none of a camera's MJPEG, H.264 or raw frames. The camera's
disposable machine is NixOS with Kicksecure and ffmpeg
(`nixos/guest/camera.nix`). Once the camera is in,
`losos-peripherals camera-machine` listens on vsock port 3246 and the router
creates `/run/losos-peripherals/camera/disposable-<cid>`.

For each socket, the host's `losos-peripherals-cameras` service creates a
v4l2loopback device named for the camera, which apps and browsers open like the
camera itself. Nothing streams until an app reads: v4l2loopback reports when
the first reader starts and the last stops, and only then does the host ask the
machine for video. The machine runs ffmpeg while that connection lasts,
converting to 1280x720 YUYV at 30 frames a second, letterboxed. The camera and
its light are on only while an app uses it, and the device shows a black frame
in between. The device goes away when the camera is unplugged.

As with sound cards, only plain characters of the camera's name show.

## BusyBox machines

Every other peripheral machine is BusyBox and the host's kernel, nothing else
(`nixos/modules/busybox-guests.nix`). There is no distribution to update, no
service manager and no shell to log in to. The machine's whole system is its
initrd, stored in the host's verity `/usr`, and it starts fresh from that at
every boot.

`/init` mounts the kernel filesystems, loads the machine's modules, mounts its
9p shares, starts mdev and runs `/etc/init.d/*`. Then it replaces itself with
the machine's program, which becomes PID 1.

| Machine | PID 1 | What else is in it |
| --- | --- | --- |
| sys-usb | `losos-peripherals usb-machine` | USB controller drivers and `usbip-host`, and no class driver: devices are enumerated and sent on |
| sys-bluetooth | `losos-peripherals bluetooth-machine` | BlueZ and its own system bus, the host's firmware read-only, and its pairings in a share of the host's `/var/lib/losos-peripherals/bluetooth` |
| disposable | `losos-peripherals receiver` | the host's kernel modules and firmware read-only, so whatever driver the device needs loads |

The kernel is the one the host booted, so an update moves both.

Each machine gets Kicksecure's hardening, the same settings as the NixOS
service machines (`nixos/guest/kicksecure-settings.nix`,
[Service machines](schizo-mode.md#service-machines)). A BusyBox machine has no
sysctl.d, so `busybox-guests.nix` applies them itself:

- Kicksecure's kernel parameters go on the command line. The kernel takes the
  ones it knows and passes the rest to `/init`, which ignores them.
- `/init` sets Kicksecure's sysctls with `sysctl -w` before anything else runs.
- `/etc/modprobe.d/kicksecure.conf` turns each disabled module into
  `install <module> /bin/false`, which blocks it even when loaded by name. A
  blacklisted module gets a `blacklist` line, which stops mdev loading it for a
  device.

A machine re-enables a module with
`losos.busyboxGuests.<name>.kicksecure.allowModules`, and sets its own sysctl
with `kicksecure.sysctl`. Three machines do:

| Machine | What it changes, and why |
| --- | --- |
| sys-net | Answers pings, because the host pings it to see it is up. Uses loose reverse-path filtering, because strict filtering drops the multicast replies that LAN edges answer a DNS-SD browse with. |
| sys-shares | Loads the CIFS and NFS clients, because mounting network shares is its job. The servers, `ksmbd` and `nfsd`, stay off. |
| sys-files | Loads the rare file systems, `cdrom` and `sr_mod`. A Mac's USB stick or a DVD should be read here, away from the host's kernel, since this machine exists to parse drives nobody vouches for. |

## Bluetooth in Settings

The **Bluetooth** page in Settings runs `bluetoothctl`. On a PC, the
`bluetoothctl` on `PATH` is `losos-peripherals bluetoothctl`, which passes its
arguments to the router over `/run/losos-peripherals/control`, and the router
runs BlueZ's `bluetoothctl` in sys-bluetooth with them.

- Turning Bluetooth on starts sys-bluetooth, and sys-usb too, because a
  laptop's radio sits on its internal USB controller. The router hands over the
  radio, waits for BlueZ to see it, and turns it on.
- Turning it off stops sys-bluetooth.
- While Bluetooth is off, the router answers the page itself: the radio, if
  any, is off, with no devices.

Anyone signed in may use the control socket, just as BlueZ lets anyone on the
system bus use Bluetooth.
