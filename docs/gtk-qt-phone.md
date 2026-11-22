# GTK and Qt on a phone

GTK and Qt applications are written for a desktop. Dialogs open at a desktop
size, the GTK3 file chooser puts a sidebar beside its file list, and a finger
on a Qt list selects instead of scrolling. On a phone, windows run off the
screen.

The overlay does not patch `gtk3`, `gtk4` or Qt. Those packages, and
everything built against them, are nixpkgs' builds and substitute from
cache.nixos.org. A patched toolkit used to drop about thirty derivations
per architecture out of that cache, and CI compiled them. Phone-sized
layout is whatever the upstream toolkit does.

![A GTK4 dialog on a phone-sized screen, its search field focused and derisk's on-screen keyboard open beneath it](images/gtk-phone-keyboard.png)

The on-screen keyboard is derisk's. GTK3, GTK4 and Qt 6 all speak Wayland's
`text-input-unstable-v3` without patches, so the keyboard can follow text
focus once derisk's compositor offers that protocol. Flatpak applications
run on their runtime's own GTK and Qt.
