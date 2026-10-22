# android-app-support: a person's own Pixel, a virtual one, and its apps
# as windows of their own on the desktop (docs/android-apps.md).
#
#   android-app-support status          what it is doing, as one line of JSON
#   android-app-support setup           download Android and make the Pixel
#   android-app-support start | stop    start the Pixel, or stop it
#   android-app-support run PACKAGE     open an app in a window of its own
#   android-app-support phone           the whole phone's screen in a window
#   android-app-support install APK...  install apps from APK files
#   android-app-support sync            list the apps again, with icons
#   android-app-support gpu [PACKAGE]   what the Pixel draws with, and how
#                                       fast an app's frames came
#
# and, for the user units in modules/android-apps.nix: `emulator`, the
# Pixel itself, `shutdown`, its clean stop, and `watch`, which keeps the
# app list current while it runs.
#
# @emulatorUrl@ and the rest are filled in by android-app-support.nix.

data=${XDG_DATA_HOME:-$HOME/.local/share}/android-app-support
state=${XDG_STATE_HOME:-$HOME/.local/state}/android-app-support
sdk=$data/sdk
avd=$data/avd
system_image=system-images/@imageDir@
mkdir -p "$data" "$state"

# The emulator and adb read these: the SDK the Pixel runs from, the
# Pixel's own files, and the adb keys and console token, which are the
# person's and stay in their data.
export ANDROID_SDK_ROOT=$sdk ANDROID_HOME=$sdk ANDROID_AVD_HOME=$avd
export ANDROID_USER_HOME=$data/user ANDROID_EMULATOR_HOME=$data/user
mkdir -p "$ANDROID_USER_HOME"

# The key adb signs in to the Pixel with. A Play image lets adb in only
# with a key the emulator gave it as Android booted, read from adbkey.pub
# here, so the key is made before anything starts the emulator or adb's
# server: when adb's server made it first, as it does when it finds none,
# a Pixel that booted meanwhile said "device unauthorized" to every
# command. ADB_VENDOR_KEYS makes the server offer this key whichever
# directory it keeps its own in. The lock stops two commands started at
# once from each writing half a pair.
adbkey=$ANDROID_USER_HOME/adbkey
if [ ! -s "$adbkey.pub" ]; then
  (
    flock 9
    [ -s "$adbkey.pub" ] || adb keygen "$adbkey" > /dev/null 2>&1
  ) 9> "$ANDROID_USER_HOME/adbkey.lock"
fi
export ADB_VENDOR_KEYS=$adbkey

# adb's server on a socket in the person's runtime directory, which only
# they can open. Its default, TCP port 5037 on localhost, is one server for
# everyone on the computer, so another person's adb would reach this
# person's Pixel through it, with this person's key.
export ADB_SERVER_SOCKET=localfilesystem:${XDG_RUNTIME_DIR:?no XDG_RUNTIME_DIR}/android-app-support-adb

# The Pixel's console port, and its adb port one above. Each person's
# Pixel has its own pair, from their UID, among the sixteen pairs adb
# looks for emulators on (5554 to 5585). adb on the Pixel still asks for
# this person's key, so a port another person can see is no way in.
port=$((5554 + 2 * ($(id -u) % 16)))
serial=emulator-$port

say() { printf '%s\n' "$*" >&2; }

# What the setup is doing, in words, for `status` to show.
progress() { printf '%s\n' "$*" > "$state/setup.new" && mv "$state/setup.new" "$state/setup"; }

json() {
  jq -cn --arg state "$1" --arg detail "${2:-}" --arg serial "$serial" \
    '{state: $state, detail: $detail, serial: $serial}'
}

# The Pixel's data partition, where its apps and their data live, in GiB.
# The emulator makes it at its first start and refuses to unless a fifth
# more than its size is free, so it is not as large as a phone's.
data_partition=8

set_up() { [ -e "$avd/pixel.ini" ] && [ -e "$sdk/$system_image/system.img" ]; }

active() { systemctl --user -q is-active "$1"; }

booted() {
  [ "$(adb -s "$serial" shell getprop sys.boot_completed 2> /dev/null | tr -d '\r')" = 1 ]
}

cmd_status() {
  if [ "$(uname -m)" != x86_64 ] || [ ! -e /dev/kvm ]; then
    json unsupported "This computer cannot run virtual machines: Android app support needs an x86-64 processor with virtualisation turned on."
  elif active android-app-support-setup.service; then
    json setting-up "$(cat "$state/setup" 2> /dev/null || echo "Starting")"
  elif systemctl --user -q is-failed android-app-support-setup.service; then
    json failed "$(cat "$state/setup" 2> /dev/null || echo "The setup stopped")"
  elif ! set_up; then
    json new
  elif ! active android-app-support.service; then
    json off
  elif booted; then
    json running
  else
    json starting "Android is starting"
  fi
}

# Bytes of `file`, for the download's progress.
size_of() { stat -c %s "$1" 2> /dev/null || echo 0; }

# `url` into `file`, checked against `sha256`, resuming a download a
# stopped setup left. What is shown is `what` and how far it has come.
fetch() {
  local url=$1 sha256=$2 size=$3 file=$4 what=$5
  if [ -e "$file" ] && echo "$sha256  $file" | sha256sum -c --status; then
    return 0
  fi
  curl -fsSL --retry 5 -C - -o "$file" "$url" &
  local curl=$!
  while kill -0 "$curl" 2> /dev/null; do
    progress "Downloading $what: $(numfmt --to=iec "$(size_of "$file")") of $(numfmt --to=iec "$size")"
    sleep 1
  done
  if ! wait "$curl"; then
    # A download that cannot resume (the server sent something else)
    # starts again from nothing next time.
    rm -f "$file"
    progress "Could not download $what. Check the network and open Android app support again."
    return 1
  fi
  if ! echo "$sha256  $file" | sha256sum -c --status; then
    rm -f "$file"
    progress "$what did not download as Google published it. Open Android app support to try again."
    return 1
  fi
}

# The emulator and the system image from Google's Android SDK repository,
# the files Android Studio's SDK Manager downloads for the same packages,
# checked against hashes the release pinned. Neither is in the image:
# both are Google's, under the Android SDK licence, which lets a person
# download and use them and does not let anyone hand them on.
cmd_setup() {
  local downloads=$data/downloads
  mkdir -p "$downloads"
  # The downloads, unpacked beside them, and the data partition.
  need=$(( (@imageSize@ * 3 + @emulatorSize@ * 3) / 1024 + data_partition * 1024 * 1024 * 6 / 5 ))
  free=$(df --output=avail -k "$data" | tail -1)
  if [ "$free" -lt "$need" ]; then
    progress "Android needs $(numfmt --to=iec $((need * 1024))) free, and this computer has $(numfmt --to=iec $((free * 1024)))."
    return 1
  fi

  if [ ! -x "$sdk/emulator/emulator" ] || [ "$(cat "$sdk/emulator/.build" 2> /dev/null)" != @emulatorBuild@ ]; then
    fetch @emulatorUrl@ @emulatorSha256@ @emulatorSize@ "$downloads/emulator.zip" "the Android Emulator" || return 1
    progress "Unpacking the Android Emulator"
    rm -rf "$sdk/emulator" "$sdk/emulator.new"
    mkdir -p "$sdk/emulator.new"
    unzip -q "$downloads/emulator.zip" -d "$sdk/emulator.new"
    mv "$sdk/emulator.new/emulator" "$sdk/emulator"
    rmdir "$sdk/emulator.new"
    echo @emulatorBuild@ > "$sdk/emulator/.build"
    rm "$downloads/emulator.zip"
  fi

  if [ ! -e "$sdk/$system_image/system.img" ] || [ "$(cat "$sdk/$system_image/.revision" 2> /dev/null)" != @imageRevision@ ]; then
    fetch @imageUrl@ @imageSha256@ @imageSize@ "$downloads/system-image.zip" "@imageName@" || return 1
    progress "Unpacking @imageName@"
    rm -rf "${sdk:?}/$system_image" "$sdk/$system_image.new"
    mkdir -p "$sdk/$system_image.new"
    unzip -q "$downloads/system-image.zip" -d "$sdk/$system_image.new"
    # The archive holds one directory, named for the ABI.
    mv "$sdk/$system_image.new"/*/ "$sdk/$system_image"
    rmdir "$sdk/$system_image.new"
    echo @imageRevision@ > "$sdk/$system_image/.revision"
    rm "$downloads/system-image.zip"
  fi

  make_pixel
  progress "Starting the Pixel for the first time"
  systemctl --user start android-app-support.service
  wait_booted 900 || {
    progress "The Pixel did not start. Open Android app support to try again."
    return 1
  }
  cmd_sync
  notify-send --app-name="Android app support" --icon=phone \
    "Android is ready" "Open Android app support to open your Android apps, or Google Play to get more." || true
}

# The Pixel: an Android Virtual Device as Android Studio makes one from its
# Pixel 10 Pro hardware profile, its screen, density and buttons, with this
# computer's memory and processors in place of the phone's 16 GB: a
# quarter of the memory, between 3 and 8 GiB, and half the processors,
# between 2 and 8. The data partition is a sparse file, as large as a
# phone's storage only once apps fill it.
make_pixel() {
  local memory cpus
  memory=$(awk '/^MemTotal:/ { m = int($2 / 1024 / 4); if (m < 3072) m = 3072; if (m > 8192) m = 8192; print m }' /proc/meminfo)
  cpus=$(( $(nproc) / 2 ))
  if [ "$cpus" -lt 2 ]; then cpus=2; fi
  if [ "$cpus" -gt 8 ]; then cpus=8; fi
  mkdir -p "$avd/pixel.avd"
  cat > "$avd/pixel.ini" << EOF
avd.ini.encoding=UTF-8
path=$avd/pixel.avd
path.rel=avd/pixel.avd
target=@imageTarget@
EOF
  # Kept across a new setup: what the person installed is in the data
  # partition, and only the settings below change.
  cat > "$avd/pixel.avd/config.ini" << EOF
AvdId=pixel
avd.ini.displayname=@deviceName@
avd.ini.encoding=UTF-8
PlayStore.enabled=@playStore@
abi.type=x86_64
hw.cpu.arch=x86_64
hw.cpu.ncore=$cpus
hw.ramSize=$memory
vm.heapSize=512
hw.device.manufacturer=Google
hw.device.name=@deviceId@
hw.lcd.width=@lcdWidth@
hw.lcd.height=@lcdHeight@
hw.lcd.density=@lcdDensity@
hw.mainKeys=no
hw.keyboard=yes
hw.dPad=no
hw.trackBall=no
hw.camera.back=none
hw.camera.front=none
hw.gps=yes
hw.audioInput=yes
hw.audioOutput=yes
hw.accelerometer=yes
hw.gyroscope=yes
hw.sensors.proximity=yes
hw.battery=yes
hw.gpu.enabled=yes
hw.gpu.mode=auto
image.sysdir.1=$system_image/
tag.id=@imageTag@
tag.display=@imageTagDisplay@
disk.dataPartition.size=${data_partition}G
sdcard.size=512M
showDeviceFrame=no
skin.dynamic=yes
skin.name=@lcdWidth@x@lcdHeight@
fastboot.forceColdBoot=no
fastboot.forceFastBoot=yes
EOF
}

# The emulator, as the user unit android-app-support.service runs it: the
# Pixel's screen goes nowhere, since each app's window comes through scrcpy
# instead (cmd_run), and its sound to the person's PipeWire. A clean stop
# (cmd_shutdown) saves a snapshot, which the next start resumes in seconds.
#
# With a GPU it draws through it: the emulator's gfxstream renders the
# guest's OpenGL ES and Vulkan with the host's own driver, from
# /run/opengl-driver, so Android's compositor and every app's drawing run
# on the GPU (docs/android-apps.md, "The GPU"). The emulator reaches the
# host's OpenGL through GLX, which on the desktop is Xwayland's, so it
# needs DISPLAY; the unit starts after xwayland-satellite has set it.
# Without a GPU, or without Xwayland, it draws with SwiftShader on the
# processor. ANDROID_APP_SUPPORT_GPU names the mode instead, and
# ANDROID_APP_SUPPORT_EMULATOR_FLAGS adds to the emulator's command line:
# CI sets `host` on Mesa's llvmpipe, whose Vulkan cannot export the memory
# gfxstream asks for, with `-feature -Vulkan`.
cmd_emulator() {
  set_up || { say "android-app-support: not set up yet; run android-app-support setup"; exit 1; }
  local gpu=${ANDROID_APP_SUPPORT_GPU:-}
  if [ -z "$gpu" ]; then
    gpu=swiftshader_indirect
    if compgen -G '/dev/dri/renderD*' > /dev/null && [ -n "${DISPLAY:-}" ]; then gpu=host; fi
  fi
  local -a flags
  read -ra flags <<< "${ANDROID_APP_SUPPORT_EMULATOR_FLAGS:-}"
  echo "android-app-support: drawing with -gpu $gpu"
  # The emulator takes a directory as an SDK only when it has
  # platform-tools, and runs adb from there to tell the person's adb
  # server about the Pixel. This package's adb is that one.
  mkdir -p "$sdk/platform-tools"
  ln -sfn "$(command -v adb)" "$sdk/platform-tools/adb"
  rm -f "$avd"/pixel.avd/*.lock
  exec @emulatorEnv@ "$sdk/emulator/emulator" -avd pixel -port "$port" \
    -no-window -no-boot-anim -no-metrics -gpu "$gpu" "${flags[@]}" \
    -netdelay none -netspeed full
}

# What the Pixel draws with, read inside Android: the renderer its
# compositor got, which names the host's GPU and driver when the emulator
# draws on it ("Android Emulator OpenGL ES Translator (AMD Radeon ...)")
# and SwiftShader when not, and how Android's graphics reach the host.
# With a package, also how long that app's frames took to draw, from its
# own frame statistics since it started.
cmd_gpu() {
  local package=${1:-}
  cmd_start
  adb -s "$serial" shell dumpsys SurfaceFlinger | tr -d '\r' | grep -E '^GLES:' || true
  adb -s "$serial" shell getprop | tr -d '\r' |
    grep -E '^\[(ro\.hardware\.(egl|vulkan)|ro\.opengles\.version|ro\.boot\.(qemu\.gltransport|hardwareegl|hardware\.vulkan)|debug\.hwui\.renderer|ro\.boot\.debug\.hwui\.renderer)\]' || true
  if [ -n "$package" ]; then
    echo "Frames of $package:"
    adb -s "$serial" shell dumpsys gfxinfo "$package" | tr -d '\r' |
      grep -E 'Total frames rendered|Janky frames|(50|90|95|99)th percentile|Pipeline=' || true
  fi
}

cmd_shutdown() {
  adb -s "$serial" emu kill > /dev/null 2>&1 || true
}

# Tells adb's server where the Pixel is. The server looks for emulators
# on their ports once, as it starts, and after that only hears of one that
# tells it on TCP port 5037, which is not where this server listens. So a
# Pixel that starts after the server is told about by hand, with the
# request the emulator would have sent: host:emulator and its adb port.
# Telling it again about a Pixel it knows does nothing.
register() {
  local request="host:emulator:$((port + 1))"
  adb start-server > /dev/null 2>&1 || return 0
  printf '%04x%s' "${#request}" "$request" |
    socat -t 2 - "UNIX-CONNECT:${ADB_SERVER_SOCKET#localfilesystem:}" 2> /dev/null || true
}

# Waits for Android to have started, for at most `seconds`.
wait_booted() {
  local seconds=$1 start=$SECONDS
  while [ $((SECONDS - start)) -lt "$seconds" ]; do
    active android-app-support.service || return 1
    register
    booted && return 0
    sleep 2
  done
  return 1
}

cmd_start() {
  if ! set_up; then
    systemctl --user start --no-block android-app-support-setup.service
    say "Android is being set up; its apps open once it is ready."
    exit 1
  fi
  systemctl --user start android-app-support.service
  wait_booted 300 || { say "android-app-support: the Pixel did not start"; exit 1; }
  # Its screen stays on: a phone whose screen goes off stops drawing the
  # apps on its other displays too.
  adb -s "$serial" shell 'svc power stayon true; settings put system screen_off_timeout 2147483647' > /dev/null 2>&1 || true
}

cmd_stop() {
  systemctl --user stop android-app-support.service
}

# The apps, as desktop entries for the app grid and a list for the Android
# app support app: each launchable app's package, name and icon, read on
# the Pixel by Apps (java/org/losos/android/Apps.java) as its launcher
# reads them. The app grid gets the apps the person installed, and Google
# Play to install more; the apps the phone came with (Settings, Contacts,
# Chrome) are in the Android app support app, so the grid has one Settings.
#
# One at a time: the setup, an install and the watcher below each list the
# apps, and two at once wrote over each other's files, here and on the
# Pixel.
cmd_sync() {
  mkdir -p "$data"
  (
    flock 9
    sync_apps
  ) 9> "$data/sync.lock"
}

sync_apps() {
  local apps=$data/apps.tsv icons=$data/icons remote=/data/local/tmp/losos-apps
  local applications=${XDG_DATA_HOME:-$HOME/.local/share}/applications
  adb -s "$serial" push @appsDex@ "$remote.dex" > /dev/null
  run_apps() { adb -s "$serial" shell CLASSPATH="$remote.dex" app_process / org.losos.android.Apps "$@"; }
  run_apps list | tr -d '\r' | LC_ALL=C sort -u > "$apps.new"
  [ -s "$apps.new" ] || { rm -f "$apps.new"; say "android-app-support: the Pixel listed no apps"; return 1; }
  run_apps icons "$remote-icons" 192 > /dev/null
  rm -rf "$icons.new"
  mkdir -p "$icons.new"
  adb -s "$serial" pull "$remote-icons/." "$icons.new" > /dev/null
  adb -s "$serial" shell rm -rf "$remote-icons" > /dev/null
  rm -rf "$icons"
  mv "$icons.new" "$icons"
  mv "$apps.new" "$apps"

  mkdir -p "$applications"
  # Entries this wrote before, which the list below replaces: an app the
  # person removed from the Pixel leaves the app grid with it.
  grep -l '^X-Android-App-Support=true' "$applications"/android-*.desktop 2> /dev/null | xargs -r rm -f || true
  while IFS=$'\t' read -r package name origin; do
    [ "$origin" = user ] || [ "$package" = com.android.vending ] || continue
    icon=$icons/$package.png
    [ -e "$icon" ] || icon=phone
    cat > "$applications/android-$package.desktop" << EOF
[Desktop Entry]
Type=Application
Name=$name
Comment=Android app
Exec=android-app-support run $package
Icon=$icon
Categories=Android;
Keywords=android;
StartupWMClass=android.$package
X-Android-Package=$package
X-Android-App-Support=true
EOF
  done < "$apps"
}

# Lists the apps again whenever the Pixel's packages change, which is how an
# app installed from Google Play reaches the app grid. Run by the user unit
# android-app-support-apps.service while the Pixel runs.
cmd_watch() {
  wait_booted 900 || exit 0
  adb -s "$serial" shell 'svc power stayon true; settings put system screen_off_timeout 2147483647' > /dev/null 2>&1 || true
  local last='' now
  while active android-app-support.service; do
    now=$(adb -s "$serial" shell pm list packages -3 2> /dev/null | LC_ALL=C sort | sha256sum) || now=
    if [ -n "$now" ] && [ "$now" != "$last" ]; then
      cmd_sync && last=$now
    fi
    sleep 10
  done
}

name_of() {
  awk -F '\t' -v p="$1" '$1 == p { print $2; exit }' "$data/apps.tsv" 2> /dev/null
}

# One app in a window of its own: scrcpy makes a display on the Pixel for
# it alone, starts the app there and shows only that display, which grows
# and shrinks with the window (--flex-display), so the app lays itself out
# as for a screen of the window's size. scrcpy's server on the Pixel
# encodes the display as H.264 and the window decodes it with FFmpeg. The
# window's app_id is android.PACKAGE, which its desktop entry's
# StartupWMClass names, so the desktop shows it with the app's name and
# icon. Closing it closes the display and the app with it.
#
# The display has no system decorations: Android 17 gives a display that
# has them its own home screen and taskbar, which came up over the app,
# and the window showed the Pixel's wallpaper instead of the app.
#
# Sound comes from the emulator itself, so no window forwards it: every
# window would forward all of it.
cmd_run() {
  local package=${1:?usage: android-app-support run PACKAGE}
  cmd_start
  local name
  name=$(name_of "$package")
  SDL_APP_ID=android.$package exec scrcpy -s "$serial" \
    --new-display=@windowWidth@x@windowHeight@/@windowDensity@ --flex-display \
    --no-vd-system-decorations --start-app="$package" --no-audio \
    --window-title="${name:-$package}" --stay-awake \
    > >(start_late "$package" >&2) 2>&1
}

# scrcpy's server waits a second for its new display before starting the
# app, and gives up with "No known display id" when Android takes longer to
# make it, as a busy or freshly resumed Pixel does. The window then shows
# the display's empty home screen. This passes scrcpy's log on to the
# journal and, when that happens, starts the app on the display once scrcpy
# names it.
start_late() {
  local package=$1 line missed='' display='' activity
  while IFS= read -r line; do
    printf '%s\n' "$line"
    case $line in
      *"No known display id to start app"*) missed=1 ;;
      *"New display:"*"(id="*")"*)
        display=${line##*(id=}
        display=${display%%)*}
        ;;
    esac
    if [ -n "$missed" ] && [ -n "$display" ]; then
      # adb shell forwards its input, which here is scrcpy's log.
      activity=$(adb -s "$serial" shell cmd package resolve-activity --brief \
        -c android.intent.category.LAUNCHER "$package" < /dev/null | tr -d '\r' | tail -1)
      adb -s "$serial" shell am start --display "$display" -n "$activity" < /dev/null > /dev/null || true
      missed=''
    fi
  done
}

# The phone's own screen, home screen and all, as the Pixel shows it.
cmd_phone() {
  cmd_start
  SDL_APP_ID=android-app-support exec scrcpy -s "$serial" --no-audio \
    --window-title="@deviceName@" --stay-awake
}

cmd_install() {
  [ $# -gt 0 ] || { say "usage: android-app-support install APK..."; exit 2; }
  cmd_start
  local apk
  for apk in "$@"; do
    adb -s "$serial" install -r "$apk" || { say "android-app-support: $apk did not install"; exit 1; }
  done
  cmd_sync
}

command=${1:-status}
shift || true
case $command in
  status | setup | start | stop | run | phone | install | sync | gpu | emulator | shutdown | watch)
    "cmd_$command" "$@"
    ;;
  *)
    say "usage: android-app-support status|setup|start|stop|run PACKAGE|phone|install APK...|sync|gpu [PACKAGE]"
    exit 2
    ;;
esac
