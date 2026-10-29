# A printer's disposable machine (modules/peripherals.nix,
# docs/peripherals.md): the printer plugged into its virtual controller,
# and CUPS for it, which the host's own CUPS sends jobs to over vsock. A
# printer's driver turns documents into its language, which is no job for
# BusyBox, so this is a NixOS service machine (service.nix), Kicksecure and
# all.
#
# The machine makes one queue, `printer`, for whatever it is given. A
# printer that speaks IPP over USB, as most made since 2015 do, is reached
# through ipp-usb, driverless; any other through CUPS' own USB backend and
# a driver its IEEE 1284 device ID matches, Gutenprint's or CUPS' own, or
# else CUPS' generic PostScript or PCL one. `losos-peripherals forward`
# then joins every connection the host makes to vsock port 3245 to CUPS' socket, so the
# host's socket for this machine, which the router makes when the port
# answers, means a queue to print to.
{ lib, pkgs, ... }:

let
  peripherals = lib.getExe pkgs.losos-peripherals;
  socket = "/run/cups/cups.sock";
in
{
  services.printing = {
    enable = true;
    # Only its socket, which only root here, and the host through it,
    # reaches.
    listenAddresses = [ ];
    startWhenNeeded = false;
    browsing = false;
    webInterface = false;
    drivers = [ pkgs.gutenprint ];
    # The host's CUPS sends `Host: localhost:0` over the socket.
    extraConf = "ServerAlias *";
  };

  # NixOS's CUPS installs xdg-utils, for its "Manage Printing" entry, and
  # Gutenprint has a script or two in Perl. Nothing here runs either:
  # perlless.nix's check (service.nix) is for the activation script, which
  # the /etc overlay replaces here all the same.
  system.forbiddenDependenciesRegexes = lib.mkForce [ ];

  # IPP over USB, on the loopback alone and announced to no one.
  environment.etc."ipp-usb/ipp-usb.conf".text = ''
    [network]
      http-min-port = 60000
      http-max-port = 60100
      dns-sd = disable
      interface = loopback
      ipv6 = disable
  '';
  systemd.services.ipp-usb = {
    description = "Reach a printer that speaks IPP over USB";
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.ipp-usb} standalone";
      Restart = "always";
      StateDirectory = "ipp-usb";
    };
  };

  # It has no network card, and so nothing to ask for an address on.
  networking.useDHCP = false;

  # The printer comes over USB/IP from sys-usb to vhci-hcd, or through
  # libvirt to the machine's own controller.
  boot.kernelModules = [ "vhci_hcd" ];

  systemd.services.losos-peripherals-receiver = {
    description = "Plug in the devices the host sends";
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${peripherals} receiver";
      Restart = "always";
    };
  };

  systemd.services.losos-peripherals-printer = {
    description = "Make a queue for the printer";
    after = [
      "cups.service"
      "ipp-usb.service"
      "losos-peripherals-receiver.service"
    ];
    requires = [ "cups.service" ];
    path = [
      pkgs.cups
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.gnused
    ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      TimeoutStartSec = "infinity";
    };
    # Until the printer is plugged in, which is once the router has seen
    # this machine listen for it.
    script = ''
      ipp_usb() {
        for d in /sys/bus/usb/devices/*:*; do
          [ "$(cat "$d/bInterfaceClass" 2>/dev/null)" = 07 ] &&
            [ "$(cat "$d/bInterfaceProtocol" 2>/dev/null)" = 04 ] && return 0
        done
        return 1
      }
      while :; do
        if ipp_usb; then
          # ipp-usb's first port, once it has the printer.
          if lpadmin -p printer -E -v ipp://localhost:60000/ipp/print -m everywhere 2>/dev/null; then
            echo "an IPP over USB printer, driverless"
            exit 0
          fi
        else
          uri=$(lpinfo --include-schemes usb -v | sed -n 's/^direct //p' | head -n 1)
          if [ -n "$uri" ]; then
            id=$(lpinfo -l --include-schemes usb -v | sed -n 's/^ *device-id = //p' | head -n 1)
            model=$(lpinfo --device-id "$id" -m 2>/dev/null | head -n 1 | cut -d ' ' -f 1)
            if [ -z "$model" ]; then
              case $id in
                *POSTSCRIPT* | *PostScript*) model=drv:///sample.drv/generic.ppd ;;
                *PCL*) model=drv:///sample.drv/generpcl.ppd ;;
              esac
            fi
            if [ -z "$model" ]; then
              echo "no driver for $id" >&2
              exit 1
            fi
            lpadmin -p printer -E -v "$uri" -m "$model"
            echo "$id, with $model"
            exit 0
          fi
        fi
        sleep 1
      done
    '';
  };

  systemd.services.losos-peripherals-print = {
    description = "Let the host reach the printer's queue";
    wantedBy = [ "multi-user.target" ];
    requires = [ "losos-peripherals-printer.service" ];
    after = [ "losos-peripherals-printer.service" ];
    serviceConfig = {
      ExecStart = "${peripherals} forward --port 3245 --to ${socket}";
      Restart = "always";
    };
  };
}
