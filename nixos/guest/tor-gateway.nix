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
{ config, ... }:

let
  addresses = import ../modules/schizo-addresses.nix;
  tor = config.ids.uids.tor;
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
            iifname "inside" ip daddr ${addresses.inside.gateway} tcp dport { ${toString addresses.ports.trans}, ${toString addresses.ports.socks} } accept
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
          chain prerouting {
            type nat hook prerouting priority dstnat;
            # DNS to any address, the gateway's included, goes to Tor's
            # DNSPort, which answers UDP only; every TCP connection that is
            # not for the gateway itself goes to its TransPort, which reads
            # where it was going from the connection and opens it through
            # Tor. Other UDP has nowhere to go and is dropped.
            iifname "inside" udp dport 53 redirect to :${toString addresses.ports.dns}
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
      VirtualAddrNetworkIPv4 = "10.192.0.0/10";
      AutomapHostsOnResolve = true;
      ClientUseIPv6 = false;
    };
  };
  # The state disk has to be there before Tor starts, or its guards would
  # be picked again on the tmpfs and forgotten.
  systemd.services.tor.unitConfig.RequiresMountsFor = [ "/var/lib/tor" ];
}
