# Android app support

Android app support opens Android apps in their own windows on the desktop.
Each person gets a Pixel of their own, a virtual one, which sets itself up
the first time without asking anything. The Pixel is Google's Android
Emulator running Android 17 with Google Play, made the way Android Studio
makes a Pixel 10 Pro. It runs on x86-64 only, and it needs KVM.

## What runs where

| | |
| --- | --- |
| The Pixel | Google's Android Emulator, a QEMU with KVM, as the person's user unit `android-app-support` |
| Android | Android 17 (API level 37.2) with Google Play, in its 16 KB page size build, as a Pixel 10's kernel uses |
| The device | Android Studio's Pixel 10 Pro profile: a 1280 by 2856 screen at 480 dpi, its buttons and sensors |
| Each app's window | a scrcpy window onto a display of the Pixel's made for that app alone |
| Sound | the emulator's own output, to the person's PipeWire |
| Graphics | the PC's GPU, through the emulator's gfxstream and the GPU's own driver; SwiftShader on the processor when there is no GPU |

The emulator and Android are Google's, under the Android SDK licence. It
lets a person download and use them and lets nobody hand them on, so the
image does not carry them. Each person's setup downloads both from Google's
own SDK repository, the same files Android Studio's SDK Manager fetches,
and checks them against hashes the release pinned
(`nixos/pkgs/android-app-support.nix`). Everything else is this
repository's and open source: the setup, the units, the libraries the
emulator links, and the small program that lists the Pixel's apps.

## The first time

The app grid's **Android** is a derisk core app, `org.derisk.android`. Like
WinBoat it has a **Home** page, the Pixel with Start and Stop beside it, and
an **Apps** page, every app on the Pixel as an icon that opens it. It is
listed only on a system that has `android-app-support`, through its desktop
entry's `TryExec`.

For a person with no Pixel yet, the app starts their user unit
`android-app-support-setup`, which runs `android-app-support setup`:

1. It checks for KVM and for room: about 9 GB while the download unpacks,
   and 10 GB for the Pixel's data partition.
2. It downloads the emulator, 350 MB, and Android, 2.5 GB, into
   `~/.local/share/android-app-support/`. A stopped download resumes, and a
   file that does not match its hash is deleted and fetched again.
3. It makes the Pixel, an Android Virtual Device in the same folder. Its
   memory is a quarter of the PC's, between 3 and 8 GiB. It has half the
   PC's processors, between 2 and 8, and an 8 GiB data partition for its
   apps. The emulator will not make the partition unless a fifth more than
   its size is free, so it is smaller than a phone's.
4. It starts the Pixel, waits for Android to finish its first boot, lists
   its apps, and sends a notification that Android is ready.

The Home page shows each step and the download's progress. The setup
carries on while the app is closed.

## The apps

`android-app-support sync` asks the Pixel which apps it has, the way its own
launcher does. A small Java program, `org.losos.android.Apps`
(`nixos/pkgs/android-app-support/java/`), runs on the Pixel under
`app_process` as adb's shell user. It lists every app the launcher would
show, with its package and name, and draws each one's icon in the Pixel's
icon shape. So an adaptive or vector icon looks as it does on the phone.

The program is compiled against stubs of the few Android classes it calls,
written in this repository, instead of the SDK's `android.jar`, which is
Google's. D8, from the open-source R8 project, turns it into Android's dex.

From that list:

- The **Android** app shows every app.
- The app grid and the palette get the apps the person installed, and
  Google Play to install more. Each gets a desktop entry,
  `~/.local/share/applications/android-<package>.desktop`, with the
  Pixel's icon. The apps the phone came with stay in the Android app, so
  the grid has one Settings and one Files.
- `android-app-support-apps`, a user unit that runs beside the Pixel, checks
  its packages every ten seconds and lists the apps again when they change.
  An app installed from Google Play reaches the app grid that way.

Opening an `.apk` in Files installs it with `android-app-support install`.
Its app then joins the grid.

## Each app's window

`android-app-support run PACKAGE` opens one app. It starts the Pixel if it
is off, then runs [scrcpy](https://github.com/Genymobile/scrcpy) with
`--new-display`. scrcpy makes a display on the Pixel for that app alone,
starts the app on it, and shows only that display in its window. Android
encodes the display as H.264, and scrcpy decodes it on the desktop with
FFmpeg. Typing, the pointer and the clipboard go back the other way.

- The window opens at the Pixel 10 Pro's size in Android's
  density-independent pixels, twice over: 828 by 1846 pixels at 320 dpi.
  `--flex-display` resizes the display with the window, so an app lays
  itself out for the window's size, as a desktop app does.
- The window's Wayland app_id is `android.PACKAGE`, which the desktop
  entry's `StartupWMClass` names. So the desktop shows the window with the
  app's name and icon.
- The display has no system decorations
  (`--no-vd-system-decorations`). Android 17 gives a display that has
  them a home screen and taskbar of its own, which covered the app.
- Closing the window closes that display, and the app with it.
- No window forwards sound, since each would forward all of it. The
  emulator plays the Pixel's sound itself.

**Show phone** on the Home page opens the Pixel's own screen, home screen
and all.

If the Pixel is slow to make an app's display, scrcpy gives up on starting
the app there and the window shows an empty home screen. `android-app-support
run` reads scrcpy's log for that and then starts the app on the display
itself.

## The GPU

The Pixel draws with the PC's GPU. Android's OpenGL ES and Vulkan calls,
from its compositor and from every app, go through the emulator's
gfxstream to the GPU's own driver on the PC, the one in
`/run/opengl-driver`, so they run on the GPU at its speed. No Android
driver or patched system image is needed: the system image's own
`emulation` EGL and `ranchu` Vulkan drivers speak gfxstream.

The emulator reaches the PC's OpenGL through GLX, which on the desktop is
Xwayland's. So the Pixel's unit starts after `xwayland-satellite`, which
puts `DISPLAY` into the user manager's environment. The unit's log says
which mode it chose:

- `-gpu host`, the GPU, when the PC has a render node in `/dev/dri` and
  Xwayland is running;
- `-gpu swiftshader_indirect`, the emulator's SwiftShader on the processor,
  when either is missing.

If the emulator dies on the GPU before Android has started, which happens
when gfxstream gets no OpenGL context from the driver, the Pixel is started
once more with SwiftShader, and its log says so. That lasts for that start
only: the next one tries the GPU again.

`android-app-support gpu` shows what Android itself got. On a PC with a GPU
its first line names it:

```
GLES: Google (AMD), Android Emulator OpenGL ES Translator (AMD Radeon 780M (radeonsi, ...)), OpenGL ES 3.0 (...)
```

With a package, `android-app-support gpu org.fdroid.fdroid`, it also gives
that app's frame times since it started, from Android's own frame
statistics.

`ANDROID_APP_SUPPORT_GPU` in the user manager's environment chooses the
mode instead (`host` or `swiftshader_indirect`), and
`ANDROID_APP_SUPPORT_EMULATOR_FLAGS` adds to the emulator's command line.

CI's runners have KVM but no GPU, so the `android` job puts Mesa at
`/run/opengl-driver` and runs the Pixel with `-gpu host` on Mesa's llvmpipe,
the same path a GPU's driver takes. It fails unless Android's renderer names
llvmpipe rather than SwiftShader. llvmpipe's Vulkan cannot export the memory
gfxstream asks for, so that run turns the emulator's Vulkan off with
`-feature -Vulkan`. A GPU's driver can, and keeps it.

The `android-desktop` job runs the whole thing inside the image
(`nixos/tests/android-apps.nix`). The image boots in a VM and a person
signs in to its desktop. The Android app support window sets the Pixel
up, and the Pixel boots inside that VM, on KVM that the runner passes on
to it. F-Droid then installs and opens from its desktop entry as one of
the desktop's windows. The setup downloads from Google, so the test runs
outside the Nix sandbox, and it is in the flake's `networkTests` rather
than `checks`. Its screenshots are the job's `android-desktop` artifact.

## The Pixel's units

Three of each person's user units, from `nixos/modules/android-apps.nix`:

| Unit | What it runs |
| --- | --- |
| `android-app-support-setup` | the first-time setup above |
| `android-app-support` | the emulator, with no window of its own |
| `android-app-support-apps` | the app list's upkeep, while the Pixel runs |

Stopping the Pixel asks the emulator to stop through its console. It then
saves a snapshot, and the next start resumes from it in seconds instead of
booting Android again.

Each person's Pixel takes a console and adb port pair of its own, chosen
from their UID. adb's server listens on a socket in the person's runtime
directory instead of TCP port 5037. Otherwise one server would serve
everyone on the computer, and another person's adb could reach this
person's Pixel with this person's key. Google Play's Android also asks
adb for that key, and the key stays in the person's data. A server looks
for emulators once, as it starts, and afterwards hears of one only when
the emulator tells it on port 5037. While the Pixel starts,
`android-app-support` sends the server that same request over its socket,
so a Pixel that starts after the server is still found.

The server runs as a user unit of its own, `android-app-support-adb`, and
every `android-app-support` command starts it before using adb. Left to
itself, adb starts a server from whichever command needs one first, and
that server dies with the command's unit. The key adb signs in with is
made before anything starts the Pixel, because Google Play's Android only
accepts the key the emulator handed it as Android booted.

## What was here before

The Android Translation Layer used to do this. It ran each app as a Linux
process with Android's APIs translated onto GTK, so an app met a
translation of Android rather than Android itself. Its package never built
here, so the option stayed off. The Pixel runs Google's own Android, and an
app sees the phone it was written for. `atl.nix`, its package and
`losos-android-run`'s sandbox are gone.
