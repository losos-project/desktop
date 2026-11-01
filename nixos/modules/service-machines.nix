# Service machines: the machines the OS runs parts of itself in, such as
# schizo mode's Tor gateway (schizo.nix) and the network machine
# (network-machine.nix), rather than the machines a person makes
# (machines.nix). Each is either a NixOS system of its own, evaluated here
# with the image's package set and kernel, and booted from the host's store
# (nixos/guest/service.nix says how), or a BusyBox machine
# (busybox-guests.nix) booted from its initrd; either way it needs no disk
# image and is updated with the OS.
#
# Each is a libvirt domain like any other, defined again from this image
# at every start of its unit, losos-service-machine-<name>.service, which
# nothing starts by itself: the module that declares one says when it runs,
# with wantedBy, partOf and the like on that unit. They are the OS's, not
# a person's, so the Machines app does not show them: their pm metadata
# says `service`, which derisk-machines skips.
#
# They are lazy: nothing starts one before it is needed, its state disk is
# made the first time it starts, and its balloon reports the pages it
# frees, which the host takes back, so an idle one holds little memory.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  libvirt = config.virtualisation.libvirtd.package;
  stateDir = name: "/var/lib/losos-service-machines/${name}";
  # The same UUID at every definition: libvirt takes a definition without
  # one as a new domain, and refuses it while the old one of that name
  # exists, so a machine that stopped could never be defined again.
  uuid =
    name:
    let
      h = builtins.hashString "sha256" "losos-service-machine-${name}";
      p = start: len: builtins.substring start len h;
    in
    "${p 0 8}-${p 8 4}-${p 12 4}-${p 16 4}-${p 20 12}";
  runDir = name: "/run/losos-service-machines/${name}";

  system =
    name: machine:
    (import "${modulesPath}/../lib/eval-config.nix" {
      system = null;
      modules = [
        ../guest/service.nix
        machine.module
        {
          losos.service.name = name;
          nixpkgs.pkgs = pkgs;
          # The image's own kernel, so host and machines share one in /usr.
          boot.kernelPackages = lib.mkForce config.boot.kernelPackages;
        }
      ];
    }).config;

  # libvirt's view of one: the image's own kernel image, which carries the
  # certificate of every module the kernel builds (kernel-hardening.nix;
  # the plain build would load none, and a machine loads no module the
  # host's tree lacks), and its initrd and command line from
  # its system, the host's store shared read-only over 9p, its state disk and
  # its cards. @TYPE@ and @CPU@ are KVM's when the host has it and
  # emulation's otherwise, filled in when it is defined.
  # What it boots: a NixOS system's kernel, initrd and command line, or a
  # BusyBox machine's.
  boot =
    name: machine:
    if machine.busybox != null then
      config.losos.busyboxGuests.${machine.busybox}.build
    else
      let
        guest = system name machine;
      in
      {
        # The image's kernel image, which carries the certificate of every
        # module (kernel-hardening.nix); the plain build would load none.
        # The guest's kernel package is the host's, so its modules are ones
        # that image trusts.
        kernel =
          config.system.build.kernelImage
            or "${guest.system.build.kernel}/${guest.system.boot.loader.kernelFile}";
        initrd = "${guest.system.build.initialRamdisk}/${guest.system.boot.loader.initrdFile}";
        cmdline = "init=${guest.system.build.toplevel}/init ${toString guest.boot.kernelParams}";
      };

  # A BusyBox machine's shares, which it mounts by tag.
  shares =
    machine:
    lib.optionals (machine.busybox != null) config.losos.busyboxGuests.${machine.busybox}.shares;

  # libvirt's view of one: its kernel, initrd and command line, the host's
  # store shared read-only over 9p, its state disk and its cards. @TYPE@
  # and @CPU@ are KVM's when the host has it and emulation's otherwise,
  # filled in when it is defined.
  domain =
    name: machine:
    let
      os = boot name machine;
    in
    pkgs.writeText "${name}.xml" ''
      <domain type='@TYPE@'>
        <name>${name}</name>
        <uuid>${uuid name}</uuid>
        <title>${machine.title}</title>
        <metadata>
          <pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>
            <pm:label>black</pm:label>
            <pm:service>true</pm:service>
          </pm:machine>
        </metadata>
        <memory unit='MiB'>${toString machine.memory}</memory>
        <vcpu>${toString machine.cpus}</vcpu>${lib.optionalString machine.sharedMemory ''

          <!-- virtiofs' daemon reads and writes the machine's memory. -->
          <memoryBacking>
            <source type='memfd'/>
            <access mode='shared'/>
          </memoryBacking>''}
        <os>
          <type arch='${pkgs.stdenv.hostPlatform.qemuArch}' machine='${
            if pkgs.stdenv.hostPlatform.isx86 then "q35" else "virt"
          }'>hvm</type>
          <kernel>${os.kernel}</kernel>
          <initrd>${os.initrd}</initrd>
          <cmdline>${os.cmdline}</cmdline>
        </os>
        @CPU@${
          # A PC's kernel finds its devices' interrupts through ACPI, which a
          # domain booted by its kernel has only when asked for; on Arm the
          # `virt` board describes itself in a device tree instead.
          lib.optionalString pkgs.stdenv.hostPlatform.isx86 ''

            <features>
              <acpi/>
              <apic/>
            </features>''
        }
        <on_poweroff>destroy</on_poweroff>
        <on_reboot>restart</on_reboot>
        <on_crash>restart</on_crash>
        <devices>
          <filesystem type='mount' accessmode='squash'>
            <source dir='/nix/store'/>
            <target dir='nix-store'/>
            <readonly/>
          </filesystem>${
            lib.concatMapStrings (share: ''

              <filesystem type='mount' accessmode='${if share.writable or false then "mapped" else "squash"}'>
                <source dir='${share.source}'/>
                <target dir='${share.tag}'/>${lib.optionalString (!(share.writable or false)) "\n    <readonly/>"}
              </filesystem>'') (shares machine)
          }${lib.optionalString machine.xhci ''

            <controller type='usb' model='qemu-xhci'/>''}${
            lib.optionalString (machine.stateSize != null) ''

              <disk type='file' device='disk'>
                <driver name='qemu' type='qcow2' discard='unmap'/>
                <source file='${stateDir name}/state.qcow2'/>
                <target dev='vda' bus='virtio'/>
              </disk>''
          }${
            lib.concatMapStrings (card: ''

              <interface type='network'>
                <source network='${card.network}'/>
                <mac address='${card.mac}'/>
                <model type='virtio'/>
              </interface>'') machine.interfaces
          }${
            lib.optionalString (machine.cid != null) ''

              <vsock model='virtio'>
                <cid auto='no' address='${toString machine.cid}'/>
              </vsock>''
          }
          <serial type='pty'>
            <!-- Kept after the machine is gone, to read why it went. -->
            <log file='/var/log/libvirt/qemu/${name}-console.log' append='off'/>
            <target port='0'/>
          </serial>
          <console type='pty'/>
          <rng model='virtio'>
            <backend model='random'>/dev/urandom</backend>
          </rng>
          <!-- Pages the machine frees go back to the host as it frees
               them, so an idle machine holds little more than it uses. -->
          <memballoon model='virtio' freePageReporting='on'/>
          ${machine.extraDevices}
          @DEVICES@
        </devices>
      </domain>
    '';

  unit = name: machine: {
    description = machine.title;
    requires = [ "libvirtd.service" ];
    after = [ "libvirtd.service" ];
    path = [
      libvirt
      pkgs.qemu-utils
      pkgs.coreutils
      pkgs.gnused
      pkgs.gawk
    ];
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      StateDirectory = "losos-service-machines/${name}";
      StateDirectoryMode = "0700";
    };
    # Defined again at every start from this image's definition, so an
    # update's machine is the one that starts. KVM when the host has it;
    # QEMU's emulation of its own CPU otherwise. Devices only known at boot,
    # such as a network card handed to the machine, are whatever other
    # units left in its run directory's devices/ before this one started;
    # not a RuntimeDirectory=, which would be emptied when this stops while
    # they stay started.
    script = ''
      run=${runDir name}
      mkdir -p "$run/devices"
      ${lib.optionalString (machine.stateSize != null) ''
        [ -e ${stateDir name}/state.qcow2 ] || qemu-img create -q -f qcow2 ${stateDir name}/state.qcow2 ${machine.stateSize}
      ''}
      if [ -e /dev/kvm ]; then
        type=kvm cpu="<cpu mode='host-passthrough'/>"
      else
        type=qemu cpu="<cpu mode='maximum'/>"
      fi
      sed -e "s|@TYPE@|$type|" -e "s|@CPU@|$cpu|" ${domain name machine} |
        awk -v devices="$run/devices" '
          /@DEVICES@/ {
            while (("ls " devices "/*.xml 2>/dev/null" | getline file) > 0)
              while ((getline line < file) > 0) print line
            next
          }
          { print }
        ' > "$run/domain.xml"
      virsh -q define "$run/domain.xml" >/dev/null
      [ "$(virsh -q domstate ${name})" = running ] || virsh -q start ${name}
    '';
    # Asked to shut down, then stopped if it has not within half a minute.
    preStop = ''
      virsh -q shutdown ${name} || true
      for _ in $(seq 30); do
        [ "$(virsh -q domstate ${name})" = running ] || exit 0
        sleep 1
      done
      virsh -q destroy ${name} || true
    '';
  };
  # Follows the machine while its unit is started, and does what it says
  # when the machine stops without the unit being stopped.
  watch = name: machine: {
    description = "Follow ${machine.title}";
    bindsTo = [ "losos-service-machine-${name}.service" ];
    after = [ "losos-service-machine-${name}.service" ];
    wantedBy = [ "losos-service-machine-${name}.service" ];
    path = [
      libvirt
      pkgs.systemd
      pkgs.coreutils
    ];
    environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
    serviceConfig.Restart = "on-failure";
    script = ''
      while :; do
        while [ "$(virsh -q domstate ${name} 2>/dev/null)" = running ]; do
          sleep 2
        done
        ${
          if machine.whenStopped == "restart" then
            ''
              echo "${name} stopped; starting it again"
              sleep 1
              virsh -q start ${name} || sleep 5
            ''
          else
            ''
              echo "${name} stopped"
              exec systemctl stop --no-block losos-service-machine-${name}.service
            ''
        }
      done
    '';
  };
in
{
  options.losos.serviceMachines = lib.mkOption {
    default = { };
    description = ''
      The machines the OS runs parts of itself in, by name: each boots from
      the host's store and is started by losos-service-machine-<name>.service.
    '';
    type = lib.types.attrsOf (
      lib.types.submodule {
        options = {
          module = lib.mkOption {
            type = lib.types.deferredModule;
            default = { };
            description = "Its NixOS configuration, on top of nixos/guest/service.nix.";
          };
          busybox = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "sys-net";
            description = ''
              The losos.busyboxGuests machine it boots instead of a NixOS
              system, whose shares it is given; `module` is unused then.
            '';
          };
          sharedMemory = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = ''
              Whether its memory is shared with the host's processes, which
              virtiofs needs: for a machine handed host directories over
              virtiofs among the devices its unit is started with.
            '';
          };
          xhci = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = ''
              Whether it has a USB controller, for USB devices handed to it
              while it runs.
            '';
          };
          title = lib.mkOption {
            type = lib.types.str;
            description = "What it is, in a few words: its unit's description and libvirt's title.";
          };
          memory = lib.mkOption {
            type = lib.types.ints.positive;
            default = 256;
            description = "Its memory in MiB.";
          };
          cpus = lib.mkOption {
            type = lib.types.ints.positive;
            default = 1;
            description = "Its virtual CPUs.";
          };
          stateSize = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = "256M";
            description = ''
              The size of its state disk, /dev/vda in the machine, kept under
              /var/lib/losos-service-machines/<name>; none without.
            '';
          };
          interfaces = lib.mkOption {
            default = [ ];
            description = "Its network cards, in order.";
            type = lib.types.listOf (
              lib.types.submodule {
                options = {
                  network = lib.mkOption {
                    type = lib.types.str;
                    description = "The libvirt network it is on.";
                  };
                  mac = lib.mkOption {
                    type = lib.types.str;
                    description = "Its MAC address, which the machine knows the card by.";
                  };
                };
              }
            );
          };
          whenStopped = lib.mkOption {
            type = lib.types.enum [
              "restart"
              "stop"
            ];
            default = "stop";
            description = ''
              What happens when the machine stops by itself (it powered off,
              or it was destroyed): started again, for a machine the host
              cannot do without, or its unit stopped too, for a lazy machine
              that powers off when idle and is started again when needed.
            '';
          };
          cid = lib.mkOption {
            type = lib.types.nullOr lib.types.ints.unsigned;
            default = null;
            description = "A fixed vsock address, for a host that talks to it; none without.";
          };
          extraDevices = lib.mkOption {
            type = lib.types.lines;
            default = "";
            description = "More libvirt <devices>, such as a host device passed through.";
          };
        };
      }
    );
  };

  config = {
    systemd.services =
      lib.mapAttrs' (
        name: machine: lib.nameValuePair "losos-service-machine-${name}" (unit name machine)
      ) config.losos.serviceMachines
      // lib.mapAttrs' (
        name: machine: lib.nameValuePair "losos-service-machine-${name}-watch" (watch name machine)
      ) config.losos.serviceMachines;

    system.build.serviceMachines = lib.mapAttrs (
      name: machine:
      if machine.busybox != null then
        pkgs.linkFarm "losos-service-machine-${name}" {
          inherit (boot name machine) kernel initrd;
        }
      else
        (system name machine).system.build.toplevel
    ) config.losos.serviceMachines;
  };
}
