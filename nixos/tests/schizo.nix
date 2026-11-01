# Schizo mode (schizo.nix, docs/schizo-mode.md) on the image's own network
# (network.nix, adblock.nix, libvirt.nix): with it off the host reaches a
# web server directly; turned on, the kill switch is up before anything
# else, the Tor gateway boots from the host's store, and the host's
# connections, its names and a libvirt machine's connections all reach the
# server through Tor, so the server sees a Tor exit and never the host.
# With the gateway down nothing leaves at all; the mode survives a restart
# with the kill switch up before NetworkManager; turned off, everything is
# as before.
#
# The internet here has no Tor network, so it runs one of its own: a
# directory authority and two relays, every one an exit, on Tor's testing
# network settings, the way chutney builds one. The gateway's Tor is told
# of that authority by an %include the test adds to its configuration;
# the rest of the gateway is the image's.
#
# The gateway runs under TCG: the test VM's KVM modules are kept out, as in
# machines.nix.
{ lib, hostPkgs, ... }:

let
  # The first node's address on the test's network, which is the internet's.
  internetIp = "192.168.1.1";
  # The web server, past the router rather than on the host's own LAN: in
  # schizo mode the LAN is out of reach, and Tor's exits cannot reach it
  # either.
  webIp = "10.200.0.2";
  authOrPort = 5000;
  dirPort = 7000;

  # The test network's authority keys, made once at build time, and the
  # line every Tor here is told about it with.
  network =
    hostPkgs.runCommand "tor-test-network"
      {
        nativeBuildInputs = [
          hostPkgs.tor
          hostPkgs.gawk
        ];
      }
      ''
        export HOME=$TMPDIR
        mkdir -p $out/auth/keys
        cd $out/auth/keys
        echo "" | tor-gencert --create-identity-key -m 12 -a ${internetIp}:${toString dirPort} --passphrase-fd 0
        v3=$(awk '/^fingerprint/ { print $2 }' authority_certificate)
        cd $out
        tor --list-fingerprint --DataDirectory $out/auth --ORPort ${toString authOrPort} \
          --Nickname auth --Log 'notice stderr' >/dev/null
        relay=$(awk '{ print $2 }' $out/auth/fingerprint)
        cat > $out/network.conf <<EOF
        TestingTorNetwork 1
        DirAuthority auth orport=${toString authOrPort} no-v2 v3ident=$v3 ${internetIp}:${toString dirPort} $relay
        EOF
      '';

  relay = name: port: authority: ''
    %include ${network}/network.conf
    DataDirectory /var/lib/tor-test/${name}
    Nickname ${name}
    Address ${internetIp}
    ORPort ${internetIp}:${toString port}
    SocksPort 0
    ExitRelay 1
    ExitPolicy accept *:*
    ContactInfo test
    AssumeReachable 1
    Log notice stdout
    ${lib.optionalString authority ''
      DirPort ${internetIp}:${toString dirPort}
      AuthoritativeDirectory 1
      V3AuthoritativeDirectory 1
      TestingDirAuthVoteExit *
      TestingDirAuthVoteGuard *
      TestingDirAuthVoteHSDir *
      TestingMinExitFlagThreshold 0
      TestingMinFastFlagThreshold 0
      TestingV3AuthInitialVotingInterval 20
      TestingV3AuthInitialVoteDelay 4
      TestingV3AuthInitialDistDelay 4
      V3AuthVotingInterval 20
      V3AuthVoteDelay 4
      V3AuthDistDelay 4
    ''}
  '';
  relays = {
    auth = {
      port = authOrPort;
      authority = true;
    };
    relay1 = {
      port = 5001;
      authority = false;
    };
    relay2 = {
      port = 5002;
      authority = false;
    };
  };
in
{
  name = "losos-desktop-schizo";

  node.pkgsReadOnly = false;

  # The internet: the router whose DHCP hands the host its address and
  # resolver, that resolver, which knows web.test, a web server past it
  # that answers with the address that asked, and the Tor network, whose
  # exits reach that server from its own address.
  nodes.internet =
    { pkgs, ... }:
    {
      networking.firewall.enable = false;
      # The exits resolve through the same dnsmasq.
      networking.nameservers = [ "127.0.0.1" ];
      services.dnsmasq = {
        enable = true;
        resolveLocalQueries = false;
        settings = {
          address = "/web.test/${webIp}";
          interface = [
            "eth1"
            "lo"
          ];
          dhcp-range = "192.168.1.100,192.168.1.200";
        };
      };
      services.nginx = {
        enable = true;
        virtualHosts.web = {
          default = true;
          locations."/".return = "200 '$remote_addr\\n'";
        };
      };

      users.users.tor-test = {
        isSystemUser = true;
        group = "tor-test";
      };
      users.groups.tor-test = { };
      systemd.services =
        lib.mapAttrs' (
          name: r:
          lib.nameValuePair "tor-${name}" {
            wantedBy = [ "multi-user.target" ];
            after = [ "network-online.target" ];
            wants = [ "network-online.target" ];
            serviceConfig = {
              User = "tor-test";
              StateDirectory = "tor-test/${name}";
              StateDirectoryMode = "0700";
              ExecStartPre = lib.optional r.authority "+${pkgs.writeShellScript "tor-auth-keys" ''
                cp -rT ${network}/auth /var/lib/tor-test/${name}
                chown -R tor-test:tor-test /var/lib/tor-test/${name}
                chmod -R u+w,go-rwx /var/lib/tor-test/${name}
              ''}";
              ExecStart = "${lib.getExe pkgs.tor} -f ${
                pkgs.writeText "${name}.torrc" (relay name r.port r.authority)
              }";
            };
          }
        ) relays
        // {
          # The web server's address, on lo. Not networking.interfaces.lo,
          # which the test framework's own interfaces override.
          web-address = {
            wantedBy = [ "multi-user.target" ];
            before = [
              "nginx.service"
              "dnsmasq.service"
            ];
            serviceConfig = {
              Type = "oneshot";
              RemainAfterExit = true;
            };
            path = [ pkgs.iproute2 ];
            script = "ip addr replace ${webIp}/32 dev lo";
          };
        };
    };

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [
        ../modules/network.nix
        ../modules/adblock.nix
        ../modules/libvirt.nix
        ../modules/service-machines.nix
        ../modules/schizo.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      # The LAN's address and resolver from its DHCP, and no user-mode NIC
      # beside it (machines.nix has the same).
      networking.interfaces.eth1.ipv4.addresses = lib.mkForce [ ];
      virtualisation.qemu.networkingOptions = lib.mkForce [ ];
      losos.adblock.lists = lib.mkForce [ "/etc/losos-adblock/test-list" ];
      environment.etc."losos-adblock/test-list".text = "||ads.example^\n";

      # The gateway's Tor knows the test network's authority instead of
      # the real ones.
      losos.serviceMachines.losos-tor-gateway.module = {
        services.tor.settings."%include" = "${network}/network.conf";
      };

      boot.blacklistedKernelModules = [
        "kvm_intel"
        "kvm_amd"
      ];
      virtualisation = {
        cores = 2;
        memorySize = 2048;
      };

      environment.systemPackages = [
        pkgs.curl
        pkgs.ethtool
      ];
    };

  testScript = ''
    import re

    virsh = "virsh -c qemu:///system"

    def via(command):
        """The address the web server saw `command`'s request come from."""
        return machine.succeed(command).strip()

    start_all()
    internet.wait_for_unit("tor-auth.service")
    machine.wait_for_unit("libvirt-defaults.service")
    machine.wait_until_succeeds("nmcli -t -f IP4.DNS device show eth1 | grep -q 192.168.1.", timeout=120)
    permanent = machine.succeed("ethtool -P eth1 | awk '{print $3}'").strip()

    def diagnose():
        print(machine.execute("ip -4 addr; ip -4 route; ip -4 rule; resolvectl query web.test; curl -sv --max-time 10 http://${webIp}/ 2>&1")[1])

    with subtest("off, the host goes to the web server itself"):
        try:
            machine.wait_until_succeeds("curl -sf --max-time 10 http://web.test/", timeout=180)
        except Exception:
            diagnose()
            raise
        seen = via("curl -sf --max-time 10 http://web.test/")
        # Its own address from the router's DHCP.
        assert re.fullmatch(r"192\.168\.1\.1\d\d", seen), seen
        machine.succeed("losos-schizo | grep -qx off")
        machine.fail("nft list table inet losos-schizo")

    with subtest("on, nothing leaves but towards the gateway"):
        machine.succeed("losos-schizo on")
        machine.succeed("losos-schizo | grep -qx on")
        machine.succeed("nft list table inet losos-schizo")
        # Straight out of the network card, which a program can ask for
        # whatever the routes say: dropped at once by the kill switch.
        machine.fail("curl -sf --max-time 5 --interface eth1 http://${webIp}/")
        machine.fail("ping -c1 -W2 -I eth1 ${webIp}")
        # NetworkManager came back with a random address on the card.
        machine.wait_until_succeeds("nmcli -t -f GENERAL.STATE device show eth1 | grep -q '(connected)'", timeout=120)
        current = machine.succeed("cat /sys/class/net/eth1/address").strip()
        assert current != permanent, f"eth1 kept its own address {permanent}"
        # The gateway is no machine of anyone's: the Machines app skips it.
        machine.succeed(
            f"{virsh} metadata losos-tor-gateway https://losos-project.github.io/xmlns/pm-machine/1"
            " | grep -q '<.*service>true</'"
        )

    with subtest("the host reaches the web server through Tor"):
        machine.wait_until_succeeds(f"{virsh} domstate losos-tor-gateway | grep -qx running")
        # The web server sees the exit, which is the internet's own Tor.
        try:
            machine.wait_until_succeeds(
                "curl -sf --max-time 60 http://${webIp}/ | grep -qx ${webIp}", timeout=2400
            )
        except Exception:
            diagnose()
            print(machine.execute(f"{virsh} dominfo losos-tor-gateway; nft list ruleset; journalctl -b -u 'losos-*' --no-pager | tail -n 80; tail -n 80 /var/log/libvirt/qemu/losos-tor-gateway-console.log")[1])
            raise

    with subtest("names resolve through Tor, through the ad blocker"):
        machine.succeed("grep -qx 'nameserver 10.152.152.10' /run/losos-adblock/upstreams")
        machine.wait_until_succeeds("curl -sf --max-time 60 http://web.test/ | grep -qx ${webIp}", timeout=600)
        machine.succeed("getent ahostsv4 ads.example | grep -q '^0.0.0.0'")
        machine.succeed("losos-schizo status | grep -qx tor=connected")

    with subtest("a machine on libvirt's network goes through Tor too"):
        # A network namespace on virbr0 stands in for a machine.
        machine.succeed(
            "ip netns add guest",
            "ip link add vnet-test type veth peer name eth0 netns guest",
            "ip link set vnet-test master virbr0 up",
            "ip -n guest addr add 192.168.122.50/24 dev eth0",
            "ip -n guest link set eth0 up",
            "ip -n guest route add default via 192.168.122.1",
        )
        machine.wait_until_succeeds(
            "ip netns exec guest curl -sf --max-time 60 http://${webIp}/ | grep -qx ${webIp}", timeout=600
        )

    with subtest("with the gateway down, nothing leaves at all"):
        machine.succeed(f"{virsh} destroy losos-tor-gateway")
        machine.fail("curl -sf --max-time 10 http://${webIp}/")
        machine.fail("ip netns exec guest curl -sf --max-time 10 http://${webIp}/")
        machine.fail("curl -sf --max-time 10 --interface eth1 http://${webIp}/")
        machine.succeed("losos-schizo status | grep -qx gateway=shut.off")

    with subtest("it stays on across a restart, the kill switch before the network"):
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("losos-schizo.target")
        switch = int(machine.succeed(
            "systemctl show -P ActiveEnterTimestampMonotonic losos-schizo-kill-switch.service"
        ))
        network = int(machine.succeed(
            "systemctl show -P ExecMainStartTimestampMonotonic NetworkManager.service"
        ))
        assert 0 < switch < network, f"kill switch at {switch}, NetworkManager at {network}"
        machine.wait_until_succeeds(f"{virsh} domstate losos-tor-gateway | grep -qx running")
        machine.fail("curl -sf --max-time 5 --interface eth1 http://${webIp}/")

    with subtest("off, everything is as it was"):
        machine.succeed("losos-schizo off")
        machine.fail("nft list table inet losos-schizo")
        machine.fail("test -e /run/losos-adblock/upstreams")
        machine.wait_until_succeeds(f"{virsh} domstate losos-tor-gateway | grep -qx 'shut off'", timeout=120)
        machine.wait_until_succeeds("curl -sf --max-time 10 http://web.test/ | grep -q '^192.168.1.1'", timeout=180)
        assert machine.succeed("cat /sys/class/net/eth1/address").strip() == permanent
  '';
}
