# First-boot setup

A fresh install has no user. `derisk setup`, set up in
`nixos/modules/setup.nix`, runs on tty1 as its own logind session, after homed
and before the display manager. It draws derisk's pages on the seat: language,
keyboard layout, time zone, network, and then the first account with its full
name, user name and password. It saves them with `localectl set-locale`,
`localectl set-x11-keymap`, `timedatectl set-timezone` and
`homectl create --member-of=wheel`, and exits. The password goes in
`NEWPASSWORD` and never on a command line. Everyone after the first account
is added in Settings, Users, as [More than one person](users.md) describes.
The keyboard layout switches live as it is picked. The display manager hands
localed's saved layout to the login screen and the session as xkbcommon's
defaults.

homed checks the password's quality and refuses a weak one, such as a short
password or a dictionary word. The account page runs the same check while
the password is typed. It loads libpwquality the way systemd does, with the
same configuration, shows under the field why a password would be refused,
and keeps "Set up" off until it would be accepted. If homed refuses it
anyway, homectl asks for another password. With no terminal to ask on, it
asks a password agent with no time limit, and nothing on the seat would ever
answer. So setup watches `/run/systemd/ask-password` for a question from the
command it runs and answers it with cancel. homectl then exits with homed's
reason, the failure dialog shows it, and Back returns to the account page.
[Setup could not finish](troubleshooting.md#setup-could-not-finish) covers
this.

![First-boot setup's Time zone, Network and Your account pages](images/first-boot-pages.png)

Setup runs on every boot and exits at once when `userdbctl` lists a regular
user, so a machine switched off halfway through asks again. If derisk cannot
draw at all, homed's console wizard asks for the user instead. homed's own
first-boot unit stays for `home.create.*` credentials, without its prompt.
The languages offered are the locales built into the archive, the thirty-odd
that derisk has names for, and `i18n.imperativeLocale` lets localed's choices
stick. On a phone-sized screen the pages fill the screen and the on-screen
keyboard opens under a focused field. On a PC the card floats and the
keyboard is a button in the corner.

## The first login

The person who just chose the password is not asked for it again a minute
later. Setup leaves a ticket with the user name and password in
`/run/derisk-first-login`. It is owned by root with mode 0600, on a tmpfs, so
it never reaches a disk. The display manager takes the ticket before it
starts the login screen. It deletes the file first, then logs that user in
through the same `derisk-login` PAM stack, so homed unlocks the home area as
usual. A ticket that is not root's, not private, a symlink or older than ten
minutes is deleted and ignored. Only that first login skips the login
screen, and every later boot and every log out shows it.

The session then opens on a short tour of the desktop: the top bar, the
window buttons, search, the overview, and the keys for each. Finishing or
skipping it leaves a mark in `~/.local/state/derisk`, and "Take the Tour" in
the command palette shows it again. The boot VM test, `nixos/tests/boot.nix`,
writes a ticket by hand and checks that the session starts without the login
screen and that the ticket is gone.
