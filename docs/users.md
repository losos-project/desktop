# More than one person

A PC can have an account for everyone who uses it, and each person's desktop
keeps running while someone else uses the screen. [Setup](first-boot.md)
creates the first account, and you add the rest in **Settings › Users**. Every
account is a systemd-homed record with its own encrypted home, so your files
stay locked while you are logged out, whoever else is logged in.

## Settings › Users

The page lists everyone `userdbctl` knows, with full name, user name and role.
Each change is a `homectl` command:

| Change | Command |
| --- | --- |
| Add someone | `homectl create <user> --real-name=… --member-of=…`, the password in `NEWPASSWORD` |
| Rename | `homectl update <user> --real-name=…` |
| Administrator or standard | `homectl update <user> --member-of=…`, with `wheel` added or taken out |
| Remove | `homectl remove <user>`, which deletes their home area and every file in it |

An administrator is a member of `wheel`, which is how polkit, and through it
`pm system` and Flatpak, know who runs the machine. Setup's account is an
administrator. A standard user can use apps and change their own settings.

homed asks polkit before it creates, removes or changes any account, and polkit
asks for an administrator's password in
[derisk's dialog](sign-in.md#polkit-and-pm). So a standard user can open the
page and have an administrator type the password. The page always keeps one
administrator: the last one can't be made standard or removed, and nobody can
remove their own account here.

> [!WARNING]
> Removing someone deletes their home, including their
> [virtual machines](hypervisor.md), which live there and run as their own user
> services. The page says so before it acts.

homed won't remove a home that is open, as it is while its owner is signed in,
even behind Switch user. The page reads the home's state, and whether homed
manages the account, from the status section of the record homed gives userdb.
For a signed-in person it offers to sign them out first with
`loginctl terminate-user`, which also needs an administrator, and warns that
their apps and machines will close and unsaved work will be lost.

A logged-out person's home is locked, so homed can't write their record into
it. Changes go to homed's own copy with `homectl update --offline`. That copy
is signed by the machine and newer than the one in the home, so homed writes it
there at their next login. A new account joins the same device groups as
setup's, such as `networkmanager` for Wi-Fi, under the same
[password age](sign-in.md#password-age) policy, so its first login has nothing
to update.

New passwords are checked as in setup: libpwquality runs as you type and
**Add** stays disabled until homed would accept it. If homed refuses anyway,
the page cancels homectl's request for another password and shows homed's
reason. The **Sign-in** page does the same when you change your own password.

## The login screen

The login screen lists everyone by full name, with an initial, and marks who is
signed in. Picking someone who isn't signed in asks for their password. Picking
someone who is switches back to their desktop instead of logging them in again:
the login screen asks logind to activate that session with `loginctl activate`,
which polkit allows the seat's active session, and that desktop's lock screen
asks for the password. Typing a user name does the same. With one account and
nobody signed in, the list is skipped and the screen goes straight to the
password.

## Switch user

Switch user locks your desktop and shows the login screen on another VT. Your
session keeps running behind its lock screen, apps and virtual machines
included. Use the button on the lock screen, the palette row, `Super+X U`, or
ask the assistant to "switch user".

The session connects to `/run/derisk-dm/switch.sock`, and the connection itself
is the request. Anyone can connect, but the display manager acts only for the
owner of the session on screen, so a program in a background session can't
take the screen from whoever is using it.

`derisk display-manager` runs each session on its own VT. A login screen takes
the first free VT from 1, and the display manager holds it open until the
session has it, because logind starts a getty on an unoccupied VT that comes to
the front. The session keeps that VT. The login screen returns when nobody is
logged in, when the session on screen ends, and on Switch user. A session
ending on another VT changes nothing on screen.
