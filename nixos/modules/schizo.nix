# Schizo mode (docs/schizo-mode.md): every connection this PC and its
# machines make goes through Tor, by way of a gateway machine built like
# Whonix's Gateway (nixos/guest/tor-gateway.nix), or does not go at all.
#
# It is off until someone turns it on, in Settings' Network page or with
# `losos-schizo on`, and stays as they left it across restarts and updates:
# the choice is a file, /var/lib/losos/schizo, and every unit below starts
# only while it is there. On, the host
#
#  - drops everything it would send anywhere but to the gateway, to its own
#    machines and to the network's DHCP server, before NetworkManager has
#    brought a single link up (the kill switch);
#  - routes its own connections, and its machines' on libvirt's networks,
#    to the gateway's inside address, where Tor takes them;
#  - asks the gateway for every name, through the ad blocker's forwarder
#    when it runs;
#  - gives every network a random MAC address and sends no host name to
#    its DHCP server;
#  - starts the gateway, whose own uplink is the only thing the host still
#    forwards to the internet.
#
# It fails closed. The kill switch is the guarantee, the routing only what
# makes things work: with the gateway stopped, crashed or still starting,
# the route to it leads nowhere and every other way out is dropped, so
# nothing leaves at all. UDP other than DNS and all of IPv6 have no way
# through Tor, and are dropped too.
#
# The PC half only: the GSI has no machines (libvirt.nix).
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  addresses = import ./schizo-addresses.nix;
  inherit (addresses) inside uplink edge;
  libvirt = config.virtualisation.libvirtd.package;
  flag = "/var/lib/losos/schizo";
  name = "losos-tor-gateway";
  # The routing table the host's and its machines' connections take while
  # schizo mode is on, and the mark its machines' packets carry to reach it.
  table = "152";
  mark = "0x1520";

  # With the network machine (Matus's network VM, which holds the PC's
  # network cards and routes for the host and every machine), that machine
  # is the gateway's router on its uplink and the host's way to it on the
  # inside network, and it does the routing, the DNS and the MAC addresses
  # itself when schizo mode starts. Without it, the host does all of it.
  standalone = !(config.losos.machines.network.enable or false);
  # The network machine's link to the host.
  netUplink = "losos-up0";

  # The two networks. Plain bridges when the network machine routes
  # them. Otherwise `open` forwarding, because libvirt's own rules for an
  # isolated or NAT network would reject what the kill switch routes here
  # from the other machines' networks, and a packet one table rejects stays
  # rejected (libvirt.nix); the kill switch is the only firewall either has.
  # No dnsmasq: the gateway's addresses are fixed and it asks no one for
  # names. The host has an address on them only when it is the one routing.
  network =
    net:
    pkgs.writeText "${net.bridge}.xml" ''
      <network>
        <name>${net.bridge}</name>
        ${lib.optionalString standalone "<forward mode='open'/>"}
        <bridge name='${net.bridge}' stp='off' delay='0'/>
        <dns enable='no'/>
        ${lib.optionalString standalone "<ip address='${net.host}' prefix='${toString net.prefix}'/>"}
      </network>
    '';

  # The kill switch. One table, loaded in one transaction, which replaces
  # itself if it is already there.
  killSwitch = pkgs.writeText "losos-schizo.nft" (if standalone then standaloneRules else netRules);

  # With the network machine, the host has no network card of its own and
  # reaches everything through that machine, which in schizo mode lets it
  # reach the gateway and nothing else. The host still sends nothing out of
  # anything else, such as a card plugged in after boot that was not handed
  # to that machine.
  netRules = ''
    table inet losos-schizo
    delete table inet losos-schizo
    table inet losos-schizo {
      chain output {
        type filter hook output priority filter; policy drop;
        oif lo accept
        oifname "${netUplink}" meta nfproto ipv4 accept
        oifname "virbr*" accept
        oifname "vnet*" accept
      }
      chain input {
        type filter hook input priority filter; policy drop;
        iif lo accept
        iifname "${netUplink}" meta nfproto ipv4 accept
        iifname "virbr*" accept
        iifname "vnet*" accept
      }
      chain forward {
        type filter hook forward priority filter; policy drop;
        iifname "virbr*" oifname "${netUplink}" meta nfproto ipv4 accept
        iifname "vnet*" oifname "${netUplink}" meta nfproto ipv4 accept
        iifname "${netUplink}" oifname "virbr*" ct state established,related accept
        iifname "${netUplink}" oifname "vnet*" ct state established,related accept
      }
    }
  '';

  standaloneRules = ''
    table inet losos-schizo
    delete table inet losos-schizo
    table inet losos-schizo {
      # What the host sends. Loopback; whatever is routed to the gateway's
      # inside network, which is everything the routing below sends out;
      # the host's own machines; and DHCP to the network's server, which
      # NetworkManager needs to keep its address and the gateway its way
      # out. Nothing else, so a program that binds to a network card, or a
      # route someone adds, still reaches nothing.
      chain output {
        type filter hook output priority filter; policy drop;
        oif lo accept
        oifname "${inside.bridge}" meta nfproto ipv4 accept
        oifname "virbr*" accept
        oifname "vnet*" accept
        udp sport 68 udp dport 67 accept
      }

      # What reaches the host: the gateway's replies, its machines, and
      # DHCP's answers. Nothing from the network it is on, which is not
      # supposed to know it is there.
      chain input {
        type filter hook input priority filter; policy drop;
        iif lo accept
        iifname "${inside.bridge}" meta nfproto ipv4 accept
        iifname "virbr*" accept
        iifname "vnet*" accept
        udp sport 67 udp dport 68 accept
      }

      # What the host forwards: the gateway's uplink out to the internet and
      # its answers back, and the machines' connections to the gateway and
      # its answers back. Not the machines' connections anywhere else,
      # which libvirt's own rules would otherwise let out through its NAT.
      chain forward {
        type filter hook forward priority filter; policy drop;
        iifname "${uplink.bridge}" ip saddr ${uplink.gateway} oifname != "${inside.bridge}" oifname != "virbr*" oifname != "vnet*" accept
        oifname "${uplink.bridge}" ip daddr ${uplink.gateway} ct state established,related accept
        iifname "virbr*" oifname "${inside.bridge}" meta nfproto ipv4 accept
        iifname "vnet*" oifname "${inside.bridge}" meta nfproto ipv4 accept
        iifname "${inside.bridge}" oifname "virbr*" ct state established,related accept
        iifname "${inside.bridge}" oifname "vnet*" ct state established,related accept
      }

      # The machines' packets, and the gateway's answers to them, take the
      # gateway's routing table below; the host's own take it by coming from
      # loopback. Kept to the mark's own bits, so another user of marks is
      # left its own.
      chain prerouting {
        type filter hook prerouting priority mangle; policy accept;
        iifname "virbr*" meta mark set meta mark | ${mark}
        iifname "vnet*" meta mark set meta mark | ${mark}
        iifname "${inside.bridge}" meta mark set meta mark | ${mark}
      }

      # The gateway's uplink leaves as the host. Whatever reaches the
      # gateway from the machines arrives from the host's own inside
      # address, which is the only one the gateway answers.
      chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        ip saddr ${uplink.gateway} oifname != "${uplink.bridge}" masquerade
        oifname "${inside.bridge}" ip saddr != ${inside.host} masquerade
      }
    }
  '';

  # A random MAC address on every network and no host name in DHCP: the
  # network the PC is on learns nothing that would tell it apart from the
  # next time. Defaults, so a profile someone set otherwise keeps its own.
  identity = pkgs.writeText "90-losos-schizo.conf" ''
    [connection-losos-schizo]
    ethernet.cloned-mac-address=random
    wifi.cloned-mac-address=random
    ipv4.dhcp-send-hostname=false
    ipv6.dhcp-send-hostname=false
  '';
  nmConf = "/run/NetworkManager/conf.d/90-losos-schizo.conf";

  # The ad blocker's forwarder asks only these servers while the file is
  # there (losos-adblock's serve.rs). Without it, resolved asks the gateway
  # itself on its bridge, and its other links' servers are past the kill
  # switch.
  adblock = config.losos.adblock.enable;
  upstreams = "/run/losos-adblock/upstreams";

  # Port 53 on the gateway's address is redirected to Tor's DNSPort there.
  dnsOn =
    if adblock then
      ''
        mkdir -p "$(dirname ${upstreams})"
        echo "nameserver ${inside.gateway}" > ${upstreams}
        systemctl try-restart losos-adblock.service
      ''
    else
      ''
        resolvectl dns ${inside.bridge} ${inside.gateway}
        resolvectl domain ${inside.bridge} '~.'
        resolvectl default-route ${inside.bridge} yes
        resolvectl dnsovertls ${inside.bridge} no
      '';
  dnsOff =
    if adblock then
      ''
        rm -f ${upstreams}
        systemctl try-restart losos-adblock.service
      ''
    else
      ''
        resolvectl revert ${inside.bridge} || true
      '';

  rules = [
    "iif lo lookup main suppress_prefixlength 0"
    "iif lo lookup ${table}"
    "fwmark ${mark}/${mark} lookup main suppress_prefixlength 0"
    "fwmark ${mark}/${mark} lookup ${table}"
  ];
  # Fixed preferences, so stopping removes exactly these and starting twice
  # does not add them twice.
  rulesWith =
    verb:
    lib.concatImapStrings (i: rule: ''
      ip -4 rule ${verb} pref ${toString (15200 + i)} ${rule}${
        lib.optionalString (verb == "del") " || true"
      }
    '') rules;

  command = pkgs.writeShellApplication {
    name = "losos-schizo";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.systemd
      libvirt
    ];
    text = ''
      mode=off
      [ -e ${flag} ] && mode=on
      case ''${1-} in
        "")
          echo "$mode"
          ;;
        on | off)
          exec systemctl start "losos-schizo@$1.service"
          ;;
        status)
          echo "mode=$mode"
          state=$(virsh -r -c qemu:///system domstate ${name} 2>/dev/null | head -n1 || true)
          echo "gateway=''${state:-none}"
          # Tor's DNSPort answers once Tor has built a circuit, so a name it
          # resolves means Tor is through. Only asked while on: off, it
          # would be asked of the network directly.
          tor=off
          if [ "$mode" = on ]; then
            tor=connecting
            if timeout 10 resolvectl query --cache=no --legend=no torproject.org >/dev/null 2>&1; then
              tor=connected
            fi
          fi
          echo "tor=$tor"
          # Set: connections leave Tor through the edge (losos-edge-chain
          # in the gateway), and nowhere while it cannot be reached.
          edge=none
          [ -e ${edge.hostDir}/${edge.file} ] && edge=set
          echo "edge=$edge"
          ;;
        *)
          echo "usage: losos-schizo [on|off|status]" >&2
          exit 2
          ;;
      esac
    '';
  };

  # Every unit of schizo mode runs only while it is on, is stopped with the
  # target, and starts again with it.
  partOf = {
    unitConfig.ConditionPathExists = flag;
    partOf = [ "losos-schizo.target" ];
    wantedBy = [ "losos-schizo.target" ];
  };
in
{
  config = {
    environment.systemPackages = [ command ];

    # libvirt makes the two bridges; NetworkManager leaves them be, as it
    # does virbr0 (network.nix).
    networking.networkmanager.unmanaged = [
      "interface-name:${inside.bridge}"
      "interface-name:${uplink.bridge}"
    ];

    systemd.targets.losos-schizo = {
      description = "Schizo mode: every connection through Tor";
      wantedBy = [ "multi-user.target" ];
      unitConfig.ConditionPathExists = flag;
    };

    # Before the network: on a boot with schizo mode on, nothing leaves
    # between NetworkManager bringing a link up and the gateway starting.
    systemd.services.losos-schizo-kill-switch = lib.recursiveUpdate partOf {
      description = "Schizo mode's kill switch: nothing leaves except through Tor";
      unitConfig.DefaultDependencies = false;
      after = [
        "sysinit.target"
        "nftables.service"
      ];
      wants = [ "network-pre.target" ];
      before = [
        "network-pre.target"
        "shutdown.target"
      ];
      conflicts = [ "shutdown.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe pkgs.nftables} -f ${killSwitch}";
        ExecStop = "${lib.getExe pkgs.nftables} delete table inet losos-schizo";
      };
    };

    # Before NetworkManager reads its configuration, so the first link it
    # brings up already has a random address.
    systemd.services.losos-schizo-identity = lib.mkIf standalone (
      lib.recursiveUpdate partOf {
        description = "Schizo mode's random MAC addresses";
        before = [ "NetworkManager.service" ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${pkgs.coreutils}/bin/install -D -m 0644 ${identity} ${nmConf}";
          ExecStop = "${pkgs.coreutils}/bin/rm -f ${nmConf}";
        };
      }
    );

    systemd.services.losos-schizo-network = lib.recursiveUpdate partOf {
      description = "Schizo mode's route to the Tor gateway";
      requires = [
        "losos-schizo-kill-switch.service"
        "libvirt-defaults.service"
      ];
      after = [
        "losos-schizo-kill-switch.service"
        "libvirt-defaults.service"
      ];
      path = [
        libvirt
        pkgs.iproute2
        pkgs.systemd
        pkgs.coreutils
        pkgs.gnused
      ];
      environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      # Each network is defined from the image's definition every time, so
      # an update that changes it is taken.
      script = ''
        for net in ${network inside} ${network uplink}; do
          virsh -q net-define "$net" >/dev/null
        done
        for name in ${inside.bridge} ${uplink.bridge}; do
          [ "$(virsh -q net-info "$name" | sed -n 's/^Active: *//p')" = yes ] || virsh -q net-start "$name"
        done

      ''
      + lib.optionalString standalone ''
        ip -4 route replace default via ${inside.gateway} dev ${inside.bridge} table ${table}
        ${rulesWith "del"}
        ${rulesWith "add"}

        ${dnsOn}
      '';
      preStop =
        lib.optionalString standalone ''
          ${dnsOff}
          ${rulesWith "del"}
          ip -4 route flush table ${table} || true
        ''
        + ''
          virsh -q net-destroy ${inside.bridge} || true
          virsh -q net-destroy ${uplink.bridge} || true
        '';
    };

    losos.serviceMachines.${name} = {
      module = ../guest/tor-gateway.nix;
      title = "Schizo mode's Tor gateway";
      memory = 384;
      # Tor's guards and what it knows of the network: a few MiB.
      stateSize = "256M";
      # The edge to leave Tor through, which whoever chose it writes here
      # (docs/schizo-mode.md, "Through an edge"): read-only in the gateway,
      # and the one directory of the host's it sees besides the store.
      extraDevices = ''
        <filesystem type='mount' accessmode='squash'>
          <source dir='${edge.hostDir}'/>
          <target dir='${edge.share}'/>
          <readonly/>
        </filesystem>
      '';
      interfaces = [
        {
          network = uplink.bridge;
          mac = addresses.gateway.uplinkMac;
        }
        {
          network = inside.bridge;
          mac = addresses.gateway.insideMac;
        }
      ];
    };
    # Only root's: the file holds the edge's credentials.
    systemd.tmpfiles.rules = [ "d ${edge.hostDir} 0700 root root -" ];
    systemd.services."losos-service-machine-${name}" = lib.recursiveUpdate partOf {
      requires = [ "losos-schizo-network.service" ];
      after = [ "losos-schizo-network.service" ];
    };

    # `losos-schizo on|off`, which Settings runs through
    # losos-schizo@on.service, so polkit asks an administrator, as for any
    # unit started by hand. NetworkManager takes each link down and up again
    # afterwards, so each comes back with the MAC address the mode gives it.
    systemd.services."losos-schizo@" = {
      description = "Turn schizo mode %i";
      path = [
        pkgs.coreutils
        pkgs.systemd
        config.networking.networkmanager.package
      ];
      serviceConfig.Type = "oneshot";
      scriptArgs = "%i";
      script = ''
        case $1 in
          on)
            mkdir -p "$(dirname ${flag})"
            touch ${flag}
            systemctl start losos-schizo.target
            ;;
          off)
            rm -f ${flag}
            systemctl stop losos-schizo.target
            ;;
          *)
            echo "losos-schizo: '$1' is neither on nor off" >&2
            exit 2
            ;;
        esac
      ''
      + lib.optionalString standalone ''
        nmcli general reload conf
        nmcli networking off
        nmcli networking on
      '';
    };

  };
}
