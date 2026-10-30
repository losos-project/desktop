# A sound card's disposable machine (modules/peripherals.nix,
# docs/peripherals.md): the card plugged into its virtual controller, and a
# sound server for it, which the host's own reaches over vsock. A sound
# card is no use with only its driver, as a BusyBox machine would have it,
# so this one is a NixOS service machine (service.nix), Kicksecure and all.
#
# Its sound server is PipeWire, system-wide, since nobody signs in here,
# with its PulseAudio server. `losos-peripherals forward` joins every
# connection the host makes to vsock port 3244 to that server's socket, and
# the host's PipeWire tunnels a sink to it, and a source when the card
# records too. It starts once the card is in PipeWire, so the host's
# socket for this machine, which the router makes when the port answers,
# means a card to play to.
{ lib, pkgs, ... }:

let
  peripherals = lib.getExe pkgs.losos-peripherals;
  socket = "/run/pulse/native";
in
{
  services.pipewire = {
    enable = true;
    systemWide = true;
    pulse.enable = true;
  };
  # It has no network card, and so nothing to ask for an address on.
  networking.useDHCP = false;

  # The card comes over USB/IP from sys-usb to vhci-hcd, or through
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

  systemd.services.losos-peripherals-sound = {
    description = "Let the host reach the sound server";
    wantedBy = [ "multi-user.target" ];
    requires = [ "pipewire-pulse.socket" ];
    after = [
      "pipewire-pulse.socket"
      "losos-peripherals-receiver.service"
    ];
    path = [ pkgs.pulseaudio ];
    environment.PULSE_SERVER = "unix:${socket}";
    # Until the card is a sink of the server's: the device is plugged in
    # once the router has seen this machine listen for it, and is in
    # PipeWire a moment after that.
    preStart = ''
      until pactl list sinks short | grep -q alsa_output; do
        sleep 1
      done
    '';
    serviceConfig = {
      ExecStart = "${peripherals} forward --port 3244 --to ${socket}";
      Restart = "always";
      TimeoutStartSec = "10min";
    };
  };
}
