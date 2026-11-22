# MIME defaults for Danube. The browser itself runs in the Danube machine
# and links that distribution's WPE WebKit (danube-machine.nix). This
# module does not build Danube or WebKit.
#
# Uranium, Chromium under the OS's name with a Flatpak build, was here
# before; Danube replaced it, and its options went with it
# (docs/danube.md, "What it replaced").
{
  lib,
  ...
}:
{
  # losos.danube.home and losos.danube.jit wrote /etc/danube/settings.conf
  # under the user's own; nothing set them, so they went, and Danube's
  # built-in defaults (an empty page, the JIT off) are what a user who
  # writes no ~/.config/danube/settings.conf gets (docs/danube.md,
  # "Settings").
  options.losos.captivePortal = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Kept so a configuration can still set it. The host no longer runs
        a captive-portal watcher: that binary was Danube linked against a
        WebKit this repository compiled itself.
      '';
    };
  };

  config = {
    # The browser is the Danube machine (danube-machine.nix), built there
    # against Arch's WPE WebKit. This module does not install a host
    # binary: that build compiled WebKit, which is not a losos-project
    # package and does not substitute from cache.nixos.org.

    # The default for web pages and links, which xdg-open, the portal's
    # OpenURI and derisk's own launcher all read from mimeapps.list.
    xdg.mime.defaultApplications = lib.genAttrs [
      "text/html"
      "application/xhtml+xml"
      "x-scheme-handler/http"
      "x-scheme-handler/https"
    ] (_: "org.losos.Danube.desktop");
  };
}
