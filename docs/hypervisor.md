# Virtual machine desktop

On a PC, LosOS Desktop is a hypervisor with a desktop on top, in the mould
of [Qubes OS](https://www.qubes-os.org). Linux with KVM runs the machines.
The kernel schedules each guest straight onto the processor's virtualisation
extensions, with no other operating system under it, which makes it a type-1
hypervisor the way Xen is under Qubes. The host keeps only what it needs to
drive the hardware and the desktop: the kernel, systemd, libvirt, derisk and
the host's own settings. The apps a person works in are meant to run in
machines. Each machine has a colour label, and its windows on the desktop
carry that colour. This page describes what is built so far: the host, the
machine templates it ships, and how the desktop shows and runs machines.

[libvirt](https://libvirt.org) is the one control plane. Nothing in the OS
starts QEMU itself or keeps its own list of machines. pm, virsh and
virt-manager all talk to the same daemon, `qemu:///system`, and see the same
machines.

> [!NOTE]
> The GSI does not do any of this. Most phones' kernels have no KVM, so apps
> there keep running on the host ([Halium GSI](halium.md)).

## The host

`nixos/modules/libvirt.nix` makes the PC the host:

- **libvirtd runs from boot**, with QEMU for the PC's own architecture and
  KVM where the processor has it. A PC without hardware virtualisation gets
  the same image. libvirt reports that its QEMU emulates instead of running
  on KVM, and machines run, slowly.
- **`LIBVIRT_DEFAULT_URI` is `qemu:///system`** for every session, so
  `virsh`, pm and virt-manager reach the machines without being told where
  they are. A per-user `qemu:///session` would see none of them. It also
  could not give a machine the vsock its windows travel over, because
  `/dev/vhost-vsock` belongs to root.
- **The `default` storage pool and network exist and start with the host.**
  The pool is a directory, `/var/lib/libvirt/images`. The network is
  libvirt's own NAT on `virbr0`, `192.168.122.0/24`. A oneshot unit,
  `libvirt-defaults`, defines and starts them through libvirt's API at boot,
  so a change made to them in virt-manager stays. The firewall accepts only
  DHCP and DNS from the machines on `virbr0`, and libvirt's own nftables
  table masquerades their traffic out.
- **`vhost_vsock` is loaded at boot.** It is the host's end of each
  machine's virtio-vsock device, and the desktop listens on that transport
  for the machines' windows.
- **waypipe is installed.** The desktop runs `waypipe client` for each
  running machine to turn what comes out of the guest back into Wayland.
- **swtpm and virt-viewer are installed** for
  [machines that install from a CD](#machines-from-an-installer-cd). swtpm
  is the TPM libvirt gives a Windows machine, and virt-viewer shows a
  machine's own screen. Windows apps in their own windows come from
  [Windows app support](windows-apps.md), which runs its own Windows.
- **Administrators manage machines without being asked.** polkit lets the
  `wheel` group, which first-boot setup puts the first account in, use
  `org.libvirt.unix.manage` outright. Handing a machine a host disk is an
  administrator's power anyway, and the desktop, pm and virt-manager connect
  many times a session. polkit's agent asks everyone else for an
  administrator's password. A read-only connection is open to every
  account, so every session can frame machines' windows. It shows which
  machines exist and run, their labels, and which vsock address belongs to
  which machine.

All of this is state under `/var/lib/libvirt` and `/run/libvirt` on the root
partition. The verity `/usr` and the [update model](systemd.md) do not
change.

## Machines

pm makes them with `pm machine create`, as
[pm on LosOS](pm.md#machines-that-stay) describes. Its libvirt plugin is the
only part of pm that knows libvirt exists. In libvirt, a machine is three
things:

- its **template**, a read-only volume in the `default` pool that every
  machine made from it shares;
- its **disk**, a qcow2 overlay on the template in the same pool, which holds
  only what the machine has changed;
- its **definition**, a persistent domain with UEFI, a virtio disk and
  network card on `default`, a channel for the QEMU guest agent, and a vsock
  device whose address libvirt picks. It has no graphics card and no USB
  controller. Its `<metadata>` holds the machine's label and template.

derisk's Machines app lists them. It makes new ones from the templates under
`share/losos/machines` through `pm machine create`, and starts, shuts down
and removes them. For a running machine it also lists the machine's apps
and opens the one picked.

## Names on the network

A machine looks up `.local` names through the host instead of multicasting
for them itself. libvirt's NAT carries no multicast between `virbr0` and the
host's own networks. An mDNS query sent from a machine would reach only the
other machines on `virbr0`, and never a LosOS box, printer or other PC on the
host's LAN. The host is on both networks, so it does the lookup:

1. The guest's resolved does no mDNS or LLMNR. NetworkManager's profile for
   its link on `default` gives that link `~local` as a routing domain, through
   `ipv4.dns-search`. A `.local` lookup then goes to the DNS server DHCP gave
   it, which is the host at `192.168.122.1`. Without that routing domain,
   resolved never sends a `.local` name to a unicast server.
2. libvirt's dnsmasq on `virbr0` answers it from the host's own resolver,
   `/etc/resolv.conf`, which is resolved's stub.
3. resolved on the host asks every link it does mDNS on, the LAN and
   `virbr0` alike, and answers with the address it found.

The machine then reaches that address through libvirt's NAT like any other. A
LosOS box's name is the exception while a LosOS boxes machine runs. dnsmasq
answers with that machine's address, and the connection goes to the box
through it, as described in [from the other
machines](losos-boxes.md#from-the-other-machines). Since nothing in the
machine multicasts, it also neither answers nor announces on `virbr0`, and the
host's answer is the only one. A machine installed from a CD keeps whatever
its own system does, which for most distributions is Avahi multicasting on
`virbr0` alone.

`nixos/tests/machines.nix` checks this. A machine made from the template
looks up `lanbox.local`, a test machine on the host's LAN that answers only
over mDNS, gets that machine's LAN address, and pings it.

## A machine's windows

A machine has no screen. Its apps' windows come to the host's desktop over
virtio-vsock instead. The desktop knows which machine each window is from by
where the connection came in, never by anything the guest says:

1. In the guest, an app runs under `waypipe --vsock -s 2:7350 server`, which
   carries its Wayland connection to the host, vsock address 2, on port 7350.
2. The desktop session listens on that port. The kernel stamps each
   connection with the address libvirt gave the sending machine, and the
   session asks libvirt which machine has that address now.
3. The session hands the connection to that machine's `waypipe client`,
   which connects to a Wayland socket the compositor opened for that machine
   alone, under `$XDG_RUNTIME_DIR/derisk/machines`.
4. The compositor marks every client of that socket with the machine's name,
   through the same security-context mechanism Flatpak's sandbox uses. The
   machine's windows then get none of the privileged protocols a host app may
   use, such as reading other windows, placing itself over them, or
   synthetic input.
5. Windows have no title bars of their own, and the top bar shows the
   focused window's title and buttons. While one of a machine's windows has
   the focus, derisk fills the top bar with the machine's label colour and
   puts the machine's name before the title. It draws a border in that
   colour round each of the machine's windows, focused or not. The top bar
   also shows a dot in that colour for each running machine.

Apps start in a machine through the QEMU guest agent. derisk asks libvirt to
run the guest's `losos-guest-run` with the app's desktop file ID, and that
starts the app as the guest's user under waypipe.

## On the desktop

Besides the Machines app, machines are a step away wherever the desktop
starts things:

- **The command palette** offers "Start work" for each machine that is off,
  "Terminal in work" for each app it has, and "Shut down work" while it
  runs. It offers a stopped machine's apps too. They come from the list the
  machine last gave or, for a machine never started, from its template's app
  list, described [below](#the-guest-template). Picking one starts the
  machine and opens the app once its guest agent answers. A machine's label
  colour is one of its search words, so typing "blue" finds the machine whose
  windows are blue. The rows come from a palette plugin like the others, fed
  by the session itself, and no other connection to derisk can speak for it.
- **The overview** has a Machines card. It shows how many machines are
  running, a start or shut down button for each, and a way into the Machines
  app.
- **Apps can open in a machine by default.** With "Open apps in work" ticked
  in the Machines app, an app started from the app grid or the palette opens
  in that machine instead of on the host, by the same desktop file ID. If the
  machine is off, it starts first. derisk's own core apps, such as Files and
  Settings, stay on the host, and so does everything once that machine has
  been removed. An app the machine does not have opens nowhere, and the
  session's log says why.
- **A machine's own screen gets a window.** While a machine with a screen
  runs, such as one installing from a CD or Windows, the session shows that
  screen in virt-viewer on that machine's socket. The window is framed in
  the machine's colour like its apps' windows. Closing it hides the screen
  until the machine starts again, or until the palette's "Show win's screen"
  brings it back.
- **Keep on Top** is in a window's menu and the palette, for any window
  except one from an untrusted, red machine. Such a window cannot be kept on
  top, and when it asks to be raised it gets attention instead of focus. A
  machine meant for untrusted work therefore cannot cover a window to
  imitate it.

## The guest template

The image ships three templates under `share/losos/machines` on
`XDG_DATA_DIRS`. They are in `/usr` and covered by its verity hash like the
rest of the OS:

- the LosOS guest, `losos-guest.qcow2`, for a person's own apps;
- the [LosOS boxes machine](losos-boxes.md#the-losos-boxes-machine),
  `losos-boxes.qcow2`, the one machine that talks to LosOS boxes;
- [the Flatpak machine](#the-flatpak-machine), `losos-flatpak.qcow2`, where
  Flatpak apps run.

All three are the same machine, `nixos/guest/common.nix`, with different
apps. `nixos/guest/default.nix` adds the Calculator, `nixos/guest/boxes.nix`
adds `losos` and box discovery, and `nixos/guest/flatpak.nix` adds Flatpak
and Flathub. Beside each template is `<name>.apps`, written when the image is
built, which holds what `losos-guest-run --list` prints in that guest. With
it the desktop can offer a machine's apps before the machine has ever
started. `nixos/modules/machines.nix` evaluates each guest with the image's
own package set and turns it into its template. The templates are the option
`losos.machines.guests`, one NixOS module per template name. A part of the
OS that needs its own machine adds one there, the way
`nixos/modules/losos-boxes.nix` adds the LosOS boxes machine. An update
brings new templates, and machines made from the old ones keep the old ones.

A part of the OS that runs in a machine can also have the host make that
machine, with `losos.machines.system.<name> = { template; label; }`, plus
`memory` and `disk` where pm's defaults are not enough.
`losos-system-machines` makes each one once at boot, as `pm machine create`
would. It records the names it made under `/var/lib/losos-machines`, so a
machine its owner removed in the Machines app is not made again. pm runs its
libvirt plugin only when a key its user trusts has signed it. So the unit
keeps its own key there, made on that computer at its first boot and never
shipped in the image, and signs the image's plugin with it at every start.

The rest of this section describes the LosOS guest.

The guest boots its UKI from the ESP's fallback path, with no boot loader.
Its root is one ext4 partition holding the Nix store and its state, made as
small as the store allows. A machine's disk is as large as
`pm machine create --disk` says, 32 GiB by default. On the first boot,
systemd-repart in the initrd grows the root partition over the rest of the
disk, and ext4 grows with it. The qcow2 holds only what the build wrote,
0.96 GB for the reduced build tested below and more with the apps that build
left out. The guest has:

- **the QEMU guest agent**, through which the host starts apps;
- **one account, `user`**, with no password, whose systemd manager runs from
  boot through lingering. Nobody logs in. Its apps run under that manager,
  which makes the folders apps expect in its home, as the host's users'
  managers do. `nixos/modules/home-folders.nix` creates the XDG base and user
  directories with `user-dirs.dirs`, and the fonts, icons and derisk
  folders that Danube shows;
- **a waypipe server** in that manager, `waypipe --vsock --socket 2:7350
  --no-gpu --display wayland-0 server`. Every app the user runs finds it as
  `WAYLAND_DISPLAY=wayland-0`, and each app's connection becomes a vsock
  stream to the host's desktop on port 7350. A machine has no GPU, so apps
  draw with Mesa's llvmpipe and lavapipe into shared memory, which waypipe
  carries;
- **apps**: derisk's Terminal, `derisk-terminal`, which draws in software
  because there is no GPU, Danube, and derisk's Calculator. The Calculator is
  the one derisk core app that is its own window, `derisk-gpui`, and the host's
  compositor draws the rest. Flatpak is not in this guest but in
  [its own machine](#the-flatpak-machine);
- **NetworkManager**, with NixOS's defaults as on the host, taking an
  address from libvirt's `default` network. Apps in the machine ask it
  through GLib or the portal whether they are online. resolved answers DNS
  and asks the host for `.local` names, as
  [above](#names-on-the-network).

`losos-guest-run`, in the guest's system profile, is what the host runs
there through the agent:

- `losos-guest-run --list` prints one line per app the desktop can open,
  with its desktop file ID, a tab and its name. It reads the user's own
  desktop files first, then the user's and the system's Flatpak exports,
  then the system's. It leaves out entries that are hidden, need a terminal
  or are not applications.
- `losos-guest-run <desktop file ID>` starts that app's `Exec=` line,
  without its field codes, in a login shell as a transient unit of the
  user's manager, with `systemd-run --user --machine=user@.host`. It returns
  once the app has started.

A reduced build of the guest, without Danube, Flatpak and the Calculator,
was booted from the template under libvirt with TCG, with no KVM, no network
and no vsock device. It answered the agent within two minutes of starting,
with its root grown from the template's 3.3 GiB to fill an 8 GiB disk.
`--list` listed foot's three desktop entries, and `losos-guest-run foot`
started foot in the user's manager, where it connected to the waypipe
server. waypipe then failed to reach the host, because that machine had no
vsock device.

## The Flatpak machine

On a PC, Flatpak apps run in their own machine, `flatpak`, labelled purple,
and none of Flatpak is on the host. `nixos/modules/flatpak-machine.nix` adds
its template, `losos-flatpak`, from `nixos/guest/flatpak.nix`, and has the
host make the machine at its first boot through `losos.machines.system`,
described above. The template is the LosOS guest's machine with Flatpak and
Flathub, using the same `flatpak.nix` a phone runs on its host
([Halium GSI](halium.md)). Its user may install Flatpaks for the whole
machine without being asked. It has no store of its own, because the host's
[Store](store.md) installs into it. Each Flatpak's own sandbox still runs
inside the machine, so an app is kept apart from the machine's other apps as
before, and the machine keeps all of them apart from the host.

- **On the desktop**, every installed app is that machine's app, shown as
  "Maps in flatpak" in the palette and the app grid and framed in purple.
  The apps are offered while the machine is off too, from the last list it
  gave, and opening one starts the machine.
- **The host's Store** installs, removes and updates apps in it. derisk finds
  the machine by its template and asks its `losos-guest-flatpak` through the
  guest agent. `store run list|remote-ls ...` answers as the guest's user
  would. `store start install|uninstall|update ...` starts `flatpak` in a
  unit of that user's manager and returns at once. `store progress` gives
  back what the job has printed so far and, once it has ended, its exit
  status. The Store reads that a few times a second for its progress bar.
  The agent hands over a program's output only after the program has exited,
  so this is the only way an install can show progress. The Store reads
  Flathub's catalogue and icons from the machine's disk with the agent's file
  calls, in 4 MiB pieces, because they are far larger than the output the
  agent keeps for a program.
- **Settings' Privacy page** lists the Flatpak apps the machine has and
  changes their permissions there. derisk asks the guest's
  `losos-guest-flatpak` through the guest agent. `apps` reads each app's
  metadata and the user's overrides, and `overrides <app>` writes them from
  standard input. `flatpak permission-show`, `permission-set` and
  `permission-remove` run as the guest's user, whose session holds the
  portals' permission store. While the machine is off, the page says so and
  offers to start it, and nothing is read from it until it runs.
- **Apps that were on the host move in once.** A PC that updates from a
  release with Flatpak on the host has its apps where the host kept them. At
  the first boot with the machine, `losos-flatpak-move` moves
  `/var/lib/flatpak`, the apps installed for everyone, into the machine and
  deletes it. At each administrator's next sign-in, a user service moves
  their `~/.local/share/flatpak`, the apps they installed for themselves,
  in and deletes it. It also copies `~/.var/app`, every app's data, to the
  machine's user, and leaves the host's copy for its owner to delete. Each
  move runs `losos-flatpak-move-host`. That makes the files into an ext4
  disk in libvirt's `default` pool and gives it to the machine read-only for
  one boot with the serial `losos-flatpak-move`. The guest's
  `losos-flatpak-move` copies it where it belongs, and then the host shuts
  the machine down and deletes the disk. The machine must be off for this,
  because the disk is added to its definition. A move that finds the machine
  running waits for the next boot or sign-in.

## Peripherals

USB controllers, Bluetooth radios and the devices plugged into them are not
the host's either. sys-usb, sys-bluetooth and a disposable machine for each
other device drive them, and the host keeps its keyboard, pointer and
security keys. These machines are the OS's, so the Machines app does not
show them. [Peripherals](peripherals.md) describes them.

## Machines from an installer CD

A machine can also start empty and install a system from its own CD, as a
PC would:

```sh
pm machine create win --installer Win11_24H2_English_x64.iso --system windows --label red
pm machine create fedora --installer Fedora-Workstation-Live-43.iso --label green
```

Each CD becomes a read-only volume in the `default` pool, shared by every
machine given the same bytes, and the machine's disk starts empty. The
machine boots its disk first, and the first CD while the disk is still empty,
so the installer runs once and the installed system starts after it. Such a
machine has a screen over SPICE, attached through libvirt, never on a port,
and with no clipboard channel. It also has a USB tablet, and its CDs are on
SATA, which every installer has a driver for. The desktop shows it in its own
window, as "On the desktop" describes. Outside a session,
`virt-viewer --attach -c qemu:///system win` shows it.

pm also gives the machine the image's **CD of guest tools** for its system.
The image ships it at `share/pm/machines/<system>-tools.iso`, where pm looks
for it. `nixos/modules/machines.nix` builds it from the sources in
`nixos/guest-tools/`.

### Windows

A Windows machine gets what Windows 11 asks of a PC. It has UEFI, a TPM 2.0
from swtpm, whose state is kept and deleted with the machine, Hyper-V's
enlightenments when the host has KVM, and its clock in local time. It gets
4096 MiB and 64 GiB unless told otherwise. It has no vsock, which Windows has
no use for. It is x86-64 only.

The Windows tools CD carries virtio's drivers and an answer file,
`autounattend.xml`, which Windows Setup reads from the root of any drive it
starts with:

1. Setup loads virtio's disk and network drivers, `viostor` and `NetKVM` for
   Windows 10 and 11, from the CD, and adds them to the Windows it installs.
   Without them it would see no disk to install on.
2. It skips Windows 11's Secure Boot, TPM, memory and processor checks. The
   machine boots without Secure Boot, because its firmware has no Microsoft
   keys to check Windows' boot loader with.
3. It wipes the machine's one disk and lays it out the way Windows lays out a
   UEFI PC.
4. It offers a local account as well as a Microsoft one.
5. At the first sign-in, `losos\setup.cmd` from the CD installs the rest of
   virtio's drivers and the QEMU guest agent, which are virtio-win's guest
   tools, and nothing else.

Setup still asks for the language, the product key or edition, and the
account. The firmware's "Press any key to boot from CD" is for the person to
answer on the machine's screen. If they miss it, the machine stops at the
firmware's boot menu, and starting it again offers it again.

A Windows machine shows its whole desktop in virt-viewer. Running its apps in
their own windows is the job of [Windows app support](windows-apps.md), which
installs its own Windows in a rootless container instead of using a machine.
WinApps used to open a Windows machine's apps once a person had described the
machine to it by hand. It went away with that change.

### Other distributions

Most distributions' installers already have virtio's drivers. What they
lack is what fits them to this desktop, and the Linux tools CD's `setup.sh`
adds it. Run it once in the installed system, as root:

```sh
sudo sh /run/media/$USER/LOSOS_TOOLS/setup.sh
```

The script does four things:

- keeps virtio's disk, network and vsock drivers in the initramfs, through
  dracut, initramfs-tools or mkinitcpio, whichever the distribution has;
- installs and starts the QEMU guest agent and waypipe with the
  distribution's own package manager, apt, dnf, zypper or pacman;
- runs a waypipe server over vsock for the user who ran it, as the LosOS
  guest does;
- installs `losos-guest-run`.

After that the desktop lists and opens the machine's apps like a LosOS
guest's, framed in its colour. Its socket is `losos-wayland`, not
`wayland-0`, so a desktop that the distribution runs on the machine's own
screen keeps its own socket.

> [!NOTE]
> Under libvirt without KVM, in the machines VM test
> (`nixos/tests/machines.nix`), a Windows machine from a stand-in CD is
> defined with the drivers CD, a TPM and a screen, starts with its swtpm, and
> is removed leaving both CDs for other machines. No Windows or other
> distribution has been installed from them yet.
