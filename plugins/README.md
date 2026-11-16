# This distribution's pm plugins

Three WebAssembly components, built against pm's `wit/plugin.wit`
([pm#4](https://github.com/dichhead/pm/pull/4)). pm loads every `*.wasm` in
`$XDG_CONFIG_HOME/pm/plugins/` and asks it two questions its built-in tables
cannot always answer.

Read pm's `plugins/README.md` for the sandbox and the trust model. What follows
is why *these three* exist.

## `losos-image` — classify-command

pm's fingerprint table knows compilers, build systems, archivers and coreutils.
It does not know the tools that turn a staged tree into something bootable,
because those are not build systems. So before this plugin, a recipe calling
`qemu-img` or `xorriso` was refused before a single step ran:

```
$ pm --no-plugins explain build.yaml
Error: no built-in fingerprint matches these commands: `mkfs.vfat …`,
       `mcopy …`, `xorriso …`, `qemu-img …`, `patchelf …`
```

With it, the same file is accepted and every command is attributed:

```
$ pm explain build.yaml
grants:     Toolchain, Coreutils, Archive
plugins:    losos-image 0.1.0

COMMAND                                              FINGERPRINT
mkfs.vfat -F32 -n LOSOS-ESP /build/esp.img           losos-image:mkfs-vfat
mcopy -i /build/esp.img … ::/EFI/BOOT/BOOTX64.EFI    losos-image:mcopy
xorriso -as mkisofs -e esp.img … -o /dest/losos.iso  losos-image:xorriso
qemu-img convert -f raw -O qcow2 … /dest/losos.qcow2 losos-image:qemu-img
patchelf --set-rpath $ORIGIN/../lib /dest/usr/bin/app  losos-image:patchelf
```

Note what is **not** in `grants`: `Network`. The manifest's ceiling is
`Toolchain, Coreutils, Archive`, so pm would drop a network capability even if a
future edit here asked for one by mistake. An image build that reaches the
network is an image build whose contents are not the ones that were reviewed.

`patchelf` is in the list for a different reason than the rest. Setting
`RUNPATH` to `$ORIGIN/../lib` is what lets a package carry its own libraries and
resolve them from inside itself — the AppImage technique, and the missing half
of pm's self-containment. `$ORIGIN` is passed literally: there is no shell in a
build step, so nothing expands it, which is exactly what patchelf wants.

Filesystem builders are named individually (`mkfs.ext4`, not `mkfs*`) so a typo
is an unrecognised command, which pm reports, rather than a wildcard quietly
matching something else.

## `losos-nix` — classify-command, and symbols

pm is the system manager on the NixOS build of this OS as well
(`docs/pm.md`), so pm is also what drives the flake — and `nix` is not in
its fingerprint table. This names the commands that read or build a flake:

```
$ pm explain build.yaml
grants:     Toolchain, Coreutils, Archive
plugins:    losos-image 0.1.0, losos-nix 0.1.0, losos-systemd 0.1.0

COMMAND                                                  FINGERPRINT
nix --store /build/nix flake check --offline /build/src  losos-nix:flake-check-offline
nix --store /build/nix build --offline /build/src#image  losos-nix:build-offline
nix-store --query --requisites /build/result             losos-nix:nix-store
nix hash path /build/result                              losos-nix:hash
```

This is the one plugin here whose ceiling includes `Network`, because
evaluating a flake fetches its inputs and building one substitutes from
cache.nixos.org. A `nix` step that says `--offline` gets none, and its
fingerprint says so (`-offline`), so the jail enforces the claim rather than
trusting it: `--offline` alone only turns substituters off. Drop it from one
step and `grants` gains `Network` for the whole file (C8). The older `nix-*`
commands have no such switch and always get it; `nix hash` and `nix nar` never
do.

pm's jail mounts `/nix/store` read-only and no daemon socket, which is why the
steps above build into `--store /build/nix`.

Recognised and deliberately left unclassified, with a log line saying why:
`nixos-rebuild`, `nixos-install`, `nix-env`, `nix profile` (they change a
machine, and this OS updates by systemd-sysupdate, never by switching a
generation); `nix run`, `shell`, `develop`, `fmt`, `bundle`, `repl`, `nix-shell`
(each runs a program the step does not name); `nix copy`, `nix-copy-closure`,
the writing `nix-store` operations (another store); `nix flake update`, `lock`,
`prefetch`, `nix-prefetch-url`, `nix-channel` (newer than the pin, and a pin
moves by hand). An option before the subcommand whose value it does not know
fails safe: the value is read as the subcommand and nothing is classified.

A flake reference that can move gets no verdict either, so nixpkgs reaches a
pm build only through a pin. `nixpkgs#hello`, `flake:nixpkgs`,
`github:NixOS/nixpkgs/nixos-26.05`, a tarball URL without `narHash=`, a
`git+` URL without `rev=`, `--override-input` with any of those, and a
`<nixpkgs>` lookup path all resolve to whatever is newest. A path, a commit,
and `--file` are classified. This flake's `.#pm-payloads.<name>` is the way in:
any nixpkgs package at the pinned revision, statically linked from `pkgsStatic`,
laid out as a pm package's `/dest` (`docs/pm.md`, "nixpkgs in a pm build").

Its symbols are the paths NixOS fixes — `store`, `current-system`,
`booted-system`, `system-bin` — for a package that installs something referring
to them. The test for a symbol is whether the ecosystem fixed the path or this
distribution chose it; a path a build chooses stays in its build file. Only the
store is visible inside the jail.

pm's own tree already carries `sysupdate`, `sysext` and `systemd` plugins,
so there is no losos copy of those; `just plugins` builds them from the
pm input's pin (next section).

## `losos-systemd` — scan-source

This one is the interesting half, because it is the only signal in pm that can
be **exact**.

Every other signal is an approximation, and pm's own documentation says so: ELF
analysis reads `DT_NEEDED` and guesses a directory; source scanning finds a
string literal shaped like a path and cannot tell a real one from an error
message. pm's own profile is 56 grants of which most come from path literals in
its dependencies' *test files*.

A systemd unit is not an approximation. `ReadWritePaths=` does not suggest that
a service might write somewhere — it is the list systemd will make writable and
nothing else will be. `StateDirectory=` names the one directory systemd creates.
For a package that ships units, the most accurate description of what it needs
at run time was already inside the package, written by whoever wrote the
service, and nothing was reading it.

A unit of thirteen directives yields twenty-one grants, each with the file and
line it came from:

```
read
  /etc/losos        plugin  covers /etc/losos/probe.env
                            losos-probe.service: losos-systemd: 14: ConfigurationDirectory= /etc/losos
                            losos-probe.service: losos-systemd: 9: EnvironmentFile= /etc/losos/probe.env
write
  /var/log/losos-probe  plugin  losos-probe.service: losos-systemd: 12: LogsDirectory= /var/log/losos-probe
exec
  /usr/bin/losos-probe  plugin  losos-probe.service: losos-systemd: 7: ExecStart= /usr/bin/losos-probe
network
  (granted)  plugin  losos-probe.service: losos-systemd: 17: IPAddressAllow= localhost
```

It is deliberately conservative. The absence of `PrivateNetwork=yes` is **not**
read as evidence that a service needs the network — almost no unit sets it, so
treating absence as evidence would grant the network to everything and mean
nothing. `ExecStart=` prefixes (`-`, `@`, `+`, `!`, `!!`) are stripped, because a
grant naming `-/usr/lib/losos/prepare` names a file that does not exist. `%`
specifiers are left alone rather than guessed at. Continued lines (trailing `\`)
are skipped rather than half-read.

`.target` and `.slice` are not scanned: they carry ordering and resource limits,
never a path.

## Building

```sh
just plugins    # nix build .#pm-plugins -o plugins/dist
```

That leaves eight components in `dist/share/pm/plugins`: this tree's three,
and five of pm's own — `libvirt`, `podman`, `sysext`, `sysupdate` and `systemd` — built
from the pm flake input, the commit pm itself is built from, rather
than copied here to drift. Three read the sysupdate transfers and sysext
definitions `nixos/` ships and name the systemd tools a recipe calls; `libvirt` boots a
package's own kernel through the libvirt the image runs (`docs/pm.md`, "Virtual
machines"); `podman` runs a person's own containers in rootless Podman
(`pm container`), which is how Windows app support starts its Windows
(`docs/windows-apps.md`). pm's `zig` is left out: nothing here
builds with it. pm consults plugins in file-name order and takes the first
verdict, so where `losos-image` and pm's `sysext` both name a tool
(`systemd-repart`, `mkfs.erofs`), `losos-image` answers. The NixOS image carries all
eight, unsigned, in `/run/current-system/sw/share/pm/plugins`
(`nixos/pkgs/pm-plugins.nix`).

pm loads plugins only from `~/.config/pm/plugins`, and only ones signed by a
key that user trusts; `docs/pm.md`, "pm's plugins", has the two commands.
Without that pm loads no plugins at all, and a build file calling `nix` is
refused with "no built-in fingerprint matches", which reads as a problem with
the build file rather than with a component that was never installed.

Nothing checks any more that `wit/plugin.wit` here still matches pm's. A
Python gate did, and it went with the pm recipe tree. pm cannot catch the
mismatch either: from pm's side a plugin built against an older contract is
simply a plugin, right up to the point where a record gains a field and every
component stops loading. Copy pm's `wit/plugin.wit` here when the
pm input's pin moves.
