# Peripherals in machines of their own (peripherals.nix,
# docs/peripherals.md), worked hard. The test VM has two USB controllers:
# the driver's own, with its tablet, which the host keeps because a pointer
# is on it, and `pt`, which nothing of the host's needs, so it is sys-usb's.
# Devices are plugged into `pt` through QEMU's monitor, as a person would
# plug them into a port:
#
# - a stick wakes sys-usb, which takes the controller from the host, and
#   reaches the files machine over USB/IP; pulled out, sys-usb stops a
#   minute later and the controller is the host's again; three times over;
# - with sys-usb up, a stick in and out ten times, then six at once, twice;
# - a USB sound card gets a disposable machine, destroyed when it is pulled;
# - a USB keyboard comes back to the host, from sys-usb over USB/IP;
# - sys-usb killed with a stick in it comes back, and so does the stick;
# - Settings' Bluetooth page reads Bluetooth as off and starts nothing.
#
# The machines run under TCG: the test VM's KVM modules are kept out, as in
# machines.nix, rather than rely on nested virtualisation. The controller
# reaches sys-usb through the test VM's emulated IOMMU.
{ lib, hostPkgs, ... }:

let
  # A FAT stick with one file, made once at build time. The test VM's QEMU
  # opens it read-only, so every plug can share it.
  stick =
    pkgs:
    pkgs.runCommand "stick.img"
      {
        nativeBuildInputs = [
          pkgs.dosfstools
          pkgs.mtools
        ];
      }
      ''
        truncate -s 16M $out
        mkfs.vfat -n STICK $out
        echo hello from a stick > hello.txt
        mcopy -i $out hello.txt ::hello.txt
      '';
in
{
  name = "losos-desktop-peripherals";
  # sys-usb boots under emulation four times over, and the files machine
  # and a disposable machine besides.
  globalTimeout = 2 * 3600;

  node.pkgsReadOnly = false;

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [
        ../modules/network.nix
        ../modules/adblock.nix
        ../modules/libvirt.nix
        ../modules/service-machines.nix
        ../modules/busybox-guests.nix
        ../modules/busybox-host-kernel.nix
        ../modules/passthrough.nix
        ../modules/vm-call.nix
        ../modules/network-machine.nix
        ../modules/files-machine.nix
        ../modules/peripherals.nix
      ];
      # The OS's overlay without its patched GTK and Qt, which nothing here
      # draws with (service-machines.nix does the same).
      nixpkgs.overlays = [
        (
          final: prev:
          removeAttrs (import ../pkgs final prev) [
            "gtk3"
            "gtk4"
            "qt5"
            "qt6"
          ]
        )
      ];
      # The host keeps its network card: the network machine is
      # service-machines.nix's to test. The files machine is the one the
      # sticks go to.
      losos.machines.network.enable = false;
      losos.machines.files.enable = true;

      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];
      # The emulated IOMMU passthrough needs, and the controller on its own
      # in its group.
      boot.kernelParams = [
        "intel_iommu=on"
        "iommu=pt"
      ];
      virtualisation = {
        cores = 4;
        memorySize = 4096;
        qemu.options = [
          "-machine q35,kernel-irqchip=split"
          "-device intel-iommu,intremap=on,caching-mode=on"
          "-device qemu-xhci,id=pt,addr=0x0e"
          "-audiodev none,id=snd"
        ];
      };

      # The console a screenshot shows: which machines run, and where each
      # device went.
      environment.systemPackages = [
        (pkgs.writeShellScriptBin "losos-test-show" ''
          printf '\033c'
          echo
          virsh -c qemu:///system list --all
          ${pkgs.jq}/bin/jq -r '.[] | "\(.busid)  \(.name)  ->  \(.machine // "host")"' \
            /run/losos-peripherals/devices.json
          lsblk -o NAME,SIZE,TRAN
        '')
      ];
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
    };

  testScript =
    { nodes, ... }:
    let
      image = stick hostPkgs;
    in
    ''
      import json

      pt = "0000:00:0e.0"
      plugs = 0

      virsh = "virsh -c qemu:///system"

      def diagnose():
          for name in ("sys-usb", "sys-files"):
              print(machine.execute(f"tail -n 100 /var/log/libvirt/qemu/{name}-console.log")[1])
          print(machine.execute(f"{virsh} list --all; cat /run/losos-peripherals/devices.json")[1])
          print(machine.execute("journalctl -b --no-pager -n 150 -u losos-peripherals -u losos-peripherals-keep -u losos-passthrough-sys-usb -u losos-service-machine-sys-usb")[1])

      def devices():
          return json.loads(machine.succeed("cat /run/losos-peripherals/devices.json"))

      def wait(pred, what, timeout=900):
          with machine.nested(f"waiting for {what}"):
              try:
                  retry(lambda _: pred(devices()), timeout)
              except Exception:
                  diagnose()
                  raise

      def plug(driver, **props):
          global plugs
          plugs += 1
          name = f"p{plugs}"
          if driver == "usb-storage":
              machine.send_monitor_command(
                  f"drive_add 0 if=none,id=d{name},file=${image},format=raw,readonly=on"
              )
              props["drive"] = f"d{name}"
          extra = "".join(f",{k}={v}" for k, v in props.items())
          machine.send_monitor_command(f"device_add {driver},bus=pt.0,id={name}{extra}")
          return name

      def unplug(name):
          machine.send_monitor_command(f"device_del {name}")

      def routed(machine_name, count=1):
          return lambda ds: len(
              [d for d in ds if d["machine"] == machine_name and not d["pending"]]
          ) >= count

      def nothing_plugged(ds):
          return all(d["route"] == "hub" for d in ds)

      def controller_driver():
          return machine.succeed(f"basename $(readlink /sys/bus/pci/devices/{pt}/driver)").strip()

      def sys_usb_stopped():
          retry(
              lambda _: machine.execute("systemctl is-active losos-service-machine-sys-usb.service")[0] != 0,
              300,
          )
          retry(lambda _: controller_driver() == "xhci_hcd", 120)

      # What a person would see on the console: which machines run, and
      # where each device went.
      def show(name):
          machine.execute("losos-test-show > /dev/tty1 2>&1")
          machine.screenshot(name)

      machine.wait_for_unit("losos-peripherals.service")

      with subtest("pt is sys-usb's, held by the router, and nothing runs yet"):
          kept = machine.succeed("cat /run/losos-passthrough/keep")
          assert pt not in kept, kept
          assert controller_driver() == "xhci_hcd"
          machine.fail("systemctl is-active losos-service-machine-sys-usb.service")
          machine.fail("virsh -c qemu:///system list --name | grep .")

      with subtest("the machines are hidden from the Machines app"):
          machine.succeed("systemctl start losos-service-machine-sys-bluetooth.service")
          machine.succeed("virsh -c qemu:///system dumpxml sys-bluetooth | grep -q '<pm:service>true</pm:service>'")
          machine.succeed("systemctl stop losos-service-machine-sys-bluetooth.service")

      with subtest("a stick wakes sys-usb, reaches the files machine, and sys-usb sleeps again"):
          for cycle in range(3):
              s = plug("usb-storage")
              wait(routed("sys-files"), f"stick {s} in the files machine")
              if cycle == 0:
                  show("stick-in-sys-files")
              machine.succeed("systemctl is-active losos-service-machine-sys-usb.service")
              assert controller_driver() == "vfio-pci", controller_driver()
              unplug(s)
              wait(nothing_plugged, f"stick {s} to go")
              sys_usb_stopped()

      s = plug("usb-storage")
      wait(routed("sys-files"), "sys-usb up for the rest")

      with subtest("a stick in and out ten times"):
          for _ in range(10):
              n = plug("usb-storage")
              wait(routed("sys-files", 2), f"stick {n}")
              unplug(n)
              wait(lambda ds: not routed("sys-files", 2)(ds), f"stick {n} to go")

      with subtest("six sticks at once, twice"):
          for _ in range(2):
              many = [plug("usb-storage") for _ in range(6)]
              wait(routed("sys-files", 7), "six sticks")
              show("six-sticks")
              for n in many:
                  unplug(n)
              wait(lambda ds: not routed("sys-files", 2)(ds), "six sticks to go")

      with subtest("a sound card gets a disposable machine, gone when it is pulled"):
          card = plug("usb-audio", audiodev="snd")
          wait(lambda ds: any(d["route"] == "audio" and not d["pending"] for d in ds), "the sound card")
          machine.succeed("virsh -c qemu:///system list --name | grep -q '^disposable-'")
          show("sound-card-in-disposable")
          machine.succeed("virsh -c qemu:///system dumpxml $(virsh -c qemu:///system list --name | grep '^disposable-') | grep -q '<pm:service>true</pm:service>'")
          unplug(card)
          retry(lambda _: machine.execute("virsh -c qemu:///system list --name | grep -q '^disposable-'")[0] != 0, 300)

      with subtest("a keyboard in sys-usb comes back to the host"):
          kbd = plug("usb-kbd")
          retry(lambda _: machine.execute("grep -q 'QEMU USB Keyboard' /proc/bus/input/devices")[0] == 0, 600)
          unplug(kbd)
          retry(lambda _: machine.execute("grep -q 'QEMU USB Keyboard' /proc/bus/input/devices")[0] != 0, 300)

      with subtest("sys-usb killed with a stick in it comes back, and so does the stick"):
          machine.succeed("virsh -c qemu:///system destroy sys-usb")
          machine.succeed("systemctl restart losos-service-machine-sys-usb.service")
          wait(routed("sys-files"), "the stick after sys-usb came back")

      with subtest("Bluetooth's page reads it as off and starts nothing"):
          out = machine.succeed("su - alice -c 'bluetoothctl show'")
          print(out)
          machine.fail("systemctl is-active losos-service-machine-sys-bluetooth.service")

      unplug(s)
      wait(nothing_plugged, "the last stick to go")
      sys_usb_stopped()
      print(machine.succeed("journalctl -u losos-peripherals.service --no-pager | tail -200"))
    '';
}
