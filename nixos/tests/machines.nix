# The virtual machine desktop's host and its template (libvirt.nix,
# machines.nix, docs/hypervisor.md), on the image's own network
# (network.nix, adblock.nix): the host takes its address and resolver from
# the LAN's DHCP and resolves names through the ad blocker, which blocks
# what its list names. pm makes a machine from the template
# the image ships, the machine boots and takes an address on libvirt's
# network, its user pings the host and past it and NetworkManager finds it
# online, its guest agent lists its apps, and opening one sends its window
# to the host over vsock from the address libvirt gave that machine, which
# is how the desktop tells whose window it is. The machine looks .local
# names up through the host, so it finds a box on the host's network that
# its own multicast could never reach past libvirt's NAT. The LosOS boxes
# machine, made at boot and started once the box answers, finds that box
# through the host's resolved and offers it as an app, its keyring opens
# with the password the host keeps for it, and the first machine's
# connections to the box go through it. Then
# a Windows machine, as
# far as libvirt goes without Windows: defined with the image's drivers CD
# and started with its TPM.
#
# The desktop's own half, the gate that hands that connection to the
# machine's waypipe client and the compositor that labels the window, is
# derisk's and is tested there; here a listener on the same port stands in
# for it and records who connected.
#
# Only the host's modules, not the whole image. The machine runs under TCG:
# the test VM's KVM modules are kept out, so pm defines it to emulate rather
# than rely on nested virtualisation the runner may not give.
{ lib, ... }:

{
  name = "losos-desktop-machines";

  # The modules' packages, the guest template's among them, come from the
  # OS's overlay: the same derivations the image builds.
  node.pkgsReadOnly = false;

  # A LosOS box on the host's LAN, as far as finding one goes: it answers
  # for its name and announces `_losos._tcp` over mDNS, with Avahi as boxes
  # before systemd-resolved did, and serves its admin pages and the
  # /setup/state.json every box has (nixos/tests/losos-boxes.nix has the
  # same stand-in).
  nodes.lanbox =
    { config, ... }:
    {
      services.avahi = {
        enable = true;
        openFirewall = true;
        publish = {
          enable = true;
          addresses = true;
        };
        extraServiceFiles.losos = ''
          <?xml version="1.0" standalone='no'?>
          <!DOCTYPE service-group SYSTEM "avahi-service.dtd">
          <service-group>
            <name replace-wildcards="yes">%h</name>
            <service>
              <type>_losos._tcp</type>
              <port>80</port>
              <txt-record>version=v0.4.0</txt-record>
            </service>
          </service-group>
        '';
      };
      services.nginx = {
        enable = true;
        virtualHosts.front = {
          default = true;
          locations."/".return = "200 'admin pages'";
        };
      };
      networking.firewall.allowedTCPPorts = [ 80 ];
    };

  # The internet as far as a machine can tell: the router whose DHCP hands
  # the host its address and resolver, that resolver, which places GNOME's
  # connectivity check here, and that check's page.
  nodes.internet =
    { config, pkgs, ... }:
    {
      networking.firewall.enable = false;
      services.dnsmasq = {
        enable = true;
        resolveLocalQueries = false;
        settings = {
          address = "/nmcheck.gnome.org/${config.networking.primaryIPAddress}";
          interface = "eth1";
          # Above the addresses the test gives the other nodes.
          dhcp-range = "192.168.1.100,192.168.1.200";
        };
      };
      systemd.services.nmcheck = {
        wantedBy = [ "multi-user.target" ];
        script = ''
          mkdir -p /srv
          echo "NetworkManager is online" > /srv/check_network_status.txt
          exec ${pkgs.python3}/bin/python3 -m http.server --directory /srv 80
        '';
      };
    };

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [
        # The image's own network: NetworkManager with resolved, the
        # firewall, and the ad blocker's forwarder every name goes through.
        ../modules/network.nix
        ../modules/adblock.nix
        ../modules/libvirt.nix
        ../modules/machines.nix
        ../modules/pm.nix
        # The LosOS boxes machine is the only one that talks to boxes: the
        # host has neither losos nor losos-boxes' service of its own.
        ../modules/losos-boxes.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      # The LAN's address and resolver come from its DHCP, as on a real
      # network, rather than the address the test driver would set, and
      # the driver's own user-mode NIC is left out so the router is the
      # only network there is. libvirt's dnsmasq passes the machines'
      # queries to the host's resolved through /etc/resolv.conf, as on the
      # image, and resolved sends them on through the ad blocker.
      networking.interfaces.eth1.ipv4.addresses = lib.mkForce [ ];
      virtualisation.qemu.networkingOptions = lib.mkForce [ ];
      # One name the ad blocker blocks; the daily lists are out of reach.
      losos.adblock.lists = lib.mkForce [ "/etc/losos-adblock/test-list" ];
      environment.etc."losos-adblock/test-list".text = "||ads.example^\n";

      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];

      virtualisation = {
        cores = 4;
        # The two machines it runs at once, and itself.
        memorySize = 6144;
        # The template is uploaded into libvirt's pool and the machine's
        # disk grows on top of it.
        diskSize = 12 * 1024;
      };

      # What the desktop's gate does first: accept on vsock port 7350 and
      # learn the peer's address, which the kernel stamps on the connection.
      environment.etc."vsock-listen.py".text = ''
        import socket, sys
        listener = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
        listener.bind((socket.VMADDR_CID_ANY, 7350))
        listener.listen()
        connection, (cid, port) = listener.accept()
        with open(sys.argv[1], "w") as peer:
            peer.write(str(cid))
      '';

      environment.systemPackages = [ pkgs.python3 ];

      # The host makes the LosOS boxes machine at boot and starts it once a
      # box answers, and the LAN's box answers from the start. Started by
      # hand here instead, so it does not boot beside `work` under TCG.
      systemd.services.losos-boxes-wake.wantedBy = lib.mkForce [ ];
    };

  testScript = ''
    import base64
    import json
    import re
    import time

    virsh = "virsh -c qemu:///system"
    guest_run = "/run/current-system/sw/bin/losos-guest-run"

    def agent(command, domain="work"):
        reply = machine.succeed(f"{virsh} qemu-agent-command {domain} '{json.dumps(command)}'")
        return json.loads(reply)["return"]

    def in_guest(path, *args, domain="work"):
        """Runs a program in the machine through its agent, as the desktop does."""
        pid = agent({
            "execute": "guest-exec",
            "arguments": {"path": path, "arg": list(args), "capture-output": True},
        }, domain)["pid"]
        for _ in range(120):
            status = agent({"execute": "guest-exec-status", "arguments": {"pid": pid}}, domain)
            if status["exited"]:
                out = base64.b64decode(status.get("out-data", "")).decode()
                err = base64.b64decode(status.get("err-data", "")).decode()
                assert status["exitcode"] == 0, f"{path} {args} failed: {err}"
                return out
            time.sleep(1)
        raise Exception(f"{path} {args} did not finish")

    start_all()
    machine.wait_for_unit("libvirt-defaults.service")

    with subtest("the host resolves names through the ad blocker, which blocks"):
        internet.wait_for_unit("dnsmasq.service")
        machine.wait_until_succeeds("nmcli -t -f IP4.DNS device show eth1 | grep -q 192.168.1.", timeout=120)
        # The networks' servers route nothing by themselves: the dummy link
        # is resolved's one way out, and it is the forwarder.
        status = machine.succeed("resolvectl status")
        print(status)
        adblock = [link for link in status.split("\nLink ") if "(adblock0)" in link]
        assert adblock and "DNS Servers: 169.254.153.53" in adblock[0], status
        internet_ip = internet.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()
        machine.wait_until_succeeds(f"getent hosts nmcheck.gnome.org | grep -q {internet_ip}", timeout=60)
        machine.succeed("ping -c1 -W10 nmcheck.gnome.org")
        machine.succeed("systemctl start losos-adblock-update.service")
        machine.wait_until_succeeds("getent ahostsv4 ads.example | grep -q '^0.0.0.0'", timeout=60)
        # NetworkManager leaves libvirt's bridge to libvirt.
        machine.succeed("nmcli -t -f DEVICE,STATE device | grep -qx 'virbr0:unmanaged'")
        # network.nix turns mDNS on for every link NetworkManager hands resolved.
        machine.succeed("resolvectl mdns eth1 | grep -q ': yes'")

    with subtest("libvirt is the host's control plane"):
        machine.succeed("systemctl is-active libvirtd.service")
        machine.succeed(f"{virsh} pool-info default | grep -E '^State: +running'")
        machine.succeed(f"{virsh} pool-info default | grep -E '^Autostart: +yes'")
        machine.succeed(f"{virsh} net-info default | grep -E '^Active: +yes'")
        machine.succeed(f"{virsh} net-info default | grep -E '^Autostart: +yes'")
        machine.succeed("bash -lc 'test \"$LIBVIRT_DEFAULT_URI\" = qemu:///system'")
        machine.succeed("test -c /dev/vhost-vsock")
        machine.succeed("test -f /run/current-system/sw/share/losos/machines/losos-guest.qcow2")
        machine.fail("test -e /dev/kvm")

    with subtest("pm makes a machine from the shipped template"):
        machine.succeed(
            "mkdir -p /root/.config/pm/plugins",
            "cp /run/current-system/sw/share/pm/plugins/libvirt.wasm /root/.config/pm/plugins/",
            "pm sign /root/.config/pm/plugins/libvirt.wasm",
        )
        machine.succeed(
            "LIBVIRT_DEFAULT_URI=qemu:///system pm machine create work"
            " --image /run/current-system/sw/share/losos/machines/losos-guest.qcow2"
            " --label blue --memory 1536 --cpus 2 --disk 8"
        )
        metadata = machine.succeed(
            f"{virsh} metadata work https://losos-project.github.io/xmlns/pm-machine/1"
        )
        assert re.search(r"<\w*:?label>blue</", metadata), metadata
        # The template as a volume of its own, which every machine made from
        # it shares, and the machine's disk over it.
        machine.succeed(f"{virsh} vol-list default | grep -q 'pm-template-losos-guest-'")
        machine.succeed(f"{virsh} vol-info --pool default pm-machine-work.qcow2")

    with subtest("the machine boots, grows its disk and takes an address"):
        machine.succeed("LIBVIRT_DEFAULT_URI=qemu:///system pm machine start work")
        machine.wait_until_succeeds(
            f"{virsh} qemu-agent-command work '{{\"execute\":\"guest-ping\"}}'", timeout=900
        )
        size = int(in_guest("/run/current-system/sw/bin/df", "--output=size", "-B1G", "/").split()[-1])
        assert size >= 7, f"the root is {size} GiB, so it did not grow into the 8 GiB disk"
        machine.wait_until_succeeds(f"{virsh} domifaddr work | grep -q 192.168.122.", timeout=300)

    with subtest("the machine pings the host and past it, and is online"):
        # As the machine's user, from the login shell its apps get, the way
        # someone types it into the machine's Terminal.
        as_user = [
            "/run/current-system/sw/bin/systemd-run", "--quiet", "--user",
            "--machine=user@.host", "--wait", "--pipe", "--",
            "/run/current-system/sw/bin/bash", "-lc",
        ]
        in_guest(*as_user, "ping -c1 -W10 192.168.122.1")
        in_guest(*as_user, "ping -c1 -W10 nmcheck.gnome.org")
        # NetworkManager's connectivity check, which apps in the machine ask
        # before they go online.
        for _ in range(60):
            if in_guest("/run/current-system/sw/bin/nmcli", "-t", "-f", "CONNECTIVITY", "general").strip() == "full":
                break
            time.sleep(5)
        else:
            raise Exception("NetworkManager never found the machine online")

    with subtest("the machine looks .local names up through the host"):
        lanbox.wait_for_unit("avahi-daemon.service")
        address = lanbox.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()
        machine.wait_until_succeeds(f"resolvectl query lanbox.local | grep -q {address}", timeout=120)
        status = in_guest("/run/current-system/sw/bin/resolvectl", "status")
        print(status)
        assert "-mDNS" in status and "+mDNS" not in status, "the machine multicasts itself"
        for _ in range(30):
            try:
                found = in_guest("/run/current-system/sw/bin/getent", "hosts", "lanbox.local")
                break
            except Exception:
                time.sleep(2)
        else:
            raise Exception("the machine cannot look up lanbox.local")
        assert found.split()[0] == address, f"lanbox.local is {found}, not {address}"
        # The address the host gave is one the machine reaches through NAT.
        in_guest("/run/current-system/sw/bin/ping", "-c", "1", "-W", "5", "lanbox.local")

    with subtest("its user has the folders apps expect"):
        # home-folders.nix, made by the user's tmpfiles when its manager
        # started, which lingering does at boot.
        for folder in [".config/derisk", ".local/share/fonts", ".icons", ".local/state", "Documents"]:
            in_guest("/run/current-system/sw/bin/test", "-d", f"/home/user/{folder}")
        dirs = in_guest("/run/current-system/sw/bin/cat", "/home/user/.config/user-dirs.dirs")
        assert 'XDG_PICTURES_DIR="$HOME/Pictures"' in dirs, dirs

    with subtest("its agent lists the apps it can open"):
        apps = {}
        for line in in_guest(guest_run, "--list").splitlines():
            app, _, name = line.partition("\t")
            apps[app] = name
        for app in ["org.derisk.terminal", "org.losos.Danube", "org.derisk.calculator"]:
            assert app in apps, f"{app} is not in {sorted(apps)}"

    with subtest("an app's window reaches the host from the machine's own address"):
        xml = machine.succeed(f"{virsh} dumpxml work")
        found = re.search(r"<cid auto=.yes. address=.(\d+).", xml)
        assert found, f"the running machine has no vsock address: {xml}"
        cid = found.group(1)
        machine.succeed("systemd-run --unit=vsock-listen python3 /etc/vsock-listen.py /tmp/peer")
        machine.wait_until_succeeds("ss -l --vsock | grep -q 7350")
        in_guest(guest_run, "org.derisk.terminal")
        machine.wait_until_succeeds("test -s /tmp/peer", timeout=120)
        peer = machine.succeed("cat /tmp/peer").strip()
        assert peer == cid, f"the connection came from {peer}, the machine is {cid}"

    with subtest("a LosOS boxes machine finds the box through the host and offers it"):
        machine.fail("command -v losos")
        machine.wait_for_unit("losos-boxes-relay.service")
        machine.wait_for_unit("losos-boxes-dns.service")
        machine.succeed(f"{virsh} net-dumpxml default | grep -q hostsdir=/run/losos-boxes/hosts")
        # Made at boot, as the system makes its machines, and not started.
        machine.wait_for_unit("losos-system-machines.service")
        assert machine.succeed(f"{virsh} domstate boxes").strip() == "shut off"
        # Started by the first box that answers, which lanbox does.
        machine.succeed("systemctl start losos-boxes-wake.service")
        machine.wait_until_succeeds(f"{virsh} domstate boxes | grep -qx running", timeout=120)
        machine.wait_until_succeeds(
            "systemctl show -P ActiveState losos-boxes-wake.service | grep -qx inactive", timeout=60
        )
        machine.succeed("journalctl -u losos-boxes-wake --no-pager | grep -q 'a box is here'")
        machine.wait_until_succeeds(
            f"{virsh} qemu-agent-command boxes '{{\"execute\":\"guest-ping\"}}'", timeout=900
        )
        machine.wait_until_succeeds(f"{virsh} domifaddr boxes | grep -q 192.168.122.", timeout=300)
        found = re.search(r"(192\.168\.122\.\d+)", machine.succeed(f"{virsh} domifaddr boxes"))
        assert found, "the LosOS boxes machine has no address"
        boxes_address = found.group(1)
        for _ in range(90):
            apps = in_guest(guest_run, "--list", domain="boxes")
            if "losos-box-lanbox.local\tlanbox" in apps:
                break
            time.sleep(2)
        else:
            print(machine.succeed("journalctl -u losos-boxes-relay --no-pager"))
            raise Exception(f"the LosOS boxes machine offers no lanbox: {apps}")
        print(apps)
        entry = in_guest(
            "/run/current-system/sw/bin/cat",
            "/home/user/.local/share/applications/losos-box-lanbox.local.desktop",
            domain="boxes",
        )
        print(entry)
        assert "\nExec=derisk-terminal --app-id=org.losos.Boxes --title=lanbox losos-boxes terminal http://lanbox.local\n" in entry
        assert "\nComment=LosOS v0.4.0 box at lanbox.local\n" in entry
        # The machine's own resolved does no mDNS: what it found came
        # through the host's.
        status = in_guest("/run/current-system/sw/bin/resolvectl", "status", domain="boxes")
        assert "+mDNS" not in status, status

    with subtest("the LosOS boxes machine's keyring opens with the password the host keeps"):
        uuid = machine.succeed(f"{virsh} domuuid boxes").strip()
        machine.wait_until_succeeds(f"test -s /var/lib/losos-boxes/keyrings/{uuid}", timeout=300)
        machine.succeed(f"stat -c %U:%a /var/lib/losos-boxes/keyrings/{uuid} | grep -qx losos-boxes:600")
        # The login keyring was made with it, with nobody asked.
        for _ in range(60):
            made = in_guest(
                "/run/current-system/sw/bin/sh", "-c",
                "test -s /home/user/.local/share/keyrings/login.keyring && echo made || true",
                domain="boxes",
            )
            if "made" in made:
                break
            time.sleep(2)
        else:
            raise Exception("the LosOS boxes machine made no login keyring")

    with subtest("the other machines reach the box through the LosOS boxes machine"):
        machine.wait_until_succeeds(
            f"grep -qx '{boxes_address} lanbox.local' /run/losos-boxes/hosts/boxes", timeout=120
        )
        for _ in range(30):
            found = in_guest("/run/current-system/sw/bin/getent", "hosts", "lanbox.local")
            if found.split()[0] == boxes_address:
                break
            time.sleep(2)
        else:
            raise Exception(f"lanbox.local is {found} for the machine, not {boxes_address}")
        request = (
            "exec 3<>/dev/tcp/{host}/80;"
            " printf 'GET / HTTP/1.1\\r\\nHost: {name}\\r\\nConnection: close\\r\\n\\r\\n' >&3;"
            " cat <&3"
        )
        answer = in_guest(
            "/run/current-system/sw/bin/bash", "-c",
            request.format(host="lanbox.local", name="lanbox.local"),
        )
        assert answer.rstrip().endswith("admin pages"), answer
        # The box saw the connection come from the host's LAN address, which
        # is where the LosOS boxes machine's traffic leaves libvirt's NAT.
        lanbox.succeed("grep -q 'GET / ' /var/log/nginx/access.log")
        # A name that is not a box gets nothing passed on.
        refused = in_guest(
            "/run/current-system/sw/bin/bash", "-c",
            request.format(host=boxes_address, name="printer.local") + " || true",
        )
        assert "admin pages" not in refused, refused

    with subtest("a stopped LosOS boxes machine routes nothing"):
        machine.succeed("LIBVIRT_DEFAULT_URI=qemu:///system pm machine remove boxes")
        machine.wait_until_fails("test -e /run/losos-boxes/hosts/boxes", timeout=60)

    with subtest("pm removes it and keeps the template"):
        machine.succeed("LIBVIRT_DEFAULT_URI=qemu:///system pm machine remove work")
        machine.fail(f"{virsh} dominfo work")
        machine.fail(f"{virsh} vol-info --pool default pm-machine-work.qcow2")
        machine.succeed(f"{virsh} vol-list default | grep -q 'pm-template-losos-guest-'")

    with subtest("the image carries the CDs of guest tools where pm looks"):
        tools = "/run/current-system/sw/share/pm/machines"
        machine.succeed(f"test -s {tools}/windows-tools.iso")
        machine.succeed(f"test -s {tools}/linux-tools.iso")
        # pm finds them through the session's XDG_DATA_DIRS, so a login
        # shell's environment, not the test driver's.
        machine.succeed("bash -lc 'echo \"$XDG_DATA_DIRS\" | grep -q /run/current-system/sw/share'")

    with subtest("a Windows machine gets the drivers CD, a TPM and a screen, and starts"):
        # Windows itself cannot be installed here, so its CD is a stand-in:
        # what is checked is that libvirt takes the machine pm defines for it
        # and starts it, swtpm and all, with the host's drivers CD beside it.
        machine.succeed("head -c 1048576 /dev/zero > /tmp/win11.iso")
        machine.succeed(
            "bash -lc 'pm machine create win --installer /tmp/win11.iso"
            " --system windows --label red --memory 512 --disk 4'"
        )
        xml = machine.succeed(f"{virsh} dumpxml --inactive win")
        for expected in [
            "<tpm model='tpm-crb'>",
            "<graphics type='spice'>",
            "<clock offset='localtime'>",
            "pm-media-win11-",
            "pm-media-windows-tools-",
        ]:
            assert expected in xml, f"{expected} is not in {xml}"
        assert "<vsock" not in xml, xml
        machine.succeed("LIBVIRT_DEFAULT_URI=qemu:///system pm machine start win")
        machine.wait_until_succeeds(f"{virsh} domstate win | grep -qx running", timeout=60)
        machine.succeed("pgrep -x swtpm")

    with subtest("removing it keeps the CDs another machine may share"):
        machine.succeed("LIBVIRT_DEFAULT_URI=qemu:///system pm machine remove win")
        machine.fail(f"{virsh} dominfo win")
        machine.fail(f"{virsh} vol-info --pool default pm-machine-win.qcow2")
        machine.succeed(f"{virsh} vol-list default | grep -q 'pm-media-windows-tools-'")
        machine.succeed(f"{virsh} vol-list default | grep -q 'pm-media-win11-'")
  '';
}
