# Hardware and NVIDIA

The image is one build for every PC. It carries every driver nixpkgs' kernel
builds, all of linux-firmware, and services that some machines have no use
for. For nearly every driver that is all it takes. The kernel names a module
for each device it finds, udev loads it, and a driver for hardware the
machine lacks is never loaded. udev cannot choose between two drivers that
both claim a device, though, and it cannot start a service only where its
hardware is. NVIDIA's GPUs are the first case. nouveau and NVIDIA's own open
kernel module both claim every NVIDIA display controller, and which one
should drive a card depends on the card. fprintd is the second case, since it
is only worth starting when there is a fingerprint reader.

## Choosing at boot from facter's report

NixOS handles per-machine hardware with `hardware.facter`, which reads a
[nixos-facter](https://github.com/nix-community/nixos-facter) report while the
configuration is evaluated. That needs a build per machine. This OS is a
signed image that every machine shares, with no Nix on it to build anything.
So the image reads the same report on the machine at boot and acts on it
with files that kmod and systemd already read from `/run`:

1. `losos-hardware.service` runs early in every boot, before udev's coldplug
   and `systemd-modules-load`. It runs `nixos-facter --hardware pci,usb,cpu`,
   which reads sysfs in well under a second, and writes the result to
   `/run/losos/hardware/facter.json`.
2. `losos-hardware plan`, from `src/losos-hardware`, matches the report
   against the rules the configuration defines in `losos.hardware.rules`.
   Those are set in `nixos/modules/hardware.nix` and readable at
   `/etc/losos/hardware-rules.json`. A rule can match on a device, by facter
   class, vendor and optionally a list of IDs, on the hypervisor facter
   detected, on the CPU's vendor, or on several of these at once. It then
   says what to do: modules to load, modules to keep off the device, a
   fallback, and flags.
3. For every match it writes four things:
   - `blacklist` lines in `/run/modprobe.d/losos-hardware.conf`, so coldplug
     does not load a displaced driver;
   - the wanted modules in `/run/modules-load.d/losos-hardware.conf`, which
     `systemd-modules-load` loads;
   - a file per flag under `/run/losos/hardware/flags/`, for units to test
     with `ConditionPathExists=`;
   - `/run/losos/hardware/plan.json`, which records what matched which
     device.

   A rule that does not match blacklists the modules it reserves, because
   only that rule may load them. That blacklist cannot live in the image's
   `/etc`. `systemd-modules-load` honours blacklists even for modules it is
   asked to load by name, so nothing could ever load them. If the probe
   fails, the plan is made from an empty report, which matches nothing and
   still blacklists the reserved modules.
4. `losos-hardware-fallback.service` runs after `systemd-modules-load`. If a
   rule's module did not load, it loads the rule's fallback by name. A driver
   that refuses a card then leaves the machine with the driver it would have
   had without the rule, instead of none.

This runs on every boot, not once at install. A card swapped in gets its
driver on the first boot it is there, and no stored report has to be kept in
step with the hardware.

> [!NOTE]
> A USB device plugged in after boot counts from the next boot.

## Services that follow the hardware

Every machine runs the same verified `/usr`, so the image cannot install a
package on one machine and not another. It can leave a service idle,
though. A service that only makes sense on some hardware ships in every
image and tests its rule's flag with `ConditionPathExists=`:

| Rule | Matches | Flag | Turns on |
| --- | --- | --- | --- |
| `nvidia-open` | an NVIDIA GPU the driver supports | `nvidia` | NVIDIA's modules, the tmpfs for video memory over suspend |
| `fingerprint` | a reader in libfprint's list, under facter's `fingerprint`, `usb` or `unknown` | `fingerprint` | fprintd |
| `intel-thermald` | an Intel CPU, on bare metal | `intel-cpu` | thermald (x86_64 only) |

fprintd starts over D-Bus, from `pam_fprintd` and from derisk's reader
prompt. On a machine without a reader, systemd refuses that start at once
and both go on to the password. The reader list is the one NixOS's
`hardware.facter` keeps, generated from the same nixpkgs' libfprint.
thermald keeps an Intel CPU under its thermal limits with gentler steps than
the firmware's emergency throttling.

Bluetooth and power profiles are not in the table, because the image leaves
them off everywhere for now. derisk has no UI for either yet, one of the
things [not done](not-done.md). When it does, each one becomes a rule
and a condition here. Phones use a different image, [Halium](halium.md), so
nothing here decides between a PC and a phone.

## NVIDIA

`nixos/modules/nvidia.nix` puts NVIDIA's current production driver in the
image. That is the open kernel module, its GSP firmware and the userspace
libraries. It leaves out OpenCL, OptiX, the CUDA debugger, Vulkan SC, the
Wine DLLs and the Xorg GLX module, which nothing in the session loads, about
210 of 784 MB. It does everything NixOS's `hardware.nvidia` module does
except the two things that assume a particular machine. That module
blacklists nouveau and loads `nvidia_uvm` at every boot. Here the rule
reserves NVIDIA's modules, so on every machine where it does not match, they
are blacklisted for that boot and udev leaves every NVIDIA card to nouveau.

The rule `nvidia-open` matches a `graphics_card` from vendor `0x10de` whose
device ID is in NVIDIA's table of supported GPUs. NVIDIA ships that table
inside its installer, and the build cuts it to the chips the current branch
supports. That branch supports Turing (GTX 16xx, RTX 20xx) and everything
newer, all on the open module. On a match, nouveau is blacklisted, `nvidia`,
`nvidia_modeset`, `nvidia_drm` and `nvidia_uvm` are loaded, and nouveau is
the fallback.

| Card | Driver | Vulkan |
| --- | --- | --- |
| Turing and newer (GTX 16xx, RTX 20xx and later) | NVIDIA's open module | NVIDIA's |
| Maxwell, Pascal, Volta (GTX 7xx/9xx/10xx, Titan V) | nouveau | Mesa's NVK |
| Older | nouveau | none |
| A card newer than the image's driver | nouveau | Mesa's NVK |

Maxwell to Volta are in NVIDIA's 580 legacy branch, and the image does not
offer them its closed module. In a laptop with an Intel or AMD GPU beside the
NVIDIA one, the rule matches only the NVIDIA card. The other GPU keeps its
driver and stays the boot display, which derisk's compositor draws on. The
NVIDIA GPU powers down while unused, because the udev rule in `nvidia.nix`
lets the kernel suspend it.

`nvidia_drm` runs with `modeset=1 fbdev=1`, which the compositor's GBM scanout
needs. Suspend uses the driver's kernel suspend notifiers. The driver saves
the card's used memory, including the session's framebuffers, to a file in
`/run/nvidia-suspend`. That is a tmpfs, mounted only on machines the rule
matched. It sits in RAM, which suspend keeps powered, instead of on the root
partition, which is [not encrypted](layout.md). VA-API decodes on the card
through `nvidia-vaapi-driver`. Flatpak apps do not use these libraries. They
take Flathub's NVIDIA GL extension of the same driver version.

## The compositor on NVIDIA

derisk's compositor, mcsapi's KMS backend, opens the seat's boot GPU, makes a
GBM device on it, and renders with EGL on that device. On NVIDIA, Mesa's
`libgbm` loads NVIDIA's backend,
`/run/opengl-driver/lib/gbm/nvidia-drm_gbm.so`, and NVIDIA's EGL takes the GBM
platform through `egl-gbm`. Each frame is queued for scanout with the fence of
its rendering, so the display never flips to a buffer NVIDIA has not finished
drawing. Clients share GPU buffers through `zwp_linux_dmabuf_v1`. The
compositor also offers `wp_linux_drm_syncobj_v1`, explicit sync, when the
GPU's DRM driver supports syncobj eventfds, which NVIDIA's does from 560 on.
NVIDIA does not do implicit sync, so without explicit sync a client's frame
could appear before NVIDIA finished drawing it.

Scanout is triple buffered. While one frame waits for its page flip, the
compositor may draw the next one and queue it behind, and the flip after
that shows it. The compositor draws on a timer at the display's refresh
rate, not at each vblank. With only one frame allowed past the screen, a
tick that landed just before a vblank used to find the flip still pending,
and that frame was dropped. The cost of the third buffer is up to one frame
of latency while the GPU keeps ahead.

## What is not tested

> [!NOTE]
> There is no NVIDIA card where this was built. `nixos/tests/hardware.nix`
> boots a VM and checks the probe runs before coldplug and module loading, that
> NVIDIA's modules are installed and that udev would load nouveau, not them,
> for an RTX 4090's ID; then it hands `losos-hardware` a report with that card
> and checks nouveau is blacklisted, NVIDIA's module is loaded and, finding no
> GPU, refuses, and the fallback loads nouveau. A real card accepting the
> module, the compositor on it, suspend, VA-API and
> hardened_malloc under NVIDIA's libraries are untested ([What is not
> done](not-done.md)).
