# pm on LosOS

## Administering the machine

There is no sudo, su or run0. An administrator changes the system with
`pm system`, and pm does the work as its own user, `pm`:

| Command | What it does |
| --- | --- |
| `pm system start\|stop\|restart\|reload <unit>` | the same to a system unit; a bare name is a service |
| `pm system update [--reboot]` | installs the newest version for the next boot (`systemd-sysupdate.service`), and reboots into it if asked and there was one |

That is all it does. It runs no command as root and opens no shell. An
administrator's session, or whatever takes it over, can only start and stop
the units the image already has and install its signed updates. A machine that
needs more, because something is broken, gets a root shell from the boot menu,
which needs someone physically at the machine. See [When something goes
wrong](troubleshooting.md#no-one-can-administer-the-machine).

`pm` asks `pmd --system`, a service that the system bus starts on the first
call. It is `pm-system.service`, set up in `nixos/modules/pm.nix`. The daemon
asks polkit whether the caller is allowed. polkit asks an administrator, a
member of `wheel`, to authenticate as themselves, either in derisk's dialog,
as [polkit and pm](sign-in.md#polkit-and-pm) shows, or on the terminal
through `pkttyagent` when the session has no agent. The password stays valid
for a few minutes. Only then does the daemon act, as `pm` and through
systemd, and the journal records who asked for what.

The `pm` user owns nothing and holds no capability. Everything it may do
comes from one polkit rule, which lets it manage units without asking, except
`debug-shell.service`. That unit is a root shell on tty9 with no password, so
starting it would hand out a root shell by another name. The recovery shell
starts it from the kernel's command line instead, where polkit has no say.

A second rule, evaluated last, refuses unit management to everyone else.
Without it an administrator could still `systemctl restart` a service, or use
the run0 that systemd itself carries, and become root without going through
pm. Root is never asked, so the system's own services and the test driver
are unaffected. Narrower rules placed before it still answer first, such as
the one in `nixos/modules/disk.nix` that lets an administrator ask for a
factory reset from Settings. Everything else polkit governs works as before:
accounts through homed, the network through NetworkManager, updates from
Settings through `sysupdated`, and Flatpak.

The image's boot test, `nixos/tests/boot.nix`, runs `pm system restart` as
root. It checks that the daemon runs as `pm`, that `pm system run` is gone,
and that pm cannot start the debug shell. It also asks polkit what it says to
`pm` and to an administrator about managing units, both directly and through
pm.

## pm's plugins

`nixos/pkgs/pm-plugins.nix` builds eight components: this repository's three
and pm's own `libvirt`, `podman`, `sysext`, `sysupdate` and `systemd`. The
image carries them in `/run/current-system/sw/share/pm/plugins`. pm loads
plugins only from a user's own `~/.config/pm/plugins`, and only when they are
signed by a key that user trusts. So the image offers the plugins but trusts
them for no one:

```
install -Dm644 -t ~/.config/pm/plugins /run/current-system/sw/share/pm/plugins/*.wasm
for p in ~/.config/pm/plugins/*.wasm; do pm sign "$p"; done
```

### With Home Manager

Home Manager runs on a build host, or on any machine with Nix. The image
itself has no Nix, and its users are systemd-homed records. Where Home
Manager does run, the flake's `homeModules.pm` installs pm and its plugins
instead of the copy above:

```nix
{
  imports = [ losos-desktop.homeModules.pm ];
  programs.pm.enable = true;
  # All eight by default; or name the ones you want:
  # programs.pm.plugins = [ "losos-nix" "systemd" ];
}
```

Each plugin is linked into `~/.config/pm/plugins` from the Home Manager
generation. A plugin taken off the list is uninstalled at the next switch,
and a rollback restores the previous set. Activation then runs the same
`pm sign` over each one with your own key, which is made and trusted on first
use as it would be by hand. So you still give the trust, not the module.
`extraPlugins` installs other components by name. `signatures` takes a
publisher's `.sig` for a plugin instead of signing it locally, and
`trustedKeys` adds that publisher's key to pm's trust store, which also
governs build files.

`nix flake check` builds the module into a Home Manager generation and runs
its signing step against it. It then checks that `pm plugins` loads every
plugin with no flags, in `nixos/tests/home-manager-pm.nix`. No
`home-manager switch` on a real account has run it yet.

## Driving the flake from pm

pm stays the system manager here too, so a pm build file can build and check
the flake. pm's fingerprint table would otherwise refuse `nix build`,
`nix flake check`, `nix eval` and the other commands that read or build, by
constraint C2. The `losos-nix` plugin names them, as `plugins/README.md`
describes. Two things about pm's jail shape such a step. `/nix/store` is
mounted read-only and the daemon socket not at all, so the step builds into a
store of its own with `nix --store /build/nix ...`. And a step that says
`--offline` gets no network, so the jail enforces what the step claims.
Without `--offline`, the step is on the host network, and by constraint C8 so
is the whole build file, because evaluating fetches the inputs and building
substitutes from the project's cache. `nixos-rebuild`, `nix profile`,
`nix run` and `nix flake update` are recognised and left unclassified on
purpose. This OS is updated by systemd-sysupdate, never by switching a
generation, and a pin moves by hand.

## nixpkgs in a pm build

All of nixpkgs is reachable from a pm build file, at the revision `flake.lock`
pins, built for the same stock glibc platform as this OS. The flake's
`legacyPackages` is the image's own package set, so `.#ripgrep` is nixpkgs'
ripgrep, the same store path the image would carry.
`.#pm-payloads.ripgrep` is what a pm package can actually hold, built by
`nixos/pkgs/pm-payloads.nix`. It is the same package from `pkgsStatic`, with
every output merged into `usr/bin`, `usr/sbin`, `usr/libexec` and
`usr/share`.

It has to be static because pm extracts a package at `/pkg` and runs it in a
jail whose `/nix/store` is either the host's or nothing, and a dynamic program
names its loader and libraries by store path. The payload sets
`allowedReferences = [ ]`, so Nix itself refuses a payload where any file
still names a store path, and says which file. `gnugrep` is refused that way,
because `egrep` is a script whose shebang is bash's store path. That is the
check doing its job, since such a package could not run from `/pkg`.

A build file that installs one:

```yaml
name: ripgrep
version: ['15', '1', '0']
dependencies: []
steps:
- stage: Build
  dl_urls: null
  name: build
  run:
  - nix --store /build/nix build --out-link /build/payload github:dasmatus/losos-desktop/<commit>#pm-payloads.ripgrep
- stage: Install
  dl_urls: null
  name: stage
  run:
  - find /build/nix/nix/store -maxdepth 1 -type d -name *-pm-payload-ripgrep-* -exec cp -rT {}/usr /dest/usr ;
```

The second step uses `find` instead of `cp /build/payload/usr`, because the
out-link points at `/nix/store/...`. In pm's jail that is the host's store,
not the one at `/build/nix` the step built into. There is no shell, so the
pattern reaches `find` unexpanded.

`losos-nix` holds the flake reference to the same rule as a pin, as
`plugins/README.md` describes. A commit, a path, or a URL with `narHash=` is
classified. `nixpkgs#ripgrep`, `github:NixOS/nixpkgs/nixos-26.05`,
`<nixpkgs>` and anything else that resolves to whatever is newest gets no
verdict, and pm refuses the build file before it runs.

> [!NOTE]
> Untested beyond evaluation and the payload builder itself: both architectures
> evaluate `.#pm-payloads.<name>`, and the builder, run against nixpkgs' own
> cached static packages, produced a static `hello` and refused `gnugrep`. No pm
> build has run the steps above, and CI does not build any payload, so the first
> one of each package compiles its whole static closure.

## Virtual machines

The image runs libvirt from boot, as `nixos/modules/libvirt.nix` sets up and
[Virtual machine desktop](hypervisor.md) describes. It uses QEMU for the
machine's own architecture, with KVM where the CPU has it. virt-manager and
`virsh` use it as on any distribution. `LIBVIRT_DEFAULT_URI` is
`qemu:///system` for every session. Administrators, the `wheel` members that
first-boot setup makes, may manage machines there without being asked, and
everyone else is asked for an administrator's password.

pm boots a package that ships its own kernel, marked `kernel(...)` in its
recipe as pm's README explains, in a virtual machine. On its own, pm starts
QEMU itself, as you. With pm's `libvirt` plugin installed and signed as
above, and it is one of the eight, pm asks libvirt instead:

```
pm run hello.cpkg          # through libvirt, when the plugin is installed
pm run --qemu hello.cpkg   # QEMU directly, as you, asking no plugin
```

The plugin is a WebAssembly component like the rest, and it can reach nothing
by itself. It writes a transient domain definition and names one program,
`virsh`, which `pm plugins` lists under `launchers:`. pm then runs
`virsh create --console --autodestroy` with it. virsh connects where it
always does, to `LIBVIRT_DEFAULT_URI`, which here is `qemu:///system`. There
QEMU runs as libvirt's own user instead of as you, in its own cgroup and
mount namespace with QEMU's seccomp sandbox on. libvirt hands it the kernel,
the initramfs and the two serial sockets for the life of the machine. While
the machine runs, `virsh list` and virt-manager show it as
`pm-vm-<random>`, titled `pm run: <program>`. It is destroyed when virsh's
connection closes, and pm stops virsh when pm dies. So a killed pm takes its
machine with it, and Ctrl-C stops the machine and gives the terminal back.
The guest is the same as under QEMU: the package and the libraries its
programs link, with no network device and no disk. Only x86-64 boots a
package's kernel.

A real boot through the plugin passed on an Ubuntu host with libvirt 10.0. It
ran a distribution kernel with a probe through `qemu:///system`, with QEMU as
libvirt's own user, under TCG. The same boot with pm's own QEMU passed too.
Neither the module nor the plugin has run on this image yet.

### Machines that stay

`pm machine` keeps virtual machines that outlive any one command. These are
the machines of the [virtual machine desktop](hypervisor.md). As with
`pm run`, pm leaves libvirt to its `libvirt` plugin, which turns each request
into the `virsh` steps that carry it out:

```
pm machine create work \
  --image /run/current-system/sw/share/losos/machines/losos-guest.qcow2 \
  --label blue --memory 4096 --cpus 4 --disk 64
pm machine start work
pm machine list
pm machine stop work        # asks the guest to shut down
pm machine kill work        # pulls the plug
pm machine remove work      # the machine and its disk; the template stays
```

The template becomes a read-only volume in libvirt's `default` pool, named
after the template and its digest, so every machine made from one image
shares it. A machine's disk, `pm-machine-<name>.qcow2`, is a qcow2 overlay on
it, as large as `--disk` says and never smaller than the template. Its
definition is a persistent domain that carries its label and template in
`<metadata>`, a vsock device, and a channel for the QEMU guest agent. derisk's
Machines app runs these same commands.

`--installer CD.iso`, given more than once for more CDs, makes an empty
machine that installs its system from those CDs instead. `--system windows`
makes it a Windows PC. pm adds the image's CD of guest tools for that system
from `share/pm/machines` on `XDG_DATA_DIRS`, or the one `--tools` names, as
[Machines from an installer CD](hypervisor.md#machines-from-an-installer-cd)
describes. Removing a machine deletes only its own disk, with
`virsh undefine --storage vda`, and never a template or CD that another
machine may share.

`nixos/tests/machines.nix` makes a machine from the image's own template this
way, under TCG. It checks that the machine boots, grows its root over its
8 GiB disk, gets an address on libvirt's network, and lists its apps through
the agent. It also checks that opening an app reaches the host's vsock port
7350 from the address libvirt gave the machine. CI runs it with the other VM
tests.
