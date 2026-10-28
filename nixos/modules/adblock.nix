# System-wide ad and tracker blocking from EasyList-style filter lists
# (nixos/pkgs/losos-adblock.nix, docs/adblock.md).
#
# Two halves from the same lists. Every program's DNS goes through
# losos-adblock's forwarder, which answers 0.0.0.0 (or ::) for a blocked
# domain and passes everything else on: resolved sends every query to it,
# because NetworkManager hands resolved a dummy link with the forwarder as
# its server for every domain, ahead of every network's own. That covers
# every app, Flatpaks and the Android layer included,
# but only whole domains. The rules that need a URL or a page (a path, a
# hidden element) are compiled into WebKit content blockers, which Danube
# loads into every page.
#
# The forwarder is also where DNS leaves the machine, so it is where it is
# encrypted: with encrypted DNS on, as it is unless someone turns it off,
# it asks the servers in `encryptedDns.servers` over HTTPS or TLS instead
# of the network's in plain DNS, and fails a query rather than fall back
# (docs/adblock.md).
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.adblock;
  # On the dummy link below, not loopback: resolved asks a link's server
  # through that link, so the forwarder listens on an address the link
  # itself holds. Link-local, so nothing past this machine routes to it.
  address = "169.254.153.53";
  # Both services run as the same user with the same confinement: a
  # reader and writer of its own state and a sender of DNS and HTTPS,
  # with no capabilities (systemd binds port 53 for the forwarder).
  hardening = {
    User = "losos-adblock";
    Group = "losos-adblock";
    CapabilityBoundingSet = "";
    NoNewPrivileges = true;
    ProtectSystem = "strict";
    ProtectHome = true;
    PrivateTmp = true;
    PrivateDevices = true;
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectControlGroups = true;
    RestrictAddressFamilies = [
      "AF_INET"
      "AF_INET6"
      "AF_UNIX"
    ];
    RestrictNamespaces = true;
    LockPersonality = true;
    SystemCallArchitectures = "native";
    SystemCallFilter = [ "@system-service" ];
  };
in
{
  options.losos.adblock = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Block ads and trackers for every app, from `lists`.";
    };

    lists = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "https://easylist.to/easylist/easylist.txt"
        "https://easylist.to/easylist/easyprivacy.txt"
        "https://adguardteam.github.io/AdGuardSDNSFilter/Filters/filter.txt"
        "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=0&mimetype=plaintext"
      ];
      description = ''
        Filter lists, by https URL or absolute path: Adblock Plus syntax
        (EasyList, EasyPrivacy, AdGuard's), hosts files, or one domain per
        line. URLs are fetched daily. A user adds more, one per line, in
        /var/lib/losos-adblock/lists.
      '';
    };

    encryptedDns = {
      mode = lib.mkOption {
        type = lib.types.enum [
          "on"
          "strict"
          "off"
        ];
        default = "on";
        description = ''
          Encrypted DNS until someone chooses otherwise
          (`losos-adblock encrypted-dns on|strict|off`, or Settings),
          which /var/lib/losos-adblock/encrypted-dns records. `on` asks
          the network's server while a captive portal is in the way, as
          Android's Private DNS does; `strict` never does.
        '';
      };

      servers = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        # Two operators, so one being down or blocked is not the end of
        # name resolution, both non-profit or no-logging and outside the
        # five-eyes jurisdictions: Quad9 (Switzerland) and Mullvad
        # (Sweden). Neither filters here: the blocking is this machine's
        # own. Over HTTPS first, which on 443 looks like any web page, and
        # over TLS after, for a network that breaks the other; one that
        # blocks 853 costs a single timeout before it rests (encrypted.rs).
        default = [
          "https://dns.quad9.net/dns-query 9.9.9.9 149.112.112.112 2620:fe::fe 2620:fe::9"
          "https://dns.mullvad.net/dns-query 194.242.2.2 2a07:e340::2"
          "tls://dns.quad9.net 9.9.9.9 149.112.112.112 2620:fe::fe 2620:fe::9"
          "tls://dns.mullvad.net 194.242.2.2 2a07:e340::2"
        ];
        description = ''
          The servers the forwarder asks while encrypted DNS is on, in
          order, each as its URL, `https://` for DNS over HTTPS or
          `tls://` for DNS over TLS, followed by the addresses to reach it
          at: the addresses are never looked up, since that lookup would
          be a plain query of its own. Servers listed after the first line
          of /var/lib/losos-adblock/encrypted-dns replace these.
        '';
      };
    };

    allow = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "example.com" ];
      description = ''
        Domains never blocked, with their subdomains, whatever the lists
        say; in the browser, nothing is blocked on their pages either.
        /var/lib/losos-adblock/allow adds more.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ pkgs.losos-adblock ];

    environment.etc."losos-adblock/lists".text = lib.concatLines cfg.lists;
    environment.etc."losos-adblock/allow".text = lib.concatLines cfg.allow;
    environment.etc."losos-adblock/encrypted-dns".text = lib.concatLines cfg.encryptedDns.servers;
    environment.etc."losos-adblock/encrypted-dns-default".text = "${cfg.encryptedDns.mode}\n";

    # Its own user, static rather than DynamicUser, so the state directory
    # it owns can stay readable by everyone: Danube reads the compiled
    # WebKit rule sets from it.
    users.users.losos-adblock = {
      isSystemUser = true;
      group = "losos-adblock";
    };
    users.groups.losos-adblock = { };

    # A dummy link whose one DNS server is the forwarder, for `~.`, every
    # domain. Its negative priority shuts out every other connection's
    # servers and domains, so resolved has no other server to ask and sends
    # everything here; the forwarder asks NetworkManager for the networks'
    # servers and sends each query there (serve.rs). A local network's own
    # names still resolve, through its server, but a bare name no longer
    # gets the network's search domain appended. DNS over TLS cannot reach
    # a forwarder that speaks plain DNS, so resolved's is off: the
    # forwarder encrypts instead, on the way out, with DNS over HTTPS or
    # TLS to servers it can verify, where resolved's "opportunistic" mode
    # took whatever the network's server offered and fell back to plain on
    # most networks.
    #
    # networkd ran the links before (network.nix), and this took the
    # default route from each of them and made the forwarder resolved's
    # global server. The forwarder read the networks' servers from
    # resolved's resolv.conf, which leaves out a link that is not a default
    # route, so it forwarded to nothing and every name failed.
    networking.networkmanager.ensureProfiles.profiles.losos-adblock = {
      connection = {
        id = "losos-adblock";
        type = "dummy";
        interface-name = "adblock0";
      };
      ipv4 = {
        method = "manual";
        address1 = "${address}/32";
        dns = "${address};";
        dns-search = "~.;";
        dns-priority = -100;
      };
      ipv6.method = "disabled";
    };
    services.resolved.settings.Resolve.DNSOverTLS = lib.mkForce "no";

    systemd.sockets.losos-adblock = {
      description = "losos-adblock DNS forwarder";
      wantedBy = [ "sockets.target" ];
      # resolved starts before sysinit.target, and a socket's default
      # dependencies order it after sysinit.target, so ordering this one
      # before resolved made a cycle systemd broke by dropping resolved's
      # start. Without them it is ready as early as resolved needs it, and
      # FreeBind lets it listen before the dummy link holds the address.
      unitConfig.DefaultDependencies = false;
      before = [
        "systemd-resolved.service"
        "sockets.target"
        "shutdown.target"
      ];
      conflicts = [ "shutdown.target" ];
      listenDatagrams = [ "${address}:53" ];
      listenStreams = [ "${address}:53" ];
      socketConfig.FreeBind = true;
    };

    systemd.services.losos-adblock = {
      description = "losos-adblock DNS forwarder";
      requires = [ "losos-adblock.socket" ];
      # nmcli, which the forwarder asks for the networks' servers.
      path = [ config.networking.networkmanager.package ];
      # Only a reader of its blocklist and of NetworkManager over D-Bus, and a
      # sender of DNS.
      serviceConfig = hardening // {
        ExecStart = "${lib.getExe pkgs.losos-adblock} serve";
        Restart = "always";
        MemoryDenyWriteExecute = true;
      };
    };

    systemd.services.losos-adblock-update = {
      description = "Fetch and compile losos-adblock's filter lists";
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      serviceConfig = hardening // {
        Type = "oneshot";
        ExecStart = "${lib.getExe pkgs.losos-adblock} update";
        # 2 means a list could not be fetched; the rest were compiled and
        # the last good copy of that one kept, so it is not a failure.
        SuccessExitStatus = [ 2 ];
        StateDirectory = "losos-adblock";
        StateDirectoryMode = "0755";
        UMask = "0022";
      };
    };

    # `losos-adblock encrypted-dns on|strict|off`, which Settings runs
    # through losos-adblock-encrypted-dns@on.service and its siblings, so
    # polkit asks an administrator, as for any unit started by hand. As the
    # forwarder's own user, the one that owns the switch in its state
    # directory; the forwarder sees it within a second.
    systemd.services."losos-adblock-encrypted-dns@" = {
      description = "Turn encrypted DNS %i";
      serviceConfig = hardening // {
        Type = "oneshot";
        ExecStart = "${lib.getExe pkgs.losos-adblock} encrypted-dns %i";
        StateDirectory = "losos-adblock";
        StateDirectoryMode = "0755";
        UMask = "0022";
        # It changes one file and talks to nobody.
        RestrictAddressFamilies = [ "AF_UNIX" ];
        PrivateNetwork = true;
      };
    };

    # Shortly after boot, so a fresh install blocks within minutes, and
    # daily after; a boot that missed a day catches up.
    systemd.timers.losos-adblock-update = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnCalendar = "daily";
        Persistent = true;
        # Spread the daily fetch, so the list hosts do not see every
        # machine at midnight.
        RandomizedDelaySec = "5min";
      };
    };

    # The state directory exists from the first boot, before the first
    # update, for the forwarder and Danube to read.
    systemd.tmpfiles.rules = [ "d /var/lib/losos-adblock 0755 losos-adblock losos-adblock -" ];
  };
}
