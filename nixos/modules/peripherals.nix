# Peripherals in machines of their own (docs/peripherals.md), as Qubes
# keeps them out of dom0: the host drives no USB device it does not need
# to be used, and no Bluetooth radio at all.
#
# - sys-usb holds every USB controller the host can spare (keep, below),
#   passed through whole, and says what is plugged into them.
# - sys-bluetooth runs BlueZ and is given the Bluetooth radio.
# - Network cards go to sys-net and drives, card readers and phones to
#   sys-files, the network and files machines (network-machines.nix).
# - Anything else, a camera, a sound card, a printer, gets a disposable
#   machine made when it is plugged in and destroyed when it is pulled
#   out, from the templates below. A sound card's is NixOS with
#   Kicksecure's hardening and a sound server (nixos/guest/sound.nix),
#   which the host's PipeWire plays to through a socket the router makes.
# - Keyboards, pointers, game pads and security keys stay on the host,
#   with the controller of the disk it booted from.
#
# losos-peripherals' router (src/losos-peripherals) decides and moves each
# device. Every other one of these machines is BusyBox and that program on
# the host's own kernel (busybox-guests.nix). All are hidden from the Machines app, and
# lazy: none runs before a device needs it, and sys-usb stops a minute
# after its last device is gone. Settings' Bluetooth page keeps working
# through the host's bluetoothctl, which is the router's.
#
# A phone has none of this (nixos/halium/): its USB and Bluetooth are the
# vendor's, behind Android's HALs.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  peripherals = lib.getExe pkgs.losos-peripherals;
  libvirt = config.virtualisation.libvirtd.package;
  guests = config.losos.busyboxGuests;

  # The system bus BlueZ needs, and nothing else on it.
  busConfig = pkgs.writeText "system.conf" ''
    <!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
     "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
    <busconfig>
      <type>system</type>
      <listen>unix:path=/run/dbus/system_bus_socket</listen>
      <auth>EXTERNAL</auth>
      <policy context="default">
        <allow user="*"/>
        <allow own="*"/>
        <allow send_type="method_call"/>
        <allow send_type="signal"/>
        <allow send_type="method_return"/>
        <allow send_type="error"/>
        <allow receive_type="method_call"/>
        <allow receive_type="signal"/>
        <allow receive_type="method_return"/>
        <allow receive_type="error"/>
      </policy>
    </busconfig>
  '';

  bluetoothd = pkgs.runCommand "bluetoothd-bin" { } ''
    mkdir -p $out/bin
    ln -s ${pkgs.bluez}/libexec/bluetooth/bluetoothd $out/bin/bluetoothd
  '';

  # Only root, which the machine's one program runs as.
  passwd = pkgs.writeText "passwd" "root:x:0:0:root:/:/bin/sh\n";
  group = pkgs.writeText "group" "root:x:0:\n";
  # D-Bus wants a machine ID; a new one each boot, as the machine is.
  busSetup = pkgs.writeScript "bus-setup" ''
    #!${pkgs.busybox}/bin/sh
    mkdir -p /run/dbus /var/lib
    ${pkgs.dbus}/bin/dbus-uuidgen --ensure=/etc/machine-id
  '';

  # The router's view of the machines: the units that start them, their
  # vsock addresses, and where Bluetooth's state is kept.
  routerConfig = pkgs.writeText "losos-peripherals.json" (
    builtins.toJSON {
      usb = {
        name = "sys-usb";
        cid = 4110;
        unit = "losos-service-machine-sys-usb.service";
        devices = "/run/losos-service-machines/sys-usb/devices";
      };
      network = {
        name = "sys-net";
        cid = 4100;
        unit = "losos-service-machine-sys-net.service";
      };
      storage = {
        name = "sys-files";
        cid = 4101;
        unit = "losos-service-machine-sys-files.service";
      };
      bluetooth = {
        name = "sys-bluetooth";
        cid = 4111;
        unit = "losos-service-machine-sys-bluetooth.service";
      };
      disposable = disposable;
      disposables.audio = {
        template = sound;
        bridges = [
          {
            dir = "/run/losos-peripherals/sound";
            port = 3244;
          }
        ];
      };
      disposable_cids = [
        4120
        4199
      ];
      state = "/var/lib/losos-peripherals";
      control = "/run/losos-peripherals/control";
    }
  );

  # The disposable machines' definitions, which the router fills in with
  # each one's name, vsock address and the host's acceleration: no disk,
  # a controller for a device on the host's own ports, the host's store
  # (service-machines.nix shares it the same way), and memory it gives
  # back as it frees it.
  domain =
    {
      file,
      title,
      memory,
      os,
    }:
    pkgs.writeText file ''
      <domain type='@TYPE@'>
        <name>@NAME@</name>
        <title>${title}</title>
        <metadata>
          <pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>
            <pm:label>black</pm:label>
            <pm:service>true</pm:service>
          </pm:machine>
        </metadata>
        <memory unit='MiB'>${toString memory}</memory>
        <vcpu>1</vcpu>
        <os>
          <type arch='${pkgs.stdenv.hostPlatform.qemuArch}' machine='${
            if pkgs.stdenv.hostPlatform.isx86 then "q35" else "virt"
          }'>hvm</type>
          <kernel>${os.kernel}</kernel>
          <initrd>${os.initrd}</initrd>
          <cmdline>${os.cmdline}</cmdline>
        </os>
        @CPU@${
          # As service-machines.nix's: a PC's kernel booted by libvirt has
          # ACPI only when asked for, and stalls without it.
          lib.optionalString pkgs.stdenv.hostPlatform.isx86 ''
            <features>
              <acpi/>
              <apic/>
            </features>''
        }
        <on_poweroff>destroy</on_poweroff>
        <on_reboot>destroy</on_reboot>
        <on_crash>destroy</on_crash>
        <devices>
          <controller type='usb' model='qemu-xhci'/>
          <filesystem type='mount' accessmode='squash'>
            <source dir='/nix/store'/>
            <target dir='nix-store'/>
            <readonly/>
          </filesystem>
          <vsock model='virtio'>
            <cid auto='no' address='@CID@'/>
          </vsock>
          <serial type='pty'>
            <!-- Kept after the machine is gone, to read what it did. -->
            <log file='/var/log/libvirt/qemu/@NAME@-console.log' append='off'/>
            <target port='0'/>
          </serial>
          <console type='pty'/>
          <rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>
          <memballoon model='virtio' freePageReporting='on'/>
        </devices>
      </domain>
    '';

  disposable = domain {
    file = "disposable.xml";
    title = "A device's disposable machine";
    memory = 256;
    os = guests.disposable.build;
  };

  # A sound card's: a NixOS service machine, evaluated as
  # service-machines.nix evaluates those, on the image's own kernel.
  soundSystem =
    (import "${modulesPath}/../lib/eval-config.nix" {
      system = null;
      modules = [
        ../guest/service.nix
        ../guest/sound.nix
        {
          losos.service.name = "sound";
          nixpkgs.pkgs = pkgs;
          boot.kernelPackages = lib.mkForce config.boot.kernelPackages;
        }
      ];
    }).config;
  sound = domain {
    file = "disposable-sound.xml";
    title = "A sound card's disposable machine";
    # systemd, PipeWire and WirePlumber, on a root in memory.
    memory = 512;
    os = {
      # The image's kernel image, which carries the certificate of every
      # module (kernel-hardening.nix), as service-machines.nix boots.
      kernel =
        config.system.build.kernelImage
          or "${soundSystem.system.build.kernel}/${soundSystem.system.boot.loader.kernelFile}";
      initrd = "${soundSystem.system.build.initialRamdisk}/${soundSystem.system.boot.loader.initrdFile}";
      cmdline = "init=${soundSystem.system.build.toplevel}/init ${toString soundSystem.boot.kernelParams}";
    };
  };

  # The host's side of a sound card's machine: for each socket the router
  # has made, a sink tunnelled to the machine's sound server, and a source
  # when the card records, in the signed-in person's PipeWire; for each
  # socket gone, its tunnels unloaded. The card's name comes from the
  # machine, which is the device's, so only plain characters of it show.
  soundTunnels = pkgs.writeShellScript "losos-peripherals-sound" ''
    dir=/run/losos-peripherals/sound
    pactl=${pkgs.pulseaudio}/bin/pactl
    loaded=$($pactl list modules short)
    for sock in "$dir"/*; do
      [ -S "$sock" ] || continue
      grep -qF "server=unix:$sock " <<<"$loaded" && continue
      name=''${sock##*/}
      remote() { $pactl --server="unix:$sock" "$@"; }
      desc=$(remote list sinks | sed -n 's/^\tDescription: //p' | head -n 1 | tr -cd '[:alnum:] ._()-' | cut -c 1-60)
      desc=''${desc:-USB sound card}
      $pactl load-module module-tunnel-sink "server=unix:$sock" "sink_name=losos-$name" \
        "sink_properties=\"device.description='$desc'\""
      if remote list sources short | grep -q alsa_input; then
        $pactl load-module module-tunnel-source "server=unix:$sock" "source_name=losos-$name-in" \
          "source_properties=\"device.description='$desc'\""
      fi
    done
    $pactl list modules short | while read -r id module args; do
      case $module in module-tunnel-sink | module-tunnel-source) ;; *) continue ;; esac
      sock=''${args#server=unix:}
      sock=''${sock%% *}
      case $sock in "$dir"/*) [ -S "$sock" ] || $pactl unload-module "$id" ;; esac
    done
  '';
in
{
  losos.busyboxGuests = {
    # Host controllers' drivers and usbip-host, and no class driver: what
    # is plugged in is enumerated here and goes on as it is.
    sys-usb = {
      init = "${peripherals} usb-machine";
      packages = [ pkgs.losos-peripherals ];
      # Its modules from the host's tree, and an initrd of a few MiB.
      fromHostStore = true;
      modules = [
        "xhci_pci"
        "ehci_pci"
        "ohci_pci"
        "uhci_hcd"
        "usbip_host"
        "virtio_balloon"
      ];
    };

    sys-bluetooth = {
      init = "${peripherals} bluetooth-machine --bus-config /etc/dbus-system.conf";
      packages = [
        pkgs.losos-peripherals
        pkgs.bluez
        pkgs.dbus
        bluetoothd
      ];
      modules = [
        "vhci_hcd"
        "btusb"
        "virtio_balloon"
      ];
      # Any radio's driver and firmware, from the host's
      # (busybox-host-kernel.nix).
      hostKernelTree = true;
      shares = [
        {
          # Pairings, kept on the host across starts.
          source = "/var/lib/losos-peripherals/bluetooth";
          target = "/var/lib/bluetooth";
          tag = "bluetooth";
          writable = true;
        }
      ];
      files = {
        "/etc/dbus-system.conf" = busConfig;
        "/etc/passwd" = passwd;
        "/etc/group" = group;
        "/etc/init.d/10-bus" = busSetup;
      };
    };

    disposable = {
      init = "${peripherals} receiver";
      packages = [ pkgs.losos-peripherals ];
      modules = [
        "vhci_hcd"
        "virtio_balloon"
      ];
      # A device's driver and firmware, whichever it is, from the host's
      # (busybox-host-kernel.nix).
      hostKernelTree = true;
    };
  };

  losos.serviceMachines = {
    sys-usb = {
      busybox = "sys-usb";
      title = "The USB machine";
      memory = 128;
      cid = 4110;
      # USB controllers: UHCI, OHCI, EHCI and xHCI, and USB4's.
      passthrough.pciClasses = [ "0x0c03" ];
      # The host's while the machine is stopped, ports held by the router;
      # libvirt takes them as it starts.
      passthrough.bindAtBoot = false;
    };
    sys-bluetooth = {
      busybox = "sys-bluetooth";
      title = "The Bluetooth machine";
      memory = 128;
      cid = 4111;
      # For a radio on one of the host's own ports.
      xhci = true;
    };
  };

  # Which controllers stay, worked out before any is passed through, from
  # what is plugged in by then: the boot disk, keyboards and pointers.
  # USB enumerates behind udev's coldplug, so this waits for the device
  # list to stop growing, a second at a time, up to ten.
  systemd.services.losos-peripherals-keep = {
    description = "Decide which USB controllers the host keeps";
    after = [ "systemd-udev-trigger.service" ];
    # Early, as the passthrough units are (passthrough.nix).
    unitConfig.DefaultDependencies = false;
    wantedBy = [ "sysinit.target" ];
    before = [
      "sysinit.target"
    ]
    ++ map (name: "losos-passthrough-${name}.service") (lib.attrNames config.losos.serviceMachines);
    requiredBy = [ "losos-passthrough-sys-usb.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    script = ''
      last=-1
      for _ in 1 2 3 4 5 6 7 8 9 10; do
        now=$(ls /sys/bus/usb/devices | wc -l)
        [ "$now" = "$last" ] && break
        last=$now
        sleep 1
      done
      exec ${peripherals} keep
    '';
    path = [ pkgs.coreutils ];
  };

  systemd.services.losos-peripherals = {
    description = "Move USB devices into the machines that drive them";
    wantedBy = [ "multi-user.target" ];
    requires = [ "libvirtd.service" ];
    after = [
      "libvirtd.service"
      "libvirt-defaults.service"
      "losos-passthrough-sys-usb.service"
    ];
    path = [
      libvirt
      pkgs.kmod
      config.systemd.package
    ];
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig = {
      ExecStart = "${peripherals} serve --config ${routerConfig}";
      Restart = "always";
      RestartSec = 1;
      StateDirectory = "losos-peripherals";
      StateDirectoryMode = "0700";
    };
  };

  systemd.tmpfiles.rules = [ "d /var/lib/losos-peripherals/bluetooth 0700 root root -" ];

  # Sound cards' machines in the PipeWire of whoever is signed in, as the
  # router makes and removes their sockets.
  systemd.user.paths.losos-peripherals-sound = lib.mkIf config.services.pipewire.pulse.enable {
    wantedBy = [ "default.target" ];
    pathConfig.PathChanged = "/run/losos-peripherals/sound";
  };
  systemd.user.services.losos-peripherals-sound = lib.mkIf config.services.pipewire.pulse.enable {
    description = "Play to sound cards in machines of their own";
    # Once at sign-in too, for cards plugged in before.
    wantedBy = [ "default.target" ];
    after = [ "pipewire-pulse.service" ];
    wants = [ "pipewire-pulse.service" ];
    serviceConfig = {
      Type = "oneshot";
      ExecStart = soundTunnels;
    };
  };

  # For devices the host keeps that sys-usb holds: they come to the host's
  # virtual controller over USB/IP.
  boot.kernelModules = [ "vhci_hcd" ];

  # Settings' Bluetooth page runs bluetoothctl, which is the router's here
  # and BlueZ's in sys-bluetooth. The host's BlueZ stays installed and
  # never starts: it starts only where /sys/class/bluetooth exists, and no
  # radio is ever the host's.
  environment.systemPackages = [
    (lib.hiPrio (
      pkgs.writeShellScriptBin "bluetoothctl" ''
        exec ${peripherals} bluetoothctl "$@"
      ''
    ))
  ];
}
