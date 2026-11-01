# Encrypted DNS (adblock.nix, src/losos-adblock/src/encrypted.rs and
# portal.rs, docs/adblock.md, "Encrypted DNS"). The network's router
# answers DNS in plain text and also serves DNS over HTTPS and over TLS,
# and each of the three gives its own answer for the same name, so the
# address a lookup gets says which way it went. A counter on the host's
# uplink counts every DNS packet that leaves in plain text, and the
# router's log names every query it got in plain text.
#
# Encrypted DNS is on from the start: names come over HTTPS and no plain
# query leaves, except for a name only the local network has, and the ad
# blocker still blocks. Servers of the person's own over TLS replace the
# OS's. With the encrypted servers unreachable, a lookup fails and nothing
# leaves in plain text, strict or not, as long as the network is open;
# behind a captive portal, and not strict, names come from the network's
# server so the sign-in page loads. While /run/losos-adblock/upstreams-forced
# is there, only the servers /run/losos-adblock/upstreams names are asked
# (schizo mode's Tor gateway). Turned
# off through its unit, the network answers.
{ lib, pkgs, ... }:

let
  # A CA the host trusts, and the HTTPS server's certificate from it, for
  # the name the server is asked under.
  certs = pkgs.runCommand "encrypted-dns-test-certs" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
      -keyout $out/ca.key -out $out/ca.pem -days 3650 -subj /CN=test-ca
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
      -keyout $out/doh.key -out doh.csr -subj /CN=doh.test
    printf 'subjectAltName=DNS:doh.test\n' > ext
    openssl x509 -req -in doh.csr -CA $out/ca.pem -CAkey $out/ca.key -CAcreateserial \
      -out $out/doh.pem -days 3650 -extfile ext
    chmod 644 $out/*.key
  '';
in
{
  name = "losos-desktop-encrypted-dns";

  node.pkgsReadOnly = false;

  nodes.router =
    { config, pkgs, ... }:
    {
      networking.firewall.enable = false;
      # The network's own DNS and DHCP, in plain text, and the address of
      # the connectivity check, which this router serves.
      services.dnsmasq = {
        enable = true;
        resolveLocalQueries = false;
        settings = {
          interface = "eth1";
          dhcp-range = "192.168.1.100,192.168.1.200";
          address = [
            "/printer.lan/192.168.1.50"
            "/nmcheck.gnome.org/${config.networking.primaryIPAddress}"
            "/test/192.168.1.77"
          ];
          log-queries = true;
        };
      };
      # DNS over HTTPS on 443 and DNS over TLS on 853, as public resolvers
      # serve them, each with answers of its own.
      services.coredns = {
        enable = true;
        config = ''
          https://.:443 {
            tls ${certs}/doh.pem ${certs}/doh.key
            template IN A test {
              answer "{{ .Name }} 60 IN A 10.0.0.42"
            }
          }
          tls://.:853 {
            tls ${certs}/doh.pem ${certs}/doh.key
            template IN A test {
              answer "{{ .Name }} 60 IN A 10.0.0.53"
            }
          }
        '';
      };
      # GNOME's connectivity check, answered as GNOME does or as a portal
      # would, by what /srv holds.
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
    { nodes, ... }:
    {
      imports = [
        ../modules/network.nix
        ../modules/adblock.nix
      ];
      nixpkgs.overlays = [ (import ../pkgs) ];

      # The address and resolver come from the router's DHCP, and the
      # router is the only network there is.
      networking.interfaces.eth1.ipv4.addresses = lib.mkForce [ ];
      virtualisation.qemu.networkingOptions = lib.mkForce [ ];

      losos.adblock.lists = lib.mkForce [ "/etc/losos-adblock/test-list" ];
      environment.etc."losos-adblock/test-list".text = "||ads.test^\n";

      # The router's HTTPS server, by its name and the address to reach it
      # at, which is never looked up.
      losos.adblock.encryptedDns.servers = lib.mkForce [
        "https://doh.test/dns-query ${nodes.router.networking.primaryIPAddress}"
      ];
      security.pki.certificateFiles = [ "${certs}/ca.pem" ];

      # Every DNS packet that leaves for the network in plain text.
      networking.nftables.tables.plain-dns = {
        family = "inet";
        content = ''
          counter plain { }
          chain output {
            type filter hook output priority 0;
            oifname "eth1" udp dport 53 counter name plain
            oifname "eth1" tcp dport 53 counter name plain
          }
        '';
      };
    };

  testScript =
    { nodes, ... }:
    let
      router = nodes.router.networking.primaryIPAddress;
    in
    ''
      import re

      def plain():
          out = machine.succeed("nft list counter inet plain-dns plain")
          found = re.search(r"packets (\d+)", out)
          assert found, out
          return int(found.group(1))

      def lookup(name):
          machine.succeed("resolvectl flush-caches")
          return machine.succeed(f"getent ahostsv4 {name} | head -n1 | cut -d' ' -f1").strip()

      def asked_in_plain(name):
          return name in router.succeed("journalctl -u dnsmasq --no-pager")

      def mode(word):
          machine.succeed(f"systemctl start losos-adblock-encrypted-dns@{word}.service")
          machine.succeed(f"losos-adblock encrypted-dns status | head -n1 | grep -qx {word}")
          # The forwarder looks at the switch every second.
          machine.sleep(2)

      start_all()
      router.wait_for_unit("dnsmasq.service")
      router.wait_for_unit("coredns.service")
      router.wait_for_open_port(443)
      router.wait_for_open_port(853)
      router.wait_for_open_port(80)
      machine.wait_for_unit("NetworkManager.service")
      machine.wait_until_succeeds("nmcli -t -f IP4.DNS device show eth1 | grep -q 192.168.1.", timeout=120)

      with subtest("on from the start, names come over HTTPS and no plain query leaves"):
          machine.succeed("losos-adblock encrypted-dns status | head -n1 | grep -qx on")
          machine.succeed("test ! -e /var/lib/losos-adblock/encrypted-dns")
          machine.wait_until_succeeds("getent ahostsv4 first.test", timeout=60)
          before = plain()
          assert lookup("site.test") == "10.0.0.42"
          assert plain() == before, "a plain DNS query left with encrypted DNS on"
          assert not asked_in_plain("site.test")

      with subtest("a name only the local network has goes to its server"):
          assert lookup("printer.lan") == "192.168.1.50"

      with subtest("the blocklist still applies"):
          machine.succeed("systemctl start losos-adblock-update.service")
          machine.wait_until_succeeds("resolvectl flush-caches; getent ahostsv4 ads.test | grep -q '^0.0.0.0'", timeout=60)

      with subtest("servers of the person's own, over TLS, replace the OS's"):
          machine.succeed("printf 'on\\ntls://doh.test ${router}\\n' > /var/lib/losos-adblock/encrypted-dns")
          machine.sleep(2)
          before = plain()
          assert lookup("tls.test") == "10.0.0.53"
          assert plain() == before
          # Turning it off and on keeps them.
          mode("off")
          mode("on")
          machine.succeed("losos-adblock encrypted-dns status | grep -q 'tls://doh.test'")
          assert lookup("tls2.test") == "10.0.0.53"
          machine.succeed("printf 'on\\n' > /var/lib/losos-adblock/encrypted-dns")
          machine.sleep(2)

      with subtest("forced upstreams stand encrypted DNS aside"):
          machine.succeed("mkdir -p /run/losos-adblock && echo 'nameserver ${router}' > /run/losos-adblock/upstreams && touch /run/losos-adblock/upstreams-forced")
          machine.wait_until_succeeds("losos-adblock encrypted-dns status | grep -q forced", timeout=10)
          machine.sleep(2)
          assert lookup("override.test") == "192.168.1.77"
          machine.succeed("rm /run/losos-adblock/upstreams-forced")
          machine.sleep(2)
          assert lookup("override2.test") == "10.0.0.42"

      router.succeed("systemctl stop coredns.service")

      with subtest("strict, with the encrypted servers gone, lookups fail and nothing leaves in plain text"):
          mode("strict")
          before = plain()
          machine.succeed("resolvectl flush-caches")
          machine.fail("getent ahostsv4 strict.test")
          assert plain() == before, "strict encrypted DNS fell back to plain DNS"

      with subtest("on, on an open network that blocks encrypted DNS, lookups still fail"):
          mode("on")
          machine.succeed("resolvectl flush-caches")
          machine.fail("getent ahostsv4 open.test")
          machine.succeed("journalctl -u losos-adblock --no-pager | grep -q 'network=Open'")
          assert not asked_in_plain("open.test"), "encrypted DNS fell back to plain DNS on an open network"

      with subtest("on, behind a captive portal, names come from the network's server"):
          router.succeed("echo 'Sign in to the Wi-Fi' > /srv/check_network_status.txt")
          machine.wait_until_succeeds("resolvectl flush-caches; test \"$(getent ahostsv4 portal.test | head -n1 | cut -d' ' -f1)\" = 192.168.1.77", timeout=60)
          machine.succeed("journalctl -u losos-adblock --no-pager | grep -q 'network=Portal'")

      with subtest("strict, behind the same portal, lookups still fail"):
          mode("strict")
          machine.succeed("resolvectl flush-caches")
          machine.fail("getent ahostsv4 strict2.test")
          assert not asked_in_plain("strict2.test")
          router.succeed("echo 'NetworkManager is online' > /srv/check_network_status.txt")

      with subtest("off, the network answers"):
          mode("off")
          assert lookup("off.test") == "192.168.1.77"

      with subtest("on again, with the servers back, names come over HTTPS"):
          router.succeed("systemctl start coredns.service")
          router.wait_for_open_port(443)
          mode("on")
          machine.wait_until_succeeds("resolvectl flush-caches; test \"$(getent ahostsv4 back.test | head -n1 | cut -d' ' -f1)\" = 10.0.0.42", timeout=60)
    '';
}
