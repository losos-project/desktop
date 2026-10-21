# Android app support: a person's own Pixel, a virtual one, and its apps
# as windows of their own on the desktop (docs/android-apps.md). The Pixel
# is Google's Android Emulator, a QEMU with KVM, running Android 17 with
# Google Play from Google's own system image, made as Android Studio makes
# a Pixel 10 Pro. Each app's window is a scrcpy window onto a display of
# the Pixel's made for that app alone.
#
# What ships here is this repository's and open source: the setup and
# the commands around it (android-app-support/android-app-support.sh), the
# libraries the emulator links, for it to run on NixOS, and the small
# program that lists the Pixel's apps with their icons. The emulator and
# the system image are Google's, under the Android SDK licence, which lets
# a person download and use them but not hand them on, so each person's
# setup downloads them from Google, checked against the hashes below.
{
  lib,
  stdenvNoCC,
  buildFHSEnv,
  writeShellApplication,
  writeShellScript,
  fetchurl,
  makeDesktopItem,
  symlinkJoin,
  jdk21_headless,
  android-tools,
  scrcpy,
  coreutils,
  curl,
  gawk,
  gnugrep,
  jq,
  libnotify,
  socat,
  systemd,
  unzip,
  findutils,
}:

let
  # The newest of each on Google's stable channel, as its SDK repository
  # (dl.google.com/android/repository/repository2-3.xml and
  # sys-img/google_apis_playstore/sys-img2-3.xml) listed them. The sizes
  # are the archives', for the setup's progress and its disk check; the
  # hashes are taken from the archives themselves, whose SHA-1s match the
  # ones Google lists.
  emulator = {
    build = "16428233";
    version = "37.2.12";
    url = "https://dl.google.com/android/repository/emulator-linux_x64-16428233.zip";
    sha256 = "b08fc43d8608d2955f607f1b287beb525041e730086bdca3af152accac3af9c1";
    size = 349654171;
  };
  # Android 17 (API level 37.2) with Google Play, in its 16 KB page size
  # build, the page size a Pixel 10's kernel uses. Google's AOSP images
  # without Google's apps stop at Android 16, and without Play Services
  # most of the apps people want do not run.
  image = {
    name = "Android 17 with Google Play";
    target = "android-37.2";
    tag = "google_apis_playstore_ps16k";
    tagDisplay = "Google Play";
    revision = "6";
    url = "https://dl.google.com/android/repository/sys-img/google_apis_playstore/x86_64-playstore-ps16k-37.2_r06.zip";
    sha256 = "e5e925057f49476646d10fc79614f513ec3a3a21b6351b0c8d1382d88d3c262b";
    size = 2461856017;
  };
  # Android Studio's Pixel 10 Pro, the newest Pixel in its hardware
  # profiles (sdklib's devices/nexus.xml): its screen in pixels and its
  # density bucket.
  device = {
    id = "pixel_10_pro";
    name = "Pixel 10 Pro";
    width = 1280;
    height = 2856;
    density = 480;
  };
  # An app's window when it opens: the Pixel 10 Pro's screen in Android's
  # density-independent pixels (414 by 923), at twice the size, so an app
  # lays itself out as on the phone. The window can then be resized, and
  # the app's display with it.
  window = {
    width = 828;
    height = 1846;
    density = 320;
  };

  # The libraries the emulator's binaries link that it does not carry
  # itself, as nixpkgs' own androidenv patches them in, for a person's
  # download to run as it does on any other distribution: a sandbox with
  # them under /usr/lib, the rest of the system as it is. /dev/kvm and the
  # GPU's render nodes come with /dev.
  emulatorEnv = buildFHSEnv {
    name = "android-app-support-emulator";
    targetPkgs =
      pkgs: with pkgs; [
        alsa-lib
        dbus
        e2fsprogs
        expat
        fontconfig
        freetype
        krb5
        libbsd
        libcxx
        libdrm
        libgbm
        libGL
        libglvnd
        libpulseaudio
        libtiff
        libuuid
        libxkbcommon
        ncurses5
        nspr
        nss
        stdenv.cc.cc
        systemd
        vulkan-loader
        wayland
        waylandpp.lib
        zlib
        libice
        libsm
        libx11
        libxcb
        libxcomposite
        libxcursor
        libxdamage
        libxext
        libxfixes
        libxi
        libxkbfile
        libxrender
        libxshmfence
        libxtst
      ];
    # The emulator's own path and arguments, with the host's GL and
    # Vulkan drivers, which NixOS keeps outside /usr.
    runScript = writeShellScript "android-app-support-emulator-run" ''
      export LD_LIBRARY_PATH=/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
      export VK_ADD_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d
      exec "$@"
    '';
  };

  # D8, the compiler from Java's class files to Android's dex, from R8
  # (BSD-3-Clause), as Google publishes it on its Maven repository.
  r8 = fetchurl {
    url = "https://dl.google.com/android/maven2/com/android/tools/r8/8.13.25/r8-8.13.25.jar";
    hash = "sha256-DU8vaMT8vJQ07/Ei9B/rb7HZjv0vqlMM7Bq6yrHOoD0=";
  };

  # The Pixel's apps, listed and drawn on the Pixel itself
  # (android-app-support/java). It is compiled against stubs of the few
  # Android classes it calls, written here, rather than the SDK's
  # android.jar, which is Google's; the stubs only give javac the names,
  # and Android's own classes answer them on the phone.
  appsDex = stdenvNoCC.mkDerivation {
    pname = "android-app-support-apps";
    version = "1";
    src = ./android-app-support;
    nativeBuildInputs = [ jdk21_headless ];
    buildPhase = ''
      mkdir stubs-classes classes dex
      javac --release 11 -d stubs-classes $(find stubs -name '*.java')
      javac --release 11 -cp stubs-classes -d classes java/org/losos/android/Apps.java
      java -cp ${r8} com.android.tools.r8.D8 --release --min-api 30 --output dex \
        $(find classes -name '*.class')
    '';
    installPhase = ''
      install -Dm0644 dex/classes.dex $out
    '';
  };

  script =
    builtins.replaceStrings
      [
        "@emulatorUrl@"
        "@emulatorSha256@"
        "@emulatorSize@"
        "@emulatorBuild@"
        "@imageUrl@"
        "@imageSha256@"
        "@imageSize@"
        "@imageName@"
        "@imageTarget@"
        "@imageTag@"
        "@imageTagDisplay@"
        "@imageRevision@"
        "@imageDir@"
        "@playStore@"
        "@deviceId@"
        "@deviceName@"
        "@lcdWidth@"
        "@lcdHeight@"
        "@lcdDensity@"
        "@windowWidth@"
        "@windowHeight@"
        "@windowDensity@"
        "@emulatorEnv@"
        "@appsDex@"
      ]
      [
        emulator.url
        emulator.sha256
        (toString emulator.size)
        emulator.build
        image.url
        image.sha256
        (toString image.size)
        image.name
        image.target
        image.tag
        image.tagDisplay
        image.revision
        "${image.target}/${image.tag}/x86_64"
        "true"
        device.id
        device.name
        (toString device.width)
        (toString device.height)
        (toString device.density)
        (toString window.width)
        (toString window.height)
        (toString window.density)
        (lib.getExe emulatorEnv)
        "${appsDex}"
      ]
      (builtins.readFile ./android-app-support/android-app-support.sh);

  cli = writeShellApplication {
    name = "android-app-support";
    runtimeInputs = [
      android-tools
      scrcpy
      coreutils
      curl
      findutils
      gawk
      gnugrep
      jq
      libnotify
      socat
      systemd
      unzip
    ];
    text = script;
  };

  # "Open" on an APK in Files installs it into the Pixel, whose app then
  # joins the app grid. It is the handler android-apps.nix makes the
  # default for Android's package type, and not an app of its own.
  installer = makeDesktopItem {
    name = "android-app-support-install";
    desktopName = "Install in Android";
    exec = "android-app-support install %F";
    icon = "phone";
    mimeTypes = [ "application/vnd.android.package-archive" ];
    noDisplay = true;
  };
in
symlinkJoin {
  pname = "android-app-support";
  version = "1";
  paths = [
    cli
    installer
  ];
  passthru = {
    inherit
      appsDex
      emulatorEnv
      emulator
      image
      device
      ;
  };
  meta = {
    description = "Android apps from a virtual Pixel as windows of their own on the desktop";
    license = lib.licenses.agpl3Plus;
    platforms = [ "x86_64-linux" ];
    mainProgram = "android-app-support";
  };
}
