# What is not done

- **arm64 macOS.** `qemu-vmapple` runs `vmapple` under TCG with an M1 CPU
  on a PC, or under KVM on an arm64 host, with a GPU from Reims. Nobody has
  booted macOS on it yet, no guest has drawn through Reims, and nothing in
  derisk starts it. [arm64 macOS machines](macos-arm64.md) lists the rest.
- **Peripherals in their machines.** [Peripherals](peripherals.md) moves
  USB devices and Bluetooth radios into machines of their own. Some of it
  is not finished:
  - Bluetooth headphones pair and connect in sys-bluetooth, but there is
    no PipeWire there and no path for their sound to the host's.
  - A camera or sound card in its disposable machine is driven there, but
    nothing yet carries its video or sound back to apps on the host.
  - A keyboard or mouse plugged into a port sys-usb holds comes to the
    host over USB/IP, so the host parses its HID descriptors. Qubes's input
    proxy, which passes only key and pointer events, is not done.
  - A Bluetooth radio that is not on USB, on a UART or SDIO as on some
    Arm laptops, stays the host's, and Settings does not see it.
  - Controller passthrough has only run under QEMU, never on a PC's own
    USB controllers.
- **The network and files machines.** [The network machine](network-machine.md)
  and [the files machine](files-machine.md) have gaps:
  - Handing a network card over by VFIO has only run in QEMU. The VM test
    runs the fallback for a PC without an IOMMU.
  - sys-net forwards IPv4 only. The host has no IPv6 while it routes
    through it.
  - The keyboard's Wi-Fi and airplane keys reach the host's rfkill, not
    the machine's. Settings' Wi-Fi switch works.
  - The edge proxies are not in sys-net yet.
  - Without an IOMMU, NetworkManager on the host keeps the networks the
    host has saved, and sys-net keeps none of its own.
  - Network shares have a command, `losos-shares`, and no page in
    Settings or Files. WebDAV shares have not been tried against a
    server.
  - OpenSSH's `sftp-server` in sys-files is not chrooted to the volume it
    serves. The host is the only one that can ask for one.
- **The portal machine.** [The portal machine](portal-machine.md) has
  gaps:
  - Its dialogs are not attached to the app's window, which is on
    another machine's display, so they open as windows of their own.
  - What a machine may write back is kept until the host restarts. After
    that, saving a copy a machine was given before changes only the copy.
  - A folder picked for an app in a machine goes back file by file. An
    empty folder made in the copy reaches the host only once a file is
    put in it, and a link made in the copy stays in the machine.
  - Changes made to a picked folder on the host after it was copied do
    not reach the machine's copy.
  - Only the files and screenshot portals copy files. A portal that hands
    an app a file in another way, such as the Account portal's picture,
    hands a machine's app a path it cannot open.
- **pm on a NixOS host.** pm's jail mirrors the host's `/bin`, `/lib` and
  `/usr`, and also mounts `/nix/store` and the store-backed `PATH`
  directories read-only. So a step whose tools come from the store runs. A
  `losos-nix` step running `nix eval` and `nix hash` worked that way in pm's
  jail on a Nix-provisioned host. A recipe that names FHS paths, such as
  `/bin/cat` or a compiler under `/usr`, still finds nothing, and every recipe
  in the old pm tree did that. In this image `/usr` is the Nix store's
  partition and `/bin` holds only `sh`. So pm itself runs here, including
  `pm --help`, `pm source-path`, signing and `explain`, and the boot test
  checks it is installed. A recipe written for an FHS host does not build on
  it, though. For the same reason, seven of pm's test targets are skipped in
  the Nix build, and `nixos/pkgs/pm.nix` lists them.
- **The Agent's tools.** The image carries neither Claude Code, which nixpkgs
  marks unfree, nor the Rust toolchain that the [Agent](agent.md)'s previews
  build with. A chat needs `claude`, and a preview needs `cargo` with the
  `wasm32-wasip2` target, both installed by the person first.
- **A default for text files.** Since Sonne left, nothing claims text and
  source files as their default for `xdg-open`. Files opens them in whatever
  app happens to list their type, if any. derisk's Text Editor cannot be
  handed a file, because its desktop entry lives inside the derisk binary
  with no file argument and no types.
- **Searching what is in every file.** `derisk-index` reads plain text only.
  A file with a NUL byte in its first 8 KiB counts as binary and is found by
  its name alone, so the text in PDFs, office documents and pictures is not
  searched. Only the first MiB of a large text file is indexed. Hidden files
  and anything `.gitignore` leaves out are not indexed, and there is no
  setting yet to index other folders or turn the index off.
- **The guest template in full.** Only a reduced build of the
  [guest](hypervisor.md), without Danube, Flatpak and the Calculator, has
  booted, and no machine's window has reached a desktop through it yet. It is
  a whole second NixOS in `/usr`, so it carries its own copy of store paths
  the host also has, such as Mesa, Danube and fonts, and takes that room from
  the 8 GiB `/usr` slot. Giving machines the host's store over virtiofs would
  save it. A machine's disk grows once, to the size it was made with, and
  nothing makes an existing machine's disk larger. Portal dialogs in a
  machine, such as a Flatpak's file chooser, are the
  [portal machine](portal-machine.md)'s now, and nobody has tried them
  with a real app in a machine.
- **The Flatpak machine's loose ends.** A `.flatpakref` or `.flatpakrepo`
  file opened on the host has nothing to open it, because Flatpak is in
  [the machine](hypervisor.md#the-flatpak-machine), so it has to be copied
  there first. Flatpak's files move off the host only for administrators,
  because anyone else would get a password dialog they did not ask for.
  Other users copy their `~/.var/app` into the machine by hand. The host's
  copy of `~/.var/app` stays after the copy, until its owner deletes it.
  Flatpak apps inside the machine draw in software, like the rest of its
  apps, so a game from Flathub runs slower than it did on the host.
- **Windows and other distributions in machines.** No Windows has been
  installed from the Windows tools CD yet, so its answer file, its drivers and
  its first sign-in have only been read, not run. The Linux tools CD's
  `setup.sh` has not run in any distribution either. The desktop does not yet
  show a machine's own screen by itself, though `virt-viewer --attach` does.
  [Machines from an installer CD](hypervisor.md#machines-from-an-installer-cd)
  describes both CDs.
- **Windows app support.** [Windows app support](windows-apps.md) has not
  set up Windows on a real PC yet. The setup, `pm container` and rootless
  Podman ran here, but dockur's Windows needs KVM, which the sessions this was
  built in lack. Its windows are FreeRDP's X11 windows, drawn through the
  session's one xwayland-satellite, so the desktop does not frame them in a
  colour of their own. It has no GPU, so Windows draws its apps and encodes
  their H.264 in software. USB passthrough, which WinBoat does only with
  Docker, is not offered. Windows is not activated.
- **App windows.** [App windows](app-windows.md) has run on GitHub's Macs,
  driven by scripts and from LosOS booted in a VM on them. Pictures go
  to the compositor through shared memory rather than the GPU's own
  buffers. The pointer is always the arrow, only text crosses the
  clipboard, and drag and drop and sound stay in the guest. A Mac package
  downloaded in another machine stays there; only this desktop's own hand
  packages to macOS.
- **macOS app support.** [macOS app support](macos-apps.md) has not set
  up macOS yet. That needs a Mac running LosOS, because Apple's licence
  lets macOS run only on Apple hardware, and CI's Macs run macOS rather
  than LosOS. The installer inside macOS builds the app server with Rust
  and Apple's command line tools, which macOS downloads, rather than
  installing a built one. Reims vGPU has not drawn macOS here.
- **No hosted proxy.** GitHub Pages serves files only. Of what
  [`proxy/`](binary-cache.md) did, updates moved to GitHub Releases, through
  an index on the documentation site and a service on each machine
  ([Releases](releases.md#updating-from-github-releases)). CI reads the Nix
  cache through the proxy's own code on its runner. Three things still need a
  deployment that runs code, set as `LOSOS_PROXY_URL`:
  - the daily `/ping` and the [choice screens](choice-screens.md) it turns
    on, which stay off without one;
  - the web flasher's own downloads, so without one it sends the person to
    the release page ([Web flasher](web-flasher.md#where-the-files-come-from));
  - a public cache URL for a builder outside CI, though `proxy/serve.ts` runs
    the same cache locally.

  Systems installed from an image built with a proxy URL keep asking that
  proxy for updates until they are reinstalled, or until the proxy serves
  them one more update.
- **A cadence picked on a machine that updates from a proxy.** An image built
  with `LOSOS_PROXY_URL` names the proxy's `/updates/<cadence>/<arch>/` for
  its own cadence in its sysupdate transfers, which are part of `/usr`.
  Settings still lists the cadences there. But `losos-update-channel` only
  exists where updates come from GitHub Releases, so the page says
  [the choice](releases.md#choosing-a-cadence) is not available.
- **A complete image build.** CI builds release images within its runner
  time budget. Local image builds may take longer.
- **Hibernation.** No machine hibernates. The locked-down kernel refuses to,
  and nothing in mainline Linux authenticates a hibernation image well
  enough to let it. [Hibernation](hibernation.md) has the details.
- **Signing.** There is no key rotation. A new key needs an update signed by
  the old one that carries both. Secure Boot signing of the UKI is not done
  either. As in the pm tree, a key belongs to whoever owns the machine.
- **[The Windows installer](windows-installer.md) has not run on Windows.**
  Its install was checked against a disk image laid out like Windows', which
  then booted. The half that talks to Windows has only been compiled. That
  half shrinks C:, issues the partition table IOCTLs, mounts the ESP, and
  handles firmware variables and BitLocker. Beyond that:
  - **Secure Boot must be off**, for the reason above.
  - **SmartScreen.** The `.exe` has no Authenticode signature, so Windows
    warns before running it.
  - **Windows' recovery partition.** Updates that grow it, as KB5034441 did,
    shrink C: to make room behind it. With LosOS behind C: they cannot, and
    they fail as they do on any disk with no free space after C:.
  - **The boot order.** Some firmware, and some Windows updates, put Windows
    Boot Manager first again. LosOS puts its own entry back in front each
    time it starts. Until it next does, Windows starts by itself, and LosOS
    is one pick away in the firmware's boot menu.
  - **arm64.** The installer is built for x86_64 only, because Windows on
    arm64 runs on laptops this OS has no kernel configuration for.
- **[Installer reports](installer.md#reporting-a-failed-install) cover a
  failed install only.** The backend writes them, so nothing is reported when
  the backend itself stops, or when derisk crashes. A report needs a GitHub
  account and a second device, or a browser, to approve the sign-in. The
  GitHub App and `LOSOS_REPORT_CLIENT_ID` are set up, but no report has
  reached the real GitHub yet: the tests sign in to a stand-in.
- **[Desktop reports](reporting.md) have no button yet.** `losos-report` is
  in the image and writes and sends them, but derisk does not run it yet:
  the "isn't responding" card, the failed services' report rows and the
  report page in the session come with derisk's side of the work.
- **The two gnome-control-center patches** in `nixos/pkgs/patches/`. Nothing
  upstream has them, so they stay. They are written against
  gnome-control-center 51.0, and nixpkgs carries 50.4, which moved the System
  panel to Blueprint. Neither patch applies, and gnome-control-center is no
  longer in the image for them to go into. What they added is on derisk
  Settings' System page instead: the security report, and the factory reset.
  Only a UEFI machine offers the factory reset, because systemd's request to
  reset needs UEFI. Neither has been tried on hardware.
- **NVIDIA on real hardware.** The [driver choice](drivers.md) was tested in a
  VM with a report that names an RTX 4090. No NVIDIA card has run the image.
  The open module taking a card, derisk's compositor on it, explicit sync,
  suspend, VA-API, hardened_malloc under NVIDIA's libraries, and laptops with
  two GPUs are all untested. A machine with an NVIDIA card and another GPU
  draws on whichever is the boot display, and nothing offloads an app to the
  other.
- **derisk drives one display.** `derisk display-manager` starts
  `derisk greeter`, derisk's lock screen used as the login screen, and after
  a login, `derisk session --execute`. Each takes the seat from logind and
  scans out through DRM/KMS on the first connected display, at its preferred
  mode, as set up in `nixos/modules/desktop.nix`. A second display stays dark,
  and hotplug is not handled. X11 apps run through xwayland-satellite, a user
  unit that starts with the session, also in `desktop.nix`. It has been seen
  serving an X11 client under a headless weston, but not yet under derisk's
  compositor.
- **derisk's portal has gaps.** Screen sharing through
  xdg-desktop-portal-wlr copies frames into shared memory, not dma-bufs. A
  shared window is cut out of the screen, so anything covering it is shared
  too. The colour and area pickers show a screenshot frozen across the
  screen. They were tried under X11, not yet in a derisk session, and the
  lock screen's picture has not been tried in one either.
- **The Halium GSI has not run on a device.** It evaluates for aarch64, and an
  x86_64 build of the same boot path booted under QEMU, as the pull request
  that made it the one target shows. No arm64 build has been flashed, no
  bootloader has loaded its `init_boot`, and the web flasher has only talked
  to a simulated fastboot device. Only a phone can confirm three things: that
  a generic kernel's configuration has everything systemd and the desktop
  use, `CONFIG_USER_NS` for Flatpak's sandbox above all; that `vendor_boot`'s
  ramdisk and this one unpack together as intended; and that Halium's
  Android 14 system image starts each vendor's HALs. Beyond that:
  - **The display.** derisk drives the display itself through DRM/KMS. A
    device whose kernel has a DRM driver, such as msm, mediatek or panfrost,
    can show the desktop with Mesa. A device that only has Android's
    hwcomposer cannot, because nothing here drives hwcomposer. That needs a
    hwcomposer backend in derisk or a compositor in front of it.
  - **Updates.** sysupdate is not wired up. The flasher writes userdata
    whole, so installing a newer release erases the phone. There is no
    verity on the root either, and the bootloader stays unlocked.
  - **Phones without `init_boot`.** The flasher writes their own boot image
    back with this ramdisk after theirs. Its repacking matches AOSP's
    `mkbootimg` byte for byte for header versions 0 to 4, and it was run
    against a simulated phone in fastbootd. No real bootloader has booted
    such an image yet, and a phone whose ramdisk links `/etc` or `/lib`
    elsewhere would merge badly with this one.
  - **Kernels older than 5.10.** Phones that launched with Android 11 or
    earlier run 4.x or 5.4 kernels, which systemd does not support at all.
    The flasher reads the kernel's version and refuses them. Supporting them
    would need an older systemd or a kernel built per device.
  - **hardened_malloc.** The PC build preloads it into every process, and the
    GSI does not. Its default configuration reserves 32 GiB per size class
    per arena and needs a 48-bit address space, but Android kernels are
    usually built with 39-bit virtual addresses. The GSI needs a
    hardened_malloc with a smaller `CONFIG_CLASS_REGION_SIZE`, and derisk
    built against it.
  - **Telephony, audio, sensors, camera.** There is no ofono, no
    PulseAudio or PipeWire droid modules, and no sensorfw. Android's init
    starts the HALs, but nothing on the Linux side talks to them yet beyond
    EGL.
- **Wi-Fi, Bluetooth and power modes in Settings.** derisk Settings' Network
  page lists, joins and forgets networks through NetworkManager, which lets
  the `networkmanager` group change them. First-boot setup puts the account
  it creates in that group. An account made before setup did that is not in
  it, and its Network page says so until
  `homectl update --member-of=wheel,networkmanager <user>` adds it. The
  Bluetooth page pairs without asking anything, so a device that wants a
  passkey typed or compared does not pair. None of the three pages has run
  against a real adapter or against `power-profiles-daemon` on hardware. The
  command palette's Power Mode rows ran against the daemon's placeholder
  driver on a private bus, which offers no Performance profile. The Halium
  GSI has no Bluetooth, because the phone's controller sits behind its vendor
  HAL and nothing here bridges that to BlueZ.
- **Two LosOS disks in one PC.** Root comes from the disk the [boot
  loader](boot-loaders.md) started from, under UEFI and a BIOS alike. Swap,
  though, is found by its partition label, `losos-swap`, in `disk.nix` and
  `hibernate.nix`. With two LosOS disks in one machine, swap may come from the
  other disk.
- **A recovery shell on an aarch64 PC.** The boot menu's root shell for a
  machine nobody can administer, described in [When something goes
  wrong](troubleshooting.md#no-one-can-administer-the-machine), starts the
  kernel kept beside the UKI for a BIOS boot. Only x86_64 images carry that
  kernel. An aarch64 PC's GRUB can only chainload its UKI, and a UKI takes
  kernel arguments only as a replacement for its whole command line.
- **sysext.** Extensions merge into `/usr`, which here holds little but the
  Nix store, so an extension can add a program but cannot replace one.
- **Android app support.** [Android app support](android-apps.md) boots
  its Pixel and opens an app's display in CI, on a runner with KVM and no
  GPU. CI's Pixel draws with `-gpu host` through Mesa's llvmpipe, the same
  path a GPU's driver takes, but no CI run has had a real GPU, and Vulkan
  through gfxstream is only on with one ([The GPU](android-apps.md#the-gpu)).
  Google Play needs a person to sign in to
  a Google account on the Pixel before it installs anything. The Pixel has
  no camera, and no USB passthrough. Its windows have no colour of their
  own, since they come from scrcpy on the desktop rather than from a
  machine. Google builds no emulator for Linux on ARM, so aarch64 PCs and
  the GSI have no Android app support.
- **Booted under QEMU only.** Every CI run boots the x86_64 image in QEMU
  with `nix flake check`, under UEFI and under SeaBIOS, as "Tested" in
  [Boot loader](boot-loaders.md) describes. First boot lays out the disk with
  systemd-repart. Root is found, by gpt-auto on UEFI and on the disk GRUB
  names on a BIOS. `/usr` is dm-verity, and a new UKI is counted down and
  blessed. A laptop with a TPM hibernates and resumes. The installer ISO
  starts under both firmwares. An install from a nightly's files was run in
  QEMU through to the installed disk's first boot, so sysupdate has written
  a disk other than the running one. Three things have not been seen yet. A
  running system has not taken an update from the channel and booted it; the
  tests rename a UKI to stand in for a new one. The aarch64 image has not
  started at all, since the arm64 runner has no KVM and the aarch64 VM under
  emulation times out waiting for its disk. And none of it has run on a PC
  instead of in QEMU.
- **Danube is unfinished in places.** A page's file upload button opens
  nothing, because [Danube](danube.md) does not answer WebKit's file chooser
  request yet. A site's notifications are never shown, even when allowed.
  Extensions cannot see requests through `webRequest`, talk to a native
  program, or use cookies, context menus, history, bookmarks or the side
  panel, as Danube's "Extensions" section lists. There are no bookmarks, no
  history list, no find in page, and no way to resume a download. Its
  sandboxes need unprivileged user namespaces, and a phone's own kernel may
  refuse them. Danube then stops instead of running a page outside a
  sandbox, so on such a phone it does not open at all. `--no-sandbox` skips
  only the outer sandbox. Danube has run against WPE WebKit in a session,
  but not yet on a desktop or phone with derisk.
- **Ad blocking is DNS-wide only for names.** A filter list's URL and element
  rules apply in Danube alone. Other browsers and apps get only whole
  [blocked domains](adblock.md), and a program
  with its own DNS over HTTPS skips it entirely.
- **The LosOS boxes machine passes on names, not everything.** It routes the
  other machines' connections to a box only on ports 80 and 443, by the name
  they ask for. An app's own port on a box is reached directly, and nothing
  stops a machine that knows a box's address from reaching it without the
  [LosOS boxes](losos-boxes.md) machine. The machine's keyring has no sign-in
  to unlock it, so the first `losos` command that stores a key or token asks
  for a keyring password in a window of the machine. Nothing makes the
  machine automatically either, and its owner makes it from the template.
  Neither half has met a real box on a real LAN yet, only a box that the
  machines test boots beside the host.
- **Backups to a LosOS box keep one copy and need a password sign-in.** LosOS
  cloud keeps old versions and the trash, but LosOS Git keeps only the latest
  state of each branch, and only the configs' repository has a history. A
  repository deleted at home stays on the box. Configs that are symbolic
  links, as Home Manager writes them, are not in the configs' repository, so
  back up the Home Manager configuration they come from instead. Accounts
  with two-factor sign-in cannot sign in from the Backups page, which sends a
  name and a password. Git LFS objects are not pushed. Symbolic links, which
  Nextcloud cannot hold, are left out. A box with HTTPS turned off through
  `losos.tls.enable` cannot be backed up to. Only an administrator's run can
  start a stopped LosOS boxes machine. Backups run once a day, with no
  setting to change that. They have run against Nextcloud and Forgejo laid
  out the way a box lays them out, not against a real box. See
  [Backups to a LosOS box](backup.md).
