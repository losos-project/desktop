# App windows

App windows puts another system's apps on this desktop. Each of the
guest's apps gets an entry in the app grid and the palette, and each of its
windows opens as a window of its own here, beside the desktop's own. The
guest can be macOS in a machine or a Mac on the same network. All it needs
is an app server that speaks the protocol below.

It is three Rust crates in `src/app-windows`:

| | |
| --- | --- |
| `protocol` | the messages both ends send, shared by every server and the client |
| `server` | `macos-app-server`, which runs inside macOS |
| `client` | `app-windows`, which runs here and is in the image |

## What happens on the guest

`macos-app-server` does for macOS what RemoteApp does for Windows under
WinApps. It lists the installed apps with their icons, skipping background
agents. It starts an app with `open -b`.

Every 100 ms it reads macOS's window list. Each app window gets a
ScreenCaptureKit stream of its own, which captures the window alone even
when another covers it on macOS's screen. VideoToolbox encodes each stream
to H.264, on the Mac's media engine where it has one and in software where
it has not. The encoder runs in real time with no reordered frames, so a
picture leaves the guest as soon as it is drawn. A window whose contents do
not change sends nothing.

Menus, tooltips and panels that macOS draws as windows of their own are
sent as popups of the app's focused window, with their offset from it. The
Dock, the menu bar and the other parts of macOS's own screen are left out.

Input comes back as positions in a window and Linux key codes, and the
server posts it into macOS with CGEvent. Keys go by their place on the
keyboard, with the text the host's layout types with them, so a shortcut
works on any layout and a typed letter comes out as the person meant. A
key held down on the host repeats at the host's rate, marked as a repeat
the way macOS's own keyboard marks it.

Accessibility raises, resizes and closes windows, and reads the focused
app's menus, which it can also pick from. The clipboard's text goes both
ways, read from macOS twice a second.

macOS asks the person once to allow **Screen Recording** and
**Accessibility** for the server. Until both are allowed, the server tells
every client what is missing, and the client says so in its log.

The server listens on TCP port 7180 and lets in only a client that sends
the token in `~/Library/Application Support/LosOS App Server/token`.

## What happens here

`app-windows` connects to the server and writes a desktop entry and a
128 px icon for each guest app into the person's own folders. An entry is
named `<prefix>.<app id>.desktop`, for example
`macos.com.apple.TextEdit.desktop`. Entries with the prefix whose app the
guest no longer lists are removed. The list is read again every five
minutes, so an app installed on the guest shows up on its own.

Each guest window becomes an xdg toplevel whose app id is the same
`<prefix>.<app id>`, so the desktop files it under that app's entry and
icon. Its pictures are decoded with FFmpeg, on the GPU through VA-API where
the host has it and on the processor otherwise. Each window decodes on a
thread of its own and keeps only its newest picture, so a slow window never
holds up another. Pictures are shown at the guest window's size in this
desktop's logical pixels, whatever the guest's scale.

The guest draws its own title bar, so the windows have none here.
Dragging the guest's title bar moves the window on this desktop. A size the
desktop gives a window, when it tiles or maximizes it, is asked of the
guest. A window that does not take that size, as a dialog or Calculator
never does, keeps its shape in the middle of it, with black bars around
it, rather than being stretched. It is made smaller to fit a smaller size,
but never larger than macOS draws it, so its text stays sharp and the size
of the desktop's own. A click on a bar lands on the window's nearest
edge.

Under derisk, a new window that would tile at a size other than its own
opens floating instead, and its tile's size is asked of macOS. The server
moves the window to the top left of macOS's screen first, so macOS does not
shrink the size to keep the window on screen, and asks twice, for apps that
take a new size in steps. Once macOS draws the window at that size, it is
tiled. A window macOS will not resize stays floating at its own size.

Under derisk, a guest window's menus go in the top bar and the palette,
through derisk's agent socket. A pick there goes back to the guest as the
item it names.

## Mac packages

A disk image (`.dmg`) or installer package (`.pkg`, `.mpkg`) that reaches
this desktop opens in macOS, as a double click would open it on a Mac: an
image is mounted and its window shown, a package starts Installer. Two ways
lead there:

- **Opening one in Files.** "Open in macOS" is the default app for both
  types. It runs `app-windows send`, which waits for macOS if it is still
  starting.
- **Downloading one.** The `app-windows-downloads` user unit runs
  `app-windows watch ~/Downloads` for each person with a macOS guest, which
  is anyone with a token in `~/.config/app-windows/macos/`. It looks at the
  folder every two seconds and hands over each new package once its size has
  stopped changing, so a download still in progress is left alone. What was
  in the folder before it started stays where it is. A package that comes
  while macOS is off is handed over once it is up.

The file travels over the server's own connection, in pieces of 1 MiB, and
lands in Downloads on macOS under a name no other file there has. One that
does not arrive whole is removed there rather than left for someone to open.
The copy here stays where it was.

| Command | What it does |
| --- | --- |
| `app-windows show` | shows the guest's windows and keeps its apps listed, until the guest goes away |
| `app-windows apps` | lists the guest's apps here and prints them |
| `app-windows open ID` | starts an app, and runs `show` when nothing shows the guest yet; this is what an entry runs |
| `app-windows record SCRIPT OUT` | runs a test script with no screen, saving every picture |
| `app-windows replay FILE [SPEED]` | shows a recording's windows, timed as they came, and lists its apps |
| `app-windows send FILE...` | hands files to the guest, which opens them; this is what "Open in macOS" runs |
| `app-windows watch DIR [EXTENSION...]` | hands over each such file that lands in `DIR`, `dmg`, `pkg` and `mpkg` unless told |

`--server HOST:PORT` names the guest's server, and `--token-file` its
token, which by default is `~/.config/app-windows/<prefix>/token`.
`--prefix` sets what app ids start with (`macos`), and `--name` what the
guest is called (`macOS`).

## The protocol

Every message is a little-endian 32-bit length, a type byte, and the rest
of the message. Control messages carry JSON. Video carries a fixed header
(window, width, height, a keyframe flag and the guest's time) and H.264 in
Annex B, which any decoder takes. The types are in
`src/app-windows/protocol/src/lib.rs`.

A client says hello with the token, and the server answers with its name,
its system, how it encodes video and what it still needs allowed. Nothing
is captured until a client asks to watch windows. A client that falls
behind misses pictures rather than queueing them, and its next picture is a
keyframe.

A file goes as a start message with its name, size and whether to open it,
its bytes in pieces, and an end message, and the guest answers with where it
saved it. Nothing in the protocol is particular to macOS. Keys are Linux key codes,
which a guest maps to its own, and menus already have derisk's shape.

## How it is tested

`cargo test` covers the protocol, the desktop entries, the key tables,
recordings played back and the menus handed to a stand-in derisk. The Nix
build runs the same tests.

`.github/workflows/app-windows.yml` runs the whole path on GitHub's macOS
runners, which are Apple hardware, on Apple silicon and on Intel. There
`macos-app-server` runs inside macOS, and `app-windows record` drives it
from the same machine with the scripts in `src/app-windows/tests`. Those
scripts open TextEdit and Calculator. They type, pick menu items, copy and
paste, open a context menu, resize, hold a key and time how long a key
takes to show. Another downloads Rectangle's newest disk image into a
folder `app-windows watch` looks at, and sends an installer package built
with `pkgbuild`, then waits for the Finder and Installer windows they
open. A step fails when its picture, size, title, menus or
clipboard never comes. Each run keeps every picture, a report with frame
rates and key latency, and a recording, which `app-windows replay` shows on
a desktop.

The same workflow's `losos-vm` job runs LosOS itself on those Macs: the
newest nightly image boots under QEMU, signs in, and fetches `app-windows`
from the runner (`src/app-windows/tests/losos-vm.sh` and `losos-vm.py`).
From inside LosOS it finds Chess through the palette, types into TextEdit,
works out a sum in Calculator, and downloads a disk image with curl and
sends it. Every step waits for the window or picture it expects, and the
job keeps pictures of both screens. The machines are the other way round
from a LosOS PC, where macOS would be the guest: Apple's licence lets macOS
run only on Apple hardware, so here the Mac is the host. The client and the
server see the same network connection either way.

## What it does not do yet

- Pictures reach the compositor through shared memory, copied once from
  the decoder. Handing over the GPU's own buffers would skip that copy.
- The pointer is always the arrow here, whatever macOS shows over a link
  or a text field.
- Only text crosses the clipboard, not pictures or files.
- Drag and drop between the guest and this desktop does not work.
- A package downloaded inside another machine stays in that machine; only
  this desktop's own Files and Downloads hand packages to macOS.
- Sound stays in the guest.
