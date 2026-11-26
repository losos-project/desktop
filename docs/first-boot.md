# First-boot setup

A fresh install has no user. On first boot, `derisk setup` asks for your
language, keyboard layout, time zone, network, and the first account (full
name, user name and password), then saves them and exits. Everyone after the
first account is added in **Settings › Users** ([More than one person](users.md)).

![First-boot setup's Time zone, Network and Your account pages](images/first-boot-pages.png)

## How it runs

`nixos/modules/setup.nix` runs `derisk setup` on tty1 as its own logind
session, after homed and before the display manager. It saves your choices
with `localectl set-locale`, `localectl set-x11-keymap`,
`timedatectl set-timezone` and `homectl create --member-of=wheel`. The password
goes in `NEWPASSWORD`, never on a command line. The keyboard layout switches as
soon as you pick it, and the display manager passes localed's saved layout to
the login screen and session as xkbcommon's defaults.

Setup runs on every boot and exits at once if `userdbctl` lists a regular user,
so a machine switched off halfway asks again. If derisk can't draw at all,
homed's console wizard asks for the user instead. homed's own first-boot unit
stays for `home.create.*` credentials, without its prompt.

The language list is the thirty-odd locales built into the archive that derisk
has names for, and `i18n.imperativeLocale` lets localed's choice stick. On a
phone-sized screen the pages fill the screen and the on-screen keyboard opens
under a focused field. On a PC the card floats and the keyboard is a button in
the corner.

## Password quality

homed refuses weak passwords, such as short ones or dictionary words. The
account page runs the same check as you type: it loads libpwquality the way
systemd does, with the same configuration, shows why a password would be
refused, and keeps **Set up** disabled until it would pass.

If homed refuses the password anyway, homectl asks a password agent for
another one, with no time limit and nobody on the seat to answer. So setup
watches `/run/systemd/ask-password` for that question and cancels it. homectl
then exits with homed's reason, the failure dialog shows it, and **Back**
returns to the account page
([Setup could not finish](troubleshooting.md#setup-could-not-finish)).

## The first login

You aren't asked for the password you just chose. Setup leaves a ticket with
the user name and password in `/run/derisk-first-login`, owned by root, mode
0600, on a tmpfs, so it never reaches a disk. The display manager takes the
ticket before starting the login screen: it deletes the file, then logs that
user in through the same `derisk-login` PAM stack, so homed unlocks the home as
usual. A ticket that isn't root's, isn't private, is a symlink, or is older
than ten minutes is deleted and ignored. Only this first login skips the login
screen.

The session opens on a short tour of the top bar, window buttons, search, the
overview, and their keys. Finishing or skipping it leaves a mark in
`~/.local/state/derisk`, and **Take the Tour** in the palette shows it again.
The boot VM test, `nixos/tests/boot.nix`, writes a ticket by hand and checks
that the session starts without the login screen and that the ticket is gone.
