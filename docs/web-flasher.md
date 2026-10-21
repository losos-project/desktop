# Web flasher

The flasher installs the [Halium GSI](halium.md) on a phone from a browser,
over WebUSB, the way GrapheneOS's web installer does for Pixels. It is the
Flasher link in the docs site's navigation bar. It is plain HTML and
JavaScript in `website/static/flasher/`, which Docusaurus copies to
`flasher/` on the site, with no build step and no third-party code.

> [!IMPORTANT]
> It needs a Chromium-based browser on a computer (Chrome, Edge, Brave), since
> only those implement WebUSB, and a phone whose bootloader can be unlocked,
> that takes GSIs, and that runs Linux 5.10 or newer (launched with Android 12
> or later).

![The flasher's page: Prepare the phone, Connect, Unlock the bootloader and the steps after them, each a card with its button](images/web-flasher-page.png)

## What it does

1. **Connect.** The person puts the phone in its bootloader with Volume
   down and Power, and picks it from the browser's USB prompt, which lists
   only devices with a fastboot interface. The flasher reads `product`,
   `unlocked`, `current-slot`, `is-userspace` to tell whether the phone is in
   fastbootd, and whether `init_boot` and `vbmeta` exist.
2. **Unlock.** For a locked bootloader it sends `flashing unlock`. The phone
   asks on its own screen and erases itself. OEM unlocking has to be turned
   on in Android's developer options first, and only the phone can do that.
3. **The phone's boot image.** This step is only for a phone without
   `init_boot`, where the flasher needs the boot image the phone runs.
   "Read it from the phone" restarts the phone with `reboot-fastboot` into
   fastbootd, the fastboot inside recovery. Unlike the bootloader, fastbootd
   can read a boot partition back with `fetch`, in `max-fetch-size` pieces.
   The person connects again and the flasher reads `boot_<slot>`.
   Alternatively the person chooses `boot.img` from the factory image of the
   exact build the phone runs. Either way, `bootimg.js` parses the header,
   reads `Linux version x.y.z` from the kernel, whether raw, gzip with device
   trees after it, or LZ4, and refuses a kernel older than 5.10.
4. **Get LosOS.** The flasher reads the newest release's `SHA256SUMS` from
   the proxy and finds the three `_gsi-` files in it by name. It downloads
   them into the browser's private file storage, the origin private file
   system, and hashes them as they arrive. A file whose SHA-256 differs from
   `SHA256SUMS` stops the install. The person can choose files they already
   have instead, such as the output of a local `nix build .#gsi`, and they
   are checked when `SHA256SUMS` is among them.
5. **Install.** It writes `vbmeta`, when the phone has one, and `init_boot`
   to the current slot, then `userdata`. On a phone without `init_boot` it
   writes `boot` instead. That is the phone's boot image with the GSI's
   ramdisk, taken out of the release's `init_boot.img`, appended to its own
   at a 4-byte boundary, and recompressed with gzip when the phone's ramdisk
   is gzip. The flasher writes the header's ramdisk size, `recovery_dtbo`
   offset and SHA-1 id as `mkbootimg` would. It drops a v4 header's boot
   signature, since vbmeta turns verification off. A result larger than the
   partition stops the install. userdata's own image is gigabytes, and a
   bootloader takes at most `max-download-size` at a time. So the flasher
   decompresses it as a stream and cuts it into sparse images of at most
   that size, and never over 256 MiB. Each one covers the whole partition,
   with "skip" chunks around its own share, as fastboot's own resparsing
   does. They are written one after another.
6. **Start.** It sends `reboot`. The first boot grows the root filesystem to
   the whole of userdata.

It writes nothing else. The phone's kernel, `vendor_boot`, `super` and every
other partition stay as they were, and that is why one image fits every
phone.

![The flasher reads SHA256SUMS and the GSI's files from the proxy in 32 MiB ranges into the browser's private storage, hashing each, then writes vbmeta, init_boot and userdata over WebUSB; vendor_boot, super and the rest stay as they were](images/web-flasher.svg)

> [!WARNING]
> The bootloader cannot be relocked over this: no bootloader outside Pixels takes
> another signing key, and a Pixel's would have to sign `init_boot` and vbmeta
> for LosOS. Every start shows the bootloader's unlocked warning.

## Where the files come from

The aarch64 release carries the GSI's files, through `release` in `flake.nix`,
so they are pushed, signed and tagged with the rest of it. With a proxy, the
flasher reads them from `<proxy>/flasher/<channel>/aarch64/<file>`.

Without a proxy, it cannot read them at all. GitHub serves release assets
without `Access-Control-Allow-Origin`, both from `github.com` and from the
storage it redirects to. GitHub Pages, which serves the flasher, can only
hold files of up to 100 MB, and `userdata` alone is over a gigabyte. So the
Download button becomes **Open the release on GitHub**. It opens the
channel's newest release page, which the site build looked up and
`website/release-index.js` wrote into `flasher/config.json`. The person
downloads the three `_gsi-` files and `SHA256SUMS-aarch64` there, and chooses
them with **Use files on this computer**, which checks them against that
manifest.

The proxy serves `/updates/` to sysupdate as redirects to GHCR's storage. A
page can only read a response from another origin if it carries
`Access-Control-Allow-Origin`, and GHCR's storage does not send it. So
`/flasher/` serves the same release, limited to the `_gsi-` files and
`SHA256SUMS`, by streaming each byte range the flasher asks for through the
proxy. The flasher asks for 32 MiB at a time, so no request comes near the
function's time limit. Those bytes count against the proxy's Vercel
bandwidth, unlike `/updates/`, whose bytes never pass through it. Every read
the proxy answers now carries `Access-Control-Allow-Origin: *`, since all of
it is public and none of it uses cookies. `/ping` does not carry it.

The proxy's address reaches the page the same way it reaches the image. CI
has it as the repository variable `LOSOS_PROXY_URL`, `docs.yml` passes it to
the site build, and the build writes it to `flasher/config.json`. A build
without it, such as `npm start`, still installs from files on the computer.

> [!NOTE]
> The SHA-256 check is against the `SHA256SUMS` the same proxy serves. It
> catches a broken download; it does not check the release's OpenPGP
> signature, which the browser has no key for.
