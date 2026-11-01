# The portal machine

Apps ask the desktop for some things through portals: a file to open, a
place to save one, a yes or no, a screenshot, a new wallpaper. The dialog
that answers sees the person's files, their names, folders and pictures,
and some of what a file chooser does with them, such as reading a picture
to show it, is code that parses what someone else made. On a PC those
dialogs run in sys-portal, a machine of their own with no network card.
That holds for the host's apps and for the apps in every machine,
Flatpak's among them.

sys-portal is a small NixOS machine booted from the host's store, like
the other [service machines](network-machine.md). It is hidden from the
Machines app. Its dialogs are GTK's file chooser and access dialog and
derisk's own, and they show on the desktop as windows like any machine's.

## How a request gets there

1. An app asks xdg-desktop-portal, on the host or in its machine, as it
   always does.
2. For the portals with a dialog, the backend is `losos-portal forward`.
   It hands the request to the host's relay, over a socket on the host and
   over vsock from a machine.
3. The relay, `losos-portal-relay.service`, knows which machine asks from
   the vsock address libvirt gave it. On the host it takes requests only
   from the person whose session is in front. It starts sys-portal if it
   is not running and passes the request on.
4. In sys-portal, `losos-portal serve` puts the request to GTK's or
   derisk's backend. A file chooser for a machine says which machine
   asks in its title.

The portals without a dialog stay where they were: the theme, which apps
run in the background, and screen sharing's chooser, which needs the
compositor.

## Files

sys-portal runs for the person in front. The relay shares their home and
the folder their drives are mounted in into it, at the same paths as on
the host. A file picked for an app on the host is just that file.

A machine sees none of the host's files. When the person picks a file
for an app in a machine, the relay records that the machine may have it,
and the machine's backend copies it in before the app hears of it. The
app gets the copy. Whenever the app saves the copy, it is written back to
the file on the host. A file picked in a save dialog is written to the
place picked the same way. A machine can fetch and write only the files
picked for it, and the relay does both as the person, so the kernel checks
their own access.

A folder picked for an app in a machine is copied in whole, and it goes
back the same way. A file the app saves or adds anywhere in the copy is
written to the same place in the host's folder, and a file it removes is
removed there. The relay lets the machine write only under that folder,
and a link in the folder that leads out of it does not count as under it.

A machine pm made for one person, such as their Agent machine, says so in
its libvirt definition (`<pm:person>`). The relay answers it only while
that person is the one in front.

A picture an app in a machine makes the wallpaper goes the other way:
the relay copies it to `~/.cache/losos-portal/uploads` on the host, where
derisk takes it from.

## Lazy

sys-portal starts at the first request after it was stopped, which takes
a few seconds, and stops after ten minutes with no request. Starting an
app does not start it: the frontend reads the backend's properties from
`forward`, not from the machine. A screenshot asks the host's session for
the screen through the relay, so the machine never holds more than the
one picture.

## The pieces

| Piece | Where |
| --- | --- |
| The machine | `nixos/guest/portal.nix` |
| The relay and the host's backend | `nixos/modules/portal-machine.nix`, `portal-backend.nix` |
| Every machine's backend | `nixos/guest/common.nix` |
| The program | `src/losos-portal` |
