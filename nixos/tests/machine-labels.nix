# Machines kept apart by their label on libvirt's default network
# (machine-labels.nix, src/losos-machine-labels, docs/hypervisor.md,
# "Labels on the network").
#
# Each stand-in machine is a network namespace whose card is plugged into
# virbr0 the way libvirt plugs a machine's, and the hook is given the
# definition libvirt would give it, label and all. So the test checks the
# table's rules for every label at once without booting a guest per label.
# One real machine, a guest with no disk that idles in its firmware, shows
# that libvirt runs the hook and that the hook finds its card.
#
# A red machine gets an address from the host's DHCP, answers from its DNS
# and the internet, here an address past the LAN's router, and not the
# host's other services, the LAN, the LosOS boxes machine or another
# machine. A blue one reaches the host, the LAN and the other blue one and
# the boxes machine, and not the green one. A card the hook never named is
# red. A machine that stops leaves its label's set, and a table someone
# removed is back with the next machine to start.
{ lib, ... }:

let
  # Beyond the LAN, where the internet is as far as a red machine can tell.
  internetAddress = "198.51.100.1";
in
{
  name = "losos-desktop-machine-labels";

  # Stock nixpkgs with this one package, not the OS's overlay, whose
  # patched GTK would have QEMU and its UI rebuilt for a test that needs
  # neither changed.
  node.pkgsReadOnly = false;

  nodes.internet =
    { pkgs, ... }:
    {
      networking.firewall.enable = false;
      environment.systemPackages = [ pkgs.python3 ];
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = internetAddress;
          prefixLength = 32;
        }
      ];
    };

  nodes.machine =
    { nodes, pkgs, ... }:
    {
      imports = [
        ../modules/libvirt.nix
        ../modules/machine-labels.nix
      ];
      nixpkgs.overlays = [
        (final: _: { losos-machine-labels = final.callPackage ../pkgs/losos-machine-labels.nix { }; })
      ];

      # The image's firewall is nftables (network.nix), as libvirt's rules are.
      networking.nftables.enable = true;
      # The way to the internet's address, through the LAN's router.
      networking.interfaces.eth1.ipv4.routes = [
        {
          address = internetAddress;
          prefixLength = 32;
          via = nodes.internet.networking.primaryIPAddress;
        }
      ];

      # One of the host's own services, open to the machines' bridge.
      networking.firewall.interfaces.virbr0.allowedTCPPorts = [ 8000 ];
      systemd.services.host-service = {
        wantedBy = [ "multi-user.target" ];
        script = ''
          mkdir -p /srv && echo host > /srv/index.html
          exec ${pkgs.python3}/bin/python3 -m http.server --directory /srv 8000
        '';
      };

      environment.systemPackages = [
        pkgs.dnsutils
        pkgs.curl
        pkgs.python3
        pkgs.losos-machine-labels
        # One lease on eth0, and nothing done with it.
        (pkgs.writeShellScriptBin "dhcp-once" ''
          exec ${pkgs.busybox}/bin/udhcpc -i eth0 -n -q -t 5 -s ${pkgs.coreutils}/bin/true
        '')
      ];

      # The real machine is emulated: no nested KVM is needed.
      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];
      virtualisation.memorySize = 2048;
    };

  testScript =
    { nodes, ... }:
    let
      lan = nodes.internet.networking.primaryIPAddress;
    in
    ''
      def definition(label=None, template=None, port="vnet0", mac=None):
          inside = ""
          if label:
              inside += f"<pm:label>{label}</pm:label>"
          if template:
              inside += f"<pm:template name='{template}' digest='d'/>"
          metadata = (
              "<pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>"
              + inside + "</pm:machine>"
          ) if inside else ""
          return (
              f"<domain type='qemu'><name>m</name><metadata>{metadata}</metadata>"
              f"<devices><interface type='network'><mac address='{mac or mac_for(label or port)}'/>"
              f"<source network='default' bridge='virbr0'/>"
              f"<target dev='{port}'/></interface></devices></domain>"
          )

      macs = {
          "red": "52:54:00:00:00:01",
          "blue": "52:54:00:00:00:02",
          "blue2": "52:54:00:00:00:03",
          "green": "52:54:00:00:00:04",
          "boxes": "52:54:00:00:00:05",
          "unknown": "52:54:00:00:00:06",
          "untouched": "52:54:00:00:00:07",
      }

      def mac_for(name):
          return macs.get(name, "52:54:00:00:00:ff")

      def hook(name, operation, xml):
          machine.succeed(f"echo \"{xml}\" | losos-machine-labels hook {name} {operation} begin -")

      # A stand-in machine: a namespace whose card's host end is `port`,
      # on virbr0, using the lease dnsmasq gives its MAC.
      def stand_in(name, port, mac):
          machine.succeed(
              f"ip netns add {name}",
              f"ip link add {port} type veth peer name eth0 netns {name}",
              f"ip link set {port} master virbr0 up",
              f"ip -n {name} link set lo up",
              f"ip -n {name} link set eth0 address {mac}",
              f"ip -n {name} link set eth0 up",
              f"ip netns exec {name} dhcp-once",
          )
          machine.wait_until_succeeds(
              f"test -n \"$(awk -v mac='{mac}' 'tolower($2) == tolower(mac) {{print $3}}' "
              "/var/lib/libvirt/dnsmasq/default.leases)\"",
              timeout=30,
          )
          address = machine.succeed(
              f"awk -v mac='{mac}' 'tolower($2) == tolower(mac) {{print $3}}' "
              "/var/lib/libvirt/dnsmasq/default.leases"
          ).strip()
          machine.wait_until_succeeds(
              f"nft list map bridge losos-machine-labels arp4 | grep -q '\"{port}\" : {address}'",
              timeout=30,
          )
          machine.succeed(
              f"ip -n {name} addr add {address}/24 dev eth0",
              f"ip -n {name} route add default via 192.168.122.1",
          )
          return address

      def reaches(name, address):
          machine.succeed(f"ip netns exec {name} ping -c1 -W3 {address}")

      def not_reaches(name, address):
          machine.fail(f"ip netns exec {name} ping -c1 -W3 {address}")

      start_all()
      internet.wait_for_unit("network.target")
      machine.wait_for_unit("losos-machine-labels.service")
      machine.wait_for_unit("libvirt-defaults.service")
      machine.wait_for_unit("host-service.service")
      machine.wait_for_open_port(8000)
      machine.succeed("nft list table bridge losos-machine-labels")
      # A name the host's DNS for machines answers itself, so the answer
      # does not depend on a resolver past it.
      machine.succeed(
          "virsh net-update default add dns-host"
          " \"<host ip='192.168.122.50'><hostname>known.test</hostname></host>\" --live"
      )

      with subtest("a red machine gets DHCP, DNS and the internet, and nothing else"):
          hook("red", "started", definition("red", port="lab-red"))
          red_ip = stand_in("red", "lab-red", macs["red"])
          for tcp in ["", "+tcp"]:
              answer = machine.succeed(f"ip netns exec red dig {tcp} +short +tries=1 +time=3 @192.168.122.1 known.test")
              assert answer.strip() == "192.168.122.50", answer
          reaches("red", "${internetAddress}")
          not_reaches("red", "${lan}")
          not_reaches("red", "192.168.122.1")
          machine.fail("ip netns exec red curl -sf --max-time 3 http://192.168.122.1:8000/")
          internet.succeed(
              "python3 -c 'import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); "
              f"s.bind((\"${lan}\",67)); s.settimeout(3); open(\"/tmp/udp67.ready\",\"w\").close(); "
              "s.recvfrom(1024); open(\"/tmp/udp67.received\",\"w\").close()' >/dev/null 2>&1 &"
          )
          internet.wait_until_succeeds("test -e /tmp/udp67.ready")
          machine.succeed(
              f"ip netns exec red python3 -c 'import socket; "
              f"socket.socket(socket.AF_INET,socket.SOCK_DGRAM).sendto(b\"probe\",(\"${lan}\",67))'"
          )
          internet.succeed("sleep 1")
          internet.fail("test -e /tmp/udp67.received")

      with subtest("a blue machine reaches the host, the LAN and the internet"):
          hook("blue", "started", definition("blue", port="lab-blue"))
          blue_ip = stand_in("blue", "lab-blue", macs["blue"])
          reaches("blue", "192.168.122.1")
          reaches("blue", "${lan}")
          reaches("blue", "${internetAddress}")
          machine.succeed("ip netns exec blue curl -sf --max-time 3 http://192.168.122.1:8000/")

      hook("blue2", "started", definition("blue", port="lab-blue2", mac=macs["blue2"]))
      blue2_ip = stand_in("blue2", "lab-blue2", macs["blue2"])
      hook("green", "started", definition("green", port="lab-green"))
      green_ip = stand_in("green", "lab-green", macs["green"])
      hook(
          "boxes",
          "started",
          definition("green", template="losos-boxes", port="lab-boxes", mac=macs["boxes"]),
      )
      boxes_ip = stand_in("boxes", "lab-boxes", macs["boxes"])
      hook("unknown", "prepare", definition(port="lab-unknown", mac=macs["unknown"]))
      unknown_ip = stand_in("unknown", "lab-unknown", macs["unknown"])

      with subtest("machines reach their own label's and the boxes machine, and no other"):
          reaches("blue", blue2_ip)
          reaches("blue2", blue_ip)
          not_reaches("blue", green_ip)
          not_reaches("green", blue_ip)
          reaches("blue", boxes_ip)
          reaches("green", boxes_ip)
          reaches("boxes", blue_ip)

      with subtest("red reaches no machine, and no machine reaches red"):
          not_reaches("red", blue_ip)
          not_reaches("red", boxes_ip)
          not_reaches("blue", red_ip)
          not_reaches("boxes", red_ip)

      with subtest("a red guest cannot claim another guest's ARP address"):
          blue_mac = macs["blue"]
          forged = (
              "import socket,struct; "
              "s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW); s.bind(('eth0',0)); "
              f"src=bytes.fromhex('{macs['red'].replace(':', ''')}'); "
              f"sha=bytes.fromhex('{macs['red'].replace(':', ''')}'); "
              f"spa=socket.inet_aton('{blue_ip}'); "
              "tha=b'\\x00'*6; tpa=socket.inet_aton('192.168.122.1'); "
              "s.send(b'\\xff'*6+src+b'\\x08\\x06'+"
              "struct.pack('!HHBBH',1,0x0800,6,4,1)+sha+spa+tha+tpa)"
          )
          machine.succeed(f"ip netns exec red python3 -c \"{forged}\"")
          machine.wait_until_succeeds(
              f"ip neigh show to {blue_ip} dev virbr0 | grep -i {blue_mac}",
              timeout=5,
          )

      with subtest("a red guest cannot learn another guest's MAC with IP traffic"):
          forged = (
              "import socket,struct; "
              "s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW); s.bind(('eth0',0)); "
              f"src=bytes.fromhex('{macs['blue'].replace(':', ''')}'); "
              "dst=b'\\xff'*6; "
              "ip=struct.pack('!BBHHHBBH4s4s',0x45,0,28,0,0,64,17,0,"
              "socket.inet_aton('192.168.122.20'),socket.inet_aton('203.0.113.1')); "
              "udp=struct.pack('!HHHH',12345,9,8,0); "
              "s.send(dst+src+b'\\x08\\x00'+ip+udp)"
          )
          machine.succeed(f"ip netns exec red python3 -c \"{forged}\"")
          machine.wait_until_succeeds(
              f"bridge fdb get {blue_mac} br virbr0 | grep -q 'dev lab-blue'",
              timeout=5,
          )

      with subtest("a red guest cannot poison the IPv6 neighbor cache"):
          target = "2001:db8::2"
          machine.succeed(
              f"ip -6 neigh replace {target} lladdr {macs['blue']} nud reachable dev virbr0"
          )
          forged = (
              "import socket,struct\n"
              "src=socket.inet_pton(socket.AF_INET6,'fe80::5054:ff:fe00:1')\n"
              f"target=socket.inet_pton(socket.AF_INET6,'{target}')\n"
              "dst=socket.inet_pton(socket.AF_INET6,'ff02::1')\n"
              "payload=struct.pack('!BBHI',136,0,0,0x20000000)+target+"
              "b'\\x02\\x01'+bytes.fromhex('"
              f"{macs['red'].replace(':', ''')}"
              "')\n"
              "pseudo=src+dst+struct.pack('!I3xB',len(payload),58)\n"
              "data=pseudo+payload\n"
              "if len(data)%2: data+=b'\\x00'\n"
              "words=struct.unpack('!%dH'%(len(data)//2),data)\n"
              "total=sum(words)\n"
              "while total>>16: total=(total&0xffff)+(total>>16)\n"
              "checksum=(~total)&0xffff\n"
              "icmp=payload[:2]+struct.pack('!H',checksum)+payload[4:]\n"
              "ip=struct.pack('!IHBB',6<<28,len(icmp),58,255)+src+dst\n"
              "s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW); s.bind(('eth0',0))\n"
              "s.send(bytes.fromhex('333300000001"
              f"{macs['red'].replace(':', ''')}"
              "86dd')+ip+icmp)"
          )
          machine.succeed(f"ip netns exec red python3 -c \"{forged}\"")
          machine.succeed("sleep 1")
          machine.succeed(f"ip -6 neigh show to {target} dev virbr0 | grep -qi {macs['blue']}")

      with subtest("a red guest cannot poison IPv6 neighbors with a solicitation"):
          blue_ip6 = "2001:db8::3"
          host_ip6 = "2001:db8::1"
          machine.succeed(
              f"ip -n blue -6 addr add {blue_ip6}/64 dev eth0 nodad",
              f"ip -6 addr add {host_ip6}/64 dev virbr0 nodad",
              f"ip -6 neigh flush to {blue_ip6} dev virbr0",
          )
          forged = (
              "import socket,struct\n"
              f"src=socket.inet_pton(socket.AF_INET6,'{blue_ip6}')\n"
              f"target=socket.inet_pton(socket.AF_INET6,'{host_ip6}')\n"
              "dst=socket.inet_pton(socket.AF_INET6,'ff02::1:ff00:1')\n"
              "payload=struct.pack('!BBHI',135,0,0,0)+target+b'\\x01\\x01'+bytes.fromhex('"
              f"{macs['red'].replace(':', ''')}"
              "')\n"
              "pseudo=src+dst+struct.pack('!I3xB',len(payload),58)\n"
              "data=pseudo+payload\n"
              "if len(data)%2: data+=b'\\x00'\n"
              "words=struct.unpack('!%dH'%(len(data)//2),data)\n"
              "total=sum(words)\n"
              "while total>>16: total=(total&0xffff)+(total>>16)\n"
              "checksum=(~total)&0xffff\n"
              "icmp=payload[:2]+struct.pack('!H',checksum)+payload[4:]\n"
              "ip=struct.pack('!IHBB',6<<28,len(icmp),58,255)+src+dst\n"
              "s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW); s.bind(('eth0',0))\n"
              "s.send(bytes.fromhex('3333ff000001"
              f"{macs['red'].replace(':', ''')}"
              "86dd')+ip+icmp)"
          )
          machine.succeed(f"ip netns exec red python3 -c \"{forged}\"")
          machine.succeed("sleep 1")
          machine.fail(f"ip -6 neigh show to {blue_ip6} dev virbr0 | grep -q .")

      with subtest("trusted peer traffic routed through the gateway stays isolated"):
          machine.succeed(f"ip -n blue route add {green_ip}/32 via 192.168.122.1")
          not_reaches("blue", green_ip)
          machine.succeed(f"ip -n blue route add {red_ip}/32 via 192.168.122.1")
          not_reaches("blue", red_ip)
          machine.succeed(f"ip -n blue route del {green_ip}/32")
          machine.succeed(f"ip -n blue route del {red_ip}/32")

      with subtest("a card no hook named is red"):
          reaches("unknown", "${internetAddress}")
          not_reaches("unknown", "${lan}")
          not_reaches("unknown", blue_ip)
          not_reaches("blue", unknown_ip)

      with subtest("a port without a hook binding stays blocked"):
          machine.succeed(
              "ip netns add untouched",
              "ip link add lab-untouched type veth peer name eth0 netns untouched",
              "ip link set lab-untouched master virbr0 up",
              "ip -n untouched link set lo up",
              f"ip -n untouched link set eth0 address {macs['untouched']}",
              "ip -n untouched link set eth0 up",
          )
          machine.fail("ip netns exec untouched dhcp-once")
          machine.fail(
              f"awk -v mac='{macs['untouched']}' 'tolower($2) == tolower(mac)' "
              "/var/lib/libvirt/dnsmasq/default.leases | grep -q ."
          )

      with subtest("a machine that stops leaves its label"):
          hook("blue2", "release", definition("blue", port="lab-blue2"))
          machine.fail("nft list set bridge losos-machine-labels blue | grep -q lab-blue2")
          not_reaches("blue", blue2_ip)

      with subtest("libvirt runs the hook for a real machine"):
          machine.succeed(
              "cat > /tmp/idle.xml <<'EOF'\n"
              "<domain type='qemu'><name>idle</name><memory unit='MiB'>64</memory><vcpu>1</vcpu>"
              "<metadata><pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>"
              "<pm:label>purple</pm:label></pm:machine></metadata>"
              "<os><type arch='x86_64'>hvm</type></os>"
              "<devices><interface type='network'><source network='default'/><model type='virtio'/></interface>"
              "<graphics type='vnc' autoport='yes' listen='127.0.0.1'/></devices></domain>\n"
              "EOF",
              "virsh define /tmp/idle.xml",
              "virsh start idle",
          )
          port = machine.succeed("virsh domiflist idle | awk '/default/ {print $1}'").strip()
          machine.wait_until_succeeds(f"nft list set bridge losos-machine-labels purple | grep -q '\"{port}\"'", timeout=60)
          machine.succeed(f"nft list set bridge losos-machine-labels trusted | grep -q '\"{port}\"'")
          # Reloading puts the running machines back, and nothing else.
          machine.succeed("systemctl reload losos-machine-labels")
          machine.succeed(f"nft list set bridge losos-machine-labels purple | grep -q '\"{port}\"'")
          machine.fail("nft list set bridge losos-machine-labels blue | grep -q lab-blue")
          machine.succeed("virsh destroy idle")
          machine.wait_until_fails(f"nft list set bridge losos-machine-labels purple | grep -q '\"{port}\"'", timeout=60)

      with subtest("a removed table is back with the next machine"):
          machine.succeed("nft delete table bridge losos-machine-labels")
          machine.succeed("virsh start idle")
          machine.wait_until_succeeds("nft list set bridge losos-machine-labels purple | grep -q vnet", timeout=60)
          machine.succeed("virsh destroy idle")
    '';
}
