# The portal machine (portal-machine.nix, docs/portal-machine.md), from
# request to file. The host's person asks for a file through the host's
# backend; then an app in another machine asks, gets a copy of the file
# picked for it, and saves it, which must change the file on the host; and
# the portal machine stops once nothing has asked for a while.
#
# Nobody is at the screen to click, so in the portal machine a stand-in
# takes GTK's backend's name before GTK's own could be started, and picks
# a fixed file each time, writing the title it was shown into the
# person's home, which the machine shares with the host. Everything else
# is what a PC runs: the backend, the relay, the machine started on the
# first request with the person's home over virtiofs, and the copy and
# write-back for the app machine.
#
# The machines run under TCG: the test VM's KVM modules are kept out, as
# in machines.nix.
{ lib, ... }:

{
  name = "losos-desktop-portal-machine";

  node.pkgsReadOnly = false;

  nodes.host =
    { pkgs, ... }:
    let
      # GTK's file chooser, as far as a request can tell, picking for the
      # person: report.txt to open, saved.txt to save.
      standIn =
        pkgs.writers.writePython3 "losos-portal-stand-in"
          {
            libraries = [
              pkgs.python3Packages.dbus-python
              pkgs.python3Packages.pygobject3
            ];
            flakeIgnore = [ "E501" ];
          }
          ''
            import os
            import dbus
            import dbus.service
            from dbus.mainloop.glib import DBusGMainLoop
            from gi.repository import GLib

            DBusGMainLoop(set_as_default=True)
            home = os.environ["HOME"]
            bus = dbus.SessionBus()
            name = dbus.service.BusName("org.freedesktop.impl.portal.desktop.gtk", bus)


            def seen(title):
                with open(os.path.join(home, ".portal-test-titles"), "a") as f:
                    f.write(title + "\n")


            class Chooser(dbus.service.Object):
                @dbus.service.method("org.freedesktop.impl.portal.FileChooser", in_signature="osssa{sv}", out_signature="ua{sv}")
                def OpenFile(self, handle, app_id, parent, title, options):
                    seen(title)
                    return (0, {"uris": dbus.Array(["file://" + home + "/Documents/report.txt"], signature="s")})

                @dbus.service.method("org.freedesktop.impl.portal.FileChooser", in_signature="osssa{sv}", out_signature="ua{sv}")
                def SaveFile(self, handle, app_id, parent, title, options):
                    seen(title)
                    return (0, {"uris": dbus.Array(["file://" + home + "/Documents/saved.txt"], signature="s")})


            Chooser(bus, "/org/freedesktop/portal/desktop")
            GLib.MainLoop().run()
          '';
    in
    {
      imports = [
        ../modules/libvirt.nix
        ../modules/service-machines.nix
        ../modules/busybox-guests.nix
        ../modules/portal-backend.nix
        ../modules/portal-machine.nix
      ];
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

      losos.machines.portal = {
        idle = 60;
        # Booted under emulation inside a VM.
        boot = 900;
      };
      losos.serviceMachines.sys-portal.module = {
        systemd.user.services.losos-portal-stand-in = {
          wantedBy = [ "default.target" ];
          before = [ "losos-portal-serve.service" ];
          serviceConfig = {
            Type = "dbus";
            BusName = "org.freedesktop.impl.portal.desktop.gtk";
            ExecStart = standIn;
          };
        };
      };

      # An app's machine: it asks for a file to open, reads its copy, and
      # saves it with a new line.
      losos.serviceMachines.app = {
        title = "An app's machine";
        cid = 4190;
        memory = 512;
        module =
          { pkgs, ... }:
          {
            imports = [ ../modules/portal-backend.nix ];
            losos.portalBackend = {
              enable = true;
              inMachine = true;
            };
            boot.kernelModules = [ "vmw_vsock_virtio_transport" ];
            services.dbus.enable = true;
            # sysusers makes only system accounts; this one is the app's.
            users.users.user = {
              isSystemUser = true;
              group = "users";
              uid = 1000;
              home = "/home/user";
              createHome = true;
              linger = true;
            };
            systemd.services.portal-test = {
              wantedBy = [ "multi-user.target" ];
              after = [ "user@1000.service" ];
              requires = [ "user@1000.service" ];
              path = [
                pkgs.systemd
                pkgs.coreutils
                pkgs.gnused
                pkgs.gnugrep
              ];
              environment = {
                DBUS_SESSION_BUS_ADDRESS = "unix:path=/run/user/1000/bus";
                XDG_RUNTIME_DIR = "/run/user/1000";
              };
              serviceConfig = {
                Type = "oneshot";
                User = "user";
                StandardOutput = "journal+console";
              };
              script = ''
                for _ in $(seq 60); do
                  [ -S /run/user/1000/bus ] && break
                  sleep 2
                done
                reply=$(busctl --user --timeout=1200 call org.freedesktop.impl.portal.desktop.losos \
                  /org/freedesktop/portal/desktop org.freedesktop.impl.portal.FileChooser \
                  OpenFile osssa{sv} /org/test/1 org.example.App "" Open 0)
                echo "PORTAL-TEST reply: $reply"
                path=$(echo "$reply" | grep -o '"file://[^"]*"' | head -n1 | tr -d '"' | sed 's|^file://||')
                echo "PORTAL-TEST content: $(cat "$path")"
                echo "saved in the machine" >> "$path"
                sleep 10
                echo "PORTAL-TEST done"
              '';
            };
          };
      };

      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
      services.getty.autologinUser = "alice";

      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];
      virtualisation = {
        cores = 4;
        memorySize = 3072;
        diskSize = 4096;
      };
    };

  testScript = ''
    virsh = "virsh -c qemu:///system"

    def diagnose():
        for name in ("sys-portal", "app"):
            print(host.execute(f"tail -n 150 /var/log/libvirt/qemu/{name}-console.log")[1])
        print(host.execute("journalctl -b --no-pager -n 120 -u losos-portal-relay -u losos-service-machine-sys-portal -u libvirtd")[1])

    import contextlib

    @contextlib.contextmanager
    def step(name):
        with subtest(name):
            try:
                yield
            except Exception:
                diagnose()
                raise

    host.wait_for_unit("losos-portal-relay.service")
    host.wait_until_succeeds("loginctl show-seat seat0 -p ActiveSession --value | grep -q .")
    host.succeed("su alice -c 'mkdir -p ~/Documents && echo hello from the host > ~/Documents/report.txt'")
    as_alice = "su alice -c 'XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "

    with step("nothing runs it until something asks"):
        host.fail(f"{virsh} domstate sys-portal 2>/dev/null | grep -q running")

    with step("a host app's request starts the portal machine and is answered there"):
        host.wait_for_unit("user@1000.service")
        out = host.succeed(
            as_alice + "busctl --user --timeout=1200 call org.freedesktop.impl.portal.desktop.losos "
            + "/org/freedesktop/portal/desktop org.freedesktop.impl.portal.FileChooser "
            + "OpenFile osssa{sv} /org/test/1 org.example.App \"\" Open 0'",
            timeout=1300,
        )
        assert "file:///home/alice/Documents/report.txt" in out, out
        host.succeed(f"{virsh} domstate sys-portal | grep -q running")
        # The person picked it on the host: the title is the app's own.
        host.succeed("grep -qx Open /home/alice/.portal-test-titles")

    with step("an app in a machine gets a copy, and its save reaches the host"):
        host.succeed("systemctl start losos-service-machine-app.service")
        host.wait_until_succeeds("grep -q 'PORTAL-TEST content: hello from the host' /var/log/libvirt/qemu/app-console.log", timeout=1500)
        host.wait_until_succeeds("grep -q 'saved in the machine' /home/alice/Documents/report.txt", timeout=120)
        # The dialog said which machine asked.
        host.succeed("grep -qx 'app: Open' /home/alice/.portal-test-titles")
        host.wait_until_succeeds("grep -q 'PORTAL-TEST done' /var/log/libvirt/qemu/app-console.log", timeout=120)

    with step("the portal machine stops once nothing asks"):
        host.succeed("systemctl stop losos-service-machine-app.service")
        host.wait_until_fails(f"{virsh} domstate sys-portal | grep -q running", timeout=300)
  '';
}
