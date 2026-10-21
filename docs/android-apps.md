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
| Graphics | the host's GPU through the emulator's gfxstream when there is one, SwiftShader on the processor when not |

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

1. It checks for KVM and for room: the download unpacks to about 9 GB.
2. It downloads the emulator, 350 MB, and Android, 2.5 GB, into
   `~/.local/share/android-app-support/`. A stopped download resumes, and a
   file that does not match its hash is deleted and fetched again.
3. It makes the Pixel, an Android Virtual Device in the same folder. Its
   memory is a quarter of the PC's, between 3 and 8 GiB. It has half the
   PC's processors, between 2 and 8, and a 32 GB data partition, a sparse
   file that grows as apps fill it.
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
- Closing the window closes that display, and the app with it.
- No window forwards sound, since each would forward all of it. The
  emulator plays the Pixel's sound itself.

**Show phone** on the Home page opens the Pixel's own screen, home screen
and all.

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
adb for that key, and the key stays in the person's data.

## What was here before

The Android Translation Layer used to do this. It ran each app as a Linux
process with Android's APIs translated onto GTK, so an app met a
translation of Android rather than Android itself. Its package never built
here, so the option stayed off. The Pixel runs Google's own Android, and an
app sees the phone it was written for. `atl.nix`, its package and
`losos-android-run`'s sandbox are gone.
