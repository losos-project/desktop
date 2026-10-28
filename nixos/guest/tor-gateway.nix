# The Tor gateway schizo mode routes through (docs/schizo-mode.md): a
# machine whose only job is Tor, built the way Whonix builds its Gateway.
# It has two network cards. Its uplink, on libvirt's losos-torext network,
# reaches the internet through whatever holds 10.152.192.1 there, the
# host's NAT or the network machine's, and only Tor may use it. Its
# inside card, on losos-tor, holds Whonix's gateway address, 10.152.152.10,
# and everything sent to it there goes through Tor or nowhere: TCP into
# Tor's TransPort, DNS into its DNSPort, SOCKS into its SocksPort. It does
# not route; nothing reaches the uplink from the inside except as Tor
# traffic.
#
# A service machine (nixos/guest/service.nix): booted from the host's own
# store, part of the image and updated with it. The one thing it keeps is
# Tor's state, on its state disk: its guards, which Tor picks once and
# keeps for months so a person is not handed a new, maybe hostile, entry
# relay on every boot. It has no resolver of its own: Tor does its own
# lookups.
#
# While an edge is set (docs/schizo-mode.md, "Through an edge"), the
# inside's TCP goes to losos-edge-chain instead of the TransPort, which
# reaches the edge's onion service through Tor and leaves from the edge.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  addresses = import ../modules/schizo-addresses.nix;
  tor = config.ids.uids.tor;
  inherit (addresses) edge ports;
  edgeFile = "${edge.gatewayDir}/${edge.file}";
  # Where Tor's DNSPort puts the onions it is asked for.
  virtualAddresses = "10.192.0.0/10";
in
{
  losos.service.state = "/var/lib/tor";

  # Names by MAC address, which schizo.nix gives each card, so the
  # firewall below cannot mistake one for the other.
  systemd.network = {
    enable = true;
    links = {
      "10-uplink" = {
        matchConfig.MACAddress = addresses.gateway.uplinkMac;
        linkConfig.Name = "uplink";
      };
      "10-inside" = {
        matchConfig.MACAddress = addresses.gateway.insideMac;
        linkConfig.Name = "inside";
      };
    };
    networks = {
      "10-uplink" = {
        matchConfig.Name = "uplink";
        address = [ "${addresses.uplink.gateway}/${toString addresses.uplink.prefix}" ];
        gateway = [ addresses.uplink.host ];
        networkConfig.LinkLocalAddressing = "no";
      };
      "10-inside" = {
        matchConfig.Name = "inside";
        address = [ "${addresses.inside.gateway}/${toString addresses.inside.prefix}" ];
        networkConfig.LinkLocalAddressing = "no";
      };
    };
  };
  networking = {
    useNetworkd = true;
    useDHCP = false;
    enableIPv6 = false;
    # Tor resolves through its circuits; the gateway itself asks no one.
    nameservers = [ ];
    # The rules below instead of NixOS's firewall, which knows only input.
    firewall.enable = false;
    nftables = {
      enable = true;
      # Whonix's gateway firewall, in nftables. Only Tor's own user may send
      # anything out of the uplink, so no program here can talk to the
      # internet past Tor; the inside reaches Tor's ports and nothing else;
      # nothing is forwarded at all. IPv6 has no address to come from, and
      # these tables drop it besides.
      ruleset = ''
        table inet gateway {
          chain input {
            type filter hook input priority filter; policy drop;
            iif lo accept
            ct state established,related accept
            iifname "inside" ip daddr ${addresses.inside.gateway} tcp dport { ${toString addresses.ports.trans}, ${toString addresses.ports.socks}, ${toString ports.edge} } accept
            iifname "inside" ip daddr ${addresses.inside.gateway} udp dport ${toString addresses.ports.dns} accept
          }
          chain forward {
            type filter hook forward priority filter; policy drop;
          }
          chain output {
            type filter hook output priority filter; policy drop;
            oif lo accept
            oifname "inside" ct state established,related accept
            oifname "uplink" meta skuid ${toString tor} accept
          }
        }

        table ip gateway-nat {
          # Empty, or one rule that sends TCP to losos-edge-chain while an
          # edge is set; losos-edge-select fills it.
          chain edge {
          }
          chain prerouting {
            type nat hook prerouting priority dstnat;
            # DNS to any address, the gateway's included, goes to Tor's
            # DNSPort, which answers UDP only; every TCP connection that is
            # not for the gateway itself goes to its TransPort, which reads
            # where it was going from the connection and opens it through
            # Tor, or to the edge chain while an edge is set. An onion,
            # whose address Tor's DNSPort handed out, is Tor's alone and
            # never goes to the edge. Other UDP has nowhere to go and is
            # dropped.
            iifname "inside" udp dport 53 redirect to :${toString addresses.ports.dns}
            iifname "inside" ip daddr ${virtualAddresses} meta l4proto tcp redirect to :${toString addresses.ports.trans}
            iifname "inside" ip daddr != ${addresses.inside.gateway} meta l4proto tcp jump edge
            iifname "inside" ip daddr != ${addresses.inside.gateway} meta l4proto tcp redirect to :${toString addresses.ports.trans}
          }
        }
      '';
    };
  };
  # Tor listens on the inside address before networkd has put it there.
  boot.kernel.sysctl."net.ipv4.ip_nonlocal_bind" = 1;

  # NTP could not leave through Tor anyway; the clock is the host's
  # (service.nix).
  services.resolved.enable = false;

  services.tor = {
    enable = true;
    # A client and nothing else: never a relay, never an exit.
    client = {
      enable = true;
      # SOCKS for programs that ask for Tor themselves. Each SOCKS
      # username, each client and each destination gets circuits of its
      # own, as Whonix's own ports isolate them.
      socksListenAddress = {
        addr = addresses.inside.gateway;
        port = addresses.ports.socks;
        IsolateClientAddr = true;
        IsolateSOCKSAuth = true;
        IsolateDestAddr = true;
        IsolateDestPort = true;
      };
    };
    settings = {
      # The inside's SOCKS above, and one on loopback for the edge chain,
      # which isolates its streams with a username per client and
      # destination port.
      SOCKSPort = [
        {
          addr = "127.0.0.1";
          port = ports.socks;
          IsolateSOCKSAuth = true;
        }
      ];
      TransPort = [
        {
          addr = addresses.inside.gateway;
          port = addresses.ports.trans;
          IsolateClientAddr = true;
          IsolateClientProtocol = true;
          IsolateDestAddr = true;
          IsolateDestPort = true;
        }
      ];
      DNSPort = [
        {
          addr = addresses.inside.gateway;
          port = addresses.ports.dns;
          IsolateClientAddr = true;
          IsolateDestAddr = true;
        }
      ];
      # An onion name looked up through the DNSPort is answered with an
      # address from this range, which the TransPort then takes as that
      # onion, so .onion works for every program, not only for a browser
      # that speaks SOCKS.
      VirtualAddrNetworkIPv4 = virtualAddresses;
      AutomapHostsOnResolve = true;
      ClientUseIPv6 = false;
    };
  };
  # The state disk has to be there before Tor starts, or its guards would
  # be picked again on the tmpfs and forgotten.
  systemd.services.tor.unitConfig.RequiresMountsFor = [ "/var/lib/tor" ];

  # The edge the host chose, if any, in a share of the host's own: present
  # while schizo mode must leave through that edge, gone otherwise. Never
  # cached, so the host's change is what the next read sees.
  fileSystems.${edge.gatewayDir} = {
    device = edge.share;
    fsType = "9p";
    options = [
      "ro"
      "trans=virtio"
      "version=9p2000.L"
      "cache=none"
      "nofail"
    ];
  };

  # The chain itself: every connection the edge chain rule sends here goes
  # through Tor to the edge's onion and out from the edge, or nowhere. It
  # reads the edge from the share for each connection and refuses one
  # without it, so a chain that cannot reach the edge fails closed instead
  # of falling back to the TransPort. Root for the file, which is the
  # host's root's own, and nothing else.
  systemd.services.losos-edge-chain = {
    description = "Schizo mode's way out through an edge";
    wantedBy = [ "multi-user.target" ];
    after = [ "tor.service" ];
    unitConfig.RequiresMountsFor = [ edge.gatewayDir ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-edge-chain} --listen ${addresses.inside.gateway}:${toString ports.edge} --tor 127.0.0.1:${toString ports.socks} --config ${edgeFile}";
      Restart = "always";
      CapabilityBoundingSet = "";
      NoNewPrivileges = true;
      PrivateTmp = true;
      PrivateDevices = true;
      ProtectSystem = "strict";
      ProtectHome = true;
      ProtectKernelTunables = true;
      ProtectKernelModules = true;
      ProtectControlGroups = true;
      RestrictAddressFamilies = [ "AF_INET" ];
      SystemCallFilter = [ "@system-service" ];
    };
  };

  # Points the inside's TCP at the chain while the host has set an edge,
  # and back at the TransPort when it has not. The host changes the file
  # through 9p, which tells no inotify here, so this looks every two
  # seconds. Its first look is done before Tor starts, so a gateway that
  # boots with an edge set never sends a connection past it.
  systemd.services.losos-edge-select = {
    description = "Send schizo mode through the edge while one is set";
    wantedBy = [ "multi-user.target" ];
    before = [ "tor.service" ];
    after = [ "nftables.service" ];
    requires = [ "nftables.service" ];
    unitConfig.RequiresMountsFor = [ edge.gatewayDir ];
    path = [ pkgs.nftables ];
    script = ''
      follow() {
        if [ -e ${edgeFile} ]; then want=on; else want=off; fi
        if nft list chain ip gateway-nat edge | grep -q redirect; then have=on; else have=off; fi
        [ "$want" = "$have" ] && return
        if [ "$want" = on ]; then
          nft add rule ip gateway-nat edge redirect to :${toString ports.edge}
        else
          nft flush chain ip gateway-nat edge
        fi
        echo "edge $want"
      }
      follow
      # Started; from here on, follow the host.
      systemd-notify --ready
      while sleep 2; do follow; done
    '';
    serviceConfig = {
      Type = "notify";
      NotifyAccess = "all";
      Restart = "always";
    };
  };
}
