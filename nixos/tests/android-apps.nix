# Android app support inside the image itself (docs/android-apps.md): the
# image boots as firmware would boot it, a person signs in to the derisk
# desktop, Android app support sets their Pixel up and boots it in a
# virtual machine of the image's own, and an Android app opens as a window
# of the desktop's, beside the Android app support window. Screenshots of
# each go in the test's output.
#
# The setup downloads the emulator and Android from Google, which may not
# be handed on, so this test needs the internet and runs outside the Nix
# sandbox: its driver is built and run by hand (CI's android-desktop job).
# It is not in `checks`, whose VM tests run in the sandbox with none. The
# Pixel runs on the image's KVM, so the machine running the test has to
# pass virtualisation on to the image's VM (nested KVM).
{ self }:

{ lib, ... }:

let
  image = import ./image.nix { inherit self; };
in
{
  name = "losos-desktop-android-apps";

  node.pkgsReadOnly = false;

  nodes.machine = {
    imports = [ image.node ];

    # The Pixel's own 4 GB and the desktop beside it, and four cores, as
    # the emulator asks for.
    virtualisation = {
      memorySize = 10240;
      cores = 4;
    };
  };

  testScript =
    { nodes, ... }:
    # The emulator, Android and the Pixel's data partition take 20 GB of
    # the person's home (android-app-support.sh, cmd_setup).
    image.diskOfSize "80G" nodes.machine
    + ''
      import json
      import shlex

      uid = 60001
      runtime = f"/run/user/{uid}"

      # As alice, in her session's user manager. The image has no su
      # (accounts.nix), so runuser, as boot.nix uses.
      def alice(command, **kwargs):
          return machine.succeed(
              f"runuser -u alice -- env XDG_RUNTIME_DIR={runtime}"
              f" DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus sh -c "
              + shlex.quote(command),
              **kwargs,
          )

      def status():
          return json.loads(alice("android-app-support status"))

      # A key that types nothing, so the desktop never sees the person go
      # idle: the setup takes longer than derisk waits before dimming and
      # locking the screen, and the screenshots would show the lock screen.
      def nudge():
          machine.send_key("shift")

      def wait(seconds):
          for _ in range(seconds // 5):
              nudge()
              machine.sleep(5)

      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("display-manager.service")

      with subtest("the image's VM can run the Pixel's"):
          machine.succeed("test -c /dev/kvm")
          print(machine.succeed("grep -m1 -o -w -E 'vmx|svm' /proc/cpuinfo"))
          print(machine.succeed("cat /sys/module/kvm_*/parameters/nested 2>/dev/null || true"))

      with subtest("a person signs in to the desktop"):
          # Directory storage, so the Pixel's 20 GB need no LUKS volume of
          # that size made first; the storage is boot.nix's to test. Wheel,
          # because first-boot setup makes its account an administrator
          # (docs/first-boot.md) and derisk manages machines only for one:
          # without it the session's libvirt connection sits behind a polkit
          # password prompt nobody in the test answers, and neither the
          # machines nor, with them, sys-net's DNS is reached.
          machine.succeed(
              f"NEWPASSWORD=correct-horse homectl create alice --uid={uid} --storage=directory"
              " --member-of=wheel --enforce-password-policy=no"
          )
          # The first login after setup, as boot.nix has it.
          machine.succeed("umask 077; printf 'alice\\0correct-horse' > /run/derisk-first-login")
          machine.succeed("systemctl restart display-manager.service")
          machine.wait_until_succeeds("loginctl list-sessions --no-legend | grep -q alice", timeout=300)
          machine.wait_until_succeeds("pgrep -u alice -f 'derisk session'", timeout=120)
          machine.wait_until_succeeds(f"test -S {runtime}/derisk/agent.sock", timeout=120)
          machine.sleep(10)
          # Escape ends the tour derisk opens at a first login, which would
          # sit over every window in the screenshots.
          machine.send_key("esc")
          machine.sleep(2)
          machine.screenshot("desktop")

      with subtest("the network reaches Google"):
          # sys-net answers DNS at 10.139.0.1 as the forwarder; anywhere
          # it does not, this carries the bridge state and sys-net's own
          # console (netd's log) into the output to say why.
          print(machine.execute(
              "ip -br link; ip -br addr;"
              " ip route show table 254; ip route show table 100;"
              " ip rule; ping -c1 -W2 10.139.0.1 || true;"
              " nft list table bridge losos-machine-labels || true;"
              " virsh domstate sys-net 2>&1;"
              " timeout 5 virsh domstats sys-net --cpu 2>&1 | grep cpu_time"
              " || echo 'domstats hung: the machine monitor is not answering';"
              " timeout 5 virsh qemu-monitor-command sys-net --hmp 'info status' 2>&1"
              " || echo 'the monitor command hung too';"
              " ps -C qemu-system-x86_64 -o pid,pcpu,etime,args 2>&1 | grep sys-net | cut -c1-80;"
              " journalctl -k --no-pager 2>&1 | grep -aE 'Unhandled|kvm:|svm:' | tail -n 8;"
              " timeout 5 losos-vm-call call nmcli -t -f GENERAL.DEVICE device show 2>&1"
              " || echo 'the vsock call to sys-net answered nothing';"
              " cat /var/log/libvirt/qemu/sys-net.log 2>&1;"
              " virsh dumpxml sys-net 2>&1 | sed -n '/<os>/,/<\\/os>/p';"
              " ls /var/lib/losos-service-machines/sys-net/ 2>&1;"
              " journalctl --no-pager -n 60 -u libvirtd -u systemd-machined"
              " -u 'qemu-1-sys-net' -u losos-service-machine-sys-net-start"
              " -u losos-service-machine-sys-net-graph 2>&1 | tail -n 60",
          )[1])
          machine.wait_until_succeeds(
              "curl -fsS -o /dev/null https://dl.google.com/android/repository/repository2-3.xml",
              timeout=300,
          )

      with subtest("Android app support sets up and boots the Pixel"):
          print(alice("android-app-support status"))
          # The Android app support window, which starts the setup for a
          # person with no Pixel yet.
          alice("derisk launch org.derisk.android --action apps")
          machine.sleep(10)
          machine.screenshot("android-app-support-new")
          if status()["state"] == "new":
              alice("systemctl --user start android-app-support-setup.service")
          last = None
          for tick in range(720):
              now = status()
              if (now["state"], now["detail"]) != last:
                  print(f"[{tick * 5:4d}s] {now['state']}: {now['detail']}")
                  last = (now["state"], now["detail"])
              if now["state"] == "running":
                  break
              if now["state"] in ("failed", "unsupported"):
                  # The emulator's reasons are only in the units' log.
                  print(machine.execute(
                      "journalctl --no-pager -n 300 _UID=60001 | grep -v __unw_add_dynamic_fde"
                  )[1])
                  raise Exception(f"the setup stopped: {now['detail']}")
              # The window as the setup goes, once every five minutes.
              if tick % 60 == 30:
                  machine.screenshot(f"android-app-support-setup-{tick // 60}")
              nudge()
              machine.sleep(5)
          else:
              raise Exception("the Pixel was not running after an hour")
          machine.screenshot("android-app-support-running")
          # What Android draws with, and that it is Google's Android 17.
          print(alice("android-app-support gpu"))

      with subtest("an Android app opens as a window of the desktop's"):
          alice(
              "curl -fsSL --retry 5 -o /tmp/fdroid.apk https://f-droid.org/repo/org.fdroid.fdroid_1023052.apk"
              " || curl -fsSL --retry 5 -o /tmp/fdroid.apk https://f-droid.org/archive/org.fdroid.fdroid_1023052.apk"
          )
          alice(
              "echo '985f5181d48bb6bafd54083a048b391271e0ab28385881cc41294fb01a222762  /tmp/fdroid.apk'"
              " | sha256sum -c -"
          )
          alice("android-app-support install /tmp/fdroid.apk")
          alice("test -e /home/alice/.local/share/applications/android-org.fdroid.fdroid.desktop")
          # As the app grid opens it: by its desktop entry.
          alice("derisk launch android-org.fdroid.fdroid.desktop")
          machine.wait_until_succeeds("pgrep -u alice -f 'scrcpy.*org.fdroid.fdroid'", timeout=300)
          wait(60)
          machine.screenshot("f-droid-window")
          # The app's frames, drawn on its own display of the Pixel's.
          print(alice("android-app-support gpu org.fdroid.fdroid"))
    '';
}
