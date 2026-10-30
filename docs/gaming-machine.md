# The gaming machine

The gaming machine is a Windows machine with a graphics card of its own. A
game in it draws on that card at full speed, while the desktop draws on
the PC's other card, or on the processor when there is no other card. It
is meant for a PC with two graphics cards. Usually that is the graphics
built into the processor and a separate card in a PCIe slot. It runs on
x86-64 only, and it needs an IOMMU (Intel VT-d or AMD-Vi) turned on in
the firmware.

`losos-gaming` (`src/losos-gaming`) does the work, and
`nixos/modules/gaming.nix` sets it up.

## Choosing the card

`losos-gaming gpus` lists the PC's graphics cards. For each one it says
whether the card can go to the machine, or why it stays:

```console
$ losos-gaming gpus
0000:00:02.0 8086:a780 i915 — the firmware's screen is on it
0000:01:00.0 1002:744c amdgpu — can go, with 2 function(s)
```

A card goes whole: the GPU and every other function on its slot. On most
cards that means the HDMI and DisplayPort sound, and on some a USB-C
controller too. Those functions share the card's IOMMU group, so they
share its memory access. A card whose IOMMU group holds anything else,
apart from PCI bridges, stays on the host, because that device would have
to go with it. Moving the card to another slot can put it in a group of
its own. A card the [peripherals](peripherals.md) router keeps for the
host also stays.

To reserve a card, an administrator runs:

```console
$ pm system start losos-gaming-reserve@0000:01:00.0
```

`losos-gaming-reserve@none` gives the card back. Either change takes
effect at the next boot. The choice is kept in `/var/lib/losos-gaming/gpu`.

> [!IMPORTANT]
> Plug your screen into the outputs the desktop keeps: the motherboard's,
> when the processor has graphics. Once the card is reserved, the host
> never draws on it, so a screen plugged into it shows the machine or
> nothing.

## What happens at boot

`losos-gaming-bind.service` runs early, before udev's coldplug loads
drivers. It sets each of the card's functions to vfio-pci and writes them
to `/run/losos-gaming/functions`. No host driver ever drives the card,
and no program that opens every GPU it finds can hold it.

The card is not handed over and back while the PC runs. Taking a card
from a driver that has used it is what leaves some cards in a state only
a reboot clears.

If the card cannot go, for example because the IOMMU is off this boot,
the unit logs why, the card stays on the host, and the boot carries on.
`losos-gaming status` shows what happened.

The desktop finds its own card by itself. mcsapi's compositor draws on the
card the firmware started on, unless that card is on vfio-pci. Then it
draws on the next card on a bus. With no card left, it draws with Mesa's
llvmpipe on the processor. The compositor redraws only what changed on
screen, so a still desktop costs nothing and a moving pointer costs two
small copies.

## Making the machine

The person brings the Windows installation ISO, for example from
Microsoft's download page. An administrator runs:

```console
$ losos-gaming create --cd ~/Downloads/Win11_English_x64.iso
$ losos-gaming start
```

`create` copies the ISO into libvirt's `default` pool, makes a 256 GiB
disk there (`--disk-gib` changes the size), and defines the machine
`gaming`. While Windows installs, the machine also has an emulated
screen. virt-viewer, or the Machines app, shows it.

Once Windows is installed:

1. Install the card's driver in Windows, from AMD, NVIDIA or Intel.
2. Install [Looking Glass](https://looking-glass.io)'s host program in
   Windows. Its version must match `looking-glass-client --version` on
   the host.
3. Shut Windows down and run `losos-gaming ready`. This takes out the
   CD and the emulated screen.

From then on, `losos-gaming start` starts the machine and
`losos-gaming show` opens it in a window on the desktop. Right Ctrl
releases the keyboard and pointer back to the desktop. While that window
is open, [focus mode](focus-mode.md) holds the host's background work.

## Two ways to see it

| Screen | How the picture comes | Keyboard and pointer |
| --- | --- | --- |
| `looking-glass`, the default | The card draws each frame into memory shared with the host, `/dev/shm/looking-glass`, and Looking Glass's client shows it in a window | through SPICE, from the client's window |
| `monitor` | A screen plugged into the card | every keyboard and pointer plugged in, through evdev; both Ctrl keys at once switch them between the machine and the desktop |

`losos-gaming screen monitor` or `losos-gaming screen looking-glass`
switches between them. A game pad plugged in when the machine starts is
handed to it over USB.

With Looking Glass, the host's other card, or its processor, shows every
frame the game draws. On the processor alone that costs a few cores at
1080p and 60 frames a second. A screen plugged into the card costs the
host nothing.

## How it looks to Windows

The machine is set up to look like the PC it runs on:

| Setting | What it hides |
| --- | --- |
| `<kvm><hidden state='on'/>` | KVM's signature in the processor's CPUID |
| Hyper-V `vendor_id` `LososGaming` | KVM's name in Hyper-V's interface, which some GPU drivers once refused |
| `<smbios mode='host'/>` | QEMU's maker, model and serial in the firmware tables; the PC's own are there instead |
| `host-passthrough` CPU, `migratable='off'`, the host's cache layout | QEMU's own processor model |
| SATA disk, Intel e1000e network card, Intel HDA sound, no memory balloon | virtio devices, whose Red Hat vendor ID names the hypervisor |
| Secure Boot with Microsoft's keys, a TPM 2.0 | a missing TPM or Secure Boot, which Windows 11 and several anti-cheats require |

Windows still knows it runs under a hypervisor. The processor's
hypervisor bit stays set, because without it Windows drops the Hyper-V
interfaces that make it fast under KVM. Windows sets the same bit on a
bare PC that uses virtualisation-based security, so many games accept it.
Device names such as "QEMU HARDDISK" also remain.

> [!WARNING]
> This is for playing your own games on your own hardware. It does not
> help anyone cheat, and it does not promise that every game runs. Some
> anti-cheat systems, such as Riot Vanguard and FACEIT's, refuse to run
> in any virtual machine they detect. Others allow it, and some have
> banned players for it. Whether playing in a virtual machine is allowed
> is up to each game's terms of service. Read them first. Playing there
> is your risk under those terms.

## Reference

| | |
| --- | --- |
| `losos-gaming gpus [--json]` | the cards, which can go, and the suggestion; `--json` is what Settings reads |
| `losos-gaming reserve <address>` | records the choice, or with `none` drops it; root, through `losos-gaming-reserve@.service` |
| `losos-gaming bind` | moves the reserved card to vfio-pci; root, at boot |
| `losos-gaming create --cd <iso>` | defines the machine; `--cd` again for more CDs, `--disk-gib <n>` for the disk's size, `--screen monitor` for a screen on the card |
| `losos-gaming start` | defines it again with the inputs and game pads plugged in now, and starts it |
| `losos-gaming ready` | takes out the installer's CD and emulated screen |
| `losos-gaming screen <screen>` | `looking-glass` or `monitor`: where the picture goes |
| `losos-gaming show` | opens Looking Glass's window |
| `losos-gaming status` | what is reserved, what moved this boot, and the machine's state |

The machine has half the host's memory, at most 32 GiB. It gets every
processor core but one, or two on a processor with more than eight cores,
with the host's threads per core.
