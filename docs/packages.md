# glibc and stock packages

`flake.nix` selects nixpkgs' standard `x86_64-linux` and `aarch64-linux`
platforms. The NixOS configuration uses nixpkgs' stock glibc packages. Its
only overlay adds this repository's own packages and patches GTK and Qt, as
[GTK and Qt on a phone](gtk-qt-phone.md) describes. ShellCheck and the
package-generated documentation, for example, keep nixpkgs' defaults. glibc's
NSS lets GDM and ordinary account lookups see systemd-homed users through
`nss-systemd` and the configured nscd forwarder.

Everything the overlay builds itself links with
[mold](https://github.com/rui314/mold) instead of nixpkgs' default `ld.bfd`.
That covers derisk, pm and its plugins, `losos-installer`, `losos-security`,
`losos-swap`, `losos-adblock`, the LosOS client `losos`, Danube, `x2mcsapi`,
libhybris, and the patched GTK and Qt. The
overlay calls them through one helper, `callWithMold`. It gives a package
nixpkgs' stdenv and a `rustPlatform` built on it, both through
`stdenvAdapters.useMoldLinker`, so the C compiler and the `cc` that rustc
links with both pass `-fuse-ld=mold`. A package added later links with mold
just by being called through the helper, and there is no list to keep. WPE
WebKit, which [Danube](danube.md) draws, is built from nixpkgs' own package
set and keeps its linker. Packages taken from nixpkgs unchanged keep its
linker too, because giving them a different stdenv would mean none of them
substitute from cache.nixos.org.
