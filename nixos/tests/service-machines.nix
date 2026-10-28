# The network and files machines under load (network-machine.nix,
# files-machine.nix, docs/network-machine.md, docs/files-machine.md).
#
# The host here has no IOMMU, so it keeps its network card and the network
# machine reaches the LAN through it (losos-wan0), which is the topology a
# PC without one runs: the host's own connections still all go through the
# machine, its ad blocker and its firewall. Then, over and over and many at
# once: web requests and downloads, checked byte for byte; DNS queries,
# blocked and not; nmcli through losos-vm-call; schizo mode switched on,
# which with no Tor gateway here must cut the host off, and off again; and
# the machine destroyed in the middle of all that, which the host must
# start again by itself. The files machine is given a USB drive, mounts it,
# and the host mounts it for the person at the screen: files are copied in
# and out and checked, the drive ejected and given back, several at once,
# and the machine powers itself off once nothing is plugged in.
#
# Only the host's modules, not the whole image. The machines run under
# TCG: the test VM's KVM modules are kept out, as in machines.nix.
{ lib, ... }:

{
  name = "losos-desktop-service-machines";

  node.pkgsReadOnly = false;

  # The LAN's router: DHCP, DNS for a few names, and a web server with a
  # page and a file to fetch.
  nodes.lan =
    { config, pkgs, ... }:
    {
      networking.firewall.enable = false;
      services.dnsmasq = {
        enable = true;
        resolveLocalQueries = false;
        settings = {
          address = [
            "/web.lan/${config.networking.primaryIPAddress}"
            "/ads.example/${config.networking.primaryIPAddress}"
          ];
          interface = "eth1";
          dhcp-range = "192.168.1.100,192.168.1.200";
        };
      };
      systemd.services.web = {
        wantedBy = [ "multi-user.target" ];
        script = ''
          mkdir -p /srv
          echo "hello from the LAN" > /srv/index.html
          head -c 20M /dev/urandom > /srv/big
          sha256sum /srv/big | cut -d' ' -f1 > /srv/big.sha256
          exec ${pkgs.python3}/bin/python3 -m http.server --directory /srv 80
        '';
      };
    };

  nodes.host =
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
      ];
      # The OS's overlay without its patched GTK and Qt, which nothing here
      # draws with and which would only be built again for the test.
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

      networking.interfaces.eth1.ipv4.addresses = lib.mkForce [ ];
      virtualisation.qemu.networkingOptions = lib.mkForce [ ];
      losos.adblock.lists = lib.mkForce [ "/etc/losos-adblock/test-list" ];
      environment.etc."losos-adblock/test-list".text = "||ads.example^\n";

      losos.machines.files.idle = 30;

      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];

      # The person at the screen, whose drives the files machine's are.
      users.users.alice = {
        isNormalUser = true;
        extraGroups = [ "networkmanager" ];
      };
      services.getty.autologinUser = "alice";

      virtualisation = {
        cores = 4;
        memorySize = 3072;
        diskSize = 4096;
      };

      environment.systemPackages = [
        pkgs.dnsutils
        pkgs.curl
        pkgs.dosfstools
        pkgs.mtools
      ];
    };

  testScript = ''
    import time

    virsh = "virsh -c qemu:///system"

    start_all()
    lan.wait_for_unit("web.service")
    host.wait_for_unit("libvirt-defaults.service")

    with subtest("the network machine starts and the host goes out through it"):
        host.wait_for_unit("losos-service-machine-sys-net.service", timeout=600)
        # Its link to the host is up once losos-netd has named and
        # addressed it; under emulation the machine takes a while to boot.
        host.wait_until_succeeds("ping -c1 -W2 10.139.0.1", timeout=900)
        host.succeed("ip route show default | grep -q 10.139.0.1")
        host.wait_until_succeeds("curl -fsS --max-time 10 http://web.lan/ | grep -q 'hello from the LAN'", timeout=300)
        # DNS is the machine's forwarder, and it blocks.
        host.succeed("dig +short @10.139.0.1 web.lan | grep -q 192.168.1")
        host.succeed("losos-adblock update || true")
        host.wait_until_succeeds("test \"$(dig +short @10.139.0.1 ads.example)\" = 0.0.0.0", timeout=120)

    with subtest("many connections and downloads at once"):
        want = lan.succeed("cat /srv/big.sha256").strip()
        host.succeed(
            "seq 1 400 | xargs -P 32 -I{} curl -fsS --max-time 60 -o /dev/null http://web.lan/?{}"
        )
        sums = host.succeed(
            "seq 1 6 | xargs -P 6 -I{} sh -c 'curl -fsS --max-time 600 http://web.lan/big | sha256sum | cut -d\" \" -f1'"
        ).split()
        assert sums == [want] * 6, sums

    with subtest("a DNS flood through the ad blocker"):
        failed = host.succeed(
            "seq 1 2000 | xargs -P 32 -I{} sh -c 'dig +short +tries=1 +time=5 @10.139.0.1 q{}.web.lan >/dev/null || echo x' | wc -l"
        ).strip()
        assert int(failed) <= 20, f"{failed} of 2000 queries failed"
        host.succeed("dig +short @10.139.0.1 web.lan | grep -q 192.168.1")

    with subtest("nmcli through losos-vm-call, many at once"):
        out = host.succeed("losos-vm-call call nmcli -t -f STATE general")
        assert out.strip() in ("connected", "disconnected", "connecting"), out
        host.succeed("seq 1 200 | xargs -P 8 -I{} losos-vm-call call nmcli -t -f DEVICE,TYPE,STATE device >/dev/null")
        # Someone who is not allowed is refused by the relay.
        host.fail("su -s /bin/sh nobody -c 'losos-vm-call call nmcli general'")

    with subtest("schizo mode with no Tor gateway cuts the host off, and comes back"):
        for _ in range(5):
            host.succeed("touch /run/losos-net/schizo")
            # Not applied: the gateway's link is not here, and says so.
            host.fail("losos-vm-call call net-apply")
            host.wait_until_fails("curl -fsS --max-time 5 http://web.lan/", timeout=60)
            host.succeed("rm /run/losos-net/schizo")
            host.succeed("losos-vm-call call net-apply")
            host.wait_until_succeeds("curl -fsS --max-time 5 http://web.lan/", timeout=60)

    with subtest("the network machine destroyed mid-download comes back by itself"):
        host.succeed("curl -s --max-time 600 -o /tmp/cut http://web.lan/big >/dev/null 2>&1 &")
        time.sleep(2)
        host.succeed(f"{virsh} destroy sys-net")
        host.wait_until_succeeds(f"{virsh} domstate sys-net | grep -q running", timeout=60)
        host.wait_until_succeeds("curl -fsS --max-time 10 http://web.lan/", timeout=900)
        got = host.succeed("curl -fsS --max-time 600 http://web.lan/big | sha256sum | cut -d' ' -f1").strip()
        assert got == want, got

    with subtest("drives in the files machine, mounted for the person at the screen"):
        host.wait_for_unit("getty@tty1.service")
        host.wait_until_succeeds("loginctl show-seat seat0 -p ActiveSession --value | grep -q .")
        for n in (1, 2):
            host.succeed(f"truncate -s 64M /tmp/stick{n}.img && mkfs.vfat -n STICK{n} /tmp/stick{n}.img")
        host.succeed("systemctl start losos-service-machine-sys-files.service")
        for cycle in range(3):
            for n in (1, 2):
                host.succeed(
                    f"{virsh} attach-disk sys-files /tmp/stick{n}.img sd{chr(ord('a') + n)} --targetbus usb --live"
                )
            for n in (1, 2):
                host.wait_until_succeeds(f"mountpoint -q /run/media/alice/STICK{n}", timeout=600)
            # Copied in by the person, several at once, and read back.
            host.succeed(
                "for n in 1 2; do su alice -c \"head -c 8M /dev/urandom > /run/media/alice/STICK$n/f$((RANDOM))\" & done; wait"
            )
            host.succeed("su alice -c 'cd /run/media/alice && sha256sum STICK1/* STICK2/* > /tmp/sums-alice'")
            host.succeed("su alice -c 'cd /run/media/alice && sha256sum -c /tmp/sums-alice'")
            for n in (1, 2):
                host.succeed(f"su alice -c 'losos-files eject STICK{n}'")
                host.wait_until_fails(f"mountpoint -q /run/media/alice/STICK{n}", timeout=60)
                host.succeed(f"{virsh} detach-disk sys-files sd{chr(ord('a') + n)} --live")
            # On the drives themselves, as written.
            for n in (1, 2):
                host.succeed(f"rm -rf /tmp/check{n} && mkdir /tmp/check{n} && mcopy -s -n -i /tmp/stick{n}.img '::*' /tmp/check{n}/")
            host.succeed("sed -e 's|STICK1/|/tmp/check1/|' -e 's|STICK2/|/tmp/check2/|' /tmp/sums-alice | sha256sum -c")

    with subtest("the files machine powers off once nothing is plugged in"):
        host.wait_until_fails("systemctl is-active losos-service-machine-sys-files.service", timeout=300)
        host.succeed(f"{virsh} domstate sys-files | grep -q 'shut off'")
  '';
}
