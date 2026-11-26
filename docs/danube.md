# Danube

Danube is the web browser and the default for links and web pages. It is
[WPE WebKit](https://wpewebkit.org), the WebKit port with no toolkit, drawn in
a window built from mcsapi components in derisk's theme. Danube is only the web
view: derisk's command palette is its address bar, tab switcher and extension
list, and derisk's top bar holds its menus. It runs Chrome extensions
unmodified, opens captive portal sign-in pages, and loads the content blockers
that [system-wide ad blocking](adblock.md) compiles. The module is
`nixos/modules/danube.nix` and the code is in `src/danube`.

## How it is put together

WPE's headless platform renders each page into a buffer instead of onto the
screen. Danube copies the active tab's frame into an egui texture and draws it.
The rest of the window is small: a progress line, permission and security key
bars, a crashed-page notice and toasts. It is ordinary mcsapi code, recoloured
whenever derisk publishes a new theme to `$XDG_RUNTIME_DIR/derisk/theme.json`.
Pointer, wheel, touch and key input goes back to WebKit as WPE events. Typed
text goes as one key press per character, and Ctrl+C, Ctrl+X and Ctrl+V move
data between WebKit's clipboard and the desktop's.

WebKit runs on the main thread under GLib's main loop, and the window on a
second thread under winit's (`engine.rs` and `ui.rs`). Two bounded crossbeam
channels join them, with no locks. The window sends commands. WebKit sends
events, such as a new tab, a title, a frame or a permission request, and asks
for repaints. A frame that doesn't fit in the channel is dropped for the next.
The other threads (derisk's menus, URLs handed over by later `danube` runs, and
the captive portal recheck) are scoped to the same `run` and end with WebKit.

There are no Rust bindings for WPE's 2.0 API, so the crate generates its own.
`build.rs` runs bindgen over the installed `wpe/webkit.h` and
`wpe/headless/wpe-headless.h`, allowlisted to WebKit's and WPE's symbols and a
few GLib calls, and `ffi.rs` includes the result. A WebKit update that changes
a signature fails to compile instead of failing at runtime. `webkit.rs` wraps
the objects in types such as `Display`, `WebView`, `View`, `Toplevel`,
`Settings`, `NetworkSession` and `FilterStore`. Each owns one reference,
releases it on drop, and takes closures for signals. `engine.rs` holds the tab
logic on top and has no `unsafe`.

The browser runs in the Danube machine and links Arch's `wpewebkit` package.
This repository doesn't compile WebKit: nixpkgs packages only WebKitGTK, and a
WPE build here would recompile on every nixpkgs bump.

![Danube's main thread runs WebKit under GLib's loop and its window thread runs winit and egui; two bounded channels carry commands one way and events and frames the other; each web process has its own sandbox; tabs and menus go to derisk over the agent socket and picked commands come back](images/danube.svg)

## The window

The window is the page, with no toolbar, address bar or tab strip. Its title
names the front tab. Over the page it draws only:

- A progress line along the top while a page loads.
- An extension's popup in the top right corner.
- A bar for a site's permission request.
- The find bar along the bottom, while open.
- A notice on a page whose web process died.
- The link under the pointer.

A phone window, one whose short side is under 600 logical pixels
([GTK and Qt on a phone](gtk-qt-phone.md)), sends a mobile User-Agent with
"Android" and "Mobile", so sites serve their phone version from the next load.

### The palette as address bar

Press Ctrl+L or Alt+D to open derisk's palette. Text that reads as a web
address, such as `example.com`, `localhost:8080` or a full URL, opens in a new
tab. Anything else is searched with the desktop's search engine, set by the
[choice screen](choice-screens.md) and **Settings › Default apps**. Danube has
no search setting of its own.

![derisk's command palette with "losos desktop" typed: matching apps first, then "Search the web for losos desktop" with the chosen engine](images/choice-screens-palette-search.png)

The palette's Tabs and Extensions headings come from derisk's bundled browser
plugin, a WebAssembly palette plugin. It shows what Danube registers with
`register_palette` on the agent socket: each tab's id, title, URL and active
state, and each extension's id, name, enabled state and whether it has options.
Danube re-sends it on every change. A picked entry comes back as a `palette`
event with a command: `activate_tab`, `close_tab`, `new_tab`, `open`,
`enable_extension`, `disable_extension` or `extension_options`. `open` carries
the URL derisk built from the typed text, with `https://` for a bare host and
`http://` for localhost. An older derisk without the plugin refuses the
registration, and the menus still list the tabs.

### Menus

The menus are derisk global menus, in the top bar and in the palette, where
"Tabs › Wikipedia" switches to that tab and "Close Tab › Wikipedia" closes it.

| Menu | Contents |
| --- | --- |
| Page | The address, "Secure" or "Not secure", and how many ad-blocking filter sets apply (top bar only), then Open Address, Copy Address and Find in Page |
| Tabs | Each tab by title, then New Tab, Next Tab and Previous Tab |
| Navigate | Back, Forward, Reload and Stop |
| Bookmarks | Bookmark This Page or Remove Bookmark, then each bookmark |
| History | The last fifteen pages, newest first, then Clear History |
| Close Tab | Each tab by title |
| Extensions | Each extension's button (its popup or click action), its options page, and Enable or Disable |

Danube registers them over `$XDG_RUNTIME_DIR/derisk/agent.sock`, finds its
window by app id `org.losos.Danube` and title, and re-sends them whenever a tab
opens, closes, or changes title, address or security state. Outside a derisk
session there are no palette or menus, and only the keyboard reaches the tabs.

| Keys | Action |
| --- | --- |
| Ctrl+L, Alt+D | The palette |
| Ctrl+T | A new tab, and the palette for its address |
| Ctrl+W | Close the tab |
| Ctrl+Tab, Ctrl+Shift+Tab | The next and previous tab |
| Ctrl+R, F5 | Reload |
| Alt+Left, Alt+Right | Back and forward |
| Ctrl+F | Find in the page |
| Ctrl+D | Bookmark the page, or remove its bookmark |

A middle click opens a link in a background tab. Extension shortcuts from a
manifest's `commands` work too, such as Bitwarden's Ctrl+Shift+L to fill a
login, and Esc closes an extension popup.

Downloads go to `~/Downloads` and never overwrite a file ([Downloads](#downloads)).

## Finding text

Press Ctrl+F, or pick **Find in Page**, to open the find bar. Typing highlights
every case-insensitive match, scrolls to the first, and shows the count, up to
1000. Enter or **Next** goes forward and Shift+Enter or **Previous** back,
wrapping around. Esc or **Close** removes the bar and highlights from anywhere.
Switching tabs searches the new tab for the same text, and a page that
navigates or reloads is searched again once loaded.

## History and bookmarks

Every page a tab finishes loading goes into the history once per address,
newest first, up to 1000 pages. The title is recorded when it arrives, often
after the load. Only web pages and local files count, not new tabs, extension
pages or the captive portal sign-in. Bookmarks keep the order you added them.
Both live in `~/.local/share/danube` as `history.json` and `bookmarks.json`,
rewritten through a replacement file on each change.

The History and Bookmarks menus show them, so the palette searches them like
any menu: type part of a bookmark's title to find it. Picking one loads it in
the front tab. **Clear History** keeps the bookmarks. The captive portal window
and a background page keep neither.

## Uploading files

A page's upload button opens the desktop's file chooser through
`org.freedesktop.portal.FileChooser`. You can pick several files where the page
allows it. The page's accepted types are the first filter and all files the
second, because a page's list is only a hint.

The sandbox shows Danube only its own folders and Downloads, so a file chosen
elsewhere is handed in through the document portal. It appears under
`$XDG_RUNTIME_DIR/doc`, which inside the sandbox is the document portal's view
for `org.losos.Danube` and holds only files you chose. The web processes read
them from there too.

The portals recognise Danube the way they recognise a Flatpak. Its
xdg-dbus-proxy has a `/.flatpak-info` at its root naming the app and an
instance, and `$XDG_RUNTIME_DIR/.flatpak/<instance>/bwrapinfo.json` names the
sandbox process. Danube runs the proxy in a small sandbox with that file,
writes `bwrapinfo.json` when its sandbox starts, and removes it at the end.

## Site notifications

Sites you allow ([Sites' permissions](#sites-permissions)) show notifications
on the desktop through derisk's notification server. Each names the sending
site on its last line, so a page can't pass its words off as the system's.
Clicking one brings its tab to the front and runs the page's click handler,
dismissing runs its close handler, and a page can close its own. A
notification with the tag of one still showing replaces it. Without a
notification server, the text shows as a toast in the window.

## Downloads

WebKit decides what is a download: a response it can't show, a link with
`download`, or an extension's `downloads.download`. Once the response arrives,
Danube names the file from the server's or extension's name. For a plain GET
over HTTP or HTTPS, it stops WebKit's transfer and hands the file to
[aria2](https://aria2.github.io/) (`src/danube/crates/danube/src/downloads.rs`).
aria2 requests the final address after redirects, with what WebKit would have
sent: the cookies from WebKit's jar, HttpOnly included, the Referer and the
User-Agent. It splits the file over one connection per hardware thread, up to
16 per server (aria2's `max-connection-per-server` limit), each range at least
1 MiB. A server that caps per-connection speed delivers that many times faster.
A server without range support gets one connection.

One aria2c runs per browser, started with the first download inside Danube's
sandbox, so it reaches the network and `~/Downloads` and nothing else in your
home. Danube talks to it over JSON-RPC on a loopback port with a random secret.
aria2 reads the secret from a user-only file in the sandbox's private `/tmp`,
removed once aria2 answers, so it never appears on the command line.

> [!WARNING]
> aria2 stops with Danube (`--stop-with-process`), and any unfinished download
> is deleted.

WebKit keeps the downloads aria2 can't take: POST responses, `blob:` and
`data:` URLs, and extension pages. It also takes over any download aria2 failed
on before the first byte, such as one behind a login only the page's session
knows, and requests it again from the same tab with the tab's cookies. If aria2
can't start at all, WebKit handles every download from then on.

Each running download is a resident desktop notification with no timeout,
showing the file name, progress, speed, aria2's connections, a progress bar and
**Cancel**. It updates at most once a second. At the end a final notification
says the download finished, failed or was cancelled, and that one expires. If
you close the notification mid-download, it stays closed until the end. Without
a notification server, Danube reports the result in a toast.

## Extensions

Danube runs Chrome extensions, Manifest V2 and V3, straight from the Chrome Web
Store, and Firefox extensions written to the same API. WebKitGTK and WPE leave
the extension runtime to the browser, so Danube has its own in
`src/danube/crates/danube/src/extensions`. Bitwarden's unmodified Chrome
extension signs in to a self-hosted server, unlocks, and fills logins from its
popup, from Ctrl+Shift+L and from its menu in the page's field.

```sh
danube install-extension bitwarden.crx   # a .zip, .crx, .xpi or a directory
danube list-extensions
danube uninstall-extension nngceckbapebfimnlniiiahkandclblb
```

These commands run outside the sandbox, because the file can be anywhere, and
tell a running Danube to reload. Each extension unpacks into
`~/.local/share/danube/extensions/<id>/`, with the id Chrome would give it
(from the manifest key or the `.crx` header), so it keeps the id its server
knows. Its storage is JSON in `~/.local/share/danube/extension-data/<id>/`.
`~/.config/danube/extensions` records which extensions are off. The Extensions
menu and palette heading turn each one on or off and open its options.

How an extension runs:

- Its files come from Danube's `chrome-extension://<id>/` scheme, each page
  with the manifest's content security policy. A web page may load only the
  files listed in `web_accessible_resources`, which Danube puts on each tab's
  CORS allow list.
- Its background is a hidden web view: a background page, or a service worker
  run as a page, because WebKit has no worker to give an extension. Popups and
  offscreen documents are hidden web views too. The window draws a popup's
  frames over the page and forwards pointer and keys to it.
- Content scripts are WebKit user scripts in the extension's isolated world,
  and its CSS goes in as user style sheets. Each tab has its own content
  manager, so Danube knows which tab sent a message without trusting the page.
- `chrome.*` and `browser.*` come from `api.js`, which runs first in each of
  those contexts. Each call is a script message to the runtime on WebKit's
  thread. The runtime covers messaging and ports, tabs, storage, scripting,
  alarms, the action's popup and badge, commands, i18n, downloads,
  notifications (as Danube toasts), and offscreen documents. WebKit can only
  call into a page's main frame, so each context collects messages and events
  by sending a request that the runtime answers once there is something to
  deliver.
- A sandboxed extension page in a web page's frame, such as Bitwarden's menu in
  a login field, has an opaque origin, and WebKit won't let it load anything
  from the extension's scheme. Danube serves it with its scripts and style
  sheets inlined and adds the script hashes to its policy. When the page
  switches style sheet, for example to a theme, Danube swaps in that file
  inlined.

What an extension can't do here:

- See or change requests. `webRequest` accepts listeners and never calls them,
  and `declarativeNetRequest` keeps no rules. Blocking is
  [system-wide ad blocking](adblock.md)'s job.
- Talk to a native program with `runtime.connectNative`, so Bitwarden can't
  reach its desktop app for biometric unlock.
- Run in a page's own world (`world: "MAIN"`) anywhere but the main frame.
- Reach its other views. `extension.getViews()` is empty and
  `getBackgroundPage()` is null, because each view is a separate web process.
- Use `cookies`, `contextMenus`, `history`, `bookmarks`, the side panel or
  `identity`, which answer as if empty or fail.
- Fetch its own files from a content script unless the manifest lists them as
  web-accessible. A page couldn't either.

## Sites' permissions

Danube denies these unless you allow them: camera, microphone, screen,
location, notifications, clipboard, pointer lock, the device list, protected
(DRM) media, and cookies on other sites. It asks in a bar over the page, for
that site, once or remembered. Remembered answers are lines in
`~/.config/danube/permissions` in the form `host kind allow|deny`, which you
can edit or delete.

## Security keys

WebKit builds Web Authentication only for Apple's ports, so on Linux a page
finds no `navigator.credentials`. Danube adds it in `webauthn.rs`, so sites
such as Proton, GitHub or your bank can register and use a security key:

- `webauthn.js` runs in the top frame's page world before the page's scripts.
  It defines `navigator.credentials.create` and `get`, `PublicKeyCredential`
  and its responses per the specification, only on secure pages (HTTPS or
  `localhost`).
- `webauthn-bridge.js` runs in Danube's own world in the same frame, the only
  place the message handler exists, and relays calls and answers. Frames inside
  the page have no bridge, so an embedded site can't ask in the top site's
  name.
- `webauthn.rs` checks each call against the tab's address as WebKit knows it,
  never what the page claims. The relying party must be the page's host or a
  parent domain, never a public suffix such as `co.uk` or `github.io`. Then
  Danube builds the client data and talks CTAP2, or CTAP1 for old U2F keys, to
  every USB key, using Mozilla's `authenticator` crate from Firefox.

A bar names the site and asks you to touch your key. If the key wants its PIN,
the bar has a field and shows the remaining tries. **Cancel**, closing the tab
or leaving the page stops the request. The key's make and model reach the site
only if it asks for attestation. Otherwise Danube strips them, as the
specification allows. Danube has no built-in key and no passkey autofill, so a
site asking for a platform authenticator is told there is none.

The browser sandbox has no host `/dev`. A key's device node appears only when
plugged in, which a bind made at sandbox start can't follow, and binding all of
`/dev` would expose every device you can open. So the launcher, the `danube`
process outside the sandbox, talks to the keys. The sandbox gets one socket,
`$XDG_RUNTIME_DIR/danube-keys`, over which the browser sends each request with
the tab's address, then your PIN and Cancel. The launcher checks the request
again before any key sees it.

In a machine, the keys are the host's, lent by
[losos-tokens](tokens.md).

## Captive portals

`danube captive-watch` is a user service in every session. It reads
`nmcli monitor` and rechecks whenever NetworkManager reports a change and says
the machine is connected. The check fetches
`http://nmcheck.gnome.org/check_network_status.txt` over plain HTTP, which a
portal intercepts, sending only the request line, the host and "Danube" as
User-Agent. Anything but a 200 saying "NetworkManager is online" means a
portal. Then it opens `danube --captive-portal`: one "Sign in to network"
window with no tabs, a throwaway profile, and no content blockers, because
portal pages are often built from what they block. The window rechecks every
three seconds and closes once traffic flows. The desktop entry's **Sign in to
network** action opens it by hand.

## The desktop background

derisk can show a web page as the desktop background (**Settings ›
Wallpaper › Web page**), and Danube draws it. derisk starts
`danube --background URL --size WIDTHxHEIGHT --scale S` at the screen's size,
as it starts ffmpeg for a video wallpaper, and reads frames from stdout. For
each frame, Danube writes width and height as 32-bit little-endian numbers,
then that many premultiplied RGBA pixels
(`src/danube/crates/danube/src/background.rs`). There is no window. Any
`http://`, `https://` or `file:///` address works, and a local page can load
files next to it, because the sandbox shows its folder read-only.

The page gets nothing of yours: a throwaway profile, no extensions, every
permission refused, no sound, keyboard or pointer. It does get the content
blockers. When a window covers the screen, or in low power mode if Settings
says so, derisk writes `hide` to Danube's stdin. WebKit stops painting and
slows timers, and Danube pauses CSS animations, videos and sounds, which
WebKit's compositor would otherwise keep running. `show` resumes exactly those.
If derisk hasn't read a frame yet, the next replaces it rather than queueing. A
failed load or a dead web process ends the run with the reason on stderr, and
derisk falls back to its own background. Stdout carries only frames, and the
web processes write to stderr.

## Sandbox and hardening

Danube uses two sandboxes, both built with the
[Hakoniwa](https://github.com/souk4711/hakoniwa) crate in
`src/danube/crates/danube-sandbox`.

### The browser

Started outside the sandbox, `danube` builds a container and runs itself again
inside it. The container has:

- Its own user, mount, PID, IPC and UTS namespaces.
- The Nix store, `/etc`, the GPU driver and sysfs read-only, and `/dev/dri`.
- An empty home where only `~/.local/share/danube`, `~/.cache/danube`,
  `~/.config/danube` and `~/Downloads` are writable, with your fonts, icons and
  derisk settings visible read-only where they exist.
- An empty runtime directory with only the Wayland, PipeWire and PulseAudio
  sockets, derisk's directory, the document portal's view as `doc`, and the
  launcher's socket for [security keys](#security-keys).
- The session bus through xdg-dbus-proxy, which passes only the desktop
  portals, notifications and the accessibility bus. The proxy runs in its own
  small sandbox with the Nix store, `/etc`, the runtime directory and the
  `/.flatpak-info` that identifies Danube to the portals
  ([Uploading files](#uploading-files)).

A seccomp filter denies what Flatpak denies, such as keyctl, ptrace's
relatives, kexec and bpf, except the namespace calls the inner sandbox needs.
There is no Landlock ruleset at this level, because a process under Landlock
can neither mount nor write its user namespace's uid map, and the inner sandbox
needs both. The network is the host's, because WebKit's network process needs
it.

### Each web process

WebKit puts every web process in its own bwrap sandbox. Arch's package doesn't
accept a replacement launcher, so the machine runs that bwrap sandbox inside
its own. `danube-sandbox` can still read bwrap's arguments and build the same
sandbox with Hakoniwa for a WebKit that honours `WEBKIT_SANDBOX_LAUNCHER`. It
uses WebKit's filesystem layout and seccomp filter (loaded last) and carries in
WebKit's file descriptors. Landlock then lets the web process read what the
layout shows and write only where WebKit made it writable. Missing paths are
left out of the rules, because Hakoniwa refuses rules for missing paths and a
machine may lack `/dev/dri`. A bind already covered by an earlier one, such as
the D-Bus proxy binary inside the Nix store, is skipped too, because Hakoniwa
can't place a file bind over a file it may not write. An unrecognised argument
is an error, so if a WebKit update asks for more, its web processes fail to
start rather than run with less protection.

### WebKit itself

WebKit is built with level-3 fortify, stack clash protection, zeroed automatic
variables, eager binding (a read-only GOT), and control-flow protection where
the CPU has it: CET on x86_64, BTI and PAC on aarch64. WebDriver, the remote
inspector and unshipped experimental features such as WebXR are not built.

JavaScriptCore's JIT compilers are off by default (`JSC_useJIT=false`).
Scripts and WebAssembly run in the interpreters, which is slower, but a page
can no longer make the browser write and run machine code, which most WebKit
exploits rely on. Set `javascript.jit = true` to turn the JIT back on. WebKit
keeps libpas with its Gigacage for JavaScript and DOM objects. Everything else
goes through `malloc`, which is hardened_malloc ([The allocator](allocator.md)).
Tracking prevention (ITP) is on, and there is no keyring for WebKit to save
passwords in.

> [!WARNING]
> The sandboxes need unprivileged user namespaces, which NixOS's kernel
> allows. A phone's kernel may not ([What is not done](not-done.md)).

## Settings

Settings live in `~/.config/danube/settings.conf`, one `key = value` per line.
A missing key keeps its default. There are no NixOS options: `losos.danube.home`
and `losos.danube.jit` were removed because nothing set them.

| Key | Meaning |
| --- | --- |
| `home` | What a new tab opens (an empty page by default) |
| `javascript.jit` | `true` turns JavaScriptCore's JIT compilers on |
| `downloads.aria2` | `false` leaves downloads to WebKit's own downloader, one connection each (on by default) |
| `downloads.connections` | How many connections aria2 splits a download over (one per hardware thread by default; at most 16 go to one server) |

There is no `search` key. `losos.danube.search` went away with the address bar,
and the palette uses the desktop's engine.

## What it replaced

Danube replaced Uranium, ungoogled-chromium under the OS's name. Uranium needed
a launcher that turned it into Chrome for Android on phones, five patches, a
rebranding script, a build CI could only finish in rounds (`uranium.yml`), a
Flatpak served from the proxy, and `uranium-tabs`, a native messaging host that
mirrored its tabs into derisk's menus. Chromium draws its own interface, so it
could never be an mcsapi window or follow derisk's theme. It is not coming back.

> [!IMPORTANT]
> Profiles don't carry over. Danube starts with a new one in
> `~/.local/share/danube`, and a `~/.config/mimeapps.list` that names
> `uranium.desktop` must name `org.losos.Danube.desktop` instead.
