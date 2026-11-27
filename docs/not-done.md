# What is not done

The known gaps, grouped by area. Each item links to the page that describes the
working part.

## Boot, install and updates

- Testing. Every CI run boots the x86_64 image in QEMU with `nix flake check`,
  under UEFI and SeaBIOS ("Tested" in [Boot loader](boot-loaders.md)). That
  covers first-boot partitioning, finding root, dm-verity `/usr`, boot counting
  and blessing a new UKI, laptop hibernate and resume with a TPM, the installer
  ISO under both firmwares, and an install from a nightly's files through to
  the installed disk's first boot. Not yet seen: a running system taking an
  update from the channel and booting it (the tests rename a UKI to fake one),
  the aarch64 image starting at all (the arm64 runner has no KVM, and emulation
  times out waiting for the disk), and any of it on a real PC.
- A complete image build. CI builds release images within its runner time
  budget. Local builds may take longer.
- A recovery shell under UEFI. The root shell from
  [When something goes wrong](troubleshooting.md#no-one-can-administer-the-machine)
  edits a BIOS entry's command line in Limine. A UEFI entry chainloads the UKI
  and has no command line, and a UKI only takes kernel arguments as a
  replacement for its whole command line. So x86_64 PCs under UEFI and aarch64
  PCs have no way to the shell since the switch from GRUB to Limine.
- A PIN at startup. Only [the installer](installer.md#asking-for-a-pin-at-startup)
  encrypts root with a TPM and PIN. A written image, first-boot setup and the
  Windows installer leave root plain, and a factory reset recreates root plain
  from `disk.nix`. The TPM slot is bound to no PCRs until the UKIs are signed.
  The VM test unlocks the installed root with `systemd-cryptsetup` next to the
  installer, but no test boots an installed system through the PIN prompt yet,
  because that needs a release to install. Settings can't change the PIN.
  Use `systemd-cryptenroll --wipe-slot=tpm2 --tpm2-device=auto --tpm2-with-pin=yes --tpm2-pcrs=`
  with the recovery key.
- Signing keys. There is no key rotation. A new key needs an update signed by
  the old key that carries both. As in the pm tree, a key belongs to whoever
  owns the machine.
- A shim of LosOS's own. [Secure Boot](secure-boot.md) uses Fedora's
  Microsoft-signed shim, because LosOS's shim has not been through
  shim-review. Each PC enrolls LosOS's key in MokManager once. The arm64 shim
  is signed only by Microsoft's 2023 CA, which older arm64 firmware may not
  trust. Releases are signed only once the `SECURE_BOOT_KEY` secret and
  `nixos/keys/secure-boot.crt` are both in place.
- Hibernation. No machine hibernates: the locked-down kernel refuses, and
  mainline Linux can't authenticate a hibernation image well enough
  ([Hibernation](hibernation.md)).
- sysext. Extensions merge into `/usr`, which holds little but the Nix store,
  so an extension can add a program but not replace one.
- No hosted proxy. GitHub Pages serves only files. Updates moved to GitHub
  Releases ([Updating from GitHub Releases](releases.md#updating-from-github-releases)),
  and CI reads the Nix cache through the proxy's code on its runner. Three
  things still need a deployment of [`proxy/`](binary-cache.md) set as
  `LOSOS_PROXY_URL`:
  - The daily `/ping` and the [choice screens](choice-screens.md) it turns on.
  - The web flasher's own downloads. Without them it sends you to the release
    page ([Where the files come from](web-flasher.md#where-the-files-come-from)).
  - A public cache URL for builders outside CI, though `proxy/serve.ts` runs
    the same cache locally.

  Systems installed from an image built with a proxy URL keep asking that
  proxy for updates until they are reinstalled or the proxy serves one more
  update.
- Choosing a cadence with a proxy. An image built with `LOSOS_PROXY_URL` names
  the proxy's `/updates/<cadence>/<arch>/` in its sysupdate transfers in
  `/usr`. Settings still lists the cadences, but `losos-update-channel` only
  exists for GitHub Releases updates, so the page says
  [the choice](releases.md#choosing-a-cadence) is not available.
- [The Windows installer](windows-installer.md) has not run on Windows. Its
  install was checked against a disk image laid out like Windows', which then
  booted. The Windows half (shrinking C:, partition table IOCTLs, mounting the
  ESP, firmware variables and BitLocker) has only been compiled. Also:
  - Secure Boot must be off. The installer registers Limine with the firmware
    directly and does not yet install shim, MokManager and the certificate
    ([Secure Boot](secure-boot.md)).
  - The `.exe` has no Authenticode signature, so SmartScreen warns.
  - Windows updates that grow the recovery partition, as KB5034441 did, shrink
    C: to make room. With LosOS behind C: they fail, as on any disk with no
    free space after C:.
  - Some firmware and Windows updates put Windows Boot Manager first again.
    LosOS moves its entry back each time it starts. Until then Windows boots by
    default, and LosOS is in the firmware boot menu.
  - It is x86_64 only, because Windows on arm64 runs on laptops this OS has no
    kernel configuration for.
- [Installer reports](installer.md#reporting-a-failed-install) cover only a
  failed install. The backend writes them, so nothing is reported when the
  backend itself stops or derisk crashes. A report needs a GitHub account and a
  second device or browser to approve the sign-in. The GitHub App and
  `LOSOS_REPORT_CLIENT_ID` are set up, but no report has reached the real
  GitHub: the tests sign in to a stand-in.

## Hardware

- NVIDIA. The [driver choice](drivers.md) was tested in a VM with a report
  naming an RTX 4090. No NVIDIA card has run the image. Untested: the open
  module taking a card, derisk's compositor on it, explicit sync, suspend,
  VA-API, hardened_malloc under NVIDIA's libraries, and dual-GPU laptops. With
  an NVIDIA card and another GPU, derisk draws on the boot display, and nothing
  offloads apps to the other.
- One display. `derisk display-manager` starts `derisk greeter` (the lock
  screen as login screen) and, after login, `derisk session --execute`
  (`nixos/modules/desktop.nix`). Each takes the seat from logind and scans out
  through DRM/KMS on the first connected display at its preferred mode. A
  second display stays dark, and hotplug is not handled. X11 apps run through
  xwayland-satellite, a user unit started with the session. It has served an
  X11 client under headless weston, not yet under derisk's compositor.
- Wi-Fi, Bluetooth and power modes in Settings. The **Network** page uses
  NetworkManager, which lets the `networkmanager` group change networks.
  First-boot setup adds its account to that group. An older account isn't in
  it, and its Network page says so until
  `homectl update --member-of=wheel,networkmanager <user>` adds it. The
  **Bluetooth** page pairs without prompting, so devices that need a passkey
  typed or compared don't pair. None of the three pages has run against a real
  adapter or `power-profiles-daemon` on hardware. The palette's Power Mode rows
  ran against the daemon's placeholder driver on a private bus, which has no
  Performance profile. The Halium GSI has no Bluetooth, because nothing bridges
  the vendor HAL to BlueZ.
- gnome-control-center is gone, with its two patches. What they added, the
  security report and factory reset, is on the **System** page of derisk
  Settings. Only UEFI machines offer factory reset, because systemd's reset
  request needs UEFI. Neither has been tried on hardware.
- [Peripherals](peripherals.md) in their machines:
  - Bluetooth headphones pair and connect in sys-bluetooth, but there is no
    PipeWire there and no path for their sound to the host.
  - Cameras reach host apps at 1280x720 only, and a camera's microphone goes to
    its machine, where nothing carries the sound to the host.
  - A printer's scanner goes to its machine, and nothing there scans. A printer
    with no Gutenprint or CUPS driver that speaks neither IPP over USB,
    PostScript nor PCL gets no queue.
  - A keyboard or mouse on a port sys-usb holds reaches the host over USB/IP,
    so the host parses its HID descriptors. Qubes's input proxy, which passes
    only key and pointer events, is not done.
  - A Bluetooth radio on UART or SDIO, as on some Arm laptops, stays on the
    host, and Settings doesn't see it.
  - All of it has run only in VMs: the router, sys-usb and a receiving machine
    nested in QEMU, and controller passthrough through QEMU's emulated IOMMU
    (`nixos/tests/peripherals.nix`). None of it has met real USB controllers or
    a real Bluetooth radio.

## Machines

- The guest template. Only a reduced [guest](hypervisor.md), without Danube,
  Flatpak and the Calculator, has booted, and no machine window has reached a
  desktop through it yet. It is a second NixOS in `/usr` with its own copies of
  store paths the host also has (Mesa, Danube, fonts), taking room from the
  8 GiB `/usr` slot. Sharing the host's store over virtiofs would save that.
  A machine's disk grows once, to its creation size, and nothing enlarges an
  existing disk. Portal dialogs in a machine now come from the
  [portal machine](portal-machine.md), untried with a real app.
- Labels on the network cover `virbr0` only. A machine on a network made in
  virt-manager is not filtered, and on a PC with a public address a red machine
  is not kept from it. A new label applies at the machine's next start
  ([Labels on the network](hypervisor.md#labels-on-the-network)).
- The Flatpak machine. Flatpaks move off the host only for administrators,
  because anyone else would get an unexpected password dialog. Other users copy
  their `~/.var/app` in by hand, and the host copy stays until its owner
  deletes it. Flatpak apps in the machine draw in software, so Flathub games
  run slower than on the host.
- Windows and other distributions. No Windows has been installed from the
  Windows tools CD, so its answer file, drivers and first sign-in have only
  been read. The Linux tools CD's `setup.sh` has not run in any distribution.
  The desktop doesn't show a machine's own screen by itself yet, though
  `virt-viewer --attach` does
  ([Machines from an installer CD](hypervisor.md#machines-from-an-installer-cd)).
- [The network machine](network-machine.md) and [the files machine](files-machine.md):
  - VFIO card handover has only run in QEMU. The VM test runs the no-IOMMU
    fallback.
  - sys-net forwards IPv4 only, so the host has no IPv6 through it. The edge
    tunnel is IPv4 only too.
  - The keyboard's Wi-Fi and airplane keys reach the host's rfkill, not the
    machine's. The Wi-Fi switch in Settings works.
  - The edge exit has a command, `losos-edge`, but no row in Settings. sys-net
    checks no gateway's passport, so an edge on the network is a gateway on its
    own word. It is only ever the way to the official edges it lists, never the
    exit.
  - The [VPN](vpn.md) has a command, `losos-vpn`, but no row in Settings, and
    the host learns only whether sys-vpn runs, not whether its tunnel is up.
  - In schizo mode with a VPN, the provider's endpoint must be an address, not
    a name: sys-vpn would ask Tor for the name, and Tor goes out through
    sys-vpn. Windscribe's files use names.
  - Without an IOMMU, the host's NetworkManager keeps the saved networks and
    sys-net keeps none.
  - Network shares have a command, `losos-shares`, but no page in Settings or
    Files. WebDAV has not been tried against a server.
  - `sftp-server` in sys-files is not chrooted to the volume it serves. Only
    the host can ask for a volume.
- [The portal machine](portal-machine.md):
  - Its dialogs open as separate windows, not attached to the app's window,
    which is on another machine's display.
  - Write-back permissions last until the host restarts. After that, saving a
    copy a machine got earlier changes only the copy.
  - A folder picked for a machine syncs back file by file. An empty folder
    reaches the host only once it has a file, and links made in the copy stay
    in the machine.
  - Changes on the host to a picked folder after the copy don't reach the
    machine.
  - Only the files and screenshot portals copy files. Other portals that pass a
    file, such as the Account portal's picture, give a machine's app a path it
    can't open.
- The LosOS boxes machine routes other machines' connections to a box only on
  ports 80 and 443, by name. An app's own port on a box is reached directly,
  and a machine that knows a box's address can bypass the
  [LosOS boxes](losos-boxes.md) machine. Its keyring has no sign-in to unlock
  it, so the first `losos` command that stores a key or token asks for a
  keyring password in a machine window. Nothing creates the machine
  automatically: you create it from the template. It has not met a real box on
  a real LAN: the machines test boots a stand-in box, and the real box in
  `nixos/tests/losos-box.nix` is found by a desktop with no machine. Away from
  home only `losos`, palette search and LosOS Cloud's link reach a box, through
  its edge. Backups and other machines' connections need the box's own network.
- [Backups to a LosOS box](backup.md) keep one copy of repositories. LosOS
  cloud keeps old versions and the trash, but LosOS Git keeps only each
  branch's latest state, and only the configs repository has history. A
  repository deleted at home stays on the box. Symbolic links are recorded for
  restore, and Git LFS objects are uploaded. A box with HTTPS turned off
  (`losos.tls.enable`) can't be backed up to. Only an administrator's run can
  start a stopped LosOS boxes machine. A box older than LosOS Login's backup
  sign-in offers password sign-in to LosOS cloud, and without a LosOS Git token
  repositories go to LosOS cloud as files. The test uses both a stand-in and a
  real box.
- [The gaming machine](gaming-machine.md) needs a second graphics card:
  - A PC with one card can't hand it over and take it back while the desktop
    waits on the processor.
  - Settings has no page yet. Use `losos-gaming` and
    `pm system start losos-gaming-reserve@<address>`.
  - It hasn't run with a real card: the VM test defines the machine and checks
    the boot unit without a card to move.
  - Disks and other devices still carry QEMU's names, which some anti-cheats
    look for.

## Apps

- [Danube](danube.md) extensions can't see requests through `webRequest`, talk
  to native programs, or use cookies, context menus, history, bookmarks or the
  side panel (see its "Extensions" section). Stopped downloads can't resume.
  Its sandboxes need unprivileged user namespaces. A phone kernel may refuse
  them, and then Danube stops rather than run a page unsandboxed, so it doesn't
  open at all. `--no-sandbox` skips only the outer sandbox. Danube has run with
  derisk in a VM, not on a phone.
- [Ad blocking](adblock.md) applies a filter list's URL and element rules in
  Danube only. Other apps get only blocked domains, and a program with its own
  DNS over HTTPS skips it.
- [Encrypted DNS](adblock.md#encrypted-dns) hides names from the network but
  not from the resolver (Quad9 or Mullvad by default). Queries are not padded
  (RFC 8467), so their sizes hint at the names, and the TLS fingerprint is
  rustls', not a browser's. Only Tor hides names from both.
- Slovak eID beyond signing in. The eID klient runs in a machine and signs in
  to slovensko.sk ([The Slovak eID](tokens.md#the-slovak-eid)). D.Signer, in
  the same download, is not set up, and the klient is x86_64 only. Nobody has
  signed in with a real card through a machine, or to the computer itself
  ([Signing in with the eID](tokens.md#signing-in-with-the-eid)). Settings has
  no row for eID sign-in. `losos-eid-signin` sets it up.
- [The Agent](agent.md). The image carries neither Claude Code (unfree in
  nixpkgs) nor the Rust toolchain for previews. You install `claude`, and
  `cargo` with the `wasm32-wasip2` target, first. The Agent works on a copy of
  the folder you pick, so host changes appear only when you pick the folder
  again. The machine has no Nix or pm, so `nix_build` and `pm_build` fail
  there. Files the Agent opens, such as an artifact, stay in the machine,
  because only web links open on the desktop. The machine draws in software.
  Nothing shows it running or stops it, except closing the Agent and waiting
  ten minutes.
- File content search. `derisk-index` reads plain text, PDFs, OpenDocument and
  Office Open XML. It can't read text in pictures or scanned PDFs (that needs
  OCR) or old binary `.doc`, `.xls` and `.ppt`. Those are found by name only.
  It indexes the first MiB of each file's text, and skips hidden files and
  anything `.gitignore` excludes unless Settings names their folder.
- A default for text files. Since Sonne left, nothing claims text and source
  files for `xdg-open`, so Files opens them in whatever app lists the type, if
  any. derisk's Text Editor can't be handed a file, because its desktop entry
  lives inside the derisk binary with no file argument and no types.
- [Windows app support](windows-apps.md) hasn't set up Windows on a real PC.
  The setup, `pm container` and rootless Podman ran, but dockur's Windows needs
  KVM, which the build sessions lacked. Its windows are FreeRDP X11 windows
  through the session's xwayland-satellite, so the desktop doesn't frame them
  in a colour. With no GPU, Windows draws apps and encodes H.264 in software.
  USB passthrough, which WinBoat does only with Docker, is not offered. Windows
  is not activated.
- [App windows](app-windows.md) has run on GitHub's Macs, from scripts and from
  LosOS booted in a VM on them. Pictures go to the compositor through shared
  memory, not GPU buffers. The pointer is always the arrow, only text crosses
  the clipboard, and drag and drop and sound stay in the guest. A Mac package
  downloaded in another machine stays there. Only this desktop hands packages
  to macOS.
- [macOS app support](macos-apps.md) hasn't set up macOS. That needs a Mac
  running LosOS, because Apple's licence allows macOS only on Apple hardware,
  and CI's Macs run macOS. The installer inside macOS builds the app server
  with Rust and Apple's command line tools instead of installing a prebuilt
  one. Reims vGPU hasn't drawn macOS here.
- arm64 macOS. The image doesn't build `qemu-vmapple` or Reims, and stock QEMU
  has no `vmapple` machine. Nobody has booted macOS on the old port, and
  nothing in derisk starts it ([arm64 macOS machines](macos-arm64.md)).
- [Android app support](android-apps.md) boots its Pixel and opens an app's
  display in CI, on a KVM runner with no GPU, drawing with `-gpu host` through
  Mesa's llvmpipe. No CI run has had a real GPU, and Vulkan through gfxstream
  is only on with one ([The GPU](android-apps.md#the-gpu)). Google Play needs
  you to sign in to a Google account on the Pixel first. The Pixel has no
  camera and no USB passthrough. Its windows have no colour, because they come
  from scrcpy on the desktop, not from a machine. Google builds no emulator for
  Linux on ARM, so aarch64 PCs and the GSI have no Android apps.
- [Focus mode](focus-mode.md) pauses background work, but the desktop's bar
  and dock stay visible: a program's full-screen request doesn't hide them yet.
- [pm](pm.md) on a NixOS host. pm's jail mirrors the host's `/bin`, `/lib` and
  `/usr` and mounts `/nix/store` and the store-backed `PATH` directories
  read-only, so steps whose tools come from the store run. A `losos-nix` step
  running `nix eval` and `nix hash` worked that way on a Nix-provisioned host.
  A recipe that names FHS paths such as `/bin/cat` or a compiler under `/usr`
  finds nothing, and every recipe in the old pm tree did that. Here `/usr` is
  the Nix store partition and `/bin` holds only `sh`. pm itself runs
  (`pm --help`, `pm source-path`, signing, `explain`), and the boot test checks
  it is installed, but FHS recipes don't build. Seven of pm's test targets are
  skipped in the Nix build for the same reason. `nixos/pkgs/pm.nix` lists them.
- [Desktop reports](reporting.md) see only some problems. A window counts as
  hung only when it misses the ping a click or key sends, so an untouched hung
  window goes unnoticed. Only Settings reports an action that never finished,
  and only for Bluetooth pairing, a backup and a factory reset request. Nothing
  is reported when derisk itself crashes.
- derisk's portal. Screen sharing through xdg-desktop-portal-wlr copies frames
  to shared memory, not dma-bufs. A shared window is cut out of the screen, so
  anything covering it is shared too. The colour and area pickers show a frozen
  screenshot. They were tried under X11, not in a derisk session, and neither
  was the lock screen's picture.

## Phones

The [Halium GSI](halium.md) has not run on a device. It evaluates for aarch64,
and an x86_64 build of the same boot path booted under QEMU. No arm64 build has
been flashed, no bootloader has loaded its `init_boot`, and the web flasher has
only talked to a simulated fastboot device. Only a phone can confirm that a
generic kernel has everything systemd and the desktop use (`CONFIG_USER_NS` for
Flatpak's sandbox above all), that `vendor_boot`'s ramdisk and this one unpack
together, and that Halium's Android 14 system image starts each vendor's HALs.
Beyond that:

- Display. derisk drives the display through DRM/KMS. A device whose kernel has
  a DRM driver (msm, mediatek, panfrost) can show the desktop with Mesa. A
  device with only Android's hwcomposer can't, until derisk gets a hwcomposer
  backend or a compositor sits in front of it.
- Updates. sysupdate is not wired up. The flasher writes userdata whole, so a
  newer release erases the phone. Root has no verity, and the bootloader stays
  unlocked.
- Phones without `init_boot`. The flasher writes their boot image back with
  this ramdisk after theirs. Its repacking matches AOSP's `mkbootimg` byte for
  byte for header versions 0 to 4 and ran against a simulated phone in
  fastbootd, but no real bootloader has booted such an image. A phone whose
  ramdisk links `/etc` or `/lib` elsewhere would merge badly.
- Kernels older than 5.10. Phones that launched with Android 11 or earlier run
  4.x or 5.4 kernels, which systemd doesn't support. The flasher reads the
  kernel version and refuses them.
- hardened_malloc. The GSI's [allocator](allocator.md) is built for a 39-bit
  address space and tested by the initrd before preloading, but no phone kernel
  has run that test. A phone with 16 KiB pages fails it and keeps glibc's
  malloc. derisk gets hardened_malloc only through the preload there, without
  the length check on Rust's frees that a PC's derisk has.
- Telephony, audio, sensors, camera. There is no ofono, no PulseAudio or
  PipeWire droid modules, and no sensorfw. Android's init starts the HALs, but
  nothing on the Linux side talks to them yet beyond EGL.
