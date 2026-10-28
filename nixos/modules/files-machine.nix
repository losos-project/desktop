# The files machine, sys-files (docs/files-machine.md): the storage a person
# plugs in is mounted there and not on the host, as Qubes keeps it out of
# dom0. A USB stick's or SD card's filesystem is whatever its last owner
# put on it, and the kernel's code that parses it is some of its most
# attacked; here it runs in a machine that has no network card at all,
# nothing of anyone's, and nothing kept from one start to the next, and
# only the files cross to the host.
#
#  - USB drives reach it over USB/IP from the peripherals' router
#    (peripherals.nix), whose receiver runs there; SD card readers, PCI
#    class 0x0805, are handed to it whole at boot.
#  - losos-filesd (src/losos-filesd) is its PID 1: it mounts each
#    filesystem it is given and serves it, on request, as SFTP over vsock.
#  - On the host, losos-files-host mounts each one for the person at the
#    screen under /run/media/<user>/<name> with sshfs, where Files and
#    every app find it, and lets go of it when it goes.
#
# It is lazy: it starts when the router hands it a drive, or at boot when
# it holds a card reader, and powers itself off five minutes after the
# last drive went.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  name = "sys-files";
  cid = 4101;
  ports = {
    list = 5100;
    sftp = 5101;
    eject = 5102;
  };
  libvirt = config.virtualisation.libvirtd.package;
  devices = "/run/losos-service-machines/${name}/devices";

  filesdConfig = pkgs.writeText "losos-filesd.conf" ''
    sftp-server ${pkgs.openssh}/libexec/sftp-server
    idle ${toString config.losos.machines.files.idle}
    run losos-vm-call serve ${toString ports.list} losos-filesd list
    run losos-vm-call serve ${toString ports.sftp} losos-filesd sftp
    run losos-vm-call serve ${toString ports.eject} losos-filesd eject
    run losos-peripherals receiver
  '';

  # sshfs's "ssh": the volume's sftp-server in the machine, on stdio.
  sftp = pkgs.writeShellScript "losos-files-sftp" ''
    exec ${lib.getExe pkgs.losos-vm-call} call files-sftp "$LOSOS_FILES_NAME"
  '';

  # The host's half: every volume the machine lists, mounted for the
  # person whose session is in front, as theirs, and nothing once the
  # machine stops.
  filesHost = pkgs.writeShellApplication {
    name = "losos-files-host";
    runtimeInputs = [
      libvirt
      pkgs.losos-vm-call
      pkgs.sshfs
      pkgs.fuse3
      pkgs.gnugrep
      pkgs.coreutils
      pkgs.systemd
      pkgs.util-linux
    ];
    text =
      builtins.replaceStrings
        [ "@machine@" "@sftp@" "@list@" "@state@" ]
        [ name "${sftp}" "files-list" "/run/losos-files" ]
        (builtins.readFile ./losos-files-host.sh);
  };

  # `losos-files eject <name>`, for Files and a terminal.
  filesTool = pkgs.writeShellApplication {
    name = "losos-files";
    runtimeInputs = [ pkgs.losos-vm-call ];
    text = ''
      case ''${1:-} in
        list) exec losos-vm-call call files-list ;;
        eject) [ $# -eq 2 ] && exec losos-vm-call call files-eject "$2" ;;
      esac
      echo "usage: losos-files list | eject <name>" >&2
      exit 2
    '';
  };
in
{
  options.losos.machines.files.idle = lib.mkOption {
    type = lib.types.ints.positive;
    default = 300;
    description = "Seconds the machine waits with nothing plugged in before it powers off.";
  };

  options.losos.machines.files.enable = lib.mkOption {
    type = lib.types.bool;
    default = config.losos.machines.network.enable;
    description = ''
      Whether drives and SD cards are mounted in the files machine rather
      than on the host.
    '';
  };

  config = lib.mkIf config.losos.machines.files.enable {
    losos.busyboxGuests.${name} = {
      init = "${lib.getExe pkgs.losos-filesd} init";
      hostKernelTree = true;
      packages = [
        pkgs.losos-filesd
        pkgs.losos-vm-call
        pkgs.losos-peripherals
      ];
      modules = [
        "virtio_balloon"
        "vhci_hcd"
        "usb_storage"
        "uas"
        "sd_mod"
        "mmc_block"
      ];
      files."/etc/losos-filesd.conf" = filesdConfig;
      # Kicksecure keeps rare file systems and optical drives out of a
      # system to keep their parsers away from its kernel. This machine is
      # where an untrusted drive's parsers belong, away from the host's
      # kernel, so a Mac's USB stick or a DVD opens here.
      kicksecure.allowModules =
        (import ../guest/kicksecure-settings.nix { inherit (pkgs.stdenv.hostPlatform) isx86; })
        .disabledModules.filesystems
        ++ [
          "cdrom"
          "sr_mod"
        ];
    };

    losos.serviceMachines.${name} = {
      busybox = name;
      title = "The files machine";
      memory = 256;
      cpus = 2;
      inherit cid;
      # No network card: nothing it reads can reach anything but the host.
      interfaces = [ ];
      # Off when idle, until the next drive (the machine's own choice).
      whenStopped = "stop";
      passthrough.pciClasses = [ "0x0805" ];
    };

    # Started by the router for a drive, or here when it holds a card
    # reader, which nothing else would start it for.
    systemd.services.losos-files-readers = {
      description = "Start the files machine for its card readers";
      wantedBy = [ "multi-user.target" ];
      after = [ "losos-passthrough-${name}.service" ];
      unitConfig.ConditionPathExistsGlob = "${devices}/pci-*.xml";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.systemd}/bin/systemctl start --no-block losos-service-machine-${name}.service";
      };
    };

    systemd.services.losos-files-host = {
      description = "Mount the files machine's drives for the person at the screen";
      bindsTo = [ "losos-service-machine-${name}.service" ];
      after = [
        "losos-service-machine-${name}.service"
        "losos-vm-call-relay.service"
      ];
      wantedBy = [ "losos-service-machine-${name}.service" ];
      environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
      serviceConfig = {
        ExecStart = lib.getExe filesHost;
        Restart = "on-failure";
        RuntimeDirectory = "losos-files";
      };
    };

    losos.vmCalls = {
      files-list = {
        inherit cid;
        port = ports.list;
        allow = [ "active-session" ];
      };
      # Root's alone: losos-files-host mounts for the person.
      files-sftp = {
        inherit cid;
        port = ports.sftp;
      };
      files-eject = {
        inherit cid;
        port = ports.eject;
        allow = [ "active-session" ];
      };
    };

    environment.systemPackages = [ filesTool ];
  };
}
