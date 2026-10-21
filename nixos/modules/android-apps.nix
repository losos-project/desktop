# Android app support (docs/android-apps.md): Android apps as windows of
# their own on the desktop, from a Pixel of each person's own that runs in
# a virtual machine. The Pixel is Google's Android Emulator, a QEMU with
# KVM, made as Android Studio makes a Pixel 10 Pro and running Android 17
# with Google Play; each person's setup downloads both from Google
# (nixos/pkgs/android-app-support.nix). Each app opens on a display of the
# Pixel's of its own, which scrcpy shows as a window.
#
# The Android Translation Layer did this before, running each app as a
# Linux process with Android's APIs translated onto GTK. It never built
# here, and an app it ran met a translation of Android rather than
# Android, so what it showed depended on how much of each API the
# translation covered. The Pixel runs Google's own Android, so an app sees
# the phone it was written for. atl.nix and its package are gone with it;
# opening an APK now installs it into the Pixel.
#
# The emulator, like WinBoat's Windows, is x86-64 only: Google builds no
# emulator for Linux on ARM.
{
  lib,
  pkgs,
  ...
}:

lib.mkIf pkgs.stdenv.hostPlatform.isx86_64 {
  environment.systemPackages = [ pkgs.android-app-support ];

  # "Open" on an APK installs it into the Pixel.
  xdg.mime = {
    enable = true;
    defaultApplications."application/vnd.android.package-archive" =
      "android-app-support-install.desktop";
  };

  # The emulator draws with the GPU's own driver when there is one, from
  # /run/opengl-driver.
  hardware.graphics.enable = true;

  # Each person's own units. The Android app support app starts the setup
  # when its person has no Pixel yet; the setup carries on while the app is
  # closed, since the system image alone is 2.5 GB. The Pixel runs while
  # anything asks for it and stops when the person stops it or signs out,
  # saving a snapshot that the next start resumes from in seconds.
  systemd.user.services = {
    android-app-support-setup = {
      description = "Set up a Pixel for Android app support";
      serviceConfig = {
        Type = "exec";
        ExecStart = "${lib.getExe pkgs.android-app-support} setup";
      };
    };
    android-app-support = {
      description = "The Pixel Android app support runs apps in";
      wants = [ "android-app-support-apps.service" ];
      # The emulator draws on the GPU through Xwayland's GLX, so it starts
      # once xwayland-satellite has put DISPLAY into the user manager's
      # environment (android-app-support.sh, cmd_emulator).
      after = [ "xwayland-satellite.service" ];
      serviceConfig = {
        Type = "exec";
        ExecStart = "${lib.getExe pkgs.android-app-support} emulator";
        # The emulator saves its snapshot on a clean stop through its
        # console, and has a minute for it before systemd kills it.
        ExecStop = "${lib.getExe pkgs.android-app-support} shutdown";
        TimeoutStopSec = 60;
        KillMode = "mixed";
      };
    };
    # Keeps the app grid's Android apps current while the Pixel runs, so an
    # app installed from Google Play joins it.
    android-app-support-apps = {
      description = "List the Pixel's apps for the desktop";
      bindsTo = [ "android-app-support.service" ];
      after = [ "android-app-support.service" ];
      serviceConfig = {
        Type = "exec";
        ExecStart = "${lib.getExe pkgs.android-app-support} watch";
      };
    };
  };
}
