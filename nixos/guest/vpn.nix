# sys-vpn (docs/vpn.md): a machine whose only job is the person's VPN, one
# provider's WireGuard tunnel, which every connection of the PC goes
# through while a VPN is set. Built like the Tor gateway (tor-gateway.nix):
# Kicksecure, with two network cards. Its inside, on losos-vpn, is where
# the network machine hands it the host's connections; they go into the
# tunnel or nowhere. Its uplink, on losos-vpnext, is its one way out, with
# the network machine as its router, and only the tunnel's own packets and
# the one name it looks up, its provider's endpoint, may use it.
#
# Disposable: it has no state disk, so every start is a new machine from
# the image, and the tunnel file is the host's, read-only over 9p. Setting
# another provider starts a new one.
{
  lib,
  pkgs,
  ...
}:

let
  addresses = import ../modules/vpn-addresses.nix;
  network = import ../modules/network-addresses.nix;
  inherit (addresses) inside uplink share;
  mount = "/run/losos-vpn";
  # losos-vpn's fwmark and table (src/losos-vpn/src/main.rs).
  mark = "51820";
in
{
  # Names by MAC address, which vpn.nix gives each card, so the firewall
  # below cannot mistake one for the other.
  systemd.network = {
    enable = true;
    links = {
      "10-uplink" = {
        matchConfig.MACAddress = addresses.machine.uplinkMac;
        linkConfig.Name = "uplink";
      };
      "10-inside" = {
        matchConfig.MACAddress = addresses.machine.insideMac;
        linkConfig.Name = "inside";
      };
    };
    networks = {
      "10-uplink" = {
        matchConfig.Name = "uplink";
        address = [ "${uplink.vpn}/${toString uplink.prefix}" ];
        gateway = [ uplink.net ];
        networkConfig.LinkLocalAddressing = "no";
      };
      "10-inside" = {
        matchConfig.Name = "inside";
        address = [ "${inside.vpn}/${toString inside.prefix}" ];
        networkConfig.LinkLocalAddressing = "no";
      };
      # losos-vpn makes the tunnel; networkd leaves it be.
      "10-wg" = {
        matchConfig.Name = "wg0";
        linkConfig.Unmanaged = true;
      };
    };
  };

  networking = {
    useNetworkd = true;
    useDHCP = false;
    enableIPv6 = false;
    # The network machine's forwarder, for the endpoint's name. Everything
    # else asks the provider's servers, through the tunnel, from there.
    nameservers = [ network.up.machine ];
    # The rules below instead of NixOS's firewall, which knows only input.
    firewall.enable = false;
    nftables = {
      enable = true;
      ruleset = ''
        table inet vpn {
          chain input {
            type filter hook input priority filter; policy drop;
            iif lo accept
            ct state established,related accept
            meta l4proto icmp accept
          }
          # The inside reaches the tunnel and nothing else: with the
          # tunnel down there is nowhere to go (losos-vpn's table).
          chain forward {
            type filter hook forward priority filter; policy drop;
            ct state established,related accept
            iifname "inside" oifname "wg0" accept
          }
          # This machine sends the tunnel's own packets, which carry its
          # mark, and asks for the endpoint's name; nothing else leaves by
          # the uplink.
          chain output {
            type filter hook output priority filter; policy drop;
            oif lo accept
            ct state established,related accept
            meta l4proto icmp accept
            oifname "wg0" accept
            oifname "uplink" meta mark ${mark} meta l4proto udp accept
            oifname "uplink" ip daddr ${network.up.machine} meta l4proto { tcp, udp } th dport 53 accept
          }
        }
        # The inside's connections leave as this end of the tunnel.
        table ip vpn-nat {
          chain postrouting {
            type nat hook postrouting priority srcnat;
            oifname "wg0" masquerade
          }
        }
      '';
    };
  };

  boot.kernel.sysctl = {
    "net.ipv4.ip_forward" = 1;
    # Loose: answers come back by the tunnel to addresses the main table
    # would send by the uplink, and strict filtering (Kicksecure's) drops
    # every one of them.
    "net.ipv4.conf.all.rp_filter" = lib.mkForce 2;
    "net.ipv4.conf.default.rp_filter" = lib.mkForce 2;
  };

  fileSystems.${mount} = {
    device = share.tag;
    fsType = "9p";
    options = [
      "ro"
      "trans=virtio"
      "version=9p2000.L"
    ];
  };

  systemd.services.losos-vpn = {
    description = "The VPN's WireGuard tunnel";
    wantedBy = [ "multi-user.target" ];
    after = [
      "systemd-networkd.service"
      "nftables.service"
    ];
    unitConfig.RequiresMountsFor = mount;
    path = [
      pkgs.iproute2
      pkgs.wireguard-tools
    ];
    serviceConfig = {
      ExecStart = "${lib.getExe pkgs.losos-vpn} up ${mount}/tunnel.conf";
      Restart = "always";
      RestartSec = 2;
    };
  };
}
