# The installer's live system: a kernel, systemd, NetworkManager and
# `derisk installer` as the session on tty1. installer.nix in the OS evaluates
# this and passes in, as `losos`, what to install and from where; iso.nix
# turns it into the ISO.
#
# derisk draws the pages and joins networks; `losos-installer serve`, its
# child, offers the disks and runs repart and sysupdate. Nothing else of the
# desktop is here: no display manager, no apps, no user. NetworkManager runs
# DHCP on every wired port that has a cable, and derisk joins Wi-Fi through
# it with nmcli, as first-boot setup does on the installed system
# (modules/network.nix).
{
  config,
  lib,
  pkgs,
  modulesPath,
  utils,
  losos,
  ...
}:

let
  systemd = config.systemd.package;

  # Where a release disk is mounted, when one is attached.
  releaseDisk = "/run/losos/release";

  # systemd-sysupdate is a libexec program, not one on PATH.
  sysupdateBin = pkgs.linkFarm "systemd-sysupdate-bin" [
    {
      name = "bin/systemd-sysupdate";
      path = "${systemd}/lib/systemd/systemd-sysupdate";
    }
  ];

  # The backend, with what it runs on PATH. This was the service's `path`,
  # but derisk starts in the session PAMName= opens, and pam_env sets that
  # session's PATH to the system profile's: neither gpg nor mcopy is in the
  # profile, and a release-disk install stopped at "gpg: No such file or
  # directory". So the backend puts them back itself, and everything it runs
  # inherits them.
  backend = pkgs.writeShellScript "losos-installer-serve" ''
    export PATH=${
      lib.makeBinPath [
        systemd
        sysupdateBin
        pkgs.coreutils
        pkgs.util-linux
        # repart formats the ESP and copies GRUB into it with these.
        pkgs.dosfstools
        pkgs.mtools
        # systemd-pull checks SHA256SUMS.gpg with gpg itself, and the
        # installer does the same for a release disk.
        pkgs.gnupg
        # An online install's files, fetched over one connection per
        # hardware thread into a scratch partition on the target disk,
        # which repart formats with mkfs.ext4 (src/losos-installer's
        # stage.rs).
        pkgs.aria2
        # Pull the release OCI artifact directly from GHCR, without a proxy.
        pkgs.oras
        # Unpack the XZ update archive directly on the target disk.
        pkgs.xz
        pkgs.gnutar
        pkgs.bash
        pkgs.e2fsprogs
      ]
    }:$PATH
    exec ${lib.getExe pkgs.losos-installer} "$@"
  '';

  # Papirus without what nixpkgs propagates with it: breeze-icons, which
  # Papirus inherits from for KDE's icon names, and through it Qt. Neither
  # draws a single icon derisk asks for, and Qt alone would double the ISO.
  papirus = pkgs.papirus-icon-theme.overrideAttrs {
    propagatedBuildInputs = [ pkgs.hicolor-icon-theme ];
  };
in
{
  imports = [
    "${modulesPath}/profiles/minimal.nix"
    ./iso.nix
    # The installed system's boot screen, which reads "LosOS Desktop
    # installer is starting" here.
    ../modules/splash.nix
    # update-source.nix was imported here, so an install from the channel
    # read it through the same socket on 127.0.0.1 as an update does. That
    # socket streams each file through curl's one connection; the backend
    # now reads the channel's index itself (`--index` below) and fetches
    # the files with aria2 over many, so the socket would serve no one.
  ];

  system.stateVersion = "26.05";

  system.image = {
    id = "losos-installer";
    inherit (losos) version;
  };
  system.nixos = {
    distroId = "losos-installer";
    distroName = "LosOS Desktop installer";
  };

  boot = {
    # GRUB on the medium starts the UKI (iso.nix), with its menu built into
    # its own image; NixOS's GRUB module, which installs one on a disk and
    # writes its menu there, has no disk here to do that on.
    loader.grub.enable = false;
    # How quiet the boot is, so nothing writes over the splash before the
    # installer takes the screen, is splash.nix's, as on the installed
    # system.
  };

  # Every storage controller and USB host in the initrd, so the medium is found
  # wherever it is plugged in, and every network driver.
  hardware.enableAllHardware = true;

  # all-hardware.nix also turns on every redistributable firmware package, and
  # most of their bytes are audio DSP and other firmware an installer never
  # loads. linux-firmware alone holds what is needed: Wi-Fi, and the GPU
  # firmware without which amdgpu or nouveau would take the console over from
  # the EFI framebuffer and then fail to drive it.
  hardware.enableRedistributableFirmware = lib.mkForce false;
  hardware.firmware = [ pkgs.linux-firmware ];

  # Mesa, for derisk's GLES renderer and the GBM buffers it scans out, as on
  # the installed system (desktop.nix).
  hardware.graphics.enable = true;

  # The same choices as the OS (boot.nix): no Nix on the system, /etc as an
  # overlay, users from systemd-sysusers. Here they also keep perl out of a
  # system that never changes after it boots.
  nix.enable = false;
  system.switch.enable = false;
  system.etc.overlay = {
    enable = true;
    mutable = true;
  };
  systemd.sysusers.enable = true;
  users.mutableUsers = false;

  # tty2 and onwards log root in without a password, for nmcli and anything
  # else the installer does not cover. The machine is in front of whoever booted it,
  # and the installer that already runs on tty1 can erase any disk.
  services.getty.autologinUser = "root";
  users.allowNoPasswordLogin = true;
  systemd.services."getty@tty1".enable = false;
  systemd.services."autovt@tty1".enable = false;

  networking = {
    hostName = "losos-installer";
    # NixOS's defaults: a cable that is plugged in, at boot or later, gets an
    # address with nothing asked, and the installer goes straight on to the
    # disks. With a cable and Wi-Fi both up, the cable's lower route metric
    # wins, so the image comes down the cable.
    networkmanager.enable = true;

    # Nothing listens: no SSH, and resolved is told below not to answer on
    # the local network. A packet filter would guard no port.
    firewall.enable = false;
  };

  # Nothing here waits for the network: the installer watches for it, and a
  # machine with neither a cable nor Wi-Fi in range must still reach it.
  systemd.services.NetworkManager-wait-online.enable = false;

  # NixOS's default, made explicit: NetworkManager hands each link's DNS to
  # resolved, as on the installed system.
  services.resolved.enable = true;
  services.resolved.settings.Resolve = {
    LLMNR = false;
    MulticastDNS = false;
  };

  # What the backend reaches GitHub's API and the channel's index with
  # (src/losos-installer/src/github.rs); aria2 fetches the files themselves.
  # In the system profile rather than the service's `path`: the service
  # opens a PAM session, and pam_env sets PATH back to the profile's, so
  # `path` never reaches the backend. NixOS
  # lists curl by default; naming it keeps the installer working if that
  # default goes.
  environment.systemPackages = [ pkgs.curl ];

  # The release signing key: sysupdate verifies SHA256SUMS.gpg here exactly
  # as it does on the installed system, under the name systemd 261's
  # systemd-pull reads (modules/update.nix).
  environment.etc."systemd/import-pubring.pgp".source = losos.pubring;

  # A release disk: any filesystem labelled LOSOS-RELEASE holding a release's
  # files (SHA256SUMS, SHA256SUMS.gpg and what they list), which the installer
  # then installs from instead of the channel. It is for a machine that cannot
  # reach the channel, such as a VM behind a network that re-signs TLS with
  # its own authority. udev starts the mount when such a disk appears, at boot
  # or plugged in later, so a boot without one waits for nothing. A boot-time
  # mount with a device timeout gave up before udev had named a disk that was
  # there all along, and x-systemd.wanted-by= in fstab is not acted on for a
  # device unit.
  systemd.mounts = [
    {
      what = "/dev/disk/by-label/LOSOS-RELEASE";
      where = releaseDisk;
      type = "auto";
      options = "ro";
    }
  ];
  services.udev.extraRules = ''
    SUBSYSTEM=="block", ENV{ID_FS_LABEL}=="LOSOS-RELEASE", ENV{SYSTEMD_WANTS}+="${utils.escapeSystemdPath releaseDisk}.mount"
  '';

  # The installer is a logind session on tty1, as the login screen is on the
  # installed system, so derisk can take the display and input devices
  # through libseat. It runs as root: its backend partitions disks.
  #
  # This was `losos-installer` alone, a terminal interface on the same tty.
  # derisk draws the pages now, the same ones first-boot setup is made of,
  # so the installer works with a mouse, a touchscreen and a phone-sized
  # screen, and losos-installer is only the backend it starts.
  systemd.defaultUnit = "graphical.target";
  systemd.services.derisk-installer = {
    description = "LosOS Desktop installer";
    wantedBy = [ "graphical.target" ];
    # After Plymouth has let go of the display, as the login screen is on
    # the installed system (desktop.nix).
    after = [
      "systemd-user-sessions.service"
      "systemd-logind.service"
      "systemd-vconsole-setup.service"
      "plymouth-quit.service"
    ];
    conflicts = [ "getty@tty1.service" ];

    # No `path`: PAMName= below opens a session whose pam_env replaces PATH
    # with the system profile's, so nothing listed here reached the backend
    # (`backend` above carries it instead).

    environment = {
      # What pam_systemd records as the session's type.
      XDG_SESSION_TYPE = "wayland";
      # The icon theme derisk's built-in themes name, for the keyboard
      # toggle and the page icons. Named directly: minimal.nix leaves icons
      # out of the system profile, and a service has no profile anyway.
      XDG_DATA_DIRS = "${papirus}/share";
    }
    # Where an error alert's section is published. The alert prints the
    # address for reading on another device: the live system has no browser
    # (docs.nix).
    // lib.optionalAttrs (losos.docsUrl != null) {
      MCSAPI_DOCS_URL = losos.docsUrl;
    };

    serviceConfig = {
      ExecStart =
        lib.escapeShellArgs [
          (lib.getExe pkgs.derisk)
          "installer"
          "--"
          backend
          "serve"
          "--name"
          "LosOS Desktop"
          "--repart-definitions"
          losos.repartDefinitions
          "--sysupdate-definitions"
          losos.sysupdateTemplates
          "--esp"
          losos.esp
          "--medium"
          "/iso"
          "--work"
          "/run/losos-installer"
          "--source"
          losos.baseUrl
          "--local-source"
          releaseDisk
        ]
        # Which GitHub release assets make up each of the channel's files,
        # when the OS updates through that index (update-source.nix), so an
        # install from the channel fetches them itself.
        +
          lib.optionalString (losos.updateIndex != null)
            " ${lib.escapeShellArgs [
               "--index"
               losos.updateIndex
             ]}"
        # The GitHub release of the version and cadence this ISO was built
        # as, which an online install reads instead of the channel's newest
        # when it can find it (docs/installer.md).
        +
          lib.optionalString (losos.releaseRepo != null)
            " ${lib.escapeShellArgs [
               "--release-repo"
               losos.releaseRepo
               "--release-version"
               losos.version
               "--release-channel"
               losos.channel
             ]}"
        # A failed install can be reported as an issue, after the person signs
        # in to GitHub with this App's device flow (docs/installer.md).
        +
          lib.optionalString (losos.reportClientId != null && losos.reportRepo != null)
            " ${lib.escapeShellArgs [
               "--report-client-id"
               losos.reportClientId
               "--report-repo"
               losos.reportRepo
             ]}"
        # GRUB into the installed disk's MBR and BIOS boot partition, so a
        # legacy BIOS PC starts it as UEFI firmware does (bios.nix).
        +
          lib.optionalString (losos.biosBoot != null)
            " ${lib.escapeShellArgs [
               "--bios-boot"
               losos.biosBoot
             ]}";
      PAMName = "derisk-installer";
      User = "root";
      TTYPath = "/dev/tty1";
      TTYReset = true;
      TTYVHangup = true;
      TTYVTDisallocate = true;
      UtmpIdentifier = "tty1";
      UtmpMode = "user";
      StandardInput = "tty-fail";
      # derisk and the tools its backend runs log to the journal; the log
      # pane on the last page shows the tools' output from the backend.
      StandardOutput = "journal";
      StandardError = "journal";
      # A crash is still a crash; the next person to look at the screen
      # should find the installer there.
      Restart = "always";
      RestartSec = 1;
    };
  };

  security.pam.services.derisk-installer.startSession = true;
}
