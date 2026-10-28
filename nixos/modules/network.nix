# The network: NetworkManager, as NixOS sets it up, with resolved answering
# DNS (and mDNS, so no avahi) and timesyncd keeping the clock.
#
# NetworkManager runs DHCP on every wired port, joins Wi-Fi through
# wpa_supplicant over D-Bus, and hands each link's DNS servers and search
# domains to resolved, which NixOS's module picks on its own when resolved is
# on. derisk's Network pages and first-boot setup talk to it through nmcli
# (derisk-install's network.rs), and Danube watches it for captive portals.
#
# networkd ran the links before, and is not coming back: it had no Wi-Fi of
# its own, so derisk spoke wpa_supplicant's control socket and read networkd's
# state files, two daemons for what NetworkManager does alone, and every app
# that asks whether it is online (GLib, the portal, Flatpaks) asks
# NetworkManager.
{ ... }:

{
  networking = {
    networkmanager = {
      enable = true;
      # libvirt makes virbr0 and a vnet port per machine, and runs DHCP and
      # DNS on them itself (libvirt.nix); NetworkManager leaves them be.
      unmanaged = [
        "interface-name:virbr*"
        "interface-name:vnet*"
      ];
      # With resolved, NetworkManager leaves a link's mDNS at resolved's
      # per-link default, which is off; LosOS boxes are found over mDNS
      # (losos-boxes.nix), so every connection turns it on. 2 is "yes".
      # LLMNR's per-link default is already on.
      settings.connection."connection.mdns" = 2;
    };

    # systemd has no packet filter, so this is the one piece of networking that
    # is not NetworkManager's or systemd's. nftables rather than the iptables
    # default, because libvirt's own rules are nftables here too.
    nftables.enable = true;

    firewall = {
      enable = true;
      # mDNS and LLMNR are answered by resolved, so it has to be reachable.
      allowedUDPPorts = [
        5353
        5355
      ];
    };
  };

  # The networkmanager group is how NixOS's polkit rule lets an account
  # change networks, and first-boot setup and the Users page put every
  # account they make in it. An account made any other way (homectl, or setup from before
  # it did so) was not, and its Network page could list networks but not
  # join one until someone ran `homectl update --member-of=` for it, which
  # an inactive LUKS home asks its password for. Every account the system
  # makes gets the group, so asking for it adds no rights: the person
  # sitting at the machine may change its networks, as on any desktop.
  # A remote session (ssh) or one in the background is still not let in.
  security.polkit.extraConfig = ''
    polkit.addRule(function (action, subject) {
      if (action.id.indexOf("org.freedesktop.NetworkManager.") === 0
          && subject.local && subject.active) {
        return polkit.Result.YES;
      }
      return polkit.Result.NOT_HANDLED;
    });
  '';

  # resolved answers mDNS and LLMNR for the whole machine, on the links
  # NetworkManager turns them on for above. DNS over TLS where the network's
  # server offers it; adblock.nix turns it off for its forwarder.
  services.resolved = {
    enable = true;
    settings.Resolve = {
      DNSOverTLS = "opportunistic";
      MulticastDNS = true;
      LLMNR = true;
    };
  };

  # timesyncd is NixOS's default already and is left as the default, not
  # restated: a VM sets it off at the same priority to use the host's clock.
  # avahi stays off (NixOS's default): resolved answers mDNS itself, and
  # avahi would be a second responder on 5353.
}
