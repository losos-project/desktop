# The Store

The Store is where you find and install apps the image doesn't carry, from
[Flathub](https://flathub.org). It is one of derisk's own apps,
`org.derisk.store` from `crates/derisk-store` in derisk, drawn like Settings
and Files, and the image ships no other store. It replaced Bazaar, whose GTK 4
window took neither scrolling nor clicks under derisk.

## What it shows

The front page leads with waiting updates, then what changed in your installed
apps, then the categories, then the newest release notes across Flathub, each
linking to its app.

Categories come from each app's AppStream data. An app goes in every main
category it names, such as `Game` or `Office`, and in the main category each of
its additional categories belongs under in the freedesktop menu specification,
so `ArcadeGame` lands in Games and `WebBrowser` in Internet. An app that names
no category goes where its keywords, name and summary point. Utilities, the
catch-all, comes last whenever an app names something narrower. Each category
page is split into sections by its apps' additional categories, such as
Arcade, Strategy and Emulators.

An app's page says what the app is, then what people think of it, then what
changed in its releases. Flathub has no ratings, so ratings and reviews come
from GNOME's [Open Desktop Ratings Service](https://odrs.gnome.org) (ODRS),
which GNOME Software also reads. The page shows the average, the star
distribution, and the five most helpful reviews in your language. The Store
asks only when a page opens and sends an all-zero user hash, because nothing
here writes reviews, so ODRS learns which app was viewed but not by whom.

Search matches names first, then keywords, then summaries and IDs, and every
typed word must match. Names, summaries, descriptions and release notes show in
your session's language where Flathub has a translation. An app's page
installs it, and once installed, opens, updates or removes it. **Refresh**
fetches the catalogue again and checks for updates.

## Where it installs

On a PC, Flatpak apps run in [the Flatpak machine](hypervisor.md#the-flatpak-machine),
and the Store installs there. It finds the first machine made from the
`losos-flatpak` template and calls its `losos-guest-flatpak` through the guest
agent, which runs `flatpak` as the machine's user, who may install for the
whole machine. Installs, removals and updates run in a unit of that user's
manager, and the Store polls their output a few times a second for its progress
bar. It reads Flathub's catalogue and icons from the machine's disk, and starts
the machine if it is off. Installed apps open on the desktop like any machine's
apps.

A phone has no machines and runs Flatpak itself, so the Store uses the system's
`flatpak`. flatpak's polkit rule lets an administrator install for everyone
without a password. Anyone else installs for themselves alone with `--user`,
from the app's flatpakref, which needs no remote set up.

## Opening a .flatpakref or .flatpakrepo

The Store is the default app for both file types, so opening one from Files or
a browser's downloads hands it to the Store. A `.flatpakref` names one app and
its source, and a `.flatpakrepo` describes a remote. The Store reads the title,
and for a remote its name, then copies the file to where flatpak runs:
`losos-guest-flatpak store put` stores it in the machine user's runtime
directory, because the machine can't see host files. There it runs
`flatpak install --user --from` the file, or
`flatpak remote-add --user --if-not-exists --from` it, as a job like the ones
above, and shows the installed apps. Both are per-user, because a file from a
web page shouldn't be added for everyone on the computer.

## How it reads Flathub

flatpak keeps each remote's catalogue at
`/var/lib/flatpak/appstream/flathub/<arch>/active/appstream.xml.gz`, with its
icons in `icons/64x64/` next to it, and refreshes it with
`flatpak update --appstream`, which any user may run. Inflated, Flathub's
catalogue is about 50 MB of XML with every string in dozens of languages, so
the Store streams it and keeps only what it shows. All of Flathub, over 3000
apps, loads in about half a second, and category and search pages lay out only
the rows on screen. A system that has never fetched the catalogue, such as one
that first booted offline, fetches it the first time the Store opens.
