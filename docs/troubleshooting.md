# When something goes wrong

derisk's apps, the installer and first-boot setup report a failure in an alert
dialog. The red title says what failed. Below it are the causes, what to do if
the app knows, and the error code. A partial failure is a warning in the accent
colour instead. **Copy details** copies the report as plain text for a bug
report, **Learn more** opens the matching section of this page, and **OK** or
Escape closes the dialog.

![An error dialog: "Could not apply the theme" in red, the cause "no theme named pastel", what to do about it, the code mcsapi_theme::not_found, and the Copy details, Learn more and OK buttons](images/error-dialog.png)

**Learn more** opens the offline copy of these docs in
`/run/current-system/sw/share/doc/losos` (set up by `nixos/modules/docs.nix`),
so it works without a network. If the copy lacks the page, it opens the
published site. The installer and first-boot setup run before there is a
browser, so their dialogs print the section's web address for you to open on
another device. The dialog is mcsapi's `ErrorDialog` drawing an
`mcsapi_ui::Error`, a miette report that knows its section.

## Files could not open or change something

Files reports what the filesystem refused: a folder that is gone, a name that
is taken, or a file you may not read or change. The cause under the message is
the system's reason, such as "Permission denied" or "Not a directory". Files
outside your home, and on disks you did not mount yourself, usually belong to
root. Files opens a file with `xdg-open`. If nothing opened, `xdg-open` did not
start. If the file opened in the wrong app, or in none, that was `xdg-open`'s
choice.

![Files showing "Could not open folder" with its cause, "Not a directory (os error 20)", and Learn more, Copy details and Dismiss](images/error-files.png)

## The editor could not open or save a file

The Text Editor saves to the path in its location field, which can differ from
the file it opened. That is how you save a copy, and an empty field has nowhere
to save to. A file that won't open is usually unreadable to you or not text. A
path that doesn't exist yet opens as a new file and is created on the first
save.

## Settings could not be read or saved

Settings keeps everything in `$XDG_CONFIG_HOME/derisk/settings.conf`, or
`~/.config/derisk/settings.conf` if that variable is unset. Each **Save**
rewrites the whole file through a temporary file next to it. "No config
directory" means neither `XDG_CONFIG_HOME` nor `HOME` is set, so your changes
last only until Settings closes. Any other read or write error comes from the
filesystem, as in Files above. A full or read-only home causes it too.

## Settings skipped some lines

A line Settings did not understand, such as `desktop.gaps = lots`, is left
out, and that setting keeps its default. The warning names the first such
line.

> [!WARNING]
> The next Save writes the file again from what Settings shows, without the
> skipped lines.

![Settings warning in its accent color: "Skipped 1 invalid line(s): line 1: ignored desktop.gaps = lots", and that those settings keep their defaults until the lines are fixed](images/error-settings.png)

## The default browser did not change

Settings writes the browser you pick under **Default apps** to
`$XDG_CONFIG_HOME/mimeapps.list`, usually `~/.config/mimeapps.list`, for web
links and HTML files. Every desktop reads that file. This error means Settings
saved its own file but could not write `mimeapps.list`, so links still open in
the old browser. The cause comes from the filesystem.
[The choice screens](choice-screens.md) explains where the browser list comes
from.

## The installer stopped

The message comes from the installer backend's failed step, and **Show
details** shows its log. A refused disk was either not in the backend's list or
is the medium the installer booted from. If a download fails to verify because
your network re-signs TLS, install from a release disk instead.
[The installer](installer.md) describes each step.

> [!WARNING]
> Nothing is written before "Erase and install"; after it, a failed install
> leaves the disk without a working system, and the install can be run again
> from the start. tty2 has a root shell for looking around first.

If the installer won't erase a disk that used LVM or LUKS, the details usually
say the partition table could not be reread. The backend deactivates volume
groups on that disk and closes LUKS on its partitions before wiping it. LosOS
never installs into an existing LVM-on-LUKS layout. It replaces the whole disk
with its own GPT partitions ([Where the OS lives](layout.md)). TPM-locked LUKS on the
root partition is [a separate option](installer.md#asking-for-a-pin-at-startup).

![The installer's "The install failed" page: partitioning and mounting done, "Downloading and writing the system" failed with systemd-sysupdate's exit status, and Show details open on a log ending in "SSL peer certificate or SSH remote key was not OK"](images/installer-failed.png)

## Setup could not finish

First-boot setup applies its pages one by one with `localectl`, `timedatectl`
and `homectl`, and the message comes from that tool. The usual failure is the
account: homed refuses a user name that is taken or invalid, and a password
that fails its quality check. **Back** returns to the account page.
[Setup](first-boot.md) runs on every boot until an account exists.

## No one can administer the machine

A running system is administered only through
[`pm system`](pm.md#administering-the-machine), which manages units and updates
and nothing else. It needs an administrator who can sign in, pm's system
service and polkit. If one of those is missing, or you need more, get a root
shell for one boot. Only someone at the machine can ask for it, from the boot
menu. Nothing in a running system can open one.

On a PC that starts by legacy BIOS, Limine's menu entry carries the kernel
command line, and its editor is on
([Boot loader and boot screen](boot-loaders.md#the-menu)):

1. When the boot menu shows, highlight the newest version and press `E`.
2. Add ` systemd.debug_shell` to the end of the `cmdline:` line.
3. Press F10 (or Ctrl+X) to start it.
4. Once the login screen shows, press Ctrl+Alt+F9. tty9 is a root shell,
   with no password.

From the shell, `homectl create <name> --member-of=wheel` creates a new
administrator, and `journalctl -u pm-system -b` shows why pm's service did not
start. The edit lasts one boot, but systemd's debug shell stays open while the
system runs, so run `systemctl reboot` when you are done.

> [!IMPORTANT]
> Under UEFI, Limine's entry chainloads the UKI and has no command line to
> edit, and a UKI takes kernel arguments only in place of its whole command
> line. The GRUB prompt and `losos_extra` that gave UEFI PCs the shell went
> with GRUB, and no Limine way to the shell has been worked out for UEFI
> yet.

The shell does not open anyone's home. Each home is its own encrypted image,
and only its owner's password unlocks it. The rest of the disk, the system and
its state, is not encrypted ([Where the OS lives](layout.md)). So the shell gives
someone at the machine nothing that a live USB stick would not.
