# More than one person

One PC can have an account for everyone who uses it, and each person's desktop
keeps running while someone else uses the screen. [Setup](first-boot.md) makes
the first account, and everyone after that is added in Settings, Users. Every
account is a systemd-homed record with its own encrypted home area, so one
person's files stay locked while they are logged out, whoever else is logged
in.

## Settings, Users

The page lists everyone `userdbctl` knows, with their full name, user name
and role. Each change is a `homectl` command:

| Change | Command |
| --- | --- |
| Add someone | `homectl create <user> --real-name=… --member-of=…`, the password in `NEWPASSWORD` |
| Rename | `homectl update <user> --real-name=…` |
| Administrator or standard | `homectl update <user> --member-of=…`, with `wheel` added or taken out |
| Remove | `homectl remove <user>`, which deletes their home area and every file in it |

An administrator is a member of `wheel`. That is how polkit, and through it
`pm system` and Flatpak, know who administers the machine. The account
first-boot setup makes is an administrator. A standard user can use apps and
change their own settings. homed asks polkit before it makes, removes or
changes anyone's account, and polkit asks for an administrator's password
through [derisk's dialog](sign-in.md#polkit-and-pm). So a standard user can
open the page and have an administrator type their password. The page always
keeps one administrator. The last one cannot be made standard or removed, and
nobody can remove their own account from the page.

Each person's [virtual machines](hypervisor.md) live in their own home area
and run as their own user services. So removing someone deletes their machines
with the rest of their files, and the page says so. homed will not remove a
home area that is open, which it is while its owner is signed in, for instance
behind Switch user. The page reads the home area's state, and whether homed
keeps the account at all, from the status section of the record homed hands
userdb. For someone who is signed in, it offers to sign them out first with
`loginctl terminate-user`, which polkit also asks an administrator for. It
warns that their apps and machines will close and that anything they have not
saved will be lost.

Someone else's home area is locked while they are logged out, and homed
cannot write their record into it. So their change goes to homed's own copy
of the record with `homectl update --offline`. That copy is signed by the
machine and newer than the one in their home area, so homed writes it there
at their next login. A new account joins the same device groups as setup's,
such as `networkmanager` for Wi-Fi, and is made under the same
[password age](sign-in.md#password-age) policy, so its first login has
nothing to bring up to date.

Settings checks the new password the way setup does. It asks libpwquality
while the password is typed and keeps "Add" off until homed would accept it.
If homed refuses it anyway, the page answers homectl's request for another
password with cancel, as setup does, and shows homed's reason. The Sign-in
page does the same when a person sets a new password of their own.

## The login screen

The login screen lists everyone by full name, with their initial, and marks
who is signed in. Picking someone who is not signed in goes to their password.
Picking someone who is signed in goes back to their desktop instead of
logging them in a second time. The login screen asks logind to make that
session the seat's active one with `loginctl activate`, which polkit lets the
active session on a seat do, and that desktop's own lock screen asks for the
password. Typing a user name does the same. With one account and nobody
signed in, the list is left out and the screen goes straight to the password,
as before.

## Switch user

Switch user locks the desktop and shows the login screen on another VT. The
session keeps running behind its lock screen, with its apps and virtual
machines. Switch user is a button on the lock screen, a row in the palette,
`Super+X U`, and something the assistant understands, as in "switch user".
The session connects to `/run/derisk-dm/switch.sock`, and the connection
itself is the whole request. Anyone can connect, but the display manager acts
only for the owner of the session that is on the screen. So a program in a
background session cannot pull the screen away from whoever is using it.

`derisk display-manager` runs every session on its own VT. A login screen
takes the first VT from 1 that nothing has open. The display manager holds
that VT open until the session has it, because logind starts a getty on a VT
that comes to the front with nobody on it. The session started from that
login screen keeps its VT. The login screen comes back when nobody is logged
in, when the session on the screen ends, and on Switch user. A session ending
on another VT changes nothing on the one in use.
