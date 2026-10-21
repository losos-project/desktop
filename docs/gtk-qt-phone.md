# GTK and Qt on a phone

GTK and Qt applications are written for a desktop. Dialogs open at a desktop
size, the GTK3 file chooser puts a sidebar beside its file list, and a finger
on a Qt list selects instead of scrolling. On a phone, windows run off the
screen. The overlay in `nixos/pkgs/default.nix` replaces `gtk3`, `gtk4` and
Qt 6's `qtbase` with patched builds, so every package that links them gets
the patched toolkit, on the PC and on Halium.

Each patch decides for itself whether it is running on a phone. It is when
every screen is under 600 logical pixels on its short side, the same line
derisk's shell draws between its phone and tablet layouts in
`src/adaptive.rs`. A phone docked to a monitor is then a desktop again, and a
PC behaves as stock. The one exception is Qt's touch scrolling, which
follows whether there is a touchscreen, not the screen size.

- **GTK3** carries Purism's adaptive series, the one PureOS and Mobian ship
  on phones. It brings an adaptive file chooser, which
  xdg-desktop-portal-gtk shows for every application that asks the portal
  for a file. It makes the about, print, font and shortcuts windows fit a
  phone, maximizes dialogs, adds a back button to dialog header bars,
  stacks the buttons in message dialogs, and fixes touch events. It copies a
  few libhandy widgets into GTK to build those. Purism turns the series on
  per device with an `org.gtk.Settings.Purism` `is-phone` key. `0033` adds
  the screen-size check, and the key still forces it on.
- **GTK4** carries Purism's three patches. Resizable dialogs and transient
  windows open maximized and get only a close button. postmarketOS carried
  the same two changes until libadwaita 1.5, whose `AdwDialog` and
  breakpoints adapt on their own, so libadwaita needs nothing. The patches
  are for plain GTK4 windows, such as gcr's prompts. `0004` is the
  screen-size check.
- **Qt 6** has no phone patches anywhere to adopt, because Plasma Mobile
  adapts in Kirigami and its own shell, so the Qt patches are this
  repository's own. A finger scrolls any Qt Widgets scroll area kinetically
  through `QScroller`. `QAbstractScrollArea` already supported
  that but left each application to turn it on. `QT_TOUCH_SCROLLER=0` turns
  it off for an application that is drawn on with a finger. On a phone,
  resizable dialogs open maximized, and the minimum size
  a window asks the compositor for is capped at the screen. Since Qt 6.10
  the Wayland client is part of qtbase, so `qtwayland` stays stock. Qt 5,
  which nothing in the image links, gets no phone patches, only the icon
  theme one from [One icon theme](icon-theme.md).

Each patched toolkit links with [mold](https://github.com/rui314/mold)
instead of nixpkgs' default `ld.bfd`, and compiles through ccache. The
overlay applies its patches through one helper, `withPatches`, which also
builds the package with nixpkgs' `stdenvAdapters.useMoldLinker` and
`ccacheStdenv`. A package patched later gets both without being listed
anywhere. The mold adapter puts `ld.mold` in the compiler wrapper and adds
`-fuse-ld=mold`, so configure probes and libtool link with mold as well as
the final link. ccache is used only when the builder binds a cache directory
to `/var/cache/losos-ccache`, so the derivation is the same with or without
one. CI's `prebuild` job keeps the cache in GHCR as `losos-ccache:<arch>`
and fetches it through the proxy. A GTK or Qt point release, or a changed
patch, then recompiles only the files it changes. A second GTK3 build took
832 of its 835 objects from the cache and finished in 3m44s instead of 6m35s,
with bit-for-bit identical output. These packages compile in CI anyway, so
neither change loses a cache hit. Packages that link against them keep
nixpkgs' stdenv, because changing it for every package would mean nothing
substitutes from cache.nixos.org at all.

![A GTK4 dialog on a phone-sized screen, its search field focused and derisk's on-screen keyboard open beneath it](images/gtk-phone-keyboard.png)

The on-screen keyboard is derisk's. GTK3, GTK4 and Qt 6 all speak Wayland's
`text-input-unstable-v3` without patches, so the keyboard can follow text
focus once derisk's compositor offers that protocol.

The cost is that a patched toolkit no longer substitutes from
cache.nixos.org, and neither does anything built against it. In this image
that comes to about thirty derivations per architecture:

- GTK3 and GTK4;
- Qt 6's qtbase, qtdeclarative, qtsvg, qttools and qtshadertools, because
  qtbase links GTK3 for its GTK platform theme, and breeze-icons, which
  papirus-icon-theme builds against, needs Qt;
- gjs, gcr, gnome-keyring, gnome-desktop, gnome-settings-daemon,
  xdg-desktop-portal and its GTK backend, ostree, flatpak and geoclue.

CI's `prebuild` job builds them before the release build starts.
`tools/nix-prebuild-plan` finds them as every derivation the release needs
that does not change with `losos.version`, so nothing lists them by name.
The job pushes them to the project's cache and hands them to the release
build in the same run, which checks that none of them is left to compile.
Later runs substitute them from the cache until nixpkgs moves GTK or Qt.
Flatpak applications run on their runtime's own GTK and Qt and do not get
these patches.

> [!NOTE]
> When nixpkgs moves GTK or Qt and a patch stops applying, the build fails at
> that patch. Purism rebases the GTK series for each Debian release;
> refreshing means copying the series from `pureos/latest` again and
> rebasing the two `Treat a display of phone-sized monitors` patches.
