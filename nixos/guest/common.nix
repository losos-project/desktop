# What every machine template the image ships has in common
# (docs/hypervisor.md, "The guest template"): default.nix, the LosOS guest,
# and boxes.nix, the LosOS boxes machine, import this and add their apps.
# machines.nix in the OS evaluates each one in losos.machines.guests with
# the image's own package set, sets `losos.guest.name` to its name there,
# and turns `system.build.image` into share/losos/machines/<name>.qcow2, which
# derisk's Machines app offers when a machine is made and `pm machine
# create` installs into libvirt.
#
# A guest has no screen and no session. Its apps' windows go to the host's
# desktop through one waypipe server, which the guest's user runs from boot
# and which carries each app's Wayland connection to the host over vsock; the
# host knows which machine a window is from by the vsock address libvirt
# gave it. The host starts apps here through the QEMU guest agent and
# `losos-guest-run`, and nothing else in the guest listens for the host.
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  # The one account in the guest, whose apps the desktop shows. Nobody logs
  # in as it: it has no password, and its systemd manager runs from boot
  # (linger), which is where the waypipe server and every app run.
  user = "user";

  efiArch = pkgs.stdenv.hostPlatform.efiArch;

  guestRun = pkgs.writeShellApplication {
    name = "losos-guest-run";
    runtimeInputs = with pkgs; [
      coreutils
      gawk
      gnused
      systemd
    ];
    text = builtins.replaceStrings [ "@user@" "@display@" ] [ user "wayland-0" ] (
      builtins.readFile ./losos-guest-run.sh
    );
  };

  inherit (config.losos.guest) name;
in
{
  imports = [
    "${modulesPath}/profiles/qemu-guest.nix"
    "${modulesPath}/image/repart.nix"
    # The same browser as the host, which opens a link in any machine.
    ../modules/danube.nix
    # The folders apps expect in the user's home, made as on the host.
    ../modules/home-folders.nix
    # uutils, sudo-rs's su and Sequoia's gpg, as on the host.
    ../modules/userland.nix
    # The host's security keys and smart card readers.
    ./tokens.nix
    # The Slovak eID klient, downloaded on its first start.
    ./eid.nix
  ];

  options.losos.guest.name = lib.mkOption {
    type = lib.types.str;
    description = ''
      The template's name: the qcow2's file name under
      share/losos/machines, which pm records as the machine's template, and
      the guest's host name and root partition label.
    '';
  };

  config = {
    system.stateVersion = "26.05";
    networking.hostName = name;

    # Nothing in the guest builds or switches: it is a template, and a machine
    # made from it keeps what its owner changes on its own disk.
    nix.enable = false;
    documentation.enable = false;

    # The firmware boots the UKI straight from the ESP's fallback path. No boot
    # loader, because a machine never chooses between generations: a new
    # template makes new machines.
    boot.loader.grub.enable = false;
    fileSystems."/" = {
      device = "/dev/disk/by-partlabel/${name}";
      fsType = "ext4";
      # ext4 grows while mounted, once repart has made the partition larger.
      autoResize = true;
    };

    # A machine's disk is larger than the template that is its base, and the
    # root partition takes the rest of it the first time the machine boots.
    # The disk is always vda: pm's libvirt plugin gives a machine one virtio
    # disk and nothing else. Partitions repart has no definition for, the
    # ESP, are left as they are.
    boot.initrd.systemd.repart = {
      enable = true;
      device = "/dev/vda";
    };
    systemd.repart.partitions.root.Type = "root";

    image.repart = {
      enable = true;
      inherit name;
      partitions = {
        esp = {
          contents."/EFI/BOOT/BOOT${lib.toUpper efiArch}.EFI".source =
            "${config.system.build.uki}/${config.system.boot.loader.ukiFile}";
          repartConfig = {
            Type = "esp";
            Format = "vfat";
            SizeMinBytes = "128M";
          };
        };
        root = {
          storePaths = [ config.system.build.toplevel ];
          repartConfig = {
            Type = "root";
            Format = "ext4";
            Label = name;
            # As small as the store allows: a machine's disk is as large as
            # its owner asks (`pm machine create --disk`, 32 GiB unless told
            # otherwise), and the first boot grows the root into it, below.
            Minimize = "guess";
          };
        };
      };
    };

    services.qemuGuest.enable = true;

    # NetworkManager with NixOS's defaults, as on the host (network.nix): it
    # takes the address and the host's resolver from libvirt's default
    # network by DHCP and hands DNS to resolved. Apps a person installs here
    # ask it whether they are online, through GLib or the portal.
    networking.networkmanager.enable = true;
    services.resolved.enable = true;
    # A machine has one virtio network card and no Wi-Fi or modem, which the
    # NetworkManager module would otherwise bring wpa_supplicant and
    # ModemManager for.
    networking.wireless.enable = lib.mkForce false;
    networking.modemmanager.enable = false;

    # .local names are the host's to look up (docs/hypervisor.md, "Names on
    # the network"). libvirt's NAT carries no multicast between the machine
    # and the host's own networks, so an mDNS query asked here would reach
    # only the other machines on virbr0 and never a box on the host's LAN.
    # Asked of the host's resolver instead, libvirt's dnsmasq hands it to
    # resolved on the host, which asks every network the host is on. resolved
    # sends .local to a unicast server only on a link that names it as a
    # routing domain, hence ~local in the card's NetworkManager profile; and
    # with mDNS and LLMNR off there and in resolved, the machine neither
    # answers nor announces on virbr0, so the host's answer is the only one.
    services.resolved.settings.Resolve = {
      MulticastDNS = false;
      LLMNR = false;
    };
    networking.networkmanager.ensureProfiles.profiles.libvirt = {
      connection = {
        id = "libvirt";
        type = "ethernet";
        mdns = 0;
        llmnr = 0;
      };
      ipv4 = {
        method = "auto";
        dns-search = "~local";
      };
      ipv6.method = "auto";
    };
    # That profile is the card's, so NetworkManager makes no "Wired
    # connection 1" of its own beside it.
    networking.networkmanager.settings.main.no-auto-default = "*";

    users.users.${user} = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };

    # The guest's display: a waypipe server whose socket, wayland-0 in the
    # user's runtime directory, every app connects to as if it were a
    # compositor. Each connection becomes a vsock stream to the host (address
    # 2, port 7350), where the desktop's gate takes it. --no-gpu, because the
    # guest has no GPU and its apps draw into shared memory (llvmpipe and
    # lavapipe below), which is what waypipe then carries.
    systemd.user.services.waypipe = {
      description = "Show this machine's apps on the host's desktop";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        ExecStart = lib.escapeShellArgs [
          (lib.getExe pkgs.waypipe)
          "--vsock"
          "--socket"
          "2:7350"
          "--no-gpu"
          "--display"
          "wayland-0"
          "server"
          "--"
          "${pkgs.coreutils}/bin/sleep"
          "infinity"
        ];
        Restart = "always";
      };
    };
    # X11 for the apps built only for it, such as the eID klient (eid.nix):
    # xwayland-satellite is an X server that makes each X11 window a
    # Wayland one, which waypipe then carries like any other.
    systemd.user.services.xwayland-satellite = {
      description = "Show this machine's X11 apps through its waypipe";
      wantedBy = [ "default.target" ];
      after = [ "waypipe.service" ];
      bindsTo = [ "waypipe.service" ];
      serviceConfig = {
        ExecStart = "${lib.getExe pkgs.xwayland-satellite} :0";
        Restart = "always";
        RestartSec = 1;
      };
    };
    # Every unit of the user's manager draws on those sockets: the apps
    # losos-guest-run starts and the portal's dialogs alike.
    systemd.user.settings.Manager.DefaultEnvironment =
      "WAYLAND_DISPLAY=wayland-0 DISPLAY=:0 XDG_SESSION_TYPE=wayland";

    # Mesa's software renderers, OpenGL and Vulkan: the guest has no GPU, and
    # derisk's GPUI apps need Vulkan.
    hardware.graphics.enable = true;

    # A portal for the apps' dialogs (open a file, pick an app), which GTK's
    # draws through the guest's waypipe like any other window.
    xdg.portal = {
      enable = true;
      extraPortals = [ pkgs.xdg-desktop-portal-gtk ];
      config.common.default = "gtk";
    };

    # A captive portal is the host's to deal with: the guest is behind
    # libvirt's NAT.
    losos.captivePortal.enable = false;

    # Hack Nerd Font for derisk-terminal, and Inter as sans-serif, so a
    # guest's windows are lettered like the host's (modules/desktop.nix).
    fonts.enableDefaultPackages = true;
    fonts.packages = [
      pkgs.inter
      pkgs.nerd-fonts.hack
    ];
    fonts.fontconfig.defaultFonts.sansSerif = [ "Inter" ];

    environment.systemPackages = [
      guestRun
      # The host's terminal, without the rest of derisk. With no GPU in the
      # machine it draws in software into the shared memory waypipe carries.
      pkgs.derisk.terminal
      pkgs.adwaita-icon-theme
    ];
  };
}
