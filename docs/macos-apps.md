# macOS app support

macOS app support puts a Mac's apps on this desktop. Each macOS app gets
an entry in the app grid and the palette, and each of its windows opens as
a window of its own here, beside the desktop's own. It does for macOS what
[Windows app support](windows-apps.md) does for Windows.

> [!IMPORTANT]
> Apple's licence lets macOS run only on Apple hardware, in a virtual
> machine or not. macOS app support starts only on a computer whose
> firmware says Apple made it, such as an Intel Mac running LosOS. Nothing
> of macOS is in the image: it downloads from Apple the first time.

## What happens the first time

Opening **macOS app support** from the app grid runs
`macos-app-support`, which starts the person's own
`macos-app-support-setup` unit:

1. It checks that the computer is a Mac and that it has KVM.
2. It makes a token that the app server in macOS and `app-windows` here
   share, in `~/.config/app-windows/macos/token`.
3. It writes a container of the person's own, with
   [dockur/macos](https://github.com/dockur/macos) 3.12, and starts it
   through `pm container` with rootless Podman. Its disk is in
   `~/.local/share/macos-app-support`. It gets half the computer's memory
   (4 to 16 GB) and half its processors (2 to 8).
4. dockur downloads macOS 15's recovery from Apple's servers and installs
   it. That takes most of an hour.
5. macOS's own screen opens in the browser, for its Setup Assistant.

When the Setup Assistant is done, one command in macOS's Terminal
installs the app server:

```sh
sudo mount_9p shared && /Volumes/shared/install
```

The shared folder holds the installer, the token and the source of
`macos-app-server` from this image. The installer installs Apple's command
line tools and Rust if macOS lacks them, and builds the server. It then
starts the server at every login as a launch agent. macOS asks once to
allow Screen Recording and Accessibility for it.

Once the server answers on port 7180, the setup says macOS is ready, and
`app-windows` lists macOS's apps (see [App windows](app-windows.md)).

## Every time after

`macos-app-support` starts the container if it has stopped, waits for the
server, and runs `app-windows show`. Each macOS app's entry runs
`macos-app-support open ID`, which does the same and then starts the app.
A `.dmg` or `.pkg` downloaded here or opened in Files opens in macOS
([Mac packages](app-windows.md#mac-packages)).

## Graphics

Where the Mac has a GPU render node, the container gets `/dev/dri` and
dockur's `GPU=Y`. macOS then draws with
[Reims vGPU](https://github.com/steelbrain/reims-vgpu): macOS's own
`AppleParavirtGPU` driver sends Metal, which the host runs on its GPU
through Vulkan. Without a render node, QEMU's VMware adapter draws in
software. Either way, the app server captures each window with
ScreenCaptureKit and encodes it with VideoToolbox, and `app-windows`
decodes it here through VA-API where it can.

## Networking

The container uses QEMU's user network. Only the server's port, 7180, goes
into macOS, and it is bound here on 127.0.0.1, so nothing else on the
network reaches the server. The token keeps out anything else on this
computer. The web viewer, on port 8006 inside the container, goes to a free
port on 127.0.0.1.

## Apple silicon

dockur/macos runs Intel macOS, so macOS app support is for x86-64 only.
On an Apple silicon Mac, arm64 macOS runs in QEMU's vmapple machine, and
[arm64 macOS](macos-arm64.md) describes that machine. The app server and
`app-windows` are the same on both.
