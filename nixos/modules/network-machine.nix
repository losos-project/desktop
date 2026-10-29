# The network machine, sys-net (docs/network-machine.md): the PC's network
# cards in a service machine of their own, as Qubes keeps them in sys-net,
# and the host on the internet only through it. The machine is BusyBox and
# losos-netd on the host's kernel (busybox-guests.nix), nothing else that
# could be updated, logged in to or persisted in: the code that parses what
# arrives from a network (the cards' drivers and firmware, Wi-Fi's
# handshakes, DHCP) runs there and not on the host. What it does for
# whatever comes in from the host:
#
#  - answers DNS, through losos-adblock's forwarder, so ad blocking happens
#    there for the host and every machine (docs/adblock.md);
#  - routes everything else out through its cards, behind its own address;
#  - in schizo mode, routes it all to the Tor gateway instead and lets
#    nothing out but Tor (docs/schizo-mode.md).
#
# The host's half:
#
#  - the cards, every Ethernet and Wi-Fi card whose IOMMU group can be
#    fenced off, bound to vfio-pci before NetworkManager starts and handed
#    to the machine (passthrough.nix), which loads their drivers and
#    firmware from the host's own (busybox-host-kernel.nix);
#  - the host's link to it, losos-up0, a bridge NetworkManager keeps up
#    with the host's address and the machine as its router, so every app
#    that asks NetworkManager whether it is online is told it is; the other
#    machines' traffic leaves the host the same way, through libvirt's NAT;
#  - DNS: resolved asks the machine, where the ad blocker's forwarder
#    answers (adblock.nix leaves the host's own off);
#  - `nmcli`, which on the host is the machine's (losos-vm-call), answered
#    there by losos-netd in NetworkManager's place, so Settings' Network
#    pages, Danube's captive portal watch and a terminal work on the cards
#    where they are;
#  - schizo mode: plugging the machine into the Tor gateway's networks and
#    telling it the mode (schizo.nix runs the gateway).
#
# Where the IOMMU cannot fence a card off (none, or a group shared with
# something else), the card stays on the host and the topology does not
# change: NetworkManager on the host runs it with its routes in a table of
# their own, and the host routes only the machine's traffic there, over
# losos-wan0. The host's own connections and its machines' still go through
# the machine, its ad blocker and, in schizo mode, Tor. That is how it runs
# in a VM test, where there is nothing to hand through.
#
# The machine stays out of the Machines app (service-machines.nix): it is
# the OS's, not a person's to start, stop or delete.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.machines.network;
  addresses = import ./network-addresses.nix;
  schizoAddresses = import ./schizo-addresses.nix;
  inherit (addresses)
    up
    wan
    tor
    torext
    ;
  name = "sys-net";
  libvirt = config.virtualisation.libvirtd.package;
  nm = config.networking.networkmanager.package;
  devices = "/run/losos-service-machines/${name}/devices";
  share = "/run/losos-net";

  # The host's `nmcli`: the network machine's while it holds the cards,
  # the host's own while the host does, so it always reaches the
  # NetworkManager that has them.
  nmcliShim = pkgs.writeShellApplication {
    name = "nmcli";
    runtimeInputs = [
      pkgs.losos-vm-call
      pkgs.coreutils
    ];
    text = ''
      if [ -n "$(ls ${devices}/pci-*.xml 2>/dev/null)" ]; then
        exec losos-vm-call call nmcli "$@"
      fi
      exec ${nm}/bin/nmcli "$@"
    '';
  };

  # libvirt's view of the two bridges NetworkManager makes, so a machine's
  # card can be put on them by network name like any other.
  bridgeNetwork =
    link:
    pkgs.writeText "${link.libvirt}.xml" ''
      <network>
        <name>${link.libvirt}</name>
        <forward mode='bridge'/>
        <bridge name='${link.bridge}'/>
      </network>
    '';

  # The servers the host's network gave it, for the machine's forwarder,
  # while the host holds the cards: the machine has no other way to learn
  # them. Rewritten whenever a connection on the host changes.
  upstreams = pkgs.writeShellApplication {
    name = "losos-net-upstreams";
    runtimeInputs = [
      nm
      pkgs.coreutils
      pkgs.gawk
      config.systemd.package
    ];
    text = ''
      # mDNS on the host's link to the machine, which reflects it to the
      # networks it joins: NetworkManager tells resolved nothing here
      # (main.dns is none), so this does, each time the link comes up.
      if [ "''${1:-}" = ${up.bridge} ] && [ "''${2:-}" = up ]; then
        resolvectl mdns ${up.bridge} yes || true
      fi
      nmcli -t -f GENERAL.DEVICE,IP4.DNS,IP6.DNS device show |
        awk -F: '
          /^GENERAL.DEVICE:/ { skip = ($2 ~ /^losos-/ || $2 == "lo") }
          /^IP[46].DNS/ && !skip {
            sub(/^[^:]*:/, "")
            gsub(/\\/, "")
            if ($0 != "") print "nameserver " $0
          }
        ' > ${share}/upstreams.new
      mv ${share}/upstreams.new ${share}/upstreams
    '';
  };

  schizoFlag = "/var/lib/losos/schizo";

  # The machine's side of what the host shares: the ad blocker's compiled
  # list and the host's word on the mode and its servers.
  hostShare = "/var/lib/losos-host";

  # The firewall, loaded whole at boot (losos-netd's nft-base). Until
  # losos-netd has applied the mode, nothing from the host is forwarded, so
  # a boot in schizo mode never leaks a packet before the mode is known.
  nftBase = pkgs.writeText "sys-net.nft" ''
    flush ruleset
    table inet network {
      chain input {
        type filter hook input priority filter; policy drop;
        iif lo accept
        ct state established,related accept
        meta l4proto icmp accept
        # DHCP's answers on the networks this machine joins.
        udp dport 68 accept
        # mDNS from the host and those networks, which losos-netd reflects
        # between them; never from the gateway's links.
        iifname != { "${wan.link}", "${tor.link}", "${torext.link}" } udp dport 5353 accept
        # The ad blocker's forwarder, the host's one DNS server.
        iifname "${up.link}" ip daddr ${up.machine} meta l4proto { tcp, udp } th dport 53 accept
      }
      chain forward {
        type filter hook forward priority filter; policy drop;
        ct state established,related accept
        jump mode-forward
      }
      chain mode-forward {
        iifname "${up.link}" drop
      }
      chain output {
        type filter hook output priority filter; policy accept;
        jump mode-output
      }
      chain mode-output {
      }
    }

    # Everything leaves behind this machine's address on its way out, the
    # gateway's own connections included; nothing going back to the host
    # is changed.
    table ip network-nat {
      chain postrouting {
        type nat hook postrouting priority srcnat;
        oifname != "${up.link}" ip saddr { ${up.network}/${toString up.prefix}, ${schizoAddresses.uplink.network}/${toString schizoAddresses.uplink.prefix} } masquerade
      }
    }
  '';

  # The two halves schizo mode changes, each a whole replacement of the
  # chains the ruleset above jumps to, applied in one transaction.
  nftOff = pkgs.writeText "sys-net-off.nft" ''
    flush chain inet network mode-forward
    flush chain inet network mode-output
    add rule inet network mode-forward iifname "${up.link}" oifname != { "${tor.link}", "${torext.link}" } accept
    add rule inet network mode-forward iifname "${torext.link}" oifname != { "${up.link}", "${tor.link}" } accept
  '';
  # Whonix's rule, here: the host's and its machines' connections reach the
  # gateway and nothing else, the gateway's own reach the internet, and
  # this machine sends nothing past its links but DHCP, so no program here
  # leaks around Tor either. Its DNS goes to the gateway (losos-netd).
  nftOn = pkgs.writeText "sys-net-on.nft" ''
    flush chain inet network mode-forward
    flush chain inet network mode-output
    add rule inet network mode-forward iifname "${up.link}" oifname "${tor.link}" accept
    add rule inet network mode-forward iifname "${torext.link}" oifname != { "${up.link}", "${tor.link}" } accept
    add rule inet network mode-output oifname { "lo", "${up.link}", "${tor.link}" } accept
    add rule inet network mode-output oifname "${torext.link}" ct state established,related accept
    add rule inet network mode-output udp dport 67 accept
    add rule inet network mode-output drop
  '';

  # losos-netd's configuration (src/losos-netd/src/lib.rs, Config): the
  # links by their MACs, which it names them by; the gateway; the rules;
  # the host's share and the state disk; and what it keeps running: the
  # host's calls, and the ad blocker's forwarder where the host sends DNS.
  netdConfig = pkgs.writeText "losos-netd.conf" ''
    link up ${up.mac} ${up.link} ${up.machine}/${toString up.prefix}
    link wan ${wan.mac} ${wan.link} ${wan.machine}/${toString wan.prefix} ${wan.host}
    link tor ${tor.mac} ${tor.link} ${schizoAddresses.inside.host}/${toString schizoAddresses.inside.prefix}
    link torext ${torext.mac} ${torext.link} ${schizoAddresses.uplink.host}/${toString schizoAddresses.uplink.prefix}
    gateway ${schizoAddresses.inside.gateway}
    nft-base ${nftBase}
    nft-on ${nftOn}
    nft-off ${nftOff}
    host ${hostShare}
    state-disk /dev/vda /var/lib/losos-network
    run losos-vm-call serve ${toString addresses.ports.nmcli} losos-netd nmcli
    run losos-vm-call serve ${toString addresses.ports.apply} losos-netd apply
    run losos-adblock --state /var/lib/losos-adblock serve --listen ${up.machine}:53
    run losos-peripherals receiver
  '';
in
{
  options.losos.machines.network.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = ''
      Whether the PC's network cards and NetworkManager run in the network
      machine rather than on the host, with every connection the host and
      its machines make going through it.
    '';
  };

  config = lib.mkIf cfg.enable {
    losos.busyboxGuests.${name} = {
      init = "${lib.getExe pkgs.losos-netd} init";
      hostKernelTree = true;
      packages = [
        pkgs.losos-netd
        pkgs.losos-vm-call
        pkgs.losos-adblock
        pkgs.wpa_supplicant
        pkgs.nftables
        # BusyBox's ip has no policy routing, which schizo mode uses.
        pkgs.iproute2
        # USB network cards from sys-usb, over USB/IP (peripherals.nix).
        pkgs.losos-peripherals
      ];
      # The rest load from the host's tree as they are needed.
      modules = [
        "virtio_net"
        "virtio_blk"
        "virtio_balloon"
        "ext4"
        "vhci_hcd"
      ];
      shares = [
        # The ad blocker's compiled list, which the host keeps fetching
        # and compiling (it compiles the browser's half from the same
        # lists).
        {
          source = "/var/lib/losos-adblock";
          target = "/var/lib/losos-adblock";
          tag = "adblock";
        }
        {
          source = share;
          target = hostShare;
          tag = "host";
        }
      ];
      files."/etc/losos-netd.conf" = netdConfig;
    };

    losos.serviceMachines.${name} = {
      busybox = name;
      title = "The network machine";
      # The host has no other way out.
      whenStopped = "restart";
      memory = 256;
      # The saved networks: a few KiB, in a disk made the first time the
      # machine starts and laid out the first time it mounts it.
      stateSize = "16M";
      inherit (addresses) cid;
      # A USB network card, handed to it while it runs.
      xhci = true;
      interfaces = [
        {
          network = up.libvirt;
          inherit (up) mac;
        }
        {
          network = wan.libvirt;
          inherit (wan) mac;
        }
      ];
      passthrough.pciClasses = [
        "0x0200"
        "0x0280"
      ];
    };

    # Before NetworkManager, so the host never brings a card up that is
    # about to leave.
    systemd.services."losos-passthrough-${name}".before = [
      "NetworkManager.service"
      "network-pre.target"
    ];

    systemd.services."losos-service-machine-${name}" = {
      wantedBy = [ "multi-user.target" ];
      requires = [ "losos-network-links.service" ];
      after = [ "losos-network-links.service" ];
    };

    # The bridges' libvirt networks, defined from the image every boot, and
    # the networks the host had saved, shared once for the machine to take
    # in. Waits for NetworkManager to have made the bridges.
    systemd.services.losos-network-links = {
      description = "The network machine's links to the host";
      requires = [ "libvirtd.service" ];
      after = [
        "libvirtd.service"
        "NetworkManager.service"
      ];
      path = [
        libvirt
        pkgs.coreutils
        pkgs.gnused
        pkgs.iproute2
      ];
      environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        StateDirectory = "losos-network";
      };
      script = ''
        for _ in $(seq 60); do
          ip link show ${up.bridge} >/dev/null 2>&1 && ip link show ${wan.bridge} >/dev/null 2>&1 && break
          sleep 0.5
        done
        for net in ${bridgeNetwork up} ${bridgeNetwork wan}; do
          virsh -q net-define "$net" >/dev/null
        done
        for net in ${up.libvirt} ${wan.libvirt}; do
          virsh -q net-autostart "$net" --disable >/dev/null 2>&1 || true
          [ "$(virsh -q net-info "$net" | sed -n 's/^Active: *//p')" = yes ] || virsh -q net-start "$net"
        done

        # QEMU reads the share as libvirt's QEMU user, so the copies are
        # that user's alone.
        if [ ! -e "$STATE_DIRECTORY/imported" ]; then
          mkdir -p ${share}/import
          for file in /etc/NetworkManager/system-connections/*.nmconnection; do
            [ -e "$file" ] && install -m 0600 -o qemu-libvirtd "$file" ${share}/import/
          done
          chown qemu-libvirtd ${share}/import
          chmod 0700 ${share}/import
          touch "$STATE_DIRECTORY/imported"
        fi
      '';
    };

    systemd.tmpfiles.rules = [ "d ${share} 0755 root root -" ];

    networking.networkmanager = {
      # The host's link to the machine, with the machine as its router and
      # nothing else. Bridges with no address of their own on the far side:
      # libvirt puts the machine's card on each (bridgeNetwork).
      ensureProfiles.profiles = {
        losos-up = {
          connection = {
            id = "losos-up";
            type = "bridge";
            interface-name = up.bridge;
            autoconnect = true;
            mdns = 0;
            llmnr = 0;
          };
          bridge.stp = false;
          ipv4 = {
            method = "manual";
            address1 = "${up.host}/${toString up.prefix},${up.machine}";
            # The main table, where the host's own connections look,
            # whatever the cards' connections default to below.
            route-table = 254;
          };
          ipv6.method = "disabled";
        };
        losos-wan = {
          connection = {
            id = "losos-wan";
            type = "bridge";
            interface-name = wan.bridge;
            autoconnect = true;
            mdns = 0;
            llmnr = 0;
          };
          bridge.stp = false;
          ipv4 = {
            method = "manual";
            address1 = "${wan.host}/${toString wan.prefix}";
            route-table = 254;
            # What comes in from the machine is routed by the cards' table.
            routing-rule1 = "priority ${toString wan.table} iif ${wan.bridge} table ${toString wan.table}";
          };
          ipv6.method = "disabled";
        };
      };
      settings = {
        # A card the host kept routes in its own table: the host's own
        # connections take the machine, and only the machine's are routed
        # to the cards (losos-wan's rule).
        connection = {
          "ipv4.route-table" = wan.table;
          "ipv6.route-table" = wan.table;
        };
        # The host's DNS is the machine's forwarder (resolved below), not
        # the servers a card's network names: those go to the machine.
        main.dns = lib.mkForce "none";
      };
      # Keeps the machine's copy of those servers current.
      dispatcherScripts = [
        {
          source = lib.getExe upstreams;
          type = "basic";
        }
      ];
    };

    # Every name, through the machine. Plain DNS, which is what its
    # forwarder speaks; DNS over TLS would have to start there.
    services.resolved.settings.Resolve = {
      DNS = up.machine;
      Domains = "~.";
      DNSOverTLS = lib.mkForce "no";
    };

    # The machine's traffic out through the host's cards, behind the host's
    # address there, and nothing of the host's own: only the machine's link
    # is let through, and only out.
    networking.nftables.tables.losos-network = {
      family = "ip";
      content = ''
        chain forward {
          type filter hook forward priority filter; policy accept;
          iifname "${wan.bridge}" oifname { "${up.bridge}", "virbr*", "vnet*" } drop
          oifname "${wan.bridge}" ct state != { established, related } drop
        }
        chain postrouting {
          type nat hook postrouting priority srcnat;
          ip saddr ${wan.network}/${toString wan.prefix} oifname != "${wan.bridge}" masquerade
        }
      '';
    };

    environment.systemPackages = [ (lib.hiPrio nmcliShim) ];

    losos.vmCalls = {
      # Whoever NetworkManager let change the network on the host: its own
      # group, administrators, and the person at the screen.
      nmcli = {
        inherit (addresses) cid;
        port = addresses.ports.nmcli;
        allow = [
          "group:networkmanager"
          "group:wheel"
          "active-session"
        ];
      };
      # Root's alone: the schizo unit below.
      net-apply = {
        inherit (addresses) cid;
        port = addresses.ports.apply;
      };
    };

    # Schizo mode (schizo.nix): the machine joins the gateway's two
    # networks, as Whonix's first Workstation on the inside and the
    # gateway's router on its uplink, and is told the mode at once rather
    # than at its next look.
    systemd.services.losos-net-schizo = {
      description = "Route the network machine through the Tor gateway";
      partOf = [
        "losos-schizo.target"
        "losos-service-machine-${name}.service"
      ];
      wantedBy = [ "losos-schizo.target" ];
      requires = [
        "losos-schizo-network.service"
        "losos-service-machine-${name}.service"
      ];
      after = [
        "losos-schizo-network.service"
        "losos-service-machine-${name}.service"
        "losos-vm-call-relay.service"
      ];
      unitConfig.ConditionPathExists = schizoFlag;
      path = [
        libvirt
        pkgs.losos-vm-call
        pkgs.coreutils
      ];
      environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        attach() {
          virsh -q domiflist ${name} | grep -qi "$2" ||
            virsh -q attach-interface ${name} network "$1" --mac "$2" --model virtio --live
        }
        attach ${schizoAddresses.inside.bridge} ${addresses.tor.mac}
        attach ${schizoAddresses.uplink.bridge} ${addresses.torext.mac}
        touch ${share}/schizo
        # Within seconds by itself; at once if the machine answers.
        losos-vm-call call net-apply || true
      '';
      preStop = ''
        rm -f ${share}/schizo
        losos-vm-call call net-apply || true
        virsh -q detach-interface ${name} network --mac ${addresses.tor.mac} --live || true
        virsh -q detach-interface ${name} network --mac ${addresses.torext.mac} --live || true
      '';
    };
  };
}
