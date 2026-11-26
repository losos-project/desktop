# Virtual machine desktop

On a PC, LosOS Desktop is a hypervisor with a desktop on top, like
[Qubes OS](https://www.qubes-os.org). Linux with KVM runs the machines
directly on the processor's virtualisation extensions, which makes it a type-1
hypervisor the way Xen is under Qubes. The host keeps only what drives the
hardware and the desktop: the kernel, systemd, libvirt, derisk and the host's
settings. Your apps are meant to run in machines. Each machine has a colour
label, and its windows carry that colour.

[libvirt](https://libvirt.org) is the only control plane. Nothing in the OS
starts QEMU itself or keeps its own list of machines. pm, virsh and
virt-manager all talk to `qemu:///system` and see the same machines.

> [!NOTE]
> The GSI does none of this. Most phone kernels have no KVM, so apps there run
> on the host ([Halium GSI](halium.md)).

## The host

`nixos/modules/libvirt.nix` turns the PC into the host:

- libvirtd runs from boot, with QEMU for the PC's architecture and KVM where
  the processor has it. Without hardware virtualisation the image is the same,
  and machines run slowly under emulation.
- `LIBVIRT_DEFAULT_URI` is `qemu:///system` in every session, so `virsh`, pm
  and virt-manager find the machines without being told. A per-user
  `qemu:///session` would see none of them, and could not give a machine a
  vsock device, because `/dev/vhost-vsock` belongs to root.
- The `default` storage pool (`/var/lib/libvirt/images`) and network
  (libvirt's NAT on `virbr0`, `192.168.122.0/24`) start with the host. The
  oneshot `libvirt-defaults` defines them through libvirt's API at boot, so
  changes you make in virt-manager stick. The firewall accepts only DHCP and
  DNS from machines on `virbr0`, and libvirt's nftables table masquerades
  their traffic.
- `vhost_vsock` loads at boot. It is the host's end of each machine's
  virtio-vsock device, which carries the machines' windows.
- waypipe is installed. The desktop runs `waypipe client` for each running
  machine to turn the guest's output back into Wayland.
- swtpm and virt-viewer are installed for
  [machines that install from a CD](#machines-from-an-installer-cd). swtpm is
  the TPM for a Windows machine, and virt-viewer shows a machine's own screen.
- polkit lets the `wheel` group, where first-boot setup puts the first account,
  use `org.libvirt.unix.manage` without a prompt. Giving a machine a host disk
  is an administrator's power anyway. Everyone else gets asked for an
  administrator's password. Every account gets a read-only connection, so every
  session can frame machine windows: it shows which machines exist and run,
  their labels, and which vsock address belongs to which machine.

All of this is state under `/var/lib/libvirt` and `/run/libvirt` on the root
partition. The verity `/usr` and the [update model](systemd.md) stay the same.

## Machines

pm creates machines with `pm machine create`
([pm on LosOS](pm.md#machines-that-stay)). Its libvirt plugin is the only part
of pm that knows about libvirt. In libvirt, a machine has three parts:

- A template: a read-only volume in the `default` pool, shared by every
  machine made from it.
- A disk: a qcow2 overlay on the template in the same pool, holding only the
  machine's changes.
- A definition: a persistent domain with UEFI, a virtio disk, a network card on
  `default`, a QEMU guest agent channel, and a vsock device whose address
  libvirt picks. It has no graphics card and no USB controller. Its
  `<metadata>` holds the label and template.

derisk's Machines app lists them, creates new ones from the templates under
`share/losos/machines` through `pm machine create`, and starts, shuts down and
removes them. For a running machine it lists the machine's apps and opens them.

## Names on the network

libvirt's NAT carries no multicast between `virbr0` and the host's networks,
so a machine's own mDNS query would never reach a LosOS box, printer or PC on
your LAN. The host is on both networks, so it looks `.local` names up for the
machine:

1. The guest's resolved does no mDNS or LLMNR. NetworkManager gives the link
   on `default` the routing domain `~local` through `ipv4.dns-search`, so
   `.local` lookups go to the DNS server from DHCP, the host at
   `192.168.122.1`. Without that routing domain, resolved never sends `.local`
   names to a unicast server.
2. libvirt's dnsmasq on `virbr0` answers from the host's resolver,
   `/etc/resolv.conf`, which is resolved's stub.
3. The host's resolved asks every link it does mDNS on, the LAN and `virbr0`
   alike, and returns the address.

The machine reaches that address through the NAT like any other. While a LosOS
boxes machine runs, a box's name is the exception: dnsmasq answers with that
machine's address, and the connection goes through it
([From the other machines](losos-boxes.md#from-the-other-machines)). Because
nothing in the machine multicasts, it never answers or announces on `virbr0`.
A machine installed from a CD does whatever its own system does, usually Avahi
on `virbr0` alone.

`nixos/tests/machines.nix` checks this: a machine made from the template looks
up `lanbox.local`, a test machine on the host's LAN that answers only over
mDNS, and pings the address it gets.

## Labels on the network

Every machine pm creates is on `virbr0`, where libvirt's NAT alone would let it
reach the host, the LAN and every other machine. The machine's label decides
what it reaches instead:

| Machine | Reaches | Does not reach |
| --- | --- | --- |
| Red | the host's DHCP and DNS, the internet | the host's other services, the LAN, LosOS boxes, any other machine |
| Any other label | the host, the LAN, the internet, machines with its own label, the LosOS boxes machine | machines with other labels, red machines |
| No label (made outside pm) | the same, with the other machines without a label | labelled machines |
| The LosOS boxes machine | every machine but a red one | red machines |

Red is for untrusted work, and red windows also cannot be kept on top. A red
machine still reaches the internet, but whatever runs in it cannot probe the
router, a printer, a LosOS box or the other machines.

`losos-machine-labels` (`nixos/modules/machine-labels.nix`) enforces this with
a bridge-family nftables table, `losos-machine-labels`, that filters between
`virbr0`'s ports. Rules in the host's own tables could not, because machines on
one bridge reach each other without the host routing anything. libvirt runs the
program as a hook whenever a machine starts or stops, with the machine's
definition on stdin. It reads the label from `<metadata>`, or recognises the
LosOS boxes template, and puts the machine's ports into that label's set.

Each port must send frames from its registered MAC before the bridge learns
that address. ARP is accepted only when the sender MAC and IPv4 address match
the port's current libvirt DHCP lease, and lease changes refresh those bindings.
Guest IPv6 neighbour advertisements and non-DAD solicitations are dropped until
the network has an authoritative per-port IPv6 binding. The guest's label also
follows traffic routed through the host, so sending to a peer via
`192.168.122.1` does not get around the label checks.

The table fails closed:

- A port no hook has named, such as a card hot-added to a running machine, is
  blocked until a hook registers it.
- An unknown label is treated as red.
- If the table is missing, the next machine start recreates it.
  `systemctl reload losos-machine-labels` rebuilds it from the running machines.
- A label changed while the machine runs applies from its next start, because
  libvirt runs no hook on a metadata change.

`nixos/tests/machine-labels.nix` checks every row of the table, with network
namespaces standing in for machines of each label and one real machine that
libvirt starts and stops.

## A machine's windows

A machine has no screen. Its windows reach the host's desktop over
virtio-vsock, and the desktop identifies the machine by where the connection
came in, never by anything the guest says:

1. In the guest, the app runs under `waypipe --vsock -s 2:7350 server`, which
   carries its Wayland connection to the host (vsock address 2), port 7350.
2. The desktop session listens on that port. The kernel stamps each connection
   with the sender's vsock address, and the session asks libvirt which machine
   has that address now.
3. The session hands the connection to that machine's `waypipe client`, which
   connects to a Wayland socket the compositor opened for that machine alone,
   under `$XDG_RUNTIME_DIR/derisk/machines`.
4. The compositor tags every client of that socket with the machine's name,
   through the same security-context mechanism as Flatpak's sandbox. The
   machine's windows get none of the privileged protocols, such as reading
   other windows, placing themselves over them, or synthetic input.
5. Windows have no title bars. While a machine's window has focus, derisk
   fills the top bar with the machine's colour and puts its name before the
   title. Each of the machine's windows has a border in that colour, focused or
   not, and the top bar shows a dot in that colour for each running machine.

To start an app, derisk asks libvirt to run the guest's `losos-guest-run` with
the app's desktop file ID through the QEMU guest agent. That starts the app as
the guest's user under waypipe.

## On the desktop

Machines are one step away wherever the desktop starts things:

- The command palette offers "Start work" for a stopped machine, "Terminal in
  work" for each of its apps, and "Shut down work" while it runs. A stopped
  machine's apps come from the list it last reported, or from its
  [template's app list](#the-guest-template) if it never started. Picking one
  starts the machine and opens the app once the guest agent answers. The label
  colour is a search word, so "blue" finds the machine whose windows are blue.
  The rows come from a palette plugin fed by the session itself, and no other
  connection to derisk can impersonate it.
- The overview has a Machines card with the number of running machines, a
  start or shut-down button for each, and a link to the Machines app.
- With **Open apps in work** ticked in the Machines app, apps you start from
  the app grid or palette open in that machine, by the same desktop file ID,
  starting it first if needed. derisk's core apps, such as Files and Settings,
  stay on the host, and so does everything once the machine is removed. An app
  the machine lacks opens nowhere, and the session log says why.
- A machine with a screen, such as Windows or one installing from a CD, gets
  its screen in a virt-viewer window framed in its colour. Closing the window
  hides the screen until the machine restarts or you pick "Show win's screen"
  in the palette.
- **Keep on Top**, in a window's menu and the palette, works for any window
  except a red machine's. A red window that asks to be raised gets attention
  instead of focus, so untrusted work cannot cover a window to imitate it.

## The guest template

The image ships three templates under `share/losos/machines` on
`XDG_DATA_DIRS`, inside the verity-protected `/usr`:

- `losos-guest.qcow2`, the LosOS guest, for your own apps.
- `losos-boxes.qcow2`, the [LosOS boxes machine](losos-boxes.md#the-losos-boxes-machine),
  the one machine that talks to LosOS boxes.
- `losos-flatpak.qcow2`, [the Flatpak machine](#the-flatpak-machine).

All three are the machine in `nixos/guest/common.nix` with different apps:
`nixos/guest/default.nix` adds the Calculator, `nixos/guest/boxes.nix` adds
`losos` and box discovery, and `nixos/guest/flatpak.nix` adds Flatpak and
Flathub. Next to each template is `<name>.apps`, written at build time with
what `losos-guest-run --list` prints in that guest, so the desktop can offer a
machine's apps before it has ever started.

`nixos/modules/machines.nix` builds each guest with the image's package set and
turns it into a template. The templates are the option `losos.machines.guests`,
one NixOS module per template name. A part of the OS that needs its own machine
adds one there, as `nixos/modules/losos-boxes.nix` does. An update brings new
templates, and existing machines keep the old ones.

A part of the OS can also have the host create its machine with
`losos.machines.system.<name> = { template; label; }`, plus `memory` and
`disk` when pm's defaults are too small. `losos-system-machines` creates each
one once at boot and records it under `/var/lib/losos-machines`, so a machine
you removed in the Machines app stays removed. pm runs its libvirt plugin only
when a key its user trusts has signed it, so the unit keeps its own key there,
generated at the computer's first boot and never shipped in the image, and
signs the image's plugin with it at every start.

### The LosOS guest

The guest boots its UKI from the ESP's fallback path, with no boot loader. Its
root is one ext4 partition holding the Nix store and state, as small as the
store allows. The disk is as large as `pm machine create --disk` says, 32 GiB
by default, and systemd-repart in the initrd grows the root over it on first
boot. The qcow2 holds only what the build wrote: 0.96 GB for the reduced build
described below, more with all apps. The guest has:

- The QEMU guest agent, through which the host starts apps.
- One account, `user`, with no password and no login. Its systemd manager runs
  from boot through lingering, and its apps run under it.
  `nixos/modules/home-folders.nix` creates the XDG base and user directories,
  `user-dirs.dirs`, and the fonts, icons and derisk folders that Danube shows.
- A waypipe server in that manager:
  `waypipe --vsock --socket 2:7350 --no-gpu --display wayland-0 server`. Apps
  find it as `WAYLAND_DISPLAY=wayland-0`, and each connection becomes a vsock
  stream to the host on port 7350. There is no GPU, so apps draw with Mesa's
  llvmpipe and lavapipe into shared memory.
- xwayland-satellite, an X server at `DISPLAY=:0` that turns X11 windows into
  Wayland ones, for X11-only apps such as
  [the Slovak eID klient](tokens.md#the-slovak-eid).
- The host's security keys and smart card readers, lent by
  [losos-tokens](tokens.md).
- Apps: `derisk-terminal` (drawing in software), Danube, and derisk's
  Calculator, the one derisk core app that is its own window (`derisk-gpui`).
  Flatpak lives in [its own machine](#the-flatpak-machine) instead.
- NetworkManager with NixOS's defaults, taking an address from `default`.
  resolved answers DNS and asks the host for `.local` names
  ([Names on the network](#names-on-the-network)).

The host runs `losos-guest-run` in the guest through the agent:

- `losos-guest-run --list` prints one line per app: its desktop file ID, a tab,
  and its name. It reads the user's desktop files, then the user's and the
  system's Flatpak exports, then the system's. It skips hidden entries,
  terminal apps and non-applications.
- `losos-guest-run <desktop file ID>` runs the app's `Exec=` line, without field
  codes, in a login shell as a transient unit of the user's manager
  (`systemd-run --user --machine=user@.host`). It returns once the app starts.

A reduced guest, without Danube, Flatpak and the Calculator, was booted from
the template under libvirt with TCG, no KVM, no network and no vsock device. It
answered the agent within two minutes, with its root grown from 3.3 GiB to fill
an 8 GiB disk. `--list` showed foot's three desktop entries, and
`losos-guest-run foot` started foot, which connected to the waypipe server.
waypipe then failed to reach the host, because there was no vsock device.

## The Flatpak machine

On a PC, Flatpak apps run in their own purple-labelled machine, `flatpak`, and
none of Flatpak is on the host. `nixos/modules/flatpak-machine.nix` adds the
`losos-flatpak` template from `nixos/guest/flatpak.nix`, the same `flatpak.nix`
a phone runs on its host, and has the host create the machine at first boot
through `losos.machines.system`. Its user may install Flatpaks system-wide
without a prompt. It has no store of its own: the host's [Store](store.md)
installs into it. Each Flatpak's sandbox still runs inside the machine, so apps
stay apart from each other, and the machine keeps all of them apart from the
host.

- On the desktop, each installed app shows as "Maps in flatpak" in the palette
  and app grid, framed in purple. Apps are offered while the machine is off,
  from the last list it gave, and opening one starts it.
- The host's Store installs, removes and updates apps through the guest's
  `losos-guest-flatpak`, which derisk calls over the guest agent.
  `store run list|remote-ls ...` answers as the guest's user.
  `store start install|uninstall|update ...` starts `flatpak` in a unit of the
  user's manager and returns at once, and `store progress` returns the output
  so far and, at the end, the exit status. The Store polls it a few times a
  second for its progress bar, because the agent hands over output only after a
  program exits. Flathub's catalogue and icons are too big for that output, so
  the Store reads them from the machine's disk with the agent's file calls in
  4 MiB pieces.
- The **Privacy** page in Settings lists the machine's Flatpak apps and changes
  their permissions, again through `losos-guest-flatpak`: `apps` reads each
  app's metadata and the user's overrides, and `overrides <app>` writes them
  from stdin. `flatpak permission-show`, `permission-set` and
  `permission-remove` run as the guest's user, whose session holds the portal
  permission store. While the machine is off, the page offers to start it and
  reads nothing.
- Apps that were on the host move in once. At the first boot with the machine,
  `losos-flatpak-move` moves `/var/lib/flatpak` into the machine and deletes
  it. At each administrator's next sign-in, a user service moves their
  `~/.local/share/flatpak` in and deletes it, and copies `~/.var/app` (app
  data) to the machine's user, leaving the host copy for you to delete. Each
  move runs `losos-flatpak-move-host`, which packs the files into an ext4 disk
  in the `default` pool and attaches it read-only for one boot with the serial
  `losos-flatpak-move`. The guest's `losos-flatpak-move` copies the files into
  place, then the host shuts the machine down and deletes the disk. The machine
  must be off, because the disk is added to its definition, so a move that
  finds it running waits for the next boot or sign-in.

## Peripherals

USB controllers, Bluetooth radios and the devices plugged into them don't
belong to the host either. sys-usb, sys-bluetooth and a disposable machine per
device drive them, and the host keeps the keyboard, pointer and security keys.
The Machines app hides these machines. [Peripherals](peripherals.md) describes
them.

## Machines from an installer CD

A machine can also start empty and install a system from a CD, like a PC:

```sh
pm machine create win --installer Win11_24H2_English_x64.iso --system windows --label red
pm machine create fedora --installer Fedora-Workstation-Live-43.iso --label green
```

Each CD becomes a read-only volume in the `default` pool, shared by every
machine given the same file. The disk starts empty. The machine boots its disk
first, and the first CD while the disk is empty, so the installer runs once and
the installed system starts after it. Such a machine has a SPICE screen
attached through libvirt, never on a port, and with no clipboard channel. It
also has a USB tablet, and its CDs are on SATA, which every installer supports.
The desktop shows the screen in its own window
([On the desktop](#on-the-desktop)). Outside a session, run
`virt-viewer --attach -c qemu:///system win`.

pm also attaches the image's guest tools CD for the machine's system, from
`share/pm/machines/<system>-tools.iso`. `nixos/modules/machines.nix` builds it
from `nixos/guest-tools/`.

### Windows

A Windows machine gets what Windows 11 expects of a PC: UEFI, a TPM 2.0 from
swtpm (kept and deleted with the machine), Hyper-V enlightenments when the host
has KVM, and a clock in local time. It gets 4096 MiB and 64 GiB unless told
otherwise, and no vsock. It is x86-64 only.

The Windows tools CD carries virtio's drivers and `autounattend.xml`, which
Windows Setup reads from the root of any drive:

1. Setup loads `viostor` and `NetKVM` for Windows 10 and 11 from the CD and
   adds them to the installed Windows. Without them it sees no disk.
2. It skips Windows 11's Secure Boot, TPM, memory and processor checks. The
   machine boots without Secure Boot, because its firmware has no Microsoft
   keys.
3. It wipes the machine's disk and lays it out as Windows does on a UEFI PC.
4. It offers a local account as well as a Microsoft one.
5. At first sign-in, `losos\setup.cmd` from the CD installs virtio-win's guest
   tools: the rest of virtio's drivers and the QEMU guest agent.

Setup still asks for the language, the product key or edition, and the
account. Answer the firmware's "Press any key to boot from CD" on the machine's
screen. If you miss it, the machine stops at the firmware boot menu, and
starting it again offers it again.

The Windows machine shows its whole desktop in virt-viewer. To run Windows apps
in their own windows, use [Windows app support](windows-apps.md), which
installs its own Windows in a rootless container. WinApps, which used to open a
Windows machine's apps, went away with that change.

### Other distributions

Most installers already have virtio drivers. To fit the installed system to
this desktop, run the Linux tools CD's `setup.sh` once, as root:

```sh
sudo sh /run/media/$USER/LOSOS_TOOLS/setup.sh
```

The script:

- Keeps the virtio disk, network and vsock drivers in the initramfs, through
  dracut, initramfs-tools or mkinitcpio.
- Installs and starts the QEMU guest agent and waypipe with apt, dnf, zypper or
  pacman.
- Runs a waypipe server over vsock for the user who ran it, as the LosOS guest
  does.
- Installs `losos-guest-run`.

After that the desktop lists and opens the machine's apps like a LosOS guest's,
framed in its colour. The socket is `losos-wayland`, not `wayland-0`, so a
desktop running on the machine's own screen keeps its socket.

> [!NOTE]
> The machines VM test (`nixos/tests/machines.nix`), under libvirt without KVM,
> defines a Windows machine from a stand-in CD with the drivers CD, a TPM and a
> screen, starts it with its swtpm, and removes it while keeping both CDs for
> other machines. Nobody has installed Windows or another distribution from
> these CDs yet.
