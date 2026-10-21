# Windows app support

Windows app support opens Windows apps in their own windows on the
desktop. Each person who opens it gets their own Windows, which sets itself
up the first time without asking anything. It is
[WinBoat](https://github.com/TibixDev/winboat), under the MIT licence, with
the desktop's name for it and three changes made for this OS. It runs on
x86-64 only, and it needs KVM.

## What runs where

| | |
| --- | --- |
| Windows | Windows 11, in [dockur/windows](https://github.com/dockur/windows)' QEMU, inside a container |
| The container | the person's own, in rootless Podman, its disk in `~/.local/share/winboat` |
| Each app's window | a FreeRDP RemoteApp session, H.264 in full colour (AVC444) |
| Who starts the container | pm (`pm container`, its podman plugin), never Podman or Docker directly |

dockur downloads Windows from Microsoft, in the person's language, and
installs it unattended with an answer file. When Microsoft's servers refuse
the download, dockur tries mirrors of the same ISO. At Windows' first
sign-in it runs WinBoat's guest server, a service that lists Windows' apps
and their icons for the app. It also imports the registry changes that let
every program run as a RemoteApp. The person's home is shared with Windows
as a network drive.

## The first time

The app grid's **Windows app support** runs `windows-app-support`, from
`nixos/pkgs/windows-app-support.nix`. For a person with no Windows yet, it
starts their user unit `windows-app-support-setup`. That unit does what
WinBoat's setup wizard does once it has its answers, and supplies the
answers itself:

- the Windows account is the person's user name, cut down to what Windows
  accepts, letters and digits up to 20 characters. Its password is random
  and nobody types it, because WinBoat reads it back from the compose file
  for every session;
- the guest server checks the host by that password's Argon2id hash, the
  way WinBoat's wizard writes it;
- half the PC's memory, between 4 and 16 GiB, half its processors,
  between 2 and 8, and a 64 GiB disk;
- Windows in the person's language when dockur has it, English otherwise.

It writes `~/.winboat/podman-compose.yml` and WinBoat's settings, then has
pm bring the container up. WinBoat opens as soon as the container exists
and shows Windows installing. The setup carries on while WinBoat is closed,
and a notification says when Windows is ready, which takes most of an hour.
Since a container already exists, WinBoat's own wizard never shows.

Windows is not activated. It runs as Windows does before a product key is
entered, and a key can be entered in Windows' own Settings.

## Rootless only

A container owned by root would run with the whole system's privilege, so
root runs no containers:

- WinBoat is patched in `nixos/pkgs/patches/winboat/` to start, stop, list
  and inspect its container through `pm container`, and to offer only
  Podman. A setting that still says Docker gets Podman too.
- [pm](pm.md)'s podman plugin turns each request into Podman's commands, or
  podman-compose's for a compose file, and refuses root's requests.
- The image's `podman` refuses root outside a user namespace. Root's Podman
  API socket, which NixOS would open to the `podman` group, is not started,
  as `nixos/modules/windows-apps.nix` sets up. Docker is not installed.

pm runs only plugins its user has signed. So that nobody has to sign one by
hand, WinBoat and the setup run their own `pm`, which keeps its
configuration under `~/.local/state/windows-app-support`. That configuration
holds its own key and the image's podman plugin signed with it, re-signed
when an update brings a new plugin. The containers still belong to the
person, in their own Podman store, and `podman ps` in a terminal shows
them.

Rootless Podman maps a container's users onto the person's subordinate
IDs. People here are systemd-homed users, made after the image was built. So
`/etc/subuid` and `/etc/subgid` give each UID in homed's range its own 65536
IDs by number, and both shadow and Podman look them up there.

## Windows' apps

WinBoat lists Windows' apps through its guest server and opens each as a
FreeRDP RemoteApp window. Each window comes over RDP's graphics pipeline as
H.264 in full colour. The image's WinBoat passes FreeRDP `/gfx:AVC444`, and
the registry changes it imports at the first sign-in set the policy that
makes Windows prefer it. Windows encodes the video in software, because the
container's QEMU has no GPU, and the host decodes it with FFmpeg.

## What was here before

WinApps used to do this. A person installed Windows into a machine from the
Windows tools CD and then told WinApps its address, account and password by
hand. WinBoat installs the Windows it serves, so there is nothing to
describe. A Windows machine made with `pm machine create --system windows`
still works, with its screen in virt-viewer, as
[Windows](hypervisor.md#windows) describes. It just has no app windows.
