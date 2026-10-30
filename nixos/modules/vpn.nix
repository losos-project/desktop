# sys-vpn (docs/vpn.md), the host's half: a disposable machine every
# connection of the PC goes through while the person has set a VPN, one
# provider's WireGuard tunnel (nixos/guest/vpn.nix). The network machine
# hands it the host's and the other machines' connections and routes its
# uplink (losos-netd's vpn.rs); in schizo mode it is the Tor gateway's
# uplink it hands over, so Tor runs inside the VPN.
#
#   losos-vpn providers         the providers it takes, with their owners
#   losos-vpn status            the provider set, and whether sys-vpn runs
#   losos-vpn set <provider>    the provider's WireGuard file on stdin
#   losos-vpn off               back to the network's own way out
#
# The choice is /var/lib/losos-vpn, root's alone, and every unit below
# runs only while it holds a tunnel file. The PC half only, with the
# network machine: it is what routes to sys-vpn.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  addresses = import ./vpn-addresses.nix;
  network = import ./network-addresses.nix;
  inherit (addresses) inside uplink share;
  libvirt = config.virtualisation.libvirtd.package;
  name = "sys-vpn";
  netName = "sys-net";
  stateDir = "/var/lib/losos-vpn";
  tunnel = "${stateDir}/tunnel.conf";
  # The network machine's share from the host (network-machine.nix): the
  # provider's servers go there, which tells it a VPN is set.
  netShare = "/run/losos-net";

  # Plain bridges: the network machine and sys-vpn hold every address on
  # them, and no dnsmasq, since sys-vpn's addresses are fixed.
  bridge =
    net:
    pkgs.writeText "${net.bridge}.xml" ''
      <network>
        <name>${net.bridge}</name>
        <bridge name='${net.bridge}' stp='off' delay='0'/>
        <dns enable='no'/>
      </network>
    '';

  # Every unit runs only while a VPN is set, stops with the target, and
  # starts again with it.
  partOf = {
    unitConfig.ConditionPathExists = tunnel;
    partOf = [ "losos-vpn.target" ];
    wantedBy = [ "losos-vpn.target" ];
  };
in
{
  config = lib.mkIf config.losos.machines.network.enable {
    environment.systemPackages = [ pkgs.losos-vpn ];

    systemd.tmpfiles.rules = [
      "d ${stateDir} 0700 root root -"
      # sys-vpn's copy, the one directory of the host's it sees besides
      # the store. Only root's: the file holds the tunnel's private key.
      "d ${share.hostDir} 0700 root root -"
    ];

    networking.networkmanager.unmanaged = [
      "interface-name:${inside.bridge}"
      "interface-name:${uplink.bridge}"
    ];

    systemd.targets.losos-vpn = {
      description = "A VPN: every connection through sys-vpn";
      wantedBy = [ "multi-user.target" ];
      unitConfig.ConditionPathExists = tunnel;
    };

    systemd.services.losos-vpn-network = lib.recursiveUpdate partOf {
      description = "sys-vpn's networks and its tunnel file";
      requires = [ "libvirt-defaults.service" ];
      after = [ "libvirt-defaults.service" ];
      path = [
        libvirt
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
        for net in ${bridge inside} ${bridge uplink}; do
          virsh -q net-define "$net" >/dev/null
        done
        for name in ${inside.bridge} ${uplink.bridge}; do
          [ "$(virsh -q net-info "$name" | sed -n 's/^Active: *//p')" = yes ] || virsh -q net-start "$name"
        done
        install -m 0600 ${tunnel} ${share.hostDir}/tunnel.conf
      '';
      preStop = ''
        rm -f ${share.hostDir}/tunnel.conf
        virsh -q net-destroy ${inside.bridge} || true
        virsh -q net-destroy ${uplink.bridge} || true
      '';
    };

    # The network machine joins both networks, as sys-vpn's router on its
    # uplink and the host's stand-in on its inside, and is told the
    # provider's servers, which turns its routing over to sys-vpn.
    systemd.services.losos-net-vpn = lib.recursiveUpdate partOf {
      description = "Route the network machine through sys-vpn";
      partOf = [
        "losos-vpn.target"
        "losos-service-machine-${netName}.service"
      ];
      requires = [
        "losos-vpn-network.service"
        "losos-service-machine-${netName}.service"
      ];
      after = [
        "losos-vpn-network.service"
        "losos-service-machine-${netName}.service"
        "losos-vm-call-relay.service"
      ];
      path = [
        libvirt
        pkgs.losos-vm-call
        pkgs.coreutils
        pkgs.gnused
        pkgs.gnugrep
      ];
      environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        attach() {
          virsh -q domiflist ${netName} | grep -qi "$2" ||
            virsh -q attach-interface ${netName} network "$1" --mac "$2" --model virtio --live
        }
        attach ${inside.bridge} ${network.vpn.mac}
        attach ${uplink.bridge} ${network.vpnext.mac}
        # The DNS line of the tunnel file losos-vpn wrote, one server a line;
        # an empty file still says a VPN is set.
        sed -n 's/^DNS = //p' ${tunnel} | tr ',' '\n' | sed 's/^ */nameserver /' > ${netShare}/vpn.new
        chmod 0644 ${netShare}/vpn.new
        mv ${netShare}/vpn.new ${netShare}/vpn
        # Within seconds by itself; at once if the machine answers.
        losos-vm-call call net-apply || true
      '';
      preStop = ''
        rm -f ${netShare}/vpn
        losos-vm-call call net-apply || true
        virsh -q detach-interface ${netName} network --mac ${network.vpn.mac} --live || true
        virsh -q detach-interface ${netName} network --mac ${network.vpnext.mac} --live || true
      '';
    };

    losos.serviceMachines.${name} = {
      module = ../guest/vpn.nix;
      title = "The VPN's machine";
      memory = 192;
      extraDevices = ''
        <filesystem type='mount' accessmode='squash'>
          <source dir='${share.hostDir}'/>
          <target dir='${share.tag}'/>
          <readonly/>
        </filesystem>
      '';
      interfaces = [
        {
          network = uplink.bridge;
          mac = addresses.machine.uplinkMac;
        }
        {
          network = inside.bridge;
          mac = addresses.machine.insideMac;
        }
      ];
    };
    systemd.services."losos-service-machine-${name}" = lib.recursiveUpdate partOf {
      requires = [ "losos-net-vpn.service" ];
      after = [ "losos-net-vpn.service" ];
    };
  };
}
