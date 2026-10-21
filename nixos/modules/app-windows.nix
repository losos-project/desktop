# app-windows on every target (docs/app-windows.md): the client that shows
# a guest system's apps in the app grid and its windows as windows here.
# Nothing starts it yet; a guest's own module does, with that guest's
# server and token.
#
# What it does without a window: Mac packages, disk images and installers,
# opened in Files or downloaded into Downloads, go to macOS and open there.
# Both wait for a person who has a macOS guest, which is when its token is
# in their config.
{ pkgs, ... }:

{
  environment.systemPackages = [ pkgs.app-windows ];

  # "Open" on a .dmg or .pkg hands it to macOS. A .pkg is a xar archive to
  # shared-mime-info, and xar is rare enough off a Mac to give it up.
  xdg.mime = {
    enable = true;
    defaultApplications = {
      "application/x-apple-diskimage" = "app-windows-open-macos.desktop";
      "application/x-xar" = "app-windows-open-macos.desktop";
    };
  };

  # A disk image or package a browser downloads here opens in macOS, as it
  # would have on a Mac. The watcher is a look at the folder every two
  # seconds, so a download counts once it stops growing; one that comes
  # while macOS is off is handed over once it is up.
  systemd.user.services.app-windows-downloads = {
    description = "Hand downloaded Mac packages to macOS";
    wantedBy = [ "graphical-session.target" ];
    partOf = [ "graphical-session.target" ];
    unitConfig.ConditionPathExists = "%h/.config/app-windows/macos/token";
    serviceConfig = {
      ExecStart = "${pkgs.app-windows}/bin/app-windows watch %h/Downloads";
      Restart = "on-failure";
      RestartSec = 10;
    };
  };
}
