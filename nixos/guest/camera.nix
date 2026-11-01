# A camera's disposable machine (modules/peripherals.nix,
# docs/peripherals.md): the camera plugged into its virtual controller, and
# ffmpeg to read it, which streams to the host over vsock only while an
# app on the host uses the camera. A camera's video is code someone else
# wrote, and MJPEG or H.264 to decode at that, which is no job for BusyBox
# or for the host, so this is a NixOS service machine (service.nix),
# Kicksecure and all.
#
# `losos-peripherals camera-machine` listens on vsock port 3246 once the
# camera is in, so the host's socket for this machine, which the router
# makes when the port answers, means a camera to open. It says the
# camera's name when asked, and when asked for the stream runs ffmpeg,
# which turns whatever the camera gives into the one size and format the
# host's v4l2loopback device has (src/losos-peripherals/src/camera.rs),
# until the host hangs up.
{ lib, pkgs, ... }:

{
  # It has no network card, and so nothing to ask for an address on.
  networking.useDHCP = false;

  # The camera comes over USB/IP from sys-usb to vhci-hcd, or through
  # libvirt to the machine's own controller.
  boot.kernelModules = [ "vhci_hcd" ];

  systemd.services.losos-peripherals-receiver = {
    description = "Plug in the devices the host sends";
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-peripherals} receiver";
      Restart = "always";
    };
  };

  systemd.services.losos-peripherals-camera = {
    description = "Let the host reach the camera";
    wantedBy = [ "multi-user.target" ];
    after = [ "losos-peripherals-receiver.service" ];
    path = [ pkgs.ffmpeg-headless ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-peripherals} camera-machine";
      Restart = "always";
    };
  };
}
