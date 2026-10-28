# LosOS boxes, the home servers of the LosOS project, from this desktop
# (docs/losos-boxes.md): `losos`, the boxes' own terminal client, which
# manages one with the panes of its admin pages and needs no SSH, and
# losos-boxes, which finds the boxes on the local network and offers each
# one in the palette and the app list.
#
# On a PC both run in the LosOS boxes machine (nixos/guest/boxes.nix),
# which this module adds to machines.nix's templates, and not on the host: the
# machine is the one place that talks to boxes, and the host only lends it
# its resolved, which does the mDNS a machine behind libvirt's NAT cannot.
# A phone has no machines (the GSI does not import libvirt.nix), so there
# the session runs them itself.
#
# The host makes the machine, `boxes`, at the first boot, and starts it the
# first time a box answers on one of its networks (`losos-boxes wake`).
# It keeps the machine's keyring password, which the machine asks the
# relay for at every boot, so nobody is asked for one.
{
  config,
  options,
  lib,
  pkgs,
  ...
}:

let
  machines = config.virtualisation.libvirtd.enable;
  # The LosOS boxes machine the host makes, and the name it has on the
  # desktop: "mattbox in boxes".
  boxesMachine = "boxes";
in
{
  config = lib.mkMerge [
    (lib.mkIf (!machines) {
      environment.systemPackages = [
        pkgs.losos
        # `losos-boxes list` prints what the service sees, and the entries'
        # terminal action runs `losos-boxes terminal`.
        pkgs.losos-boxes
      ];

      # One per session, since the entries are the signed-in person's: it
      # browses through resolved, which answers mDNS on this OS (network.nix),
      # so no Avahi. The entries go when the session does, so a box that left
      # while nobody watched is not offered at the next sign-in.
      systemd.user.services.losos-boxes = {
        description = "Offer the LosOS boxes on this network as applications";
        # The login screen's own user, and any other system account that gets
        # a user manager, is not a person.
        unitConfig.ConditionUser = "!@system";
        wantedBy = [ "graphical-session.target" ];
        partOf = [ "graphical-session.target" ];
        after = [ "graphical-session.target" ];
        serviceConfig = {
          ExecStart = "${lib.getExe pkgs.losos-boxes} watch";
          ExecStopPost = "${lib.getExe pkgs.losos-boxes} clear";
          # resolved restarting ends a browse; the next run starts over.
          Restart = "on-failure";
          RestartSec = "10s";
        };
      };
    })

    # The template, wherever machines.nix builds templates. It is looked
    # for among the options rather than behind `machines`, since a system
    # without machines.nix has no such option to set.
    (lib.optionalAttrs (options.losos ? machines) {
      losos.machines.guests.losos-boxes = ../guest/boxes.nix;

      # Made once, as machines.nix makes every machine the system needs; one
      # its owner removed in the Machines app is not made again.
      losos.machines.system = lib.mkIf machines {
        ${boxesMachine} = {
          template = "losos-boxes";
          label = "green";
          # It browses, routes and runs a terminal; Danube at most.
          memory = 1536;
        };
      };
    })

    (lib.mkIf machines {
      # The LosOS boxes machine's way to this host's resolved, and to the
      # other machines (`losos-boxes relay`), for a machine libvirt has as
      # made from the losos-boxes template; any other machine is refused.
      # On vsock port 7351 it is let through to resolved's Varlink socket:
      # lookups only, which change nothing on the host. On port 7352 it
      # names the boxes it found, and the relay writes those names at its
      # address into a hosts file of HOSTS_DIR, below, so the other machines'
      # connections to a box go to it. On port 7353 it is handed its
      # keyring's password, made the first time and kept in the relay's state
      # directory, which only the relay's user reads.
      systemd.services.losos-boxes-relay = {
        description = "Lend the LosOS boxes machine this host's resolved";
        wantedBy = [ "multi-user.target" ];
        after = [
          "libvirtd.service"
          "systemd-resolved.service"
        ];
        # `virsh -r` asks libvirt which machine is at a connection's address,
        # and what address its network gave it.
        path = [ config.virtualisation.libvirtd.package ];
        environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
        serviceConfig = {
          ExecStart = "${lib.getExe pkgs.losos-boxes} relay";
          # libvirt's read-only socket and resolved's Varlink socket are both
          # open to every account, and a vsock port above 1023 needs nothing.
          # A user of its own rather than a dynamic one, as it owns the
          # hosts files, which outlive a restart.
          User = "losos-boxes";
          Group = "losos-boxes";
          Restart = "on-failure";
          ProtectSystem = "strict";
          ReadWritePaths = [ "/run/losos-boxes" ];
          StateDirectory = "losos-boxes";
          StateDirectoryMode = "0700";
          UMask = "0077";
          ProtectHome = true;
          PrivateTmp = true;
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_VSOCK"
          ];
        };
      };

      # Starts the LosOS boxes machine the first time a box answers on one of
      # the host's networks, browsing through the host's resolved as the
      # machine does through the relay, and ends there: a machine its owner
      # stops stays stopped until the next boot. Root's, as starting a
      # machine takes libvirt's read-write socket; it reads nothing of
      # anyone's.
      systemd.services.losos-boxes-wake = {
        description = "Start the LosOS boxes machine when a box shows up";
        wantedBy = [ "multi-user.target" ];
        after = [
          "losos-system-machines.service"
          "libvirtd.service"
          "systemd-resolved.service"
        ];
        path = [ config.virtualisation.libvirtd.package ];
        environment = {
          LIBVIRT_DEFAULT_URI = "qemu:///system";
          # virsh keeps its own files under HOME, which is not root's here.
          HOME = "/run/losos-boxes-wake";
        };
        serviceConfig = {
          ExecStart = "${lib.getExe pkgs.losos-boxes} wake ${boxesMachine}";
          # resolved restarting ends a browse; the next run starts over.
          Restart = "on-failure";
          RestartSec = "10s";
          RuntimeDirectory = "losos-boxes-wake";
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          # libvirt's and resolved's sockets, and a box from before
          # `_losos._tcp`, which is asked over HTTP whether it is one.
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_INET"
            "AF_INET6"
          ];
        };
      };

      users.users.losos-boxes = {
        isSystemUser = true;
        group = "losos-boxes";
      };
      users.groups.losos-boxes = { };
      # Made at boot, before libvirt's dnsmasq starts and watches it, and
      # kept while the relay restarts.
      systemd.tmpfiles.rules = [
        "d /run/losos-boxes 0755 losos-boxes losos-boxes -"
        "d /run/losos-boxes/hosts 0755 losos-boxes losos-boxes -"
      ];

      # libvirt's dnsmasq answers the machines' DNS on virbr0. hostsdir has
      # it read every hosts file in /run/losos-boxes/hosts and read each
      # again whenever it changes, so a box's name answered with the LosOS
      # boxes machine's address is answered before dnsmasq asks the host's
      # resolved for it (docs/hypervisor.md, "Names on the network"). An
      # option of libvirt's dnsmasq namespace on the default network,
      # added once, before any machine starts at boot; the network restarts
      # for it only while no machine runs on it.
      systemd.services.losos-boxes-dns = {
        description = "Have libvirt's DNS for machines read the LosOS boxes' names";
        wantedBy = [ "multi-user.target" ];
        requires = [ "libvirt-defaults.service" ];
        after = [ "libvirt-defaults.service" ];
        before = [ "libvirt-guests.service" ];
        path = [
          config.virtualisation.libvirtd.package
          pkgs.gnused
          pkgs.gnugrep
        ];
        environment.LIBVIRT_DEFAULT_URI = "qemu:///system";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
        };
        script = ''
          xml=$(virsh net-dumpxml --inactive default)
          if printf '%s' "$xml" | grep -q 'hostsdir=/run/losos-boxes/hosts'; then
            exit 0
          fi
          ns=http://libvirt.org/schemas/network/dnsmasq/1.0
          printf '%s\n' "$xml" |
            sed -e "0,/<network/s|<network|<network xmlns:dnsmasq='$ns'|" \
              -e "s|</network>|<dnsmasq:options><dnsmasq:option value='hostsdir=/run/losos-boxes/hosts'/></dnsmasq:options></network>|" \
              >/run/losos-boxes/network.xml
          virsh -q net-define /run/losos-boxes/network.xml
          rm /run/losos-boxes/network.xml
          if [ "$(virsh -q net-info default | sed -n 's/^Active: *//p')" = yes ]; then
            if [ -z "$(virsh -q list --name)" ]; then
              virsh -q net-destroy default
              virsh -q net-start default
            else
              echo "Machines are running: the default network reads the names from its next start."
            fi
          fi
        '';
      };
    })
  ];
}
