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

  nodes.internet = {
    networking.firewall.enable = false;
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
      def definition(label=None, template=None, port="vnet0"):
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
              f"<devices><interface type='network'><source network='default' bridge='virbr0'/>"
              f"<target dev='{port}'/></interface></devices></domain>"
          )

      def hook(name, operation, xml):
          machine.succeed(f"echo \"{xml}\" | losos-machine-labels hook {name} {operation} begin -")

      # A stand-in machine: a namespace whose card's host end is `port`,
      # on virbr0, at 192.168.122.<n>.
      def stand_in(name, port, n, dhcp=False):
          machine.succeed(
              f"ip netns add {name}",
              f"ip link add {port} type veth peer name eth0 netns {name}",
              f"ip link set {port} master virbr0 up",
              f"ip -n {name} link set lo up",
              f"ip -n {name} link set eth0 up",
          )
          if dhcp:
              machine.succeed(f"ip netns exec {name} dhcp-once")
          machine.succeed(
              f"ip -n {name} addr add 192.168.122.{n}/24 dev eth0",
              f"ip -n {name} route add default via 192.168.122.1",
          )

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
          stand_in("red", "lab-red", 11, dhcp=True)
          for tcp in ["", "+tcp"]:
              answer = machine.succeed(f"ip netns exec red dig {tcp} +short +tries=1 +time=3 @192.168.122.1 known.test")
              assert answer.strip() == "192.168.122.50", answer
          reaches("red", "${internetAddress}")
          not_reaches("red", "${lan}")
          not_reaches("red", "192.168.122.1")
          machine.fail("ip netns exec red curl -sf --max-time 3 http://192.168.122.1:8000/")

      with subtest("a blue machine reaches the host, the LAN and the internet"):
          hook("blue", "started", definition("blue", port="lab-blue"))
          stand_in("blue", "lab-blue", 12)
          reaches("blue", "192.168.122.1")
          reaches("blue", "${lan}")
          reaches("blue", "${internetAddress}")
          machine.succeed("ip netns exec blue curl -sf --max-time 3 http://192.168.122.1:8000/")

      hook("blue2", "started", definition("blue", port="lab-blue2"))
      stand_in("blue2", "lab-blue2", 13)
      hook("green", "started", definition("green", port="lab-green"))
      stand_in("green", "lab-green", 14)
      hook("boxes", "started", definition("green", template="losos-boxes", port="lab-boxes"))
      stand_in("boxes", "lab-boxes", 15)
      stand_in("unknown", "lab-unknown", 16)

      with subtest("machines reach their own label's and the boxes machine, and no other"):
          reaches("blue", "192.168.122.13")
          reaches("blue2", "192.168.122.12")
          not_reaches("blue", "192.168.122.14")
          not_reaches("green", "192.168.122.12")
          reaches("blue", "192.168.122.15")
          reaches("green", "192.168.122.15")
          reaches("boxes", "192.168.122.12")

      with subtest("red reaches no machine, and no machine reaches red"):
          not_reaches("red", "192.168.122.12")
          not_reaches("red", "192.168.122.15")
          not_reaches("blue", "192.168.122.11")
          not_reaches("boxes", "192.168.122.11")

      with subtest("a card no hook named is red"):
          reaches("unknown", "${internetAddress}")
          not_reaches("unknown", "${lan}")
          not_reaches("unknown", "192.168.122.12")
          not_reaches("blue", "192.168.122.16")

      with subtest("a machine that stops leaves its label"):
          hook("blue2", "release", definition("blue", port="lab-blue2"))
          machine.fail("nft list set bridge losos-machine-labels blue | grep -q lab-blue2")
          not_reaches("blue", "192.168.122.13")

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
