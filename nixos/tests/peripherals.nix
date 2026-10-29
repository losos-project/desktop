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
# - a USB sound card gets a disposable machine, Kicksecure NixOS with a
#   sound server, which alice's PipeWire plays to; destroyed when the card
#   is pulled, and alice's sink with it;
# - a USB printer on one of the host's own ports, the kernel's printer
#   gadget on dummy_hcd, gets a Kicksecure machine with CUPS, and alice
#   prints to it from the host's CUPS; pulled, its queue goes too;
# - a USB keyboard comes back to the host, from sys-usb over USB/IP;
# - sys-usb killed with a stick in it comes back, and so does the stick;
# - Settings' Bluetooth page gets BlueZ's own answer and starts nothing.
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
  # and a sound card's NixOS machine besides.
  globalTimeout = 3 * 3600;

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
          echo
          su alice -c 'XDG_RUNTIME_DIR=/run/user/1000 pactl list sinks short' 2>/dev/null
          lpstat -v 2>/dev/null
        '')
        pkgs.pulseaudio
      ];
      # Someone signed in, as far as their PipeWire goes, for a sound
      # card's machine to be played to.
      services.pipewire = {
        enable = true;
        pulse.enable = true;
      };
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        linger = true;
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
          for name in ("sys-usb", "sys-files", "disposable-*"):
              print(machine.execute(f"tail -n 100 /var/log/libvirt/qemu/{name}-console.log")[1])
          print(machine.execute(f"{virsh} list --all; cat /run/losos-peripherals/devices.json")[1])
          # Whatever libvirt is still busy with, and what it said.
          print(machine.execute("ps -eo pid,etime,args | grep -E '[v]irsh|[q]emu-system|[m]odprobe'; journalctl -b --no-pager -n 80 -u libvirtd -u virtqemud")[1])
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
          out = machine.send_monitor_command(f"device_add {driver},bus=pt.0,id={name}{extra}")
          # QEMU's monitor answers a device it cannot plug in with a line
          # saying why, and nothing at all otherwise.
          assert "rror" not in out and "arning" not in out, out
          return name

      def unplug(name):
          machine.send_monitor_command(f"device_del {name}")

      def routed(machine_name, count=1):
          return lambda ds: len(
              [d for d in ds if d["machine"] == machine_name and d["attached"]]
          ) >= count

      # Hubs and the driver's own tablet, which the host keeps, stay.
      def nothing_plugged(ds):
          return all(d["route"] in ("hub", "host") for d in ds)

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

      with subtest("a sound card gets a Kicksecure machine, which alice plays to, gone when it is pulled"):
          card = plug("usb-audio", audiodev="snd")
          wait(lambda ds: any(d["route"] == "audio" and d["attached"] for d in ds), "the sound card")
          dom = f"$({virsh} list --name | grep '^disposable-')"
          machine.succeed(f"{virsh} dumpxml {dom} | grep -q '<pm:service>true</pm:service>'")
          machine.succeed(f"{virsh} dumpxml {dom} | grep -q \"<title>A sound card's disposable machine</title>\"")
          # The router's socket for it, once its sound server has the card,
          # and the sink alice's PipeWire tunnels to it.
          pactl = "su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 pactl list sinks short'"
          try:
              retry(lambda _: machine.execute(f"{pactl} | grep -q losos-disposable-")[0] == 0, 1800)
          except Exception:
              diagnose()
              print(machine.execute(f"ls -l /run/losos-peripherals/sound; {pactl}; journalctl -b --no-pager -n 40 _UID=1000")[1])
              raise
          # And a stream plays to it.
          machine.succeed("su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 timeout 3 pacat -d $(XDG_RUNTIME_DIR=/run/user/1000 pactl list sinks short | grep -o \"losos-disposable-[0-9]*\") /dev/urandom' || [ $? = 124 ]")
          show("sound-card-in-disposable")
          unplug(card)
          retry(lambda _: machine.execute(f"{virsh} list --name | grep -q '^disposable-'")[0] != 0, 300)
          retry(lambda _: machine.execute(f"{pactl} | grep -q losos-disposable-")[0] != 0, 120)
          machine.fail("ls /run/losos-peripherals/sound/disposable-*")

      with subtest("a printer gets a Kicksecure machine with CUPS, which alice prints to"):
          # The kernel's own printer gadget on its dummy controller, a port
          # of the host's, saying it takes PostScript; what it is sent comes
          # out of /dev/g_printer0.
          machine.succeed("modprobe dummy_hcd")
          machine.succeed("modprobe g_printer pnp_string='MFG:LosOS;MDL:Test Printer;CMD:POSTSCRIPT;CLS:PRINTER;'")
          wait(lambda ds: any(d["route"] == "printer" and d["attached"] for d in ds), "the printer")
          dom = f"$({virsh} list --name | grep '^disposable-')"
          machine.succeed(f"{virsh} dumpxml {dom} | grep -q \"<title>A printer's disposable machine</title>\"")
          machine.succeed("systemd-run --unit printed sh -c 'cat /dev/g_printer0 > /tmp/printed'")
          try:
              retry(lambda _: machine.execute("lpstat -v | grep -q 'losos-peripherals%2Fprinter'")[0] == 0, 1800)
          except Exception:
              diagnose()
              print(machine.execute("ls -l /run/losos-peripherals/printer; lpstat -t; journalctl -b --no-pager -n 40 -u cups -u losos-peripherals-printers")[1])
              raise
          queue = machine.succeed("lpstat -v | sed -n 's/^device for \\([^:]*\\): .*/\\1/p'").strip()
          machine.succeed(f"su - alice -c 'lp -d {queue} /etc/os-release'")
          retry(lambda _: machine.execute("head -c 2 /tmp/printed | grep -q '%!'")[0] == 0, 900)
          show("printer-in-disposable")
          machine.succeed("rmmod g_printer")
          retry(lambda _: machine.execute(f"{virsh} list --name | grep -q '^disposable-'")[0] != 0, 300)
          retry(lambda _: machine.execute("lpstat -v | grep -q 'losos-peripherals%2Fprinter'")[0] != 0, 120)
          machine.succeed("rmmod dummy_hcd")

      with subtest("a keyboard in sys-usb comes back to the host"):
          kbd = plug("usb-kbd")
          retry(lambda _: machine.execute("grep -q 'QEMU USB Keyboard' /proc/bus/input/devices")[0] == 0, 600)
          unplug(kbd)
          retry(lambda _: machine.execute("grep -q 'QEMU USB Keyboard' /proc/bus/input/devices")[0] != 0, 300)

      with subtest("sys-usb killed with a stick in it comes back, and so does the stick"):
          machine.succeed("virsh -c qemu:///system destroy sys-usb")
          machine.succeed("systemctl restart losos-service-machine-sys-usb.service")
          wait(routed("sys-files"), "the stick after sys-usb came back")

      with subtest("Bluetooth's page gets BlueZ's answer and starts nothing"):
          # The test VM has no radio, so it answers as BlueZ does when
          # there is none, rather than starting sys-bluetooth to find out.
          status, out = machine.execute("su - alice -c 'bluetoothctl show'")
          assert status == 1 and "No default controller available" in out, out
          machine.fail("systemctl is-active losos-service-machine-sys-bluetooth.service")

      unplug(s)
      wait(nothing_plugged, "the last stick to go")
      sys_usb_stopped()
      print(machine.succeed("journalctl -u losos-peripherals.service --no-pager | tail -200"))
    '';
}
