# Machines on libvirt's default network, kept apart by their colour label
# (docs/hypervisor.md, "Labels on the network"). pm puts every machine on
# that one network (libvirt.nix), and libvirt's NAT alone lets each reach
# the host, the LAN and every other machine. Here
#
#  - a red machine, the untrusted one, gets the host's DHCP and DNS and the
#    internet, and not the host's other services, the LAN, LosOS boxes or
#    any other machine;
#  - any other machine reaches the machines with its own label and the
#    LosOS boxes machine, which is how it reaches the boxes
#    (docs/losos-boxes.md), and not the machines with other labels.
#
# losos-machine-labels does it with an nftables table on the bridge itself,
# since machines on one bridge reach each other without the host routing
# anything. libvirt runs it as a hook as each machine starts and stops, and
# it puts the machine's ports into its label's set from the label pm wrote
# into the machine's metadata. A port nothing has named yet is treated as
# red, so the table fails closed.
#
# A label changed while a machine runs applies from its next start: libvirt
# runs no hook for a change of metadata.
{ pkgs, ... }:

{
  # libvirt runs every file in hooks/qemu.d with the machine's name, the
  # operation and its definition on stdin.
  virtualisation.libvirtd.hooks.qemu.losos-machine-labels =
    pkgs.writeShellScript "losos-machine-labels-hook" ''
      exec ${pkgs.losos-machine-labels}/bin/losos-machine-labels hook "$@"
    '';

  # The table, before libvirtd starts a machine marked to start with the
  # host. Its own table rather than one of networking.nftables', which
  # reloading would empty under running machines; `systemctl reload` puts
  # back every running machine's ports.
  systemd.services.losos-machine-labels = {
    description = "Keep libvirt's machines apart by their label";
    wantedBy = [ "libvirtd.service" ];
    before = [ "libvirtd.service" ];
    after = [ "nftables.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
      ExecStart = "${pkgs.losos-machine-labels}/bin/losos-machine-labels load";
      ExecReload = "${pkgs.losos-machine-labels}/bin/losos-machine-labels sync";
      # No ExecStop: a table taken away would leave every machine unfiltered.
    };
  };
}
