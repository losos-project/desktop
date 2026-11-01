# PCI devices handed to service machines (service-machines.nix), the way
# Qubes hands sys-net the network cards: a machine that lists device
# classes in losos.serviceMachines.<name>.passthrough.pciClasses gets every
# such device whose IOMMU group can be fenced off, bound to vfio-pci before
# anything on the host drives it, by losos-passthrough-<name>.service
# (losos-passthrough.sh says which go). Its unit runs before the machine's,
# and before whatever host service would otherwise take the devices, which
# the module that declares the machine orders it before.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  passthrough = pkgs.writeShellApplication {
    name = "losos-passthrough";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.kmod
    ];
    text = builtins.readFile ./losos-passthrough.sh;
  };

  machines = lib.filterAttrs (
    _: machine: machine.passthrough.pciClasses != [ ]
  ) config.losos.serviceMachines;
in
{
  options.losos.serviceMachines = lib.mkOption {
    type = lib.types.attrsOf (
      lib.types.submodule {
        options.passthrough.bindAtBoot = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = ''
            Whether the devices leave the host at boot, so the host never
            drives them. Without, they stay on the host until the machine
            starts and come back when it stops (managed <hostdev>s), for a
            machine started only when it is needed.
          '';
        };
        options.passthrough.pciClasses = lib.mkOption {
          type = lib.types.listOf (lib.types.strMatching "0x[0-9a-f]{2,6}");
          default = [ ];
          example = [
            "0x0200"
            "0x0280"
          ];
          description = ''
            PCI classes, as sysfs prints them and matched as prefixes, whose
            devices the machine is given instead of the host: 0x0200 is
            Ethernet, 0x0280 other network cards such as Wi-Fi.
          '';
        };
      }
    );
  };

  config = lib.mkIf (machines != { }) {
    environment.systemPackages = [ passthrough ];

    systemd.services = lib.mapAttrs' (
      name: machine:
      lib.nameValuePair "losos-passthrough-${name}" {
        description = "Hand ${machine.title} its devices";
        # Early, with the devices udev has found but before the services
        # that would use them: the module declaring the machine adds those
        # to `before`.
        unitConfig.DefaultDependencies = false;
        after = [
          "systemd-udev-trigger.service"
          "systemd-modules-load.service"
        ];
        before = [
          "losos-service-machine-${name}.service"
          "sysinit.target"
          "shutdown.target"
        ];
        conflicts = [ "shutdown.target" ];
        wantedBy = [ "sysinit.target" ];
        requiredBy = [ "losos-service-machine-${name}.service" ];
        environment.LOSOS_PASSTHROUGH_MANAGED = if machine.passthrough.bindAtBoot then "0" else "1";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = lib.escapeShellArgs (
            [
              (lib.getExe passthrough)
              name
            ]
            ++ machine.passthrough.pciClasses
          );
        };
      }
    ) machines;
  };
}
