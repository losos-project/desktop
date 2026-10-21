# The Store

Apps the image does not carry come from [Flathub](https://flathub.org), and
the Store is where a person finds and installs them. It is one of derisk's
own apps, `org.derisk.store` from `crates/derisk-store` in derisk, and the
desktop draws it like Settings and Files. The image ships no other store. The
Store replaced Bazaar, whose GTK 4 window took neither scrolling nor clicks
under derisk.

- **Categories come from what the apps say.** Every app on Flathub ships
  AppStream data, and the Store sorts by all of it. An app goes in each main
  category it names, such as `Game` or `Office`. It also goes in each main
  category that its additional categories belong under in the freedesktop
  menu specification, such as `ArcadeGame` under Games or `WebBrowser` under
  Internet. So an app that names only `BoardGame` is still a game. An app
  that names no category at all goes where its keywords, name and summary
  point. Utilities, the catch-all, comes last whenever an app names
  something narrower. A category's page is split into sections by the
  additional categories its apps name, such as Arcade, Strategy and
  Emulators.
- **Release notes on the front page.** Apps list their releases, with notes,
  in their AppStream data. The front page leads with the updates waiting,
  then what changed in the apps you have installed, then the categories,
  then the newest release notes across Flathub, each linking to its app.
- **An app's page** says what the app is, then what people think of it, then
  what changed in its releases. Flathub's catalogue has no ratings, so the
  ratings and reviews come from GNOME's [Open Desktop Ratings
  Service](https://odrs.gnome.org), ODRS, which GNOME Software reads too. The
  page shows the average, how many people gave each number of stars, and the
  five most helpful reviews in the session's language. The Store asks only
  when a page opens, and it sends a user hash of all zeros, because nothing
  here writes reviews. So ODRS learns which app was looked at and nothing
  about who looked.
- **Search** looks at names first, then keywords, then summaries and IDs.
  Every word typed has to be found.
- **Your language.** Names, summaries, descriptions and release notes are
  shown in the session's language where Flathub has them translated.
- **Install, open, update and remove.** An app's page installs it, and once
  it is installed, opens, updates or removes it. The Refresh button fetches
  Flathub's catalogue again and looks for updates.

## Where it installs

On a PC, Flatpak apps run in their own machine, described in
[The Flatpak machine](hypervisor.md#the-flatpak-machine), and the Store
installs into it. It finds the first machine made from the `losos-flatpak`
template and asks that machine's `losos-guest-flatpak` through the guest
agent. That runs `flatpak` as the machine's user, whom the machine lets
install for everyone in it. Installs, removals and updates run in a unit of
that user's manager, and the Store reads what they have printed so far a few
times a second for its progress bar. It reads Flathub's catalogue and icons
from the machine's disk. If the machine is off, the Store starts it. An app
installed there opens on the desktop like any other machine's app.

A phone has no machines and keeps Flatpak on itself, so there the Store runs
the system's `flatpak`. flatpak's polkit rule lets an administrator install
for everyone without a password. For anyone else, the Store installs for that
person alone with `--user`, from the app's flatpakref, which needs no remote
set up.

## How it reads Flathub

flatpak keeps each remote's catalogue at
`/var/lib/flatpak/appstream/flathub/<arch>/active/appstream.xml.gz`, with the
icons it names in `icons/64x64/` beside it. It refreshes the catalogue with
`flatpak update --appstream`, which any user may run. Flathub's catalogue is
about 50 MB of XML once inflated, with every string in dozens of languages.
So the Store reads it as a stream and keeps only what it shows. All of
Flathub, over three thousand apps, reads in about half a second. A category
or search page lays out only the rows on screen. A system that has never
fetched the catalogue, such as one on a first boot without network, fetches
it the first time the Store opens.
